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

// Весь крейт — это граница C: КАЖДАЯ точка входа принимает сырые указатели и
// разыменовывает их (внутри `cstr`, помеченной `unsafe`). Пометить сами
// `bmv_*` как `unsafe extern "C"` нельзя: Android-мост (apps/android/rust,
// вне workspace) зовёт их как обычные Rust-функции — сборка APK молча
// сломалась бы, а CI этого не увидел бы. Контракт вместо пометки описан в
// `cstr`: указатель либо null, либо валидная нуль-терминированная строка.
//
// Оба обещания моста — «не помечена unsafe» и «тело завёрнуто в `ffi!`» —
// теперь ПРОВЕРЯЮТСЯ, а не обещаются здесь на словах: сторож
// `every_bridge_entry_keeps_the_shape_of_all_the_others` внизу файла.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{CStr, CString};
use std::os::fd::RawFd;
use std::os::raw::c_char;
use std::pin::Pin;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

/// Не пускать панику через границу C.
///
/// `extern "C"`-функция при развёртывании стека вызывает `abort` — приложение
/// исчезает с экрана мгновенно и без следа, а на iOS это ещё и «краш» в глазах
/// системы. Любая паника внутри (битый JSON от чужой машины, `unwrap` в чужом
/// коде, OOM в аллокаторе) обязана превратиться в честный неуспех вызова.
/// Одним макросом, а не двумя десятками копий: обёртка должна быть одинаковой
/// во всех точках входа, иначе новую функцию забудут прикрыть.
macro_rules! ffi {
    ($fallback:expr, $body:block) => {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body)) {
            Ok(v) => v,
            Err(_) => {
                log::error!("паника в FFI-вызове подавлена (иначе abort процесса)");
                $fallback
            }
        }
    };
}

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
/// ПОЧЕМУ сеанс кончился сам: 0 — не кончался (идёт/выключили сами),
/// 1 — ХОСТ ЗАВЕРШИЛ РАЗДАЧУ (прислал BYE), 2 — связь с хостом потеряна.
///
/// Отдельно от `VPN_STATUS`, потому что для оболочки это разные вопросы: статус
/// отвечает «работает ли VPN», причина — «что человеку теперь делать». Без неё
/// оба конца сеанса выглядят одинаково (статус 3), и штатно погашенная раздача
/// показывалась бы как поломка.
///
/// Сбрасывается ТОЛЬКО в `bmv_connect` (начало новой попытки), а НЕ в `bmv_stop`:
/// шелл узнаёт о конце сеанса опросом статуса, и на Android/iOS сторож туннеля
/// успевает позвать стоп раньше, чем экран опросит статус, — сброс в стопе съел
/// бы причину прямо перед показом.
static STOP_REASON: AtomicI32 = AtomicI32::new(0);
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
/// Мягкая остановка гостевой сессии — СЧЁТЧИК стопов, а не `Notify`.
///
/// `Notify::notify_waiters` будит только тех, кто УЖЕ ждёт: стоп, пришедший в
/// щель между двумя `select!`, терялся молча, и качалку приходилось добивать
/// abort'ом (а он рубит задачу вместе с её `close(fd)`). `watch` хранит
/// состояние: подписчик берёт `subscribe()` ОДИН раз в начале и увидит любой
/// последующий инкремент, даже если в момент сигнала ничего не ждал.
static STOP: Lazy<tokio::sync::watch::Sender<u64>> = Lazy::new(|| tokio::sync::watch::channel(0).0);
/// Сигнал «сеть сменилась» — платформа (iOS NWPathMonitor) дёргает его при смене
/// интерфейса/вышки, чтобы ФОРСИРОВАТЬ реконнект не дожидаясь keepalive-таймаута.
static NUDGE: Lazy<tokio::sync::Notify> = Lazy::new(tokio::sync::Notify::new);
/// Готовый (пробитый+зашифрованный) канал между фазами connect → start_tunnel.
static PENDING_LINK: Lazy<Mutex<Option<Box<dyn bmv_common::Link>>>> = Lazy::new(|| Mutex::new(None));
/// Номер ожидающего канала — растёт на каждую удачную фазу 1. Нужен сторожу
/// фазы 2, чтобы он гасил ИМЕННО СВОЙ канал: два подключения подряд без стопа
/// иначе привели бы к тому, что сторож первого прибил канал второго.
static PENDING_SEQ: AtomicU64 = AtomicU64::new(0);
/// Активная гостевая сессия (качалка пакетов).
static SESSION: Lazy<Mutex<Option<JoinHandle<()>>>> = Lazy::new(|| Mutex::new(None));
/// ЖИВОЙ канал текущей сессии — чтобы `bmv_stop` мог ПРЯМО и синхронно послать
/// BYE хосту, не полагаясь на тайминг async-ветки pump_tunnel. Ставится при
/// установке канала, снимается при его смерти/реконнекте.
static ACTIVE_LINK: Lazy<Mutex<Option<std::sync::Arc<dyn bmv_common::Link>>>> = Lazy::new(|| Mutex::new(None));
/// Хост-режим.
static HOST_SESSION: Lazy<Mutex<Option<JoinHandle<()>>>> = Lazy::new(|| Mutex::new(None));
static HOST_ENGINE: Lazy<Mutex<Option<bmv_core::BmvEngine>>> = Lazy::new(|| Mutex::new(None));
/// Стоп хост-режима — счётчик, а не `Notify`, по той же причине, что и `STOP`.
static HOST_STOP: Lazy<tokio::sync::watch::Sender<u64>> = Lazy::new(|| tokio::sync::watch::channel(0).0);
/// Один кешированный движок сигналинга на адрес координатора (персистентный WS).
static SIG_ENGINE: Lazy<Mutex<Option<(String, bmv_core::BmvEngine)>>> = Lazy::new(|| Mutex::new(None));
/// Параметры гостя для АВТО-РЕКОННЕКТА. На мобильном смена вышки меняет NAT-мэппинг
/// → пробитая UDP-дырка протухает и туннель «висит». Ядро при обрыве само
/// переустанавливает канал (заново пробивает NAT), НЕ роняя utun — приложения не
/// видят разрыва VPN, а трафик возобновляется сам.
static RECONNECT: Lazy<Mutex<Option<GuestParams>>> = Lazy::new(|| Mutex::new(None));

/// Сколько ждём, пока качалка выйдет САМА, прежде чем рубить её abort'ом.
/// Обычный выход занимает миллисекунды (канал уже закрыт, стоп-флаг взведён).
const STOP_WAIT: Duration = Duration::from_secs(2);

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

/// Взвести стоп-флаг гостевой сессии (см. `STOP`).
fn signal_stop() {
    STOP.send_modify(|v| *v += 1);
}

fn sig_engine(coordinator: &str) -> bmv_core::BmvEngine {
    let mut g = SIG_ENGINE.lock();
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
///
/// # Safety
/// `p` — либо null, либо валидный указатель на нуль-терминированную строку,
/// живущую всё время вызова. Функция РАЗЫМЕНОВЫВАЕТ его — пометка `unsafe`
/// заставляет каждую точку входа явно подтвердить это обещание вызывающего.
unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("").to_string()
}

/// String → свежий `char*` для возврата в Swift (освобождать `bmv_free_string`).
///
/// Внутренние нули ВЫРЕЗАЕМ, а не превращаем вызов в null: имя хоста приходит с
/// чужой машины, и один подсунутый `\0` посреди строки раньше обнулял весь ответ —
/// каталог у человека становился пустым из-за чужой карточки.
fn to_c(s: String) -> *mut c_char {
    let clean = if s.as_bytes().contains(&0) { s.replace('\0', "") } else { s };
    CString::new(clean).map(|c| c.into_raw()).unwrap_or(std::ptr::null_mut())
}

/// Освободить строку, полученную из любой bmv_* функции.
///
/// # Safety
/// `p` — ровно тот указатель, что вернула bmv_*-функция (или null); освобождать
/// один раз. ABI не меняется — для Swift/Kotlin функция выглядит как раньше.
#[no_mangle]
pub unsafe extern "C" fn bmv_free_string(p: *mut c_char) {
    ffi!((), {
        if !p.is_null() {
            unsafe { drop(CString::from_raw(p)) };
        }
    })
}

// ── ПРАВИЛА ПОКАЗА: мост к справочнику `bmv_common::view` ─────────────────────
//
// Справочник — чистые функции без сети и без памяти, но телефоны позвать его не
// могли ВООБЩЕ: Swift и Kotlin в Rust не ходят иначе как через этот файл.
// Поэтому каждое правило там жило ещё и своей копией здесь, на двух чужих
// языках, и копии разошлись — молча, потому что грепалка
// `crates/bmv-common/tests/one_place_per_rule.rs` читает только `.rs`.
//
// Дороже всех обошёлся адрес координатора: на телефоне его разбирал сам шелл
// (`if !u.startsWith("http") return`), и «bemyvpn.net» без схемы просто НЕ
// СОХРАНЯЛСЯ — кнопка нажата, ничего не записано, ничего не сказано.
//
// УРОВНИ ОТДАЮТСЯ ЧИСЛОМ, А НЕ ЦВЕТОМ И НЕ ИМЕНЕМ ЗНАЧКА: наборы значков у
// оболочек разные и общего имени у них нет (см. шапку `view.rs`). Числа — это
// номера вариантов перечислений справочника, они там проставлены явно.
//
// Функции этого блока ЧИСТЫЕ: ни рантайма, ни сети, ни блокировок — их можно
// звать прямо с UI-потока, в отличие от всего остального в этом файле.

