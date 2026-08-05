//! host — выходная нода БЕЗ прав и без TUN. Работает на любой ОС.
//!
//! Принимаем IP-пакеты гостя из канала, скармливаем userspace-стеку `ipstack`.
//! Он терминирует TCP/UDP-потоки локально и отдаёт их нам как обычные потоки —
//! на каждый мы открываем НАСТОЯЩИЙ сокет к адресату и перекачиваем байты.
//!
//! Почему так: обычные сокеты есть на всех платформах, root/iptables не нужны →
//! хостом может быть сервер, ПК, Android, iOS. Стек сам согласует TCP-сегменты
//! под MTU туннеля, так что MSS-костыли не нужны.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bmv_common::{Link, Result};
use ipstack::IpStackStream;
use ipstack::{IpStack, IpStackConfig, TcpConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

use crate::linkio::LinkIo;
use crate::MTU;

/// Буфер перекачки на ОДНО направление TCP. Это не окно TCP (оно своё, 1 МБ, см.
/// `tcp_window`), а лишь укрупнение системных вызовов, поэтому 32 КБ хватает с
/// головой. Прежние 64 КБ давали 128 КБ на соединение — при лимите в 1024 штуки
/// это 134 МБ памяти хоста, которые гость занимал одним циклом открытия сокетов.
const TCP_COPY_BUF: usize = 32 * 1024;
/// Буфер «от гостя». Резать здесь можно только потому, что размер ОГРАНИЧЕН
/// СВЕРХУ по дороге: `LinkIo::poll_read` выбрасывает пакет крупнее буфера, а
/// ipstack читает устройство буфером в один MTU (1400) — до разбора датаграмма
/// больше просто не доходит. Держим 3-кратный запас. ВАЖНО: ipstack отдаёт
/// полезную нагрузку через `put_slice`, а он не обрезает, а ПАДАЕТ, — так что
/// уменьшать это число ниже MTU нельзя.
const UDP_BUF_FROM_GUEST: usize = 4 * 1024;
/// Буфер «от сервера»: датаграмма приходит из настоящего интернета, и всё, что
/// не влезло, `recv` МОЛЧА ОТРЕЗАЕТ (сокет, а не паника). Поэтому запас щедрый —
/// DNS с EDNS0 просит 4 КБ, реальные ответы не приближаются и к 32 КБ. На потолок
/// памяти это не влияет: худший случай задают TCP-соединения, они «дороже».
const UDP_BUF_FROM_SERVER: usize = 32 * 1024;

/// Максимум одновременных проксируемых соединений на ОДНОГО гостя. Заслон от
/// исчерпания файловых дескрипторов и памяти хоста. Настраивается env
/// `BMV_MAX_CONNS` (0 = без лимита).
///
/// 512 — компромисс: браузер с десятками вкладок держит сотни сокетов, торрент
/// хочет больше, а верхняя граница обязана оставаться посчитанной. При нынешних
/// буферах потолок памяти на гостя ≈ 512 × 64 КБ ≈ 32 МБ.
/// ponytail: лимит по числу соединений, а не по памяти; если понадобится точнее —
/// считать байты в пуле буферов, а не штуки.
fn max_conns() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| num_env("BMV_MAX_CONNS", 512))
}

/// ОБЩИЙ потолок проксируемых соединений НА ВЕСЬ ХОСТ (все гости вместе).
/// Настраивается env `BMV_MAX_CONNS_TOTAL` (0 = без лимита).
///
/// Пер-гостевой лимит сам по себе ничего не гарантирует: он считается ВНУТРИ
/// одной сессии, поэтому при `max_guests=128` потолок был 128 × 512 = 65536
/// дескрипторов — при системном лимите 1024 на Linux и 256 у GUI-приложения на
/// macOS. А исчерпание дескрипторов бьёт не только по сессиям: рвётся сокет к
/// координатору, и хост целиком пропадает из каталога.
///
/// 1024 — заведомо ниже любого разумного `ulimit -n`, с запасом на служебные
/// сокеты (координатор, хаб, STUN). Кому мало — поднимают лимит ОС и эту
/// переменную осознанно.
/// ponytail: одно число вместо чтения RLIMIT_NOFILE; если понадобится точнее —
/// брать мягкий лимит ОС и делить, но это тянет libc в крейт.
fn max_conns_total() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| num_env("BMV_MAX_CONNS_TOTAL", 1024))
}

/// Живые проксируемые соединения ВСЕХ гостей этого процесса.
static TOTAL_CONNS: AtomicUsize = AtomicUsize::new(0);

