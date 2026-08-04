//! bmv-signal — клиент КООРДИНАТОРА поверх ЧИСТОГО WebSocket.
//!
//! Один персистентный сокет клиент→координатор (исходящий — NAT проходит без
//! хол-панча). Сокет = ЖИВОСТЬ хоста: закрылся → координатор убирает мгновенно.
//! Каталог и знакомство приходят пушем (дельты). Публичный API (методы
//! `Coordinator`) сохранён прежним — ядро/оболочки не переписываются, меняется
//! только транспорт (был JSON-над-HTTP, стал JSON-над-WS).
//!
//! Внутри: супервизор держит сокет и сам ПЕРЕПОДКЛЮЧАЕТСЯ, восстанавливая
//! состояние (повторный анонс хоста + подписка на каталог). RPC (newcode/whoami/
//! resolve/connect) коррелируются по id; каталог хранится локально из дельт.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bmv_common::{Error, Result};
use futures_util::{SinkExt, StreamExt};
// Мьютексы БЕЗ отравления (как в bmv-core/bmv-net): паника под локом иначе
// отравляла его навсегда, и КАЖДЫЙ следующий `.lock().unwrap()` паниковал бы
// снова — а выше по стеку стоит граница `extern "C"`, где паника = abort.
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

/// Анонс хоста координатору.
///
/// Здесь был `params: BTreeMap<String, String>` (плюс зеркальный `host_params` в
/// [`ConnectResponse`] и общий алиас `Params`) — «параметры протокола на будущее».
/// Карта всегда оставалась ПУСТОЙ: ни одна строчка проекта не клала в неё ключ.
/// У координатора этого поля больше нет вовсе — он его удалил, оставив надгробие
/// в коде: клиент слал, сервер хранил, а отдавал никому. При этом `params` не
/// имел `skip_serializing_if`, то есть `"params":{}` уезжало в КАЖДОМ анонсе и в
/// каждом восстановлении после реконнекта — чистый мусор на проводе.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HostAnnounce {
    pub id: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub public: bool,
    #[serde(default)]
    pub max_guests: u32,
    #[serde(default)]
    pub guests: u32,
    #[serde(default)]
    pub has_password: bool,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub code_sig: String,
}

/// Запрос гостя: узнать адреса хоста по его id + отдать свои (для встречного
/// пробития NAT хостом).
#[derive(Clone, Debug, Default, Serialize)]
pub struct GuestConnect {
    pub host_id: String,
    #[serde(default)]
    pub candidates: Vec<String>,
}

// Здесь был `PendingGuests { version, guests }` — «ответ хосту со списком
// ждущих гостей». Ни одного поля не пережило разбора: координатор не шлёт такой
// структуры вовсе (его кадр — `{"t":"guest","candidates":[...]}`, ровно один
// гость), `version` собиралась вручную из литерального нуля, а `guests` всегда
// содержала либо один набор, либо ноль по таймауту. Теперь см. `next_guest`.

/// Ответ координатора гостю: как достучаться до хоста.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConnectResponse {
    #[serde(default)]
    pub host_endpoints: Vec<String>,
    // `host_params` убран вместе с `HostAnnounce::params` — см. там. Он
    // конструировался всегда пустым и не читался НИ ОДНОЙ строчкой; координатор
    // в ответе `Connected` шлёт только `{id, endpoints}`.
}

/// Карточка хоста в каталоге.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct HostInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub ip: String,
    #[serde(default)]
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub public: bool,
    #[serde(default)]
    pub has_password: bool,
    #[serde(default, alias = "load")]
    pub guests: u32,
    #[serde(default)]
    pub max_guests: u32,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub protocol: String,
}

/// Ответ сервера на запрос нового кода: код + его подпись сервером.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct NewCode {
    pub code: String,
    #[serde(default)]
    pub sig: String,
}

/// Фильтр каталога.
///
/// Здесь был `public_only: bool` — протянутый через публичный API ядра
/// (`guest_list`/`guest_watch`), пять мест вызова и даже флаг `--public` в CLI.
/// На провод он не попадал НИКОГДА: `Filter` не сериализуется вовсе, кадр
/// подписки — `{"t":"watch"[,"country":"XX"]}`, и всё. И дело не в забытой
/// строчке: каталог координатора и так отдаёт ТОЛЬКО публичные хосты
/// (безусловный `filter(|e| e.ann.public)` на его стороне), поэтому фильтр
/// нечего было бы фильтровать. `--public true` и `--public false` давали
/// побайтово один и тот же список.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub country: Option<String>,
}

/// Снимок каталога (версия + список) — тот же контракт, что был у long-poll.
#[derive(Clone, Debug, Deserialize)]
pub struct DirectoryUpdate {
    pub version: u64,
    #[serde(default)]
    pub hosts: Vec<HostInfo>,
}

/// Таймауты RPC/подключения.
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(4);
// `PENDING_TIMEOUT` (20с) убран вместе с `pending()`: он изображал возврат
// long-poll'а по сроку, а ждать гостя по расписанию незачем — тот приходит
// пушем, и `next_guest` просто ждёт.

/// Потолок каталога на клиенте.
///
/// Сломанный (или враждебный) координатор может лить `diradd` бесконечно —
/// раньше клиент честно складывал всё в память до OOM, а на телефоне это смерть
/// приложения. Десять тысяч карточек (~3 МБ) на порядки больше любого реального
/// каталога, так что живой человек потолка не увидит.
const MAX_HOSTS: usize = 10_000;

/// Локальное состояние каталога, собранное из снапшота + дельт.
///
/// Список — `Vec`, потому что ПОРЯДОК ЗАДАЁТ СЕРВЕР (он сортирует по своему
/// `seq`, новые хосты приходят дельтой в конец): переложить каталог в map значило
/// бы перетасовать список у людей на каждом обновлении. Рядом — индекс id→позиция,
/// иначе каждая дельта искала бы линейно (O(n) на дельту, O(n²) на наполнение).
#[derive(Default)]
struct DirState {
    version: u64, // локальный монотонный счётчик (растёт на каждое изменение)
    hosts: Vec<HostInfo>,
    idx: HashMap<String, usize>,
}

impl DirState {
    /// Полный снимок от сервера — порядок берём как есть, лишнее отсекаем.
    fn replace_all(&mut self, mut hosts: Vec<HostInfo>) {
        hosts.truncate(MAX_HOSTS);
        self.hosts = hosts;
        self.reindex();
    }

