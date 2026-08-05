//! ГРЕПАЛКА: у клиента НЕТ ЖУРНАЛА. Ни файла, ни системного, ни выключенного
//! по умолчанию.
//!
//! BeMyVPN — VPN. Запись о том, к кому человек подключился, куда пошёл его
//! трафик и с какого он адреса, противоречит смыслу продукта: у того, кто ведёт
//! такую запись, её можно потребовать. Поэтому убрано не «лишнее логирование», а
//! сама возможность: в клиентских крейтах не осталось ни подписчика, ни вызовов
//! записи, а в форках `vendor/` журнал вырезан на сборке (`max_level_off`).
//!
//! Возвращается это молча и по одной строке за раз — «на минутку, для отладки».
//! Каждая такая строка компилируется, тесты от неё зелёные, а следы в системе
//! появляются. Дешевле поймать грепом.
//!
//! Что НЕ является журналом и здесь не запрещено:
//!   * вывод команд терминала (`bemyvpn ping`, список хостов) — человек сам их
//!     набрал и ждёт ответа на экране, поэтому `apps/bmv-cli` печатать волен;
//!   * `build.rs` — это директивы cargo сборщику, а не работа программы;
//!   * тесты — им положено объяснять, почему они упали.

use std::path::{Path, PathBuf};

/// Где живёт КЛИЕНТСКИЙ боевой код.
///
/// `server/` не сканируем НАРОЧНО: координатор — сервер на чужой машине, и её
/// владелец вправе видеть, что происходит. Что именно он пишет, стережёт его
/// собственный тест (`log_hint_never_reveals_the_code`): коды сетей уходят в
/// журнал только необратимой меткой, адресов гостей там нет вовсе.
///
/// `vendor/` — чужой код, правкой его не удержать (перезатрётся при обновлении
/// вверх по течению). Там журнал выключен на сборке фичей `max_level_off` в
/// `vendor/*/Cargo.toml`; за неё отвечает `the_vendored_forks_are_built_mute`.
const ROOTS: [&str; 2] = ["crates", "apps"];

/// Расширения всех четырёх оболочек: ядро и десктоп (`rs`), iPhone (`swift`),
/// Android (`kt`). Первая утечка была именно в оболочке — код сети и адрес
/// координатора открытым текстом в системном журнале iPhone.
const EXTS: [&str; 3] = ["rs", "swift", "kt"];

/// Вызовы, которые пишут В ЖУРНАЛ. Запрещены во ВСЕХ клиентских файлах.
///
/// Вместе с самими вызовами — то, чем журнал ВКЛЮЧАЮТ: подписчик без вызовов
/// безобиден ровно до первого вызова, а вызовы без подписчика — заряженное
/// ружьё, которое выстрелит, как только подписчика вернут.
const JOURNAL: [&str; 20] = [
    // Rust
    "log::trace!", "log::debug!", "log::info!", "log::warn!", "log::error!",
    "tracing::trace!", "tracing::debug!", "tracing::info!", "tracing::warn!", "tracing::error!",
    "tracing::event!", "tracing::span!", "#[instrument",
    "tracing_subscriber", "env_logger", "set_logger", "android_logger",
    // iOS: os.Logger/os_log писали код сети с `privacy: .public` — читается
    // другими приложениями и уезжает в диагностические выгрузки Apple.
    "os_log", "NSLog",
    // Android
    "android.util.Log",
];

/// Печать в поток. Запрещена везде, КРОМЕ перечисленных в `TALKS_TO_HUMAN`.
///
/// stdout/stderr — не «в никуда»: у процесса под ярлыком рабочего стола их
/// забирает системный журнал ОС, у службы — journald, у Android — logcat.
/// Порядок ЗНАЧИМ: `eprintln!` содержит в себе `println!`, и при обратном
/// порядке в отчёте о падении стояло бы не то имя, что человек написал.
const STREAMS: [&str; 8] = [
    "eprintln!", "println!", "eprint!", "print!", "dbg!", // Rust
    "debugPrint(", "printStackTrace(", "System.out", // Swift/Kotlin
];

/// Кому печатать МОЖНО, потому что он отвечает человеку на его же команду.
fn talks_to_human(rel: &str) -> bool {
    // Терминальная утилита: весь её вывод — ответ на набранную команду.
    rel.starts_with("apps/bmv-cli/")
        // Директивы cargo (`cargo:rerun-if-changed`) — это сборка, не работа.
        || rel.ends_with("build.rs")
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            // `tests` — тестам объясняться положено. `build`/`target` — сборочный
            // мусор (в apps/ios/build лежат чужие исходники, они не наши).
            if !matches!(&*name, "target" | "tests" | "build" | ".git" | "node_modules" | ".claude") {
                source_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| EXTS.iter().any(|e| x == *e)) {
            out.push(p);
        }
    }
}

