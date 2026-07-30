//! UDP-транспорт: `Link` поверх реального сокета.
//!
//! Датаграммная модель UDP идеально ложится на `Link` (пакет = пакет). Это
//! «сырой путь» (`RawPath`), поверх которого протокол наложит шифрование.
//!
//! Один `UdpEndpoint` = один забинженный сокет. С него же делаем STUN (чтобы
//! анонсированный порт совпал с портом, на котором слушаем), учим адрес пира
//! (`accept`) или соединяемся с известным (`connect`).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use parking_lot::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bmv_common::{Error, Link, Result};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::stun;

const MAX_DATAGRAM: usize = 65_535;

/// Потолок числа пиров в одном хабе. Реальных гостей — сотни максимум (UI-лимит
/// 256). Это заслон памяти от флуда PUNCH со спуфнутых адресов: сверх потолка
/// новые «гости» не заводятся (карта не растёт бесконечно). Живой хост не касается.
const MAX_HUB_PEERS: usize = 4096;

/// Желаемые буферы UDP-сокета. Большие окна TCP-over-tunnel приходят взрывами
/// в сотни пакетов; дефолтный rcvbuf (~212 КБ) их не вмещает → потери прямо на
/// сокете. 4 МБ вмещает взрыв целиком. Ставим best-effort: ОС может урезать
/// до своего максимума (net.core.rmem_max) — это нормально.
const SOCK_BUF: usize = 4 * 1024 * 1024;

/// Пул переиспользуемых буферов демультиплексора (хост↔много гостей). demux
/// раскладывает пакеты в пер-гостевые очереди; без пула это `Vec` на КАЖДЫЙ
/// пакет КАЖДОГО гостя (аллокатор потеет при сотне гостей на высоком pps).
/// HubPeerLink после копирования в вызывающего ВОЗВРАЩАЕТ буфер сюда → в
/// устоявшемся режиме приём хоста не аллоцирует. Потолок — заслон роста.
type BufPool = Arc<Mutex<Vec<Vec<u8>>>>;
const DEMUX_POOL_CAP: usize = 4096;

/// Взять буфер из пула (или свежий) и заполнить его `data` без лишней аллокации.
fn pooled_copy(pool: &BufPool, data: &[u8]) -> Vec<u8> {
    let mut b = pool.lock().pop().unwrap_or_default();
    b.clear();
    b.extend_from_slice(data);
    b
}

/// Вернуть буфер в пул (с ограничением, чтобы пул не рос бесконечно).
fn pool_return(pool: &BufPool, mut b: Vec<u8>) {
    let mut p = pool.lock();
    if p.len() < DEMUX_POOL_CAP {
        b.clear();
        p.push(b);
    }
}

/// Забиндить UDP-сокет с большими буферами (best-effort) → tokio.
async fn bind_udp(addr: SocketAddr) -> Result<UdpSocket> {
    let domain = if addr.is_ipv4() { socket2::Domain::IPV4 } else { socket2::Domain::IPV6 };
    let sock = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    let _ = sock.set_recv_buffer_size(SOCK_BUF);
    let _ = sock.set_send_buffer_size(SOCK_BUF);
    sock.set_nonblocking(true)?;
    sock.bind(&addr.into())?;
    Ok(UdpSocket::from_std(sock.into())?)
}

// Маркеры фазы пробивания NAT. РАНЬШЕ были общие ASCII `BMV-PUNCH`/`BMV-PACK` —
// одинаковые во ВСЕХ сессиях: готовая сигнатура для DPI И оракул активного
// зондирования (пошли «BMV-PUNCH» на порт хоста → он отвечал «BMV-PACK»,
// подтверждая BeMyVPN). Теперь токены выводятся из host_id: его знают обе стороны,
// но снаружи он не виден (идёт к координатору по TLS) → для наблюдателя это
// непредсказуемые байты, разные у разных сетей, и зонд без host_id ответа не получит.
fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x00000100000001b3);
    }
    h
}
/// Пер-хостовые токены (punch, pack) из host_id. FNV-1a — детерминирован и
/// СТАБИЛЕН между версиями/платформами (std DefaultHasher этого НЕ гарантирует,
/// а гость и хост могут быть на разных сборках).
pub fn punch_tokens(host_id: &str) -> (Vec<u8>, Vec<u8>) {
    let punch = fnv1a(format!("bmv-punch:{host_id}").as_bytes()).to_le_bytes().to_vec();
    let pack = fnv1a(format!("bmv-pack:{host_id}").as_bytes()).to_le_bytes().to_vec();
    (punch, pack)
}

