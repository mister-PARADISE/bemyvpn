//! bmv-android — ТОНКИЙ JNI-мост Kotlin ↔ bmv-ffi (общее ядро с iOS).
//!
//! Логики здесь НЕТ: каждая функция конвертирует JString ↔ C-строки и зовёт
//! одноимённую bmv_* из crates/bmv-ffi. Так Android и iOS ведут себя байт-в-байт
//! одинаково: двухфазное подключение (connect → start_tunnel), авто-реконнект
//! pump_tunnel при смене сети, синхронный BYE в bmv_stop, nudge, хост-режим.
//!
//! Разница платформ — ровно один флаг: bmv_start_tunnel(fd, utun=false), потому
//! что Android-TUN отдаёт сырые IP-пакеты без 4-байтового заголовка iOS/utun.
//!
//! Логов нет сознательно (как в bmv-ffi): хост не хранит записей о трафике
//! гостей и не может их выдать.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jint, jlong, jstring, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;

/// JString → CString (пустая при null/ошибке) — на границу C-ABI.
fn c_arg(env: &mut JNIEnv, s: &JString) -> CString {
    let v: String = env.get_string(s).map(Into::into).unwrap_or_default();
    CString::new(v).unwrap_or_default()
}

/// Забрать строку из bmv-ffi (освободив её) и вернуть в Kotlin.
fn take(env: &mut JNIEnv, p: *mut c_char) -> jstring {
    let s = if p.is_null() {
        String::new()
    } else {
        // SAFETY: p — свежая C-строка из bmv_ffi (не null), освобождаем один раз.
        let v = unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("").to_string();
        unsafe { bmv_ffi::bmv_free_string(p) };
        v
    };
    env.new_string(s).map(|j| j.into_raw()).unwrap_or(std::ptr::null_mut())
}

fn b(v: bool) -> jboolean {
    if v { JNI_TRUE } else { JNI_FALSE }
}

