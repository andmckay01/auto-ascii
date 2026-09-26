//! Viewport / letterbox math.
//!
//! Inputs: terminal `cols × rows` and cell aspect `a = cell_h_px / cell_w_px`
//! (from `CSI 16 t` or `TIOCGWINSZ` px fields; fallback 2.0; re-queried on every
//! resize since font zoom changes it). Target is the ASSET's picture aspect
//! `P = aspect_num / aspect_den` from the ASCI header (16:9 for all production
//! assets), so the column/row ratio is `R = P · a` (P = 16/9, a = 2 →
//! R ≈ 3.5556). [`compute_viewport`] is the 16:9 convenience wrapper;
//! [`compute_viewport_for`] takes the header aspect.
//!
//! Worked examples at 16:9 (frozen as unit tests): 80×24 → 80×23;
//! 213×58 → 206×58 (pads L3/R4/T0/B0); 320×90 → exact fit.

/// Fallback cell aspect when the terminal reports no pixel size.
pub const DEFAULT_CELL_ASPECT: f64 = 2.0;

/// Below this terminal size the player renders a centered "enlarge terminal"
/// card instead of video: `compute_viewport` returns `None`.
pub const MIN_COLS: u16 = 32;
/// See [`MIN_COLS`].
pub const MIN_ROWS: u16 = 9;

/// A letterboxed viewport inside the terminal grid, targeting the asset's
/// picture aspect (16:9 for production assets).
///
/// Invariants: `cols + pad_left + pad_right == term cols`,
/// `rows + pad_top + pad_bottom == term rows`, pads symmetric ±1 with the
/// remainder on the right/bottom, `cols ≥ 1`, `rows ≥ 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    /// Video area width in cells (`Vc`).
    pub cols: u16,
    /// Video area height in cells (`Vr`).
    pub rows: u16,
    pub pad_left: u16,
    pub pad_right: u16,
    pub pad_top: u16,
    pub pad_bottom: u16,
}

/// Compute the letterboxed **16:9** viewport for a `term_cols × term_rows`
/// terminal with cell aspect `cell_aspect` — the convenience wrapper over
/// [`compute_viewport_for`] for the production 16:9 aspect.
pub fn compute_viewport(term_cols: u16, term_rows: u16, cell_aspect: f64) -> Option<Viewport> {
    compute_viewport_for(term_cols, term_rows, cell_aspect, 16, 9)
}