/// Токены ПРОБЫ ЗАДЕРЖКИ (ping, pong) — чтобы гость мог узнать, далеко ли хост,
/// ДО подключения к нему.
///
/// Зачем отдельные, а не те же punch/pack: `PUNCH` на хосте заводит СЕССИЮ —
/// создаёт запись пира, канал и отдаёт «гостя» в accept-цикл. Такой «гость»
/// никогда не пожмёт руку, провисит до таймаута рукопожатия (6с) и всё это время
/// будет держать слот в воротах анти-флуда. Если каждый гость станет так мерить
/// задержку до всех хостов в списке, хосты захлебнутся ничем.
///
/// Ответ на `ping` НЕ СОЗДАЁТ НИЧЕГО: восемь байт пришло — восемь ушло. Усиления
/// нет (ответ ровно того же размера), состояния нет, поэтому злоупотребить этим
/// сложнее, чем уже существующим `PUNCH`.
pub fn ping_tokens(host_id: &str) -> (Vec<u8>, Vec<u8>) {
    let ping = fnv1a(format!("bmv-ping:{host_id}").as_bytes()).to_le_bytes().to_vec();
    let pong = fnv1a(format!("bmv-pong:{host_id}").as_bytes()).to_le_bytes().to_vec();
    (ping, pong)
}


/// Измерить задержку до хоста, НЕ подключаясь к нему.
///
/// Гость выбирает хост из списка вслепую: хост в 20мс и хост в 300мс выглядят
/// одинаково, а разница между ними — это разница между «работает» и «мучение».
///
/// Шлём пробу на ВСЕ известные адреса хоста разом и берём первый ответ: адресов
/// у него обычно два (домашний и внешний), заранее неизвестно, какой рабочий, а
/// последовательный перебор удвоил бы ожидание. Ответ подтверждает и живость, и
/// достижимость — то есть меряется ровно тот путь, по которому пойдёт туннель.
///
/// `None` — ответа нет за отведённое время: хост либо недостижим отсюда, либо
/// за таким NAT, что без пробивания к нему не достучаться. Это ЧЕСТНЫЙ ответ,
/// а не ошибка: показать «нет ответа» полезнее, чем нарисовать выдуманное число.
pub async fn probe_rtt(host_id: &str, endpoints: &[String], timeout: Duration) -> Option<Duration> {
    let addrs: Vec<SocketAddr> = endpoints.iter().filter_map(|s| s.parse().ok()).collect();
    if addrs.is_empty() {
        return None;
    }
    let (ping, pong) = ping_tokens(host_id);
    // Свой одноразовый сокет: чужой (хаб хоста или туннель) трогать нельзя —
    // ответы на пробу смешались бы с рабочим трафиком.
    let sock = bind_udp("0.0.0.0:0".parse().ok()?).await.ok()?;
    let started = std::time::Instant::now();
    for a in &addrs {
        let _ = sock.send_to(&ping, a).await;
    }
    let mut buf = [0u8; 64];
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        match tokio::time::timeout(left, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, from))) => {
                // Чужие датаграммы (сканеры, поздние ответы) не считаем за ответ.
                if buf[..n] == pong[..] && addrs.contains(&from) {
                    return Some(started.elapsed());
                }
            }
            _ => return None,
        }
    }
}

/// Забинженный UDP-эндпоинт. Один сокет — источник и STUN, и данных.
pub struct UdpEndpoint {
    sock: Arc<UdpSocket>,
}

impl UdpEndpoint {
    /// Забиндить локальный UDP-сокет.
    pub async fn bind(addr: SocketAddr) -> Result<Self> {
        let sock = bind_udp(addr).await?;
        Ok(UdpEndpoint {
            sock: Arc::new(sock),
        })
    }

