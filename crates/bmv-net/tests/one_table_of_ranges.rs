//! ЧАСОВОЙ ПРОТИВ ВТОРОЙ ТАБЛИЦЫ АДРЕСОВ: диапазоны живут ровно в ОДНОМ месте.
//!
//! Так уже было. Один и тот же список внутренних диапазонов (петля, приватные
//! сети, link-local с метаданными облака, CGNAT, документационные, 240/4) лежал
//! ДВУМЯ копиями — в SSRF-фильтре хоста (`bmv-tunnel`) и в разборе ответа STUN
//! (`bmv-net`), — и копии молча разошлись: site-local `fec0::/10` и IPv4,
//! зашитый в IPv6 (v4-mapped/NAT64/6to4), знала только первая. Обе копии
//! компилировались, обе были покрыты тестами, и обе были по-своему правы.
//!
//! Теперь таблица одна (`bmv-net/src/reach.rs`), а политик над ней три:
//! куда открывать сокет, куда слать PUNCH, что считать своим внешним адресом.
//! Четвёртая политика пишется строкой над той же таблицей — а не новой копией
//! диапазонов, что этот тест и стережёт.
//!
//! Приём одолжен у `bmv-common` (`a_display_rule_exists_in_exactly_one_place`):
//! дешевле поймать грепом, чем однажды обнаружить расхождение в бою.

use std::path::{Path, PathBuf};

/// Дом таблицы — единственное законное место признаков ниже.
const HOME: &str = "crates/bmv-net/src/reach.rs";

/// Признаки таблицы диапазонов. Выбраны так, что переписать таблицу заново, не
/// написав их, нельзя: это и есть проверки диапазонов.
///
/// `is_loopback` — ГЛАВНАЯ проверка таблицы (с неё начинается и v4-, и
/// v6-ветка), и её тут не было вовсе: можно было завести второй фильтр
/// «петля + link-local + CGNAT», не задев ни одного признака. Ровно так
/// разошлись прошлые две копии.
///
/// БЕЗ СКОБОК нарочно: `is_private()` ловило один стиль записи, а мимо
/// проходили `is_private ()`, `Ipv4Addr::is_private`, `.is_private`
/// в цепочке — то есть та же проверка, набранная чуть иначе.
const NEEDLES: [&str; 3] = ["is_loopback", "is_private", "is_documentation"];

/// Координатор (`server/coordinator`) держит свою копию СОЗНАТЕЛЬНО: он
/// самодостаточный крейт и не тянет `bmv-*` (сказано в его Cargo.toml), поэтому
/// сюда не смотрим. Оболочки и ядро — смотрим: им таблица доступна вызовом.
///
/// `apps` тут стояло в комментарии, но не в списке: часовой обещал оболочки и
/// не заходил в них ни разу. Не-Rust оболочки (Kotlin/Swift/разметка) этих
/// признаков не пишут — про таблицу адресов они вообще не знают, весь разбор
/// живёт в ядре, — поэтому расширений сюда добавлять нечего.
const ROOTS: [&str; 2] = ["crates", "apps"];

/// Законная вторая площадка: вопрос ДРУГОЙ.
///
/// `has_ipv6` спрашивает «есть ли у ЭТОЙ МАШИНЫ живой v6-адрес» — про свой
/// собственный адрес, а не про то, куда можно ходить. Таблица `reach` отвечает
/// на второй вопрос и для первого не годится: она сочла бы годным любой
/// публичный адрес, в том числе чужой.
const ALLOWED: [&str; 1] = ["crates/bmv-desktop/src/lib.rs"];

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            // Каталоги `tests` мимо: назвать там конкретный диапазон законно —
            // тест как раз и проверяет, что таблица судит о нём правильно.
            if !matches!(&*name, "target" | "tests" | "build" | ".git" | "vendor" | "node_modules") {
                source_files(&p, out);
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
fn the_table_of_internal_ranges_exists_in_exactly_one_place() {
    let root = repo_root();
    let home = root.join(HOME);
    let home_src = std::fs::read_to_string(&home)
        .unwrap_or_else(|e| panic!("таблица переехала — поправь HOME в этом тесте ({}): {e}", home.display()));
    for n in NEEDLES {
        assert!(
            home_src.contains(n),
            "признак «{n}» пропал из самой таблицы ({HOME}) — часовой перестал что-либо стеречь"
        );
    }

    let mut files = Vec::new();
    for r in ROOTS {
        source_files(&root.join(r), &mut files);
    }
    assert!(files.len() > 20, "грепалка ничего не нашла — сломан обход каталогов ({})", root.display());

    let mut guilty = Vec::new();
    for f in &files {
        if *f == home {
            continue; // дом правила
        }
        let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string().replace('\\', "/");
        if ALLOWED.contains(&&*rel) {
            continue; // другой вопрос, см. ALLOWED
        }
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        // Всё после `#[cfg(test)]` — тесты: назвать там конкретный адрес законно.
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        for (i, line) in code.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue; // комментарий ОБЪЯСНЯЕТ правило, а не повторяет его
            }
            for n in NEEDLES {
                if t.contains(n) {
                    guilty.push(format!("{rel}:{}: {}", i + 1, t));
                }
            }
        }
    }

    assert!(
        guilty.is_empty(),
        "таблица внутренних диапазонов заведена вторым местом — эти двое разойдутся, \
         как уже разошлись прошлые две копии:\n  {}\n\
         Новый фильтр — это ОДНА СТРОКА над `bmv_net::reach` ({HOME}), а не свой список диапазонов.",
        guilty.join("\n  ")
    );
}
