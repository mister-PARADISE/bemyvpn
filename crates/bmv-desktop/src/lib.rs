//! Десктопный роутинг VPN-гостя: создать TUN и завернуть ВЕСЬ трафик в туннель,
//! а на Drop — откатить. Один код для CLI, GUI и привилегированного хелпера GUI.
//! Android/iOS сюда не ходят — там свой fd из платформенного шелла.
//!
//! Настоящий VPN, а не прокси: split-default (`0.0.0.0/1` + `128.0.0.0/1`)
//! перекрывает дефолтный маршрут НЕ удаляя его; сам хост пинуется через реальный
//! шлюз (иначе шифрованные пакеты зациклятся). `RouteGuard::drop` всё откатывает.
//! Реализовано для Linux и macOS (обе требуют root); Windows — задел.
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

/// Создать десктопный TUN (нужен root/sudo). Android/iOS дают готовый fd мимо этого.
/// Возвращает устройство И РЕАЛЬНОЕ имя интерфейса: на macOS ядро само выдаёт
/// `utunN` (кастомное «bmv0» там невалидно → «invalid device name»), а маршруты
/// ставить надо именно по выданному имени.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub fn make_tun(params: &TunParams) -> Result<(TunDevice, String), String> {
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
    let dev = tun::create_as_async(&config).map_err(|e| e.to_string())?;
    let name = dev.get_ref().name().map_err(|e| e.to_string())?;
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

/// Windows: `wintun.dll` вшита в exe (bmv-desktop::ensure_wintun) — распаковываем,
/// чтобы LoadLibrary нашёл. Лицензия wintun разрешает немодифицированную копию.
///
/// Крейт `wintun`, который её грузит, у нас форкнут (vendor/wintun) — там `netsh`
/// запускается с CREATE_NO_WINDOW, иначе при подключении мигают окна консоли.
/// Про запрет на подъём `tun` до 0.8 — комментарий в корневом Cargo.toml.
#[cfg(target_os = "windows")]
fn ensure_wintun() -> Result<(), String> {
    static WINTUN: &[u8] =
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/windows/wintun.dll"));
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
    std::fs::create_dir_all(&base).map_err(|e| format!("wintun: {e}"))?;
    write_dll(&base).map_err(|e| format!("wintun: {e}"))?;
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
    if mode == Ipv6Mode::Allow {
        tracing::warn!("IPv6 НЕ блокируется (guest.ipv6 = \"allow\"): v6-трафик пойдёт МИМО туннеля");
        return Ok(());
    }
    let live = has_ipv6(); // спрашиваем ДО блокировки — потом ответ всегда «нет»
    match ipv6_block(tun) {
        Ok(()) => {
            tracing::info!("IPv6 заглушён на время сеанса ({})", IPV6_HALVES.join(" + "));
            Ok(())
        }
        Err(e) if live => Err(format!(
            "IPv6 работает, но заглушить его не удалось ({e}). Подключение отменено: \
             иначе часть трафика ушла бы мимо туннеля вместе с настоящим адресом. \
             Если IPv6 у вас единственный способ выйти в сеть — поставьте в bemyvpn.toml \
             [guest] ipv6 = \"allow\" и знайте, что защиты по IPv6 не будет"
        )),
        Err(e) => {
            tracing::debug!("IPv6 не заглушён ({e}), но и выхода по IPv6 на машине нет — утекать нечему");
            Ok(())
        }
    }
}

/// Разворачивает маршруты так, что ВЕСЬ трафик идёт в туннель, а на Drop —
/// откатывает (в т.ч. если задачу прервут или туннель оборвётся).
pub struct RouteGuard {
    #[cfg(target_os = "linux")]
    host_ip: String,
    #[cfg(target_os = "linux")]
    resolv: Option<Vec<u8>>,
    #[cfg(target_os = "macos")]
    host_ip: String,
    #[cfg(target_os = "macos")]
    dns_service: Option<String>,
    #[cfg(target_os = "macos")]
    dns_old: Vec<String>,
    #[cfg(target_os = "windows")]
    host_ip: String,
    #[cfg(target_os = "windows")]
    tun_name: String,
    #[cfg(target_os = "windows")]
    tun_idx: String,
    /// Ставили ли блокировку IPv6 — значит, её надо снять в `Drop`. Лишнее
    /// удаление безобидно (маршрута нет → ошибка, которую мы игнорируем), а вот
    /// стереть чужой `::/1` у человека, выбравшего `ipv6 = "allow"`, — нет.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    ipv6_blocked: bool,
}

