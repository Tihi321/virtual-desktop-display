//! Tray application: a hidden top-level window owns the notification icon,
//! the context menu and the system notifications (display change, power).
use crate::autostart;
use crate::capture::init_winrt;
use crate::config::Config;
use crate::session_worker::{Command, SessionWorker, Status, WM_WORKER_STATUS};
use anyhow::{anyhow, Result};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO,
    NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, LoadIconW, PostMessageW,
    PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow, SetTimer,
    TrackPopupMenu, TranslateMessage, HICON, IDI_APPLICATION, MF_CHECKED, MF_SEPARATOR, MF_STRING,
    MF_UNCHECKED, MSG, SW_SHOWNORMAL, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND,
    WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_ENDSESSION, WM_LBUTTONDBLCLK, WM_NULL,
    WM_POWERBROADCAST, WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

const WM_TRAY: u32 = WM_APP + 1;
const WM_START_INITIAL: u32 = WM_APP + 2;

const ID_TOGGLE: usize = 1001;
const ID_CYCLE: usize = 1002;
const ID_OPEN_CONFIG: usize = 1003;
const ID_RELOAD: usize = 1004;
const ID_AUTOSTART: usize = 1005;
const ID_EXIT: usize = 1006;

const TIMER_DISPLAY: usize = 1;
const TIMER_RESUME: usize = 2;

const PBT_APMSUSPEND: usize = 0x4;
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;
const PBT_APMRESUMESUSPEND: usize = 0x7;

struct App {
    hwnd: Cell<HWND>,
    cfg: RefCell<Config>,
    /// Owns the session; all start/stop work happens on its thread.
    worker: RefCell<Option<SessionWorker>>,
    /// Last worker status the UI reacted to.
    last_status: RefCell<Status>,
    /// The session was running when the machine went to sleep.
    resume_restart: Cell<bool>,
    taskbar_created: Cell<u32>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let n = dst.len() - 1;
    for (i, c) in s.encode_utf16().take(n).enumerate() {
        dst[i] = c;
    }
}

fn load_icon() -> HICON {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        // Resource id 1 (res/app.rc); fall back to the stock icon.
        LoadIconW(
            Some(hinst.into()),
            PCWSTR(std::ptr::without_provenance::<u16>(1)),
        )
        .or_else(|_| LoadIconW(None, IDI_APPLICATION))
        .unwrap_or_default()
    }
}

fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    }
}

fn tip_text(status: &Status) -> String {
    match status {
        Status::Running { splits } => {
            format!("Virtual Desktop Display - {} split(s) active", splits.len())
        }
        Status::Starting => "Virtual Desktop Display - starting...".into(),
        Status::Stopping => "Virtual Desktop Display - stopping...".into(),
        _ => "Virtual Desktop Display - stopped".into(),
    }
}

fn status(app: &App) -> Status {
    app.worker
        .borrow()
        .as_ref()
        .map(|w| w.status())
        .unwrap_or(Status::Idle)
}

fn is_active(app: &App) -> bool {
    status(app).is_active()
}

fn send(app: &App, cmd: Command) {
    if let Some(w) = app.worker.borrow().as_ref() {
        w.send(cmd);
    }
}

fn add_tray_icon(app: &App) {
    let mut d = nid(app.hwnd.get());
    d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    d.uCallbackMessage = WM_TRAY;
    d.hIcon = load_icon();
    copy_wide(&mut d.szTip, &tip_text(&status(app)));
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &d);
    }
}

