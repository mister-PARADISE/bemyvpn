//! Привилегированный туннель-хелпер и его клиент.
//!
//! Десктопный VPN требует root (создать TUN, править таблицу маршрутов, DNS).
//! Просить пользователя запускать из терминала под sudo — плохо. Вместо этого GUI
//! (под обычным пользователем) поднимает ОТДЕЛЬНЫЙ root-процесс через системный
//! диалог пароля (macOS osascript «with administrator privileges», Linux pkexec,
//! Windows UAC) — как система показывает запрос при установке VPN-профиля на iOS.
//!
//! Общение — по локальному TCP (127.0.0.1, случайный порт) с токеном (файл 0600):
//!   GUI → хелпер:  CONNECT\t<coord>\t<host>\t<pw>\t<proto>
//!                  QUICK\t<coord>\t<host1>\t<proto1>\t<host2>\t<proto2>…
//!                  STOP
//!   хелпер → GUI:  STATE\t<n>\t<id>\t<err>     (n: 0 выкл·1 подключаюсь·2 готово·3 ошибка)
//!
//! Хелпер обслуживает ОДНУ управляющую сессию (это приложение). Закрылось
//! соединение (GUI вышел) → хелпер шлёт BYE, откатывает маршруты и выходит:
//! «повисшего» root-VPN не остаётся. root запрашивается ОДИН раз за сессию.

use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::time::Duration;

use bmv_config::Config;
use bmv_core::BmvEngine;

// ── Root-сторона: сам хелпер ─────────────────────────────────────────────────

/// Запустить хелпер (мы — root). Слушает 127.0.0.1:0, порт пишет в port_file,
/// обслуживает одну сессию и выходит. Никогда не возвращается.
///
/// Только Unix (macOS/Linux): там GUI под обычным пользователем поднимает ЭТОТ
/// же бинарь отдельным root-процессом. На Windows схема иная — один процесс под
/// админом (манифест UAC), туннель крутится в нём (см. `inproc_serve`).
#[cfg(not(windows))]
pub fn run_helper(port_file: &str, token_file: &str, up_file: &str) -> ! {
    let token = std::fs::read_to_string(token_file).unwrap_or_default().trim().to_string();
    let up_file = up_file.to_string();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("rt");
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        let _ = std::fs::write(port_file, port.to_string());
        if let Ok((conn, _)) = listener.accept().await {
            serve(conn, token, up_file.clone()).await;
        }
        let _ = std::fs::remove_file(&up_file);
    });
    std::process::exit(0);
}

/// Зеркалим состояние туннеля в файл — НАДЁЖНЫЙ канal для UI в обход TCP-STATE,
/// который на «спящем» event-loop macOS до окна мог не дойти. При «подключено» (2)
/// пишем id хоста, при выкл/ошибке (0/3) — удаляем. GUI-цикл читает файл.
#[cfg(not(windows))]
fn mirror_state(up_file: &str, msg: &str) {
    let f: Vec<&str> = msg.trim().split('\t').collect();
    if f.first() == Some(&"STATE") {
        if f.get(1) == Some(&"2") {
            let _ = std::fs::write(up_file, f.get(2).copied().unwrap_or(""));
        } else {
            let _ = std::fs::remove_file(up_file);
        }
    }
}