/// Отрезать тесты и комментарии — остаётся боевой код.
///
/// Тестовый модуль опознаём по `#[cfg(` + `test` в одной строке: в проекте
/// встречаются и голый `#[cfg(test)]`, и `#[cfg(all(test, target_os = "macos"))]`,
/// и по одной только первой форме отрезалось бы не всё.
fn live_code(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("#[cfg(") && t.contains("test") {
            break; // дальше тесты — до конца файла
        }
        // Комментарии как раз ОБЪЯСНЯЮТ, почему записи больше нет.
        if t.starts_with("//") || t.starts_with("*") || t.starts_with("/*") {
            continue;
        }
        out.push((i + 1, t.to_string()));
    }
    out
}

/// `print(` в Swift, но не `sprint(`/`footprint(`. Отдельной проверкой, потому
/// что подстрока слишком короткая, чтобы искать её как все остальные.
fn calls_bare_print(line: &str) -> bool {
    line.match_indices("print(").any(|(at, _)| {
        at == 0 || !matches!(line.as_bytes()[at - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.')
    })
}

/// `Log.d(` и родня — только в Kotlin, где `Log` это `android.util.Log`.
fn calls_android_log(line: &str) -> bool {
    ["Log.d(", "Log.i(", "Log.w(", "Log.e(", "Log.v(", "Log.wtf("].iter().any(|m| line.contains(m))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

#[test]
fn no_journal_in_the_client() {
    let root = repo_root();
    let mut files = Vec::new();
    for r in ROOTS {
        source_files(&root.join(r), &mut files);
    }
    assert!(files.len() > 30, "грепалка ничего не нашла — сломан обход каталогов ({})", root.display());

    let mut guilty: Vec<String> = Vec::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string();
        let rel = rel.replace('\\', "/");
        let swift_or_kt = f.extension().is_some_and(|x| x == "swift" || x == "kt");
        let free_to_print = talks_to_human(&rel);

        for (n, line) in live_code(&src) {
            let mut hit = JOURNAL.iter().find(|m| line.contains(**m)).map(|m| m.to_string());
            if hit.is_none() && swift_or_kt && calls_android_log(&line) {
                hit = Some("Log.*(".to_string());
            }
            if hit.is_none() && !free_to_print {
                hit = STREAMS.iter().find(|m| line.contains(**m)).map(|m| m.to_string());
                if hit.is_none() && swift_or_kt && calls_bare_print(&line) {
                    hit = Some("print(".to_string());
                }
            }
            if let Some(m) = hit {
                guilty.push(format!("  {rel}:{n}: [{m}] {line}"));
            }
        }
    }

    assert!(
        guilty.is_empty(),
        "\nВ КЛИЕНТЕ ЗАВЕЛАСЬ ЗАПИСЬ В ЖУРНАЛ. BeMyVPN — VPN: запись о том, к кому \
         человек подключился и куда пошёл его трафик, у того, кто её ведёт, можно \
         потребовать. Поэтому журнала нет НИГДЕ и включить его нечем.\n\n{}\n\n\
         Если это ответ ЧЕЛОВЕКУ на его же команду (вывод `bemyvpn ...`, текст \
         ошибки в окне) — ему место в `apps/bmv-cli` или в строке состояния \
         интерфейса, а не в потоке вывода библиотеки. Если это отладка — она \
         живёт ровно до конца вашей отладки и в коммит не едет.\n\
         Правило и его список исключений: crates/bmv-common/tests/no_journal_in_the_client.rs\n",
        guilty.join("\n")
    );
}

/// Чужие форки правкой не удержать — там журнал выключен НА СБОРКЕ.
///
/// В `vendor/ipstack` десятки `log::debug!` про состояния TCP-сессий, и в них
/// адреса назначения — то есть трафик человека. Вычищать их руками пришлось бы
/// заново при каждом обновлении вверх по течению, поэтому вместо правок стоит
/// фича `max_level_off`: она ставит `STATIC_MAX_LEVEL` в `Off`, и макросы `log::`
/// не разворачиваются вовсе. Тест стережёт, чтобы её не потеряли при обновлении.
#[test]
fn the_vendored_forks_are_built_mute() {
    let root = repo_root();
    for fork in ["ipstack", "wintun"] {
        let path = root.join("vendor").join(fork).join("Cargo.toml");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("не читается {}: {e}", path.display()));
        // Строки `#` — комментарии, и в них слово `max_level_off` как раз
        // объясняет, зачем фича стоит. Искать в них — значит стеречь пояснение
        // вместо самой фичи: ровно так этот тест и прошёл в первый раз, когда
        // фичу убрали, а комментарий остался.
        let live = src.lines().filter(|l| !l.trim_start().starts_with('#')).collect::<Vec<_>>().join("\n");
        assert!(
            live.contains("max_level_off"),
            "\nФОРК vendor/{fork} СОБИРАЕТСЯ С ЖИВЫМ ЖУРНАЛОМ.\n\
             В {} у зависимости `log` пропала фича `max_level_off` — скорее всего, \
             при обновлении форка вверх по течению. Без неё макросы `log::` в чужом \
             коде разворачиваются и пишут состояния TCP-сессий вместе с адресами \
             назначения: это трафик человека.\n\
             Вернуть: [dependencies.log] → features = [\"max_level_off\"]\n",
            path.display()
        );
    }
}