    /// Локальный адрес сокета.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.sock.local_addr()?)
    }

    /// Внешний адрес этого сокета через STUN (переиспользует тот же сокет).
    pub async fn reflexive(&self, servers: &[String], wait: Duration) -> Result<SocketAddr> {
        stun::reflexive_addr_on(&self.sock, servers, wait).await
    }

    /// ГОСТЬ: отдать Link после пробития. `punch`/`pack` — пер-хостовые токены
    /// фазы пробивания (их «хвост» recv отфильтрует, чтобы не принять за данные).
    /// `primed` — возможный первый пакет данных (вернётся первым `recv`).
    pub fn connect_primed(&self, peer: SocketAddr, primed: Option<Vec<u8>>, punch: Vec<u8>, pack: Vec<u8>) -> UdpLink {
        UdpLink {
            sock: self.sock.clone(),
            peer,
            primed: Mutex::new(primed),
            scratch: Mutex::new(None),
            punch,
            pack,
        }
    }

    /// Пробить NAT к одному из кандидатов пира (симметрично для обеих сторон).
    ///
    /// Обе стороны шлют PUNCH всем кандидатам; на чужой PUNCH отвечают PACK.
    /// Получили PACK — round-trip подтверждён, путь готов. Если во время
    /// пробивания прилетел НЕ punch-пакет (пир уже начал протокол) — считаем
    /// путь подтверждённым и возвращаем этот пакет как «первый» (не теряем его).
    ///
    /// Возвращает (адрес пира, возможный первый пакет данных).
    pub async fn hole_punch(
        &self,
        peers: &[SocketAddr],
        window: Duration,
        punch: &[u8],
        pack: &[u8],
    ) -> Result<(SocketAddr, Option<Vec<u8>>)> {
        let deadline = Instant::now() + window;
        let mut buf = vec![0u8; MAX_DATAGRAM];
        // Если получили PUNCH пира, но PACK ещё не пришёл — путь, вероятно, уже
        // открыт. Не возвращаемся сразу: продолжаем слать PUNCH, чтобы и НАШ
        // PUNCH дошёл до пира (за NAT это обязательно — иначе он нас не заведёт).
        let mut punched_peer: Option<SocketAddr> = None;
        log::info!("ПАНЧ: пробиваю к {} кандидатам: {:?}", peers.len(), peers);
        let mut rounds = 0u32;
        let mut stray = 0u32; // пакеты от НЕОЖИДАННЫХ адресов (признак симм. NAT)
        // ОБРАТНОЕ ПРОБИТИЕ: реальные порты строгого (симметричного) хоста,
        // выученные из его контр-панча (STUN назвал ему другой порт). Кап — заслон
        // от флуда подставными адресами; дальше всё равно аутентифицирует Noise.
        let mut learned: Vec<SocketAddr> = Vec::new();
        const MAX_LEARNED: usize = 4;

        while Instant::now() < deadline {
            rounds += 1;
            // Панчим кандидатов + выученные реальные порты строгого хоста.
            for p in peers.iter().chain(learned.iter()) {
                let _ = self.sock.send_to(punch, p).await;
            }
            let step = Instant::now() + Duration::from_millis(250);
            loop {
                let left = step.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match timeout(left, self.sock.recv_from(&mut buf)).await {
                    Ok(Ok((n, from))) => {
                        // ВАЖНО: принимаем только от кандидатов пира. Иначе «хвост»
                        // поздних STUN-ответов (мы STUN'или этот же сокет для сбора
                        // кандидатов) был бы принят за данные пира — и мы бы
                        // «соединились» со STUN-сервером вместо хоста. Ломало
                        // подключение на путях, где STUN отвечает (мобильные сети).
                        // Валидный источник — кандидат ИЛИ уже выученный реальный порт
                        // строгого хоста (обратное пробитие).
                        if !peers.contains(&from) && !learned.contains(&from) {
                            // Не кандидат. Если это PUNCH с IP кандидата (но другого
                            // порта) — это ХОСТ ЗА СТРОГИМ (симметричным) NAT
                            // контр-панчит со своего РЕАЛЬНОГО порта (STUN назвал ему
                            // другой). Раньше мы это отбрасывали как «нужен релей».
                            // Теперь УЧИМ его: со следующего раунда панчим туда и
                            // отвечаем — прямой прострел «строгий хост × мягкий гость»
                            // становится возможен. IP ОБЯЗАН совпасть с кандидатом —
                            // это отсекает поздние ответы STUN (у них другой IP).
                            if &buf[..n] == punch
                                && learned.len() < MAX_LEARNED
                                && peers.iter().any(|c| c.ip() == from.ip())
                            {
                                learned.push(from);
                                log::info!("ПАНЧ: выучил реальный порт строгого хоста {from} — обратное пробитие ↩");
                                // падаем в общий разбор ниже как валидный from
                            } else {
                                // Пакет с чужого IP (или не-PUNCH с чужого порта):
                                // диагностика + отброс, как раньше.
                                stray += 1;
                                let kind = if &buf[..n] == punch { "PUNCH" } else if &buf[..n] == pack { "PACK" } else { "данные" };
                                if stray <= 4 {
                                    log::warn!("ПАНЧ: пакет {kind} от НЕОЖИДАННОГО {from} (нет в кандидатах) — похоже на СИММЕТРИЧНЫЙ NAT");
                                }
                                continue;
                            }
                        }
                        let data = &buf[..n];
                        if data == punch {
                            // Пир пробивается к нам: подтверждаем PACK'ом, но НЕ
                            // выходим — продолжаем слать свой PUNCH.
                            let _ = self.sock.send_to(pack, from).await;
                            punched_peer = Some(from);
                            log::info!("ПАНЧ: получил PUNCH от {from} — путь открывается");
                        } else if data == pack {
                            // Пир подтвердил — путь точно двусторонний.
                            log::info!("ПАНЧ: PACK от {from} за {rounds} раундов — УСПЕХ ✅");
                            return Ok((from, None));
                        } else {
                            // Пир уже перешёл к протоколу — путь есть, пакет не теряем.
                            log::info!("ПАНЧ: данные от {from} — путь есть ✅");
                            return Ok((from, Some(data.to_vec())));
                        }
                    }
                    Ok(Err(e)) => return Err(e.into()),
                    Err(_) => break,
                }
            }
            // Видели PUNCH и продержали ещё раунд — считаем путь открытым.
            if let Some(peer) = punched_peer {
                log::info!("ПАНЧ: путь с {peer} держится — УСПЕХ ✅");
                return Ok((peer, None));
            }
        }
        if stray > 0 {
            log::warn!("ПАНЧ: НЕ пробит за {rounds} раундов; было {stray} пакетов с чужих портов → СИММЕТРИЧНЫЙ NAT (нужен релей)");
        } else {
            log::warn!("ПАНЧ: НЕ пробит за {rounds} раундов; ни одного пакета от пира (порт закрыт / пир недостижим / UDP режется)");
        }
        Err(Error::Net("NAT не пробит: пир не ответил".into()))
    }
}