/// Трёхзначное «на связи» с границы C: 1 — да, 0 — нет, всё прочее (обычно -1) —
/// ещё не знаем. Третье значение обязано быть: без него «ещё не проверяли»
/// сливается с «связи нет», и свежезапущенное приложение обвиняет сеть.
fn tri(v: i32) -> Option<bool> {
    match v {
        1 => Some(true),
        0 => Some(false),
        _ => None,
    }
}

/// Пинг с границы C: отрицательное — ответа не было.
fn ping_ms(ms: i32) -> Option<u32> {
    (ms >= 0).then_some(ms as u32)
}

/// Набранный человеком адрес координатора → пригодный для работы.
/// ПУСТАЯ СТРОКА — ОТКАЗ, о котором оболочка обязана сказать вслух.
#[no_mangle]
pub extern "C" fn bmv_coordinator_url(input: *const c_char) -> *mut c_char {
    ffi!(std::ptr::null_mut(), {
        let s = unsafe { cstr(input) };
        to_c(bmv_common::view::coordinator_url(&s).unwrap_or_default())
    })
}

/// Адрес координатора ДЛЯ ПОКАЗА — без схемы и без хвостового слэша.
#[no_mangle]
pub extern "C" fn bmv_display_coordinator(url: *const c_char) -> *mut c_char {
    ffi!(std::ptr::null_mut(), {
        let s = unsafe { cstr(url) };
        to_c(bmv_common::view::without_scheme(&s))
    })
}

/// Уровень защиты по имени протокола: 0 шифр · 1 маскировка · 2 без шифра ·
/// 3 неизвестно (номера — варианты `view::Protection`).
#[no_mangle]
pub extern "C" fn bmv_protection(protocol: *const c_char) -> i32 {
    ffi!(bmv_common::view::Protection::Unknown as i32, {
        let p = unsafe { cstr(protocol) };
        bmv_common::view::protection(&p) as i32
    })
}

/// Часы сеанса: `MM:SS`, после часа `H:MM:SS`.
#[no_mangle]
pub extern "C" fn bmv_session_clock(seconds: u64) -> *mut c_char {
    ffi!(std::ptr::null_mut(), { to_c(bmv_common::view::session_clock(seconds)) })
}

/// Подпись пинга («24 мс» или прочерк). Отрицательное `ms` — ответа не было.
#[no_mangle]
pub extern "C" fn bmv_ping_text(ms: i32) -> *mut c_char {
    ffi!(std::ptr::null_mut(), { to_c(bmv_common::view::ping(ping_ms(ms)).0) })
}

/// Тревожность пинга: 0 спокойно · 1 янтарь · 2 красный · 3 приглушённо
/// (номера — варианты `view::Alarm`). Пороги живут в справочнике.
#[no_mangle]
pub extern "C" fn bmv_ping_alarm(ms: i32) -> i32 {
    ffi!(bmv_common::view::Alarm::Muted as i32, { bmv_common::view::ping(ping_ms(ms)).1 as i32 })
}

/// Годен ли хост для подключения (живой и есть место) — на этом гасится кнопка.
#[no_mangle]
pub extern "C" fn bmv_host_usable(online: bool, guests: u32, max_guests: u32) -> bool {
    ffi!(false, { bmv_common::view::host_usable(online, guests, max_guests) })
}

/// Подпись состояния связи с координатором. `online`: 1 да · 0 нет · -1 не знаем.
#[no_mangle]
pub extern "C" fn bmv_link_text(online: i32) -> *mut c_char {
    ffi!(std::ptr::null_mut(), { to_c(bmv_common::view::link_state(tri(online)).0.to_string()) })
}

/// Тревожность состояния связи (номера — варианты `view::Alarm`).
/// Обрыв — ЯНТАРЬ: сокет чинит супервизор сам, красным тут пугать нечем.
#[no_mangle]
pub extern "C" fn bmv_link_alarm(online: i32) -> i32 {
    ffi!(bmv_common::view::Alarm::Muted as i32, { bmv_common::view::link_state(tri(online)).1 as i32 })
}

/// Что написать на месте ПУСТОГО списка хостов: ждать связи или действовать.
#[no_mangle]
pub extern "C" fn bmv_empty_directory_hint(online: i32) -> *mut c_char {
    ffi!(std::ptr::null_mut(), { to_c(bmv_common::view::empty_directory_hint(tri(online)).to_string()) })
}

/// Состояние VPN числом: 0 выкл · 1 подключаюсь · 2 переподключение · 3 работает ·
/// 4 хост завершил раздачу · 5 связь потеряна (варианты `view::Vpn`).
///
/// На входе — ровно то, что уже отдают `bmv_vpn_status` и `bmv_stop_reason`, плюс
/// «сеанс уже был» (у оболочки есть отметка начала). Склеивать эти три числа
/// каждая оболочка раньше училась сама, и получалось по-разному: окно и терминал
/// не отличают авто-реконнект от первого подключения и пишут «Подключаюсь…»
/// человеку, у которого VPN и не выключался.
#[no_mangle]
pub extern "C" fn bmv_vpn_kind(status: i32, stop_reason: i32, was_connected: bool) -> i32 {
    ffi!(bmv_common::view::Vpn::Off as i32, {
        bmv_common::view::vpn_state(status, stop_reason, was_connected) as i32
    })
}

/// Подпись состояния VPN. Аргументы те же, что у `bmv_vpn_kind` — обратного
/// перевода числа в состояние нарочно нет: иначе номера пришлось бы разбирать
/// ВТОРОЙ раз, уже здесь.
#[no_mangle]
pub extern "C" fn bmv_vpn_text(status: i32, stop_reason: i32, was_connected: bool) -> *mut c_char {
    ffi!(std::ptr::null_mut(), {
        let v = bmv_common::view::vpn_state(status, stop_reason, was_connected);
        to_c(bmv_common::view::vpn_text(v).to_string())
    })
}

// ── сигналинг (каталог / коды / IP / связь) ──────────────────────────────────

/// Живой каталог: since из прошлого ответа. JSON `{"version":N,"hosts":[...]}`.
#[no_mangle]
pub extern "C" fn bmv_list_watch(coordinator: *const c_char, since: u64) -> *mut c_char {
    ffi!(std::ptr::null_mut(), {
        let engine = sig_engine(&unsafe { cstr(coordinator) });
        let r = RUNTIME.block_on(async { engine.guest_watch(None, since).await });
        let json = match r {
            Ok(u) => format!("{{\"version\":{},\"hosts\":{}}}", u.version, hosts_to_json(&u.hosts)),
            Err(e) => {
                log::error!("bmv_list_watch: {e}");
                "{\"version\":0,\"hosts\":[]}".to_string()
            }
        };
        to_c(json)
    })
}

/// Найти хост по КОДУ (в т.ч. скрытый). JSON-объект хоста или "" если не найден.
#[no_mangle]
pub extern "C" fn bmv_resolve(coordinator: *const c_char, code: *const c_char) -> *mut c_char {
    ffi!(std::ptr::null_mut(), {
        let engine = sig_engine(&unsafe { cstr(coordinator) });
        let code = unsafe { cstr(code) };
        let found = RUNTIME.block_on(async { engine.guest_resolve(&code).await });
        let json = match found {
            Ok(Some(h)) => {
                let arr = hosts_to_json(std::slice::from_ref(&h));
                arr.trim_start_matches('[').trim_end_matches(']').to_string()
            }
            _ => String::new(),
        };
        to_c(json)
    })
}

/// Новый код хоста ОТ СЕРВЕРА как "CODE|SIG" ("" при ошибке).
#[no_mangle]
pub extern "C" fn bmv_new_code(coordinator: *const c_char) -> *mut c_char {
    ffi!(std::ptr::null_mut(), {
        let engine = sig_engine(&unsafe { cstr(coordinator) });
        let (code, sig) = RUNTIME.block_on(async { engine.host_new_code().await }).unwrap_or_default();
        to_c(if code.is_empty() { String::new() } else { format!("{code}|{sig}") })
    })
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
    ffi!(-1, {
        let eps: Vec<String> = unsafe { cstr(endpoints) }
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if eps.is_empty() {
            return -1;
        }
        let engine = sig_engine(&unsafe { cstr(coordinator) });
        let id = unsafe { cstr(host_id) };
        RUNTIME
            .block_on(async { engine.probe_host_rtt(&id, &eps).await })
            .map(|ms| ms.min(i32::MAX as u32) as i32)
            .unwrap_or(-1)
    })
}

