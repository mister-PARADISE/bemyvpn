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
use std::sync::atomic::{AtomicUsize, Ordering};
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
/// ЧЕТЫРЕ пер-хостовых токена одной сессии пробивания, выведенные из host_id.
///
/// Один Copy-тип вместо четырёх `Vec<u8>`, которые раньше поодиночке кочевали по
/// семи аргументам конструкторов (`#[allow(too_many_arguments)]` был именно про
/// это) и клонировались на каждого гостя. 32 байта на стеке — копия дешевле
/// счётчика ссылок, а перепутать местами `pack` и `pong` больше нельзя.
///
/// FNV-1a — детерминирован и СТАБИЛЕН между версиями/платформами (std
/// DefaultHasher этого НЕ гарантирует, а гость и хост могут быть на разных
/// сборках).
///
/// Зачем ping/pong отдельно от punch/pack: `PUNCH` на хосте заводит СЕССИЮ —
/// создаёт запись пира, канал и отдаёт «гостя» в accept-цикл. Такой «гость»
/// никогда не пожмёт руку, провисит до таймаута рукопожатия (6с) и всё это время
/// будет держать слот в воротах анти-флуда. Если каждый гость станет так мерить
/// задержку до всех хостов в списке, хосты захлебнутся ничем. Ответ на `ping` НЕ
/// СОЗДАЁТ НИЧЕГО: восемь байт пришло — восемь ушло (усиления нет, состояния нет).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PunchTokens {
    /// «Я пробиваюсь к тебе».
    pub punch: [u8; 8],
    /// «Слышу тебя» — подтверждение обратного пути.
    pub pack: [u8; 8],
    /// «Ты далеко?» — проба задержки, состояния не создаёт.
    pub ping: [u8; 8],
    /// Ответ на пробу.
    pub pong: [u8; 8],
}

impl PunchTokens {
    /// Вывести все четыре токена из идентификатора хоста.
    pub fn for_host(host_id: &str) -> Self {
        let t = |kind: &str| fnv1a(format!("{kind}:{host_id}").as_bytes()).to_le_bytes();
        PunchTokens {
            punch: t("bmv-punch"),
            pack: t("bmv-pack"),
            ping: t("bmv-ping"),
            pong: t("bmv-pong"),
        }
    }