/// `Link` поверх UDP к конкретному пиру. Один сокет, send/recv параллельны.
pub struct UdpLink {
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
    primed: Mutex<Option<Vec<u8>>>,
    /// Переиспользуемый приёмный буфер: аллоцируем один раз на весь Link, а не
    /// зануляем 64 КБ на каждый пакет (критично для CPU на телефоне при высоком
    /// pps). Берём из Mutex до `.await`, кладём обратно после — лок НЕ держится
    /// через await, поэтому future остаётся Send.
    scratch: Mutex<Option<Vec<u8>>>,
    /// Пер-хостовые токены фазы пробивания — их «хвост» отфильтровываем в recv.
    punch: Vec<u8>,
    pack: Vec<u8>,
}

impl UdpLink {
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }
}

#[async_trait]
impl Link for UdpLink {
    async fn send(&self, packet: &[u8]) -> Result<()> {
        self.sock.send_to(packet, self.peer).await?;
        Ok(())
    }

    async fn recv_into(&self, out: &mut Vec<u8>) -> Result<bool> {
        if let Some(first) = self.primed.lock().take() {
            out.clear();
            out.extend_from_slice(&first);
            return Ok(true);
        }
        // Достаём переиспользуемый буфер приёма (или заводим, если это первый recv).
        let mut buf = self
            .scratch
            .lock()
            .take()
            .unwrap_or_else(|| vec![0u8; MAX_DATAGRAM]);
        let res = loop {
            match self.sock.recv_from(&mut buf).await {
                Ok((n, from)) => {
                    if from != self.peer {
                        continue; // чужой пир
                    }
                    let data = &buf[..n];
                    // «Хвост» пакетов фазы пробивания NAT — не данные, пропускаем.
                    if data == self.punch.as_slice() || data == self.pack.as_slice() {
                        continue;
                    }
                    // В буфер вызывающего — без аллокации (переиспользуется).
                    out.clear();
                    out.extend_from_slice(data);
                    break Ok(true);
                }
                Err(e) => break Err(e.into()),
            }
        };
        // Возвращаем приёмный буфер в пул (лок берём заново, через await не держали).
        *self.scratch.lock() = Some(buf);
        res
    }
}

