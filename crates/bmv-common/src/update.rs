//! Проверка обновлений: разбор манифеста и КОНТРОЛЬ ПОДПИСИ.
//!
//! Автообновление означает, что приложение скачивает и запускает код. Наши
//! пользователи — люди, обходящие блокировки, то есть ровно та аудитория,
//! которую заинтересован атаковать серьёзный противник. Поэтому доверие здесь
//! строится НЕ на источнике загрузки (сервер могут подменить, трафик — перехватить),
//! а на подписи: манифест подписан ключом Ed25519, приватная часть которого
//! лежит офлайн у мейнтейнера, публичная — вшита ниже.
//!
//! Порядок доверия жёсткий и обратного хода не имеет:
//!   1. подпись манифеста сходится с вшитым ключом — иначе манифест выбрасываем;
//!   2. версия в манифесте НОВЕЕ нашей — иначе обновлять нечего;
//!   3. sha256 скачанного файла совпадает с манифестом — иначе файл выбрасываем.
//! Рабочий файл трогается ТОЛЬКО после всех трёх проверок.
//!
//! Ed25519 берём из `ring` — он уже в дереве зависимостей (через rustls),
//! отдельная крипто-библиотека ради этого не заводится.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{Error, Result};

/// Публичный ключ проверки обновлений (Ed25519, 32 байта hex).
///
/// Приватная часть НИКОГДА не попадает в репозиторий и не хранится в CI: ею
/// подписывают вручную, офлайн. Утечка приватного ключа = возможность раздать
/// произвольный код всем пользователям, поэтому смена ключа — это выпуск новой
/// версии приложения, а не правка на сервере.
pub const UPDATE_PUBKEY_HEX: &str = "ab0ad24adffdd6e2b6e17803dbf41565f07e5858e2a18b762fe229d406dcde18";

/// Манифест обновления — что вышло и с какими хэшами.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Версия релиза, «1.6».
    pub version: String,
    /// Ниже этой версии клиент больше не совместим с сетью и обязан обновиться.
    /// Позволяет честно сказать «дальше не поедет», а не молча отваливаться.
    #[serde(default)]
    pub min_supported: String,
    /// Ссылка на описание релиза (человеку — почитать, что изменилось).
    #[serde(default)]
    pub notes: String,
    /// Имя файла → sha256 в hex. Имена те же, что у ассетов релиза.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

impl Manifest {
    /// sha256 нужного файла из манифеста.
    pub fn sha256_of(&self, file: &str) -> Option<&str> {
        self.files.get(file).map(|s| s.as_str())
    }

    /// Новее ли этот релиз, чем текущая сборка.
    pub fn is_newer_than_current(&self) -> bool {
        crate::version::is_release_build() && crate::version::is_newer(&self.version, crate::version::VERSION)
    }

    /// Текущая сборка уже не поддерживается сетью?
    pub fn current_is_unsupported(&self) -> bool {
        !self.min_supported.is_empty()
            && crate::version::is_release_build()
            && crate::version::is_newer(&self.min_supported, crate::version::VERSION)
    }
}

/// Разобрать манифест, ПРЕДВАРИТЕЛЬНО проверив подпись.
///
/// `json` — точные байты файла манифеста (подпись считается по ним, поэтому
/// переформатировать или пересобирать JSON перед проверкой нельзя).
/// `sig_hex` — 64 байта подписи в hex.
pub fn verify_manifest(json: &[u8], sig_hex: &str) -> Result<Manifest> {
    let key = hex_decode(UPDATE_PUBKEY_HEX).ok_or_else(|| Error::Protocol("плохой вшитый ключ".into()))?;
    let sig = hex_decode(sig_hex.trim()).ok_or_else(|| Error::Protocol("подпись не hex".into()))?;

    let pk = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key);
    pk.verify(json, &sig)
        .map_err(|_| Error::Protocol("подпись манифеста НЕ сходится — обновление отклонено".into()))?;

    serde_json::from_slice(json).map_err(|e| Error::Protocol(format!("манифест не разобран: {e}")))
}

