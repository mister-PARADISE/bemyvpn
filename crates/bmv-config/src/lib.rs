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

/// ПРОТОКОЛ ПО УМОЛЧАНИЮ — ОДИН НА ВЕСЬ ПРОЕКТ.
///
/// «Маскировка»: то же шифрование, что и «Обычный», плюс защита от DPI —
/// провайдер видит просто случайные данные, а не VPN. Цена — небольшой оверхед,
/// выигрыш в местах с блокировками важнее.
///
/// Константа, а не строка в трёх местах: раньше ядро для гостя подставляло
/// «noise», а конфиг и хост — «noise-obfs», и пустая настройка из оболочки
/// разводила стороны по разным протоколам. Со стороны человека это выглядело как
/// 12 секунд тишины при подключении, а не как ошибка.
pub const DEFAULT_PROTOCOL: &str = "noise-obfs";

/// Корневой конфиг клиента. Один на всё приложение.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Куда ходить за списком хостов и знакомством. НЕ захардкожено.
    pub coordinators: Vec<String>,
    /// Протокол по умолчанию: `noise` | `noise-obfs` | `plain`.
    ///
    /// РОВНО ЭТОТ и никакой другой. Здесь было написано «не поднялся — ядро
    /// само пробует следующий»; перебора нет и не было (обе стороны берут одно
    /// имя, договориться на лету по UDP не с кем). Незнакомое имя связью не
    /// станет — см. `BmvEngine::default_proto`.
    pub default_protocol: String,
    pub guest: GuestConfig,
    pub host: HostConfig,
    pub stun: StunConfig,
    // Здесь были `protocols: ProtocolsConfig` (секции `[protocols.reality]` и
    // `[protocols.wireguard]`) и `log: LogConfig` (`level` + `file`) — четыре
    // ключа, у которых на весь репозиторий НОЛЬ читателей.
    //
    // `[protocols.*]` описывали протоколы, которых нет: в реестре
    // (`bmv_protocol::Registry::with_builtins`) только `noise-obfs`, `noise` и
    // `plain`; файлов `reality.rs`/`wireguard.rs` не существует, `sing-box`
    // ниоткуда не запускается, `boringtun` не подключён. `WireguardConfig` был
    // даже без единого поля — пустая секция «на будущее». Задел, который не
    // стоит ничего: когда протокол появится, он придёт со СВОИМИ настройками, и
    // угадать их заранее всё равно нельзя. А пока `sni = "www.mi.com"` в файле
    // выглядит как работающая мимикрия — то есть врёт.
    //
    // `[log]` врал сильнее. Файл `bemyvpn.log` не открывался никогда и никем, а
    // человек, поставивший `level = "debug"`, видел в конфиге имя лог-файла и
    // считал, что его переписка с координатором лежит на диске.
    //
    // Ручка не вернётся: у клиента журнала нет ВОВСЕ — ни файла, ни системного,
    // ни выключенного по умолчанию. Ни подписчика, ни самих вызовов записи в
    // клиентских крейтах не осталось, и держит это сторож
    // `no_journal_in_the_client` (crates/bmv-common/tests). Печатает `bemyvpn`
    // только ответ на команду, которую человек сам набрал.
    /// Настройки режима СЕРВЕРА (свой координатор). Тот же бинарь `bemyvpn`
    /// поднимает координатор командой `server` или из меню — отдельного бинаря нет.
    pub server: ServerConfig,
    /// Файл, ИЗ КОТОРОГО прочитан этот конфиг. Сохраняем обратно ТУДА ЖЕ.
    ///
    /// Без этого правки терялись: читали по приоритету (--config, env,
    /// ./bemyvpn.toml, ~/.config/…), а писали ВСЕГДА в ~/.config. На сервере
    /// рядом с бинарём лежит bemyvpn.toml, он приоритетнее — поэтому меню
    /// сохраняло в один файл, а при следующем запуске читало другой, и человек
    /// видел, что настройки «не меняются».
    ///
    /// В сам TOML не пишется (skip): путь — свойство загрузки, а не настройка.
    #[serde(skip)]
    pub source: Option<PathBuf>,
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
    // Здесь был `dns: String` ("tunnel" | "1.1.1.1" | "system") — объявленный,
    // задокументированный и НИГДЕ не читаемый. DNS назначается в каждой оболочке
    // и всегда одинаково — 8.8.8.8: RouteGuard (три ветки: linux/macos/windows),
    // NEDNSSettings на iOS, addDnsServer на Android. То есть "system" НЕ отключал подмену, а
    // "1.1.1.1" НЕ менял сервер — ручка врала обоими значениями сразу.
    //
    // Убрана, а не реализована, по двум причинам. Первая: "system" — это ручка,
    // ОТКЛЮЧАЮЩАЯ защиту. Резолв ушёл бы к DNS провайдера мимо туннеля, то есть
    // провайдер снова видел бы список посещаемых сайтов, а на экране висело бы
    // «Защищено». Ровно тот тихий дефект, из-за которого убран `kill_switch`,
    // только вывернутый наизнанку. Вторая: конфига НЕТ на двух оболочках из
    // четырёх — iOS получает настройки через providerConfiguration, Android через
    // intent-extras, файла bemyvpn.toml у них нет вовсе. «Провести значение до
    // всех» означало бы новый FFI-параметр плюс экран настройки в каждом из двух
    // приложений — это отдельная фича, а не починка. Сделать наполовину (работает
    // на десктопе, молчит на телефоне) — снова обещание, которого нет.
    //
    // Выбор сервера (8.8.8.8 → 1.1.1.1) сам по себе безвреден, но и бесполезен:
    // оба запроса идут через туннель и резолвит их сеть хоста. Понадобится защита
    // от подмены DNS на стороне хоста — это DoH/DoT в `bmv-tunnel`, а не строчка
    // в конфиге гостя.
    //
    // Здесь был `kill_switch: bool` — объявленный, задокументированный и НИГДЕ не
    // реализованный. Защитная ручка, которая ничего не делает, хуже отсутствующей:
    // человек её включает и считает себя защищённым. Настоящий рубильник — это
    // правила брандмауэра на десктопе (и застрявшее правило оставит машину вообще
    // без сети), а на Android приложение его в принципе не включит: это системная
    // настройка «блокировать соединения без VPN». Делать — отдельным заходом и
    // целиком, а не половиной. Старые конфиги со строкой kill_switch читаются
    // по-прежнему: неизвестные поля serde молча пропускает.
    /// Что делать с IPv6 на время сеанса: `"block"` (по умолчанию) | `"allow"`.
    /// Разбирается через [`Ipv6Mode::parse`] — см. там, почему это не косметика.
    pub ipv6: String,
    // Здесь был `auto_reconnect: bool` (дефолт `true`) — тоже никем не читаемый,
    // и вдобавок описывавший поведение не той оболочки. Авто-реконнекта на
    // десктопе НЕТ: `bmv_desktop::tunnel::run_tunnel` при обрыве завершается в
    // `State::Off`, и переподключение — это кнопка «Старт». Цикл живёт в
    // `bmv-ffi` (мобильные оболочки: машина, метро, смена сети), включён всегда
    // и конфига не читает. То есть ручка стояла в файле, которого нет на той
    // единственной платформе, где есть само поведение.
    //
    // Отключать авто-реконнект незачем: туннель рвётся не по желанию человека, а
    // от сети. Понадобится «не поднимать сам» — это переключатель в UI приложения
    // рядом с кнопкой, а не ключ в TOML, до которого с телефона не добраться.
}

