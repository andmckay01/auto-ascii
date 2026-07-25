//! M0 compositor: resampled luma → `Grid<Cell>` (PLAN §3.4 L0 base layer only;
//! edge/highlight layers, hysteresis and half-blocks land M3).
//!
//! One glyph per cell from the base ramp, truecolor gray foreground from the
//! same luma sample (PLAN §7 "ASCII base ramps, truecolor gray fg"), black
//! background everywhere, [`Cell::BLANK`] in the letterbox pads.

use crate::cell::{Cell, Rgb};
use crate::grid::Grid;
use crate::ramp::ramp_glyph;
use crate::viewport::Viewport;

/// Fill `out` from a resampled luma plane, letterboxed per `vp` (PLAN §3.2/§3.4).
///
/// * `luma` — normalized luma (factory-baked p2/p98 levels, M0 simplification),
///   row-major `vp.cols × vp.rows`; extra trailing bytes are ignored so a
///   shared oversized scratch buffer is fine.
/// * `ramp` — a base ramp (see [`crate::ramp::base_ramp_for_cols`]).
/// * `out` — must already be sized to the full terminal grid
///   (`vp.cols + pad_left + pad_right` × `vp.rows + pad_top + pad_bottom`);
///   sizing happens in `Backend::resize`, the only hot-path allocation point
///   (PLAN §6) — this function never allocates.
///
/// Pads are filled with [`Cell::BLANK`] (space on black) every call, so a grid
/// reused across resizes needs no separate clear.
///
/// # Panics
/// If `out` does not match the viewport's terminal dimensions, if `luma` is
/// shorter than `vp.cols × vp.rows`, or if `ramp` is empty.
pub fn compose_luma(luma: &[u8], vp: &Viewport, ramp: &[char], out: &mut Grid<Cell>) {
    let vc = vp.cols as usize;
    let vr = vp.rows as usize;
    assert_eq!(
        out.cols(),
        vp.cols + vp.pad_left + vp.pad_right,
        "grid cols != viewport + pads"
    );
    assert_eq!(
        out.rows(),
        vp.rows + vp.pad_top + vp.pad_bottom,
        "grid rows != viewport + pads"
    );
    assert!(luma.len() >= vc * vr, "luma plane smaller than viewport");
    assert!(!ramp.is_empty(), "empty ramp");

    // Pads: blank the whole grid first (memset-cheap, keeps one code path).
    out.fill(Cell::BLANK);

    let pad_left = vp.pad_left as usize;
    for r in 0..vr {
        let src = &luma[r * vc..r * vc + vc];
        let drow = &mut out.row_mut(vp.pad_top + r as u16)[pad_left..pad_left + vc];
        for (cell, &n) in drow.iter_mut().zip(src) {
            *cell = Cell::new(ramp_glyph(ramp, n), Rgb::gray(n), Rgb::BLACK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ramp::{ASCII_BASE_COARSE, ASCII_BASE_FINE, base_ramp_for_cols};
    use crate::viewport::compute_viewport;

    #[test]
    fn pads_are_blank_and_viewport_is_mapped() {
        // 213×58 → 206×58 with pads L3/R4 (PLAN §3.2 worked example).
        let vp = compute_viewport(213, 58, 2.0).unwrap();
        let ramp = base_ramp_for_cols(vp.cols);
        assert_eq!(ramp, ASCII_BASE_FINE);

        let luma: Vec<u8> = (0..vp.cols as usize * vp.rows as usize)
            .map(|i| (i % 256) as u8)
            .collect();
        let mut grid: Grid<Cell> = Grid::new(213, 58);
        // Pre-poison so we prove pads get overwritten to BLANK.
        grid.fill(Cell::new('X', Rgb::WHITE, Rgb::WHITE));
        compose_luma(&luma, &vp, ramp, &mut grid);

        for row in 0..58u16 {
            for col in 0..213u16 {
                let cell = grid.get(col, row);
                let in_vp = col >= vp.pad_left && col < vp.pad_left + vp.cols;
                if !in_vp {
                    assert_eq!(cell, Cell::BLANK, "pad at ({col},{row})");
                } else {
                    let n = luma[row as usize * vp.cols as usize
                        + (col - vp.pad_left) as usize];
                    assert_eq!(cell.glyph(), ramp_glyph(ramp, n));
                    assert_eq!(cell.fg, Rgb::gray(n), "gray fg from luma");
                    assert_eq!(cell.bg, Rgb::BLACK);
                }
            }
        }
    }

    #[test]
    fn top_bottom_pads_blank() {
        // 80×24 → 80×23: one pad row at the bottom (remainder bottom, §3.2).
        let vp = compute_viewport(80, 24, 2.0).unwrap();
        assert_eq!((vp.pad_top, vp.pad_bottom), (0, 1));
        let luma = vec![255u8; vp.cols as usize * vp.rows as usize];
        let mut grid: Grid<Cell> = Grid::new(80, 24);
        compose_luma(&luma, &vp, ASCII_BASE_FINE, &mut grid);
        assert!(grid.row(23).iter().all(|&c| c == Cell::BLANK));
        assert!(grid.row(0).iter().all(|c| c.glyph() == '@'));
    }

    #[test]
    fn oversized_luma_buffer_is_fine() {
        let vp = compute_viewport(320, 90, 2.0).unwrap(); // exact fit, no pads
        let mut luma = vec![128u8; 480 * 270]; // shared scratch bigger than needed
        luma[0] = 0;
        let mut grid: Grid<Cell> = Grid::new(320, 90);
        compose_luma(&luma, &vp, ASCII_BASE_COARSE, &mut grid);
        assert_eq!(grid.get(0, 0).glyph(), ' ');
        assert_eq!(grid.get(1, 0).fg, Rgb::gray(128));
    }

    #[test]
    #[should_panic(expected = "grid cols")]
    fn mismatched_grid_panics() {
        let vp = compute_viewport(80, 24, 2.0).unwrap();
        let luma = vec![0u8; vp.cols as usize * vp.rows as usize];
        let mut grid: Grid<Cell> = Grid::new(79, 24);
        compose_luma(&luma, &vp, ASCII_BASE_COARSE, &mut grid);
    }
}
