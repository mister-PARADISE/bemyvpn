//! BeMyVPN Coordinator — «главный сервер»: справочник хостов + знакомство.
//!
//! Что делает:
//!   • держит КАТАЛОГ живых хостов (кто раздаёт, где, публичный/приватный…);
//!   • сводит гостя с хостом (обмен кандидатами для пробития NAT);
//!   • и уходит — в ТРАФИК НЕ ЛЕЗЕТ, приватных ключей НЕ хранит.
//!
//! Данные только в памяти: хост живёт РОВНО пока держит WebSocket. Любой может
//! поднять свой координатор этим бинарём.
//!
//! Весь протокол — JSON-сообщения по ОДНОМУ WebSocket (`GET /v1/ws`). Координатор
//! «открывает сокет» (как любой сайт), клиент/хост к нему подключаются (исходящее
//! соединение проходит NAT без хол-панча) и не открывают ничего у себя:
//!   • Хост    → регистрируется (`host`) и держит сокет; закрылся → убран мгновенно.
//!               ОДИН СОКЕТ = ОДИН код сети: анонс с другим кодом снимает прежний,
//!               а запись принадлежит соединению (переподключение её не теряет).
//!   • Гость   → подписывается (`watch`): снапшот каталога + дельты пушем в реальном
//!               времени; просит хоста (`connect`) — его кандидаты летят хосту в сокет.
//!   • Сервис  → `newcode` (выдать код), `whoami` (свой внешний IP), `resolve` (найти
//!               по коду, в т.ч. скрытый).
//! Живость = сам сокет: событие закрытия ловит краш/выход мгновенно, WS-пинг ловит
//! «тихую смерть». Никакого HTTP-API и никаких опросов больше нет.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
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

/// Имя файла секрета. Раньше путь был ОТНОСИТЕЛЬНЫЙ, то есть считался от рабочего
/// каталога процесса. В systemd-юните `WorkingDirectory` не задан → рабочий каталог
/// это `/`, и файл ложился в корень файловой системы. Стоило записи не пройти —
/// секрет становился разовым, а хосты получали отказ НАВСЕГДА: клиент просит новый
/// код только когда подпись пустая, а она у него сохранена.
const SECRET_FILE: &str = "bmv-coordinator.secret";

/// Прочитать/создать секрет по ЯВНЫМ путям. `legacy` — старое расположение
/// (относительный путь от рабочего каталога): боевой сервер уже живёт с ним, и
/// потерять его — значит разом отказать всему парку хостов. Поэтому он читается
/// запасным вариантом и переносится на новое место.
///
/// Отказ вместо предупреждения: молча сгенерированный «разовый» секрет ломает
/// парк тихо и необратимо (хост видит только «отклонён»), а отказ старта видит
/// оператор сразу и чинит правами или `BMV_CODE_SECRET`.
fn secret_from_files(path: &std::path::Path, legacy: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use rand::Rng;
    let read = |p: &std::path::Path| -> Option<Vec<u8>> {
        let h = std::fs::read_to_string(p).ok()?;
        let b = hex::decode(h.trim()).ok()?;
        (b.len() >= 16).then_some(b)
    };
    if let Some(b) = read(path) {
        return Ok(b);
    }
    if let Some(b) = read(legacy) {
        // Перенос старого секрета: НЕ фатально, если не вышло — секрет у нас уже
        // есть, парк работает, а на следующем запуске снова прочитается старый.
        match write_secret(path, &b) {
            Ok(()) => tracing::info!(path = %path.display(), "секрет перенесён со старого пути"),
            Err(e) => tracing::warn!(%e, path = %path.display(), "секрет со старого пути перенести не вышло"),
        }
        return Ok(b);
    }
    let mut secret = vec![0u8; 32];
    rand::thread_rng().fill(&mut secret[..]);
    write_secret(path, &secret).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "секрет подписи кодов не сохранить в {}: {e}. Без стабильного секрета \
                 ВСЕ хосты получат отказ после перезапуска. Дайте права на этот каталог \
                 или задайте BMV_CODE_SECRET (hex) / BMV_CODE_SECRET_FILE (путь).",
                path.display()
            ),
        )
    })?;
    tracing::info!(path = %path.display(), "секрет подписи кодов создан и сохранён");
    Ok(secret)
}

/// Записать секрет так, чтобы его не прочитал никто, кроме владельца процесса:
/// зная секрет, можно выпускать коды за сервер.
fn write_secret(path: &std::path::Path, secret: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, hex::encode(secret))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Загрузить СТАБИЛЬНЫЙ секрет подписи кодов. Приоритет: env `BMV_CODE_SECRET`
/// (hex) → файл `BMV_CODE_SECRET_FILE` → файл рядом с ИСПОЛНЯЕМЫМ ФАЙЛОМ →
/// старое место (рабочий каталог) → сгенерировать и сохранить.
fn load_code_secret() -> std::io::Result<Vec<u8>> {
    if let Ok(h) = std::env::var("BMV_CODE_SECRET") {
        if let Ok(b) = hex::decode(h.trim()) {
            if b.len() >= 16 {
                return Ok(b);
            }
        }
    }
    let path = match std::env::var("BMV_CODE_SECRET_FILE") {
        Ok(p) if !p.is_empty() => std::path::PathBuf::from(p),
        // От каталога бинаря, а не от рабочего каталога: он один и тот же при
        // запуске из терминала, из юнита службы и из меню — секрет не «переезжает».
        _ => std::env::current_exe()?
            .parent()
            .ok_or_else(|| std::io::Error::other("у исполняемого файла нет каталога"))?
            .join(SECRET_FILE),
    };
    secret_from_files(&path, std::path::Path::new(SECRET_FILE))
}

/// Грейс для ОСИРОТЕВШЕЙ записи (см. `sweep_orphans`).
///
/// TTL по времени убран сознательно. Он был мёртвой страховкой: `ws_live` не
/// снимался никогда, поэтому `alive()` всегда возвращал true и `retain` не
/// удалял ничего. Оживить его нельзя — периодического хартбита у клиента нет,
/// и любой таймер по `last_seen` выкинул бы из каталога честный хост, который
/// просто молчит в живой сокет. Живость = сокет; страховка теперь не по
/// времени, а по владению: запись без своего канала доставки — сирота.
const ORPHAN_GRACE: Duration = Duration::from_secs(15);

// ── лимиты (анти-DDoS / анти-флуд / анти-мусор) ──────────────────────────────
// WS-заслон живёт на самом соединении (пер-IP лимит коннектов + размер сообщения).
// Здесь — потолки каталога и полей анонса: заслон от Sybil/мусора/амплификации.

/// Потолок числа хостов в каталоге (защита памяти от Sybil-флуда новых id).
const MAX_HOSTS: usize = 5000;
/// Как часто ОДНОМУ хосту разрешено будить всех подписчиков каталога мгновенно.
/// Реальные события (зашёл гость, сменилось имя) случаются раз в минуты, так что
/// секунда никого не задерживает; всё, что чаще, уходит в общий дебаунс.
const INSTANT_BUMP_GAP: Duration = Duration::from_secs(1);
/// Потолок объявленной вместимости хоста. Реальный лимит подбирается по ОЗУ и
/// упирается в 128 (см. `bmv_config::suggested_max_guests`); 256 оставляет запас
/// тому, кто сознательно поднял планку руками на мощной машине, но отсекает
/// заведомую ложь ради первого места в списке.
const MAX_ANNOUNCED_GUESTS: u32 = 256;

// Потолки полей анонса/коннекта — режем мусор и амплификацию в каталоге.
const MAX_ID: usize = 64;
const MAX_NAME: usize = 64;
const MAX_COUNTRY: usize = 16;
const MAX_PROTOCOL: usize = 16;
const MAX_TOKEN: usize = 128;
const MAX_ENDPOINTS: usize = 8;
const MAX_CANDIDATES: usize = 8;
const MAX_ADDR: usize = 64; // длина строки ip:port

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
            // IPv4, записанный как IPv6 (`::ffff:10.0.0.1`), проходил ВСЕ проверки
            // выше: is_loopback у Ipv6Addr — это только `::1`. Так в каталог
            // попадал бы адрес вроде `[::ffff:127.0.0.1]`, и гости били бы в свой
            // же localhost или LAN. Разворачиваем и судим по правилам IPv4.
            if let Some(v4) = a.to_ipv4_mapped() {
                return sane_addr(&SocketAddr::new(IpAddr::V4(v4), sa.port()).to_string());
            }
            a.is_loopback() || a.is_unspecified() || a.is_multicast() || unique_local || link_local
        }
    };
    if bad {
        None
    } else {
        Some(sa.to_string())
    }
}

/// СЕРВЕР СТРОИТ АДРЕС САМ: наблюдаемый IP + порт, который назвал участник.
///
/// Заявленный участником адрес не проверяется, а ИГНОРИРУЕТСЯ ЦЕЛИКОМ — от него
/// берётся только номер порта. Раньше поле принимали и потом «авторизовали», и
/// обе дыры с подменой адреса жили ровно там: у хоста адрес чужого семейства
/// проходил на веру, а кандидаты гостя не переписывались вовсе, из-за чего в
/// сторону любой названной жертвы летело 12 секунд пробивающих пакетов с чужих
/// хостов. Поля, которого нет, подделать нельзя — поэтому его больше и нет.
///
/// Почему порт всё-таки со слов клиента: координатор говорит с ним по TCP и его
/// UDP-порт не наблюдает. Без порта пробитие NAT невозможно вовсе, а вред от
/// вранья ограничен собственным адресом врущего.
///
/// Результат ещё раз проходит `sane_addr`: если сам координатор видит участника
/// с приватного адреса (свой сервер в локальной сети), публиковать такое в
/// каталоге нельзя — снаружи туда всё равно не достучаться.
fn endpoints_for(claimed: &[String], observed: IpAddr, max: usize) -> Vec<String> {
    // Данные ходят только по IPv4 (сокеты и STUN бьются в `0.0.0.0:0`, см.
    // `bmv_net::local_ip`). Значит участник, которого мы видим по IPv6, снаружи
    // по нашей схеме недостижим, и честнее не публиковать ничего, чем выдать
    // адрес, в который никто не сможет попасть. КОГДА появится IPv6 в данных —
    // менять здесь.
    if !observed.is_ipv4() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for ep in claimed {
        let Ok(sa) = ep.parse::<SocketAddr>() else { continue };
        // Заявленный адрес нужен РОВНО для одного: подтвердить, что участник
        // нашёл своё ПУБЛИЧНОЕ отображение (STUN отработал). Домашний адрес
        // сюда не годится — хост за NAT раздавать не может, и лучше отказать
        // сразу, чем показать его в каталоге и заставить гостей биться впустую.
        // Сам адрес дальше не используется: берём из него только номер порта.
        if sane_addr(ep).is_none() {
            continue;
        }
        let Some(fixed) = sane_addr(&SocketAddr::new(observed, sa.port()).to_string()) else {
            continue;
        };
        if !out.contains(&fixed) {
            out.push(fixed);
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Привести чужую строку к безопасному виду: выкинуть управляющие символы и
/// обрезать по длине (пустую оставляем пустой).
///
/// Управляющие символы убираются потому, что строка идёт в ДВА чужих места: в
/// список хостов у каждого гостя и в журнал сервера. Перевод строки в журнале
/// подделывает соседние записи, а `\x1b[…` в терминальном интерфейсе
/// перерисовывает уже нарисованные чужие строки. В названии сети их не бывает.
fn clamp(s: &str, max: usize) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    let s = s.as_str();
    if s.len() <= max {
        return s.to_string();
    }
    // Режем по БАЙТАМ, а не по символам: предел задан в байтах, а `chars().take`
    // на кириллице возвращал вдвое, на эмодзи вчетверо больше предела — то есть
    // ограничение размера не работало. Границу ищем по символам, чтобы не
    // разрубить многобайтный символ пополам.
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if i + c.len_utf8() > max {
            break;
        }
        end = i + c.len_utf8();
    }
    s[..end].to_string()
}

// ── модель ───────────────────────────────────────────────────────────────────

/// Анонс хоста (что он о себе сообщает).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct HostAnnounce {
    id: String,
    #[serde(default)]
    token: String, // секрет владельца записи (НЕ показывается в каталоге)
    #[serde(default)]
    name: String, // человекочитаемое имя для UI
    // Поля `params` больше нет: клиент его шлёт, сервер хранил, а отдавал НИКОМУ
    // (в карточке каталога его не было, host_params у гостя всегда пустые).
    // Неизвестные поля serde молча игнорирует, поэтому старый клиент не ломается.
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

/// Каналы в сокет ХОСТА. Номер соединения обязателен: без него уборка старого
/// сокета снимала бы запись, которую только что создало новое (см. `release_host`).
struct HostChan {
    conn: u64,
    /// Кандидаты пришедшего гостя — для встречного пробития NAT.
    cands: tokio::sync::mpsc::Sender<Vec<String>>,
    /// Общий исходящий канал: по нему уходит подсказка «проверь соседа».
    out: tokio::sync::mpsc::Sender<WsServerMsg>,
}
type HostChannels = HashMap<String, HostChan>;

/// Кого предупредить, когда пара расходится: host_id → каналы в сокеты гостей,
/// которые просили ЗНАКОМСТВА с этим хостом.
///
/// Пару координатор наблюдает САМ, в `connect` — клиент не называет соседа ни
/// здесь, ни где-либо ещё. Поэтому послать подсказку «мимо своей пары»
/// невозможно в принципе, и отдельный токен на право подсказки не нужен: право
/// вытекает из того, что запись сделал сам сервер. Нового о людях сервер при
/// этом не узнаёт — пару он знал с момента поручительства за адреса пробития.
type PeerLinks = HashMap<String, HashMap<u64, tokio::sync::mpsc::Sender<WsServerMsg>>>;

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
    /// КАКОЕ соединение владеет записью сейчас. Клиент считает сокет мёртвым
    /// раньше сервера (6с против 8с) и успевает зарегистрироваться заново, пока
    /// старое соединение ещё живо. Уборка старого сокета без этой проверки стирала
    /// запись, только что созданную новым, — и хост пропадал из каталога навсегда,
    /// потому что периодического анонса у клиента нет.
    conn: u64,
    /// Когда этому хосту в последний раз разрешили МГНОВЕННУЮ рассылку каталога.
    /// `None` — ещё ни разу. См. `register_host`: без этой отметки хост, дёргающий
    /// видимое поле в цикле, рассылал бы дельту всем гостям на каждое сообщение.
    last_instant_bump: Option<Instant>,
}

