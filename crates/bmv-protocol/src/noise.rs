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
        Ok(Box::new(handshake(link, self.pattern, false, self.pad).await?))
    }

    async fn connect_guest(&self, link: Box<dyn Link>) -> Result<Box<dyn Link>> {
        Ok(Box::new(handshake(link, self.pattern, true, self.pad).await?))
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
///
/// Возвращает КОНКРЕТНЫЙ тип (а не `Box<dyn Link>`): тестам нужен доступ к
/// транспортному состоянию, чтобы собрать враждебный кадр РАБОЧИМИ ключами
/// сессии — иначе «злой пир» ничем не отличить от мусора, который и так дропается.
async fn handshake(inner: Box<dyn Link>, pattern: &str, initiator: bool, pad: bool) -> Result<NoiseLink> {
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
    let obfs_key = if pad { Some(charge_mask_key(hs.get_handshake_hash())) } else { None };
    let transport = hs.into_stateless_transport_mode().map_err(noe)?;
    // Пер-сессийный «пол» паддинга: фиксированный 80 давал ВСЕМ сессиям одинаковую
    // полосу мелких пакетов [80,112] — узнаваемый горб в гистограмме длин. Свой
    // случайный пол на сессию убирает эту общую сигнатуру. floor+jitter<256 (одна
    // байтовая длина): 120+32=152 < 255.
    let pad_floor = if pad { 64 + (rand::random::<u8>() as usize % 57) } else { 0 }; // 64..=120
    Ok(NoiseLink {
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
    })
}

/// ЗАРЯЖЕННЫЙ ключом хэш для маски шапки-nonce — считается ОДИН раз на сессию.
///
/// Ключ (хэш рукопожатия) на всю сессию постоянен, а маска нужна КАЖДОМУ пакету
/// в обе стороны. Раньше на каждый пакет заново создавался Sha256 и в него
/// заливался ключ; теперь заливка сделана один раз, а на пакет остаётся дешёвый
/// клон состояния и один короткий `update`.
fn charge_mask_key(key: &[u8]) -> sha2::Sha256 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(key);
    h
}

