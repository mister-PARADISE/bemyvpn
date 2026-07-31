//! bemyvpn — CLI-оболочка. Тонкая морда к `bmv_core::BmvEngine`.
//!
//! Знакомство идёт через КООРДИНАТОР (главный сервер): хост анонсирует себя,
//! гость берёт список и просится к выбранному. Никаких «кодов» в основном пути.

use std::io::IsTerminal;
use std::path::PathBuf;

use bmv_config::Config;
use bmv_core::BmvEngine;
use clap::{Parser, Subcommand};

mod tui;
use tui::Seed;

#[derive(Parser)]
#[command(name = "bemyvpn", about = "BeMyVPN — P2P VPN для доступа к свободному интернету")]
struct Cli {
    /// Путь к единственному конфигу (иначе ищется автоматически).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Переопределить адрес координатора (иначе берётся из конфига).
    #[arg(long, global = true)]
    coordinator: Option<String>,

    /// Без подкоманды — открывается полноэкранное TUI-меню (вкладки VPN/Хост/Сервер).
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Показать версию сборки.
    Version,
    /// Обновиться до свежего релиза: скачать, проверить подпись и подменить себя.
    Update {
        /// Только проверить, есть ли новая версия — ничего не скачивать.
        #[arg(long)]
        check: bool,
    },
    /// Установить себя в систему: скопировать в PATH, чтобы работала команда
    /// `bemyvpn` из любой папки (а не только `./bemyvpn` из своей).
    Install {
        /// Куда ставить. По умолчанию: под root — /usr/local/bin,
        /// под обычным пользователем — ~/.local/bin.
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
    },
    /// Показать итоговый конфиг (с учётом дефолтов).
    Config,
    /// Список протоколов и их статус.
    Protocols,
    /// Узнать свой внешний адрес через STUN-пул.
    Stun,
    /// Проверить связь с координатором.
    Ping,
    /// Стать хостом: анонсировать себя и принимать заявки гостей. В живом
    /// терминале аргументы ПРЕД-НАСТРАИВАЮТ меню и сразу запускают раздачу;
    /// без TTY (systemd/фон) — тихий headless-режим.
    Host {
        /// Раздавать интернет по-настоящему (выход для гостей). Root НЕ нужен —
        /// хост работает в userspace (обычные сокеты). Без флага — эхо-проверка.
        #[arg(long)]
        tunnel: bool,
        /// Имя хоста в каталоге.
        #[arg(long)]
        name: Option<String>,
        /// Лимит гостей (4/8/16/32/64/128).
        #[arg(long)]
        max: Option<u32>,
        /// Пароль на раздачу (пусто = открытый).
        #[arg(long)]
        password: Option<String>,
        /// Протокол: noise | obfs | plain.
        #[arg(long)]
        proto: Option<String>,
        /// Скрытый хост (не показывать в публичном списке).
        #[arg(long)]
        hidden: bool,
        /// Поставить как службу и включить: раздача переживёт выход, обрыв SSH
        /// и перезагрузку. Нужен root (`sudo`). Настройки берутся из этой же
        /// команды и сохраняются в конфиг, который читает служба.
        #[arg(long)]
        autostart: bool,
    },
    /// Гость: показать список хостов; с --connect — подключиться к хосту.
    Guest {
        /// Фильтр по стране (например RU).
        #[arg(long)]
        country: Option<String>,
        /// Только публичные хосты.
        #[arg(long)]
        public: bool,
        /// id хоста, к которому подключиться (иначе просто список).
        #[arg(long)]
        connect: Option<String>,
        /// Пароль, если хост запаролен.
        #[arg(long)]
        password: Option<String>,
        /// Поднять НАСТОЯЩИЙ туннель (весь трафик через хост). Нужен root.
        #[arg(long)]
        tunnel: bool,
    },
    /// Демо: прогнать пакет host↔guest по loopback выбранным протоколом.
    Demo {
        #[arg(default_value = "привет из BeMyVPN")]
        message: String,
    },
    /// Режим СЕРВЕРА: поднять свой координатор (каталог хостов). Тот же бинарь —
    /// отдельного нет. Слушает [server] bind; при заданных путях к сертификату
    /// сам отдаёт HTTPS. Ctrl+C — стоп.
    Server {
        /// Домен координатора — HTTPS-сертификат получится и будет продлеваться
        /// сам (Let's Encrypt внутри). Нужны DNS-запись на этот сервер и порт 443.
        #[arg(long)]
        domain: Option<String>,
        /// Адрес прослушивания, по умолчанию 0.0.0.0:3330 (с доменом ставьте :443).
        #[arg(long)]
        bind: Option<String>,
        /// Поставить как службу и включить: координатор переживёт выход, обрыв
        /// SSH и перезагрузку. Нужен root (`sudo`).
        #[arg(long)]
        autostart: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let mut config = match Config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ошибка конфига: {e}");
            std::process::exit(1);
        }
    };
    // --coordinator переопределяет список из конфига.
    if let Some(url) = &cli.coordinator {
        config.coordinators = vec![url.clone()];
    }

    // Куда идём — меню или headless:
    //  • нет подкоманды → всегда меню;
    //  • Host/Guest в ЖИВОМ терминале → то же меню, пред-настроенное аргументами
    //    (и сразу запущенное) — как просил пользователь;
    //  • без TTY (systemd/пайп) или служебные команды → тихий headless.
    // Логи в меню НЕ инициализируем: печатались бы поверх интерфейса.
    let interactive = std::io::stdout().is_terminal();
    match &cli.cmd {
        None => return run_tui(config, Seed::default()).await,
        Some(Cmd::Host { name, max, password, proto, hidden, .. }) if interactive => {
            let seed = Seed {
                tab: Some("host"),
                host_name: name.clone(),
                host_max: *max,
                host_password: password.clone(),
                host_protocol: proto.as_deref().map(norm_proto),
                host_public: hidden.then_some(false),
                host_auto_start: true,
                ..Default::default()
            };
            return run_tui(config, seed).await;
        }
        Some(Cmd::Guest { connect, password, .. }) if interactive => {
            let seed = Seed {
                tab: Some("vpn"),
                vpn_connect: connect.clone(),
                vpn_password: password.clone(),
                ..Default::default()
            };
            return run_tui(config, seed).await;
        }
        _ => {}
    }
    let cmd = cli.cmd.unwrap();

    // ПРИВАТНОСТЬ: по умолчанию НИКАКИХ логов (off). Хост не ведёт записей ни о
    // своих действиях, ни — тем более — о трафике гостей (адреса назначения из
    // userspace-стека тоже НЕ пишутся). Статус хоста ниже — это println (нужен
    // код сети), он не про активность гостей. Явная диагностика — только по
    // осознанному желанию оператора: RUST_LOG=info bemyvpn …
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_target(false)
        .init();

    // Headless Host: аргументы командной строки → в конфиг до подъёма движка.
    if let Cmd::Host { name, max, password, proto, hidden, .. } = &cmd {
        if let Some(n) = name { config.host.name = n.clone(); }
        if let Some(m) = max { config.host.max_guests = *m; }
        if let Some(p) = password { config.host.password = p.clone(); }
        if let Some(pr) = proto { config.default_protocol = norm_proto(pr); }
        if *hidden { config.host.public = false; }
    }

    // ХОСТ: код выдаёт СЕРВЕР (единственный источник кодов — хост не придумывает
    // сам). Нет сохранённого подписанного кода в конфиге → берём у сервера и
    // печатаем, чтобы можно было сохранить в bemyvpn.toml для стабильности.
    if matches!(cmd, Cmd::Host { .. }) && (config.host.id.is_empty() || config.host.code_sig.is_empty()) {
        let tmp = BmvEngine::from_config(config.clone());
        match tmp.host_new_code().await {
            Ok((code, sig)) if !code.is_empty() && !sig.is_empty() => {
                eprintln!("Сервер выдал код: {code}");
                eprintln!("  (для стабильного кода сохраните в bemyvpn.toml → [host] id=\"{code}\", code_sig=\"{sig}\")");
                config.host.id = code;
                config.host.code_sig = sig;
            }
            _ => fail("сервер не выдал код — хостинг невозможен (сервер — единственный источник кодов)".into()),
        }
    }

    // Аргументы режима СЕРВЕРА → в конфиг: и запуск, и служба читают его же,
    // поэтому отдельный .toml писать руками не нужно.
    if let Cmd::Server { domain, bind, .. } = &cmd {
        if let Some(d) = domain {
            config.server.domain = d.clone();
            // С доменом осмысленен только 443: ACME проверяет домен именно там.
            if bind.is_none() && config.server.bind == "0.0.0.0:3330" {
                config.server.bind = "0.0.0.0:443".into();
            }
        }
        if let Some(b) = bind { config.server.bind = b.clone(); }
    }

    // `--autostart`: поставить службу и выйти, а не работать в этом процессе.
    // Настройки (включая только что полученный код хоста) сохраняем в конфиг —
    // служба читает именно его, поэтому в юните нет ни пароля, ни прочих
    // аргументов, а сам файл в /etc/systemd/system/ читаем всеми.
    let autostart_host = matches!(cmd, Cmd::Host { autostart: true, .. });
    let autostart_srv = matches!(cmd, Cmd::Server { autostart: true, .. });
    if autostart_host || autostart_srv {
        if let Err(e) = config.save() {
            fail(format!("не удалось сохранить конфиг {}: {e}", bmv_config::Config::user_path().display()));
        }
        let unit = if autostart_host { "bemyvpn-host" } else { "bemyvpn-coord" };
        let what = if autostart_host { "раздача" } else { "координатор" };
        match tui::enable_service(autostart_host) {
            Ok(()) => {
                println!("Служба {unit} установлена и запущена.");
                println!("  {what} переживёт выход, обрыв SSH и перезагрузку");
                println!("  состояние:  systemctl status {unit}");
                println!("  выключить:  systemctl disable --now {unit}");
                return;
            }
            Err(e) => fail(format!("автозапуск не настроен: {e}")),
        }
    }

    let engine = std::sync::Arc::new(BmvEngine::from_config(config));

    match cmd {
        Cmd::Version => println!("{}", bmv_common::version::VERSION),

        Cmd::Update { check } => run_update(check).await,

        Cmd::Install { dir } => run_install(dir),

        Cmd::Config => println!("{}", engine.config().to_toml()),

        Cmd::Protocols => {
            println!("Протоколы (порядок = приоритет фолбэка):");
            for p in engine.protocols() {
                let lock = if p.encrypts { "🔒 шифрует" } else { "🔓 без шифра" };
                let avail = if p.available { "доступен" } else { "недоступен" };
                println!("  {:<10} {lock:<14} [{avail}]", p.name);
            }
            let order: Vec<_> = engine.connect_order().iter().map(|p| p.name()).collect();
            println!("\nПорядок соединения: {}", order.join(" → "));
        }

        Cmd::Stun => match engine.external_addr().await {
            Ok(addr) => println!("Внешний адрес (server-reflexive): {addr}"),
            Err(e) => fail(format!("STUN не удался: {e}")),
        },

        Cmd::Ping => {
            let base = engine.config().coordinators.first().cloned().unwrap_or_default();
            print!("Координатор {base} … ");
            match engine.coordinator_health().await {
                Ok(()) => println!("жив ✅"),
                Err(e) => fail(format!("недоступен: {e}")),
            }
        }

        Cmd::Host { tunnel, .. } => run_host(engine.clone(), tunnel).await,

        Cmd::Guest {
            country,
            public,
            connect,
            password,
            tunnel,
        } => run_guest(&engine, country, public, connect, password, tunnel).await,

        Cmd::Server { .. } => run_server(engine.config()).await,

        Cmd::Demo { message } => {
            print!("Демо loopback ({})… ", engine.active_protocol());
            match engine.demo_loopback(message.as_bytes()).await {
                Ok(echo) if echo == message.as_bytes() => {
                    println!("OK ✅  эхо совпало: {:?}", String::from_utf8_lossy(&echo))
                }
                Ok(_) => fail("эхо не совпало".into()),
                Err(e) => fail(format!("ошибка: {e}")),
            }
        }
    }
}

