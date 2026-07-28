//! BeMyVPN Coordinator — «главный сервер»: справочник хостов + знакомство.
//!
//! Что делает:
//!   • держит КАТАЛОГ живых хостов (кто раздаёт, где, публичный/приватный…);
//!   • сводит гостя с хостом (обмен кандидатами и параметрами протокола);
//!   • и уходит — в ТРАФИК НЕ ЛЕЗЕТ, приватных ключей НЕ хранит.
//!
//! Данные только в памяти: хост живёт РОВНО пока держит WebSocket. Любой может
//! поднять свой координатор этим бинарём.
//!
//! Весь протокол — JSON-сообщения по ОДНОМУ WebSocket (`GET /v1/ws`). Координатор
//! «открывает сокет» (как любой сайт), клиент/хост к нему подключаются (исходящее
//! соединение проходит NAT без хол-панча) и не открывают ничего у себя:
//!   • Хост    → регистрируется (`host`) и держит сокет; закрылся → убран мгновенно.
//!   • Гость   → подписывается (`watch`): снапшот каталога + дельты пушем в реальном
//!               времени; просит хоста (`connect`) — его кандидаты летят хосту в сокет.
//!   • Сервис  → `newcode` (выдать код), `whoami` (свой внешний IP), `resolve` (найти
//!               по коду, в т.ч. скрытый).
//! Живость = сам сокет: событие закрытия ловит краш/выход мгновенно, WS-пинг ловит
//! «тихую смерть». Никакого HTTP-API и никаких опросов больше нет.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::Mutex;

type HmacSha256 = Hmac<Sha256>;

/// Подписать код секретом сервера (HMAC-SHA256, усечён до 128 бит = 32 hex).
/// Сервер — ЕДИНСТВЕННЫЙ, кто знает секрет, поэтому только он может выдать код.
fn sign_code(secret: &[u8], code: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC принимает ключ любой длины");
    mac.update(code.as_bytes());
    hex::encode(&mac.finalize().into_bytes()[..16])
}

