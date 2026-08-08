//! Десктопный роутинг VPN-гостя: создать TUN и завернуть ВЕСЬ трафик в туннель,
//! а на Drop — откатить. Один код для CLI, GUI и привилегированного хелпера GUI.
//! Android/iOS сюда не ходят — там свой fd из платформенного шелла.
//!
//! Настоящий VPN, а не прокси: split-default (см. `IPV4_HALVES`) перекрывает
//! дефолтный маршрут НЕ удаляя его; сам хост пинуется через реальный шлюз
//! (иначе шифрованные пакеты зациклятся). `RouteGuard::drop` всё откатывает.
//! Три ветки, и все три боевые: Linux и macOS (обе требуют root) и Windows
//! (админ-права, wintun). Здесь стояло «Windows — задел» — неправда с тех пор,
//! как появились `windows_net_info`, `ensure_wintun` и `ensure_firewall_allow`.
//!
//! ТОЧНО ТАК ЖЕ ПЕРЕКРЫВАЕТСЯ IPv6 — `::/1` + `8000::/1` (см. `IPV6_HALVES`).
//! Без этого туннель на IPv4 «подключён», а весь v6-трафик уходит мимо него
//! напрямую, и настоящий адрес человека виден сайтам.

use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use bmv_config::Ipv6Mode;
use bmv_tunnel::TunParams;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub mod hosting;
pub mod tunnel;

/// ПОЧЕМУ не поднялся сетевой адаптер — ПРИЗНАКОМ, а не строкой.
///
/// Раньше `make_tun` отдавал `Result<_, String>`, а гостевой слой выбрасывал этот
/// текст не глядя (`Err(_)`) и печатал человеку «нужны права администратора» — на
/// ЛЮБУЮ неудачу. Так владельцу на Windows-ARM и предлагали ввести пароль, хотя
/// дело было в библиотеке не той разрядности: он вводил пароль, ничего не
/// менялось, и понять почему было нельзя. Догадка стояла на месте утверждения.
///
/// Приём одолжен у `bmv_common::Error::Refused { code, reason }`: род неудачи
/// едет ОТДЕЛЬНЫМ значением, а слова к нему подбираются в одном месте (`human`).
/// Разбирать чужой текст подстрокой нельзя — это то же враньё, только позже
/// (сторож `crates/bmv-common/tests/no_code_by_substring.rs` ровно про это).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunFail {
    /// Не хватило прав. ЗНАЕМ ТОЧНО: система ответила «доступ запрещён».
    NoRights,
    /// Библиотека адаптера не загрузилась. Windows: `wintun.dll` не той
    /// разрядности (та самая поломка на ARM) либо её не удалось положить рядом.
    DriverLoad,
    /// Устройства для туннеля в системе нет. Linux: не собран/не загружен модуль
    /// `tun`, либо в контейнер не проброшен `/dev/net/tun`.
    NoDevice,
    /// Причина неизвестна. Так и говорим — советов из воздуха не берём.
    Unknown,
}

impl TunFail {
    /// Все виды — чтобы сторож ниже прошёлся по каждому, а не по тем, что вспомнил.
    pub const ALL: [TunFail; 4] = [TunFail::NoRights, TunFail::DriverLoad, TunFail::NoDevice, TunFail::Unknown];

    /// Что читает человек. ОДНО МЕСТО на обе десктопные оболочки: окно и терминал
    /// показывают эту строку как есть (`State::Failed`), поэтому копий у неё нет
    /// и разъехаться нечему.
    ///
    /// Правило простое: знаем причину — называем её и говорим, что делать; не
    /// знаем — так и пишем. Совет про пароль стоит РОВНО у `NoRights`, и это
    /// проверяет `the_password_advice_belongs_only_to_a_rights_failure`.
    pub fn human(self) -> &'static str {
        match self {
            TunFail::NoRights => {
                "Не удалось включить VPN — нужны права администратора. Запустите приложение ещё раз и введите пароль."
            }
            TunFail::DriverLoad => {
                "Не удалось включить VPN: библиотека сетевого адаптера не загрузилась. Проверьте, что стоит \
                 сборка для вашего процессора (ARM или x86), и обновите приложение."
            }
            TunFail::NoDevice => {
                "Не удалось включить VPN: в системе нет устройства для туннеля (/dev/net/tun). На Linux его \
                 даёт модуль tun — «modprobe tun»; в контейнере устройство надо пробросить внутрь."
            }
            TunFail::Unknown => {
                "Не удалось включить VPN: сетевой адаптер не создался, а причину система не назвала — назвать \
                 её не можем и мы. Попробуйте ещё раз."
            }
        }
    }
}

/// Ошибка крейта `tun` → род неудачи. ЕДИНСТВЕННОЕ место, где неизвестное
/// становится известным, поэтому и сторож у него один (`only_a_permission_error…`).
///
/// Всё, чего мы не опознали, идёт в `Unknown` — и это не лень, а обещание: сюда
/// нельзя дописать «а ещё это скорее всего права», иначе вернётся ровно та ложь,
/// из-за которой писан весь этот кусок.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn classify(e: &tun::Error) -> TunFail {
    match e {
        // Windows: `wintun::load()` не смог поднять библиотеку. Именно так
        // выглядела ARM-поломка — LoadLibrary отказывает файлу чужой разрядности.
        #[cfg(target_os = "windows")]
        tun::Error::WintunError(wintun::Error::LibLoading(_)) => TunFail::DriverLoad,
        tun::Error::Io(io) => match io.kind() {
            // EPERM/EACCES: /dev/net/tun на Linux, utun-сокет на macOS.
            std::io::ErrorKind::PermissionDenied => TunFail::NoRights,
            std::io::ErrorKind::NotFound => TunFail::NoDevice,
            _ => TunFail::Unknown,
        },
        _ => TunFail::Unknown,
    }
}

/// Создать десктопный TUN (нужен root/sudo). Android/iOS дают готовый fd мимо этого.
/// Возвращает устройство И РЕАЛЬНОЕ имя интерфейса: на macOS ядро само выдаёт
/// `utunN` (кастомное «bmv0» там невалидно → «invalid device name»), а маршруты
/// ставить надо именно по выданному имени.
///
/// Ошибка — `TunFail`, а не строка: см. пояснение при типе.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub fn make_tun(params: &TunParams) -> Result<(TunDevice, String), TunFail> {
    use tun::Device; // трейт с .name()
    #[cfg(target_os = "windows")]
    ensure_wintun()?; // dll вшита в exe — никакого отдельного файла в релизе
    let mut config = tun::Configuration::default();
    config
        .address(params.address)
        .netmask(params.netmask)
        .mtu(params.mtu as i32)
        .up();
    // Имя задаём только там, где оно валидно. macOS требует формат utunN и сам
    // выберет свободный — навязывать «bmv0» нельзя.
    #[cfg(not(target_os = "macos"))]
    config.name(&params.name);
    #[cfg(target_os = "linux")]
    config.platform(|p| {
        p.packet_information(false); // чистые IP-пакеты, без 4-байтного заголовка
    });
    let dev = tun::create_as_async(&config).map_err(|e| classify(&e))?;
    let name = dev.get_ref().name().map_err(|e| classify(&e))?;
    Ok((
        TunDevice {
            inner: dev,
            #[cfg(target_os = "macos")]
            rx: vec![0u8; RX_SCRATCH],
        },
        name,
    ))
}

/// Разовый буфер чтения под macOS-кадрирование. 65540 = максимальный IP-пакет
/// плюс 4 байта AF-заголовка utun.
#[cfg(target_os = "macos")]
const RX_SCRATCH: usize = 65540;

/// Обёртка TUN-устройства с корректным кадрированием под платформу.
///
/// На macOS utun ВСЕГДА оборачивает каждый пакет 4-байтовым заголовком семейства
/// адресов (AF_INET/AF_INET6, big-endian). Ядро BeMyVPN качает ЧИСТЫЕ IP-пакеты
/// (как на Linux и как iOS-FFI с utun=true), поэтому здесь на чтении снимаем эти
/// 4 байта, на записи — добавляем. Без этого хост получал бы пакет с мусорными
/// 4 байтами спереди, а запись в utun без заголовка падала бы → туннель рвался
/// мгновенно. На Linux/Windows — прозрачная передача (там заголовка нет).
pub struct TunDevice {
    inner: tun::AsyncDevice,
    /// Буфер под снятие utun-заголовка. ЖИВЁТ В СТРУКТУРЕ, а не создаётся на
    /// каждый пакет: `[0u8; 65540]` на стеке — это 64 КБ обнуления памяти на
    /// КАЖДЫЙ прочитанный пакет, обычно длиной 1500 байт. На гигабитном канале
    /// это десятки гигабайт записи в никуда каждую секунду.
    #[cfg(target_os = "macos")]
    rx: Vec<u8>,
}