#[cfg(not(windows))]
async fn serve(conn: tokio::net::TcpStream, token: String, up_file: String) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as ABufReader};
    let (rd, mut wr) = conn.into_split();
    let mut lines = ABufReader::new(rd).lines();

    // Первая строка — токен.
    match lines.next_line().await {
        Ok(Some(t)) if t.trim() == token => {}
        _ => return,
    }

    // Канал исходящих STATE-строк (пишут туннель-задачи, дренит этот цикл).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut current: Option<Arc<tokio::sync::Notify>> = None;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break }; // EOF: GUI закрылся
                let f: Vec<&str> = line.split('\t').collect();
                match f.first().copied() {
                    Some("CONNECT") if f.len() >= 5 => {
                        stop_current(&mut current).await;
                        let stop = Arc::new(tokio::sync::Notify::new());
                        current = Some(stop.clone());
                        let (coord, host, pw, proto) = (f[1].to_string(), f[2].to_string(), f[3].to_string(), f[4].to_string());
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let cands = vec![(host, pw, proto)];
                            run_candidates(coord, cands, tx, stop).await;
                        });
                    }
                    Some("QUICK") if f.len() >= 4 => {
                        stop_current(&mut current).await;
                        let stop = Arc::new(tokio::sync::Notify::new());
                        current = Some(stop.clone());
                        let coord = f[1].to_string();
                        let mut cands = Vec::new();
                        let mut i = 2;
                        while i + 1 < f.len() { cands.push((f[i].to_string(), String::new(), f[i + 1].to_string())); i += 2; }
                        let tx = tx.clone();
                        tokio::spawn(async move { run_candidates(coord, cands, tx, stop).await; });
                    }
                    Some("STOP") => {
                        stop_current(&mut current).await;
                        let _ = tx.send("STATE\t0\t\t".to_string());
                    }
                    _ => {}
                }
            }
            Some(msg) = rx.recv() => {
                mirror_state(&up_file, &msg);
                if wr.write_all(format!("{msg}\n").as_bytes()).await.is_err() { break; }
                let _ = wr.flush().await;
            }
        }
    }
    // GUI закрылся → гасим туннель (BYE + откат маршрутов происходит в задаче).
    stop_current(&mut current).await;
    let _ = std::fs::remove_file(&up_file);
    // Небольшая пауза, чтобы BYE успел уйти до выхода процесса.
    tokio::time::sleep(Duration::from_millis(250)).await;
}

async fn stop_current(current: &mut Option<Arc<tokio::sync::Notify>>) {
    if let Some(stop) = current.take() {
        stop.notify_waiters();
        // Дать задаче закрыть канал (BYE) и снять RouteGuard.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Перебрать кандидатов: первый, к кому удалось подключиться за 8с, — запускаем
/// туннель. Умный «Старт» на iOS так же перебирает; порядок задаёт GUI.
async fn run_candidates(
    coord: String,
    cands: Vec<(String, String, String)>, // (host, pw, proto)
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    stop: Arc<tokio::sync::Notify>,
) {
    let cfg = Config { coordinators: vec![coord], ..Default::default() };
    let eng = Arc::new(BmvEngine::from_config(cfg));

    for (host, pw, proto) in cands {
        let _ = tx.send(format!("STATE\t1\t{host}\t"));
        let pw_opt = (!pw.is_empty()).then_some(pw.clone());
        let proto_opt = (!proto.is_empty()).then_some(proto.clone());
        let est_fut = tokio::time::timeout(Duration::from_secs(8), eng.guest_establish(&host, pw_opt.as_deref(), proto_opt.as_deref()));
        tokio::pin!(est_fut);
        let est = tokio::select! {
            _ = stop.notified() => {
                // Отключились ВО ВРЕМЯ подключения: даём establish короткую фору —
                // если link успел подняться, шлём BYE (хост мог уже завести сессию).
                if let Ok(Ok(Ok((_p, link)))) = tokio::time::timeout(Duration::from_secs(2), &mut est_fut).await {
                    let _ = tokio::time::timeout(Duration::from_millis(600), link.close()).await;
                }
                let _ = tx.send("STATE\t0\t\t".to_string());
                return;
            }
            r = &mut est_fut => r,
        };
        if let Ok(Ok((peer, link))) = est {
            run_tunnel(&host, peer, link, &tx, &stop).await;
            return;
        }
    }
    let _ = tx.send("STATE\t3\t\tне удалось подключиться".to_string());
}

async fn run_tunnel(
    host: &str,
    peer: std::net::SocketAddr,
    link: Box<dyn bmv_common::Link>,
    tx: &tokio::sync::mpsc::UnboundedSender<String>,
    stop: &Arc<tokio::sync::Notify>,
) {
    let params = bmv_tunnel::TunParams::guest();
    let (device, ifname) = match bmv_desktop::make_tun(&params) {
        Ok(d) => d,
        Err(e) => { let _ = tx.send(format!("STATE\t3\t\tTUN: {e}")); return; }
    };
    let _guard = match bmv_desktop::RouteGuard::install(peer.ip(), &ifname) {
        Ok(g) => g,
        Err(e) => { let _ = tx.send(format!("STATE\t3\t\tмаршрут: {e}")); return; }
    };
    let _ = tx.send(format!("STATE\t2\t{host}\t"));
    let link_arc: Arc<dyn bmv_common::Link> = Arc::from(link);
    tokio::select! {
        _ = bmv_tunnel::run_guest(device, link_arc.clone()) => {}
        _ = stop.notified() => {}
    }
    // ВСЕГДА прощаемся (BYE) — и при «Стоп», и если run_guest завершился сам
    // (обрыв/ошибка). Хост увидит EOF сразу и снимет гостя из счётчика, не ожидая
    // keepalive-таймаута (8с). BYE идёт ДО снятия RouteGuard (_guard жив до конца).
    let _ = tokio::time::timeout(Duration::from_millis(600), link_arc.close()).await;
    let _ = tx.send("STATE\t0\t\t".to_string()); // guard снимется здесь (Drop) — маршруты откатятся
}

// ── Клиентская сторона (в GUI под пользователем) ─────────────────────────────

/// Управляет root-хелпером через ФОНОВЫЙ поток-воркер: подъём хелпера (osascript
/// с запросом пароля БЛОКИРУЕТ — поэтому не на главном потоке, иначе окно замрёт),
/// отправка команд, чтение STATE. GUI лишь шлёт строки в канал (мгновенно).
///
/// Только Unix. На Windows — версия ниже (`#[cfg(windows)]`), которая крутит
/// туннель прямо в процессе (он уже под админом), без отдельного root-exe.
#[cfg(not(windows))]
pub struct Helper {
    tx: std::sync::mpsc::Sender<String>,
}

#[cfg(not(windows))]
impl Helper {
    pub fn new(on_state: Arc<dyn Fn(i32, String, String) + Send + Sync>, up_file: std::path::PathBuf) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || worker(rx, on_state, up_file));
        Self { tx }
    }

    /// Подключиться к одному хосту.
    pub fn connect(&self, coord: &str, host: &str, pw: &str, proto: &str) {
        let _ = self.tx.send(format!("CONNECT\t{}\t{}\t{}\t{}", clean(coord), clean(host), clean(pw), clean(proto)));
    }

    /// Умный «Старт»: упорядоченный список (host,proto) — перебор в хелпере.
    pub fn quick(&self, coord: &str, cands: &[(String, String)]) {
        let mut cmd = format!("QUICK\t{}", clean(coord));
        for (h, p) in cands {
            cmd.push('\t'); cmd.push_str(&clean(h));
            cmd.push('\t'); cmd.push_str(&clean(p));
        }
        let _ = self.tx.send(cmd);
    }

    pub fn stop(&self) {
        let _ = self.tx.send("STOP".to_string());
    }
}

