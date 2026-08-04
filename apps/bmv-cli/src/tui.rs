//! Полноэкранное TUI-меню `bemyvpn` — паритет с iOS-приложением в терминале.
//! Три вкладки Сервер / VPN / Хост, живое управление всем (имя/лимит/пароль/
//! протокол/видимость хоста, подключение по коду, смена координатора). Аргументы
//! командной строки лишь ПРЕД-НАСТРАИВАЮТ это же меню (см. Seed). НАСТОЯЩИЙ VPN:
//! при подключении весь трафик заворачивается в туннель (bmv-desktop RouteGuard).
//!
//! Всё async: фон обновляет каталог/связь/свой IP, главный цикл рисует и ловит
//! клавиши; подключение/раздача/сервер — отдельные задачи, статус в общий App.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bmv_common::view::{self, Alarm};
use bmv_config::Config;
use bmv_core::BmvEngine;
use bmv_signal::HostInfo;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use futures::StreamExt;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

/// Предустановка меню из аргументов CLI (`bemyvpn host --max 16` и т.п.). Меню
/// открывается уже с этими значениями (и, при auto_start, сразу запущенное).
#[derive(Default, Clone)]
pub struct Seed {
    pub tab: Option<&'static str>,          // "server" | "vpn" | "host"
    pub host_name: Option<String>,
    pub host_max: Option<u32>,
    pub host_password: Option<String>,
    pub host_protocol: Option<String>,
    pub host_public: Option<bool>,
    pub host_auto_start: bool,               // сразу начать раздачу
    pub vpn_connect: Option<String>,         // id/код хоста — сразу подключиться
    pub vpn_password: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Server,
    Vpn,
    Host,
}
impl Tab {
    const ALL: [Tab; 3] = [Tab::Server, Tab::Vpn, Tab::Host];
    fn idx(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap()
    }
    fn title(self) -> &'static str {
        match self {
            Tab::Server => "  🌐 Сервер  ",
            Tab::Vpn => "  🔒 VPN  ",
            Tab::Host => "  📡 Хост  ",
        }
    }
}

enum Vpn {
    Off,
    Connecting(String),
    On { id: String, name: String, since: Instant },
    /// Хост сам погасил раздачу. Отдельно от `Off` и от `Failed`: выключили не
    /// мы, но и не сломалось ничего — сеанс просто окончен, и надо взять другой
    /// хост. С `Off` это выглядело как VPN, погасший сам по себе.
    Ended,
    Failed(String),
}

enum HostMode {
    Off,
    Starting,
    On { code: String },
    Failed(String),
}

enum SrvState {
    Off,
    On,
    Failed(String),
}

/// Живые настройки хоста (как вкладка «Хост» на iOS). Меняются на ходу — если
/// раздача активна, применяются к живому движку сразу; иначе — при старте.
#[derive(Clone)]
struct HostSettings {
    name: String,
    max_guests: u32,
    password: String,
    protocol: String, // "noise" | "noise-obfs" | "plain"
    public: bool,
}
const PROTOCOLS: [&str; 3] = ["noise", "noise-obfs", "plain"];
/// Строки-поля вкладки «Хост» (по ним ходит курсор ↑↓).
const HOST_FIELDS: usize = 6; // Имя, Лимит, Пароль, Протокол, Видимость, [Действие]
/// Строки-поля вкладки «Сервер»: Координатор, Домен, Порт, [Старт/стоп].
const SRV_FIELDS: usize = 4;

/// Модальный текстовый ввод (пароль гостя / код / имя хоста / пароль хоста /
/// адрес координатора) — один механизм на все случаи.
enum InputKind {
    GuestPassword(HostInfo),
    ConnectCode,
    HostName,
    HostMaxGuests,
    HostPassword,
    Coordinator,
    SrvDomain,
    SrvBind,
}
struct Input {
    kind: InputKind,
    buffer: String,
    masked: bool,
}
impl Input {
    fn title(&self) -> &'static str {
        match self.kind {
            InputKind::GuestPassword(_) => " 🔒 Пароль хоста ",
            InputKind::ConnectCode => " Подключиться по коду ",
            InputKind::HostName => " Имя хоста ",
            InputKind::HostMaxGuests => " Лимит гостей (число) ",
            InputKind::HostPassword => " Пароль на раздачу ",
            InputKind::Coordinator => " Адрес координатора ",
            InputKind::SrvDomain => " Домен своего сервера (пусто = HTTP) ",
            InputKind::SrvBind => " Порт/адрес прослушивания ",
        }
    }
}

struct App {
    tab: Tab,
    hosts: Vec<HostInfo>,
    sel: usize,
    vpn: Vpn,
    host: HostMode,
    hset: HostSettings,
    host_field: usize,
    host_code: String, // выданный сервером код (виден и до старта раздачи)
    host_sig: String,  // подпись кода сервером — координатор требует её при анонсе
    host_started: Option<Instant>,
    show_qr: bool,
    coord: String,
    coord_ok: Option<bool>,
    coord_ping: u32,
    /// Отклик до ВЫБРАННОГО хоста: (id, Some(мс) — ответил · None — молчит).
    /// Ключ по id, а не по номеру строки: список пересобирается фоном, и цифра
    /// от прежнего хоста осталась бы висеть под новым.
    host_ping: Option<(String, Option<u32>)>,
    my_ip: String,
    srv: SrvState,
    srv_cfg: bmv_config::ServerConfig,
    srv_field: usize,        // курсор ↑↓ по вкладке «Сервер»
    auto_srv: Option<bool>,  // автозапуск координатора (systemd); None — не Linux
    auto_host: Option<bool>, // автозапуск хоста
    input: Option<Input>,
    toast: Option<(String, Instant)>,
    quit: bool,
}

impl App {
    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }
}

type Shared = Arc<Mutex<App>>;
/// Движок в слоте — чтобы менять координатор на лету (пересоздаём движок).
type EngineSlot = Arc<Mutex<Arc<BmvEngine>>>;
/// Живой движок раздачи — для host_set_* на ходу.
type HostEngine = Arc<Mutex<Option<Arc<BmvEngine>>>>;
/// Задача гостя + её «стоп». Отдельно от JoinHandle, потому что просто оборвать
/// задачу нельзя: перед уходом гость обязан попрощаться (BYE), иначе хост держит
/// его в счётчике ещё 8 секунд до keepalive-таймаута.
type VpnTask = Option<(tokio::task::JoinHandle<()>, Arc<tokio::sync::Notify>)>;

pub async fn run(config: Config, seed: Seed) -> Result<(), Box<dyn std::error::Error>> {
    let engine: EngineSlot = Arc::new(Mutex::new(Arc::new(BmvEngine::from_config(config.clone()))));
    let coord = config.coordinators.first().cloned().unwrap_or_default();

    // Стартовые настройки хоста: конфиг, поверх — аргументы (Seed).
    let hset = HostSettings {
        name: seed
            .host_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| Some(config.host.name.clone()).filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(default_host_name),
        max_guests: seed.host_max.unwrap_or(config.host.max_guests.max(1)),
        password: seed.host_password.clone().unwrap_or_else(|| config.host.password.clone()),
        protocol: seed
            .host_protocol
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| {
                let p = config.default_protocol.clone();
                if p.is_empty() { "noise-obfs".into() } else { p }
            }),
        public: seed.host_public.unwrap_or(config.host.public),
    };

    let start_tab = match seed.tab {
        Some("server") => Tab::Server,
        Some("host") => Tab::Host,
        _ => Tab::Vpn,
    };

    let app: Shared = Arc::new(Mutex::new(App {
        tab: start_tab,
        hosts: Vec::new(),
        sel: 0,
        vpn: Vpn::Off,
        host: HostMode::Off,
        hset,
        host_field: 0,
        host_code: config.host.id.clone(),
        host_sig: config.host.code_sig.clone(),
        host_started: None,
        show_qr: false,
        coord,
        coord_ok: None,
        coord_ping: 0,
        host_ping: None,
        my_ip: String::new(),
        srv: SrvState::Off,
        srv_cfg: config.server.clone(),
        srv_field: 0,
        auto_srv: None, // заполним фоном (systemctl не в потоке событий — без фриза старта)
        auto_host: None,
        input: None,
        toast: None,
        quit: false,
    }));

    let host_engine: HostEngine = Arc::new(Mutex::new(None));

    // Состояние автозапуска (systemctl) — фоном, чтобы не тормозить старт TUI.
    {
        let app2 = app.clone();
        std::thread::spawn(move || {
            let (s, h) = (autostart_state(false), autostart_state(true));
            let mut a = app2.lock().unwrap();
            a.auto_srv = s;
            a.auto_host = h;
        });
    }

    // Фон: каждые ~3с каталог + связь + свой IP.
    spawn_refresh(engine.clone(), app.clone());
    // Код хоста запрашиваем сразу, чтобы был виден во вкладке «Хост» (как iOS onAppear).
    ensure_host_code(&engine, &app);

    let mut vpn_task: VpnTask = None;
    let mut host_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut srv_task: Option<tokio::task::JoinHandle<()>> = None;

    // Предустановка из аргументов: авто-старт раздачи / авто-подключение.
    if seed.host_auto_start {
        start_host(&engine, &app, &host_engine, &mut host_task);
    }
    if let Some(code) = seed.vpn_connect.clone() {
        let host = HostInfo { id: code.clone(), name: code, ..Default::default() };
        connect_to(&engine, &app, &mut vpn_task, host, seed.vpn_password.clone());
    }

    let mut terminal = ratatui::init();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    // Ошибку отрисовки НЕЛЬЗЯ отдавать через `?` прямо отсюда: выход мимо
    // `ratatui::restore()` оставляет терминал в альтернативном буфере с выключенным
    // эхом — человек получает «сломанный» терминал и вслепую набирает `reset`.
    let mut draw_err = None;
    loop {
        {
            let a = app.lock().unwrap();
            if let Err(e) = terminal.draw(|f| ui(f, &a)) {
                draw_err = Some(e);
                break;
            }
            if a.quit {
                break;
            }
        }
        tokio::select! {
            maybe = events.next() => {
                if let Some(Ok(Event::Key(k))) = maybe {
                    if k.kind == KeyEventKind::Press {
                        handle_key(k.code, &engine, &app, &host_engine, &mut vpn_task, &mut host_task, &mut srv_task);
                    }
                }
            }
            _ = tick.tick() => {}
        }
    }

    // Красивый выход. Гостю сначала даём попрощаться (BYE) — иначе хост держит
    // нас в счётчике ещё 8 секунд, — и только потом рвём остальное: Drop снимает
    // TUN и откатывает маршруты.
    if let Some((mut h, stop)) = vpn_task.take() {
        stop.notify_waiters();
        if tokio::time::timeout(Duration::from_secs(3), &mut h).await.is_err() {
            h.abort();
        }
    }
    for h in [host_task.take(), srv_task.take()].into_iter().flatten() {
        h.abort();
    }
    // Снять запись хоста из каталога (bye), если раздавали. Гард мьютекса НЕ
    // держим через await — сначала забираем движок, потом ждём.
    let heng = host_engine.lock().unwrap().take();
    if let Some(eng) = heng {
        let _ = tokio::time::timeout(Duration::from_secs(2), eng.host_deannounce()).await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    ratatui::restore();
    if let Some(e) = draw_err {
        return Err(e.into());
    }
    Ok(())
}

