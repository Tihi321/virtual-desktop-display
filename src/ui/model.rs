//! Pure helpers of the settings UI (no egui): split normalisation, ratio
//! editing, the per-piece table and config validation. Unit tested.
use crate::config::{Config, LayoutKind, SplitConfig};
use crate::displays::{self, Rect, TargetInfo};
use crate::geometry;
use crate::hotkeys;
use crate::modes::{self, Mode, SlotPlan};
use crate::vdd::parsec::VDD_MAX_DISPLAYS;

/// Smallest share of a display one custom piece may get.
pub const MIN_FRACTION: f64 = 0.1;
/// Largest column / row count offered by the steppers.
pub const MAX_STEPPER: u32 = 4;
pub const REFRESH_CHOICES: [u32; 4] = [30, 60, 120, 144];

/// `n` equal fractions summing to 1.
pub fn equal_ratios(n: usize) -> Vec<f64> {
    let n = n.max(1);
    vec![1.0 / n as f64; n]
}

/// Inner boundaries (cumulative fractions) of `ratios`, length `n - 1`.
pub fn boundaries(ratios: &[f64]) -> Vec<f64> {
    let sum: f64 = ratios.iter().sum();
    let sum = if sum > 0.0 { sum } else { 1.0 };
    let mut acc = 0.0;
    ratios
        .iter()
        .take(ratios.len().saturating_sub(1))
        .map(|r| {
            acc += r / sum;
            acc
        })
        .collect()
}

/// Allowed range of inner boundary `k` so that every piece keeps `MIN_FRACTION`.
pub fn boundary_range(ratios: &[f64], k: usize) -> (f64, f64) {
    let b = boundaries(ratios);
    let lo = if k == 0 { 0.0 } else { b[k - 1] } + MIN_FRACTION;
    let hi = if k + 1 >= b.len() { 1.0 } else { b[k + 1] } - MIN_FRACTION;
    (lo, hi.max(lo))
}

/// Move inner boundary `k` to `v` (clamped); the ratios keep summing to 1.
pub fn set_boundary(ratios: &mut [f64], k: usize, v: f64) {
    let n = ratios.len();
    if k + 1 >= n {
        return;
    }
    let (lo, hi) = boundary_range(ratios, k);
    let mut b = boundaries(ratios);
    b[k] = v.clamp(lo, hi);
    let mut prev = 0.0;
    for (i, r) in ratios.iter_mut().enumerate() {
        let cur = if i + 1 == n { 1.0 } else { b[i] };
        *r = cur - prev;
        prev = cur;
    }
}

pub fn piece_count(sc: &SplitConfig) -> usize {
    geometry::piece_count(&sc.spec())
}

/// Make a split internally consistent after any edit: counts within the
/// stepper range, custom ratios usable, `order` a permutation, main piece in range.
pub fn normalize(sc: &mut SplitConfig) {
    sc.columns = sc.columns.clamp(1, MAX_STEPPER);
    sc.rows = sc.rows.clamp(1, MAX_STEPPER);
    if sc.layout == LayoutKind::Custom {
        let ok = |r: &Vec<f64>| {
            !r.is_empty()
                && r.len() <= MAX_STEPPER as usize
                && r.iter().all(|x| x.is_finite() && *x >= MIN_FRACTION - 1e-6)
                && (r.iter().sum::<f64>() - 1.0).abs() < 1e-6
        };
        if !ok(&sc.ratios_x) {
            sc.ratios_x = equal_ratios(sc.ratios_x.len().clamp(1, MAX_STEPPER as usize).max(2));
        }
        if !ok(&sc.ratios_y) {
            sc.ratios_y = equal_ratios(sc.ratios_y.len().clamp(1, MAX_STEPPER as usize));
        }
    }
    let n = piece_count(sc);
    sc.order = geometry::repair_order(&sc.order, n);
    sc.main_piece = sc.main_piece.min(n - 1);
}