    /// Дельта: обновить на месте (порядок сохраняется) или добавить в конец.
    fn upsert(&mut self, host: HostInfo) {
        match self.idx.get(&host.id) {
            Some(&i) => self.hosts[i] = host,
            None if self.hosts.len() < MAX_HOSTS => {
                self.idx.insert(host.id.clone(), self.hosts.len());
                self.hosts.push(host);
            }
            None => {} // потолок: молча игнорируем (лога в этом крейте нет)
        }
    }

    /// Удаление редкое, поэтому переиндексация целиком — честный O(n) вместо
    /// хитрой книги дырок, которую потом никто не разберёт.
    fn remove(&mut self, id: &str) {
        if self.idx.remove(id).is_some() {
            self.hosts.retain(|h| h.id != id);
            self.reindex();
        }
    }

    fn reindex(&mut self) {
        self.idx = self.hosts.iter().enumerate().map(|(i, h)| (h.id.clone(), i)).collect();
    }
}

/// Разделяемое состояние соединения.
struct Shared {
    ws_url: String,
    host: String, // хост координатора без схемы (для coord_host)
    started: AtomicBool,
    next_id: AtomicU64,
    /// Исходящие кадры (JSON-строки) → супервизор → сокет.
    ///
    /// В ОФФЛАЙНЕ СЮДА НЕ ПИШЕМ (см. `send_if_live`): супервизор в это время
    /// очередь не читает, и час без связи превращался в сотни протухших кадров,
    /// улетающих залпом уже ПОСЛЕ восстановительного анонса. Отдельно противное:
    /// застрявший `bye` доезжал после следующего старта раздачи и гасил её.
    out: mpsc::UnboundedSender<String>,
    out_rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
    /// Ожидающие ответы RPC по id.
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    /// Ответ на announce (hostok/error без id).
    host_ack: Mutex<Option<oneshot::Sender<Result<()>>>>,
    /// Очередь пришедших гостей (кандидаты) — для pending().
    guest_tx: mpsc::UnboundedSender<Vec<String>>,
    guest_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<String>>>,
    /// Каталог + сигнал изменения (для directory_watch).
    dir: Mutex<DirState>,
    dir_ver: watch::Sender<u64>,
    /// Счётчик подсказок «проверь соседа» от координатора (см. `peer_check`).
    peer_check: watch::Sender<u64>,
    /// Круг до координатора в миллисекундах, 0 — ещё не мерили ИЛИ связи нет
    /// (обнуляем на обрыве: бодрая цифра от мёртвого сокета — враньё на экране).
    ///
    /// Меряется НАСТОЯЩИМ обменом: свой Ping с меткой времени → Pong с той же
    /// меткой. Раньше «пинг» в интерфейсе брали из `health()`, а тот просто
    /// читает флаг «сокет жив» и возвращается мгновенно — на экране всегда
    /// стоял ноль, то есть время чтения переменной, а не время до сервера.
    rtt_ms: AtomicU32,
    /// Для восстановления после реконнекта.
    last_announce: Mutex<Option<HostAnnounce>>,
    /// Подписка на каталог — ВМЕСТЕ С ФИЛЬТРОМ, а не просто «подписан да/нет».
    /// Восстановление слало голый `watch`, и после первого же реконнекта человек
    /// с фильтром по стране получал весь каталог целиком.
    watching: Mutex<Option<Filter>>,
    /// true, когда сокет установлен (для health).
    connected: watch::Sender<bool>,
    /// true → все клоны Coordinator уронены, супервизору пора выйти (сокет закрыть).
    closed: watch::Receiver<bool>,
}

/// Гард жизни клиента: на Drop ПОСЛЕДНЕГО клона Coordinator сигналит супервизору
/// «выходи» — сокет закрывается, реконнект-цикл не живёт вечно. Без этого каждый
/// брошенный Coordinator утекал бы вечным супервизором с открытым коннектом.
struct LiveGuard {
    tx: watch::Sender<bool>,
}
impl Drop for LiveGuard {
    fn drop(&mut self) {
        let _ = self.tx.send(true);
    }
}

/// Клиент координатора (чистый WebSocket). Клонируется дёшево (общий сокет);
/// сокет закрывается, когда уронен последний клон.
pub struct Coordinator {
    shared: Arc<Shared>,
    _live: Arc<LiveGuard>,
}

impl Clone for Coordinator {
    fn clone(&self) -> Self {
        Coordinator { shared: self.shared.clone(), _live: self._live.clone() }
    }
}

/// Разобрать кадр «error» координатора в ошибку.
///
/// КОД ОТКАЗА БЕРЁМ ТОЖЕ. Раньше здесь читали один `reason`, а число (422 «до
/// вас не достучаться снаружи», 403 «код не наш») выбрасывали — при том, что
/// координатор его исправно шлёт. Все четыре оболочки искали это число
/// подстрокой в тексте ошибки и, разумеется, не находили: человек вместо
/// объяснения читал сырую строку сервера, а протухшая подпись кода не лечилась
/// сама, хотя код для самолечения написан в каждой оболочке.
fn refusal(v: &Value) -> Error {
    Error::Refused {
        code: v.get("code").and_then(|c| c.as_u64()).unwrap_or(0) as u16,
        reason: v
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("Сервер отклонил запрос. Попробуйте ещё раз.")
            .to_string(),
    }
}

impl Coordinator {
    pub fn new(base: impl Into<String>) -> Result<Self> {
        let base = base.into();
        let trimmed = base.trim_end_matches('/');
        let ws_url = {
            let u = if let Some(r) = trimmed.strip_prefix("https://") {
                format!("wss://{r}")
            } else if let Some(r) = trimmed.strip_prefix("http://") {
                format!("ws://{r}")
            } else {
                format!("wss://{trimmed}")
            };
            format!("{u}/v1/ws")
        };
        let host = trimmed
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_string();
        let (out, out_rx) = mpsc::unbounded_channel();
        let (guest_tx, guest_rx) = mpsc::unbounded_channel();
        let (dir_ver, _) = watch::channel(0u64);
        let (peer_check, _) = watch::channel(0u64);
        let (connected, _) = watch::channel(false);
        let (closed_tx, closed_rx) = watch::channel(false);
        Ok(Coordinator {
            shared: Arc::new(Shared {
                ws_url,
                host,
                started: AtomicBool::new(false),
                next_id: AtomicU64::new(1),
                out,
                out_rx: Mutex::new(Some(out_rx)),
                pending: Mutex::new(HashMap::new()),
                host_ack: Mutex::new(None),
                guest_tx,
                guest_rx: tokio::sync::Mutex::new(guest_rx),
                dir: Mutex::new(DirState::default()),
                dir_ver,
                peer_check,
                rtt_ms: AtomicU32::new(0),
                last_announce: Mutex::new(None),
                watching: Mutex::new(None),
                connected,
                closed: closed_rx,
            }),
            _live: Arc::new(LiveGuard { tx: closed_tx }),
        })
    }