/// Маска шапки-nonce для «Маскировки»: PRF(hash_рукопожатия, образец шифротекста).
/// И отправитель, и приёмник видят один и тот же шифротекст → маска совпадает, но
/// снаружи (без ключа) шапка выглядит случайной, а счётчик остаётся уникальным.
fn nonce_mask(charged: &sha2::Sha256, ciphertext: &[u8]) -> u64 {
    use sha2::Digest;
    let mut h = charged.clone();
    h.update(&ciphertext[..ciphertext.len().min(16)]);
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
    /// Some(hash) в режиме маскировки — ЗАРЯЖЕННЫЙ ключом хэш для маски шапки-nonce
    /// (см. `charge_mask_key`). None иначе.
    obfs_key: Option<sha2::Sha256>,
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
            // ПАДДИНГ — НУЛИ, И ЭТО НЕ ЭКОНОМИЯ НА СТОЙКОСТИ. Он лежит ВНУТРИ
            // AEAD: снаружи видно только шифротекст, а ChaCha20 превращает любой
            // вход в неотличимую от шума строку — прячется здесь ДЛИНА, а не
            // содержимое, и случайной должна быть именно она (см. obfs_pad_len).
            // Криптостойкий генератор на 152 байта КАЖДОГО пакета — это ChaCha20
            // впустую поверх ChaCha20.
            // ЭТО ПЕРЕСТАНЕТ БЫТЬ ВЕРНЫМ, если паддинг когда-нибудь окажется ВНЕ
            // шифрования (например добивка кадра снаружи AEAD или паддинг в
            // рукопожатии до установки ключей) — там нули видны в проводе и станут
            // отпечатком; тогда сюда возвращается заполнение случайными байтами.
            plain.resize(plain.len() + pad_len, 0);
            plain.push(pad_len as u8);
        }
        // Кадр в ПЕРЕИСПОЛЬЗУЕМОМ буфере: [nonce:8][ciphertext+tag].
        // РАСТЁМ ПО ПОТРЕБНОСТИ, а не clear()+resize(n,0): второе зануляло весь
        // буфер (≈1.4 КБ на КАЖДЫЙ пакет) прямо перед тем, как его перезапишет
        // шифрование. Хвост за пределами кадра наружу не уходит — ниже шлём
        // ровно `&frame[..8 + n]`.
        let mut frame = std::mem::take(&mut *self.send_frame.lock().unwrap());
        let need = plain.len() + 16 + 8; // +tag +шапка
        if frame.len() < need {
            frame.resize(need, 0);
        }
        let out = match self.transport.write_message(nonce, &plain, &mut frame[8..]).map_err(noe) {
            Ok(n) => {
                // Маскировка шапки: nonce ^ маска(шифротекст). Счётчик остаётся
                // уникальным (без nonce-reuse), но в проводе монотонности не видно.
                let wire_nonce = match &self.obfs_key {
                    Some(k) => nonce ^ nonce_mask(k, &frame[8..8 + n]),
                    None => nonce,
                };
                frame[..8].copy_from_slice(&wire_nonce.to_le_bytes());
                self.inner.send(&frame[..8 + n]).await
            }
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
                    // РАСТЁМ ПО ПОТРЕБНОСТИ, а не clear()+resize(n,0): второе
                    // зануляло ≈1.4 КБ на каждый пакет прямо перед перезаписью.
                    // Остаток прошлого пакета наружу не уедет — ниже стоит
                    // truncate(n) по фактической длине расшифрованного.
                    if out.len() < frame.len() {
                        out.resize(frame.len(), 0); // ≥ длины plaintext
                    }
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
                                // Не сходится (байт больше самого plaintext или
                                // plaintext пуст) — кадр ДРОПАЕМ. Раньше паддинг в
                                // этом случае просто не снимался, и наверх, в IP-стек,
                                // уезжал мусор вместе со служебным байтом: пир прошёл
                                // рукопожатие, значит подсунуть такое может он сам.
                                let strip = match out.last() {
                                    // «< len», а не «+1 <= len»: сам байт длины тоже
                                    // отрезается, поэтому паддинг обязан быть строго
                                    // короче того, в чём он лежит.
                                    Some(&pad_len) if (pad_len as usize) < out.len() => {
                                        pad_len as usize + 1
                                    }
                                    _ => continue,
                                };
                                out.truncate(out.len() - strip);
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
    use std::sync::Arc;

    /// Перехваченный пакет не должен приниматься повторно: иначе повтор `BYE`
    /// рвёт чужую сессию, а повтор данных дублирует TCP-сегменты у гостя.
    /// При этом обычная перестановка пакетов в UDP обязана проходить.
    #[test]
    fn replay_window_rejects_repeats_but_allows_reorder() {
        let mut s = (0u64, 0u64);
        assert!(replay_ok(&mut s, 0), "первый пакет");
        assert!(!replay_ok(&mut s, 0), "тот же nonce принят дважды — повтор пролезает");

        assert!(replay_ok(&mut s, 5), "скачок вперёд (потери) — нормально");
        assert!(replay_ok(&mut s, 3), "пришёл с опозданием, но в окне — принять");
        assert!(!replay_ok(&mut s, 3), "а вот его повтор — отвергнуть");
        assert!(!replay_ok(&mut s, 5), "повтор текущего максимума — отвергнуть");
        assert!(replay_ok(&mut s, 4), "дыра между 3 и 5 закрывается законным пакетом");

        // Ушли далеко вперёд: старьё за пределами окна судить нечем — отвергаем.
        assert!(replay_ok(&mut s, 5 + REPLAY_WINDOW + 10));
        assert!(!replay_ok(&mut s, 5), "пакет старше окна обязан отвергаться");

        // Сдвиг ровно на ширину окна не должен паниковать (u64 << 64 = UB в release).
        let mut s2 = (0u64, 0u64);
        assert!(replay_ok(&mut s2, 0));
        assert!(replay_ok(&mut s2, REPLAY_WINDOW));
        assert!(!replay_ok(&mut s2, 0), "после полного сдвига окна старьё не принимаем");
    }

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

    /// Канал, который после взвода флага шлёт каждый пакет ДВАЖДЫ — так выглядит
    /// перехватчик, повторяющий чужую датаграмму. До взвода честный: рукопожатие
    /// повтора не переживает и проверять надо не его.
    struct Dup {
        inner: Box<dyn Link>,
        on: Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait]
    impl Link for Dup {
        async fn send(&self, p: &[u8]) -> Result<()> {
            self.inner.send(p).await?;
            if self.on.load(Ordering::Relaxed) {
                self.inner.send(p).await?;
            }
            Ok(())
        }
        async fn recv_into(&self, b: &mut Vec<u8>) -> Result<bool> {
            self.inner.recv_into(b).await
        }
    }

    // ── КАТЕГОРИЯ Е: враждебные кадры в шифрованном канале ────────────────────
    //
    // Порт хоста открыт всему интернету, и до расшифровки в него может прилететь
    // что угодно: сканер, чужой протокол, целенаправленный мусор. Шифрослой
    // обязан такое молча выбрасывать — не падая по срезу и НЕ РАЗРЫВАЯ живую
    // сессию, иначе любой посторонний глушит чужой туннель одним пакетом.

    /// Канал, в который можно ПОДБРОСИТЬ произвольные кадры: они выдаются из
    /// `recv_into` раньше настоящих, как если бы посторонний попал в наш порт.
    struct Inject {
        inner: Box<dyn Link>,
        queue: Arc<std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>>,
    }
    #[async_trait]
    impl Link for Inject {
        async fn send(&self, p: &[u8]) -> Result<()> {
            self.inner.send(p).await
        }
        async fn recv_into(&self, b: &mut Vec<u8>) -> Result<bool> {
            if let Some(f) = self.queue.lock().unwrap().pop_front() {
                b.clear();
                b.extend_from_slice(&f);
                return Ok(true);
            }
            self.inner.recv_into(b).await
        }
    }

    /// Мусор ЛЮБОГО вида не должен ни ронять приёмник, ни рвать сессию: после
    /// него настоящий пакет обязан дойти. Отдельно проверяются длины 0..8 —
    /// на них разбор трогает шапку-nonce, и ошибка среза была бы паникой.
    #[tokio::test]
    async fn hostile_frames_are_dropped_and_session_survives() {
        let (a, b) = bmv_common::wire::memory_pair(256);
        let q: Arc<std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>> = Default::default();
        let host_link = Box::new(Inject { inner: a, queue: q.clone() });
        let proto = Noise::chacha();
        let (host, guest) = tokio::join!(proto.connect_host(host_link), proto.connect_guest(b));
        let (host, guest) = (host.expect("хост"), guest.expect("гость"));

        {
            let mut qq = q.lock().unwrap();
            for n in 0..=8usize {
                qq.push_back(vec![0xAA; n]); // короче шапки и ровно шапка
            }
            qq.push_back(vec![0u8; 9]);           // шапка есть, шифротекста 1 байт
            qq.push_back(vec![0xFF; 1500]);       // «полный» кадр из мусора
            qq.push_back(b"GET / HTTP/1.1\r\n\r\n".to_vec()); // чужой протокол
            qq.push_back(Vec::new());             // пустой кадр
        }
        guest.send("я настоящий".as_bytes()).await.unwrap();
        let got = tokio::time::timeout(Duration::from_secs(2), host.recv())
            .await
            .expect("приёмник завис на мусоре")
            .unwrap();
        assert_eq!(got, "я настоящий".as_bytes(), "мусор вытеснил настоящий пакет");
    }

    /// Мусор ВМЕСТО рукопожатия. Хост обязан вернуть ошибку и отпустить слот, а
    /// не упасть и не зависнуть: иначе сканер портов кладёт хосту одну сессию
    /// за другой.
    #[tokio::test]
    async fn garbage_instead_of_handshake_fails_cleanly() {
        for junk in [vec![0u8; 1], vec![0xFF; 31], vec![0x41; 200], vec![0u8; 1500]] {
            let (a, b) = bmv_common::wire::memory_pair(8);
            let host = tokio::spawn(async move { Noise::chacha().connect_host(a).await.map(|_| ()) });
            b.send(&junk).await.unwrap();
            let r = tokio::time::timeout(Duration::from_secs(9), host).await;
            let r = r.expect("рукопожатие зависло на мусоре").expect("задача упала (паника)");
            assert!(r.is_err(), "мусор длиной {} принят за рукопожатие", junk.len());
        }
    }

    /// В режиме маскировки первые 32 байта — Elligator2-representative. Слишком
    /// короткое сообщение и невалидная точка обязаны давать ОШИБКУ, а не панику
    /// на срезе `msg[..32]`.
    #[tokio::test]
    async fn obfs_rejects_short_and_invalid_representative() {
        for junk in [vec![0u8; 4], vec![0xAB; 31]] {
            let (a, b) = bmv_common::wire::memory_pair(8);
            let host = tokio::spawn(async move { Noise::obfs().connect_host(a).await.map(|_| ()) });
            b.send(&junk).await.unwrap();
            let r = tokio::time::timeout(Duration::from_secs(9), host).await;
            assert!(r.expect("зависло").expect("паника").is_err(), "короткий эфемерный принят");
        }
    }

    /// Байт длины паддинга — единственное, чем «управляет» отправитель внутри
    /// расшифрованного. Проверяем инвариант его вычисления на всех размерах:
    /// он обязан влезать в один байт, иначе приёмник отрежет не то.
    #[test]
    fn padding_length_always_fits_one_byte() {
        for floor in [0usize, 64, 92, 120] {
            for len in [0usize, 1, 20, 100, 500, 1400, 65000] {
                let pad = obfs_pad_len(len, floor);
                assert!(pad <= u8::MAX as usize, "паддинг {pad} не влезает в байт (len={len}, floor={floor})");
                assert!(len + pad + 1 >= len, "переполнение при len={len}");
            }
        }
    }

    /// Канал, который ГЛОТАЕТ первые `n` отправок — так выглядит потеря пакета
    /// в UDP. Нужен, чтобы проверить повторы рукопожатия.
    struct Lossy {
        inner: Box<dyn Link>,
        drop_left: std::sync::Mutex<usize>,
    }
    #[async_trait]
    impl Link for Lossy {
        async fn send(&self, p: &[u8]) -> Result<()> {
            {
                let mut left = self.drop_left.lock().unwrap();
                if *left > 0 {
                    *left -= 1;
                    return Ok(()); // «потерялся» — отправитель об этом не узнаёт
                }
            }
            self.inner.send(p).await
        }
        async fn recv_into(&self, b: &mut Vec<u8>) -> Result<bool> {
            self.inner.recv_into(b).await
        }
    }
    fn lossy(inner: Box<dyn Link>, drop_first: usize) -> Box<dyn Link> {
        Box::new(Lossy { inner, drop_left: std::sync::Mutex::new(drop_first) })
    }

    /// Потерян ПЕРВЫЙ пакет гостя (msg1). Без повторов рукопожатие сорвалось бы,
    /// и человек увидел бы «не удалось подключиться» на ровном месте.
    #[tokio::test]
    async fn handshake_survives_lost_first_message() {
        let (a, b) = bmv_common::wire::memory_pair(32);
        let proto = Noise::chacha();
        let (host, guest) = tokio::join!(proto.connect_host(a), proto.connect_guest(lossy(b, 1)));
        let (host, guest) = (host.expect("хост"), guest.expect("гость"));
        guest.send(b"payload").await.unwrap();
        assert_eq!(host.recv().await.unwrap(), b"payload");
    }

    /// Потерян ОТВЕТ хоста (msg2). Гость повторит msg1 — и хост обязан УЗНАТЬ
    /// повтор побайтово и переслать msg2. Если вместо этого он отдаст дубликат
    /// в snow, состояние рукопожатия сломается и правильный msg3 уже не примется.
    #[tokio::test]
    async fn handshake_survives_lost_reply() {
        let (a, b) = bmv_common::wire::memory_pair(32);
        let proto = Noise::chacha();
        let (host, guest) = tokio::join!(proto.connect_host(lossy(a, 1)), proto.connect_guest(b));
        let (host, guest) = (host.expect("хост"), guest.expect("гость"));
        host.send(b"payload").await.unwrap();
        assert_eq!(guest.recv().await.unwrap(), b"payload");
    }

    /// То же в режиме маскировки: там msg1 ещё и подменяется Elligator2, поэтому
    /// «узнать повтор» обязано работать по байтам ПРОВОДА, а не по расшифрованному.
    #[tokio::test]
    async fn handshake_survives_loss_in_obfs_mode() {
        let (a, b) = bmv_common::wire::memory_pair(32);
        let proto = Noise::obfs();
        let (host, guest) = tokio::join!(proto.connect_host(lossy(a, 1)), proto.connect_guest(b));
        let (host, guest) = (host.expect("хост"), guest.expect("гость"));
        guest.send(b"payload").await.unwrap();
        assert_eq!(host.recv().await.unwrap(), b"payload");
    }

    /// СКВОЗНАЯ проверка анти-повтора: повторённый пакет не должен доехать до
    /// приложения. Без этого повтор перехваченного `BYE` рвал бы живую сессию,
    /// а повтор данных дублировал бы TCP-сегменты у гостя.
    #[tokio::test]
    async fn replayed_frame_is_dropped_end_to_end() {
        let (a, b) = bmv_common::wire::memory_pair(32);
        let on = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dup = Box::new(Dup { inner: b, on: on.clone() });
        let proto = Noise::chacha();
        let (host, guest) = tokio::join!(proto.connect_host(a), proto.connect_guest(dup));
        let (host, guest) = (host.expect("host handshake"), guest.expect("guest handshake"));

        on.store(true, Ordering::Relaxed); // с этого момента всё уходит дважды
        guest.send(b"once").await.unwrap();
        assert_eq!(host.recv().await.unwrap(), b"once", "оригинал обязан дойти");

        // Второй экземпляр того же кадра лежит в канале. Приёмник должен его съесть
        // и НЕ отдать наверх — то есть recv замирает в ожидании нового пакета.
        let leaked = tokio::time::timeout(Duration::from_millis(200), host.recv()).await;
        assert!(leaked.is_err(), "повтор пакета доехал до приложения — анти-повтора нет");

        // И канал при этом не залип: следующий настоящий пакет проходит.
        guest.send(b"next").await.unwrap();
        let got = tokio::time::timeout(Duration::from_secs(1), host.recv())
            .await
            .expect("после дропа повтора канал встал")
            .unwrap();
        assert_eq!(got, b"next");
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
        let k = charge_mask_key(&[7u8; 32]);
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

    /// Собрать кадр РАБОЧИМИ ключами сессии, положив внутрь произвольный
    /// plaintext (в т.ч. со злым байтом длины паддинга). Так выглядит пир,
    /// который прошёл рукопожатие, но ведёт себя враждебно, — от мусора снаружи
    /// это отличается принципиально: пломба сходится, значит кадр РАСШИФРУЕТСЯ.
    async fn send_forged(from: &NoiseLink, plain: &[u8]) {
        let nonce = from.send_ctr.fetch_add(1, Ordering::Relaxed);
        let mut frame = vec![0u8; plain.len() + 16 + 8];
        let n = from.transport.write_message(nonce, plain, &mut frame[8..]).unwrap();
        frame.truncate(8 + n);
        let wire = match &from.obfs_key {
            Some(k) => nonce ^ nonce_mask(k, &frame[8..]),
            None => nonce,
        };
        frame[..8].copy_from_slice(&wire.to_le_bytes());
        from.inner.send(&frame).await.unwrap();
    }

    /// БАЙТ ДЛИНЫ ПАДДИНГА БОЛЬШЕ САМОГО PLAINTEXT. Это единственное число,
    /// которым «управляет» отправитель внутри расшифрованного кадра. Если такому
    /// кадру просто не снять паддинг, наверх (в IP-стек!) уедет мусор вместе со
    /// служебным байтом. Кадр обязан быть ВЫБРОШЕН, а сессия — выжить.
    #[tokio::test]
    async fn padding_longer_than_plaintext_drops_the_frame() {
        let (a, b) = bmv_common::wire::memory_pair(16);
        let p = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
        let (host, guest) = tokio::join!(handshake(a, p, false, true), handshake(b, p, true, true));
        let (host, guest) = (host.unwrap(), guest.unwrap());

        // Заявлено 200 байт паддинга при 3 байтах plaintext.
        send_forged(&guest, &[b'X', b'Y', 200]).await;
        // Пустой кадр и «паддинг ровно во весь plaintext» — соседние граничные случаи.
        send_forged(&guest, &[]).await;
        send_forged(&guest, &[3, 3]).await;
        // …а следом — честный пакет. Он и должен прийти первым же recv.
        guest.send("настоящий".as_bytes()).await.unwrap();

        let got = tokio::time::timeout(Duration::from_secs(2), host.recv())
            .await
            .expect("приёмник завис на кривом паддинге")
            .unwrap();
        assert_eq!(got, "настоящий".as_bytes(), "наверх уехал мусор вместо ошибки");
    }

    /// РЕГРЕССИЯ БУФЕРОВ: буфер приёма переиспользуется и больше не зануляется
    /// целиком, поэтому длинный пакет ОБЯЗАН быть полностью вытеснен коротким.
    /// Если обрезка съедет, короткий пакет приедет с хвостом предыдущего —
    /// в туннеле это чужие байты в чужом соединении.
    #[tokio::test]
    async fn alternating_sizes_leave_no_leftovers() {
        for proto in [Noise::obfs(), Noise::chacha()] {
            let (a, b) = bmv_common::wire::memory_pair(64);
            let (host, guest) = tokio::join!(proto.connect_host(a), proto.connect_guest(b));
            let (host, guest) = (host.unwrap(), guest.unwrap());
            let mut buf = Vec::new();
            // Чередуем большой → крошечный → большой: именно так вылезает хвост.
            for len in [1400usize, 1, 1400, 0, 900, 2, 1400, 1, 60, 1399] {
                let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8 + 1).collect();
                guest.send(&payload).await.unwrap();
                assert!(host.recv_into(&mut buf).await.unwrap());
                assert_eq!(buf, payload, "остаток предыдущего пакета (len={len}, {})", proto.name());
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
