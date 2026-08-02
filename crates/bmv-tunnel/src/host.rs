//! host — выходная нода БЕЗ прав и без TUN. Работает на любой ОС.
//!
//! Принимаем IP-пакеты гостя из канала, скармливаем userspace-стеку `ipstack`.
//! Он терминирует TCP/UDP-потоки локально и отдаёт их нам как обычные потоки —
//! на каждый мы открываем НАСТОЯЩИЙ сокет к адресату и перекачиваем байты.
//!
//! Почему так: обычные сокеты есть на всех платформах, root/iptables не нужны →
//! хостом может быть сервер, ПК, Android, iOS. Стек сам согласует TCP-сегменты
//! под MTU туннеля, так что MSS-костыли не нужны.

use std::net::{IpAddr, SocketAddr};
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
/// По умолчанию режем петлю/линк-локал/приватные/зарезервированные/мультикаст —
/// нормальному гостю (сёрфинг в интернет) они не нужны, а хозяин защищён.
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

/// Само правило, без чтения окружения.
///
/// Разделено ради тестов: `env::var` — состояние на весь процесс, а тесты идут
/// параллельно, поэтому тест, включающий доступ к LAN, ронял соседние. Заодно
/// пропал вызов `env::var` на каждое новое соединение.
fn dst_allowed_with(dst: &SocketAddr, allow_private: bool) -> bool {
    if allow_private {
        return true;
    }
    match dst.ip() {
        IpAddr::V4(a) => {
            !(a.is_loopback()
                || a.is_private()
                || a.is_link_local() // включает 169.254.169.254 (cloud metadata)
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_multicast()
                || a.is_documentation()
                || a.octets()[0] == 0
                || (a.octets()[0] == 100 && (a.octets()[1] & 0xC0) == 64) // 100.64/10 CGNAT
                || a.octets()[0] >= 240) // 240/4 reserved
        }
        IpAddr::V6(a) => {
            // IPv6 умеет НЕСТИ В СЕБЕ адрес IPv4, и шлюз по дороге его развернёт.
            // Значит одну и ту же внутреннюю цель можно записать несколькими
            // способами, и проверять надо не запись, а то, куда пакет доедет.
            if let Some(v4) = embedded_v4(a) {
                return dst_allowed_with(&SocketAddr::new(v4.into(), dst.port()), allow_private);
            }
            let s0 = a.segments()[0];
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s0 & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s0 & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (s0 & 0xffc0) == 0xfec0) // site-local fec0::/10: из стандарта
                                            // убран, но стеки его понимают, и в
                                            // старых корпоративных сетях он живой
        }
    }
}

/// Вытащить IPv4, зашитый в IPv6-адрес, если он там есть.
///
/// Три способа записать «тот же самый» адрес, каждый из которых обходил бы
/// проверку по IPv4-диапазонам, если смотреть только на внешнюю форму:
///   * `::ffff:a.b.c.d`  — v4-mapped, ядро идёт прямо по IPv4;
///   * `64:ff9b::a.b.c.d` — NAT64 (RFC 6052), шлюз переводит в IPv4-пакет;
///   * `2002:AABB:CCDD::/48` — 6to4 (RFC 3056), адрес шлюза в следующих 32 битах.
///
/// Возвращаем вложенный адрес, чтобы судить о нём по правилам IPv4.
fn embedded_v4(a: std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    if let Some(v4) = a.to_ipv4_mapped() {
        return Some(v4);
    }
    let s = a.segments();
    // NAT64 well-known prefix 64:ff9b::/96 — адрес в последних двух группах.
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        return Some(std::net::Ipv4Addr::from(((s[6] as u32) << 16) | s[7] as u32));
    }
    // 6to4 2002::/16 — адрес шлюза в группах 1 и 2.
    if s[0] == 0x2002 {
        return Some(std::net::Ipv4Addr::from(((s[1] as u32) << 16) | s[2] as u32));
    }
    None
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
pub async fn run_host(link: Box<dyn Link>) -> Result<()> {
    let link: std::sync::Arc<dyn Link> = std::sync::Arc::from(link);
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
    tracing::info!(window, "TCP-окно");

    let stack = IpStack::new(config, device);
    tracing::info!("userspace-стек хоста поднят (без root)");

    // Крутим приём потоков, но прерываемся, как только канал умер (keepalive).
    // Стек остановится сам при выходе из функции (Drop → abort фонового таска).
    let mut stack = stack;
    let conns = Arc::new(AtomicUsize::new(0));
    tokio::select! {
        _ = accept_loop(&mut stack, conns) => {}
        _ = dead_rx => tracing::debug!("keepalive: пир мёртв — закрываю сессию"),
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
        tracing::warn!(total_cap, "общий лимит соединений хоста достигнут — поток отклонён");
        return None;
    }
    if cap > 0 && conns.fetch_add(1, Ordering::Relaxed) >= cap {
        conns.fetch_sub(1, Ordering::Relaxed);
        TOTAL_CONNS.fetch_sub(1, Ordering::Relaxed);
        return None;
    }
    Some(ConnGuard(conns.clone()))
}

