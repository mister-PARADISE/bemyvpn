//! Привилегированный туннель-хелпер и его клиент.
//!
//! Десктопный VPN требует root (создать TUN, править таблицу маршрутов, DNS).
//! Просить пользователя запускать из терминала под sudo — плохо. Вместо этого GUI
//! (под обычным пользователем) поднимает ОТДЕЛЬНЫЙ root-процесс через системный
//! диалог пароля (macOS osascript «with administrator privileges», Linux pkexec,
//! Windows UAC) — как система показывает запрос при установке VPN-профиля на iOS.
//!
//! Файлы обмена (порт, токен, маркер «туннель поднят») лежат в СВОЁМ каталоге
//! 0700 во временной папке, а не прямо в ней: на Linux /tmp общий для всех
//! пользователей машины, и по предсказуемому пути сосед подкладывал симлинк на
//! чужой файл — писал-то по нему root. См. `private_dir` и `write_nofollow`.
//!
//! Общение — по локальному TCP (127.0.0.1, случайный порт) с токеном (файл 0600):
//!   GUI → хелпер:  CONNECT\t<coord>\t<host>\t<pw>\t<proto>
//!                  QUICK\t<coord>\t<host1>\t<proto1>\t<host2>\t<proto2>…
//!                  STOP
//!   хелпер → GUI:  STATE\t<n>\t<id>\t<err>
//!                  (n: 0 выкл · 1 подключаюсь · 2 готово · 3 ошибка ·
//!                      4 хост завершил раздачу — конец сеанса БЕЗ ошибки)
//!
//! Хелпер обслуживает ОДНУ УДОСТОВЕРЕННУЮ управляющую сессию (это приложение),
//! но подключения принимает В ЦИКЛЕ: неудачная попытка соседа больше не роняет
//! его до прихода настоящего GUI. Закрылось соединение (GUI вышел) → хелпер
//! шлёт BYE, откатывает маршруты и выходит: «повисшего» root-VPN не остаётся.
//! root запрашивается ОДИН раз за сессию.

use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::time::Duration;

use bmv_desktop::tunnel::{self, State};

// ── Root-сторона: сам хелпер ─────────────────────────────────────────────────

/// Сколько root-помощник ждёт СВОЁ окно, прежде чем уйти сам.
///
/// Без предела помощник жил вечно: не сумев прочитать файл порта (была ошибка
/// прав), окно уходило с ошибкой, а помощник продолжал крутить приём в ожидании
/// клиента, которого уже не будет. Каждая неудачная попытка оставляла на машине
/// ещё один бессмертный root-процесс.
///
/// Щедро НАРОЧНО. Пароль спрашивают ДО запуска помощника (osascript/pkexec
/// возвращаются, уже подняв root-процесс), поэтому в норме окно подключается за
/// доли секунды — но уходить раньше самого окна нельзя: оно ждёт файл порта до
/// 120 секунд, и помощник, исчезнувший на 119-й, превратил бы редкую заминку в
/// стабильный отказ. Три минуты перекрывают его ожидание с запасом.
///
/// Срок относится ТОЛЬКО к ожиданию клиента: как только сессия удостоверена,
/// помощник живёт ровно столько, сколько живёт окно, — хоть сутки.
#[cfg(not(windows))]
const CLAIM_WINDOW: Duration = Duration::from_secs(180);

/// Годится ли предъявленный токен.
///
/// Отдельной функцией потому, что ошибиться тут стоит дороже всего в программе:
/// хелпер — это root, который по команде CONNECT заворачивает ВЕСЬ трафик
/// машины на указанный ему координатор. Раньше токен читался как
/// `read_to_string(...).unwrap_or_default()`, то есть при любом сбое чтения
/// становился ПУСТОЙ строкой — и тогда любой локальный процесс, приславший
/// пустую первую строку, получал этого root-хелпера в своё распоряжение.
fn token_ok(expected: &str, got: &str) -> bool {
    !expected.is_empty() && got.trim() == expected
}

/// Запустить хелпер (мы — root). Слушает 127.0.0.1:0, порт пишет в port_file,
/// обслуживает одну УДОСТОВЕРЕННУЮ сессию и выходит. Никогда не возвращается.
///
/// Только Unix (macOS/Linux): там GUI под обычным пользователем поднимает ЭТОТ
/// же бинарь отдельным root-процессом. На Windows схема иная — один процесс под
/// админом (манифест UAC), туннель крутится в нём (см. `inproc_serve`).
#[cfg(not(windows))]
pub fn run_helper(port_file: &str, token_file: &str, up_file: &str) -> ! {
    // Весь вывод уходит в журнал рядом с файлом порта (см. `helper_log`), и это
    // ЕДИНСТВЕННОЕ, что видно про root-процесс: окно его не породило напрямую,
    // ни кода возврата, ни stderr оно не получит.
    let raw = std::fs::read_to_string(token_file);
    if let Err(e) = &raw {
        eprintln!("хелпер: токен {token_file} не прочитался: {e}");
    }
    let token = raw.unwrap_or_default().trim().to_string();
    // Нет токена — нет и работы. Продолжить значило бы поднять root-хелпера,
    // который принимает команды от кого угодно.
    if token.is_empty() {
        eprintln!("хелпер: токен пуст — выходим, туннеля не будет");
        std::process::exit(1);
    }
    // Путь к настройкам — ПЯТЫЙ аргумент, и читаем мы его ЗДЕСЬ, а не в main.rs:
    // командную строку помощника собирает `elevate_launch` из этого же файла, и
    // оба конца договорённости должны лежать рядом. Пусто/нет — файла настроек у
    // человека нет вовсе, тогда правда в умолчаниях (см. `guest_config`).
    let cfg_file = std::env::args().nth(5).filter(|s| !s.is_empty()).map(std::path::PathBuf::from);
    // В журнал — иначе «настройки не доехали» выглядит как «настройки не работают»,
    // а отличить одно от другого в root-процессе больше нечем.
    match &cfg_file {
        Some(p) => eprintln!("хелпер: настройки беру из {}", p.display()),
        None => eprintln!("хелпер: файла настроек нет — беру стандартные"),
    }
    let up_file = up_file.to_string();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("rt");
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        // 0644, а НЕ 0600: этот файл пишет root, а читает окно под ОБЫЧНЫМ
        // пользователем. См. `write_readable`.
        match write_readable(std::path::Path::new(port_file), &port.to_string()) {
            Ok(()) => eprintln!("хелпер: слушаю 127.0.0.1:{port}, порт записан в {port_file}"),
            // Окно теперь ждёт впустую все 120 секунд — пусть хотя бы знает, почему.
            Err(e) => eprintln!("хелпер: порт {port} не записался в {port_file}: {e}"),
        }
        accept_until(listener, &token, up_file.clone(), cfg_file, tokio::time::Instant::now() + CLAIM_WINDOW).await;
        let _ = std::fs::remove_file(&up_file);
    });
    std::process::exit(0);
}

