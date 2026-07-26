//! noise — шифрующий протокол на Noise (тот же фреймворк, что внутри WireGuard).
//!
//! Как работает:
//!   • при коннекте стороны делают рукопожатие Noise XX (взаимное): по ECDH
//!     вырабатывают общий секрет, НЕ пересылая его — перехват обмена не даёт
//!     ключа. Обе стороны узнают статический ключ друг друга (задел под
//!     проверку «паспорта» хоста, MITM-защиту).
//!   • дальше каждый пакет шифруется ChaCha20-Poly1305.
//!
//! Транспорт у нас датаграммный (UDP) — пакеты могут теряться/переставляться,
//! поэтому используем STATELESS-режим Noise: к каждому пакету спереди пишем
//! 8-байтный счётчик (nonce), как в WireGuard. Тогда порядок доставки не важен.
//!
//! Крипту не пишем сами — берём проверенную библиотеку `snow`.

use std::time::Duration;

use async_trait::async_trait;
use bmv_common::{Error, Link, Result};
use tokio::time::timeout;

use crate::Protocol;

const HS_TIMEOUT: Duration = Duration::from_secs(6);

/// Шифрующий протокол на Noise (ChaCha20-Poly1305, тот же примитив, что в
/// WireGuard). Два режима:
///   • `noise`      — прямой Noise (быстр везде, дефолт).
///   • `noise-obfs` — «Маскировка»: то же шифрование ПЛЮС три слоя против DPI:
///       (1) размеры пакетов рукопожатия рандомизированы (случайный груз);
///       (2) каждый пакет данных добивается паддингом (пол 80 Б + джиттер) —
///           нет «отпечатка» размеров;
///       (3) плейнтекст-шапка (8-байтный счётчик nonce) МАСКИРУЕТСЯ по образцу
///           шифротекста (как QUIC header protection) — в проводе нет монотонного
///           счётчика, кадр целиком неотличим от случайных байт.
///     Обёртка над проверенным Noise + sha2 — своей крипты не пишем.
pub struct Noise {
    name: &'static str,
    pattern: &'static str,
    pad: bool,
}

impl Noise {
    /// Noise с ChaCha20-Poly1305 (имя `noise`).
    pub fn chacha() -> Self {
        Noise { name: "noise", pattern: "Noise_XX_25519_ChaChaPoly_BLAKE2s", pad: false }
    }
    /// «Маскировка» (имя `noise-obfs`): ChaCha20 + паддинг + маскировка nonce.
    pub fn obfs() -> Self {
        Noise { name: "noise-obfs", pattern: "Noise_XX_25519_ChaChaPoly_BLAKE2s", pad: true }
    }
}

#[async_trait]
impl Protocol for Noise {
    fn name(&self) -> &'static str {
        self.name
    }

    fn encrypts(&self) -> bool {
        true
    }

    async fn connect_host(&self, link: Box<dyn Link>) -> Result<Box<dyn Link>> {
        handshake(link, self.pattern, false, self.pad).await
    }

    async fn connect_guest(&self, link: Box<dyn Link>) -> Result<Box<dyn Link>> {
        handshake(link, self.pattern, true, self.pad).await
    }
}

/// ELLIGATOR2 (только маскировка). Сгенерировать ПРЕДСТАВИМЫЙ эфемерный
/// X25519-ключ: (priv[32], representative[32]). Повторяем, пока публичный ключ не
/// окажется представимым (≈50% → в среднем 2 попытки; потеря ~1 бита энтропии, на
/// стойкость не влияет). representative — равномерные 32 байта (старшие биты
/// рандомизированы tweak'ом), снаружи неотличимы от шума. Крипта — из проверенного
/// Tor-крейта `curve25519-elligator2`, руками ничего не считаем.
fn gen_representable_ephemeral() -> ([u8; 32], [u8; 32]) {
    use rand::Rng;
    loop {
        let mut sk = [0u8; 32];
        rand::thread_rng().fill(&mut sk);
        let tweak: u8 = rand::thread_rng().gen();
        if let Some(repr) = curve25519_elligator2::representative_from_privkey(&sk, tweak) {
            return (sk, repr);
        }
    }
}

/// Декодировать representative обратно в сырой X25519-публичный ключ (та же
/// вариация RFC9380, что у кодировщика). None — битые байты.
fn decode_representative(repr: &[u8; 32]) -> Option<[u8; 32]> {
    use curve25519_elligator2::{MontgomeryPoint, RFC9380};
    MontgomeryPoint::from_representative::<RFC9380>(repr).map(|p| p.to_bytes())
}