/// Числовая настройка из окружения (кривое значение → дефолт).
fn num_env(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

/// SSRF-ЗАЩИТА ХОСТА. Гость сам задаёт адрес назначения (это его IP-пакеты),
/// поэтому без фильтра он может дотянуться до ВНУТРЕННЕЙ инфраструктуры хозяина
/// хоста: облачные метаданные `169.254.169.254` (кража cloud-креденшелов!),
/// `127.0.0.1` (локальные админки/БД), LAN-роутер и соседние устройства.
/// Наружу открываем сокет ТОЛЬКО в обычный интернет (`bmv_net::public_only` —
/// там же живут диапазоны и все обходные записи адреса).
/// Кто СОЗНАТЕЛЬНО хочет отдать доступ к своей LAN — env `BMV_HOST_ALLOW_PRIVATE=1`.
fn dst_allowed(dst: &SocketAddr) -> bool {
    dst_allowed_with(dst, allow_private())
}

/// Отдал ли хозяин хоста доступ к своей приватной сети СОЗНАТЕЛЬНО.
/// Читается ОДИН раз: за время жизни процесса окружение не меняется, а звалось
/// это на КАЖДОЕ новое соединение гостя (см. `env_settings_are_read_once`).
fn allow_private() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| yes(std::env::var("BMV_HOST_ALLOW_PRIVATE").ok()))
}

/// «Да» — это только «1»/«true»: иначе `BMV_HOST_ALLOW_PRIVATE=0` открывало бы LAN.
fn yes(v: Option<String>) -> bool {
    matches!(v.as_deref(), Some("1") | Some("true"))
}

/// Само правило, без чтения окружения: согласие хозяина ИЛИ обычный интернет.
///
/// Разделено ради тестов: `env::var` — состояние на весь процесс, а тесты идут
/// параллельно, поэтому тест, включающий доступ к LAN, ронял соседние. Заодно
/// пропал вызов `env::var` на каждое новое соединение.
///
/// Сами диапазоны (и все способы записать внутренний адрес так, чтобы он не
/// выглядел внутренним, — v4-mapped, NAT64, 6to4) живут в `bmv_net::reach`
/// ОДНИМ местом на весь клиент: тем же правилом отбираются цели встречного
/// пробивания и проверяется ответ STUN. Раньше эта таблица была записана здесь и
/// в разборе STUN отдельно, и копии уже разошлись.
fn dst_allowed_with(dst: &SocketAddr, allow_private: bool) -> bool {
    allow_private || bmv_net::public_only(dst)
}