/// Проверить подпись кода (в постоянное время). true → код выдан этим сервером.
fn verify_code(secret: &[u8], code: &str, sig: &str) -> bool {
    let expected = sign_code(secret, code);
    if expected.len() != sig.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(sig.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Загрузить СТАБИЛЬНЫЙ секрет подписи кодов. Приоритет: env `BMV_CODE_SECRET`
/// (hex) → файл `BMV_CODE_SECRET_FILE` (по умолчанию `bmv-coordinator.secret`
/// рядом) → сгенерировать и сохранить в файл. Стабильность важна: сменится
/// секрет — все ранее выданные коды станут недействительны. Если файл записать
/// нельзя (напр. встроенный координатор на телефоне) — секрет живёт в памяти
/// (коды действительны в пределах запуска процесса).
fn load_code_secret() -> Vec<u8> {
    use rand::Rng;
    if let Ok(h) = std::env::var("BMV_CODE_SECRET") {
        if let Ok(b) = hex::decode(h.trim()) {
            if b.len() >= 16 {
                return b;
            }
        }
    }
    let path = std::env::var("BMV_CODE_SECRET_FILE")
        .unwrap_or_else(|_| "bmv-coordinator.secret".to_string());
    if let Ok(h) = std::fs::read_to_string(&path) {
        if let Ok(b) = hex::decode(h.trim()) {
            if b.len() >= 16 {
                return b;
            }
        }
    }
    let mut secret = vec![0u8; 32];
    rand::thread_rng().fill(&mut secret[..]);
    if std::fs::write(&path, hex::encode(&secret)).is_ok() {
        tracing::info!(%path, "секрет подписи кодов создан и сохранён");
    } else {
        tracing::warn!("секрет подписи кодов не удалось сохранить — живёт в памяти (коды сбросятся при перезапуске)");
    }
    secret
}

/// Бэкстоп-TTL: WS-хост убирается МГНОВЕННО при закрытии сокета; это лишь страховка
/// на случай записи, которая почему-то осталась без живого сокета (не должно быть).
const HOST_TTL: Duration = Duration::from_secs(30);

// ── лимиты (анти-DDoS / анти-флуд / анти-мусор) ──────────────────────────────
// WS-заслон живёт на самом соединении (пер-IP лимит коннектов + размер сообщения).
// Здесь — потолки каталога и полей анонса: заслон от Sybil/мусора/амплификации.

/// Потолок числа хостов в каталоге (защита памяти от Sybil-флуда новых id).
const MAX_HOSTS: usize = 5000;

// Потолки полей анонса/коннекта — режем мусор и амплификацию в каталоге.
const MAX_ID: usize = 64;
const MAX_NAME: usize = 64;
const MAX_COUNTRY: usize = 16;
const MAX_PROTOCOL: usize = 16;
const MAX_TOKEN: usize = 128;
const MAX_ENDPOINTS: usize = 8;
const MAX_CANDIDATES: usize = 8;
const MAX_ADDR: usize = 64; // длина строки ip:port
const MAX_PARAMS: usize = 8;
const MAX_PARAM_LEN: usize = 128;

/// Отфильтровать адрес-кандидат: строго `ip:port`, БЕЗ петли/мультикаста/
/// Оставляем ТОЛЬКО глобально маршрутизируемые адреса. Приватные/LAN (192.168,
/// 10.x, 172.16-31, link-local 169.254), CGNAT (100.64/10), loopback, мультикаст
/// и пр. ВЫКИДЫВАЕМ: снаружи они недостижимы — гость до такого хоста не дозвонится,
/// а в каталоге они лишь мусорят «ложным» IP. Координатор — единственное место
/// этой проверки (клиенты не дублируют). Заодно перекрывает reflection-амплификацию.
fn sane_addr(s: &str) -> Option<String> {
    if s.len() > MAX_ADDR {
        return None;
    }
    let sa: SocketAddr = s.parse().ok()?;
    if sa.port() == 0 {
        return None;
    }
    let bad = match sa.ip() {
        IpAddr::V4(a) => {
            let o = a.octets();
            let cgnat = o[0] == 100 && (o[1] & 0xC0) == 0x40; // 100.64.0.0/10 (CGNAT)
            a.is_loopback() || a.is_unspecified() || a.is_multicast() || a.is_broadcast()
                || a.is_documentation() || a.is_private() || a.is_link_local() || cgnat
        }
        IpAddr::V6(a) => {
            let s0 = a.segments()[0];
            let unique_local = (s0 & 0xfe00) == 0xfc00; // fc00::/7
            let link_local = (s0 & 0xffc0) == 0xfe80; // fe80::/10
            a.is_loopback() || a.is_unspecified() || a.is_multicast() || unique_local || link_local
        }
    };
    if bad {
        None
    } else {
        Some(sa.to_string())
    }
}

/// СЕРВЕР-АВТОРИТЕТ НАД IP: адрес хоста в каталоге = тот, с которого он РЕАЛЬНО
/// пришёл (наблюдаемый HTTP-источник), а не что он о себе написал. Иначе можно
/// «сделать сеть» с чужим/красивым IP (спуфинг 8.8.8.8): гость бы ломился в никуда.
/// Порт доверяем (его координатор со стороны HTTP не видит — это рефлексивный порт
/// NAT), но IP ставим свой, наблюдаемый. IPv6-эндпоинты (когда HTTP пришёл по IPv4)
/// не трогаем — их источник не наблюдаем этим соединением.
fn authorize_endpoints(endpoints: &[String], observed: &str) -> Vec<String> {
    let obs: Option<Ipv4Addr> = observed.parse().ok();
    let mut out: Vec<String> = Vec::new();
    for ep in endpoints {
        let Ok(sa) = ep.parse::<SocketAddr>() else { continue };
        let fixed = match (sa.ip(), obs) {
            (IpAddr::V4(_), Some(o)) => SocketAddr::new(IpAddr::V4(o), sa.port()).to_string(),
            _ => ep.clone(),
        };
        if !out.contains(&fixed) {
            out.push(fixed);
        }
    }
    out
}

/// Обрезать/проверить строку по длине (пустую оставляем пустой).
fn clamp(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

// ── модель ───────────────────────────────────────────────────────────────────

type Params = std::collections::BTreeMap<String, String>;

/// Анонс хоста (что он о себе сообщает).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct HostAnnounce {
    id: String,
    #[serde(default)]
    token: String, // секрет владельца записи (НЕ показывается в каталоге)
    #[serde(default)]
    name: String, // человекочитаемое имя для UI
    #[serde(default)]
    params: Params, // параметры протокола (публичные: pubkey и т.п.)
    #[serde(default)]
    endpoints: Vec<String>, // кандидаты ip:port (host + srflx)
    #[serde(default)]
    country: String,
    #[serde(default)]
    public: bool,
    #[serde(default = "one")]
    max_guests: u32,
    #[serde(default)]
    guests: u32, // подключено гостей сейчас (реальное число от хоста)
    #[serde(default)]
    has_password: bool, // приватный ли (сам пароль сюда НЕ шлём)
    #[serde(default)]
    protocol: String, // plain/noise/noise-aes — гость подключается тем же
    #[serde(default)]
    code_sig: String, // подпись кода сервером (HMAC) — нужна при ПЕРВОМ клейме id
}

fn one() -> u32 {
    1
}

/// Запись в реестре координатора.
struct HostEntry {
    ann: HostAnnounce,
    /// Секрет владельца, зафиксированный при ПЕРВОМ анонсе (TOFU). Пустой →
    /// «незаклеймленная» легаси-запись (совместимость со старыми хостами без
    /// токена — их может обновить кто угодно, как раньше). Непустой → менять и
    /// снимать запись может только владелец этим же токеном.
    owner: String,
    last_seen: Instant,
    /// Публичный IP, каким координатор ВИДИТ хост на HTTP-соединении (не то, что
    /// хост о себе сообщил). Это авторитетный адрес для показа/страны в каталоге:
    /// хост за NAT/хотспотом показывает свой реальный внешний IP, а не 192.168.
    observed_ip: String,
    /// Порядковый номер ПЕРВОГО клейма (стабилен через heartbeat/re-announce).
    /// Каталог сортируется по нему: у всех клиентов одинаковый порядок, старые
    /// хосты не прыгают, новые встают в конец. Без этого HashMap отдавал хаос
    /// (hash-порядок) — свежезапущенный хост «вставал поверх» чужого в списке.
    seq: u64,
    /// Хост держит ОТКРЫТЫЙ WebSocket к координатору → жив «по факту сокета»:
    /// смерть ловится МГНОВЕННО по событию закрытия (краш/выход), а «тихая смерть»
    /// (завис/пропал сигнал) — WS-пингом. Закрылся сокет → запись сразу убирается.
    /// Это ЕДИНСТВЕННЫЙ сигнал живости: живой в каталоге == держит сокет.
    ws_live: bool,
}

impl HostEntry {
    /// Ещё в памяти — бэкстоп по TTL. Штатно запись живёт ровно пока открыт сокет
    /// (на закрытии удаляется мгновенно); TTL — лишь страховка от «повисшей» записи.
    fn alive(&self) -> bool {
        self.ws_live || self.last_seen.elapsed() < HOST_TTL
    }
    /// ЖИВ ли хост (показывать ли в каталоге) — держит ли WS-сокет. Сервер решает сам.
    fn live(&self) -> bool {
        self.ws_live
    }
}

/// Общее состояние: реестр хостов + версия каталога для long-poll.
/// Версия растёт при каждом ВИДИМОМ изменении каталога (не на каждый heartbeat) —
/// подписчики просыпаются только когда есть что показать.
struct AppState {
    db: Mutex<HashMap<String, HostEntry>>,
    version: tokio::sync::watch::Sender<u64>,
    /// Флаг «каталог изменился». Версию каталога бампаем НЕ сразу, а раз в ~600мс,
    /// если флаг взведён — иначе всплеск частых анонсов (мигающий reflexive, утёкшие
    /// heartbeat'ы) будил бы клиентов по 7 раз/сек, и список мельтешил.
    dir_dirty: std::sync::atomic::AtomicBool,
    /// Секрет подписи кодов (HMAC). Сервер — единственный источник кодов.
    code_secret: Vec<u8>,
    /// Счётчик порядковых номеров для HostEntry.seq (порядок каталога).
    next_seq: std::sync::atomic::AtomicU64,
    /// WS-хосты: host_id → канал доставки кандидатов гостя ПРЯМО в его сокет
    /// (мгновенный relay). Есть запись → хост онлайн по WS.
    ws_hosts: Mutex<HashMap<String, tokio::sync::mpsc::Sender<Vec<String>>>>,
    /// Счётчик открытых WS-коннектов на IP — заслон от исчерпания сокетов флудом.
    ws_conns: std::sync::Mutex<HashMap<IpAddr, u32>>,
    /// Кэш манифеста обновления (перечитывается по mtime, см. load_update).
    update: std::sync::Mutex<UpdateInfo>,
}

impl AppState {
    /// Пометить каталог изменённым — реальный бамп сделает дебаунс-таск (~раз в
    /// 300мс). Для ЧАСТЫХ шумных событий (ре-анонс/heartbeat, мигающий reflexive),
    /// чтобы список не мельтешил.
    fn bump(&self) {
        self.dir_dirty
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    /// МГНОВЕННЫЙ бамп версии — для ДИСКРЕТНЫХ событий (новый хост появился / ушёл):
    /// их видно у всех сразу, без дебаунс-задержки. Появление и исчезновение теперь
    /// одинаково быстрые (раньше add ждал дебаунс, а remove на закрытии — почти нет).
    fn bump_now(&self) {
        self.dir_dirty.store(false, std::sync::atomic::Ordering::SeqCst);
        self.version.send_modify(|v| *v += 1);
    }
}

type Db = Arc<AppState>;

// ── ответы ───────────────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
struct DirectoryItem {
    id: String,
    name: String,
    country: String,
    ip: String, // публичный IP как ВИДИТ координатор — по нему клиент рисует флаг/страну
    endpoints: Vec<String>, // кандидаты для пробития (могут включать srflx)
    public: bool,
    has_password: bool,
    guests: u32,
    max_guests: u32,
    online: bool,
    protocol: String,
}

#[derive(Deserialize, Default)]
struct DirectoryQuery {
    /// Необязательный фильтр по стране (гость смотрит только нужную).
    #[serde(default)]
    country: Option<String>,
    // Каталог и так отдаёт ТОЛЬКО публичные хосты (скрытые доступны лишь по коду).
}

// ── хендлеры ─────────────────────────────────────────────────────────────────

/// Реальный IP клиента. Если запрос пришёл с LOOPBACK — перед нами наш локальный
/// обратный прокси (Caddy/nginx): снаружи до 127.0.0.1 не достучаться, значит
/// X-Forwarded-For поставил прокси, и ему можно верить. Берём ПОСЛЕДНИЙ адрес в
/// XFF — его добавляет прокси (реальный клиент); левые записи клиент может
/// подставить сам, но правую перезапишет прокси. Для прямого доступа доверяем
/// XFF только по явному `BMV_TRUST_XFF=1` (иначе заголовок подделывается кем угодно).
fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
    let trust = peer.ip().is_loopback()
        || std::env::var("BMV_TRUST_XFF").map(|v| v == "1" || v == "true").unwrap_or(false);
    if trust {
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next_back())
            .and_then(|s| s.trim().parse::<IpAddr>().ok())
        {
            return ip;
        }
        if let Some(ip) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse().ok()) {
            return ip;
        }
    }
    peer.ip()
}