// ── обработка клавиш ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn handle_key(
    code: KeyCode,
    engine: &EngineSlot,
    app: &Shared,
    host_engine: &HostEngine,
    vpn_task: &mut VpnTask,
    host_task: &mut Option<tokio::task::JoinHandle<()>>,
    srv_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    // Модальный ввод перехватывает все клавиши.
    if app.lock().unwrap().input.is_some() {
        handle_input_key(code, engine, app, host_engine, vpn_task);
        return;
    }
    let tab = app.lock().unwrap().tab;
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.lock().unwrap().quit = true,
        // ←→/Tab переключают вкладки НА ВСЕХ вкладках одинаково (в т.ч. Хост).
        // Значения полей Хоста меняются через Enter (число лимита — вводом).
        KeyCode::Tab | KeyCode::Right => {
            let mut a = app.lock().unwrap();
            a.tab = Tab::ALL[(a.tab.idx() + 1) % Tab::ALL.len()];
        }
        KeyCode::BackTab | KeyCode::Left => {
            let mut a = app.lock().unwrap();
            a.tab = Tab::ALL[(a.tab.idx() + Tab::ALL.len() - 1) % Tab::ALL.len()];
        }
        _ => match tab {
            Tab::Vpn => vpn_key(code, engine, app, vpn_task),
            Tab::Host => host_key(code, engine, app, host_engine, host_task),
            Tab::Server => server_key(code, engine, app, srv_task),
        },
    }
}

fn handle_input_key(
    code: KeyCode,
    engine: &EngineSlot,
    app: &Shared,
    host_engine: &HostEngine,
    vpn_task: &mut VpnTask,
) {
    match code {
        KeyCode::Esc => {
            app.lock().unwrap().input = None;
        }
        KeyCode::Enter => {
            let inp = app.lock().unwrap().input.take();
            let Some(inp) = inp else { return };
            let buf = inp.buffer;
            match inp.kind {
                InputKind::GuestPassword(host) => {
                    let pw = (!buf.is_empty()).then_some(buf);
                    connect_to(engine, app, vpn_task, host, pw);
                }
                InputKind::ConnectCode => {
                    let code = buf.trim().to_uppercase();
                    if !code.is_empty() {
                        // Если код есть в каталоге — берём протокол оттуда.
                        let known = app.lock().unwrap().hosts.iter().find(|h| h.id == code).cloned();
                        let host = known.unwrap_or(HostInfo { id: code.clone(), name: code, ..Default::default() });
                        if host.has_password {
                            app.lock().unwrap().input = Some(Input { kind: InputKind::GuestPassword(host), buffer: String::new(), masked: true });
                        } else {
                            connect_to(engine, app, vpn_task, host, None);
                        }
                    }
                }
                InputKind::HostName => {
                    app.lock().unwrap().hset.name = buf.clone();
                    apply_host(host_engine, move |e| async move { let _ = e.host_set_name(&buf).await; });
                    app.lock().unwrap().toast("Имя обновлено");
                    persist(engine, app);
                }
                InputKind::HostMaxGuests => match buf.trim().parse::<u32>() {
                    Ok(n) => {
                        let val = n.clamp(1, 100_000);
                        app.lock().unwrap().hset.max_guests = val;
                        apply_host(host_engine, move |e| async move { let _ = e.host_set_max_guests(val).await; });
                        app.lock().unwrap().toast("Лимит обновлён");
                        persist(engine, app);
                    }
                    Err(_) => app.lock().unwrap().toast("Нужно число, например 8"),
                },
                InputKind::HostPassword => {
                    app.lock().unwrap().hset.password = buf.clone();
                    apply_host(host_engine, move |e| async move { let _ = e.host_set_password(&buf).await; });
                    app.lock().unwrap().toast("Пароль обновлён");
                    persist(engine, app);
                }
                InputKind::Coordinator => match view::coordinator_url(&buf) {
                    Some(url) => {
                        switch_coordinator(engine, app, url);
                        persist(engine, app);
                    }
                    // Раньше здесь требовалось набрать «https://» руками, и на
                    // «bemyvpn.net» человек получал отказ вместо адреса.
                    None => app.lock().unwrap().toast("Адрес пустой — введите адрес сервера"),
                },
                InputKind::SrvDomain => {
                    // Схему срезаем тем же правилом, что и на экране адреса.
                    let d = view::without_scheme(&buf);
                    {
                        let mut a = app.lock().unwrap();
                        a.srv_cfg.domain = d.clone();
                        // Домен задан → HTTPS:443, сертификат получится/продлится сам.
                        a.srv_cfg.bind = if d.is_empty() { "0.0.0.0:3330".into() } else { "0.0.0.0:443".into() };
                        a.toast(if d.is_empty() { "Домен убран — HTTP на :3330" } else { "HTTPS: сертификат получится сам (нужен DNS на этот сервер и открытый 443)" });
                    }
                    persist(engine, app);
                }
                InputKind::SrvBind => {
                    let b = buf.trim().to_string();
                    let full = if b.chars().all(|c| c.is_ascii_digit()) && !b.is_empty() { format!("0.0.0.0:{b}") } else { b };
                    if full.parse::<std::net::SocketAddr>().is_ok() {
                        app.lock().unwrap().srv_cfg.bind = full;
                        app.lock().unwrap().toast("Адрес прослушивания сохранён");
                        persist(engine, app);
                    } else {
                        app.lock().unwrap().toast("Нужен порт (443) или адрес с портом (0.0.0.0:443)");
                    }
                }
            }
        }
        KeyCode::Backspace => {
            if let Some(i) = app.lock().unwrap().input.as_mut() {
                i.buffer.pop();
            }
        }
        KeyCode::Char(c) => {
            if let Some(i) = app.lock().unwrap().input.as_mut() {
                // Поле лимита — только цифры.
                if matches!(i.kind, InputKind::HostMaxGuests) && !c.is_ascii_digit() {
                    return;
                }
                i.buffer.push(c);
            }
        }
        _ => {}
    }
}

fn vpn_key(code: KeyCode, engine: &EngineSlot, app: &Shared, vpn_task: &mut VpnTask) {
    match code {
        KeyCode::Up => {
            let mut a = app.lock().unwrap();
            a.sel = a.sel.saturating_sub(1);
        }
        KeyCode::Down => {
            let mut a = app.lock().unwrap();
            let n = a.hosts.len();
            if n > 0 {
                a.sel = (a.sel + 1).min(n - 1);
            }
        }
        KeyCode::Enter => {
            let connected = matches!(app.lock().unwrap().vpn, Vpn::On { .. } | Vpn::Connecting(_));
            if connected {
                disconnect_vpn(app, vpn_task);
            } else {
                start_vpn(engine, app, vpn_task);
            }
        }
        // «Старт»: та же очередь кандидатов, что в окне — сначала ЧУЖАЯ страна,
        // потом где свободнее, и с перебором запасных. Раньше здесь брался
        // ПЕРВЫЙ попавшийся хост в порядке каталога: ни страны, ни перебора —
        // молчащий хост означал «не подключилось», хотя рядом стояли живые.
        KeyCode::Char('c') | KeyCode::Char('C') => {
            if matches!(app.lock().unwrap().vpn, Vpn::On { .. } | Vpn::Connecting(_)) {
                return;
            }
            let cands = {
                let a = app.lock().unwrap();
                let own = a.host_code.clone();
                // Своя страна — из поля анонса: встроенной базы IP→страна в
                // терминале нет (см. default_host_name).
                let my_cc = a.hosts.iter().find(|h| h.id == own).map(|h| h.country.clone());
                bmv_desktop::tunnel::rank_candidates(&a.hosts, my_cc.as_deref(), &own, &|h| {
                    (!h.country.is_empty()).then(|| h.country.clone())
                })
            };
            if cands.is_empty() {
                app.lock().unwrap().toast("Сейчас нет свободного хоста без пароля — выберите в списке");
            } else {
                connect_queue(engine, app, vpn_task, cands, None);
            }
        }
        // Подключиться по коду (как поле «КОД СЕТИ» на iOS).
        KeyCode::Char('k') | KeyCode::Char('K')
            if !matches!(app.lock().unwrap().vpn, Vpn::On { .. } | Vpn::Connecting(_)) =>
        {
            app.lock().unwrap().input = Some(Input { kind: InputKind::ConnectCode, buffer: String::new(), masked: false });
        }
        _ => {}
    }
}

