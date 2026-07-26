# Запуск: свой координатор и хосты

Всё делает **один файл** `bemyvpn` — ставить ничего не надо, скачал и запустил.

---

## 🚀 Что нужно, чтобы поднять проект с нуля

Минимум — **один VPS и одно доменное имя**. Всё остальное код делает сам.

| Что | Обязательно? | Зачем | Примечание |
|---|---|---|---|
| **VPS под координатор** | да | сводит гостей с хостами | 1 ядра и 512 МБ хватает: координатор ест ~7 МБ и ~0% CPU. Трафик через него **не идёт** |
| **Домен** | нет, но желательно | HTTPS-адрес вместо голого IP | без домена работает по HTTP на `IP:3330` |
| **Порт 443 открыт** | да, если домен | ACME сам берёт сертификат | без домена — свой порт, по умолчанию 3330 |
| **Хост(ы)** | да, хоть один | собственно раздают интернет | нужен **белый IP** (за NAT раздавать нельзя) |
| Сертификат Apple/Google | **нет** | — | всё раздаётся файлами, магазины не нужны |
| Своя база/Redis/nginx | **нет** | — | координатор — один бинарь, состояние в памяти |

### Если хотите свой координатор вместо общего

По умолчанию приложения ходят на `https://bemyvpn.net` — ничего настраивать не
нужно, всё работает из коробки. Свой адрес пригодится, если не хотите зависеть
от общего сервера: тогда поменяйте его в этих файлах и пересоберите:

| Файл | Что там |
|---|---|
| `crates/bmv-config/src/lib.rs` | значение по умолчанию для CLI/сервера |
| `apps/bmv-gui/src/main.rs` | десктопный GUI |
| `apps/bmv-cli/src/tui.rs` | меню терминала |
| `apps/android/app/src/main/assets/config.json` | Android |
| `apps/ios/BeMyVPN/Core.swift` | iOS (если будете собирать) |

Пользователь может вписать любой другой координатор прямо в меню — это лишь
значение по умолчанию.

### Секреты, которые НЕ лежат в репозитории

| Что | Где создаётся | Нужно ли хранить |
|---|---|---|
| `bmv-coordinator.secret` | координатор создаёт **сам** при первом запуске | да, рядом с бинарём: иначе у всех хостов сменятся коды |
| Сертификат TLS | ACME получает сам по домену | нет, кэшируется в `acme-cache/` |
| Apple Team ID | только локально (`DEVELOPMENT_TEAM`) | не коммитить |
| Android keystore | `apps/android/keystore.properties` (в `.gitignore`) | не коммитить |

### Конфиг

Отдельно писать не нужно — **меню сохраняет само** в `~/.config/bemyvpn/config.toml`.
Путь можно задать флагом `--config`. Правится руками только если хочется.

## Скачать (для сервера нужна «терминальная» версия)

| Что | Файл | |
|---|---|---|
| 🐧 Linux (обычный сервер) | [bemyvpn-linux-x86_64-terminal](https://github.com/mister-PARADISE/bemyvpn/releases/latest/download/bemyvpn-linux-x86_64-terminal) | ⏳ скоро в релизах |
| 🍎 macOS | [bemyvpn-macos-arm64-terminal](https://github.com/mister-PARADISE/bemyvpn/releases/latest/download/bemyvpn-macos-arm64-terminal) | ✅ |
| 🪟 Windows | [bemyvpn-windows-x86_64-terminal.exe](https://github.com/mister-PARADISE/bemyvpn/releases/latest/download/bemyvpn-windows-x86_64-terminal.exe) | ⏳ скоро |

Приложения с кнопками (Android/десктоп) — в [README](../README.md#-скачать).

---

## 📡 Хост — раздать интернет (без конфига)

Root не нужен. Одна команда (пример для Linux):
```bash
chmod +x bemyvpn-linux-x86_64-terminal
./bemyvpn-linux-x86_64-terminal --coordinator https://bemyvpn.net \
  host --tunnel --name "Мой сервер" --max 16
```
Флаги: `--tunnel` (реальная раздача), `--name`, `--max` (4/8/16/32/64/128),
`--password` (пусто = открытый), `--proto noise|obfs|plain`, `--hidden` (скрыть из списка).
Гости подключаются, выбрав твой хост в приложении.

**Фоном навсегда (systemd)** — `/etc/systemd/system/bemyvpn-host.service`:
```ini
[Unit]
Description=BeMyVPN host
After=network-online.target
[Service]
ExecStart=/opt/bemyvpn/bemyvpn --coordinator https://bemyvpn.net host --tunnel --name "Мой сервер" --max 16
Restart=always
[Install]
WantedBy=multi-user.target
```
```bash
systemctl enable --now bemyvpn-host
```

---

## 🗂 Свой координатор (сервер-каталог)

**Просто/локально** — HTTP на порту `3330` (для теста или по IP):
```bash
./bemyvpn server
```
Клиенты указывают `http://ТВОЙ_IP:3330`.

**Боевой (HTTPS, свой домен).** Нужно заранее: домен, DNS **A-запись** домена на IP сервера, открытый порт **443**. Мини-конфиг `coord.toml`:
```toml
[server]
bind       = "0.0.0.0:443"
domain     = "coord.твойдомен.com"    # сертификат выпустится сам (Let's Encrypt)
acme_email = "ты@почта.com"
```
```bash
./bemyvpn --config coord.toml server
```
Клиенты указывают `https://coord.твойдомен.com`.

**Фоном навсегда (systemd)** — `/etc/systemd/system/bemyvpn-coord.service`:
```ini
[Unit]
Description=BeMyVPN coordinator
After=network-online.target
[Service]
ExecStart=/opt/bemyvpn/bemyvpn --config /opt/bemyvpn/coord.toml server
Restart=always
WorkingDirectory=/opt/bemyvpn
[Install]
WantedBy=multi-user.target
```
```bash
systemctl enable --now bemyvpn-coord
```

---

## Проверка
```bash
./bemyvpn --coordinator https://coord.твойдомен.com ping    # → «жив ✅»
```

---

## ⚙️ Без конфигов — всё в меню (рекомендуемый путь)

Запусти просто `./bemyvpn` (без аргументов) — откроется меню. В нём:

- **Вкладка «Сервер»**: `D` — ввести домен своего координатора (HTTPS-сертификат
  Let's Encrypt **получится и будет продлеваться сам**, почта не нужна; требуется
  только DNS-запись домена на этот сервер и открытый порт 443), `B` — порт,
  `S` — старт/стоп, `A` — **автозапуск ВКЛ/ВЫКЛ** (при загрузке сервера).
- **Вкладка «Хост»**: имя/лимит/пароль/протокол/видимость — стрелками и Enter,
  `A` — автозапуск раздачи.
- **Все настройки сохраняются сами** в `~/.config/bemyvpn/config.toml` — руками
  файлы создавать/править не нужно. Код сети тоже сохраняется навсегда.

Автозапуск работает на Linux (systemd) и требует root: `sudo ./bemyvpn`.
Ручные `.toml` и systemd-юниты выше — для тех, кто хочет полный контроль.
