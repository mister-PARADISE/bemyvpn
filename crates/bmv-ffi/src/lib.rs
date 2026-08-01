//! bmv-ffi — ПЛОСКИЙ C-ABI поверх ядра BeMyVPN для не-JVM платформ (iOS/macOS).
//!
//! Те же операции, что даёт Android-мост (JNI), но экспортированы как обычные
//! C-функции — Swift (или любой C) зовёт их напрямую. Логику НЕ дублируем: всё
//! идёт в `bmv-core`, здесь только тонкая обёртка типов на границе FFI.
//!
//! Строки: вход — `const char*` (UTF-8, null-terminated); выход — `char*`,
//! который вызывающая сторона ОБЯЗАНА вернуть через `bmv_free_string`.
//!
//! Поток: почти все функции блокирующие (внутри `Runtime::block_on`) — зови их
//! из фонового потока, не из UI. Статус VPN читается неблокирующе.

use std::ffi::{CStr, CString};
use std::os::fd::RawFd;
use std::os::raw::c_char;
use std::pin::Pin;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::task::{Context, Poll};

use once_cell::sync::Lazy;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

// ── общее состояние (как в Android-мосте, но платформо-независимое) ───────────

static RUNTIME: Lazy<Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio runtime")
});

/// 0=выкл, 1=подключаюсь, 2=подключено, 3=ошибка.
static VPN_STATUS: AtomicI32 = AtomicI32::new(0);
/// FD туннеля (utun/TUN), которым качает pump_tunnel. Храним отдельно, чтобы
/// ЗАКРЫТЬ его ДЕТЕРМИНИРОВАННО на стопе. КРИТИЧНО для Android: там fd владеем
/// МЫ (VpnService.detachFd), и если задачу pump_tunnel аборуть, её финальный
/// `libc::close(fd)` НЕ выполнится → TUN висит → система держит VPN активным, а
/// трафик уходит в мёртвый туннель = «нет интернета после отключения». Поэтому
/// закрываем fd явно в bmv_stop. Закрытие ровно один раз (swap на -1).
static TUNNEL_FD: AtomicI32 = AtomicI32::new(-1);

/// Закрыть tunnel-fd РОВНО ОДИН РАЗ (кто первый — pump_tunnel в конце или
/// bmv_stop). Двойного close нет: swap возвращает прежнее, -1 = уже закрыт.
fn close_tunnel_fd() {
    let fd = TUNNEL_FD.swap(-1, Ordering::SeqCst);
    if fd >= 0 {
        unsafe { libc::close(fd) };
    }
}
/// Поколение попытки — инкремент в bmv_stop отменяет идущее подключение.
static CONNECT_GEN: AtomicU64 = AtomicU64::new(0);
/// Мягкая остановка гостевой сессии.
static STOP: Lazy<tokio::sync::Notify> = Lazy::new(tokio::sync::Notify::new);
/// Сигнал «сеть сменилась» — платформа (iOS NWPathMonitor) дёргает его при смене
/// интерфейса/вышки, чтобы ФОРСИРОВАТЬ реконнект не дожидаясь keepalive-таймаута.
static NUDGE: Lazy<tokio::sync::Notify> = Lazy::new(tokio::sync::Notify::new);
/// Готовый (пробитый+зашифрованный) канал между фазами connect → start_tunnel.
static PENDING_LINK: Lazy<Mutex<Option<Box<dyn bmv_common::Link>>>> = Lazy::new(|| Mutex::new(None));
/// Активная гостевая сессия (качалка пакетов).
static SESSION: Lazy<Mutex<Option<JoinHandle<()>>>> = Lazy::new(|| Mutex::new(None));
/// ЖИВОЙ канал текущей сессии — чтобы `bmv_stop` мог ПРЯМО и синхронно послать
/// BYE хосту, не полагаясь на тайминг async-ветки pump_tunnel. Ставится при
/// установке канала, снимается при его смерти/реконнекте.
static ACTIVE_LINK: Lazy<Mutex<Option<std::sync::Arc<dyn bmv_common::Link>>>> = Lazy::new(|| Mutex::new(None));
/// Хост-режим.
static HOST_SESSION: Lazy<Mutex<Option<JoinHandle<()>>>> = Lazy::new(|| Mutex::new(None));
static HOST_ENGINE: Lazy<Mutex<Option<bmv_core::BmvEngine>>> = Lazy::new(|| Mutex::new(None));
static HOST_STOP: Lazy<tokio::sync::Notify> = Lazy::new(tokio::sync::Notify::new);
/// Один кешированный движок сигналинга на адрес координатора (персистентный WS).
static SIG_ENGINE: Lazy<Mutex<Option<(String, bmv_core::BmvEngine)>>> = Lazy::new(|| Mutex::new(None));
/// Параметры гостя для АВТО-РЕКОННЕКТА. На мобильном смена вышки меняет NAT-мэппинг
/// → пробитая UDP-дырка протухает и туннель «висит». Ядро при обрыве само
/// переустанавливает канал (заново пробивает NAT), НЕ роняя utun — приложения не
/// видят разрыва VPN, а трафик возобновляется сам.
static RECONNECT: Lazy<Mutex<Option<GuestParams>>> = Lazy::new(|| Mutex::new(None));