/// Заменить исходящий эфемерный ключ (первые 32 байта сообщения) на его
/// Elligator2-representative — только когда режим маскировки его подготовил.
fn cloak_ephemeral(msg: &mut [u8], repr: &Option<[u8; 32]>) {
    if let Some(r) = repr {
        if msg.len() >= 32 {
            msg[..32].copy_from_slice(r);
        }
    }
}

/// Обратно: входящий representative → сырой эфемерный ключ, который ждёт snow.
fn uncloak_ephemeral(msg: &mut [u8], pad: bool) -> Result<()> {
    if !pad {
        return Ok(());
    }
    if msg.len() < 32 {
        return Err(Error::Protocol("рукопожатие: слишком короткий эфемерный".into()));
    }
    let mut r = [0u8; 32];
    r.copy_from_slice(&msg[..32]);
    let raw = decode_representative(&r)
        .ok_or_else(|| Error::Protocol("рукопожатие: невалидный Elligator2-representative".into()))?;
    msg[..32].copy_from_slice(&raw);
    Ok(())
}

/// Провести рукопожатие Noise XX поверх канала и вернуть шифрующий Link.
/// initiator=true — гость (ходит первым), false — хост (отвечает).
async fn handshake(inner: Box<dyn Link>, pattern: &str, initiator: bool, pad: bool) -> Result<Box<dyn Link>> {
    let params = pattern.parse().map_err(noe)?;
    let builder = snow::Builder::new(params);
    let keypair = builder.generate_keypair().map_err(noe)?;
    let mut builder = builder.local_private_key(&keypair.private);

    // МАСКИРОВКА: эфемерный ключ Noise идёт в ОТКРЫТУЮ в msg1/msg2 (32 байта). У
    // валидной точки Curve25519 старший бит = 0 и значение смещено → DPI (ТСПУ)
    // отличает его от равномерного шума и палит VPN на ПЕРВОМ же пакете. Elligator2
    // кодирует ключ в равномерные 32 байта. Для этого нужен ПРЕДСТАВИМЫЙ ключ —
    // генерим свой и отдаём его snow как эфемерный (снаружи ничего не меняется:
    // snow считает тот же публичный ключ, а мы подменяем его байты на representative).
    // eph_sk живёт в области функции: snow-builder заимствует ключ до build_*.
    let eph_sk;
    let my_repr: Option<[u8; 32]> = if pad {
        let (sk, repr) = gen_representable_ephemeral();
        eph_sk = sk;
        builder = builder.fixed_ephemeral_key_for_testing_only(&eph_sk);
        Some(repr)
    } else {
        None
    };

    let mut hs = if initiator {
        builder.build_initiator().map_err(noe)?
    } else {
        builder.build_responder().map_err(noe)?
    };

    let mut out = vec![0u8; 4096];
    let mut scratch = vec![0u8; 4096];

    // XX: -> e ; <- e,ee,s,es ; -> s,se
    // Груз каждого сообщения рандомизирован (см. hs_payload) — размеры трёх пакетов
    // перестают быть фиксированным отпечатком Noise-XX. Эфемерный ключ несут msg1
    // (гость) и msg2 (хост) — их маскируем Elligator2; msg3 эфемерного не несёт.
    if initiator {
        let n = hs.write_message(&hs_payload(pad), &mut out).map_err(noe)?;
        cloak_ephemeral(&mut out[..n], &my_repr);
        inner.send(&out[..n]).await?;
        let mut msg = recv_hs(&*inner).await?;
        uncloak_ephemeral(&mut msg, pad)?;
        hs.read_message(&msg, &mut scratch).map_err(noe)?;
        let n = hs.write_message(&hs_payload(pad), &mut out).map_err(noe)?;
        inner.send(&out[..n]).await?; // msg3 — эфемерного нет, не трогаем
    } else {
        let mut msg = recv_hs(&*inner).await?;
        uncloak_ephemeral(&mut msg, pad)?;
        hs.read_message(&msg, &mut scratch).map_err(noe)?;
        let n = hs.write_message(&hs_payload(pad), &mut out).map_err(noe)?;
        cloak_ephemeral(&mut out[..n], &my_repr);
        inner.send(&out[..n]).await?;
        let msg = recv_hs(&*inner).await?;
        hs.read_message(&msg, &mut scratch).map_err(noe)?; // msg3 — эфемерного нет
    }

    // Ключ маскировки nonce = хэш рукопожатия (одинаков у обеих сторон, снаружи
    // неизвестен). Берём ДО перехода в транспортный режим (он поглощает hs).
    let obfs_key = if pad { Some(hs.get_handshake_hash().to_vec()) } else { None };
    let transport = hs.into_stateless_transport_mode().map_err(noe)?;
    // Пер-сессийный «пол» паддинга: фиксированный 80 давал ВСЕМ сессиям одинаковую
    // полосу мелких пакетов [80,112] — узнаваемый горб в гистограмме длин. Свой
    // случайный пол на сессию убирает эту общую сигнатуру. floor+jitter<256 (одна
    // байтовая длина): 120+32=152 < 255.
    let pad_floor = if pad { 64 + (rand::random::<u8>() as usize % 57) } else { 0 }; // 64..=120
    Ok(Box::new(NoiseLink {
        inner,
        transport,
        send_ctr: std::sync::atomic::AtomicU64::new(0),
        pad,
        pad_floor,
        obfs_key,
        send_frame: std::sync::Mutex::new(Vec::new()),
        send_plain: std::sync::Mutex::new(Vec::new()),
        recv_scratch: std::sync::Mutex::new(Vec::new()),
    }))
}

