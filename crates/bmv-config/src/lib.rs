//! bmv-config — ЕДИНСТВЕННЫЙ источник настроек и дефолтов.
//!
//! Один файл `bemyvpn.toml`. Любого ключа может не быть — подставится дефолт
//! (все дефолты живут ЗДЕСЬ и больше нигде). Порядок поиска файла:
//!   1. явный путь (--config PATH)
//!   2. переменная окружения BEMYVPN_CONFIG
//!   3. ./bemyvpn.toml рядом с бинарём/в рабочей папке
//!   4. платформенный конфиг-каталог (позже)
//!
//! Файла нет вовсе — тоже ок, берётся всё по умолчанию.

use std::path::{Path, PathBuf};

use bmv_common::{Error, Result};
use serde::{Deserialize, Serialize};

/// Корневой конфиг клиента. Один на всё приложение.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Куда ходить за списком хостов и знакомством. НЕ захардкожено.
    pub coordinators: Vec<String>,
    /// Протокол по умолчанию. Не поднялся — ядро само пробует следующий.
    pub default_protocol: String,
    pub guest: GuestConfig,
    pub host: HostConfig,
    pub protocols: ProtocolsConfig,
    pub stun: StunConfig,
    pub log: LogConfig,
    /// Настройки режима СЕРВЕРА (свой координатор). Тот же бинарь `bemyvpn`
    /// поднимает координатор командой `server` или из меню — отдельного бинаря нет.
    pub server: ServerConfig,
}

/// Режим «Сервер» (координатор): где слушать и пути к TLS-сертификату. Всё здесь —
/// один конфиг; пути можно задать и визуально в TUI. Тот же режим, что вкладка
/// «Сервер» на Android (только там локальный HTTP), и на нём же работает боевой.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Адрес прослушивания. Боевой HTTPS — `0.0.0.0:443`; локально — `0.0.0.0:3330`.
    pub bind: String,
    /// Домен для АВТО-HTTPS (Let's Encrypt ВНУТРИ бинаря). Задан → сам получает и
    /// продлевает сертификат, certbot/nginx НЕ нужны. Несколько — через запятую.
    /// Это главный путь: «скачал, вписал домен, запустил — HTTPS работает».
    pub domain: String,
    /// Email для Let's Encrypt (уведомления об истечении). Не обязателен.
    pub acme_email: String,
    /// Папка кэша ACME-сертификата (переживает рестарты). Держи в бэкапе.
    pub acme_cache: String,
    /// Свой сертификат (если НЕ авто-ACME): пути к fullchain и ключу.
    pub tls_cert: String,
    pub tls_key: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            bind: "0.0.0.0:3330".into(),
            domain: String::new(),
            acme_email: String::new(),
            acme_cache: "acme-cache".into(),
            tls_cert: String::new(),
            tls_key: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GuestConfig {
    /// DNS всегда через туннель (анти-утечка). Менять только осознанно.
    pub dns: String, // "tunnel" | "1.1.1.1" | "system"
    pub kill_switch: bool,
    pub ipv6: String, // "route" | "block"
    pub auto_reconnect: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    pub enabled: bool,
    /// Стабильный id хоста. Пусто → генерится случайный на сессию.
    pub id: String,
    /// Секрет владельца записи в каталоге. Координатор привязывает id→token при
    /// первом анонсе и требует его на обновлениях/bye — защита от угона записи и
    /// снятия чужого хоста. Пусто → движок сгенерит на сессию (для стабильной
    /// защиты между рестартами задайте постоянный в конфиге/prefs).
    pub token: String,
    /// Человекочитаемое имя для каталога (UI). Пусто → показывается id.
    pub name: String,
    /// Подпись кода `id` сервером (HMAC из `new_code`). Сервер — единственный
    /// источник кодов; координатор требует эту подпись при первом анонсе. Пусто
    /// → у хоста ещё нет выданного кода (нужно запросить у сервера).
    pub code_sig: String,
    pub public: bool,
    pub password: String,
    pub max_guests: u32,
    pub country_hint: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolsConfig {
    pub reality: RealityConfig,
    pub wireguard: WireguardConfig,
}

/// ЗАДЕЛ на будущее: REALITY (TLS-мимикрия через внешний sing-box) ещё НЕ в
/// реестре протоколов (см. bmv-protocol). Секция читается, но пока не
/// задействована — оставлено как план, чтобы конфиг не менялся при внедрении.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RealityConfig {
    pub sing_box_path: String,
    pub sni: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WireguardConfig {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StunConfig {
    /// Отдельный файл со списком серверов (host:port на строку, # — коммент).
    pub file: String,
    /// Явный список — если задан, имеет приоритет над файлом.
    pub servers: Vec<String>,
}

impl Default for StunConfig {
    fn default() -> Self {
        StunConfig {
            file: "stun_servers.txt".into(),
            servers: Vec::new(),
        }
    }
}

impl StunConfig {
    /// Итоговый список STUN-серверов: явный `servers` → иначе файл → иначе пусто
    /// (пустой список означает «взять встроенный пул» на стороне bmv-net).
    pub fn resolve(&self) -> Vec<String> {
        if !self.servers.is_empty() {
            return self.servers.clone();
        }
        if !self.file.is_empty() {
            if let Ok(text) = std::fs::read_to_string(&self.file) {
                return parse_stun_file(&text);
            }
        }
        Vec::new()
    }
}

/// Разобрать текст файла STUN: строки host:port, пропуская # и пустые.
pub fn parse_stun_file(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    pub level: String,
    pub file: String,
}

// ── дефолты (единственное место) ────────────────────────────────────────────

impl Default for Config {
    fn default() -> Self {
        Config {
            // Референс-координатор сообщества (каталог хостов). Работает из
            // коробки; поставь свой одной строкой в bemyvpn.toml или подними
            // собственный (см. README → «Свой сервер»). Не захардкожено в логике —
            // это лишь дефолт, любой адрес переопределяет.
            coordinators: vec!["https://bemyvpn.net".into()],
            // По умолчанию «Маскировка»: то же шифрование, что и «Обычный», плюс
            // защита от DPI — провайдер видит просто случайные данные, а не VPN.
            // Цена — небольшой оверхед; выигрыш в местах с блокировками важнее.
            default_protocol: "noise-obfs".into(),
            guest: GuestConfig::default(),
            host: HostConfig::default(),
            protocols: ProtocolsConfig::default(),
            stun: StunConfig::default(),
            log: LogConfig::default(),
            server: ServerConfig::default(),
        }
    }
}

impl Default for GuestConfig {
    fn default() -> Self {
        GuestConfig {
            dns: "tunnel".into(),
            kill_switch: false,
            ipv6: "block".into(),
            auto_reconnect: true,
        }
    }
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            enabled: false,
            id: String::new(),
            token: String::new(),
            name: String::new(),
            code_sig: String::new(),
            public: false,
            password: String::new(),
            max_guests: suggested_max_guests(),
            country_hint: "auto".into(),
        }
    }
}

