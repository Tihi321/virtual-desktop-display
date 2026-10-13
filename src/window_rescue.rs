//! Window rescue: any normal top-level window that shows up on (or is dragged
//! onto) a physical display hidden behind our renderer is moved to the virtual
//! display of the piece under the window centre (piece 0 as a fallback),
//! keeping its offset inside the piece and clamping its size to fit.
use crate::displays::Rect;
use anyhow::{anyhow, Result};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONULL};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, GetAncestor, GetClassNameW, GetMessageW, GetWindowLongPtrW,
    GetWindowRect, IsIconic, IsWindowVisible, IsZoomed, PostThreadMessageW, SetWindowPos,
    ShowWindow, TranslateMessage, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MOVESIZEEND, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, MSG, OBJID_WINDOW, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_MAXIMIZE, SW_RESTORE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_QUIT,
    WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// One hidden physical display and the virtual displays standing in for it.
#[derive(Debug, Clone)]
pub struct RescueSplit {
    pub physical_hmon: isize,
    /// Desktop rect of the hidden physical display.
    pub physical: Rect,
    /// Per piece: (source rect relative to `physical`, desktop rect of the
    /// virtual display showing it).
    pub pieces: Vec<(Rect, Rect)>,
}

struct State {
    splits: Vec<RescueSplit>,
    /// Renderer windows (also skipped by SKIPOWNPROCESS, but be explicit).
    own_hwnds: Vec<isize>,
}

/// The piece under the window centre (relative to `physical`), else piece 0.
/// Returns (source rect in desktop coordinates, virtual target rect).
pub fn pick_piece(split: &RescueSplit, win: Rect) -> Option<(Rect, Rect)> {
    let cx = win.x + win.w / 2 - split.physical.x;
    let cy = win.y + win.h / 2 - split.physical.y;
    let (src, virt) = split
        .pieces
        .iter()
        .find(|(s, _)| cx >= s.x && cx < s.right() && cy >= s.y && cy < s.bottom())
        .or(split.pieces.first())?;
    Some((
        Rect::new(
            split.physical.x + src.x,
            split.physical.y + src.y,
            src.w,
            src.h,
        ),
        *virt,
    ))
}

static STATE: RwLock<Option<Arc<State>>> = RwLock::new(None);

/// Pure placement: keep the window's offset relative to the physical display,
/// clamp its size to the target, then clamp its position inside the target.
pub fn rescue_rect(physical: Rect, target: Rect, win: Rect) -> Rect {
    let w = win.w.min(target.w).max(1);
    let h = win.h.min(target.h).max(1);
    let x = (target.x + (win.x - physical.x)).clamp(target.x, (target.right() - w).max(target.x));
    let y = (target.y + (win.y - physical.y)).clamp(target.y, (target.bottom() - h).max(target.y));
    Rect { x, y, w, h }
}

fn is_ignored_class(hwnd: HWND) -> bool {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    let class = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    matches!(
        class.as_str(),
        "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "#32768"
            | "VirtualDesktopDisplayRenderer"
    )
}

