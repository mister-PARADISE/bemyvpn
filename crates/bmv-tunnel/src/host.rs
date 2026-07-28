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
    std::env::var("BMV_MAX_CONNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(512)
}

/// SSRF-ЗАЩИТА ХОСТА. Гость сам задаёт адрес назначения (это его IP-пакеты),
/// поэтому без фильтра он может дотянуться до ВНУТРЕННЕЙ инфраструктуры хозяина
/// хоста: облачные метаданные `169.254.169.254` (кража cloud-креденшелов!),
/// `127.0.0.1` (локальные админки/БД), LAN-роутер и соседние устройства.
/// По умолчанию режем петлю/линк-локал/приватные/зарезервированные/мультикаст —
/// нормальному гостю (сёрфинг в интернет) они не нужны, а хозяин защищён.
/// Кто СОЗНАТЕЛЬНО хочет отдать доступ к своей LAN — env `BMV_HOST_ALLOW_PRIVATE=1`.
fn dst_allowed(dst: &SocketAddr) -> bool {
    if std::env::var("BMV_HOST_ALLOW_PRIVATE").map(|v| v == "1" || v == "true").unwrap_or(false) {
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
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (a.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (a.segments()[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                || a.to_ipv4_mapped().map(|v4| !dst_allowed(&SocketAddr::new(v4.into(), dst.port()))).unwrap_or(false))
        }
    }
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

/// Считает живые проксируемые соединения гостя; на Drop уменьшает счётчик.
struct ConnGuard(Arc<AtomicUsize>);
impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Принять новое соединение, если гость не превысил лимит. None → отказ (лимит).
fn admit_conn(conns: &Arc<AtomicUsize>) -> Option<ConnGuard> {
    let cap = max_conns();
    if cap > 0 && conns.fetch_add(1, Ordering::Relaxed) >= cap {
        conns.fetch_sub(1, Ordering::Relaxed);
        None
    } else {
        Some(ConnGuard(conns.clone()))
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
            tracing::debug!(%dst, "TCP connect не удался: {e}");
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
    use super::dst_allowed;
    use std::net::SocketAddr;

    fn a(s: &str) -> SocketAddr { s.parse().unwrap() }

    #[test]
    fn ssrf_blocks_internal_allows_public() {
        // По умолчанию env не задан → приватка/петля/метаданные запрещены.
        std::env::remove_var("BMV_HOST_ALLOW_PRIVATE");
        // Публичный интернет — можно.
        assert!(dst_allowed(&a("8.8.8.8:443")));
        assert!(dst_allowed(&a("1.1.1.1:53")));
        // Внутреннее/опасное — нельзя.
        assert!(!dst_allowed(&a("127.0.0.1:8080")));      // локальные сервисы
        assert!(!dst_allowed(&a("169.254.169.254:80")));  // cloud metadata!
        assert!(!dst_allowed(&a("10.0.0.5:22")));         // LAN
        assert!(!dst_allowed(&a("192.168.1.1:80")));      // роутер
        assert!(!dst_allowed(&a("172.16.4.4:445")));      // LAN
        assert!(!dst_allowed(&a("100.64.0.1:80")));       // CGNAT
        assert!(!dst_allowed(&a("0.0.0.0:80")));
        assert!(!dst_allowed(&a("[::1]:80")));            // v6 петля
        assert!(!dst_allowed(&a("[fe80::1]:80")));        // v6 link-local
        assert!(!dst_allowed(&a("[fc00::1]:80")));        // v6 ULA
        assert!(!dst_allowed(&a("[::ffff:10.0.0.1]:80"))); // v4-mapped приватка
    }
}