fn update_tip(app: &App) {
    let mut d = nid(app.hwnd.get());
    d.uFlags = NIF_TIP;
    copy_wide(&mut d.szTip, &tip_text(&status(app)));
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

fn balloon(app: &App, title: &str, text: &str, warning: bool) {
    let mut d = nid(app.hwnd.get());
    d.uFlags = NIF_INFO;
    d.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
    copy_wide(&mut d.szInfoTitle, title);
    copy_wide(&mut d.szInfo, text);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

fn start_session(app: &App) {
    if is_active(app) {
        return;
    }
    let cfg = app.cfg.borrow().clone();
    send(app, Command::Start(Box::new(cfg)));
}

fn stop_session(app: &App) {
    send(app, Command::Stop);
}

/// Stop the session and end the worker thread, waiting for it (exit paths).
fn shutdown_worker(app: &App) {
    let w = app.worker.borrow_mut().take();
    if let Some(w) = w {
        w.shutdown();
    }
}

fn show_menu(app: &App) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let running = is_active(app);
        let check = |b: bool| if b { MF_CHECKED } else { MF_UNCHECKED };
        let toggle = wide(if running {
            "Stop splits"
        } else {
            "Start splits"
        });
        let cycle = wide("Cycle pieces");
        let open = wide("Open config");
        let reload = wide("Reload config");
        let auto = wide("Start with Windows");
        let exit = wide("Exit");
        let _ = AppendMenuW(menu, MF_STRING, ID_TOGGLE, PCWSTR(toggle.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, ID_CYCLE, PCWSTR(cycle.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_CONFIG, PCWSTR(open.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, ID_RELOAD, PCWSTR(reload.as_ptr()));
        let _ = AppendMenuW(
            menu,
            MF_STRING | check(autostart::is_enabled()),
            ID_AUTOSTART,
            PCWSTR(auto.as_ptr()),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, PCWSTR(exit.as_ptr()));

        let hwnd = app.hwnd.get();
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

fn open_config(app: &App) {
    let Some(path) = Config::path() else { return };
    if !path.exists() {
        if let Err(e) = app.cfg.borrow().save() {
            tracing::error!("could not create config: {e:#}");
            return;
        }
    }
    let arg = wide(&format!("\"{}\"", path.display()));
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            w!("notepad.exe"),
            PCWSTR(arg.as_ptr()),
            None,
            SW_SHOWNORMAL,
        );
    }
}

fn reload_config(app: &App) {
    let new = Config::load();
    tracing::info!("config reloaded: {new:?}");
    if let Err(e) = autostart::set_enabled(new.autostart) {
        tracing::warn!("autostart sync failed: {e:#}");
    }
    *app.cfg.borrow_mut() = new.clone();
    // The worker restarts a running session with the new config.
    if is_active(app) {
        send(app, Command::Start(Box::new(new)));
    }
}

fn cycle_pieces(app: &App) {
    if matches!(status(app), Status::Running { .. }) {
        send(app, Command::CycleAll);
    }
}

/// React to a worker status change: tooltip, balloon on errors, and persist
/// the piece orders after a cycle.
fn on_worker_status(app: &App) {
    let st = status(app);
    if *app.last_status.borrow() == st {
        return;
    }
    *app.last_status.borrow_mut() = st.clone();
    update_tip(app);
    match st {
        Status::Error(msg) => balloon(app, "Split stopped", &msg, true),
        Status::Running { .. } => persist_orders(app),
        _ => {}
    }
}

/// Save the worker's current piece orders into the config if they changed.
fn persist_orders(app: &App) {
    let orders = app
        .worker
        .borrow()
        .as_ref()
        .map(|w| w.orders())
        .unwrap_or_default();
    let mut cfg = app.cfg.borrow_mut();
    let mut changed = false;
    for (i, o) in orders {
        if let Some(sc) = cfg.splits.get_mut(i) {
            if sc.order != o {
                sc.order = o;
                changed = true;
            }
        }
    }
    if changed {
        if let Err(e) = cfg.save() {
            tracing::warn!("could not save config: {e:#}");
        }
    }
}

fn toggle_autostart(app: &App) {
    let new = !autostart::is_enabled();
    match autostart::set_enabled(new) {
        Ok(()) => {
            let mut cfg = app.cfg.borrow_mut();
            cfg.autostart = new;
            if let Err(e) = cfg.save() {
                tracing::warn!("could not save config: {e:#}");
            }
        }
        Err(e) => tracing::error!("autostart change failed: {e:#}"),
    }
}

fn check_health(app: &App) {
    if matches!(status(app), Status::Running { .. }) {
        send(app, Command::CheckHealth);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(app) = APP.with(|a| a.borrow().clone()) else {
        return DefWindowProcW(hwnd, msg, wp, lp);
    };
    match msg {
        WM_TRAY => {
            match (lp.0 as u32) & 0xFFFF {
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(&app),
                WM_LBUTTONDBLCLK => {
                    if is_active(&app) {
                        stop_session(&app);
                    } else {
                        start_session(&app);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match wp.0 & 0xFFFF {
                ID_TOGGLE => {
                    if is_active(&app) {
                        stop_session(&app);
                    } else {
                        start_session(&app);
                    }
                }
                ID_CYCLE => cycle_pieces(&app),
                ID_OPEN_CONFIG => open_config(&app),
                ID_RELOAD => reload_config(&app),
                ID_AUTOSTART => toggle_autostart(&app),
                ID_EXIT => {
                    shutdown_worker(&app);
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_START_INITIAL => {
            start_session(&app);
            LRESULT(0)
        }
        WM_WORKER_STATUS => {
            on_worker_status(&app);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            // Debounce: our own mode/layout changes and driver (un)plugging
            // produce bursts. The check validates the actual target set.
            SetTimer(Some(hwnd), TIMER_DISPLAY, 1500, None);
            LRESULT(0)
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_DISPLAY => {
                    let _ = KillTimer(Some(hwnd), TIMER_DISPLAY);
                    check_health(&app);
                }
                TIMER_RESUME => {
                    let _ = KillTimer(Some(hwnd), TIMER_RESUME);
                    if app.resume_restart.replace(false) {
                        tracing::info!("resume: restarting session");
                        // Queued behind the Stop sent on resume, so order is safe.
                        let cfg = app.cfg.borrow().clone();
                        send(&app, Command::Start(Box::new(cfg)));
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            match wp.0 {
                PBT_APMSUSPEND => {
                    if is_active(&app) {
                        tracing::info!("suspend: session will be restarted on resume");
                        app.resume_restart.set(true);
                    }
                }
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND
                    if app.resume_restart.get() && is_active(&app) =>
                {
                    // The virtual displays vanished while the keep-alive was
                    // frozen; tear the stale session down, then start fresh.
                    tracing::info!("resume: dropping stale session");
                    stop_session(&app);
                    SetTimer(Some(hwnd), TIMER_RESUME, 4000, None);
                }
                _ => {}
            }
            LRESULT(1)
        }
        WM_ENDSESSION => {
            if wp.0 != 0 {
                shutdown_worker(&app);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let d = nid(hwnd);
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
            PostQuitMessage(0);
            LRESULT(0)
        }
        m if m == app.taskbar_created.get() && m != 0 => {
            add_tray_icon(&app);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Run the tray application until Exit.
pub fn run(cfg: Config) -> Result<()> {
    init_winrt();
    let start_on_launch = cfg.start_on_launch;
    let has_splits = !cfg.splits.is_empty();
    let autostart_wanted = cfg.autostart;
    let app = Rc::new(App {
        hwnd: Cell::new(HWND::default()),
        cfg: RefCell::new(cfg),
        worker: RefCell::new(None),
        last_status: RefCell::new(Status::Idle),
        resume_restart: Cell::new(false),
        taskbar_created: Cell::new(0),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));

    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("VirtualDesktopDisplayTray");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(anyhow!("RegisterClassExW failed"));
        }
        app.taskbar_created
            .set(RegisterWindowMessageW(w!("TaskbarCreated")));
        // Top-level (not message-only) so it receives broadcast messages.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class,
            w!("Virtual Desktop Display"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        app.hwnd.set(hwnd);
        *app.worker.borrow_mut() = Some(SessionWorker::for_hwnd(hwnd.0 as isize)?);
        add_tray_icon(&app);
        // Only ensure the entry exists; never delete it implicitly at startup.
        if autostart_wanted {
            if let Err(e) = autostart::set_enabled(true) {
                tracing::warn!("autostart sync failed: {e:#}");
            }
        }
        if start_on_launch && has_splits {
            let _ = PostMessageW(Some(hwnd), WM_START_INITIAL, WPARAM(0), LPARAM(0));
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // Safety net if the loop ended without Exit.
    shutdown_worker(&app);
    APP.with(|a| *a.borrow_mut() = None);
    Ok(())
}