#[derive(Clone)]
struct GuestParams {
    coordinator: String,
    host_id: String,
    password: Option<String>,
    proto: Option<String>,
}

fn set_status(v: i32) {
    VPN_STATUS.store(v, Ordering::SeqCst);
}

fn sig_engine(coordinator: &str) -> bmv_core::BmvEngine {
    let mut g = SIG_ENGINE.lock().unwrap();
    if let Some((url, e)) = g.as_ref() {
        if url == coordinator {
            return e.clone();
        }
    }
    let cfg = bmv_config::Config { coordinators: vec![coordinator.to_string()], ..Default::default() };
    let e = bmv_core::BmvEngine::from_config(cfg);
    *g = Some((coordinator.to_string(), e.clone()));
    e
}

// Логгер НЕ ставится сознательно: без `log::set_logger` все `log::` вызовы —
// no-op, ничего не пишется и не копится. Хост физически не имеет записей о
// трафике гостей и не может их выдать. Не возвращать буфер логов.

// ── C-строки: helpers ────────────────────────────────────────────────────────

/// `const char*` → String (пустая при null/битом UTF-8).
fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("").to_string()
}

/// String → свежий `char*` для возврата в Swift (освобождать `bmv_free_string`).
fn to_c(s: String) -> *mut c_char {
    CString::new(s).map(|c| c.into_raw()).unwrap_or(std::ptr::null_mut())
}

/// Освободить строку, полученную из любой bmv_* функции.
///
/// # Safety
/// `p` — ровно тот указатель, что вернула bmv_*-функция (или null); освобождать
/// один раз. ABI не меняется — для Swift/Kotlin функция выглядит как раньше.
#[no_mangle]
pub unsafe extern "C" fn bmv_free_string(p: *mut c_char) {
    if !p.is_null() {
        unsafe { drop(CString::from_raw(p)) };
    }
}

// ── сигналинг (каталог / коды / IP / связь) ──────────────────────────────────

/// Живой каталог: since из прошлого ответа. JSON `{"version":N,"hosts":[...]}`.
#[no_mangle]
pub extern "C" fn bmv_list_watch(coordinator: *const c_char, since: u64) -> *mut c_char {
    let engine = sig_engine(&cstr(coordinator));
    let r = RUNTIME.block_on(async { engine.guest_watch(None, true, since).await });
    let json = match r {
        Ok(u) => format!("{{\"version\":{},\"hosts\":{}}}", u.version, hosts_to_json(&u.hosts)),
        Err(e) => {
            log::error!("bmv_list_watch: {e}");
            "{\"version\":0,\"hosts\":[]}".to_string()
        }
    };
    to_c(json)
}

/// Найти хост по КОДУ (в т.ч. скрытый). JSON-объект хоста или "" если не найден.
#[no_mangle]
pub extern "C" fn bmv_resolve(coordinator: *const c_char, code: *const c_char) -> *mut c_char {
    let engine = sig_engine(&cstr(coordinator));
    let code = cstr(code);
    let found = RUNTIME.block_on(async { engine.guest_resolve(&code).await });
    let json = match found {
        Ok(Some(h)) => {
            let arr = hosts_to_json(std::slice::from_ref(&h));
            arr.trim_start_matches('[').trim_end_matches(']').to_string()
        }
        _ => String::new(),
    };
    to_c(json)
}

/// Новый код хоста ОТ СЕРВЕРА как "CODE|SIG" ("" при ошибке).
#[no_mangle]
pub extern "C" fn bmv_new_code(coordinator: *const c_char) -> *mut c_char {
    let engine = sig_engine(&cstr(coordinator));
    let (code, sig) = RUNTIME.block_on(async { engine.host_new_code().await }).unwrap_or_default();
    to_c(if code.is_empty() { String::new() } else { format!("{code}|{sig}") })
}

