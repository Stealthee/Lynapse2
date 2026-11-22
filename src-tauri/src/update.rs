// Update notice: ~90 s after start and then every 12 h, ask GitHub for the latest published Lynapse release. If it is
// newer than this build and the user hasn't ignored that version, show a notification (Update now / What's new /
// Ignore this version) and an "Update available" item in the tray menu. Ignoring is remembered per version, so the
// next release is announced again. Settings: ~/.config/razerctl/update.json. Only an anonymous GET of the public
// release list is sent (no device or user data).
//
// "Update" copies the update command (cd <folder Lynapse was installed from> && ./update.sh; install.sh records the
// folder in ~/.config/razerctl/install.json) to the clipboard and says so, for pasting into a terminal.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, io::Write, os::unix::process::CommandExt, path::PathBuf, process::{Command, Stdio}, sync::Arc, time::Duration};
use zbus::zvariant::Value;

const API: &str = "https://api.github.com/repos/Stealthee/Lynapse2/releases/latest";
/// This build's version (RAZERCTL_PRETEND_VERSION=2.0.0 pretends to be older, to test the update notice).
fn current() -> String { std::env::var("RAZERCTL_PRETEND_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string()) }

#[derive(Serialize, Deserialize, Clone)]
struct Cfg {
    #[serde(default = "yes")]
    check: bool,
    #[serde(default)]
    ignored: Vec<String>,
}
fn yes() -> bool { true }
impl Default for Cfg {
    fn default() -> Self { Cfg { check: true, ignored: vec![] } }
}

#[derive(Serialize, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

static LATEST: Mutex<Option<Release>> = Mutex::new(None);
static ON_CHANGE: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

fn dir() -> PathBuf { dirs::config_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join("razerctl") }
fn load() -> Cfg { fs::read_to_string(dir().join("update.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default() }
fn save(c: &Cfg) {
    let _ = fs::create_dir_all(dir());
    let _ = fs::write(dir().join("update.json"), serde_json::to_string_pretty(c).unwrap_or_default());
}

fn parse(v: &str) -> (u64, u64, u64) {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let v = v.split(['-', '+']).next().unwrap_or("");
    let mut it = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}
fn newer(a: &str, b: &str) -> bool { parse(a) > parse(b) }

fn fetch() -> Option<Release> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "20", "-H", "Accept: application/vnd.github+json", "-A", concat!("Lynapse/", env!("CARGO_PKG_VERSION")), API])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    if j["draft"].as_bool() == Some(true) || j["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = j["tag_name"].as_str()?;
    Some(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        url: j["html_url"].as_str().unwrap_or("https://github.com/Stealthee/Lynapse2/releases/latest").to_string(),
    })
}

/// The newer release to announce, unless the user ignored it.
pub fn available() -> Option<Release> {
    let r = LATEST.lock().clone()?;
    (newer(&r.version, &current()) && !load().ignored.contains(&r.version)).then_some(r)
}

fn changed() {
    let f = ON_CHANGE.lock().clone();
    if let Some(f) = f { f() }
}

/// Ask GitHub now. notify = show the notification if there is something new to announce.
fn check(notify_user: bool) -> Option<Release> {
    let r = fetch()?;
    let before = available();
    *LATEST.lock() = Some(r);
    let now = available();
    if now != before {
        changed();
    }
    if let (true, Some(n)) = (notify_user, now.clone()) {
        if before.as_ref() != Some(&n) {
            std::thread::spawn(move || notify(&n));
        }
    }
    now
}

pub fn start(on_change: Arc<dyn Fn() + Send + Sync>) {
    *ON_CHANGE.lock() = Some(on_change);
    std::thread::spawn(|| {
        let delay = std::env::var("RAZERCTL_UPDATE_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(90);
        std::thread::sleep(Duration::from_secs(delay));
        loop {
            if load().check {
                check(true);
            }
            std::thread::sleep(Duration::from_secs(12 * 3600));
        }
    });
}

fn open_url(url: &str) {
    let _ = Command::new("xdg-open").arg(url).process_group(0).spawn();
}

fn ignore(version: &str) {
    let mut c = load();
    if !c.ignored.iter().any(|v| v == version) {
        c.ignored.push(version.to_string());
        save(&c);
    }
    changed();
}

/// Persistent notification with buttons; waits (in its own thread) for the click.
fn notify(r: &Release) {
    let Ok(conn) = zbus::blocking::Connection::session() else { return };
    let Ok(proxy) = zbus::blocking::Proxy::new(&conn, "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications") else { return };
    let Ok(signals) = proxy.receive_all_signals() else { return }; // subscribe before showing it, so no click is missed
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("desktop-entry", Value::from("razerctl"));
    hints.insert("urgency", Value::from(1u8));
    let actions = vec!["default", "What's new", "update", "Copy update command", "notes", "What's new", "ignore", "Ignore this version"];
    let body = format!("Version {} is available (you have {}).", r.version, current());
    // only one update notice at a time: close the one an earlier run left on screen
    let id_file = dir().join("update_notice_id");
    if let Some(old) = fs::read_to_string(&id_file).ok().and_then(|s| s.trim().parse::<u32>().ok()) {
        let _: zbus::Result<()> = proxy.call("CloseNotification", &(old,));
    }
    let id: u32 = match proxy.call("Notify", &("Lynapse", 0u32, "razerctl", "Lynapse update available", body.as_str(), actions, hints, 0i32)) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("update: notify failed: {e}");
            return;
        }
    };
    let _ = fs::write(&id_file, id.to_string());
    for msg in signals {
        let h = msg.header();
        match h.member().map(|m| m.as_str().to_string()).as_deref() {
            Some("ActionInvoked") => {
                let Ok((nid, key)) = msg.body().deserialize::<(u32, String)>() else { continue };
                if nid != id { continue }
                match key.as_str() {
                    "update" => {
                        let (cmd, copied) = copy_update_command();
                        let msg = if copied {
                            format!("Copied to your clipboard. Paste it into a terminal and press Enter:\n{cmd}")
                        } else {
                            format!("Couldn't reach the clipboard. Run this in a terminal:\n{cmd}")
                        };
                        simple_notify(&proxy, "Lynapse update command", &msg);
                    }
                    "ignore" => ignore(&r.version),
                    _ => open_url(&r.url),
                }
                break;
            }
            Some("NotificationClosed") => {
                if msg.body().deserialize::<(u32, u32)>().map(|(nid, _)| nid == id).unwrap_or(false) {
                    break;
                }
            }
            _ => {}
        }
    }
}

/// Folder Lynapse was installed from (written by install.sh), if it still has update.sh.
fn source_dir() -> Option<PathBuf> {
    let j: serde_json::Value = serde_json::from_str(&fs::read_to_string(dir().join("install.json")).ok()?).ok()?;
    let p = PathBuf::from(j["src"].as_str()?);
    p.join("update.sh").is_file().then_some(p)
}

fn shell_quote(s: &str) -> String { format!("'{}'", s.replace('\'', "'\\''")) }

/// The command that updates this install.
pub fn update_command() -> String {
    match source_dir() {
        Some(p) => format!("cd {} && ./update.sh", shell_quote(&p.to_string_lossy())),
        // installed before update.sh existed: fetch it first (assumes the usual ~/Lynapse2 clone)
        None => "cd ~/Lynapse2 && git pull --ff-only && ./update.sh".to_string(),
    }
}

/// Put the update command on the clipboard (Wayland: wl-copy, X11: xclip / xsel). Returns (command, copied).
pub fn copy_update_command() -> (String, bool) {
    let cmd = update_command();
    let tools: [(&str, &[&str]); 3] = [("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("xsel", &["--clipboard", "--input"])];
    for (t, args) in tools {
        let Ok(mut c) = Command::new(t).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn() else { continue };
        let ok = c.stdin.take().map(|mut i| i.write_all(cmd.as_bytes()).is_ok()).unwrap_or(false);
        if ok && c.wait().map(|s| s.success()).unwrap_or(false) {
            return (cmd, true);
        }
    }
    (cmd, false)
}

/// Tray menu "Update available": copy the command and say so.
pub fn copy_and_tell() {
    std::thread::spawn(|| {
        let (cmd, copied) = copy_update_command();
        let Ok(conn) = zbus::blocking::Connection::session() else { return };
        let Ok(proxy) = zbus::blocking::Proxy::new(&conn, "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications") else { return };
        let msg = if copied {
            format!("Copied to your clipboard. Paste it into a terminal and press Enter:\n{cmd}")
        } else {
            format!("Couldn't reach the clipboard. Run this in a terminal:\n{cmd}")
        };
        simple_notify(&proxy, "Lynapse update command", &msg);
    });
}

fn simple_notify(proxy: &zbus::blocking::Proxy, summary: &str, body: &str) {
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("desktop-entry", Value::from("razerctl"));
    let _: zbus::Result<u32> = proxy.call("Notify", &("Lynapse", 0u32, "razerctl", summary, body, Vec::<&str>::new(), hints, 15000i32));
}

#[derive(Serialize)]
pub struct Status {
    current: String,
    check: bool,
    available: Option<Release>,
    command: String,
}

#[tauri::command]
pub fn update_status() -> Status {
    Status { current: current(), check: load().check, available: available(), command: update_command() }
}

#[tauri::command]
pub fn update_set_check(on: bool) {
    let mut c = load();
    c.check = on;
    save(&c);
}

/// "Check now" from Settings: answers right away and shows the notification too.
#[tauri::command]
pub async fn update_check_now() -> Result<Status, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let r = fetch().ok_or("Couldn't reach GitHub.")?;
        // an explicit check shows a version the user ignored before
        let mut c = load();
        c.ignored.retain(|v| *v != r.version);
        save(&c);
        check(false);
        Ok(update_status())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Copy the update command; Err if no clipboard tool worked (the UI then shows the command to copy by hand).
#[tauri::command]
pub fn update_copy() -> Result<String, String> {
    let (cmd, ok) = copy_update_command();
    if ok { Ok(cmd) } else { Err(cmd) }
}

#[tauri::command]
pub fn update_ignore(version: String) { ignore(&version) }

/// "What's New" on the dashboard: the newest `n` CHANGELOG.md sections up to this version (built into the app),
/// as (version, date, text). Sections for newer or unreleased versions are skipped.
fn changelog_sections(text: &str, current: &str, n: usize) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, Vec<&str>)> = Vec::new();
    let mut keep = false;
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            let mut it = h.split_whitespace();
            let v = it.next().unwrap_or("").to_string();
            let date = it.collect::<Vec<_>>().join(" ").trim_matches(['(', ')']).to_string();
            keep = v.chars().next().is_some_and(|c| c.is_ascii_digit()) && !newer(&v, current);
            if keep { out.push((v, date, vec![])) }
            continue;
        }
        if keep { if let Some(l) = out.last_mut() { l.2.push(line) } }
    }
    out.into_iter().take(n).map(|(v, d, l)| (v, d, l.join("\n").trim().to_string())).collect()
}