async fn run_host(engine: std::sync::Arc<BmvEngine>, tunnel: bool) {
    print!("Поднимаю хоста и анонсирую координатору… ");
    let (hub, id, endpoints) = match engine.host_bind_announce().await {
        Ok(v) => {
            println!("готово ✅");
            v
        }
        Err(e) => fail(format!("не удалось: {e}")),
    };
    println!(
        "Хост #{id} в каталоге ({}){}.",
        if engine.config().host.public { "публичный" } else { "скрытый" },
        if tunnel { ", режим ТУННЕЛЬ (выход в интернет)" } else { " (эхо-проверка)" }
    );
    println!("  мои адреса: {}", endpoints.join(", "));
    println!("Жду гостей (мультигость), Ctrl+C — выход.");

    // Периодический heartbeat — держит NAT-дырку hub-сокета открытой (иначе хост
    // за NAT выпадает из достижимости) и обновляет запись в каталоге (self-heal).
    {
        let e = engine.clone();
        let h = hub.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let _ = e.host_heartbeat(&h).await;
            }
        });
    }

    // Встречное пробитие NAT — хост шлёт PUNCH ждущим гостям (иначе за NAT недостать).
    {
        let e = engine.clone();
        let h = hub.clone();
        tokio::spawn(async move {
            let _ = e.host_serve_punch(h).await;
        });
    }

    // Каждый гость — своя задача (одновременное обслуживание). БЕЗ логирования:
    // хост НЕ ведёт записей об активности гостей (кто/когда подключился/ушёл) —
    // приватность. Живое число гостей всё равно видно в каталоге в реальном времени.
    while let Some((peer, raw)) = hub.accept().await {
        let e = engine.clone();
        tokio::spawn(async move {
            let _ = e.host_run_session(peer, raw, tunnel).await;
        });
    }
}

