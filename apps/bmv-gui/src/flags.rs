//! Флаг страны как КАРТИНКА (femtovg не рисует флаг-эмодзи из региональных
//! индикаторов — показывал «тофу»). PNG-набор (flags/, w40) встроен в бинарь;
//! по ISO-коду страны отдаём slint::Image. Вызывается на UI-потоке при обновлении
//! каталога, поэтому Image создаётся здесь же.
use include_dir::{include_dir, Dir};

static FLAGS: Dir = include_dir!("$CARGO_MANIFEST_DIR/flags");

/// Флаг по коду страны (ISO-2). None — нет такого флага.
pub fn flag(cc: &str) -> Option<slint::Image> {
    let name = format!("{}.png", cc.to_ascii_lowercase());
    let file = FLAGS.get_file(&name)?;
    let rgba = image::load_from_memory(file.contents()).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(rgba.as_raw(), w, h);
    Some(slint::Image::from_rgba8(buf))
}
