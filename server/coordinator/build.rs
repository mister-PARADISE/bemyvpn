//! Готовит страницу скачивания К СБОРКЕ, чтобы в момент запроса работы не было.
//!
//! Делает три вещи, и все — ровно один раз, при компиляции:
//!   • ужимает `site/index.html`: выкидывает пояснения, отступы и пустые строки;
//!   • сжимает ужатое в gzip и кладёт рядом с объектниками — координатор
//!     подхватит готовое через `include_bytes!`;
//!   • считает SHA-256 содержимого и отдаёт его в код через переменную окружения
//!     — это ETag, и он тоже не должен считаться на каждый запрос.
//!
//! ПОЧЕМУ УЖИМАЕМ ЗДЕСЬ, А НЕ В ИСХОДНИКЕ. В репозитории `site/index.html`
//! остаётся с пояснениями: они объясняют, почему страница устроена именно так,
//! и нужны тому, кто её правит. Посетителю они не нужны — он их не увидит, но
//! заплатит за них трафиком. Поэтому исходник читаемый, а в бинарь едет сухая
//! копия. Ужатие стоит ПЕРЕД gzip: жмётся уже подсушенное.
//!
//! Почему сжатие здесь, а не слоем `tower-http`: содержимое известно заранее и
//! не меняется, значит жать его на каждый запрос — платить процессором сервера
//! за один и тот же ответ. `CompressionLayer` вдобавок тянет в отдельно
//! собираемый бинарь целую новую ветку зависимостей; здесь же `flate2` — только
//! build-dependency, в сам бинарь она не попадает.

use std::io::Write;

/// Путь к странице от каталога крейта. Страница живёт в `site/`, а не внутри
/// координатора: это ресурс сайта, у него своя жизнь и свой автор.
const PAGE: &str = "../../site/index.html";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={PAGE}");

    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR задаёт cargo");
    let src = std::fs::read_to_string(std::path::Path::new(&dir).join(PAGE))
        .unwrap_or_else(|e| panic!("не читается {PAGE}: {e}"));

    let html = squeeze(&src);

    let out_dir = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR задаёт cargo"))
        .to_path_buf();

    // Ужатую кладём файлом: её же вшивает в бинарь `include_str!` и с ней же
    // сверяются тесты — второй раз ужимать в другом месте значило бы завести
    // второй ужиматель.
    let plain = out_dir.join("index.html");
    std::fs::write(&plain, html.as_bytes())
        .unwrap_or_else(|e| panic!("не пишется {}: {e}", plain.display()));

    // ETag. Восьми байт хеша (16 hex) с запасом хватает: задача метки — меняться
    // вместе с содержимым, а не сопротивляться подбору.
    let sum = <sha2::Sha256 as sha2::Digest>::digest(html.as_bytes());
    let etag: String = sum[..8].iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=BMV_PAGE_ETAG={etag}");

    // `Compression::best` — жмём на сборке, поэтому лишние миллисекунды здесь
    // ничего не стоят, а сэкономленные байты уходят каждому посетителю.
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(html.as_bytes()).expect("запись в память не падает");
    let gz = enc.finish().expect("gzip не падает на корректном входе");

    let out = out_dir.join("index.html.gz");
    std::fs::write(&out, gz).unwrap_or_else(|e| panic!("не пишется {}: {e}", out.display()));
}

// ── ужиматель ────────────────────────────────────────────────────────────────

/// В какой части файла мы сейчас: у разметки, стилей и скрипта пояснения
/// пишутся по-разному, и путать их нельзя.
enum Zone {
    Html,
    Style,
    Script,
}

/// Ужать страницу.
///
/// ЧТО УБИРАЕМ: пояснения (`<!-- … -->` в разметке, `/* … */` в стилях, `//` в
/// начале строки скрипта), ведущие отступы, хвостовые пробелы, пустые строки.
///
/// ЧЕГО НЕ ТРОГАЕМ: имена классов, идентификаторов, свойств и ключевых кадров
/// (переименование в «a», «b», «c» — не ужатие, а порча читаемости ради байтов,
/// которые gzip и так съедает); любой видимый человеку текст; содержимое
/// `<pre>`. Переводы строк остаются на месте: в разметке перенос — это пробел,
/// а в скрипте на нём держится автоподстановка точки с запятой.
///
/// ДВЕ ЛОВУШКИ, ради которых разбор построчный, а не одной заменой:
///   • в скрипте лежат адреса вида `"https://github.com/…"`. Наивное удаление
///     всего после `//` съело бы половину адреса и оставило незакрытую строку —
///     страница молча перестала бы работать. Поэтому `//` считается пояснением
///     ТОЛЬКО в начале строки (после отступа), а внутри строки не значит ничего;
///   • в `<pre>` стоят `&amp;&amp;` и `$(uname -m | sed s/aarch64/arm64/)`. Это
///     команда, которую человек копирует на сервер: любое схлопывание пробелов
///     там её ломает. Поэтому от `<pre` и до `</pre>` строка уходит как есть.
fn squeeze(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut zone = Zone::Html;
    // Пояснение, не закрывшееся на своей строке, тянется на следующие.
    let mut inside_comment = false;
    let mut inside_pre = false;

    for raw in html.lines() {
        // Внутри `<pre>` — байт в байт, никакой подрезки.
        if inside_pre {
            out.push_str(raw);
            out.push('\n');
            inside_pre = !raw.contains("</pre>");
            continue;
        }

        // Строка, открывающая `<pre>`: подрезать можно только слева от тега.
        if !inside_comment {
            if let Some(at) = raw.find("<pre") {
                let tail = &raw[at..];
                out.push_str(raw[..at].trim_start());
                out.push_str(tail);
                out.push('\n');
                inside_pre = !tail.contains("</pre>");
                continue;
            }
        }

        let trimmed = raw.trim();

        // Границы зон. Теги стоят на своих строках — так их и ищем.
        match trimmed {
            "<style>" => zone = Zone::Style,
            "</style>" => zone = Zone::Html,
            "<script>" => zone = Zone::Script,
            "</script>" => zone = Zone::Html,
            _ => {}
        }

        let line = match zone {
            Zone::Html => cut(trimmed, "<!--", "-->", &mut inside_comment),
            Zone::Style => cut(trimmed, "/*", "*/", &mut inside_comment),
            // ЛОВУШКА С АДРЕСАМИ: пояснением считается только целая строка.
            Zone::Script if trimmed.starts_with("//") => String::new(),
            Zone::Script => trimmed.to_string(),
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Вырезать из строки пояснения `open … close`, помня незакрытое через `inside`.
fn cut(line: &str, open: &str, close: &str, inside: &mut bool) -> String {
    let mut res = String::new();
    let mut rest = line;
    loop {
        if *inside {
            match rest.find(close) {
                Some(i) => {
                    rest = &rest[i + close.len()..];
                    *inside = false;
                }
                None => return res,
            }
        } else {
            match rest.find(open) {
                Some(i) => {
                    res.push_str(&rest[..i]);
                    rest = &rest[i + open.len()..];
                    *inside = true;
                }
                None => {
                    res.push_str(rest);
                    return res;
                }
            }
        }
    }
}
