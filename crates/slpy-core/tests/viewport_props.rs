//! Property tests on `compute_viewport` directly (PLAN §3.2/§6, M2 item D):
//! the letterbox/aspect invariants as cheap unit properties, so math drift
//! is caught here before the full SimBackend storm fuzz even runs.
//!
//! Case count: proptest's default 256, env-scalable via `PROPTEST_CASES`
//! (read by `ProptestConfig::default()`).

use proptest::prelude::*;
use slpy_core::{DEFAULT_CELL_ASPECT, MIN_COLS, MIN_ROWS, Viewport, compute_viewport};

/// PLAN §3.2 candidate math, replicated from the spec (not from the
/// implementation): width-limited `(cols, round(cols/R))` vs height-limited
/// `(round(rows·R), rows)`, log aspect error `|ln((c/(r·a))·9/16)|`.
struct Candidates {
    cand1: (u16, u16),
    cand1_valid: bool,
    cand2: (u16, u16),
    cand2_valid: bool,
    a: f64,
}

fn spec_candidates(cols: u16, rows: u16, aspect: f64) -> Candidates {
    let a = if aspect.is_finite() && aspect > 0.0 { aspect } else { DEFAULT_CELL_ASPECT };
    let r_ratio = (16.0 / 9.0) * a;
    let clamp1 = |v: f64| -> u16 { (v.round().max(1.0)) as u16 };
    let r1 = clamp1(f64::from(cols) / r_ratio);
    let c2 = clamp1(f64::from(rows) * r_ratio);
    Candidates {
        cand1: (cols, r1),
        cand1_valid: r1 <= rows,
        cand2: (c2, rows),
        cand2_valid: c2 <= cols,
        a,
    }
}

fn log_aspect_err(c: u16, r: u16, a: f64) -> f64 {
    ((f64::from(c) / (f64::from(r) * a)) * (9.0 / 16.0)).ln().abs()
}

/// The full §6 invariant set on one result. Returns Err via prop_assert.
fn check_invariants(
    cols: u16,
    rows: u16,
    aspect: f64,
    vp: Option<Viewport>,
) -> Result<(), TestCaseError> {
    let Some(v) = vp else {
        // None exactly and only below the 32×9 minimum.
        prop_assert!(
            cols < MIN_COLS || rows < MIN_ROWS,
            "None for {cols}x{rows} which is >= {MIN_COLS}x{MIN_ROWS}"
        );
        return Ok(());
    };
    prop_assert!(cols >= MIN_COLS && rows >= MIN_ROWS);

    // Viewport within terminal, ≥ 1×1.
    prop_assert!(v.cols >= 1 && v.rows >= 1);
    prop_assert!(v.cols <= cols && v.rows <= rows, "viewport must fit: {v:?} in {cols}x{rows}");

    // Pads + viewport tile the terminal exactly.
    prop_assert_eq!(v.cols + v.pad_left + v.pad_right, cols);
    prop_assert_eq!(v.rows + v.pad_top + v.pad_bottom, rows);

    // Letterbox pads symmetric ±1, remainder on the right/bottom (§3.2).
    prop_assert!(
        v.pad_right == v.pad_left || v.pad_right == v.pad_left + 1,
        "h-pads not centered: {v:?}"
    );
    prop_assert!(
        v.pad_bottom == v.pad_top || v.pad_bottom == v.pad_top + 1,
        "v-pads not centered: {v:?}"
    );

    // Aspect error minimal-among-candidates (§6): the chosen viewport is one
    // of the two spec candidates (or the both-invalid clamped fallback), and
    // when candidates are valid its log aspect error is the minimum.
    let cand = spec_candidates(cols, rows, aspect);
    let chosen = (v.cols, v.rows);
    match (cand.cand1_valid, cand.cand2_valid) {
        (true, false) => prop_assert_eq!(chosen, cand.cand1),
        (false, true) => prop_assert_eq!(chosen, cand.cand2),
        (true, true) => {
            prop_assert!(chosen == cand.cand1 || chosen == cand.cand2);
            let err = log_aspect_err(chosen.0, chosen.1, cand.a);
            let best = log_aspect_err(cand.cand1.0, cand.cand1.1, cand.a)
                .min(log_aspect_err(cand.cand2.0, cand.cand2.1, cand.a));
            prop_assert!(
                err <= best + 1e-12,
                "chosen {chosen:?} err {err} > best {best} for {cols}x{rows} a={aspect}"
            );
        }
        // Defensive rounding corner: both candidates out of range — the
        // implementation clamps; fitting was asserted above.
        (false, false) => {}
    }
    Ok(())
}

proptest! {
    /// The §6 fuzz dimension range at cell-aspect 2.0 (the storm fuzz's
    /// fixed aspect) and varied aspects 0.1..=8.0.
    #[test]
    fn viewport_invariants_hold(
        cols in 1u16..=1000,
        rows in 1u16..=1000,
        aspect_milli in 100u32..=8000,
    ) {
        let aspect = f64::from(aspect_milli) / 1000.0;
        check_invariants(cols, rows, aspect, compute_viewport(cols, rows, aspect))?;
        // The storm fuzz's fixed aspect too.
        check_invariants(cols, rows, 2.0, compute_viewport(cols, rows, 2.0))?;
    }

    /// Full u16 range (terminals lie): no panic, invariants hold to 65535.
    #[test]
    fn viewport_invariants_hold_extreme_dims(
        cols in prop_oneof![1u16..=u16::MAX, Just(u16::MAX), Just(1u16)],
        rows in prop_oneof![1u16..=u16::MAX, Just(u16::MAX), Just(1u16)],
    ) {
        check_invariants(cols, rows, 2.0, compute_viewport(cols, rows, 2.0))?;
    }

    /// Non-finite / non-positive aspects fall back to the default rather
    /// than poisoning the math (PLAN §3.2).
    #[test]
    fn degenerate_aspect_falls_back(cols in 32u16..=1000, rows in 9u16..=1000) {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -2.0] {
            prop_assert_eq!(
                compute_viewport(cols, rows, bad),
                compute_viewport(cols, rows, DEFAULT_CELL_ASPECT)
            );
        }
    }
}