fn host_key(
    code: KeyCode,
    engine: &EngineSlot,
    app: &Shared,
    host_engine: &HostEngine,
    host_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    match code {
        KeyCode::Up => {
            let mut a = app.lock().unwrap();
            a.host_field = a.host_field.saturating_sub(1);
        }
        KeyCode::Down => {
            let mut a = app.lock().unwrap();
            a.host_field = (a.host_field + 1).min(HOST_FIELDS - 1);
        }
        // Enter меняет значение выбранного поля: имя/пароль/лимит — ввод с
        // клавиатуры; протокол — перебор; видимость — переключение; действие — старт/стоп.
        KeyCode::Enter => {
            let field = app.lock().unwrap().host_field;
            match field {
                0 => {
                    let cur = app.lock().unwrap().hset.name.clone();
                    app.lock().unwrap().input = Some(Input { kind: InputKind::HostName, buffer: cur, masked: false });
                }
                1 => {
                    let cur = app.lock().unwrap().hset.max_guests.to_string();
                    app.lock().unwrap().input = Some(Input { kind: InputKind::HostMaxGuests, buffer: cur, masked: false });
                }
                2 => {
                    let cur = app.lock().unwrap().hset.password.clone();
                    app.lock().unwrap().input = Some(Input { kind: InputKind::HostPassword, buffer: cur, masked: true });
                }
                3 => {
                    let cur = app.lock().unwrap().hset.protocol.clone();
                    let i = PROTOCOLS.iter().position(|&v| v == cur).unwrap_or(0);
                    let val = PROTOCOLS[(i + 1) % PROTOCOLS.len()].to_string();
                    app.lock().unwrap().hset.protocol = val.clone();
                    apply_host(host_engine, move |e| async move { let _ = e.host_set_protocol(&val).await; });
                    persist(engine, app);
                }
                4 => {
                    // С паролем выбора нет — сеть всегда скрыта. Молча переключать
                    // флаг, который ничего не изменит, значит врать: человек жмёт
                    // Enter, надпись прыгает, а в каталоге по-прежнему пусто.
                    if !app.lock().unwrap().hset.password.is_empty() {
                        app.lock().unwrap().toast("С паролем сеть всегда скрыта");
                    } else {
                        let val = !app.lock().unwrap().hset.public;
                        app.lock().unwrap().hset.public = val;
                        apply_host(host_engine, move |e| async move { let _ = e.host_set_public(val).await; });
                        persist(engine, app);
                    }
                }
                // ЕДИНЫЙ тумблер «Стать хостом»: на Linux-сервере (root+systemd) это
                // демон 24/7 (живёт после q/выхода из SSH); иначе — раздача в окне.
                // Никаких двух режимов сразу: перед демоном гасим оконную раздачу,
                // чтобы два процесса не дрались за один код хоста (сигналинг: отклонён).
                5 => {
                    if daemon_mode() {
                        if matches!(app.lock().unwrap().host, HostMode::On { .. } | HostMode::Starting) {
                            stop_host(app, host_engine, host_task);
                        }
                        spawn_autostart_toggle(true, engine, app);
                    } else if app.lock().unwrap().auto_host == Some(true) {
                        // Демон включён, а прав на systemctl нет — не плодим второй
                        // процесс с тем же кодом (драка за анонс у координатора).
                        app.lock().unwrap().toast("Хост работает 24/7 демоном — управлять через sudo");
                    } else {
                        toggle_host(engine, app, host_engine, host_task);
                    }
                }
                _ => {}
            }
        }
        // N — новый код (как iOS «Новый код»).
        KeyCode::Char('n') | KeyCode::Char('N') => new_host_code(engine, app, host_engine, host_task),
        // Q — показать/скрыть QR кода сети.
        KeyCode::Char('Q') => {
            let mut a = app.lock().unwrap();
            a.show_qr = !a.show_qr;
        }
        _ => {}
    }
}

fn server_key(code: KeyCode, engine: &EngineSlot, app: &Shared, srv_task: &mut Option<tokio::task::JoinHandle<()>>) {
    match code {
        KeyCode::Up => {
            let mut a = app.lock().unwrap();
            a.srv_field = a.srv_field.saturating_sub(1);
        }
        KeyCode::Down => {
            let mut a = app.lock().unwrap();
            a.srv_field = (a.srv_field + 1).min(SRV_FIELDS - 1);
        }
        // Enter действует на выбранное поле: адрес/домен/порт — ввод; автозапуск и
        // «свой сервер» — переключение.
        // ВАЖНО: срез поля в локальную — иначе MutexGuard из скрутинии `match`
        // живёт через все ветки, а они лочат `app` снова → самодедлок (виснет TUI).
        KeyCode::Enter => {
            let field = app.lock().unwrap().srv_field;
            match field {
                0 => {
                    let cur = app.lock().unwrap().coord.clone();
                    app.lock().unwrap().input = Some(Input { kind: InputKind::Coordinator, buffer: cur, masked: false });
                }
                1 => {
                    let cur = app.lock().unwrap().srv_cfg.domain.clone();
                    app.lock().unwrap().input = Some(Input { kind: InputKind::SrvDomain, buffer: cur, masked: false });
                }
                2 => {
                    let cur = app.lock().unwrap().srv_cfg.bind.clone();
                    app.lock().unwrap().input = Some(Input { kind: InputKind::SrvBind, buffer: cur, masked: false });
                }
                // ЕДИНЫЙ тумблер «Запустить свой сервер»: на Linux-сервере — демон 24/7,
                // иначе — в окне (пока открыто меню). Перед демоном гасим оконный
                // сервер, чтобы два процесса не дрались за порт.
                3 => {
                    if daemon_mode() {
                        if matches!(app.lock().unwrap().srv, SrvState::On) {
                            if let Some(h) = srv_task.take() { h.abort(); }
                            app.lock().unwrap().srv = SrvState::Off;
                        }
                        spawn_autostart_toggle(false, engine, app);
                    } else if app.lock().unwrap().auto_srv == Some(true) {
                        app.lock().unwrap().toast("Сервер работает 24/7 демоном — управлять через sudo");
                    } else {
                        let on = matches!(app.lock().unwrap().srv, SrvState::On);
                        if on {
                            if let Some(h) = srv_task.take() { h.abort(); }
                            let mut a = app.lock().unwrap();
                            a.srv = SrvState::Off;
                            a.toast("Свой сервер остановлен");
                        } else {
                            start_server(app, srv_task);
                        }
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Применить настройку к ЖИВОМУ движку раздачи (если раздача активна).
///
/// Движок снимается в ЛОКАЛЬНУЮ до всякого ветвления. Раньше здесь стояло
/// `if let Some(eng) = host_engine.lock().unwrap().clone()`, и guard мьютекса
/// жил через ВСЁ тело ветки (edition 2021) — а тело вызывает `f`, ЧУЖОЕ
/// замыкание, пришедшее снаружи. Тронь оно `host_engine` (а это ровно тот слот,
/// с которым работает вкладка «Хост») — и поток встаёт навсегда, вместе с
/// перерисовкой всего меню. Из всех пяти таких мест это было самое опасное:
/// остальные держат лок над СВОИМ кодом, здесь — над неизвестным.
fn apply_host<F, Fut>(host_engine: &HostEngine, f: F)
where
    F: FnOnce(Arc<BmvEngine>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let eng = host_engine.lock().unwrap().clone();
    if let Some(eng) = eng {
        tokio::spawn(f(eng));
    }
}

/// Записать текущие настройки меню в ~/.config/bemyvpn/config.toml. Зовётся при
/// каждом изменении — руками конфиг создавать/править не нужно, всё само.
fn persist(engine: &EngineSlot, app: &Shared) {
    let mut cfg = engine.lock().unwrap().config().clone();
    {
        let a = app.lock().unwrap();
        cfg.coordinators = vec![a.coord.clone()];
        cfg.host.name = a.hset.name.clone();
        cfg.host.max_guests = a.hset.max_guests;
        cfg.host.password = a.hset.password.clone();
        cfg.host.public = a.hset.public;
        cfg.default_protocol = a.hset.protocol.clone();
        // Код+подпись — пара; сохраняем только вместе (иначе анонс отклонят).
        if !a.host_code.is_empty() && !a.host_sig.is_empty() {
            cfg.host.id = a.host_code.clone();
            cfg.host.code_sig = a.host_sig.clone();
        }
        cfg.server = a.srv_cfg.clone();
    }
    let _ = cfg.save();
}

// ── Автозапуск (Linux/systemd): юнит пишем сами, конфиг он читает из user_path ──

#[cfg(target_os = "linux")]
fn unit_name(host: bool) -> &'static str {
    if host { "bemyvpn-host" } else { "bemyvpn-coord" }
}

/// Включён ли автозапуск. None — не Linux (или systemd недоступен).
#[cfg(target_os = "linux")]
fn autostart_state(host: bool) -> Option<bool> {
    let out = std::process::Command::new("systemctl").args(["is-enabled", unit_name(host)]).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim() == "enabled")
}

#[cfg(not(target_os = "linux"))]
fn autostart_state(_host: bool) -> Option<bool> {
    None
}

/// Переключить автозапуск: ВКЛ пишет юнит + enable (поднимется при загрузке),
/// ВЫКЛ — disable --now. Нужен root (иначе понятная ошибка).
#[cfg(target_os = "linux")]
fn toggle_autostart(host: bool) -> Result<bool, String> {
    let unit = unit_name(host);
    let run = |args: &[&str]| {
        std::process::Command::new("systemctl")
            .args(args)
            .output()
            .map_err(|e| e.to_string())
            .and_then(|o| if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) })
    };
    if autostart_state(host).unwrap_or(false) {
        run(&["disable", "--now", unit])?;
        return Ok(false);
    }
    enable_service(host)?;
    Ok(true)
}

/// Поставить и включить службу — ИДЕМПОТЕНТНО. Отдельно от `toggle_autostart`,
/// потому что этим же пользуется флаг `--autostart` в командной строке: сервер
/// настраивают скриптом, а меню скриптом не понажимаешь.
///
/// Настройки в юнит НЕ пишем — он читает тот же сохранённый конфиг. Так пароль
/// раздачи не оказывается ни в `/etc/systemd/system/` (читаемом всеми), ни в
/// списке процессов.
#[cfg(target_os = "linux")]
pub fn enable_service(host: bool) -> Result<(), String> {
    let unit = unit_name(host);
    let run = |args: &[&str]| {
        std::process::Command::new("systemctl")
            .args(args)
            .output()
            .map_err(|e| e.to_string())
            .and_then(|o| if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) })
    };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let cmd = if host { "host --tunnel" } else { "server" };
    let text = format!(
        "[Unit]\nDescription=BeMyVPN ({unit})\nAfter=network-online.target\n\n\
         [Service]\nExecStart={} --config {} {cmd}\nRestart=always\n\n\
         [Install]\nWantedBy=multi-user.target\n",
        exe.display(),
        Config::user_path().display()
    );
    std::fs::write(format!("/etc/systemd/system/{unit}.service"), text)
        .map_err(|_| "Нужны права администратора — запустите с sudo.".to_string())?;
    run(&["daemon-reload"])?;
    // --now: поднять СРАЗУ фоновым демоном (не только при загрузке). Тогда после
    // выхода из меню (q/Ctrl+C) раздача/сервер ПРОДОЛЖАЮТ работать — это и есть
    // «работает 24/7». Демон читает тот же сохранённый конфиг (user_path).
    run(&["enable", "--now", unit])
}