/// Что делать с IPv6, пока поднят туннель.
///
/// ЗАЧЕМ ЭТО ВООБЩЕ ЕСТЬ. Туннель у нас несёт ТОЛЬКО IPv4. Если у провайдера
/// есть IPv6 и его не тронуть, часть трафика (а на dual-stack сайтах — почти
/// весь, потому что клиенты предпочитают v6) уходит МИМО туннеля напрямую:
/// приложение показывает «Защищено», а настоящий адрес человека виден сайтам.
/// Утечка тихая — сам он её никогда не заметит.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ipv6Mode {
    /// Заглушить IPv6 на время туннеля. Приложения мгновенно откатываются на
    /// IPv4 (см. `RouteGuard`: маршруты ставятся отвергающие, а не «в никуда»,
    /// поэтому соединение не висит до таймаута, а сразу пробует IPv4).
    Block,
    /// НЕ трогать IPv6 — честное имя того, что при этом происходит: v6-трафик
    /// идёт мимо туннеля и утекает. Нужно там, где IPv6 — ЕДИНСТВЕННЫЙ
    /// транспорт (часть мобильных сетей, NAT64/464XLAT): глухая блокировка
    /// оставит человека не «без VPN», а вообще без интернета.
    Allow,
}

impl Ipv6Mode {
    /// Разбор значения из конфига. Всё, что не `"allow"`, — это `Block`.
    ///
    /// Падать в БЕЗОПАСНУЮ сторону обязательно: опечатка («blok», «blocked»)
    /// или значение из старой документации не должны молча открывать утечку.
    /// Сюда же попадает `"route"` — он был задокументирован, но НИКОГДА не был
    /// реализован (туннель IPv6 не несёт: у интерфейса гостя нет v6-адреса, а
    /// `bmv_tunnel::from_host_allowed` режет v6 на обратном пути), поэтому его
    /// значение = «блокировать», а не «вести».
    pub fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("allow") {
            Ipv6Mode::Allow
        } else {
            Ipv6Mode::Block
        }
    }

    /// ТРЕБУЕТ ЛИ ЭТОТ РЕЖИМ БЛОКИРОВКИ — единственный ответ на этот вопрос.
    ///
    /// Спрашивают его ДВОЕ и в разные стороны: `apply_ipv6_policy` — «ставить
    /// ли?», а `Drop` у `RouteGuard` — «снимать ли?». Пока это были два ручных
    /// сравнения с вариантами (одно с `Allow`, другое с `Block`), они
    /// расходились молча: третий режим получил бы блокировку при подключении и
    /// не получил бы снятия при выключении — человек остался бы без IPv6 до
    /// перезагрузки, а под Windows его некому даже подобрать после падения.
    ///
    /// `match` тут НАРОЧНО вместо `matches!` или `!= Allow`: новый вариант
    /// обязан уронить СБОРКУ ровно здесь, а не утечь мимо туннеля.
    pub fn needs_block(self) -> bool {
        match self {
            Ipv6Mode::Block => true,
            Ipv6Mode::Allow => false,
        }
    }
}

