//! Hysteresis state — the flicker killer (PLAN §3.5): per-cell ramp-index
//! hysteresis (switch only past a step boundary ± 0.35·step), the temporal
//! dual-threshold edge gate (Canny-style T_on/T_off with a `was_edge` buffer),
//! and the previous orientation bin for the 8° guard.
//!
//! All state lives in one `Grid<CellState>` (3 B/cell): [`HysteresisState::reset`]
//! is the scene-cut reset (no realloc), [`HysteresisState::resize`] the
//! resize-path realloc+reset (PLAN §3.5 graft from C). Nothing here allocates
//! outside `new`/`resize`.

use crate::grid::Grid;
use crate::orient::BIN_UNSET;

/// Sentinel for "no previous ramp index" (fresh cell / after reset).
pub const IDX_UNSET: u8 = 0xFF;

/// Default index hysteresis width in Q8 fractions of one ramp step:
/// round(0.35 · 256) = 90 (PLAN §3.5 "boundary ± 0.35·step"). The live
/// width is `ComposeParams::idx_hyst_q8` (M3 Tune: promoted to params so
/// the sweep harness can trade stickiness vs responsiveness); this constant
/// remains the documented spec default.
pub const IDX_HYST_Q8: u32 = 90;

/// Per-cell hysteresis flags. Bits 0–1 are the shared gates below; bits 2–7
/// are codec-private temporal memory (a [`crate::codec`] may use them freely —
/// a codec switch resets all state, so no bit outlives the codec that set it).
pub mod cell_flags {
    /// The edge gate was on last frame (dual-threshold memory).
    pub const WAS_EDGE: u8 = 1;
    /// The quadrant-refinement magnitude gate was on last frame — its own
    /// dual-threshold memory, on a far lower band than [`WAS_EDGE`] (see
    /// `ComposeParams::quad_e_on`). Separate flag because the two gates
    /// arm independently: a cell can be well below the edge gate and still
    /// carry real sub-cell diagonal structure.
    pub const WAS_QUADRANT: u8 = 1 << 1;
}

/// Per-cell temporal state: previous ramp index, orientation bin, flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellState {
    /// Previous effective ramp index ([`IDX_UNSET`] = none).
    pub idx: u8,
    /// Previous orientation bin ([`BIN_UNSET`] = none).
    pub bin: u8,
    /// See [`cell_flags`].
    pub flags: u8,
}

impl Default for CellState {
    #[inline]
    fn default() -> CellState {
        CellState { idx: IDX_UNSET, bin: BIN_UNSET, flags: 0 }
    }
}

/// All per-cell temporal state for one viewport, sized `cols × rows`
/// (viewport cells, not terminal cells — pads carry no state).
#[derive(Clone, Debug)]
pub struct HysteresisState {
    cells: Grid<CellState>,
}

impl HysteresisState {
    pub fn new(cols: u16, rows: u16) -> HysteresisState {
        HysteresisState { cells: Grid::new(cols, rows) }
    }

    #[inline]
    pub fn cols(&self) -> u16 {
        self.cells.cols()
    }

    #[inline]
    pub fn rows(&self) -> u16 {
        self.cells.rows()
    }

    /// Scene-cut reset (PLAN §3.5: cut flags from the asset reset ALL
    /// hysteresis state, else ghosting across cuts). No allocation.
    pub fn reset(&mut self) {
        self.cells.fill(CellState::default());
    }

    /// Resize-path realloc + reset (PLAN §3.5 graft from C: "all
    /// hysteresis/temporal state is reset and reallocated on resize").
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cells.resize(cols, rows);
    }

    /// Read one cell's state (tests/invariants).
    #[inline]
    pub fn cell(&self, col: u16, row: u16) -> CellState {
        self.cells.get(col, row)
    }

    /// Mutable access for the compositor.
    #[inline]
    pub fn cell_mut(&mut self, col: u16, row: u16) -> &mut CellState {
        let i = row as usize * self.cells.cols() as usize + col as usize;
        &mut self.cells.as_mut_slice()[i]
    }
}

/// Ramp-index hysteresis (PLAN §3.5): quantize `n` (0..=255) onto `len` steps,
/// but move off `prev` only when the position crosses the old step's boundary
/// by more than `hyst_q8` Q8 fractions of a step (spec default
/// [`IDX_HYST_Q8`] = 0.35·step; the compositor passes
/// `ComposeParams::idx_hyst_q8`). `prev` = [`IDX_UNSET`] (or any value ≥
/// `len`, e.g. after a palette/density change without reset) quantizes
/// plainly. `hyst_q8` must be < 256 (a full step) — u8-sourced by contract.
#[inline]
pub fn hysteresis_idx(n: u8, len: u8, prev: u8, hyst_q8: u32) -> u8 {
    debug_assert!(len >= 1);
    debug_assert!(hyst_q8 < 256);
    let p = n as u32 * len as u32; // position in Q8 step units
    if prev >= len {
        return (p >> 8) as u8;
    }
    let lo = prev as u32 * 256;
    let hi = lo + 256;
    if p >= hi + hyst_q8 {
        ((p - hyst_q8) >> 8) as u8
    } else if p + hyst_q8 < lo {
        ((p + hyst_q8) >> 8) as u8
    } else {
        prev
    }
}