#[cfg(not(target_os = "linux"))]
pub fn enable_service(_host: bool) -> Result<(), String> {
    Err("Автозапуск бывает только на Linux с systemd.".into())
}

#[cfg(not(target_os = "linux"))]
fn toggle_autostart(_host: bool) -> Result<bool, String> {
    Err("Автозапуск бывает только на Linux-сервере (systemd).".into())
}

/// Переключить автозапуск НЕ блокируя интерфейс: `systemctl` на слабом VPS отвечает
/// секундами, а раньше вызывался прямо в потоке событий → весь TUI замирал («лаг»).
/// Теперь — в отдельном потоке, состояние/тост обновляются по готовности.
/// Полноценный режим 24/7 доступен, когда мы root на Linux с systemd — тогда
/// «ВКЛ» значит демон (живёт после выхода), «ВЫКЛ» значит выключено совсем.
#[cfg(target_os = "linux")]
fn daemon_mode() -> bool {
    std::path::Path::new("/run/systemd/system").exists() && unsafe { libc::geteuid() } == 0
}

#[cfg(not(target_os = "linux"))]
fn daemon_mode() -> bool {
    false
}

fn spawn_autostart_toggle(host: bool, engine: &EngineSlot, app: &Shared) {
    persist(engine, app); // юнит должен видеть свежий конфиг
    app.lock().unwrap().toast("Переключаю…");
    let app2 = app.clone();
    std::thread::spawn(move || {
        let res = toggle_autostart(host);
        let mut a = app2.lock().unwrap();
        match res {
            Ok(on) => {
                if host { a.auto_host = Some(on); } else { a.auto_srv = Some(on); }
                a.toast(match (host, on) {
                    (true, true) => "Хост ВКЛ — работает 24/7, даже после выхода",
                    (true, false) => "Хост ВЫКЛ",
                    (false, true) => "Сервер ВКЛ — работает 24/7, даже после выхода",
                    (false, false) => "Сервер ВЫКЛ",
                });
            }
            Err(e) => a.toast(e),
        }
    });
}

/// Сменить координатор: пересоздаём движок с новым адресом (список станет живым в фоне).
fn switch_coordinator(engine: &EngineSlot, app: &Shared, url: String) {
    let mut cfg = engine.lock().unwrap().config().clone();
    cfg.coordinators = vec![url.clone()];
    *engine.lock().unwrap() = Arc::new(BmvEngine::from_config(cfg));
    let mut a = app.lock().unwrap();
    a.coord = url;
    a.coord_ok = None;
    a.toast("Координатор изменён");
}

fn ensure_host_code(engine: &EngineSlot, app: &Shared) {
    if !app.lock().unwrap().host_code.is_empty() {
        return;
    }
    let eng = engine.lock().unwrap().clone();
    let (slot, app2) = (engine.clone(), app.clone());
    tokio::spawn(async move {
        if let Ok((code, sig)) = eng.host_new_code().await {
            if !code.is_empty() {
                {
                    let mut a = app2.lock().unwrap();
                    a.host_code = code;
                    a.host_sig = sig; // без подписи координатор отклонит анонс
                }
                persist(&slot, &app2); // код вечный — переживает перезапуск
            }
        }
    });
}

// ── VPN (гость) ──────────────────────────────────────────────────────────────

fn start_vpn(engine: &EngineSlot, app: &Shared, vpn_task: &mut VpnTask) {
    let host = {
        let a = app.lock().unwrap();
        a.hosts.get(a.sel).cloned()
    };
    let Some(host) = host else {
        app.lock().unwrap().toast("Сначала выберите хост в списке");
        return;
    };
    // Забитый (и мёртвый) хост не годится — то же правило, по которому в трёх
    // других оболочках гаснет кнопка. Раньше терминал позволял нажать, уходил в
    // сеть и возвращался отказом: выглядело как «программа не работает».
    if !view::host_usable(host.online, host.guests, host.max_guests) {
        app.lock().unwrap().toast(if host.online {
            "На этом хосте нет свободных мест — выберите другой"
        } else {
            "Этот хост не на связи — выберите другой"
        });
        return;
    }
    if host.has_password {
        app.lock().unwrap().input = Some(Input { kind: InputKind::GuestPassword(host), buffer: String::new(), masked: true });
    } else {
        connect_to(engine, app, vpn_task, host, None);
    }
}

/// Подключиться к очереди кандидатов (первый успешный — тот и качает трафик).
///
/// Вся гостевая механика — общая с окном (`bmv_desktop::tunnel`): бюджеты на
/// попытку, перебор запасных и ОБЯЗАТЕЛЬНЫЙ BYE на выходе. Раньше здесь жила
/// своя упрощённая копия: один хост без запасных, без поводка и без прощания —
/// хост держал ушедшего гостя лишние 8 секунд, до keepalive-таймаута.
fn connect_queue(engine: &EngineSlot, app: &Shared, vpn_task: &mut VpnTask, cands: Vec<HostInfo>, password: Option<String>) {
    let Some(first) = cands.first() else {
        app.lock().unwrap().toast("Подходящего хоста нет — выберите в списке");
        return;
    };
    app.lock().unwrap().vpn = Vpn::Connecting(host_name(first));
    // Имена для показа — по id, чтобы сообщения не зависели от того, что в этот
    // момент лежит в каталоге.
    let names: std::collections::HashMap<String, String> =
        cands.iter().map(|h| (h.id.clone(), host_name(h))).collect();
    let pw = password.unwrap_or_default();
    let list: Vec<(String, String, String)> =
        cands.iter().map(|h| (h.id.clone(), pw.clone(), h.protocol.clone())).collect();
    // Конфиг гостя — НАСТОЯЩИЙ, из движка: там свои STUN-серверы и настройки
    // протоколов. Одного адреса координатора мало — остальное молча уехало бы
    // на умолчания, хотя человек прописал своё в файле.
    let mut cfg = engine.lock().unwrap().config().clone();
    cfg.coordinators = vec![app.lock().unwrap().coord.clone()];
    let had_password = !pw.is_empty();

    let stop = Arc::new(tokio::sync::Notify::new());
    let (app2, stop2) = (app.clone(), stop.clone());
    let handle = tokio::spawn(async move {
        bmv_desktop::tunnel::run_candidates(
            cfg,
            list,
            move |s| {
                let name = |id: &String| names.get(id).cloned().unwrap_or_else(|| id.clone());
                let mut a = app2.lock().unwrap();
                match s {
                    bmv_desktop::tunnel::State::Connecting(id) => a.vpn = Vpn::Connecting(name(&id)),
                    bmv_desktop::tunnel::State::Up(id) => {
                        a.vpn = Vpn::On { name: name(&id), id, since: Instant::now() }
                    }
                    bmv_desktop::tunnel::State::Off => {
                        if matches!(a.vpn, Vpn::On { .. }) {
                            a.toast("Туннель закрыт");
                        }
                        a.vpn = Vpn::Off;
                    }
                    // Хост выключил раздачу — говорим об этом словами и без
                    // красного: подключение сработало, просто хоста больше нет.
                    bmv_desktop::tunnel::State::HostLeft => {
                        a.toast("Хост завершил раздачу — выберите другой");
                        a.vpn = Vpn::Ended;
                    }
                    bmv_desktop::tunnel::State::Failed(e) => {
                        a.vpn = Vpn::Failed(if had_password {
                            format!("{e} Возможно, неверный пароль.")
                        } else {
                            e
                        })
                    }
                }
            },
            stop2,
        )
        .await;
    });
    *vpn_task = Some((handle, stop));
}

fn connect_to(engine: &EngineSlot, app: &Shared, vpn_task: &mut VpnTask, host: HostInfo, password: Option<String>) {
    connect_queue(engine, app, vpn_task, vec![host], password);
}

/// Как подписать хост: имя, а без имени — код (общее правило, `bmv_common::view`).
fn host_name(h: &HostInfo) -> String {
    view::host_display_name(&h.name, &h.id).to_string()
}

/// Отключиться ВЕЖЛИВО: сначала «стоп» (задача успеет отправить BYE и откатить
/// маршруты), и только если она за 3с не управилась — обрыв.
fn disconnect_vpn(app: &Shared, vpn_task: &mut VpnTask) {
    if let Some((mut h, stop)) = vpn_task.take() {
        stop.notify_waiters();
        tokio::spawn(async move {
            if tokio::time::timeout(Duration::from_secs(3), &mut h).await.is_err() {
                h.abort(); // не попрощалась — хотя бы не висим
            }
        });
    }
    let mut a = app.lock().unwrap();
    a.vpn = Vpn::Off;
    a.toast("Отключено");
}

// ── Хост (раздача) ───────────────────────────────────────────────────────────

fn toggle_host(engine: &EngineSlot, app: &Shared, host_engine: &HostEngine, host_task: &mut Option<tokio::task::JoinHandle<()>>) {
    let running = matches!(app.lock().unwrap().host, HostMode::On { .. } | HostMode::Starting);
    if running {
        stop_host(app, host_engine, host_task);
    } else {
        start_host(engine, app, host_engine, host_task);
    }
}