impl GuestConfig {
    /// Режим IPv6 из конфига (разобранный, а не строкой).
    pub fn ipv6_mode(&self) -> Ipv6Mode {
        Ipv6Mode::parse(&self.ipv6)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    // Здесь был `enabled: bool` — тоже без читателей. Раздача НИКОГДА не
    // стартовала по конфигу: её включает человек — подкоманда `bemyvpn host`,
    // кнопка в TUI/GUI, `bmv_host_start` через FFI на iOS и Android (там
    // состояние вообще живёт в prefs, а файла конфига нет). Ключ обещал
    // автозапуск при старте приложения, которого не было: поставив
    // `enabled = true`, человек получал ровно ничего.
    //
    // Автозапуск раздачи — это не строка в конфиге, а служба системы (systemd на
    // сервере) плюс отдельное решение, что делать с паролем и токеном владения
    // при старте без человека. Понадобится — заводить целиком, а не флагом.
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

// ── дефолты (единственное место) ────────────────────────────────────────────

impl Default for Config {
    fn default() -> Self {
        Config {
            // Референс-координатор сообщества (каталог хостов). Работает из
            // коробки; поставь свой одной строкой в bemyvpn.toml или подними
            // собственный (см. README → «Свой сервер»). Не захардкожено в логике —
            // это лишь дефолт, любой адрес переопределяет.
            coordinators: vec!["https://bemyvpn.net".into()],
            default_protocol: DEFAULT_PROTOCOL.into(), // см. константу выше
            guest: GuestConfig::default(),
            host: HostConfig::default(),
            source: None,
            stun: StunConfig::default(),
            server: ServerConfig::default(),
        }
    }
}

impl Default for GuestConfig {
    fn default() -> Self {
        GuestConfig {
            ipv6: "block".into(),
        }
    }
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
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

// ── загрузка ────────────────────────────────────────────────────────────────

impl Config {
    /// Загрузить конфиг, разрешая путь по правилам поиска.
    pub fn load(explicit: Option<&Path>) -> Result<Config> {
        match resolve_path(explicit) {
            Some(path) => {
                let mut c = Self::from_file(&path)?;
                c.source = Some(path); // сохранять будем СЮДА ЖЕ
                Ok(c)
            }
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

    /// Где лежит конфиг по тем же правилам поиска, что и у [`Config::load`],
    /// БЕЗ чтения и разбора. `None` — файла нет вовсе, значит правда в умолчаниях.
    ///
    /// Нужно тому, кто конфиг читать будет НЕ САМ: GUI поднимает туннель отдельным
    /// root-процессом, а у того HOME чужой (`/var/root`, `/root`), и автопоиск в нём
    /// нашёл бы не тот файл или ничего. Путь ищет сторона пользователя и передаёт
    /// готовым (см. `spawn_and_connect` в apps/bmv-gui/src/helper.rs).
    pub fn path() -> Option<PathBuf> {
        resolve_path(None)
    }

    /// Сохранить конфиг в пользовательский путь (создаёт папку). Меню зовёт это
    /// при каждом изменении — руками .toml править не нужно.
    pub fn save(&self) -> Result<()> {
        // Туда, откуда прочитали. Нового конфига ещё нет — в пользовательский путь.
        let path = self.source.clone().unwrap_or_else(Self::user_path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Config(format!("{}: {e}", dir.display())))?;
        }
        std::fs::write(&path, self.to_toml()).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        // Только владельцу: в файле лежат пароль хоста и token владения записью в
        // каталоге. С правами по умолчанию (0644) их прочитал бы ЛЮБОЙ пользователь
        // машины — на общем сервере это выдача чужого хоста вместе с паролем.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
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
/// Считаем от ОЗУ, а не от ядер. Процессор тут почти не при чём: ChaCha20 на
/// современном ядре молотит гигабайты в секунду, а гости почти всё время
/// простаивают — один vCPU спокойно тянет десятки человек. Ограничивает память
/// В ПИКЕ: у каждого TCP-соединения окно до 1 МБ (`BMV_TCP_WINDOW`), причём это
/// ПОТОЛОК, а не занятая память — `Tcb` заводит пустые BTreeMap и растёт только
/// под реальной нагрузкой. Поэтому считаем по бюджету «~8 МБ на гостя в пике»,
/// оставив 256 МБ системе и самому приложению.
///
/// Это стартовая оценка, а не измерение: настоящий потолок задаёт ширина канала,
/// её без прогона трафика не узнать. В меню значение меняется одним нажатием.
pub fn suggested_max_guests() -> u32 {
    let raw = match total_ram_mb() {
        Some(mb) => (mb.saturating_sub(256) / 8).clamp(4, 128) as u32,
        // ОЗУ не определили — берём умеренно, как для машины на ~1 ГБ.
        None => 64,
    };
    // Округляем ВНИЗ до значения из набора пресетов интерфейса: промежуточное
    // число не подсветит ни одну кнопку. Вниз, а не вверх — занизить безопаснее.
    match raw {
        128.. => 128,
        64..=127 => 64,
        32..=63 => 32,
        16..=31 => 16,
        8..=15 => 8,
        _ => 4,
    }
}

/// Всего оперативной памяти, МБ. None — определить не удалось.
fn total_ram_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        return Some(kb / 1024);
    }
    #[cfg(target_os = "macos")]
    {
        let mut bytes: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        let name = c"hw.memsize";
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                &mut bytes as *mut u64 as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        return (rc == 0 && bytes > 0).then_some(bytes / 1024 / 1024);
    }
    #[allow(unreachable_code)]
    None
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
        assert_eq!(c.default_protocol, DEFAULT_PROTOCOL);
        // Лимит подбирается от числа ядер, поэтому проверяем не число, а рамки:
        // не ниже минимума и не выше потолка, и всегда одно из значений пресетов.
        assert!((4..=128).contains(&c.host.max_guests), "лимит вне рамок: {}", c.host.max_guests);
        assert_eq!(c.host.max_guests, suggested_max_guests());
    }

    /// НЕПОНЯТОЕ ЗНАЧЕНИЕ `guest.ipv6` ОБЯЗАНО ЗНАЧИТЬ «БЛОКИРОВАТЬ».
    ///
    /// Ручка защитная: если опечатка или значение из старой документации молча
    /// прочитаются как «не трогать», человек получит «Защищено» на экране и
    /// утечку настоящего адреса по IPv6 — ровно тот тихий дефект, ради которого
    /// эта настройка и заведена. Поэтому «allow» — единственный способ отказаться
    /// от защиты, и он должен быть написан явно.
    #[test]
    fn anything_but_an_explicit_allow_means_block() {
        assert_eq!(Ipv6Mode::parse("allow"), Ipv6Mode::Allow, "явный отказ обязан работать");
        assert_eq!(Ipv6Mode::parse("  ALLOW  "), Ipv6Mode::Allow, "регистр и пробелы не должны мешать");

        assert_eq!(Ipv6Mode::parse("block"), Ipv6Mode::Block);
        // «route» жил в документации и НИКОГДА не был реализован — он значит block.
        assert_eq!(Ipv6Mode::parse("route"), Ipv6Mode::Block, "невыполненное обещание не должно снимать защиту");
        assert_eq!(Ipv6Mode::parse(""), Ipv6Mode::Block, "пустое значение");
        assert_eq!(Ipv6Mode::parse("allowed"), Ipv6Mode::Block, "похожее, но другое слово");
        assert_eq!(Ipv6Mode::parse("all"), Ipv6Mode::Block, "префикс «allow»");
        assert_eq!(Ipv6Mode::parse("да"), Ipv6Mode::Block, "мусор");

        // Умолчание конфига — тоже защита, а не «как получится».
        assert_eq!(GuestConfig::default().ipv6_mode(), Ipv6Mode::Block);
        assert_eq!(Config::default().guest.ipv6_mode(), Ipv6Mode::Block);
    }

    /// Авто-сохранение: save() → load обратно с теми же значениями, и файл виден
    /// ТОЛЬКО владельцу.
    ///
    /// Изолированно, без окружения: раньше тест подменял XDG_CONFIG_HOME, а это
    /// переменная всего ПРОЦЕССА — соседние тесты, идущие параллельно, на это
    /// время считали своим конфигом временную папку этого теста. Здесь путь задан
    /// прямо через `source` (тот же код сохранения, что и в бою).
    #[test]
    fn save_roundtrip_and_owner_only_permissions() {
        let dir = std::env::temp_dir().join(format!("bmv-cfg-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        let path = dir.join("config.toml");
        let mut c = Config {
            default_protocol: DEFAULT_PROTOCOL.into(),
            source: Some(path.clone()),
            ..Default::default()
        };
        c.host.max_guests = 42;
        c.save().unwrap();
        let loaded = Config::from_file(&path).unwrap();
        assert_eq!(loaded.default_protocol, DEFAULT_PROTOCOL);
        assert_eq!(loaded.host.max_guests, 42);
        // В конфиге лежат пароль хоста и token владения записью в каталоге —
        // читать его должен ТОЛЬКО владелец, иначе на общей машине сосед забирает
        // и то, и другое.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "конфиг с паролем доступен не только владельцу: {mode:o}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Пользовательский путь: имя файла и папка — те, что ждёт остальной проект
    /// (сюда пишет авто-сохранение, отсюда читает старт).
    #[test]
    fn user_path_points_into_bemyvpn_dir() {
        let p = Config::user_path();
        assert!(p.ends_with("bemyvpn/config.toml"), "необычный путь конфига: {}", p.display());
        assert!(p.is_absolute() || p.starts_with("."), "путь ниоткуда: {}", p.display());
    }


    /// Конфиг сохраняется ТУДА, ОТКУДА прочитан.
    ///
    /// Раньше читали по приоритету, а писали всегда в ~/.config — и если рядом
    /// лежал bemyvpn.toml (на сервере он именно так и лежит), правки из меню
    /// уходили в другой файл, а при следующем запуске читался прежний. Человек
    /// менял настройку и видел, что «ничего не меняется».
    #[test]
    fn saves_back_to_the_file_it_was_loaded_from() {
        let dir = std::env::temp_dir().join(format!("bmv-src-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bemyvpn.toml");
        std::fs::write(&path, "default_protocol = \"plain\"\n").unwrap();

        let mut c = Config::load(Some(&path)).unwrap();
        assert_eq!(c.source.as_deref(), Some(path.as_path()), "источник не запомнен");
        c.default_protocol = "noise-obfs".into();
        c.save().unwrap();

        // Изменение обязано оказаться В ТОМ ЖЕ файле, а не в пользовательском.
        let again = Config::load(Some(&path)).unwrap();
        assert_eq!(again.default_protocol, "noise-obfs", "правка не вернулась из своего файла");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Нового конфига ещё нет — сохраняем в пользовательский путь, как раньше.
    #[test]
    fn without_source_saves_to_user_path() {
        let c = Config::default();
        assert!(c.source.is_none());
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let c = Config::from_toml("default_protocol = \"plain\"").unwrap();
        assert_eq!(c.default_protocol, "plain");
        // остальное — дефолты
        assert_eq!(c.guest.ipv6, "block");
    }

    /// СТАРЫЙ КОНФИГ ОБЯЗАН ЧИТАТЬСЯ ПОСЛЕ УБОРКИ МЁРТВЫХ РУЧЕК.
    ///
    /// Здесь лежит НАСТОЯЩИЙ файл из документации (docs/ARCHITECTURE.md, раздел
    /// «ОДИН конфиг») — тот, что люди копировали к себе целиком. В нём разом все
    /// убранные ключи: `guest.dns`, `guest.auto_reconnect`, `kill_switch`,
    /// `host.enabled`, секции `[protocols.reality]`, `[protocols.wireguard]` и
    /// `[log]`. Убрать поле из структуры безопасно ровно до тех пор, пока serde
    /// молча пропускает незнакомые ключи: стоит кому-нибудь дописать сюда
    /// `deny_unknown_fields` — и у этих людей приложение перестанет стартовать
    /// на строках, которые и раньше ничего не делали. Тест держит эту дверь.
    ///
    /// Заодно проверяем ЖИВЫХ соседей — и по секции (`guest.ipv6` рядом с
    /// выкинутыми `dns`/`auto_reconnect`, `host.max_guests` рядом с `enabled`), и
    /// целой секцией после выкинутых (`[stun]` идёт следом за `[protocols.*]`):
    /// удаление полей не должно сдвинуть разбор остального.
    #[test]
    fn the_documented_old_config_still_loads_whole() {
        let c = Config::from_toml(
            r#"
# ─────────────── BeMyVPN — единственный конфиг ───────────────
coordinators = [
  "https://coord.bemyvpn.org",
]
default_protocol = "reality"          # plain | wireguard | webrtc | reality

[guest]                                # когда я подключаюсь
dns          = "tunnel"                # tunnel (по умолч., принудительно) | 1.1.1.1 | system
kill_switch  = true
ipv6         = "allow"                 # block | allow
auto_reconnect = true

[host]                                 # когда я раздаю интернет
enabled      = false
public       = false
password     = ""
max_guests   = 4
country_hint = "auto"

[protocols.reality]
sing_box_path = ""
sni           = "www.mi.com"

[protocols.wireguard]

[stun]
servers = ["stun.example.org:3478"]

[log]
level = "info"                         # error | warn | info | debug
file  = "bemyvpn.log"
"#,
        )
        .expect("документированный конфиг обязан читаться, а не ронять запуск");

        // Живое до выкинутых ключей.
        assert_eq!(c.coordinators, vec!["https://coord.bemyvpn.org".to_string()]);
        assert_eq!(c.default_protocol, "reality");
        // Живой сосед по [guest] — между выкинутыми dns/kill_switch и auto_reconnect.
        assert_eq!(c.guest.ipv6_mode(), Ipv6Mode::Allow, "ipv6 потерялся среди мусора");
        // Живые соседи по [host] — сразу после выкинутого enabled.
        assert_eq!(c.host.max_guests, 4);
        assert_eq!(c.host.country_hint, "auto");
        // Целая живая секция ПОСЛЕ выкинутых [protocols.*].
        assert_eq!(c.stun.servers, vec!["stun.example.org:3478".to_string()]);
    }

    /// МЁРТВАЯ НАСТРОЙКА НЕ ДОЛЖНА ПЕРЕЖИТЬ СБОРКУ.
    ///
    /// За одну сессию разбора в конфиге нашлось ШЕСТЬ ручек без единого читателя,
    /// две из них — защитные (`kill_switch`, `guest.dns`), то есть обещавшие
    /// защиту, которой нет. Ловить такое глазами раз в полгода — не механизм.
    ///
    /// Компилятор здесь не помощник: поля `pub` в библиотечном крейте считаются
    /// частью публичного API, а `derive(Serialize)` читает КАЖДОЕ поле, поэтому
    /// `dead_code` молчит по построению. Поэтому проверка простая и грубая: взять
    /// имена полей из ЭТОГО же файла (`include_str!` — список не ведётся руками и
    /// не устареет) и убедиться, что каждое имя где-то читается как `.имя`.
    ///
    /// Читателем считается ТОЛЬКО боевой код: у каждого файла отрезается всё от
    /// `#[cfg(test)]`, каталоги `tests/` пропускаются целиком. Иначе ручку
    /// «оживлял» бы её собственный тест — ровно так `guest.dns` и держался
    /// живым на вид. Зато свой же `impl` — читатель настоящий: `stun.file`
    /// никто снаружи не трогает, его читает `StunConfig::resolve`, и это
    /// нормально.
    ///
    /// ЧЕСТНО ПРО ПОТОЛОК: это грубый текстовый поиск. Он надёжно ловит новую
    /// ручку со своим именем (`kill_switch`, `auto_reconnect`, `sing_box_path`,
    /// `max_guests`) и НЕ ловит ту, чьё имя — частое слово (`file`, `level`,
    /// `name`): у неё найдётся случайный однофамилец в чужом коде, и тест
    /// промолчит. Ошибка только в сторону «пропустил», не в сторону ложной
    /// тревоги — то есть хуже сегодняшнего не станет. Настоящая непроходимость
    /// (поле физически не существует без читателя) стоит proc-macro и перевода
    /// всего конфига на методы-читатели вместо `pub`-полей; за шесть находок это
    /// не окупается.
    #[test]
    fn every_knob_is_read_by_someone() {
        let me = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = me.parent().and_then(|p| p.parent()).expect("корень репозитория");

        // Имена полей всех структур конфига — из собственного исходника.
        let src = include_str!("lib.rs");
        let knobs: Vec<&str> = src
            .lines()
            .map(str::trim)
            .filter_map(|l| l.strip_prefix("pub "))
            .filter_map(|l| l.split_once(':'))
            // `pub fn`/`pub struct`/`pub enum` сюда не попадают: у них нет `:` до `(`.
            .filter(|(name, _)| !name.contains(' ') && !name.contains('('))
            .map(|(name, _)| name)
            // `source` живёт под `#[serde(skip)]` и читается внутри самого
            // bmv-config (`save`) — искать его снаружи бессмысленно.
            .filter(|name| *name != "source")
            .collect();
        assert!(knobs.len() > 10, "разбор полей сломался, нашлось всего {}", knobs.len());

        let mut haystack = String::new();
        for dir in ["crates", "apps"] {
            collect_rust_sources(&root.join(dir), &mut haystack);
        }
        assert!(haystack.len() > 100_000, "исходники не собрались: {} байт", haystack.len());

        let dead: Vec<&str> = knobs
            .iter()
            .filter(|k| !haystack.contains(&format!(".{k}")))
            .copied()
            .collect();
        assert!(
            dead.is_empty(),
            "настройки объявлены, но их никто не читает: {dead:?}\n\
             Ручка без читателя — обещание, которого нет. Либо подключи её к делу, \
             либо убери из конфига (см. историю kill_switch и guest.dns)."
        );
    }

    /// Сложить БОЕВОЙ код (все .rs из `dir`) в одну строку: у каждого файла
    /// отрезаем всё от `#[cfg(test)]`, каталоги `tests`/`target`/`vendor`
    /// пропускаем целиком. Тест — не читатель: ручка, которую трогает только
    /// её собственная проверка, обязана считаться мёртвой.
    fn collect_rust_sources(dir: &std::path::Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n == "target" || n == "vendor" || n == "tests") {
                continue;
            }
            if p.is_dir() {
                collect_rust_sources(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    out.push_str(text.split("#[cfg(test)]").next().unwrap_or(""));
                }
            }
        }
    }
}