/// Отклик до хоста в миллисекундах, БЕЗ подключения к нему. -1 = не ответил.
///
/// `endpoints` — адреса хоста из каталога, разделённые запятой. Зовётся, когда
/// человек раскрыл карточку хоста: проба это сетевой запрос к чужой машине, и
/// делать её для всего списка сразу незачем. Сессию на хосте НЕ создаёт
/// (см. bmv_net::ping_tokens).
#[no_mangle]
pub extern "C" fn bmv_probe_rtt(
    coordinator: *const c_char,
    host_id: *const c_char,
    endpoints: *const c_char,
) -> i32 {
    let eps: Vec<String> = cstr(endpoints)
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if eps.is_empty() {
        return -1;
    }
    let engine = sig_engine(&cstr(coordinator));
    let id = cstr(host_id);
    RUNTIME
        .block_on(async { engine.probe_host_rtt(&id, &eps).await })
        .map(|ms| ms.min(i32::MAX as u32) as i32)
        .unwrap_or(-1)
}

/// Свой внешний IP через координатор ("" при ошибке).
#[no_mangle]
pub extern "C" fn bmv_my_ip(coordinator: *const c_char) -> *mut c_char {
    let engine = sig_engine(&cstr(coordinator));
    let ip = RUNTIME
        .block_on(async { tokio::time::timeout(std::time::Duration::from_secs(6), engine.my_ip()).await })
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    to_c(ip)
}

/// Быстрая проверка связи с координатором. true = сервер жив.
#[no_mangle]
pub extern "C" fn bmv_health(coordinator: *const c_char) -> bool {
    let engine = sig_engine(&cstr(coordinator));
    RUNTIME.block_on(async { engine.coordinator_health().await.is_ok() })
}

/// Круг до координатора в мс; 0 — ещё не мерили или связи нет.
///
/// Шеллы раньше засекали, сколько длится сам `bmv_health`, и показывали это как
/// пинг. Но health читает флаг «сокет жив» и возвращается мгновенно — на экране
/// стояло время вызова функции, а не время до сервера. Здесь настоящий замер:
/// свой Ping с меткой времени и Pong с той же меткой (см. bmv_signal).
#[no_mangle]
pub extern "C" fn bmv_rtt_ms(coordinator: *const c_char) -> u32 {
    sig_engine(&cstr(coordinator)).coordinator_rtt().unwrap_or(0)
}

// ── гость: подключение ───────────────────────────────────────────────────────

/// ФАЗА 1: пробитие NAT + рукопожатие БЕЗ TUN. true — канал поднят (зови
/// bmv_start_tunnel с fd от Packet Tunnel). false — не удалось (маршрут цел).
#[no_mangle]
pub extern "C" fn bmv_connect(
    coordinator: *const c_char,
    host_id: *const c_char,
    password: *const c_char,
    protocol: *const c_char,
) -> bool {
    let coordinator = cstr(coordinator);
    let host_id = cstr(host_id);
    let password = cstr(password);
    let protocol = cstr(protocol);
    let gen0 = CONNECT_GEN.load(Ordering::SeqCst);
    set_status(1);

    let engine = sig_engine(&coordinator);
    let pw = if password.is_empty() { None } else { Some(password) };
    let proto = if protocol.is_empty() { None } else { Some(protocol) };
    // Копия параметров для авто-реконнекта — async-блок ниже забирает оригиналы.
    let reconnect_params = GuestParams {
        coordinator: coordinator.clone(),
        host_id: host_id.clone(),
        password: pw.clone(),
        proto: proto.clone(),
    };

    let result: std::result::Result<_, String> = RUNTIME.block_on(async move {
        let establish = async {
            let mut last = String::new();
            for attempt in 1..=2 {
                match engine.guest_establish(&host_id, pw.as_deref(), proto.as_deref()).await {
                    Ok(v) => return Ok(v),
                    Err(e) => {
                        log::warn!("bmv_connect попытка {attempt}: {e}");
                        last = e.to_string();
                    }
                }
            }
            Err(last)
        };
        tokio::select! {
            r = establish => r,
            _ = STOP.notified() => Err("отменено пользователем".to_string()),
        }
    });

    let cancelled = CONNECT_GEN.load(Ordering::SeqCst) != gen0;
    match result {
        Ok((peer, link)) if !cancelled => {
            log::info!("bmv_connect: канал поднят к {peer}");
            // Запоминаем параметры — по ним pump_tunnel сам переустановит канал
            // при обрыве пути (мобильный роуминг), не роняя utun.
            *RECONNECT.lock().unwrap() = Some(reconnect_params);
            *PENDING_LINK.lock().unwrap() = Some(link);
            true
        }
        _ => {
            set_status(0);
            false
        }
    }
}

