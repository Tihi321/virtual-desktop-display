//! Tray application: a hidden top-level window owns the notification icon,
//! the context menu, the hotkeys, the IPC messages from the settings process
//! and the system notifications (display / device change, power).
use crate::autosplit::{self, Action as AutoAction, AutoState};
use crate::autostart;
use crate::capture::init_winrt;
use crate::config::Config;
use crate::driver;
use crate::hotkeys::{self, Registration};
use crate::ipc::{self, Msg, StatusFile};
use crate::session_worker::{Command, SessionWorker, Status, WM_WORKER_STATUS};
use anyhow::{anyhow, Result};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, LoadIconW, PostMessageW,
    PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow,
    SetMenuDefaultItem, SetTimer, TrackPopupMenu, TranslateMessage, HICON, IDI_APPLICATION,
    MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON,
    WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_DEVICECHANGE, WM_DISPLAYCHANGE,
    WM_ENDSESSION, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_NULL, WM_POWERBROADCAST, WM_RBUTTONUP, WM_TIMER,
    WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

const WM_TRAY: u32 = WM_APP + 1;
const WM_START_INITIAL: u32 = WM_APP + 2;
const WM_OPEN_SETTINGS: u32 = WM_APP + 3;

const ID_SETTINGS: usize = 1001;
const ID_TOGGLE: usize = 1002;
const ID_CYCLE: usize = 1003;
const ID_AUTOSTART: usize = 1004;
const ID_EXIT: usize = 1005;
/// Per-split items: `ID_SPLIT_BASE + config index`.
const ID_SPLIT_BASE: usize = 2000;

/// Debounce for display / device change bursts (also re-armed while the worker is busy).
const TIMER_DISPLAY: usize = 1;
const TIMER_RESUME: usize = 2;
const DEBOUNCE_MS: u32 = 1500;

const DBT_DEVNODES_CHANGED: usize = 0x0007;
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
    /// Registered window message ids (Reload, StartAll, StopAll, OpenSettings).
    msgs: [Cell<u32>; 4],
    auto: RefCell<AutoState>,
    /// Per split: its display is connected (last evaluation).
    present: RefCell<Vec<bool>>,
    hotkeys: RefCell<Vec<Registration>>,
    driver: Cell<driver::DriverStatus>,
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

// ---- status.json -------------------------------------------------------------

fn write_status(app: &App) {
    let st = status(app);
    let cfg = app.cfg.borrow();
    let present = app.present.borrow();
    let running_set: Vec<usize> = match &st {
        Status::Running { splits } => splits.clone(),
        _ => Vec::new(),
    };
    let (state, message) = match &st {
        Status::Idle => ("idle", String::new()),
        Status::Starting => ("starting", String::new()),
        Status::Running { splits } => ("running", format!("{} split(s) running", splits.len())),
        Status::Stopping => ("stopping", String::new()),
        Status::Error(m) => ("error", m.clone()),
    };
    let d = app.driver.get();
    let s = StatusFile {
        running: matches!(st, Status::Running { .. }),
        state: state.into(),
        message,
        splits: cfg
            .splits
            .iter()
            .enumerate()
            .map(|(i, sc)| ipc::SplitStatus {
                index: i,
                name: sc.name.clone(),
                running: running_set.contains(&i),
                display_present: present.get(i).copied().unwrap_or(false),
            })
            .collect(),
        hotkeys: app
            .hotkeys
            .borrow()
            .iter()
            .map(|r| ipc::HotkeyStatus {
                action: r.action.key().into(),
                keys: r.keys.clone(),
                enabled: r.enabled,
                registered: r.registered,
                error: r.error.clone(),
            })
            .collect(),
        driver: ipc::DriverInfo {
            installed: d.installed,
            version: d.version,
        },
        updated_at: ipc::now_unix(),
    };
    if let Err(e) = ipc::write_status(&s) {
        tracing::warn!("could not write status.json: {e:#}");
    }
}