/// Свой внешний IP через координатор ("" при ошибке).
#[no_mangle]
pub extern "C" fn bmv_my_ip(coordinator: *const c_char) -> *mut c_char {
    ffi!(std::ptr::null_mut(), {
        let engine = sig_engine(&unsafe { cstr(coordinator) });
        let ip = RUNTIME
            .block_on(async { tokio::time::timeout(Duration::from_secs(6), engine.my_ip()).await })
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();
        to_c(ip)
    })
}

/// Быстрая проверка связи с координатором. true = сервер жив.
#[no_mangle]
pub extern "C" fn bmv_health(coordinator: *const c_char) -> bool {
    ffi!(false, {
        let engine = sig_engine(&unsafe { cstr(coordinator) });
        RUNTIME.block_on(async { engine.coordinator_health().await.is_ok() })
    })
}

/// Круг до координатора в мс; 0 — ещё не мерили или связи нет.
///
/// Шеллы раньше засекали, сколько длится сам `bmv_health`, и показывали это как
/// пинг. Но health читает флаг «сокет жив» и возвращается мгновенно — на экране
/// стояло время вызова функции, а не время до сервера. Здесь настоящий замер:
/// свой Ping с меткой времени и Pong с той же меткой (см. bmv_signal).
#[no_mangle]
pub extern "C" fn bmv_rtt_ms(coordinator: *const c_char) -> u32 {
    ffi!(0, { sig_engine(&unsafe { cstr(coordinator) }).coordinator_rtt().unwrap_or(0) })
}

// ── гость: подключение ───────────────────────────────────────────────────────

/// Сколько ждём ФАЗУ 2 после удачной фазы 1. Не дождались — канал и статус
/// «подключаюсь» снимаем сами (см. ниже).
const PHASE2_DEADLINE: Duration = Duration::from_secs(30);

/// ФАЗА 1: пробитие NAT + рукопожатие БЕЗ TUN. true — канал поднят (зови
/// bmv_start_tunnel с fd от Packet Tunnel). false — не удалось (маршрут цел).
#[no_mangle]
pub extern "C" fn bmv_connect(
    coordinator: *const c_char,
    host_id: *const c_char,
    password: *const c_char,
    protocol: *const c_char,
) -> bool {
    ffi!(false, {
        let coordinator = unsafe { cstr(coordinator) };
        let host_id = unsafe { cstr(host_id) };
        let password = unsafe { cstr(password) };
        let protocol = unsafe { cstr(protocol) };
        let gen0 = CONNECT_GEN.load(Ordering::SeqCst);
        // Подписка на стоп ДО начала работы: иначе стоп, пришедший пока мы
        // пробиваем NAT, пролетел бы мимо (см. `STOP`).
        let mut stop = STOP.subscribe();
        // Новая попытка — старая причина конца сеанса больше не про неё.
        STOP_REASON.store(0, Ordering::SeqCst);
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
                _ = stop.changed() => Err("отменено пользователем".to_string()),
            }
        });

        let cancelled = CONNECT_GEN.load(Ordering::SeqCst) != gen0;
        match result {
            Ok((peer, link)) if !cancelled => {
                log::info!("bmv_connect: канал поднят к {peer}");
                // Запоминаем параметры — по ним pump_tunnel сам переустановит канал
                // при обрыве пути (мобильный роуминг), не роняя utun.
                *RECONNECT.lock() = Some(reconnect_params);
                *PENDING_LINK.lock() = Some(link);
                let seq = PENDING_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
                // СТОРОЖ ФАЗЫ 2. Если шелл не позовёт bmv_start_tunnel (расширение
                // не поднялось, человек ушёл с экрана), статус «подключаюсь» висел
                // бы вечно, а пробитый канал — жил бы в PENDING_LINK, и хост держал
                // бы под нас слот. Снимаем и то и другое сами.
                RUNTIME.spawn(async move {
                    tokio::time::sleep(PHASE2_DEADLINE).await;
                    if CONNECT_GEN.load(Ordering::SeqCst) != gen0 || PENDING_SEQ.load(Ordering::SeqCst) != seq {
                        return; // уже остановились, или в слоте лежит ЧУЖОЙ канал
                    }
                    // Лок снимаем ДО await (иначе гард живёт через точку ожидания).
                    let stale = PENDING_LINK.lock().take();
                    if let Some(l) = stale {
                        log::warn!("фаза 2 не наступила за {PHASE2_DEADLINE:?} — гашу канал");
                        let _ = l.close().await;
                        let _ = VPN_STATUS.compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst);
                    }
                });
                true
            }
            _ => {
                set_status(0);
                false
            }
        }
    })
}

/// ФАЗА 2: качать пакеты через утун-fd на уже поднятом канале. fd даёт Packet
/// Tunnel (iOS) / VpnService (Android). `utun`=true — снимать/добавлять
/// 4-байтовый заголовок (iOS/macOS utun); false — сырой IP (Android). true —
/// качалка запущена.
#[no_mangle]
pub extern "C" fn bmv_start_tunnel(fd: i32, utun: bool) -> bool {
    ffi!(false, {
        // Повторный старт БЕЗ стопа: иначе прежний хэндл затирается, старая
        // качалка продолжает жить и писать в СВОЙ fd, закрыть который уже некому.
        stop_guest_session();
        let link = match PENDING_LINK.lock().take() {
            Some(l) => l,
            None => {
                log::error!("bmv_start_tunnel: нет готового канала");
                set_status(3);
                return false;
            }
        };
        let params = RECONNECT.lock().clone();
        // Подсказки координатора «проверь соседа» (второй слой обнаружения ухода:
        // хоста могли убить, и прощание по UDP послать было некому).
        let hints = params.as_ref().and_then(|p| sig_engine(&p.coordinator).peer_check());
        let gen = CONNECT_GEN.load(Ordering::SeqCst);
        TUNNEL_FD.store(fd, Ordering::SeqCst); // владелец fd — pump_tunnel, здесь только публикуем номер
        log::info!("bmv_start_tunnel: fd={fd}, utun={utun}, качаю туннель (с авто-реконнектом)");
        // Как переустанавливать канал — ОТДЕЛЬНЫМ параметром, а не «движок внутри
        // качалки»: только так петлю реконнекта можно прогнать в тесте без сети.
        let reestablish = params.map(|p| {
            move || {
                let p = p.clone();
                async move {
                    let engine = sig_engine(&p.coordinator);
                    match engine.guest_establish(&p.host_id, p.password.as_deref(), p.proto.as_deref()).await {
                        Ok((peer, l)) => {
                            log::info!("авто-реконнект: канал восстановлен к {peer}");
                            Some(l)
                        }
                        Err(e) => {
                            log::warn!("авто-реконнект: {e}");
                            None
                        }
                    }
                }
            }
        });
        let handle = RUNTIME.spawn(async move {
            set_status(2);
            pump_tunnel(fd, utun, link, reestablish, hints, gen).await;
            log::info!("туннель завершён");
        });
        *SESSION.lock() = Some(handle);
        true
    })
}

/// Текущий статус VPN (0/1/2/3). Неблокирующая.
///
/// Из 3 («не смогли») выход только явный: `bmv_stop` ставит 0, новый
/// `bmv_connect` ставит 1. Сама тройка не рассасывается НАРОЧНО — иначе экран
/// молча вернулся бы в «выключено», и человек не узнал бы, что VPN отвалился
/// (например, из-за неверного пароля).
#[no_mangle]
pub extern "C" fn bmv_vpn_status() -> i32 {
    ffi!(0, { VPN_STATUS.load(Ordering::SeqCst) })
}

/// Почему сеанс кончился САМ: 0 — не кончался (идёт или выключили сами),
/// 1 — ХОСТ ЗАВЕРШИЛ РАЗДАЧУ, 2 — связь с хостом потеряна. Неблокирующая.
///
/// Читать вместе с концом сеанса (статус 3 или переход в 0): единица означает,
/// что всё сработало правильно и ошибки НЕТ — хост просто выключил раздачу, и
/// человеку надо предложить другой хост, а не показывать отказ.
#[no_mangle]
pub extern "C" fn bmv_stop_reason() -> i32 {
    ffi!(0, { STOP_REASON.load(Ordering::SeqCst) })
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
    *RECONNECT.lock() = None;
    signal_stop();
    // Канал СНИМАЕМ В ЛОКАЛЬНУЮ и только потом прощаемся: если оставить `if let`
    // прямо на `lock()`, временный гард живёт до конца ветки — мьютекс заперт на
    // всё время блокирующего `close()`, а его в это же время берёт качалка из
    // потока рантайма (публикация/снятие ACTIVE_LINK). Залипал и интерфейс, и
    // рантайм разом.
    let taken = ACTIVE_LINK.lock().take();
    if let Some(l) = taken {
        RUNTIME.block_on(async {
            let _ = tokio::time::timeout(Duration::from_millis(700), l.close()).await;
        });
    }
}