    /// Запустить супервизор соединения (один раз). Он сам подключается и
    /// переподключается, восстанавливая анонс/подписку.
    fn ensure_started(&self) {
        if self.shared.started.swap(true, Ordering::SeqCst) {
            return;
        }
        // Крипто-провайдер rustls — ЯВНО и идемпотентно (Err, если уже стоит — ок).
        // Без этого на Android/iOS (где в дереве нет других rustls-потребителей)
        // первый wss:// РОНЯЛ приложение паникой «no process-level CryptoProvider».
        let _ = rustls::crypto::ring::default_provider().install_default();
        let sh = self.shared.clone();
        let rx = self.shared.out_rx.lock().take();
        if let Some(rx) = rx {
            tokio::spawn(supervisor(sh, rx));
        }
    }

    /// Отправить кадр, ТОЛЬКО пока сокет жив (см. `Shared::out`). В оффлайне кадр
    /// выбрасываем осознанно: состояние восстанавливается супервизором из
    /// `last_announce`/`watching`, так что терять нечего, а копить — вредно.
    fn send_if_live(&self, frame: String) {
        if *self.shared.connected.borrow() {
            let _ = self.shared.out.send(frame);
        }
    }

    /// Отправить кадр (в оффлайне — выбросить, см. `send_if_live`).
    fn send(&self, v: Value) {
        self.ensure_started();
        self.send_if_live(v.to_string());
    }

    /// RPC: послать сообщение с новым id и дождаться ответа по этому id.
    async fn rpc(&self, mut v: Value) -> Result<Value> {
        // Пока связи нет, кадр в очередь НЕ кладём: ждём короткое окно на
        // подключение (health) и, если его нет, отвечаем сразу. Иначе человек
        // смотрит на «приложение задумалось» все десять секунд таймаута, а
        // запрос всё это время лежит в мёртвой очереди.
        self.health().await?;
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        v["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().insert(id, tx);
        self.send_if_live(v.to_string());
        match tokio::time::timeout(RPC_TIMEOUT, rx).await {
            Ok(Ok(val)) => {
                if val.get("t").and_then(|t| t.as_str()) == Some("error") {
                    return Err(refusal(&val));
                }
                Ok(val)
            }
            // Сюда же попадает ОБРЫВ: `go_offline` роняет отправителя, и
            // ожидание кончается мгновенно, а не через десять секунд.
            _ => {
                self.shared.pending.lock().remove(&id);
                Err(Error::Signal("Сервер не ответил. Проверьте интернет и попробуйте ещё раз.".into()))
            }
        }
    }

    /// Быстрая проверка живости: подключились ли за короткий срок.
    /// Круг до координатора в мс. `None` — ещё не мерили или связь оборвана.
    pub fn rtt_ms(&self) -> Option<u32> {
        match self.shared.rtt_ms.load(Ordering::Relaxed) {
            0 => None,
            v => Some(v),
        }
    }

    pub async fn health(&self) -> Result<()> {
        self.ensure_started();
        if *self.shared.connected.subscribe().borrow() {
            return Ok(());
        }
        let mut c = self.shared.connected.subscribe();
        match tokio::time::timeout(HEALTH_TIMEOUT, async {
            loop {
                if *c.borrow() {
                    return;
                }
                if c.changed().await.is_err() {
                    return;
                }
            }
        })
        .await
        {
            Ok(()) if *self.shared.connected.subscribe().borrow() => Ok(()),
            _ => Err(Error::Signal("Нет связи с сервером. Проверьте интернет или адрес во вкладке «Сервер».".into())),
        }
    }

    /// ХОСТ: анонсировать/обновить себя (сообщение `host`). Открытый сокет = живость;
    /// ждём подтверждения `hostok` (или ошибку отклонения от координатора).
    pub async fn announce(&self, ann: &HostAnnounce) -> Result<()> {
        self.ensure_started();
        // Порядок важен: сначала запомнили анонс, потом отправили. Если сокет
        // сейчас лежит, кадр выбрасывается, но супервизор пошлёт ровно этот
        // анонс на (пере)подключении — подтверждение придёт в тот же `host_ack`.
        *self.shared.last_announce.lock() = Some(ann.clone());
        let (tx, rx) = oneshot::channel();
        *self.shared.host_ack.lock() = Some(tx);
        let mut v = serde_json::to_value(ann)
            .map_err(|_| Error::Signal("Не удалось отправить заявку о раздаче. Попробуйте ещё раз.".into()))?;
        v["t"] = json!("host");
        self.send_if_live(v.to_string());
        match tokio::time::timeout(RPC_TIMEOUT, rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e),
            _ => Err(Error::Signal("Сервер не ответил на заявку о раздаче. Попробуйте ещё раз.".into())),
        }
    }

    /// ПОДСКАЗКИ «проверь соседа» от координатора: счётчик растёт на каждую.
    ///
    /// Координатор шлёт её, когда сосед по паре отвалился ОТ НЕГО (закрыл сокет
    /// или прислал `bye`). Ценность в том, что его связь — TCP: он узнаёт об
    /// уходе даже там, где прямое прощание по UDP послать физически некому
    /// (приложение убили, процесс упал, сеть исчезла).
    ///
    /// Это ПОДСКАЗКА, А НЕ КОМАНДА разорвать сессию: получатель обязан сам
    /// опросить пира и решить (см. `bmv_common::Link::check_peer_now`). Иначе
    /// координатор — и всякий, кто сумеет к нему подделаться, — получил бы кнопку
    /// удалённого отключения чужого туннеля, при том что весь смысл продукта в
    /// том, что трафик через него не идёт.
    pub fn peer_check(&self) -> watch::Receiver<u64> {
        self.ensure_started();
        self.shared.peer_check.subscribe()
    }

    /// Хост координатора (без схемы/порта/пути).
    pub fn coord_host(&self) -> String {
        self.shared.host.clone()
    }

    /// ХОСТ: «я ухожу». Убираем восстановление (чтобы реконнект не воскресил) и
    /// шлём bye — координатор снимет запись мгновенно.
    ///
    /// Без аргументов: раньше принимал `id` и `token` и выбрасывал оба. Кто
    /// уходит — координатор знает по САМОМУ СОКЕТУ, на проводе кадр выглядит как
    /// `{"t":"bye"}` и никогда не нёс ни id, ни токена. Просить их у вызывающего
    /// значило обещать, что запись снимут по предъявлению секрета, — а на деле
    /// снимают по владению соединением.
    pub async fn bye(&self) -> Result<()> {
        *self.shared.last_announce.lock() = None;
        self.send(json!({ "t": "bye" }));
        Ok(())
    }