/// ЯДРО РЕГИСТРАЦИИ ХОСТА (вызывается на WS-сообщение Host). Валидирует,
/// санитизирует, авторизует IP, применяет анти-угон/анти-пустышку и кладёт/обновляет
/// запись. `observed_ip` — реальный IP WS-соединения, это авторитет над адресом хоста.
/// Возвращает (статус, visible_changed). `visible_changed` = новый хост ИЛИ
/// изменилось видимое поле (число гостей и т.п.) → вызывающий бампает МГНОВЕННО.
async fn register_host(state: &Db, mut ann: HostAnnounce, observed_ip: String) -> (StatusCode, bool) {
    if ann.id.is_empty() || ann.id.len() > MAX_ID || ann.token.len() > MAX_TOKEN {
        return (StatusCode::BAD_REQUEST, false);
    }
    // Санитизация полей: режем размеры и адреса — защита от мусора/амплификации.
    ann.name = clamp(&ann.name, MAX_NAME);
    ann.country = clamp(&ann.country, MAX_COUNTRY);
    // Пароль ⇒ скрытая сеть. Ядро это уже соблюдает, но клиент можно подменить,
    // а каталог обязан быть верным при любом клиенте — правило и здесь.
    if ann.has_password {
        ann.public = false;
    }
    ann.protocol = clamp(&ann.protocol, MAX_PROTOCOL);
    ann.endpoints = ann.endpoints.iter().filter_map(|s| sane_addr(s)).take(MAX_ENDPOINTS).collect();
    // СЕРВЕР ставит IP хоста сам (наблюдаемый), а не берёт на веру — анти-спуфинг.
    ann.endpoints = authorize_endpoints(&ann.endpoints, &observed_ip);
    // Нет ни одного публичного адреса (за NAT / STUN не удался) → недостижим снаружи.
    if ann.endpoints.is_empty() {
        return (StatusCode::UNPROCESSABLE_ENTITY, false);
    }
    // Хост, не принимающий гостей (лимит 0) — «пустышка», а не рабочая сеть.
    if ann.max_guests == 0 {
        return (StatusCode::UNPROCESSABLE_ENTITY, false);
    }
    ann.params = ann
        .params
        .into_iter()
        .take(MAX_PARAMS)
        .map(|(k, v)| (clamp(&k, MAX_PARAM_LEN), clamp(&v, MAX_PARAM_LEN)))
        .collect();

    let new_fp = announce_visible_fp(&ann);
    let visible_changed;
    {
        let mut db = state.db.lock().await;
        // Владение (TOFU): заклеймлена НЕПУСТЫМ токеном и он не совпал → угон, отбой.
        match db.get(&ann.id) {
            Some(e) => {
                if !e.owner.is_empty() && e.owner != ann.token {
                    tracing::warn!(id = %ann.id, "отклонён: неверный owner-токен (угон)");
                    return (StatusCode::FORBIDDEN, false);
                }
                // Видимое поле изменилось (гость зашёл/вышел, смена имени/лимита…)?
                visible_changed = announce_visible_fp(&e.ann) != new_fp;
            }
            None => {
                // ПЕРВЫЙ клейм: код ОБЯЗАН быть подписан сервером (источник кодов — сервер).
                if !verify_code(&state.code_secret, &ann.id, &ann.code_sig) {
                    tracing::warn!(id = %ann.id, "отклонён: код без валидной подписи сервера");
                    return (StatusCode::FORBIDDEN, false);
                }
                if db.len() >= MAX_HOSTS {
                    tracing::warn!("каталог полон ({MAX_HOSTS}), новый хост отклонён");
                    return (StatusCode::SERVICE_UNAVAILABLE, false);
                }
                visible_changed = true; // новый хост — показать сразу
            }
        }
        let owner = ann.token.clone();
        let entry = db.entry(ann.id.clone()).or_insert_with(|| HostEntry {
            ann: ann.clone(),
            owner,
            last_seen: Instant::now(),
            observed_ip: observed_ip.clone(),
            seq: state.next_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ws_live: false,
        });
        entry.ann = ann;
        entry.observed_ip = observed_ip;
        entry.last_seen = Instant::now();
    }
    // Каталог будит вызывающий (WS Host всегда bump'ает после установки ws_live).
    (StatusCode::OK, visible_changed)
}

/// Собрать видимый каталог под фильтр (db уже залочен).
/// ВАЖНО: в общий каталог попадают ТОЛЬКО публичные хосты. Скрытые (public=false)
/// не отображаются нигде — к ним подключаются лишь по коду (id) через WS Resolve.
fn collect_items(db: &HashMap<String, HostEntry>, q: &DirectoryQuery) -> Vec<DirectoryItem> {
    let mut alive: Vec<&HostEntry> = db
        .values()
        // ТОЛЬКО ЖИВЫЕ: держит WS-сокет. Умер — сокет закрылся, запись убрана
        // мгновенно; «пустышка» без сокета не появляется вовсе.
        .filter(|e| e.live())
        .filter(|e| e.ann.public) // скрытые сети в каталоге не видны
        .filter(|e| match &q.country {
            Some(c) if !c.is_empty() => e.ann.country.eq_ignore_ascii_case(c),
            _ => true,
        })
        .collect();
    // Стабильный порядок для ВСЕХ клиентов: по первому клейму (seq), новые — в конец.
    // Без сортировки HashMap отдавал hash-хаос и список у клиентов перетасовывался.
    alive.sort_unstable_by_key(|e| e.seq);
    alive.into_iter().map(item_of).collect()
}

/// Карточка каталога из записи (общая для directory и resolve-по-коду).
fn item_of(e: &HostEntry) -> DirectoryItem {
    DirectoryItem {
        id: e.ann.id.clone(),
        name: e.ann.name.clone(),
        country: e.ann.country.clone(),
        ip: e.observed_ip.clone(),
        endpoints: e.ann.endpoints.clone(),
        public: e.ann.public,
        has_password: e.ann.has_password,
        guests: e.ann.guests,
        max_guests: e.ann.max_guests,
        online: e.ws_live,
        protocol: e.ann.protocol.clone(),
    }
}

/// Алфавит кодов хостов: заглавные без похожих символов (0/O/1/I) — читаемо голосом.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Длина кода сети. 12 символов из 32-символьного алфавита = 32^12 ≈ 1.15e18
/// вариантов: перебор скрытых хостов практически невозможен, а на глаз/по QR
/// разница с 8 незаметна (подключение всё равно тапом по каталогу).
const CODE_LEN: usize = 12;

/// ВЫДАТЬ НОВЫЙ КОД хоста (генерит СЕРВЕР, а не клиент — чтобы не сквоттили
/// «красивые» коды и не было коллизий). CODE_LEN символов, гарантированно свободный.
/// Сгенерировать свободный код + его подпись (общее для HTTP и WS newcode).
async fn gen_code(state: &Db) -> (String, String) {
    use rand::Rng;
    let db = state.db.lock().await;
    let mut rng = rand::thread_rng();
    let mut code = String::new();
    for _ in 0..20 {
        let c: String = (0..CODE_LEN).map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char).collect();
        if !db.contains_key(&c) {
            code = c;
            break;
        }
    }
    // Пустой code (не нашли свободный) → пустая подпись.
    let sig = if code.is_empty() { String::new() } else { sign_code(&state.code_secret, &code) };
    (code, sig)
}