pub fn compute_viewport_for(
    term_cols: u16,
    term_rows: u16,
    cell_aspect: f64,
    aspect_num: u16,
    aspect_den: u16,
) -> Option<Viewport> {
    if term_cols < MIN_COLS || term_rows < MIN_ROWS {
        return None;
    }
    let cell_aspect = if cell_aspect.is_finite() && cell_aspect > 0.0 {
        cell_aspect
    } else {
        DEFAULT_CELL_ASPECT
    };
    let (num, den) = if aspect_num == 0 || aspect_den == 0 { (16, 9) } else { (aspect_num, aspect_den) };
    let (num, den) = (f64::from(num), f64::from(den));
    let cols_per_row = (num / den) * cell_aspect;

    let log_aspect_error = |c: u16, r: u16| -> f64 {
        ((c as f64 / (r as f64 * cell_aspect)) * (den / num)).ln().abs()
    };
    let rounded_nonzero_cells = |v: f64| -> u16 { (v.round().max(1.0)) as u16 };

    let width_limited_rows = rounded_nonzero_cells(term_cols as f64 / cols_per_row);
    let width_limited_fits = width_limited_rows <= term_rows;
    let height_limited_cols = rounded_nonzero_cells(term_rows as f64 * cols_per_row);
    let height_limited_fits = height_limited_cols <= term_cols;

    let width_limited = (term_cols, width_limited_rows);
    let height_limited = (height_limited_cols, term_rows);
    let (c, r) = match (width_limited_fits, height_limited_fits) {
        (true, false) => width_limited,
        (false, true) => height_limited,
        (true, true) => {
            if log_aspect_error(width_limited.0, width_limited.1)
                <= log_aspect_error(height_limited.0, height_limited.1)
            {
                width_limited
            } else {
                height_limited
            }
        }
        (false, false) => (height_limited_cols.min(term_cols), width_limited_rows.min(term_rows)),
    };

    let pad_left = (term_cols - c) >> 1;
    let pad_top = (term_rows - r) >> 1;
    Some(Viewport {
        cols: c,
        rows: r,
        pad_left,
        pad_right: term_cols - c - pad_left,
        pad_top,
        pad_bottom: term_rows - r - pad_top,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worked_examples() {
        let v = compute_viewport(80, 24, 2.0).unwrap();
        assert_eq!((v.cols, v.rows), (80, 23));

        let v = compute_viewport(213, 58, 2.0).unwrap();
        assert_eq!((v.cols, v.rows), (206, 58));
        assert_eq!(
            (v.pad_left, v.pad_right, v.pad_top, v.pad_bottom),
            (3, 4, 0, 0)
        );

        let v = compute_viewport(320, 90, 2.0).unwrap();
        assert_eq!((v.cols, v.rows), (320, 90));
        assert_eq!(
            (v.pad_left, v.pad_right, v.pad_top, v.pad_bottom),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn too_small_renders_card() {
        assert_eq!(compute_viewport(31, 40, 2.0), None);
        assert_eq!(compute_viewport(100, 8, 2.0), None);
        assert!(compute_viewport(32, 9, 2.0).is_some());
    }

    #[test]
    fn pads_and_bounds_hold() {
        for (c, r) in [(32u16, 9u16), (80, 24), (137, 41), (999, 1000), (320, 90)] {
            let v = compute_viewport(c, r, 2.0).unwrap();
            assert!(v.cols >= 1 && v.rows >= 1);
            assert!(v.cols <= c && v.rows <= r);
            assert_eq!(v.cols + v.pad_left + v.pad_right, c);
            assert_eq!(v.rows + v.pad_top + v.pad_bottom, r);
            assert!(v.pad_right as i32 - v.pad_left as i32 <= 1);
            assert!(v.pad_bottom as i32 - v.pad_top as i32 <= 1);
        }
    }

    #[test]
    fn edge_cases_never_panic_and_always_fit() {
        for (c, r) in [(1u16, 1u16), (0, 0), (31, 9), (32, 8), (2, 1000), (1000, 2)] {
            assert_eq!(compute_viewport(c, r, 2.0), None, "{c}x{r} should be too small");
        }

        let sizes = [
            (32u16, 9u16),
            (33, 10),
            (80, 24),
            (213, 58),
            (320, 90),
            (1000, 1000),
            (65535, 9),
            (32, 65535),
            (65535, 65535),
        ];
        let aspects = [0.001, 0.1, 0.5, 1.0, 2.0, 3.7, 10.0, 1000.0];
        for &(c, r) in &sizes {
            for &a in &aspects {
                let v = compute_viewport(c, r, a)
                    .unwrap_or_else(|| panic!("None for {c}x{r} a={a}"));
                assert!(v.cols >= 1 && v.rows >= 1, "{c}x{r} a={a}");
                assert!(v.cols <= c && v.rows <= r, "viewport ⊆ terminal, {c}x{r} a={a}");
                assert_eq!(v.cols + v.pad_left + v.pad_right, c, "{c}x{r} a={a}");
                assert_eq!(v.rows + v.pad_top + v.pad_bottom, r, "{c}x{r} a={a}");
                assert!(
                    v.pad_right == v.pad_left || v.pad_right == v.pad_left + 1,
                    "{c}x{r} a={a}"
                );
                assert!(
                    v.pad_bottom == v.pad_top || v.pad_bottom == v.pad_top + 1,
                    "{c}x{r} a={a}"
                );
            }
        }
    }

    #[test]
    fn bad_aspect_falls_back() {
        assert_eq!(
            compute_viewport(80, 24, f64::NAN),
            compute_viewport(80, 24, DEFAULT_CELL_ASPECT)
        );
        assert_eq!(
            compute_viewport(80, 24, -1.0),
            compute_viewport(80, 24, DEFAULT_CELL_ASPECT)
        );
    }

    #[test]
    fn wrapper_is_the_16_9_case() {
        for (c, r) in [(32u16, 9u16), (80, 24), (213, 58), (320, 90), (999, 1000)] {
            for a in [0.5, 1.0, 2.0, 3.7] {
                assert_eq!(
                    compute_viewport(c, r, a),
                    compute_viewport_for(c, r, a, 16, 9),
                    "{c}x{r} a={a}"
                );
            }
        }
    }

    #[test]
    fn non_16_9_worked_examples() {
        let v = compute_viewport_for(80, 24, 2.0, 4, 3).unwrap();
        assert_eq!((v.cols, v.rows), (64, 24));
        assert_eq!((v.pad_left, v.pad_right, v.pad_top, v.pad_bottom), (8, 8, 0, 0));

        let v = compute_viewport_for(40, 40, 2.0, 4, 3).unwrap();
        assert_eq!((v.cols, v.rows), (40, 15));
        assert_eq!((v.pad_left, v.pad_right, v.pad_top, v.pad_bottom), (0, 0, 12, 13));

        let v = compute_viewport_for(80, 24, 2.0, 9, 16).unwrap();
        assert_eq!((v.cols, v.rows), (27, 24));
        assert_eq!((v.pad_left, v.pad_right, v.pad_top, v.pad_bottom), (26, 27, 0, 0));

        let v = compute_viewport_for(64, 24, 2.0, 4, 3).unwrap();
        assert_eq!((v.cols, v.rows), (64, 24));
        assert_eq!((v.pad_left, v.pad_right, v.pad_top, v.pad_bottom), (0, 0, 0, 0));
    }

    #[test]
    fn non_16_9_invariants_hold() {
        let aspects: [(u16, u16); 6] = [(4, 3), (9, 16), (1, 1), (21, 9), (2, 3), (64, 27)];
        let sizes = [(32u16, 9u16), (80, 24), (137, 41), (213, 58), (320, 90), (1000, 47)];
        for &(num, den) in &aspects {
            let pic = f64::from(num) / f64::from(den);
            for &(c, r) in &sizes {
                for a in [1.0, 2.0, 2.5] {
                    let v = compute_viewport_for(c, r, a, num, den)
                        .unwrap_or_else(|| panic!("None for {c}x{r} {num}:{den}"));
                    assert!(v.cols >= 1 && v.rows >= 1 && v.cols <= c && v.rows <= r);
                    assert_eq!(v.cols + v.pad_left + v.pad_right, c);
                    assert_eq!(v.rows + v.pad_top + v.pad_bottom, r);
                    assert!(v.pad_right == v.pad_left || v.pad_right == v.pad_left + 1);
                    assert!(v.pad_bottom == v.pad_top || v.pad_bottom == v.pad_top + 1);

                    let err = |cc: u16, rr: u16| {
                        ((f64::from(cc) / (f64::from(rr) * a)) / pic).ln().abs()
                    };
                    let r_ratio = pic * a;
                    let clamp1 = |x: f64| -> u16 { (x.round().max(1.0)) as u16 };
                    let r1 = clamp1(f64::from(c) / r_ratio);
                    let c2 = clamp1(f64::from(r) * r_ratio);
                    let mut best = f64::INFINITY;
                    if r1 <= r {
                        best = best.min(err(c, r1));
                    }
                    if c2 <= c {
                        best = best.min(err(c2, r));
                    }
                    if best.is_finite() {
                        assert!(
                            err(v.cols, v.rows) <= best + 1e-12,
                            "aspect error not minimal for {c}x{r} {num}:{den} a={a}: \
                             chose {}x{}",
                            v.cols,
                            v.rows
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn zero_picture_aspect_falls_back_to_16_9() {
        assert_eq!(compute_viewport_for(80, 24, 2.0, 0, 9), compute_viewport(80, 24, 2.0));
        assert_eq!(compute_viewport_for(80, 24, 2.0, 16, 0), compute_viewport(80, 24, 2.0));
    }
}
