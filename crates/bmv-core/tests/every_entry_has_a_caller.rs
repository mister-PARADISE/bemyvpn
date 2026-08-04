//! ГРЕПАЛКА: у публичного входа движка есть хотя бы один зовущий.
//!
//! `bmv-core` — единственный фасад для всех оболочек, и вход в него стоит куда
//! дороже строки в конфиге: его нужно держать, о нём нужно помнить, он тянет за
//! собой чужой код (`classify_nat` в одиночку удерживал живым весь разбор типа
//! NAT в `bmv-net`). За одну уборку здесь нашлось ДВА входа, которых не звал
//! никто и никогда: `classify_nat` («для UI и раннего вывода „нужен релей“» —
//! ни того UI, ни релея в продукте нет) и `host_reannounce` («совместимость» —
//! совместимость с никем).
//!
//! Приём одолжен у `bmv-config` (`every_knob_is_read_by_someone`) и у
//! `bmv-common` (`one_place_per_rule`): дешевле поймать грепом, чем каждый раз
//! ловить руками.
//!
//! ЧЕСТНО ПРО ПОТОЛОК. Это грубый текстовый поиск, и ошибается он только в
//! сторону «пропустил»: вход с редким именем (`classify_nat`, `host_reannounce`,
//! `demo_loopback`) ловится надёжно, а вход, чьё имя — частое слово, найдёт себе
//! случайного однофамильца в чужом файле, и тест промолчит. Ложной тревоги не
//! бывает по построению — значит хуже сегодняшнего не станет.
//!
//! Зовущим считается ТОЛЬКО боевой код: у каждого файла отрезается всё от
//! `#[cfg(test)]`, каталоги `tests/` пропускаются целиком, строки-комментарии
//! выбрасываются. Иначе вход «оживлял» бы собственный тест или объяснение в
//! комментарии — ровно так и держатся мёртвые ручки. Свой же файл — зовущий
//! настоящий: `announce_state` наружу не зовут, её зовут соседние методы, и это
//! нормально.

use std::path::{Path, PathBuf};

/// Где живёт боевой код. `vendor/` — чужой, `target/` — сборка.
const ROOTS: [&str; 3] = ["crates", "apps", "server"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

fn collect(dir: &Path, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == "target" || n == "vendor" || n == "tests") {
            continue;
        }
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            if let Ok(text) = std::fs::read_to_string(&p) {
                for line in text.split("#[cfg(test)]").next().unwrap_or("").lines() {
                    if !line.trim_start().starts_with("//") {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
            }
        }
    }
}

#[test]
fn every_public_entry_of_the_engine_has_a_caller() {
    // Имена входов — из собственного исходника: список не ведётся руками и
    // потому не устареет.
    let src = include_str!("../src/lib.rs");
    let entries: Vec<&str> = src
        .split("#[cfg(test)]")
        .next()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("pub fn ").or_else(|| l.strip_prefix("pub async fn ")))
        .filter_map(|l| l.split(['(', '<']).next())
        .collect();
    assert!(entries.len() > 15, "разбор входов сломался, нашлось всего {}", entries.len());

    let root = repo_root();
    let mut haystack = String::new();
    for r in ROOTS {
        collect(&root.join(r), &mut haystack);
    }
    assert!(haystack.len() > 100_000, "исходники не собрались: {} байт", haystack.len());

    // Метод зовут через точку (`eng.my_ip()`), общую функцию — через `::`
    // (`BmvEngine::relay_peer_checks(...)`).
    let dead: Vec<&str> = entries
        .iter()
        .filter(|n| !haystack.contains(&format!(".{n}(")) && !haystack.contains(&format!("::{n}(")))
        .copied()
        .collect();

    assert!(
        dead.is_empty(),
        "публичные входы движка, которых никто не зовёт: {dead:?}\n\
         Вход без зовущего — обещание, которого нет, и он держит живым код под собой. \
         Либо подключи его к делу, либо убери (см. историю classify_nat и host_reannounce)."
    );
}