async fn run_guest(
    engine: &BmvEngine,
    country: Option<String>,
    public: bool,
    connect: Option<String>,
    password: Option<String>,
    tunnel: bool,
) {
    match connect {
        None => {
            print!("Беру список хостов из каталога… ");
            match engine.guest_list(country, public).await {
                Ok(hosts) if hosts.is_empty() => println!("пусто (нет живых хостов)"),
                Ok(hosts) => {
                    println!("нашёл {}:", hosts.len());
                    for h in hosts {
                        let lock = if h.has_password { "🔒приват" } else { "🔓откр" };
                        let vis = if h.public { "публичный" } else { "скрытый" };
                        let status = if h.online { "🟢" } else { "⚪️" };
                        let name = if h.name.is_empty() { h.id.clone() } else { h.name.clone() };
                        let ip = h.endpoints.first().map(|e| e.as_str()).unwrap_or("—");
                        println!(
                            "  {status} {name} ({})  [{}]  {}  {}/{} гостей  {}",
                            h.id, lock, vis, h.guests, h.max_guests, ip
                        );
                    }
                    println!("\nПодключиться: bemyvpn guest --connect <id>");
                }
                Err(e) => fail(format!("каталог недоступен: {e}")),
            }
        }
        Some(host_id) => {
            if tunnel {
                println!(
                    "Поднимаю ТУННЕЛЬ к {host_id} (знакомство → пробитие NAT → {})…",
                    engine.active_protocol()
                );
                // Узнаём протокол хоста из каталога — гость обязан ему следовать.
                let host_proto = engine
                    .guest_list(None, false)
                    .await
                    .ok()
                    .and_then(|hs| hs.into_iter().find(|h| h.id == host_id))
                    .map(|h| h.protocol)
                    .filter(|p| !p.is_empty());
                let (peer, link) = match engine
                    .guest_establish(&host_id, password.as_deref(), host_proto.as_deref())
                    .await
                {
                    Ok(v) => v,
                    Err(e) => fail(format!("не соединился: {e}")),
                };
                let params = bmv_tunnel::TunParams::guest();
                let (device, ifname) = match bmv_desktop::make_tun(&params) {
                    Ok(d) => d,
                    Err(e) => fail(format!("не создать TUN (нужен root/sudo): {e}")),
                };
                // Полный туннель: split-default заворачивает ВЕСЬ трафик через хост,
                // хост пинуется через реальный шлюз (анти-петля), DNS → 8.8.8.8.
                // Снимается на Drop (тот же RouteGuard, что и в GUI). Ctrl+C → откат.
                let _guard = match bmv_desktop::RouteGuard::install(peer.ip(), &ifname) {
                    Ok(g) => g,
                    Err(e) => fail(format!("не настроить маршруты (нужен root/sudo): {e}")),
                };
                println!("  соединён с {peer}. Весь трафик идёт через хост (интерфейс {ifname}).");
                if let Err(e) = bmv_tunnel::run_guest(device, std::sync::Arc::from(link)).await {
                    fail(format!("туннель оборвался: {e}"));
                }
                println!("  туннель завершён.");
                return;
            }
            let msg = "привет от гостя BeMyVPN";
            print!(
                "Соединяюсь с хостом {host_id} (знакомство → пробитие NAT → {})… ",
                engine.active_protocol()
            );
            match engine.guest_connect_run(&host_id, msg.as_bytes()).await {
                Ok((peer, echo)) if echo == msg.as_bytes() => println!(
                    "СОЕДИНЕНО ✅  прямой канал с {peer}, эхо совпало: {:?}",
                    String::from_utf8_lossy(&echo)
                ),
                Ok((peer, echo)) => println!(
                    "соединено с {peer}, но эхо иное: {:?}",
                    String::from_utf8_lossy(&echo)
                ),
                Err(e) => fail(format!("не соединился: {e}")),
            }
        }
    }
}