impl HostEntry {
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
    /// WS-хосты: host_id → (номер соединения-владельца, канал доставки кандидатов
    /// гостя ПРЯМО в его сокет). Есть запись → хост онлайн по WS.
    ws_hosts: Mutex<HostChannels>,
    /// Кто с кем знакомился (см. `PeerLinks`) — для подсказки «проверь соседа».
    peers: Mutex<PeerLinks>,
    /// Счётчик открытых WS-коннектов на КЛЮЧ адреса (см. `conn_key`) — заслон от
    /// исчерпания сокетов флудом.
    ws_conns: std::sync::Mutex<HashMap<IpAddr, u32>>,
    /// Номер следующего WS-соединения (владение записями каталога).
    next_conn: std::sync::atomic::AtomicU64,
    /// Открыто WS-соединений ВСЕГО — общий потолок поверх пер-IP (иначе владелец
    /// одной /64 набирает сколько угодно ключей и пер-IP лимит ничего не значит).
    conns_now: std::sync::atomic::AtomicU64,
    /// ── наблюдаемость (только счётчики, поверхности атаки не добавляют) ──
    /// Сейчас подписано наблюдателей каталога.
    watchers: std::sync::atomic::AtomicU64,
    /// Сколько раз каталог реально рассылался (частота = дельта за период сводки).
    dir_bumps: std::sync::atomic::AtomicU64,
    /// Отказы по причинам: угон / неверная подпись / каталог полон / за NAT.
    rej_hijack: std::sync::atomic::AtomicU64,
    rej_sig: std::sync::atomic::AtomicU64,
    rej_full: std::sync::atomic::AtomicU64,
    rej_nat: std::sync::atomic::AtomicU64,
    /// Сколько сообщений отброшено токен-бакетом (флуд по открытому сокету).
    throttled: std::sync::atomic::AtomicU64,
    /// Пики за всё время работы: хостов в каталоге и одновременных соединений.
    peak_hosts: std::sync::atomic::AtomicU64,
    peak_conns: std::sync::atomic::AtomicU64,
}