/// КУДА МОЖНО СЛАТЬ ВСТРЕЧНЫЙ PUNCH. Адреса ждущего гостя хост узнаёт С ЕГО СЛОВ
/// (через координатор) и бьёт в них 48 пачками UDP — то есть чужими руками можно
/// было сканировать LAN хозяина и облачные метаданные.
///
/// Правило то же, что для трафика (`dst_allowed_with`), с ОДНИМ послаблением:
/// приватные адреса разрешены. Гость в той же локальной сети — рабочий сценарий
/// (ради него в кандидатах и есть `local_ip()`), а вот петля, link-local
/// (169.254.169.254 — метаданные облака), мультикаст и зарезервированные
/// диапазоны точками входа гостя не бывают никогда.
pub fn punch_target_allowed(dst: &SocketAddr) -> bool {
    if dst.port() == 0 {
        return false; // пробивать некуда
    }
    // Публичный адрес — обычным правилом (оно же режет всё внутреннее).
    if dst_allowed_with(dst, false) {
        return true;
    }
    // Осталось разрешить РОВНО локальные сети — и ничего больше.
    match dst.ip() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private() || (o[0] == 100 && (o[1] & 0xC0) == 64) // 10/8,172.16/12,192.168/16 + CGNAT
        }
        IpAddr::V6(v6) => match embedded_v4(v6) {
            // IPv4 внутри IPv6 судим по правилам IPv4 (иначе фильтр обходится
            // записью адреса в другой форме — см. тесты SSRF).
            Some(v4) => punch_target_allowed(&SocketAddr::new(v4.into(), dst.port())),
            None => (v6.segments()[0] & 0xfe00) == 0xfc00, // ULA fc00::/7
        },
    }
}

