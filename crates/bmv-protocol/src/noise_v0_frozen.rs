//! ЗАМОРОЖЕННАЯ ПРЕДЫДУЩАЯ ВЕРСИЯ `noise` — эталон ПРОВОДА (только тесты).
//!
//! ЗАЧЕМ. Наши тесты гоняют новый код против нового: они докажут, что реализация
//! самосогласована, и не заметят ни одного изменения формата кадра. А в реальном
//! мире гость обновился, а хост — нет (и наоборот): чужая машина месяцами живёт
//! на старой сборке. Разойдись формат — рукопожатие просто не сойдётся, и
//! человек увидит вечное «Подключаюсь…» вместо ошибки. Поймать это можно ровно
//! одним способом: держать копию СТАРОГО кода и сводить её с новым.
//!
//! ЧТО ЭТО ЗА КОД. Дословная копия `crates/bmv-protocol/src/noise.rs` из коммита
//! перед «Полный разбор кода» (ff3e07e~1), без блока `mod tests` и без
//! счётчика аллокаций. МЕНЯТЬ ЕЁ НЕЛЬЗЯ — она описывает то, что уже выпущено в
//! мир. Когда провод изменится СОЗНАТЕЛЬНО (новая версия формата), сюда кладётся
//! новый снимок, а старый остаётся, пока в природе есть его носители.
//!
//! Модуль собирается только под `cfg(test)`, в бинарь не попадает.
#![allow(dead_code)]

use std::time::Duration;

use async_trait::async_trait;
use bmv_common::{Error, Link, Result};
use tokio::time::timeout;

use crate::Protocol;

const HS_TIMEOUT: Duration = Duration::from_secs(6);
/// Сколько ждать ответа, прежде чем ПОСЛАТЬ СВОЁ СООБЩЕНИЕ ЗАНОВО.
///
/// Рукопожатие Noise — ровно три сообщения, и собственных повторов у него нет.
/// А идут они по UDP: один потерянный пакет = сорванное подключение и «не удалось
/// подключиться» на ровном месте. На обычной линии это единицы процентов попыток,
/// на мобильной — куда больше. Пробитие NAT перед этим уже доказало, что путь
/// рабочий, поэтому 500мс — щедрая оценка круга.
const HS_RETRY: Duration = Duration::from_millis(500);
/// Сколько раз повторяем. 4 × 500мс = 2с сверху в худшем случае — укладывается
/// в бюджет подключения и делает потерю пакета почти безобидной.
const HS_TRIES: usize = 4;

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
        // Шлём msg1 и ждём msg2, повторяя msg1 при тишине (потерю не отличить от
        // задумчивости — но повтор безвреден: хост узнаёт его побайтово).
        let mut msg = send_and_await(&*inner, &out[..n]).await?;
        uncloak_ephemeral(&mut msg, pad)?;
        hs.read_message(&msg, &mut scratch).map_err(noe)?;
        let n = hs.write_message(&hs_payload(pad), &mut out).map_err(noe)?;
        // msg3 — эфемерного нет, не трогаем. Ответа на него не будет, поэтому
        // ждать нечего; шлём ДВАЖДЫ. Потеря именно msg3 — худший случай: мы уже
        // считаем себя подключёнными, а хост ещё ждёт и через таймаут уходит.
        // Второй экземпляр стоит один пакет, а хост отбросит его как мусор.
        inner.send(&out[..n]).await?;
        let _ = inner.send(&out[..n]).await;
    } else {
        let mut msg1 = recv_hs(&*inner).await?;
        let msg1_raw = msg1.clone(); // для опознания повтора (см. ниже)
        uncloak_ephemeral(&mut msg1, pad)?;
        hs.read_message(&msg1, &mut scratch).map_err(noe)?;
        let n = hs.write_message(&hs_payload(pad), &mut out).map_err(noe)?;
        cloak_ephemeral(&mut out[..n], &my_repr);
        let msg2 = out[..n].to_vec();
        inner.send(&msg2).await?;

        // Ждём msg3. Пришёл ПОВТОР msg1 — значит гость не увидел нашего msg2:
        // шлём msg2 ещё раз. Тишина — тоже шлём ещё раз (мог потеряться он сам).
        //
        // Повтор msg1 обязательно опознать ПОБАЙТОВО и не отдавать в snow:
        // `read_message` подмешивает данные в хэш ДО проверки пломбы, поэтому
        // «попробовать и не получилось» ломает состояние рукопожатия навсегда —
        // после такого не примется уже и правильный msg3.
        let mut done = false;
        for _ in 0..HS_TRIES {
            match timeout(HS_RETRY, inner.recv()).await {
                Ok(Ok(m)) if m.is_empty() => {
                    return Err(Error::Protocol("канал закрыт во время рукопожатия".into()))
                }
                Ok(Ok(m)) if m == msg1_raw => {
                    inner.send(&msg2).await?; // гость не получил msg2 — повторяем
                }
                Ok(Ok(m)) => {
                    hs.read_message(&m, &mut scratch).map_err(noe)?;
                    done = true;
                    break;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => inner.send(&msg2).await?, // тишина — msg2 мог не дойти
            }
        }
        if !done {
            return Err(Error::Protocol("таймаут рукопожатия Noise".into()));
        }
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
        replay: std::sync::Mutex::new((0, 0)),
    }))
}

