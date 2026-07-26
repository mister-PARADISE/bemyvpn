//! Страна/флаг по IP — ЛОКАЛЬНО (порт GeoFlags.kt с Android). База ip2cc.dat
//! (gzip, формат BMV2: "BMV2" + n:u32 + deltas[n] + lens[n] + cc[n]×2 ASCII, BE).
//! start[i]=end[i-1]+delta (wrap u32), end[i]=start[i]+len. Поиск — двоичный по
//! start (беззнаково). Грузится один раз лениво; не доверяем самоотчёту хоста.
use std::sync::OnceLock;

struct Db {
    starts: Vec<u32>,
    ends: Vec<u32>,
    cc: Vec<u8>,
}

static DB: OnceLock<Option<Db>> = OnceLock::new();

fn db() -> Option<&'static Db> {
    DB.get_or_init(load).as_ref()
}

fn load() -> Option<Db> {
    use std::io::Read;
    let gz: &[u8] = include_bytes!("../data/ip2cc.dat");
    let mut dec = flate2::read::GzDecoder::new(gz);
    let mut raw = Vec::new();
    dec.read_to_end(&mut raw).ok()?;

    let mut p = 0usize;
    let mut rd = |raw: &[u8]| -> u32 {
        let v = u32::from_be_bytes([raw[p], raw[p + 1], raw[p + 2], raw[p + 3]]);
        p += 4;
        v
    };
    if rd(&raw) != 0x424D_5632 {
        return None; // "BMV2"
    }
    let n = rd(&raw) as usize;
    let deltas: Vec<u32> = (0..n).map(|_| rd(&raw)).collect();
    let lens: Vec<u32> = (0..n).map(|_| rd(&raw)).collect();
    let mut starts = Vec::with_capacity(n);
    let mut ends = Vec::with_capacity(n);
    let mut prev_end: u32 = 0;
    for i in 0..n {
        let s = prev_end.wrapping_add(deltas[i]);
        let e = s.wrapping_add(lens[i]);
        starts.push(s);
        ends.push(e);
        prev_end = e;
    }
    let cc = raw.get(p..p + n * 2)?.to_vec();
    Some(Db { starts, ends, cc })
}

/// Код страны (ISO-2, верхний регистр) по IPv4 «a.b.c.d», либо None.
pub fn country_of(ip: &str) -> Option<String> {
    let db = db()?;
    let key = ipv4_to_u32(ip)?;
    // последняя запись со start <= key (u32 сравнивается беззнаково)
    let (mut lo, mut hi) = (0usize, db.starts.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if db.starts[mid] <= key {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == 0 {
        return None;
    }
    let i = lo - 1;
    if key < db.starts[i] || key > db.ends[i] {
        return None;
    }
    Some(format!("{}{}", db.cc[i * 2] as char, db.cc[i * 2 + 1] as char))
}

fn ipv4_to_u32(ip: &str) -> Option<u32> {
    let parts: Vec<&str> = ip.trim().split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut v: u32 = 0;
    for s in parts {
        let o: u32 = s.parse().ok()?;
        if o > 255 {
            return None;
        }
        v = (v << 8) | o;
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ips_resolve() {
        // Порт формата верен, если известные IP дают ожидаемую страну.
        assert_eq!(country_of("8.8.8.8").as_deref(), Some("US"));
        assert_eq!(country_of("77.88.8.8").as_deref(), Some("RU")); // Яндекс DNS
        assert!(country_of("10.0.0.1").is_none()); // приватный — нет в базе
        assert!(country_of("bad").is_none());
    }
}