/// Зеркалим состояние туннеля в файл — НАДЁЖНЫЙ канал для UI в обход разового
/// сигнала STATE, который до окна может не дойти. При «подключено» (2) пишем id
/// хоста, при выкл/ошибке (0/3) — удаляем. GUI-цикл читает файл и догоняет.
///
/// Маркер пишут ВСЕ платформы. Раньше Windows его не писала: считалось, что там
/// «нет спящего event-loop, как на macOS», и разовый STATE дойдёт всегда. Не
/// дошёл — статус навсегда застревал на «Подключаюсь…», хотя туннель уже качал
/// трафик. Догонять было нечем: единственный канал и потерялся. Одинаковый
/// механизм на всех платформах надёжнее двух расходящихся.
fn mirror_state(up_file: &str, msg: &str) {
    let f: Vec<&str> = msg.trim().split('\t').collect();
    if f.first() == Some(&"STATE") {
        if f.get(1) == Some(&"2") {
            // Тоже 0644: маркер пишет root, а тик-цикл окна читает под пользователем.
            let _ = write_readable(std::path::Path::new(up_file), f.get(2).copied().unwrap_or(""));
        } else {
            let _ = std::fs::remove_file(up_file);
        }
    }
}

/// Записать файл, НЕ ИДЯ ПО СИМЛИНКУ.
///
/// Этим пишет root-хелпер, а раньше здесь стоял `std::fs::write`, который по
/// симлинку идёт. На Linux /tmp общий для всех пользователей машины, поэтому
/// локальный сосед мог заранее положить по нашему пути ссылку на любой
/// root-овый файл — и root покорно затирал его нашим содержимым. Тем же файлом
/// сосед рисовал в чужом окне ложное «подключено»: тик-цикл в main.rs считает
/// этот файл авторитетом.
///
/// O_NOFOLLOW — вторая линия обороны (первая — приватный каталог 0700, см.
/// `private_dir`): открытие ПАДАЕТ, если по пути оказался симлинк, вместо того
/// чтобы молча писать в чужой файл. На Windows временный каталог и так
/// пер-пользовательский, флага там нет — пишем обычно.
///
/// `mode` — права В МОМЕНТ СОЗДАНИЯ (а не chmod'ом после): см. `write_private`
/// про токен и `write_readable` про файлы, которые пишет root, а читает окно.
fn write_nofollow(path: &std::path::Path, data: &str, mode: u32) -> std::io::Result<()> {
    let _ = mode; // на Windows прав POSIX нет
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
        opts.mode(mode);
    }
    let mut f = opts.open(path)?;
    f.write_all(data.as_bytes())
}