fn start_host(engine: &EngineSlot, app: &Shared, host_engine: &HostEngine, host_task: &mut Option<tokio::task::JoinHandle<()>>) {
    app.lock().unwrap().host = HostMode::Starting;
    let base = engine.lock().unwrap().config().clone();
    let hset = app.lock().unwrap().hset.clone();
    let (code, sig) = {
        let a = app.lock().unwrap();
        (a.host_code.clone(), a.host_sig.clone())
    };
    let (slot, app2) = (engine.clone(), app.clone());
    let heng = host_engine.clone();
    let handle = tokio::spawn(async move {
        let mut hcfg = base.clone();
        hcfg.host.name = hset.name;
        hcfg.host.public = hset.public;
        hcfg.host.max_guests = hset.max_guests;
        hcfg.host.password = hset.password;
        hcfg.default_protocol = hset.protocol;
        // Код И его подпись выдаёт сервер парой — координатор проверяет подпись при
        // анонсе, поэтому нужны оба. Нет пары в руках → просим свежую.
        if code.is_empty() || sig.is_empty() {
            let tmp = BmvEngine::from_config(base.clone());
            match tmp.host_new_code().await {
                Ok((c, s)) if !c.is_empty() && !s.is_empty() => {
                    hcfg.host.id = c;
                    hcfg.host.code_sig = s;
                }
                _ => {
                    app2.lock().unwrap().host = HostMode::Failed("Сервер не выдал код сети. Проверьте связь и попробуйте ещё раз.".into());
                    return;
                }
            }
        } else {
            hcfg.host.id = code;
            hcfg.host.code_sig = sig;
        }
        let mut used_sig = hcfg.host.code_sig.clone();
        // `with_owner_token` — владельческий токен записи в каталоге. Без него
        // каждый запуск приходил к координатору новым устройством и на свой же
        // код получал «занят другим устройством», пока прежняя запись не
        // протухнет. См. крышку функции в main.rs.
        let mut engine = Arc::new(BmvEngine::from_config(crate::with_owner_token(hcfg.clone())));
        let mut announce = engine.host_bind_announce().await;
        // 403 = координатор не признаёт нашу пару код+подпись (сервер сменили,
        // код отозвали, конфиг переехал). Раньше терминал на этом просто вставал
        // с непонятным «403», хотя лечится оно само: просим новый код и пробуем
        // ещё раз — ровно так делает окно.
        if announce.as_ref().err().and_then(|e| e.refusal_code()) == Some(403) {
            if let Ok((c, s)) = BmvEngine::from_config(base.clone()).host_new_code().await {
                if !c.is_empty() && !s.is_empty() {
                    let mut fresh = hcfg.clone();
                    fresh.host.id = c;
                    fresh.host.code_sig = s.clone();
                    used_sig = s;
                    engine = Arc::new(BmvEngine::from_config(crate::with_owner_token(fresh)));
                    announce = engine.host_bind_announce().await;
                }
            }
        }
        let hub = match announce {
            Ok((hub, _id, _eps)) => hub,
            Err(err) => {
                // По КОДУ отказа, а не по буквам в тексте: раньше `contains("422")`
                // не срабатывал никогда — число ехало отдельным полем и терялось.
                let msg = if err.refusal_code() == Some(422) {
                    "Не удалось определить ваш адрес в интернете — без него гости не найдут дорогу. Попробуйте ещё раз, а если не проходит — из другой сети.".to_string()
                } else {
                    err.to_string()
                };
                app2.lock().unwrap().host = HostMode::Failed(msg);
                return;
            }
        };
        *heng.lock().unwrap() = Some(engine.clone());
        {
            let mut a = app2.lock().unwrap();
            a.host = HostMode::On { code: engine.host_id().to_string() };
            a.host_code = engine.host_id().to_string();
            a.host_sig = used_sig;
            a.host_started = Some(Instant::now());
        }
        persist(&slot, &app2); // код+настройки переживают перезапуск
        // Всё дерево раздачи — в одном JoinSet (bmv_desktop::hosting). Раньше
        // здесь heartbeat, пробитие и КАЖДАЯ гостевая сессия были отдельными
        // `tokio::spawn`: «Выключить» обрывало только этот цикл, гости
        // продолжали ходить в интернет через машину, а heartbeat каждые 10с
        // заново чинил запись в каталоге — de-announce отменялся сам собой, и
        // хост-призрак висел в публичном списке до выхода из программы.
        bmv_desktop::hosting::serve_host(engine.clone(), hub).await;
        heng.lock().unwrap().take();
        let mut a = app2.lock().unwrap();
        a.host = HostMode::Off;
        a.host_started = None;
    });
    *host_task = Some(handle);
}

fn stop_host(app: &Shared, host_engine: &HostEngine, host_task: &mut Option<tokio::task::JoinHandle<()>>) {
    // СНАЧАЛА обрываем дерево задач (в нём heartbeat, который каждые 10с чинит
    // запись в каталоге), и только потом прощаемся — иначе bye отменялся бы
    // собственным heartbeat'ом.
    if let Some(h) = host_task.take() {
        h.abort();
    }
    // Движок — в ЛОКАЛЬНУЮ до ветвления: guard мьютекса из скрутинии `if let`
    // живёт через всё тело ветки (edition 2021), а внутри мы порождаем задачу.
    let eng = host_engine.lock().unwrap().take();
    if let Some(eng) = eng {
        tokio::spawn(async move { let _ = tokio::time::timeout(Duration::from_secs(2), eng.host_deannounce()).await; });
    }
    let mut a = app.lock().unwrap();
    a.host = HostMode::Off;
    a.host_started = None;
    a.toast("Раздача остановлена");
}

/// Новый код: сброс + (если раздавали) авто-рестарт под свежим кодом (как iOS).
fn new_host_code(engine: &EngineSlot, app: &Shared, host_engine: &HostEngine, host_task: &mut Option<tokio::task::JoinHandle<()>>) {
    let was = matches!(app.lock().unwrap().host, HostMode::On { .. } | HostMode::Starting);
    // Тот же порядок и тот же срез в локальную, что и в stop_host.
    if let Some(h) = host_task.take() {
        h.abort();
    }
    let eng = host_engine.lock().unwrap().take();
    if let Some(eng) = eng {
        tokio::spawn(async move { let _ = tokio::time::timeout(Duration::from_secs(2), eng.host_deannounce()).await; });
    }
    {
        let mut a = app.lock().unwrap();
        a.host = HostMode::Off;
        a.host_started = None;
        a.host_code = String::new();
        a.host_sig = String::new();
    }
    // Запрашиваем свежий код (с подписью — иначе анонс отклонят).
    let eng = engine.lock().unwrap().clone();
    let (slot, app2) = (engine.clone(), app.clone());
    let daemon_on = app.lock().unwrap().auto_host == Some(true);
    tokio::spawn(async move {
        if let Ok((code, sig)) = eng.host_new_code().await {
            if !code.is_empty() {
                {
                    let mut a = app2.lock().unwrap();
                    a.host_code = code;
                    a.host_sig = sig;
                }
                persist(&slot, &app2);
                // Демон 24/7 читает конфиг при старте — перезапускаем, чтобы
                // раздача пошла под новым кодом (не в потоке событий).
                #[cfg(target_os = "linux")]
                if daemon_on && daemon_mode() {
                    std::thread::spawn(|| {
                        let _ = std::process::Command::new("systemctl").args(["restart", unit_name(true)]).output();
                    });
                }
                #[cfg(not(target_os = "linux"))]
                let _ = daemon_on;
            }
        }
    });
    if was {
        start_host(engine, app, host_engine, host_task);
    }
    app.lock().unwrap().toast("Новый код запрошен");
}

// ── Сервер (свой координатор) ────────────────────────────────────────────────

fn start_server(app: &Shared, srv_task: &mut Option<tokio::task::JoinHandle<()>>) {
    let cfg = app.lock().unwrap().srv_cfg.clone();
    let bind: std::net::SocketAddr = match cfg.bind.parse() {
        Ok(b) => b,
        Err(e) => {
            app.lock().unwrap().srv = SrvState::Failed(format!("bind «{}»: {e}", cfg.bind));
            return;
        }
    };
    let tls = crate::tls_from_config(&cfg);
    let app2 = app.clone();
    let handle = tokio::spawn(async move {
        app2.lock().unwrap().srv = SrvState::On;
        if let Err(e) = bmv_coordinator::serve(bind, tls, None, std::future::pending()).await {
            app2.lock().unwrap().srv = SrvState::Failed(format!("{e}"));
        }
    });
    *srv_task = Some(handle);
    app.lock().unwrap().toast("Свой сервер запущен");
}

// ── фон: каталог + связь + свой IP ───────────────────────────────────────────