/// ФАЗА 2: качать пакеты через утун-fd на уже поднятом канале. fd даёт Packet
/// Tunnel (iOS) / VpnService (Android). `utun`=true — снимать/добавлять
/// 4-байтовый заголовок (iOS/macOS utun); false — сырой IP (Android). true —
/// качалка запущена.
#[no_mangle]
pub extern "C" fn bmv_start_tunnel(fd: i32, utun: bool) -> bool {
    let link = match PENDING_LINK.lock().unwrap().take() {
        Some(l) => l,
        None => {
            log::error!("bmv_start_tunnel: нет готового канала");
            set_status(3);
            return false;
        }
    };
    let params = RECONNECT.lock().unwrap().clone();
    let gen = CONNECT_GEN.load(Ordering::SeqCst);
    TUNNEL_FD.store(fd, Ordering::SeqCst); // чтобы bmv_stop мог закрыть fd детерминированно
    log::info!("bmv_start_tunnel: fd={fd}, utun={utun}, качаю туннель (с авто-реконнектом)");
    let handle = RUNTIME.spawn(async move {
        set_status(2);
        pump_tunnel(fd, utun, link, params, gen).await;
        log::info!("туннель завершён");
    });
    *SESSION.lock().unwrap() = Some(handle);
    true
}

/// Текущий статус VPN (0/1/2/3). Неблокирующая.
#[no_mangle]
pub extern "C" fn bmv_vpn_status() -> i32 {
    VPN_STATUS.load(Ordering::SeqCst)
}

/// Остановить VPN (отменяет и идущее подключение). Маршрут спадает у вызывающего
/// (Swift закрывает утун), здесь — гасим сессию и прощаемся с хостом.
/// Пометить сеанс остановленным (авто-реконнект ОТМЕНЁН) и синхронно попрощаться
/// с хостом — послать BYE на живом канале. КРИТИЧНО ставить стоп-флаги ДО закрытия
/// канала: иначе pump_tunnel увидит «канал умер» и переустановит его — клиент
/// послал бы BYE и тут же переподключился. BYE идёт внутри зашифрованной сессии
/// (Noise), поэтому подделать его за чужого гостя нельзя — только сам гость,
/// владеющий сессионными ключами, может послать валидный BYE.
fn mark_stop_and_farewell() {
    CONNECT_GEN.fetch_add(1, Ordering::SeqCst); // отменяет авто-реконнект
    *RECONNECT.lock().unwrap() = None;
    STOP.notify_waiters();
    if let Some(l) = ACTIVE_LINK.lock().unwrap().take() {
        RUNTIME.block_on(async {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(700), l.close()).await;
        });
    }
}

#[no_mangle]
pub extern "C" fn bmv_stop() {
    mark_stop_and_farewell();
    // Фолбэк: гасим сессию принудительно (если не вышла сама по STOP).
    if let Some(h) = SESSION.lock().unwrap().take() {
        h.abort();
    }
    // ЗАКРЫВАЕМ TUN-fd ЯВНО: abort роняет задачу pump_tunnel ДО её финального
    // close(fd), поэтому закрываем здесь — иначе на Android TUN висел бы и система
    // держала VPN активным (интернет пропадает). Idempotent (close-once).
    close_tunnel_fd();
    *PENDING_LINK.lock().unwrap() = None;
    set_status(0);
}

/// Попрощаться с хостом (BYE) ДО остановки туннеля — из app→extension сообщения,
/// ПОКА сокет жив. На stopTunnel iOS уже рвёт ресурсы туннеля, и отправка BYE по
/// UDP молча проваливается, поэтому основной BYE шлётся отсюда, заранее. После
/// этого pump_tunnel сам выйдет (стоп-флаги уже стоят), а bmv_stop добьёт сессию.
#[no_mangle]
pub extern "C" fn bmv_send_bye() {
    mark_stop_and_farewell();
}

/// Платформа сообщает о СМЕНЕ СЕТИ (iOS NWPathMonitor: WiFi↔сотовая, смена вышки).
/// Форсирует немедленный реконнект гостевого туннеля, не дожидаясь keepalive —
/// utun при этом НЕ роняется, приложения разрыва не видят. Неблокирующая, безопасна
/// вне активной сессии (просто no-op, если качать нечего).
#[no_mangle]
pub extern "C" fn bmv_nudge_reconnect() {
    NUDGE.notify_waiters();
}

// ── хост-режим ───────────────────────────────────────────────────────────────