impl AppState {
    /// Собрать состояние (одно место на боевой запуск и на тесты).
    fn new(code_secret: Vec<u8>) -> Db {
        let (version, _) = tokio::sync::watch::channel(1u64);
        Arc::new(AppState {
            db: Mutex::new(HashMap::new()),
            version,
            dir_dirty: std::sync::atomic::AtomicBool::new(false),
            code_secret,
            next_seq: std::sync::atomic::AtomicU64::new(1),
            ws_hosts: Mutex::new(HashMap::new()),
            peers: Mutex::new(HashMap::new()),
            ws_conns: std::sync::Mutex::new(HashMap::new()),
            next_conn: std::sync::atomic::AtomicU64::new(1),
            conns_now: std::sync::atomic::AtomicU64::new(0),
            watchers: std::sync::atomic::AtomicU64::new(0),
            dir_bumps: std::sync::atomic::AtomicU64::new(0),
            rej_hijack: std::sync::atomic::AtomicU64::new(0),
            rej_sig: std::sync::atomic::AtomicU64::new(0),
            rej_full: std::sync::atomic::AtomicU64::new(0),
            rej_nat: std::sync::atomic::AtomicU64::new(0),
            throttled: std::sync::atomic::AtomicU64::new(0),
            peak_hosts: std::sync::atomic::AtomicU64::new(0),
            peak_conns: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Запомнить пик (максимум наблюдённого значения).
    fn peak(counter: &std::sync::atomic::AtomicU64, now: u64) {
        counter.fetch_max(now, std::sync::atomic::Ordering::Relaxed);
    }

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
        self.bump_version();
    }
    /// Собственно рассылка версии каталога (и учёт частоты для сводки).
    fn bump_version(&self) {
        self.dir_bumps.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
/// санитизирует, авторизует IP, применяет анти-угон/анти-пустышку, кладёт/обновляет
/// запись, помечает её живой за соединением `conn` и будит каталог.
/// `observed_ip` — реальный IP WS-соединения, это авторитет над адресом хоста.
///
/// Всё в ОДНОМ захвате лока: раньше обработчик брал глобальный лок трижды на один
/// анонс (регистрация → пометка ws_live → решение о рассылке), и между захватами
/// запись успевала уехать под другим соединением.
///
/// Возвращает (статус, ПРИЧИНА). Причина уходит хосту дословно: без неё все отказы
/// выглядели одинаково («отклонён»), и клиент не мог понять, надо ли просить новый
/// код, чинить NAT или ждать места в каталоге.
async fn register_host(
    state: &Db,
    mut ann: HostAnnounce,
    observed_ip: String,
    conn: u64,
) -> (StatusCode, &'static str) {
    if ann.id.is_empty() || ann.id.len() > MAX_ID || ann.token.len() > MAX_TOKEN {
        return (StatusCode::BAD_REQUEST, "пустой или слишком длинный код/токен");
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
    // СЕРВЕР СТРОИТ адреса хоста сам: наблюдаемый IP + названные порты.
    // Заявленный хостом адрес не рассматривается вовсе (см. endpoints_for).
    let obs: Option<IpAddr> = observed_ip.parse().ok();
    // Хост, которого мы видим по IPv6, отсекался МОЛЧА (внутри endpoints_for), и
    // человек видел «отклонён» без единой подсказки, хотя причина — наша, а не его.
    if obs.is_some_and(|o| !o.is_ipv4()) {
        state.rej_nat.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "координатор видит вас только по IPv6, а данные у нас ходят по IPv4 — включите IPv4",
        );
    }
    ann.endpoints = match obs {
        Some(o) => endpoints_for(&ann.endpoints, o, MAX_ENDPOINTS),
        None => Vec::new(), // наблюдаемый адрес не разобрался — публиковать нечего
    };
    // Нет ни одного публичного адреса (за NAT / STUN не удался) → недостижим снаружи.
    if ann.endpoints.is_empty() {
        state.rej_nat.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "нет публичного адреса: вы за NAT или STUN не нашёл внешний порт",
        );
    }
    // Хост, не принимающий гостей (лимит 0) — «пустышка», а не рабочая сеть.
    if ann.max_guests == 0 {
        return (StatusCode::UNPROCESSABLE_ENTITY, "лимит гостей 0 — раздавать нечего");
    }
    // Вместимость — со слов хоста, и раньше её никто не проверял. Объявив
    // max_guests = 4 000 000 000 при нуле гостей, хост оказывался САМЫМ СВОБОДНЫМ
    // в каталоге — а «Быстрый старт» у всех сортирует именно по свободным местам.
    // То есть одной строкой в объявлении злоумышленник становился выходной нодой
    // по умолчанию для всех и видел их трафик. Врать всё ещё можно, но не больше
    // потолка — и обгонять честные хосты этим уже не выйдет.
    ann.max_guests = ann.max_guests.min(MAX_ANNOUNCED_GUESTS);
    // Гостей «сейчас» больше вместимости не бывает: такая пара — либо ошибка
    // хоста, либо попытка нарисовать себе красивую загрузку. Приводим к правде.
    ann.guests = ann.guests.min(ann.max_guests);

    let new_fp = announce_visible_fp(&ann);
    let now = Instant::now();
    // Мгновенная рассылка каталога — ДИСКРЕТНОЕ событие (новый хост / видимое
    // изменение), но не чаще INSTANT_BUMP_GAP на хост: одно сообщение хоста
    // превращается в дельту каждому гостю, и хост, дёргающий `guests` 0↔1 в
    // цикле, гонял бы этим весь исходящий канал сервера. Поток изменений
    // сваливается в общий 300мс-дебаунс, который их схлопывает.
    let instant;
    {
        let mut db = state.db.lock().await;
        // Владение (TOFU): заклеймлена НЕПУСТЫМ токеном и он не совпал → угон, отбой.
        let visible_changed = match db.get(&ann.id) {
            Some(e) => {
                if !e.owner.is_empty() && e.owner != ann.token {
                    state.rej_hijack.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!(code = %code_hint(&state.code_secret, &ann.id), "отклонён: неверный owner-токен (угон)");
                    return (StatusCode::FORBIDDEN, "код занят другим владельцем");
                }
                // Видимое поле изменилось (гость зашёл/вышел, смена имени/лимита…)?
                announce_visible_fp(&e.ann) != new_fp
            }
            None => {
                // ПЕРВЫЙ клейм: код ОБЯЗАН быть подписан сервером (источник кодов — сервер).
                if !verify_code(&state.code_secret, &ann.id, &ann.code_sig) {
                    state.rej_sig.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!(code = %code_hint(&state.code_secret, &ann.id), "отклонён: код без валидной подписи сервера");
                    return (StatusCode::FORBIDDEN, "код без подписи этого сервера — запросите новый код");
                }
                if db.len() >= MAX_HOSTS {
                    state.rej_full.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!("каталог полон ({MAX_HOSTS}), новый хост отклонён");
                    return (StatusCode::SERVICE_UNAVAILABLE, "каталог координатора переполнен");
                }
                true // новый хост — показать сразу
            }
        };
        let owner = ann.token.clone();
        let entry = db.entry(ann.id.clone()).or_insert_with(|| HostEntry {
            ann: ann.clone(),
            owner,
            last_seen: now,
            observed_ip: observed_ip.clone(),
            seq: state.next_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ws_live: false,
            conn,
            last_instant_bump: None,
        });
        entry.ann = ann;
        entry.observed_ip = observed_ip;
        entry.last_seen = now;
        // Владение переходит к ТЕКУЩЕМУ соединению: переподключившийся хост забирает
        // свою запись, а уборка прежнего сокета её уже не тронет (conn не совпадёт).
        entry.conn = conn;
        entry.ws_live = true;
        instant = visible_changed
            && entry.last_instant_bump.is_none_or(|t| now.duration_since(t) >= INSTANT_BUMP_GAP);
        if instant {
            entry.last_instant_bump = Some(now);
        }
        AppState::peak(&state.peak_hosts, db.len() as u64);
    }
    // Будим каталог ВНЕ лока: bump_now рассылает подписчикам.
    if instant {
        state.bump_now();
    } else {
        state.bump();
    }
    (StatusCode::OK, "")
}

/// Что писать в журнал ВМЕСТО кода сети. `id` — это КЛЮЧ доступа к скрытой сети:
/// кто прочёл лог (а логи уезжают в journald, в сборщики, в чужие глаза при
/// разборе), тот к ней подключится. Метка — HMAC на секрете сервера: строки в
/// журнале сопоставляются между собой, а восстановить код по ней нельзя, не
/// зная секрета. Отдельный префикс `log:` — чтобы метка не была куском
/// настоящей подписи кода.
fn code_hint(secret: &[u8], id: &str) -> String {
    sign_code(secret, &format!("log:{id}"))[..8].to_string()
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

/// Убрать ОСИРОТЕВШИЕ записи: те, чьё соединение больше не держит канал доставки.
///
/// Штатно запись снимает сам сокет при закрытии, и сюда ничего не доходит.
/// Страховка нужна на случай, когда обработчик умер в обход своей уборки (паника
/// в задаче): тогда запись висела бы в каталоге вечно — гости идут к хосту,
/// которого нет. Проверяем ВЛАДЕНИЕ, а не время: часы врут про живой молчаливый
/// сокет, а канал — нет. Грейс нужен затем, что запись появляется на мгновение
/// раньше своего канала (регистрация и канал заводятся подряд, но не атомарно).
fn sweep_orphans(
    db: &mut HashMap<String, HostEntry>,
    chans: &HostChannels,
) -> usize {
    let before = db.len();
    db.retain(|id, e| {
        chans.get(id).is_some_and(|h| h.conn == e.conn) || e.last_seen.elapsed() < ORPHAN_GRACE
    });
    before - db.len()
}

/// Как часто печатать сводку. Раньше было намертво «раз в 5 минут»; на живом
/// сервере хочется чаще во время разбора и реже в спокойный день.
fn summary_period() -> Duration {
    let secs = std::env::var("BMV_SUMMARY_SECS").ok().and_then(|v| v.parse::<u64>().ok());
    Duration::from_secs(secs.filter(|s| *s > 0).unwrap_or(300))
}

async fn reaper(state: Db) {
    use std::sync::atomic::Ordering::Relaxed;
    let period = summary_period();
    let mut next_summary = Instant::now() + period;
    let mut last_bumps = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let removed = {
            let chans = state.ws_hosts.lock().await;
            let mut db = state.db.lock().await;
            sweep_orphans(&mut db, &chans)
        };
        if removed > 0 {
            tracing::warn!(removed, "reaper: убрал осиротевшие записи (соединения нет)");
            state.bump();
        }
        if Instant::now() < next_summary {
            continue;
        }
        next_summary = Instant::now() + period;
        // Сводка — единственное окно в здоровье прода (HTTP-эндпойнтов нет).
        // Ноль поверхности атаки: просто лог.
        let hosts = state.db.lock().await.len();
        let (ips, conns) = {
            let m = state.ws_conns.lock().unwrap_or_else(|e| e.into_inner());
            (m.len(), m.values().sum::<u32>())
        };
        let bumps = state.dir_bumps.load(Relaxed);
        let per_min = (bumps - last_bumps) as f64 * 60.0 / period.as_secs_f64();
        last_bumps = bumps;
        tracing::info!(
            hosts,
            ws_conns = conns,
            uniq_prefixes = ips,
            watchers = state.watchers.load(Relaxed),
            dir_updates_per_min = format!("{per_min:.1}"),
            peak_hosts = state.peak_hosts.load(Relaxed),
            peak_conns = state.peak_conns.load(Relaxed),
            rej_hijack = state.rej_hijack.load(Relaxed),
            rej_sig = state.rej_sig.load(Relaxed),
            rej_full = state.rej_full.load(Relaxed),
            rej_nat = state.rej_nat.load(Relaxed),
            throttled = state.throttled.load(Relaxed),
            "сводка координатора"
        );
    }
}


// ── WebSocket-сигналинг (Стадия 1: РЯДОМ с HTTP, ничего не ломает) ───────────
// Один сокет клиент→координатор (исходящий, NAT проходит без хол-панча). Хост
// держит сокет открытым → жив «по факту сокета»: закрылся → УБИРАЕМ МГНОВЕННО по
// событию, без пингов и опроса. Гость подписывается — каталог и его изменения
// приходят пушем; хочет подключиться — кандидаты мгновенно летят хосту в его сокет.

/// Максимум WS-коннектов с одного ключа адреса (CGNAT-щедро, но не даст исчерпать
/// сокеты). Ключ — см. `conn_key`: у IPv6 это /64, а не адрес.
const MAX_WS_PER_IP: u32 = 64;
/// Потолок ОДНОВРЕМЕННЫХ соединений на весь сервер. Пер-IP лимит один защищает
/// плохо: даже с учётом /64 крупная сеть даёт сколько угодно ключей, и память
/// с файловыми дескрипторами кончатся раньше, чем сработает пер-IP заслон.
/// 20 тысяч — заведомо выше любой честной нагрузки (каталог всего на 5000 хостов).
const MAX_WS_TOTAL: u64 = 20_000;
/// Сколько сообщений в секунду разрешено ОДНОМУ соединению.
///
/// Раньше не было ничего: ни newcode, ни resolve, ни connect, ни host не
/// ограничивались, а каждое берёт ГЛОБАЛЬНЫЙ лок каталога. Один открытый сокет
/// сериализовывал весь сервер. Живому клиенту хватает единиц сообщений в
/// секунду (анонс — раз в минуты, resolve/connect — на действие человека).
const WS_MSG_RATE: f64 = 10.0;
/// Запас на «пачку» в начале: клиент при подключении шлёт host+watch+whoami
/// подряд, и рвать его за это нельзя.
const WS_MSG_BURST: f64 = 40.0;
/// Сколько ждём, пока клиент ПРИМЕТ сообщение. Отправка живёт в том же цикле, что
/// и проверка пинга: клиент, который не читает свой сокет (завис, приостановлен
/// системой), забивал TCP-окно, `send().await` не возвращался НИКОГДА, и цикл
/// вставал вместе с проверкой живости — соединение и запись хоста висели вечно.
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Ключ квоты соединений. У IPv6 провайдер выдаёт абоненту сразу /64 (2^64
/// адресов), поэтому счёт «по адресу» отменял пер-IP лимит целиком: злоумышленник
/// брал новый адрес на каждый коннект. Считаем по префиксу — как и наказываем.
fn conn_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(a) => {
            let mut o = a.octets();
            o[8..].fill(0);
            IpAddr::V6(std::net::Ipv6Addr::from(o))
        }
    }
}

/// Токен-бакет на СОЕДИНЕНИЕ: `rate` сообщений в секунду, накопить не больше
/// `burst`. Лишнее молча дропаем — рвать сокет нельзя (мобильный клиент может
/// разогнаться на реконнекте), а отвечать ошибкой значит усиливать флуд ответами.
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new() -> Self {
        Bucket { tokens: WS_MSG_BURST, last: Instant::now() }
    }
    /// Взять токен на одно сообщение. false → обработку пропускаем.
    fn take(&mut self, now: Instant) -> bool {
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * WS_MSG_RATE)
            .min(WS_MSG_BURST);
        self.last = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// Отправить кадр с таймаутом. `false` → соединение считаем мёртвым и рвём.
async fn send_guarded<S>(sink: &mut S, msg: Message) -> bool
where
    S: futures_util::Sink<Message> + Unpin,
{
    matches!(tokio::time::timeout(WS_SEND_TIMEOUT, sink.send(msg)).await, Ok(Ok(())))
}
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
    /// ПОДСКАЗКА «проверь соседа»: тот, с кем ты знакомился через нас, отвалился
    /// ОТ НАС (сокет закрылся или прислал `bye`). Это НЕ команда рвать туннель —
    /// координатор такого права не имеет и иметь не должен: получатель обязан сам
    /// опросить пира и решить (см. `Link::check_peer_now`). Полезна там, где
    /// прямое прощание по UDP послать физически некому: приложение убили, процесс
    /// упал, сеть исчезла.
    ///
    /// Ничего о паре не сообщаем — координатор и так знает её с момента
    /// знакомства, а получателю адресат не нужен: гость проверяет свою
    /// единственную сессию, хост — все свои (живые отвечают и не замечают).
    PeerCheck,
}

/// Гард WS-коннекта: на Drop уменьшает счётчики (даже при обрыве и панике).
struct WsConnGuard {
    state: Db,
    key: IpAddr,
}
impl Drop for WsConnGuard {
    fn drop(&mut self) {
        // Общий счётчик правим ПОД ТЕМ ЖЕ локом, что и пер-IP: иначе «прочитал →
        // записал» в ws_admit затирал бы параллельное освобождение, счётчик полз
        // бы вверх и однажды сервер отказал бы всем при пустом сервере.
        let mut m = self.state.ws_conns.lock().unwrap_or_else(|e| e.into_inner());
        self.state.conns_now.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(c) = m.get_mut(&self.key) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                m.remove(&self.key);
            }
        }
    }
}
/// Впустить новый WS-коннект с IP (или None при переполнении квоты — пер-префикс
/// или общей по серверу).
fn ws_admit(state: &Db, ip: IpAddr) -> Option<WsConnGuard> {
    use std::sync::atomic::Ordering::Relaxed;
    let key = conn_key(ip);
    let mut m = state.ws_conns.lock().unwrap_or_else(|e| e.into_inner());
    let c = m.entry(key).or_insert(0);
    if *c >= MAX_WS_PER_IP {
        return None;
    }
    // Общий потолок под тем же локом, что и пер-IP: иначе два коннекта проскочат
    // проверку одновременно и потолок перестанет быть потолком.
    let total = state.conns_now.load(Relaxed);
    if total >= MAX_WS_TOTAL {
        if *c == 0 {
            m.remove(&key);
        }
        return None;
    }
    *c += 1;
    state.conns_now.store(total + 1, Relaxed);
    AppState::peak(&state.peak_conns, total + 1);
    drop(m);
    Some(WsConnGuard { state: state.clone(), key })
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

/// Счётчик активных наблюдателей каталога (сколько сейчас держат подписку).
struct WatcherGuard(Db);
impl WatcherGuard {
    fn new(state: &Db) -> Self {
        state.watchers.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        WatcherGuard(state.clone())
    }
}
impl Drop for WatcherGuard {
    fn drop(&mut self) {
        self.0.watchers.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Снять запись хоста — но ТОЛЬКО если она всё ещё принадлежит соединению `conn`.
///
/// Проверка владения обязательна: клиент считает сокет мёртвым через 6с, сервер
/// держит его до 8с, и в эту щель хост уже переподключился и перерегистрировался.
/// Уборка старого сокета без проверки стирала СВЕЖУЮ запись, а вернуть её было
/// некому — периодического анонса у клиента нет, хост исчезал до перезапуска.
/// Максимум гостей, о которых помним «знакомился с этим хостом». Заслон памяти:
/// список чистится сам при уходе соединений, но потолок нужен на случай, когда
/// кто-то дёргает `connect` на один хост с тысяч сокетов. Больше `max_guests`
/// хоста тут не нужно, а верхняя граница объявленной вместимости — 256.
const MAX_PEERS_PER_HOST: usize = 256;

/// Подсказать всем, кто знакомился с `host_id`: «проверь соседа».
///
/// Не приказ и не адресное указание — просто сигнал «пара, возможно, распалась»
/// (см. `WsServerMsg::PeerCheck`). `try_send`: подсказка не стоит того, чтобы
/// ждать место в чужой очереди, а забитая очередь и так означает, что тому сокету
/// не до нас.
async fn hint_peers_of(state: &Db, host_id: &str) {
    let gone = state.peers.lock().await.remove(host_id);
    for (_, tx) in gone.into_iter().flatten() {
        let _ = tx.try_send(WsServerMsg::PeerCheck);
    }
}

/// Снять знакомство соединения `conn` с хостом `host_id` и подсказать ХОСТУ
/// «проверь соседа»: гость отвалился от координатора.
async fn unpair(state: &Db, host_id: &str, conn: u64) {
    {
        let mut pairs = state.peers.lock().await;
        let Some(slot) = pairs.get_mut(host_id) else { return };
        if slot.remove(&conn).is_none() {
            return; // не наша запись — молчим
        }
        if slot.is_empty() {
            pairs.remove(host_id);
        }
    }
    let out = state.ws_hosts.lock().await.get(host_id).map(|h| h.out.clone());
    if let Some(out) = out {
        let _ = out.try_send(WsServerMsg::PeerCheck);
    }
}

async fn release_host(state: &Db, id: &str, conn: u64) -> bool {
    {
        let mut chans = state.ws_hosts.lock().await;
        match chans.get(id) {
            Some(h) if h.conn == conn => {
                chans.remove(id);
            }
            _ => return false, // каналом владеет другое соединение — не наше дело
        }
    }
    // Хост ушёл (bye ИЛИ закрытие сокета — для нас это одно событие). Гости, что
    // знакомились с ним, узнают об этом ДАЖЕ ЕСЛИ прямое прощание по UDP послать
    // было некому: приложение убили, процесс упал, сеть исчезла.
    hint_peers_of(state, id).await;
    let removed = {
        let mut db = state.db.lock().await;
        match db.get(id) {
            Some(e) if e.conn == conn => db.remove(id).is_some(),
            _ => false,
        }
    };
    if removed {
        state.bump_now();
    }
    removed
}

/// Одно WS-соединение: демультиплексируем роли (хост/гость) по сообщениям.
async fn ws_conn(socket: WebSocket, state: Db, ip: IpAddr, _guard: WsConnGuard) {
    let conn = state.next_conn.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut bucket = Bucket::new();
    let (mut sink, mut stream) = socket.split();
    // Все исходящие сообщения — через один канал → единственный писатель в сокет
    // (read-loop и пуш-задачи не дерутся за sink).
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<WsServerMsg>(64);
    let mut host_id: Option<String> = None; // стал ли хостом
    // С каким хостом это соединение знакомилось как ГОСТЬ (см. `PeerLinks`).
    let mut paired_host: Option<String> = None;
    let mut watch_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut guest_task: Option<tokio::task::JoinHandle<()>> = None;
    // Ping-часы + отметка последнего Pong. Один цикл: и читаем, и пишем в сокет,
    // и пингуем — единственный владелец sink (нет гонок за него).
    let mut ping_iv = tokio::time::interval(WS_PING_INTERVAL);
    ping_iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pong = Instant::now();

    'conn: loop {
        let text = tokio::select! {
            _ = ping_iv.tick() => {
                if last_pong.elapsed() > WS_PONG_DEADLINE { break 'conn; } // тихая смерть
                if !send_guarded(&mut sink, Message::Ping(Vec::new())).await { break 'conn; }
                continue 'conn;
            }
            // Исходящие (ответы + пуши из watch/guest-задач) — пишем в сокет здесь же.
            Some(msg) = out_rx.recv() => {
                if let Ok(t) = serde_json::to_string(&msg) {
                    if !send_guarded(&mut sink, Message::Text(t)).await { break 'conn; }
                }
                continue 'conn;
            }
            inc = stream.next() => match inc {
                Some(Ok(Message::Text(t))) => t,
                Some(Ok(Message::Pong(_))) => { last_pong = Instant::now(); continue 'conn; }
                Some(Ok(Message::Ping(p))) => { if !send_guarded(&mut sink, Message::Pong(p)).await { break 'conn; } continue 'conn; }
                // Close / ошибка / None — сокет закрылся: ловим МГНОВЕННО.
                _ => break 'conn,
            },
        };
        if text.len() > MAX_WS_MSG {
            continue;
        }
        // Частота: лишнее молча дропаем ДО разбора JSON и до глобального лока —
        // иначе один сокет держал бы каталог заблокированным на весь свой флуд.
        if !bucket.take(Instant::now()) {
            state.throttled.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            continue;
        }
        let Ok(m) = serde_json::from_str::<WsClientMsg>(&text) else {
            let _ = out_tx.send(WsServerMsg::Error { id: 0, code: 400, reason: "плохой JSON".into() }).await;
            continue;
        };
        match m {
            WsClientMsg::Host(ann) => {
                let id = ann.id.clone();
                let (code, reason) = register_host(&state, *ann, ip.to_string(), conn).await;
                if code != StatusCode::OK {
                    let _ = out_tx.send(WsServerMsg::Error { id: 0, code: code.as_u16(), reason: reason.into() }).await;
                    continue;
                }
                // ОДИН СОКЕТ = ОДИН id. Клиент меняет код сети, не разрывая
                // соединение (пункт «новый код» в меню), и раньше прежний id
                // оставался в каталоге НАВСЕГДА: канал заводился только под
                // первый, «жив» ставилось каждому, а уборка снимала только один.
                // Так каталог забивался призраками до перезапуска сервера.
                if host_id.as_deref() != Some(id.as_str()) {
                    if let Some(prev) = host_id.take() {
                        release_host(&state, &prev, conn).await;
                    }
                    if let Some(t) = guest_task.take() {
                        t.abort();
                    }
                    let (cand_tx, mut cand_rx) = tokio::sync::mpsc::channel::<Vec<String>>(32);
                    let chan = HostChan { conn, cands: cand_tx, out: out_tx.clone() };
                    state.ws_hosts.lock().await.insert(id.clone(), chan);
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
                        // Счётчик наблюдателей — через гард: задачу снимают
                        // `abort()`ом, и обычный код в конце функции не выполнится.
                        let _seen = WatcherGuard::new(&st);
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
                        Some(e) if e.live() => Some(e.ann.endpoints.clone()),
                        _ => None,
                    }
                };
                let Some(endpoints) = endpoints else {
                    let _ = out_tx.send(WsServerMsg::Error { id, code: 404, reason: "хост не найден".into() }).await;
                    continue;
                };
                // Адреса ГОСТЯ проходят ровно тот же конвейер, что адреса хоста:
                // сначала отсев внутренних/мусорных, потом СЕРВЕР СТАВИТ IP САМ.
                //
                // Без второго шага здесь была отражённая атака. Гость называл
                // кандидатами любые ЧУЖИЕ адреса, координатор передавал их хосту
                // как есть, а хост честно бил в них встречным PUNCH — 48 пачек с
                // шагом 250мс, то есть 12 секунд по 8 адресам с одного запроса.
                // Повторяя это по всему каталогу, злоумышленник превращал сеть
                // хостов в машину для UDP-флуда на любой адрес, причём трафик шёл
                // с адресов НИ В ЧЁМ НЕ ВИНОВНЫХ людей — жалобы прилетали бы им.
                //
                // Порт по-прежнему на веру (его координатор не наблюдает), но
                // адрес теперь только свой: направить пробитие на чужую машину
                // больше нельзя.
                let cands = endpoints_for(&candidates, ip, MAX_CANDIDATES);
                if !cands.is_empty() {
                    // Хост держит WS (иначе его не было бы в каталоге) → кандидаты
                    // гостя летят прямо в его сокет для встречного пробития NAT.
                    if let Some(tx) = state.ws_hosts.lock().await.get(&hid).map(|h| h.cands.clone()) {
                        let _ = tx.try_send(cands);
                    }
                }
                // Запоминаем ПАРУ (наблюдение сервера, не слова клиента): когда
                // одна сторона отвалится от координатора, второй уйдёт подсказка
                // «проверь соседа». Гость знакомится с одним хостом за сеанс;
                // сменил — прежнюю запись за собой убираем.
                if paired_host.as_deref() != Some(hid.as_str()) {
                    if let Some(prev) = paired_host.replace(hid.clone()) {
                        unpair(&state, &prev, conn).await;
                    }
                    let mut pairs = state.peers.lock().await;
                    let slot = pairs.entry(hid.clone()).or_default();
                    if slot.len() < MAX_PEERS_PER_HOST || slot.contains_key(&conn) {
                        slot.insert(conn, out_tx.clone());
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
                    db.get(&code).filter(|e| e.live()).map(item_of)
                };
                let _ = out_tx.send(WsServerMsg::Resolved { id, host }).await;
            }
            WsClientMsg::Bye => break 'conn,
        }
    }

    // Сокет закрыт (событие!) → МГНОВЕННО убираем хоста из каталога и будим всех
    // без дебаунса (исчезновение — дискретное событие, как и появление). Но только
    // СВОЮ запись: за время нашей агонии хост мог переподключиться (см. release_host).
    if let Some(id) = host_id {
        if release_host(&state, &id, conn).await {
            tracing::info!(code = %code_hint(&state.code_secret, &id), "WS-хост ушёл (сокет закрыт)");
        }
    }
    // Гость отвалился от нас — подсказываем хосту проверить своих. Это ровно тот
    // случай, ради которого подсказка и нужна: приложение убили или сеть исчезла,
    // и прямое прощание по UDP послать физически некому.
    if let Some(hid) = paired_host {
        unpair(&state, &hid, conn).await;
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
    // Секрет — ДО того как что-то поднялось: без стабильного секрета сервер
    // выдаёт коды, которые сам же перестанет признавать после перезапуска.
    let state: Db = AppState::new(load_code_secret()?);
    let reap = tokio::spawn(reaper(state.clone()));
    // Дебаунс версии каталога для ШУМНЫХ обновлений (ре-анонс/heartbeat): не чаще
    // ~3 раз/сек. Дискретные появления/уходы идут мимо, через bump_now (мгновенно).
    let deb = {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(300)).await;
                if st.dir_dirty.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    st.bump_version();
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

    /// IPv4, записанный в форме IPv6, ОБЯЗАН отсекаться наравне с обычным.
    /// Раньше `[::ffff:127.0.0.1]` проходил как «глобальный»: is_loopback у
    /// Ipv6Addr — это только `::1`. Такой адрес попадал в каталог, и гости
    /// били бы в собственный localhost или свою LAN.
    #[test]
    fn ipv4_mapped_addresses_are_filtered() {
        for s in ["[::ffff:127.0.0.1]:443", "[::ffff:10.0.0.1]:443", "[::ffff:192.168.1.1]:443", "[::ffff:169.254.169.254]:80"] {
            assert!(sane_addr(s).is_none(), "должен быть отсечён: {s}");
        }
        // Настоящий публичный адрес в той же форме — проходит.
        assert!(sane_addr("[::ffff:8.8.8.8]:443").is_some(), "публичный не должен отсекаться");
    }

    /// Хост не может объявить ЧУЖОЙ адрес: координатор ставит наблюдаемый.
    /// А если семейства не совпали (пришёл по IPv6, объявляет IPv4), подтвердить
    /// адрес нечем — раньше он брался на веру, и через каталог получалась
    /// отражённая атака: гости слали пробивающие пакеты на указанный чужой IP.
    #[test]
    fn endpoints_cannot_claim_foreign_ip() {
        // Обычный случай: IP переписывается на наблюдаемый, порт сохраняется.
        let got = endpoints_for(&["1.2.3.4:5555".into()], ip("9.9.9.9"), MAX_ENDPOINTS);
        assert_eq!(got, vec!["9.9.9.9:5555".to_string()]);

        // Клиент пришёл по IPv6, объявляет чужой IPv4 — подтвердить нечем.
        let got = endpoints_for(&["8.8.8.8:443".into()], ip("2001:db8::1"), MAX_ENDPOINTS);
        assert!(got.is_empty(), "неподтверждаемый адрес обязан выбрасываться, получено: {got:?}");

        // Наблюдаемый адрес не разобрался — тоже ничего не подтверждаем.
        // «мусорный» наблюдаемый адрес теперь отсекается ещё до вызова (см. register_host).
    }

    /// Предел полей задан в БАЙТАХ, и обрезка обязана его соблюдать.
    /// Раньше `chars().take(max)` возвращала на кириллице вдвое больше предела,
    /// на эмодзи вчетверо — ограничение размера фактически не работало.
    #[test]
    fn clamp_respects_byte_limit() {
        let cyrillic = "я".repeat(100); // 200 байт
        let out = clamp(&cyrillic, MAX_NAME);
        assert!(out.len() <= MAX_NAME, "получилось {} байт при пределе {MAX_NAME}", out.len());
        assert!(!out.is_empty());

        let emoji = "🎭".repeat(100); // 400 байт
        let out = clamp(&emoji, MAX_NAME);
        assert!(out.len() <= MAX_NAME, "получилось {} байт", out.len());

        // Символ не должен быть разрублен пополам — строка обязана остаться валидной.
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        // Короткая строка не трогается.
        assert_eq!(clamp("привет", MAX_NAME), "привет");
    }

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

    /// Разобрать адрес в тестах (в бою он приходит уже разобранным от сокета).
    fn ip(s: &str) -> IpAddr { s.parse().unwrap() }

    fn mk_state() -> Db {
        AppState::new(TEST_SECRET.to_vec())
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
    /// с наблюдаемым IP = test_peer (45.11.22.33) от соединения №1. Живым по WS
    /// запись помечает само ядро — так тесты видят хост в каталоге, как в бою.
    async fn reg(st: &Db, a: HostAnnounce) -> StatusCode {
        register_host(st, a, test_peer().ip().to_string(), 1).await.0
    }

    /// Взведён ли флаг «каталог изменился» (дебаунс-таск в юнит-тестах не крутится).
    fn dirty(st: &Db) -> bool {
        st.dir_dirty.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test]
    async fn guest_count_change_bumps_instantly() {
        let st = mk_state();
        let mut a = ann("HOSTAAAA1234", "tok", "Сеть");
        a.guests = 0;
        // Новый хост → мгновенная рассылка (показать сразу).
        let before = *st.version.borrow();
        assert_eq!(reg(&st, a.clone()).await, StatusCode::OK);
        assert!(*st.version.borrow() > before, "новый хост обязан рассылаться мгновенно");
        // Идентичный ре-анонс (чистый heartbeat) → мгновенной рассылки нет.
        st.dir_dirty.store(false, std::sync::atomic::Ordering::SeqCst);
        let before = *st.version.borrow();
        assert_eq!(reg(&st, a.clone()).await, StatusCode::OK);
        assert_eq!(*st.version.borrow(), before, "heartbeat без изменений разослан мгновенно");
        // Зашёл гость (guests 0→1): изменение НЕ ТЕРЯЕТСЯ — либо сразу, либо дебаунсом.
        let mut a2 = a.clone();
        a2.guests = 1;
        assert_eq!(reg(&st, a2).await, StatusCode::OK);
        assert!(*st.version.borrow() > before || dirty(&st), "смена числа гостей потерялась");
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
            db.get("SECRET42").filter(|e| e.live()).map(item_of)
        };
        assert_eq!(got.map(|i| i.name), Some("Тайная".to_string()));
        // Несуществующий код → ничего.
        let none = {
            let db = st.db.lock().await;
            db.get("NOPE").filter(|e| e.live()).map(item_of)
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

    // ── КАТЕГОРИЯ Г: усиление и наводнение со стороны ЗЛОГО ХОСТА ─────────────
    //
    // Хост держит один сокет и шлёт в него что хочет. Опасно не то, что он
    // испортит СВОЮ запись, а то, что одним своим сообщением заставит сервер
    // сделать много работы или разослать много чужих сообщений.

    /// МИГАЮЩИЙ СЧЁТЧИК ГОСТЕЙ. Смена видимого поля будит ВСЕХ подписчиков
    /// каталога мгновенно (bump_now, без дебаунса). Значит хост, дёргающий
    /// `guests` 0↔1 в цикле, превращает одно своё сообщение в рассылку по всем
    /// гостям сразу — усиление тем больше, чем популярнее сервис.
    ///
    /// Тест считает, сколько раз подскочила версия каталога на 20 таких анонсов.
    #[tokio::test]
    async fn flapping_guest_count_cannot_spam_every_watcher() {
        let st = mk_state();
        let mut a = ann("FLAP0001", "tok", "Мигалка");
        a.guests = 0;
        assert_eq!(reg(&st, a.clone()).await, StatusCode::OK);

        let before = *st.version.borrow();
        for i in 0..20 {
            let mut m = a.clone();
            m.guests = (i % 2) as u32; // 0,1,0,1,…
            reg(&st, m).await;
        }
        let bumps = *st.version.borrow() - before;
        assert!(
            bumps <= 2,
            "20 анонсов дали {bumps} рассылок по всем подписчикам — \
             хост усиливает свой трафик в число гостей раз"
        );
    }

    /// Смена ИМЕНИ — такое же видимое поле, тот же приём. Проверяем отдельно,
    /// чтобы «починка» одного поля не оставила лазейку в соседнем.
    #[tokio::test]
    async fn flapping_name_cannot_spam_every_watcher() {
        let st = mk_state();
        let a = ann("FLAP0002", "tok", "Имя");
        assert_eq!(reg(&st, a.clone()).await, StatusCode::OK);

        let before = *st.version.borrow();
        for i in 0..20 {
            let mut m = a.clone();
            m.name = format!("Имя-{i}");
            reg(&st, m).await;
        }
        let bumps = *st.version.borrow() - before;
        assert!(bumps <= 2, "20 переименований дали {bumps} рассылок");
    }

    /// Честное ОДИНОЧНОЕ изменение обязано доезжать до гостей — иначе защита выше
    /// превратилась бы в «счётчик гостей обновляется через минуту». Дебаунс-таск в
    /// юнит-тесте не крутится, поэтому годится любой из двух путей: рассылка сразу
    /// или взведённый флаг, который разошлёт дебаунс (~300мс).
    #[tokio::test]
    async fn single_real_change_still_reaches_watchers() {
        let st = mk_state();
        let mut a = ann("HONEST01", "tok", "Честный");
        a.guests = 0;
        assert_eq!(reg(&st, a.clone()).await, StatusCode::OK);
        st.dir_dirty.store(false, std::sync::atomic::Ordering::SeqCst);
        let before = *st.version.borrow();
        a.guests = 1;
        assert_eq!(reg(&st, a).await, StatusCode::OK);
        assert!(*st.version.borrow() > before || dirty(&st), "настоящее изменение не дошло до гостей");
    }

    // ── КАТЕГОРИЯ И: подмена АДРЕСА и отражённая атака ────────────────────────
    //
    // На IP завязано всё: куда бить пробитием, чей лимит соединений тратится,
    // какая страна в каталоге. Правило одно: адрес НАБЛЮДАЕТ сервер, участник
    // его только предлагает.

    /// ГОСТЬ не должен уметь направить пробитие NAT на ЧУЖУЮ машину.
    ///
    /// Хост на присланные кандидаты шлёт встречный PUNCH 48 раз с шагом 250мс —
    /// 12 секунд по каждому адресу. Если бы адрес брался на веру, один запрос
    /// превращался бы в 12-секундный UDP-поток на любую названную жертву, и
    /// повторив это по каталогу, злоумышленник получал бы сеть отражателей из
    /// чужих хостов — с их адресов, то есть и с их репутацией.
    #[test]
    fn guest_cannot_aim_hole_punch_at_a_stranger() {
        let guest_ip = "45.11.22.33";
        let claimed = vec![
            "8.8.8.8:53".to_string(),          // чужой публичный
            "1.1.1.1:443".to_string(),         // чужой публичный
            "9.9.9.9:40000".to_string(),       // чужой публичный
        ];
        let out = endpoints_for(&claimed, ip(guest_ip), MAX_CANDIDATES);
        for a in &out {
            assert!(a.starts_with(guest_ip), "в хост уехал чужой адрес: {a}");
        }
        assert!(!out.is_empty(), "свои порты обязаны сохраниться, иначе пробитие не заработает");
    }

    /// ГЛАВНОЕ СВОЙСТВО новой схемы: заявленный адрес не попадает в результат.
    /// Какой бы ПУБЛИЧНЫЙ адрес участник ни назвал, при одном наблюдаемом адресе
    /// и одинаковых портах результат обязан совпадать. Значит подделывать нечего:
    /// это не «проверка», которую можно обойти, а отсутствие самой возможности.
    /// (Публичность заявленного всё же смотрится — но лишь как признак «STUN
    /// отработал», см. `only_private_claims_yield_nothing`.)
    #[test]
    fn claimed_address_does_not_influence_the_result() {
        let obs = ip("45.11.22.33");
        let a = endpoints_for(&["8.8.8.8:40000".into()], obs, MAX_ENDPOINTS);
        let b = endpoints_for(&["1.1.1.1:40000".into()], obs, MAX_ENDPOINTS);
        let c = endpoints_for(&["9.9.9.9:40000".into()], obs, MAX_ENDPOINTS);
        assert_eq!(a, b, "результат зависит от заявленного адреса");
        assert_eq!(b, c, "результат зависит от заявленного адреса");
        assert_eq!(a, vec!["45.11.22.33:40000".to_string()]);
    }

    /// Заявленный адрес нужен ровно для одного — подтвердить, что участник нашёл
    /// своё публичное отображение. Только домашние адреса = хост за NAT, раздавать
    /// он не может, и координатор обязан отказать сразу, а не пускать его в
    /// каталог гонять гостей впустую.
    #[test]
    fn only_private_claims_yield_nothing() {
        let obs = ip("45.11.22.33");
        let private = vec!["192.168.1.5:40000".to_string(), "10.0.0.7:40001".to_string(), "127.0.0.1:40002".to_string()];
        assert!(endpoints_for(&private, obs, MAX_ENDPOINTS).is_empty(),
            "хост без публичного отображения попал в каталог");
        // А смесь «домашний + публичный» оставляет порт от публичного.
        let mixed = vec!["192.168.1.5:40000".to_string(), "45.11.22.33:54321".to_string()];
        assert_eq!(endpoints_for(&mixed, obs, MAX_ENDPOINTS), vec!["45.11.22.33:54321".to_string()]);
    }

    /// Участник, которого мы видим по IPv6, недостижим: данные у нас ходят только
    /// по IPv4. Публиковать адрес, в который никто не попадёт, — хуже, чем
    /// отказать. Тест фиксирует это как ОСОЗНАННЫЙ предел, а не случайность.
    #[test]
    fn ipv6_observed_yields_nothing_while_data_plane_is_v4_only() {
        let out = endpoints_for(&["8.8.8.8:40000".into()], ip("2001:db8::1"), MAX_ENDPOINTS);
        assert!(out.is_empty(), "опубликован IPv6-адрес, до которого не дотянется UDP-слой: {out:?}");
    }

    /// Мусор и нулевые порты не должны ни падать, ни доезжать до хоста.
    #[test]
    fn malformed_claims_are_dropped_without_panic() {
        let obs = ip("45.11.22.33");
        let junk = vec![
            "".to_string(), "не адрес".into(), ":::".into(), "8.8.8.8".into(),
            "8.8.8.8:0".into(),        // нулевой порт
            "8.8.8.8:99999".into(),    // порт за границей u16
            "8.8.8.8:-1".into(),
            "[::ffff:8.8.8.8]:40000".into(), // публичный в форме IPv6 — порт годится
        ];
        let out = endpoints_for(&junk, obs, MAX_ENDPOINTS);
        assert_eq!(out, vec!["45.11.22.33:40000".to_string()], "пролез мусор: {out:?}");
    }

    /// Потолок числа адресов соблюдается — иначе один запрос заставлял бы хост
    /// пробивать в сколько угодно точек.
    #[test]
    fn endpoint_count_is_capped() {
        let obs = ip("45.11.22.33");
        let many: Vec<String> = (1..100u16).map(|p| format!("8.8.8.8:{}", 40000 + p)).collect();
        assert_eq!(endpoints_for(&many, obs, MAX_CANDIDATES).len(), MAX_CANDIDATES);
    }

    /// Порты гость выбирает сам (координатор их не наблюдает) — они обязаны
    /// доехать без изменений, иначе пробитие NAT перестанет работать вовсе.
    #[test]
    fn guest_ports_are_preserved_while_ip_is_replaced() {
        let out = endpoints_for(&["8.8.8.8:12345".into(), "9.9.9.9:54321".into()], ip("45.11.22.33"), MAX_CANDIDATES);
        assert!(out.contains(&"45.11.22.33:12345".to_string()), "порт 12345 потерян: {out:?}");
        assert!(out.contains(&"45.11.22.33:54321".to_string()), "порт 54321 потерян: {out:?}");
    }

    /// Полный конвейер кандидатов гостя, как в обработчике: сначала отсев
    /// внутренних адресов и мусора, потом подстановка наблюдаемого IP. Ни один
    /// чужой или внутренний адрес не должен доехать до хоста.
    #[test]
    fn guest_candidate_pipeline_leaves_only_own_public_address() {
        let guest_ip = "45.11.22.33";
        let claimed: Vec<String> = vec![
            "127.0.0.1:40000".into(),          // петля
            "192.168.1.5:40000".into(),        // LAN
            "169.254.169.254:80".into(),       // метаданные облака
            "[::ffff:10.0.0.1]:40000".into(),  // приватка в форме IPv6
            "не адрес".into(),                 // мусор
            "8.8.8.8:40000".into(),            // ЧУЖОЙ публичный
            "45.11.22.33:40001".into(),        // свой
        ];
        let filtered: Vec<String> = claimed.iter().filter_map(|s| sane_addr(s)).take(MAX_CANDIDATES).collect();
        let out = endpoints_for(&filtered, ip(guest_ip), MAX_CANDIDATES);
        assert!(!out.is_empty(), "у гостя не осталось ни одного адреса — пробитие не начнётся");
        for a in &out {
            assert!(a.starts_with(guest_ip), "до хоста доехал не адрес гостя: {a}");
        }
    }

    /// Заголовку X-Forwarded-For можно верить ТОЛЬКО от локального прокси.
    /// Иначе кто угодно приписывает себе любой IP: обходит лимит соединений на
    /// адрес, подделывает страну в каталоге и — до починки выше — направлял бы
    /// пробитие куда захочет.
    #[test]
    fn forwarded_headers_are_ignored_on_direct_connections() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "8.8.8.8".parse().unwrap());
        h.insert("x-real-ip", "1.1.1.1".parse().unwrap());
        let direct: SocketAddr = "45.11.22.33:50000".parse().unwrap();
        assert_eq!(client_ip(&h, direct), direct.ip(), "подделанный заголовок приняли за настоящий IP");

        // А от локального прокси — берём ПОСЛЕДНИЙ адрес: левые записи клиент
        // может дописать сам, но правую перезаписывает сам прокси.
        let via_proxy: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        let mut h2 = HeaderMap::new();
        h2.insert("x-forwarded-for", "9.9.9.9, 45.11.22.33".parse().unwrap());
        assert_eq!(client_ip(&h2, via_proxy).to_string(), "45.11.22.33", "взята подделанная левая запись");
    }

    /// Хост тоже не должен уметь назвать чужой адрес — это зеркало теста выше.
    /// Иначе гости всем каталогом пошли бы пробивать NAT к жертве.
    #[test]
    fn host_cannot_publish_a_stranger_address() {
        let mut a = ann("SPOOF001", "tok", "Подменщик");
        a.endpoints = vec!["8.8.8.8:443".into(), "9.9.9.9:40000".into()];
        let filtered: Vec<String> = a.endpoints.iter().filter_map(|s| sane_addr(s)).take(MAX_ENDPOINTS).collect();
        let out = endpoints_for(&filtered, ip("45.11.22.33"), MAX_ENDPOINTS);
        for e in &out {
            assert!(e.starts_with("45.11.22.33"), "в каталог попал чужой адрес: {e}");
        }
    }

    // ── КАТЕГОРИЯ Д: чем ЗЛОЙ ХОСТ может испортить каталог остальным ──────────

    /// Управляющие символы в имени. Имя показывается в списке хостов и уходит в
    /// логи сервера. Перевод строки в логе — это подделка соседних записей
    /// («log injection»), а возврат каретки и escape-последовательности в
    /// терминальном UI перерисовывают чужие строки. Ни один из них в имени
    /// сети смысла не имеет.
    #[tokio::test]
    async fn control_characters_are_stripped_from_name() {
        let st = mk_state();
        let mut a = ann("CTRL0001", "tok", "");
        a.name = "Обычный\n2026-01-01 ПОДДЕЛЬНАЯ СТРОКА ЛОГА\r\x1b[2Jчисто".into();
        assert_eq!(reg(&st, a).await, StatusCode::OK);
        let db = st.db.lock().await;
        let name = &db.get("CTRL0001").unwrap().ann.name;
        for bad in ['\n', '\r', '\t', '\x1b', '\0'] {
            assert!(!name.contains(bad), "в имени остался управляющий символ {bad:?}: {name:?}");
        }
    }

    /// Хост объявляет протокол, которого не существует. Гость возьмёт эту строку
    /// из каталога и пойдёт с ней подключаться — она не должна ни ронять его, ни
    /// заставлять молча падать на неизвестное имя.
    #[tokio::test]
    async fn unknown_protocol_name_does_not_break_catalog() {
        let st = mk_state();
        let mut a = ann("PROTO001", "tok", "Выдумщик");
        a.protocol = "noise-🎭-выдуманный".into();
        assert_eq!(reg(&st, a).await, StatusCode::OK);
        let db = st.db.lock().await;
        let p = &db.get("PROTO001").unwrap().ann.protocol;
        assert!(p.len() <= MAX_NAME, "строка протокола не обрезана: {} байт", p.len());
    }

    /// Заклеймленную запись НЕЛЬЗЯ отобрать, прикинувшись «старым клиентом без
    /// токена». Иначе весь смысл владения пропадал бы: достаточно не прислать
    /// поле token, чтобы перехватить чужой код хоста.
    #[tokio::test]
    async fn empty_token_cannot_steal_claimed_entry() {
        let st = mk_state();
        assert_eq!(reg(&st, ann("OWNED001", "секрет-владельца", "Мой")).await, StatusCode::OK);

        let mut thief = ann("OWNED001", "", "Угонщик");
        thief.endpoints = vec!["1.1.1.1:40000".into()];
        assert_ne!(reg(&st, thief).await, StatusCode::OK, "запись угнали пустым токеном");

        let db = st.db.lock().await;
        assert_eq!(db.get("OWNED001").unwrap().ann.name, "Мой", "имя подменили");
    }

    /// Сосед по каталогу не должен уметь снять чужой хост, прислав за него `bye`
    /// или анонс с чужим id и своим токеном.
    #[tokio::test]
    async fn foreign_token_cannot_modify_entry() {
        let st = mk_state();
        assert_eq!(reg(&st, ann("OWNED002", "правильный", "Мой")).await, StatusCode::OK);
        let mut a = ann("OWNED002", "чужой-токен", "Подмена");
        a.max_guests = 1;
        assert_ne!(reg(&st, a).await, StatusCode::OK);
        let db = st.db.lock().await;
        let e = db.get("OWNED002").unwrap();
        assert_eq!(e.ann.name, "Мой");
        assert_eq!(e.ann.max_guests, 8, "чужой изменил вместимость");
    }

    /// Гость может прислать сколько угодно своих адресов — в хост уедет не больше
    /// потолка, иначе один запрос превращался бы в мешок работы для чужой машины.
    #[test]
    fn guest_candidate_list_is_capped_and_filtered() {
        let mut many: Vec<String> = (0..500).map(|i| format!("8.8.{}.{}:40000", i / 256, i % 256)).collect();
        many.push("192.168.1.1:40000".into()); // приватный — отсеять
        many.push("не адрес".into());          // мусор — отсеять
        let out: Vec<String> = many.iter().filter_map(|s| sane_addr(s)).take(MAX_CANDIDATES).collect();
        assert_eq!(out.len(), MAX_CANDIDATES, "потолок кандидатов не соблюдён");
        assert!(out.iter().all(|s| !s.starts_with("192.168")), "приватный адрес пролез в кандидаты");
    }

    /// Хост не должен покупать себе первое место в каталоге враньём о вместимости:
    /// «Быстрый старт» сортирует по свободным местам, поэтому max_guests в миллиард
    /// сделал бы объявившего выходной нодой по умолчанию для всех.
    #[tokio::test]
    async fn announced_capacity_is_capped() {
        let st = mk_state();
        let mut a = ann("GREEDY01", "tok", "Жадный");
        a.max_guests = u32::MAX;
        a.guests = 999_999; // и «занято» тоже нарисовано
        assert_eq!(reg(&st, a).await, StatusCode::OK);
        let db = st.db.lock().await;
        let e = db.get("GREEDY01").expect("хост в каталоге");
        assert_eq!(e.ann.max_guests, MAX_ANNOUNCED_GUESTS, "вместимость не обрезана до потолка");
        assert!(e.ann.guests <= e.ann.max_guests, "занято больше вместимости — такого не бывает");
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

    // ── КАТЕГОРИЯ Е: страховка, частота, журнал, секрет ───────────────────────

    /// СТРАХОВКА ОТ «ПОВИСШЕЙ» ЗАПИСИ. Раньше её не было вовсе: `ws_live` никогда
    /// не сбрасывался, `alive()` возвращал true всегда, и `retain` не удалял
    /// НИЧЕГО — TTL был мёртвым кодом. Теперь живость меряется не часами, а
    /// владением: есть свой канал доставки → жив, нет → сирота.
    ///
    /// Отдельно проверяем главное последствие выбора: молчащий хост с живым
    /// сокетом остаётся в каталоге. Периодического анонса у клиента нет, и TTL
    /// по last_seen выкинул бы честную раздачу через полминуты тишины.
    #[tokio::test]
    async fn orphan_sweep_keeps_only_owned_entries() {
        let st = mk_state();
        let mk = |conn: u64, age: Duration| HostEntry {
            ann: ann("X", "t", "n"),
            owner: "t".into(),
            last_seen: Instant::now() - age,
            observed_ip: "45.11.22.33".into(),
            seq: 1,
            ws_live: true,
            conn,
            last_instant_bump: None,
        };
        let mut db: HashMap<String, HostEntry> = HashMap::new();
        let long_ago = ORPHAN_GRACE * 4;
        db.insert("OWNED".into(), mk(1, long_ago)); // свой канал, молчит давно
        db.insert("STOLEN".into(), mk(1, long_ago)); // канал у ДРУГОГО соединения
        db.insert("ORPHAN".into(), mk(1, long_ago)); // канала нет вовсе
        db.insert("FRESH".into(), mk(1, Duration::from_secs(0))); // канал ещё не завели
        let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<String>>(1);
        let (out, _out_rx) = tokio::sync::mpsc::channel::<WsServerMsg>(1);
        let chan = |conn: u64| HostChan { conn, cands: tx.clone(), out: out.clone() };
        let mut chans = HashMap::new();
        chans.insert("OWNED".to_string(), chan(1));
        chans.insert("STOLEN".to_string(), chan(2));

        assert_eq!(sweep_orphans(&mut db, &chans), 2);
        assert!(db.contains_key("OWNED"), "хост с живым сокетом выкинут из-за тишины");
        assert!(db.contains_key("FRESH"), "запись убрали в щель между регистрацией и каналом");
        assert!(!db.contains_key("ORPHAN"), "запись без соединения осталась в каталоге");
        assert!(!db.contains_key("STOLEN"), "запись прежнего владельца осталась в каталоге");
        let _ = &st; // состояние здесь не нужно, но пусть тест собирается как остальные
    }

    /// ЧАСТОТА. Ни одно сообщение не ограничивалось, а каждое берёт ГЛОБАЛЬНЫЙ
    /// лок каталога: один сокет, шлющий resolve в цикле, сериализовал весь сервер.
    #[test]
    fn token_bucket_limits_sustained_rate() {
        let t0 = Instant::now();
        let mut b = Bucket::new();
        // Пачка на старте разрешена (клиент шлёт host+watch+whoami подряд).
        let burst = (0..WS_MSG_BURST as u32).filter(|_| b.take(t0)).count();
        assert_eq!(burst as f64, WS_MSG_BURST, "стартовая пачка не должна резаться");
        // А дальше — стоп: запас исчерпан, время не шло.
        assert!(!b.take(t0), "поток сверх запаса не ограничен");
        // Через секунду доступно РОВНО столько, сколько накапало (не пачка целиком).
        let t1 = t0 + Duration::from_secs(1);
        let after = (0..1000).filter(|_| b.take(t1)).count();
        assert_eq!(after as f64, WS_MSG_RATE, "накопление токенов не совпадает со ставкой");
    }

    /// ОБЩИЙ ПОТОЛОК СОЕДИНЕНИЙ. Пер-IP лимита мало: ключей у атакующего больше,
    /// чем у сервера дескрипторов.
    #[test]
    fn total_connection_cap_holds() {
        let st = mk_state();
        // Забиваем потолок с РАЗНЫХ адресов (пер-IP лимит не должен мешать).
        let mut guards: Vec<WsConnGuard> = Vec::new();
        for i in 0..MAX_WS_TOTAL {
            let o = i.to_be_bytes();
            let ip = IpAddr::V4(std::net::Ipv4Addr::new(45, o[5], o[6], o[7]));
            guards.push(ws_admit(&st, ip).expect("честный коннект отвергнут до потолка"));
        }
        assert!(ws_admit(&st, ip("8.8.8.8")).is_none(), "общий потолок соединений не работает");
        guards.pop(); // освободилось одно место — снова пускаем
        assert!(ws_admit(&st, ip("8.8.8.8")).is_some(), "место не освободилось после разрыва");
    }

    /// IPv6 СЧИТАЕТСЯ ПО /64. Провайдер выдаёт абоненту сразу 2^64 адресов:
    /// счёт «по адресу» отменял пер-IP лимит целиком — новый адрес на каждый коннект.
    #[test]
    fn ipv6_quota_is_per_prefix_not_per_address() {
        let st = mk_state();
        let mut guards: Vec<WsConnGuard> = Vec::new();
        for i in 0..MAX_WS_PER_IP as u64 {
            let a: std::net::Ipv6Addr = format!("2001:db8::{i:x}").parse().unwrap();
            guards.push(ws_admit(&st, IpAddr::V6(a)).expect("до квоты обязаны пускать"));
        }
        // Ещё один адрес ИЗ ТОЙ ЖЕ /64 — это тот же абонент, квота исчерпана.
        assert!(ws_admit(&st, ip("2001:db8::ffff")).is_none(), "квота обходится сменой адреса в своей /64");
        // Соседняя /64 — другой абонент, его не наказываем.
        assert!(ws_admit(&st, ip("2001:db8:0:1::1")).is_some(), "наказан сосед по /48");
    }

    /// МЕДЛЕННЫЙ КЛИЕНТ. Отправка живёт в том же цикле, что и проверка пинга:
    /// клиент, не читающий сокет, забивал TCP-окно, `send().await` не возвращался
    /// НИКОГДА — цикл вставал вместе с проверкой живости, а запись хоста висела.
    #[tokio::test(start_paused = true)]
    async fn stuck_client_does_not_hang_the_loop() {
        use std::pin::Pin;
        use std::task::{Context, Poll};
        /// Сокет клиента, который ничего не забирает (окно забито).
        struct StuckSink;
        impl futures_util::Sink<Message> for StuckSink {
            type Error = std::io::Error;
            fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Pending
            }
            fn start_send(self: Pin<&mut Self>, _: Message) -> std::io::Result<()> {
                Ok(())
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Pending
            }
            fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Pending
            }
        }
        let mut stuck = StuckSink;
        let started = tokio::time::Instant::now();
        assert!(
            !send_guarded(&mut stuck, Message::Text("привет".into())).await,
            "отправка в мёртвого клиента вернула успех"
        );
        assert!(started.elapsed() >= WS_SEND_TIMEOUT, "таймаут отправки не сработал");

        // Живой приёмник по-прежнему получает сообщения (таймаут не мешает).
        let mut ok: Vec<Message> = Vec::new();
        assert!(send_guarded(&mut ok, Message::Text("привет".into())).await);
        assert_eq!(ok.len(), 1);
    }

    /// КОДЫ В ЖУРНАЛЕ. `id` — ключ доступа к скрытой сети: кто прочёл лог, тот
    /// подключился. В журнал уходит только метка.
    #[test]
    fn log_hint_never_reveals_the_code() {
        let code = "SECRETCODE42";
        let hint = code_hint(TEST_SECRET, code);
        assert!(!hint.contains(code), "код целиком попал в журнал");
        assert!(!code.to_lowercase().contains(&hint), "метка — это кусок самого кода");
        assert_ne!(hint, sign_code(TEST_SECRET, code), "метка = настоящая подпись кода");
        // Одинаковая для одного кода (строки в журнале сопоставимы)…
        assert_eq!(hint, code_hint(TEST_SECRET, code));
        // …и разная у разных кодов и разных серверов.
        assert_ne!(hint, code_hint(TEST_SECRET, "SECRETCODE43"));
        assert_ne!(hint, code_hint(b"other-server-secret-0123456789ab", code));
    }

    // ── КАТЕГОРИЯ З: СЕКРЕТ ПОДПИСИ (потеря = отказ всему парку хостов) ───────

    /// Каталог под тесты (в системном временном, без внешних крейтов).
    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let uniq = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("bmv-secret-{tag}-{uniq}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Секрет ОБЯЗАН переживать перезапуск и лежать закрытым. Если он теряется,
    /// сервер перестаёт признавать собственные коды, а хост просит новый только
    /// когда подпись пустая — то есть отказ становится вечным.
    #[test]
    fn secret_is_stable_and_private() {
        let d = tmp_dir("stable");
        let path = d.join(SECRET_FILE);
        let legacy = d.join("нет-такого");
        let first = secret_from_files(&path, &legacy).unwrap();
        assert!(first.len() >= 16);
        let second = secret_from_files(&path, &legacy).unwrap();
        assert_eq!(first, second, "секрет сменился при перезапуске — все коды стали чужими");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "секрет читается кем угодно (права {mode:o})");
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// ОБНОВЛЕНИЕ БОЕВОГО СЕРВЕРА. Секрет там уже лежит по СТАРОМУ пути (от
    /// рабочего каталога процесса). Не прочитать его = разом отказать всему живому
    /// парку хостов, поэтому старое место читается запасным и переносится.
    #[test]
    fn legacy_secret_survives_the_move() {
        let d = tmp_dir("legacy");
        let path = d.join(SECRET_FILE);
        let legacy = d.join("старое-место.secret");
        let old = vec![7u8; 32];
        std::fs::write(&legacy, hex::encode(&old)).unwrap();

        let got = secret_from_files(&path, &legacy).unwrap();
        assert_eq!(got, old, "секрет боевого сервера потерян — весь парк хостов получил бы отказ");
        assert!(path.exists(), "старый секрет не перенесён на новое место");
        // Старый файл можно убирать — новое место самодостаточно.
        std::fs::remove_file(&legacy).unwrap();
        assert_eq!(secret_from_files(&path, &legacy).unwrap(), old);
        std::fs::remove_dir_all(&d).ok();
    }

    /// Не сохранился — ОТКАЗ СТАРТА. Молча сгенерированный «разовый» секрет ломает
    /// парк тихо и необратимо; отказ старта оператор видит сразу.
    #[test]
    #[cfg(unix)]
    fn unwritable_secret_refuses_to_start() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp_dir("ro");
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = secret_from_files(&d.join(SECRET_FILE), &d.join("нет")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("BMV_CODE_SECRET"), "в ошибке нет подсказки, чем чинить: {msg}");
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&d).ok();
    }

    // ── КАТЕГОРИЯ К: за что именно отказали ──────────────────────────────────

    /// Все отказы были схлопнуты в слово «отклонён»: человек не понимал, просить
    /// ли новый код, чинить NAT или ждать места. А хост за IPv6 отсекался вообще
    /// МОЛЧА, внутри фильтра адресов.
    #[tokio::test]
    async fn rejections_explain_themselves_and_are_counted() {
        let st = mk_state();
        let seen = |c: &std::sync::atomic::AtomicU64| c.load(std::sync::atomic::Ordering::Relaxed);

        // Виден только по IPv6 — наша сторона, и об этом надо сказать.
        let (code, why) = register_host(&st, ann("V6HOST000001", "t", "n"), "2001:db8::1".into(), 1).await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(why.contains("IPv6"), "молчаливый отказ хосту по IPv6: {why:?}");

        // За NAT (только домашние адреса) — причина другая.
        let mut nat = ann("NATHOST00001", "t", "n");
        nat.endpoints = vec!["192.168.0.5:40000".into()];
        let (code, why) = register_host(&st, nat, test_peer().ip().to_string(), 1).await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(why.contains("NAT"), "причина отказа за NAT не названа: {why:?}");
        assert_eq!(seen(&st.rej_nat), 2, "отказы «недостижим» не считаются");

        // Код без подписи — единственный случай, когда надо просить НОВЫЙ код.
        let mut forged = ann("FORGEDCODE01", "t", "n");
        forged.code_sig = String::new();
        let (code, why) = register_host(&st, forged, test_peer().ip().to_string(), 1).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        assert!(why.contains("новый код"), "клиент не поймёт, что нужен новый код: {why:?}");
        assert_eq!(seen(&st.rej_sig), 1);

        // Угон чужого кода — просить новый код бесполезно, код чужой.
        assert_eq!(reg(&st, ann("OWNEDCODE001", "мой", "n")).await, StatusCode::OK);
        let (code, why) = register_host(&st, ann("OWNEDCODE001", "чужой", "n"), test_peer().ip().to_string(), 2).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        assert!(why.contains("владельцем"), "причина угона не названа: {why:?}");
        assert_eq!(seen(&st.rej_hijack), 1);
        assert_eq!(seen(&st.peak_hosts), 1, "пик хостов не считается");
    }

    // ── КАТЕГОРИЯ Ж: ЖИЗНЬ СОКЕТА (кто владеет записью в каталоге) ────────────
    //
    // Всё, что ниже, проверяется ТОЛЬКО через настоящее WS-соединение: правила
    // владения («один сокет = один id», «уборка снимает лишь СВОЮ запись»)
    // живут внутри обработчика сокета, и из юнит-теста туда не заглянуть.
    mod ws_life {
        use super::*;
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message as CMsg;

        /// Поднять координатор на случайном порту с ИЗВЕСТНЫМ секретом подписи
        /// (иначе анонс отклонят на первом клейме).
        async fn spawn_coord() -> SocketAddr {
            // Секрет через env — тот же путь, что у оператора; файл не трогаем.
            // Ровно один раз на весь бинарь: setenv в многопоточном процессе
            // гонится с getenv соседних тестов.
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| std::env::set_var("BMV_CODE_SECRET", hex::encode(TEST_SECRET)));
            let (tx, rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let _ = serve("127.0.0.1:0".parse().unwrap(), Tls::None, Some(tx), std::future::pending()).await;
            });
            rx.await.expect("координатор не забиндился")
        }

        /// Клиент с ПОДДЕЛЬНЫМ (для сервера — доверенным, т.к. мы с loopback)
        /// внешним IP: иначе наблюдаемый 127.0.0.1 не публикуется и анонс = 422.
        async fn client(
            addr: SocketAddr,
            ip: &str,
        ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
            let mut req = format!("ws://{addr}/v1/ws").into_client_request().unwrap();
            req.headers_mut().insert("x-forwarded-for", ip.parse().unwrap());
            tokio_tungstenite::connect_async(req).await.expect("WS не подключился").0
        }

        /// Отправить анонс хоста и дождаться ответа сервера (hostok/error).
        async fn announce<S>(ws: &mut S, id: &str, token: &str) -> String
        where
            S: SinkExt<CMsg, Error = tokio_tungstenite::tungstenite::Error>
                + StreamExt<Item = std::result::Result<CMsg, tokio_tungstenite::tungstenite::Error>>
                + Unpin,
        {
            let mut a = serde_json::to_value(ann(id, token, "Сеть")).unwrap();
            a["t"] = serde_json::json!("host");
            ws.send(CMsg::Text(a.to_string())).await.unwrap();
            reply(ws).await
        }

        /// Прочитать ближайший СОДЕРЖАТЕЛЬНЫЙ ответ (пинги пропускаем), вернуть тип `t`.
        async fn reply<S>(ws: &mut S) -> String
        where
            S: SinkExt<CMsg, Error = tokio_tungstenite::tungstenite::Error>
                + StreamExt<Item = std::result::Result<CMsg, tokio_tungstenite::tungstenite::Error>>
                + Unpin,
        {
            loop {
                match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                    Ok(Some(Ok(CMsg::Text(t)))) => {
                        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                        return v["t"].as_str().unwrap_or("").to_string();
                    }
                    Ok(Some(Ok(_))) => continue, // ping/pong
                    other => panic!("нет ответа от координатора: {other:?}"),
                }
            }
        }

        /// Спросить у координатора, есть ли ещё такой хост (resolve по коду).
        async fn resolvable(addr: SocketAddr, id: &str) -> bool {
            let mut ws = client(addr, "45.11.22.44").await;
            ws.send(CMsg::Text(serde_json::json!({"t":"resolve","id":7,"code":id}).to_string())).await.unwrap();
            loop {
                match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                    Ok(Some(Ok(CMsg::Text(t)))) => {
                        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                        if v["t"] == "resolved" {
                            return !v["host"].is_null();
                        }
                    }
                    Ok(Some(Ok(_))) => continue,
                    other => panic!("нет ответа на resolve: {other:?}"),
                }
            }
        }

        /// ХОСТЫ-ПРИЗРАКИ. Один сокет присылает `host` с РАЗНЫМИ id (так делает
        /// живой клиент, когда меняет код сети, не разрывая соединение). Канал
        /// заводился только для первого id, а «жив» ставилось каждому — и все,
        /// кроме первого, оставались в каталоге НАВСЕГДА: reaper их не трогает,
        /// уборка при закрытии снимает только первый. 10000 сообщений забивали
        /// потолок каталога, и честные хосты получали отказ до перезапуска.
        #[tokio::test]
        async fn one_socket_owns_exactly_one_id() {
            let addr = spawn_coord().await;
            let mut ws = client(addr, "45.11.22.33").await;
            assert_eq!(announce(&mut ws, "GHOSTAAA0001", "tok").await, "hostok");
            assert_eq!(announce(&mut ws, "GHOSTBBB0002", "tok").await, "hostok");
            drop(ws); // сокет закрыт — уйти обязаны ОБА
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(!resolvable(addr, "GHOSTBBB0002").await, "id, которым сокет владел, не убран при закрытии");
            assert!(!resolvable(addr, "GHOSTAAA0001").await, "прежний id остался в каталоге призраком");
        }

        /// ГОНКА ПЕРЕПОДКЛЮЧЕНИЯ. Клиент считает сокет мёртвым через 6с, сервер
        /// держит старый до 8с. В эти две секунды хост уже зарегистрирован НОВЫМ
        /// соединением, а уборка СТАРОГО стирала запись без проверки владельца —
        /// хост пропадал из каталога и не возвращался (периодического анонса нет).
        #[tokio::test]
        async fn stale_socket_cleanup_keeps_new_registration() {
            let addr = spawn_coord().await;
            let mut old = client(addr, "45.11.22.33").await;
            assert_eq!(announce(&mut old, "RACEHOST0001", "tok").await, "hostok");
            let mut fresh = client(addr, "45.11.22.33").await;
            assert_eq!(announce(&mut fresh, "RACEHOST0001", "tok").await, "hostok");
            drop(old); // «мёртвый» сокет уходит ПОСЛЕ переподключения
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(
                resolvable(addr, "RACEHOST0001").await,
                "уборка старого сокета стёрла запись, только что созданную новым"
            );
        }

        /// Дождаться подсказки «проверь соседа» — с ОГРАНИЧЕННЫМ сроком.
        ///
        /// Отдельно от `reply`: тот крутится на служебных кадрах (координатор
        /// пингует раз в 3с), поэтому без своего дедлайна тест не падал бы, а
        /// висел — а висящий тест не показывает, что именно сломалось.
        async fn hint_arrives<S>(ws: &mut S, within: Duration) -> bool
        where
            S: StreamExt<Item = std::result::Result<CMsg, tokio_tungstenite::tungstenite::Error>> + Unpin,
        {
            let deadline = tokio::time::Instant::now() + within;
            loop {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                if left.is_zero() {
                    return false;
                }
                match tokio::time::timeout(left, ws.next()).await {
                    Ok(Some(Ok(CMsg::Text(t)))) => {
                        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                        if v["t"] == "peercheck" {
                            return true;
                        }
                    }
                    Ok(Some(Ok(_))) => continue, // ping/pong
                    Ok(_) => return false,       // сокет закрылся
                    Err(_) => return false,      // срок вышел
                }
            }
        }

        /// Познакомить гостя с хостом (`connect`) и дождаться ответа.
        async fn connect_to<S>(ws: &mut S, host: &str)
        where
            S: SinkExt<CMsg, Error = tokio_tungstenite::tungstenite::Error>
                + StreamExt<Item = std::result::Result<CMsg, tokio_tungstenite::tungstenite::Error>>
                + Unpin,
        {
            let m = serde_json::json!({
                "t": "connect", "id": 1, "host_id": host, "candidates": ["45.11.22.44:40000"],
            });
            ws.send(CMsg::Text(m.to_string())).await.unwrap();
            loop {
                match reply(ws).await.as_str() {
                    "connected" => return,
                    "guest" | "hostok" => continue,
                    other => panic!("знакомство не состоялось: {other}"),
                }
            }
        }

        /// ПОДСКАЗКА «ПРОВЕРЬ СОСЕДА», НАПРАВЛЕНИЕ ХОСТ → ГОСТЬ.
        ///
        /// Прямое прощание по UDP шлёт САМ уходящий, и там, где его убили
        /// (`kill`, вылет, пропавшая сеть), послать его физически некому. У
        /// координатора же связь по TCP: сокет хоста рвётся, и это событие он
        /// обязан донести до гостя, который с этим хостом знакомился.
        #[tokio::test]
        async fn a_dying_host_makes_the_coordinator_hint_its_guest() {
            let addr = spawn_coord().await;
            let mut host = client(addr, "45.11.22.33").await;
            assert_eq!(announce(&mut host, "PAIRHOST0001", "tok").await, "hostok");
            let mut guest = client(addr, "45.11.22.44").await;
            connect_to(&mut guest, "PAIRHOST0001").await;

            drop(host); // хост УБИТ: прощания по UDP не будет

            assert!(
                hint_arrives(&mut guest, Duration::from_secs(5)).await,
                "гость не получил подсказку об уходе хоста — узнает о разрыве только по тишине",
            );
        }

        /// ТО ЖЕ В ОБРАТНУЮ СТОРОНУ: убитый гость → подсказка хосту.
        /// Хосту это нужно, чтобы освободить место и поправить счётчик гостей в
        /// каталоге, не дожидаясь keepalive-таймаута.
        #[tokio::test]
        async fn a_dying_guest_makes_the_coordinator_hint_its_host() {
            let addr = spawn_coord().await;
            let mut host = client(addr, "45.11.22.33").await;
            assert_eq!(announce(&mut host, "PAIRHOST0002", "tok").await, "hostok");
            let mut guest = client(addr, "45.11.22.44").await;
            connect_to(&mut guest, "PAIRHOST0002").await;
            assert_eq!(reply(&mut host).await, "guest", "хосту не доехали кандидаты гостя");

            drop(guest); // гость УБИТ

            assert!(
                hint_arrives(&mut host, Duration::from_secs(5)).await,
                "хост не получил подсказку об уходе гостя — место освободится только по таймауту",
            );
        }

        /// ПОДСКАЗКА АДРЕСНАЯ. Посторонний, который с этим хостом НЕ знакомился,
        /// её не получает: иначе уход одного гостя дёргал бы проверку живости у
        /// всех подряд, а сам факт «эта пара расходится» утекал бы наблюдателям.
        #[tokio::test]
        async fn the_hint_reaches_only_the_pair_it_belongs_to() {
            let addr = spawn_coord().await;
            let mut host = client(addr, "45.11.22.33").await;
            assert_eq!(announce(&mut host, "PAIRHOST0003", "tok").await, "hostok");
            let mut guest = client(addr, "45.11.22.44").await;
            connect_to(&mut guest, "PAIRHOST0003").await;
            // Посторонний: сокет открыт, но знакомства не просил.
            let mut stranger = client(addr, "45.11.22.55").await;

            drop(host);

            assert!(hint_arrives(&mut guest, Duration::from_secs(5)).await, "своя пара подсказку не получила");
            assert!(
                !hint_arrives(&mut stranger, Duration::from_secs(1)).await,
                "подсказка ушла постороннему: уход гостя дёргал бы проверку живости у всех подряд",
            );
        }
    }
}
