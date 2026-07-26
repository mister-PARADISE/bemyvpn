//! STUN-клиент (RFC 5389) — узнать свой внешний IP:port через пул серверов.
//!
//! Зачем: чтобы пир мог сказать другому, по какому адресу его искать (кандидат
//! server-reflexive). Сам STUN-сервер трафик не пропускает и ничего про нас не
//! хранит — только отражает адрес, каким нас видит внешний мир.
//!
//! Есть два входа: `reflexive_addr` (заводит свой временный сокет — для
//! диагностики) и `reflexive_addr_on` (использует УЖЕ забинженный сокет — важно,
//! чтобы анонсированный порт был тем же, на котором мы потом слушаем пира).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bmv_common::{Error, Result};
use rand::RngCore;
use tokio::net::{lookup_host, UdpSocket};
use tokio::time::timeout;

/// Встроенный пул STUN-серверов (пусто в конфиге → берём это).
pub const DEFAULT_STUN: &[&str] = &[
    "stun.l.google.com:19302",
    "stun1.l.google.com:19302",
    "stun.cloudflare.com:3478",
    "stun.nextcloud.com:443",
];

const MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Узнать внешний адрес, заведя свой временный сокет (для диагностики/CLI).
pub async fn reflexive_addr(servers: &[String], per_server: Duration) -> Result<SocketAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    reflexive_addr_on(&sock, servers, per_server).await
}

/// Узнать внешний адрес, используя УЖЕ забинженный сокет (переиспользуемый для
/// данных). Опрашиваем ВСЕ серверы ПАРАЛЛЕЛЬНО с одного сокета: шлём запрос
/// каждому (свой txid), затем берём первый валидный ответ. Один мёртвый сервер
/// больше не крадёт свои 5 секунд — общий таймаут на всё.
pub async fn reflexive_addr_on(
    sock: &UdpSocket,
    servers: &[String],
    wait: Duration,
) -> Result<SocketAddr> {
    let list: Vec<String> = if servers.is_empty() {
        DEFAULT_STUN.iter().map(|s| s.to_string()).collect()
    } else {
        servers.to_vec()
    };

    // Резолвим адреса и шлём запросы веером; каждому серверу — свой txid, чтобы
    // сматчить ответ. Отвалившийся на DNS/send сервер просто пропускаем.
    let mut pending: Vec<(SocketAddr, [u8; 12])> = Vec::with_capacity(list.len());
    for server in &list {
        let dst = match lookup_host(server).await.ok().and_then(|mut a| a.next()) {
            Some(d) => d,
            None => continue,
        };
        let (req, txid) = build_request();
        if sock.send_to(&req, dst).await.is_ok() {
            pending.push((dst, txid));
        }
    }
    if pending.is_empty() {
        return Err(Error::Net("ни один STUN-сервер не резолвится".into()));
    }

    // Ждём первый ПОДХОДЯЩИЙ ответ (совпал источник и txid) до общего дедлайна.
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 512];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Net("STUN: таймаут (никто не ответил)".into()));
        }
        let (n, from) = match timeout(left, sock.recv_from(&mut buf)).await {
            Ok(r) => r?,
            Err(_) => return Err(Error::Net("STUN: таймаут (никто не ответил)".into())),
        };
        // Ищем отправителя среди опрошенных и проверяем его txid.
        if let Some((_, txid)) = pending.iter().find(|(dst, _)| *dst == from) {
            if let Some(addr) = parse_response(&buf[..n], txid) {
                log::info!("STUN: мой внешний адрес {addr} (по ответу {from})");
                return Ok(addr);
            }
        }
        // чужая датаграмма — игнорируем, ждём дальше
    }
}