// ── Windows: тот же интерфейс, но туннель В ЭТОМ ЖЕ процессе ──────────────────
//
// Приложение поднято с манифестом requireAdministrator → уже под админом, и
// отдельный root-процесс не нужен (никаких «двух процессов»). Команды идут в
// канал, а STATE — прямо в on_state; логика туннеля (`run_candidates`) — общая
// с Unix. UAC-диалог показывает сама Windows при запуске.
#[cfg(windows)]
pub struct Helper {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
}

#[cfg(windows)]
impl Helper {
    // _up_file не нужен на Windows: туннель в этом же процессе, on_state зовётся
    // напрямую (нет «спящего» event-loop как на macOS). Приняли для общей сигнатуры.
    pub fn new(on_state: Arc<dyn Fn(i32, String, String) + Send + Sync>, _up_file: std::path::PathBuf) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("rt");
            rt.block_on(inproc_serve(rx, on_state));
        });
        Self { tx }
    }

    pub fn connect(&self, coord: &str, host: &str, pw: &str, proto: &str) {
        let _ = self.tx.send(format!("CONNECT\t{}\t{}\t{}\t{}", clean(coord), clean(host), clean(pw), clean(proto)));
    }

    pub fn quick(&self, coord: &str, cands: &[(String, String)]) {
        let mut cmd = format!("QUICK\t{}", clean(coord));
        for (h, p) in cands {
            cmd.push('\t'); cmd.push_str(&clean(h));
            cmd.push('\t'); cmd.push_str(&clean(p));
        }
        let _ = self.tx.send(cmd);
    }

    pub fn stop(&self) {
        let _ = self.tx.send("STOP".to_string());
    }
}