/// Change the layout kind, keeping the piece counts sensible.
pub fn set_layout(sc: &mut SplitConfig, kind: LayoutKind) {
    if sc.layout == kind {
        return;
    }
    // Carry the visible counts over to the new kind.
    let (c, r) = geometry::grid_dims(&sc.spec());
    sc.layout = kind;
    match kind {
        LayoutKind::Columns => {
            sc.columns = (c * r).clamp(1, MAX_STEPPER as usize).max(2) as u32;
        }
        LayoutKind::Rows => {
            sc.rows = (c * r).clamp(1, MAX_STEPPER as usize).max(2) as u32;
        }
        LayoutKind::Grid => {
            sc.columns = c.clamp(1, MAX_STEPPER as usize) as u32;
            sc.rows = r.clamp(1, MAX_STEPPER as usize) as u32;
        }
        LayoutKind::Custom => {
            sc.ratios_x = equal_ratios(c.clamp(1, MAX_STEPPER as usize).max(2));
            sc.ratios_y = equal_ratios(r.clamp(1, MAX_STEPPER as usize));
        }
    }
    sc.order.clear();
    normalize(sc);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceStatus {
    Builtin,
    Registered,
    NeedsSlot,
    /// More than 5 custom sizes: this fallback mode is used and stretched.
    Overflow(Mode),
}

impl PieceStatus {
    pub fn is_problem(&self) -> bool {
        matches!(self, PieceStatus::NeedsSlot | PieceStatus::Overflow(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PieceRow {
    /// Virtual display index (0-based).
    pub index: usize,
    /// Piece rect relative to the display.
    pub rect: Rect,
    pub mode: Mode,
    pub status: PieceStatus,
    /// The main piece (shown only for a primary display).
    pub main: bool,
}

/// Table of the pieces of `sc` on a display of `size`.
pub fn piece_rows(
    sc: &SplitConfig,
    size: (i32, i32),
    is_primary: bool,
    registered: &[Mode],
    plan: &SlotPlan,
) -> Vec<PieceRow> {
    let spec = sc.spec();
    let pieces = geometry::pieces(size.0, size.1, &spec);
    let order = geometry::repair_order(&spec.order, pieces.len());
    let main_v = geometry::virtual_index_of_cell(&order, sc.main_piece.min(pieces.len() - 1));
    let hz = sc.refresh_hz.max(1);
    pieces
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mode = Mode::new(p.w.max(0) as u32, p.h.max(0) as u32, hz);
            let status = if modes::is_builtin(mode) {
                PieceStatus::Builtin
            } else if registered.contains(&mode) {
                PieceStatus::Registered
            } else if plan.overflow.contains(&mode) {
                PieceStatus::Overflow(modes::fallback_for(mode))
            } else {
                PieceStatus::NeedsSlot
            };
            PieceRow {
                index: i,
                rect: *p,
                mode,
                status,
                main: is_primary && i == main_v,
            }
        })
        .collect()
}

/// The modes the enabled, connected splits need, and how they map to slots.
pub fn slot_plan(
    cfg: &Config,
    targets: &[TargetInfo],
    registered: &[Mode],
) -> (Vec<Mode>, SlotPlan) {
    let inputs: Vec<(SplitConfig, (i32, i32))> = cfg
        .splits
        .iter()
        .filter_map(|sc| {
            displays::resolve_split(targets, sc).map(|t| (sc.clone(), (t.rect.w, t.rect.h)))
        })
        .collect();
    let req = modes::required(&inputs);
    let plan = modes::plan_slots(&req, registered);
    (req, plan)
}

/// Pieces across all enabled splits.
pub fn total_pieces(cfg: &Config) -> usize {
    cfg.splits
        .iter()
        .filter(|s| s.enabled)
        .map(piece_count)
        .sum()
}

fn split_label(i: usize, sc: &SplitConfig) -> String {
    if sc.name.trim().is_empty() {
        format!("Split {}", i + 1)
    } else {
        format!("\"{}\"", sc.name.trim())
    }
}

/// Everything that must be fixed before the config may be saved.
pub fn validate(cfg: &Config, targets: &[TargetInfo]) -> Vec<String> {
    let mut errs = Vec::new();
    let total = total_pieces(cfg);
    if total > VDD_MAX_DISPLAYS {
        errs.push(format!(
            "The enabled splits need {total} virtual displays; the driver supports at most {VDD_MAX_DISPLAYS}."
        ));
    }
    // Identity of the display of each split: the connected display's target
    // key, else the configured id / name.
    let ident = |sc: &SplitConfig| -> Option<String> {
        if let Some(t) = displays::resolve_split(targets, sc) {
            return Some(format!("{:?}", t.key));
        }
        let id = sc.display_id.trim().to_ascii_lowercase();
        if !id.is_empty() {
            return Some(format!("id:{id}"));
        }
        let n = sc.name_match.trim().to_ascii_lowercase();
        (!n.is_empty()).then(|| format!("name:{n}"))
    };
    let mut seen: Vec<(String, usize)> = Vec::new();
    for (i, sc) in cfg.splits.iter().enumerate() {
        if !sc.enabled {
            continue;
        }
        match ident(sc) {
            None => errs.push(format!("{}: choose a display.", split_label(i, sc))),
            Some(k) => {
                if let Some((_, j)) = seen.iter().find(|(s, _)| *s == k) {
                    errs.push(format!(
                        "{} and {} use the same display; a display can only be split once.",
                        split_label(*j, &cfg.splits[*j]),
                        split_label(i, sc)
                    ));
                } else {
                    seen.push((k, i));
                }
            }
        }
    }
    // Hotkeys: parseable and not duplicated among the active ones.
    let mut active: Vec<(hotkeys::Action, hotkeys::Combo)> = Vec::new();
    for a in hotkeys::Action::ALL {
        let b = hotkeys::binding(&cfg.hotkeys, a);
        if !cfg.hotkeys.enabled || !b.enabled {
            continue;
        }
        match hotkeys::parse(&b.keys) {
            Err(e) => errs.push(format!("Hotkey \"{}\": {e}.", a.label())),
            Ok(c) => {
                if !hotkeys::is_acceptable(c) {
                    errs.push(format!(
                        "Hotkey \"{}\": add a modifier (Ctrl, Alt, Shift or Win).",
                        a.label()
                    ));
                }
                if let Some((o, _)) = active.iter().find(|(_, oc)| *oc == c) {
                    errs.push(format!(
                        "Hotkeys \"{}\" and \"{}\" use the same keys.",
                        o.label(),
                        a.label()
                    ));
                }
                active.push((a, c));
            }
        }
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::TargetKey;

    fn sc(layout: LayoutKind, c: u32, r: u32) -> SplitConfig {
        SplitConfig {
            name: "x".into(),
            display_id: "ID".into(),
            layout,
            columns: c,
            rows: r,
            order: Vec::new(),
            ..SplitConfig::default()
        }
    }

    fn target(id: u32, w: i32, h: i32, path_id: &str) -> TargetInfo {
        TargetInfo {
            key: TargetKey {
                luid_low: 1,
                luid_high: 0,
                target_id: id,
            },
            friendly_name: format!("Mon{id}"),
            device_path: format!(r"\\?\DISPLAY#{path_id}#x"),
            gdi_name: String::new(),
            rect: Rect::new(0, 0, w, h),
            hmonitor: 0,
            primary: false,
        }
    }

    #[test]
    fn ratios_equal_and_boundaries() {
        let r = equal_ratios(4);
        assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        let b = boundaries(&r);
        assert_eq!(b.len(), 3);
        assert!((b[0] - 0.25).abs() < 1e-9 && (b[2] - 0.75).abs() < 1e-9);
        assert!(boundaries(&[1.0]).is_empty());
    }

    #[test]
    fn set_boundary_keeps_sum_and_minimum() {
        let mut r = equal_ratios(3);
        set_boundary(&mut r, 0, 0.0); // clamp to 10 %
        assert!((r[0] - 0.1).abs() < 1e-9);
        assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        set_boundary(&mut r, 0, 0.95); // cannot pass the next boundary - 10 %
        assert!(r.iter().all(|x| *x >= 0.1 - 1e-9));
        assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        set_boundary(&mut r, 1, 0.99);
        assert!((r[2] - 0.1).abs() < 1e-9);
        assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        // out of range index is ignored
        let before = r.clone();
        set_boundary(&mut r, 5, 0.5);
        assert_eq!(r, before);
    }

    #[test]
    fn normalize_repairs() {
        let mut s = sc(LayoutKind::Grid, 9, 0);
        s.main_piece = 99;
        s.order = vec![5, 5];
        normalize(&mut s);
        assert_eq!((s.columns, s.rows), (4, 1));
        assert_eq!(s.order, vec![0, 1, 2, 3]);
        assert_eq!(s.main_piece, 3);

        let mut c = sc(LayoutKind::Custom, 1, 1);
        c.ratios_x = vec![0.7, 0.7];
        normalize(&mut c);
        assert!((c.ratios_x.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert_eq!(c.ratios_y.len(), 1);
        assert_eq!(c.order.len(), 2);
    }

    #[test]
    fn set_layout_carries_counts() {
        let mut s = SplitConfig::default(); // rows, 2
        set_layout(&mut s, LayoutKind::Columns);
        assert_eq!(s.columns, 2);
        assert_eq!(s.order, vec![0, 1]);
        set_layout(&mut s, LayoutKind::Custom);
        assert_eq!(s.ratios_x.len(), 2);
        assert_eq!(piece_count(&s), 2);
        set_layout(&mut s, LayoutKind::Grid);
        assert_eq!(piece_count(&s), 2);
    }

    #[test]
    fn rows_flag_slot_status() {
        let s = sc(LayoutKind::Columns, 2, 1);
        let reg = [Mode::new(1720, 1440, 60)];
        let plan = modes::plan_slots(&[Mode::new(1720, 1440, 60)], &reg);
        let rows = piece_rows(&s, (3440, 1440), true, &reg, &plan);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, PieceStatus::Registered);
        assert!(rows[0].main && !rows[1].main);

        let plan = modes::plan_slots(&[Mode::new(1720, 1440, 60)], &[]);
        let rows = piece_rows(&s, (3440, 1440), false, &[], &plan);
        assert_eq!(rows[0].status, PieceStatus::NeedsSlot);
        assert!(rows[0].status.is_problem() && !rows[0].main);

        // built-in
        let s2 = sc(LayoutKind::Rows, 1, 2);
        let plan = modes::plan_slots(&[Mode::new(1920, 1080, 60)], &[]);
        let rows = piece_rows(&s2, (1920, 2160), false, &[], &plan);
        assert!(rows.iter().all(|r| r.status == PieceStatus::Builtin));
        // non-60 Hz is never built in
        let mut s3 = s2.clone();
        s3.refresh_hz = 144;
        let plan = modes::plan_slots(&[Mode::new(1920, 1080, 144)], &[]);
        let rows = piece_rows(&s3, (1920, 2160), false, &[], &plan);
        assert_eq!(rows[0].status, PieceStatus::NeedsSlot);
    }

    #[test]
    fn overflow_reports_fallback() {
        let req: Vec<Mode> = (0..6).map(|i| Mode::new(1301 + i * 2, 1000, 60)).collect();
        let plan = modes::plan_slots(&req, &[]);
        assert_eq!(plan.overflow.len(), 1);
        let s = sc(LayoutKind::Columns, 1, 1);
        let m = plan.overflow[0];
        let plan2 = modes::plan_slots(&req, &[]);
        let rows = piece_rows(&s, (m.w as i32, m.h as i32), false, &[], &plan2);
        assert_eq!(
            rows[0].status,
            PieceStatus::Overflow(modes::fallback_for(m))
        );
    }

    #[test]
    fn main_piece_follows_order() {
        let mut s = sc(LayoutKind::Columns, 2, 1);
        s.order = vec![1, 0];
        s.main_piece = 0; // cell 0 is shown by virtual display 1
        let plan = modes::plan_slots(&[], &[]);
        let rows = piece_rows(&s, (3840, 1080), true, &[], &plan);
        assert!(!rows[0].main && rows[1].main);
    }

    #[test]
    fn validate_total_and_duplicates() {
        let t = vec![target(1, 1920, 2160, "ID"), target(2, 3440, 1440, "ID2")];
        let mut cfg = Config::default();
        cfg.splits.push(sc(LayoutKind::Rows, 1, 2));
        let mut b = sc(LayoutKind::Grid, 3, 3);
        b.display_id = "ID2".into();
        cfg.splits.push(b);
        // 2 + 9 > 8, but Grid is clamped to 4 per axis only by the UI; here 9
        let e = validate(&cfg, &t);
        assert!(e.iter().any(|m| m.contains("virtual displays")), "{e:?}");

        cfg.splits[1].layout = LayoutKind::Columns;
        cfg.splits[1].columns = 2;
        assert!(validate(&cfg, &t).is_empty());

        cfg.splits[1].display_id = "ID".into(); // same display as split 0
        let e = validate(&cfg, &t);
        assert!(e.iter().any(|m| m.contains("same display")), "{e:?}");

        // disabled duplicates are fine
        cfg.splits[1].enabled = false;
        assert!(validate(&cfg, &t).is_empty());

        // unknown display, no name: error
        cfg.splits[1].enabled = true;
        cfg.splits[1].display_id.clear();
        cfg.splits[1].name_match.clear();
        let e = validate(&cfg, &t);
        assert!(e.iter().any(|m| m.contains("choose a display")), "{e:?}");
    }

    #[test]
    fn validate_not_connected_duplicates() {
        let mut cfg = Config::default();
        cfg.splits.push(sc(LayoutKind::Rows, 1, 2));
        cfg.splits.push(sc(LayoutKind::Rows, 1, 2));
        let e = validate(&cfg, &[]);
        assert!(e.iter().any(|m| m.contains("same display")), "{e:?}");
    }

    #[test]
    fn validate_hotkeys() {
        let mut cfg = Config::default();
        assert!(validate(&cfg, &[]).is_empty());
        cfg.hotkeys.cycle_order.keys = cfg.hotkeys.toggle_all.keys.clone();
        assert!(validate(&cfg, &[]).iter().any(|m| m.contains("same keys")));
        cfg.hotkeys.cycle_order.enabled = false;
        assert!(validate(&cfg, &[]).is_empty());
        cfg.hotkeys.toggle_all.keys = "Ctrl+".into();
        assert!(!validate(&cfg, &[]).is_empty());
        cfg.hotkeys.toggle_all.keys = "S".into();
        assert!(validate(&cfg, &[]).iter().any(|m| m.contains("modifier")));
        cfg.hotkeys.enabled = false;
        assert!(validate(&cfg, &[]).is_empty());
    }

    #[test]
    fn slot_plan_uses_connected_displays() {
        let t = vec![target(1, 3440, 1440, "ID")];
        let mut cfg = Config::default();
        cfg.splits.push(sc(LayoutKind::Columns, 2, 1));
        let (req, plan) = slot_plan(&cfg, &t, &[]);
        assert_eq!(req, vec![Mode::new(1720, 1440, 60)]);
        assert_eq!(plan.to_register, req);
        // not connected: nothing required
        let (req, _) = slot_plan(&cfg, &[], &[]);
        assert!(req.is_empty());
    }
}
