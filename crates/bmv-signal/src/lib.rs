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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bmv_common::{Error, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

pub type Params = std::collections::BTreeMap<String, String>;

/// Анонс хоста координатору.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HostAnnounce {
    pub id: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub params: Params,
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

/// Ответ хосту: список ждущих гостей (каждый — набор своих адресов).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PendingGuests {
    pub version: u64,
    #[serde(default)]
    pub guests: Vec<Vec<String>>,
}

/// Ответ координатора гостю: как достучаться до хоста.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConnectResponse {
    #[serde(default)]
    pub host_endpoints: Vec<String>,
    #[serde(default)]
    pub host_params: Params,
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
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub country: Option<String>,
    pub public_only: bool,
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
/// Как долго ждём гостя в pending() прежде чем вернуть пусто (мимик long-poll).
const PENDING_TIMEOUT: Duration = Duration::from_secs(20);

/// Локальное состояние каталога, собранное из снапшота + дельт.
#[derive(Default)]
struct DirState {
    version: u64, // локальный монотонный счётчик (растёт на каждое изменение)
    hosts: Vec<HostInfo>,
}

/// Разделяемое состояние соединения.
struct Shared {
    ws_url: String,
    host: String, // хост координатора без схемы (для coord_host)
    started: AtomicBool,
    next_id: AtomicU64,
    /// Исходящие кадры (JSON-строки) → супервизор → сокет. Буферизуются, если
    /// соединение сейчас переустанавливается.
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
    /// Для восстановления после реконнекта.
    last_announce: Mutex<Option<HostAnnounce>>,
    watching: Mutex<bool>,
    /// Сведения о свежем релизе от координатора: ПРОВЕРЕННЫЙ манифест.
    /// Храним уже разобранным — значит подпись сошлась; непроверенное сюда не
    /// попадает вовсе, чтобы выше по стеку не было соблазна ему поверить.
    update: Mutex<Option<bmv_common::update::Manifest>>,
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
                update: Mutex::new(None),
                last_announce: Mutex::new(None),
                watching: Mutex::new(false),
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
        let rx = self.shared.out_rx.lock().unwrap().take();
        if let Some(rx) = rx {
            tokio::spawn(supervisor(sh, rx));
        }
    }

    /// Отправить кадр (буферизуется, если соединение переустанавливается).
    fn send(&self, v: Value) {
        self.ensure_started();
        let _ = self.shared.out.send(v.to_string());
    }

    /// RPC: послать сообщение с новым id и дождаться ответа по этому id.
    async fn rpc(&self, mut v: Value) -> Result<Value> {
        self.ensure_started();
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        v["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(id, tx);
        let _ = self.shared.out.send(v.to_string());
        match tokio::time::timeout(RPC_TIMEOUT, rx).await {
            Ok(Ok(val)) => {
                if val.get("t").and_then(|t| t.as_str()) == Some("error") {
                    let reason = val.get("reason").and_then(|r| r.as_str()).unwrap_or("ошибка");
                    return Err(Error::Signal(reason.to_string()));
                }
                Ok(val)
            }
            _ => {
                self.shared.pending.lock().unwrap().remove(&id);
                Err(Error::Signal("координатор не ответил".into()))
            }
        }
    }

    /// Быстрая проверка живости: подключились ли за короткий срок.
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
            _ => Err(Error::Signal("координатор недоступен".into())),
        }
    }

    /// ХОСТ: анонсировать/обновить себя (сообщение `host`). Открытый сокет = живость;
    /// ждём подтверждения `hostok` (или ошибку отклонения от координатора).
    pub async fn announce(&self, ann: &HostAnnounce) -> Result<()> {
        self.ensure_started();
        *self.shared.last_announce.lock().unwrap() = Some(ann.clone());
        let (tx, rx) = oneshot::channel();
        *self.shared.host_ack.lock().unwrap() = Some(tx);
        let mut v = serde_json::to_value(ann).map_err(|e| Error::Signal(e.to_string()))?;
        v["t"] = json!("host");
        let _ = self.shared.out.send(v.to_string());
        match tokio::time::timeout(RPC_TIMEOUT, rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e),
            _ => Err(Error::Signal("анонс: координатор не ответил".into())),
        }
    }

    /// Хост координатора (без схемы/порта/пути).
    /// Свежий релиз, если координатор о нём сообщил И подпись сошлась.
    /// None — обновлять нечего, либо манифест не проверен (в том числе подделан).
    pub fn latest_update(&self) -> Option<bmv_common::update::Manifest> {
        self.shared.update.lock().unwrap().clone()
    }

    pub fn coord_host(&self) -> String {
        self.shared.host.clone()
    }

    /// ХОСТ: «я ухожу». Убираем восстановление (чтобы реконнект не воскресил) и
    /// шлём bye — координатор снимет запись мгновенно.
    pub async fn bye(&self, _id: &str, _token: &str) -> Result<()> {
        *self.shared.last_announce.lock().unwrap() = None;
        self.send(json!({ "t": "bye" }));
        Ok(())
    }

    /// ХОСТ: дождаться следующего ждущего гостя (или пусто по таймауту). Гости
    /// приходят пушем `guest` в сокет — здесь просто снимаем из очереди.
    pub async fn pending(&self, _id: &str, _since: u64) -> Result<PendingGuests> {
        self.ensure_started();
        let mut rx = self.shared.guest_rx.lock().await;
        match tokio::time::timeout(PENDING_TIMEOUT, rx.recv()).await {
            Ok(Some(cands)) => Ok(PendingGuests { version: 0, guests: vec![cands] }),
            Ok(None) => Err(Error::Signal("канал гостей закрыт".into())),
            Err(_) => Ok(PendingGuests { version: 0, guests: vec![] }),
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
        Ok(self.shared.dir.lock().unwrap().hosts.clone())
    }

    /// ГОСТЬ: дождаться ИЗМЕНЕНИЯ каталога (since = прошлая версия). Возвращает
    /// снимок с новой версией. since=0 или устаревшая → отдаём сразу.
    pub async fn directory_watch(&self, filter: &Filter, since: u64) -> Result<DirectoryUpdate> {
        self.ensure_watch(filter);
        // СНАЧАЛА дождаться первого снапшота: свежий клиент иначе отдавал version=0,
        // и UI решал «нет связи», хотя мы просто ещё подключались.
        let _ = tokio::time::timeout(CONNECT_TIMEOUT, self.wait_first_snapshot()).await;
        if self.shared.dir_ver.subscribe().borrow().eq(&0) {
            return Err(Error::Signal("координатор недоступен".into())); // честно: не подключились
        }
        let mut c = self.shared.dir_ver.subscribe();
        if *c.borrow() == since {
            // Ждём следующего изменения (или подержим до таймаута — как long-poll).
            let _ = tokio::time::timeout(Duration::from_secs(25), c.changed()).await;
        }
        // Сокет сейчас лежит (обрыв/переподключение)? Не выдаём устаревший каталог
        // за правду — честная ошибка, UI покажет «нет связи» без миганий.
        if !*self.shared.connected.subscribe().borrow() {
            return Err(Error::Signal("координатор недоступен".into()));
        }
        let d = self.shared.dir.lock().unwrap();
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
        Ok(ConnectResponse { host_endpoints: endpoints, host_params: Params::new() })
    }

    // ── вспомогательное ──
    fn ensure_watch(&self, filter: &Filter) {
        self.ensure_started();
        let mut w = self.shared.watching.lock().unwrap();
        if !*w {
            *w = true;
            drop(w);
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
async fn supervisor(sh: Arc<Shared>, mut out_rx: mpsc::UnboundedReceiver<String>) {
    let mut closed = sh.closed.clone();
    // Бэкофф с джиттером: при рестарте координатора тысячи клиентов иначе ломятся
    // назад СИНХРОННО (каждые 2с) — «стадо», которое кладёт сервер снова. Растём
    // 0.5→8с и размазываем случайной добавкой; сбрасываем при удачном коннекте.
    let mut backoff = RECONNECT_MIN;
    loop {
        if *closed.borrow() {
            let _ = sh.connected.send_replace(false);
            return;
        }
        let stream = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(&sh.ws_url)).await {
            Ok(Ok((s, _))) => s,
            _ => {
                let _ = sh.connected.send_replace(false);
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
        let restore_ann = sh.last_announce.lock().unwrap().clone();
        if let Some(ann) = restore_ann {
            if let Ok(mut v) = serde_json::to_value(&ann) {
                v["t"] = json!("host");
                let _ = sink.send(Message::Text(v.to_string())).await;
            }
        }
        let restore_watch = *sh.watching.lock().unwrap();
        if restore_watch {
            let _ = sink.send(Message::Text(json!({ "t": "watch" }).to_string())).await;
        }

        // Часы живости: любой входящий кадр (в т.ч. Ping/Pong) — признак жизни.
        // MissedTickBehavior::Delay: после заморозки процесса (сон, App Nap)
        // накопленные тики не выстреливают пачкой, а проверка идёт один раз —
        // и сразу видит огромную тишину, то есть будим связь мгновенно.
        let mut alive_iv = tokio::time::interval(LINK_CHECK_EVERY);
        alive_iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_rx = std::time::Instant::now();

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
                    _ => last_rx = std::time::Instant::now(), // Pong и прочие кадры — тоже жизнь
                },
                // Тишина дольше дедлайна → сокет мёртв, рвём и переподключаемся.
                // Реконнект восстановит host/watch и пришлёт СВЕЖИЙ снапшот каталога.
                _ = alive_iv.tick() => {
                    if last_rx.elapsed() > LINK_SILENCE_LIMIT { break; }
                }
                // Последний клон Coordinator уронен → аккуратно закрываем сокет и выходим
                // (хост при этом мгновенно исчезает из каталога — «прощание» = закрытие).
                _ = closed.changed() => {
                    let _ = sink.send(Message::Close(None)).await;
                    let _ = sh.connected.send_replace(false);
                    return;
                }
            }
        }
        let _ = sh.connected.send_replace(false);
        // Сорвались — короткая пауза с джиттером (не синхронно со всеми), затем
        // переподключаемся и восстанавливаем состояние. Реагируем на закрытие сразу.
        tokio::select! {
            _ = tokio::time::sleep(with_jitter(RECONNECT_MIN)) => {}
            _ = closed.changed() => { let _ = sh.connected.send_replace(false); return; }
        }
    }
}

/// Границы бэкоффа реконнекта.
const RECONNECT_MIN: Duration = Duration::from_millis(500);
const RECONNECT_MAX: Duration = Duration::from_secs(8);

/// Ничего не пришло дольше этого → считаем сокет мёртвым и переподключаемся.
///
/// Без такой проверки клиент висел на `read.next()` бесконечно: сокет умирает
/// ТИХО (ноутбук уснул, сменилась сеть, NAT выкинул трансляцию) — FIN/RST не
/// приходит, читатель не просыпается. Итог: `connected` остаётся true, дельты
/// каталога не идут, счётчики гостей и список хостов заморожены навсегда.
/// Координатор шлёт Ping каждые 3с (WS_PING_INTERVAL), так что тишина в 8с —
/// это два подряд не дошедших пинга. Столько же держит и сам координатор
/// (WS_PONG_DEADLINE): обе стороны считают связь мёртвой примерно одновременно.
const LINK_SILENCE_LIMIT: Duration = Duration::from_secs(8);
/// Как часто сверяем тишину. Реже дедлайна — проверка дешёвая, но не суетливая.
const LINK_CHECK_EVERY: Duration = Duration::from_secs(2);

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
                if let Some(tx) = sh.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(v);
                }
            }
        }
        "error" => {
            let id = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
            if id != 0 {
                if let Some(tx) = sh.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(v);
                }
            } else if let Some(tx) = sh.host_ack.lock().unwrap().take() {
                let reason = v.get("reason").and_then(|r| r.as_str()).unwrap_or("отклонено");
                let _ = tx.send(Err(Error::Signal(reason.to_string())));
            }
        }
        "hostok" => {
            if let Some(tx) = sh.host_ack.lock().unwrap().take() {
                let _ = tx.send(Ok(()));
            }
        }
        "guest" => {
            if let Some(c) = v.get("candidates").and_then(|c| serde_json::from_value::<Vec<String>>(c.clone()).ok()) {
                let _ = sh.guest_tx.send(c);
            }
        }
        "dirfull" => {
            if let Some(hosts) = v.get("hosts").and_then(|h| serde_json::from_value::<Vec<HostInfo>>(h.clone()).ok()) {
                let mut d = sh.dir.lock().unwrap();
                d.hosts = hosts;
                d.version += 1;
                let _ = sh.dir_ver.send_replace(d.version);
            }
        }
        "diradd" | "dirupdate" => {
            if let Some(host) = v.get("host").and_then(|h| serde_json::from_value::<HostInfo>(h.clone()).ok()) {
                let mut d = sh.dir.lock().unwrap();
                if let Some(e) = d.hosts.iter_mut().find(|x| x.id == host.id) {
                    *e = host;
                } else {
                    d.hosts.push(host);
                }
                d.version += 1;
                let _ = sh.dir_ver.send_replace(d.version);
            }
        }
        // Манифест обновления. Подпись проверяем ЗДЕСЬ, до сохранения:
        // координатор — обычный посредник, доверия ему нет, и подделать
        // манифест он не может именно потому, что проверка идёт вшитым ключом.
        "update" => {
            let (Some(m), Some(s)) = (
                v.get("manifest").and_then(|x| x.as_str()),
                v.get("sig").and_then(|x| x.as_str()),
            ) else { return };
            // Не сошлось — молча игнорируем: это либо клиент с другим ключом,
            // либо попытка подмены. В обоих случаях делать нечего.
            if let Ok(man) = bmv_common::update::verify_manifest(m.as_bytes(), s) {
                *sh.update.lock().unwrap() = Some(man);
            }
        }
        "dirremove" => {
            if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
                let mut d = sh.dir.lock().unwrap();
                d.hosts.retain(|x| x.id != id);
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