/// Проверить, что скачанный файл — тот самый (sha256 из манифеста).
pub fn verify_file_hash(bytes: &[u8], expected_sha256_hex: &str) -> bool {
    let got = ring::digest::digest(&ring::digest::SHA256, bytes);
    match hex_decode(expected_sha256_hex.trim()) {
        // Сравнение обычное, не постоянного времени: хэш публичен (лежит в
        // подписанном манифесте), утечки секрета по времени тут нет.
        Some(want) => got.as_ref() == want.as_slice(),
        None => false,
    }
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    /// Подписать тестовым ключом и подменить вшитый — иначе проверку не
    /// прогнать: настоящий приватный ключ в репозитории отсутствует по замыслу.
    fn signed_with_temp_key(json: &[u8]) -> (String, String) {
        let rng = ring::rand::SystemRandom::new();
        let doc = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap();
        let sig = kp.sign(json);
        let hexify = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        (hexify(kp.public_key().as_ref()), hexify(sig.as_ref()))
    }

    fn verify_with(json: &[u8], sig_hex: &str, pubkey_hex: &str) -> bool {
        let key = hex_decode(pubkey_hex).unwrap();
        let sig = hex_decode(sig_hex).unwrap();
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
            .verify(json, &sig)
            .is_ok()
    }

    const MANIFEST: &str = r#"{"version":"1.6","min_supported":"1.2","notes":"https://example.org/n","files":{"bemyvpn-linux-x86_64-terminal":"aa"}}"#;

    #[test]
    fn tampered_manifest_is_rejected() {
        let (pk, sig) = signed_with_temp_key(MANIFEST.as_bytes());
        assert!(verify_with(MANIFEST.as_bytes(), &sig, &pk), "честный манифест должен проходить");

        // ГЛАВНОЕ свойство: подменили хоть байт — подпись обязана развалиться.
        // Без этого автообновление становится каналом доставки чужого кода.
        let evil = MANIFEST.replace("1.6", "9.9");
        assert!(!verify_with(evil.as_bytes(), &sig, &pk), "подменённый манифест ОБЯЗАН быть отвергнут");

        // Чужой ключ тоже не должен подходить.
        let (other_pk, _) = signed_with_temp_key(b"other");
        assert!(!verify_with(MANIFEST.as_bytes(), &sig, &other_pk), "чужой ключ не должен подходить");
    }

    #[test]
    fn real_pubkey_rejects_garbage_signature() {
        // Вшитый ключ обязан быть валидным и не принимать мусор.
        let bad = "00".repeat(64);
        assert!(verify_manifest(MANIFEST.as_bytes(), &bad).is_err());
    }

    #[test]
    fn file_hash_check() {
        let data = b"hello";
        let sha = ring::digest::digest(&ring::digest::SHA256, data);
        let hex: String = sha.as_ref().iter().map(|x| format!("{x:02x}")).collect();
        assert!(verify_file_hash(data, &hex));
        assert!(!verify_file_hash(b"hell0", &hex), "изменённый файл не должен проходить");
        assert!(!verify_file_hash(data, "не-hex"));
    }

    #[test]
    fn manifest_fields_parse() {
        let (pk, sig) = signed_with_temp_key(MANIFEST.as_bytes());
        assert!(verify_with(MANIFEST.as_bytes(), &sig, &pk));
        let m: Manifest = serde_json::from_slice(MANIFEST.as_bytes()).unwrap();
        assert_eq!(m.version, "1.6");
        assert_eq!(m.min_supported, "1.2");
        assert_eq!(m.sha256_of("bemyvpn-linux-x86_64-terminal"), Some("aa"));
        assert_eq!(m.sha256_of("нет-такого"), None);
    }
}