    /// Это «хвост» фазы пробивания, а не данные? (punch/pack фильтруются в recv)
    fn is_punch_noise(&self, data: &[u8]) -> bool {
        data == self.punch || data == self.pack
    }
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
    let t = PunchTokens::for_host(host_id);
    // Свой одноразовый сокет: чужой (хаб хоста или туннель) трогать нельзя —
    // ответы на пробу смешались бы с рабочим трафиком.
    let sock = bind_udp("0.0.0.0:0".parse().ok()?).await.ok()?;
    let started = std::time::Instant::now();
    for a in &addrs {
        let _ = sock.send_to(&t.ping, a).await;
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
                if buf[..n] == t.pong && addrs.contains(&from) {
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

    /// ГОСТЬ: отдать Link после пробития. `tokens` — пер-хостовые токены фазы
    /// пробивания (их «хвост» recv отфильтрует, чтобы не принять за данные).
    /// `primed` — возможный первый пакет данных (вернётся первым `recv`).
    pub fn connect_primed(&self, peer: SocketAddr, primed: Option<Vec<u8>>, tokens: PunchTokens) -> UdpLink {
        UdpLink {
            sock: self.sock.clone(),
            peer,
            primed: Mutex::new(primed),
            scratch: Mutex::new(None),
            tokens,
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
        tokens: PunchTokens,
    ) -> Result<(SocketAddr, Option<Vec<u8>>)> {
        let (punch, pack) = (&tokens.punch[..], &tokens.pack[..]);
        let deadline = Instant::now() + window;
        let mut buf = vec![0u8; MAX_DATAGRAM];
        // Если получили PUNCH пира, но PACK ещё не пришёл — путь, вероятно, уже
        // открыт. Не возвращаемся сразу: продолжаем слать PUNCH, чтобы и НАШ
        // PUNCH дошёл до пира (за NAT это обязательно — иначе он нас не заведёт).
        let mut punched_peer: Option<SocketAddr> = None;
        // ОБРАТНОЕ ПРОБИТИЕ: реальные порты строгого (симметричного) хоста,
        // выученные из его контр-панча (STUN назвал ему другой порт). Кап — заслон
        // от флуда подставными адресами; дальше всё равно аутентифицирует Noise.
        let mut learned: Vec<SocketAddr> = Vec::new();
        const MAX_LEARNED: usize = 4;

        while Instant::now() < deadline {
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
                                // падаем в общий разбор ниже как валидный from
                            } else {
                                // Пакет с чужого IP (или не-PUNCH с чужого порта): отброс.
                                continue;
                            }
                        }
                        let data = &buf[..n];
                        if data == punch {
                            // Пир пробивается к нам: подтверждаем PACK'ом, но НЕ
                            // выходим — продолжаем слать свой PUNCH.
                            let _ = self.sock.send_to(pack, from).await;
                            punched_peer = Some(from);
                        } else if data == pack {
                            // Пир подтвердил — путь точно двусторонний.
                            return Ok((from, None));
                        } else {
                            // Пир уже перешёл к протоколу — путь есть, пакет не теряем.
                            return Ok((from, Some(data.to_vec())));
                        }
                    }
                    Ok(Err(e)) => return Err(e.into()),
                    Err(_) => break,
                }
            }
            // Видели PUNCH и продержали ещё раунд — считаем путь открытым.
            if let Some(peer) = punched_peer {
                return Ok((peer, None));
            }
        }
        Err(Error::Net("Не получилось соединиться с хостом напрямую. Попробуйте ещё раз или выберите другой.".into()))
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
    tokens: PunchTokens,
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
                    if self.tokens.is_punch_noise(data) {
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
    state: Arc<HubState>,
    /// Пер-хостовые токены (см. `PunchTokens`).
    tokens: PunchTokens,
}

/// Сколько держим «поручительство» координатора за адрес гостя. Гость пробивает
/// окном 12с; двух минут хватает и на очередь, и на повторную попытку, а дольше
/// держать незачем — это просто разрешение зайти, не сессия.
const VOUCH_TTL: Duration = Duration::from_secs(120);
/// Потолок таблицы поручительств (заслон памяти, если координатор сойдёт с ума).
const MAX_VOUCHED: usize = 1024;
/// Сколько пиров ОДНОВРЕМЕННО терпим БЕЗ поручительства координатора.
///
/// ПОЧЕМУ ВООБЩЕ КВОТА. Токен пробивания выводится из host_id, а он у публичного
/// хоста лежит в открытом каталоге; адрес источника в UDP подделывается. Раньше
/// каждый PUNCH от нового адреса сразу заводил пира и «гостя» в accept, и тот
/// занимал пермит ворот рукопожатия на 6 секунд: 4096 датаграмм по 8 байт (32 КБ!)
/// — и легальные гости не подключались минутами.
///
/// ПОЧЕМУ НЕ ЖЁСТКИЙ СПИСОК, А КВОТА. Гость всегда проходит через координатор, и
/// тот называет хосту его адрес (`UdpHub::punch`) — казалось бы, можно пускать
/// ТОЛЬКО названных. Но адрес, с которого гость реально приходит, не обязан
/// совпадать с наблюдённым координатором: у части операторских NAT (CGNAT с пулом
/// адресов) UDP-поток выходит с другого IP, чем TCP-сессия к координатору. Жёсткий
/// список выключил бы таким людям связь совсем.
///
/// ПОЧЕМУ НЕ COOKIE. Подтверждение обратного пути (ответить непредсказуемым
/// значением и требовать его эхо) — самый честный вариант, но он меняет ПРОВОД:
/// уже выпущенные гости не знают, что нужно вернуть cookie, и перестали бы
/// подключаться к обновлённым хостам. Квота даёт тот же эффект по живучести без
/// разрыва совместимости: флуд занимает максимум восемь мест, а гость, о котором
/// координатор предупредил, заходит поверх квоты.
const MAX_UNVOUCHED_PEERS: usize = 8;

