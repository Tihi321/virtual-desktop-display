//! The settings window (`--settings`): a separate process with its own
//! eframe / egui event loop. It edits `config.toml`, tells the running tray
//! app to reload (see `ipc`) and shows the tray's `status.json`.
pub mod app;
pub mod driver;
pub mod general;
pub mod hotkeys;
pub mod model;
pub mod preview;
pub mod splits;

use crate::ipc::SETTINGS_TITLE;
use anyhow::{anyhow, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

/// Bring an already open settings window to the front. False if none was found.
fn raise_existing() -> bool {
    let title: Vec<u16> = SETTINGS_TITLE.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let Ok(h) = FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) else {
            return false;
        };
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
        true
    }
}

/// Run the settings window until it is closed. A second instance only
/// raises the first one.
pub fn run_settings() -> Result<()> {
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\VirtualDesktopDisplay.Settings"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        tracing::info!("settings window already open; raising it");
        raise_existing();
        return Ok(());
    }
    let first_run = crate::config::Config::path().is_none_or(|p| !p.exists());
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(SETTINGS_TITLE)
            .with_inner_size([1040.0, 760.0])
            .with_min_inner_size([860.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        SETTINGS_TITLE,
        options,
        Box::new(move |_cc| Ok(Box::new(app::SettingsApp::new(first_run)))),
    )
    .map_err(|e| anyhow!("settings window failed: {e}"))
}