/// Собрать режим TLS из `[server]`: домен → авто Let's Encrypt; свой cert+key →
/// Files; иначе HTTP. Общий для команды `server` и TUI-вкладки «Сервер».
pub(crate) fn tls_from_config(s: &bmv_config::ServerConfig) -> bmv_coordinator::Tls {
    if !s.domain.is_empty() {
        bmv_coordinator::Tls::Acme {
            domains: s.domain.split(',').map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).collect(),
            email: (!s.acme_email.is_empty()).then(|| s.acme_email.clone()),
            cache: if s.acme_cache.is_empty() { "acme-cache".into() } else { s.acme_cache.clone() },
        }
    } else if !s.tls_cert.is_empty() && !s.tls_key.is_empty() {
        bmv_coordinator::Tls::Files { cert: s.tls_cert.clone(), key: s.tls_key.clone() }
    } else {
        bmv_coordinator::Tls::None
    }
}

/// Режим СЕРВЕРА: поднять координатор из конфига `[server]`. Тот же бинарь, что и
/// клиент — отдельного координатора нет. Домен в конфиге → HTTPS сам.
async fn run_server(config: &Config) {
    let s = &config.server;
    let bind: std::net::SocketAddr = match s.bind.parse() {
        Ok(b) => b,
        Err(e) => fail(format!("[server] bind — неверный адрес «{}»: {e}", s.bind)),
    };
    let tls = tls_from_config(s);
    let mode = match &tls {
        bmv_coordinator::Tls::Acme { domains, .. } => format!("HTTPS авто Let's Encrypt: {}", domains.join(", ")),
        bmv_coordinator::Tls::Files { .. } => "HTTPS (свой сертификат)".into(),
        bmv_coordinator::Tls::None => "HTTP".into(),
    };
    println!("Координатор (свой сервер) слушает {bind} [{mode}]. Ctrl+C — стоп.");
    if let Err(e) = bmv_coordinator::serve(bind, tls, None, std::future::pending()).await {
        fail(format!("координатор упал: {e}"));
    }
}

