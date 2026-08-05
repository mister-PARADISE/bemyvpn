//! Обмен файлами и рукопожатие с туннель-хелпером — НА НАСТОЯЩЕМ БИНАРЕ.
//!
//! Проверяет ровно ту цепочку, которая на macOS вставала намертво:
//! окно кладёт токен в приватный каталог → поднимает бинарь с
//! `--tunnel-helper <порт> <токен> <маркер>` → хелпер читает токен, слушает,
//! ПИШЕТ ПОРТ В ФАЙЛ → окно читает порт, подключается, шлёт токен первой
//! строкой → шлёт команду → получает STATE.
//!
//! Root здесь не нужен и не используется: до создания TUN дело не доходит,
//! проверяется только управляющая половина. Поэтому разницу «файл от root с
//! правами 0600» этот тест увидеть не может — под одним uid владельцу читается
//! и 0600 (за неё отвечает `what_root_writes_the_user_must_still_be_able_to_read`
//! в helper.rs). Зато он ловит всё остальное: порядок аргументов, потерянный
//! каталог, молчаливую смерть хелпера, отказ принять верный токен.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_bemyvpn-gui");

/// Приватный каталог обмена — как его делает окно (`helper::private_dir`).
fn temp_dir() -> std::path::PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    let n: u128 = rand::random();
    let dir = std::env::temp_dir().join(format!("bmv-test-{n:032x}"));
    std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    dir
}

/// Дождаться, пока хелпер запишет порт. Возврат 0 — не дождались.
fn wait_port(port_file: &std::path::Path, limit: Duration) -> u16 {
    let start = Instant::now();
    while start.elapsed() < limit {
        if let Ok(s) = std::fs::read_to_string(port_file) {
            if let Ok(p) = s.trim().parse::<u16>() {
                if p != 0 {
                    return p;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    0
}

/// Полный оборот: токен на месте → порт записан → верный токен принят → STATE.
#[test]
fn the_helper_publishes_its_port_and_answers_an_authenticated_session() {
    let dir = temp_dir();
    let (port_file, token_file, up_file) = (dir.join("port"), dir.join("token"), dir.join("up"));
    std::fs::write(&token_file, "0123456789abcdef0123456789abcdef").unwrap();

    let mut child = std::process::Command::new(EXE)
        .arg("--tunnel-helper")
        .arg(&port_file)
        .arg(&token_file)
        .arg(&up_file)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let port = wait_port(&port_file, Duration::from_secs(20));
    assert_ne!(port, 0, "хелпер не записал порт — окно ждало бы его все 120 секунд");

    // Сосед по машине с мусором вместо токена НЕ должен ронять хелпера: приём
    // идёт в цикле, настоящее окно подключается следом.
    {
        let mut bad = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        writeln!(bad, "не тот токен").unwrap();
        let mut line = String::new();
        // Ответа нет, соединение закрывается со стороны хелпера.
        let _ = BufReader::new(bad.try_clone().unwrap()).read_line(&mut line);
        assert!(line.is_empty(), "неудостоверенному соседу отвечать нечего");
    }

    // А теперь мы — с верным токеном.
    let mut sock = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut rd = BufReader::new(sock.try_clone().unwrap());
    writeln!(sock, "0123456789abcdef0123456789abcdef").unwrap();
    writeln!(sock, "STOP").unwrap();

    let mut line = String::new();
    rd.read_line(&mut line).unwrap();
    // Обрезаем ТОЛЬКО перевод строки: пустые поля протокола — это концевые табы,
    // и обычный trim_end() съел бы их вместе с ним.
    assert_eq!(
        line.trim_end_matches(['\r', '\n']),
        "STATE\t0\t\t",
        "хелпер не ответил на команду по удостоверенной сессии"
    );

    // Окно закрылось → хелпер обязан выйти сам, а не остаться root-процессом.
    drop(rd);
    drop(sock);
    let code = child.wait().unwrap();
    assert!(code.success(), "хелпер вышел с ошибкой: {code:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// ПУТЬ К НАСТРОЙКАМ ДОЕЗЖАЕТ ДО ROOT-ПОМОЩНИКА.
///
/// Помощник — отдельный процесс с ЧУЖИМ HOME (`/var/root`, `/root`), сам файл
/// настроек он не найдёт: путь ищет окно под пользователем и передаёт пятым
/// аргументом. Пока этого не было, помощник крутил туннель на умолчаниях, то
/// есть молча игнорировал и STUN-серверы, и протокол, и `guest.ipv6`.
///
/// Проверяем на НАСТОЯЩЕМ бинаре, потому что ломается это ровно на границе
/// процессов: перепутанный порядок аргументов внутри одного файла не заметен.
#[test]
fn the_helper_is_told_where_the_users_settings_live() {
    let dir = temp_dir();
    let (port_file, token_file, up_file) = (dir.join("port"), dir.join("token"), dir.join("up"));
    let cfg_file = dir.join("config.toml");
    std::fs::write(&token_file, "0123456789abcdef0123456789abcdef").unwrap();
    std::fs::write(&cfg_file, "[guest]\nipv6 = \"allow\"\n").unwrap();

    let child = std::process::Command::new(EXE)
        .arg("--tunnel-helper")
        .arg(&port_file)
        .arg(&token_file)
        .arg(&up_file)
        .arg(&cfg_file)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Лишний аргумент не должен ломать рукопожатие: порт публикуется как обычно.
    let port = wait_port(&port_file, Duration::from_secs(20));
    assert_ne!(port, 0, "пятый аргумент сбил разбор командной строки");

    let mut sock = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    writeln!(sock, "0123456789abcdef0123456789abcdef").unwrap();
    drop(sock);
    // ЧТО ЭТОТ ТЕСТ ДОКАЗЫВАЕТ ТЕПЕРЬ — и чего он больше НЕ доказывает.
    //
    // Доказывает: пятый аргумент (путь к настройкам) не сбивает разбор командной
    // строки. Это и была настоящая поломка — помощник просто не поднимался.
    //
    // НЕ доказывает: что путь и вправду ПРОЧИТАН. Раньше это проверялось по
    // строке в журнале — журнала больше нет и не будет: запись о работе VPN, у
    // того, кто её ведёт, можно потребовать. Наблюдаемого следа у чтения
    // настроек снаружи нет, и выдумывать его ради теста мы не станем.
    let out = child.wait_with_output().unwrap();
    assert!(out.stderr.is_empty(), "помощник не смеет ничего писать наружу");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Токен не прочитался → хелпер обязан ВЫЙТИ, а не встать root-ом, принимающим
/// команды от кого угодно.
///
/// Прежде тест требовал ещё и объяснения в журнале — «молчаливая смерть
/// неотличима от отмены пароля». Журнал убран совсем, и требование снято
/// осознанно: различать эти два случая нужно было при отладке, а человеку в
/// обоих один и тот же следующий шаг. Доказательство осталось прежней силы —
/// код возврата и отсутствие опубликованного порта.
#[test]
fn a_helper_without_a_token_refuses_to_serve_anyone() {
    let dir = temp_dir();
    let (port_file, token_file, up_file) = (dir.join("port"), dir.join("token"), dir.join("up"));
    std::fs::write(&token_file, "   \n").unwrap(); // пусто по существу

    let out = std::process::Command::new(EXE)
        .arg("--tunnel-helper")
        .arg(&port_file)
        .arg(&token_file)
        .arg(&up_file)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(1));
    assert!(!port_file.exists(), "порт без токена публиковать нечему");
    assert!(out.stderr.is_empty(), "помощник не смеет ничего писать наружу");

    let _ = std::fs::remove_dir_all(&dir);
}