fn spawn_refresh(engine: EngineSlot, app: Shared) {
    tokio::spawn(async move {
        let mut ver = 0u64; // версия каталога: watch отвечает МГНОВЕННО при изменении
        loop {
            let eng = engine.lock().unwrap().clone();
            // Живой каталог: long-poll до изменения (гость зашёл/вышел — сразу
            // видно), либо ~25с тишины как idle-heartbeat. Смена координатора
            // в меню прерывает ожидание (движок пересоздан — ptr сменился).
            let upd = tokio::select! {
                r = eng.guest_watch(None, ver) => Some(r),
                _ = async {
                    loop {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        if !Arc::ptr_eq(&eng, &engine.lock().unwrap()) { break; }
                        if app.lock().unwrap().quit { break; }
                    }
                } => None,
            };
            if app.lock().unwrap().quit {
                break;
            }
            let Some(upd) = upd else { ver = 0; continue }; // координатор сменили
            // Каталог: обновляем ТОЛЬКО при успехе; на Err (в т.ч. «снапшот ещё не
            // пришёл») старый список НЕ трём.
            let list = match upd {
                Ok(u) => { ver = u.version; Some(u.hosts) }
                Err(_) => { ver = 0; None }
            };
            // СТАТУС связи = состояние WS-сокета (health), а НЕ успех watch: сокет
            // может быть жив, пока снапшот каталога ещё в пути (хост при этом уже
            // анонсирован). Иначе показывало «нет связи» на живом соединении.
            let connected = eng.coordinator_health().await.is_ok();
            // НАСТОЯЩИЙ круг до координатора. Раньше здесь секундомером мерили,
            // сколько длился сам вызов health(), — а он читает флаг «сокет жив»
            // и возвращается мгновенно, поэтому на экране вечно стоял ноль.
            let ping = eng.coordinator_rtt().unwrap_or(0);
            let got_catalog = list.is_some();
            let need_ip = app.lock().unwrap().my_ip.is_empty();
            let ip = if connected && need_ip { eng.my_ip().await.ok() } else { None };
            // Отклик до ВЫБРАННОГО хоста — так же, как окно мерит раскрытую
            // карточку: настоящая проба к чужой машине, поэтому только один
            // хост, а не весь список.
            let target = {
                let a = app.lock().unwrap();
                a.hosts.get(a.sel).map(|h| (h.id.clone(), h.endpoints.clone()))
            };
            let host_ping = match target {
                Some((id, eps)) if !eps.is_empty() => Some((id.clone(), eng.probe_host_rtt(&id, &eps).await)),
                Some((id, _)) => Some((id, None)), // адресов нет — честное «не ответил»
                None => None,
            };
            {
                let mut a = app.lock().unwrap();
                a.coord_ok = Some(connected);
                if connected { a.coord_ping = ping; }
                if let Some(list) = list {
                    a.hosts = list;
                }
                if let Some(ip) = ip {
                    a.my_ip = ip;
                }
                if let Some((id, ms)) = host_ping {
                    a.host_ping = Some((id, ms));
                }
                if a.sel >= a.hosts.len() {
                    a.sel = a.hosts.len().saturating_sub(1);
                }
                if a.quit {
                    break;
                }
            }
            // Пауза нужна на ЛЮБОЙ неудаче, а не только когда сокет объявлен
            // мёртвым: watch может вернуть ошибку при живом сокете, и тогда цикл
            // молотил бы сотнями запросов в секунду в чужой сервер.
            if !connected || !got_catalog {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    });
}

// ── отрисовка ────────────────────────────────────────────────────────────────

// ЦВЕТА — ТЕ ЖЕ, ЧТО В ТРЁХ ГРАФИЧЕСКИХ ОБОЛОЧКАХ (палитра «Уголь»).
//
// Терминал жил своей палитрой и держал самый первый акцент `#5E93FF` — его не
// трогали ни в одну из двух переделок цвета. Итог: одна и та же запись каталога
// на четырёх экранах выглядела по-разному, а «синий» означал два разных синих.
//
// ЗЕЛЁНОГО ЗДЕСЬ БОЛЬШЕ НЕТ. Акцент — мята, а прежний зелёный «работает»
// (#34E29E) от неё не отличим глазом: два разных смысла одним цветом. Мята
// несёт обе роли; в терминале их и без цвета различает слово («Подключено»,
// «Раздаю») и эмодзи-кружок слева, которому тема не указ.
//
// Здесь только те роли, что есть в терминале: поверхностей у него нет, кроме
// подсветки строки (SEL — ступень s2 из общей палитры).
//
// РУКАМИ ЭТИ СЕМЬ СТРОК НЕ ПРАВЯТ. Они живут в `design/palette.toml` и
// раскладываются сюда и по трём графическим темам скриптом
// `design/sync-palette.py`. Раньше менять цвет надо было в четырёх местах разом
// — и именно поэтому терминал отстал на две переделки цвета.
//
// FG и BG стоят здесь ИМЕНОВАННЫМИ НЕ ЗРЯ: раньше на их местах были `Color::White`
// и `Color::Black`, а те берутся из палитры ТЕРМИНАЛА, не из нашей, и на чужой
// теме уезжали в свой оттенок мимо всех расчётов.
// ── НАЧАЛО: значения из design/palette.toml (правит design/sync-palette.py) ──
const ACCENT: Color = Color::Rgb(0x2F, 0xE0, 0xA8); // #2FE0A8 — мята: и «это можно нажать», и «это работает»
const AMBER: Color = Color::Rgb(0xF5, 0xB1, 0x4C);  // #F5B14C — беда, которая пройдёт сама
const RED: Color = Color::Rgb(0xFF, 0x79, 0x74);    // #FF7974 — беда, которая сама не пройдёт
const DIM: Color = Color::Rgb(0xA5, 0xAA, 0xB5);    // #A5AAB5 — второстепенный текст и «выключено»
const FG: Color = Color::Rgb(0xED, 0xF1, 0xF8);     // #EDF1F8 — основной текст
const BG: Color = Color::Rgb(0x08, 0x09, 0x0B);     // #08090B — s0 — страница
const SEL: Color = Color::Rgb(0x1B, 0x1F, 0x25);    // #1B1F25 — s2 — раскрытая карточка, чип
// ── КОНЕЦ: значения из design/palette.toml ──

/// Единый вид карточки: скруглённая рамка, тусклый контур, акцентный жирный
/// заголовок и горизонтальный паддинг, чтобы текст не липнул к рамке.
fn card(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(DIM))
        .title(Span::styled(title, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
        .padding(ratatui::widgets::Padding::horizontal(1))
}

/// Эмодзи-индикатор состояния — одинаковый на всех вкладках.
fn dot_on() -> &'static str { "🟢" }
fn dot_off() -> &'static str { "⚪" }
fn dot_wait() -> &'static str { "🟡" }
fn dot_err() -> &'static str { "🔴" }

fn ui(f: &mut Frame, a: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .horizontal_margin(2)
        .vertical_margin(1)
        .constraints([Constraint::Length(3), Constraint::Min(3), Constraint::Length(1)])
        .spacing(1)
        .split(f.area());

    let dot = match a.coord_ok {
        Some(true) => Span::raw(format!(" {} ", dot_on())),
        Some(false) => Span::raw(format!(" {} ", dot_err())),
        None => Span::raw(format!(" {} ", dot_off())),
    };
    let titles: Vec<Line> = Tab::ALL.iter().map(|t| Line::from(t.title())).collect();
    let tabs = Tabs::new(titles)
        .select(a.tab.idx())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(DIM))
                .title(Line::from(vec![
                    Span::styled(" BeMyVPN ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                    dot,
                ])),
        )
        .highlight_style(Style::default().fg(BG).bg(ACCENT).add_modifier(Modifier::BOLD))
        .divider("");
    f.render_widget(tabs, chunks[0]);

    match a.tab {
        Tab::Vpn => vpn_tab(f, chunks[1], a),
        Tab::Host => host_tab(f, chunks[1], a),
        Tab::Server => server_tab(f, chunks[1], a),
    }

    if let Some(inp) = &a.input {
        render_input(f, chunks[1], inp);
    }

    let hint = if a.input.is_some() {
        "Enter — ок · Esc — отмена"
    } else {
        match a.tab {
            Tab::Vpn => "↑↓ выбор · Enter подкл/откл · C старт · K по коду · Tab вкладки · q выход",
            Tab::Host => "↑↓ поле · Enter изменить/вкл · N код · Shift+Q QR · ←→ вкладки · q выход",
            Tab::Server => "↑↓ поле · Enter изменить/переключить · ←→ вкладки · q выход",
        }
    };
    let toast = a
        .toast
        .as_ref()
        .filter(|(_, t)| t.elapsed() < Duration::from_secs(4))
        .map(|(m, _)| m.as_str());
    let bottom = Line::from(vec![
        Span::styled(format!(" {hint} "), Style::default().fg(DIM)),
        Span::styled(toast.unwrap_or("").to_string(), Style::default().fg(ACCENT)),
    ]);
    f.render_widget(Paragraph::new(bottom), chunks[2]);
}

fn render_input(f: &mut Frame, area: Rect, inp: &Input) {
    let w = area.width.clamp(30, 60);
    let h = 5u16;
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    let shown = if inp.masked { "•".repeat(inp.buffer.chars().count()) } else { inp.buffer.clone() };
    let body = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("› ", Style::default().fg(ACCENT)),
            Span::styled(shown, Style::default().fg(FG)),
            Span::styled("▏", Style::default().fg(ACCENT)),
        ]),
        Line::from(Span::styled("Enter — ок · Esc — отмена", Style::default().fg(DIM))),
    ];
    f.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(ACCENT))
                .title(inp.title())
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        rect,
    );
}