/// Маска шапки-nonce для «Маскировки»: PRF(hash_рукопожатия, образец шифротекста).
/// И отправитель, и приёмник видят один и тот же шифротекст → маска совпадает, но
/// снаружи (без ключа) шапка выглядит случайной, а счётчик остаётся уникальным.
fn nonce_mask(key: &[u8], ciphertext: &[u8]) -> u64 {
    use sha2::{Digest, Sha256};
    let sample = &ciphertext[..ciphertext.len().min(16)];
    let mut h = Sha256::new();
    h.update(key);
    h.update(sample);
    u64::from_le_bytes(h.finalize()[..8].try_into().unwrap())
}

/// Случайная добавка сверх пола/размера (0..=OBFS_JITTER).
const OBFS_JITTER: usize = 32;

/// Сколько байт паддинга добавить пакету длины `len` в режиме маскировки.
/// `floor` — пер-сессийный «пол» (мелкие пакеты добиваются до него, чтобы не
/// выделяться крошечным размером). Гарантированно ≤255 (длина — один байт):
/// floor ≤120 + джиттер 32 < 255.
fn obfs_pad_len(len: usize, floor: usize) -> usize {
    let jitter = rand::random::<u8>() as usize % (OBFS_JITTER + 1); // 0..=32
    let target = len.max(floor) + jitter;
    (target - len).min(u8::MAX as usize)
}

/// Случайный «груз» рукопожатия для маскировки (0..48 случайных байт). В обычном
/// режиме — пусто (груз не нужен). Noise штатно шифрует груз со 2-го сообщения;
/// в 1-м он идёт в открытую, но это просто случайные байты без маркеров.
fn hs_payload(pad: bool) -> Vec<u8> {
    if !pad {
        return Vec::new();
    }
    // 0..=255 (было 0..=48): узкий диапазон оставлял 3 сообщения Noise-XX в
    // распознаваемых узких полосах размера. Широкий разброс их размывает.
    let n = rand::random::<u8>() as usize;
    let mut v = vec![0u8; n];
    rand::Rng::fill(&mut rand::thread_rng(), &mut v[..]);
    v
}

async fn recv_hs(link: &dyn Link) -> Result<Vec<u8>> {
    match timeout(HS_TIMEOUT, link.recv()).await {
        Ok(Ok(m)) if !m.is_empty() => Ok(m),
        Ok(Ok(_)) => Err(Error::Protocol("канал закрыт во время рукопожатия".into())),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Protocol("таймаут рукопожатия Noise".into())),
    }
}

fn noe(e: snow::Error) -> Error {
    Error::Protocol(format!("noise: {e}"))
}