/// В-процессный аналог `serve` (Windows): команды приходят из канала, а не по TCP,
/// и STATE идёт прямо в on_state. Логика перебора кандидатов и качания туннеля —
/// та же `run_candidates`, что на Unix. Канал сброшен (GUI закрылся) → гасим туннель.
#[cfg(windows)]
async fn inproc_serve(
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    on_state: Arc<dyn Fn(i32, String, String) + Send + Sync>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut current: Option<Arc<tokio::sync::Notify>> = None;
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(line) = cmd else { break }; // GUI закрылся
                let f: Vec<&str> = line.split('\t').collect();
                match f.first().copied() {
                    Some("CONNECT") if f.len() >= 5 => {
                        stop_current(&mut current).await;
                        let stop = Arc::new(tokio::sync::Notify::new());
                        current = Some(stop.clone());
                        let (coord, host, pw, proto) = (f[1].to_string(), f[2].to_string(), f[3].to_string(), f[4].to_string());
                        let tx = tx.clone();
                        tokio::spawn(async move { run_candidates(coord, vec![(host, pw, proto)], tx, stop).await; });
                    }
                    Some("QUICK") if f.len() >= 4 => {
                        stop_current(&mut current).await;
                        let stop = Arc::new(tokio::sync::Notify::new());
                        current = Some(stop.clone());
                        let coord = f[1].to_string();
                        let mut cands = Vec::new();
                        let mut i = 2;
                        while i + 1 < f.len() { cands.push((f[i].to_string(), String::new(), f[i + 1].to_string())); i += 2; }
                        let tx = tx.clone();
                        tokio::spawn(async move { run_candidates(coord, cands, tx, stop).await; });
                    }
                    Some("STOP") => {
                        stop_current(&mut current).await;
                        on_state(0, String::new(), String::new());
                    }
                    _ => {}
                }
            }
            Some(msg) = rx.recv() => {
                let f: Vec<&str> = msg.trim_end().split('\t').collect();
                if f.first() == Some(&"STATE") && f.len() >= 4 {
                    let n: i32 = f[1].parse().unwrap_or(0);
                    on_state(n, f[2].to_string(), f[3].to_string());
                }
            }
        }
    }
    stop_current(&mut current).await; // откат маршрутов при закрытии GUI
    tokio::time::sleep(Duration::from_millis(250)).await;
}

/// Фоновый воркер: держит соединение к root-хелперу, гонит команды из канала,
/// читает STATE в отдельном под-потоке. Первая команда поднимает хелпер (пароль).
#[cfg(not(windows))]
fn worker(rx: std::sync::mpsc::Receiver<String>, on_state: Arc<dyn Fn(i32, String, String) + Send + Sync>, up_file: std::path::PathBuf) {
    let mut conn: Option<std::net::TcpStream> = None;
    while let Ok(cmd) = rx.recv() {
        if cmd == "STOP" && conn.is_none() {
            continue; // нечего останавливать
        }
        if conn.is_none() {
            // UI уже показывает «Подключаюсь…» (begin() на главном потоке); пока
            // ждём пароль/подъём — не трогаем, хелпер пришлёт свой STATE 1 с id.
            match spawn_and_connect(on_state.clone(), &up_file) {
                Ok(s) => conn = Some(s),
                Err(e) => { on_state(3, String::new(), e); continue; }
            }
        }
        if let Some(s) = conn.as_mut() {
            if writeln!(s, "{cmd}").is_err() {
                conn = None; // умерло — следующая команда переподнимет
            }
        }
    }
}

