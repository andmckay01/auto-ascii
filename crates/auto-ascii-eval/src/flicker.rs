//! Flicker score — mean glyph switches per cell per second (PLAN §6).
//!
//! The M3 gate is ≤ 2 switches/cell/s on static shots. Segment selection
//! (which frames count as "static") is the eval driver's job — it has the
//! NORM shot table; this accumulator just counts over the frames it is fed.

use auto_ascii_core::{Cell, Grid};

/// Streaming glyph-switch counter. Feed successive rendered grids with
/// [`FlickerAccum::push`]; read the rate with [`FlickerAccum::score`].
///
/// Only the glyph (`Cell::ch`) is compared — color-only changes are not
/// flicker in the §6 sense (they don't strobe glyph shapes).
///
/// A grid-dimension change resets the comparison state (a resize invalidates
/// the whole frame and legitimately reglyphs every cell — PLAN §3.5 resets
/// hysteresis the same way); the first frame after a reset contributes no
/// pairs.
#[derive(Clone, Debug, Default)]
pub struct FlickerAccum {
    prev: Vec<u32>,
    cols: u16,
    rows: u16,
    have_prev: bool,
    switches: u64,
    cell_pairs: u64,
}

impl FlickerAccum {
    pub fn new() -> FlickerAccum {
        FlickerAccum::default()
    }

    /// Consume one rendered frame.
    pub fn push(&mut self, grid: &Grid<Cell>) {
        let cells = grid.as_slice();
        if self.have_prev && self.cols == grid.cols() && self.rows == grid.rows() {
            self.switches += cells
                .iter()
                .zip(&self.prev)
                .filter(|(c, p)| c.ch != **p)
                .count() as u64;
            self.cell_pairs += cells.len() as u64;
            for (p, c) in self.prev.iter_mut().zip(cells) {
                *p = c.ch;
            }
        } else {
            self.prev.clear();
            self.prev.extend(cells.iter().map(|c| c.ch));
            self.cols = grid.cols();
            self.rows = grid.rows();
            self.have_prev = true;
        }
    }

    /// Total glyph switches counted so far.
    #[inline]
    pub fn switches(&self) -> u64 {
        self.switches
    }

    /// Total cell·frame-pair comparisons made so far.
    #[inline]
    pub fn cell_pairs(&self) -> u64 {
        self.cell_pairs
    }

    /// Mean switches per cell per frame-pair, or `None` before two
    /// comparable frames have been pushed.
    pub fn switches_per_cell_frame(&self) -> Option<f64> {
        (self.cell_pairs > 0).then(|| self.switches as f64 / self.cell_pairs as f64)
    }

    /// The §6 flicker score: mean glyph switches per cell per **second** at
    /// the given playback rate.
    pub fn score(&self, fps: f64) -> Option<f64> {
        self.switches_per_cell_frame().map(|s| s * fps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_ascii_core::Rgb;

    fn checker(cols: u16, rows: u16, phase: bool) -> Grid<Cell> {
        let mut g: Grid<Cell> = Grid::new(cols, rows);
        for r in 0..rows {
            for c in 0..cols {
                let on = ((c + r) % 2 == 0) ^ phase;
                let ch = if on { '#' } else { ' ' };
                g.set(c, r, Cell::new(ch, Rgb::WHITE, Rgb::BLACK));
            }
        }
        g
    }

    #[test]
    fn identical_frames_score_zero() {
        let g = checker(10, 10, false);
        let mut acc = FlickerAccum::new();
        for _ in 0..5 {
            acc.push(&g);
        }
        assert_eq!(acc.switches(), 0);
        assert_eq!(acc.cell_pairs(), 400);
        assert_eq!(acc.score(30.0), Some(0.0));
    }

    #[test]
    fn alternating_checkerboard_scores_one_switch_per_cell_frame() {
        let a = checker(10, 10, false);
        let b = checker(10, 10, true);
        let mut acc = FlickerAccum::new();
        acc.push(&a);
        acc.push(&b);
        acc.push(&a);
        acc.push(&b);
        // 3 frame pairs × 100 cells, every cell switches every pair.
        assert_eq!(acc.switches(), 300);
        assert_eq!(acc.cell_pairs(), 300);
        assert_eq!(acc.switches_per_cell_frame(), Some(1.0));
        assert_eq!(acc.score(30.0), Some(30.0)); // catastrophic vs the ≤2 gate
    }

    #[test]
    fn color_only_change_is_not_flicker() {
        let a = checker(4, 4, false);
        let mut b = a.clone();
        for r in 0..4 {
            for c in 0..4 {
                let mut cell = b.get(c, r);
                cell.fg = Rgb::gray(10);
                b.set(c, r, cell);
            }
        }
        let mut acc = FlickerAccum::new();
        acc.push(&a);
        acc.push(&b);
        assert_eq!(acc.switches(), 0);
    }

    #[test]
    fn resize_resets_comparison() {
        let a = checker(10, 10, false);
        let b = checker(8, 8, true);
        let mut acc = FlickerAccum::new();
        acc.push(&a);
        acc.push(&b); // dims changed: no pair counted
        assert_eq!(acc.cell_pairs(), 0);
        assert_eq!(acc.switches_per_cell_frame(), None);
        acc.push(&b); // comparable again
        assert_eq!(acc.cell_pairs(), 64);
        assert_eq!(acc.switches(), 0);
    }

    #[test]
    fn empty_accumulator_has_no_score() {
        let acc = FlickerAccum::new();
        assert_eq!(acc.score(30.0), None);
    }
}