/// Определить основной локальный IP (для кандидата host в LAN) без отправки
/// пакетов: «подключаем» UDP-сокет к внешнему адресу и читаем свой local_addr.
pub fn local_ip() -> Option<IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

// ── UdpHub: один сокет — много гостей (мультигость) ──────────────────────────

/// Хаб: один UDP-сокет обслуживает МНОГО пиров одновременно, демультиплексируя
/// входящие датаграммы по адресу источника. Так хост принимает нескольких
/// гостей на одном порту — стандартный приём (как в WireGuard/QUIC), не костыль.
///
/// Реактивный hole-punch: на `PUNCH` от НОВОГО адреса отвечаем `PACK` и заводим
/// этому пиру персональный `Link`. Хосту не нужно знать адреса гостей заранее —
/// значит не нужна и очередь заявок на координаторе.
pub struct UdpHub {
    sock: Arc<UdpSocket>,
    accept_rx: tokio::sync::Mutex<mpsc::Receiver<(SocketAddr, Box<dyn Link>)>>,
    /// Пер-хостовый токен встречного PUNCH (см. `punch_tokens`).
    punch: Vec<u8>,
}

type Peers = Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>>;

impl UdpHub {
    /// Забиндить хаб и УЗНАТЬ внешний адрес STUN'ом ДО запуска демультиплексора.
    /// Критично: если STUN'ить после старта demux, тот съест STUN-ответ и хост за
    /// NAT анонсирует только LAN-адрес → гости к нему не пробьются. Поэтому STUN —
    /// на «чистом» сокете, и лишь потом включаем демукс.
    /// `punch`/`pack` — пер-хостовые токены фазы пробивания (см. `punch_tokens`),
    /// `ping`/`pong` — токены пробы задержки (см. `ping_tokens`).
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_reflexive(
        addr: SocketAddr,
        servers: &[String],
        wait: Duration,
        punch: Vec<u8>,
        pack: Vec<u8>,
        ping: Vec<u8>,
        pong: Vec<u8>,
    ) -> Result<(Arc<Self>, Option<SocketAddr>)> {
        let sock = Arc::new(bind_udp(addr).await?);
        let reflexive = if wait.is_zero() {
            None
        } else {
            stun::reflexive_addr_on(&sock, servers, wait).await.ok()
        };
        let peers: Peers = Arc::new(Mutex::new(HashMap::new()));
        let pool: BufPool = Arc::new(Mutex::new(Vec::new()));
        let (accept_tx, accept_rx) = mpsc::channel(32);
        tokio::spawn(demux(sock.clone(), peers, pool, accept_tx, punch.clone(), pack, ping, pong));
        let hub = Arc::new(UdpHub {
            sock,
            accept_rx: tokio::sync::Mutex::new(accept_rx),
            punch,
        });
        Ok((hub, reflexive))
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.sock.local_addr()?)
    }

    /// Внешний адрес хаб-сокета через STUN (тот же сокет — тот же порт).
    pub async fn reflexive(&self, servers: &[String], wait: Duration) -> Result<SocketAddr> {
        stun::reflexive_addr_on(&self.sock, servers, wait).await
    }

    /// NAT-keepalive: пнуть STUN-сервер с hub-сокета, чтобы NAT не закрыл его
    /// мэппинг (иначе гости перестают пробиваться к хосту за NAT). Fire-and-forget
    /// — ответ (если придёт) съест демультиплексор и молча отбросит, это ок.
    pub async fn nat_keepalive(&self, servers: &[String]) {
        let list: Vec<String> = if servers.is_empty() {
            stun::DEFAULT_STUN.iter().map(|s| s.to_string()).collect()
        } else {
            servers.to_vec()
        };
        let req = stun::binding_request();
        for s in list.iter().take(2) {
            if let Ok(mut addrs) = tokio::net::lookup_host(s).await {
                if let Some(dst) = addrs.next() {
                    let _ = self.sock.send_to(&req, dst).await;
                }
            }
        }
    }

    /// ВСТРЕЧНОЕ ПРОБИТИЕ: хост шлёт PUNCH по адресам ждущего гостя (узнаёт их у
    /// координатора). Это открывает NAT хоста навстречу гостю — иначе PUNCH
    /// гостя не дойдёт до хоста за NAT. Демультиплексор при этом заведёт гостя,
    /// когда ЕГО PUNCH наконец пробьётся.
    pub async fn punch(&self, addrs: &[SocketAddr]) {
        for a in addrs {
            let _ = self.sock.send_to(&self.punch, a).await;
        }
    }

    /// Отправить произвольную датаграмму с ЭТОГО же hub-сокета (тот же
    /// внешний адрес). Нужно для UDP-keepalive координатору: он видит источником
    /// именно рефлексивный адрес хоста и держит его как авторитетный. Ответ
    /// координатора (если будет) съест демультиплексор и молча отбросит — ок.
    pub async fn send_raw(&self, data: &[u8], dst: SocketAddr) {
        let _ = self.sock.send_to(data, dst).await;
    }

    /// Дождаться следующего нового гостя. `None` — хаб закрыт.
    pub async fn accept(&self) -> Option<(SocketAddr, Box<dyn Link>)> {
        self.accept_rx.lock().await.recv().await
    }
}