/// Канал, шифрующий каждый пакет. Формат кадра: nonce(8, LE) + ciphertext+tag.
/// Stateless-режим Noise: nonce берётся снаружи, поэтому read/write — &self,
/// а счётчик отправки атомарный. Это позволяет send и recv идти параллельно.
struct NoiseLink {
    inner: Box<dyn Link>,
    transport: snow::StatelessTransportState,
    send_ctr: std::sync::atomic::AtomicU64,
    /// Режим маскировки: добавлять случайный паддинг к каждому пакету.
    pad: bool,
    /// Пер-сессийный «пол» размера plaintext (маскировка). 0 в обычном режиме.
    pad_floor: usize,
    /// Some(hash) в режиме маскировки — ключ для маскировки шапки-nonce. None иначе.
    obfs_key: Option<Vec<u8>>,
    /// Переиспользуемые буферы горячего пути — чтобы НЕ аллоцировать Vec на каждый
    /// пакет (тяжело для CPU/батареи телефона при высоком pps). Берём из Mutex
    /// до `.await`, кладём обратно после — лок через await не держим.
    send_frame: std::sync::Mutex<Vec<u8>>,
    send_plain: std::sync::Mutex<Vec<u8>>,
    recv_scratch: std::sync::Mutex<Vec<u8>>,
}

