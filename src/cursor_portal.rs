//! Cursor portal: a WH_MOUSE_LL hook that keeps the cursor off the physical
//! (hidden-behind-our-window) display. A move that would land inside the
//! physical rect is blocked and the cursor is warped to the matching point on
//! V-top (upper half) or V-bot (lower half).
use crate::displays::Rect;
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::thread::JoinHandle;
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, PostThreadMessageW, SetCursorPos,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HC_ACTION, MSG, MSLLHOOKSTRUCT,
    WH_MOUSE_LL, WM_MOUSEMOVE, WM_QUIT,
};

/// Map a point on the physical display to the corresponding point on the
/// virtual display shown in that half. `top`/`bot` are the rects of the
/// displays placed upper/lower in the Windows layout (= drawn in the upper/lower half).
/// Returns None when `pt` is outside `physical`.
pub fn map_point(physical: Rect, top: Rect, bot: Rect, pt: (i32, i32)) -> Option<(i32, i32)> {
    if physical.w <= 0
        || physical.h < 2
        || pt.0 < physical.x
        || pt.0 >= physical.right()
        || pt.1 < physical.y
        || pt.1 >= physical.bottom()
    {
        return None;
    }
    let half = physical.h / 2;
    let rel_y = pt.1 - physical.y;
    let (target, y_in, span) = if rel_y < half {
        (top, rel_y, half)
    } else {
        (bot, rel_y - half, physical.h - half)
    };
    let x = target.x + ((pt.0 - physical.x) as i64 * target.w as i64 / physical.w as i64) as i32;
    let y = target.y + (y_in as i64 * target.h as i64 / span as i64) as i32;
    Some((
        x.clamp(target.x, target.right() - 1),
        y.clamp(target.y, target.bottom() - 1),
    ))
}

/// What the hook should do with a mouse move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Allow,
    MoveTo(i32, i32),
}

fn inside(r: Rect, p: (i32, i32)) -> bool {
    p.0 >= r.x && p.0 < r.right() && p.1 >= r.y && p.1 < r.bottom()
}

/// Decide what to do with a move from `prev` to `new`.
///
/// * Not entering the physical rect: Allow.
/// * Entering it from outside: the rect is a WALL; the cursor is put back just
///   outside on the side it came from, keeping the other coordinate so sliding
///   along the edge works.
/// * Already inside (warped there by an app or startup): map onto V-top/V-bot.
///
/// `top`/`bot` are the virtual displays by layout position (upper / lower).
pub fn resolve_move(
    physical: Rect,
    top: Rect,
    bot: Rect,
    prev: (i32, i32),
    new: (i32, i32),
) -> Action {
    if !inside(physical, new) {
        return Action::Allow;
    }
    if inside(physical, prev) {
        return match map_point(physical, top, bot, new) {
            Some((x, y)) => Action::MoveTo(x, y),
            None => Action::Allow,
        };
    }
    if prev.1 < physical.y {
        Action::MoveTo(new.0, physical.y - 1)
    } else if prev.1 >= physical.bottom() {
        Action::MoveTo(new.0, physical.bottom())
    } else if prev.0 < physical.x {
        Action::MoveTo(physical.x - 1, new.1)
    } else {
        Action::MoveTo(physical.right(), new.1)
    }
}

struct State {
    physical: Rect,
    v_top: Rect,
    v_bot: Rect,
    /// Last known cursor position (only touched from the hook thread).
    prev_x: AtomicI32,
    prev_y: AtomicI32,
    has_prev: AtomicBool,
}

impl State {
    fn resolve(&self, prev: (i32, i32), new: (i32, i32)) -> Action {
        resolve_move(self.physical, self.v_top, self.v_bot, prev, new)
    }

    fn prev(&self) -> Option<(i32, i32)> {
        if self.has_prev.load(Ordering::Relaxed) {
            return Some((
                self.prev_x.load(Ordering::Relaxed),
                self.prev_y.load(Ordering::Relaxed),
            ));
        }
        let mut p = POINT::default();
        unsafe { GetCursorPos(&mut p) }.ok().map(|_| (p.x, p.y))
    }

    fn set_prev(&self, p: (i32, i32)) {
        self.prev_x.store(p.0, Ordering::Relaxed);
        self.prev_y.store(p.1, Ordering::Relaxed);
        self.has_prev.store(true, Ordering::Relaxed);
    }
}

static STATE: RwLock<Option<Arc<State>>> = RwLock::new(None);