    /// ХОСТ: дождаться СЛЕДУЮЩЕГО ждущего гостя — его кандидатов для встречного
    /// пробития NAT. Ждёт сколько нужно; пустого ответа не бывает.
    ///
    /// Раньше это был `pending(id, since)`, возвращавший `PendingGuests` со
    /// списком гостей и «версией» — форма времён HTTP-опроса. От опроса не
    /// осталось ничего: координатор ТОЛКАЕТ гостя кадром `guest` в сокет, метод
    /// не отправлял на провод ни байта, оба аргумента выбрасывал, «версия»
    /// всегда была нулём (ядро исправно клало этот ноль обратно в `since`,
    /// который тоже выбрасывался), а 20-секундный таймаут существовал только
    /// затем, чтобы вызывающий крутил вокруг цикл. Теперь форма совпадает с
    /// сутью: один гость, одно ожидание.
    ///
    /// ЖДЁМ ПОД ЗАМКОМ, А ПРИЁМНИК НАРУЖУ НЕ ОТДАЁМ. Соблазн вернуть сам
    /// `Receiver` («пусть вызывающий крутит цикл сам») ломает вторую раздачу:
    /// приёмник у канала ОДИН, а `host_serve_punch` запускается на КАЖДЫЙ старт
    /// раздачи — второй раз забирать было бы уже нечего, и цикл молча замер бы
    /// навсегда. Здесь же приёмник остаётся в `Shared`, и повторный старт просто
    /// берёт замок заново (см. тест `a_second_hosting_start_still_gets_a_guest`).
    pub async fn next_guest(&self) -> Result<Vec<String>> {
        self.ensure_started();
        let mut rx = self.shared.guest_rx.lock().await;
        match rx.recv().await {
            Some(cands) => Ok(cands),
            None => Err(Error::Signal("Связь с сервером оборвалась — восстанавливаю.".into())),
        }
    }

    /// Узнать свой внешний IP через координатор.
    pub async fn my_ip(&self) -> Result<String> {
        let v = self.rpc(json!({ "t": "whoami" })).await?;
        Ok(v.get("addr").and_then(|x| x.as_str()).unwrap_or("").to_string())
    }

    /// ХОСТ: получить НОВЫЙ код от сервера (сервер генерит и подписывает).
    pub async fn new_code(&self) -> Result<NewCode> {
        let v = self.rpc(json!({ "t": "newcode" })).await?;
        Ok(NewCode {
            code: v.get("code").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            sig: v.get("sig").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        })
    }

    /// ГОСТЬ: найти ОДИН хост по коду (в т.ч. СКРЫТЫЙ). None — не найден.
    pub async fn resolve(&self, id: &str) -> Result<Option<HostInfo>> {
        let v = self.rpc(json!({ "t": "resolve", "code": id })).await?;
        match v.get("host") {
            Some(Value::Null) | None => Ok(None),
            Some(h) => Ok(serde_json::from_value(h.clone()).ok()),
        }
    }

    /// ГОСТЬ: текущий снимок каталога (подписываемся, если ещё нет).
    pub async fn directory(&self, filter: &Filter) -> Result<Vec<HostInfo>> {
        self.ensure_watch(filter);
        // Дадим первому снапшоту прийти, если только что подписались.
        let _ = tokio::time::timeout(CONNECT_TIMEOUT, self.wait_first_snapshot()).await;
        Ok(self.shared.dir.lock().hosts.clone())
    }

    /// ГОСТЬ: дождаться ИЗМЕНЕНИЯ каталога (since = прошлая версия). Возвращает
    /// снимок с новой версией. since=0 или устаревшая → отдаём сразу.
    pub async fn directory_watch(&self, filter: &Filter, since: u64) -> Result<DirectoryUpdate> {
        self.ensure_watch(filter);
        // СНАЧАЛА дождаться первого снапшота: свежий клиент иначе отдавал version=0,
        // и UI решал «нет связи», хотя мы просто ещё подключались.
        let _ = tokio::time::timeout(CONNECT_TIMEOUT, self.wait_first_snapshot()).await;
        if self.shared.dir_ver.subscribe().borrow().eq(&0) {
            return Err(Error::Signal("Нет связи с сервером. Проверьте интернет или адрес во вкладке «Сервер».".into())); // честно: не подключились
        }
        let mut c = self.shared.dir_ver.subscribe();
        if *c.borrow() == since {
            // Ждём следующего изменения (или подержим до таймаута — как long-poll).
            let _ = tokio::time::timeout(Duration::from_secs(25), c.changed()).await;
        }
        // Сокет сейчас лежит (обрыв/переподключение)? Не выдаём устаревший каталог
        // за правду — честная ошибка, UI покажет «нет связи» без миганий.
        if !*self.shared.connected.subscribe().borrow() {
            return Err(Error::Signal("Нет связи с сервером. Проверьте интернет или адрес во вкладке «Сервер».".into()));
        }
        let d = self.shared.dir.lock();
        Ok(DirectoryUpdate { version: d.version, hosts: d.hosts.clone() })
    }

    /// ГОСТЬ: попроситься к хосту, отдав кандидаты; получить адреса хоста.
    pub async fn connect(&self, req: &GuestConnect) -> Result<ConnectResponse> {
        let v = self
            .rpc(json!({ "t": "connect", "host_id": req.host_id, "candidates": req.candidates }))
            .await?;
        let endpoints: Vec<String> = v
            .get("endpoints")
            .and_then(|e| serde_json::from_value(e.clone()).ok())
            .unwrap_or_default();
        Ok(ConnectResponse { host_endpoints: endpoints })
    }

    // ── вспомогательное ──
    fn ensure_watch(&self, filter: &Filter) {
        self.ensure_started();
        let mut w = self.shared.watching.lock();
        if w.is_none() {
            *w = Some(filter.clone());
            drop(w); // лок снят ДО отправки: send не должен ждать под ним
            self.send(watch_frame(filter));
        }
    }