/// Погасить гостевую качалку и ДОЖДАТЬСЯ её смерти.
///
/// Ждём НЕ из вежливости: единственный владелец tunnel-fd — сама качалка, и
/// закрывает его она. Прежний порядок был `abort()` → `close(fd)`: abort не
/// ждёт, задача в этот момент могла сидеть в `readv`/`writev` по этому же fd, а
/// освобождённый номер дескриптора система тут же выдаёт следующему сокету —
/// и пакеты человека уходили в чужое соединение. Теперь: сигнал → ожидание →
/// закрытие делает владелец. abort остаётся только аварийным добиванием.
fn stop_guest_session() {
    let session = SESSION.lock().take();
    let Some(mut h) = session else { return };
    signal_stop();
    let finished = RUNTIME.block_on(async { tokio::time::timeout(STOP_WAIT, &mut h).await.is_ok() });
    if !finished {
        log::error!("качалка не вышла за {STOP_WAIT:?} — добиваю (fd закрою сам)");
        h.abort();
    }
    // Штатно fd уже закрыт владельцем — здесь no-op (close-once). Не no-op только
    // после abort'а выше: тогда TUN обязан закрыться, иначе система держит VPN
    // активным и «интернета нет после отключения».
    close_tunnel_fd();
}

#[no_mangle]
pub extern "C" fn bmv_stop() {
    ffi!((), {
        mark_stop_and_farewell();
        stop_guest_session();
        close_tunnel_fd(); // на случай, если фаза 2 успела положить fd без качалки
        *PENDING_LINK.lock() = None;
        set_status(0);
    })
}

/// Попрощаться с хостом (BYE) ДО остановки туннеля — из app→extension сообщения,
/// ПОКА сокет жив. На stopTunnel iOS уже рвёт ресурсы туннеля, и отправка BYE по
/// UDP молча проваливается, поэтому основной BYE шлётся отсюда, заранее. После
/// этого pump_tunnel сам выйдет (стоп-флаги уже стоят), а bmv_stop добьёт сессию.
#[no_mangle]
pub extern "C" fn bmv_send_bye() {
    ffi!((), { mark_stop_and_farewell() })
}

/// Платформа сообщает о СМЕНЕ СЕТИ (iOS NWPathMonitor: WiFi↔сотовая, смена вышки).
/// Форсирует немедленный реконнект гостевого туннеля, не дожидаясь keepalive —
/// utun при этом НЕ роняется, приложения разрыва не видят. Неблокирующая, безопасна
/// вне активной сессии (просто no-op, если качать нечего).
#[no_mangle]
pub extern "C" fn bmv_nudge_reconnect() {
    ffi!((), { NUDGE.notify_waiters() })
}

// ── хост-режим ───────────────────────────────────────────────────────────────

/// Отказ координатора «подпись кода не наша» (403) — единственный, который
/// лечится сам: берём свежий код и повторяем анонс.
///
/// Отдельной функцией, потому что решение принимается ПО КОДУ, а не по буквам
/// в тексте. Тексты отказов координатора — русские фразы без единой цифры, так
/// что прежнее `e.to_string().contains("403")` было ложью ВСЕГДА: самолечение
/// на iOS и Android не включалось ни разу.
fn stale_code_signature(e: &bmv_common::Error) -> bool {
    e.refusal_code() == Some(403)
}

/// Сентинел для оболочки по КОДУ отказа: "!NAT" — снаружи до хоста не
/// достучаться (422), "!SIG" — код не наш и свежий взять не вышло (403).
/// Пусто — ошибка не от сервера (нет связи, кривой адрес и т.п.).
fn host_start_sentinel(e: &bmv_common::Error) -> &'static str {
    match e.refusal_code() {
        Some(422) => "!NAT",
        Some(403) => "!SIG",
        _ => "",
    }
}

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
    ffi!(std::ptr::null_mut(), {
        // Повторный старт без стопа терял прежний хэндл — раздача продолжала
        // жить в фоне вместе со всеми сессиями гостей. Гасим её честно.
        // Лок снимаем ДО ветки: bmv_host_stop берёт этот же мьютекс, а он не
        // реентрантный — держать гард через вызов нельзя.
        let already_hosting = HOST_SESSION.lock().is_some();
        if already_hosting {
            log::warn!("bmv_host_start поверх работающей раздачи — гашу прежнюю");
            bmv_host_stop();
        }
        let mut cfg = bmv_config::Config { coordinators: vec![unsafe { cstr(coordinator) }], ..Default::default() };
        let stable_id = unsafe { cstr(host_id) };
        if !stable_id.is_empty() {
            cfg.host.id = stable_id;
        }
        cfg.host.token = unsafe { cstr(token) };
        cfg.host.code_sig = unsafe { cstr(code_sig) };
        cfg.host.name = unsafe { cstr(name) };
        cfg.host.password = unsafe { cstr(password) };
        cfg.host.public = public;
        cfg.host.max_guests = if max_guests > 0 { max_guests as u32 } else { 8 };
        let proto = unsafe { cstr(protocol) };
        if !proto.is_empty() {
            cfg.default_protocol = proto;
        }
        let mut engine = bmv_core::BmvEngine::from_config(cfg.clone());
        let mut host_id = engine.host_id().to_string();
        // Подпись возвращаем ВМЕСТЕ с кодом: при самолечении меняются обе, и если
        // отдать только код, приложение сохранит новый код со старой подписью —
        // при следующем запуске координатор снова ответит 403.
        let mut host_sig = cfg.host.code_sig.clone();
        *HOST_ENGINE.lock() = Some(engine.clone());

        let mut announce = RUNTIME.block_on(engine.host_bind_announce());

        // ПРОТУХШАЯ ПОДПИСЬ ЛЕЧИТСЯ САМА. Координатор отвечает 403, если подпись кода
        // не сходится — так бывает после смены координатора или его секрета. Раньше
        // мобилки на это отдавали «!SIG», а человек читал «Код устарел, обновите и
        // повторите» и должен был сам нажать «Новый код»: то есть раздача не
        // включалась с первого раза без всякой его вины. Десктоп уже давно берёт
        // свежий код молча и повторяет анонс — делаем то же здесь, и правило
        // становится общим для Android и iOS сразу.
        if announce.as_ref().err().is_some_and(stale_code_signature) {
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
                    *HOST_ENGINE.lock() = Some(engine.clone());
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
                log::error!("хост-режим не поднялся: {e}");
                return to_c(host_start_sentinel(&e).to_string());
            }
        };

        let mut stop = HOST_STOP.subscribe();
        let handle = RUNTIME.spawn(async move {
            // ВСЯ фоновая работа хоста — в ОДНОМ наборе: `JoinSet` абортит свои
            // задачи на собственном drop'е. Значит гашение внешней задачи гасит и
            // heartbeat, и puncher, и КАЖДУЮ сессию гостя. Раньше сессии были
            // самостоятельными spawn'ами: после «остановить» гости продолжали
            // ходить через нас, а если внешняя задача не успевала дойти до
            // `heartbeat.abort()`, эти две задачи оставались жить НАВСЕГДА —
            // по паре на каждый цикл старт/стоп.
            let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
            let beat = engine.clone();
            let beat_hub = hub.clone();
            tasks.spawn(async move {
                loop {
                    // Тик ОДИН на все три дерева задач хоста — раньше это число
                    // было вписано в каждое отдельно.
                    tokio::time::sleep(bmv_core::HOST_HEARTBEAT).await;
                    let _ = beat.host_heartbeat(&beat_hub).await;
                }
            });
            let punch_engine = engine.clone();
            let punch_hub = hub.clone();
            tasks.spawn(async move {
                let _ = punch_engine.host_serve_punch(punch_hub).await;
            });
            loop {
                tokio::select! {
                    guest = hub.accept() => match guest {
                        Some((peer, raw)) => {
                            log::info!("хост-режим: гость {peer}");
                            let e = engine.clone();
                            tasks.spawn(async move {
                                match e.host_run_session(peer, raw, true).await {
                                    Ok(()) => log::info!("гость {peer} отключился"),
                                    Err(err) => log::warn!("гость {peer}: {err}"),
                                }
                            });
                        }
                        None => break, // hub закрылся
                    },
                    // Жнём завершившиеся сессии, иначе их хэндлы копятся в наборе
                    // всё время раздачи (по одному на каждого ушедшего гостя).
                    Some(_) = tasks.join_next() => {}
                    _ = stop.changed() => {
                        log::info!("хост-режим: стоп");
                        break;
                    }
                }
            }
            // Выход = drop набора = аборт heartbeat, puncher и всех живых сессий.
        });
        *HOST_SESSION.lock() = Some(handle);
        // «код|подпись» — сентинелы (!NAT/!SIG/пусто) чёрточки не содержат.
        to_c(format!("{host_id}|{host_sig}"))
    })
}