/// Стать хостом: раздать интернет (userspace).
///
/// Возвращает "КОД|ПОДПИСЬ" — пару, а не один код: при протухшей подписи ядро
/// САМО берёт свежий код у сервера и повторяет анонс, и тогда меняются обе части.
/// Приложение обязано сохранить обе, иначе при следующем запуске уйдёт новый код
/// со старой подписью и координатор снова ответит отказом.
///
/// Либо сентинел: "!NAT" (нет публичного адреса) / "!SIG" (взять свежий код тоже
/// не вышло) / "" (иная ошибка). Чёрточки в сентинелах нет — их не спутать.
#[no_mangle]
pub extern "C" fn bmv_host_start(
    coordinator: *const c_char,
    host_id: *const c_char,
    token: *const c_char,
    code_sig: *const c_char,
    name: *const c_char,
    max_guests: i32,
    password: *const c_char,
    protocol: *const c_char,
    public: bool,
) -> *mut c_char {
    let mut cfg = bmv_config::Config { coordinators: vec![cstr(coordinator)], ..Default::default() };
    let stable_id = cstr(host_id);
    if !stable_id.is_empty() {
        cfg.host.id = stable_id;
    }
    cfg.host.token = cstr(token);
    cfg.host.code_sig = cstr(code_sig);
    cfg.host.name = cstr(name);
    cfg.host.password = cstr(password);
    cfg.host.public = public;
    cfg.host.max_guests = if max_guests > 0 { max_guests as u32 } else { 8 };
    let proto = cstr(protocol);
    if !proto.is_empty() {
        cfg.default_protocol = proto;
    }
    let mut engine = bmv_core::BmvEngine::from_config(cfg.clone());
    let mut host_id = engine.host_id().to_string();
    // Подпись возвращаем ВМЕСТЕ с кодом: при самолечении меняются обе, и если
    // отдать только код, приложение сохранит новый код со старой подписью —
    // при следующем запуске координатор снова ответит 403.
    let mut host_sig = cfg.host.code_sig.clone();
    *HOST_ENGINE.lock().unwrap() = Some(engine.clone());

    let mut announce = RUNTIME.block_on(engine.host_bind_announce());

    // ПРОТУХШАЯ ПОДПИСЬ ЛЕЧИТСЯ САМА. Координатор отвечает 403, если подпись кода
    // не сходится — так бывает после смены координатора или его секрета. Раньше
    // мобилки на это отдавали «!SIG», а человек читал «Код устарел, обновите и
    // повторите» и должен был сам нажать «Новый код»: то есть раздача не
    // включалась с первого раза без всякой его вины. Десктоп уже давно берёт
    // свежий код молча и повторяет анонс — делаем то же здесь, и правило
    // становится общим для Android и iOS сразу.
    if announce.as_ref().err().map(|e| e.to_string().contains("403")).unwrap_or(false) {
        log::warn!("хост-режим: подпись кода не принята — беру свежий код");
        let fresh = RUNTIME.block_on(async {
            bmv_core::BmvEngine::from_config(bmv_config::Config {
                coordinators: cfg.coordinators.clone(),
                ..Default::default()
            })
            .host_new_code()
            .await
        });
        if let Ok((c, sg)) = fresh {
            if !c.is_empty() && !sg.is_empty() {
                let mut cfg2 = cfg.clone();
                cfg2.host.id = c;
                cfg2.host.code_sig = sg.clone();
                engine = bmv_core::BmvEngine::from_config(cfg2);
                host_id = engine.host_id().to_string();
                host_sig = sg;
                *HOST_ENGINE.lock().unwrap() = Some(engine.clone());
                announce = RUNTIME.block_on(engine.host_bind_announce());
            }
        }
    }

    let hub = match announce {
        Ok((hub, _, eps)) => {
            log::info!("хост-режим: анонсирован #{host_id} ({eps:?})");
            hub
        }
        Err(e) => {
            let msg = e.to_string();
            log::error!("хост-режим не поднялся: {msg}");
            let sentinel = if msg.contains("422") {
                "!NAT"
            } else if msg.contains("403") {
                "!SIG"
            } else {
                ""
            };
            return to_c(sentinel.to_string());
        }
    };

    let handle = RUNTIME.spawn(async move {
        let beat = engine.clone();
        let beat_hub = hub.clone();
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let _ = beat.host_heartbeat(&beat_hub).await;
            }
        });
        let punch_engine = engine.clone();
        let punch_hub = hub.clone();
        let puncher = tokio::spawn(async move {
            let _ = punch_engine.host_serve_punch(punch_hub).await;
        });
        let accept = async {
            while let Some((peer, raw)) = hub.accept().await {
                log::info!("хост-режим: гость {peer}");
                let e = engine.clone();
                tokio::spawn(async move {
                    match e.host_run_session(peer, raw, true).await {
                        Ok(()) => log::info!("гость {peer} отключился"),
                        Err(err) => log::warn!("гость {peer}: {err}"),
                    }
                });
            }
        };
        tokio::select! {
            _ = accept => {}
            _ = HOST_STOP.notified() => log::info!("хост-режим: стоп"),
        }
        heartbeat.abort();
        puncher.abort();
    });
    *HOST_SESSION.lock().unwrap() = Some(handle);
    // «код|подпись» — сентинелы (!NAT/!SIG/пусто) чёрточки не содержат.
    to_c(format!("{host_id}|{host_sig}"))
}