/// Поднять root-хелпер (запрос пароля), дождаться порта, подключиться, запустить
/// чтение STATE. Блокирующая — вызывается ТОЛЬКО из воркер-потока.
#[cfg(not(windows))]
fn spawn_and_connect(on_state: Arc<dyn Fn(i32, String, String) + Send + Sync>, up_file: &std::path::Path) -> Result<std::net::TcpStream, String> {
    // Внутри AppImage current_exe() — путь в FUSE-монтировании (/tmp/.mount_*),
    // недоступном для root от pkexec. AppImage кладёт внешний путь в $APPIMAGE —
    // запускаем помощника через него (перемонтирует себя и увидит --tunnel-helper).
    let exe = std::env::var_os("APPIMAGE")
        .map(std::path::PathBuf::from)
        .map_or_else(std::env::current_exe, Ok)
        .map_err(|e| e.to_string())?;
    let dir = std::env::temp_dir();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let port_file = dir.join(format!("bemyvpn-port-{stamp}"));
    let token_file = dir.join(format!("bemyvpn-tok-{stamp}"));
    let token = format!("{stamp:x}{:x}", std::process::id());
    write_private(&token_file, &token)?;
    let _ = std::fs::remove_file(&port_file);
    let _ = std::fs::remove_file(up_file); // свежий старт: старый маркер не путает GUI

    elevate_launch(&exe, &port_file, &token_file, up_file)?;

    // Ждём, пока хелпер (после ввода пароля) напишет порт (до 120с).
    let mut port = 0u16;
    for _ in 0..1200 {
        if let Ok(s) = std::fs::read_to_string(&port_file) {
            if let Ok(p) = s.trim().parse::<u16>() { if p != 0 { port = p; break; } }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(&token_file);
    let _ = std::fs::remove_file(&port_file);
    if port == 0 {
        return Err("привилегии не получены (пароль отменён?)".into());
    }

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    writeln!(stream, "{token}").map_err(|e| e.to_string())?;
    let reader = stream.try_clone().map_err(|e| e.to_string())?;
    std::thread::spawn(move || read_loop(reader, on_state));
    Ok(stream)
}

#[cfg(not(windows))]
fn read_loop(stream: std::net::TcpStream, on_state: Arc<dyn Fn(i32, String, String) + Send + Sync>) {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match r.read_line(&mut line) {
            Ok(0) | Err(_) => { on_state(0, String::new(), String::new()); break; }
            Ok(_) => {
                let f: Vec<&str> = line.trim_end().split('\t').collect();
                if f.first() == Some(&"STATE") && f.len() >= 4 {
                    let n: i32 = f[1].parse().unwrap_or(0);
                    on_state(n, f[2].to_string(), f[3].to_string());
                }
            }
        }
    }
}

/// Табы/переводы строк в полях недопустимы (разделители протокола) — вырезаем.
fn clean(s: &str) -> String {
    s.chars().filter(|c| *c != '\t' && *c != '\n' && *c != '\r').collect()
}

#[cfg(not(windows))]
fn write_private(path: &std::path::Path, data: &str) -> Result<(), String> {
    std::fs::write(path, data).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

// ── Запуск root-процесса с системным запросом пароля ─────────────────────────

#[cfg(target_os = "macos")]
fn elevate_launch(exe: &std::path::Path, port_file: &std::path::Path, token_file: &std::path::Path, up_file: &std::path::Path) -> Result<(), String> {
    // Шелл-команда в одинарных кавычках (пути с пробелами ок), фоном (&) —
    // osascript вернётся сразу после ввода пароля. AppleScript-строка в двойных.
    let sh = format!(
        "'{}' --tunnel-helper '{}' '{}' '{}' >/dev/null 2>&1 &",
        exe.display(), port_file.display(), token_file.display(), up_file.display()
    );
    let script = format!("do shell script \"{}\" with administrator privileges", sh.replace('\\', "\\\\").replace('"', "\\\""));
    let status = std::process::Command::new("osascript").arg("-e").arg(script).status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err("запрос прав отменён".into()) }
}

#[cfg(target_os = "linux")]
fn elevate_launch(exe: &std::path::Path, port_file: &std::path::Path, token_file: &std::path::Path, up_file: &std::path::Path) -> Result<(), String> {
    // pkexec показывает графический запрос пароля (нужен polkit-агент рабочего стола).
    std::process::Command::new("pkexec")
        .arg(exe).arg("--tunnel-helper").arg(port_file).arg(token_file).arg(up_file)
        .spawn().map(|_| ()).map_err(|e| format!("pkexec: {e}"))
}

// На Windows отдельного root-процесса нет: приложение уже под админом (манифест
// UAC), туннель качается в самом процессе (см. `inproc_serve`). Поэтому здесь
// нет windows-варианта elevate_launch — он не нужен.