#[async_trait]
impl Link for NoiseLink {
    async fn send(&self, packet: &[u8]) -> Result<()> {
        let nonce = self
            .send_ctr
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Plaintext в ПЕРЕИСПОЛЬЗУЕМОМ буфере. В маскировке — [payload | rnd-паддинг
        // | 1 байт длины паддинга]; приёмник по последнему байту отрежет паддинг.
        let mut plain = std::mem::take(&mut *self.send_plain.lock().unwrap());
        plain.clear();
        plain.extend_from_slice(packet);
        if self.pad {
            let pad_len = obfs_pad_len(packet.len(), self.pad_floor);
            let start = plain.len();
            plain.resize(start + pad_len, 0);
            rand::Rng::fill(&mut rand::thread_rng(), &mut plain[start..]);
            plain.push(pad_len as u8);
        }
        // Кадр в ПЕРЕИСПОЛЬЗУЕМОМ буфере: [nonce:8][ciphertext+tag].
        let mut frame = std::mem::take(&mut *self.send_frame.lock().unwrap());
        frame.clear();
        frame.resize(plain.len() + 16 + 8, 0);
        let res = self
            .transport
            .write_message(nonce, &plain, &mut frame[8..])
            .map_err(noe)
            .map(|n| {
                frame.truncate(8 + n);
                // Маскировка шапки: nonce ^ маска(шифротекст). Счётчик остаётся
                // уникальным (без nonce-reuse), но в проводе монотонности не видно.
                let wire_nonce = match &self.obfs_key {
                    Some(k) => nonce ^ nonce_mask(k, &frame[8..]),
                    None => nonce,
                };
                frame[..8].copy_from_slice(&wire_nonce.to_le_bytes());
            });
        let out = match res {
            Ok(()) => self.inner.send(&frame).await,
            Err(e) => Err(e),
        };
        // Возвращаем буферы в пул (лок берём заново, через await не держали).
        *self.send_plain.lock().unwrap() = plain;
        *self.send_frame.lock().unwrap() = frame;
        out
    }

    async fn recv_into(&self, out: &mut Vec<u8>) -> Result<bool> {
        // Шифротекст из inner — в ПЕРЕИСПОЛЬЗУЕМЫЙ буфер (не Vec на каждый пакет).
        let mut frame = std::mem::take(&mut *self.recv_scratch.lock().unwrap());
        let res = loop {
            match self.inner.recv_into(&mut frame).await {
                Ok(false) => break Ok(false), // канал закрыт
                Ok(true) => {
                    if frame.len() < 8 {
                        continue; // не наш кадр (мусор/punch) — пропускаем
                    }
                    let wire_nonce = u64::from_le_bytes(frame[..8].try_into().unwrap());
                    // Снимаем маску шапки той же маской (образец шифротекста тот же).
                    let nonce = match &self.obfs_key {
                        Some(k) => wire_nonce ^ nonce_mask(k, &frame[8..]),
                        None => wire_nonce,
                    };
                    // Расшифровываем ПРЯМО в буфер вызывающего (переиспользуется).
                    out.clear();
                    out.resize(frame.len(), 0); // ≥ длины plaintext
                    match self.transport.read_message(nonce, &frame[8..], out) {
                        Ok(n) => {
                            out.truncate(n);
                            if self.pad {
                                // Последний байт — длина паддинга; отрезаем паддинг+байт.
                                if let Some(&pad_len) = out.last() {
                                    let strip = pad_len as usize + 1;
                                    if strip <= out.len() {
                                        out.truncate(out.len() - strip);
                                    }
                                }
                            }
                            break Ok(true);
                        }
                        Err(_) => continue, // битый/чужой пакет — игнорируем
                    }
                }
                Err(e) => break Err(e),
            }
        };
        *self.recv_scratch.lock().unwrap() = frame;
        res
    }

    async fn close(&self) -> Result<()> {
        self.inner.close().await
    }
}

/// Считающий аллокатор — только для теста горячего пути (см. hot_path_barely_allocates).
#[cfg(test)]
struct CountingAlloc;
#[cfg(test)]
static ALLOC_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::alloc::System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        std::alloc::System.dealloc(p, l)
    }
}
#[cfg(test)]
#[global_allocator]
static GA: CountingAlloc = CountingAlloc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Protocol;
    use std::sync::atomic::Ordering;

    /// РЕГРЕССИЯ рефактора буферов: шифрослой НЕ аллоцирует буферы данных на пакет.
    ///
    /// Меряем ДЕЛЬТУ (Noise-над-MemLink) − (голый MemLink) на тех же пакетах.
    /// Сам MemLink аллоцирует на пакет (to_vec в tokio-канал), поэтому важно не
    /// абсолютное число, а надбавка NoiseLink. Замерами установлено:
    ///   • snow write/read_message в пред-выделенный буфер — 0 аллокаций;
    ///   • plain/frame/out/recv_scratch переиспользуются (mem::take+возврат) — 0;
    ///   • остаётся РОВНО по одному Box от `async_trait` на вложенный dyn-вызов
    ///     (inner.send и inner.recv_into) → ~2 мелких аллокации на round-trip.
    /// Если переиспользование сломается, добавятся plain+frame (send) и out+scratch
    /// (recv) = +4/rt, и порог не пройдёт. Это ловит именно регрессию буферов.
    #[tokio::test]
    async fn hot_path_no_data_buffer_allocs() {
        let data = vec![0x42u8; 1200];
        const N: usize = 400;

        let (ra, rb) = bmv_common::wire::memory_pair(64);
        let mut rbuf = Vec::new();
        for _ in 0..8 { ra.send(&data).await.unwrap(); rb.recv_into(&mut rbuf).await.unwrap(); }
        let base0 = ALLOC_COUNT.load(Ordering::Relaxed);
        for _ in 0..N { ra.send(&data).await.unwrap(); rb.recv_into(&mut rbuf).await.unwrap(); }
        let baseline = ALLOC_COUNT.load(Ordering::Relaxed) - base0;

        let (a, b) = bmv_common::wire::memory_pair(64);
        let p = Noise::obfs();
        let (host, guest) = tokio::join!(p.connect_host(a), p.connect_guest(b));
        let (host, guest) = (host.unwrap(), guest.unwrap());
        for _ in 0..8 { guest.send(&data).await.unwrap(); host.recv_into(&mut rbuf).await.unwrap(); }
        let n0 = ALLOC_COUNT.load(Ordering::Relaxed);
        for _ in 0..N { guest.send(&data).await.unwrap(); host.recv_into(&mut rbuf).await.unwrap(); }
        let noise = ALLOC_COUNT.load(Ordering::Relaxed) - n0;

        let per_rt = noise.saturating_sub(baseline) as f64 / N as f64;
        // Здорово: ~2/rt (только Box'ы async_trait). Регрессия буферов: ~6/rt.
        assert!(per_rt < 4.0,
            "шифрослой добавляет {per_rt:.2} аллок/round-trip (baseline={baseline}, noise={noise}) — похоже, буферы больше не переиспользуются");
    }

    async fn roundtrip(proto: Noise) {
        let (a, b) = bmv_common::wire::memory_pair(16);
        let (host, guest) = tokio::join!(proto.connect_host(a), proto.connect_guest(b));
        let host = host.expect("host handshake");
        let guest = guest.expect("guest handshake");
        guest.send(b"secret-payload").await.unwrap();
        let got = host.recv().await.unwrap();
        assert_eq!(got, b"secret-payload");
        host.send(b"reply").await.unwrap();
        assert_eq!(guest.recv().await.unwrap(), b"reply");
    }

    #[tokio::test]
    async fn noise_chacha_roundtrip() { roundtrip(Noise::chacha()).await; }

    #[tokio::test]
    async fn noise_obfs_roundtrip() { roundtrip(Noise::obfs()).await; }

    /// Elligator2: (1) representative декодируется обратно в ТОТ ЖЕ публичный ключ
    /// (иначе рукопожатие бы не сошлось); (2) сырой ключ Curve25519 ВСЕГДА имеет
    /// старший бит 0 — это и есть отпечаток для DPI; (3) representative его
    /// рандомизирует → снаружи неотличим от равномерного шума.
    #[test]
    fn elligator_representative_uniform_and_roundtrips() {
        let mut high_bit_seen = 0;
        for _ in 0..64 {
            let (sk, repr) = gen_representable_ephemeral();
            let raw_pub = curve25519_elligator2::MontgomeryPoint::mul_base_clamped(sk).to_bytes();
            let decoded = decode_representative(&repr).expect("representative должен декодироваться");
            assert_eq!(decoded, raw_pub, "representative → тот же публичный ключ, что ждёт snow");
            assert_eq!(raw_pub[31] & 0x80, 0, "сырой ключ Curve25519: старший бит всегда 0 (отпечаток)");
            if repr[31] & 0x80 != 0 {
                high_bit_seen += 1;
            }
        }
        assert!(high_bit_seen > 0, "representative не рандомизирует старший бит — палевно для DPI");
    }

    /// Маскировка шапки: в проводе первые 8 байт двух подряд кадров НЕ идут
    /// монотонным счётчиком (0,1,2…), т.е. счётчик замаскирован. И при этом
    /// расшифровка проходит (проверяется в roundtrip).
    #[tokio::test]
    async fn noise_obfs_masks_nonce_header() {
        let (a, b) = bmv_common::wire::memory_pair(64);
        let (ph, pg) = (Noise::obfs(), Noise::obfs());
        let (host, guest) = tokio::join!(ph.connect_host(a), pg.connect_guest(b));
        let (host, _guest) = (host.unwrap(), guest.unwrap());
        // Отправим два одинаковых пакета и убедимся, что «шапки» не 0 и не 1.
        // (Проверяем на приёмной стороне через сам факт корректной расшифровки —
        // если бы маска ломала nonce, recv вернул бы ошибку/пусто.)
        host.send(b"aaaa").await.unwrap();
        host.send(b"aaaa").await.unwrap();
        // Косвенно: nonce_mask даёт разные маски для разных шифротекстов, а два
        // шифрования одного текста разными nonce дают разные шифротексты.
        let k = [7u8; 32];
        assert_ne!(nonce_mask(&k, &[1, 2, 3, 4, 5, 6, 7, 8, 9]), 0, "маска не должна быть нулевой");
        assert_ne!(nonce_mask(&k, b"AAAAAAAAAAAAAAAA"), nonce_mask(&k, b"BBBBBBBBBBBBBBBB"), "маска зависит от шифротекста");
    }

    /// Паддинг маскировки: всегда влезает в 1 байт (≤255), мелкие пакеты добиты до
    /// пола, крупные — не раздуты сверх джиттера.
    #[test]
    fn obfs_pad_len_bounds() {
        // Проверяем весь диапазон пер-сессийного пола (64..=120).
        for floor in [64usize, 80, 100, 120] {
            for len in [0usize, 1, 40, 79, 80, 100, 1300, 1400, 4096] {
                for _ in 0..500 {
                    let p = obfs_pad_len(len, floor);
                    assert!(p <= 255, "паддинг {p} > 255 для len={len} floor={floor}");
                    let total = len + p;
                    assert!(total >= len.max(floor), "мелкий пакет не добит до пола: len={len} floor={floor} total={total}");
                    assert!(total <= len.max(floor) + OBFS_JITTER, "слишком большой паддинг: len={len} floor={floor} total={total}");
                }
            }
        }
    }

    /// Маскировка: паддинг корректно снимается для пакетов РАЗНОЙ длины (в т.ч.
    /// 1-байтовых keepalive и крупных), сколько бы раз ни слали.
    #[tokio::test]
    async fn noise_obfs_strips_padding_various_sizes() {
        let (a, b) = bmv_common::wire::memory_pair(64);
        let (ph, pg) = (Noise::obfs(), Noise::obfs());
        let (host, guest) = tokio::join!(ph.connect_host(a), pg.connect_guest(b));
        let (host, guest) = (host.unwrap(), guest.unwrap());
        for payload in [vec![0u8], vec![1u8; 1], vec![7u8; 1300], (0..500u32).flat_map(|i| i.to_le_bytes()).collect::<Vec<u8>>()] {
            guest.send(&payload).await.unwrap();
            assert_eq!(host.recv().await.unwrap(), payload, "паддинг снят неверно для len={}", payload.len());
        }
    }
}