/// Размер TCP-окна userspace-стека. Потолок потока = окно/RTT, поэтому окно
/// должно покрывать BDP пути (100 Мбит при RTT 80мс ≈ 1 МБ). Наш форк ipstack
/// умеет window scaling (RFC 7323) — окна больше 64 КБ реально работают в обе
/// стороны. Настраивается env `BMV_TCP_WINDOW` (байт).
fn tcp_window() -> u32 {
    std::env::var("BMV_TCP_WINDOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024 * 1024) // 1 МБ — покрывает BDP типичных путей
}

/// ХОСТ: обслуживать гостя, пока канал жив. Открывает реальные сокеты в интернет.
/// Завершается, когда keepalive обнаружил обрыв (сигнал от `LinkIo`).
pub async fn run_host(link: Arc<dyn Link>) -> Result<()> {
    let (dead_tx, dead_rx) = tokio::sync::oneshot::channel();
    let device = LinkIo::new(link.clone(), dead_tx);

    let window = tcp_window();
    let mut config = IpStackConfig::default();
    config.mtu_unchecked(MTU);
    let mut tcp = TcpConfig::default();
    tcp.max_unacked_bytes = window;
    tcp.read_buffer_size = window as usize;
    // Дефолтные 3 ретрансмита — мало: после них сегмент ВЫБРАСЫВАЕТСЯ и поток
    // умирает навсегда (дыру не заполнить). Даём больше попыток. RTO и порог
    // dup-ACK НЕ трогаем: агрессивные значения провоцируют спуривные
    // ретрансмиты и делают только хуже (проверено).
    tcp.max_retransmit_count = 10;
    config.with_tcp_config(tcp);

    let stack = IpStack::new(config, device);

    // Крутим приём потоков, но прерываемся, как только канал умер (keepalive).
    // Стек остановится сам при выходе из функции (Drop → abort фонового таска).
    let mut stack = stack;
    let conns = Arc::new(AtomicUsize::new(0));
    tokio::select! {
        _ = accept_loop(&mut stack, conns) => {}
        _ = dead_rx => {}
    }
    let _ = link.close().await; // прощаемся — гость увидит EOF сразу
    Ok(())
}

/// Считает живые проксируемые соединения гостя И общий счётчик хоста; на Drop
/// возвращает место обоим (в т.ч. при панике/отмене задачи моста).
struct ConnGuard(Arc<AtomicUsize>);
impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
        TOTAL_CONNS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Принять новое соединение, если не превышен ни пер-гостевой, ни ОБЩИЙ лимит.
/// None → отказ.
fn admit_conn(conns: &Arc<AtomicUsize>) -> Option<ConnGuard> {
    let (cap, total_cap) = (max_conns(), max_conns_total());
    if total_cap > 0 && TOTAL_CONNS.fetch_add(1, Ordering::Relaxed) >= total_cap {
        TOTAL_CONNS.fetch_sub(1, Ordering::Relaxed);
        return None;
    }
    if cap > 0 && conns.fetch_add(1, Ordering::Relaxed) >= cap {
        conns.fetch_sub(1, Ordering::Relaxed);
        TOTAL_CONNS.fetch_sub(1, Ordering::Relaxed);
        return None;
    }
    Some(ConnGuard(conns.clone()))
}

/// Принимать потоки гостя и мостить каждый к реальному сокету, пока стек жив.
async fn accept_loop(stack: &mut IpStack, conns: Arc<AtomicUsize>) {
    loop {
        let stream = match stack.accept().await {
            Ok(s) => s,
            Err(_) => break,
        };
        match stream {
            IpStackStream::Tcp(tcp) => {
                let dst = tcp.peer_addr(); // адрес, к которому подключился гость
                if !dst_allowed(&dst) {
                    drop(tcp); // не открываем сокет во внутреннюю сеть
                    continue;
                }
                let Some(guard) = admit_conn(&conns) else {
                    drop(tcp);
                    continue;
                };
                tokio::spawn(async move { let _g = guard; bridge_tcp(tcp, dst).await });
            }
            IpStackStream::Udp(udp) => {
                let dst = udp.peer_addr();
                if !dst_allowed(&dst) {
                    drop(udp);
                    continue;
                }
                let Some(guard) = admit_conn(&conns) else {
                    drop(udp);
                    continue;
                };
                tokio::spawn(async move { let _g = guard; bridge_udp(udp, dst).await });
            }
            // ICMP и прочее пока не проксируем (TCP/UDP/DNS покрывают всё нужное)
            IpStackStream::UnknownTransport(_) | IpStackStream::UnknownNetwork(_) => {}
        }
    }
}

/// Сколько ждём установления соединения к адресату. Без таймаута на «чёрную дыру»
/// (пакеты молча теряются) ждала бы сама ОС — до двух минут, — и всё это время
/// попытка занимала бы слот из лимита соединений гостя: сотня таких адресов, и
/// гость не может открыть ничего живого. 10с щедро даже для дальних хостов.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Мост одного TCP-потока гостя к настоящему серверу.
async fn bridge_tcp(mut guest: ipstack::IpStackTcpStream, dst: SocketAddr) {
    let mut server = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(dst)).await {
        Ok(Ok(s)) => s,
        Ok(Err(_)) | Err(_) => return,
    };
    let _ = tokio::io::copy_bidirectional_with_sizes(&mut guest, &mut server, TCP_COPY_BUF, TCP_COPY_BUF).await;
}

