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

// Адрес из ответа STUN уезжает в каталог, и все гости пойдут пробивать NAT
// именно туда, — а сам ответ приходит по UDP от ЧУЖОЙ машины, то есть
// подделывается кем угодно, кто угадал наш порт. Полностью проверить «наш ли это
// адрес» нельзя (в том и смысл STUN — мы его сами не знаем), но заведомо
// невозможные варианты режет общая таблица диапазонов (`crate::reach`).
use crate::reach::plausible_external;

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
        return Err(Error::Net("Не удаётся узнать свой адрес в интернете. Проверьте подключение.".into()));
    }

    // Ждём первый ПОДХОДЯЩИЙ ответ (совпал источник и txid) до общего дедлайна.
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 512];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Net("Сеть не отвечает — не удаётся узнать свой адрес в интернете. Проверьте подключение.".into()));
        }
        let (n, from) = match timeout(left, sock.recv_from(&mut buf)).await {
            Ok(r) => r?,
            Err(_) => return Err(Error::Net("Сеть не отвечает — не удаётся узнать свой адрес в интернете. Проверьте подключение.".into())),
        };
        // Ищем отправителя среди опрошенных и проверяем его txid.
        if let Some((_, txid)) = pending.iter().find(|(dst, _)| *dst == from) {
            if let Some(addr) = parse_response(&buf[..n], txid) {
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
        // Граница — по ОБЪЯВЛЕННОЙ длине тела, а не по размеру буфера. Иначе за
        // концом тела можно дописать «невидимый» атрибут, и разбор возьмёт адрес
        // из него (заявленное и фактическое содержимое пакета разошлись бы).
        if val_end > end {
            break;
        }
        let val = &buf[val_start..val_end];
        match attr_type {
            ATTR_XOR_MAPPED_ADDRESS => {
                if let Some(addr) = parse_xor_mapped(val).filter(plausible_external) {
                    return Some(addr);
                }
            }
            ATTR_MAPPED_ADDRESS if fallback.is_none() => {
                fallback = parse_mapped(val).filter(plausible_external)
            }
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

// ── КАТЕГОРИЯ Ж: враждебный ответ STUN ────────────────────────────────────────
//
// STUN-серверы — ЧУЖИЕ машины (Google, Cloudflare), и ответ приходит по UDP,
// то есть подделать его может кто угодно, кто угадает наш порт. Разбор этого
// ответа — первый чужой байт, который трогает программа при запуске. Он обязан
// пережить любой мусор: не упасть по срезу, не зациклиться, не выдать мусорный
// адрес за наш внешний.
#[cfg(test)]
mod tests {
    use super::*;

    /// Собрать ответ STUN: заголовок + произвольные байты тела.
    fn resp(txid: &[u8; 12], body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
        v.extend_from_slice(&(body.len() as u16).to_be_bytes());
        v.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        v.extend_from_slice(txid);
        v.extend_from_slice(body);
        v
    }

    /// Атрибут с длиной больше, чем осталось байт. Классика: разбор верит полю
    /// длины и режет срез за концом буфера — это паника, то есть падение
    /// приложения от одного чужого пакета.
    #[test]
    fn oversized_attribute_length_does_not_panic() {
        let txid = [7u8; 12];
        for claimed in [0xFFFFu16, 0x8000, 64, 21] {
            let mut body = Vec::new();
            body.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
            body.extend_from_slice(&claimed.to_be_bytes());
            body.extend_from_slice(&[0u8; 4]); // тела заведомо меньше заявленного
            assert_eq!(parse_response(&resp(&txid, &body), &txid), None,
                "заявленная длина {claimed} принята за настоящую");
        }
    }

    /// Атрибуты нулевой длины подряд. Если разбор не сдвигает позицию хотя бы на
    /// заголовок атрибута, цикл крутится вечно и поток встаёт навсегда — а тест
    /// на зависание выглядит как «тест не закончился».
    #[test]
    fn zero_length_attributes_terminate() {
        let txid = [9u8; 12];
        let mut body = Vec::new();
        for _ in 0..500 {
            body.extend_from_slice(&0xDEADu16.to_be_bytes()); // неизвестный тип
            body.extend_from_slice(&0u16.to_be_bytes());      // длина 0
        }
        assert_eq!(parse_response(&resp(&txid, &body), &txid), None);
    }

    /// Ответ с ЧУЖИМ идентификатором транзакции. Иначе поддельный пакет от
    /// постороннего выдал бы нам не наш внешний адрес — а он идёт в каталог, и
    /// гости пошли бы пробивать NAT к указанной жертве.
    #[test]
    fn foreign_transaction_id_is_rejected() {
        let mine = [1u8; 12];
        let theirs = [2u8; 12];
        let mut body = Vec::new();
        body.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        body.extend_from_slice(&8u16.to_be_bytes());
        body.push(0);
        body.push(0x01); // IPv4
        body.extend_from_slice(&[0x11, 0x22]);
        body.extend_from_slice(&[0x33, 0x44, 0x55, 0x66]);
        assert!(parse_response(&resp(&mine, &body), &mine).is_some(), "свой ответ обязан разбираться");
        assert_eq!(parse_response(&resp(&theirs, &body), &mine), None, "чужой ответ принят за свой");
    }

    /// Обрезки и мусор: пустой пакет, половина заголовка, ответ не того типа,
    /// подделанный magic cookie. Ни один не должен ронять разбор.
    #[test]
    fn truncated_and_bogus_responses_are_rejected() {
        let txid = [3u8; 12];
        assert_eq!(parse_response(&[], &txid), None);
        for n in 1..20 {
            assert_eq!(parse_response(&vec![0u8; n], &txid), None, "обрезка {n} байт");
        }
        // Правильный размер, но не тот тип сообщения.
        let mut wrong = resp(&txid, &[]);
        wrong[0..2].copy_from_slice(&0x0111u16.to_be_bytes()); // binding error
        assert_eq!(parse_response(&wrong, &txid), None);
        // Правильный тип, но чужой magic cookie.
        let mut bad_cookie = resp(&txid, &[]);
        bad_cookie[4..8].copy_from_slice(&0xDEADBEEFu32.to_be_bytes());
        assert_eq!(parse_response(&bad_cookie, &txid), None);
    }

    /// Собрать тело с одним XOR-MAPPED-ADDRESS на заданный адрес.
    fn xor_body(ip: [u8; 4], port: u16) -> Vec<u8> {
        let magic = MAGIC_COOKIE.to_be_bytes();
        let mut body = Vec::new();
        body.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        body.extend_from_slice(&8u16.to_be_bytes());
        body.push(0);
        body.push(0x01); // IPv4
        body.extend_from_slice(&(port ^ ((MAGIC_COOKIE >> 16) as u16)).to_be_bytes());
        for i in 0..4 {
            body.push(ip[i] ^ magic[i]);
        }
        body
    }

    /// ВРАЖДЕБНЫЙ STUN НАЗЫВАЕТ НЕ ТОТ АДРЕС. Ответ приходит по UDP от ЧУЖОЙ
    /// машины, а полученный адрес мы анонсируем в каталог — и все гости пойдут
    /// пробивать NAT туда. Значит бессмысленный «внешний» адрес принимать нельзя:
    /// петля/приватка/мультикаст/0.0.0.0 внешними не бывают никогда.
    #[test]
    fn implausible_external_address_is_rejected() {
        let txid = [4u8; 12];
        for ip in [
            [127, 0, 0, 1],   // петля
            [10, 0, 0, 5],    // LAN
            [192, 168, 1, 1], // роутер
            [172, 16, 0, 1],
            [169, 254, 169, 254], // link-local / метаданные облака
            [224, 0, 0, 1],       // мультикаст
            [255, 255, 255, 255], // бродкаст
            [0, 0, 0, 0],
            [0, 1, 2, 3], // 0.0.0.0/8
        ] {
            assert_eq!(
                parse_response(&resp(&txid, &xor_body(ip, 40000)), &txid),
                None,
                "принят невозможный внешний адрес {ip:?} — он уедет в каталог"
            );
        }
        // Нулевой порт — тоже не адрес: пробиваться туда некуда.
        assert_eq!(parse_response(&resp(&txid, &xor_body([8, 8, 8, 8], 0)), &txid), None);
        // А нормальный публичный адрес обязан проходить.
        assert!(parse_response(&resp(&txid, &xor_body([45, 11, 22, 33], 40000)), &txid).is_some());
    }

    /// ИЗ РАЗБОРА STUN НИКОГДА НЕ ВЫХОДИТ АДРЕС IPv6.
    ///
    /// Это не мелочь, а причина, по которой у проверки адреса нет отдельной ветки
    /// про IPv6: семейство `0x02` (IPv6) отбрасывается ЗДЕСЬ, в разборе, — и
    /// раньше на этом месте лежала вторая таблица диапазонов «на случай IPv6»,
    /// которая ничего не решала, но успела разойтись с первой. Если однажды сюда
    /// добавят разбор IPv6, этот тест покраснеет и напомнит проверить политику.
    #[test]
    fn an_ipv6_stun_answer_is_never_parsed() {
        let txid = [8u8; 12];
        for attr in [ATTR_XOR_MAPPED_ADDRESS, ATTR_MAPPED_ADDRESS] {
            let mut body = Vec::new();
            body.extend_from_slice(&attr.to_be_bytes());
            body.extend_from_slice(&20u16.to_be_bytes());
            body.push(0);
            body.push(0x02); // семейство IPv6
            body.extend_from_slice(&[0x9c, 0x40]); // порт
            body.extend_from_slice(&[0x20, 0x01, 0x0d, 0xb8]); // 2001:db8::1 — публичный
            body.extend_from_slice(&[0u8; 11]);
            body.push(1);
            assert_eq!(
                parse_response(&resp(&txid, &body), &txid),
                None,
                "разбор STUN выдал адрес IPv6 — политика адресов его не рассматривает"
            );
        }
    }

    /// Атрибут вылезает ЗА объявленную длину тела. Граница проверялась по размеру
    /// буфера, а не по слову отправителя, — значит в пакет можно было дописать
    /// «невидимый» атрибут после конца тела, и разбор брал адрес из него.
    #[test]
    fn attribute_past_declared_body_is_ignored() {
        let txid = [6u8; 12];
        let body = xor_body([45, 11, 22, 33], 40000);
        let mut v = resp(&txid, &body);
        // Говорим «тела всего 4 байта» (только заголовок атрибута), значение —
        // уже за границей тела.
        v[2..4].copy_from_slice(&4u16.to_be_bytes());
        assert_eq!(parse_response(&v, &txid), None, "атрибут за концом тела принят");
    }

    /// Поле длины тела врёт в БОЛЬШУЮ сторону (тела столько нет). Разбор обязан
    /// смотреть на реальный размер буфера, а не на слова отправителя.
    #[test]
    fn body_length_larger_than_packet_is_clamped() {
        let txid = [5u8; 12];
        let mut v = resp(&txid, &[0u8; 4]);
        v[2..4].copy_from_slice(&4096u16.to_be_bytes()); // «тела 4 КБ», а его 4 байта
        assert_eq!(parse_response(&v, &txid), None);
    }
}