/// Общее состояние хаба: кому раскладывать пакеты, кто представлен координатором
/// и сколько сейчас непредставленных. Делят demux, сам хаб и пер-гостевые Link'и.
struct HubState {
    peers: Mutex<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>,
    /// IP, о которых координатор сказал «к тебе идёт гость» + момент, когда сказал.
    /// Ключ — IP, а НЕ ip:port: за строгим (симметричным) NAT гость приходит с
    /// другого порта, чем назвал координатору, и по порту мы бы отсекли своего же.
    vouched: Mutex<HashMap<IpAddr, Instant>>,
    /// Сколько сейчас заведено пиров, за которых никто не ручался (см. квоту).
    unvouched: AtomicUsize,
    pool: BufPool,
}

impl HubState {
    /// Координатор назвал адреса ждущего гостя — с этого момента пускаем их
    /// PUNCH мимо квоты.
    fn vouch(&self, addrs: &[SocketAddr]) {
        let now = Instant::now();
        let mut v = self.vouched.lock();
        v.retain(|_, t| now.duration_since(*t) < VOUCH_TTL);
        for a in addrs {
            if v.len() >= MAX_VOUCHED && !v.contains_key(&a.ip()) {
                break; // таблица переполнена живыми записями — новых не берём
            }
            v.insert(a.ip(), now);
        }
    }

    /// Завести пира на адрес `from`, если он проходит допуск. `None` — отказ
    /// (потолок пиров или исчерпана квота непредставленных).
    fn admit(
        self: &Arc<Self>,
        from: SocketAddr,
        sock: &Arc<UdpSocket>,
        tokens: PunchTokens,
    ) -> Option<HubPeerLink> {
        let now = Instant::now();
        let vouched = self
            .vouched
            .lock()
            .get(&from.ip())
            .is_some_and(|t| now.duration_since(*t) < VOUCH_TTL);
        let mut peers = self.peers.lock();
        if peers.len() >= MAX_HUB_PEERS {
            return None;
        }
        if !vouched && self.unvouched.load(Ordering::Relaxed) >= MAX_UNVOUCHED_PEERS {
            return None;
        }
        let (tx, rx) = mpsc::channel(1024);
        peers.insert(from, tx);
        if !vouched {
            self.unvouched.fetch_add(1, Ordering::Relaxed);
        }
        Some(HubPeerLink {
            sock: sock.clone(),
            peer: from,
            rx: tokio::sync::Mutex::new(rx),
            state: self.clone(),
            tokens,
            vouched,
        })
    }
}

