//! Pure desktop layout planning for a session.
//!
//! * Each virtual piece goes to the physical display's origin + the piece offset.
//! * Each split (hidden) physical display is PARKED sprite-safely: its bottom
//!   edge on the top edge of a visible display (right-aligned, then
//!   left-aligned), else its right edge on the left edge of a visible display
//!   (top-aligned, then bottom-aligned). Above/left, never below/right, so the
//!   hardware cursor sprite (which extends down/right of the hotspot) never
//!   shows on the hidden panel while the cursor rests at the visible edge.
//! * If a split display was the primary one, everything is translated so the
//!   main piece sits at (0,0) and becomes the new primary.
use crate::displays::{Rect, TargetKey};

/// A real (non-virtual) display before the session.
#[derive(Debug, Clone, Copy)]
pub struct DisplayGeom {
    pub key: TargetKey,
    pub rect: Rect,
    pub primary: bool,
}

/// One split: the physical display and its pieces (virtual display order).
#[derive(Debug, Clone)]
pub struct SplitGeom {
    pub physical_key: TargetKey,
    pub physical_rect: Rect,
    /// Piece rects relative to the physical display, per virtual display.
    pub pieces_rel: Vec<Rect>,
    pub is_primary: bool,
    /// Index into `pieces_rel` of the piece that becomes the primary origin.
    pub main_piece: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutPlan {
    /// Per split, per piece: desktop position of the virtual display.
    pub virtual_positions: Vec<Vec<(i32, i32)>>,
    /// Hidden physical displays and where they are parked.
    pub parked: Vec<(TargetKey, (i32, i32))>,
    /// Real displays that are not split, with their (possibly translated) position.
    pub real: Vec<(TargetKey, (i32, i32))>,
}

impl LayoutPlan {
    /// Every (key, position) of the plan, flat: virtual displays in split/piece
    /// order given `virt_keys`, then parked, then real.
    pub fn positions(&self, virt_keys: &[Vec<TargetKey>]) -> Vec<(TargetKey, (i32, i32))> {
        let mut v = Vec::new();
        for (keys, pos) in virt_keys.iter().zip(&self.virtual_positions) {
            for (k, p) in keys.iter().zip(pos) {
                v.push((*k, *p));
            }
        }
        v.extend(self.parked.iter().copied());
        v.extend(self.real.iter().copied());
        v
    }
}

/// Edge contact or overlap (closed intervals).
fn touches(a: &Rect, b: &Rect) -> bool {
    a.x <= b.right() && b.x <= a.right() && a.y <= b.bottom() && b.y <= a.bottom()
}

fn shift(r: Rect, d: (i32, i32)) -> Rect {
    Rect::new(r.x + d.0, r.y + d.1, r.w, r.h)
}

/// Choose a parking position for a hidden display of size `w` x `h`.
/// `visible`: (rect, is_virtual_piece). `blocked`: everything it must not
/// overlap (visible displays and already parked ones); `hidden`: parked ones it
/// must not even touch.
fn park(w: i32, h: i32, visible: &[(Rect, bool)], hidden: &[Rect]) -> Option<(i32, i32)> {
    let mut order: Vec<&(Rect, bool)> = visible.iter().collect();
    // Real displays first (a piece edge is a last resort), topmost then leftmost.
    order.sort_by_key(|(r, virt)| (*virt, r.y, r.x));
    let mut cands: Vec<(i32, i32)> = Vec::new();
    for virt_pass in [false, true] {
        let group = || order.iter().filter(move |(_, v)| *v == virt_pass);
        for (r, _) in group() {
            if w <= r.w {
                cands.push((r.right() - w, r.y - h)); // above, right-aligned
            }
            cands.push((r.x, r.y - h)); // above, left-aligned
        }
        for (r, _) in group() {
            cands.push((r.x - w, r.y)); // left, top-aligned
            cands.push((r.x - w, r.bottom() - h)); // left, bottom-aligned
        }
    }
    cands.into_iter().find(|&(x, y)| {
        let c = Rect::new(x, y, w, h);
        !visible.iter().any(|(v, _)| c.intersects(v)) && !hidden.iter().any(|hd| touches(&c, hd))
    })
}

/// Compute the layout. `real` lists all real displays (split ones included,
/// identified by `SplitGeom::physical_key`).
pub fn plan(real: &[DisplayGeom], splits: &[SplitGeom]) -> LayoutPlan {
    let is_split = |k: &TargetKey| splits.iter().any(|s| s.physical_key == *k);

    // Absolute rects of all virtual pieces (untranslated).
    let pieces_abs: Vec<Vec<Rect>> = splits
        .iter()
        .map(|s| {
            s.pieces_rel
                .iter()
                .map(|p| shift(*p, (s.physical_rect.x, s.physical_rect.y)))
                .collect()
        })
        .collect();

    // Translation so the main piece of a primary split ends up at (0,0).
    let mut d = (0, 0);
    if let Some((i, s)) = splits.iter().enumerate().find(|(_, s)| s.is_primary) {
        if let Some(p) = pieces_abs[i].get(s.main_piece).or(pieces_abs[i].first()) {
            d = (-p.x, -p.y);
        }
    }

    let real_unsplit: Vec<(TargetKey, Rect)> = real
        .iter()
        .filter(|r| !is_split(&r.key))
        .map(|r| (r.key, shift(r.rect, d)))
        .collect();

    let mut visible: Vec<(Rect, bool)> = real_unsplit.iter().map(|(_, r)| (*r, false)).collect();
    for ps in &pieces_abs {
        visible.extend(ps.iter().map(|p| (shift(*p, d), true)));
    }

    let mut hidden: Vec<Rect> = Vec::new();
    let mut parked = Vec::new();
    for s in splits {
        let (w, h) = (s.physical_rect.w, s.physical_rect.h);
        let pos = park(w, h, &visible, &hidden).unwrap_or_else(|| {
            // No sprite-safe spot: right of everything (still connected if
            // anything is visible).
            let right = visible.iter().map(|(r, _)| r.right()).max().unwrap_or(0);
            let top = visible.iter().map(|(r, _)| r.y).min().unwrap_or(0);
            tracing::warn!(
                "no sprite-safe parking spot for {:?}; using the right side",
                s.physical_key
            );
            (right, top)
        });
        hidden.push(Rect::new(pos.0, pos.1, w, h));
        parked.push((s.physical_key, pos));
    }

    LayoutPlan {
        virtual_positions: pieces_abs
            .iter()
            .map(|ps| ps.iter().map(|p| (p.x + d.0, p.y + d.1)).collect())
            .collect(),
        parked,
        real: real_unsplit.iter().map(|(k, r)| (*k, (r.x, r.y))).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: u32) -> TargetKey {
        TargetKey {
            luid_low: 1,
            luid_high: 0,
            target_id: id,
        }
    }

    fn real(id: u32, r: Rect, primary: bool) -> DisplayGeom {
        DisplayGeom {
            key: key(id),
            rect: r,
            primary,
        }
    }

    fn rows2(id: u32, phys: Rect, primary: bool, main: usize) -> SplitGeom {
        SplitGeom {
            physical_key: key(id),
            physical_rect: phys,
            pieces_rel: vec![
                Rect::new(0, 0, phys.w, phys.h / 2),
                Rect::new(0, phys.h / 2, phys.w, phys.h - phys.h / 2),
            ],
            is_primary: primary,
            main_piece: main,
        }
    }

    fn cols2(id: u32, phys: Rect, primary: bool, main: usize) -> SplitGeom {
        SplitGeom {
            physical_key: key(id),
            physical_rect: phys,
            pieces_rel: vec![
                Rect::new(0, 0, phys.w / 2, phys.h),
                Rect::new(phys.w / 2, 0, phys.w - phys.w / 2, phys.h),
            ],
            is_primary: primary,
            main_piece: main,
        }
    }

    const ASUS: Rect = Rect {
        x: 0,
        y: 0,
        w: 3440,
        h: 1440,
    };
    const FLIP: Rect = Rect {
        x: -1920,
        y: 0,
        w: 1920,
        h: 2160,
    };

    /// All final rects of a plan: (rect, is_hidden).
    fn rects(
        plan: &LayoutPlan,
        real_in: &[DisplayGeom],
        splits: &[SplitGeom],
    ) -> Vec<(Rect, bool)> {
        let mut v = Vec::new();
        for (s, ps) in splits.iter().zip(&plan.virtual_positions) {
            for (p, pos) in s.pieces_rel.iter().zip(ps) {
                v.push((Rect::new(pos.0, pos.1, p.w, p.h), false));
            }
        }
        for (k, pos) in &plan.parked {
            let s = splits.iter().find(|s| s.physical_key == *k).unwrap();
            v.push((
                Rect::new(pos.0, pos.1, s.physical_rect.w, s.physical_rect.h),
                true,
            ));
        }
        for (k, pos) in &plan.real {
            let r = real_in.iter().find(|r| r.key == *k).unwrap();
            v.push((Rect::new(pos.0, pos.1, r.rect.w, r.rect.h), false));
        }
        v
    }

    fn shares_edge(a: &Rect, b: &Rect) -> bool {
        let ox = a.x.max(b.x) < a.right().min(b.right());
        let oy = a.y.max(b.y) < a.bottom().min(b.bottom());
        ((a.right() == b.x || b.right() == a.x) && oy)
            || ((a.bottom() == b.y || b.bottom() == a.y) && ox)
    }

    fn connected(rs: &[(Rect, bool)]) -> bool {
        if rs.is_empty() {
            return true;
        }
        let mut seen = vec![false; rs.len()];
        let mut stack = vec![0];
        seen[0] = true;
        while let Some(i) = stack.pop() {
            for j in 0..rs.len() {
                if !seen[j] && shares_edge(&rs[i].0, &rs[j].0) {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        seen.iter().all(|s| *s)
    }

    fn check_invariants(plan: &LayoutPlan, real_in: &[DisplayGeom], splits: &[SplitGeom]) {
        let rs = rects(plan, real_in, splits);
        for (i, (a, _)) in rs.iter().enumerate() {
            for (b, _) in &rs[i + 1..] {
                assert!(!a.intersects(b), "{a:?} overlaps {b:?}");
            }
        }
        // no contact between hidden displays
        let hid: Vec<&Rect> = rs.iter().filter(|r| r.1).map(|r| &r.0).collect();
        for (i, a) in hid.iter().enumerate() {
            for b in &hid[i + 1..] {
                assert!(!touches(a, b), "hidden {a:?} touches {b:?}");
            }
        }
        assert!(connected(&rs), "layout not connected: {rs:?}");
    }

    #[test]
    fn flipgo_reproduces_v01() {
        let r = [real(1, ASUS, true), real(2, FLIP, false)];
        let s = [rows2(2, FLIP, false, 0)];
        let p = plan(&r, &s);
        assert_eq!(p.virtual_positions, vec![vec![(-1920, 0), (-1920, 1080)]]);
        assert_eq!(p.parked, vec![(key(2), (1520, -2160))]);
        assert_eq!(p.real, vec![(key(1), (0, 0))]);
        check_invariants(&p, &r, &s);
        // parked touches only the primary's top edge, right-aligned
        let ph = Rect::new(1520, -2160, 1920, 2160);
        assert_eq!(ph.bottom(), ASUS.y);
        assert_eq!(ph.right(), ASUS.right());
    }

    #[test]
    fn flipgo_with_offset_y_keeps_v01_behaviour() {
        // The real machine: FlipGo top at y=-91.
        let flip = Rect::new(-1920, -91, 1920, 2160);
        let r = [real(1, ASUS, true), real(2, flip, false)];
        let s = [rows2(2, flip, false, 0)];
        let p = plan(&r, &s);
        assert_eq!(p.virtual_positions, vec![vec![(-1920, -91), (-1920, 989)]]);
        assert_eq!(p.parked, vec![(key(2), (1520, -2160))]);
        check_invariants(&p, &r, &s);
    }

    #[test]
    fn narrow_primary_parks_left_aligned() {
        let primary = Rect::new(0, 0, 1000, 1440);
        let r = [real(1, primary, true), real(2, FLIP, false)];
        let s = [rows2(2, FLIP, false, 0)];
        let p = plan(&r, &s);
        assert_eq!(p.parked, vec![(key(2), (0, -2160))]);
        check_invariants(&p, &r, &s);
    }

    #[test]
    fn asus_two_columns_primary_main_0() {
        let r = [real(1, ASUS, true), real(2, FLIP, false)];
        let s = [cols2(1, ASUS, true, 0)];
        let p = plan(&r, &s);
        assert_eq!(p.virtual_positions, vec![vec![(0, 0), (1720, 0)]]);
        assert_eq!(p.real, vec![(key(2), (-1920, 0))]);
        check_invariants(&p, &r, &s);
        // hidden Asus is not the primary origin
        assert_ne!(p.parked[0].1, (0, 0));
    }

    #[test]
    fn asus_two_columns_primary_main_1_translates() {
        let r = [real(1, ASUS, true), real(2, FLIP, false)];
        let s = [cols2(1, ASUS, true, 1)];
        let p = plan(&r, &s);
        // main piece (right half) origin becomes (0,0)
        assert_eq!(p.virtual_positions, vec![vec![(-1720, 0), (0, 0)]]);
        assert_eq!(p.real, vec![(key(2), (-3640, 0))]);
        check_invariants(&p, &r, &s);
    }

    #[test]
    fn both_splits_at_once() {
        let r = [real(1, ASUS, true), real(2, FLIP, false)];
        let s = [cols2(1, ASUS, true, 0), rows2(2, FLIP, false, 0)];
        let p = plan(&r, &s);
        assert_eq!(
            p.virtual_positions,
            vec![vec![(0, 0), (1720, 0)], vec![(-1920, 0), (-1920, 1080)]]
        );
        assert!(p.real.is_empty());
        assert_eq!(p.parked.len(), 2);
        check_invariants(&p, &r, &s);
    }

    #[test]
    fn both_splits_main_piece_1() {
        let r = [real(1, ASUS, true), real(2, FLIP, false)];
        let s = [cols2(1, ASUS, true, 1), rows2(2, FLIP, false, 0)];
        let p = plan(&r, &s);
        assert_eq!(p.virtual_positions[0], vec![(-1720, 0), (0, 0)]);
        assert_eq!(p.virtual_positions[1], vec![(-3640, 0), (-3640, 1080)]);
        check_invariants(&p, &r, &s);
    }

    #[test]
    fn laptop_single_monitor_parks_above_pieces() {
        let lap = Rect::new(0, 0, 1920, 1080);
        let r = [real(1, lap, true)];
        let s = [rows2(1, lap, true, 0)];
        let p = plan(&r, &s);
        assert_eq!(p.virtual_positions, vec![vec![(0, 0), (0, 540)]]);
        assert_eq!(p.parked, vec![(key(1), (0, -1080))]);
        assert!(p.real.is_empty());
        check_invariants(&p, &r, &s);
    }

    #[test]
    fn three_displays_two_splits_and_a_bystander() {
        let side = Rect::new(3440, 0, 1920, 1080);
        let r = [
            real(1, ASUS, true),
            real(2, FLIP, false),
            real(3, side, false),
        ];
        let s = [rows2(2, FLIP, false, 0), cols2(3, side, false, 0)];
        let p = plan(&r, &s);
        assert_eq!(p.real, vec![(key(1), (0, 0))]);
        check_invariants(&p, &r, &s);
    }

    #[test]
    fn grid_pieces_keep_positions() {
        let big = Rect::new(0, 0, 3840, 2160);
        let r = [real(1, big, true)];
        let pieces_rel = vec![
            Rect::new(0, 0, 1920, 1080),
            Rect::new(1920, 0, 1920, 1080),
            Rect::new(0, 1080, 1920, 1080),
            Rect::new(1920, 1080, 1920, 1080),
        ];
        let s = [SplitGeom {
            physical_key: key(1),
            physical_rect: big,
            pieces_rel,
            is_primary: true,
            main_piece: 3,
        }];
        let p = plan(&r, &s);
        assert_eq!(
            p.virtual_positions[0],
            vec![(-1920, -1080), (0, -1080), (-1920, 0), (0, 0)]
        );
        check_invariants(&p, &r, &s);
    }

    #[test]
    fn positions_flat_order() {
        let r = [real(1, ASUS, true), real(2, FLIP, false)];
        let s = [rows2(2, FLIP, false, 0)];
        let p = plan(&r, &s);
        let flat = p.positions(&[vec![key(10), key(11)]]);
        assert_eq!(
            flat,
            vec![
                (key(10), (-1920, 0)),
                (key(11), (-1920, 1080)),
                (key(2), (1520, -2160)),
                (key(1), (0, 0)),
            ]
        );
    }
}