/// Фоновый демультиплексор: раскидывает датаграммы по персональным очередям.
#[allow(clippy::too_many_arguments)]
async fn demux(
    sock: Arc<UdpSocket>,
    peers: Peers,
    pool: BufPool,
    accept_tx: mpsc::Sender<(SocketAddr, Box<dyn Link>)>,
    punch: Vec<u8>,
    pack: Vec<u8>,
    ping: Vec<u8>,
    pong: Vec<u8>,
) {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        let (n, from) = match sock.recv_from(&mut buf).await {
            Ok(x) => x,
            Err(_) => break,
        };
        let data = &buf[..n];
        let tx = peers.lock().get(&from).cloned();
        if let Some(tx) = tx {
            // Буфер из пула (не аллокация на пакет). Полна очередь → try_send
            // вернёт буфер в Err, кладём обратно в пул (для UDP дроп норм).
            match tx.try_send(pooled_copy(&pool, data)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(b)) | Err(mpsc::error::TrySendError::Closed(b)) => {
                    pool_return(&pool, b);
                }
            }
        } else if data == ping.as_slice() {
            // Проба задержки: отвечаем и НЕ заводим ни пира, ни канала, ни сессии
            // (см. ping_tokens). Ровно поэтому её можно звать для всего списка.
            let _ = sock.send_to(&pong, from).await;
        } else if data == punch.as_slice() && peers.lock().len() < MAX_HUB_PEERS {
            // новый гость: подтверждаем и заводим персональный канал (пока не
            // упёрлись в потолок пиров — иначе флуд PUNCH раздул бы карту).
            let _ = sock.send_to(&pack, from).await;
            let (tx, rx) = mpsc::channel(1024);
            peers.lock().insert(from, tx);
            let link = HubPeerLink {
                sock: sock.clone(),
                peer: from,
                rx: tokio::sync::Mutex::new(rx),
                peers: peers.clone(),
                pool: pool.clone(),
                punch: punch.clone(),
                pack: pack.clone(),
            };
            if accept_tx.send((from, Box::new(link))).await.is_err() {
                break; // хаб уронили — выходим
            }
        }
        // датаграммы от неизвестных без PUNCH — игнорируем
    }
}

/// Персональный `Link` одного гостя внутри хаба: шлём через общий сокет на его
/// адрес, принимаем из своей очереди. При Drop снимаем себя с учёта в хабе.
struct HubPeerLink {
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
    rx: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
    peers: Peers,
    pool: BufPool,
    punch: Vec<u8>,
    pack: Vec<u8>,
}

impl Drop for HubPeerLink {
    fn drop(&mut self) {
        self.peers.lock().remove(&self.peer);
    }
}

#[async_trait]
impl Link for HubPeerLink {
    async fn send(&self, packet: &[u8]) -> Result<()> {
        self.sock.send_to(packet, self.peer).await?;
        Ok(())
    }

