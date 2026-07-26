//! Десктопный роутинг VPN-гостя: создать TUN и завернуть ВЕСЬ трафик в туннель,
//! а на Drop — откатить. Один код для CLI, GUI и привилегированного хелпера GUI.
//! Android/iOS сюда не ходят — там свой fd из платформенного шелла.
//!
//! Настоящий VPN, а не прокси: split-default (`0.0.0.0/1` + `128.0.0.0/1`)
//! перекрывает дефолтный маршрут НЕ удаляя его; сам хост пинуется через реальный
//! шлюз (иначе шифрованные пакеты зациклятся). `RouteGuard::drop` всё откатывает.
//! Реализовано для Linux и macOS (обе требуют root); Windows — задел.

use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use bmv_tunnel::TunParams;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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
    Ok((TunDevice { inner: dev }, name))
}

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
}

#[cfg(target_os = "macos")]
impl AsyncRead for TunDevice {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        // Читаем во временный буфер, снимаем 4-байтовый AF-заголовок.
        let mut tmp = [0u8; 65540];
        let mut rb = ReadBuf::new(&mut tmp);
        match Pin::new(&mut self.inner).poll_read(cx, &mut rb) {
            Poll::Ready(Ok(())) => {
                let filled = rb.filled();
                if filled.len() > 4 {
                    buf.put_slice(&filled[4..]);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
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
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<_> = std::env::split_paths(&path).collect();
    paths.insert(0, base);
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
    }
    Ok(())
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
}

impl RouteGuard {
    // ── Linux ──
    #[cfg(target_os = "linux")]
    pub fn install(host_ip: IpAddr, tun: &str) -> Result<Self, String> {
        let (gw, dev) = default_route_linux().ok_or("не найден шлюз по умолчанию")?;
        let hip = host_ip.to_string();
        let _ = ip(&["route", "add", &format!("{hip}/32"), "via", &gw, "dev", &dev]);
        ip(&["route", "add", "0.0.0.0/1", "dev", tun])?;
        ip(&["route", "add", "128.0.0.0/1", "dev", tun])?;
        let resolv = std::fs::read("/etc/resolv.conf").ok();
        let _ = std::fs::write("/etc/resolv.conf", "nameserver 8.8.8.8\n");
        Ok(Self { host_ip: hip, resolv })
    }

    // ── macOS ──
    // route(8) split-default в utun + пин хоста через реальный шлюз (анти-петля).
    // DNS переставляем на 8.8.8.8 у активного сетевого сервиса (иначе резолв ушёл
    // бы к LAN-DNS мимо туннеля). Всё откатывается в Drop.
    #[cfg(target_os = "macos")]
    pub fn install(host_ip: IpAddr, tun: &str) -> Result<Self, String> {
        let (gw, dev) = default_route_macos().ok_or("не найден шлюз по умолчанию")?;
        let hip = host_ip.to_string();
        // Хост-пин через реальный шлюз, чтобы шифрованные UDP не зациклились в туннель.
        let _ = route(&["-n", "add", "-host", &hip, &gw]);
        // Split-default в utun (перекрывает дефолт, не удаляя его).
        route(&["-n", "add", "-net", "0.0.0.0/1", "-interface", tun])?;
        route(&["-n", "add", "-net", "128.0.0.0/1", "-interface", tun])?;
        // DNS → 8.8.8.8 (через туннель). Запоминаем сервис и старые серверы.
        let (dns_service, dns_old) = match dns_service_for_dev(&dev) {
            Some(svc) => {
                let old = current_dns(&svc);
                let _ = networksetup(&["-setdnsservers", &svc, "8.8.8.8"]);
                (Some(svc), old)
            }
            None => (None, Vec::new()),
        };
        Ok(Self { host_ip: hip, dns_service, dns_old })
    }

    // ── Windows ──
    // route(8)-аналог: split-default через wintun-адаптер + пин хоста через реальный
    // шлюз (анти-петля). DNS переставляем на 8.8.8.8 у самого tun-адаптера, иначе
    // резолв ушёл бы к LAN-DNS мимо туннеля. Всё требует админ-прав (процесс поднят
    // с манифестом requireAdministrator — один процесс, без отдельного хелпера).
    #[cfg(target_os = "windows")]
    pub fn install(host_ip: IpAddr, tun: &str) -> Result<Self, String> {
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
        Ok(Self { host_ip: hip, tun_name: tun.to_string() })
    }
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            let _ = ip(&["route", "del", "0.0.0.0/1"]);
            let _ = ip(&["route", "del", "128.0.0.0/1"]);
            let _ = ip(&["route", "del", &format!("{}/32", self.host_ip)]);
            if let Some(r) = &self.resolv {
                let _ = std::fs::write("/etc/resolv.conf", r);
            }
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
    let out = std::process::Command::new("ip").args(["route", "show", "default"]).output().ok()?;
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
    let out = std::process::Command::new("route").args(["-n", "get", "default"]).output().ok()?;
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
    let out = std::process::Command::new("networksetup").arg("-listnetworkserviceorder").output().ok()?;
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
    let out = std::process::Command::new("networksetup").args(["-getdnsservers", service]).output();
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

/// Захват stdout нативной команды (без окна консоли).
#[cfg(target_os = "windows")]
fn cmd_out(cmd: &str, args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new(cmd)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
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

// CREATE_NO_WINDOW: не создавать консольное окно для дочернего процесса. Без
// него route/netsh/powershell мигают чёрным окном cmd при каждом вызове.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ── общий запуск команды ──
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let mut command = std::process::Command::new(cmd);
    command.args(args);
    // Windows: без CREATE_NO_WINDOW каждый route/netsh мигает окном cmd.
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let out = command.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
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