/// Отпечаток видимых полей карточки — чтобы слать в каталог ДЕЛЬТУ (изменился ли
/// хост), а не весь список. Меняется вместе с тем, что видит гость.
fn item_fingerprint(i: &DirectoryItem) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    i.name.hash(&mut h);
    i.ip.hash(&mut h);
    i.endpoints.hash(&mut h);
    i.country.hash(&mut h);
    i.has_password.hash(&mut h);
    i.guests.hash(&mut h);
    i.max_guests.hash(&mut h);
    i.protocol.hash(&mut h);
    h.finish()
}

/// Отпечаток ВИДИМЫХ полей АНОНСА — чтобы решить, бампать ли каталог МГНОВЕННО.
/// Меняется при том, что гость реально видит в списке (число гостей, имя, лимит,
/// пароль, протокол, публичность). НЕ включает endpoints/ip: reflexive-адрес
/// мигает на каждом heartbeat, и если бы он бампал мгновенно — список бы мельтешил.
fn announce_visible_fp(a: &HostAnnounce) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    a.name.hash(&mut h);
    a.country.hash(&mut h);
    a.has_password.hash(&mut h);
    a.guests.hash(&mut h);
    a.max_guests.hash(&mut h);
    a.protocol.hash(&mut h);
    a.public.hash(&mut h);
    h.finish()
}

// ── обслуживание: чистка протухших хостов ────────────────────────────────────

async fn reaper(state: Db) {
    // Наблюдаемость: раз в ~5 мин логируем сводку (после сноса /health это
    // единственное окно в здоровье прода). Ноль поверхности атаки — просто лог.
    let mut ticks: u64 = 0;
    loop {
        // Штатно WS-хосты убираются мгновенно на закрытии сокета; reaper — лишь
        // бэкстоп по TTL для «повисших» записей (не должно быть, но подстрахуемся).
        tokio::time::sleep(Duration::from_secs(5)).await;
        let removed = {
            let mut db = state.db.lock().await;
            let before = db.len();
            db.retain(|_, e| e.alive());
            before - db.len()
        };
        if removed > 0 {
            tracing::info!(removed, "reaper: убрал протухшие записи (бэкстоп)");
            state.bump();
        }
        ticks += 1;
        if ticks % 60 == 0 {
            let hosts = state.db.lock().await.len();
            let (ips, conns) = {
                let m = state.ws_conns.lock().unwrap();
                (m.len(), m.values().sum::<u32>())
            };
            tracing::info!(hosts, ws_conns = conns, uniq_ips = ips, "сводка координатора");
        }
    }
}


// ── WebSocket-сигналинг (Стадия 1: РЯДОМ с HTTP, ничего не ломает) ───────────
// Один сокет клиент→координатор (исходящий, NAT проходит без хол-панча). Хост
// держит сокет открытым → жив «по факту сокета»: закрылся → УБИРАЕМ МГНОВЕННО по
// событию, без пингов и опроса. Гость подписывается — каталог и его изменения
// приходят пушем; хочет подключиться — кандидаты мгновенно летят хосту в его сокет.

/// Максимум WS-коннектов с одного IP (CGNAT-щедро, но не даст исчерпать сокеты).
const MAX_WS_PER_IP: u32 = 64;
/// Максимальный размер WS-сообщения (JSON анонса/коннекта — единицы КБ).
const MAX_WS_MSG: usize = 8 * 1024;
/// Как часто шлём WS-Ping. Закрытие сокета (краш/закрытие приложения) ловится
/// мгновенно и без пинга; ping нужен ТОЛЬКО чтобы поймать «тихую смерть» (клиент
/// завис / пропал сигнал — FIN не пришёл). Проверено: без него призрак висит вечно.
const WS_PING_INTERVAL: Duration = Duration::from_secs(3);
/// Нет Pong дольше этого → сокет мёртв, закрываем. Щедро (8с): обычные обрывы
/// (краш/закрытие) ловятся МГНОВЕННО закрытием сокета; этот таймаут нужен лишь для
/// РЕДКОЙ «тихой смерти» (завис/пропал сигнал) — незачем дёргать чаще.
const WS_PONG_DEADLINE: Duration = Duration::from_secs(8);

/// Сообщения клиента (хост/гость) → координатор. `id` — необязательная метка для
/// корреляции ответа (newcode/whoami/resolve/connect); сервер вернёт её же.
#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum WsClientMsg {
    /// Хост регистрируется/обновляет настройки (то же ядро, что register_host).
    Host(Box<HostAnnounce>),
    /// Явный уход (не обязателен — закрытие сокета убирает и так).
    Bye,
    /// Гость подписывается на каталог: сразу снапшот (`dirfull`), дальше ДЕЛЬТЫ.
    Watch(DirectoryQuery),
    /// Гость просит подключиться к хосту — его кандидаты летят хосту в сокет.
    /// В ответ (`connected`) — адреса хоста для пробития.
    Connect {
        #[serde(default)]
        id: u64,
        host_id: String,
        #[serde(default)]
        candidates: Vec<String>,
    },
    /// Запросить у сервера свежий код хоста (ответ `code`).
    NewCode {
        #[serde(default)]
        id: u64,
    },
    /// Узнать свой внешний IP как видит координатор (ответ `ip`).
    WhoAmI {
        #[serde(default)]
        id: u64,
    },
    /// Найти хост по коду, в т.ч. СКРЫТЫЙ (ответ `resolved`).
    Resolve {
        #[serde(default)]
        id: u64,
        code: String,
    },
}

/// Сообщения координатор → клиент.
#[derive(Serialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum WsServerMsg {
    /// Хост принят/обновлён.
    HostOk,
    /// Ошибка. `id` эхом (0 — не к запросу).
    Error { id: u64, code: u16, reason: String },
    /// Полный снапшот каталога (один раз на `watch`).
    DirFull { version: u64, hosts: Vec<DirectoryItem> },
    /// ДЕЛЬТЫ каталога — только изменившийся хост (масштаб: не шлём весь список).
    DirAdd { host: DirectoryItem },
    DirUpdate { host: DirectoryItem },
    DirRemove { id: String },
    /// Хосту: пришёл гость, вот его кандидаты — пробивай встречно.
    Guest { candidates: Vec<String> },
    /// Ответ на connect: адреса хоста (гость пробивает к ним).
    Connected { id: u64, endpoints: Vec<String> },
    /// Ответ на newcode.
    Code { id: u64, code: String, sig: String },
    /// Ответ на whoami.
    Ip { id: u64, addr: String },
    /// Ответ на resolve (найденный хост или null).
    Resolved { id: u64, host: Option<DirectoryItem> },
    /// Сведения о свежем релизе — ПОДПИСАННЫЙ манифест и подпись к нему, как есть.
    /// Координатор их НЕ разбирает и не может подделать: подпись проверяет сам
    /// клиент вшитым ключом. Смысл — донести факт обновления туда, где GitHub
    /// недоступен: узнал → подключился к VPN → скачал.
    Update { manifest: String, sig: String },
}

/// Манифест обновления с диска рядом с координатором.
///
/// Файлы кладёт оператор после `tools/sign-release.sh` (в CI подписи нет и быть
/// не должно). Перечитываем по mtime — выложил новый, подхватится без
/// перезапуска: рестарт координатора рвёт сокеты всем хостам сразу.
#[derive(Default)]
struct UpdateInfo {
    manifest: String,
    sig: String,
    mtime: Option<std::time::SystemTime>,
}

