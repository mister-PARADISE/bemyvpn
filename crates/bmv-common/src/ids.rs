//! Идентификаторы: id хоста и rid слота гостя.
//!
//! Алфавит и длина как в прототипе — длинные, без двусмысленных символов, чтобы
//! при большом числе людей коды не совпадали и читались голосом.

use rand::Rng;

const ID_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Короткий стабильный id хоста (для каталога).
pub fn new_host_id(len: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..len.max(4))
        .map(|_| ID_ALPHABET[rng.gen_range(0..ID_ALPHABET.len())] as char)
        .collect()
}