impl RouteGuard {
    /// Как `install_with`, но с блокировкой IPv6 — это безопасное умолчание.
    ///
    /// Отдельный вход нужен оболочкам, у которых под рукой нет конфига (терминал
    /// поднимает туннель напрямую). Умолчание тут не «как получится», а самая
    /// защищённая ветка: забыть про IPv6 — значит молча утечь.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    pub fn install(host_ip: IpAddr, tun: &str) -> Result<Self, String> {
        Self::install_with(host_ip, tun, Ipv6Mode::Block)
    }

    // ── Linux ──
    #[cfg(target_os = "linux")]
    pub fn install_with(host_ip: IpAddr, tun: &str, ipv6: Ipv6Mode) -> Result<Self, String> {
        // ПЕРВЫМ делом — доесть крошку прошлого запуска, иначе снимком «текущего»
        // DNS окажется наш же 8.8.8.8, и настоящий резолвер потеряется навсегда.
        recover_after_crash();
        let (gw, dev) = default_route_linux().ok_or("не найден шлюз по умолчанию")?;
        let hip = host_ip.to_string();
        let _ = ip(&["route", "add", &format!("{hip}/32"), "via", &gw, "dev", &dev]);
        ip(&["route", "add", "0.0.0.0/1", "dev", tun])?;
        ip(&["route", "add", "128.0.0.0/1", "dev", tun])?;
        let resolv = std::fs::read("/etc/resolv.conf").ok();
        // Крошку пишем ВСЕГДА, даже если resolv.conf не прочитался: по ней же
        // чинятся v6-маршруты после смерти процесса, а они переживают её и
        // без DNS (см. recover_after_crash).
        write_dns_crumb(&resolv.as_deref().map(String::from_utf8_lossy).unwrap_or_default());
        let _ = std::fs::write("/etc/resolv.conf", "nameserver 8.8.8.8\n");
        // Guard заводим ДО блокировки IPv6: если она сорвётся на полпути,
        // возврат ошибки дропнет его и откатит уже поставленное.
        let guard = Self { host_ip: hip, resolv, ipv6_blocked: ipv6 == Ipv6Mode::Block };
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
        let (gw, dev) = default_route_macos().ok_or("не найден шлюз по умолчанию")?;
        let hip = host_ip.to_string();
        // Хост-пин через реальный шлюз, чтобы шифрованные UDP не зациклились в туннель.
        let _ = route(&["-n", "add", "-host", &hip, &gw]);
        // Split-default в utun (перекрывает дефолт, не удаляя его).
        route(&["-n", "add", "-net", "0.0.0.0/1", "-interface", tun])?;
        route(&["-n", "add", "-net", "128.0.0.0/1", "-interface", tun])?;
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
        let guard = Self { host_ip: hip, dns_service, dns_old, ipv6_blocked: ipv6 == Ipv6Mode::Block };
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
        let (gw, tun_idx, tun_ip) = windows_net_info(tun).ok_or("не удалось получить сетевые параметры")?;
        let hip = host_ip.to_string();
        // 1) Пин хоста через реальный шлюз, чтобы шифрованный UDP не зациклился в туннель.
        let _ = route(&["add", &hip, "mask", "255.255.255.255", &gw, "metric", "1"]);
        // 2) Низкая метрика tun-интерфейса — иначе Windows предпочтёт маршруты
        // физического адаптера и трафик пойдёт мимо туннеля («подключено, но нет
        // интернета»). metric=1 = высший приоритет.
        let _ = netsh(&["interface", "ipv4", "set", "interface", &tun_idx, "metric=1"]);
        // 3) Split-default через tun (перекрывает дефолт, не удаляя его).
        route(&["add", "0.0.0.0", "mask", "128.0.0.0", &tun_ip, "metric", "1", "if", &tun_idx])?;
        route(&["add", "128.0.0.0", "mask", "128.0.0.0", &tun_ip, "metric", "1", "if", &tun_idx])?;
        // 4) DNS → 8.8.8.8 на tun-адаптере.
        let _ = netsh(&["interface", "ipv4", "set", "dnsservers", &format!("name={tun}"), "static", "8.8.8.8", "primary"]);
        // Guard заводим ДО блокировки IPv6 — см. пояснение в linux-ветке.
        let guard = Self {
            host_ip: hip,
            tun_name: tun.to_string(),
            tun_idx: tun_idx.clone(),
            ipv6_blocked: ipv6 == Ipv6Mode::Block,
        };
        apply_ipv6_policy(ipv6, &tun_idx)?;
        Ok(guard)
    }
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.ipv6_blocked {
            ipv6_unblock();
        }
        #[cfg(target_os = "windows")]
        if self.ipv6_blocked {
            ipv6_unblock(&self.tun_idx);
        }
        #[cfg(target_os = "linux")]
        {
            let _ = ip(&["route", "del", "0.0.0.0/1"]);
            let _ = ip(&["route", "del", "128.0.0.0/1"]);
            let _ = ip(&["route", "del", &format!("{}/32", self.host_ip)]);
            if let Some(r) = &self.resolv {
                let _ = std::fs::write("/etc/resolv.conf", r);
            }
            let _ = std::fs::remove_file(DNS_CRUMB); // откатились штатно — чинить нечего
        }
        #[cfg(target_os = "macos")]
        {
            let _ = route(&["-n", "delete", "-net", "0.0.0.0/1"]);
            let _ = route(&["-n", "delete", "-net", "128.0.0.0/1"]);
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
            let _ = route(&["delete", "0.0.0.0", "mask", "128.0.0.0"]);
            let _ = route(&["delete", "128.0.0.0", "mask", "128.0.0.0"]);
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
/// ощущается как зависание. `route.exe`/`netsh.exe` нативные (~50мс). tun-IP у
/// гостя фиксирован (10.7.0.2). Возвращает (шлюз, tun_idx, tun_ip).
#[cfg(target_os = "windows")]
fn windows_net_info(tun: &str) -> Option<(String, String, String)> {
    let gw = default_gateway_windows()?;
    let idx = adapter_index_windows(tun)?;
    Some((gw, idx, "10.7.0.2".to_string()))
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

// ── общий запуск команды ──
//
// Через `bmv_common::command`, а не `Command::new`: на Windows он ставит
// CREATE_NO_WINDOW, без которого каждый route/netsh мигает окном консоли.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = bmv_common::command(cmd).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod ipv6_tests {
    use super::{has_ipv6, IPV6_HALVES};
    use std::net::Ipv6Addr;

    /// «::/1» → (адрес сети, длина префикса).
    fn parse(p: &str) -> (Ipv6Addr, u32) {
        let (addr, len) = p.split_once('/').unwrap_or_else(|| panic!("префикс без длины: {p}"));
        (
            addr.parse().unwrap_or_else(|e| panic!("{p}: не IPv6-адрес ({e})")),
            len.parse().unwrap_or_else(|e| panic!("{p}: не длина префикса ({e})")),
        )
    }

    /// БЛОКИРОВКА ОБЯЗАНА ЗАКРЫВАТЬ ВСЁ АДРЕСНОЕ ПРОСТРАНСТВО IPv6 ЦЕЛИКОМ.
    ///
    /// Это ровно та проверка, которую человеку самому не сделать: дыра в
    /// покрытии выглядит как полностью рабочий VPN — просто часть сайтов
    /// (те, чьи адреса попали в незакрытую половину) видит настоящий адрес.
    /// Поэтому проверяем не «похоже на правду», а замощение отрезка
    /// [0, 2^128): первый префикс начинается с нуля, каждый следующий — сразу
    /// за предыдущим, последний кончается на максимуме. Ни щели, ни нахлёста.
    #[test]
    fn the_two_halves_tile_the_entire_ipv6_space() {
        let mut spans: Vec<(u128, u128)> = IPV6_HALVES
            .iter()
            .map(|p| {
                let (net, len) = parse(p);
                // /0 закрыл бы всё одной строкой, но проиграл бы маршруту
                // провайдера: у того тоже /0, а побеждает САМЫЙ ДЛИННЫЙ.
                assert!((1..=128).contains(&len), "{p}: длина префикса должна быть 1..=128, иначе дефолт провайдера победит");
                let first = u128::from(net);
                let size = 1u128 << (128 - len);
                assert_eq!(first % size, 0, "{p}: адрес не выровнен по своей же длине префикса");
                (first, first + (size - 1))
            })
            .collect();
        spans.sort_unstable();

        assert_eq!(spans.first().expect("список половин пуст").0, 0, "начало IPv6 не закрыто");
        for w in spans.windows(2) {
            assert_eq!(w[1].0, w[0].1 + 1, "между {:?} и {:?} щель или нахлёст", w[0], w[1]);
        }
        assert_eq!(spans.last().expect("список половин пуст").1, u128::MAX, "конец IPv6 не закрыт");

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