/// Re-probe the driver, but never while a session may hold or be opening it.
fn refresh_driver(app: &App) {
    if matches!(status(app), Status::Idle | Status::Error(_)) {
        app.driver.set(driver::status());
    }
}

fn refresh_presence(app: &App) -> Vec<bool> {
    let p = autosplit::current_presence(&app.cfg.borrow());
    *app.present.borrow_mut() = p.clone();
    p
}

// ---- actions -----------------------------------------------------------------

/// Start (or restart) with the splits the auto state wants running.
fn apply_desired(app: &App) {
    let cfg = app.cfg.borrow().clone();
    let present = refresh_presence(app);
    let desired = app.auto.borrow().desired(&cfg, &present);
    if desired.is_empty() {
        if is_active(app) {
            send(app, Command::Stop);
        }
        return;
    }
    send(
        app,
        Command::Start(Box::new(autosplit::config_for(&cfg, &desired))),
    );
}

/// Explicit start: all enabled splits whose display is connected.
fn start_all(app: &App) {
    if is_active(app) {
        return;
    }
    let cfg = app.cfg.borrow().clone();
    app.auto.borrow_mut().arm_all(&cfg);
    refresh_presence(app);
    // The worker resolves the enabled splits itself and reports a clear
    // error if none of the displays is connected.
    send(app, Command::Start(Box::new(cfg)));
}

fn stop_all(app: &App) {
    // A manual stop pauses auto-start until the displays are replugged.
    app.auto.borrow_mut().disarm_all();
    send(app, Command::Stop);
}

fn toggle_all(app: &App) {
    if is_active(app) {
        stop_all(app);
    } else {
        start_all(app);
    }
}

/// Stop the session and end the worker thread, waiting for it (exit paths).
fn shutdown_worker(app: &App) {
    let w = app.worker.borrow_mut().take();
    if let Some(w) = w {
        w.shutdown();
    }
    hotkeys::unregister_all(app.hwnd.get());
}

fn open_settings() {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("cannot find own exe: {e}");
            return;
        }
    };
    // A second settings process just brings the existing window to the front.
    if let Err(e) = std::process::Command::new(exe).arg("--settings").spawn() {
        tracing::error!("could not launch settings: {e}");
    }
}

fn show_menu(app: &App) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let running = is_active(app);
        let check = |b: bool| if b { MF_CHECKED } else { MF_UNCHECKED };
        let settings = wide("Settings...");
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, PCWSTR(settings.as_ptr()));
        let _ = SetMenuDefaultItem(menu, ID_SETTINGS as u32, 0);
        {
            let cfg = app.cfg.borrow();
            let present = app.present.borrow();
            if !cfg.splits.is_empty() {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            }
            for (i, sc) in cfg.splits.iter().enumerate() {
                let mut label = if sc.name.trim().is_empty() {
                    format!("Split {}", i + 1)
                } else {
                    sc.name.clone()
                };
                if !present.get(i).copied().unwrap_or(false) {
                    label.push_str(" (not connected)");
                }
                let l = wide(&label);
                let _ = AppendMenuW(
                    menu,
                    MF_STRING | check(sc.enabled),
                    ID_SPLIT_BASE + i,
                    PCWSTR(l.as_ptr()),
                );
            }
        }
        let toggle = wide(if running { "Stop all" } else { "Start all" });
        let cycle = wide("Cycle pieces");
        let auto = wide("Start with Windows");
        let exit = wide("Exit");
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_TOGGLE, PCWSTR(toggle.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, ID_CYCLE, PCWSTR(cycle.as_ptr()));
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

/// Toggle `enabled` of one split from the menu, save, and apply when running.
fn toggle_split(app: &App, index: usize) {
    let now_enabled = {
        let mut cfg = app.cfg.borrow_mut();
        let Some(sc) = cfg.splits.get_mut(index) else {
            return;
        };
        sc.enabled = !sc.enabled;
        if let Err(e) = cfg.save() {
            tracing::warn!("could not save config: {e:#}");
        }
        cfg.splits[index].enabled
    };
    if now_enabled {
        app.auto.borrow_mut().arm(index);
    }
    if is_active(app) {
        apply_desired(app);
    }
    write_status(app);
}