// ── правила показа (справочник bmv_common::view) ─────────────────────────────
//
// Чистые функции: ни сети, ни рантайма, ни блокировок — их можно звать прямо с
// UI-потока, в отличие от всего остального в этом файле. Пока Kotlin их не
// видел, каждое правило жило здесь ВТОРОЙ копией на другом языке, и копии
// расходились молча. Уровни отдаются ЧИСЛОМ, а не цветом и не именем значка:
// наборы значков у оболочек разные (см. шапку view.rs).

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeCoordinatorUrl(
    mut env: JNIEnv, _c: JClass, input: JString,
) -> jstring {
    let s = c_arg(&mut env, &input);
    let r = bmv_ffi::bmv_coordinator_url(s.as_ptr());
    take(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeDisplayCoordinator(
    mut env: JNIEnv, _c: JClass, url: JString,
) -> jstring {
    let s = c_arg(&mut env, &url);
    let r = bmv_ffi::bmv_display_coordinator(s.as_ptr());
    take(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeProtection(
    mut env: JNIEnv, _c: JClass, protocol: JString,
) -> jint {
    let s = c_arg(&mut env, &protocol);
    bmv_ffi::bmv_protection(s.as_ptr())
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeSessionClock(
    mut env: JNIEnv, _c: JClass, seconds: jlong,
) -> jstring {
    let r = bmv_ffi::bmv_session_clock(seconds.max(0) as u64);
    take(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativePingText(
    mut env: JNIEnv, _c: JClass, ms: jint,
) -> jstring {
    let r = bmv_ffi::bmv_ping_text(ms);
    take(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativePingAlarm(
    _env: JNIEnv, _c: JClass, ms: jint,
) -> jint {
    bmv_ffi::bmv_ping_alarm(ms)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeHostUsable(
    _env: JNIEnv, _c: JClass, online: jboolean, guests: jint, max_guests: jint,
) -> jboolean {
    b(bmv_ffi::bmv_host_usable(online == JNI_TRUE, guests.max(0) as u32, max_guests.max(0) as u32))
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeLinkText(
    mut env: JNIEnv, _c: JClass, online: jint,
) -> jstring {
    let r = bmv_ffi::bmv_link_text(online);
    take(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeLinkAlarm(
    _env: JNIEnv, _c: JClass, online: jint,
) -> jint {
    bmv_ffi::bmv_link_alarm(online)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeEmptyDirectoryHint(
    mut env: JNIEnv, _c: JClass, online: jint,
) -> jstring {
    let r = bmv_ffi::bmv_empty_directory_hint(online);
    take(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeVpnKind(
    _env: JNIEnv, _c: JClass, status: jint, stop_reason: jint, was_connected: jboolean,
) -> jint {
    bmv_ffi::bmv_vpn_kind(status, stop_reason, was_connected == JNI_TRUE)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeVpnText(
    mut env: JNIEnv, _c: JClass, status: jint, stop_reason: jint, was_connected: jboolean,
) -> jstring {
    let r = bmv_ffi::bmv_vpn_text(status, stop_reason, was_connected == JNI_TRUE);
    take(&mut env, r)
}

// ── сигналинг (каталог / коды / IP / связь) ──────────────────────────────────

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeListWatch(
    mut env: JNIEnv, _c: JClass, coordinator: JString, since: jlong,
) -> jstring {
    let coord = c_arg(&mut env, &coordinator);
    let r = bmv_ffi::bmv_list_watch(coord.as_ptr(), since as u64);
    take(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeResolve(
    mut env: JNIEnv, _c: JClass, coordinator: JString, code: JString,
) -> jstring {
    let coord = c_arg(&mut env, &coordinator);
    let code = c_arg(&mut env, &code);
    let r = bmv_ffi::bmv_resolve(coord.as_ptr(), code.as_ptr());
    take(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeNewCode(
    mut env: JNIEnv, _c: JClass, coordinator: JString,
) -> jstring {
    let coord = c_arg(&mut env, &coordinator);
    let r = bmv_ffi::bmv_new_code(coord.as_ptr());
    take(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeMyIp(
    mut env: JNIEnv, _c: JClass, coordinator: JString,
) -> jstring {
    let coord = c_arg(&mut env, &coordinator);
    let r = bmv_ffi::bmv_my_ip(coord.as_ptr());
    take(&mut env, r)
}

/// Отклик до хоста в мс, -1 = не ответил. Сессию на хосте не создаёт.
#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeProbeRtt(
    mut env: JNIEnv, _c: JClass, coordinator: JString, hostId: JString, endpoints: JString,
) -> i32 {
    let coord = c_arg(&mut env, &coordinator);
    let id = c_arg(&mut env, &hostId);
    let eps = c_arg(&mut env, &endpoints);
    bmv_ffi::bmv_probe_rtt(coord.as_ptr(), id.as_ptr(), eps.as_ptr())
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeHealth(
    mut env: JNIEnv, _c: JClass, coordinator: JString,
) -> jboolean {
    let coord = c_arg(&mut env, &coordinator);
    b(bmv_ffi::bmv_health(coord.as_ptr()))
}

/// Круг до координатора в мс; 0 — ещё не мерили или связи нет.
#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeRttMs(
    mut env: JNIEnv, _c: JClass, coordinator: JString,
) -> jint {
    let coord = c_arg(&mut env, &coordinator);
    bmv_ffi::bmv_rtt_ms(coord.as_ptr()) as jint
}

// ── гость: подключение ───────────────────────────────────────────────────────

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeConnect(
    mut env: JNIEnv, _c: JClass,
    coordinator: JString, host_id: JString, password: JString, protocol: JString,
) -> jboolean {
    let coord = c_arg(&mut env, &coordinator);
    let host = c_arg(&mut env, &host_id);
    let pw = c_arg(&mut env, &password);
    let proto = c_arg(&mut env, &protocol);
    b(bmv_ffi::bmv_connect(coord.as_ptr(), host.as_ptr(), pw.as_ptr(), proto.as_ptr()))
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeStartTunnel(
    _env: JNIEnv, _c: JClass, fd: jint,
) -> jboolean {
    // utun=false: Android-TUN — сырые IP-пакеты, без 4-байтового заголовка iOS.
    b(bmv_ffi::bmv_start_tunnel(fd, false))
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeVpnStatus(_env: JNIEnv, _c: JClass) -> jint {
    bmv_ffi::bmv_vpn_status()
}

/// Почему сеанс кончился сам: 0 — не кончался, 1 — хост завершил раздачу,
/// 2 — связь потеряна. Экран читает вместе с концом сеанса, чтобы не показывать
/// отказ там, где всё сработало правильно.
#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeStopReason(_env: JNIEnv, _c: JClass) -> jint {
    bmv_ffi::bmv_stop_reason()
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeStop(_env: JNIEnv, _c: JClass) {
    // Синхронно шлёт BYE хосту на живом канале и гасит сессию (как на iOS).
    bmv_ffi::bmv_stop();
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeNudge(_env: JNIEnv, _c: JClass) {
    // Смена сети (WiFi↔сотовая/вышка) — форсировать реконнект, не роняя TUN.
    bmv_ffi::bmv_nudge_reconnect();
}

// ── хост-режим ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeHostStart(
    mut env: JNIEnv, _c: JClass,
    coordinator: JString, host_id: JString, token: JString, code_sig: JString,
    name: JString, max_guests: jint, password: JString, protocol: JString,
    public: jboolean,
) -> jstring {
    let coord = c_arg(&mut env, &coordinator);
    let id = c_arg(&mut env, &host_id);
    let token = c_arg(&mut env, &token);
    let sig = c_arg(&mut env, &code_sig);
    let name = c_arg(&mut env, &name);
    let pw = c_arg(&mut env, &password);
    let proto = c_arg(&mut env, &protocol);
    let r = bmv_ffi::bmv_host_start(
        coord.as_ptr(), id.as_ptr(), token.as_ptr(), sig.as_ptr(), name.as_ptr(),
        max_guests, pw.as_ptr(), proto.as_ptr(), public == JNI_TRUE,
    );
    take(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeHostStop(_env: JNIEnv, _c: JClass) {
    bmv_ffi::bmv_host_stop();
}

#[no_mangle]
pub extern "system" fn Java_org_bemyvpn_Native_nativeHostUpdate(
    mut env: JNIEnv, _c: JClass,
    name: JString, max_guests: jint, password: JString, protocol: JString, public: jboolean,
) {
    let name = c_arg(&mut env, &name);
    let pw = c_arg(&mut env, &password);
    let proto = c_arg(&mut env, &protocol);
    bmv_ffi::bmv_host_update(name.as_ptr(), max_guests, pw.as_ptr(), proto.as_ptr(), public == JNI_TRUE);
}