/// Перестать быть хостом (снимает запись из каталога, глушит фоновые задачи).
#[no_mangle]
pub extern "C" fn bmv_host_stop() {
    if let Some(engine) = HOST_ENGINE.lock().unwrap().take() {
        let _ = RUNTIME.block_on(async { engine.host_deannounce().await });
    }
    HOST_STOP.notify_waiters();
    if let Some(h) = HOST_SESSION.lock().unwrap().take() {
        h.abort();
    }
}

/// Сменить имя/лимит/пароль/протокол/видимость хоста НА ЛЕТУ.
#[no_mangle]
pub extern "C" fn bmv_host_update(
    name: *const c_char,
    max_guests: i32,
    password: *const c_char,
    protocol: *const c_char,
    public: bool,
) {
    let engine = match HOST_ENGINE.lock().unwrap().clone() {
        Some(e) => e,
        None => return,
    };
    let name = cstr(name);
    let pw = cstr(password);
    let proto = cstr(protocol);
    RUNTIME.spawn(async move {
        if !name.is_empty() {
            let _ = engine.host_set_name(&name).await;
        }
        if max_guests > 0 {
            let _ = engine.host_set_max_guests(max_guests as u32).await;
        }
        if !proto.is_empty() {
            let _ = engine.host_set_protocol(&proto).await;
        }
        let _ = engine.host_set_password(&pw).await;
        let _ = engine.host_set_public(public).await;
    });
}

// ── карточки каталога → JSON (как в Android-мосте) ────────────────────────────

fn hosts_to_json(list: &[bmv_signal::HostInfo]) -> String {
    let arr: Vec<serde_json::Value> = list
        .iter()
        .map(|h| {
            let ip = if !h.ip.is_empty() {
                h.ip.as_str()
            } else {
                h.endpoints.first().map(|s| s.as_str()).unwrap_or("")
            };
            serde_json::json!({
                "id": h.id,
                "name": h.name,
                "ip": ip,
                "country": h.country,
                "guests": h.guests,
                "max": h.max_guests,
                "hasPassword": h.has_password,
                "online": h.online,
                "public": h.public,
                "protocol": h.protocol,
                // Адреса нужны приложению, чтобы замерить отклик до хоста ДО
                // подключения (bmv_probe_rtt). Одной строкой через запятую —
                // разбирать массив на каждой платформе ради этого незачем.
                "endpoints": h.endpoints.join(","),
            })
        })
        .collect();
    serde_json::Value::Array(arr).to_string()
}

// ── TUN-fd как async-устройство (тот же код, что в Android-мосте) ─────────────