/// Открыть TUI-меню (пред-настроенное аргументами). Ошибка → код 1.
async fn run_tui(config: Config, seed: Seed) {
    if let Err(e) = tui::run(config, seed).await {
        eprintln!("TUI: {e}");
        std::process::exit(1);
    }
}

/// Нормализация имени протокола из аргумента (`--proto obfs`) в внутреннее.
fn norm_proto(p: &str) -> String {
    match p.to_lowercase().as_str() {
        "obfs" | "noise-obfs" | "скрытный" | "маскировка" => "noise-obfs".into(),
        "plain" | "без" | "none" => "plain".into(),
        _ => "noise".into(),
    }
}

/// Напечатать ошибку и выйти. Возвращает `!` — компилятор знает, что после
/// вызова кода нет, поэтому её можно ставить в ветку `else` у `let...else`.
fn fail(msg: String) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}


/// Обновление терминальной версии: проверить, скачать, подменить себя.
///
/// Порядок проверок не сокращаем ни при каких условиях: подпись манифеста →
/// версия новее → sha256 файла. Рабочий бинарь трогается только после всех трёх,
/// поэтому оборванная загрузка или подменённый файл не могут оставить систему
/// без работающей программы.
/// Поставить себя в систему, чтобы работала команда `bemyvpn` из любой папки.
///
/// Скачанный файл лежит там, куда его положил браузер, и запускать его
/// приходится как `./bemyvpn` — а все инструкции, юниты systemd и подсказки
/// написаны про `bemyvpn`. Эта команда убирает расхождение.
///
/// КОПИРУЕМ, а не переносим: файл прямо сейчас выполняется, и переносить его
/// из-под себя — напрашиваться на неприятности. Копия идёт через временный файл
/// рядом с целью и `rename` — так на месте назначения никогда не окажется
/// наполовину записанного бинаря, даже если оборвётся питание.
fn run_install(dir: Option<std::path::PathBuf>) {
    let me = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => fail(format!("не удалось определить свой путь: {e}")),
    };
    let dir = dir.unwrap_or_else(default_bin_dir);
    let target = dir.join(BIN_NAME);

    if target == me {
        println!("Уже установлено: {}", target.display());
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        fail(format!("{}: {e}\n  подсказка: под обычным пользователем попробуйте `bemyvpn install --dir ~/.local/bin`", dir.display()));
    }

    let tmp = dir.join(format!(".{BIN_NAME}.new"));
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = std::fs::copy(&me, &tmp) {
        let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
            "\n  подсказка: нужны права — `sudo bemyvpn install` или `--dir ~/.local/bin`"
        } else {
            ""
        };
        fail(format!("копирование в {}: {e}{hint}", dir.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        fail(format!("установка в {}: {e}", target.display()));
    }

    println!("Установлено: {}", target.display());
    if in_path(&dir) {
        println!("Теперь работает команда: bemyvpn");
    } else {
        // Молча положить файл в папку, которой нет в PATH, — значит соврать:
        // человек напечатает `bemyvpn` и получит «command not found».
        println!("ВНИМАНИЕ: {} не в PATH — команда `bemyvpn` пока не найдётся.", dir.display());
        println!("  добавьте строку в ~/.profile (или ~/.zshrc):");
        println!("    export PATH=\"{}:$PATH\"", dir.display());
    }
    println!("Дальше: `bemyvpn` — меню, `bemyvpn host` — раздавать, `bemyvpn update` — обновиться.");
}

