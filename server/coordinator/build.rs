//! Готовит страницу скачивания К СБОРКЕ, чтобы в момент запроса работы не было.
//!
//! Делает две вещи, и обе — ровно один раз, при компиляции:
//!   • сжимает `site/index.html` в gzip (≈19 КБ → ≈5 КБ) и кладёт рядом с
//!     объектниками — координатор подхватит готовое через `include_bytes!`;
//!   • считает SHA-256 содержимого и отдаёт его в код через переменную окружения
//!     — это ETag, и он тоже не должен считаться на каждый запрос.
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
    let html = std::fs::read(std::path::Path::new(&dir).join(PAGE))
        .unwrap_or_else(|e| panic!("не читается {PAGE}: {e}"));

    // ETag. Восьми байт хеша (16 hex) с запасом хватает: задача метки — меняться
    // вместе с содержимым, а не сопротивляться подбору.
    let sum = <sha2::Sha256 as sha2::Digest>::digest(&html);
    let etag: String = sum[..8].iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=BMV_PAGE_ETAG={etag}");

    // `Compression::best` — жмём на сборке, поэтому лишние миллисекунды здесь
    // ничего не стоят, а сэкономленные байты уходят каждому посетителю.
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(&html).expect("запись в память не падает");
    let gz = enc.finish().expect("gzip не падает на корректном входе");

    let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR задаёт cargo"))
        .join("index.html.gz");
    std::fs::write(&out, gz).unwrap_or_else(|e| panic!("не пишется {}: {e}", out.display()));
}