/// Мост одного UDP-потока гостя (например DNS) к настоящему серверу.
async fn bridge_udp(mut guest: ipstack::IpStackUdpStream, dst: SocketAddr) {
    let bind = if dst.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let server = match UdpSocket::bind(bind).await {
        Ok(s) => s,
        Err(_) => return,
    };
    if server.connect(dst).await.is_err() {
        return;
    }
    let mut from_guest = vec![0u8; UDP_BUF_FROM_GUEST];
    let mut from_server = vec![0u8; UDP_BUF_FROM_SERVER];
    loop {
        tokio::select! {
            r = guest.read(&mut from_guest) => match r {
                Ok(0) | Err(_) => break,
                Ok(n) => { if server.send(&from_guest[..n]).await.is_err() { break; } }
            },
            r = server.recv(&mut from_server) => match r {
                Ok(0) | Err(_) => break,
                Ok(n) => { if guest.write_all(&from_server[..n]).await.is_err() { break; } }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::dst_allowed_with;
    use std::net::SocketAddr;

    fn a(s: &str) -> SocketAddr { s.parse().unwrap() }

    /// ХОСТ, ПОГАСИВШИЙ РАЗДАЧУ, ОБЯЗАН ПОПРОЩАТЬСЯ С ГОСТЕМ.
    ///
    /// Все оболочки выключают раздачу ОТМЕНОЙ задачи (дроп `JoinSet`), а не
    /// возвратом из `run_host` — значит `link.close()` в конце функции не
    /// наступает. Пока фоновые задачи `LinkIo` держали `Arc` канала, канал не
    /// дропался и прощание не уходило: гость висел «подключён» до
    /// keepalive-таймаута (8с). Здесь раздачу гасят ровно так, как в жизни, и
    /// гость обязан увидеть EOF ИМЕННО ПО BYE и за доли секунды.
    #[tokio::test]
    async fn aborted_hosting_still_says_goodbye_to_the_guest() {
        use bmv_common::{KeepaliveLink, Link};
        let (host_raw, guest_raw) = bmv_common::wire::memory_pair(64);
        let host_link: std::sync::Arc<dyn Link> = std::sync::Arc::new(KeepaliveLink::new(host_raw));
        let guest = KeepaliveLink::new(guest_raw);

        let session = tokio::spawn(super::run_host(host_link));
        // Даём стеку подняться (иначе гасим то, чего ещё нет).
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        session.abort(); // ровно это делает дроп JoinSet при «Выключить»

        let mut buf = Vec::new();
        let r = tokio::time::timeout(std::time::Duration::from_secs(1), guest.recv_into(&mut buf)).await;
        assert!(
            !r.expect("гость не увидел разрыв за 1с — хост погасил раздачу молча, гость ждёт keepalive-таймаут (8с)")
                .unwrap(),
            "после остановки раздачи канал гостя обязан быть закрыт"
        );
        assert!(
            guest.peer_said_bye(),
            "разрыв обнаружен по ТИШИНЕ, а не по BYE — прощание с отменённой сессии не ушло",
        );
    }

    // ── SSRF-фильтр: диапазоны переехали, ручка осталась ─────────────────────
    //
    // Сама таблица внутренних диапазонов и все обходные записи адреса (v4-mapped,
    // NAT64, 6to4, site-local, границы блоков) живут и проверяются в
    // `bmv-net/src/reach.rs` — одним местом на весь клиент. Здесь остаётся то,
    // что принадлежит ХОСТУ: разбор согласия хозяина и доказательство, что фильтр
    // реально позван из `accept_loop`.

    /// Явное разрешение приватных адресов — осознанный выбор хозяина хоста.
    /// Тест держит семантику значения: включает ТОЛЬКО «1»/«true», а не любое
    /// непустое (иначе `BMV_HOST_ALLOW_PRIVATE=0` открывало бы LAN).
    ///
    /// Разбор проверяется на ЧИСТОЙ функции, без окружения: переменные окружения
    /// глобальны на процесс, а тесты идут параллельно — прежняя версия этого теста
    /// на секунду открывала LAN всему бинарю тестов.
    #[test]
    fn ssrf_opt_in_requires_explicit_yes() {
        for (val, expect) in [("1", true), ("true", true), ("0", false), ("нет", false), ("", false)] {
            assert_eq!(super::yes(Some(val.into())), expect, "значение «{val}» разобрано неверно");
        }
        assert!(!super::yes(None), "без переменной доступ в LAN обязан быть закрыт");

        // И само правило: включённый флаг открывает приватку, выключенный — нет.
        assert!(dst_allowed_with(&a("192.168.1.1:80"), true), "явное согласие обязано открывать LAN");
        assert!(!dst_allowed_with(&a("192.168.1.1:80"), false));
    }

    /// НАСТРОЙКИ ЧИТАЮТСЯ ОДИН РАЗ, А НЕ НА КАЖДОЕ СОЕДИНЕНИЕ. `env::var` — это
    /// поиск по таблице окружения с блокировкой; на горячем пути (новое соединение
    /// гостя) он не нужен: переменная за время работы процесса не меняется, и
    /// комментарий рядом с фильтром это давно утверждал, а код делал иначе.
    #[test]
    fn env_settings_are_read_once() {
        let first = super::allow_private();
        assert!(!first, "в тестах переменная не задана");
        std::env::set_var("BMV_HOST_ALLOW_PRIVATE", "1");
        let second = super::allow_private();
        std::env::remove_var("BMV_HOST_ALLOW_PRIVATE");
        assert_eq!(second, first, "значение перечитано из окружения на ходу");
    }

    /// ЛИМИТ СОЕДИНЕНИЙ ОБЩИЙ НА ХОСТ, А НЕ НА ГОСТЯ. Пер-гостевой потолок (512)
    /// считался внутри одной сессии, поэтому при 128 гостях потолок был 65536
    /// дескрипторов при системном лимите 1024 (а на macOS-GUI и вовсе 256). Их
    /// исчерпание валит не только сессии: рвётся и связь с координатором, то есть
    /// хост пропадает из каталога целиком.
    #[test]
    fn conn_limit_is_global_not_per_guest() {
        let guests: Vec<_> = (0..3).map(|_| std::sync::Arc::new(super::AtomicUsize::new(0))).collect();
        let cap = super::max_conns_total();
        let per_guest = super::max_conns();
        assert!(cap > 0 && per_guest > 0, "лимиты должны быть заданы");

        // Набираем соединения, пока пускают, — по всем гостям сразу.
        let mut held = Vec::new();
        'outer: for _ in 0..per_guest {
            for g in &guests {
                match super::admit_conn(g) {
                    Some(guard) => held.push(guard),
                    None => break 'outer,
                }
            }
        }
        assert!(
            held.len() <= cap,
            "выдано {} соединений при общем потолке {cap} — потолок считается на гостя",
            held.len()
        );
        // Место освобождается по мере закрытия соединений (иначе хост «залипал» бы
        // на потолке до перезапуска).
        held.truncate(held.len().saturating_sub(1));
        assert!(super::admit_conn(&guests[0]).is_some(), "освободившееся место не вернулось");
    }

    // ── Фильтр ПРИМЕНЁН, а не просто написан ─────────────────────────────────
    //
    // Правило адресов проверено у себя дома (`bmv-net/src/reach.rs`), но ни один
    // тест ТАМ не заметит удаления строки `if !dst_allowed(&dst) { continue }`
    // здесь, в `accept_loop`: правило осталось бы безупречным и никем не
    // позванным, а дыра — открытой. Ниже настоящий `run_host` и настоящий
    // сокет-жертва на петле.

    /// Собрать IP-пакет гостя к жертве. `tcp = true` — SYN, иначе датаграмма.
    fn packet_to(victim: std::net::SocketAddr, tcp: bool) -> Vec<u8> {
        let dst = match victim.ip() {
            std::net::IpAddr::V4(v4) => v4.octets(),
            _ => unreachable!("жертва слушает на IPv4"),
        };
        let ip = etherparse::PacketBuilder::ipv4([10, 7, 0, 2], dst, 64);
        let mut out = Vec::new();
        if tcp {
            let b = ip.tcp(40000, victim.port(), 1000, 32 * 1024).syn();
            b.write(&mut out, &[]).unwrap();
        } else {
            let b = ip.udp(40000, victim.port());
            b.write(&mut out, b"payload").unwrap();
        }
        out
    }

    /// TCP-ПОТОК ГОСТЯ ВО ВНУТРЕННЮЮ СЕТЬ НЕ ОТКРЫВАЕТСЯ. Гость сам пишет адрес
    /// назначения — здесь это 127.0.0.1, то есть локальная админка/БД хозяина.
    #[tokio::test]
    async fn a_guest_tcp_stream_never_reaches_the_loopback() {
        let victim = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = victim.local_addr().unwrap();

        let (host_link, guest) = bmv_common::wire::memory_pair(64);
        let _session = tokio::spawn(super::run_host(std::sync::Arc::from(host_link)));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // стек поднимается
        bmv_common::Link::send(&*guest, &packet_to(addr, true)).await.unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_secs(2), victim.accept()).await;
        assert!(got.is_err(), "хост открыл гостю TCP-сокет на петлю ({addr}) — SSRF-фильтр не применён");
    }

    /// То же для UDP: своя ветка в `accept_loop`, свой `continue`, своя дыра.
    #[tokio::test]
    async fn a_guest_udp_datagram_never_reaches_the_loopback() {
        let victim = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = victim.local_addr().unwrap();

        let (host_link, guest) = bmv_common::wire::memory_pair(64);
        let _session = tokio::spawn(super::run_host(std::sync::Arc::from(host_link)));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        bmv_common::Link::send(&*guest, &packet_to(addr, false)).await.unwrap();

        let mut buf = [0u8; 64];
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), victim.recv_from(&mut buf)).await;
        assert!(got.is_err(), "хост отправил датаграмму гостя на петлю ({addr}) — SSRF-фильтр не применён");
    }
}