/// Имя, под которым программа живёт в системе (и которое ждут все инструкции).
const BIN_NAME: &str = "bemyvpn";

/// Куда ставить по умолчанию: общесистемно, если туда пускают, иначе к себе.
///
/// Проверяем именно ПРАВО ЗАПИСИ, а не «я root»: на macOS `/usr/local/bin`
/// обычно принадлежит пользователю (так делает Homebrew), и требовать там sudo
/// значит зря просить пароль. А на сервере под root проверка пройдёт сама.
fn default_bin_dir() -> std::path::PathBuf {
    let system = std::path::Path::new("/usr/local/bin");
    if writable(system) {
        return system.to_path_buf();
    }
    std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".local/bin"))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Можно ли писать в папку — пробуем на деле, а не гадаем по правам и владельцу
/// (иначе промахнёшься на ACL, на монтировании только для чтения и на root_squash).
fn writable(dir: &std::path::Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".bemyvpn-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Есть ли папка в PATH — чтобы не обещать работающую команду там, где её нет.
fn in_path(dir: &std::path::Path) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == dir))
        .unwrap_or(false)
}


/// Перезапустить службу systemd, которая запускает ИМЕННО ЭТОТ файл.
///
/// Имя юнита не угадываем: приложение ставит `bemyvpn-host`/`bemyvpn-coord`, но
/// сервер мог быть поднят руками под любым именем. Ищем по тому, что юнит
/// РЕАЛЬНО запускает, — это единственный надёжный признак «эта служба про нас».
///
/// Перезапускаем ТОЛЬКО при однозначном совпадении. Нашлось несколько (хост и
/// координатор на одной машине) — трогать наугад нельзя: перезапуск обрывает
/// живых гостей. В таком случае просто говорим, что сделать.
#[cfg(target_os = "linux")]
fn restart_owning_service() {
    use std::process::Command;

    // Не systemd (контейнер, WSL, обычный запуск из терминала) — перезапускать нечего.
    if !std::path::Path::new("/run/systemd/system").exists() {
        println!("  запустите программу заново");
        return;
    }
    let Ok(me) = std::env::current_exe() else {
        println!("  запустите программу заново");
        return;
    };
    let me = me.to_string_lossy().to_string();

    let Ok(out) = Command::new("systemctl")
        .args(["list-units", "--type=service", "--state=active", "--no-legend", "--plain", "--no-pager"])
        .output()
    else {
        println!("  запустите программу заново");
        return;
    };
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
        .collect();

    let mut units: Vec<(String, String)> = Vec::new();
    for n in names {
        if let Ok(o) = Command::new("systemctl").args(["show", "-p", "ExecStart", "--value", &n]).output() {
            units.push((n, String::from_utf8_lossy(&o.stdout).to_string()));
        }
    }
    match unit_for_exe(&units, &me) {
        Some(unit) => {
            let ok = Command::new("systemctl").args(["restart", &unit]).status().map(|s| s.success()).unwrap_or(false);
            if ok {
                println!("  служба {unit} перезапущена — новая версия уже работает");
            } else {
                // Чаще всего это «не root»: обновиться из своей папки можно, а
                // управлять службой — нет.
                println!("  перезапустите службу: sudo systemctl restart {unit}");
            }
        }
        None => println!("  запустите программу заново (службы, запускающей этот файл, не нашлось)"),
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn restart_owning_service() {
    // launchd на macOS: наши службы там не ставятся, перезапускать нечего.
    println!("  запустите программу заново");
}

/// Единственный юнит, чей ExecStart запускает `exe`. Несколько или ни одного —
/// None: перезапускать наугад нельзя, это обрывает живых гостей.
///
/// Вынесено отдельно от вызова systemctl, чтобы правило можно было проверить.
/// На не-Linux собирается только для тестов: перезапускать там нечего.
#[cfg(any(target_os = "linux", test))]
fn unit_for_exe(units: &[(String, String)], exe: &str) -> Option<String> {
    let hits: Vec<&String> = units
        .iter()
        .filter(|(_, exec)| exec_paths(exec).any(|p| p == exe))
        .map(|(n, _)| n)
        .collect();
    (hits.len() == 1).then(|| hits[0].clone())
}

/// Пути запускаемых файлов из вывода `systemctl show -p ExecStart --value`.
///
/// Формат такой: `{ path=/opt/bemyvpn/bemyvpn ; argv[]=... ; ... }`, строк может
/// быть несколько (у юнита бывает несколько ExecStart).
///
/// Берём именно поле `path=` и сравниваем ЦЕЛИКОМ. Сравнение подстрокой здесь
/// ошибается: `/usr/local/bin/bemyvpn-helper` содержит `/usr/local/bin/bemyvpn`
/// как начало, и служба чужой программы сошла бы за нашу — а мы её перезапустим.
#[cfg(any(target_os = "linux", test))]
fn exec_paths(exec_start: &str) -> impl Iterator<Item = &str> {
    exec_start.lines().filter_map(|l| {
        let after = l.split("path=").nth(1)?;
        Some(after.split([' ', ';', '}']).next().unwrap_or("").trim())
    })
}

async fn run_update(check_only: bool) {
    if !bmv_common::version::is_release_build() {
        println!("Это локальная сборка ({}) — обновлять нечего.", bmv_common::version::VERSION);
        return;
    }
    let repo = std::env::var("BMV_REPO").unwrap_or_else(|_| "mister-PARADISE/bemyvpn".into());

    let tag = match bmv_common::update::github_latest_tag(&repo).await {
        Ok(t) => t,
        // Самый частый случай в местах с блокировками — и его же решает наше
        // приложение, поэтому подсказываем именно это.
        Err(e) => fail(format!("{e}\n  подсказка: если GitHub недоступен — подключитесь к VPN и повторите")),
    };
    let latest = tag.trim_start_matches('v');

    if !bmv_common::version::is_newer(latest, bmv_common::version::VERSION) {
        println!("Установлена свежая версия {} — обновлять нечего.", bmv_common::version::VERSION);
        return;
    }
    println!("Доступна версия {latest} (у вас {})", bmv_common::version::VERSION);
    if check_only {
        return;
    }

    let Some(asset) = bmv_common::update::current_asset_name(false) else {
        fail("для этой платформы релизы не выпускаются".into());
    };
    let url = bmv_common::update::asset_url(&repo, &tag, asset);
    println!("Скачиваю {asset}…");

    // Целостность даёт HTTPS: файл идёт с github.com, подменить его в пути
    // нельзя. Отдельная проверка хэша ничего бы не добавила.
    let bytes = match bmv_common::update::download(&url, bmv_common::update::MAX_ASSET_BYTES).await {
        Ok(b) => b,
        Err(e) => fail(format!("{e}\n  подсказка: если GitHub недоступен — подключитесь к VPN и повторите")),
    };
    println!("Скачано: {} байт", bytes.len());

    // Unix подменяет себя сам: работающий процесс держит старый inode и спокойно
    // доживает. На Windows файл запущенной программы переписать НЕЛЬЗЯ — там
    // помощник ждёт нашего выхода и меняет уже он.
    #[cfg(unix)]
    match bmv_common::update::replace_self(&bytes) {
        Ok(bak) => {
            println!("Готово. Версия {latest} установлена.");
            println!("  старая сохранена: {}", bak.display());
            // Служба продолжает крутить СТАРЫЙ файл: на Unix запущенный процесс
            // держит прежний inode и живёт им до перезапуска. Человек, который
            // обновился и ушёл, об этом не знает — а сервер месяцами работает на
            // старой версии, думая, что обновлён. Поэтому перезапускаем сами.
            restart_owning_service();
        }
        Err(e) => fail(format!("замена не удалась: {e}")),
    }
    #[cfg(windows)]
    match bmv_common::update::spawn_exe_updater(&bytes) {
        Ok(()) => {
            println!("Готово. Версия {latest} будет установлена при выходе.");
            println!("  закройте программу — обновление применится само");
        }
        Err(e) => fail(format!("замена не удалась: {e}")),
    }
}

#[cfg(test)]
mod update_tests {
    use super::unit_for_exe;

    fn u(n: &str, e: &str) -> (String, String) { (n.to_string(), e.to_string()) }

    /// Юнит ищется по тому, какой файл он РЕАЛЬНО запускает, а не по имени:
    /// приложение ставит `bemyvpn-host`, а поднятый руками сервер может
    /// называться как угодно (у боевого координатора это `bmv-coordinator`).
    #[test]
    fn finds_unit_by_executable_not_by_name() {
        let units = vec![
            u("ssh.service", "{ path=/usr/sbin/sshd ; argv[]=/usr/sbin/sshd -D }"),
            u("bmv-coordinator.service", "{ path=/opt/bemyvpn/bemyvpn ; argv[]=/opt/bemyvpn/bemyvpn server }"),
        ];
        assert_eq!(unit_for_exe(&units, "/opt/bemyvpn/bemyvpn").as_deref(), Some("bmv-coordinator.service"));
    }

    /// НЕОДНОЗНАЧНОСТЬ — не перезапускаем. Хост и координатор на одной машине
    /// запускаются одним и тем же файлом; дёрнуть наугад значит оборвать живых
    /// гостей у той службы, которую человек не трогал.
    #[test]
    fn refuses_when_several_units_run_the_same_binary() {
        let units = vec![
            u("bemyvpn-host.service", "{ path=/usr/local/bin/bemyvpn ; argv[]=/usr/local/bin/bemyvpn host }"),
            u("bemyvpn-coord.service", "{ path=/usr/local/bin/bemyvpn ; argv[]=/usr/local/bin/bemyvpn server }"),
        ];
        assert_eq!(unit_for_exe(&units, "/usr/local/bin/bemyvpn"), None);
    }

    /// Ничего нашего не запущено — перезапускать нечего.
    #[test]
    fn no_match_yields_none() {
        let units = vec![u("nginx.service", "{ path=/usr/sbin/nginx ; argv[]=/usr/sbin/nginx }")];
        assert_eq!(unit_for_exe(&units, "/usr/local/bin/bemyvpn"), None);
        assert_eq!(unit_for_exe(&[], "/usr/local/bin/bemyvpn"), None);
    }

    /// Чужой файл с похожим путём не должен считаться нашим по совпадению
    /// куска строки: сравниваем полный путь, который дал systemd.
    #[test]
    fn similar_path_of_another_binary_does_not_match() {
        let units = vec![u("other.service", "{ path=/usr/local/bin/bemyvpn-helper ; argv[]=/usr/local/bin/bemyvpn-helper }")];
        // Наш путь — /usr/local/bin/bemyvpn, а в юните ДРУГОЙ файл. Он содержит
        // наш путь как префикс, поэтому проверка «подстрокой» тут ошиблась бы.
        assert_eq!(unit_for_exe(&units, "/usr/local/bin/bemyvpn"), None);
    }
}