/// Качать туннель с АВТО-РЕКОННЕКТОМ. utun-fd открыт ОДИН раз и живёт весь сеанс;
/// при обрыве канала (смена вышки на мобильном, протухший NAT, пропал хост) ядро
/// само переустанавливает Link по сохранённым параметрам, НЕ трогая utun —
/// приложения не видят разрыва VPN, трафик возобновляется. Сдаёмся только после
/// серии неудач подряд (хост реально мёртв) или по стопу пользователя.
async fn pump_tunnel(
    fd: RawFd,
    utun: bool,
    first_link: Box<dyn bmv_common::Link>,
    params: Option<GuestParams>,
    gen: u64,
) {
    /// Столько неудачных реконнектов ПОДРЯД → считаем хост мёртвым, выходим.
    /// ~20 попыток с бэкоффом до 8с ≈ 2 минуты — переживает мёртвые зоны (тоннель,
    /// метро, лифт), но не крутит вечно, если хост реально пропал.
    const MAX_FAILS: u32 = 20;
    let mut link: Option<Box<dyn bmv_common::Link>> = Some(first_link);
    let mut fails: u32 = 0;

    'session: loop {
        if CONNECT_GEN.load(Ordering::SeqCst) != gen {
            break; // пользователь остановил / началось новое подключение
        }
        // Взять текущий канал или ПЕРЕУСТАНОВИТЬ (авто-реконнект).
        let l = match link.take() {
            Some(l) => l,
            None => {
                let Some(p) = params.clone() else { break };
                set_status(1); // reasserting: VPN «жив», но временно переподключается
                // Бэкофф растёт с числом неудач (0.5с…8с), прерывается стопом ИЛИ
                // сменой сети (nudge) — тогда пробуем сразу, не досыпая.
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(500u64 << fails.min(4))) => {}
                    _ = NUDGE.notified() => {}
                    _ = STOP.notified() => break,
                }
                if CONNECT_GEN.load(Ordering::SeqCst) != gen { break; }
                let engine = sig_engine(&p.coordinator);
                let got = tokio::select! {
                    r = tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        engine.guest_establish(&p.host_id, p.password.as_deref(), p.proto.as_deref()),
                    ) => r.ok().and_then(|r| r.ok()),
                    _ = STOP.notified() => break,
                };
                match got {
                    Some((peer, l)) => {
                        log::info!("авто-реконнект: канал восстановлен к {peer}");
                        fails = 0;
                        set_status(2);
                        l
                    }
                    None => {
                        fails += 1;
                        log::warn!("авто-реконнект: неудача {fails}/{MAX_FAILS}");
                        if fails >= MAX_FAILS {
                            log::error!("авто-реконнект: хост недостижим — выходим");
                            break;
                        }
                        continue 'session;
                    }
                }
            }
        };

        // Свежий device на ТОМ ЖЕ fd (TunFd НЕ закрывает fd на drop).
        let device = match TunFd::new(fd, utun) {
            Ok(d) => d,
            Err(e) => {
                log::error!("utun недоступен: {e}");
                let _ = l.close().await;
                break;
            }
        };
        let l_arc: std::sync::Arc<dyn bmv_common::Link> = std::sync::Arc::from(l);
        // Публикуем живой канал: bmv_stop пошлёт по нему BYE ПРЯМО и синхронно.
        *ACTIVE_LINK.lock().unwrap() = Some(l_arc.clone());
        tokio::select! {
            _ = bmv_tunnel::run_guest(device, l_arc.clone()) => {
                // Канал умер (обрыв пути / хост пропал) → следующая итерация
                // переустановит его (link уже None). utun не трогаем.
                log::info!("канал оборвался — пробую восстановить…");
            }
            _ = NUDGE.notified() => {
                // Платформа сообщила о смене сети (новая вышка/интерфейс). Старый
                // NAT-мэппинг мёртв — не ждём keepalive-таймаут, реконнектим сразу.
                log::info!("сеть сменилась — форсирую реконнект");
                // link уже None → следующая итерация переустановит канал.
            }
            _ = STOP.notified() => {
                break 'session; // BYE уже отправил bmv_stop синхронно
            }
        }
        // Канал больше не актуален (умер/реконнект) — снимаем публикацию.
        *ACTIVE_LINK.lock().unwrap() = None;
    }

    // utun-fd закрываем ОДИН раз (device'ы его не закрывали). close-once: если
    // bmv_stop уже закрыл — тут no-op; `fd` == TUNNEL_FD, снятый на старте туннеля.
    let _ = fd;
    close_tunnel_fd();
    let _ = VPN_STATUS.compare_exchange(2, 0, Ordering::SeqCst, Ordering::SeqCst);
    let _ = VPN_STATUS.compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst);
}

/// fd-обёртка БЕЗ владения: drop НЕ закрывает дескриптор (AsyncFd только
/// снимает его с реактора). utun-fd так переживает реконнекты — pump_tunnel
/// закрывает его ровно один раз в самом конце сеанса.
struct BorrowedTunFd(RawFd);
impl std::os::fd::AsRawFd for BorrowedTunFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

struct TunFd {
    inner: AsyncFd<BorrowedTunFd>,
    /// iOS/macOS utun: каждый пакет на fd обёрнут 4-байтовым заголовком с семейством
    /// адресов (AF_INET/AF_INET6, big-endian u32). Снимаем на чтении, добавляем на
    /// записи — тогда ядру достаётся/отдаётся чистый IP, как на Android-TUN.
    utun: bool,
}

impl TunFd {
    fn new(fd: RawFd, utun: bool) -> std::io::Result<Self> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(TunFd { inner: AsyncFd::new(BorrowedTunFd(fd))?, utun })
    }
}