/// Принимать потоки гостя и мостить каждый к реальному сокету, пока стек жив.
async fn accept_loop(stack: &mut IpStack, conns: Arc<AtomicUsize>) {
    loop {
        let stream = match stack.accept().await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("стек завершился: {e}");
                break;
            }
        };
        match stream {
            IpStackStream::Tcp(tcp) => {
                let dst = tcp.peer_addr(); // адрес, к которому подключился гость
                if !dst_allowed(&dst) {
                    tracing::debug!(%dst, "SSRF-фильтр: адрес запрещён (внутренний), поток отклонён");
                    drop(tcp); // не открываем сокет во внутреннюю сеть
                    continue;
                }
                let Some(guard) = admit_conn(&conns) else {
                    tracing::debug!("лимит соединений гостя достигнут, TCP-поток отклонён");
                    drop(tcp);
                    continue;
                };
                tokio::spawn(async move { let _g = guard; bridge_tcp(tcp, dst).await });
            }
            IpStackStream::Udp(udp) => {
                let dst = udp.peer_addr();
                if !dst_allowed(&dst) {
                    tracing::debug!(%dst, "SSRF-фильтр: адрес запрещён (внутренний), UDP-поток отклонён");
                    drop(udp);
                    continue;
                }
                let Some(guard) = admit_conn(&conns) else {
                    tracing::debug!("лимит соединений гостя достигнут, UDP-поток отклонён");
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
        Ok(Err(e)) => {
            // Кончились дескрипторы — это не «сайт не отвечает», а отказ ВСЕГО
            // хоста: следом рвётся и сокет к координатору. Такое обязано быть
            // видно в журнале, а не тонуть среди обычных отказов соединения.
            // EMFILE=24 (лимит процесса), ENFILE=23 (лимит системы) — на Linux и
            // macOS номера совпадают.
            if matches!(e.raw_os_error(), Some(23) | Some(24)) {
                tracing::warn!(%dst, "КОНЧИЛИСЬ ДЕСКРИПТОРЫ ({e}) — поднимите ulimit -n или BMV_MAX_CONNS_TOTAL");
            } else {
                tracing::debug!(%dst, "TCP connect не удался: {e}");
            }
            return;
        }
        Err(_) => {
            tracing::debug!(%dst, "TCP connect: таймаут");
            return;
        }
    };
    let _ = tokio::io::copy_bidirectional_with_sizes(&mut guest, &mut server, TCP_COPY_BUF, TCP_COPY_BUF).await;
}

/// Мост одного UDP-потока гостя (например DNS) к настоящему серверу.
async fn bridge_udp(mut guest: ipstack::IpStackUdpStream, dst: SocketAddr) {
    let bind = if dst.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let server = match UdpSocket::bind(bind).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(%dst, "UDP bind не удался: {e}");
            return;
        }
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
    use super::{dst_allowed_with, embedded_v4};
    use std::net::SocketAddr;

    fn a(s: &str) -> SocketAddr { s.parse().unwrap() }

    // ── КАТЕГОРИЯ А: обходные пути SSRF-фильтра ───────────────────────────────
    //
    // Гость сам пишет адрес назначения в своих IP-пакетах. Фильтр обязан резать
    // всё внутреннее — а способов записать внутренний адрес так, чтобы он не
    // выглядел внутренним, придумано много. Каждый тест ниже — отдельный приём.

    /// v4-mapped v6 (`::ffff:a.b.c.d`) — тот же адрес, записанный по-другому.
    /// Классика обхода: проверка «это же IPv6» пропускает, а ядро идёт по IPv4.
    #[test]
    fn ssrf_v4_mapped_covers_every_internal_range() {
        for s in [
            "[::ffff:127.0.0.1]:22",       // петля
            "[::ffff:169.254.169.254]:80", // метаданные облака
            "[::ffff:10.0.0.5]:22",
            "[::ffff:192.168.1.1]:80",
            "[::ffff:172.16.0.1]:445",
            "[::ffff:100.64.0.1]:80",      // CGNAT
            "[::ffff:0.0.0.0]:80",
        ] {
            assert!(!dst_allowed_with(&a(s), false), "v4-mapped обход пролез: {s}");
        }
        // …и при этом обычный публичный адрес в той же записи проходить обязан.
        assert!(dst_allowed_with(&a("[::ffff:8.8.8.8]:53"), false));
    }

    /// Устаревший site-local IPv6 `fec0::/10`. Из стандарта его убрали, но стеки
    /// его понимают, и в старых корпоративных сетях он живой — то есть это
    /// рабочий адрес внутренней сети, который наш фильтр обязан резать.
    #[test]
    fn ssrf_blocks_ipv6_site_local() {
        assert!(!dst_allowed_with(&a("[fec0::1]:80"), false), "site-local fec0::/10 пролез");
        assert!(!dst_allowed_with(&a("[feff::1]:80"), false), "верхняя граница fec0::/10 пролезла");
    }

    /// NAT64 `64:ff9b::/96`: адрес несёт в себе IPv4 в младших 32 битах, и шлюз
    /// NAT64 переведёт его в настоящий IPv4-пакет. То есть `64:ff9b::7f00:1`
    /// в сети с NAT64 — это 127.0.0.1.
    #[test]
    fn ssrf_blocks_nat64_embedded_internal_v4() {
        assert!(!dst_allowed_with(&a("[64:ff9b::7f00:1]:22"), false), "NAT64 к петле пролез");
        assert!(!dst_allowed_with(&a("[64:ff9b::a9fe:a9fe]:80"), false), "NAT64 к метаданным пролез");
        assert!(!dst_allowed_with(&a("[64:ff9b::c0a8:101]:80"), false), "NAT64 к роутеру пролез");
    }

    /// 6to4 `2002::/16`: следующие 32 бита — IPv4-адрес шлюза. На хосте с 6to4
    /// это ещё один способ записать внутренний адрес.
    #[test]
    fn ssrf_blocks_6to4_embedded_internal_v4() {
        assert!(!dst_allowed_with(&a("[2002:7f00:1::1]:22"), false), "6to4 к 127.0.0.1 пролез");
        assert!(!dst_allowed_with(&a("[2002:c0a8:101::1]:80"), false), "6to4 к 192.168.1.1 пролез");
        // Обычный 6to4 в публичный адрес запрещать не за что.
        assert!(dst_allowed_with(&a("[2002:808:808::1]:80"), false), "6to4 к 8.8.8.8 зря отклонён");
    }

    /// Границы приватных диапазонов: соседние адреса СНАРУЖИ блока обязаны
    /// проходить, иначе фильтр отрезает кусок настоящего интернета.
    #[test]
    fn ssrf_boundaries_are_exact() {
        // 172.16/12 — ровно 172.16.0.0 … 172.31.255.255.
        assert!(!dst_allowed_with(&a("172.16.0.0:80"), false));
        assert!(!dst_allowed_with(&a("172.31.255.255:80"), false));
        assert!(dst_allowed_with(&a("172.15.255.255:80"), false), "172.15.x — публичный, зря отрезан");
        assert!(dst_allowed_with(&a("172.32.0.0:80"), false), "172.32.x — публичный, зря отрезан");
        // 100.64/10 (CGNAT) — 100.64.0.0 … 100.127.255.255.
        assert!(!dst_allowed_with(&a("100.127.255.255:80"), false));
        assert!(dst_allowed_with(&a("100.63.255.255:80"), false), "100.63.x — публичный, зря отрезан");
        assert!(dst_allowed_with(&a("100.128.0.0:80"), false), "100.128.x — публичный, зря отрезан");
        // 240/4 зарезервирован, 239.x — мультикаст (тоже нельзя), 223.x — можно.
        assert!(!dst_allowed_with(&a("240.0.0.1:80"), false));
        assert!(dst_allowed_with(&a("223.255.255.255:80"), false), "223.x — публичный, зря отрезан");
    }

    /// Лазейка «сам себе прокси» на ОДНУ строку ниже по стеку: порт 0.
    /// Подключиться к нему нельзя, но проверить, что фильтр не падает и не
    /// пропускает внутренний адрес из-за нулевого порта, стоит.
    #[test]
    fn ssrf_zero_port_does_not_bypass() {
        assert!(!dst_allowed_with(&a("127.0.0.1:0"), false));
        assert!(!dst_allowed_with(&a("[::1]:0"), false));
        assert!(!dst_allowed_with(&a("[::ffff:10.0.0.1]:0"), false));
    }

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

    /// ЦЕЛИ ВСТРЕЧНОГО ПРОБИТИЯ. Адреса ждущего гостя приходят хосту С ЕГО СЛОВ
    /// (через координатор), и хост шлёт туда 48 пачек UDP с шагом 250мс. Без
    /// фильтра это сканер чужой LAN и облачных метаданных, оплаченный чужой
    /// машиной. При этом гость в ТОЙ ЖЕ локальной сети — рабочий сценарий (потому
    /// в кандидатах и есть local_ip), и приватку резать нельзя.
    #[test]
    fn punch_targets_exclude_internal_but_keep_lan() {
        use super::punch_target_allowed as ok;
        // Свой же хост, чужие админки, метаданные облака, мультикаст — нельзя.
        assert!(!ok(&a("127.0.0.1:40000")), "петля");
        assert!(!ok(&a("169.254.169.254:80")), "метаданные облака");
        assert!(!ok(&a("169.254.1.1:40000")), "link-local");
        assert!(!ok(&a("224.0.0.1:40000")), "мультикаст");
        assert!(!ok(&a("255.255.255.255:40000")), "бродкаст");
        assert!(!ok(&a("0.0.0.0:40000")));
        assert!(!ok(&a("240.0.0.1:40000")), "зарезервированный 240/4");
        assert!(!ok(&a("8.8.8.8:0")), "нулевой порт — пробивать некуда");
        assert!(!ok(&a("[::1]:40000")), "v6 петля");
        assert!(!ok(&a("[::ffff:127.0.0.1]:40000")), "петля в записи v4-mapped");
        assert!(!ok(&a("[64:ff9b::a9fe:a9fe]:80")), "метаданные через NAT64");
        // Публичный интернет и ЛОКАЛЬНАЯ СЕТЬ — можно (иначе гость из той же
        // квартиры/офиса перестал бы подключаться).
        assert!(ok(&a("45.11.22.33:40000")), "публичный адрес зря отрезан");
        assert!(ok(&a("192.168.1.5:40000")), "гость в той же LAN — рабочий случай");
        assert!(ok(&a("10.1.2.3:40000")), "LAN 10/8 зря отрезана");
        assert!(ok(&a("172.16.5.5:40000")), "LAN 172.16/12 зря отрезана");
        assert!(ok(&a("100.64.0.1:40000")), "CGNAT — обычный мобильный гость");
        assert!(ok(&a("[fc00::1]:40000")), "ULA — та же LAN по IPv6");
    }

    /// Разбор вложенного IPv4 — отдельно от политики: если он ошибётся, фильтр
    /// начнёт судить не тот адрес, и это не заметит ни один тест выше.
    #[test]
    fn embedded_v4_extracts_exactly_the_right_address() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        let v6 = |s: &str| s.parse::<Ipv6Addr>().unwrap();
        assert_eq!(embedded_v4(v6("::ffff:1.2.3.4")), Some(Ipv4Addr::new(1, 2, 3, 4)));
        assert_eq!(embedded_v4(v6("64:ff9b::102:304")), Some(Ipv4Addr::new(1, 2, 3, 4)));
        assert_eq!(embedded_v4(v6("2002:102:304::1")), Some(Ipv4Addr::new(1, 2, 3, 4)));
        // Похожие, но НЕ несущие адрес: не выдумываем IPv4 там, где его нет.
        assert_eq!(embedded_v4(v6("64:ff9b:1::102:304")), None, "не well-known NAT64-префикс");
        assert_eq!(embedded_v4(v6("2003:102:304::1")), None, "не 6to4");
        assert_eq!(embedded_v4(v6("2001:db8::1")), None);
    }

    #[test]
    fn ssrf_blocks_internal_allows_public() {
        // По умолчанию env не задан → приватка/петля/метаданные запрещены.
        // Публичный интернет — можно.
        assert!(dst_allowed_with(&a("8.8.8.8:443"), false));
        assert!(dst_allowed_with(&a("1.1.1.1:53"), false));
        // Внутреннее/опасное — нельзя.
        assert!(!dst_allowed_with(&a("127.0.0.1:8080"), false));      // локальные сервисы
        assert!(!dst_allowed_with(&a("169.254.169.254:80"), false));  // cloud metadata!
        assert!(!dst_allowed_with(&a("10.0.0.5:22"), false));         // LAN
        assert!(!dst_allowed_with(&a("192.168.1.1:80"), false));      // роутер
        assert!(!dst_allowed_with(&a("172.16.4.4:445"), false));      // LAN
        assert!(!dst_allowed_with(&a("100.64.0.1:80"), false));       // CGNAT
        assert!(!dst_allowed_with(&a("0.0.0.0:80"), false));
        assert!(!dst_allowed_with(&a("[::1]:80"), false));            // v6 петля
        assert!(!dst_allowed_with(&a("[fe80::1]:80"), false));        // v6 link-local
        assert!(!dst_allowed_with(&a("[fc00::1]:80"), false));        // v6 ULA
        assert!(!dst_allowed_with(&a("[::ffff:10.0.0.1]:80"), false)); // v4-mapped приватка
    }
}
