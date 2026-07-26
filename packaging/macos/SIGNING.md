# Подпись и нотаризация macOS (когда купишь Apple Developer)

Сейчас `.dmg` собирается **ад-хок** — открывается правым кликом → «Открыть».
Чтобы был обычный двойной клик без предупреждений, нужен Apple Developer ($99/год)
и два шага: **подпись** сертификатом Developer ID + **нотаризация** у Apple.
Код к этому уже готов: `build-app.sh` сам включит настоящую подпись, если увидит
переменные окружения / секреты. Тебе — только завести их.

## 1. Что получить в аккаунте Apple Developer
- **Сертификат Developer ID Application** (Keychain → экспорт в `.p12` с паролем).
  Строка identity выглядит как `Developer ID Application: Имя Фамилия (TEAMID)`.
- **App Store Connect API-ключ** для нотаризации: Users and Access → Integrations →
  App Store Connect API → создать ключ (роль Developer). Скачается `AuthKey_XXXX.p8`.
  Запиши **Key ID** и **Issuer ID**.

## 2. Секреты GitHub (Settings → Secrets and variables → Actions)
| Секрет | Что положить |
|---|---|
| `MACOS_SIGN_ID` | `Developer ID Application: Имя (TEAMID)` |
| `MACOS_CERTIFICATE` | `base64 -i cert.p12` (весь вывод) |
| `MACOS_CERTIFICATE_PWD` | пароль от `.p12` |
| `AC_API_KEY_ID` | Key ID API-ключа |
| `AC_API_ISSUER_ID` | Issuer ID |
| `AC_API_KEY_B64` | `base64 -i AuthKey_XXXX.p8` |

Появится `MACOS_SIGN_ID` → CI сам импортирует сертификат, подпишет с hardened
runtime и (если заданы `AC_*`) нотаризует + застейплит. Ничего в коде менять не надо.

## 3. Локальная сборка с подписью (по желанию)
```bash
export MACOS_SIGN_ID="Developer ID Application: Имя (TEAMID)"
# нотаризация — профиль notarytool (разово):
xcrun notarytool store-credentials bemyvpn-notary \
  --key AuthKey_XXXX.p8 --key-id KEYID --issuer ISSUERID
export MACOS_NOTARY_PROFILE=bemyvpn-notary
VERSION=0.56 bash packaging/macos/build-app.sh
```

## iOS (на будущее)
iOS-приложение — отдельный тонкий Swift-шелл поверх того же Rust-ядра (как Android
на Kotlin). Ядро (`crates/*`) переиспользуется как статическая библиотека
(`cargo build --target aarch64-apple-ios`), рисуется нативный UI. Подпись iOS —
через тот же Apple Developer аккаунт (provisioning profile + Xcode). Это отдельная
задача, не входит в текущую десктоп-сборку.