    async fn recv_into(&self, out: &mut Vec<u8>) -> Result<bool> {
        let mut rx = self.rx.lock().await;
        loop {
            match rx.recv().await {
                None => return Ok(false), // хаб закрыт
                Some(d) if d.as_slice() == self.punch.as_slice() || d.as_slice() == self.pack.as_slice() => {
                    pool_return(&self.pool, d); // «хвост» пробивания — вернуть буфер
                    continue;
                }
                Some(d) => {
                    out.clear();
                    out.extend_from_slice(&d);
                    pool_return(&self.pool, d); // буфер отработал — назад в пул
                    return Ok(true);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// БЫСТРЫЙ ВЫХОД через РЕАЛЬНЫЙ UDP: close() одной стороны (KeepaliveLink над
    /// UdpLink) шлёт BYE, и другая сторона видит EOF СРАЗУ, не по keepalive-таймауту.
    /// Изолирует механизм от iOS: если тест зелёный — ядро шлёт BYE по UDP исправно,
    /// значит проблема на устройстве в тайминге/сокете iOS, а не в ядре.
    #[tokio::test]
    async fn keepalive_bye_over_real_udp_is_instant() {
        let a = UdpEndpoint::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let b = UdpEndpoint::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let a_addr = a.local_addr().unwrap();
        let b_addr = b.local_addr().unwrap();
        let (pt, pk) = punch_tokens("bye-udp-test");
        // Два UdpLink, направленных друг на друга (как гость↔хост после пробития).
        let la = a.connect_primed(b_addr, None, pt.clone(), pk.clone());
        let lb = b.connect_primed(a_addr, None, pt, pk);
        let ka = bmv_common::KeepaliveLink::new(Box::new(la));
        let kb = bmv_common::KeepaliveLink::new(Box::new(lb));

        // Сторона A прощается (как клиент при «Стоп»).
        ka.close().await.unwrap();

        // B ОБЯЗАН увидеть EOF много быстрее DEAD_AFTER (8с). 2с — с запасом.
        let mut buf = Vec::new();
        let r = tokio::time::timeout(std::time::Duration::from_secs(2), {
            use bmv_common::Link;
            kb.recv_into(&mut buf)
        }).await;
        assert!(
            !r.expect("BYE по UDP не дошёл за 2с — ядро НЕ шлёт прощание").unwrap(),
            "close()+BYE по реальному UDP должен дать EOF мгновенно"
        );
    }

    /// Регресс на баг «соединился со STUN-сервером»: если в буфере сокета лежит
    /// чужой пакет (поздний STUN-ответ), hole_punch должен его ИГНОРИРОВАТЬ и
    /// соединиться с настоящим пиром, а не с чужим адресом.
    #[tokio::test]
    async fn hole_punch_ignores_stranger_packets() {
        let a = UdpEndpoint::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let b = UdpEndpoint::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let a_addr = a.local_addr().unwrap();
        let b_addr = b.local_addr().unwrap();
        // Обе стороны — один host_id → одинаковые токены пробивания.
        let (pt, pk) = punch_tokens("loopback-host");

        // «STUN-сервер» шлёт мусор на A ДО начала пробития — он оседает в буфере.
        let stranger = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        stranger.send_to(b"stale-stun-binding-response", a_addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // B пробивается к A навстречу (те же токены).
        let (pt2, pk2) = (pt.clone(), pk.clone());
        let b_task = tokio::spawn(async move {
            let _ = b.hole_punch(&[a_addr], Duration::from_secs(3), &pt2, &pk2).await;
        });
        // A должен соединиться с B (b_addr), а НЕ с адресом «STUN-сервера».
        let (peer, _) = a.hole_punch(&[b_addr], Duration::from_secs(3), &pt, &pk).await.unwrap();
        assert_eq!(peer, b_addr, "hole_punch поймал чужой пакет вместо пира");
        let _ = b_task.await;
    }


    /// Проба задержки обязана: (1) вернуть измеренное время, (2) НЕ ЗАВЕСТИ на
    /// хосте сессию. Второе — главное: если бы проба вела себя как PUNCH, каждый
    /// «пинг» вешал бы на хост мёртвого гостя на 6 секунд, и список из десятка
    /// хостов превращался бы в атаку на всех сразу.
    #[tokio::test]
    async fn probe_measures_rtt_without_creating_a_session() {
        let (pt, pk) = punch_tokens("probe-test");
        let (pi, po) = ping_tokens("probe-test");
        let (hub, _refl) = UdpHub::bind_reflexive(
            "127.0.0.1:0".parse().unwrap(), &[], Duration::ZERO, pt, pk, pi, po,
        ).await.unwrap();
        let addr = hub.local_addr().unwrap().to_string();

        let rtt = probe_rtt("probe-test", std::slice::from_ref(&addr), Duration::from_secs(2)).await;
        assert!(rtt.is_some(), "хост не ответил на пробу");
        assert!(rtt.unwrap() < Duration::from_secs(1), "по петле должно быть мгновенно");

        // Никакого «гостя» появиться не должно: accept обязан молчать.
        let accepted = tokio::time::timeout(Duration::from_millis(300), hub.accept()).await;
        assert!(accepted.is_err(), "проба завела на хосте сессию — так делать нельзя");
    }

    /// Чужой host_id — чужие токены: ответа быть не должно. Иначе пробой можно
    /// было бы «нащупать» хост, кода которого не знаешь.
    #[tokio::test]
    async fn probe_with_wrong_host_id_gets_no_answer() {
        let (pt, pk) = punch_tokens("real-host");
        let (pi, po) = ping_tokens("real-host");
        let (hub, _refl) = UdpHub::bind_reflexive(
            "127.0.0.1:0".parse().unwrap(), &[], Duration::ZERO, pt, pk, pi, po,
        ).await.unwrap();
        let addr = hub.local_addr().unwrap().to_string();
        let rtt = probe_rtt("НЕ-тот-хост", &[addr], Duration::from_millis(400)).await;
        assert!(rtt.is_none(), "ответили на пробу с чужим идентификатором");
    }

    /// Мёртвый адрес и мусор на входе не должны ни падать, ни ждать дольше срока.
    #[tokio::test]
    async fn probe_on_dead_address_returns_none_in_time() {
        let started = std::time::Instant::now();
        // 127.0.0.1 со заведомо свободным портом — ответить некому.
        let rtt = probe_rtt("nobody", &["127.0.0.1:1".into()], Duration::from_millis(300)).await;
        assert!(rtt.is_none());
        assert!(started.elapsed() < Duration::from_secs(2), "проба не уложилась в свой срок");
        // Пустой и мусорный список адресов — сразу None, без сети.
        assert!(probe_rtt("x", &[], Duration::from_secs(5)).await.is_none());
        assert!(probe_rtt("x", &["не адрес".into()], Duration::from_secs(5)).await.is_none());
    }

    /// Хаб принимает гостя по PUNCH и доставляет его данные через пул буферов.
    /// Проверяет мультигость-путь end-to-end + переиспользование буферов demux.
    #[tokio::test]
    async fn hub_accepts_guest_and_delivers_via_pool() {
        let (pt, pk) = punch_tokens("hub-test");
        let (pi, po) = ping_tokens("hub-test");
        let (hub, _refl) = UdpHub::bind_reflexive(
            "127.0.0.1:0".parse().unwrap(), &[], Duration::ZERO, pt.clone(), pk.clone(), pi, po,
        ).await.unwrap();
        let hub_addr = hub.local_addr().unwrap();

        let guest = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        guest.send_to(&pt, hub_addr).await.unwrap(); // PUNCH → хаб заводит гостя

        let (_gaddr, link) = timeout(Duration::from_secs(2), hub.accept())
            .await.expect("accept не дождался").expect("хаб закрыт");

        // Гость шлёт данные; демукс кладёт их в пер-гостевую очередь через пул.
        guest.send_to(b"hello-world", hub_addr).await.unwrap();
        let mut buf = Vec::new();
        assert!(timeout(Duration::from_secs(2), link.recv_into(&mut buf)).await.unwrap().unwrap());
        assert_eq!(&buf, b"hello-world");

        // Ещё несколько пакетов — гоняем пул (буферы возвращаются и переиспользуются).
        for i in 0..5u8 {
            guest.send_to(&[i; 200], hub_addr).await.unwrap();
            let mut b = Vec::new();
            assert!(timeout(Duration::from_secs(2), link.recv_into(&mut b)).await.unwrap().unwrap());
            assert_eq!(b, vec![i; 200]);
        }
    }

    /// Токены пробивания: детерминированы, зависят от host_id, не ASCII-маркер.
    #[test]
    fn punch_tokens_are_per_host_and_stable() {
        let (p1, a1) = punch_tokens("net-ABC");
        let (p2, a2) = punch_tokens("net-ABC");
        assert_eq!((&p1, &a1), (&p2, &a2), "один host_id → одинаковые токены");
        let (p3, _) = punch_tokens("net-XYZ");
        assert_ne!(p1, p3, "разные host_id → разные токены");
        assert_ne!(p1, a1, "punch и pack различаются");
        assert_ne!(p1.as_slice(), b"BMV-PUNCH", "не открытый ASCII-маркер");
        assert_eq!(p1.len(), 8, "8-байтовый токен");
    }
}