fn update_paths() -> (String, String) {
    let base = std::env::var("BMV_UPDATE_MANIFEST").unwrap_or_else(|_| "update-manifest.json".into());
    let sig = format!("{base}.sig");
    (base, sig)
}

/// Свежий манифест или None, если файлов нет. Дёшево: читаем только при смене mtime.
fn load_update(cache: &std::sync::Mutex<UpdateInfo>) -> Option<(String, String)> {
    let (mp, sp) = update_paths();
    let mtime = std::fs::metadata(&mp).ok()?.modified().ok();
    {
        let c = cache.lock().unwrap();
        if c.mtime == mtime && !c.manifest.is_empty() {
            return Some((c.manifest.clone(), c.sig.clone()));
        }
    }
    let manifest = std::fs::read_to_string(&mp).ok()?;
    let sig = std::fs::read_to_string(&sp).ok()?.trim().to_string();
    let mut c = cache.lock().unwrap();
    *c = UpdateInfo { manifest: manifest.clone(), sig: sig.clone(), mtime };
    Some((manifest, sig))
}

/// Гард WS-коннекта: на Drop уменьшает пер-IP счётчик (даже при обрыве).
struct WsConnGuard {
    state: Db,
    ip: IpAddr,
}
impl Drop for WsConnGuard {
    fn drop(&mut self) {
        let mut m = self.state.ws_conns.lock().unwrap();
        if let Some(c) = m.get_mut(&self.ip) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                m.remove(&self.ip);
            }
        }
    }
}
/// Впустить новый WS-коннект с IP (или None при переполнении квоты).
fn ws_admit(state: &Db, ip: IpAddr) -> Option<WsConnGuard> {
    let mut m = state.ws_conns.lock().unwrap();
    let c = m.entry(ip).or_insert(0);
    if *c >= MAX_WS_PER_IP {
        return None;
    }
    *c += 1;
    drop(m);
    Some(WsConnGuard { state: state.clone(), ip })
}

/// Апгрейд HTTP→WS. IP берём с учётом доверенного XFF (как везде).
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Db>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let ip = client_ip(&headers, peer);
    let Some(guard) = ws_admit(&state, ip) else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    ws.max_message_size(MAX_WS_MSG)
        .on_upgrade(move |socket| ws_conn(socket, state, ip, guard))
}

