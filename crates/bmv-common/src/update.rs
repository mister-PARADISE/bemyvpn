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
//!
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
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

// ── скачивание и установка ───────────────────────────────────────────────────

/// Имя файла релиза для ЭТОЙ сборки — по нему ищем хэш в манифесте и строим
/// адрес загрузки. Имена совпадают с ассетами релиза (см. release.yml).
pub fn current_asset_name(is_gui: bool) -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH, is_gui) {
        ("linux", "x86_64", false) => "bemyvpn-linux-x86_64-terminal",
        ("linux", "x86_64", true) => "bemyvpn-linux-x86_64.AppImage",
        ("macos", "aarch64", false) => "bemyvpn-macos-arm64-terminal",
        ("macos", "aarch64", true) => "bemyvpn-macos-arm64.dmg",
        ("windows", "x86_64", false) => "bemyvpn-windows-x86_64-terminal.exe",
        ("windows", "x86_64", true) => "bemyvpn-windows-x86_64.exe",
        // Сборка под платформу, которую мы не выпускаем (например linux-aarch64):
        // обновлять нечем, и честнее сказать «нет», чем подсунуть чужой файл.
        _ => return None,
    })
}

/// Прямая ссылка на файл последнего релиза.
pub fn asset_url(repo: &str, tag: &str, asset: &str) -> String {
    format!("https://github.com/{repo}/releases/download/{tag}/{asset}")
}

/// Скачать по HTTPS, следуя перенаправлениям (GitHub уводит на свой CDN).
///
/// Ограничение размера обязательно: без него подменённый ответ мог бы занять всю
/// память. Наши файлы — десятки мегабайт, потолок берём с большим запасом.
pub async fn download(url: &str, max_bytes: usize) -> Result<Vec<u8>> {
    use http_body_util::BodyExt;

    let tls = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_only() // только HTTPS: по http подсунуть подмену куда проще
        .enable_http1()
        .build();
    let client: hyper_util::client::legacy::Client<_, String> =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build(tls);

    let mut url = url.to_string();
    // До 5 перенаправлений: GitHub отдаёт 302 на objects.githubusercontent.com.
    for _ in 0..5 {
        let req = hyper::Request::get(&url)
            .header(hyper::header::USER_AGENT, "bemyvpn")
            .body(String::new())
            .map_err(|e| Error::Net(format!("запрос: {e}")))?;
        let resp = client.request(req).await.map_err(|e| Error::Net(format!("загрузка: {e}")))?;

        if resp.status().is_redirection() {
            let loc = resp
                .headers()
                .get(hyper::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| Error::Net("перенаправление без адреса".into()))?;
            url = loc.to_string();
            continue;
        }
        if !resp.status().is_success() {
            return Err(Error::Net(format!("сервер ответил {}", resp.status())));
        }

        let body = resp.into_body().collect().await.map_err(|e| Error::Net(format!("тело: {e}")))?;
        let bytes = body.to_bytes();
        if bytes.len() > max_bytes {
            return Err(Error::Net("файл больше ожидаемого — отклонён".into()));
        }
        return Ok(bytes.to_vec());
    }
    Err(Error::Net("слишком много перенаправлений".into()))
}

/// Потолок размера скачиваемого файла. Самый большой наш ассет — Windows-GUI
/// (~19 МБ); 64 МБ дают запас на рост и при этом не дают залить память.
pub const MAX_ASSET_BYTES: usize = 64 * 1024 * 1024;

/// Заменить СВОЙ исполняемый файл скачанным и вернуть путь к резервной копии.
///
/// Порядок важен: сначала кладём новый файл РЯДОМ, потом переименованиями
/// меняем местами. `rename` в пределах одной папки атомарен, поэтому на диске
/// никогда не оказывается наполовину записанного бинаря — оборвись питание в
/// любой момент, останется либо старый файл, либо новый, но не огрызок.
///
/// Работающий процесс при этом не страдает: на Unix он держит СТАРЫЙ inode и
/// продолжает выполняться до перезапуска.
#[cfg(unix)]
pub fn replace_self(new_bytes: &[u8]) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let me = std::env::current_exe().map_err(|e| Error::Net(format!("свой путь: {e}")))?;
    let dir = me.parent().ok_or_else(|| Error::Net("нет родительской папки".into()))?;
    let tmp = dir.join(format!(".bemyvpn-new-{}", std::process::id()));
    let bak = me.with_extension("bak");

    std::fs::write(&tmp, new_bytes).map_err(|e| Error::Net(format!("запись: {e}")))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| Error::Net(format!("права: {e}")))?;

    // Старый — в .bak (откат), новый — на его место.
    let _ = std::fs::remove_file(&bak);
    std::fs::rename(&me, &bak).map_err(|e| Error::Net(format!("резервная копия: {e}")))?;
    if let Err(e) = std::fs::rename(&tmp, &me) {
        // Подмена не удалась — возвращаем старый файл, иначе останемся без бинаря.
        let _ = std::fs::rename(&bak, &me);
        return Err(Error::Net(format!("подмена: {e}")));
    }
    Ok(bak)
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
    fn asset_name_and_url() {
        // Под платформу, на которой идут тесты, имя обязано находиться —
        // иначе обновление молча стало бы недоступно на своей же системе.
        let term = current_asset_name(false).expect("нет имени терминального файла");
        assert!(term.starts_with("bemyvpn-"), "имя не по паттерну: {term}");
        let gui = current_asset_name(true).expect("нет имени приложения");
        assert!(gui.starts_with("bemyvpn-"), "имя не по паттерну: {gui}");
        assert_ne!(term, gui, "терминал и приложение — разные файлы");

        let url = asset_url("owner/repo", "v1.6", term);
        assert_eq!(url, format!("https://github.com/owner/repo/releases/download/v1.6/{term}"));
        assert!(url.starts_with("https://"), "только HTTPS");
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

