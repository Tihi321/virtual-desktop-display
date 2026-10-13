//! A dedicated worker thread owns the `Session` so the UI thread never blocks
//! on the 10 s+ start or the stop (WinRT is initialised there, MTA).
//!
//! The UI sends `Command`s; every status change is stored (see `status()`,
//! `orders()`) and announced through a notify callback. `for_hwnd` builds the
//! usual callback: it posts `WM_WORKER_STATUS` to a window.
use crate::capture::init_winrt;
use crate::config::Config;
use crate::displays;
use crate::session::{self, Session};
use anyhow::Result;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the window given to `SessionWorker::for_hwnd` after every status change.
pub const WM_WORKER_STATUS: u32 = WM_APP + 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    /// Starting (also used while a cycle rebuilds the pipeline).
    Starting,
    /// Running; `splits` are the config indices of the running splits.
    Running {
        splits: Vec<usize>,
    },
    Stopping,
    /// The last start/cycle failed or the session was lost; nothing is running.
    Error(String),
}

impl Status {
    /// True while a session is running or about to.
    pub fn is_active(&self) -> bool {
        matches!(self, Status::Running { .. } | Status::Starting)
    }
}

#[derive(Debug)]
pub enum Command {
    /// (Re)start with the enabled splits of this config whose displays are
    /// present; an existing session is stopped first.
    Start(Box<Config>),
    Stop,
    /// Rotate the piece order of every running split.
    CycleAll,
    /// Rotate the piece order of one split (config index).
    Cycle(usize),
    /// Stop with an error status if a required display has disappeared.
    CheckHealth,
    Shutdown,
}

struct Shared {
    status: Status,
    orders: Vec<(usize, Vec<usize>)>,
}

pub struct SessionWorker {
    tx: Sender<Command>,
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
}

impl SessionWorker {
    /// Spawn the worker; `notify` is called (on the worker thread) after each
    /// status change.
    pub fn spawn(notify: Box<dyn Fn() + Send>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Command>();
        let shared = Arc::new(Mutex::new(Shared {
            status: Status::Idle,
            orders: Vec::new(),
        }));
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("session-worker".into())
            .spawn(move || worker_loop(rx, sh, notify))?;
        Ok(Self {
            tx,
            shared,
            thread: Some(thread),
        })
    }

    /// Spawn a worker that posts `WM_WORKER_STATUS` to `hwnd` on every change.
    pub fn for_hwnd(hwnd: isize) -> Result<Self> {
        Self::spawn(Box::new(move || unsafe {
            let _ = PostMessageW(
                Some(HWND(hwnd as *mut _)),
                WM_WORKER_STATUS,
                WPARAM(0),
                LPARAM(0),
            );
        }))
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    pub fn status(&self) -> Status {
        self.shared.lock().unwrap().status.clone()
    }

    /// Piece order per running split: (config index, order).
    pub fn orders(&self) -> Vec<(usize, Vec<usize>)> {
        self.shared.lock().unwrap().orders.clone()
    }

    /// Stop the session (if any), end the thread and wait for it.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = self.tx.send(Command::Shutdown);
            let _ = t.join();
        }
    }
}

impl Drop for SessionWorker {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

fn start_session(cfg: &Config) -> Result<Session> {
    let targets = displays::enumerate()?;
    let resolved = session::resolve_all(cfg, &targets);
    Session::start(cfg, resolved)
}

fn worker_loop(
    rx: mpsc::Receiver<Command>,
    shared: Arc<Mutex<Shared>>,
    notify: Box<dyn Fn() + Send>,
) {
    init_winrt();
    let mut session: Option<Session> = None;
    let set = |status: Status, session: &Option<Session>| {
        {
            let mut g = shared.lock().unwrap();
            g.orders = session.as_ref().map(|s| s.orders()).unwrap_or_default();
            if g.status == status {
                return;
            }
            g.status = status;
        }
        notify();
    };
    let running = |s: &Session| Status::Running {
        splits: s.split_indices(),
    };

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Command::Start(cfg) => {
                if let Some(mut s) = session.take() {
                    set(Status::Stopping, &session);
                    s.stop();
                }
                set(Status::Starting, &session);
                match start_session(&cfg) {
                    Ok(s) => {
                        let st = running(&s);
                        session = Some(s);
                        set(st, &session);
                    }
                    Err(e) => {
                        tracing::error!("start failed: {e:#}");
                        set(Status::Error(format!("{e:#}")), &session);
                    }
                }
            }
            Command::Stop => {
                if let Some(mut s) = session.take() {
                    set(Status::Stopping, &session);
                    s.stop();
                }
                set(Status::Idle, &session);
            }
            Command::CycleAll | Command::Cycle(_) => {
                if session.is_none() {
                    continue;
                }
                set(Status::Starting, &session);
                let r = match (cmd, session.as_mut()) {
                    (Command::Cycle(i), Some(s)) => s.cycle_order(i),
                    (_, Some(s)) => s.cycle_all(),
                    (_, None) => Ok(()),
                };
                match r {
                    Ok(()) => {
                        let st = running(session.as_ref().unwrap());
                        set(st, &session);
                    }
                    Err(e) => {
                        // The session already stopped itself cleanly.
                        tracing::error!("cycle failed: {e:#}");
                        session = None;
                        set(
                            Status::Error(format!(
                                "Cycling the pieces failed ({e:#}); the split was stopped."
                            )),
                            &session,
                        );
                    }
                }
            }
            Command::CheckHealth => {
                let healthy = session.as_ref().map(|s| s.is_healthy());
                if healthy == Some(false) {
                    tracing::warn!(
                        "display configuration changed: a required display is gone; stopping"
                    );
                    if let Some(mut s) = session.take() {
                        set(Status::Stopping, &session);
                        s.stop();
                    }
                    set(
                        Status::Error(
                            "A split display or one of its virtual displays disappeared; the layout was restored."
                                .into(),
                        ),
                        &session,
                    );
                }
            }
            Command::Shutdown => break,
        }
    }
    if let Some(mut s) = session.take() {
        s.stop();
    }
    set(Status::Idle, &session);
}
