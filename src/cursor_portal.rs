//! Cursor portal: a WH_MOUSE_LL hook that keeps the cursor off the physical
//! (hidden-behind-our-window) display. A move that would land inside the
//! physical rect is blocked and the cursor is warped to the matching point on
//! V-top (upper half) or V-bot (lower half).
use crate::displays::Rect;
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
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
/// displays rendered in the upper/lower half (already swapped if configured).
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

struct State {
    physical: Rect,
    v_top: Rect,
    v_bot: Rect,
    swap: Arc<AtomicBool>,
}

impl State {
    fn map(&self, pt: (i32, i32)) -> Option<(i32, i32)> {
        let (t, b) = if self.swap.load(Ordering::Relaxed) {
            (self.v_bot, self.v_top)
        } else {
            (self.v_top, self.v_bot)
        };
        map_point(self.physical, t, b, pt)
    }
}

static STATE: RwLock<Option<Arc<State>>> = RwLock::new(None);

unsafe extern "system" fn hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wp.0 as u32 == WM_MOUSEMOVE {
        let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
        if let Ok(g) = STATE.try_read() {
            if let Some(s) = g.as_ref() {
                if let Some((x, y)) = s.map((info.pt.x, info.pt.y)) {
                    let _ = SetCursorPos(x, y);
                    return LRESULT(1);
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
    pub fn start(physical: Rect, v_top: Rect, v_bot: Rect, swap: Arc<AtomicBool>) -> Result<Self> {
        let state = Arc::new(State {
            physical,
            v_top,
            v_bot,
            swap,
        });
        *STATE.write().unwrap() = Some(state.clone());

        // If the cursor is already on the physical display, move it off now.
        let mut p = POINT::default();
        if unsafe { GetCursorPos(&mut p) }.is_ok() {
            if let Some((x, y)) = state.map((p.x, p.y)) {
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
    fn swap_exchanges_halves() {
        let st = State {
            physical: PHYS,
            v_top: TOP,
            v_bot: BOT,
            swap: Arc::new(AtomicBool::new(true)),
        };
        assert_eq!(st.map((1520, 1440)), Some((-1920, 1080)));
        assert_eq!(st.map((1520, 2520)), Some((-1920, 0)));
        st.swap.store(false, Ordering::Relaxed);
        assert_eq!(st.map((1520, 1440)), Some((-1920, 0)));
    }
}