/// Одно WS-соединение: демультиплексируем роли (хост/гость) по сообщениям.
async fn ws_conn(socket: WebSocket, state: Db, ip: IpAddr, _guard: WsConnGuard) {
    let (mut sink, mut stream) = socket.split();
    // Все исходящие сообщения — через один канал → единственный писатель в сокет
    // (read-loop и пуш-задачи не дерутся за sink).
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<WsServerMsg>(64);
    let mut host_id: Option<String> = None; // стал ли хостом
    let mut watch_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut guest_task: Option<tokio::task::JoinHandle<()>> = None;
    // Ping-часы + отметка последнего Pong. Один цикл: и читаем, и пишем в сокет,
    // и пингуем — единственный владелец sink (нет гонок за него).
    let mut ping_iv = tokio::time::interval(WS_PING_INTERVAL);
    ping_iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pong = Instant::now();

    // Сразу сообщаем о свежем релизе, если оператор выложил манифест. Отправляем
    // ОДИН раз на соединение: клиент держит сокет часами, а релизы выходят реже.
    if let Some((manifest, sig)) = load_update(&state.update) {
        let _ = out_tx.send(WsServerMsg::Update { manifest, sig }).await;
    }

    'conn: loop {
        let text = tokio::select! {
            _ = ping_iv.tick() => {
                if last_pong.elapsed() > WS_PONG_DEADLINE { break 'conn; } // тихая смерть
                if sink.send(Message::Ping(Vec::new())).await.is_err() { break 'conn; }
                continue 'conn;
            }
            // Исходящие (ответы + пуши из watch/guest-задач) — пишем в сокет здесь же.
            Some(msg) = out_rx.recv() => {
                if let Ok(t) = serde_json::to_string(&msg) {
                    if sink.send(Message::Text(t)).await.is_err() { break 'conn; }
                }
                continue 'conn;
            }
            inc = stream.next() => match inc {
                Some(Ok(Message::Text(t))) => t,
                Some(Ok(Message::Pong(_))) => { last_pong = Instant::now(); continue 'conn; }
                Some(Ok(Message::Ping(p))) => { let _ = sink.send(Message::Pong(p)).await; continue 'conn; }
                // Close / ошибка / None — сокет закрылся: ловим МГНОВЕННО.
                _ => break 'conn,
            },
        };
        if text.len() > MAX_WS_MSG {
            continue;
        }
        let Ok(m) = serde_json::from_str::<WsClientMsg>(&text) else {
            let _ = out_tx.send(WsServerMsg::Error { id: 0, code: 400, reason: "плохой JSON".into() }).await;
            continue;
        };
        match m {
            WsClientMsg::Host(ann) => {
                let id = ann.id.clone();
                let (code, visible_changed) = register_host(&state, *ann, ip.to_string()).await;
                if code != StatusCode::OK {
                    let _ = out_tx.send(WsServerMsg::Error { id: 0, code: code.as_u16(), reason: "отклонён".into() }).await;
                    continue;
                }
                // Первый Host на этом сокете → регистрируем канал доставки гостей.
                if host_id.is_none() {
                    let (cand_tx, mut cand_rx) = tokio::sync::mpsc::channel::<Vec<String>>(32);
                    state.ws_hosts.lock().await.insert(id.clone(), cand_tx);
                    let out = out_tx.clone();
                    guest_task = Some(tokio::spawn(async move {
                        while let Some(c) = cand_rx.recv().await {
                            if out.send(WsServerMsg::Guest { candidates: c }).await.is_err() {
                                break;
                            }
                        }
                    }));
                    host_id = Some(id.clone());
                }
                // Пометить живым по WS и разбудить каталог. МГНОВЕННО (bump_now),
                // если хост НОВЫЙ (first_claim) ИЛИ изменилось видимое поле
                // (счётчик гостей зашёл/вышел, смена имени/лимита/пароля) — у всех
                // видно сразу, без 1-3с лага. Чистый heartbeat / мигающий reflexive
                // без видимых изменений — дебаунсом, чтобы список не мельтешил.
                let first_claim = {
                    let mut db = state.db.lock().await;
                    match db.get_mut(&id) {
                        Some(e) => { let was = e.ws_live; e.ws_live = true; !was }
                        None => false,
                    }
                };
                if first_claim || visible_changed { state.bump_now() } else { state.bump() }
                let _ = out_tx.send(WsServerMsg::HostOk).await;
            }
            WsClientMsg::Watch(q) => {
                // Подписка одна на соединение (повторный Watch игнорируем).
                // Снапшот один раз, дальше — ТОЛЬКО дельты (масштаб: не гоняем весь
                // каталог каждому смотрящему на каждое изменение).
                if watch_task.is_none() {
                    let out = out_tx.clone();
                    let st = state.clone();
                    watch_task = Some(tokio::spawn(async move {
                        let mut rx = st.version.subscribe();
                        let mut last: HashMap<String, u64> = HashMap::new();
                        let (version, items) = {
                            let db = st.db.lock().await;
                            (*rx.borrow(), collect_items(&db, &q))
                        };
                        for it in &items {
                            last.insert(it.id.clone(), item_fingerprint(it));
                        }
                        if out.send(WsServerMsg::DirFull { version, hosts: items }).await.is_err() {
                            return;
                        }
                        loop {
                            if rx.changed().await.is_err() {
                                break;
                            }
                            let cur = {
                                let db = st.db.lock().await;
                                collect_items(&db, &q)
                            };
                            let mut seen = std::collections::HashSet::new();
                            for it in &cur {
                                seen.insert(it.id.clone());
                                let fp = item_fingerprint(it);
                                let msg = match last.get(&it.id) {
                                    None => Some(WsServerMsg::DirAdd { host: it.clone() }),
                                    Some(old) if *old != fp => Some(WsServerMsg::DirUpdate { host: it.clone() }),
                                    _ => None,
                                };
                                last.insert(it.id.clone(), fp);
                                if let Some(msg) = msg {
                                    if out.send(msg).await.is_err() {
                                        return;
                                    }
                                }
                            }
                            let gone: Vec<String> = last.keys().filter(|k| !seen.contains(*k)).cloned().collect();
                            for id in gone {
                                last.remove(&id);
                                if out.send(WsServerMsg::DirRemove { id }).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }));
                }
            }
            WsClientMsg::Connect { id, host_id: hid, candidates } => {
                if hid.is_empty() || hid.len() > MAX_ID {
                    let _ = out_tx.send(WsServerMsg::Error { id, code: 400, reason: "плохой host_id".into() }).await;
                    continue;
                }
                // Достаём адреса хоста (гость к ним пробивает) и проверяем живость.
                let endpoints = {
                    let db = state.db.lock().await;
                    match db.get(&hid) {
                        Some(e) if e.alive() => Some(e.ann.endpoints.clone()),
                        _ => None,
                    }
                };
                let Some(endpoints) = endpoints else {
                    let _ = out_tx.send(WsServerMsg::Error { id, code: 404, reason: "хост не найден".into() }).await;
                    continue;
                };
                let cands: Vec<String> = candidates.iter().filter_map(|s| sane_addr(s)).take(MAX_CANDIDATES).collect();
                if !cands.is_empty() {
                    // Хост держит WS (иначе его не было бы в каталоге) → кандидаты
                    // гостя летят прямо в его сокет для встречного пробития NAT.
                    if let Some(tx) = state.ws_hosts.lock().await.get(&hid).cloned() {
                        let _ = tx.try_send(cands);
                    }
                }
                let _ = out_tx.send(WsServerMsg::Connected { id, endpoints }).await;
            }
            WsClientMsg::NewCode { id } => {
                let (code, sig) = gen_code(&state).await;
                let _ = out_tx.send(WsServerMsg::Code { id, code, sig }).await;
            }
            WsClientMsg::WhoAmI { id } => {
                let _ = out_tx.send(WsServerMsg::Ip { id, addr: ip.to_string() }).await;
            }
            WsClientMsg::Resolve { id, code } => {
                if code.is_empty() || code.len() > MAX_ID {
                    let _ = out_tx.send(WsServerMsg::Resolved { id, host: None }).await;
                    continue;
                }
                let host = {
                    let db = state.db.lock().await;
                    db.get(&code).filter(|e| e.alive()).map(item_of)
                };
                let _ = out_tx.send(WsServerMsg::Resolved { id, host }).await;
            }
            WsClientMsg::Bye => break 'conn,
        }
    }

    // Сокет закрыт (событие!) → МГНОВЕННО убираем хоста из каталога и будим всех
    // без дебаунса (исчезновение — дискретное событие, как и появление).
    if let Some(id) = host_id {
        state.ws_hosts.lock().await.remove(&id);
        state.db.lock().await.remove(&id);
        state.bump_now();
        tracing::info!(id = %id, "WS-хост ушёл (сокет закрыт)");
    }
    if let Some(t) = watch_task {
        t.abort();
    }
    if let Some(t) = guest_task {
        t.abort();
    }
}

// ── публичный вход (для бинаря и встраивания в приложения) ───────────────────

/// Поднять координатор на `bind` и работать до срабатывания `shutdown`.
/// Возвращает реально забинженный адрес через канал `bound` (порт мог быть 0).
///
/// Встраиваемость — сознательная фича: «главный сервер» может запустить кто
/// угодно и где угодно, хоть на телефоне (вкладка «Сервер» в приложении).
/// Как отдавать TLS в режиме сервера. `Acme` — авто Let's Encrypt ВНУТРИ бинаря
/// (домен из конфига; certbot/nginx НЕ нужны, только порт 443). `Files` — свои
/// cert+key. `None` — plain HTTP (локально / за своим прокси).
#[derive(Clone, Debug, Default)]
pub enum Tls {
    #[default]
    None,
    Files { cert: String, key: String },
    Acme { domains: Vec<String>, email: Option<String>, cache: String },
}

/// Handle с мягкой остановкой по сигналу `shutdown` (общее для Files/Acme путей).
#[cfg(feature = "tls")]
fn graceful(
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> axum_server::Handle {
    let handle = axum_server::Handle::new();
    let h = handle.clone();
    tokio::spawn(async move {
        shutdown.await;
        h.graceful_shutdown(Some(Duration::from_secs(3)));
    });
    handle
}

pub async fn serve(
    bind: SocketAddr,
    tls: Tls,
    bound: Option<tokio::sync::oneshot::Sender<SocketAddr>>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    // Без фичи tls (напр. встроенный координатор на телефоне) — режим игнорируем.
    #[cfg(not(feature = "tls"))]
    let _ = &tls;
    let (version_tx, _) = tokio::sync::watch::channel(1u64);
    let state: Db = Arc::new(AppState {
        db: Mutex::new(HashMap::new()),
        version: version_tx,
        dir_dirty: std::sync::atomic::AtomicBool::new(false),
        code_secret: load_code_secret(),
        next_seq: std::sync::atomic::AtomicU64::new(1),
        ws_hosts: Mutex::new(HashMap::new()),
        ws_conns: std::sync::Mutex::new(HashMap::new()),
        update: Default::default(),
    });
    let reap = tokio::spawn(reaper(state.clone()));
    // Дебаунс версии каталога для ШУМНЫХ обновлений (ре-анонс/heartbeat): не чаще
    // ~3 раз/сек. Дискретные появления/уходы идут мимо, через bump_now (мгновенно).
    let deb = {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(300)).await;
                if st.dir_dirty.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    st.version.send_modify(|v| *v += 1);
                }
            }
        })
    };

    // Весь протокол — по одному WebSocket (/v1/ws): регистрация хоста, каталог
    // (снапшот + дельты), знакомство гостя с хостом. Живость = сам сокет (закрылся →
    // хост убран мгновенно; «тихая смерть» — WS-пингом). Больше нет ни одного
    // HTTP-эндпойнта: даже мониторинг живости сервера — это открытие /v1/ws.
    let app = Router::new()
        .route("/v1/ws", get(ws_handler))
        .with_state(state);

    // НАТИВНЫЙ HTTPS: если переданы пути к сертификату (из конфига `[server]`) —
    // ядро само отдаёт TLS, без реверс-прокси. Иначе plain HTTP (локально /
    // встроенный на телефоне). Один бинарь, один конфиг.
    // Acme → сам получает Let's Encrypt по домену (certbot не нужен, только 443);
    // Files → свои cert+key; None → обычный HTTP (ниже). Провайдер rustls (ring).
    #[cfg(feature = "tls")]
    match tls {
        Tls::None => {}
        Tls::Files { cert, key } => {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let cfg = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                .await
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("TLS cert/key: {e}")))?;
            let handle = graceful(shutdown);
            if let Some(tx) = bound {
                let _ = tx.send(bind);
            }
            tracing::info!(%bind, "BeMyVPN coordinator слушает (HTTPS, свой сертификат)");
            let res = axum_server::bind_rustls(bind, cfg)
                .handle(handle)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await;
            reap.abort();
            deb.abort();
            return res;
        }
        Tls::Acme { domains, email, cache } => {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let mut acme = rustls_acme::AcmeConfig::new(domains.clone())
                .cache(rustls_acme::caches::DirCache::new(cache))
                .directory_lets_encrypt(true);
            if let Some(e) = email {
                acme = acme.contact_push(format!("mailto:{e}"));
            }
            let mut acme_state = acme.state();
            let acceptor = acme_state.axum_acceptor(acme_state.default_rustls_config());
            tokio::spawn(async move {
                use futures_util::StreamExt;
                while let Some(ev) = acme_state.next().await {
                    match ev {
                        Ok(ok) => tracing::info!(?ok, "acme"),
                        Err(err) => tracing::error!(%err, "acme"),
                    }
                }
            });
            let handle = graceful(shutdown);
            if let Some(tx) = bound {
                let _ = tx.send(bind);
            }
            tracing::info!(%bind, ?domains, "BeMyVPN coordinator слушает (HTTPS, авто Let's Encrypt)");
            let res = axum_server::bind(bind)
                .handle(handle)
                .acceptor(acceptor)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await;
            reap.abort();
            deb.abort();
            return res;
        }
    }

    let listener = tokio::net::TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    if let Some(tx) = bound {
        let _ = tx.send(addr);
    }
    tracing::info!(%addr, "BeMyVPN coordinator слушает (HTTP)");
    // ConnectInfo нужен, чтобы видеть IP WS-клиента (whoami + авторитет над адресом).
    let res = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await;
    reap.abort();
    deb.abort();
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &[u8] = b"test-secret-key-0123456789abcdef";

    fn test_peer() -> SocketAddr { "45.11.22.33:5000".parse().unwrap() }

    #[test]
    fn loopback_trusts_xff_last() {
        use std::net::Ipv4Addr;
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "1.2.3.4, 45.11.22.33".parse().unwrap());
        // с loopback (за прокси) берём ПОСЛЕДНИЙ адрес — его добавил прокси.
        assert_eq!(client_ip(&h, "127.0.0.1:5000".parse().unwrap()), IpAddr::V4(Ipv4Addr::new(45, 11, 22, 33)));
        // прямое подключение (не loopback, без env) — XFF игнорим, берём peer.
        assert_eq!(client_ip(&h, "8.8.8.8:5000".parse().unwrap()), IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)));
    }

    fn mk_state() -> Db {
        let (v, _) = tokio::sync::watch::channel(1u64);
        Arc::new(AppState {
            db: Mutex::new(HashMap::new()),
            version: v,
            dir_dirty: std::sync::atomic::AtomicBool::new(false),
            code_secret: TEST_SECRET.to_vec(),
            next_seq: std::sync::atomic::AtomicU64::new(1),
            ws_hosts: Mutex::new(HashMap::new()),
            ws_conns: std::sync::Mutex::new(HashMap::new()),
            update: Default::default(),
        })
    }

    fn ann(id: &str, token: &str, name: &str) -> HostAnnounce {
        HostAnnounce {
            id: id.into(),
            token: token.into(),
            name: name.into(),
            code_sig: sign_code(TEST_SECRET, id), // валидная подпись сервера
            endpoints: vec!["8.8.8.8:40000".into()], // публичный адрес (иначе 422)
            max_guests: 8,                           // рабочая сеть (0 → «пустышка», 422)
            ..Default::default()
        }
    }

    /// Регистрация хоста РОВНО как это делает WS-обработчик Host: ядро register_host
    /// с наблюдаемым IP = test_peer (45.11.22.33), затем пометка ws_live (хост держит
    /// сокет). Так тесты видят его в каталоге, как в бою.
    async fn reg(st: &Db, a: HostAnnounce) -> StatusCode {
        let id = a.id.clone();
        let (code, _visible_changed) = register_host(st, a, test_peer().ip().to_string()).await;
        if code == StatusCode::OK {
            if let Some(e) = st.db.lock().await.get_mut(&id) {
                e.ws_live = true;
            }
        }
        code
    }

    #[tokio::test]
    async fn guest_count_change_bumps_instantly() {
        let st = mk_state();
        // Новый хост → мгновенный бамп (показать сразу).
        let mut a = ann("HOSTAAAA1234", "tok", "Сеть");
        a.guests = 0;
        let (code, changed) = register_host(&st, a.clone(), test_peer().ip().to_string()).await;
        assert_eq!(code, StatusCode::OK);
        assert!(changed, "новый хост обязан бампаться мгновенно");
        // Идентичный ре-анонс (чистый heartbeat) → НЕ мгновенно (пойдёт дебаунсом).
        let (_c, changed) = register_host(&st, a.clone(), test_peer().ip().to_string()).await;
        assert!(!changed, "heartbeat без изменений не должен бампать мгновенно");
        // Зашёл гость (guests 0→1) → мгновенный бамп: счётчик виден сразу.
        let mut a2 = a.clone();
        a2.guests = 1;
        let (_c, changed) = register_host(&st, a2, test_peer().ip().to_string()).await;
        assert!(changed, "смена числа гостей обязана бампаться мгновенно");
    }

    #[test]
    fn sane_addr_keeps_only_public() {
        // Публичные — оставляем.
        assert!(sane_addr("8.8.8.8:53").is_some());
        assert!(sane_addr("1.2.3.4:40000").is_some());
        // Приватные/LAN/CGNAT/link-local — теперь РЕЖЕМ (снаружи недостижимы).
        assert!(sane_addr("192.168.1.5:40000").is_none());
        assert!(sane_addr("10.0.0.7:40000").is_none());
        assert!(sane_addr("172.16.5.5:40000").is_none());
        assert!(sane_addr("169.254.1.1:40000").is_none()); // link-local
        assert!(sane_addr("100.64.0.1:40000").is_none()); // CGNAT
        // Петля/мультикаст/0.0.0.0/порт 0/мусор/бродкаст — режем.
        assert!(sane_addr("127.0.0.1:80").is_none());
        assert!(sane_addr("0.0.0.0:80").is_none());
        assert!(sane_addr("224.0.0.1:80").is_none());
        assert!(sane_addr("255.255.255.255:80").is_none());
        assert!(sane_addr("8.8.8.8:0").is_none());
        assert!(sane_addr("не-адрес").is_none());
        assert!(sane_addr(&"1.1.1.1:1".repeat(20)).is_none()); // слишком длинно
    }

    #[tokio::test]
    async fn lan_only_host_rejected() {
        let st = mk_state();
        // Хост, у которого ТОЛЬКО локальные адреса (за NAT, STUN не удался) —
        // в каталог не берём (недостижим снаружи).
        let mut lan = ann("LANHOST1", "tok", "За NAT");
        lan.endpoints = vec!["192.168.0.10:40000".into(), "10.1.2.3:5000".into()];
        assert_eq!(reg(&st, lan).await, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(st.db.lock().await.is_empty(), "LAN-хост не попал в каталог");
        // А с публичным адресом среди локальных — берём (публичный выживает).
        let mut mixed = ann("PUBHOST1", "tok", "С белым IP");
        mixed.endpoints = vec!["192.168.0.10:40000".into(), "203.0.113.5:6000".into(), "45.11.22.33:7000".into()];
        assert_eq!(reg(&st, mixed).await, StatusCode::OK);
        let db = st.db.lock().await;
        let eps = &db.get("PUBHOST1").unwrap().ann.endpoints;
        assert_eq!(eps, &vec!["45.11.22.33:7000".to_string()], "остался только публичный адрес");
    }

    #[tokio::test]
    async fn token_prevents_hijack() {
        let st = mk_state();
        // Владелец клеймит запись своим токеном.
        assert_eq!(reg(&st, ann("HOST", "secret", "Мой")).await, StatusCode::OK);
        // Чужой с ДРУГИМ токеном не перепишет endpoints/имя.
        assert_eq!(reg(&st, ann("HOST", "evil", "Угон")).await, StatusCode::FORBIDDEN);
        assert_eq!(st.db.lock().await.get("HOST").unwrap().ann.name, "Мой");
        // Владелец своим токеном — обновляет.
        assert_eq!(reg(&st, ann("HOST", "secret", "Новое")).await, StatusCode::OK);
        assert_eq!(st.db.lock().await.get("HOST").unwrap().ann.name, "Новое");
    }

    // Примечание: отдельного «bye» больше нет — хост снимается САМ по закрытию
    // своего WS-сокета (владение = владение сокетом), чужой снять его не может в
    // принципе (у него нет этого сокета). Прежний тест bye-по-токену устарел.

    #[tokio::test]
    async fn hidden_not_in_catalog_but_resolvable_by_code() {
        let st = mk_state();
        // Скрытый хост (public=false).
        let mut hidden = ann("SECRET42", "tk", "Тайная");
        hidden.public = false;
        reg(&st, hidden).await;
        // Публичный хост.
        let mut vis = ann("PUBLIC01", "tk2", "Открытая");
        vis.public = true;
        reg(&st, vis).await;

        // В каталоге — только публичный.
        let items = collect_items(&*st.db.lock().await, &DirectoryQuery::default());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "PUBLIC01");

        // Но скрытый находится ПО КОДУ (как WS Resolve: db.get + item_of).
        let got = {
            let db = st.db.lock().await;
            db.get("SECRET42").filter(|e| e.alive()).map(item_of)
        };
        assert_eq!(got.map(|i| i.name), Some("Тайная".to_string()));
        // Несуществующий код → ничего.
        let none = {
            let db = st.db.lock().await;
            db.get("NOPE").filter(|e| e.alive()).map(item_of)
        };
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn directory_order_is_stable() {
        // Порядок каталога = порядок ПЕРВОГО клейма, стабилен для всех клиентов:
        // heartbeat/повторный анонс НЕ двигает хост, новый — всегда в конец.
        // (Раньше HashMap отдавал hash-хаос: свежий хост «вставал поверх» чужого.)
        let st = mk_state();
        // ann() по умолчанию скрытый — каталог показывает только публичные.
        let pub_ann = |id: &str, name: &str| {
            let mut a = ann(id, "tok", name);
            a.public = true;
            a
        };
        for (id, name) in [("AAAA2222", "Первый"), ("BBBB3333", "Второй"), ("CCCC4444", "Третий")] {
            reg(&st, pub_ann(id, name)).await;
        }
        let ids = |items: Vec<DirectoryItem>| items.into_iter().map(|i| i.id).collect::<Vec<_>>();
        let before = ids(collect_items(&*st.db.lock().await, &DirectoryQuery::default()));
        assert_eq!(before, ["AAAA2222", "BBBB3333", "CCCC4444"]);
        // Повторный анонс середины (heartbeat/изменение) — порядок тот же.
        reg(&st, pub_ann("BBBB3333", "Второй-обновлён")).await;
        assert_eq!(ids(collect_items(&*st.db.lock().await, &DirectoryQuery::default())), before);
        // Новый хост — в КОНЕЦ, существующие не сдвинулись.
        reg(&st, pub_ann("DDDD5555", "Новичок")).await;
        assert_eq!(
            ids(collect_items(&*st.db.lock().await, &DirectoryQuery::default())),
            ["AAAA2222", "BBBB3333", "CCCC4444", "DDDD5555"]
        );
        // Ушёл (закрыл сокет → запись убрана) и вернулся — встаёт в конец как новый.
        st.db.lock().await.remove("AAAA2222");
        reg(&st, pub_ann("AAAA2222", "Вернулся")).await;
        assert_eq!(
            ids(collect_items(&*st.db.lock().await, &DirectoryQuery::default())),
            ["BBBB3333", "CCCC4444", "DDDD5555", "AAAA2222"]
        );
    }

    #[tokio::test]
    async fn endpoint_ip_is_authoritative() {
        // Хост врёт, что он на 8.8.8.8 — сервер ставит РЕАЛЬНЫЙ наблюдаемый IP
        // (test_peer = 45.11.22.33). Спуфинг чужого/красивого IP невозможен.
        let st = mk_state();
        let mut a = ann("SPOOFER1", "tok", "Врун");
        a.endpoints = vec!["8.8.8.8:40000".into()];
        a.public = true;
        assert_eq!(reg(&st, a).await, StatusCode::OK);
        let db = st.db.lock().await;
        assert_eq!(db.get("SPOOFER1").unwrap().ann.endpoints, vec!["45.11.22.33:40000".to_string()]);
        // И гость получит именно наблюдаемый адрес (endpoints в каталоге).
        let items = collect_items(&db, &DirectoryQuery::default());
        assert_eq!(items[0].endpoints, vec!["45.11.22.33:40000".to_string()]);
    }

    #[tokio::test]
    async fn dummy_zero_max_guests_rejected() {
        // «Пустышка» — хост, не принимающий гостей (лимит 0): сервер не берёт.
        let st = mk_state();
        let mut a = ann("DUMMY000", "tok", "Пустышка");
        a.max_guests = 0;
        assert_eq!(reg(&st, a).await, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(st.db.lock().await.is_empty(), "пустышка не попала в каталог");
    }

    #[tokio::test]
    async fn unsigned_code_rejected() {
        let st = mk_state();
        // Самовольный код без подписи сервера — отказ (хост не создаёт коды сам).
        let mut forged = ann("FORGED42", "tok", "Самозванец");
        forged.code_sig = String::new();
        assert_eq!(reg(&st, forged).await, StatusCode::FORBIDDEN);
        // Неверная подпись — тоже отказ.
        let mut bad = ann("FORGED43", "tok", "Подделка");
        bad.code_sig = "deadbeef".repeat(4);
        assert_eq!(reg(&st, bad).await, StatusCode::FORBIDDEN);
        assert!(st.db.lock().await.is_empty(), "неподписанные коды не попали в каталог");
        // Валидно подписанный (ann() подписывает) — принят.
        assert_eq!(reg(&st, ann("REALCODE", "tok", "Настоящий")).await, StatusCode::OK);
        // И код от gen_code проходит: подпись согласована с секретом сервера.
        let (code, sig) = gen_code(&st).await;
        let mut fresh = ann(&code, "tok2", "Выданный");
        fresh.code_sig = sig;
        assert_eq!(reg(&st, fresh).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn legacy_no_token_still_updatable() {
        // Хост без токена (старый клиент) работает как раньше — не ломаем.
        let st = mk_state();
        assert_eq!(reg(&st, ann("OLD", "", "A")).await, StatusCode::OK);
        assert_eq!(reg(&st, ann("OLD", "", "B")).await, StatusCode::OK);
        assert_eq!(st.db.lock().await.get("OLD").unwrap().ann.name, "B");
    }
}