fn maybe_rescue(st: &State, hwnd: HWND) {
    unsafe {
        if hwnd.0.is_null()
            || st.own_hwnds.contains(&(hwnd.0 as isize))
            || !IsWindowVisible(hwnd).as_bool()
            || IsIconic(hwnd).as_bool()
            || GetAncestor(hwnd, GA_ROOT) != hwnd
        {
            return;
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if style & WS_CHILD.0 != 0 || ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
            return;
        }
        if is_ignored_class(hwnd) {
            return;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return;
        }
        let mon: HMONITOR = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
        let Some(split) = st.splits.iter().find(|s| s.physical_hmon == mon.0 as isize) else {
            return;
        };

        let maximized = IsZoomed(hwnd).as_bool();
        if maximized {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return;
        }
        let win = Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top);
        let Some((src, target)) = pick_piece(split, win) else {
            return;
        };
        let n = rescue_rect(src, target, win);
        tracing::info!("window rescue: {hwnd:?} {win:?} -> {n:?} (maximized={maximized})");
        let _ = SetWindowPos(
            hwnd,
            None,
            n.x,
            n.y,
            n.w,
            n.h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        if maximized {
            let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        }
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != OBJID_WINDOW.0 || id_child != 0 {
        return;
    }
    let st = match STATE.read() {
        Ok(g) => g.clone(),
        Err(_) => None,
    };
    if let Some(st) = st {
        maybe_rescue(&st, hwnd);
    }
}

unsafe extern "system" fn enum_proc(hwnd: HWND, _lp: LPARAM) -> BOOL {
    let st = STATE.read().ok().and_then(|g| g.clone());
    if let Some(st) = st {
        maybe_rescue(&st, hwnd);
    }
    BOOL(1)
}

pub struct WindowRescue {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl WindowRescue {
    pub fn start(splits: Vec<RescueSplit>, own_hwnds: Vec<isize>) -> Result<Self> {
        *STATE.write().unwrap() = Some(Arc::new(State { splits, own_hwnds }));
        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let thread = std::thread::Builder::new()
            .name("window-rescue".into())
            .spawn(move || unsafe {
                let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
                let mut hooks = Vec::new();
                for ev in [
                    EVENT_OBJECT_SHOW,
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_MOVESIZEEND,
                ] {
                    let h = SetWinEventHook(ev, ev, None, Some(win_event_proc), 0, 0, flags);
                    if h.is_invalid() {
                        let _ = tx.send(Err(format!("SetWinEventHook({ev:#x}) failed")));
                        for h in hooks {
                            let _ = UnhookWinEvent(h);
                        }
                        return;
                    }
                    hooks.push(h);
                }
                let _ = tx.send(Ok(GetCurrentThreadId()));
                // Initial sweep for windows already on a hidden display.
                let _ = EnumWindows(Some(enum_proc), LPARAM(0));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                for h in hooks {
                    let _ = UnhookWinEvent(h);
                }
            })?;
        match rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                *STATE.write().unwrap() = None;
                Err(anyhow!(e))
            }
            Err(_) => {
                *STATE.write().unwrap() = None;
                Err(anyhow!("window rescue thread died"))
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = t.join();
            *STATE.write().unwrap() = None;
        }
    }
}

impl Drop for WindowRescue {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHYS: Rect = Rect {
        x: 1520,
        y: 1440,
        w: 1920,
        h: 2160,
    };
    const TOP: Rect = Rect {
        x: -1920,
        y: 0,
        w: 1920,
        h: 1080,
    };

    #[test]
    fn keeps_relative_offset() {
        let r = rescue_rect(PHYS, TOP, Rect::new(1620, 1540, 800, 600));
        assert_eq!(r, Rect::new(-1820, 100, 800, 600));
    }

    #[test]
    fn clamps_size_and_position() {
        // too large: shrunk to target
        let r = rescue_rect(PHYS, TOP, Rect::new(1520, 1440, 1920, 2160));
        assert_eq!(r, TOP);
        // lower-half window: pushed up into V-top
        let r = rescue_rect(PHYS, TOP, Rect::new(2000, 3000, 500, 400));
        assert_eq!(r, Rect::new(-1440, 680, 500, 400));
    }

    #[test]
    fn picks_piece_under_window_centre() {
        let split = RescueSplit {
            physical_hmon: 1,
            physical: PHYS,
            pieces: vec![
                (Rect::new(0, 0, 1920, 1080), TOP),
                (
                    Rect::new(0, 1080, 1920, 1080),
                    Rect::new(-1920, 1080, 1920, 1080),
                ),
            ],
        };
        // centre in the upper half
        let (src, t) = pick_piece(&split, Rect::new(1620, 1540, 800, 600)).unwrap();
        assert_eq!((src, t), (Rect::new(1520, 1440, 1920, 1080), TOP));
        // centre in the lower half
        let (src, t) = pick_piece(&split, Rect::new(2000, 3000, 500, 400)).unwrap();
        assert_eq!(src, Rect::new(1520, 2520, 1920, 1080));
        assert_eq!(t, Rect::new(-1920, 1080, 1920, 1080));
        // lower-half window keeps its offset inside its own piece
        assert_eq!(
            rescue_rect(src, t, Rect::new(2000, 3000, 500, 400)),
            Rect::new(-1440, 1560, 500, 400)
        );
        // centre outside every piece: piece 0
        let (_, t) = pick_piece(&split, Rect::new(0, 0, 10, 10)).unwrap();
        assert_eq!(t, TOP);
        // no pieces: nothing to do
        let empty = RescueSplit {
            pieces: vec![],
            ..split
        };
        assert!(pick_piece(&empty, Rect::new(0, 0, 1, 1)).is_none());
    }

    #[test]
    fn column_pieces() {
        let hidden = Rect::new(0, -1440, 3440, 1440);
        let split = RescueSplit {
            physical_hmon: 1,
            physical: hidden,
            pieces: vec![
                (Rect::new(0, 0, 1720, 1440), Rect::new(0, 0, 1720, 1440)),
                (
                    Rect::new(1720, 0, 1720, 1440),
                    Rect::new(1720, 0, 1720, 1440),
                ),
            ],
        };
        let win = Rect::new(2000, -1000, 400, 300);
        let (src, t) = pick_piece(&split, win).unwrap();
        assert_eq!(t.x, 1720);
        assert_eq!(rescue_rect(src, t, win), Rect::new(2000, 440, 400, 300));
    }
}