#[cfg(target_os = "macos")]
impl AsyncRead for TunDevice {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        // Разделяем заимствования полей: буфер и устройство нужны одновременно.
        let this = self.get_mut();
        loop {
            let n = {
                let mut rb = ReadBuf::new(&mut this.rx);
                match Pin::new(&mut this.inner).poll_read(cx, &mut rb) {
                    Poll::Ready(Ok(())) => rb.filled().len(),
                    other => return other,
                }
            };
            // РОВНО НОЛЬ — настоящий конец файла (устройство закрыли).
            if n == 0 {
                return Poll::Ready(Ok(()));
            }
            // 1..=4 байта — кадр без полезной нагрузки (один AF-заголовок).
            // Раньше условие было `len > 4`, и такой кадр отдавался наверх как
            // «прочитано 0 байт», то есть как EOF: туннель гасился на ровном
            // месте, и со стороны это выглядело «сам отвалился». Пропускаем и
            // читаем дальше.
            if n > 4 {
                buf.put_slice(&this.rx[4..n]);
                return Poll::Ready(Ok(()));
            }
        }
    }
}

/// 4-байтовый заголовок семейства адресов utun (big-endian u32): AF_INET(2) для
/// IPv4, AF_INET6(30) для IPv6 — по версии в старшем ниббле первого байта пакета.
#[cfg(target_os = "macos")]
fn utun_af_header(data: &[u8]) -> [u8; 4] {
    let af: u8 = if data.first().map(|b| b >> 4) == Some(6) { 30 } else { 2 };
    [0, 0, 0, af]
}

