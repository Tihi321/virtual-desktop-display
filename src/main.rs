#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Default: tray application. `--cli`: console mode (Ctrl+C to stop).
//! `--seconds N` (implies `--cli`): stop automatically after N seconds.
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use vdd_app::{config::Config, session::Session, set_dpi_aware, tray};
use windows::core::{w, BOOL};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Console::{
    AttachConsole, GetConsoleWindow, SetConsoleCtrlHandler, ATTACH_PARENT_PROCESS,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};

static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn ctrl_handler(_ctrl: u32) -> BOOL {
    // Ctrl+C, Ctrl+Break, console close: all request a clean stop.
    STOP.store(true, Ordering::SeqCst);
    BOOL(1)
}

/// Log to stderr and to a daily rolling file in %APPDATA%\virtual-desktop-display\logs.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    // Only log to stderr when a console is attached (GUI subsystem has none).
    let has_console = !unsafe { GetConsoleWindow() }.0.is_null();
    let stderr = has_console.then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    let (file_layer, guard) = match Config::dir() {
        Some(d) => {
            let appender = tracing_appender::rolling::daily(d.join("logs"), "vdd.log");
            let (nb, guard) = tracing_appender::non_blocking(appender);
            (
                Some(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(nb),
                ),
                Some(guard),
            )
        }
        None => (None, None),
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(file_layer)
        .init();
    guard
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let seconds = args
        .iter()
        .position(|a| a == "--seconds")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<u64>().ok());
    let cli = args.iter().any(|a| a == "--cli") || seconds.is_some();
    if cli {
        // Release builds use the GUI subsystem; attach to the parent console for output.
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
    let _log_guard = init_logging();
    set_dpi_aware(); // fallback; the manifest already requests PerMonitorV2

    // Single instance.
    let _mutex: HANDLE = unsafe { CreateMutexW(None, true, w!("Local\\VirtualDesktopDisplay"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        tracing::warn!("another instance is already running");
        unsafe {
            MessageBoxW(
                None,
                w!("Virtual Desktop Display is already running (see the tray icon)."),
                w!("Virtual Desktop Display"),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        return Ok(());
    }

    let cfg = Config::load();
    tracing::info!("config: {cfg:?}");

    if !cli {
        tracing::info!("tray mode started, version {}", env!("CARGO_PKG_VERSION"));
        return tray::run(cfg);
    }

    unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), true)? };
    let deadline = seconds.map(|n| std::time::Instant::now() + Duration::from_secs(n));
    let mut session = Session::start(&cfg)?;
    tracing::info!("running; press Ctrl+C to stop");
    while !STOP.load(Ordering::SeqCst) && deadline.is_none_or(|d| std::time::Instant::now() < d) {
        std::thread::sleep(Duration::from_millis(100));
    }
    tracing::info!("stopping");
    session.stop();
    Ok(())
}