unsafe extern "system" fn hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wp.0 as u32 == WM_MOUSEMOVE {
        let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
        if let Ok(g) = STATE.try_read() {
            if let Some(s) = g.as_ref() {
                let new = (info.pt.x, info.pt.y);
                let prev = s.prev().unwrap_or(new);
                match s.resolve(prev, new) {
                    Action::MoveTo(x, y) => {
                        let _ = SetCursorPos(x, y);
                        s.set_prev((x, y));
                        return LRESULT(1);
                    }
                    Action::Allow => s.set_prev(new),
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

pub struct CursorPortal {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl CursorPortal {
    pub fn start(physical: Rect, v_top: Rect, v_bot: Rect) -> Result<Self> {
        let state = Arc::new(State {
            physical,
            v_top,
            v_bot,
            prev_x: AtomicI32::new(0),
            prev_y: AtomicI32::new(0),
            has_prev: AtomicBool::new(false),
        });
        *STATE.write().unwrap() = Some(state.clone());

        // If the cursor is already on the physical display, move it off now.
        let mut p = POINT::default();
        if unsafe { GetCursorPos(&mut p) }.is_ok() {
            // prev == new: if it is inside the physical rect it gets mapped off.
            if let Action::MoveTo(x, y) = state.resolve((p.x, p.y), (p.x, p.y)) {
                unsafe {
                    let _ = SetCursorPos(x, y);
                }
            }
        }

        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let thread = std::thread::Builder::new()
            .name("cursor-portal".into())
            .spawn(move || unsafe {
                let hinst = GetModuleHandleW(None).unwrap_or_default();
                let hook =
                    match SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), Some(hinst.into()), 0) {
                        Ok(h) => h,
                        Err(e) => {
                            let _ = tx.send(Err(format!("SetWindowsHookExW: {e}")));
                            return;
                        }
                    };
                let _ = tx.send(Ok(GetCurrentThreadId()));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let _ = UnhookWindowsHookEx(hook);
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
                Err(anyhow!("cursor portal thread died"))
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

impl Drop for CursorPortal {
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
    const BOT: Rect = Rect {
        x: -1920,
        y: 1080,
        w: 1920,
        h: 1080,
    };

    #[test]
    fn outside_is_none() {
        assert_eq!(map_point(PHYS, TOP, BOT, (1519, 1500)), None);
        assert_eq!(map_point(PHYS, TOP, BOT, (3440, 1500)), None); // right edge exclusive
        assert_eq!(map_point(PHYS, TOP, BOT, (2000, 1439)), None);
        assert_eq!(map_point(PHYS, TOP, BOT, (2000, 3600)), None); // bottom exclusive
    }

    #[test]
    fn corners_and_split() {
        assert_eq!(map_point(PHYS, TOP, BOT, (1520, 1440)), Some((-1920, 0)));
        assert_eq!(map_point(PHYS, TOP, BOT, (3439, 2519)), Some((-1, 1079)));
        assert_eq!(map_point(PHYS, TOP, BOT, (1520, 2520)), Some((-1920, 1080)));
        assert_eq!(map_point(PHYS, TOP, BOT, (3439, 3599)), Some((-1, 2159)));
    }

    #[test]
    fn proportional_and_never_on_physical() {
        let p = Rect::new(0, 0, 3840, 2160);
        assert_eq!(map_point(p, TOP, BOT, (1920, 540)), Some((-960, 540)));
        for y in (1440..3600).step_by(37) {
            for x in (1520..3440).step_by(41) {
                let (mx, my) = map_point(PHYS, TOP, BOT, (x, y)).unwrap();
                assert!(!PHYS.intersects(&Rect::new(mx, my, 1, 1)));
            }
        }
    }

    #[test]
    fn outside_to_outside_is_allowed() {
        let r = |prev, new| resolve_move(PHYS, TOP, BOT, prev, new);
        assert_eq!(r((100, 100), (110, 120)), Action::Allow);
        assert_eq!(r((1519, 1439), (1519, 1439)), Action::Allow);
    }

    #[test]
    fn enter_from_top_is_a_wall() {
        let r = |prev, new| resolve_move(PHYS, TOP, BOT, prev, new);
        // moving down off the Asus at x=2000: stay on the Asus's last row
        assert_eq!(r((2000, 1439), (2000, 1441)), Action::MoveTo(2000, 1439));
        // diagonal: keep the new x so sliding along the edge works
        assert_eq!(r((1500, 1430), (1530, 1450)), Action::MoveTo(1530, 1439));
    }

    #[test]
    fn enter_from_other_sides() {
        let r = |prev, new| resolve_move(PHYS, TOP, BOT, prev, new);
        assert_eq!(r((1500, 2000), (1530, 2010)), Action::MoveTo(1519, 2010)); // from left
        assert_eq!(r((3450, 2000), (3430, 2005)), Action::MoveTo(3440, 2005)); // from right
        assert_eq!(r((2000, 3605), (2000, 3590)), Action::MoveTo(2000, 3600)); // from below
    }

    #[test]
    fn enter_from_below_when_physical_is_above() {
        let above = Rect::new(1520, -2160, 1920, 2160);
        let r = |prev, new| resolve_move(above, TOP, BOT, prev, new);
        // moving up off the primary's top edge: stay on the primary's first row
        assert_eq!(r((2000, 0), (2000, -2)), Action::MoveTo(2000, 0));
        assert_eq!(r((1500, 5), (1530, -3)), Action::MoveTo(1530, 0));
    }

    #[test]
    fn already_inside_is_mapped() {
        let r = |prev, new| resolve_move(PHYS, TOP, BOT, prev, new);
        assert_eq!(r((2000, 2000), (1520, 1440)), Action::MoveTo(-1920, 0));
        assert_eq!(r((2000, 2000), (1520, 2520)), Action::MoveTo(-1920, 1080));
        // the displays are given by position: exchanging them exchanges the targets
        assert_eq!(
            resolve_move(PHYS, BOT, TOP, (2000, 2000), (1520, 1440)),
            Action::MoveTo(-1920, 1080)
        );
    }
}
