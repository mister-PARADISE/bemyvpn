//! ГРЕПАЛКА: код отказа сервера опознаётся ПОЛЕМ, а не буквами в тексте.
//!
//! Коммит c54df8e завёл `Error::Refused { code, reason }` и `refusal_code()`,
//! но один `e.to_string().contains("403")` пережил уборку и жил ещё долго —
//! молча, потому что всё компилируется. `Display` у `Refused` показывает ТОЛЬКО
//! человеческую причину (русская фраза без цифр), так что такое условие ложно
//! ВСЕГДА: ветка мертва, а по коду не видно.
//!
//! Дешевле поймать грепом, чем каждый раз ловить руками.

use std::path::{Path, PathBuf};

/// Где живёт боевой код. `vendor/` — чужой, `target/` — сборка.
const ROOTS: [&str; 3] = ["crates", "apps", "server"];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            // target/build/сборочный мусор и сами тесты — мимо: в тестах
            // подстрока с цифрами законна (мы ей и доказываем, что приём мёртв).
            if !matches!(&*name, "target" | "tests" | "build" | ".git" | "node_modules") {
                rs_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn refusal_codes_are_never_matched_as_text() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    let mut files = Vec::new();
    for r in ROOTS {
        rs_files(&root.join(r), &mut files);
    }
    assert!(files.len() > 20, "грепалка ничего не нашла — сломан обход каталогов ({})", root.display());

    let mut guilty = Vec::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        // Всё после `#[cfg(test)]` — тесты, там подстрока разрешена.
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        for (i, line) in code.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue; // комментарии как раз ОБЪЯСНЯЮТ старый приём
            }
            // `.contains("403")` и любой другой трёхзначный код в тексте ошибки.
            for part in t.split(".contains(\"").skip(1) {
                let num: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
                if num.len() == 3 && part[num.len()..].starts_with('"') {
                    guilty.push(format!("{}:{}: {}", f.strip_prefix(root).unwrap_or(f).display(), i + 1, t));
                }
            }
        }
    }

    assert!(
        guilty.is_empty(),
        "код отказа опознан ПОДСТРОКОЙ В ТЕКСТЕ — эта ветка не сработает никогда \
         (в тексте `Refused` цифр нет). Берите `Error::refusal_code()`:\n{}",
        guilty.join("\n")
    );
}
