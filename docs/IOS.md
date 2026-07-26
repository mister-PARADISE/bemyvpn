# iOS: собрать и поставить себе на устройство

iOS-приложение **не выкладывается в релизы** и не собирается в CI — Apple не даёт
установить VPN-приложение файлом, как APK или DMG. Поставить его можно только
собрав самому, и для этого нужен **платный аккаунт Apple Developer Program**
(бесплатный не даёт нужного права `packet-tunnel-provider`).

Код лежит в репозитории целиком: `apps/ios/`. Собрать может любой, у кого есть
свой аккаунт разработчика — идентификатор команды в репозитории не хранится.

## Что понадобится

| | |
|---|---|
| Mac с **Xcode** | не Command Line Tools: `sudo xcode-select -s /Applications/Xcode.app/Contents/Developer` |
| Аккаунт **Apple Developer Program** | платный; нужен для NetworkExtension |
| Ваш **Team ID** | Apple Developer → Membership details, вид `A1B2C3D4E5` |

## 1. Собрать ядро

Rust-ядро подключается как `BmvFFI.xcframework` — сначала собираем его:

```bash
bash apps/ios/build-xcframework.sh
```

Скрипт сам доставит нужные таргеты Rust и соберёт под устройство и симулятор.

## 2. Собрать приложение

Идентификатор команды **не коммитится** — он публично привязан к владельцу
аккаунта. Передайте его сборке:

```bash
xcodebuild -project apps/ios/BeMyVPN.xcodeproj -scheme BeMyVPN -configuration Release DEVELOPMENT_TEAM=ВАШ_TEAM_ID
```

Или откройте `apps/ios/BeMyVPN.xcodeproj` в Xcode и выберите свою команду в
**Signing & Capabilities** у обоих таргетов (`BeMyVPN` и `BeMyVPNTunnel`).
Xcode запишет её в файл проекта — **не коммитьте это изменение**.

## 3. Поставить на устройство

Подключите iPhone кабелем, выберите его как цель и нажмите ▶ (Run). Приложение
установится и будет работать; профиль разработчика живёт год, потом пересобрать.

При первом запуске iOS спросит разрешение на добавление VPN-конфигурации — это
обязательно, туннель без него не поднимется.

## Почему bundle id может потребовать смены

В проекте `org.bemyvpn.app` и `org.bemyvpn.app.tunnel`. Если этот идентификатор
уже занят в вашем аккаунте или Xcode ругается на профиль — поменяйте префикс на
свой (`project.yml` → `bundleIdPrefix`, затем `xcodegen`, либо прямо в Xcode).

## Чего здесь нет и не будет

**App Store не рассматривается.** Публикация VPN требует организационного
аккаунта (Guideline 5.4), а он привязан к юрлицу. Проект раздаётся файлами:
Android — APK, macOS — DMG, Windows — exe, Linux — AppImage. iOS остаётся
«собери себе сам».