    async fn wait_first_snapshot(&self) {
        let mut c = self.shared.dir_ver.subscribe();
        while *c.borrow() == 0 {
            if c.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Кадр подписки на каталог.
fn watch_frame(filter: &Filter) -> Value {
    let mut v = json!({ "t": "watch" });
    if let Some(c) = &filter.country {
        v["country"] = json!(c);
    }
    v
}

/// Супервизор: держит сокет, читает/пишет, ПЕРЕПОДКЛЮЧАЕТСЯ и восстанавливает
/// состояние (повторный анонс + подписка). Один на Coordinator; ВЫХОДИТ (и
/// закрывает сокет), когда уронен последний клон Coordinator (сигнал `closed`).
/// Связь пропала: погасить флаг И РАЗБУДИТЬ ЖДУЩИХ ОТВЕТА RPC.
///
/// Без этого каждый RPC, отправленный до обрыва, честно висел все свои десять
/// секунд — ровно то, что человек видит как «приложение задумалось», хотя ответ
/// уже физически неоткуда взять: перепосылки RPC при реконнекте нет. Дроп
/// отправителя = мгновенная ошибка у ждущего.
///
/// А вот `host_ack` НЕ трогаем нарочно: анонс супервизор ПЕРЕПОСЫЛАЕТ сам из
/// `last_announce`, и `hostok` придёт в то же самое ожидание секундой позже.
/// Разбуди мы его ошибкой — короткое дрожание связи в момент старта раздачи
/// превратилось бы в «раздача не поднялась» на ровном месте.
///
/// Круг обнуляем: бодрая цифра от мёртвого сокета — враньё на экране.
fn go_offline(sh: &Shared) {
    let _ = sh.connected.send_replace(false);
    sh.rtt_ms.store(0, Ordering::Relaxed);
    sh.pending.lock().clear();
}

async fn supervisor(sh: Arc<Shared>, mut out_rx: mpsc::UnboundedReceiver<String>) {
    let mut closed = sh.closed.clone();
    // Бэкофф с джиттером: при рестарте координатора тысячи клиентов иначе ломятся
    // назад СИНХРОННО (каждые 2с) — «стадо», которое кладёт сервер снова. Растём
    // 0.5→8с и размазываем случайной добавкой; сбрасываем при удачном коннекте.
    let mut backoff = RECONNECT_MIN;
    loop {
        if *closed.borrow() {
            go_offline(&sh);
            return;
        }
        let stream = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(&sh.ws_url)).await {
            Ok(Ok((s, _))) => s,
            _ => {
                go_offline(&sh);
                // Пауза с джиттером — просыпаемся сразу, если клиент уронен.
                tokio::select! {
                    _ = tokio::time::sleep(with_jitter(backoff)) => {}
                    _ = closed.changed() => {}
                }
                backoff = (backoff * 2).min(RECONNECT_MAX);
                continue;
            }
        };
        backoff = RECONNECT_MIN; // подключились → сбрасываем бэкофф
        let _ = sh.connected.send_replace(true);
        let (mut sink, mut read) = stream.split();

        // Восстановление состояния на (пере)подключении. Локи снимаем ДО await
        // (иначе MutexGuard живёт через await → future не Send).
        let restore_ann = sh.last_announce.lock().clone();
        if let Some(ann) = restore_ann {
            if let Ok(mut v) = serde_json::to_value(&ann) {
                v["t"] = json!("host");
                let _ = sink.send(Message::Text(v.to_string())).await;
            }
        }
        // Подписку восстанавливаем С ТЕМ ЖЕ ФИЛЬТРОМ, с каким подписывались.
        let restore_watch = sh.watching.lock().clone();
        if let Some(filter) = restore_watch {
            let _ = sink.send(Message::Text(watch_frame(&filter).to_string())).await;
        }

        // Часы живости: любой входящий кадр (в т.ч. Ping/Pong) — признак жизни.
        // MissedTickBehavior::Delay: после заморозки процесса (сон, App Nap)
        // накопленные тики не выстреливают пачкой, а проверка идёт один раз —
        // и сразу видит огромную тишину, то есть будим связь мгновенно.
        let mut alive_iv = tokio::time::interval(LINK_CHECK_EVERY);
        alive_iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_rx = std::time::Instant::now();

        // Свой Ping для ЗАМЕРА круга. Отвечать на чужой Ping мы умели и раньше,
        // но это не даёт числа: чтобы узнать время до сервера, спрашивать должны
        // МЫ. В полезной нагрузке — метка времени; сервер обязан вернуть её в
        // Pong байт в байт (RFC 6455), по ней и считаем круг.
        let started = std::time::Instant::now();
        let mut rtt_iv = tokio::time::interval(RTT_EVERY);
        rtt_iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                out = out_rx.recv() => match out {
                    Some(frame) => {
                        if sink.send(Message::Text(frame)).await.is_err() { break; }
                    }
                    None => return, // канал исходящих закрыт
                },
                inc = read.next() => match inc {
                    Some(Ok(Message::Text(txt))) => { last_rx = std::time::Instant::now(); handle_incoming(&sh, &txt); }
                    Some(Ok(Message::Ping(p))) => { last_rx = std::time::Instant::now(); let _ = sink.send(Message::Pong(p)).await; }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(Message::Pong(p))) => {
                        last_rx = std::time::Instant::now();
                        // Считаем круг только по СВОЕЙ метке: чужие Pong (или
                        // ответ на давно устаревший Ping) дали бы чушь.
                        if p.len() == 8 {
                            let sent = u64::from_be_bytes(p[..8].try_into().unwrap());
                            let now = started.elapsed().as_millis() as u64;
                            if now >= sent {
                                let rtt = (now - sent).min(u32::MAX as u64) as u32;
                                sh.rtt_ms.store(rtt.max(1), Ordering::Relaxed);
                            }
                        }
                    }
                    _ => last_rx = std::time::Instant::now(), // прочие кадры — тоже жизнь
                },
                // Тишина дольше дедлайна → сокет мёртв, рвём и переподключаемся.
                // Реконнект восстановит host/watch и пришлёт СВЕЖИЙ снапшот каталога.
                _ = alive_iv.tick() => {
                    if last_rx.elapsed() > LINK_SILENCE_LIMIT { break; }
                }
                _ = rtt_iv.tick() => {
                    let mark = (started.elapsed().as_millis() as u64).to_be_bytes().to_vec();
                    if sink.send(Message::Ping(mark)).await.is_err() { break; }
                }
                // Последний клон Coordinator уронен → аккуратно закрываем сокет и выходим
                // (хост при этом мгновенно исчезает из каталога — «прощание» = закрытие).
                _ = closed.changed() => {
                    let _ = sink.send(Message::Close(None)).await;
                    go_offline(&sh);
                    return;
                }
            }
        }
        go_offline(&sh);
        // Сорвались — короткая пауза с джиттером (не синхронно со всеми), затем
        // переподключаемся и восстанавливаем состояние. Реагируем на закрытие сразу.
        tokio::select! {
            _ = tokio::time::sleep(with_jitter(RECONNECT_MIN)) => {}
            _ = closed.changed() => { go_offline(&sh); return; }
        }
    }
}

/// Границы бэкоффа реконнекта.
const RECONNECT_MIN: Duration = Duration::from_millis(500);
/// Верхняя граница ужата с 8с: человек смотрит на «нет связи» и ждёт, а не
/// экономит нам трафик. Восемь секунд между попытками читались как «зависло».
const RECONNECT_MAX: Duration = Duration::from_secs(3);

