//! ЧАСОВОЙ ПРОТИВ ВТОРОЙ ПАЛИТРЫ: цвет живёт ровно в ОДНОМ месте.
//!
//! Источник — `design/palette.toml`; по четырём темам (Slint, Swift, Kotlin,
//! Rust) его раскладывает `design/sync-palette.py`. Часовой ловит ОБЕ беды:
//!
//!   1. ТЕМА РАЗОШЛАСЬ С ИСТОЧНИКОМ — кто-то поправил цвет прямо в теме, мимо
//!      источника. Ловится прогоном скрипта в режиме `--check`: он собирает
//!      участок заново и сравнивает побайтно.
//!   2. ОБОЛОЧКИ РАЗОШЛИСЬ МЕЖДУ СОБОЙ — ровно то, что уже случилось однажды:
//!      терминал два круга правок держал самый первый акцент `#5E93FF`, а окно
//!      кромку `#FFFFFF14` (0.0784) там, где на телефонах стояло 0.08. Ловится
//!      вторым тестом, независимо от скрипта: каждый цвет источника обязан
//!      лежать в КАЖДОЙ теме, в её собственной записи.
//!
//! ЖИТЬ ЭТОМУ ФАЙЛУ РЯДОМ С ОСТАЛЬНЫМИ ЧАСОВЫМИ, в `crates/bmv-common/tests/`
//! (там уже `one_place_per_rule.rs` и `no_code_by_substring.rs` — та же порода).
//! Здесь он лежит временно: `crates/` в этой работе был чужим участком. При
//! переносе поправить только `repo_root()` — на один `parent()` меньше.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Корень репозитория. `CARGO_MANIFEST_DIR` = `<корень>/apps/bmv-gui`.
fn repo_root() -> PathBuf {
    // crates/bmv-common → crates → корень репозитория.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

const BEGIN: &str = "── НАЧАЛО: значения из design/palette.toml";
const END: &str = "── КОНЕЦ: значения из design/palette.toml";

/// Размеченный участок темы — только то, что раскладывает скрипт.
fn marked(root: &Path, rel: &str) -> String {
    let src = std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let start = src.find(BEGIN).unwrap_or_else(|| panic!("{rel}: нет маркера начала участка"));
    let end = src.find(END).unwrap_or_else(|| panic!("{rel}: нет маркера конца участка"));
    assert!(end > start, "{rel}: маркеры участка перепутаны местами");
    src[start..end].to_string()
}

/// Цвета из `[colors]` источника: (имя, «08090B»). Разбор плоский — файл тоже
/// плоский, а полноценный TOML ради тридцати строк тянул бы зависимость.
fn palette_colors(root: &Path) -> Vec<(String, String)> {
    let src = std::fs::read_to_string(root.join("design/palette.toml")).expect("design/palette.toml");
    let mut out = Vec::new();
    let mut in_colors = false;
    for line in src.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_colors = line == "[colors]";
            continue;
        }
        if !in_colors || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = line.split_once('=').expect("строка вида «ключ = значение»");
        let hex = rest.trim().trim_start_matches('"').get(..7).expect("цвет вида «#RRGGBB»");
        out.push((key.trim().to_string(), hex.trim_start_matches('#').to_uppercase()));
    }
    assert!(out.len() >= 8, "в [colors] подозрительно мало цветов ({}) — сломан разбор", out.len());
    out
}