/// Маска шапки-nonce для «Маскировки»: PRF(hash_рукопожатия, образец шифротекста).
/// И отправитель, и приёмник видят один и тот же шифротекст → маска совпадает, но
/// снаружи (без ключа) шапка выглядит случайной, а счётчик остаётся уникальным.
// pub(crate) — единственное отступление от дословности: тест сверяет маску
// напрямую со старой формулой (см. `nonce_mask_matches_the_previous_version`).
pub(crate) fn nonce_mask(key: &[u8], ciphertext: &[u8]) -> u64 {
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

/// Послать сообщение рукопожатия и дождаться ответа, ПОВТОРЯЯ при тишине.
///
/// Повтор безопасен: получатель сравнивает байты и узнаёт дубликат, а в snow его
/// не отдаёт (см. ветку хоста в `handshake`). Общий потолок ожидания тот же
/// HS_TIMEOUT — повторы его не удлиняют, а лишь заполняют полезной работой.
async fn send_and_await(link: &dyn Link, msg: &[u8]) -> Result<Vec<u8>> {
    let deadline = tokio::time::Instant::now() + HS_TIMEOUT;
    for _ in 0..HS_TRIES {
        link.send(msg).await?;
        let wait = HS_RETRY.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        match timeout(wait, link.recv()).await {
            Ok(Ok(m)) if !m.is_empty() => return Ok(m),
            Ok(Ok(_)) => return Err(Error::Protocol("канал закрыт во время рукопожатия".into())),
            Ok(Err(e)) => return Err(e),
            Err(_) if tokio::time::Instant::now() < deadline => continue, // тишина — шлём заново
            Err(_) => break,
        }
    }
    Err(Error::Protocol("таймаут рукопожатия Noise".into()))
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
    /// Анти-повтор: (наибольший принятый nonce, маска 64 предыдущих). См. `replay_ok`.
    replay: std::sync::Mutex<(u64, u64)>,
}

/// Ширина окна анти-повтора (как в IPsec) — сколько перестановок в сети терпим.
const REPLAY_WINDOW: u64 = 64;

/// Учесть nonce УЖЕ РАСШИФРОВАННОГО пакета; `false` — это повтор, пакет дропаем.
///
/// Зачем: stateless-режим Noise берёт nonce из провода и сам повторы не ловит.
/// Значит перехваченный пакет можно послать заново — и это не теория: `BYE`
/// (прощание) тоже шифрованный пакет, его повтор РВЁТ живую сессию. Плюс
/// дублированные TCP-сегменты внутри туннеля бьют по стеку гостя.
///
/// Схема стандартная (RFC 4303 anti-replay): помним наибольший принятый nonce и
/// битовую маску 64 предыдущих. Окно нужно потому, что UDP переставляет пакеты, и
/// требовать строгого возрастания значило бы терять законный трафик.
fn replay_ok(state: &mut (u64, u64), nonce: u64) -> bool {
    let (last, mask) = *state;
    if nonce > last {
        // Свежий: сдвигаем окно вперёд, младший бит = только что принятый.
        let shift = nonce - last;
        *state = (nonce, if shift >= REPLAY_WINDOW { 1 } else { (mask << shift) | 1 });
        return true;
    }
    let back = last - nonce;
    // Старше окна — судить не можем, поэтому отвергаем (иначе повтор пролезал бы).
    if back >= REPLAY_WINDOW {
        return false;
    }
    let bit = 1u64 << back;
    if mask & bit != 0 {
        return false; // такой nonce уже принимали
    }
    *state = (last, mask | bit);
    true
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
                            // Окно двигаем ТОЛЬКО после успешной расшифровки: иначе
                            // подделанным nonce можно было бы «состарить» окно и
                            // выбить из него законные пакеты.
                            if !replay_ok(&mut self.replay.lock().unwrap(), nonce) {
                                continue; // повтор — молча дропаем
                            }
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