fn vpn_tab(f: &mut Frame, area: Rect, a: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(3)])
        .spacing(1)
        .split(area);

    // Статус-карточка (при подключении — детали хоста из каталога).
    let status: Vec<Line> = match &a.vpn {
        Vpn::Off => vec![Line::from(Span::styled(format!("{} VPN выключен", dot_off()), Style::default().fg(DIM)))],
        Vpn::Connecting(h) => vec![Line::from(Span::styled(format!("{} Подключаюсь к {h}…", dot_wait()), Style::default().fg(AMBER)))],
        Vpn::On { id, name, since } => {
            let h = a.hosts.iter().find(|h| &h.id == id);
            let ip = h.and_then(|h| h.endpoints.first()).map(|e| e.as_str()).unwrap_or("—");
            let guests = h.map(|h| format!("{}/{}", h.guests, h.max_guests)).unwrap_or_else(|| "—".into());
            let proto = h.map(|h| proto_short(&h.protocol)).unwrap_or_default();
            vec![
                Line::from(vec![
                    Span::styled(format!("{} Подключено", dot_on()), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("   {name}   ⏳ {}", uptime(*since)), Style::default().fg(DIM)),
                ]),
                Line::from(Span::styled(format!("🌍 {ip}    👥 {guests}    {proto}"), Style::default().fg(DIM))),
            ]
        }
        // Тот же тихий серый, что и у «Отключено»: конец раздачи — не отказ.
        // Подсказка в той же строке, потому что без неё «завершил» звучит как
        // приговор, а на деле рядом стоят другие хосты.
        Vpn::Ended => vec![Line::from(Span::styled(
            format!("{} Хост завершил раздачу — выберите другой (c — «Старт»)", dot_off()),
            Style::default().fg(DIM),
        ))],
        Vpn::Failed(e) => vec![Line::from(Span::styled(format!("{} {e}", dot_err()), Style::default().fg(RED)))],
    };
    f.render_widget(Paragraph::new(status).wrap(Wrap { trim: true }).block(card(" Статус ")), rows[0]);

    let items: Vec<ListItem> = if a.hosts.is_empty() {
        vec![ListItem::new(Line::from(Span::styled("Живых хостов сейчас нет…", Style::default().fg(DIM))))]
    } else {
        a.hosts
            .iter()
            .map(|h| {
                let name = view::host_display_name(&h.name, &h.id);
                let dot = if h.online { dot_on() } else { dot_off() };
                let lock = if h.has_password { " 🔒" } else { "" };
                let cc = if h.country.is_empty() { String::new() } else { format!("  {}", h.country) };
                // Забитый хост ГАСНЕТ — здесь это замена погашенной кнопке из
                // трёх других оболочек: подключиться к нему всё равно нельзя.
                let usable = view::host_usable(h.online, h.guests, h.max_guests);
                let mut spans = vec![
                    Span::raw(format!("{dot} ")),
                    Span::styled(format!("{name}{lock}"), Style::default().fg(if usable { FG } else { DIM })),
                    Span::styled(
                        format!("   👥 {}/{}{cc}   {}", h.guests, h.max_guests, proto_short(&h.protocol)),
                        Style::default().fg(DIM),
                    ),
                ];
                // Отклик есть только у выбранного хоста (меряем один — см.
                // spawn_refresh), и цифра красится по общей линейке тревоги.
                if let Some((pid, ms)) = &a.host_ping {
                    if pid == &h.id {
                        let (text, alarm) = view::ping(*ms);
                        spans.push(Span::styled(format!("   ⏱ {text}"), Style::default().fg(alarm_color(alarm))));
                    }
                }
                ListItem::new(Line::from(spans))
            })
            .collect()
    };
    let mut st = ListState::default();
    if !a.hosts.is_empty() {
        st.select(Some(a.sel.min(a.hosts.len() - 1)));
    }
    // При обрыве список НЕ трётся (иначе он мигал бы пустотой на каждой
    // заминке), но и выдавать вчерашние цифры за живые нельзя — говорим прямо.
    let title = if a.coord_ok == Some(false) && !a.hosts.is_empty() {
        " Доступные хосты — последние известные (нет связи) "
    } else {
        " Доступные хосты "
    };
    let list = List::new(items)
        .block(card(title))
        .highlight_style(Style::default().bg(SEL).fg(FG).add_modifier(Modifier::BOLD))
        .highlight_symbol("");
    f.render_stateful_widget(list, rows[1], &mut st);
}

