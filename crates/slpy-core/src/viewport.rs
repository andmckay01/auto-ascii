//! Viewport / letterbox math (PLAN §3.2).
//!
//! Inputs: terminal `cols × rows` and cell aspect `a = cell_h_px / cell_w_px`
//! (from `CSI 16 t` or `TIOCGWINSZ` px fields; fallback 2.0; re-queried on every
//! resize since font zoom changes it). Target is the ASSET's picture aspect
//! `P = aspect_num / aspect_den` from the SLPY header (PLAN §4; 16:9 for all
//! production assets), so the column/row ratio is `R = P · a`
//! (P = 16/9, a = 2 → R ≈ 3.5556). [`compute_viewport`] is the 16:9
//! convenience wrapper; [`compute_viewport_for`] takes the header aspect.
//!
//! Worked examples at 16:9 (frozen as unit tests): 80×24 → 80×23;
//! 213×58 → 206×58 (pads L3/R4/T0/B0); 320×90 → exact fit.

/// Fallback cell aspect when the terminal reports no pixel size (PLAN §3.2).
pub const DEFAULT_CELL_ASPECT: f64 = 2.0;

/// Below this terminal size the player renders a centered "enlarge terminal"
/// card instead of video (PLAN §3.2): `compute_viewport` returns `None`.
pub const MIN_COLS: u16 = 32;
/// See [`MIN_COLS`].
pub const MIN_ROWS: u16 = 9;

/// A letterboxed viewport inside the terminal grid (PLAN §3.2), targeting
/// the asset's picture aspect (16:9 for production assets).
///
/// Invariants (fuzzed at M2, PLAN §6): `cols + pad_left + pad_right == term cols`,
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
/// terminal with cell aspect `cell_aspect` (PLAN §3.2) — the convenience
/// wrapper over [`compute_viewport_for`] for the production 16:9 aspect.
pub fn compute_viewport(term_cols: u16, term_rows: u16, cell_aspect: f64) -> Option<Viewport> {
    compute_viewport_for(term_cols, term_rows, cell_aspect, 16, 9)
}