#[cfg(target_os = "macos")]
impl AsyncWrite for TunDevice {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<std::io::Result<usize>> {
        if data.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let mut framed = Vec::with_capacity(data.len() + 4);
        framed.extend_from_slice(&utun_af_header(data));
        framed.extend_from_slice(data);
        match Pin::new(&mut self.inner).poll_write(cx, &framed) {
            // utun пишет датаграмму целиком; возвращаем длину ИСХОДНОГО пакета.
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(data.len())),
            other => other,
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// Linux/Windows: заголовка нет — прозрачно делегируем в устройство.
#[cfg(not(target_os = "macos"))]
impl AsyncRead for TunDevice {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
#[cfg(not(target_os = "macos"))]
impl AsyncWrite for TunDevice {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, data)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Вшитая `wintun.dll` — ПОД СВОЮ АРХИТЕКТУРУ, файл выбирается на сборке.
///
/// Раньше файл был ОДИН, `packaging/windows/wintun.dll`, и он был x86_64. Вшивался
/// он во все Windows-сборки, включая ARM64, а такую библиотеку `LoadLibrary` на
/// ARM не грузит в принципе. Адаптер не создавался, `make_tun` отдавал ошибку, и
/// человек читал «нужны права администратора» — текст, не имеющий к причине
/// никакого отношения. Внешние сторожа этого не видели: они смотрят импорты exe,
/// а библиотека лежит внутри блобом.
///
/// ОТКУДА ФАЙЛЫ И ПОЧЕМУ ИМ МОЖНО ВЕРИТЬ. Оба — из одного официального архива
/// `wintun-0.14.1.zip` с wintun.net: `bin/amd64/wintun.dll` и `bin/arm64/wintun.dll`.
/// Подлинность архива доказана сличением, а не доверием к ссылке: amd64-файл из
/// него оказался ПОБАЙТОВО равен библиотеке, которую проект возил с самого начала
/// (SHA-256 `e5da8447dc2c…`, 427 552 Б). Значит архив — та же самая сборка, и
/// arm64-файл (SHA-256 `f7ba89005544…`, 222 488 Б) взят из неё же.
/// Лицензия wintun разрешает немодифицированную копию.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const WINTUN_DLL: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/windows/wintun-x86_64.dll"));
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
const WINTUN_DLL: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/windows/wintun-arm64.dll"));

/// Ожидаемая метка архитектуры в заголовке PE вшитого файла.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const WINTUN_MACHINE: u16 = 0x8664;
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
const WINTUN_MACHINE: u16 = 0xAA64;

// ТРЕТЬЕЙ АРХИТЕКТУРЫ У НАС НЕТ — И СОБИРАТЬСЯ ОНА НЕ ДОЛЖНА.
// Без этого `WINTUN_DLL` просто не определился бы, и сообщение было бы про
// «не найдено значение», а не про то, что делать. Молчаливая сборка exe без
// рабочего туннеля — ровно та беда, из-за которой писан этот блок.
#[cfg(all(target_os = "windows", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
compile_error!(
    "Windows: нет вшитой wintun.dll для этой архитектуры. Возьмите файл из официального \
     архива wintun-0.14.1.zip с wintun.net (bin/<арх>/wintun.dll), положите его как \
     packaging/windows/wintun-<арх>.dll и добавьте ветку к WINTUN_DLL/WINTUN_MACHINE в \
     crates/bmv-desktop/src/lib.rs. Собирать VPN, у которого заведомо не поднимется \
     туннель, нельзя."
);

/// СТОРОЖ РАЗРЯДНОСТИ, СЧИТАННЫЙ НА СБОРКЕ. Имя файла — это обещание, а не факт:
/// прежняя поломка и состояла в том, что под общим именем лежал x86_64. Здесь
/// читается сам заголовок PE, поэтому подменённый или перепутанный файл роняет
/// компиляцию, а не гостя на чужой машине.
#[cfg(target_os = "windows")]
const _: () = {
    // PE-заголовок: по смещению 0x3C лежит e_lfanew (адрес сигнатуры), дальше
    // «PE\0\0» и два байта Machine. Хватает младших двух байт e_lfanew — он у
    // всех наших файлов меньше 64 КБ.
    let d = WINTUN_DLL;
    let pe = d[0x3c] as usize | (d[0x3d] as usize) << 8;
    assert!(
        d[pe] == b'P' && d[pe + 1] == b'E' && d[pe + 2] == 0 && d[pe + 3] == 0,
        "packaging/windows/wintun-*.dll: это не PE-файл. Нужна библиотека из официального \
         архива wintun-0.14.1.zip с wintun.net."
    );
    assert!(
        d[pe + 4] as u16 | (d[pe + 5] as u16) << 8 == WINTUN_MACHINE,
        "packaging/windows/wintun-*.dll НЕ той разрядности, что цель сборки. В файл с именем \
         одной архитектуры положили библиотеку другой — ровно так ARM-сборка и уехала людям с \
         x86_64-библиотекой внутри. Возьмите bin/<арх>/wintun.dll из wintun-0.14.1.zip."
    );
};

/// Windows: `wintun.dll` вшита в exe — распаковываем, чтобы LoadLibrary нашёл.
///
/// Крейт `wintun`, который её грузит, у нас форкнут (vendor/wintun) — там `netsh`
/// запускается с CREATE_NO_WINDOW, иначе при подключении мигают окна консоли.
/// Про запрет на подъём `tun` до 0.8 — комментарий в корневом Cargo.toml.
///
/// Неудача здесь — это `DriverLoad`, а не «нет прав»: библиотеку не удалось даже
/// положить рядом, значит грузить нечего. Причина у такого отказа (диск полон,
/// каталог только для чтения) человеку из строки состояния всё равно не видна, а
/// делать ему надо то же самое — взять исправную сборку.
#[cfg(target_os = "windows")]
fn ensure_wintun() -> Result<(), TunFail> {
    const WINTUN: &[u8] = WINTUN_DLL;
    // Сверка ПО ДЛИНЕ заодно чинит машины, на которых уже лежит чужая копия:
    // ARM-сборка прошлых выпусков распаковывала сюда x86_64-библиотеку (427 552 Б
    // против 222 488 Б), и при первом же запуске новой версии файл перепишется сам.
    let write_dll = |dir: &std::path::Path| -> std::io::Result<()> {
        let dst = dir.join("wintun.dll");
        if dst.metadata().map(|m| m.len() == WINTUN.len() as u64).unwrap_or(false) {
            return Ok(());
        }
        std::fs::write(&dst, WINTUN)
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if write_dll(dir).is_ok() {
                return Ok(());
            }
        }
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("BeMyVPN");
    std::fs::create_dir_all(&base).map_err(|_| TunFail::DriverLoad)?;
    write_dll(&base).map_err(|_| TunFail::DriverLoad)?;
    // Правка PATH — ГЛОБАЛЬНАЯ и не потокобезопасная (в многопоточном процессе
    // чужой поток может читать окружение ровно в этот момент). Делаем её
    // РОВНО ОДИН раз за процесс: повторные вызовы make_tun иначе и правили бы
    // окружение на каждое подключение, и удлиняли PATH одним и тем же путём.
    // ponytail: правильный путь — SetDllDirectoryW из windows-sys вместо PATH;
    // меняем, если понадобится второй такой случай.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths: Vec<_> = std::env::split_paths(&path).collect();
        paths.insert(0, base);
        if let Ok(joined) = std::env::join_paths(paths) {
            std::env::set_var("PATH", joined);
        }
    });
    Ok(())
}

/// «Хлебная крошка» — что вернуть, если процесс УМЕР, не отработав `Drop`.
///
/// Маршруты чинятся сами: они привязаны к tun-интерфейсу, а тот исчезает вместе
/// с процессом, и ядро убирает их следом. А вот DNS — нет. Ctrl-C, `kill`,
/// падение — и настройка «резолвер = 8.8.8.8» остаётся НАВСЕГДА: интернет
/// работает, поэтому никто не замечает, что все запросы имён с этого момента
/// молча уходят в Google. Для VPN, который делают ради приватности, это худший
/// вид поломки — тихий. Обработчиком сигнала это не лечится (после него `Drop`
/// всё равно не выполняется), поэтому пишем состояние на диск и доедаем крошку
/// при следующем `install`.
///
/// Путь ТОЛЬКО root-овый: положи мы файл в /tmp — любой пользователь машины
/// подсунул бы туда своё содержимое, и root покорно записал бы его в
/// /etc/resolv.conf.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const DNS_CRUMB: &str = "/etc/bemyvpn-dns.bak";

/// Вернуть DNS и IPv6, если прошлый запуск умер, не откатившись. Тихо, без
/// ошибок: нет крошки — нечего чинить; нет прав — мы и туннель поднять не сможем.
///
/// IPv6-маршруты чинятся ЗДЕСЬ ЖЕ и по той же причине, что DNS: они отвергающие,
/// а значит НЕ привязаны к нашему интерфейсу и вместе с ним не исчезают. Ctrl-C,
/// `kill`, падение — и машина остаётся без IPv6 навсегда. Это тот же тихий вид
/// поломки, только наизнанку: не «утекает незаметно», а «половина интернета
/// незаметно недоступна».
#[cfg(target_os = "linux")]
fn recover_after_crash() {
    if let Ok(old) = std::fs::read(DNS_CRUMB) {
        // Пустая крошка = resolv.conf в прошлый раз не прочитался. Записать
        // пустоту значило бы стереть настоящий резолвер.
        if !old.is_empty() {
            let _ = std::fs::write("/etc/resolv.conf", old);
        }
        ipv6_unblock();
        let _ = std::fs::remove_file(DNS_CRUMB);
    }
}

/// macOS: в крошке строки «сервис», затем прежние серверы (пусто = было «авто»).
#[cfg(target_os = "macos")]
fn recover_after_crash() {
    let Ok(txt) = std::fs::read_to_string(DNS_CRUMB) else { return };
    let mut lines = txt.lines();
    if let Some(svc) = lines.next().filter(|s| !s.is_empty()) {
        let old: Vec<&str> = lines.filter(|s| !s.is_empty()).collect();
        let mut a = vec!["-setdnsservers", svc];
        if old.is_empty() {
            a.push("empty");
        } else {
            a.extend(old);
        }
        let _ = networksetup(&a);
    }
    ipv6_unblock(); // см. пояснение в linux-ветке: маршруты переживают процесс
    let _ = std::fs::remove_file(DNS_CRUMB);
}

// ── блокировка IPv6 (по половине адресного пространства на команду) ──────────

/// Linux: `unreachable` — ядро сразу отдаёт приложению ENETUNREACH, и оно берёт
/// IPv4 без паузы. Привязки к устройству нет намеренно: маршрут обязан работать
/// и в тот момент, когда tun-интерфейс уже снят, а сеанс ещё не закрыт.
#[cfg(target_os = "linux")]
fn ipv6_block(_tun: &str) -> Result<(), String> {
    for half in IPV6_HALVES {
        ip(&["-6", "route", "add", "unreachable", half])?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ipv6_unblock() {
    for half in IPV6_HALVES {
        // Без слова `unreachable`: удаление обязано пройти НАВЕРНЯКА. Не удалить
        // — значит оставить машину без IPv6 до перезагрузки, и человек не поймёт,
        // почему половина интернета отвалилась после того, как он выключил VPN.
        let _ = ip(&["-6", "route", "del", half]);
    }
}

/// macOS: `-reject` = RTF_REJECT, ICMP unreachable в ответ (см. route(8)).
/// Шлюзом ставим `::1`: сам он никуда не ведёт, флаг отвергает всё раньше.
#[cfg(target_os = "macos")]
fn ipv6_block(_tun: &str) -> Result<(), String> {
    for half in IPV6_HALVES {
        route(&["-n", "add", "-inet6", "-net", half, "::1", "-reject"])?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn ipv6_unblock() {
    for half in IPV6_HALVES {
        let _ = route(&["-n", "delete", "-inet6", "-net", half]);
    }
}

/// Windows: отвергающих маршрутов у netsh нет, поэтому IPv6 заворачивается В
/// ТУННЕЛЬ — а там его добивает ядро (`bmv_tunnel::to_host_allowed`). Утечки нет
/// так же надёжно, но приложение узнаёт об этом не мгновенно, а по таймауту.
///
/// `store=active` обязателен: без него netsh пишет маршрут в ПОСТОЯННУЮ
/// конфигурацию, и он переживёт перезагрузку — то есть машина останется без
/// IPv6 навсегда.
/// ponytail: потолок — таймауты вместо мгновенного отката на IPv4; чинится
/// отвергающим маршрутом через API маршрутизации (CreateIpForwardEntry2 с
/// нулевым next hop), а это уже windows-sys в зависимостях.
#[cfg(target_os = "windows")]
fn ipv6_block(tun_idx: &str) -> Result<(), String> {
    for half in IPV6_HALVES {
        netsh(&["interface", "ipv6", "add", "route", half, &format!("interface={tun_idx}"), "store=active"])?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn ipv6_unblock(tun_idx: &str) {
    for half in IPV6_HALVES {
        let _ = netsh(&["interface", "ipv6", "delete", "route", half, &format!("interface={tun_idx}")]);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_dns_crumb(data: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // 0600 задаём В МОМЕНТ создания, а не chmod'ом после: иначе между созданием
    // и правкой прав есть окно, в котором файл открыт всем.
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(DNS_CRUMB);
    if let Ok(mut f) = f {
        let _ = f.write_all(data.as_bytes());
    }
}

/// ДВЕ ПОЛОВИНЫ IPv4 — приём тот же, что у `IPV6_HALVES` ниже (там он и
/// расписан подробно), просто эти строки появились раньше.
///
/// Раньше они были ДВЕНАДЦАТЬЮ отдельными литералами: по паре на установку и на
/// откат в каждой из трёх платформенных веток. Опечатка в откате не мешает
/// подключиться и никак не видна — она оставляет человека с половиной
/// завёрнутого трафика ПОСЛЕ того, как он выключил VPN.
// Под Windows префиксную запись не читает никто (там своя, `IPV4_HALVES_WIN`),
// но константа нужна тесту `both_spellings_of_the_ipv4_halves_agree` — он
// сверяет обе записи на ЛЮБОЙ платформе. Без этой строки `-D warnings` роняет
// сборку под Windows на `dead_code`. Зеркало соседки ниже.
#[cfg_attr(target_os = "windows", allow(dead_code))]
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const IPV4_HALVES: [&str; 2] = ["0.0.0.0/1", "128.0.0.0/1"];

/// ТЕ ЖЕ ДВЕ ПОЛОВИНЫ в записи `route.exe`: (сеть, маска). Отдельная константа
/// потому, что Windows не понимает префиксной записи, — а НЕ потому, что правило
/// там другое. Что обе записи описывают один и тот же кусок адресного
/// пространства, проверяет `both_spellings_of_the_ipv4_halves_agree`: сверить их
/// глазами нельзя, ветку Windows на этой машине никто не собирает.
// Тест читает константу на ЛЮБОЙ платформе — иначе правку половин на маке
// проверял бы только мак, а ломалась бы она у человека под Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const IPV4_HALVES_WIN: [(&str, &str); 2] = [("0.0.0.0", "128.0.0.0"), ("128.0.0.0", "128.0.0.0")];

/// ДВЕ ПОЛОВИНЫ IPv6 вместо `::/0` — тот же приём, что `0.0.0.0/1` +
/// `128.0.0.0/1` у IPv4: маршрут длиной /1 бьёт провайдерский /0 по длине
/// префикса, поэтому перекрывает его НЕ удаляя (и откат не зависит от того,
/// каким был дефолт).
///
/// Обе половины обязаны стоять вместе: `::/1` — это адреса, начинающиеся с
/// нулевого бита (весь глобальный `2000::/3`), `8000::/1` — с единичного
/// (link-local `fe80::/10`, ULA `fc00::/7`, мультикаст `ff00::/8`). Оставить
/// одну — значит оставить открытой половину адресного пространства.
///
/// Маршруты ОТВЕРГАЮЩИЕ (reject/unreachable), а не «в никуда» (blackhole): по
/// ним `connect()` падает МГНОВЕННО с «сеть недостижима», и приложение сразу
/// берёт IPv4 (Happy Eyeballs, RFC 8305). Чёрная дыра вместо этого дала бы
/// таймауты — на глаз это «интернет тормозит», хотя IPv4 рядом и работает.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const IPV6_HALVES: [&str; 2] = ["::/1", "8000::/1"];

/// Есть ли на машине РАБОЧИЙ выход в интернет по IPv6 прямо сейчас.
///
/// `connect` у UDP не шлёт ни байта — он лишь просит ядро выбрать маршрут и
/// адрес источника. Ошибка (или источник вида `fe80::`, то есть только внутри
/// сегмента) значит, что наружу по IPv6 не выйти.
///
/// Нужно РОВНО для одного решения: считать ли провал блокировки смертельным.
/// На машине без IPv6 команды блокировки могут не пройти (стек v6 выключен
/// целиком) — и это не повод отказывать в подключении: утекать нечему.
/// Спрашивать НАДО ДО блокировки: после неё ответ, разумеется, «нет».
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn has_ipv6() -> bool {
    let Ok(s) = std::net::UdpSocket::bind("[::]:0") else { return false };
    if s.connect("[2001:4860:4860::8888]:53").is_err() {
        return false;
    }
    match s.local_addr().map(|a| a.ip()) {
        Ok(IpAddr::V6(a)) => {
            !(a.is_loopback() || a.is_unspecified() || (a.segments()[0] & 0xffc0) == 0xfe80)
        }
        _ => false,
    }
}

/// Решить судьбу IPv6 и привести решение в исполнение.
///
/// `Err` — IPv6 на машине ЖИВОЙ, а заглушить его не вышло. Подключаться в таком
/// виде нельзя: человек увидит «Защищено», а часть трафика пойдёт мимо туннеля
/// со своим настоящим адресом. Лучше честный отказ, чем тихая утечка.
///
/// Убирать за собой не нужно даже при ошибке (одна половина могла встать, вторая
/// нет): вызывающий заводит guard ДО этого вызова, и его `Drop` стирает обе.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn apply_ipv6_policy(mode: Ipv6Mode, tun: &str) -> Result<(), String> {
    // Вопрос «требует ли режим блокировки» задаём ТОЙ ЖЕ функции, что и `Drop`
    // при уборке, — иначе поставить и снять начинают решать разные правила.
    if !mode.needs_block() {
        return Ok(());
    }
    let live = has_ipv6(); // спрашиваем ДО блокировки — потом ответ всегда «нет»
    match ipv6_block(tun) {
        Ok(()) => Ok(()),
        // ТЕКСТ ЧИТАЕТ ЧЕЛОВЕК, и читает его в строке состояния. Причина отказа
        // тут одна и объяснить её надо в одну фразу; вывод системной команды
        // человеку не говорит ничего, поэтому никуда и не идёт.
        Err(_) if live => Err("Не удалось закрыть IPv6 — через него ваш настоящий адрес утёк бы мимо туннеля. Попробуйте \
                 ещё раз; если IPv6 у вас единственный выход в сеть, разрешите его в bemyvpn.toml: \
                 [guest] ipv6 = \"allow\"."
            .to_string()),
        // Не заглушён, но и выхода по IPv6 на машине нет — утекать нечему.
        Err(_) => Ok(()),
    }
}

/// Что читает человек, когда шлюза по умолчанию не видно. ОДНА фраза на три
/// ветки: править её приходится в одном месте, а проверить правку можно только
/// на своей ОС — три копии значили, что две останутся непроверенными.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const NO_GATEWAY: &str = "Не видно выхода в интернет — проверьте подключение к сети.";

/// Разворачивает маршруты так, что ВЕСЬ трафик идёт в туннель, а на Drop —
/// откатывает (в т.ч. если задачу прервут или туннель оборвётся).
pub struct RouteGuard {
    /// Адрес хоста, приколотый к реальному шлюзу (анти-петля). Нужен ВСЕМ трём
    /// платформам одинаково — поэтому объявлен один раз, а не тремя отдельными
    /// строками под тремя разными `cfg`, из-за которых общее поле читалось как
    /// три платформенных.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    host_ip: String,
    // Дальше — то, что и правда своё у каждой платформы.
    #[cfg(target_os = "linux")]
    resolv: Option<Vec<u8>>,
    #[cfg(target_os = "macos")]
    dns_service: Option<String>,
    #[cfg(target_os = "macos")]
    dns_old: Vec<String>,
    #[cfg(target_os = "windows")]
    tun_name: String,
    #[cfg(target_os = "windows")]
    tun_idx: String,
    /// С КАКОЙ ПОЛИТИКОЙ IPv6 поставлен этот guard — по ней `Drop` и решает,
    /// убирать ли за собой. Лишнее удаление безобидно (маршрута нет → ошибка,
    /// которую мы игнорируем), а вот стереть чужой `::/1` у человека,
    /// выбравшего `ipv6 = "allow"`, — нет.
    ///
    /// Здесь лежит РЕЖИМ, а не пережёванный `bool`: сам вопрос «требует ли этот
    /// режим блокировки» — это правило, и живёт оно ОДНИМ `match` в
    /// `Ipv6Mode::needs_block` (крейт настроек). Сюда его не переписывать: и
    /// установка, и уборка обязаны спрашивать одну функцию, иначе полярности
    /// разъезжаются. Сторож — `each_rule_is_written_in_exactly_one_place`.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    ipv6: Ipv6Mode,
}

// ВТОРОГО ВХОДА СЮДА НЕТ. Была ещё `install(host_ip, tun)` — «как `install_with`,
// но с блокировкой IPv6», с пояснением «нужна оболочкам, у которых под рукой нет
// конфига (терминал поднимает туннель напрямую)». Пояснение устарело: терминал
// давно ходит общим путём `tunnel::run_candidates`, и звал ту функцию НИКТО.
// Умолчание «Block» она при этом дублировала — а живёт оно в
// `bmv_config::Ipv6Mode::parse`, где на него есть тесты.
impl RouteGuard {
    // ── Linux ──
    #[cfg(target_os = "linux")]
    pub fn install_with(host_ip: IpAddr, tun: &str, ipv6: Ipv6Mode) -> Result<Self, String> {
        // ПЕРВЫМ делом — доесть крошку прошлого запуска, иначе снимком «текущего»
        // DNS окажется наш же 8.8.8.8, и настоящий резолвер потеряется навсегда.
        recover_after_crash();
        let (gw, dev) = default_route_linux().ok_or(NO_GATEWAY)?;
        let hip = host_ip.to_string();
        let _ = ip(&["route", "add", &format!("{hip}/32"), "via", &gw, "dev", &dev]);
        for half in IPV4_HALVES {
            ip(&["route", "add", half, "dev", tun])?;
        }
        let resolv = std::fs::read("/etc/resolv.conf").ok();
        // Крошку пишем ВСЕГДА, даже если resolv.conf не прочитался: по ней же
        // чинятся v6-маршруты после смерти процесса, а они переживают её и
        // без DNS (см. recover_after_crash).
        write_dns_crumb(&resolv.as_deref().map(String::from_utf8_lossy).unwrap_or_default());
        let _ = std::fs::write("/etc/resolv.conf", "nameserver 8.8.8.8\n");
        // Guard заводим ДО блокировки IPv6: если она сорвётся на полпути,
        // возврат ошибки дропнет его и откатит уже поставленное.
        let guard = Self { host_ip: hip, resolv, ipv6 };
        apply_ipv6_policy(ipv6, tun)?;
        Ok(guard)
    }

    // ── macOS ──
    // route(8) split-default в utun + пин хоста через реальный шлюз (анти-петля).
    // DNS переставляем на 8.8.8.8 у активного сетевого сервиса (иначе резолв ушёл
    // бы к LAN-DNS мимо туннеля). Всё откатывается в Drop.
    #[cfg(target_os = "macos")]
    pub fn install_with(host_ip: IpAddr, tun: &str, ipv6: Ipv6Mode) -> Result<Self, String> {
        // Первым делом — доесть крошку прошлого запуска (см. DNS_CRUMB): иначе
        // «прежними» серверами запомнится наш же 8.8.8.8.
        recover_after_crash();
        let (gw, dev) = default_route_macos().ok_or(NO_GATEWAY)?;
        let hip = host_ip.to_string();
        // Хост-пин через реальный шлюз, чтобы шифрованные UDP не зациклились в туннель.
        let _ = route(&["-n", "add", "-host", &hip, &gw]);
        // Split-default в utun (перекрывает дефолт, не удаляя его).
        for half in IPV4_HALVES {
            route(&["-n", "add", "-net", half, "-interface", tun])?;
        }
        // DNS → 8.8.8.8 (через туннель). Запоминаем сервис и старые серверы.
        // networksetup меняет СПИСОК сервиса целиком, то есть заодно убирает и
        // v6-резолверы — отдельной возни с ними не нужно.
        let (dns_service, dns_old) = match dns_service_for_dev(&dev) {
            Some(svc) => {
                let old = current_dns(&svc);
                write_dns_crumb(&format!("{svc}\n{}", old.join("\n")));
                let _ = networksetup(&["-setdnsservers", &svc, "8.8.8.8"]);
                (Some(svc), old)
            }
            // Крошка нужна и здесь: по ней чинятся v6-маршруты после смерти
            // процесса. Пустая первая строка = «DNS чинить нечего».
            None => {
                write_dns_crumb("");
                (None, Vec::new())
            }
        };
        // Guard заводим ДО блокировки IPv6 — см. пояснение в linux-ветке.
        let guard = Self { host_ip: hip, dns_service, dns_old, ipv6 };
        apply_ipv6_policy(ipv6, tun)?;
        Ok(guard)
    }

    // ── Windows ──
    // route(8)-аналог: split-default через wintun-адаптер + пин хоста через реальный
    // шлюз (анти-петля). DNS переставляем на 8.8.8.8 у самого tun-адаптера, иначе
    // резолв ушёл бы к LAN-DNS мимо туннеля. Всё требует админ-прав (процесс поднят
    // с манифестом requireAdministrator — один процесс, без отдельного хелпера).
    #[cfg(target_os = "windows")]
    pub fn install_with(host_ip: IpAddr, tun: &str, ipv6: Ipv6Mode) -> Result<Self, String> {
        // Сетевые параметры (шлюз, индекс и IP tun-адаптера) — нативными route/netsh,
        // без PowerShell (он грузится ~15с на холодную → ощущается как зависание).
        let (gw, tun_idx, tun_ip) = windows_net_info(tun).ok_or(NO_GATEWAY)?;
        let hip = host_ip.to_string();
        // 1) Пин хоста через реальный шлюз, чтобы шифрованный UDP не зациклился в туннель.
        let _ = route(&["add", &hip, "mask", "255.255.255.255", &gw, "metric", "1"]);
        // 2) Низкая метрика tun-интерфейса — иначе Windows предпочтёт маршруты
        // физического адаптера и трафик пойдёт мимо туннеля («подключено, но нет
        // интернета»). metric=1 = высший приоритет.
        let _ = netsh(&["interface", "ipv4", "set", "interface", &tun_idx, "metric=1"]);
        // 3) Split-default через tun (перекрывает дефолт, не удаляя его).
        for (net, mask) in IPV4_HALVES_WIN {
            route(&["add", net, "mask", mask, &tun_ip, "metric", "1", "if", &tun_idx])?;
        }
        // 4) DNS → 8.8.8.8 на tun-адаптере.
        let _ = netsh(&["interface", "ipv4", "set", "dnsservers", &format!("name={tun}"), "static", "8.8.8.8", "primary"]);
        // Guard заводим ДО блокировки IPv6 — см. пояснение в linux-ветке.
        let guard = Self { host_ip: hip, tun_name: tun.to_string(), tun_idx: tun_idx.clone(), ipv6 };
        apply_ipv6_policy(ipv6, &tun_idx)?;
        Ok(guard)
    }
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        // Тот же вопрос и та же функция, что при установке (`apply_ipv6_policy`).
        // Ручное сравнение с вариантом здесь запрещено сторожем ниже: полярности
        // «ставить?» и «снимать?» обязаны быть ОДНИМ правилом, иначе новый режим
        // получит блокировку и не получит снятия.
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        if self.ipv6.needs_block() {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            ipv6_unblock();
            #[cfg(target_os = "windows")]
            ipv6_unblock(&self.tun_idx);
        }
        #[cfg(target_os = "linux")]
        {
            for half in IPV4_HALVES {
                let _ = ip(&["route", "del", half]);
            }
            let _ = ip(&["route", "del", &format!("{}/32", self.host_ip)]);
            if let Some(r) = &self.resolv {
                let _ = std::fs::write("/etc/resolv.conf", r);
            }
            let _ = std::fs::remove_file(DNS_CRUMB); // откатились штатно — чинить нечего
        }
        #[cfg(target_os = "macos")]
        {
            for half in IPV4_HALVES {
                let _ = route(&["-n", "delete", "-net", half]);
            }
            let _ = route(&["-n", "delete", "-host", &self.host_ip]);
            if let Some(svc) = &self.dns_service {
                if self.dns_old.is_empty() {
                    let _ = networksetup(&["-setdnsservers", svc, "empty"]);
                } else {
                    let mut a = vec!["-setdnsservers", svc];
                    a.extend(self.dns_old.iter().map(|s| s.as_str()));
                    let _ = networksetup(&a);
                }
            }
            let _ = std::fs::remove_file(DNS_CRUMB); // откатились штатно — чинить нечего
        }
        #[cfg(target_os = "windows")]
        {
            for (net, mask) in IPV4_HALVES_WIN {
                let _ = route(&["delete", net, "mask", mask]);
            }
            let _ = route(&["delete", &self.host_ip]);
            // DNS у tun-адаптера обратно на авто (адаптер всё равно исчезнет вместе с TUN).
            let _ = netsh(&["interface", "ipv4", "set", "dnsservers", &format!("name={}", self.tun_name), "dhcp"]);
        }
    }
}

// ── Linux helpers ──
#[cfg(target_os = "linux")]
fn ip(args: &[&str]) -> Result<(), String> {
    run("ip", args)
}

#[cfg(target_os = "linux")]
fn default_route_linux() -> Option<(String, String)> {
    let out = bmv_common::command("ip").args(["route", "show", "default"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let toks: Vec<&str> = s.split_whitespace().collect();
    let (mut gw, mut dev) = (None, None);
    for w in toks.windows(2) {
        match w[0] {
            "via" => gw = Some(w[1].to_string()),
            "dev" => dev = Some(w[1].to_string()),
            _ => {}
        }
    }
    Some((gw?, dev?))
}

// ── macOS helpers ──
#[cfg(target_os = "macos")]
fn route(args: &[&str]) -> Result<(), String> {
    run("route", args)
}

#[cfg(target_os = "macos")]
fn networksetup(args: &[&str]) -> Result<(), String> {
    run("networksetup", args)
}

#[cfg(target_os = "macos")]
fn default_route_macos() -> Option<(String, String)> {
    let out = bmv_common::command("route").args(["-n", "get", "default"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let (mut gw, mut dev) = (None, None);
    for line in s.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("gateway:") {
            gw = Some(v.trim().to_string());
        } else if let Some(v) = l.strip_prefix("interface:") {
            dev = Some(v.trim().to_string());
        }
    }
    Some((gw?, dev?))
}

/// Имя сетевого сервиса (напр. «Wi-Fi») по устройству (en0) — для networksetup.
#[cfg(target_os = "macos")]
fn dns_service_for_dev(dev: &str) -> Option<String> {
    let out = bmv_common::command("networksetup").arg("-listnetworkserviceorder").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    // Блоки вида:
    //   (1) Wi-Fi
    //   (Hardware Port: Wi-Fi, Device: en0)
    let mut last_name: Option<String> = None;
    for line in s.lines() {
        let t = line.trim();
        if t.starts_with("(Hardware Port:") {
            // "(Hardware Port: Wi-Fi, Device: en0)"
            if t.contains(&format!("Device: {dev})")) {
                return last_name.clone();
            }
        } else if t.starts_with('(') {
            // "(1) Wi-Fi" → имя сервиса после ") "
            if let Some(p) = t.find(") ") {
                last_name = Some(t[p + 2..].trim().to_string());
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn current_dns(service: &str) -> Vec<String> {
    let out = bmv_common::command("networksetup").args(["-getdnsservers", service]).output();
    let Ok(out) = out else { return Vec::new() };
    let s = String::from_utf8_lossy(&out.stdout);
    // «There aren't any DNS Servers set on …» → пусто.
    if s.contains("aren't any") {
        return Vec::new();
    }
    s.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':'))
        .filter(|l| !l.is_empty())
        .collect()
}

/// Разрешить нашему exe входящий UDP в брандмауэре Windows. Нужно для UDP
/// hole-punching: пакеты пробития приходят от адреса, которому мы ещё не слали,
/// и стейтфул-брандмауэр их режет → рукопожатие с хостом не завершается (на
/// Android этого барьера нет — там подключение быстрое, а на Windows виснет).
/// Требует админ-прав (процесс уже под ними). Вызывать один раз при старте.
#[cfg(target_os = "windows")]
pub fn ensure_firewall_allow() {
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.to_string_lossy().to_string();
    // Пересоздаём (обновить путь, не плодить дубли).
    let _ = netsh(&["advfirewall", "firewall", "delete", "rule", "name=BeMyVPN"]);
    let _ = netsh(&[
        "advfirewall", "firewall", "add", "rule", "name=BeMyVPN",
        "dir=in", "action=allow", &format!("program={exe}"), "protocol=udp", "enable=yes",
    ]);
}

// ── Windows helpers ──
#[cfg(target_os = "windows")]
fn route(args: &[&str]) -> Result<(), String> {
    run("route", args)
}

#[cfg(target_os = "windows")]
fn netsh(args: &[&str]) -> Result<(), String> {
    run("netsh", args)
}

/// Сетевые параметры для маршрутизации: шлюз по умолчанию, ifIndex tun-адаптера,
/// его IPv4. БЕЗ PowerShell — он грузится ~15с на холодную (модули Get-Net*), что
/// ощущается как зависание. `route.exe`/`netsh.exe` нативные (~50мс).
/// Возвращает (шлюз, tun_idx, tun_ip).
///
/// tun-IP БЕРЁТСЯ У `TunParams::guest()`, а не вписан числом: это тот самый
/// адрес, который `make_tun` только что назначил интерфейсу, и Windows требует
/// его как next hop. Вписанная копия разошлась бы с оригиналом молча — ветку
/// Windows на маке и линуксе никто не собирает, а у человека маршруты просто не
/// встали бы («подключено, но нет интернета»).
#[cfg(target_os = "windows")]
fn windows_net_info(tun: &str) -> Option<(String, String, String)> {
    let gw = default_gateway_windows()?;
    let idx = adapter_index_windows(tun)?;
    Some((gw, idx, bmv_tunnel::TunParams::guest().address.to_string()))
}

/// Захват stdout нативной команды (без окна консоли — см. `bmv_common::command`).
#[cfg(target_os = "windows")]
fn cmd_out(cmd: &str, args: &[&str]) -> Option<String> {
    let out = bmv_common::command(cmd).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Шлюз по умолчанию из `route print -4`: строка данных дефолт-маршрута —
/// «0.0.0.0  0.0.0.0  <шлюз>  <интерфейс>  <метрика>» (числа/IP, не локализованы).
#[cfg(target_os = "windows")]
fn default_gateway_windows() -> Option<String> {
    let out = cmd_out("route", &["print", "-4"])?;
    for line in out.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() >= 3 && t[0] == "0.0.0.0" && t[1] == "0.0.0.0" && t[2] != "0.0.0.0" && t[2].contains('.') {
            return Some(t[2].to_string());
        }
    }
    None
}

/// ifIndex tun-адаптера из `netsh interface ipv4 show interfaces`. Строки данных:
/// «Idx Met MTU State Name» — берём строку, чьё Name == наше имя (bmv0, без
/// пробелов), первый токен = индекс. Заголовок локализован — не используем его.
#[cfg(target_os = "windows")]
fn adapter_index_windows(name: &str) -> Option<String> {
    let out = cmd_out("netsh", &["interface", "ipv4", "show", "interfaces"])?;
    for line in out.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() >= 5 && t[0].parse::<u32>().is_ok() && t[4..].join(" ") == name {
            return Some(t[0].to_string());
        }
    }
    None
}

/// Что читает человек, когда системная команда маршрутизации отказала.
///
/// ЗДЕСЬ СТОЯЛО «нужны права администратора. Запустите приложение ещё раз и
/// введите пароль» — вторая копия той же лжи, что жила в гостевом слое. И здесь
/// она была даже наглее: до `run` доходят ТОЛЬКО после того, как `make_tun`
/// отработал успешно, а сетевой адаптер без прав администратора (root, CAP_NET_ADMIN,
/// elevated-процесс на Windows) не создаётся вовсе. Значит права у нас ЕСТЬ —
/// доказано предыдущей строкой, — и обвинять их нельзя ни при какой ошибке.
///
/// Причину назвать нечем: `route`/`netsh`/`ip` отказывают одинаково молча, а их
/// ругань мы не сохраняем нарочно (см. ниже). Поэтому — честное «не знаем».
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const ROUTES_FAILED: &str = "Не удалось направить трафик в туннель: система отклонила настройку маршрутов и \
                             причину не назвала. Попробуйте ещё раз.";

// ── общий запуск команды ──
//
// Через `bmv_common::command`, а не `Command::new`: на Windows он ставит
// CREATE_NO_WINDOW, без которого каждый route/netsh мигает окном консоли.
//
// Текст ошибки отсюда доезжает ДО ЧЕЛОВЕКА, в строку состояния — поэтому он и
// стоит отдельной константой (`ROUTES_FAILED`), под сторожем.
//
// Саму ругань НЕ СОХРАНЯЕМ НИГДЕ, хотя разбирать поломку по ней было удобно: в
// аргументах сетевых команд стоят адрес шлюза и имя интерфейса, то есть запись
// об этом вызове — запись о сети человека.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = bmv_common::command(cmd).args(args).output().map_err(|_| ROUTES_FAILED.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(ROUTES_FAILED.to_string())
    }
}

/// СТОРОЖ НА САМУ ЛОЖЬ: совет про пароль стоит РОВНО там, где дело в правах.
///
/// Поломка, ради которой он написан, выглядела так: человек на Windows-ARM читал
/// «нужны права администратора… введите пароль», послушно вводил его — и ничего
/// не менялось, потому что дело было в библиотеке не той разрядности. Совет был
/// не просто бесполезен: он увёл человека от настоящей причины на несколько дней.
///
/// Проверяется не намерение, а ТЕКСТ — тот самый, что читает человек. Слова
/// подобраны так, чтобы поймать возврат прежней фразы в любом её пересказе.
#[cfg(test)]
mod honest_failures {
    use super::TunFail;

    /// Слова, которыми объясняют нехватку ПРАВ. Годятся только для `NoRights`.
    const BLAMES_RIGHTS: [&str; 4] = ["админ", "пароль", "sudo", "root"];

    #[test]
    fn the_password_advice_belongs_only_to_a_rights_failure() {
        for f in TunFail::ALL {
            let text = f.human();
            assert!(!text.is_empty(), "{f:?} без слов для человека");
            let blames = BLAMES_RIGHTS.iter().find(|w| text.contains(**w));
            assert_eq!(
                blames.is_some(),
                f == TunFail::NoRights,
                "неудача {f:?} объясняется правами{}: «{text}»\n\
                 Права называют ТОЛЬКО у TunFail::NoRights — там система прямо ответила «доступ \
                 запрещён». В остальных случаях совет «введите пароль» уводит от настоящей причины: \
                 ровно так владелец на Windows-ARM вводил пароль, пока в exe лежала библиотека не \
                 той разрядности.",
                blames.map(|w| format!(" (слово «{w}»)")).unwrap_or_default(),
            );
        }
        // И наоборот: у настоящей нехватки прав совет обязан БЫТЬ — иначе человек
        // не узнает, что делать, там, где мы точно знаем.
        let rights = TunFail::NoRights.human();
        assert!(rights.contains("админ") && rights.contains("пароль"), "«{rights}»");
        // Каждый род неудачи говорит СВОЁ: одинаковые тексты — это тот же
        // «один заготовленный ответ на всё», только записанный четырьмя строками.
        for (i, a) in TunFail::ALL.iter().enumerate() {
            for b in &TunFail::ALL[i + 1..] {
                assert_ne!(a.human(), b.human(), "{a:?} и {b:?} объяснены одинаково");
            }
        }
    }

    /// Маршруты тоже не смеют винить права: до них доходят ПОСЛЕ удачного
    /// `make_tun`, а он без прав администратора не отрабатывает вовсе.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn a_failed_route_command_does_not_blame_rights_we_already_have() {
        let text = super::ROUTES_FAILED;
        for w in BLAMES_RIGHTS {
            assert!(
                !text.contains(w),
                "настройка маршрутов объясняется словом «{w}»: «{text}»\n\
                 Права к этому моменту уже доказаны: сетевой адаптер создан, а он без них не \
                 создаётся. Причина отказа route/netsh/ip нам неизвестна — так и надо писать.",
            );
        }
    }

    /// А ЭТО — вход в ложь: место, где неизвестное становится «известным».
    ///
    /// Сторож на текст поймает переписанную фразу, но не поймает случая, когда
    /// прежний текст оставили на месте, а в `NoRights` завернули посторонний
    /// отказ. Ловится здесь: правами объявляется РОВНО «доступ запрещён».
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn only_a_permission_error_is_called_a_permission_error() {
        use std::io::{Error as IoError, ErrorKind};
        let io = |k: ErrorKind| tun::Error::Io(IoError::from(k));
        assert_eq!(super::classify(&io(ErrorKind::PermissionDenied)), TunFail::NoRights);
        assert_eq!(super::classify(&io(ErrorKind::NotFound)), TunFail::NoDevice);
        // Всё остальное — НЕ права. Именно эти отказы и получал человек на
        // Windows-ARM, а читал про пароль.
        for k in [ErrorKind::AddrInUse, ErrorKind::InvalidInput, ErrorKind::Other, ErrorKind::TimedOut] {
            assert_eq!(super::classify(&io(k)), TunFail::Unknown, "{k:?} объявлен нехваткой прав");
        }
        assert_eq!(super::classify(&tun::Error::InvalidName), TunFail::Unknown);
        assert_eq!(super::classify(&tun::Error::InvalidConfig), TunFail::Unknown);
        assert_eq!(super::classify(&tun::Error::NotImplemented), TunFail::Unknown);
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod route_tests {
    use super::{has_ipv6, IPV4_HALVES, IPV4_HALVES_WIN, IPV6_HALVES};
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// Префикс → отрезок адресов [первый, последний] числом.
    ///
    /// `bits` — размер адресного пространства (32 для IPv4, 128 для IPv6): всё
    /// считается в u128, потому что правило у обеих версий ОДНО и проверять его
    /// двумя разными арифметиками значило бы завести вторую копию проверки.
    fn span(p: &str, first: u128, len: u32, bits: u32) -> (u128, u128) {
        // /0 закрыл бы всё одной строкой, но проиграл бы маршруту провайдера:
        // у того тоже /0, а побеждает САМЫЙ ДЛИННЫЙ.
        assert!(
            (1..=bits).contains(&len),
            "{p}: длина префикса должна быть 1..={bits}, иначе дефолт провайдера победит",
        );
        let size = 1u128 << (bits - len);
        assert_eq!(first % size, 0, "{p}: адрес не выровнен по своей же длине префикса");
        (first, first + (size - 1))
    }

    /// Отрезки обязаны замостить [0, 2^bits) ЦЕЛИКОМ: первый начинается с нуля,
    /// каждый следующий — сразу за предыдущим, последний кончается на максимуме.
    /// Ни щели, ни нахлёста. Возвращает их же, отсортированными.
    fn tiles_everything(mut spans: Vec<(u128, u128)>, bits: u32) -> Vec<(u128, u128)> {
        spans.sort_unstable();
        assert_eq!(spans.first().expect("список половин пуст").0, 0, "начало пространства не закрыто");
        for w in spans.windows(2) {
            assert_eq!(w[1].0, w[0].1 + 1, "между {:?} и {:?} щель или нахлёст", w[0], w[1]);
        }
        let last = u128::MAX >> (128 - bits);
        assert_eq!(spans.last().expect("список половин пуст").1, last, "конец пространства не закрыт");
        spans
    }

    /// «::/1» → отрезок.
    fn span_v6(p: &str) -> (u128, u128) {
        let (addr, len) = p.split_once('/').unwrap_or_else(|| panic!("префикс без длины: {p}"));
        let net: Ipv6Addr = addr.parse().unwrap_or_else(|e| panic!("{p}: не IPv6-адрес ({e})"));
        let len = len.parse().unwrap_or_else(|e| panic!("{p}: не длина префикса ({e})"));
        span(p, u128::from(net), len, 128)
    }

    /// «0.0.0.0/1» → отрезок.
    fn span_v4(p: &str) -> (u128, u128) {
        let (addr, len) = p.split_once('/').unwrap_or_else(|| panic!("префикс без длины: {p}"));
        let net: Ipv4Addr = addr.parse().unwrap_or_else(|e| panic!("{p}: не IPv4-адрес ({e})"));
        let len = len.parse().unwrap_or_else(|e| panic!("{p}: не длина префикса ({e})"));
        span(p, u128::from(u32::from(net)), len, 32)
    }

    /// БЛОКИРОВКА ОБЯЗАНА ЗАКРЫВАТЬ ВСЁ АДРЕСНОЕ ПРОСТРАНСТВО IPv6 ЦЕЛИКОМ.
    ///
    /// Это ровно та проверка, которую человеку самому не сделать: дыра в
    /// покрытии выглядит как полностью рабочий VPN — просто часть сайтов
    /// (те, чьи адреса попали в незакрытую половину) видит настоящий адрес.
    #[test]
    fn the_two_halves_tile_the_entire_ipv6_space() {
        let spans = tiles_everything(IPV6_HALVES.iter().map(|p| span_v6(p)).collect(), 128);

        // Точечно — по одному живому адресу из каждой половины, чтобы поломка
        // читалась глазами, а не только через арифметику выше.
        let hit = |a: &str| {
            let addr = u128::from(a.parse::<Ipv6Addr>().unwrap());
            spans.iter().filter(|(lo, hi)| (*lo..=*hi).contains(&addr)).count()
        };
        for a in ["2001:4860:4860::8888", "2606:4700::1111", "::1", "fe80::1", "fc00::1", "ff02::1"] {
            assert_eq!(hit(a), 1, "{a} закрыт не ровно одной половиной");
        }
    }

    /// ТО ЖЕ САМОЕ ДЛЯ IPv4 — и в ОБЕИХ записях сразу.
    ///
    /// Половины IPv4 записаны дважды: префиксами (linux/macos) и парой
    /// «сеть+маска» (Windows, `route.exe` префиксов не понимает). Сверить их
    /// глазами нельзя — на маке и линуксе ветка Windows даже не собирается,
    /// поэтому расхождение доехало бы до человека под Windows целым: туннель
    /// поднят, «Защищено» горит, а половина трафика идёт мимо него.
    ///
    /// Незакрытая половина IPv4 — это не «часть сайтов», это ПОЛОВИНА интернета
    /// со настоящим адресом человека.
    #[test]
    fn both_spellings_of_the_ipv4_halves_agree() {
        let prefixes = tiles_everything(IPV4_HALVES.iter().map(|p| span_v4(p)).collect(), 32);

        let windows: Vec<(u128, u128)> = IPV4_HALVES_WIN
            .iter()
            .map(|(net, mask)| {
                let m = u32::from(mask.parse::<Ipv4Addr>().unwrap_or_else(|e| panic!("{mask}: не IPv4-маска ({e})")));
                // Маска обязана быть СПЛОШНОЙ (единицы слева) — иначе это не
                // префикс, а решето, и «половина» перестаёт быть половиной.
                assert_eq!(m.leading_ones() + m.trailing_zeros(), 32, "{mask}: маска не сплошная");
                span_v4(&format!("{net}/{}", m.leading_ones()))
            })
            .collect();
        let windows = tiles_everything(windows, 32);

        assert_eq!(prefixes, windows, "две записи одних и тех же половин разошлись — под Windows завернётся не тот трафик");
    }

    /// ЧАСОВОЙ ПРОТИВ ВТОРОЙ КОПИИ. Приём одолжен у `bmv-common`
    /// (`one_place_per_rule.rs`): дешевле поймать грепом, чем ловить руками.
    ///
    /// Здесь он нужен острее, чем где бы то ни было: из трёх платформенных
    /// веток на машине разработчика СОБИРАЕТСЯ ОДНА. Копия, разошедшаяся в
    /// ветке Windows, не краснеет ни в одном тесте и доезжает до человека
    /// целой — а на его стороне выглядит как «подключено, но нет интернета»
    /// или, хуже, как работающий VPN с утечкой.
    #[test]
    fn each_rule_is_written_in_exactly_one_place() {
        const SRC: &str = include_str!("lib.rs");
        // Считаем ТОЛЬКО боевой код: дальше идут тестовые модули, и признаки
        // ниже встречаются в них самих.
        let code = SRC.split("#[cfg(all(test").next().expect("split всегда даёт хотя бы кусок");
        assert!(code.len() < SRC.len(), "тестовые модули не отрезались — счёт пойдёт по самим признакам");

        // КОММЕНТАРИЙ — НЕ КОПИЯ ПРАВИЛА. Считали по сырому тексту, и потому
        // назвать правило в пояснении рядом с ним было нельзя: часовой краснел
        // от объяснения, которое сам же и требует. Выкидываем строки-комментарии
        // целиком и хвостовые пояснения после « // ».
        //
        // Потолок: пробелы вокруг `//` обязательны, иначе под нож попали бы
        // `http://` и прочие косые внутри строк. Это осознанно — правило
        // «признак в кавычках» так не потерять.
        let code: String = code
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .map(|l| l.split(" // ").next().unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");
        let code = code.as_str();

        // (правило, признак, сколько раз он ВПРАВЕ встретиться)
        let rules: [(&str, String, usize); 6] = [
            // Адрес гостевого TUN. Windows требует его как next hop в каждом
            // маршруте, и он был вписан сюда числом второй раз.
            ("адрес гостевого TUN живёт в bmv_tunnel::TunParams::guest()", bmv_tunnel::TunParams::guest().address.to_string(), 0),
            // ПОЛЯРНОСТЬ IPv6. Правило «требует ли режим блокировки» живёт одним
            // match в Ipv6Mode::needs_block. Сравнивать режим с вариантом РУКАМИ
            // здесь нельзя ни разу: ровно так оно и разъехалось — установка
            // спрашивала «не Allow ли?», уборка «Block ли?», и третий режим
            // получил бы блокировку без снятия.
            ("полярность IPv6 не пишется руками (Ipv6Mode::needs_block)", "Ipv6Mode::Block".to_string(), 0),
            ("полярность IPv6 не пишется руками (Ipv6Mode::needs_block)", "Ipv6Mode::Allow".to_string(), 0),
            // ...и спросить её обязаны ОБА: тот, кто ставит (apply_ipv6_policy),
            // и тот, кто снимает (RouteGuard::drop). Станет один — блокировка
            // либо не встанет, либо не снимется.
            ("и установка, и уборка спрашивают одну функцию", "needs_block()".to_string(), 2),
            // Фраза «нет выхода в интернет» — её читает человек, и она была
            // скопирована в каждую из трёх веток.
            ("текст «не видно выхода в интернет» (NO_GATEWAY)", "Не видно выхода в интернет".to_string(), 1),
            // ГЛАВНОЕ ПРАВИЛО ЭТОГО ФАЙЛА, которого в таблице не было: половины
            // IPv4/IPv6 — по одной константе на запись, а не литералами по
            // веткам. Двенадцать копий тут уже жили, и опечатка в откате не
            // видна никак: она оставляет человека с завёрнутым трафиком ПОСЛЕ
            // выключения VPN.
            ("половины маршрутов живут в константах, а не литералами", "\"0.0.0.0/1\"".to_string(), 1),
        ];
        for (rule, mark, allowed) in rules {
            let n = code.matches(&mark).count();
            assert_eq!(n, allowed, "{rule}: признак «{mark}» встречается {n} раз(а), а можно {allowed}");
        }
    }

    /// Проба на живой IPv6 не должна ни падать, ни зависать: она стоит на пути
    /// КАЖДОГО подключения, и её отказ решает, пускать человека или нет.
    #[test]
    fn the_ipv6_probe_is_harmless_without_network() {
        let t = std::time::Instant::now();
        let _ = has_ipv6(); // ответ зависит от машины; важно, что он есть и быстро
        assert!(t.elapsed() < std::time::Duration::from_secs(1), "проба IPv6 подвисает: {:?}", t.elapsed());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::utun_af_header;

    /// Кадрирование utun (macOS) — ИМЕННО из-за него туннель рвался мгновенно:
    /// ядро качает чистые IP-пакеты, а utun требует 4-байтовый AF-заголовок.
    /// Проверяем, что заголовок ставится верно по версии IP, и что «снятие» на
    /// чтении (срез первых 4 байт) возвращает исходный пакет.
    #[test]
    fn utun_framing_roundtrip() {
        // IPv4: старший ниббл первого байта = 4 → AF_INET (2).
        let ip4 = [0x45u8, 0x00, 0x00, 0x28, 0xDE, 0xAD];
        assert_eq!(utun_af_header(&ip4), [0, 0, 0, 2]);
        // IPv6: старший ниббл = 6 → AF_INET6 (30).
        let ip6 = [0x60u8, 0x00, 0x00, 0x00];
        assert_eq!(utun_af_header(&ip6), [0, 0, 0, 30]);
        // На записи добавляем заголовок; на чтении снимаем первые 4 байта.
        let mut framed = utun_af_header(&ip4).to_vec();
        framed.extend_from_slice(&ip4);
        assert_eq!(&framed[4..], &ip4, "снятие 4-байтового заголовка должно вернуть пакет");
        // Пустой ввод не должен паниковать.
        assert_eq!(utun_af_header(&[]), [0, 0, 0, 2]);
    }
}

/// Windows: `wintun.dll` обязана ГРУЗИТЬСЯ на этой машине.
///
/// Проверка живая, а не по заголовку файла: библиотека вшита в exe, распакована
/// нами и поднята `LoadLibrary` внутри крейта `tun` — сломаться может любое из
/// трёх звеньев, а видно это только на настоящей Windows. Ровно это звено и
/// отказало на Windows-ARM: в exe уезжала x86_64-библиотека, гость получал
/// «нужны права администратора» вместо туннеля.
///
/// `#[ignore]`: нужен админ и заводится НАСТОЯЩИЙ адаптер. Гоняется на обоих
/// Windows-раннерах CI (см. job `windows-check`), там админ есть.
/// Вывод латиницей — кириллица в консоли Windows-раннера роняет шаг.
#[cfg(all(test, target_os = "windows"))]
mod wintun_live {
    #[test]
    #[ignore]
    fn wintun_loads() {
        match super::make_tun(&bmv_tunnel::TunParams::guest()) {
            Ok((_dev, name)) => eprintln!("wintun OK: adapter '{name}' created"),
            // {e:?}, а не {e}: у `TunFail` намеренно НЕТ Display. Слова к
            // признаку подбирает `human()` — по-русски и для человека, а сюда
            // они не годятся: кириллица в консоли Windows-раннера роняет шаг.
            // В журнал прогона нужен сам признак — `DriverLoad` говорит о
            // причине больше, чем любая фраза.
            Err(e) => panic!("wintun FAILED: {e:?}"),
        }
    }
}

/// Регрессия на утечку TUN-интерфейсов: 5 циклов «создать + дропнуть» НЕ должны
/// оставлять интерфейсов (fd закрывается на Drop, persist не ставится). Нужен root
/// и живой /dev/net/tun, поэтому #[ignore] — не запускается в обычном cargo test.
/// CI: `sudo -E cargo test -p bmv-desktop --release tun_no_leak -- --ignored --nocapture`.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod leak_test {
    use super::make_tun;

    fn count_tun() -> usize {
        #[cfg(target_os = "linux")]
        let cmd = "ip -o link show type tun 2>/dev/null | wc -l";
        #[cfg(target_os = "macos")]
        let cmd = "ifconfig -l | tr ' ' '\\n' | grep -c '^utun'";
        let out = std::process::Command::new("sh").arg("-c").arg(cmd).output().expect("count");
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
    }

    #[test]
    #[ignore]
    fn tun_no_leak() {
        let base = count_tun();
        eprintln!("LEAK: TUN до цикла = {base}");
        for i in 0..5 {
            let (dev, name) = make_tun(&bmv_tunnel::TunParams::guest()).expect("make_tun (нужен root)");
            let during = count_tun();
            eprintln!("LEAK: цикл {i}: создан {name}, всего TUN = {during}");
            assert!(during > base, "интерфейс должен появиться на время жизни device");
            drop(dev);
            std::thread::sleep(std::time::Duration::from_millis(400)); // ядру на снятие
        }
        let after = count_tun();
        eprintln!("LEAK: TUN после 5 циклов = {after} (было {base})");
        assert_eq!(after, base, "TUN-интерфейсы НЕ должны накапливаться после дропа");
    }
}