fn host_tab(f: &mut Frame, area: Rect, a: &App) {
    let code = match &a.host {
        HostMode::On { code } => code.clone(),
        _ => a.host_code.clone(),
    };

    // QR по коду, если включён показ — на весь экран.
    if a.show_qr && !code.is_empty() {
        let lines = qr_lines(&format!("bemyvpn://{code}"));
        let text: Vec<Line> = std::iter::once(Line::from(Span::styled(format!("🔑 {code}"), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))))
            .chain(lines.into_iter().map(Line::from))
            .chain(std::iter::once(Line::from(Span::styled("Shift+Q — скрыть", Style::default().fg(DIM)))))
            .collect();
        f.render_widget(Paragraph::new(text).alignment(Alignment::Center).block(card(" QR приглашения ")), area);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(6)])
        .spacing(1)
        .split(area);

    // ── Карточка статуса: состояние + код + (при раздаче) живые гости/время.
    // ВКЛ = либо демон 24/7 (Linux-сервер), либо раздача в окне — одно понятие.
    let on247 = a.auto_host == Some(true);
    let status = if on247 {
        Span::styled(format!("{} Раздаю — 24/7 (живёт и после выхода)", dot_on()), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
    } else {
        match &a.host {
            HostMode::Off => Span::styled(format!("{} Раздача выключена", dot_off()), Style::default().fg(DIM)),
            HostMode::Starting => Span::styled(format!("{} Запускаюсь…", dot_wait()), Style::default().fg(AMBER)),
            HostMode::On { .. } => Span::styled(format!("{} Раздаю", dot_on()), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            HostMode::Failed(e) => Span::styled(format!("{} {e}", dot_err()), Style::default().fg(RED)),
        }
    };
    let mut head = vec![Line::from(vec![
        status,
        Span::styled(if code.is_empty() { String::new() } else { format!("    🔑 {code}") }, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
    ])];
    if on247 || matches!(a.host, HostMode::On { .. }) {
        // Гости — из живого каталога (работает и для демона, если хост публичный).
        let guests = a.hosts.iter().find(|h| h.id == code).map(|h| h.guests).unwrap_or(0);
        let up = a.host_started.map(uptime).unwrap_or_default();
        let tail = if up.is_empty() { String::new() } else { format!("    ⏳ {up}") };
        head.push(Line::from(Span::styled(format!("👥 {guests}/{}{tail}", a.hset.max_guests), Style::default().fg(ACCENT))));
    }
    f.render_widget(Paragraph::new(head).wrap(Wrap { trim: true }).block(card(" Раздача ")), rows[0]);

    // ── Список настроек с фоновой подсветкой выбранной строки (курсор ↑↓).
    // ПАРОЛЬ ⇒ СЕТЬ ВСЕГДА СКРЫТАЯ. Правило соблюдает ядро (build_announce), но
    // строка показывала сохранённое ЖЕЛАНИЕ, а не факт: с паролем здесь горело
    // «Публичный», хотя в каталоге сети нет. Показываем то, что есть на самом
    // деле, и говорим почему.
    let vis = if !a.hset.password.is_empty() {
        "🙈 Скрытый (пароль)"
    } else if a.hset.public {
        "🌐 Публичный"
    } else {
        "🙈 Скрытый"
    };
    let pw = if a.hset.password.is_empty() { "🔓 без пароля".to_string() } else { format!("🔒 {}", "•".repeat(a.hset.password.chars().count())) };
    let name = if a.hset.name.is_empty() { "(не задано)".to_string() } else { a.hset.name.clone() };
    let frow = |label: &str, val: String| {
        ListItem::new(Line::from(vec![
            Span::styled(format!("{label:<14}"), Style::default().fg(DIM)),
            Span::styled(val, Style::default().fg(FG)),
        ]))
    };
    let hosting = a.auto_host == Some(true) || matches!(a.host, HostMode::On { .. } | HostMode::Starting);
    let action = if hosting { "■  Выключить хост" } else { "▶  Стать хостом" };
    let items = vec![
        frow("Имя", name),
        frow("Лимит гостей", a.hset.max_guests.to_string()),
        frow("Пароль", pw),
        frow("Протокол", proto_short(&a.hset.protocol).to_string()),
        frow("Видимость", vis.to_string()),
        ListItem::new(Line::from(Span::styled(action, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))),
    ];
    let mut st = ListState::default();
    st.select(Some(a.host_field.min(HOST_FIELDS - 1)));
    let list = List::new(items)
        .block(card(" Настройки — Enter меняет "))
        .highlight_style(Style::default().bg(SEL).add_modifier(Modifier::BOLD))
        .highlight_symbol("");
    f.render_stateful_widget(list, rows[1], &mut st);
}

fn server_tab(f: &mut Frame, area: Rect, a: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(7)])
        .spacing(1)
        .split(area);

    // ── Статус-карточка: связь с координатором + состояние своего сервера.
    // Пинг координатора — по той же линейке, что и до хоста. Мятой осталось
    // «на связи»: мята тут значит «работает», а не «быстро», и на цифре она
    // хвалила бы любой отклик, включая красный.
    let link: Vec<Span> = match a.coord_ok {
        Some(true) => {
            let (text, alarm) = view::ping(Some(a.coord_ping));
            vec![
                Span::styled(format!("{} на связи · ", dot_on()), Style::default().fg(ACCENT)),
                Span::styled(text, Style::default().fg(alarm_color(alarm))),
            ]
        }
        // Янтарь, а не красный: связь восстанавливается САМА за секунды. Красный
        // бережём для того, что само не пройдёт, — иначе настоящую беду нечем
        // будет покрасить. Так же сделано в трёх графических оболочках.
        Some(false) => vec![Span::styled(format!("{} нет связи — восстанавливаю", dot_wait()), Style::default().fg(AMBER))],
        None => vec![Span::styled(format!("{} проверяю…", dot_wait()), Style::default().fg(DIM))],
    };
    let ip = if a.my_ip.is_empty() { "—".to_string() } else { a.my_ip.clone() };
    let srv_line = if a.auto_srv == Some(true) {
        Span::styled(format!("{} свой сервер работает — 24/7 (живёт и после выхода)", dot_on()), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
    } else {
        match &a.srv {
            SrvState::Off => Span::styled(format!("{} свой сервер выключен", dot_off()), Style::default().fg(DIM)),
            SrvState::On => Span::styled(format!("{} свой сервер работает", dot_on()), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            SrvState::Failed(e) => Span::styled(format!("{} {e}", dot_err()), Style::default().fg(RED)),
        }
    };
    let head = vec![
        Line::from([link, vec![Span::styled(format!("    🌍 ваш IP: {ip}"), Style::default().fg(DIM))]].concat()),
        Line::from(srv_line),
    ];
    f.render_widget(Paragraph::new(head).wrap(Wrap { trim: true }).block(card(" Связь ")), rows[0]);

    // ── Навигируемые поля (курсор ↑↓, Enter — изменить/переключить).
    let sel = a.srv_field;
    let domain = if a.srv_cfg.domain.is_empty() { "— (без HTTPS, работает по HTTP)".to_string() } else { format!("🔒 {}", a.srv_cfg.domain) };
    let srv_on = a.auto_srv == Some(true) || matches!(a.srv, SrvState::On);
    let action = if srv_on { "■  Остановить свой сервер" } else { "▶  Запустить свой сервер" };
    let frow = |label: &str, val: String| {
        ListItem::new(Line::from(vec![
            Span::styled(format!("{label:<16}"), Style::default().fg(DIM)),
            Span::styled(val, Style::default().fg(FG)),
        ]))
    };
    let items = vec![
        frow("Координатор", a.coord.clone()),
        frow("Свой домен", domain),
        frow("Свой порт", a.srv_cfg.bind.clone()),
        ListItem::new(Line::from(Span::styled(action, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))),
    ];
    let mut st = ListState::default();
    st.select(Some(sel.min(SRV_FIELDS - 1)));
    let list = List::new(items)
        .block(card(" Настройки — Enter меняет "))
        .highlight_style(Style::default().bg(SEL).add_modifier(Modifier::BOLD))
        .highlight_symbol("");
    f.render_stateful_widget(list, rows[1], &mut st);
}

fn qr_lines(text: &str) -> Vec<String> {
    match qrcode::QrCode::new(text.as_bytes()) {
        Ok(code) => code
            .render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build()
            .lines()
            .map(|l| l.to_string())
            .collect(),
        Err(_) => vec!["(QR недоступен)".into()],
    }
}

/// Часы сеанса — общие с окном: «12:34», а не «12 мин».
///
/// Минутная подпись стояла на месте по шестьдесят секунд и читалась как
/// зависшая программа — а смотрят на неё как раз тогда, когда сомневаются,
/// работает ли VPN.
fn uptime(since: Instant) -> String {
    view::session_clock(since.elapsed().as_secs())
}

/// Имя хоста по умолчанию. НЕ имя машины: оно уходит в публичный каталог,
/// который видят все, а у людей это обычно «MacBook Air — Armen», то есть
/// настоящее имя владельца — проверено на живой машине.
///
/// Страну (как в приложениях) здесь не подставляем: базы IP→страна в терминале
/// нет, а тащить ради имени 3.4 МБ в серверный бинарь не стоит. На сервере имя
/// всё равно задают руками — меню предложит нейтральное.
fn default_host_name() -> String {
    bmv_common::default_host_name(None)
}

/// Протокол: подпись из общего справочника, значок — свой, терминальный.
///
/// Значки — эмодзи-презентации (ширина 2 в любом Unicode), чтобы терминал не
/// «съедал» пробел после них (🕶/🛡 были текст-презентации шириной 1 → липли).
/// Именно поэтому справочник отдаёт УРОВЕНЬ защиты, а не имя картинки: у окна и
/// телефонов на его месте свои наборы значков.
fn proto_short(p: &str) -> String {
    let name = view::proto_name(p);
    match view::protection(p) {
        view::Protection::Encrypted => format!("🔐 {name}"),
        view::Protection::Masked => format!("🎭 {name}"),
        view::Protection::Plain => format!("🔓 {name}"),
        // Про незнакомый протокол значок соврал бы в любую сторону.
        view::Protection::Unknown => name.to_string(),
    }
}

/// Чем красить цифру отклика. Порог задаёт справочник, цвет выбирает оболочка.
/// «Норма молчит» и «нет ответа» в терминале одинаково тихие — тревога цветная.
fn alarm_color(a: Alarm) -> Color {
    match a {
        Alarm::Amber => AMBER,
        Alarm::Red => RED,
        Alarm::Calm | Alarm::Muted => DIM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn mk(tab: Tab, vpn: Vpn, host: HostMode) -> App {
        App {
            tab,
            hosts: Vec::new(),
            sel: 0,
            vpn,
            host,
            hset: HostSettings { name: "Мой ПК".into(), max_guests: 8, password: String::new(), protocol: "noise".into(), public: true },
            host_field: 1,
            host_code: "ABCD1234".into(),
            host_sig: "SIG".into(),
            host_started: None,
            show_qr: false,
            coord: "https://bemyvpn.net".into(),
            coord_ok: Some(true),
            coord_ping: 42,
            host_ping: Some(("NG4RDJDM".into(), Some(24))),
            my_ip: "1.2.3.4".into(),
            srv: SrvState::On,
            srv_cfg: bmv_config::ServerConfig::default(),
            srv_field: 0,
            auto_srv: Some(true),
            auto_host: Some(false),
            input: None,
            toast: Some(("тост".into(), Instant::now())),
            quit: false,
        }
    }

    /// Рендер всех вкладок и состояний не паникует и укладывается в область.
    #[test]
    fn renders_all_states() {
        let mut apps = vec![
            mk(Tab::Vpn, Vpn::Off, HostMode::Off),
            mk(Tab::Vpn, Vpn::Connecting("Хост".into()), HostMode::Off),
            mk(Tab::Vpn, Vpn::On { id: "X".into(), name: "Хост".into(), since: Instant::now() }, HostMode::Off),
            mk(Tab::Vpn, Vpn::Failed("ошибка".into()), HostMode::Off),
            mk(Tab::Host, Vpn::Off, HostMode::On { code: "ABCD1234".into() }),
            mk(Tab::Server, Vpn::Off, HostMode::Off),
        ];
        // Вариант с QR и с открытым вводом.
        let mut qr = mk(Tab::Host, Vpn::Off, HostMode::On { code: "ABCD1234".into() });
        qr.show_qr = true;
        apps.push(qr);
        let mut inp = mk(Tab::Vpn, Vpn::Off, HostMode::Off);
        inp.input = Some(Input { kind: InputKind::ConnectCode, buffer: "AB12".into(), masked: false });
        apps.push(inp);

        for app in &apps {
            let mut term = ratatui::Terminal::new(TestBackend::new(90, 32)).unwrap();
            term.draw(|f| ui(f, app)).unwrap();
        }
    }

    /// Рендер со заполненным каталогом (флаги/пароль/протоколы/выбор) — без паники.
    #[test]
    fn renders_populated() {
        let mut app = mk(Tab::Vpn, Vpn::Off, HostMode::Off);
        app.hosts = vec![
            HostInfo { id: "NG4RDJDM".into(), name: "USA · Dallas".into(), online: true, guests: 2, max_guests: 120, protocol: "noise".into(), country: "🇺🇸".into(), has_password: false, endpoints: vec!["203.0.113.10:60861".into()], ..Default::default() },
            HostInfo { id: "AB12CD34".into(), name: "Домашний".into(), online: true, guests: 0, max_guests: 8, protocol: "noise-obfs".into(), country: "🇷🇺".into(), has_password: true, endpoints: vec![], ..Default::default() },
        ];
        app.sel = 1;
        for tab in [Tab::Vpn, Tab::Host, Tab::Server] {
            app.tab = tab;
            let mut term = ratatui::Terminal::new(TestBackend::new(74, 22)).unwrap();
            term.draw(|f| ui(f, &app)).unwrap();
        }
    }

    #[test]
    fn helpers() {
        // Часы сеанса и подпись протокола — общие (`bmv_common::view`), там же
        // их проверки. Здесь остаётся то, что делает сам терминал: свой значок
        // поверх общей подписи и QR.
        assert_eq!(uptime(Instant::now()), "00:00");
        assert_eq!(proto_short("noise-obfs"), "🎭 Маскировка");
        assert_eq!(proto_short(""), "🔐 Обычный");
        assert_eq!(proto_short("plain"), "🔓 Без шифра");
        // Незнакомому протоколу значка не выдаём — врать нечем.
        assert_eq!(proto_short("wireguard"), "wireguard");
        assert!(!qr_lines("bemyvpn://ABCD1234").is_empty());
    }

    /// Забитый хост не подключается: нажатие отвечает словами, а не сетью.
    ///
    /// Раньше терминал пускал попытку к хосту без мест — она уходила наружу и
    /// возвращалась отказом, а выглядело это как «программа не работает». Три
    /// другие оболочки в тот же момент гасили кнопку.
    #[test]
    fn a_full_host_is_refused_before_anything_goes_to_the_network() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _g = rt.enter();

        let mut a = mk(Tab::Vpn, Vpn::Off, HostMode::Off);
        a.hosts = vec![HostInfo { id: "FULL".into(), online: true, guests: 8, max_guests: 8, ..Default::default() }];
        a.toast = None;
        let app: Shared = Arc::new(Mutex::new(a));
        let engine: EngineSlot = Arc::new(Mutex::new(Arc::new(BmvEngine::from_config(Config::default()))));
        let mut task: VpnTask = None;

        start_vpn(&engine, &app, &mut task);

        let a = app.lock().unwrap();
        assert!(task.is_none(), "к забитому хосту не должно уходить ни одной попытки");
        assert!(matches!(a.vpn, Vpn::Off), "состояние VPN трогать не за что");
        assert!(a.toast.as_ref().is_some_and(|(t, _)| t.contains("нет свободных мест")), "молчаливый отказ — это «ничего не произошло»: {:?}", a.toast);
    }

    /// ЧАСОВОЙ НА САМОДЕДЛОК: `apply_host` не должна держать лок, пока крутится
    /// чужое замыкание.
    ///
    /// Внутрь ей передают произвольный код (`f`), а раньше вызов шёл из
    /// скрутинии `if let Some(eng) = host_engine.lock()…`, то есть guard жил
    /// через всё тело ветки. Замыкание, тронувшее тот же слот, вешало поток —
    /// вместе с перерисовкой всего меню. Дедлок не падает, он ВИСИТ, поэтому у
    /// теста обязан быть срок.
    #[test]
    fn applying_a_setting_does_not_hold_the_lock_across_foreign_code() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _g = rt.enter();

        let host_engine: HostEngine = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();

        let slot = host_engine.clone();
        let probe = host_engine.clone();
        std::thread::spawn(move || {
            // Пустой слот: замыкание не зовётся, но лок всё равно не должен
            // оставаться взятым после возврата.
            apply_host(&slot, |_e| async {});
            let free = probe.try_lock().is_ok();
            let _ = tx.send(free);
        });

        let free = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("apply_host зависла — снова lock() прямо в скрутинии ветвления");
        assert!(free, "после apply_host слот обязан быть свободен");
    }
}