/// Temporal dual-threshold edge gate (PLAN §3.5, Canny-style):
/// `e > T_on || (was_edge && e > T_off)`.
#[inline]
pub fn edge_gate(e: u8, was_edge: bool, t_on: u8, t_off: u8) -> bool {
    e > t_on || (was_edge && e > t_off)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M3 acceptance 5: idx boundary ± 0.35·step, exact in Q8 (90/256).
    #[test]
    fn idx_boundary_plus_minus_035() {
        // len 10: step boundaries at multiples of 256 in p = n·10 units.
        // Sitting at idx 4 (p ∈ [1024, 1280)).
        assert_eq!(hysteresis_idx(110, 10, IDX_UNSET, IDX_HYST_Q8), 4); // p = 1100, seed

        // Up-switch needs p ≥ 1280 + 90 = 1370 → n = 137.
        assert_eq!(hysteresis_idx(136, 10, 4, IDX_HYST_Q8), 4, "0.31 past boundary holds");
        assert_eq!(hysteresis_idx(137, 10, 4, IDX_HYST_Q8), 5, "0.35+ past boundary moves");

        // Down-switch needs p + 90 < 1024 → n ≤ 93.
        assert_eq!(hysteresis_idx(94, 10, 4, IDX_HYST_Q8), 4, "0.33 below boundary holds");
        assert_eq!(hysteresis_idx(93, 10, 4, IDX_HYST_Q8), 3, "0.36 below boundary moves");

        // Multi-step jumps land 0.35 short of the plain quantize.
        assert_eq!(hysteresis_idx(255, 10, 0, IDX_HYST_Q8), 9);
        assert_eq!(hysteresis_idx(0, 10, 9, IDX_HYST_Q8), 0);
    }

    /// The width is a live parameter (M3 Tune): wider holds longer, zero
    /// degenerates to plain quantization at the boundary.
    #[test]
    fn idx_width_is_parametric() {
        // Same cell as above, width 128 (0.5 step): up-switch needs
        // p ≥ 1280 + 128 → n = 141; width 0 switches exactly at n = 128.
        assert_eq!(hysteresis_idx(140, 10, 4, 128), 4, "0.47 past boundary holds at width 128");
        assert_eq!(hysteresis_idx(141, 10, 4, 128), 5, "0.5+ past boundary moves at width 128");
        assert_eq!(hysteresis_idx(128, 10, 4, 0), 5, "width 0 = plain step");
        assert_eq!(hysteresis_idx(127, 10, 4, 0), 4);
        // Down direction at width 128: needs p + 128 < 1024 → n ≤ 89.
        assert_eq!(hysteresis_idx(90, 10, 4, 128), 4);
        assert_eq!(hysteresis_idx(89, 10, 4, 128), 3);
    }

    #[test]
    fn idx_unset_or_stale_prev_quantizes_plainly() {
        assert_eq!(hysteresis_idx(255, 16, IDX_UNSET, IDX_HYST_Q8), 15);
        assert_eq!(hysteresis_idx(0, 16, IDX_UNSET, IDX_HYST_Q8), 0);
        // prev from a longer ramp than the current one → treated as unset.
        assert_eq!(hysteresis_idx(128, 8, 12, IDX_HYST_Q8), 4);
        // len 1 is degenerate but legal.
        assert_eq!(hysteresis_idx(200, 1, IDX_UNSET, IDX_HYST_Q8), 0);
    }

    /// M3 acceptance 5: dual-threshold edge gate with was_edge memory.
    #[test]
    fn edge_dual_threshold() {
        let (t_on, t_off) = (96, 48);
        assert!(!edge_gate(96, false, t_on, t_off), "T_on is strict");
        assert!(edge_gate(97, false, t_on, t_off));
        // Once on, holds anywhere above T_off…
        assert!(edge_gate(49, true, t_on, t_off));
        assert!(!edge_gate(48, true, t_on, t_off), "T_off is strict");
        // …and once off, T_off is not enough to re-arm.
        assert!(!edge_gate(90, false, t_on, t_off));
    }

    /// M3 acceptance 5: scene-cut reset clears idx/edge/bin without realloc.
    #[test]
    fn scene_cut_reset() {
        let mut st = HysteresisState::new(4, 3);
        *st.cell_mut(2, 1) = CellState { idx: 5, bin: 3, flags: cell_flags::WAS_EDGE };
        st.reset();
        assert_eq!(st.cell(2, 1), CellState::default());
        assert_eq!((st.cols(), st.rows()), (4, 3));
    }

    /// M3 acceptance 5: resize reallocs to the new grid and resets everything.
    #[test]
    fn resize_reallocs_and_resets() {
        let mut st = HysteresisState::new(2, 2);
        *st.cell_mut(1, 1) = CellState { idx: 7, bin: 1, flags: cell_flags::WAS_EDGE };
        st.resize(5, 4);
        assert_eq!((st.cols(), st.rows()), (5, 4));
        for r in 0..4 {
            for c in 0..5 {
                assert_eq!(st.cell(c, r), CellState::default());
            }
        }
    }
}