/// Перестать быть хостом (снимает запись из каталога, глушит фоновые задачи).
#[no_mangle]
pub extern "C" fn bmv_host_stop() {
    ffi!((), {
        // Движок СНАЧАЛА в локальную: `if let` прямо на `lock()` держал бы мьютекс
        // всё время блокирующего deannounce (сетевой запрос!), а его берут и
        // bmv_host_update, и повторный старт.
        let engine = HOST_ENGINE.lock().take();
        if let Some(engine) = engine {
            let _ = RUNTIME.block_on(async { engine.host_deannounce().await });
        }
        HOST_STOP.send_modify(|v| *v += 1);
        let session = HOST_SESSION.lock().take();
        if let Some(mut h) = session {
            // Ждём мягкого выхода: тогда JoinSet дропается штатно. Не дождались —
            // abort, и набор всё равно дропается вместе с задачей (сессии гостей
            // гасятся в обоих случаях).
            let done = RUNTIME.block_on(async { tokio::time::timeout(STOP_WAIT, &mut h).await.is_ok() });
            if !done {
                h.abort();
            }
        }
    })
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
    ffi!((), {
        // Клон движка в локальную — лок снят до spawn'а (и до любого ожидания).
        let engine = HOST_ENGINE.lock().clone();
        let Some(engine) = engine else { return };
        let name = unsafe { cstr(name) };
        let pw = unsafe { cstr(password) };
        let proto = unsafe { cstr(protocol) };
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
    })
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

/// Столько неудачных сессий ПОДРЯД → считаем хост недостижимым, выходим.
/// ~20 попыток с бэкоффом до 8с ≈ 2 минуты — переживает мёртвые зоны (тоннель,
/// метро, лифт), но не крутит вечно, если хост реально пропал.
const MAX_FAILS: u32 = 20;
/// Сессия короче этого — НЕУДАЧА, а не успех.
///
/// Раньше неудачей считался только провал `guest_establish`. Но при НЕВЕРНОМ
/// ПАРОЛЕ (и при «хост полон») establish возвращает Ok: гость шлёт auth-кадр и
/// не ждёт подтверждения, а хост отвергает его позже, закрытием канала. Гость
/// видел мгновенный EOF, обнулял счётчик неудач — и круг «пауза → STUN → пробитие
/// → рукопожатие → EOF» повторялся ВЕЧНО: батарея садилась, в интерфейсе стояло
/// честное «Подключено», а человек с опечаткой в пароле никогда не узнавал, что
/// пароль неверный. Десять секунд — заведомо больше любого рукопожатия и заведомо
/// меньше осмысленной сессии.
const MIN_GOOD_SESSION: Duration = Duration::from_secs(10);

/// Качать туннель с АВТО-РЕКОННЕКТОМ. utun-fd открыт ОДИН раз и живёт весь сеанс;
/// при обрыве канала (смена вышки на мобильном, протухший NAT, пропал хост) ядро
/// само переустанавливает Link по сохранённым параметрам, НЕ трогая utun —
/// приложения не видят разрыва VPN, трафик возобновляется. Сдаёмся только после
/// серии неудач подряд (хост реально мёртв) или по стопу пользователя.
///
/// `reestablish` — как поднять канал заново; `None` = реконнекта нет (одна сессия).
async fn pump_tunnel<F, Fut>(
    fd: RawFd,
    utun: bool,
    first_link: Box<dyn bmv_common::Link>,
    reestablish: Option<F>,
    hints: Option<tokio::sync::watch::Receiver<u64>>,
    gen: u64,
) where
    F: Fn() -> Fut + Send,
    Fut: std::future::Future<Output = Option<Box<dyn bmv_common::Link>>> + Send,
{
    let mut stop = STOP.subscribe();
    let mut link: Option<Box<dyn bmv_common::Link>> = Some(first_link);
    let mut fails: u32 = 0;
    let mut gave_up = false;
    // Сеанс кончился прощанием хоста, а не обрывом? (см. STOP_REASON)
    let mut host_left = false;

    'session: loop {
        if CONNECT_GEN.load(Ordering::SeqCst) != gen {
            break; // пользователь остановил / началось новое подключение
        }
        // Взять текущий канал или ПЕРЕУСТАНОВИТЬ (авто-реконнект).
        let l = match link.take() {
            Some(l) => l,
            None => {
                let Some(mk) = reestablish.as_ref() else { break };
                set_status(1); // reasserting: VPN «жив», но временно переподключается
                // Бэкофф растёт с числом неудач (0.5с…8с), прерывается стопом ИЛИ
                // сменой сети (nudge) — тогда пробуем сразу, не досыпая.
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500u64 << fails.min(4))) => {}
                    _ = NUDGE.notified() => {}
                    _ = stop.changed() => break,
                }
                if CONNECT_GEN.load(Ordering::SeqCst) != gen {
                    break;
                }
                let got = tokio::select! {
                    r = tokio::time::timeout(Duration::from_secs(15), mk()) => r.ok().flatten(),
                    _ = stop.changed() => break,
                };
                match got {
                    Some(l) => {
                        set_status(2);
                        l
                    }
                    None => {
                        fails += 1;
                        log::warn!("авто-реконнект: неудача {fails}/{MAX_FAILS}");
                        if fails >= MAX_FAILS {
                            gave_up = true;
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
                gave_up = true;
                break;
            }
        };
        let l_arc: std::sync::Arc<dyn bmv_common::Link> = std::sync::Arc::from(l);
        // Публикуем живой канал: bmv_stop пошлёт по нему BYE ПРЯМО и синхронно.
        *ACTIVE_LINK.lock() = Some(l_arc.clone());
        // Часы сессии ОБЫЧНЫЕ (std), а не тиковые: важно реальное время жизни.
        let started = Instant::now();
        let died = tokio::select! {
            _ = bmv_tunnel::run_guest(device, l_arc.clone()) => {
                // ХОСТ ПОПРОЩАЛСЯ — это не обрыв пути, а конец сеанса: раздачу
                // погасили. Переустанавливать канал НЕКУДА, и попытки (20 штук с
                // бэкоффом) держали бы на экране «подключаюсь» две с половиной
                // минуты вместо честного «отключено» через секунду.
                if l_arc.peer_said_bye() {
                    log::info!("хост попрощался (BYE) — сеанс окончен");
                    *ACTIVE_LINK.lock() = None;
                    // Причина — наверх, оболочке: это НЕ ошибка, а штатный конец
                    // раздачи (см. STOP_REASON и bmv_stop_reason).
                    host_left = true;
                    gave_up = true;
                    break 'session;
                }
                // Канал умер (обрыв пути / хост пропал / нас отвергли) → следующая
                // итерация переустановит его (link уже None). utun не трогаем.
                log::info!("канал оборвался — пробую восстановить…");
                true
            }
            _ = NUDGE.notified() => {
                // Платформа сообщила о смене сети (новая вышка/интерфейс). Старый
                // NAT-мэппинг мёртв — не ждём keepalive-таймаут, реконнектим сразу.
                // Это НЕ неудача: сессию оборвали мы сами.
                log::info!("сеть сменилась — форсирую реконнект");
                false
            }
            _ = stop.changed() => {
                *ACTIVE_LINK.lock() = None;
                break 'session; // BYE уже отправил bmv_stop синхронно
            }
            // Подсказки координатора «проверь соседа» — крутятся рядом с сессией
            // и сами не завершаются; ветка нужна только чтобы дать им работать.
            _ = bmv_core::BmvEngine::relay_peer_checks(&*l_arc, hints.clone()) => unreachable!(),
        };
        // Канал больше не актуален (умер/реконнект) — снимаем публикацию.
        *ACTIVE_LINK.lock() = None;
        if died {
            if started.elapsed() >= MIN_GOOD_SESSION {
                fails = 0; // сессия пожила по-настоящему — прошлые неудачи не в счёт
            } else {
                fails += 1;
                log::warn!("сессия умерла за {:?} — неудача {fails}/{MAX_FAILS}", started.elapsed());
                if fails >= MAX_FAILS {
                    gave_up = true;
                    break;
                }
            }
        }
    }

    // utun-fd закрывает ЕДИНСТВЕННЫЙ владелец — эта задача, и только здесь, когда
    // ни одна операция по нему больше не идёт. close-once: если bmv_stop уже
    // добил нас abort'ом и закрыл сам — тут no-op.
    close_tunnel_fd();
    if gave_up {
        // Честная ошибка вместо тихого «выключено» (см. bmv_vpn_status): сеанс
        // кончился не по воле человека — хост попрощался либо перестал отвечать.
        // ПРИЧИНУ ставим ДО статуса: шелл, увидевший 3, обязан застать причину
        // уже на месте, иначе прочитает ноль и покажет отказ вместо «раздача
        // завершена».
        STOP_REASON.store(if host_left { 1 } else { 2 }, Ordering::SeqCst);
        log::error!("гостевой туннель завершён извне (неудач подряд: {fails})");
        set_status(3);
    } else {
        let _ = VPN_STATUS.compare_exchange(2, 0, Ordering::SeqCst, Ordering::SeqCst);
        let _ = VPN_STATUS.compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
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
                    match n {
                        n if n < 0 => Err(std::io::Error::last_os_error()),
                        0 => Ok(0), // настоящий EOF: устройство закрыто
                        // 0 < n < 4 — обрубок без пакета. Возвращать 0 нельзя:
                        // для AsyncRead ноль означает EOF, и туннель молча вставал
                        // бы на первом же таком кадре. Это ошибка, а не конец.
                        n if (n as usize) < 4 => Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "utun: кадр короче 4-байтового заголовка",
                        )),
                        n => Ok(n as usize - 4),
                    }
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
                    // Сколько байт ПАКЕТА ушло — результат writev, а не длина
                    // данных: раньше мы отвечали «записал всё» даже когда ядро
                    // взяло меньше, и хвост пакета терялся молча.
                    match n {
                        n if n < 0 => Err(std::io::Error::last_os_error()),
                        n if (n as usize) <= 4 => {
                            Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "utun: ушёл только заголовок"))
                        }
                        n => Ok(n as usize - 4),
                    }
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
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// СТОРОЖ ФОРМЫ МОСТА: у тридцати точек входа она обязана быть ОДНА.
    ///
    /// Оба обещания ломаются МОЛЧА — ни компилятор, ни CI не краснеют:
    ///
    /// 1. ПОМЕТКА `unsafe`. Android-мост (apps/android/rust) лежит ВНЕ рабочего
    ///    пространства и зовёт эти функции как обычные Rust-функции. Пометь одну
    ///    из них — и перестанет собираться APK, а здесь всё зелено. Исключение
    ///    ровно одно: `bmv_free_string` помечена с самого начала, и мост уже
    ///    зовёт её из `unsafe`-блока.
    /// 2. ОБЁРТКА `ffi!`. Паника через `extern "C"` — это abort всего приложения
    ///    (на iOS ещё и «краш» в глазах системы). Шапка макроса обещает, что
    ///    обёртка одинакова во всех точках входа, «иначе новую функцию забудут
    ///    прикрыть», — и её действительно забыли в четырёх: `bmv_free_string`,
    ///    `bmv_vpn_status`, `bmv_stop_reason`, `bmv_nudge_reconnect`.
    ///
    /// Читаем СВОЙ ЖЕ исходник: путь от рабочего каталога не зависит.
    #[test]
    fn every_bridge_entry_keeps_the_shape_of_all_the_others() {
        const SRC: &str = include_str!("lib.rs");
        // Единственная точка входа, которой пометка разрешена (см. пункт 1).
        const UNSAFE_BY_DESIGN: [&str; 1] = ["bmv_free_string"];
        // Кавычки экранированы, поэтому эта строка НЕ находит саму себя.
        let marker = "extern \"C\" fn bmv_";

        let lines: Vec<&str> = SRC.lines().collect();
        let mut seen = 0;
        for (i, line) in lines.iter().enumerate() {
            let Some((head, tail)) = line.split_once(marker) else { continue };
            let name = format!(
                "bmv_{}",
                tail.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next().unwrap_or_default()
            );
            // Тело — до первой строки, закрывающей функцию на нулевом отступе.
            let end = lines[i..].iter().position(|l| l.trim_end() == "}").map_or(lines.len() - 1, |k| i + k);
            let body = lines[i..=end].join("\n");
            seen += 1;

            assert_eq!(
                head.contains("unsafe"),
                UNSAFE_BY_DESIGN.contains(&name.as_str()),
                "{name}: пометка unsafe разрешена ТОЛЬКО у {UNSAFE_BY_DESIGN:?}. Андроидный мост вне рабочего \
                 пространства зовёт остальные как обычные Rust-функции — пометка молча ломает сборку APK",
            );
            assert!(
                body.contains("ffi!("),
                "{name}: тело не завёрнуто в ffi! — паника внутри станет abort'ом всего приложения",
            );
        }
        assert!(seen >= 20, "разбор точек входа сломался: найдено {seen}, а их три десятка");
    }

    /// САМОЛЕЧЕНИЕ ПРОТУХШЕЙ ПОДПИСИ ОБЯЗАНО ВКЛЮЧАТЬСЯ. Смотрим со стороны
    /// ПОТРЕБИТЕЛЯ: даём ровно тот отказ, что приезжает с координатора, и
    /// требуем, чтобы ветка сработала.
    ///
    /// Здесь стоял `e.to_string().contains("403")`, а `Display` у `Refused`
    /// показывает ТОЛЬКО человеческую причину — русскую фразу без единой цифры.
    /// Условие было ложным всегда: на iOS и Android человек читал «Код устарел»
    /// и жал «Новый код» руками, хотя весь код самолечения написан.
    #[test]
    fn stale_signature_heals_itself_and_maps_to_sentinels() {
        let refused = |code, reason: &str| bmv_common::Error::Refused { code, reason: reason.into() };

        let sig = refused(403, "Код сети не подтверждён этим сервером.");
        assert!(!sig.to_string().contains("403"), "текст отказа с цифрами — тест перестал ловить старый приём");
        assert!(stale_code_signature(&sig), "403 не опознан: самолечение протухшей подписи снова мертво");
        assert_eq!(host_start_sentinel(&sig), "!SIG");

        let nat = refused(422, "Не удалось определить ваш адрес в интернете.");
        assert!(!nat.to_string().contains("422"), "текст отказа с цифрами — тест перестал ловить старый приём");
        assert!(!stale_code_signature(&nat), "422 — это не протухшая подпись, свежий код тут не поможет");
        assert_eq!(host_start_sentinel(&nat), "!NAT");

        // Не отказ сервера (нет связи) — ни лечения, ни сентинела.
        let net = bmv_common::Error::Net("Сервер недоступен.".into());
        assert!(!stale_code_signature(&net));
        assert_eq!(host_start_sentinel(&net), "");
    }

    /// Тесты трогают ОДНИ И ТЕ ЖЕ глобальные статики (STOP/SESSION/ACTIVE_LINK/
    /// TUNNEL_FD), а cargo гоняет их параллельно в одном процессе. Без этой
    /// очереди стоп из одного теста рвал сессию другого.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Пара соединённых дескрипторов с границами датаграмм — заменяет utun.
    fn socketpair() -> (RawFd, RawFd) {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) }, 0, "socketpair()");
        (fds[0], fds[1])
    }

    /// Канал, который умирает сразу: ровно так ведёт себя хост, отвергший гостя
    /// (неверный пароль, «мест нет») — рукопожатие прошло, а сессии нет.
    struct DeadLink;
    #[async_trait::async_trait]
    impl bmv_common::Link for DeadLink {
        async fn send(&self, _p: &[u8]) -> bmv_common::Result<()> {
            Ok(())
        }
        async fn recv_into(&self, _b: &mut Vec<u8>) -> bmv_common::Result<bool> {
            Ok(false) // EOF немедленно
        }
    }

    /// Канал, пир которого ПОПРОЩАЛСЯ: EOF плюс явная причина «это был BYE».
    struct ByeLink;
    #[async_trait::async_trait]
    impl bmv_common::Link for ByeLink {
        async fn send(&self, _p: &[u8]) -> bmv_common::Result<()> {
            Ok(())
        }
        async fn recv_into(&self, _b: &mut Vec<u8>) -> bmv_common::Result<bool> {
            Ok(false)
        }
        fn peer_said_bye(&self) -> bool {
            true
        }
    }

    /// Канал, чей `close()` висит: имитирует отправку BYE в живую сеть.
    struct SlowClose;
    #[async_trait::async_trait]
    impl bmv_common::Link for SlowClose {
        async fn send(&self, _p: &[u8]) -> bmv_common::Result<()> {
            Ok(())
        }
        async fn recv_into(&self, _b: &mut Vec<u8>) -> bmv_common::Result<bool> {
            std::future::pending().await
        }
        async fn close(&self) -> bmv_common::Result<()> {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(())
        }
    }

    /// Доказательство фикса «VPN висит после отключения»: tunnel-fd ДОЛЖЕН
    /// закрыться на стопе (иначе система держит VPN активным, интернет пропадает).
    /// Кладём реальный fd (из pipe) в TUNNEL_FD, зовём close_tunnel_fd — fd закрыт
    /// (fcntl → EBADF), TUNNEL_FD снова -1, повторный вызов безопасен (close-once).
    #[test]
    fn tunnel_fd_closed_once_on_stop() {
        let _serial = SERIAL.lock();
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
        let _serial = SERIAL.lock();
        let (a, b) = bmv_common::wire::memory_pair(64);
        // KeepaliveLink спавнит пингер — создаём в контексте рантайма, затем выходим.
        let ka: Box<dyn bmv_common::Link> = {
            let _g = RUNTIME.enter();
            Box::new(bmv_common::KeepaliveLink::new(a))
        };
        *ACTIVE_LINK.lock() = Some(std::sync::Arc::from(ka));
        *SESSION.lock() = None;

        bmv_stop(); // должен СИНХРОННО отправить BYE и только потом вернуться

        // BYE уже должен быть на проводе (bmv_stop вернулся). Пингер шлёт первый
        // KEEPALIVE не раньше 1.5с, так что первым придёт именно BYE=[1].
        let got = RUNTIME
            .block_on(async { tokio::time::timeout(Duration::from_millis(500), b.recv()).await })
            .expect("BYE не пришёл за 500мс — bmv_stop вернулся, НЕ отправив BYE")
            .unwrap();
        assert_eq!(got, vec![1u8], "ожидали BYE-маркер [1] сразу после bmv_stop");
        // Прибираем глобальное состояние за собой.
        *ACTIVE_LINK.lock() = None;
    }

    /// ВЕЧНЫЙ ЦИКЛ ПРИ НЕВЕРНОМ ПАРОЛЕ. Подставной establish всегда «успешен», но
    /// канал умирает мгновенно — так выглядит отказ хоста. Петля ОБЯЗАНА исчерпать
    /// MAX_FAILS и выйти с ошибкой; до фикса счётчик обнулялся на каждом успешном
    /// establish и круг повторялся вечно (тест не дожил бы до конца).
    ///
    /// Время рантайма приостановлено — бэкоффы пролетают мгновенно, но каждый круг
    /// двигает часы минимум на 0.5с, поэтому внешний таймаут срабатывает даже на
    /// сломанном коде (тест падает, а не висит).
    // Очередь тестов держится через await намеренно: гонка за глобальными
    // статиками страшнее, чем удержание мьютекса в тесте.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(start_paused = true)]
    async fn guest_gives_up_when_every_session_dies_instantly() {
        let _serial = SERIAL.lock();
        let (a, b) = socketpair();
        let tries = Arc::new(AtomicU64::new(0));
        let counter = tries.clone();
        let reestablish = Some(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Some(Box::new(DeadLink) as Box<dyn bmv_common::Link>)
            }
        });
        let gen = CONNECT_GEN.load(Ordering::SeqCst);
        TUNNEL_FD.store(a, Ordering::SeqCst);

        let done = tokio::time::timeout(
            Duration::from_secs(600),
            pump_tunnel(a, false, Box::new(DeadLink), reestablish, None, gen),
        )
        .await;

        assert!(done.is_ok(), "петля реконнекта не выдохлась — это и есть вечный круг при неверном пароле");
        assert_eq!(VPN_STATUS.load(Ordering::SeqCst), 3, "сдавшийся туннель обязан оставить статус ошибки");
        let n = tries.load(Ordering::SeqCst);
        assert!(n < u64::from(MAX_FAILS) + 2, "попыток {n}, а лимит {MAX_FAILS}");
        assert_eq!(TUNNEL_FD.load(Ordering::SeqCst), -1, "качалка обязана закрыть fd за собой");
        // Хост НЕ прощался — значит это потеря связи, а не завершённая раздача.
        // Перепутать нельзя: «выберите другой хост» вместо «связь потеряна»
        // отправило бы человека искать замену живому хосту.
        assert_eq!(bmv_stop_reason(), 2, "выдохшийся реконнект — это потеря связи, а не прощание хоста");
        set_status(0);
        STOP_REASON.store(0, Ordering::SeqCst);
        unsafe { libc::close(b) };
    }

    /// ПОПРОЩАВШИЙСЯ ХОСТ — ЭТО КОНЕЦ СЕАНСА, А НЕ ОБРЫВ ПУТИ.
    ///
    /// Обрыв пути на мобильном — обычное дело, поэтому качалка переустанавливает
    /// канал: до 20 попыток с бэкоффом, две с половиной минуты. Но если хост
    /// ПОПРОЩАЛСЯ, переустанавливать некуда — раздачу погасили. Пока разницу не
    /// различали, человек все эти минуты смотрел на «подключаюсь» вместо честного
    /// «отключено» через секунду: прощание доезжало, а толку от него не было.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(start_paused = true)]
    async fn a_host_goodbye_ends_the_session_without_reconnect_attempts() {
        let _serial = SERIAL.lock();
        let (a, b) = socketpair();
        let tries = Arc::new(AtomicU64::new(0));
        let counter = tries.clone();
        let reestablish = Some(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Some(Box::new(DeadLink) as Box<dyn bmv_common::Link>)
            }
        });
        let gen = CONNECT_GEN.load(Ordering::SeqCst);
        TUNNEL_FD.store(a, Ordering::SeqCst);

        let done = tokio::time::timeout(
            Duration::from_secs(30),
            pump_tunnel(a, false, Box::new(ByeLink), reestablish, None, gen),
        )
        .await;

        assert!(
            done.is_ok(),
            "качалка не вышла на прощание хоста, а ушла крутить реконнект — это и есть минуты «подключаюсь» вместо «отключено»",
        );
        assert_eq!(
            tries.load(Ordering::SeqCst),
            0,
            "после прощания хоста качалка полезла переподключаться — это и есть две минуты «подключаюсь» вместо «отключено»",
        );
        assert_eq!(VPN_STATUS.load(Ordering::SeqCst), 3, "конец сеанса обязан быть виден, а не тихо «выключено»");
        // …и ПОЧЕМУ он кончился. Без этого оболочка не отличит штатно погашенную
        // раздачу от обрыва и скажет «ошибка» там, где всё сработало правильно.
        assert_eq!(
            bmv_stop_reason(),
            1,
            "прощание хоста обязано доехать наверх как «раздача завершена», а не как отказ",
        );
        set_status(0);
        STOP_REASON.store(0, Ordering::SeqCst);
        unsafe { libc::close(b) };
    }

    /// ЗАХВАТ БЛОКИРОВКИ ЧЕРЕЗ БЛОКИРУЮЩИЙ ВЫЗОВ. Пока прощание висит в close(),
    /// ACTIVE_LINK обязан быть СВОБОДЕН — иначе качалка, которая берёт этот же
    /// мьютекс из потока рантайма, залипает вместе с интерфейсом.
    #[test]
    fn farewell_does_not_hold_active_link_lock() {
        let _serial = SERIAL.lock();
        let _g = RUNTIME.enter();
        *ACTIVE_LINK.lock() = Some(Arc::new(SlowClose));
        let t = std::thread::spawn(mark_stop_and_farewell);
        std::thread::sleep(Duration::from_millis(80)); // прощание уже внутри close()
        assert!(ACTIVE_LINK.try_lock().is_some(), "мьютекс заперт на всё время блокирующего close()");
        t.join().unwrap();
    }

    /// ЗАКРЫТИЕ ДЕСКРИПТОРА НА НЕОСТАНОВЛЕННОЙ ЗАДАЧЕ. bmv_stop обязан ДОЖДАТЬСЯ
    /// качалки и дать ей закрыть fd самой: иначе освободившийся номер достаётся
    /// другому сокету, пока старая задача ещё пишет в него.
    #[test]
    fn stop_waits_for_pump_before_fd_is_closed() {
        let _serial = SERIAL.lock();
        let (a, b) = socketpair();
        TUNNEL_FD.store(b, Ordering::SeqCst);
        let closed_by_pump = Arc::new(AtomicBool::new(false));
        let flag = closed_by_pump.clone();
        // Подписываемся ДО спавна — иначе стоп мог бы прилететь раньше подписки.
        let mut stop = STOP.subscribe();
        let h = RUNTIME.spawn(async move {
            let _ = stop.changed().await;
            // «Дописываем пакет» — окно, в котором fd ещё в работе.
            tokio::time::sleep(Duration::from_millis(150)).await;
            close_tunnel_fd();
            flag.store(true, Ordering::SeqCst);
        });
        *SESSION.lock() = Some(h);

        bmv_stop();

        assert!(closed_by_pump.load(Ordering::SeqCst), "bmv_stop обязан дождаться качалки, а не рубить её abort'ом");
        assert_eq!(unsafe { libc::fcntl(b, libc::F_GETFD) }, -1, "fd закрыт владельцем");
        assert_eq!(TUNNEL_FD.load(Ordering::SeqCst), -1);
        unsafe { libc::close(a) };
    }

    /// Кадрирование utun через socketpair: на записи добавляется 4-байтовая шапка
    /// с семейством адресов, на чтении — снимается. Ошибка здесь = битые пакеты на
    /// iOS/macOS при полностью «зелёном» остальном коде.
    #[tokio::test]
    async fn utun_framing_roundtrip() {
        let (a, b) = socketpair();
        let mut dev = TunFd::new(a, true).unwrap();

        // Запись: IPv4-пакет уходит с шапкой AF_INET (2).
        let mut pkt = vec![0u8; 20];
        pkt[0] = 0x45;
        pkt[19] = 7;
        dev.write_all(&pkt).await.unwrap();
        let mut raw = [0u8; 64];
        let n = unsafe { libc::read(b, raw.as_mut_ptr() as *mut libc::c_void, raw.len()) };
        assert_eq!(n as usize, pkt.len() + 4, "шапка не добавлена");
        assert_eq!(&raw[..4], &[0, 0, 0, 2], "AF_INET в шапке");
        assert_eq!(&raw[4..n as usize], &pkt[..]);

        // IPv6 отмечается своим семейством (30) — иначе ядро отвергнет пакет.
        let mut v6 = vec![0u8; 40];
        v6[0] = 0x60;
        dev.write_all(&v6).await.unwrap();
        let n = unsafe { libc::read(b, raw.as_mut_ptr() as *mut libc::c_void, raw.len()) };
        assert!(n >= 4);
        assert_eq!(&raw[..4], &[0, 0, 0, 30], "AF_INET6 в шапке");

        // Чтение: шапка снимается, наверх идёт чистый IP-пакет.
        let mut framed = vec![0u8, 0, 0, 2];
        framed.extend_from_slice(&pkt);
        assert!(unsafe { libc::write(b, framed.as_ptr() as *const libc::c_void, framed.len()) } > 0);
        let mut got = vec![0u8; 128];
        let n = dev.read(&mut got).await.unwrap();
        assert_eq!(&got[..n], &pkt[..], "шапка не снята при чтении");

        // Обрубок короче шапки — ОШИБКА, а не «ноль байт»: ноль для AsyncRead
        // означает конец потока, и туннель молча вставал бы навсегда.
        assert!(unsafe { libc::write(b, [0u8, 0].as_ptr() as *const libc::c_void, 2) } > 0);
        assert!(dev.read(&mut got).await.is_err(), "кадр короче шапки не должен читаться как EOF");

        unsafe {
            libc::close(a);
            libc::close(b);
        }
    }

    // ── мост к справочнику: смотрим ГЛАЗАМИ ТЕЛЕФОНА ─────────────────────────
    //
    // Тесты зовут `bmv_*` ровно так, как их зовёт Swift и Kotlin: строка на вход
    // сырым указателем, ответ читается и освобождается `bmv_free_string`. Именно
    // на этой границе правило и терялось: в справочнике оно есть и покрыто
    // тестами, а до телефона не доезжало.

    /// C-строка на вход (живёт до конца выражения — как у вызывающего).
    fn c_in(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    /// Прочитать и ОСВОБОДИТЬ ответ моста — как обязана делать оболочка.
    fn c_out(p: *mut c_char) -> String {
        assert!(!p.is_null(), "мост вернул NULL вместо строки");
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
        unsafe { bmv_free_string(p) };
        s
    }

    /// АДРЕС БЕЗ СХЕМЫ ОБЯЗАН СОХРАНЯТЬСЯ. Телефон разбирал адрес сам
    /// (`if !u.startsWith("http") return`) и на «bemyvpn.net» молча выходил из
    /// настройки: кнопка нажата, ничего не записано, ничего не сказано. Правило
    /// в справочнике это чинит с самого начала — не хватало только двери.
    #[test]
    fn a_phone_can_finally_type_the_address_without_its_scheme() {
        assert_eq!(c_out(bmv_coordinator_url(c_in("bemyvpn.net").as_ptr())), "https://bemyvpn.net");
        assert_eq!(c_out(bmv_coordinator_url(c_in("  bemyvpn.net/ ").as_ptr())), "https://bemyvpn.net");
        // Явную схему не трогаем — в том числе незашифрованную.
        assert_eq!(c_out(bmv_coordinator_url(c_in("http://10.0.0.5:3330").as_ptr())), "http://10.0.0.5:3330");
        // ОТКАЗ приезжает пустой строкой, а не молчанием: оболочке есть что сказать.
        assert_eq!(c_out(bmv_coordinator_url(c_in("   ").as_ptr())), "");
        assert_eq!(c_out(bmv_coordinator_url(c_in("https://").as_ptr())), "", "схема без адреса — не адрес");
        assert_eq!(c_out(bmv_coordinator_url(std::ptr::null())), "", "null не должен ронять границу C");

        // Показ и работа: с экрана схема уходит вместе с хвостовым слэшем (свои
        // копии на телефонах слэш оставляли), а обратно приезжает рабочий адрес.
        assert_eq!(c_out(bmv_display_coordinator(c_in("https://bemyvpn.net/").as_ptr())), "bemyvpn.net");
        let shown = c_out(bmv_display_coordinator(c_in("https://свой.сервер:8443").as_ptr()));
        assert_eq!(c_out(bmv_coordinator_url(c_in(&shown).as_ptr())), "https://свой.сервер:8443");
    }

    /// Уровни едут ЧИСЛОМ, и номера совпадают со справочником — на них телефон
    /// вешает свой значок и свой цвет.
    #[test]
    fn levels_cross_the_border_as_numbers() {
        use bmv_common::view::{Alarm, Protection};
        assert_eq!(bmv_protection(c_in("").as_ptr()), Protection::Encrypted as i32, "хост без протокола шифрует");
        assert_eq!(bmv_protection(c_in("noise-obfs").as_ptr()), Protection::Masked as i32);
        assert_eq!(bmv_protection(c_in("plain").as_ptr()), Protection::Plain as i32);
        assert_eq!(bmv_protection(c_in("wireguard").as_ptr()), Protection::Unknown as i32);

        // Пороги пинга — из справочника, второй линейки на телефоне быть не должно.
        assert_eq!(bmv_ping_alarm(249), Alarm::Calm as i32);
        assert_eq!(bmv_ping_alarm(250), Alarm::Amber as i32);
        assert_eq!(bmv_ping_alarm(501), Alarm::Red as i32);
        assert_eq!(bmv_ping_alarm(-1), Alarm::Muted as i32, "нет ответа — тревожить нечем");
        assert_eq!(c_out(bmv_ping_text(24)), "24 мс");
        assert_eq!(c_out(bmv_ping_text(-1)), bmv_common::view::PING_NO_ANSWER);

        // Обрыв связи — янтарь, а не красный: его чинит супервизор сам.
        assert_eq!(bmv_link_alarm(0), Alarm::Amber as i32);
        assert_ne!(bmv_link_alarm(0), Alarm::Red as i32);
        assert_eq!(bmv_link_alarm(1), Alarm::Calm as i32);
        assert_eq!(bmv_link_alarm(-1), Alarm::Muted as i32);
        assert_eq!(c_out(bmv_link_text(1)), "На связи");
        assert_eq!(c_out(bmv_link_text(0)), "Нет связи — восстанавливаю…");
        // «Ещё не знаем» — ОТДЕЛЬНОЕ третье состояние, а не «связи нет».
        assert_eq!(c_out(bmv_link_text(-1)), "Проверяю связь…");
        assert_ne!(c_out(bmv_link_text(-1)), c_out(bmv_link_text(0)));
        // Любое неожиданное число с границы C — тоже «не знаем», а не паника.
        assert_eq!(c_out(bmv_link_text(42)), c_out(bmv_link_text(-1)));
    }

    /// Часы тикают секундами, и забитый хост не предлагается.
    #[test]
    fn the_clock_ticks_and_a_full_host_is_not_offered() {
        assert_eq!(c_out(bmv_session_clock(75)), "01:15");
        assert_eq!(c_out(bmv_session_clock(3661)), "1:01:01");
        // Каждая секунда видна — минутная подпись читается как зависшая программа.
        assert_ne!(c_out(bmv_session_clock(61)), c_out(bmv_session_clock(62)));

        assert!(bmv_host_usable(true, 7, 8));
        assert!(!bmv_host_usable(true, 8, 8), "мест нет — подключаться некуда");
        assert!(!bmv_host_usable(false, 0, 8), "офлайн — не годен даже пустой");
        assert!(!bmv_host_usable(true, 0, 0), "потолок не объявлен — мест не обещано");
    }

    /// Три числа моста складываются в одно состояние — и конец раздачи не отказ.
    #[test]
    fn a_finished_share_crosses_the_border_as_its_own_state() {
        use bmv_common::view::Vpn;
        // Авто-реконнект отличается от первого подключения ТОЛЬКО этим флагом.
        assert_eq!(bmv_vpn_kind(1, 0, false), Vpn::Connecting as i32);
        assert_eq!(bmv_vpn_kind(1, 0, true), Vpn::Reconnecting as i32);
        assert_eq!(c_out(bmv_vpn_text(1, 0, true)), "Переподключение…");
        assert_ne!(c_out(bmv_vpn_text(1, 0, true)), c_out(bmv_vpn_text(1, 0, false)));

        assert_eq!(bmv_vpn_kind(2, 0, true), Vpn::On as i32);
        // Хост попрощался — это не поломка, и не важно, куда ушёл статус.
        assert_eq!(bmv_vpn_kind(0, 1, true), Vpn::Ended as i32);
        assert_eq!(bmv_vpn_kind(3, 1, true), Vpn::Ended as i32);
        assert_eq!(c_out(bmv_vpn_text(3, 1, true)), "Хост завершил раздачу — выберите другой");
        assert_eq!(bmv_vpn_kind(3, 2, true), Vpn::Lost as i32);
        // Выключили сами — причины нет, объяснять нечего.
        assert_eq!(bmv_vpn_kind(3, 0, true), Vpn::Off as i32);

        assert_eq!(c_out(bmv_empty_directory_hint(0)).lines().next().unwrap(), "Нет связи с сервером — восстанавливаю.");
        assert!(c_out(bmv_empty_directory_hint(1)).starts_with("Хостов пока нет"));
    }

    /// Имя хоста приходит с ЧУЖОЙ машины: внутренний нуль не должен обнулять
    /// весь ответ (раньше каталог пропадал целиком из-за одной карточки).
    #[test]
    fn to_c_survives_interior_nul() {
        let p = to_c("им\0я".to_string());
        assert!(!p.is_null(), "строка с внутренним нулём не должна давать NULL");
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
        unsafe { bmv_free_string(p) };
        assert_eq!(s, "имя");
    }
}
