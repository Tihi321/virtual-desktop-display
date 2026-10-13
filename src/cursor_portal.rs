//! Cursor portal: a WH_MOUSE_LL hook that keeps the cursor off the hidden
//! physical displays (each hidden behind a renderer window). A move that would
//! enter a hidden display is blocked (it is a WALL); a cursor already inside
//! one is warped to the matching point on the virtual display whose piece it
//! is on.
use crate::displays::Rect;
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::thread::JoinHandle;
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, PostThreadMessageW, SetCursorPos,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HC_ACTION, MSG, MSLLHOOKSTRUCT,
    WH_MOUSE_LL, WM_MOUSEMOVE, WM_QUIT,
};

/// One hidden physical display and the virtual displays standing in for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalSplit {
    /// Desktop rect of the hidden physical display.
    pub physical: Rect,
    /// Per piece: (source rect relative to `physical`, desktop rect of the
    /// virtual display showing it).
    pub pieces: Vec<(Rect, Rect)>,
}

/// Map a point on the physical display to the corresponding point on the
/// virtual display of the piece under it. Returns None when `pt` is outside
/// `split.physical` (or the split has no pieces).
pub fn map_point(split: &PortalSplit, pt: (i32, i32)) -> Option<(i32, i32)> {
    let physical = split.physical;
    if physical.w <= 0 || physical.h <= 0 || split.pieces.is_empty() || !inside(physical, pt) {
        return None;
    }
    let rel = (pt.0 - physical.x, pt.1 - physical.y);
    // The piece under the point; pieces tile the display, but fall back to the
    // first one rather than failing on a rounding gap.
    let (src, virt) = split
        .pieces
        .iter()
        .find(|(src, _)| inside(*src, rel))
        .unwrap_or(&split.pieces[0]);
    if src.w <= 0 || src.h <= 0 || virt.w <= 0 || virt.h <= 0 {
        return None;
    }
    let x = virt.x + ((rel.0 - src.x) as i64 * virt.w as i64 / src.w as i64) as i32;
    let y = virt.y + ((rel.1 - src.y) as i64 * virt.h as i64 / src.h as i64) as i32;
    Some((
        x.clamp(virt.x, virt.right() - 1),
        y.clamp(virt.y, virt.bottom() - 1),
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

/// Decide what to do with a move from `prev` to `new`. `on_desktop` tells
/// whether a point is on any display (hidden ones included).
///
/// * Not entering any hidden display: Allow.
/// * Entering one from outside: it is a WALL; the cursor is put back just
///   outside on the side it came from, keeping the other coordinate so sliding
///   along the edge works.
/// * Already inside (warped there by an app or at startup): mapped onto the
///   virtual display of the piece under the point.
/// * Off every display, across an edge line a hidden display lies on: also a
///   wall. Windows would otherwise clamp the cursor or, with "Ease cursor
///   movement between displays", jump it onto the hidden display, which sits
///   beside the edge but not straight across it.
pub fn resolve_move(
    splits: &[PortalSplit],
    prev: (i32, i32),
    new: (i32, i32),
    on_desktop: impl Fn((i32, i32)) -> bool,
) -> Action {
    // Where to stop; when that point is on no display, stay where we were.
    let stop = |p: (i32, i32)| {
        let (x, y) = if on_desktop(p) { p } else { prev };
        Action::MoveTo(x, y)
    };
    if let Some(split) = splits.iter().find(|s| inside(s.physical, new)) {
        let physical = split.physical;
        if inside(physical, prev) {
            return match map_point(split, new) {
                Some((x, y)) => Action::MoveTo(x, y),
                None => Action::Allow,
            };
        }
        return if prev.1 < physical.y {
            stop((new.0, physical.y - 1))
        } else if prev.1 >= physical.bottom() {
            stop((new.0, physical.bottom()))
        } else if prev.0 < physical.x {
            stop((physical.x - 1, new.1))
        } else {
            stop((physical.right(), new.1))
        };
    }
    if on_desktop(new) {
        return Action::Allow;
    }
    for split in splits {
        let p = split.physical;
        if prev.1 >= p.bottom() && new.1 < p.bottom() {
            return stop((new.0, p.bottom()));
        }
        if prev.1 < p.y && new.1 >= p.y {
            return stop((new.0, p.y - 1));
        }
        if prev.0 >= p.right() && new.0 < p.right() {
            return stop((p.right(), new.1));
        }
        if prev.0 < p.x && new.0 >= p.x {
            return stop((p.x - 1, new.1));
        }
    }
    Action::Allow
}

struct State {
    splits: Vec<PortalSplit>,
    /// Last known cursor position (only touched from the hook thread).
    prev_x: AtomicI32,
    prev_y: AtomicI32,
    has_prev: AtomicBool,
}

impl State {
    fn resolve(&self, prev: (i32, i32), new: (i32, i32)) -> Action {
        resolve_move(&self.splits, prev, new, on_desktop)
    }

    /// Where the cursor is now, before this move. Read from Windows first:
    /// the last hooked point can be stale when Windows clamped the move or
    /// eased the cursor onto another display after the hook let it through.
    fn prev(&self) -> Option<(i32, i32)> {
        let mut p = POINT::default();
        if unsafe { GetCursorPos(&mut p) }.is_ok() {
            return Some((p.x, p.y));
        }
        if self.has_prev.load(Ordering::Relaxed) {
            return Some((
                self.prev_x.load(Ordering::Relaxed),
                self.prev_y.load(Ordering::Relaxed),
            ));
        }
        None
    }

    fn set_prev(&self, p: (i32, i32)) {
        self.prev_x.store(p.0, Ordering::Relaxed);
        self.prev_y.store(p.1, Ordering::Relaxed);
        self.has_prev.store(true, Ordering::Relaxed);
    }
}

fn on_desktop(p: (i32, i32)) -> bool {
    !unsafe { MonitorFromPoint(POINT { x: p.0, y: p.1 }, MONITOR_DEFAULTTONULL) }.is_invalid()
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
    pub fn start(splits: Vec<PortalSplit>) -> Result<Self> {
        let state = Arc::new(State {
            splits,
            prev_x: AtomicI32::new(0),
            prev_y: AtomicI32::new(0),
            has_prev: AtomicBool::new(false),
        });
        *STATE.write().unwrap() = Some(state.clone());

        // If the cursor is already on a hidden display, move it off now.
        let mut p = POINT::default();
        if unsafe { GetCursorPos(&mut p) }.is_ok() {
            // prev == new: if it is inside a hidden rect it gets mapped off.
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

    /// FlipGo parked above the Asus: hidden at (1520,-2160); upper piece shown
    /// by the virtual display at (-1920,0), lower piece by the one at (-1920,1080).
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

    /// Every point is on a display (no gaps), as in the wall tests.
    fn rm(splits: &[PortalSplit], prev: (i32, i32), new: (i32, i32)) -> Action {
        resolve_move(splits, prev, new, |_| true)
    }

    fn rows(physical: Rect, top: Rect, bot: Rect) -> PortalSplit {
        let half = physical.h / 2;
        PortalSplit {
            physical,
            pieces: vec![
                (Rect::new(0, 0, physical.w, half), top),
                (Rect::new(0, half, physical.w, physical.h - half), bot),
            ],
        }
    }

    fn cols(physical: Rect, left: Rect, right: Rect) -> PortalSplit {
        let half = physical.w / 2;
        PortalSplit {
            physical,
            pieces: vec![
                (Rect::new(0, 0, half, physical.h), left),
                (Rect::new(half, 0, physical.w - half, physical.h), right),
            ],
        }
    }

    fn grid22(physical: Rect, v: [Rect; 4]) -> PortalSplit {
        let (hw, hh) = (physical.w / 2, physical.h / 2);
        PortalSplit {
            physical,
            pieces: vec![
                (Rect::new(0, 0, hw, hh), v[0]),
                (Rect::new(hw, 0, physical.w - hw, hh), v[1]),
                (Rect::new(0, hh, hw, physical.h - hh), v[2]),
                (Rect::new(hw, hh, physical.w - hw, physical.h - hh), v[3]),
            ],
        }
    }

    #[test]
    fn outside_is_none() {
        let s = rows(PHYS, TOP, BOT);
        assert_eq!(map_point(&s, (1519, 1500)), None);
        assert_eq!(map_point(&s, (3440, 1500)), None); // right edge exclusive
        assert_eq!(map_point(&s, (2000, 1439)), None);
        assert_eq!(map_point(&s, (2000, 3600)), None); // bottom exclusive
        assert_eq!(
            map_point(
                &PortalSplit {
                    physical: PHYS,
                    pieces: vec![]
                },
                (2000, 2000)
            ),
            None
        );
    }

    #[test]
    fn corners_and_split() {
        let s = rows(PHYS, TOP, BOT);
        assert_eq!(map_point(&s, (1520, 1440)), Some((-1920, 0)));
        assert_eq!(map_point(&s, (3439, 2519)), Some((-1, 1079)));
        assert_eq!(map_point(&s, (1520, 2520)), Some((-1920, 1080)));
        assert_eq!(map_point(&s, (3439, 3599)), Some((-1, 2159)));
    }

    #[test]
    fn proportional_and_never_on_physical() {
        let p = Rect::new(0, 0, 3840, 2160);
        assert_eq!(
            map_point(&rows(p, TOP, BOT), (1920, 540)),
            Some((-960, 540))
        );
        let s = rows(PHYS, TOP, BOT);
        for y in (1440..3600).step_by(37) {
            for x in (1520..3440).step_by(41) {
                let (mx, my) = map_point(&s, (x, y)).unwrap();
                assert!(!PHYS.intersects(&Rect::new(mx, my, 1, 1)));
            }
        }
    }

    #[test]
    fn outside_to_outside_is_allowed() {
        let s = [rows(PHYS, TOP, BOT)];
        assert_eq!(rm(&s, (100, 100), (110, 120)), Action::Allow);
        assert_eq!(rm(&s, (1519, 1439), (1519, 1439)), Action::Allow);
        assert_eq!(rm(&[], (1, 1), (2, 2)), Action::Allow);
    }

    #[test]
    fn enter_from_top_is_a_wall() {
        let s = [rows(PHYS, TOP, BOT)];
        let r = |prev, new| rm(&s, prev, new);
        // moving down off the Asus at x=2000: stay on the Asus's last row
        assert_eq!(r((2000, 1439), (2000, 1441)), Action::MoveTo(2000, 1439));
        // diagonal: keep the new x so sliding along the edge works
        assert_eq!(r((1500, 1430), (1530, 1450)), Action::MoveTo(1530, 1439));
    }

    #[test]
    fn enter_from_other_sides() {
        let s = [rows(PHYS, TOP, BOT)];
        let r = |prev, new| rm(&s, prev, new);
        assert_eq!(r((1500, 2000), (1530, 2010)), Action::MoveTo(1519, 2010)); // from left
        assert_eq!(r((3450, 2000), (3430, 2005)), Action::MoveTo(3440, 2005)); // from right
        assert_eq!(r((2000, 3605), (2000, 3590)), Action::MoveTo(2000, 3600)); // from below
    }

    #[test]
    fn enter_from_below_when_physical_is_above() {
        let above = Rect::new(1520, -2160, 1920, 2160);
        let s = [rows(above, TOP, BOT)];
        let r = |prev, new| rm(&s, prev, new);
        // moving up off the primary's top edge: stay on the primary's first row
        assert_eq!(r((2000, 0), (2000, -2)), Action::MoveTo(2000, 0));
        assert_eq!(r((1500, 5), (1530, -3)), Action::MoveTo(1530, 0));
    }

    #[test]
    fn already_inside_is_mapped() {
        let s = [rows(PHYS, TOP, BOT)];
        assert_eq!(rm(&s, (2000, 2000), (1520, 1440)), Action::MoveTo(-1920, 0));
        assert_eq!(
            rm(&s, (2000, 2000), (1520, 2520)),
            Action::MoveTo(-1920, 1080)
        );
        // the displays are given by piece: exchanging them exchanges the targets
        let swapped = [rows(PHYS, BOT, TOP)];
        assert_eq!(
            rm(&swapped, (2000, 2000), (1520, 1440)),
            Action::MoveTo(-1920, 1080)
        );
    }

    #[test]
    fn column_pieces() {
        let hidden = Rect::new(-3440, -1440, 3440, 1440);
        let left = Rect::new(0, 0, 1720, 1440);
        let right = Rect::new(1720, 0, 1720, 1440);
        let s = [cols(hidden, left, right)];
        assert_eq!(map_point(&s[0], (-3440, -1440)), Some((0, 0)));
        assert_eq!(map_point(&s[0], (-1721, -1)), Some((1719, 1439)));
        assert_eq!(map_point(&s[0], (-1720, -1440)), Some((1720, 0)));
        assert_eq!(map_point(&s[0], (-1, -1)), Some((3439, 1439)));
        // wall from below (hidden is above its neighbour)
        assert_eq!(rm(&s, (-2000, 5), (-2000, -3)), Action::MoveTo(-2000, 0));
        assert_eq!(rm(&s, (-100, -1), (-100, -1)), Action::MoveTo(3340, 1439));
    }

    #[test]
    fn grid_pieces() {
        let hidden = Rect::new(0, -2160, 3840, 2160);
        let v = [
            Rect::new(0, 0, 1920, 1080),
            Rect::new(1920, 0, 1920, 1080),
            Rect::new(0, 1080, 1920, 1080),
            Rect::new(1920, 1080, 1920, 1080),
        ];
        let s = grid22(hidden, v);
        assert_eq!(map_point(&s, (10, -2150)), Some((10, 10)));
        assert_eq!(map_point(&s, (2000, -2150)), Some((2000, 10)));
        assert_eq!(map_point(&s, (10, -1000)), Some((10, 1160)));
        assert_eq!(map_point(&s, (3839, -1)), Some((3839, 2159)));
    }

    #[test]
    fn several_hidden_displays() {
        let a = rows(PHYS, TOP, BOT);
        let hidden_b = Rect::new(-6000, -1440, 3440, 1440);
        let b = cols(
            hidden_b,
            Rect::new(5000, 0, 1720, 1440),
            Rect::new(6720, 0, 1720, 1440),
        );
        let s = [a, b];
        // inside B: mapped through B only
        assert_eq!(
            rm(&s, (-5000, -500), (-5000, -500)),
            Action::MoveTo(5000 + 1000, 940)
        );
        // wall works per display
        assert_eq!(
            rm(&s, (2000, 1439), (2000, 1441)),
            Action::MoveTo(2000, 1439)
        );
        assert_eq!(rm(&s, (-5000, 5), (-5000, -5)), Action::MoveTo(-5000, 0));
        // between the two hidden displays: free
        assert_eq!(rm(&s, (0, 0), (10, 10)), Action::Allow);
    }

    /// The user's v0.2 layout: Asus primary at (0,0); FlipGo pieces where the
    /// FlipGo was, left of it; the FlipGo parked above the Asus, right-aligned.
    const ASUS: Rect = Rect {
        x: 0,
        y: 0,
        w: 3440,
        h: 1440,
    };
    const PARKED: Rect = Rect {
        x: 1520,
        y: -2160,
        w: 1920,
        h: 2160,
    };
    const LEFT_TOP: Rect = Rect {
        x: -1920,
        y: 0,
        w: 1920,
        h: 1080,
    };
    const LEFT_BOT: Rect = Rect {
        x: -1920,
        y: 1080,
        w: 1920,
        h: 1080,
    };

    fn on_user_desktop(p: (i32, i32)) -> bool {
        [ASUS, PARKED, LEFT_TOP, LEFT_BOT]
            .iter()
            .any(|r| inside(*r, p))
    }

    fn user(prev: (i32, i32), new: (i32, i32)) -> Action {
        resolve_move(
            &[rows(PARKED, LEFT_TOP, LEFT_BOT)],
            prev,
            new,
            on_user_desktop,
        )
    }

    #[test]
    fn up_into_the_gap_beside_the_parked_display_is_a_wall() {
        // Up off the Asus left of the parked FlipGo: Windows would ease the
        // cursor onto the FlipGo; stop on the Asus's first row instead.
        assert_eq!(user((500, 0), (500, -3)), Action::MoveTo(500, 0));
        assert_eq!(user((1519, 2), (1519, -40)), Action::MoveTo(1519, 0));
        // also up off the FlipGo pieces, whose top is the same edge line
        assert_eq!(user((-500, 0), (-500, -1)), Action::MoveTo(-500, 0));
    }

    #[test]
    fn up_into_the_parked_display_is_a_wall() {
        assert_eq!(user((2000, 0), (2000, -2)), Action::MoveTo(2000, 0));
        // diagonal from the gap side
        assert_eq!(user((1500, 1), (1530, -3)), Action::MoveTo(1530, 0));
    }

    #[test]
    fn wall_target_off_every_display_keeps_the_cursor() {
        // Coming at the parked display from the empty space on its left
        // (cursor already in the gap): the spot left of it is on no display.
        assert_eq!(user((1510, -5), (1525, -6)), Action::MoveTo(1510, -5));
    }

    #[test]
    fn already_on_the_parked_display_goes_to_the_piece() {
        // Eased there anyway: the next move maps it onto the bottom piece.
        assert_eq!(user((1520, -1), (1525, -3)), Action::MoveTo(-1915, 2157));
    }

    #[test]
    fn moves_between_visible_displays_are_free() {
        assert_eq!(user((0, 500), (-3, 500)), Action::Allow);
        assert_eq!(user((-3, 1200), (2, 1200)), Action::Allow);
        // off the bottom: no hidden display on that edge, Windows clamps
        assert_eq!(user((500, 1439), (500, 1442)), Action::Allow);
        // right of the pieces below the Asus: not an edge of the parked display
        assert_eq!(user((-1, 1800), (3, 1800)), Action::Allow);
    }
}
