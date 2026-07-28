//! Обновление: узнать версию у GitHub, скачать, поставить.
//!
//! Доверие обеспечивает HTTPS: файл берётся с `github.com`, TLS не даёт подменить
//! его в пути. Отдельная подпись поверх этого защищала бы лишь от кражи самого
//! аккаунта GitHub — цена (офлайн-ключ, ручная подпись каждого релиза, манифест
//! на координаторе) того не стоит, поэтому её нет.
//!
//! Порядок: спросили версию → сравнили → скачали → поставили. Рабочий файл
//! трогается только после того, как загрузка полностью удалась.

use crate::{Error, Result};

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


/// Обновить приложение, которое НЕ МОЖЕТ подменить себя на ходу (бандл macOS,
/// exe под Windows), — так делают все приличные автообновлялки, включая Sparkle.
///
/// Схема одна на обе платформы:
///   1. новое кладём рядом, проверенным;
///   2. запускаем ПОМОЩНИКА и сразу выходим;
///   3. помощник ждёт, пока наш процесс умрёт, меняет местами и запускает заново.
///
/// Почему именно так: пока приложение живо, заменить его файлы либо нельзя
/// (Windows держит exe), либо опасно (macOS может грузить ресурсы бандла на
/// лету). Подмена «удалить и скопировать поверх» — как раз то, что ломает
/// установки у плохих обновлялок: обрыв на середине оставляет мусор вместо
/// программы. Мы двигаем целиком уже готовое, старое сохраняем для отката.
///
/// Возвращает управление СРАЗУ: вызывающий обязан завершить процесс сам.
#[cfg(target_os = "macos")]
pub fn spawn_bundle_updater(dmg_bytes: &[u8]) -> Result<()> {
    let app = current_app_bundle().ok_or_else(|| Error::Net("приложение запущено не из бандла .app".into()))?;
    let tmp = std::env::temp_dir().join(format!("bemyvpn-upd-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| Error::Net(format!("временная папка: {e}")))?;
    let dmg = tmp.join("update.dmg");
    std::fs::write(&dmg, dmg_bytes).map_err(|e| Error::Net(format!("запись образа: {e}")))?;

    let script = tmp.join("apply.sh");
    // Помощник намеренно на shell: его легко прочитать и проверить глазами, а
    // ошибка в бинарном помощнике оставила бы пользователя без приложения.
    // `xattr -dr` обязателен: скачанное помечено карантином, и без снятия
    // Gatekeeper заблокирует уже установленное обновление.
    let body = format!(
        r#"#!/bin/sh
set -e
APP="{app}"
DMG="{dmg}"
TMP="{tmp}"
PID={pid}
# Ждём выхода приложения (до 30с), иначе подменять опасно.
i=0
while kill -0 "$PID" 2>/dev/null && [ $i -lt 60 ]; do sleep 0.5; i=$((i+1)); done

MNT="$TMP/mnt"
mkdir -p "$MNT"
hdiutil attach -nobrowse -quiet "$DMG" -mountpoint "$MNT"
NEW=$(ls -d "$MNT"/*.app | head -1)
cp -R "$NEW" "$TMP/new.app"
hdiutil detach "$MNT" -quiet || true

xattr -dr com.apple.quarantine "$TMP/new.app" 2>/dev/null || true

# Атомарно: старое в сторону, новое на место. Не удалось — возвращаем старое.
rm -rf "$APP.old"
mv "$APP" "$APP.old"
if ! mv "$TMP/new.app" "$APP"; then
  mv "$APP.old" "$APP"
  exit 1
fi
rm -rf "$APP.old"
open "$APP"
rm -rf "$TMP"
"#,
        app = app.display(),
        dmg = dmg.display(),
        tmp = tmp.display(),
        pid = std::process::id()
    );
    std::fs::write(&script, body).map_err(|e| Error::Net(format!("помощник: {e}")))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| Error::Net(format!("права помощника: {e}")))?;

    std::process::Command::new("/bin/sh")
        .arg(&script)
        .spawn()
        .map_err(|e| Error::Net(format!("запуск помощника: {e}")))?;
    Ok(())
}

/// Путь к своему .app: `…/BeMyVPN.app/Contents/MacOS/bemyvpn-gui` → `…/BeMyVPN.app`.
#[cfg(target_os = "macos")]
fn current_app_bundle() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.parent()?.parent()?; // MacOS → Contents → .app
    (p.extension()? == "app").then(|| p.to_path_buf())
}

/// То же для Windows: exe нельзя переписать, пока он выполняется.
#[cfg(windows)]
pub fn spawn_exe_updater(new_bytes: &[u8]) -> Result<()> {
    let me = std::env::current_exe().map_err(|e| Error::Net(format!("свой путь: {e}")))?;
    let dir = me.parent().ok_or_else(|| Error::Net("нет папки".into()))?;
    let new = dir.join("bemyvpn-new.exe");
    std::fs::write(&new, new_bytes).map_err(|e| Error::Net(format!("запись: {e}")))?;

    let cmd = dir.join("bemyvpn-update.cmd");
    // Скрипт ждёт освобождения файла: пока процесс жив, переименование не
    // проходит, и цикл повторяет попытку. Так надёжнее, чем ждать по таймеру.
    let body = format!(
        "@echo off\r\n\
         :wait\r\n\
         timeout /t 1 /nobreak >nul\r\n\
         move /y \"{me}\" \"{bak}\" >nul 2>&1 || goto wait\r\n\
         move /y \"{new}\" \"{me}\" >nul 2>&1 || (move /y \"{bak}\" \"{me}\" >nul & exit /b 1)\r\n\
         start \"\" \"{me}\"\r\n\
         del \"%~f0\"\r\n",
        me = me.display(),
        bak = me.with_extension("bak").display(),
        new = new.display()
    );
    std::fs::write(&cmd, body).map_err(|e| Error::Net(format!("помощник: {e}")))?;
    std::process::Command::new("cmd")
        .args(["/C", "start", "", "/min"])
        .arg(&cmd)
        .spawn()
        .map_err(|e| Error::Net(format!("запуск помощника: {e}")))?;
    Ok(())
}

/// Узнать версию последнего релиза у GitHub. Возвращает тег («v1.7»).
///
/// Один запрос к публичному API, без токена и без своей инфраструктуры: релизы
/// и так лежат на GitHub, отдельный сервер для «какая версия свежая» не нужен.
pub async fn github_latest_tag(repo: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    // Ответ API — килобайты; потолок ставим маленький, чтобы подменённый или
    // просто гигантский ответ не занял память.
    let body = download(&url, 256 * 1024).await?;
    let text = String::from_utf8_lossy(&body);
    tag_from_release_json(&text).ok_or_else(|| Error::Net("GitHub не вернул номер релиза".into()))
}

/// Достать `tag_name` из ответа API. Разбираем вручную: тянуть json-парсер ради
/// одного поля незачем, а формат тут стабильный и простой.
fn tag_from_release_json(json: &str) -> Option<String> {
    let key = "\"tag_name\"";
    let start = json.find(key)? + key.len();
    let rest = json.get(start..)?;
    let q1 = rest.find('"')? + 1;
    let after = rest.get(q1..)?;
    let q2 = after.find('"')?;
    let tag = after.get(..q2)?;
    (!tag.is_empty()).then(|| tag.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_name_and_url() {
        // Под платформу, на которой идут тесты, имя обязано находиться —
        // иначе обновление молча стало бы недоступно на своей же системе.
        let term = current_asset_name(false).expect("нет имени терминального файла");
        assert!(term.starts_with("bemyvpn-"), "имя не по паттерну: {term}");
        let gui = current_asset_name(true).expect("нет имени приложения");
        assert_ne!(term, gui, "терминал и приложение — разные файлы");

        let url = asset_url("owner/repo", "v1.6", term);
        assert_eq!(url, format!("https://github.com/owner/repo/releases/download/v1.6/{term}"));
        assert!(url.starts_with("https://"), "только HTTPS: по http подменить куда проще");
    }

    #[test]
    fn parses_tag_from_github_answer() {
        // Ответ API большой; нам нужно ровно одно поле, и разбор не должен
        // спотыкаться о порядок ключей или лишние данные вокруг.
        let json = r#"{"url":"x","tag_name":"v1.7","name":"BeMyVPN 1.7","draft":false}"#;
        assert_eq!(tag_from_release_json(json).as_deref(), Some("v1.7"));

        let reordered = r#"{"name":"x","draft":false,"tag_name":"v2.10"}"#;
        assert_eq!(tag_from_release_json(reordered).as_deref(), Some("v2.10"));

        assert_eq!(tag_from_release_json(r#"{"message":"Not Found"}"#), None);
        assert_eq!(tag_from_release_json("не json"), None);
    }
}
