//! Per-cell temporal state and ramp-index and edge-gate hysteresis.

use crate::grid::Grid;
use crate::orient::BIN_UNSET;

pub const IDX_UNSET: u8 = 0xFF;

pub const IDX_HYST_Q8: u32 = 90;

pub mod cell_flags {
    pub const WAS_EDGE: u8 = 1;
    pub const WAS_QUADRANT: u8 = 1 << 1;
    pub const SHARED_MASK: u8 = WAS_EDGE | WAS_QUADRANT;
    pub const CODEC_PRIVATE_MASK: u8 = !SHARED_MASK;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellState {
    pub idx: u8,
    pub bin: u8,
    pub flags: u8,
}

impl Default for CellState {
    #[inline]
    fn default() -> CellState {
        CellState { idx: IDX_UNSET, bin: BIN_UNSET, flags: 0 }
    }
}

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

    pub fn reset(&mut self) {
        self.cells.fill(CellState::default());
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cells.resize(cols, rows);
    }

    #[inline]
    pub fn cell(&self, col: u16, row: u16) -> CellState {
        self.cells.get(col, row)
    }

    #[inline]
    pub fn cell_mut(&mut self, col: u16, row: u16) -> &mut CellState {
        let i = row as usize * self.cells.cols() as usize + col as usize;
        &mut self.cells.as_mut_slice()[i]
    }
}

#[inline]
pub fn hysteresis_idx(n: u8, len: u8, prev: u8, hyst_q8: u32) -> u8 {
    debug_assert!(len >= 1);
    debug_assert!(hyst_q8 < 256);
    let p = n as u32 * len as u32;
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

#[inline]
pub fn edge_gate(e: u8, was_edge: bool, t_on: u8, t_off: u8) -> bool {
    e > t_on || (was_edge && e > t_off)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idx_boundary_plus_minus_035() {
        assert_eq!(hysteresis_idx(110, 10, IDX_UNSET, IDX_HYST_Q8), 4);

        assert_eq!(hysteresis_idx(136, 10, 4, IDX_HYST_Q8), 4, "0.31 past boundary holds");
        assert_eq!(hysteresis_idx(137, 10, 4, IDX_HYST_Q8), 5, "0.35+ past boundary moves");

        assert_eq!(hysteresis_idx(94, 10, 4, IDX_HYST_Q8), 4, "0.33 below boundary holds");
        assert_eq!(hysteresis_idx(93, 10, 4, IDX_HYST_Q8), 3, "0.36 below boundary moves");

        assert_eq!(hysteresis_idx(255, 10, 0, IDX_HYST_Q8), 9);
        assert_eq!(hysteresis_idx(0, 10, 9, IDX_HYST_Q8), 0);
    }

    #[test]
    fn idx_width_is_parametric() {
        assert_eq!(hysteresis_idx(140, 10, 4, 128), 4, "0.47 past boundary holds at width 128");
        assert_eq!(hysteresis_idx(141, 10, 4, 128), 5, "0.5+ past boundary moves at width 128");
        assert_eq!(hysteresis_idx(128, 10, 4, 0), 5, "width 0 = plain step");
        assert_eq!(hysteresis_idx(127, 10, 4, 0), 4);
        assert_eq!(hysteresis_idx(90, 10, 4, 128), 4);
        assert_eq!(hysteresis_idx(89, 10, 4, 128), 3);
    }

    #[test]
    fn idx_unset_or_stale_prev_quantizes_plainly() {
        assert_eq!(hysteresis_idx(255, 16, IDX_UNSET, IDX_HYST_Q8), 15);
        assert_eq!(hysteresis_idx(0, 16, IDX_UNSET, IDX_HYST_Q8), 0);
        assert_eq!(hysteresis_idx(128, 8, 12, IDX_HYST_Q8), 4);
        assert_eq!(hysteresis_idx(200, 1, IDX_UNSET, IDX_HYST_Q8), 0);
    }

    #[test]
    fn edge_dual_threshold() {
        let (t_on, t_off) = (96, 48);
        assert!(!edge_gate(96, false, t_on, t_off), "T_on is strict");
        assert!(edge_gate(97, false, t_on, t_off));
        assert!(edge_gate(49, true, t_on, t_off));
        assert!(!edge_gate(48, true, t_on, t_off), "T_off is strict");
        assert!(!edge_gate(90, false, t_on, t_off));
    }

    #[test]
    fn scene_cut_reset() {
        let mut st = HysteresisState::new(4, 3);
        *st.cell_mut(2, 1) = CellState { idx: 5, bin: 3, flags: cell_flags::WAS_EDGE };
        st.reset();
        assert_eq!(st.cell(2, 1), CellState::default());
        assert_eq!((st.cols(), st.rows()), (4, 3));
    }

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