/// Compute the letterboxed viewport for a `term_cols × term_rows` terminal
/// with cell aspect `cell_aspect`, targeting the asset's picture aspect
/// `aspect_num : aspect_den` (the SLPY header's `aspect_num/den`, PLAN §4 —
/// the M5 fix for the M4-low "compute_viewport hard-codes 16:9").
///
/// Candidate selection (PLAN §3.2 generalized): with `R = (num/den) · a`,
/// width-limited `(cols, round(cols/R))` vs height-limited
/// `(round(rows·R), rows)`; the valid candidate minimizing the log aspect
/// error `|ln((c/(r·a))·den/num)|` wins. Pads split centered, remainder
/// right/bottom. For `(16, 9)` this is bit-for-bit the pre-M5 math — the
/// 16:9 goldens cannot shift.
///
/// Returns `None` when the terminal is smaller than [`MIN_COLS`]×[`MIN_ROWS`]
/// (caller renders the "enlarge terminal" card). Non-finite or non-positive
/// `cell_aspect` falls back to [`DEFAULT_CELL_ASPECT`]; a zero
/// `aspect_num`/`aspect_den` falls back to 16:9 (the header default). All
/// divisions clamp the viewport to ≥ 1×1 — no division by zero, ever.
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
    let a = if cell_aspect.is_finite() && cell_aspect > 0.0 {
        cell_aspect
    } else {
        DEFAULT_CELL_ASPECT
    };
    // Degenerate header aspect → the 16:9 default (same fallback the facade
    // documents for aspect_den == 0 headers).
    let (num, den) = if aspect_num == 0 || aspect_den == 0 { (16, 9) } else { (aspect_num, aspect_den) };
    let (num, den) = (f64::from(num), f64::from(den));
    let r_ratio = (num / den) * a;

    // log aspect error of a (c, r) candidate: |ln((c/(r*a)) * den/num)|
    let aspect_err = |c: u16, r: u16| -> f64 {
        ((c as f64 / (r as f64 * a)) * (den / num)).ln().abs()
    };
    let clamp1 = |v: f64| -> u16 { (v.round().max(1.0)) as u16 };

    let r1 = clamp1(term_cols as f64 / r_ratio);
    let cand1_valid = r1 <= term_rows; // width-limited
    let c2 = clamp1(term_rows as f64 * r_ratio);
    let cand2_valid = c2 <= term_cols; // height-limited

    let (c, r) = match (cand1_valid, cand2_valid) {
        (true, false) => (term_cols, r1),
        (false, true) => (c2, term_rows),
        (true, true) => {
            if aspect_err(term_cols, r1) <= aspect_err(c2, term_rows) {
                (term_cols, r1)
            } else {
                (c2, term_rows)
            }
        }
        // Defensive: rounding can in principle push both candidates out of
        // range; clamp to the terminal (fuzz invariant: viewport ⊆ terminal).
        (false, false) => (c2.min(term_cols), r1.min(term_rows)),
    };

    let pad_l = (term_cols - c) >> 1;
    let pad_t = (term_rows - r) >> 1;
    Some(Viewport {
        cols: c,
        rows: r,
        pad_left: pad_l,
        pad_right: term_cols - c - pad_l,
        pad_top: pad_t,
        pad_bottom: term_rows - r - pad_t,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PLAN §3.2 worked examples, a = 2.0.
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

    /// Property-style edge cases: tiny, huge, and extreme aspects — no panics,
    /// viewport always fits, pads always centered ±1 (M0 slice of the §6 fuzz).
    #[test]
    fn edge_cases_never_panic_and_always_fit() {
        // Tiny terminals (incl. 1×1) → "enlarge terminal" card, never a panic.
        for (c, r) in [(1u16, 1u16), (0, 0), (31, 9), (32, 8), (2, 1000), (1000, 2)] {
            assert_eq!(compute_viewport(c, r, 2.0), None, "{c}x{r} should be too small");
        }

        // Sweep sizes × aspects, including absurd aspects and u16::MAX dims.
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
                // Centered, remainder right/bottom (§3.2).
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

    /// `compute_viewport` is exactly the 16:9 case of `compute_viewport_for`
    /// — same math, same bits, so the 16:9 goldens cannot shift (M5 fix 2).
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

    /// Non-16:9 worked examples (M5 fix 2): a 4:3 asset at a=2 has
    /// R = (4/3)·2 ≈ 2.667, a 9:16 portrait asset R = (9/16)·2 = 1.125.
    #[test]
    fn non_16_9_worked_examples() {
        // 4:3 at 80×24: width-limited candidate r1 = round(80/2.667) = 30
        // (> 24, invalid), height-limited c2 = round(24·2.667) = 64 wins.
        let v = compute_viewport_for(80, 24, 2.0, 4, 3).unwrap();
        assert_eq!((v.cols, v.rows), (64, 24));
        assert_eq!((v.pad_left, v.pad_right, v.pad_top, v.pad_bottom), (8, 8, 0, 0));

        // 4:3 at 40×40: width-limited (40, round(40/2.667) = 15) wins.
        let v = compute_viewport_for(40, 40, 2.0, 4, 3).unwrap();
        assert_eq!((v.cols, v.rows), (40, 15));
        assert_eq!((v.pad_left, v.pad_right, v.pad_top, v.pad_bottom), (0, 0, 12, 13));

        // 9:16 portrait at 80×24: c2 = round(24·1.125) = 27 wins.
        let v = compute_viewport_for(80, 24, 2.0, 9, 16).unwrap();
        assert_eq!((v.cols, v.rows), (27, 24));
        assert_eq!((v.pad_left, v.pad_right, v.pad_top, v.pad_bottom), (26, 27, 0, 0));

        // Exact fit: 4:3 at 64×24 (a=2 → R = 8/3, 24·8/3 = 64).
        let v = compute_viewport_for(64, 24, 2.0, 4, 3).unwrap();
        assert_eq!((v.cols, v.rows), (64, 24));
        assert_eq!((v.pad_left, v.pad_right, v.pad_top, v.pad_bottom), (0, 0, 0, 0));
    }

    /// The §6 invariants (fit, tiling, centered pads, aspect error minimal
    /// among the two candidates) hold for arbitrary picture aspects.
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

                    // Aspect error minimal-among-candidates, from the spec
                    // formula (not the implementation).
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

    /// Zero header aspect fields fall back to 16:9 (degenerate headers).
    #[test]
    fn zero_picture_aspect_falls_back_to_16_9() {
        assert_eq!(compute_viewport_for(80, 24, 2.0, 0, 9), compute_viewport(80, 24, 2.0));
        assert_eq!(compute_viewport_for(80, 24, 2.0, 16, 0), compute_viewport(80, 24, 2.0));
    }
}