impl Default for RealityConfig {
    fn default() -> Self {
        RealityConfig {
            sing_box_path: String::new(),
            sni: "www.mi.com".into(),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        LogConfig {
            level: "info".into(),
            file: "bemyvpn.log".into(),
        }
    }
}

// ── загрузка ────────────────────────────────────────────────────────────────

impl Config {
    /// Загрузить конфиг, разрешая путь по правилам поиска.
    pub fn load(explicit: Option<&Path>) -> Result<Config> {
        match resolve_path(explicit) {
            Some(path) => Self::from_file(&path),
            None => Ok(Config::default()),
        }
    }

    pub fn from_file(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        Self::from_toml(&text)
    }

    pub fn from_toml(text: &str) -> Result<Config> {
        toml::from_str(text).map_err(|e| Error::Config(e.to_string()))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Постоянный пользовательский путь конфига — сюда пишет авто-сохранение
    /// (настройки из меню), отсюда же conфиг подхватывается при старте.
    /// Linux/macOS: ~/.config/bemyvpn/config.toml · Windows: %APPDATA%\bemyvpn.
    pub fn user_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("bemyvpn").join("config.toml")
    }

    /// Сохранить конфиг в пользовательский путь (создаёт папку). Меню зовёт это
    /// при каждом изменении — руками .toml править не нужно.
    pub fn save(&self) -> Result<()> {
        let path = Self::user_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Config(format!("{}: {e}", dir.display())))?;
        }
        std::fs::write(&path, self.to_toml()).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        restore_owner_after_sudo(&path);
        Ok(())
    }
}