/// Файл, который пишет ROOT-ХЕЛПЕР, а читает окно под ОБЫЧНЫМ пользователем:
/// порт хелпера и маркер «туннель поднят».
///
/// РЕГРЕССИЯ, из-за которой на macOS подключение висло на «Подключаюсь…»:
/// эти два файла создавались с правами 0600 — как токен. Владельцем-то был
/// root (он их и пишет), и окно под пользователем читало их с EACCES. Порт не
/// прочитывался НИКОГДА, цикл ожидания вырабатывал все 120 секунд и врал
/// «привилегии не получены»; маркер по той же причине не читался, и тик-цикл
/// гасил бы уже поднятый туннель обратно в «VPN выключен». Раньше здесь стоял
/// `fs::write`, дававший 0644 по umask, — оттого и работало.
///
/// Приватность даёт КАТАЛОГ 0700 (`private_dir`), а не права файла: внутрь него
/// посторонний не войдёт при любом содержимом. Явный chmod следом — потому что
/// umask root-процесса нам неизвестен, а `open` режет права по нему (при umask
/// 077 из 0644 вышло бы ровно то же 0600 и ровно то же зависание).
fn write_readable(path: &std::path::Path, data: &str) -> std::io::Result<()> {
    write_nofollow(path, data, 0o644)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

/// Каталог обмена с root-хелпером — ТОЛЬКО НАШ (0700, случайное имя).
///
/// Все три файла (порт, токен, маркер «туннель поднят») лежат здесь, а не прямо
/// в общем /tmp. В каталог с правами 0700 сосед не войдёт и симлинк внутри не
/// подложит, поэтому подмена цели записи невозможна ещё до всяких O_NOFOLLOW.
/// Имя случайное, а не по pid: pid переиспользуются, и остаток от прошлого
/// запуска (в т.ч. чужого) мешал бы создать каталог заново.
pub fn private_dir() -> std::path::PathBuf {
    let stamp: u128 = {
        use rand::Rng;
        rand::thread_rng().gen()
    };
    let dir = std::env::temp_dir().join(format!("bemyvpn-{stamp:032x}"));
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    // create (не create_all) и без recursive: имя случайное, столкновение = чужой
    // каталог, и тогда лучше упасть на записи, чем писать в него.
    let _ = b.create(&dir);
    dir
}

/// Принимать соединения, пока не отработает НАСТОЯЩАЯ сессия — либо пока не
/// истёк `deadline` (окно так и не пришло).
///
/// ЦИКЛ, а не одно соединение: раньше хелпер обслуживал ровно первое подключение,
/// и сосед по машине, успевший подключиться раньше нашего окна и приславший
/// мусор вместо токена, ронял хелпера — VPN не поднимался вовсе, а выглядело это
/// как «не сработал запрос пароля».
///
/// Срок АБСОЛЮТНЫЙ (`timeout_at` от общего момента), а не «столько-то на каждый
/// accept»: иначе тот же сосед, стучащийся раз в минуту, продлевал бы жизнь
/// root-процесса бесконечно.
///
/// Отдельной функцией — чтобы это можно было проверить тестом: `run_helper`
/// возвращает `!` и зовёт `exit`, внутри теста его не запустить.
#[cfg(not(windows))]
async fn accept_until(
    listener: tokio::net::TcpListener,
    token: &str,
    up_file: String,
    cfg_file: Option<std::path::PathBuf>,
    deadline: tokio::time::Instant,
) {
    loop {
        match tokio::time::timeout_at(deadline, listener.accept()).await {
            Err(_) => {
                eprintln!("хелпер: окно не подключилось в срок — выхожу, чтобы не висеть root-процессом");
                return;
            }
            Ok(Err(e)) => {
                eprintln!("хелпер: accept не работает ({e}) — выхожу");
                return;
            }
            Ok(Ok((conn, _))) => {
                if serve(conn, token, up_file.clone(), cfg_file.clone()).await {
                    return;
                }
            }
        }
    }
}

/// Обслужить одно управляющее соединение. `true` — это была НАША сессия (можно
/// выходить), `false` — не удостоверился, ждём следующего.
#[cfg(not(windows))]
async fn serve(conn: tokio::net::TcpStream, token: &str, up_file: String, cfg_file: Option<std::path::PathBuf>) -> bool {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as ABufReader};
    let (rd, mut wr) = conn.into_split();
    let mut lines = ABufReader::new(rd).lines();

    // Первая строка — токен.
    match lines.next_line().await {
        Ok(Some(t)) if token_ok(token, &t) => {}
        _ => return false,
    }

    // Канал исходящих STATE-строк (пишут туннель-задачи, дренит этот цикл).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut current: Current = None;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break }; // EOF: GUI закрылся
                let f: Vec<&str> = line.split('\t').collect();
                match f.first().copied() {
                    Some("CONNECT") if f.len() >= 5 => {
                        stop_current(&mut current).await;
                        let stop = Arc::new(tokio::sync::Notify::new());
                        let (coord, host, pw, proto) = (f[1].to_string(), f[2].to_string(), f[3].to_string(), f[4].to_string());
                        let (tx, stop2, cfg) = (tx.clone(), stop.clone(), cfg_file.clone());
                        let h = tokio::spawn(async move {
                            let cands = vec![(host, pw, proto)];
                            tunnel::run_candidates(guest_config(cfg.as_deref(), coord), cands, move |s| { let _ = tx.send(state_line(&s)); }, stop2).await;
                        });
                        current = Some((stop, h));
                    }
                    Some("QUICK") if f.len() >= 4 => {
                        stop_current(&mut current).await;
                        let stop = Arc::new(tokio::sync::Notify::new());
                        let coord = f[1].to_string();
                        let cands = parse_quick(&f);
                        let (tx, stop2, cfg) = (tx.clone(), stop.clone(), cfg_file.clone());
                        let h = tokio::spawn(async move {
                            tunnel::run_candidates(guest_config(cfg.as_deref(), coord), cands, move |s| { let _ = tx.send(state_line(&s)); }, stop2).await;
                        });
                        current = Some((stop, h));
                    }
                    Some("STOP") => {
                        stop_current(&mut current).await;
                        let _ = tx.send(state_line(&State::Off));
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
    // ЕДИНСТВЕННЫЙ выход из цикла — соединение с окном кончилось: окно закрыли,
    // окно упало, окно убили. Разбираться, что именно, незачем: управлять
    // помощником больше некому, и жить ему дальше не для кого. Гасим туннель и
    // ЖДЁМ, пока задача попрощается и откатит маршруты (stop_current), — только
    // после этого возвращаем true, по которому процесс выходит.
    stop_current(&mut current).await;
    let _ = std::fs::remove_file(&up_file);
    true
}

/// Конфиг гостя для туннеля — НАСТОЯЩИЙ файл настроек, а не умолчания.
///
/// Здесь стояло `Config::default()` с одним подставленным координатором, и всё
/// остальное, что человек прописал в файле, до туннеля не доезжало МОЛЧА: свой
/// список STUN-серверов, настройки протоколов и `guest.ipv6`. Последнее означало,
/// что снять блокировку IPv6 из окна было в принципе невозможно — а она нужна
/// там, где v6 единственный транспорт (NAT64/464XLAT), иначе человек остаётся не
/// «без VPN», а вообще без интернета. TUI ту же беду уже прошёл, см. `connect_queue`
/// в apps/bmv-cli/src/tui.rs.
///
/// Путь ПРИХОДИТ ИЗВНЕ, а не ищется здесь: на Unix эта функция крутится в
/// root-процессе с чужим HOME (`/var/root`, `/root`), и автопоиск нашёл бы там не
/// тот файл или ничего. Ищет его сторона пользователя (`spawn_and_connect`).
/// `None` — файла нет вовсе, тогда умолчания и есть правда.
///
/// Координатор всё равно подменяем: в окне его меняют, не сохраняя, и адрес из
/// поля должен побеждать записанный в файле.
///
/// ОШИБКУ РАЗБОРА НЕ ПЕРЕСКАЗЫВАЕМ. Текст ошибки toml несёт в себе кусок самого
/// файла, а открывает файл здесь root — по пути, пришедшему снаружи. Показать
/// такую ошибку значило бы чужими руками читать чужие root-овые файлы.
fn guest_config(cfg_path: Option<&std::path::Path>, coord: String) -> bmv_config::Config {
    let mut cfg = cfg_path
        .and_then(|p| {
            bmv_config::Config::from_file(p)
                .map_err(|_| eprintln!("хелпер: настройки {} не прочитались — беру стандартные", p.display()))
                .ok()
        })
        .unwrap_or_default();
    cfg.coordinators = vec![coord];
    cfg
}

/// Состояние туннеля → строка проводного протокола `STATE\t<n>\t<id>\t<err>`.
fn state_line(s: &State) -> String {
    match s {
        State::Connecting(id) => format!("STATE\t1\t{id}\t"),
        State::Up(id) => format!("STATE\t2\t{id}\t"),
        State::Off => "STATE\t0\t\t".to_string(),
        // 4, а не 0 с текстом: хост завершил раздачу — окно должно сказать
        // человеку, что произошло, но БЕЗ вида ошибки (для этого есть 3).
        State::HostLeft => "STATE\t4\t\t".to_string(),
        State::Failed(e) => format!("STATE\t3\t\t{e}"),
    }
}

/// Разобрать команду QUICK: пары (host, proto) начиная с поля 2.
fn parse_quick(f: &[&str]) -> Vec<(String, String, String)> {
    let mut cands = Vec::new();
    let mut i = 2;
    while i + 1 < f.len() {
        cands.push((f[i].to_string(), String::new(), f[i + 1].to_string()));
        i += 2;
    }
    cands
}

/// Текущая туннель-задача: её «стоп» И её хэндл.
///
/// Хэндл нужен, чтобы ДОЖДАТЬСЯ конца задачи, а не спать наугад: перед выходом
/// она шлёт BYE (до 600мс) и откатывает маршруты с DNS, а на macOS откат — это
/// `networksetup`, то есть ещё до секунды. Раньше здесь спали 50мс, а `serve` в
/// конце — ещё 250мс, после чего root-процесс выходил. Когда окно ПАДАЛО с
/// поднятым туннелем, помощник успевал исчезнуть раньше отката, и машина
/// оставалась с дефолтным маршрутом в мёртвый TUN — то есть без интернета.
type Current = Option<(Arc<tokio::sync::Notify>, tokio::task::JoinHandle<()>)>;

async fn stop_current(current: &mut Current) {
    let Some((stop, handle)) = current.take() else { return };
    stop.notify_waiters();
    // Предел на случай залипшей задачи: вечно висящий root-процесс хуже, чем
    // недооткаченный маршрут (его чинит следующий запуск).
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
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
    pub fn new(on_state: Arc<dyn Fn(i32, String, String) + Send + Sync>, up_file: std::path::PathBuf) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let up = up_file.to_string_lossy().to_string();
        // Здесь туннель крутится в ЭТОМ ЖЕ процессе (под тем же пользователем),
        // поэтому путь к настройкам ищется прямо тут — отдельного root-процесса,
        // которому его пришлось бы передавать, на Windows нет.
        let cfg_file = bmv_config::Config::path();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("rt");
            rt.block_on(inproc_serve(rx, on_state, up, cfg_file));
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
    up_file: String,
    cfg_file: Option<std::path::PathBuf>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut current: Current = None;
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(line) = cmd else { break }; // GUI закрылся
                let f: Vec<&str> = line.split('\t').collect();
                match f.first().copied() {
                    Some("CONNECT") if f.len() >= 5 => {
                        stop_current(&mut current).await;
                        let stop = Arc::new(tokio::sync::Notify::new());
                        let (coord, host, pw, proto) = (f[1].to_string(), f[2].to_string(), f[3].to_string(), f[4].to_string());
                        let (tx, stop2, cfg) = (tx.clone(), stop.clone(), cfg_file.clone());
                        let h = tokio::spawn(async move {
                            tunnel::run_candidates(guest_config(cfg.as_deref(), coord), vec![(host, pw, proto)], move |s| { let _ = tx.send(state_line(&s)); }, stop2).await;
                        });
                        current = Some((stop, h));
                    }
                    Some("QUICK") if f.len() >= 4 => {
                        stop_current(&mut current).await;
                        let stop = Arc::new(tokio::sync::Notify::new());
                        let coord = f[1].to_string();
                        let cands = parse_quick(&f);
                        let (tx, stop2, cfg) = (tx.clone(), stop.clone(), cfg_file.clone());
                        let h = tokio::spawn(async move {
                            tunnel::run_candidates(guest_config(cfg.as_deref(), coord), cands, move |s| { let _ = tx.send(state_line(&s)); }, stop2).await;
                        });
                        current = Some((stop, h));
                    }
                    Some("STOP") => {
                        stop_current(&mut current).await;
                        mirror_state(&up_file, "STATE\t0\t\t");
                        on_state(0, String::new(), String::new());
                    }
                    _ => {}
                }
            }
            Some(msg) = rx.recv() => {
                // Зеркалим ДО отправки в UI: иначе «подключились, но файла ещё
                // нет» — и догоняющий цикл сбросил бы состояние обратно.
                mirror_state(&up_file, &msg);
                let f: Vec<&str> = msg.trim_end().split('\t').collect();
                if f.first() == Some(&"STATE") && f.len() >= 4 {
                    let n: i32 = f[1].parse().unwrap_or(0);
                    on_state(n, f[2].to_string(), f[3].to_string());
                }
            }
        }
    }
    // Канал команд оборвался — окно ушло (на Windows туннель крутится в его же
    // процессе, поэтому дождаться отката маршрутов тем более обязательно).
    stop_current(&mut current).await;
    let _ = std::fs::remove_file(&up_file);
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
    // Свой каталог 0700 на каждый подъём хелпера: и порт, и токен лежат там, где
    // соседу по машине нечего подложить (см. private_dir).
    let dir = private_dir();
    let port_file = dir.join("port");
    let token_file = dir.join("token");
    // Токен — из криптостойкого генератора, а НЕ из времени и pid, как было.
    // По этому токену root-хелпер принимает команду CONNECT с ЛЮБЫМ адресом
    // координатора, то есть угадавший его сосед по машине уводит весь трафик
    // пользователя через себя. Время с точностью до наносекунды и pid — это
    // десятки битов угадываемого, а не 128 случайных.
    let token: String = {
        use rand::Rng;
        let b: [u8; 16] = rand::thread_rng().gen();
        b.iter().map(|x| format!("{x:02x}")).collect()
    };
    write_private(&token_file, &token)?;
    let _ = std::fs::remove_file(&port_file);
    let _ = std::fs::remove_file(up_file); // свежий старт: старый маркер не путает GUI
    // Журнал заводим МЫ, под пользователем: `>` в root-овом шелле существующий
    // файл только усечёт и владельца не переназначит. Иначе при строгом umask у
    // root мы не прочитали бы собственную улику — ровно та же ловушка, из-за
    // которой не читался файл порта.
    let _ = std::fs::File::create(helper_log(&port_file));

    // Путь к настройкам ищем ЗДЕСЬ, под пользователем, и отдаём помощнику готовым:
    // у root свой HOME (`/var/root`, `/root`), и сам он нашёл бы не тот файл или
    // ничего (см. `guest_config`). Абсолютный — рабочий каталог root-процесса не
    // наш, а поиск умеет возвращать относительный `bemyvpn.toml`. Пусто — файла
    // нет вовсе, помощник возьмёт умолчания.
    let cfg_file = bmv_config::Config::path()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .unwrap_or_default();
    elevate_launch(&exe, &port_file, &token_file, up_file, &cfg_file)?;

    // Ждём, пока хелпер (после ввода пароля) напишет порт (до 120с).
    //
    // ПРИЧИНУ НЕУДАЧИ ЗАПОМИНАЕМ. Раньше здесь стояло `if let Ok(...)`, то есть
    // ошибка чтения молча приравнивалась к «файла ещё нет», и любая поломка
    // выглядела одинаково: две минуты «Подключаюсь…», а потом неверное
    // «привилегии не получены». Ровно так пряталось EACCES на root-овом файле
    // 0600. «Нет файла» — это ожидание, всё остальное — диагноз.
    let mut port = 0u16;
    let mut last_err = String::new();
    for _ in 0..1200 {
        match std::fs::read_to_string(&port_file) {
            Ok(s) => {
                if let Ok(p) = s.trim().parse::<u16>() {
                    if p != 0 { port = p; break; }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => last_err = format!("файл порта: {e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Журнал хелпера читаем ДО уборки каталога — он и есть улика, когда root-процесс
    // умер молча (не тот путь к бинарю, паника в рантайме, пустой токен → exit 1).
    let log = std::fs::read_to_string(helper_log(&port_file)).unwrap_or_default();
    let _ = std::fs::remove_file(&token_file);
    let _ = std::fs::remove_file(&port_file);
    let _ = std::fs::remove_file(helper_log(&port_file));
    let _ = std::fs::remove_dir(&dir);
    if port == 0 {
        let tail: Vec<&str> = log.lines().rev().take(5).collect();
        let mut msg = "помощник не отдал порт".to_string();
        if !last_err.is_empty() { msg.push_str(&format!(" ({last_err})")); }
        if tail.is_empty() {
            msg.push_str(" — привилегии не получены (пароль отменён?)");
        } else {
            msg.push_str(&format!(" — {}", tail.into_iter().rev().collect::<Vec<_>>().join(" | ")));
        }
        return Err(msg);
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

/// Файл только для владельца — права 0600 ставятся В МОМЕНТ СОЗДАНИЯ.
///
/// Раньше здесь было `fs::write` + `set_permissions` следом, и между этими двумя
/// строками файл существовал с правами по умолчанию (0644). Микросекунды — но
/// содержимое этого файла и есть токен управления root-хелпером, а сосед по
/// машине может просто крутить открытие в цикле.
#[cfg(not(windows))]
fn write_private(path: &std::path::Path, data: &str) -> Result<(), String> {
    // 0600 здесь уместен: пишет пользователь, а читает root — ему прав хватит.
    write_nofollow(path, data, 0o600).map_err(|e| e.to_string())
}

// ── Запуск root-процесса с системным запросом пароля ─────────────────────────

/// Куда root-хелпер пишет свой вывод — рядом с файлом порта, в том же каталоге
/// 0700. Раньше вывод уходил в /dev/null, и молча умерший root-процесс (не тот
/// путь к бинарю, паника в рантайме, `exit 1` из-за нечитаемого токена) был
/// неотличим от «человек отменил пароль». В общий /tmp этот файл класть нельзя —
/// туда сосед по машине подложит симлинк, а пишет по нему root.
#[cfg(not(windows))]
fn helper_log(port_file: &std::path::Path) -> std::path::PathBuf {
    port_file.with_file_name("helper.log")
}

/// Путь → безопасный аргумент для `/bin/sh`. Одинарные кавычки закрывают пробелы,
/// но НЕ закрывают саму одинарную кавычку: путь вида `/Users/o'brien/Моё.app`
/// разрывал команду — а команда эта выполняется С ПРАВАМИ АДМИНИСТРАТОРА, то есть
/// апостроф в имени папки был готовой подстановкой чужого кода под root.
/// Приём стандартный: закрыть строку, вставить экранированную кавычку, открыть снова.
#[cfg(target_os = "macos")]
pub fn sh_quote(p: &std::path::Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

#[cfg(target_os = "macos")]
fn elevate_launch(exe: &std::path::Path, port_file: &std::path::Path, token_file: &std::path::Path, up_file: &std::path::Path, cfg_file: &std::path::Path) -> Result<(), String> {
    // Шелл-команда в одинарных кавычках (пути с пробелами ок), фоном (&) —
    // osascript вернётся сразу после ввода пароля. AppleScript-строка в двойных.
    // Переменные окружения помощнику не передать (osascript и pkexec их не несут),
    // поэтому путь к настройкам идёт ПЯТЫМ аргументом; читает его `run_helper`.
    let sh = format!(
        "{} --tunnel-helper {} {} {} {} >{} 2>&1 &",
        sh_quote(exe), sh_quote(port_file), sh_quote(token_file), sh_quote(up_file), sh_quote(cfg_file),
        sh_quote(&helper_log(port_file))
    );
    let script = format!("do shell script \"{}\" with administrator privileges", sh.replace('\\', "\\\\").replace('"', "\\\""));
    let status = std::process::Command::new("osascript").arg("-e").arg(script).status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err("запрос прав отменён".into()) }
}

#[cfg(target_os = "linux")]
fn elevate_launch(exe: &std::path::Path, port_file: &std::path::Path, token_file: &std::path::Path, up_file: &std::path::Path, cfg_file: &std::path::Path) -> Result<(), String> {
    // pkexec показывает графический запрос пароля (нужен polkit-агент рабочего стола).
    // Вывод — в тот же журнал, что и на macOS: из-под ярлыка рабочего стола stderr
    // родителя не видит никто, и молчаливая смерть root-процесса необъяснима.
    //
    // Путь к настройкам — АРГУМЕНТОМ, а не через окружение: pkexec окружение
    // вычищает нарочно, и `BEMYVPN_CONFIG` до помощника бы не доехал.
    let mut cmd = std::process::Command::new("pkexec");
    cmd.arg(exe).arg("--tunnel-helper").arg(port_file).arg(token_file).arg(up_file).arg(cfg_file);
    // Файл создаём МЫ (пользователь) и отдаём дескриптором: так root не создаёт
    // его сам и путь не переоткрывается по нашу сторону.
    if let Ok(f) = std::fs::File::create(helper_log(port_file)) {
        if let Ok(dup) = f.try_clone() {
            cmd.stdout(std::process::Stdio::from(f)).stderr(std::process::Stdio::from(dup));
        }
    }
    cmd.spawn().map(|_| ()).map_err(|e| format!("pkexec: {e}"))
}

// На Windows отдельного root-процесса нет: приложение уже под админом (манифест
// UAC), туннель качается в самом процессе (см. `inproc_serve`). Поэтому здесь
// нет windows-варианта elevate_launch — он не нужен.

#[cfg(test)]
mod tests {
    use super::*;

    /// ПУСТОЙ ТОКЕН НЕ ГОДИТСЯ НИКОГДА.
    ///
    /// Токен читается из файла через `unwrap_or_default()`, то есть сбой чтения
    /// превращает его в "". Со сравнением «строка == строка» такой хелпер
    /// принимал ЛЮБОГО, кто прислал пустую первую строку, — а принимает он
    /// команду CONNECT с произвольным координатором, то есть весь трафик машины.
    #[test]
    fn an_empty_token_never_authenticates_anyone() {
        assert!(!token_ok("", ""), "пустой токен обязан отвергать всех");
        assert!(!token_ok("", "\n"));
        assert!(!token_ok("", "что угодно"));
        assert!(!token_ok("секрет", ""));
        assert!(!token_ok("секрет", "не секрет"));
        // Настоящий токен по-прежнему работает, в том числе с переводом строки.
        assert!(token_ok("секрет", "секрет"));
        assert!(token_ok("секрет", " секрет\n"));
    }

    /// Запись НЕ ИДЁТ ПО СИМЛИНКУ — иначе root затирает чужой файл.
    ///
    /// Ровно эта атака и была возможна: файлы обмена лежали в общем /tmp, писал
    /// их root через `std::fs::write`, а сосед по машине заранее клал по этому
    /// пути ссылку на любой root-овый файл.
    #[cfg(unix)]
    #[test]
    fn a_root_write_refuses_to_follow_a_planted_symlink() {
        let dir = private_dir();
        let victim = dir.join("чужой-важный-файл");
        std::fs::write(&victim, "не трогать").unwrap();
        let trap = dir.join("маркер");
        std::os::unix::fs::symlink(&victim, &trap).unwrap();

        let res = write_nofollow(&trap, "ABCD1234", 0o600);
        assert!(res.is_err(), "запись по симлинку обязана падать, а не менять цель");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "не трогать");
        // И через «читаемый» вариант — симлинк там тот же самый капкан.
        assert!(write_readable(&trap, "ABCD1234").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "не трогать");

        // Обычный файл пишется как ни в чём не бывало, и сразу с запрошенными правами.
        let plain = dir.join("токен");
        write_nofollow(&plain, "51234", 0o600).unwrap();
        assert_eq!(std::fs::read_to_string(&plain).unwrap(), "51234");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&plain).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "файл не должен ни мгновения существовать читаемым всем");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ЧТО ПИШЕТ ROOT — ПОЛЬЗОВАТЕЛЬ ОБЯЗАН ПРОЧЕСТЬ.
    ///
    /// Регрессия, из-за которой окно на macOS вставало на «Подключаюсь…»
    /// намертво: файл порта и маркер «туннель поднят» создавались с правами
    /// 0600, как токен. Пишет их root-хелпер, значит владелец — root, и окно под
    /// обычным пользователем читало их с «Permission denied». Порт не
    /// прочитывался никогда → 120 секунд ожидания и ложное «привилегии не
    /// получены»; маркер не прочитывался никогда → тик-цикл гасил бы поднятый
    /// туннель обратно в «VPN выключен».
    ///
    /// Проверяем правами, а не вторым пользователем: под одним uid отличить
    /// 0600 от 0644 чтением нельзя — владельцу оба читаются. Бит чтения «для
    /// всех остальных» и есть здесь единственный наблюдаемый признак.
    #[cfg(unix)]
    #[test]
    fn what_root_writes_the_user_must_still_be_able_to_read() {
        use std::os::unix::fs::PermissionsExt;
        let dir = private_dir();
        let mode_of = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        // Файл порта — то, чего окно ждёт после запроса пароля.
        let port = dir.join("port");
        write_readable(&port, "51234").unwrap();
        assert_eq!(mode_of(&port), 0o644, "порт от root под пользователем не прочитается");

        // Маркер «туннель поднят» — тем же путём, через настоящий mirror_state.
        let up = dir.join("up");
        mirror_state(&up.to_string_lossy(), "STATE\t2\tAB12\t");
        assert_eq!(mode_of(&up), 0o644, "маркер от root под пользователем не прочитается");

        // Перезапись поверх уже существующего файла прав не роняет.
        write_readable(&port, "51235").unwrap();
        assert_eq!(mode_of(&port), 0o644);
        assert_eq!(std::fs::read_to_string(&port).unwrap(), "51235");

        // А токен — наоборот: его пишет пользователь, читает root, и «для всех»
        // он открыт быть не должен.
        let token = dir.join("token");
        write_private(&token, "секрет").unwrap();
        assert_eq!(mode_of(&token), 0o600, "токен управления root-хелпером — только владельцу");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Каталог обмена закрыт для соседей по машине.
    #[cfg(unix)]
    #[test]
    fn the_exchange_directory_is_ours_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = private_dir();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "в каталог 0777 сосед подложит симлинк ещё до записи");
        // Два вызова подряд не должны дать один и тот же каталог.
        let other = private_dir();
        assert_ne!(dir, other);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }

    /// ROOT-ПОМОЩНИК, К КОТОРОМУ НИКТО НЕ ПРИШЁЛ, ОБЯЗАН УЙТИ САМ.
    ///
    /// Живая улика: на машине разработчика нашлись ТРИ процесса
    /// `bemyvpn-gui --tunnel-helper` от root, каждый старше часа. Все три —
    /// остатки неудачных попыток: окно не смогло прочитать файл порта (была
    /// ошибка прав) и ушло, а помощник остался крутить приём в ожидании клиента,
    /// которого больше не будет. Цикл приёма был `while let Ok(..) = accept()`,
    /// то есть без выхода вообще.
    ///
    /// Проверяем ровно это: соединения приходят (сосед по машине стучится с
    /// мусором вместо токена — такие не считаются), а НАШЕЙ сессии нет. Функция
    /// обязана вернуться сама. На старом коде тест не заканчивается никогда и
    /// падает по внешнему сроку.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn a_helper_nobody_ever_claims_must_not_live_forever() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Чужие стуки идут ВЕСЬ срок и должны не продлевать его, а лишь мешать.
        let knocker = tokio::spawn(async move {
            for _ in 0..10 {
                if let Ok(mut s) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                    let _ = s.write_all("не токен\n".as_bytes()).await;
                }
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
        });

        let dir = private_dir();
        let up = dir.join("up").to_string_lossy().to_string();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
        let done = tokio::time::timeout(Duration::from_secs(10), accept_until(listener, "секрет", up, None, deadline)).await;
        knocker.abort();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(done.is_ok(), "помощник остался ждать клиента, которого не будет, — это и есть вечный root-процесс");
    }

    /// Помощник не имеет права сдаться РАНЬШЕ, чем сдастся окно.
    ///
    /// Окно ждёт файл порта 1200 × 100мс = 120 секунд (см. `spawn_and_connect`).
    /// Урезав `CLAIM_WINDOW` до чего-нибудь «поаккуратнее», получим ровно
    /// обратную поломку: редкая заминка при вводе пароля превратится в
    /// стабильный отказ, потому что помощник умрёт за секунду до прихода окна.
    #[cfg(not(windows))]
    #[test]
    fn the_helper_never_gives_up_before_the_window_does() {
        assert!(
            CLAIM_WINDOW > Duration::from_millis(100) * 1200,
            "помощник уходит раньше, чем окно перестаёт его ждать",
        );
    }

    /// Разделители протокола не должны приезжать внутри поля.
    ///
    /// Поля разделяются табом, команды — переводом строки, и эти же строки
    /// человек может вставить из буфера обмена (код сети, пароль, адрес
    /// сервера). Уцелевший таб превратил бы пароль в лишнее поле команды, а
    /// перевод строки — во ВТОРУЮ команду root-хелперу.
    #[test]
    fn field_separators_never_survive_inside_a_field() {
        assert_eq!(clean("AB\tCD"), "ABCD");
        assert_eq!(clean("STOP\nCONNECT\thttp://зло"), "STOPCONNECThttp://зло");
        assert_eq!(clean("пароль\r\n"), "пароль");
        // Обычный текст не портим, включая пробелы и юникод.
        assert_eq!(clean("мой пароль 123"), "мой пароль 123");
    }

    /// Маркер «туннель поднят» ставится только по состоянию 2 и снимается всеми
    /// остальными — окно верит этому файлу как единственному авторитету.
    #[test]
    fn the_up_marker_follows_the_tunnel_and_nothing_else() {
        let dir = private_dir();
        let up = dir.join("up");
        let path = up.to_string_lossy().to_string();

        mirror_state(&path, "STATE\t2\tAB12\t");
        assert_eq!(std::fs::read_to_string(&up).unwrap(), "AB12");

        mirror_state(&path, "STATE\t1\tAB12\t"); // «подключаюсь» — это ещё не туннель
        assert!(!up.exists());

        mirror_state(&path, "STATE\t2\tCD34\t");
        assert_eq!(std::fs::read_to_string(&up).unwrap(), "CD34");
        mirror_state(&path, "STATE\t0\t\t");
        assert!(!up.exists());

        // Чужая строка не трогает маркер вообще.
        mirror_state(&path, "STATE\t2\tEF56\t");
        mirror_state(&path, "ЧУШЬ\t2\t\t");
        assert_eq!(std::fs::read_to_string(&up).unwrap(), "EF56");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// НАСТРОЙКИ ЧЕЛОВЕКА ОБЯЗАНЫ ДОЕХАТЬ ДО ТУННЕЛЯ.
    ///
    /// Здесь строился `Config::default()` с одним подставленным координатором, и
    /// всё остальное из файла молча заменялось умолчаниями: свои STUN-серверы,
    /// протокол, `guest.ipv6`. Последнее означало, что снять блокировку IPv6 из
    /// окна было НЕВОЗМОЖНО — а без неё в сетях NAT64/464XLAT человек остаётся не
    /// «без VPN», а вообще без интернета.
    #[test]
    fn the_users_own_settings_reach_the_tunnel() {
        let dir = private_dir();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "coordinators = [\"https://из-файла\"]\n\
             default_protocol = \"noise-obfs\"\n\
             [guest]\nipv6 = \"allow\"\n\
             [stun]\nservers = [\"stun.свой:3478\"]\n",
        )
        .unwrap();

        let cfg = guest_config(Some(&path), "https://из-окна".into());
        assert_eq!(cfg.guest.ipv6_mode(), bmv_config::Ipv6Mode::Allow, "решение человека про IPv6 до маршрутов не доехало");
        assert_eq!(cfg.stun.servers, vec!["stun.свой:3478".to_string()], "STUN-пул подменён умолчаниями");
        assert_eq!(cfg.default_protocol, "noise-obfs", "протокол подменён умолчанием");
        // А вот координатор — ИЗ ОКНА: там его меняют, не сохраняя в файл.
        assert_eq!(cfg.coordinators, vec!["https://из-окна".to_string()]);

        // Файла нет вовсе → умолчания, и IPv6 при этом БЛОКИРУЕТСЯ.
        let cfg = guest_config(None, "https://из-окна".into());
        assert_eq!(cfg.guest.ipv6_mode(), bmv_config::Ipv6Mode::Block);
        assert_eq!(cfg.coordinators, vec!["https://из-окна".to_string()]);

        // Битый файл не роняет туннель и падает в защиту, а не в утечку.
        let bad = dir.join("bad.toml");
        std::fs::write(&bad, "это не toml =").unwrap();
        let cfg = guest_config(Some(&bad), "https://из-окна".into());
        assert_eq!(cfg.guest.ipv6_mode(), bmv_config::Ipv6Mode::Block);
        assert_eq!(cfg.coordinators, vec!["https://из-окна".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Разбор STATE-строк и команды QUICK — проводной протокол с root-хелпером.
    #[test]
    fn the_wire_protocol_round_trips() {
        assert_eq!(state_line(&State::Connecting("AB12".into())), "STATE\t1\tAB12\t");
        assert_eq!(state_line(&State::Up("AB12".into())), "STATE\t2\tAB12\t");
        assert_eq!(state_line(&State::Off), "STATE\t0\t\t");
        assert_eq!(state_line(&State::Failed("нет NAT".into())), "STATE\t3\t\tнет NAT");
        // Конец раздачи — СВОЙ код. Скатится он в 0 — человек увидит беспричинно
        // погасший VPN; скатится в 3 — увидит ошибку там, где всё сработало.
        assert_eq!(state_line(&State::HostLeft), "STATE\t4\t\t");
        // И маркер «туннель поднят» при этом снимается, как на любом не-2.
        let dir = private_dir();
        let up = dir.join("up");
        mirror_state(&up.to_string_lossy(), "STATE\t2\tAB12\t");
        mirror_state(&up.to_string_lossy(), &state_line(&State::HostLeft));
        assert!(!up.exists(), "хост ушёл, а маркер остался — часы сеанса продолжат тикать");
        let _ = std::fs::remove_dir_all(&dir);

        let f: Vec<&str> = "QUICK\thttps://s\tH1\tnoise\tH2\tnoise-obfs".split('\t').collect();
        assert_eq!(
            parse_quick(&f),
            vec![
                ("H1".to_string(), String::new(), "noise".to_string()),
                ("H2".to_string(), String::new(), "noise-obfs".to_string()),
            ]
        );
        // Нечётный хвост (протокол не пришёл) не должен ни паниковать, ни
        // подставлять чужой протокол следующему хосту.
        let f: Vec<&str> = "QUICK\thttps://s\tH1".split('\t').collect();
        assert!(parse_quick(&f).is_empty());
    }
}