/// Беда №1: тема разошлась с источником.
///
/// Проверяет тем же кодом, что и раскладывает, — второго генератора нарочно нет:
/// два генератора и есть та самая вторая правда, от которой всё затевалось.
#[test]
fn every_theme_matches_the_palette_source() {
    let root = repo_root();
    let out = Command::new("python3")
        .arg("design/sync-palette.py")
        .arg("--check")
        .current_dir(&root)
        .output()
        // Молча пропустить проверку нельзя: часовой, который умеет не сработать,
        // не часовой.
        .expect("нужен python3 в PATH — им проверяется и раскладывается палитра");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Беда №2: оболочки разошлись между собой.
///
/// Не зовёт скрипт вовсе: если участок кто-то сотрёт вместе с маркерами, первый
/// тест этого может и не заметить, а этот заметит.
#[test]
fn every_shell_carries_every_color() {
    let root = repo_root();
    let colors = palette_colors(&root);

    // Как цвет записан в каждой оболочке.
    let slint = marked(&root, "apps/bmv-gui/ui/theme.slint");
    let swift = marked(&root, "apps/ios/BeMyVPN/Theme.swift");
    let kotlin = marked(&root, "apps/android/app/src/main/java/org/bemyvpn/Theme.kt");
    let rust = marked(&root, "apps/bmv-cli/src/tui.rs");

    let mut missing = Vec::new();
    for (name, hex) in &colors {
        for (shell, text, literal) in [
            ("окно (Slint)", &slint, format!("#{hex}")),
            ("iPhone (Swift)", &swift, format!("0x{hex}")),
            ("Android (Kotlin)", &kotlin, format!("0xFF{hex}")),
        ] {
            if !text.contains(&literal) {
                missing.push(format!("{shell}: нет {name} = {literal}"));
            }
        }
    }

    // У терминала поверхностей нет, поэтому он берёт не все роли — только эти.
    // Список явный: «сколько-нибудь цветов» проверкой не является.
    for name in ["accent", "amber", "red", "dim", "fg", "bg", "card_hi"] {
        let hex = &colors.iter().find(|(n, _)| n == name).expect(name).1;
        let literal = format!(
            "Color::Rgb(0x{}, 0x{}, 0x{})",
            &hex[0..2],
            &hex[2..4],
            &hex[4..6]
        );
        if !rust.contains(&literal) {
            missing.push(format!("терминал (Rust): нет {name} = {literal}"));
        }
    }

    assert!(
        missing.is_empty(),
        "оболочки разошлись по цвету — а глазами это ловится только на снимках:\n  {}\n\
         Цвета живут в design/palette.toml; починить: python3 design/sync-palette.py",
        missing.join("\n  ")
    );
}

/// Канон бренда лежит не только в SVG.
///
/// Логотип «Звено» палитре НЕ подчиняется (см. brand/README.md — «не
/// перекрашивать половинки»), поэтому его цвета не раскладываются по темам. Но
/// сырыми они лежать не должны: `[brand]` в источнике — их именованное место, а
/// этот тест держит источник и картинки в одной правде. Однажды в окне уже жила
/// третья версия логотипа с цветами позапрошлой палитры.
#[test]
fn brand_rings_match_the_canon() {
    let root = repo_root();
    let src = std::fs::read_to_string(root.join("design/palette.toml")).expect("design/palette.toml");
    let logo = std::fs::read_to_string(root.join("brand/logo.svg")).expect("brand/logo.svg");
    let icon = std::fs::read_to_string(
        root.join("apps/android/app/src/main/res/drawable/ic_launcher_foreground.xml"),
    )
    .expect("android adaptive icon");

    let mut in_brand = false;
    let mut checked = 0;
    for line in src.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_brand = line == "[brand]";
            continue;
        }
        if !in_brand || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = line.split_once('=').expect("строка вида «ключ = значение»");
        let hex = rest.trim().trim_start_matches('"')[1..7].to_uppercase();
        let (key, logo_up, icon_up) = (key.trim(), logo.to_uppercase(), icon.to_uppercase());
        assert!(logo_up.contains(&hex), "brand/logo.svg разошёлся с [brand]: нет {key} = #{hex}");
        assert!(
            icon_up.contains(&hex),
            "андроидная иконка разошлась с [brand]: нет {key} = #{hex}"
        );
        checked += 1;
    }
    assert_eq!(checked, 4, "в [brand] должно быть четыре цвета колец, найдено {checked}");
}
