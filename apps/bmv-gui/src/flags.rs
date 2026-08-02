//! Флаг страны как КАРТИНКА (femtovg не рисует флаг-эмодзи из региональных
//! индикаторов — показывал «тофу»). PNG-набор (flags/, w40) встроен в бинарь;
//! по ISO-коду страны отдаём slint::Image. Вызывается на UI-потоке при обновлении
//! каталога, поэтому Image создаётся здесь же.
use include_dir::{include_dir, Dir};
use std::cell::RefCell;
use std::collections::HashMap;

static FLAGS: Dir = include_dir!("$CARGO_MANIFEST_DIR/flags");

thread_local! {
    /// Готовые картинки по коду страны.
    ///
    /// Без кеша PNG декодировался ЗАНОВО для КАЖДОЙ строки списка на КАЖДОМ
    /// обновлении каталога — а каталог живой, и обновление приходит сразу, как
    /// только у любого хоста сменился счётчик гостей. Всё это на потоке
    /// интерфейса, то есть за счёт плавности окна. `slint::Image` внутри —
    /// разделяемый буфер, клонировать её дёшево.
    ///
    /// thread_local, а не глобальный кеш под мьютексом: `slint::Image` не Send,
    /// да и зовётся это только с потока интерфейса.
    static CACHE: RefCell<HashMap<String, Option<slint::Image>>> = RefCell::new(HashMap::new());
}

/// Флаг по коду страны (ISO-2). None — нет такого флага.
pub fn flag(cc: &str) -> Option<slint::Image> {
    CACHE.with(|c| {
        c.borrow_mut()
            .entry(cc.to_ascii_lowercase())
            .or_insert_with_key(|key| decode(key))
            .clone()
    })
}

fn decode(lower_cc: &str) -> Option<slint::Image> {
    let file = FLAGS.get_file(format!("{lower_cc}.png"))?;
    let rgba = image::load_from_memory(file.contents()).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(rgba.as_raw(), w, h);
    Some(slint::Image::from_rgba8(buf))
}