/// Ничего не пришло дольше этого → считаем сокет мёртвым и переподключаемся.
///
/// Без такой проверки клиент висел на `read.next()` бесконечно: сокет умирает
/// ТИХО (ноутбук уснул, сменилась сеть, NAT выкинул трансляцию) — FIN/RST не
/// приходит, читатель не просыпается. Итог: `connected` остаётся true, дельты
/// каталога не идут, счётчики гостей и список хостов заморожены навсегда.
/// Координатор шлёт Ping каждые 3с (WS_PING_INTERVAL), и с тех пор, как мы шлём
/// СВОЙ Ping для замера круга (RTT_EVERY), кадры идут в обе стороны втрое чаще.
/// Поэтому дедлайн ужат с 8с до 6с — это по-прежнему два подряд не дошедших
/// пинга, то есть на ложные срабатывания запас тот же, а обрыв замечается
/// раньше. Ниже дедлайна координатора (WS_PONG_DEADLINE) — и правильно: чинить
/// связь должен клиент, а не ждать, пока сервер выкинет его первым.
const LINK_SILENCE_LIMIT: Duration = Duration::from_secs(6);
/// Как часто сверяем тишину. Секунда, а не две: проверка дешёвая (сравнение
/// времени), а две секунды добавлялись к ожиданию ни за что.
const LINK_CHECK_EVERY: Duration = Duration::from_secs(1);
/// Как часто меряем круг до координатора.
///
/// Секунда, как и отклик до хостов: цифра должна дышать одинаково везде, а не
/// обновляться на одном экране втрое реже, чем на соседнем. Кадр Ping — это
/// несколько байт, на такой частоте это ничто.
const RTT_EVERY: Duration = Duration::from_secs(1);

/// Прибавить к паузе случайный джиттер до +100% (размазать «стадо» реконнектов).
/// rand не тянем — дешёвого псевдослучая из наносекунд часов хватает для джиттера.
fn with_jitter(base: Duration) -> Duration {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let extra = (base.as_millis() as u64).saturating_mul(nanos as u64 % 101) / 100; // 0..100%
    base + Duration::from_millis(extra)
}