fn register_hotkeys(app: &App) {
    let regs = hotkeys::register_all(app.hwnd.get(), &app.cfg.borrow().hotkeys);
    *app.hotkeys.borrow_mut() = regs;
}

fn reload_config(app: &App) {
    let new = Config::load();
    tracing::info!("config reloaded: {new:?}");
    if let Err(e) = autostart::set_enabled(new.autostart) {
        tracing::warn!("autostart sync failed: {e:#}");
    }
    *app.cfg.borrow_mut() = new.clone();
    register_hotkeys(app);
    let present = refresh_presence(app);
    *app.auto.borrow_mut() = AutoState::new(&new, &present, false);
    refresh_driver(app);
    // A running session restarts with the new config (all enabled splits).
    if is_active(app) {
        app.auto.borrow_mut().arm_all(&new);
        apply_desired(app);
    }
    write_status(app);
}

fn cycle_pieces(app: &App) {
    if matches!(status(app), Status::Running { .. }) {
        send(app, Command::CycleAll);
    }
}

/// React to a worker status change: tooltip, balloon on errors, persist the
/// piece orders after a cycle, status.json.
fn on_worker_status(app: &App) {
    let st = status(app);
    if *app.last_status.borrow() == st {
        return;
    }
    *app.last_status.borrow_mut() = st.clone();
    update_tip(app);
    match &st {
        Status::Error(msg) => {
            app.auto.borrow_mut().note_failure();
            balloon(app, "Split stopped", msg, true);
        }
        Status::Running { .. } => persist_orders(app),
        _ => {}
    }
    refresh_driver(app);
    write_status(app);
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

/// The debounced display / device change: auto-split decision, then the
/// health check of a running session.
fn on_display_settled(app: &App, hwnd: HWND) {
    let st = status(app);
    if matches!(st, Status::Starting | Status::Stopping) {
        // Our own layout changes; look again once the worker is done.
        unsafe {
            SetTimer(Some(hwnd), TIMER_DISPLAY, DEBOUNCE_MS, None);
        }
        return;
    }
    let cfg = app.cfg.borrow().clone();
    let present = refresh_presence(app);
    let running: Vec<usize> = match &st {
        Status::Running { splits } => splits.clone(),
        _ => Vec::new(),
    };
    let action = autosplit::decide(&mut app.auto.borrow_mut(), &running, &present, &cfg);
    match action {
        AutoAction::Nothing => check_health(app),
        AutoAction::Stop => {
            tracing::info!("auto-split: no split display left; stopping");
            send(app, Command::Stop);
            balloon(
                app,
                "Split stopped",
                "A split display was disconnected; the layout was restored.",
                false,
            );
        }
        AutoAction::Restart(set) => {
            tracing::info!("auto-split: restarting with splits {set:?}");
            send(
                app,
                Command::Start(Box::new(autosplit::config_for(&cfg, &set))),
            );
        }
    }
    write_status(app);
}

fn check_health(app: &App) {
    if matches!(status(app), Status::Running { .. }) {
        send(app, Command::CheckHealth);
    }
}

fn on_hotkey(app: &App, id: i32) {
    match hotkeys::Action::from_id(id) {
        Some(hotkeys::Action::ToggleAll) => toggle_all(app),
        Some(hotkeys::Action::CycleOrder) => cycle_pieces(app),
        Some(hotkeys::Action::OpenSettings) => open_settings(),
        None => {}
    }
}

fn on_ipc(app: &App, which: usize) {
    match which {
        0 => reload_config(app),
        1 => start_all(app),
        2 => stop_all(app),
        _ => open_settings(),
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(app) = APP.with(|a| a.borrow().clone()) else {
        return DefWindowProcW(hwnd, msg, wp, lp);
    };
    match msg {
        WM_TRAY => {
            match (lp.0 as u32) & 0xFFFF {
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    refresh_presence(&app);
                    show_menu(&app);
                }
                WM_LBUTTONDBLCLK => open_settings(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wp.0 & 0xFFFF;
            match id {
                ID_SETTINGS => open_settings(),
                ID_TOGGLE => toggle_all(&app),
                ID_CYCLE => cycle_pieces(&app),
                ID_AUTOSTART => toggle_autostart(&app),
                ID_EXIT => {
                    shutdown_worker(&app);
                    let _ = DestroyWindow(hwnd);
                }
                i if i >= ID_SPLIT_BASE => toggle_split(&app, i - ID_SPLIT_BASE),
                _ => {}
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            on_hotkey(&app, wp.0 as i32);
            LRESULT(0)
        }
        WM_START_INITIAL => {
            apply_desired(&app);
            LRESULT(0)
        }
        WM_OPEN_SETTINGS => {
            open_settings();
            LRESULT(0)
        }
        WM_WORKER_STATUS => {
            on_worker_status(&app);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            // Debounce: our own mode/layout changes and driver (un)plugging
            // produce bursts.
            SetTimer(Some(hwnd), TIMER_DISPLAY, DEBOUNCE_MS, None);
            LRESULT(0)
        }
        WM_DEVICECHANGE => {
            if wp.0 == DBT_DEVNODES_CHANGED {
                SetTimer(Some(hwnd), TIMER_DISPLAY, DEBOUNCE_MS, None);
            }
            LRESULT(1)
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_DISPLAY => {
                    let _ = KillTimer(Some(hwnd), TIMER_DISPLAY);
                    on_display_settled(&app, hwnd);
                }
                TIMER_RESUME => {
                    let _ = KillTimer(Some(hwnd), TIMER_RESUME);
                    if app.resume_restart.replace(false) {
                        tracing::info!("resume: restarting session");
                        // Queued behind the Stop sent on resume, so order is safe.
                        apply_desired(&app);
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
                    send(&app, Command::Stop);
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
        m if m != 0 && app.msgs.iter().any(|c| c.get() == m) => {
            let which = app.msgs.iter().position(|c| c.get() == m).unwrap_or(0);
            on_ipc(&app, which);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Run the tray application until Exit. `first_run`: no config file existed;
/// the settings window opens right away.
pub fn run(cfg: Config, first_run: bool) -> Result<()> {
    init_winrt();
    let start_on_launch = cfg.start_on_launch;
    let autostart_wanted = cfg.autostart;
    let present = autosplit::current_presence(&cfg);
    let auto = AutoState::new(&cfg, &present, start_on_launch);
    let app = Rc::new(App {
        hwnd: Cell::new(HWND::default()),
        auto: RefCell::new(auto),
        present: RefCell::new(present),
        cfg: RefCell::new(cfg),
        worker: RefCell::new(None),
        last_status: RefCell::new(Status::Idle),
        resume_restart: Cell::new(false),
        taskbar_created: Cell::new(0),
        msgs: Default::default(),
        hotkeys: RefCell::new(Vec::new()),
        driver: Cell::new(driver::DriverStatus {
            installed: false,
            version: None,
            adapter_present: false,
        }),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));

    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: ipc::TRAY_CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(anyhow!("RegisterClassExW failed"));
        }
        app.taskbar_created
            .set(RegisterWindowMessageW(w!("TaskbarCreated")));
        for (c, m) in
            app.msgs
                .iter()
                .zip([Msg::Reload, Msg::StartAll, Msg::StopAll, Msg::OpenSettings])
        {
            c.set(m.id());
        }
        // Top-level (not message-only) so it receives broadcast messages.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            ipc::TRAY_CLASS,
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
        register_hotkeys(&app);
        app.driver.set(driver::status());
        write_status(&app);
        // Only ensure the entry exists; never delete it implicitly at startup.
        if autostart_wanted {
            if let Err(e) = autostart::set_enabled(true) {
                tracing::warn!("autostart sync failed: {e:#}");
            }
        }
        if first_run {
            let _ = PostMessageW(Some(hwnd), WM_OPEN_SETTINGS, WPARAM(0), LPARAM(0));
        }
        if start_on_launch {
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