/// АВТООПРЕДЕЛЕНИЕ ТИПА NAT (mapping behavior, RFC 4787). Шлём Binding Request на
/// ДВА разных STUN-сервера с одного сокета и сравниваем внешние порты:
///   одинаковый порт → endpoint-independent mapping = «мягкий» (cone);
///   разный порт     → «строгий» (symmetric): внешний порт зависит от адресата,
///                     значит прямой прострел к такому пиру невозможен без
///                     обучения его реального порта (обратное пробитие).
/// Возвращает "cone" | "symmetric" | "" (не удалось определить — <2 ответов).
pub async fn classify_mapping(servers: &[String], wait: Duration) -> String {
    let list: Vec<String> = if servers.is_empty() {
        DEFAULT_STUN.iter().map(|s| s.to_string()).collect()
    } else {
        servers.to_vec()
    };
    let sock = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    // Два сервера с РАЗНЫМИ IP (иначе сравнение бессмысленно).
    let mut pending: Vec<(SocketAddr, [u8; 12])> = Vec::new();
    let mut seen_ips = std::collections::HashSet::new();
    for server in &list {
        if pending.len() >= 2 {
            break;
        }
        let dst = match lookup_host(server).await.ok().and_then(|mut a| a.next()) {
            Some(d) => d,
            None => continue,
        };
        if !seen_ips.insert(dst.ip()) {
            continue;
        }
        let (req, txid) = build_request();
        if sock.send_to(&req, dst).await.is_ok() {
            pending.push((dst, txid));
        }
    }
    if pending.len() < 2 {
        return String::new();
    }
    let deadline = Instant::now() + wait;
    let mut ports: Vec<u16> = Vec::new();
    let mut buf = [0u8; 512];
    while ports.len() < 2 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let (n, from) = match timeout(left, sock.recv_from(&mut buf)).await {
            Ok(Ok(x)) => x,
            _ => break,
        };
        if let Some((_, txid)) = pending.iter().find(|(dst, _)| *dst == from) {
            if let Some(addr) = parse_response(&buf[..n], txid) {
                ports.push(addr.port());
            }
        }
    }
    if ports.len() < 2 {
        return String::new();
    }
    if ports[0] == ports[1] { "cone".into() } else { "symmetric".into() }
}

/// Голый STUN Binding Request — для NAT-keepalive (шлём и не ждём ответа, лишь
/// чтобы NAT не закрыл мэппинг порта). txid тут не нужен.
pub fn binding_request() -> Vec<u8> {
    build_request().0
}

fn build_request() -> (Vec<u8>, [u8; 12]) {
    let mut txid = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut txid);
    let mut msg = Vec::with_capacity(20);
    msg.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    msg.extend_from_slice(&0u16.to_be_bytes());
    msg.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    msg.extend_from_slice(&txid);
    (msg, txid)
}

fn parse_response(buf: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    if buf.len() < 20 {
        return None;
    }
    if u16::from_be_bytes([buf[0], buf[1]]) != BINDING_SUCCESS {
        return None;
    }
    if u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) != MAGIC_COOKIE {
        return None;
    }
    if &buf[8..20] != txid {
        return None;
    }

    let body_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let mut pos = 20;
    let end = (20 + body_len).min(buf.len());
    let mut fallback: Option<SocketAddr> = None;
    while pos + 4 <= end {
        let attr_type = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let attr_len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        let val_start = pos + 4;
        let val_end = val_start + attr_len;
        if val_end > buf.len() {
            break;
        }
        let val = &buf[val_start..val_end];
        match attr_type {
            ATTR_XOR_MAPPED_ADDRESS => {
                if let Some(addr) = parse_xor_mapped(val) {
                    return Some(addr);
                }
            }
            ATTR_MAPPED_ADDRESS if fallback.is_none() => fallback = parse_mapped(val),
            _ => {}
        }
        pos = val_end + ((4 - (attr_len % 4)) % 4);
    }
    fallback
}

fn parse_xor_mapped(val: &[u8]) -> Option<SocketAddr> {
    if val.len() < 8 || val[1] != 0x01 {
        return None;
    }
    let x_port = u16::from_be_bytes([val[2], val[3]]);
    let port = x_port ^ ((MAGIC_COOKIE >> 16) as u16);
    let magic = MAGIC_COOKIE.to_be_bytes();
    let ip = [
        val[4] ^ magic[0],
        val[5] ^ magic[1],
        val[6] ^ magic[2],
        val[7] ^ magic[3],
    ];
    Some(SocketAddr::from((ip, port)))
}

fn parse_mapped(val: &[u8]) -> Option<SocketAddr> {
    if val.len() < 8 || val[1] != 0x01 {
        return None;
    }
    let port = u16::from_be_bytes([val[2], val[3]]);
    let ip = [val[4], val[5], val[6], val[7]];
    Some(SocketAddr::from((ip, port)))
}
