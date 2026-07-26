//! Персистентные списки (недавние хосты по координатору + история серверов) и
//! стабильные креды хоста — аналог prefs на iOS/Android. Файлы в конфиг-папке ОС.

use std::path::PathBuf;

fn dir() -> Option<PathBuf> {
    let d = directories::ProjectDirs::from("net", "BeMyVPN", "BeMyVPN")?;
    let p = d.config_dir().to_path_buf();
    std::fs::create_dir_all(&p).ok()?;
    Some(p)
}

fn read_lines(name: &str) -> Vec<String> {
    dir()
        .map(|d| d.join(name))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

fn write_lines(name: &str, list: &[String]) {
    if let Some(d) = dir() {
        let _ = std::fs::write(d.join(name), list.join("\n"));
    }
}

/// Имя файла недавних для координатора (санитизируем URL в имя).
fn recent_file(coord: &str) -> String {
    let safe: String = coord.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    format!("recent_{safe}.txt")
}

pub fn load_recent(coord: &str) -> Vec<String> {
    read_lines(&recent_file(coord))
}

pub fn add_recent(coord: &str, id: &str) -> Vec<String> {
    let mut r = load_recent(coord);
    r.retain(|x| x != id);
    r.insert(0, id.to_string());
    r.truncate(6);
    write_lines(&recent_file(coord), &r);
    r
}

pub fn load_server_history() -> Vec<String> {
    read_lines("servers.txt")
}

pub fn add_server_history(url: &str) -> Vec<String> {
    let mut h = load_server_history();
    h.retain(|x| x != url);
    h.insert(0, url.to_string());
    h.truncate(6);
    write_lines("servers.txt", &h);
    h
}

// ── стабильный код хоста между запусками (как device_host_id на мобильных) ──

fn creds_path() -> Option<PathBuf> {
    dir().map(|d| d.join("host.txt"))
}

pub fn load_host_creds() -> (String, String) {
    if let Some(p) = creds_path() {
        if let Ok(s) = std::fs::read_to_string(p) {
            let mut it = s.trim().splitn(2, '|');
            return (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
        }
    }
    (String::new(), String::new())
}

pub fn save_host_creds(id: &str, sig: &str) {
    if let Some(p) = creds_path() {
        let _ = std::fs::write(p, format!("{id}|{sig}"));
    }
}

pub fn clear_host_creds() {
    if let Some(p) = creds_path() {
        let _ = std::fs::remove_file(p);
    }
}