impl AsyncRead for TunFd {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        use std::os::fd::AsRawFd;
        let this = self.get_mut();
        let utun = this.utun;
        loop {
            let mut guard = match this.inner.poll_read_ready(cx) {
                Poll::Ready(Ok(g)) => g,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };
            let unfilled = buf.initialize_unfilled();
            let res = guard.try_io(|inner| {
                let fd = inner.as_raw_fd();
                if utun {
                    // readv: 4-байтовый заголовок в отдельный буфер, IP-пакет — сразу
                    // в буфер вызова. Отдаём только длину пакета (без заголовка).
                    let mut hdr = [0u8; 4];
                    let iov = [
                        libc::iovec { iov_base: hdr.as_mut_ptr() as *mut libc::c_void, iov_len: 4 },
                        libc::iovec { iov_base: unfilled.as_mut_ptr() as *mut libc::c_void, iov_len: unfilled.len() },
                    ];
                    let n = unsafe { libc::readv(fd, iov.as_ptr(), 2) };
                    if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok((n as usize).saturating_sub(4)) }
                } else {
                    let n = unsafe { libc::read(fd, unfilled.as_mut_ptr() as *mut libc::c_void, unfilled.len()) };
                    if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(n as usize) }
                }
            });
            match res {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl AsyncWrite for TunFd {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<std::io::Result<usize>> {
        use std::os::fd::AsRawFd;
        let this = self.get_mut();
        let utun = this.utun;
        loop {
            let mut guard = match this.inner.poll_write_ready(cx) {
                Poll::Ready(Ok(g)) => g,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };
            let res = guard.try_io(|inner| {
                let fd = inner.as_raw_fd();
                if utun {
                    if data.is_empty() {
                        return Ok(0usize);
                    }
                    // Семейство по версии IP: 4→AF_INET(2), 6→AF_INET6(30). Заголовок —
                    // это семейство как big-endian u32, т.е. [0,0,0,af]. writev шлёт
                    // заголовок+пакет одной атомарной датаграммой.
                    let af: u8 = if data[0] >> 4 == 6 { 30 } else { 2 };
                    let hdr = [0u8, 0, 0, af];
                    let iov = [
                        libc::iovec { iov_base: hdr.as_ptr() as *mut libc::c_void, iov_len: 4 },
                        libc::iovec { iov_base: data.as_ptr() as *mut libc::c_void, iov_len: data.len() },
                    ];
                    let n = unsafe { libc::writev(fd, iov.as_ptr(), 2) };
                    if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(data.len()) }
                } else {
                    let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
                    if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(n as usize) }
                }
            });
            match res {
                Ok(Ok(n)) => return Poll::Ready(Ok(n)),
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Доказательство фикса «VPN висит после отключения»: tunnel-fd ДОЛЖЕН
    /// закрыться на стопе (иначе система держит VPN активным, интернет пропадает).
    /// Кладём реальный fd (из pipe) в TUNNEL_FD, зовём close_tunnel_fd — fd закрыт
    /// (fcntl → EBADF), TUNNEL_FD снова -1, повторный вызов безопасен (close-once).
    #[test]
    fn tunnel_fd_closed_once_on_stop() {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe()");
        let (r, w) = (fds[0], fds[1]);
        TUNNEL_FD.store(w, Ordering::SeqCst);
        // fd пока валиден.
        assert_ne!(unsafe { libc::fcntl(w, libc::F_GETFD) }, -1, "fd должен быть валиден до close");
        close_tunnel_fd();
        // fd закрыт, слот обнулён.
        assert_eq!(unsafe { libc::fcntl(w, libc::F_GETFD) }, -1, "fd должен быть закрыт");
        assert_eq!(TUNNEL_FD.load(Ordering::SeqCst), -1, "TUNNEL_FD снова -1");
        // Повторный вызов не должен ничего ломать (close-once).
        close_tunnel_fd();
        unsafe { libc::close(r) };
    }

    /// ГЛАВНОЕ доказательство фикса «быстрый выход»: bmv_stop ОБЯЗАН синхронно
    /// послать BYE на живой канал ДО возврата. Кладём KeepaliveLink в ACTIVE_LINK,
    /// зовём bmv_stop, и на другом конце СРАЗУ должен лежать BYE-маркер [1].
    /// Если bmv_stop вернулся, не отправив BYE — тест падает (recv по таймауту).
    #[test]
    fn bmv_stop_sends_bye_synchronously() {
        let (a, b) = bmv_common::wire::memory_pair(64);
        // KeepaliveLink спавнит пингер — создаём в контексте рантайма, затем выходим.
        let ka: Box<dyn bmv_common::Link> = {
            let _g = RUNTIME.enter();
            Box::new(bmv_common::KeepaliveLink::new(a))
        };
        *ACTIVE_LINK.lock().unwrap() = Some(std::sync::Arc::from(ka));
        *SESSION.lock().unwrap() = None;

        bmv_stop(); // должен СИНХРОННО отправить BYE и только потом вернуться

        // BYE уже должен быть на проводе (bmv_stop вернулся). Пингер шлёт первый
        // KEEPALIVE не раньше 1.5с, так что первым придёт именно BYE=[1].
        let got = RUNTIME
            .block_on(async { tokio::time::timeout(std::time::Duration::from_millis(500), b.recv()).await })
            .expect("BYE не пришёл за 500мс — bmv_stop вернулся, НЕ отправив BYE")
            .unwrap();
        assert_eq!(got, vec![1u8], "ожидали BYE-маркер [1] сразу после bmv_stop");
        // Прибираем глобальное состояние за собой.
        *ACTIVE_LINK.lock().unwrap() = None;
    }
}
