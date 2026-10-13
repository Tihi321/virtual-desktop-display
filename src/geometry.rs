//! Pure geometry: how a display is cut into pieces.
//!
//! Pieces are rects RELATIVE to the display. Inner boundaries are rounded to
//! even pixels and the last piece of each axis absorbs the remainder. The
//! returned list is in VIRTUAL DISPLAY order: element `i` is the cell shown by
//! the i-th virtual display (`order[i]` = geometric cell, row-major).
use crate::config::LayoutKind;
use crate::displays::Rect;

/// Largest number of cells per axis honoured.
pub const MAX_AXIS: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub layout: LayoutKind,
    pub columns: u32,
    pub rows: u32,
    pub ratios_x: Vec<f64>,
    pub ratios_y: Vec<f64>,
    pub order: Vec<usize>,
}

impl Spec {
    pub fn new(layout: LayoutKind, columns: u32, rows: u32) -> Self {
        Self {
            layout,
            columns,
            rows,
            ratios_x: Vec::new(),
            ratios_y: Vec::new(),
            order: Vec::new(),
        }
    }
}

/// Normalised fractions (sum 1) for an axis; equal split if the custom
/// ratios are unusable (empty, non-finite, non-positive).
fn fractions(ratios: &[f64], n: usize) -> Vec<f64> {
    let ok = !ratios.is_empty() && ratios.iter().all(|r| r.is_finite() && *r > 0.0);
    if ok {
        let sum: f64 = ratios.iter().sum();
        return ratios.iter().map(|r| r / sum).collect();
    }
    vec![1.0 / n as f64; n]
}

/// Cells per axis `(columns, rows)` of a spec.
pub fn grid_dims(spec: &Spec) -> (usize, usize) {
    let clamp = |v: usize| v.clamp(1, MAX_AXIS);
    match spec.layout {
        LayoutKind::Columns => (clamp(spec.columns as usize), 1),
        LayoutKind::Rows => (1, clamp(spec.rows as usize)),
        LayoutKind::Grid => (clamp(spec.columns as usize), clamp(spec.rows as usize)),
        LayoutKind::Custom => (
            clamp(spec.ratios_x.len().max(1)),
            clamp(spec.ratios_y.len().max(1)),
        ),
    }
}

/// Number of pieces (= virtual displays) of a spec.
pub fn piece_count(spec: &Spec) -> usize {
    let (c, r) = grid_dims(spec);
    c * r
}

/// Round to the nearest even integer.
fn even(x: f64) -> i32 {
    ((x / 2.0).round() as i32) * 2
}

/// Boundaries `[0, b1, .., total]` of one axis.
fn boundaries(total: i32, fracs: &[f64]) -> Vec<i32> {
    let n = fracs.len();
    let mut b = vec![0];
    let mut cum = 0.0;
    for (i, f) in fracs.iter().enumerate().take(n - 1) {
        cum += f;
        let prev = *b.last().unwrap();
        let remaining = (n - 1 - i) as i32; // pieces still to place after this boundary
        let lo = prev + 1;
        let hi = (total - remaining).max(lo);
        b.push(even(total as f64 * cum).clamp(lo, hi));
    }
    b.push(total);
    b
}

/// True when `order` is a permutation of `0..n`.
pub fn is_valid_order(order: &[usize], n: usize) -> bool {
    if order.len() != n {
        return false;
    }
    let mut seen = vec![false; n];
    for &o in order {
        if o >= n || seen[o] {
            return false;
        }
        seen[o] = true;
    }
    true
}

/// `order` if it is a valid permutation of `0..n`, else the identity.
pub fn repair_order(order: &[usize], n: usize) -> Vec<usize> {
    if is_valid_order(order, n) {
        order.to_vec()
    } else {
        (0..n).collect()
    }
}

/// Rotate the order by one (for 2 pieces this is a swap):
/// virtual display `i` takes the cell shown by display `i + 1`.
pub fn rotate_order(order: &[usize], n: usize) -> Vec<usize> {
    let o = repair_order(order, n);
    (0..n).map(|i| o[(i + 1) % n]).collect()
}

