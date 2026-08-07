//! Логотип на странице обязан совпадать с бренд-каноном.
//!
//! Зачем: на странице скачивания однажды нарисовали СВОЮ, упрощённую сцепку
//! колец — поверх мятного кольца легла синяя дуга, и часть мятного кольца
//! оказалась чужого цвета. Заметил это владелец, а не проверка: разметка была
//! синтаксически безупречна, просто рисовала не тот знак.
//!
//! Поэтому сверяем не «есть ли логотип», а геометрию и краску колец с
//! `brand/logo.svg` — единственным источником. Разойдутся — задача краснеет.

use std::path::Path;

/// Значение атрибута `name="…"` внутри одного тега.
fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = start + tag[start..].find('"')?;
    Some(tag[start..end].to_string())
}

/// Кольца из SVG: (cx, cy, r, краска, толщина) в порядке отрисовки.
///
/// Разбираем сами, без крейта разбора XML: это четыре тега в двух файлах,
/// которые мы же и пишем — зависимость обошлась бы дороже самой проверки.
fn rings(svg: &str) -> Vec<(String, String, String, String, String)> {
    svg.match_indices("<circle")
        .filter_map(|(i, _)| {
            let tag = &svg[i..i + svg[i..].find('>')?];
            Some((
                attr(tag, "cx")?,
                attr(tag, "cy")?,
                attr(tag, "r")?,
                attr(tag, "stroke")?,
                attr(tag, "stroke-width")?,
            ))
        })
        .collect()
}

#[test]
fn the_logo_on_the_page_is_the_brand_one() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let canon = std::fs::read_to_string(root.join("brand/logo.svg")).expect("brand/logo.svg");
    let page = std::fs::read_to_string(root.join("site/index.html")).expect("site/index.html");

    let want = rings(&canon);
    let got = rings(&page);

    assert!(
        want.len() >= 4,
        "в brand/logo.svg нашлось {} колец вместо четырёх — сломался разбор, а не логотип",
        want.len()
    );
    assert_eq!(
        want, got,
        "\nЛОГОТИП НА СТРАНИЦЕ РАЗОШЁЛСЯ С БРЕНДОМ (brand/logo.svg).\n\
         Кольца должны совпадать один в один: те же центры, радиусы, краска и толщина,\n\
         в том же порядке — порядок и задаёт сцепку.\n\
         Своя, упрощённая сцепка уже приводила к тому, что часть мятного кольца\n\
         рисовалась синим: знак получался неправильный, а разметка при этом верной."
    );
}
