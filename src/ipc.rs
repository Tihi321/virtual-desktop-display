//! Communication between the settings process and the tray process.
//!
//! * Settings -> tray: registered window messages posted to the tray window
//!   (class `VirtualDesktopDisplayTray`).
//! * Tray -> settings: `%APPDATA%\virtual-desktop-display\status.json`, written
//!   on every status change.
use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, RegisterWindowMessageW};

/// Window class of the tray's hidden window.
pub const TRAY_CLASS: PCWSTR = w!("VirtualDesktopDisplayTray");
/// Title of the settings window (also used to find an existing one).
pub const SETTINGS_TITLE: &str = "Virtual Desktop Display - Settings";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg {
    Reload,
    StartAll,
    StopAll,
    OpenSettings,
}

impl Msg {
    fn name(self) -> PCWSTR {
        match self {
            Msg::Reload => w!("VirtualDesktopDisplay.Reload"),
            Msg::StartAll => w!("VirtualDesktopDisplay.StartAll"),
            Msg::StopAll => w!("VirtualDesktopDisplay.StopAll"),
            Msg::OpenSettings => w!("VirtualDesktopDisplay.OpenSettings"),
        }
    }

    /// The system-wide message id (0 on failure).
    pub fn id(self) -> u32 {
        unsafe { RegisterWindowMessageW(self.name()) }
    }
}

/// The tray window, if a tray instance is running.
pub fn find_tray() -> Option<HWND> {
    unsafe { FindWindowW(TRAY_CLASS, PCWSTR::null()).ok() }
}

/// Post `msg` to the running tray. False if no tray is running.
pub fn post_to_tray(msg: Msg) -> bool {
    let Some(h) = find_tray() else { return false };
    unsafe { PostMessageW(Some(h), msg.id(), WPARAM(0), LPARAM(0)).is_ok() }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SplitStatus {
    pub index: usize,
    pub name: String,
    pub running: bool,
    pub display_present: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyStatus {
    pub action: String,
    pub keys: String,
    pub enabled: bool,
    pub registered: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DriverInfo {
    pub installed: bool,
    pub version: Option<u32>,
}

/// Contents of status.json.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusFile {
    pub running: bool,
    /// idle | starting | running | stopping | error
    pub state: String,
    pub message: String,
    pub splits: Vec<SplitStatus>,
    pub hotkeys: Vec<HotkeyStatus>,
    pub driver: DriverInfo,
    /// Unix seconds.
    pub updated_at: u64,
}

pub fn status_path() -> Option<PathBuf> {
    Config::dir().map(|d| d.join("status.json"))
}

/// Write status.json (atomically: temp file + rename).
pub fn write_status(s: &StatusFile) -> anyhow::Result<()> {
    let p = status_path().ok_or_else(|| anyhow::anyhow!("%APPDATA% not set"))?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// Read status.json (None if missing or unreadable).
pub fn read_status() -> Option<StatusFile> {
    let b = std::fs::read(status_path()?).ok()?;
    serde_json::from_slice(&b).ok()
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_roundtrip_and_tolerance() {
        let s = StatusFile {
            running: true,
            state: "running".into(),
            message: "ok".into(),
            splits: vec![SplitStatus {
                index: 0,
                name: "A".into(),
                running: true,
                display_present: true,
            }],
            hotkeys: vec![HotkeyStatus {
                action: "toggle_all".into(),
                keys: "Ctrl+Alt+S".into(),
                enabled: true,
                registered: false,
                error: Some("in use by another app".into()),
            }],
            driver: DriverInfo {
                installed: true,
                version: Some(1),
            },
            updated_at: 5,
        };
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<StatusFile>(&j).unwrap(), s);
        // unknown / missing fields do not break reading
        let p: StatusFile = serde_json::from_str(r#"{"state":"idle","extra":1}"#).unwrap();
        assert_eq!(p.state, "idle");
        assert!(!p.running);
    }
}