/// Index of the virtual display that shows geometric `cell`.
pub fn virtual_index_of_cell(order: &[usize], cell: usize) -> usize {
    order.iter().position(|&c| c == cell).unwrap_or(0)
}

/// Geometric cells in row-major order, relative to the display.
pub fn cells(display_w: i32, display_h: i32, spec: &Spec) -> Vec<Rect> {
    let (cols, rows) = grid_dims(spec);
    let (fx, fy) = match spec.layout {
        LayoutKind::Custom => (
            fractions(&spec.ratios_x, cols),
            fractions(&spec.ratios_y, rows),
        ),
        _ => (fractions(&[], cols), fractions(&[], rows)),
    };
    let bx = boundaries(display_w, &fx);
    let by = boundaries(display_h, &fy);
    let mut v = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            v.push(Rect::new(
                bx[c],
                by[r],
                bx[c + 1] - bx[c],
                by[r + 1] - by[r],
            ));
        }
    }
    v
}

/// Pieces in virtual display order (the `order` permutation applied; an
/// invalid order is repaired to the identity).
pub fn pieces(display_w: i32, display_h: i32, spec: &Spec) -> Vec<Rect> {
    let cells = cells(display_w, display_h, spec);
    let order = repair_order(&spec.order, cells.len());
    order.iter().map(|&i| cells[i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(layout: LayoutKind, c: u32, r: u32) -> Spec {
        Spec::new(layout, c, r)
    }

    #[test]
    fn rows_flipgo() {
        let p = pieces(1920, 2160, &spec(LayoutKind::Rows, 1, 2));
        assert_eq!(
            p,
            vec![Rect::new(0, 0, 1920, 1080), Rect::new(0, 1080, 1920, 1080)]
        );
    }

    #[test]
    fn columns_ultrawide() {
        let p = pieces(3440, 1440, &spec(LayoutKind::Columns, 2, 1));
        assert_eq!(
            p,
            vec![Rect::new(0, 0, 1720, 1440), Rect::new(1720, 0, 1720, 1440)]
        );
    }

    #[test]
    fn single_piece_is_whole_display() {
        assert_eq!(
            pieces(2560, 1440, &spec(LayoutKind::Grid, 1, 1)),
            vec![Rect::new(0, 0, 2560, 1440)]
        );
        // zero counts are treated as one
        assert_eq!(pieces(100, 100, &spec(LayoutKind::Columns, 0, 0)).len(), 1);
    }

    #[test]
    fn grid_2x2_row_major() {
        let p = pieces(3840, 2160, &spec(LayoutKind::Grid, 2, 2));
        assert_eq!(
            p,
            vec![
                Rect::new(0, 0, 1920, 1080),
                Rect::new(1920, 0, 1920, 1080),
                Rect::new(0, 1080, 1920, 1080),
                Rect::new(1920, 1080, 1920, 1080),
            ]
        );
    }

    #[test]
    fn thirds_round_to_even_and_last_absorbs() {
        let p = pieces(1000, 100, &spec(LayoutKind::Columns, 3, 1));
        // 333.3 -> 334 (nearest even), 666.7 -> 666 (nearest even), remainder last
        assert_eq!(p[0].w, 334);
        assert_eq!(p[1].x, 334);
        assert_eq!(p[2].x, 666);
        assert_eq!(p[2].right(), 1000);
        for r in &p {
            assert_eq!(r.x % 2, 0);
        }
        assert_eq!(p.iter().map(|r| r.w).sum::<i32>(), 1000);
    }

    #[test]
    fn odd_total_remainder_in_last() {
        let p = pieces(1081, 11, &spec(LayoutKind::Rows, 1, 2));
        assert_eq!(p[0], Rect::new(0, 0, 1081, 6));
        assert_eq!(p[1], Rect::new(0, 6, 1081, 5));
    }

    #[test]
    fn custom_ratios() {
        let mut s = spec(LayoutKind::Custom, 0, 0);
        s.ratios_x = vec![0.25, 0.75];
        s.ratios_y = vec![0.5, 0.5];
        let p = pieces(3440, 1440, &s);
        assert_eq!(p.len(), 4);
        assert_eq!(p[0], Rect::new(0, 0, 860, 720));
        assert_eq!(p[1], Rect::new(860, 0, 2580, 720));
        assert_eq!(p[2], Rect::new(0, 720, 860, 720));
        assert_eq!(p[3], Rect::new(860, 720, 2580, 720));
    }

    #[test]
    fn custom_ratios_are_normalised_and_bad_ones_fall_back() {
        let mut s = spec(LayoutKind::Custom, 0, 0);
        s.ratios_x = vec![1.0, 3.0]; // sums to 4
        let p = pieces(1000, 100, &s);
        assert_eq!(p[0].w, 250);
        s.ratios_x = vec![1.0, -3.0];
        let p = pieces(1000, 100, &s);
        assert_eq!(p[0].w, 500);
        s.ratios_x = vec![f64::NAN, 1.0];
        assert_eq!(pieces(1000, 100, &s)[0].w, 500);
    }

    #[test]
    fn pieces_tile_the_display_without_overlap() {
        for (w, h, c, r) in [(1920, 1080, 3, 2), (3440, 1440, 4, 1), (777, 555, 3, 3)] {
            let ps = pieces(w, h, &spec(LayoutKind::Grid, c, r));
            assert_eq!(ps.len() as u32, c * r);
            let area: i64 = ps.iter().map(|p| p.w as i64 * p.h as i64).sum();
            assert_eq!(area, w as i64 * h as i64);
            for (i, a) in ps.iter().enumerate() {
                assert!(a.w > 0 && a.h > 0);
                for b in &ps[i + 1..] {
                    assert!(!a.intersects(b));
                }
            }
        }
    }

    #[test]
    fn order_permutes_pieces() {
        let mut s = spec(LayoutKind::Rows, 1, 2);
        s.order = vec![1, 0];
        let p = pieces(1920, 2160, &s);
        assert_eq!(p[0], Rect::new(0, 1080, 1920, 1080));
        assert_eq!(p[1], Rect::new(0, 0, 1920, 1080));
    }

    #[test]
    fn invalid_order_is_repaired() {
        for bad in [vec![], vec![0], vec![0, 0], vec![0, 2], vec![0, 1, 2]] {
            let mut s = spec(LayoutKind::Rows, 1, 2);
            s.order = bad.clone();
            assert_eq!(
                pieces(1920, 2160, &s),
                pieces(1920, 2160, &spec(LayoutKind::Rows, 1, 2)),
                "{bad:?}"
            );
        }
        assert_eq!(repair_order(&[2, 0, 1], 3), vec![2, 0, 1]);
        assert_eq!(repair_order(&[2, 0, 0], 3), vec![0, 1, 2]);
    }

    #[test]
    fn rotate_and_cell_lookup() {
        assert_eq!(rotate_order(&[0, 1], 2), vec![1, 0]);
        assert_eq!(rotate_order(&[1, 0], 2), vec![0, 1]);
        assert_eq!(rotate_order(&[0, 1, 2], 3), vec![1, 2, 0]);
        assert_eq!(rotate_order(&[], 3), vec![1, 2, 0]);
        // n rotations return to the start
        let mut o = vec![0, 1, 2, 3];
        for _ in 0..4 {
            o = rotate_order(&o, 4);
        }
        assert_eq!(o, vec![0, 1, 2, 3]);
        assert_eq!(virtual_index_of_cell(&[1, 0], 0), 1);
        assert_eq!(virtual_index_of_cell(&[1, 0], 1), 0);
    }

    #[test]
    fn piece_count_matches() {
        assert_eq!(piece_count(&spec(LayoutKind::Grid, 3, 2)), 6);
        assert_eq!(piece_count(&spec(LayoutKind::Rows, 5, 2)), 2);
        let mut s = spec(LayoutKind::Custom, 0, 0);
        s.ratios_x = vec![1.0, 1.0, 1.0];
        assert_eq!(piece_count(&s), 3);
    }
}
