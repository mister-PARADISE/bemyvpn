//! ЧАСОВОЙ ПРОТИВ РАСХОЖДЕНИЯ: правило показа живёт ровно в ОДНОМ месте.
//!
//! До этого справочника (`bmv_common::view`) каждое правило существовало копиями
//! в четырёх оболочках, и копии разошлись МОЛЧА: пустой протокол в окне —
//! «Обычный», на телефоне — «Без шифра»; часы сеанса в терминале — «12 мин»;
//! забитый хост в терминале давал нажать «Подключить». Ни одна проверка при этом
//! не краснела, потому что каждая копия по отдельности компилировалась и даже
//! была права по-своему.
//!
//! Здесь два часовых:
//!   1. `a_display_rule_exists_in_exactly_one_place` — грепалка по боевому Rust:
//!      признак правила за пределами `view.rs` = вторая правда, тест падает.
//!   2. `ping_thresholds_match_the_window_skin` — та же линейка пинга в разметке
//!      окна (`ui/*.slint`): её оттуда не позвать, значит сверяем числами.
//!
//! Приём одолжен у `bmv-config` (`every_knob_is_read_by_someone`): дешевле
//! поймать грепом, чем каждый раз ловить руками.

use std::path::{Path, PathBuf};

use bmv_common::view::{PING_AMBER_MS, PING_RED_MS};

/// Где живёт боевой код. `vendor/` — чужой, `target/` — сборка.
const ROOTS: [&str; 3] = ["crates", "apps", "server"];

/// Сам справочник — единственное законное место для всех признаков ниже.
const HOME: &str = "crates/bmv-common/src/view.rs";

/// Правило → признаки, которые обязаны встретиться НА ОДНОЙ строке → где ещё
/// законно (кроме `HOME`).
///
/// Признак выбран так, чтобы его нельзя было не написать, переписывая правило
/// заново: подпись, из которой правило состоит, или сравнение, в котором вся его
/// суть.
type Rule = (&'static str, &'static [&'static str], &'static [&'static str]);
const RULES: &[Rule] = &[
    // Подпись протокола: заново её пишут только вместе с этим словом. Без
    // кавычек — оболочка вольна приклеить к подписи свой значок («🎭 Маскировка»).
    ("имя протокола (view::proto_name)", &["Маскировка"], &[]),
    ("часы сеанса (view::session_clock)", &["% 3600"], &[]),
    ("подпись пинга (view::ping)", &["мс\""], &[]),
    ("годен ли хост (view::host_usable)", &[".guests <"], &[]),
    (
        "адрес координатора (view::coordinator_url)",
        &["\"http://\""],
        // bmv-signal переводит адрес координатора в адрес ВЕБСОКЕТА
        // (https→wss, http→ws). Это транспорт, а не разбор того, что набрал
        // человек: показывать там нечего и подставлять схему не надо.
        &["crates/bmv-signal/src/lib.rs"],
    ),
    ("имя хоста (view::host_display_name)", &["name.is_empty()", "id"], &[]),
];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            // Каталоги `tests` мимо: в тесте оболочки повторить подпись законно —
            // тест как раз и проверяет, что оболочка показывает ровно её.
            if !matches!(&*name, "target" | "tests" | "build" | ".git" | "vendor" | "node_modules") {
                rs_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

#[test]
fn a_display_rule_exists_in_exactly_one_place() {
    let root = repo_root();
    let mut files = Vec::new();
    for r in ROOTS {
        rs_files(&root.join(r), &mut files);
    }
    assert!(files.len() > 20, "грепалка ничего не нашла — сломан обход каталогов ({})", root.display());

    let home = root.join(HOME);
    assert!(home.exists(), "справочник правил переехал — поправь HOME в этом тесте");

    let mut guilty = Vec::new();
    for f in &files {
        if *f == home {
            continue; // дом правила
        }
        let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string();
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        // Всё после `#[cfg(test)]` — тесты: там повтор подписи законен.
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        for (i, line) in code.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue; // комментарии как раз ОБЪЯСНЯЮТ правило, а не повторяют
            }
            for (rule, needles, allowed) in RULES {
                if allowed.iter().any(|a| rel.replace('\\', "/") == *a) {
                    continue;
                }
                if needles.iter().all(|n| t.contains(n)) {
                    guilty.push(format!("{rel}:{}: {rule}\n      {t}", i + 1));
                }
            }
        }
    }

    assert!(
        guilty.is_empty(),
        "правило показа заведено вторым местом — рано или поздно эти двое разойдутся:\n  {}\n\
         Правило живёт в {HOME} целиком; оболочка его ЗОВЁТ, а не повторяет.",
        guilty.join("\n  ")
    );
}

/// Пороги пинга в разметке окна — те же числа, что в справочнике.
///
/// Slint не умеет звать Rust-функции из выражения цвета, поэтому линейка там
/// записана числами. Значит либо сверять, либо однажды обнаружить, что окно
/// краснеет с 400 мс, а всё остальное — с 500.
#[test]
fn ping_thresholds_match_the_window_skin() {
    let root = repo_root();
    // Оба места, где окно раскрашивает пинг: плитка хоста и строка координатора.
    for rel in ["apps/bmv-gui/ui/components.slint", "apps/bmv-gui/ui/server_page.slint"] {
        let src = std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        for needle in [format!("< {PING_AMBER_MS}"), format!("<= {PING_RED_MS}")] {
            assert!(
                src.contains(&needle),
                "{rel}: линейка пинга разошлась со справочником — там нет «{needle}»\n\
                 (bmv_common::view: янтарь с {PING_AMBER_MS} мс, красный после {PING_RED_MS} мс)"
            );
        }
    }
}