/// Под `sudo` вернуть конфиг во владение тому, кто запускал.
///
/// `sudo` сохраняет HOME пользователя, поэтому root писал файл прямо в его
/// `~/.config/bemyvpn/` — и после первого же запуска под sudo приложение,
/// запущенное обычным пользователем, больше НЕ МОГЛО сохранить настройки
/// (Permission denied), причём молча. Возвращаем владельца по SUDO_UID/SUDO_GID.
/// Ошибки глотаем: не смогли — файл просто остаётся root'овым, как раньше.
#[cfg(unix)]
fn restore_owner_after_sudo(path: &Path) {
    let (Ok(uid), Ok(gid)) = (std::env::var("SUDO_UID"), std::env::var("SUDO_GID")) else { return };
    let (Ok(uid), Ok(gid)) = (uid.parse::<u32>(), gid.parse::<u32>()) else { return };
    for p in [path.parent(), Some(path)].into_iter().flatten() {
        if let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) {
            unsafe { libc::chown(c.as_ptr(), uid, gid) };
        }
    }
}

#[cfg(not(unix))]
fn restore_owner_after_sudo(_path: &Path) {}

/// Сколько гостей предлагать по умолчанию НОВОМУ хосту.
///
/// Считаем от числа ядер: шифрование и userspace-TCP упираются в процессор.
/// Плоская четвёрка была одинаковой и для Raspberry Pi, и для 16-ядерного VPS —
/// мощная машина занижала себя в разы, а список хостов выглядел одинаково
/// бедным. Оценка НАМЕРЕННО скромная (4 гостя на ядро, не выше 64): настоящий
/// потолок задаёт ширина канала, а её без прогона трафика не измерить. Это лишь
/// стартовое значение — в меню оно меняется одним нажатием.
pub fn suggested_max_guests() -> u32 {
    let cores = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1);
    let raw = cores.saturating_mul(4).clamp(4, 64);
    // Округляем ВНИЗ до значения из набора пресетов интерфейса — иначе на
    // 10 ядрах вышло бы 40, а такой кнопки в меню нет, и выбранным не подсветится
    // ни один пресет. Вниз, а не вверх: занизить безопаснее, чем пообещать лишнее.
    match raw {
        64.. => 64,
        32..=63 => 32,
        16..=31 => 16,
        8..=15 => 8,
        _ => 4,
    }
}

fn resolve_path(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Ok(env) = std::env::var("BEMYVPN_CONFIG") {
        if !env.is_empty() {
            return Some(PathBuf::from(env));
        }
    }
    let local = PathBuf::from("bemyvpn.toml");
    if local.exists() {
        return Some(local);
    }
    // Авто-сохранённый конфиг из меню (~/.config/bemyvpn/config.toml).
    let user = Config::user_path();
    if user.exists() {
        return Some(user);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        // По умолчанию «Маскировка» — шифрование плюс защита от DPI.
        assert_eq!(c.default_protocol, "noise-obfs");
        assert_eq!(c.guest.dns, "tunnel");
        assert_eq!(c.protocols.reality.sni, "www.mi.com");
        // Лимит подбирается от числа ядер, поэтому проверяем не число, а рамки:
        // не ниже минимума и не выше потолка, и всегда одно из значений пресетов.
        assert!((4..=64).contains(&c.host.max_guests), "лимит вне рамок: {}", c.host.max_guests);
        assert_eq!(c.host.max_guests, suggested_max_guests());
    }

    /// Авто-сохранение: save() → user_path() → load обратно с теми же значениями.
    #[test]
    fn save_roundtrip_user_path() {
        let dir = std::env::temp_dir().join(format!("bmv-cfg-test-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let mut c = Config {
            default_protocol: "noise-obfs".into(),
            ..Default::default()
        };
        c.host.max_guests = 42;
        c.save().unwrap();
        let loaded = Config::from_file(&Config::user_path()).unwrap();
        assert_eq!(loaded.default_protocol, "noise-obfs");
        assert_eq!(loaded.host.max_guests, 42);
        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let c = Config::from_toml("default_protocol = \"plain\"").unwrap();
        assert_eq!(c.default_protocol, "plain");
        // остальное — дефолты
        assert_eq!(c.guest.dns, "tunnel");
    }
}