#[tauri::command]
pub fn whats_new() -> serde_json::Value {
    let v = env!("CARGO_PKG_VERSION");
    let list: Vec<_> = changelog_sections(include_str!("../../CHANGELOG.md"), v, 5)
        .into_iter().map(|(version, date, text)| serde_json::json!({ "version": version, "date": date, "text": text })).collect();
    serde_json::json!({ "version": v, "releases": list })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions() {
        assert!(newer("v2.2.0", "2.1.0"));
        assert!(newer("2.10.0", "2.9.3"));
        assert!(!newer("v2.1.0", "2.1.0"));
        assert!(!newer("2.0.9", "2.1.0"));
        assert!(newer("3", "2.9.9"));
    }
    #[test]
    fn changelog() {
        let t = "# C\n\n## Unreleased\n- next\n\n## 2.2.0 (z)\n- future\n\n## 2.1.0 (x)\n### New\n- a\n\n## 2.0.0 (y)\nold\n";
        let s = changelog_sections(t, "2.1.0", 5);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].0.as_str(), s[0].1.as_str(), s[0].2.as_str()), ("2.1.0", "x", "### New\n- a"));
        assert_eq!(s[1].2, "old");
        assert_eq!(changelog_sections(t, "2.1.0", 1).len(), 1);
        assert!(!changelog_sections(include_str!("../../CHANGELOG.md"), env!("CARGO_PKG_VERSION"), 5).is_empty());
    }
}
