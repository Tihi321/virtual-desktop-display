#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Default: tray application. `--settings`: the settings window (own process).
//! `--cli`: console mode (Ctrl+C to stop).
//! `--seconds N` (implies `--cli`): stop automatically after N seconds.
//! `--register-modes "WxH@HZ,..."`: elevated helper that writes the Parsec VDD
//! custom modes to HKLM and exits (exit code 0 = success).
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use vdd_app::{
    config::Config, displays, modes, session, session::Session, set_dpi_aware, tray, ui,
};
use windows::core::{w, BOOL};
use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AttachConsole, GetStdHandle, SetConsoleCtrlHandler, SetStdHandle, ATTACH_PARENT_PROCESS,
    STD_ERROR_HANDLE,
};
use windows::Win32::System::Threading::CreateMutexW;

static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn ctrl_handler(_ctrl: u32) -> BOOL {
    // Ctrl+C, Ctrl+Break, console close: all request a clean stop.
    STOP.store(true, Ordering::SeqCst);
    BOOL(1)
}

/// Log to stderr and to a daily rolling file in %APPDATA%\virtual-desktop-display\logs.
///
/// `cli`: the stderr layer is always on (the console was attached by hand and
/// `GetConsoleWindow` is not a reliable test for a console).
fn init_logging(cli: bool) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    // The GUI-subsystem tray app has no console, so only `--cli` logs to stderr.
    let stderr = cli.then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
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

/// A GUI-subsystem process starts with null standard handles unless they were
/// redirected. After attaching to the parent console, point stderr at it so
/// the log shows up there.
fn redirect_stderr_to_console() {
    unsafe {
        let cur = GetStdHandle(STD_ERROR_HANDLE).unwrap_or_default();
        if !cur.is_invalid() && !cur.0.is_null() {
            return; // already redirected to a file/pipe by the caller
        }
        if let Ok(h) = CreateFileW(
            w!("CONOUT$"),
            GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            let _ = SetStdHandle(STD_ERROR_HANDLE, h);
        }
    }
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
        redirect_stderr_to_console();
    }
    let _log_guard = init_logging(cli);
    set_dpi_aware(); // fallback; the manifest already requests PerMonitorV2

    // Elevated helper: no tray, no mutex.
    if let Some(i) = args.iter().position(|a| a == "--register-modes") {
        let list = args.get(i + 1).map(String::as_str).unwrap_or("");
        let code = modes::register_helper(list);
        drop(_log_guard); // flush the log file
        std::process::exit(code);
    }

    // Settings window: its own process and its own single-instance mutex.
    if args.iter().any(|a| a == "--settings") {
        tracing::info!(
            "settings mode started, version {}",
            env!("CARGO_PKG_VERSION")
        );
        return ui::run_settings();
    }

    // Single instance. A second tray launch opens the settings window instead.
    let _mutex: HANDLE = unsafe { CreateMutexW(None, true, w!("Local\\VirtualDesktopDisplay"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if cli {
            tracing::error!("another instance is already running");
            return Ok(());
        }
        tracing::info!("another instance is already running; opening settings");
        return ui::run_settings();
    }
    // No config file yet: the tray opens the settings window right away.
    let first_run = Config::path().is_none_or(|p| !p.exists());

    let cfg = Config::load();
    tracing::info!("config: {cfg:?}");

    if !cli {
        tracing::info!("tray mode started, version {}", env!("CARGO_PKG_VERSION"));
        return tray::run(cfg, first_run);
    }

    unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), true)? };
    let deadline = seconds.map(|n| std::time::Instant::now() + Duration::from_secs(n));
    let targets = displays::enumerate()?;
    let resolved = session::resolve_all(&cfg, &targets);
    for r in &resolved {
        tracing::info!(
            "split {} -> {} ({} pieces)",
            r.index,
            r.physical.friendly_name,
            r.pieces_rel.len()
        );
    }
    let mut session = Session::start(&cfg, resolved)?;
    tracing::info!("running; press Ctrl+C to stop");
    while !STOP.load(Ordering::SeqCst) && deadline.is_none_or(|d| std::time::Instant::now() < d) {
        std::thread::sleep(Duration::from_millis(100));
    }
    tracing::info!("stopping");
    session.stop();
    Ok(())
}
