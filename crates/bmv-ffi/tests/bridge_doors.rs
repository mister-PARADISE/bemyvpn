//! СТОРОЖ ДВЕРЕЙ: точка входа моста, которую не видит НИ ОДНА оболочка, — это
//! работа, сделанная в пустоту.
//!
//! Так и случилось с двенадцатью правилами показа (`bmv_coordinator_url`,
//! `bmv_ping_text`, `bmv_vpn_text`, …): в `crates/bmv-ffi` они были написаны и
//! покрыты тестами, а в `apps/ios/bmv_ffi.h` не объявлены и в JNI-мост не
//! проброшены. Swift и Kotlin их физически не видели, и в оболочках продолжали
//! жить 24 копии уже переехавших правил. Ни компилятор, ни CI не краснели:
//! Rust-часть собирается сама по себе, а заголовок и мост — просто файлы.
//!
//! Проверяем ровно две вещи, обе — «дверь прорезана»:
//!   1. каждая `extern "C" fn bmv_*` ОБЪЯВЛЕНА в `apps/ios/bmv_ffi.h`;
//!   2. у каждой есть пробрасывающая `Java_org_bemyvpn_Native_*`, которая её
//!      ЗОВЁТ (мало объявить рядом — вызов должен быть внутри точки входа JNI).
//!
//! Приём одолжен у `every_bridge_entry_keeps_the_shape_of_all_the_others`
//! (`crates/bmv-ffi/src/lib.rs`) и грепалок в `crates/bmv-common/tests/`.

use std::path::{Path, PathBuf};

/// Ядро FFI — источник истины о том, какие двери вообще существуют.
const FFI: &str = "crates/bmv-ffi/src/lib.rs";
/// Заголовок для Swift. ИМЕННО ЭТОТ файл: `apps/ios/headers/` — генерируемая
/// копия (`build-xcframework.sh` делает туда `rm -rf` и `cp`), она в .gitignore.
const HEADER: &str = "apps/ios/bmv_ffi.h";
/// JNI-мост для Kotlin.
const BRIDGE: &str = "apps/android/rust/src/lib.rs";

/// Точки входа, которым мост в Kotlin НЕ НУЖЕН. Список ЗАКРЫТЫЙ: у каждой
/// строки — причина, по которой двери там нет и быть не должно.
///
/// `bmv_free_string` — освобождает C-строку, а Kotlin указателей не видит
/// вообще: её зовёт `take()` внутри моста, отдавая наверх готовый `String`. В
/// заголовке она при этом обязана быть — Swift освобождает строки сам.
///
/// `bmv_send_bye` — прощание с хостом ОТДЕЛЬНО от остановки нужно только iOS:
/// там расширение туннеля — ЧУЖОЙ процесс, и на `stopTunnel` сокет уже мёртв,
/// поэтому приложение шлёт «bye» заранее через app→extension сообщение. На
/// Android `BmvVpnService` живёт в процессе приложения и зовёт `nativeStop`,
/// а `bmv_stop` начинается с того же прощания на живом канале.
const NO_JNI_DOOR: [&str; 2] = ["bmv_free_string", "bmv_send_bye"];

/// Корень репозитория — от каталога крейта вверх, пока не найдётся ядро FFI.
/// Ищем, а не считаем «..» по числу: тогда этот файл можно перенести в любой
/// крейт (например в `crates/bmv-ffi/tests/`) простым `mv`, без правок.
fn repo_root() -> PathBuf {
    let mut d: &Path = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        if d.join(FFI).exists() {
            return d.to_path_buf();
        }
        d = d.parent().unwrap_or_else(|| panic!("корень репозитория не найден: нет {FFI} ни в одном родителе"));
    }
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Убрать комментарии C/Rust: иначе `bmv_free_string()`, упомянутая в шапке
/// заголовка, сошла бы за объявление, а мёртвая ссылка в комментарии моста — за
/// вызов. Строк с кавычками здесь нет, поэтому хватает состояния «в коде / в //
/// / в /* */».
fn strip_comments(src: &str) -> String {
    let (mut out, mut it, mut mode) = (String::with_capacity(src.len()), src.chars().peekable(), 0u8);
    while let Some(c) = it.next() {
        match mode {
            1 => {
                if c == '\n' {
                    mode = 0;
                    out.push('\n');
                }
            }
            2 => {
                if c == '*' && it.peek() == Some(&'/') {
                    it.next();
                    mode = 0;
                }
            }
            _ => match (c, it.peek()) {
                ('/', Some('/')) => {
                    it.next();
                    mode = 1;
                }
                ('/', Some('*')) => {
                    it.next();
                    mode = 2;
                }
                _ => out.push(c),
            },
        }
    }
    out
}

#[test]
fn every_ffi_door_is_cut_through_to_both_shells() {
    let root = repo_root();
    let ffi = read(&root, FFI);
    let header = strip_comments(&read(&root, HEADER));
    let bridge = strip_comments(&read(&root, BRIDGE));

    // Тело каждой точки входа JNI — от её имени до имени следующей. Вызов ядра
    // обязан быть ВНУТРИ такого куска, а не просто где-то в файле.
    let jni: Vec<&str> = bridge.split("fn Java_org_bemyvpn_Native_").skip(1).collect();
    assert!(jni.len() > 10, "разбор JNI-моста сломался: точек входа найдено {}", jni.len());

    // Кавычки экранированы — эта строка не находит саму себя.
    let marker = "extern \"C\" fn bmv_";
    let (mut seen, mut missing) = (0, Vec::new());
    for line in ffi.lines() {
        let Some((_, tail)) = line.split_once(marker) else { continue };
        let name = format!(
            "bmv_{}",
            tail.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next().unwrap_or_default()
        );
        seen += 1;
        let call = format!("{name}(");
        if !header.contains(&call) {
            missing.push(format!("{name}: НЕ объявлена в {HEADER} — Swift её не видит"));
        }
        if !NO_JNI_DOOR.contains(&name.as_str()) && !jni.iter().any(|f| f.contains(&format!("bmv_ffi::{call}"))) {
            missing.push(format!("{name}: нет Java_org_bemyvpn_Native_*, зовущей её в {BRIDGE} — Kotlin её не видит"));
        }
    }

    assert!(seen >= 20, "разбор точек входа сломался: найдено {seen}, а их три десятка");
    assert!(
        missing.is_empty(),
        "точка входа моста есть в ядре, но НЕ ПРОРЕЗАНА в оболочку — правило переехало в общее место, а \
         телефон продолжает жить на своей копии:\n  {}\n\
         Объяви её в {HEADER} и/или проброси через Java_org_bemyvpn_Native_* в {BRIDGE}.",
        missing.join("\n  ")
    );
}