impl UdpHub {
    /// Забиндить хаб и УЗНАТЬ внешний адрес STUN'ом ДО запуска демультиплексора.
    /// Критично: если STUN'ить после старта demux, тот съест STUN-ответ и хост за
    /// NAT анонсирует только LAN-адрес → гости к нему не пробьются. Поэтому STUN —
    /// на «чистом» сокете, и лишь потом включаем демукс.
    pub async fn bind_reflexive(
        addr: SocketAddr,
        servers: &[String],
        wait: Duration,
        tokens: PunchTokens,
    ) -> Result<(Arc<Self>, Option<SocketAddr>)> {
        let sock = Arc::new(bind_udp(addr).await?);
        let reflexive = if wait.is_zero() {
            None
        } else {
            stun::reflexive_addr_on(&sock, servers, wait).await.ok()
        };
        let state = Arc::new(HubState {
            peers: Mutex::new(HashMap::new()),
            vouched: Mutex::new(HashMap::new()),
            unvouched: AtomicUsize::new(0),
            pool: Arc::new(Mutex::new(Vec::new())),
        });
        let (accept_tx, accept_rx) = mpsc::channel(32);
        tokio::spawn(demux(sock.clone(), state.clone(), accept_tx, tokens));
        let hub = Arc::new(UdpHub {
            sock,
            accept_rx: tokio::sync::Mutex::new(accept_rx),
            state,
            tokens,
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
        // Заодно это ПОРУЧИТЕЛЬСТВО: адреса пришли от координатора, значит их
        // PUNCH пускаем мимо квоты непредставленных (см. MAX_UNVOUCHED_PEERS).
        self.state.vouch(addrs);
        for a in addrs {
            let _ = self.sock.send_to(&self.tokens.punch, a).await;
        }
    }

    /// Дождаться следующего нового гостя. `None` — хаб закрыт.
    pub async fn accept(&self) -> Option<(SocketAddr, Box<dyn Link>)> {
        self.accept_rx.lock().await.recv().await
    }
}

/// Фоновый демультиплексор: раскидывает датаграммы по персональным очередям.
async fn demux(
    sock: Arc<UdpSocket>,
    state: Arc<HubState>,
    accept_tx: mpsc::Sender<(SocketAddr, Box<dyn Link>)>,
    tokens: PunchTokens,
) {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        let (n, from) = match sock.recv_from(&mut buf).await {
            Ok(x) => x,
            Err(_) => break,
        };
        let data = &buf[..n];
        let tx = state.peers.lock().get(&from).cloned();
        if let Some(tx) = tx {
            // Буфер из пула (не аллокация на пакет). Полна очередь → try_send
            // вернёт буфер в Err, кладём обратно в пул (для UDP дроп норм).
            match tx.try_send(pooled_copy(&state.pool, data)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(b)) | Err(mpsc::error::TrySendError::Closed(b)) => {
                    pool_return(&state.pool, b);
                }
            }
        } else if data == tokens.ping {
            // Проба задержки: отвечаем и НЕ заводим ни пира, ни канала, ни сессии
            // (см. PunchTokens). Ровно поэтому её можно звать для всего списка.
            let _ = sock.send_to(&tokens.pong, from).await;
        } else if data == tokens.punch {
            // Новый гость. Состояние заводим ТОЛЬКО если он проходит допуск
            // (поручительство координатора или свободная квота) — иначе флуд с
            // подделанных адресов выключал бы хост (см. MAX_UNVOUCHED_PEERS).
            if let Some(link) = state.admit(from, &sock, tokens) {
                // PACK — только когда гость ДЕЙСТВИТЕЛЬНО принят. Иначе он бы
                // счёл путь открытым, перестал пробиваться и молча висел бы до
                // таймаута рукопожатия; без PACK он продолжает стучаться и
                // зайдёт следующей попыткой, когда очередь разгребут.
                match accept_tx.try_send((from, Box::new(link) as Box<dyn Link>)) {
                    Ok(()) => {
                        let _ = sock.send_to(&tokens.pack, from).await;
                    }
                    // ОЧЕРЕДЬ ПОЛНА — гостя роняем, но приём НЕ ОСТАНАВЛИВАЕМ.
                    // Раньше здесь был `send().await`: пока верх не разберёт 32
                    // новых гостя, demux не читал сокет — и трафик ВСЕХ уже
                    // подключённых вставал. Дроп нового гостя дешевле: он
                    // продолжает пробиваться и зайдёт следующей попыткой.
                    // `Drop` у HubPeerLink сам снимет запись из peers и вернёт
                    // место в квоте — откат делать руками не нужно.
                    Err(mpsc::error::TrySendError::Full(_)) => {}
                    Err(mpsc::error::TrySendError::Closed(_)) => break, // хаб уронили
                }
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
    state: Arc<HubState>,
    tokens: PunchTokens,
    /// За этого гостя ручался координатор? От этого зависит, чьё место он занимал
    /// (квота непредставленных) и что надо вернуть на Drop.
    vouched: bool,
}

impl Drop for HubPeerLink {
    fn drop(&mut self) {
        self.state.peers.lock().remove(&self.peer);
        if !self.vouched {
            self.state.unvouched.fetch_sub(1, Ordering::Relaxed);
        }
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
                Some(d) if self.tokens.is_punch_noise(&d) => {
                    pool_return(&self.state.pool, d); // «хвост» пробивания — вернуть буфер
                    continue;
                }
                Some(d) => {
                    out.clear();
                    out.extend_from_slice(&d);
                    pool_return(&self.state.pool, d); // буфер отработал — назад в пул
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
        let t = PunchTokens::for_host("bye-udp-test");
        // Два UdpLink, направленных друг на друга (как гость↔хост после пробития).
        let la = a.connect_primed(b_addr, None, t);
        let lb = b.connect_primed(a_addr, None, t);
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
        let t = PunchTokens::for_host("loopback-host");

        // «STUN-сервер» шлёт мусор на A ДО начала пробития — он оседает в буфере.
        let stranger = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        stranger.send_to(b"stale-stun-binding-response", a_addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // B пробивается к A навстречу (те же токены).
        let b_task = tokio::spawn(async move {
            let _ = b.hole_punch(&[a_addr], Duration::from_secs(3), t).await;
        });
        // A должен соединиться с B (b_addr), а НЕ с адресом «STUN-сервера».
        let (peer, _) = a.hole_punch(&[b_addr], Duration::from_secs(3), t).await.unwrap();
        assert_eq!(peer, b_addr, "hole_punch поймал чужой пакет вместо пира");
        let _ = b_task.await;
    }


    /// Проба задержки обязана: (1) вернуть измеренное время, (2) НЕ ЗАВЕСТИ на
    /// хосте сессию. Второе — главное: если бы проба вела себя как PUNCH, каждый
    /// «пинг» вешал бы на хост мёртвого гостя на 6 секунд, и список из десятка
    /// хостов превращался бы в атаку на всех сразу.
    #[tokio::test]
    async fn probe_measures_rtt_without_creating_a_session() {
        let t = PunchTokens::for_host("probe-test");
        let (hub, _refl) = UdpHub::bind_reflexive(
            "127.0.0.1:0".parse().unwrap(), &[], Duration::ZERO, t,
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
        let t = PunchTokens::for_host("real-host");
        let (hub, _refl) = UdpHub::bind_reflexive(
            "127.0.0.1:0".parse().unwrap(), &[], Duration::ZERO, t,
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
        let t = PunchTokens::for_host("hub-test");
        let (hub, _refl) = UdpHub::bind_reflexive(
            "127.0.0.1:0".parse().unwrap(), &[], Duration::ZERO, t,
        ).await.unwrap();
        let hub_addr = hub.local_addr().unwrap();

        let guest = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        guest.send_to(&t.punch, hub_addr).await.unwrap(); // PUNCH → хаб заводит гостя

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

    /// ФЛУД PUNCH НЕ ДОЛЖЕН ВЫКЛЮЧАТЬ ХОСТ. Токен пробивания выводится из host_id,
    /// а он у публичного хоста лежит в открытом каталоге; адрес источника в UDP
    /// подделывается. Значит кто угодно может залить хаб PUNCH'ами с тысяч чужих
    /// адресов, и раньше КАЖДЫЙ заводил пира и «гостя» в accept — легальный гость
    /// уже не пробивался. Пиры без поручительства координатора обязаны иметь
    /// жёсткую квоту, а гость, о котором координатор предупредил, — проходить
    /// поверх этой квоты.
    #[tokio::test]
    async fn unvouched_punch_flood_cannot_lock_out_a_real_guest() {
        let t = PunchTokens::for_host("flood-test");
        let (hub, _refl) = UdpHub::bind_reflexive(
            "127.0.0.1:0".parse().unwrap(), &[], Duration::ZERO, t,
        ).await.unwrap();
        let hub_addr = hub.local_addr().unwrap();

        // Флуд: 24 «гостя», за которых никто не ручался.
        let mut flood = Vec::new();
        for _ in 0..24 {
            let s = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            s.send_to(&t.punch, hub_addr).await.unwrap();
            flood.push(s);
        }
        // Собираем всё, что хаб отдал (держим Link'и — они занимают квоту).
        let mut admitted = Vec::new();
        while let Ok(Some(g)) = timeout(Duration::from_millis(300), hub.accept()).await {
            admitted.push(g);
        }
        assert!(
            admitted.len() <= 8,
            "флуд без поручительства завёл {} сессий — хост выключается 32 КБ мусора",
            admitted.len()
        );

        // А вот НАСТОЯЩИЙ гость: координатор назвал хосту его адрес (host_serve_punch
        // зовёт hub.punch на кандидатах гостя). Он обязан пройти НЕСМОТРЯ на флуд.
        let real = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let real_addr = real.local_addr().unwrap();
        hub.punch(&[real_addr]).await;
        real.send_to(&t.punch, hub_addr).await.unwrap();
        let (got, _link) = timeout(Duration::from_secs(2), hub.accept())
            .await
            .expect("настоящий гость не принят: флуд занял все места")
            .expect("хаб закрыт");
        assert_eq!(got, real_addr);
    }

    /// ACCEPT НЕ ДОЛЖЕН БЛОКИРОВАТЬ ПРИЁМ. Очередь новых гостей — 32; пока верх её
    /// не разберёт, демультиплексор не читает сокет, то есть трафик ВСЕХ уже
    /// подключённых встаёт. Здесь accept намеренно не зовут: данные живого гостя
    /// обязаны идти дальше.
    #[tokio::test]
    async fn accept_backlog_does_not_stall_existing_guest() {
        let t = PunchTokens::for_host("backlog-test");
        let (hub, _refl) = UdpHub::bind_reflexive(
            "127.0.0.1:0".parse().unwrap(), &[], Duration::ZERO, t,
        ).await.unwrap();
        let hub_addr = hub.local_addr().unwrap();

        // Живой гость (координатор о нём предупредил — обычный путь).
        let guest = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        hub.punch(&[guest.local_addr().unwrap()]).await;
        guest.send_to(&t.punch, hub_addr).await.unwrap();
        let (_a, link) = timeout(Duration::from_secs(2), hub.accept())
            .await.expect("accept не дождался").expect("хаб закрыт");

        // 40 новых гостей подряд — больше, чем вмещает очередь accept (32).
        let mut newcomers = Vec::new();
        for _ in 0..40 {
            let s = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            hub.punch(&[s.local_addr().unwrap()]).await; // все «настоящие»
            s.send_to(&t.punch, hub_addr).await.unwrap();
            newcomers.push(s);
        }

        // accept НЕ зовём. Данные живого гостя обязаны дойти.
        guest.send_to(b"still-alive", hub_addr).await.unwrap();
        let mut buf = Vec::new();
        assert!(
            timeout(Duration::from_secs(2), link.recv_into(&mut buf)).await
                .expect("демультиплексор встал: очередь accept заблокировала приём").unwrap(),
        );
        assert_eq!(&buf, b"still-alive");
    }

    /// Токены пробивания: детерминированы, зависят от host_id, не ASCII-маркер.
    #[test]
    fn punch_tokens_are_per_host_and_stable() {
        let t1 = PunchTokens::for_host("net-ABC");
        let t2 = PunchTokens::for_host("net-ABC");
        assert_eq!(t1, t2, "один host_id → одинаковые токены");
        let t3 = PunchTokens::for_host("net-XYZ");
        assert_ne!(t1.punch, t3.punch, "разные host_id → разные токены");
        // Все четыре роли обязаны различаться: совпади punch с ping — проба
        // задержки заводила бы сессию, а это ровно то, чего мы избегаем.
        let all = [t1.punch, t1.pack, t1.ping, t1.pong];
        for i in 0..all.len() {
            for j in i + 1..all.len() {
                assert_ne!(all[i], all[j], "токены {i} и {j} совпали");
            }
        }
        assert_ne!(&t1.punch[..], b"BMV-PUNCH", "не открытый ASCII-маркер");
    }
}