/// Разобрать одно серверное сообщение и разложить по адресатам.
fn handle_incoming(sh: &Arc<Shared>, txt: &str) {
    let Ok(v) = serde_json::from_str::<Value>(txt) else { return };
    let t = v.get("t").and_then(|t| t.as_str()).unwrap_or("");
    match t {
        // Ответы RPC по id.
        "code" | "ip" | "resolved" | "connected" => {
            if let Some(id) = v.get("id").and_then(|x| x.as_u64()) {
                let waiter = sh.pending.lock().remove(&id);
                if let Some(tx) = waiter {
                    let _ = tx.send(v);
                }
            }
        }
        "error" => {
            let id = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
            if id != 0 {
                let waiter = sh.pending.lock().remove(&id);
                if let Some(tx) = waiter {
                    let _ = tx.send(v);
                }
            } else {
                let ack = sh.host_ack.lock().take();
                if let Some(tx) = ack {
                    let _ = tx.send(Err(refusal(&v)));
                }
            }
        }
        "hostok" => {
            let ack = sh.host_ack.lock().take();
            if let Some(tx) = ack {
                let _ = tx.send(Ok(()));
            }
        }
        // ПОДСКАЗКА «проверь соседа»: сосед по паре отвалился ОТ КООРДИНАТОРА.
        // Не команда рвать туннель (см. `Coordinator::peer_check`) — просто повод
        // немедленно опросить пира.
        "peercheck" => {
            sh.peer_check.send_modify(|v| *v += 1);
        }
        "guest" => {
            if let Some(c) = v.get("candidates").and_then(|c| serde_json::from_value::<Vec<String>>(c.clone()).ok()) {
                let _ = sh.guest_tx.send(c);
            }
        }
        "dirfull" => {
            if let Some(hosts) = v.get("hosts").and_then(|h| serde_json::from_value::<Vec<HostInfo>>(h.clone()).ok()) {
                let mut d = sh.dir.lock();
                d.replace_all(hosts);
                d.version += 1;
                let _ = sh.dir_ver.send_replace(d.version);
            }
        }
        "diradd" | "dirupdate" => {
            if let Some(host) = v.get("host").and_then(|h| serde_json::from_value::<HostInfo>(h.clone()).ok()) {
                let mut d = sh.dir.lock();
                d.upsert(host);
                d.version += 1;
                let _ = sh.dir_ver.send_replace(d.version);
            }
        }
        "dirremove" => {
            if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
                let mut d = sh.dir.lock();
                d.remove(id);
                d.version += 1;
                let _ = sh.dir_ver.send_replace(d.version);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Клиент, не поднимающий сокет: нужен только ради `Shared` под тесты
    /// маршрутизации (`handle_incoming` синхронна и сети не требует).
    fn offline_client() -> Coordinator {
        Coordinator::new("http://127.0.0.1:1").unwrap()
    }

    fn host_frame(t: &str, id: &str, name: &str) -> String {
        json!({ "t": t, "host": { "id": id, "name": name } }).to_string()
    }

    /// КОД ОТКАЗА ОБЯЗАН ДОЕХАТЬ ДО ОБОЛОЧКИ, а текст — остаться человеческим.
    ///
    /// Раньше здесь брали один `reason`, а число выбрасывали. Оболочки искали
    /// его подстрокой в тексте (`contains("422")`) и не находили никогда: вместо
    /// «ваша сеть не пропускает гостей внутрь» человек читал сырую строку
    /// сервера, а протухшая подпись кода (403) не лечилась сама. Ловушка тихая —
    /// всё компилируется и работает, просто ни одна ветка не срабатывает.
    #[test]
    fn refusal_carries_code_and_shows_only_human_text() {
        let e = refusal(&json!({ "t": "error", "code": 422, "reason": "Ваша сеть не пропускает гостей внутрь." }));
        assert_eq!(e.refusal_code(), Some(422), "код отказа потерян — ветки в оболочках снова мертвы");
        assert_eq!(e.to_string(), "Ваша сеть не пропускает гостей внутрь.", "в текст для человека подмешалось лишнее");

        // Кадр без кода — не отказ по коду, но текст всё равно есть.
        let e = refusal(&json!({ "t": "error" }));
        assert_eq!(e.refusal_code(), Some(0));
        assert!(!e.to_string().is_empty(), "молчаливый отказ: человеку нечего прочитать");
    }

    /// ЗВЕНО ВТОРОГО СЛОЯ ПРОЩАНИЯ. Координатор шлёт `{"t":"peercheck"}`, когда
    /// сосед по паре отвалился ОТ НЕГО (см. серверный
    /// `a_dying_host_makes_the_coordinator_hint_its_guest`). Здесь кадр обязан
    /// превратиться в подсказку «опроси пира немедленно».
    ///
    /// Ветка не была покрыта ничем: её можно было удалить целиком — подсказка
    /// не доходила бы никогда, слоёв обнаружения оставалось два вместо трёх, а
    /// падало ноль тестов. Имя кадра здесь и на сервере — одна и та же строка,
    /// так что переименование ломает ровно один из двух тестов.
    #[tokio::test]
    async fn a_peercheck_frame_becomes_a_liveness_probe() {
        let c = offline_client();
        let mut rx = c.peer_check();
        assert!(!rx.has_changed().unwrap(), "подсказка появилась до кадра");

        handle_incoming(&c.shared, &json!({ "t": "peercheck" }).to_string());

        assert!(rx.has_changed().unwrap(), "кадр «peercheck» разобран молча — сессия узнает об уходе пира только по тишине");
        assert_eq!(*rx.borrow_and_update(), 1);
    }

    /// ПОРЯДОК КАТАЛОГА ЗАДАЁТ СЕРВЕР. Дельты не должны его перетасовывать:
    /// обновление правит карточку НА МЕСТЕ, новые уходят в конец, удаление не
    /// сдвигает остальных друг относительно друга.
    #[test]
    fn directory_deltas_keep_server_order() {
        let c = offline_client();
        let sh = &c.shared;
        handle_incoming(sh, &json!({ "t": "dirfull", "hosts": [
            { "id": "a", "name": "первый" },
            { "id": "b", "name": "второй" },
            { "id": "c", "name": "третий" },
        ] }).to_string());
        handle_incoming(sh, &host_frame("dirupdate", "a", "первый-обновлён"));
        handle_incoming(sh, &host_frame("diradd", "d", "четвёртый"));
        handle_incoming(sh, &json!({ "t": "dirremove", "id": "b" }).to_string());

        let d = sh.dir.lock();
        let ids: Vec<&str> = d.hosts.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["a", "c", "d"], "порядок сервера обязан сохраниться");
        assert_eq!(d.hosts[0].name, "первый-обновлён", "dirupdate правит карточку на месте");
        // Индекс обязан оставаться согласованным со списком — иначе следующая
        // дельта запишет чужую карточку поверх соседней.
        for (i, h) in d.hosts.iter().enumerate() {
            assert_eq!(d.idx.get(&h.id), Some(&i), "индекс разъехался на {}", h.id);
        }
        assert_eq!(d.idx.len(), d.hosts.len(), "в индексе остался мусор от удалённых");
    }

    /// Сломанный (или враждебный) сервер льёт `diradd` без конца — память клиента
    /// не должна расти вместе с его фантазией.
    #[test]
    fn directory_is_capped() {
        let c = offline_client();
        let sh = &c.shared;
        for i in 0..(MAX_HOSTS + 50) {
            handle_incoming(sh, &host_frame("diradd", &format!("h{i}"), "x"));
        }
        assert_eq!(sh.dir.lock().hosts.len(), MAX_HOSTS, "потолок каталога не держится");
        // Уже известный хост поверх потолка обязан обновляться (это не рост).
        handle_incoming(sh, &host_frame("dirupdate", "h0", "обновлён"));
        assert_eq!(sh.dir.lock().hosts[0].name, "обновлён");
    }

    /// Ответы RPC разбираются по id, а подтверждение анонса — по `hostok`.
    #[tokio::test]
    async fn incoming_routes_answers_to_waiters() {
        let c = offline_client();
        let sh = &c.shared;
        let (tx, rx) = oneshot::channel();
        sh.pending.lock().insert(7, tx);
        let (atx, arx) = oneshot::channel();
        *sh.host_ack.lock() = Some(atx);

        handle_incoming(sh, &json!({ "t": "ip", "id": 7, "addr": "1.2.3.4" }).to_string());
        handle_incoming(sh, &json!({ "t": "hostok" }).to_string());

        let v = rx.await.expect("ответ не доехал до ждущего RPC");
        assert_eq!(v.get("addr").unwrap(), "1.2.3.4");
        assert!(arx.await.unwrap().is_ok(), "hostok обязан подтвердить анонс");
        assert!(sh.pending.lock().is_empty(), "ожидающий не убран из карты");
    }

    /// Ошибка БЕЗ id — это отказ по анонсу, а не потерянный ответ RPC.
    #[tokio::test]
    async fn incoming_error_without_id_rejects_announce() {
        let c = offline_client();
        let (tx, rx) = oneshot::channel();
        *c.shared.host_ack.lock() = Some(tx);
        handle_incoming(&c.shared, &json!({ "t": "error", "reason": "код устарел" }).to_string());
        let err = rx.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("код устарел"), "причина отказа обязана дойти до хоста: {err}");
    }

    /// ОБРЫВ БУДИТ ЖДУЩИХ. Пока этого не было, каждый запрос после обрыва висел
    /// все десять секунд таймаута — это и есть «приложение задумалось».
    #[tokio::test]
    async fn disconnect_wakes_pending_rpc_and_clears_rtt() {
        let c = offline_client();
        let sh = &c.shared;
        sh.rtt_ms.store(42, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        sh.pending.lock().insert(1, tx);
        let (atx, mut arx) = oneshot::channel::<Result<()>>();
        *sh.host_ack.lock() = Some(atx);

        go_offline(sh);

        assert!(rx.await.is_err(), "ждущий RPC обязан очнуться на обрыве, а не досиживать таймаут");
        assert_eq!(sh.rtt_ms.load(Ordering::Relaxed), 0, "круг от мёртвого сокета обязан обнулиться");
        assert!(sh.pending.lock().is_empty());
        // Анонс ждёт дальше: его перепошлёт супервизор на реконнекте, и `hostok`
        // придёт в это же ожидание (иначе дрожание связи ломало бы старт раздачи).
        assert!(arx.try_recv().is_err(), "ожидание анонса рвать нельзя — оно переживает реконнект");
        handle_incoming(sh, &json!({ "t": "hostok" }).to_string());
        assert!(arx.await.unwrap().is_ok(), "подтверждение после реконнекта обязано дойти");
    }

    /// В ОФФЛАЙНЕ НЕ БУФЕРИЗУЕМ. Иначе час без связи = сотни протухших кадров,
    /// которые улетят залпом при реконнекте (и `bye` доедет после нового старта).
    ///
    /// Здесь же закреплена ФОРМА `bye` на проводе. Зовём настоящий `c.bye()`, а
    /// не пишем кадр руками: раньше тест собирал `{"t":"bye"}` сам и потому не
    /// покрывал метод, у которого как раз и убрали два аргумента (`id`, `token`).
    /// Кто уходит — координатор знает по сокету, и в кадре этого никогда не было.
    #[tokio::test]
    async fn offline_frames_are_dropped_not_queued() {
        let c = offline_client();
        let mut rx = c.shared.out_rx.lock().take().expect("очередь исходящих");
        c.send_if_live(json!({ "t": "watch" }).to_string());
        c.bye().await.unwrap();
        assert!(rx.try_recv().is_err(), "в оффлайне очередь исходящих обязана остаться пустой");
        // А на живом сокете кадр обязан уходить (иначе «фикс» — просто заглушка).
        let _ = c.shared.connected.send_replace(true);
        c.bye().await.unwrap();
        assert_eq!(
            rx.try_recv().unwrap(),
            json!({ "t": "bye" }).to_string(),
            "кадр bye изменился: он не нёс ни id, ни токена"
        );
    }

    /// ВТОРОЙ СТАРТ РАЗДАЧИ В ТОМ ЖЕ ПРОЦЕССЕ ВСЁ ЕЩЁ ПОЛУЧАЕТ ГОСТЯ.
    ///
    /// Ловит ровно одну опасность, и она не теоретическая. Соблазн при переходе
    /// с опроса на пуш — отдать наружу сам `Receiver` («пусть цикл встречного
    /// пробития крутит его сам, без замка»). Приёмник у mpsc ОДИН: первый же
    /// `take()` опустошил бы `Shared`, и `host_serve_punch`, который запускается
    /// НА КАЖДЫЙ старт раздачи, со второго раза не получил бы ничего. Отказ
    /// тихий — ни ошибки, ни лога: раздача работает, гости за строгим NAT просто
    /// перестают подключаться, потому что встречный PUNCH больше не уходит.
    ///
    /// Поэтому здесь проигрывается жизненный цикл целиком: первый старт встаёт в
    /// ожидание, оболочка гасит его задачу (`h.abort()` — так делают и GUI, и
    /// TUI, и FFI при остановке раздачи), затем стартует второй и обязан гостя
    /// дождаться. Заодно проверяется, что снятая задача ОТПУСКАЕТ замок: ждём мы
    /// теперь без таймаута, и залипший guard подвесил бы вторую раздачу навсегда.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_hosting_start_still_gets_a_guest() {
        let c = offline_client();

        // ПЕРВЫЙ старт раздачи: цикл встречного пробития ждёт гостя под замком.
        let first = {
            let c = c.clone();
            tokio::spawn(async move { c.next_guest().await })
        };
        // Даём задаче дойти до ожидания (иначе снимем её раньше взятия замка).
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Человек остановил раздачу — оболочка снимает задачу.
        first.abort();
        let _ = first.await;

        // ВТОРОЙ старт. Координатор толкает ждущего гостя в сокет.
        handle_incoming(
            &c.shared,
            &json!({ "t": "guest", "candidates": ["203.0.113.9:41000"] }).to_string(),
        );

        let cands = tokio::time::timeout(Duration::from_secs(5), c.next_guest())
            .await
            .expect("вторая раздача не дождалась гостя: приёмник забрали или замок не отпущен")
            .expect("канал гостей закрыт");
        assert_eq!(cands, vec!["203.0.113.9:41000".to_string()]);
    }

    /// ПОСЛЕ РЕКОННЕКТА ФИЛЬТР НЕ ТЕРЯЕТСЯ. Восстановление слало голый `watch`,
    /// и человек, выбравший страну, после первого же обрыва получал весь каталог.
    /// Сервер тут рвёт первое соединение сразу после подписки.
    #[tokio::test(flavor = "multi_thread")]
    async fn reconnect_restores_watch_with_filter() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else { return };
                    // Первый текстовый кадр — это подписка; забираем и рвём связь.
                    while let Some(Ok(msg)) = ws.next().await {
                        if let Message::Text(t) = msg {
                            let _ = tx.send(t);
                            break;
                        }
                    }
                    // Уронив сокет, заставляем клиента переподключиться.
                });
            }
        });

        let c = Coordinator::new(format!("http://127.0.0.1:{port}")).unwrap();
        let filter = Filter { country: Some("NL".into()) };
        let bg = c.clone();
        tokio::spawn(async move {
            let _ = bg.directory(&filter).await;
        });

        for n in 1..=2 {
            let frame = tokio::time::timeout(Duration::from_secs(15), rx.recv())
                .await
                .unwrap_or_else(|_| panic!("подписка №{n} не пришла — реконнект не восстановил watch"))
                .expect("канал закрыт");
            let v: Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(v["t"], "watch", "кадр №{n} — не подписка: {frame}");
            assert_eq!(v["country"], "NL", "фильтр по стране потерян в кадре №{n}: {frame}");
        }
    }

    /// ТИХО умерший сокет обязан приводить к переподключению.
    ///
    /// Ноутбук уснул, сменилась сеть, NAT выкинул трансляцию — FIN/RST не
    /// приходит, `read.next()` не просыпается. Раньше клиент висел в таком
    /// состоянии вечно: `connected` = true, дельт нет, каталог заморожен
    /// (счётчики гостей врут, ушедшие хосты остаются в списке). Тест поднимает
    /// координатора-молчуна: он принимает рукопожатие и НЕ шлёт ничего, даже
    /// пингов. Второе подключение возможно, только если клиент сам заметил
    /// тишину по LINK_SILENCE_LIMIT.
    #[tokio::test(flavor = "multi_thread")]
    async fn silent_socket_triggers_reconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, mut rx) = mpsc::unbounded_channel::<()>();

        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    if let Ok(ws) = tokio_tungstenite::accept_async(sock).await {
                        let _ = tx.send(()); // подключение состоялось
                        let _keep = ws; // держим сокет открытым и МОЛЧИМ
                        std::future::pending::<()>().await;
                    }
                });
            }
        });

        let c = Coordinator::new(format!("http://127.0.0.1:{port}")).unwrap();
        let _ = c.health().await; // разбудить ленивый супервизор

        tokio::time::timeout(Duration::from_secs(15), rx.recv())
            .await
            .expect("клиент не подключился к тестовому координатору")
            .expect("канал закрыт");

        // Таймаут НАМЕРЕННО литеральный, а не LINK_SILENCE_LIMIT + запас: иначе
        // при поломке самой константы тест не падает, а виснет на её значении.
        tokio::time::timeout(Duration::from_secs(25), rx.recv())
            .await
            .expect("клиент НЕ переподключился при полной тишине — дедлайн живости не работает")
            .expect("канал закрыт");
    }
}
