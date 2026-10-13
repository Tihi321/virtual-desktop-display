//! Split a FlipGo stacked monitor into two virtual 1920x1080 displays.
pub mod autostart;
pub mod capture;
pub mod config;
pub mod cursor_portal;
pub mod displays;
pub mod renderer;
pub mod session;
pub mod tray;
pub mod vdd;
pub mod window_rescue;

/// Make the process per-monitor-v2 DPI aware (physical pixels everywhere).
pub fn set_dpi_aware() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}
