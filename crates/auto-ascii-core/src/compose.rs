//! Luma and glyph-style frame composition with per-cell layer metadata.

use crate::cell::{Cell, Rgb};
use crate::style::GlyphStyle;
use crate::style::pixels::Pixels;
use crate::grid::Grid;
use crate::hysteresis::HysteresisState;
use crate::palette::PaletteSet;
use crate::ramp::ramp_glyph;
use crate::viewport::Viewport;

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

pub mod h_flags {
    pub const HIGHLIGHT: u8 = 1;
    pub const DEEP_SHADOW: u8 = 1 << 1;
}

pub mod layer {
    pub const BASE: u8 = 0;
    pub const EDGE: u8 = 1;
    pub const HIGHLIGHT: u8 = 2;
    pub const SHADOW: u8 = 3;
    pub const STRUCTURE: u8 = 4;
}

#[derive(Clone, Copy, Debug)]
pub struct CellInputs {
    pub luma_top: u8,
    pub luma_bottom: u8,
    pub e: u8,
    pub ex: u8,
    pub ey: u8,
    pub h: u8,
    pub chroma: Option<Rgb>,
    pub dither: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComposeParams {
    pub edge_t_on: u8,
    pub edge_t_off: u8,
    pub coh_min_q8: u8,
    pub coh_dir_q8: u8,
    pub hi_cut_q8: u8,
    pub edge_white_cut_q8: u8,
    pub halfblock_min_delta: u8,
    pub edge_strong: u8,
    pub quad_e_on: u8,
    pub quad_e_off: u8,
    pub idx_hyst_q8: u8,
    pub shadow_lift: u8,
    pub lift_color: u8,
    pub dither: u8,
}

impl Default for ComposeParams {
    fn default() -> ComposeParams {
        ComposeParams {
            edge_t_on: 32,
            edge_t_off: 16,
            coh_min_q8: 96,
            coh_dir_q8: 160,
            hi_cut_q8: 160,
            edge_white_cut_q8: 240,
            halfblock_min_delta: 64,
            edge_strong: 96,
            quad_e_on: 2,
            quad_e_off: 1,
            idx_hyst_q8: crate::hysteresis::IDX_HYST_DEFAULT_Q8,
            shadow_lift: 0,
            lift_color: 0,
            dither: 0,
        }
    }
}

#[inline]
pub(crate) fn boost(c: Rgb) -> Rgb {
    Rgb::new(
        c.r + ((255 - c.r) >> 2),
        c.g + ((255 - c.g) >> 2),
        c.b + ((255 - c.b) >> 2),
    )
}

#[inline]
pub(crate) fn shade(c: Rgb, l: u8, m: u8) -> Rgb {
    let s = |v: u8| ((v as u32 * l as u32) / m as u32).min(255) as u8;
    Rgb::new(s(c.r), s(c.g), s(c.b))
}

pub fn compose_cell(
    inp: &CellInputs,
    lut: &[u8; 256],
    set: &PaletteSet,
    params: &ComposeParams,
    state: &mut HysteresisState,
    col: u16,
    row: u16,
) -> Cell {
    compose_cell_layer(inp, lut, set, params, state, col, row).0
}

pub fn compose_cell_layer(
    inp: &CellInputs,
    lut: &[u8; 256],
    set: &PaletteSet,
    params: &ComposeParams,
    state: &mut HysteresisState,
    col: u16,
    row: u16,
) -> (Cell, u8) {
    Pixels::cell(inp, lut, set, params, state.cell_mut(col, row))
}

#[derive(Clone, Copy, Debug)]
pub struct FramePlanes<'a> {
    pub luma2: &'a [u8],
    pub e: Option<&'a [u8]>,
    pub ex: Option<&'a [u8]>,
    pub ey: Option<&'a [u8]>,
    pub h: Option<&'a [u8]>,
    pub chroma: Option<(&'a [u8], &'a [u8], &'a [u8])>,
}

pub fn compose_frame(
    planes: &FramePlanes<'_>,
    vp: &Viewport,
    lut: &[u8; 256],
    set: &PaletteSet,
    params: &ComposeParams,
    state: &mut HysteresisState,
    out: &mut Grid<Cell>,
) {
    frame_impl::<Pixels>(planes, vp, lut, set, params, state, out, None);
}

#[allow(clippy::too_many_arguments)]
pub fn compose_frame_masked(
    planes: &FramePlanes<'_>,
    vp: &Viewport,
    lut: &[u8; 256],
    set: &PaletteSet,
    params: &ComposeParams,
    state: &mut HysteresisState,
    out: &mut Grid<Cell>,
    mask: &mut Grid<u8>,
) {
    assert_eq!((mask.cols(), mask.rows()), (out.cols(), out.rows()), "layer mask != grid dims");
    frame_impl::<Pixels>(planes, vp, lut, set, params, state, out, Some(mask));
}

const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

#[inline]
fn dither_index(mode: u8, c: usize, r: usize) -> u8 {
    match mode {
        0 => 0,
        1 => BAYER4[r & 3][c & 3],
        _ => {
            let mut h = (c as u32).wrapping_mul(0x9E37_79B1) ^ (r as u32).wrapping_mul(0x85EB_CA77);
            h ^= h >> 15;
            h = h.wrapping_mul(0x2C1B_3C6D);
            h ^= h >> 12;
            (h & 15) as u8
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn frame_impl<C: GlyphStyle>(
    planes: &FramePlanes<'_>,
    vp: &Viewport,
    lut: &[u8; 256],
    set: &PaletteSet,
    params: &ComposeParams,
    state: &mut HysteresisState,
    out: &mut Grid<Cell>,
    mut mask: Option<&mut Grid<u8>>,
) {
    let vc = vp.cols as usize;
    let vr = vp.rows as usize;
    assert_eq!(out.cols(), vp.cols + vp.pad_left + vp.pad_right, "grid cols != viewport + pads");
    assert_eq!(out.rows(), vp.rows + vp.pad_top + vp.pad_bottom, "grid rows != viewport + pads");
    assert_eq!((state.cols(), state.rows()), (vp.cols, vp.rows), "hysteresis state size");
    assert!(planes.luma2.len() >= vc * 2 * vr, "luma2 plane smaller than Vc x 2Vr");
    let edge = match (planes.e, planes.ex, planes.ey) {
        (Some(e), Some(ex), Some(ey)) => {
            assert!(e.len() >= vc * vr && ex.len() >= vc * vr && ey.len() >= vc * vr);
            Some((e, ex, ey))
        }
        _ => None,
    };
    if let Some(h) = planes.h {
        assert!(h.len() >= vc * vr, "H plane smaller than viewport");
    }
    if let Some((r, g, b)) = planes.chroma {
        assert!(r.len() >= vc * vr && g.len() >= vc * vr && b.len() >= vc * vr);
    }

    out.fill(C::PAD);
    if let Some(m) = mask.as_deref_mut() {
        m.fill(layer::BASE);
    }
    for r in 0..vr {
        let top = &planes.luma2[2 * r * vc..2 * r * vc + vc];
        let bot = &planes.luma2[(2 * r + 1) * vc..(2 * r + 1) * vc + vc];
        for c in 0..vc {
            let i = r * vc + c;
            let (e, ex, ey) = match edge {
                Some((pe, px, py)) => (pe[i], px[i], py[i]),
                None => (0, 128, 128),
            };
            let inp = CellInputs {
                luma_top: top[c],
                luma_bottom: bot[c],
                e,
                ex,
                ey,
                h: planes.h.map_or(0, |p| p[i]),
                chroma: planes.chroma.map(|(pr, pg, pb)| Rgb::new(pr[i], pg[i], pb[i])),
                dither: dither_index(params.dither, c, r),
            };
            let (cell, won) =
                C::cell(&inp, lut, set, params, state.cell_mut(c as u16, r as u16));
            out.set(vp.pad_left + c as u16, vp.pad_top + r as u16, cell);
            if let Some(m) = mask.as_deref_mut() {
                m.set(vp.pad_left + c as u16, vp.pad_top + r as u16, won);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hysteresis::hysteresis_idx;
    use crate::ramp::{ASCII_BASE_COARSE, ASCII_BASE_FINE, base_ramp_for_cols};
    use crate::viewport::compute_viewport;

    #[test]
    fn pads_are_blank_and_viewport_is_mapped() {
        let vp = compute_viewport(213, 58, 2.0).unwrap();
        let ramp = base_ramp_for_cols(vp.cols);
        assert_eq!(ramp, ASCII_BASE_FINE);

        let luma: Vec<u8> = (0..vp.cols as usize * vp.rows as usize)
            .map(|i| (i % 256) as u8)
            .collect();
        let mut grid: Grid<Cell> = Grid::new(213, 58);
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
        let vp = compute_viewport(320, 90, 2.0).unwrap();
        let mut luma = vec![128u8; 480 * 270];
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

    use crate::palette::{ColorDepth, GlyphTier, select_palettes};

    fn ident() -> [u8; 256] {
        core::array::from_fn(|i| i as u8)
    }

    fn exy(theta_deg: f64, mag: f64) -> (u8, u8) {
        let a = (2.0 * theta_deg).to_radians();
        (
            (128.0 - (mag / 2.0) * a.cos()).round() as u8,
            (128.0 - (mag / 2.0) * a.sin()).round() as u8,
        )
    }

    fn cell_with(inp: &CellInputs, set: &PaletteSet) -> Cell {
        let mut st = HysteresisState::new(1, 1);
        compose_cell(inp, &ident(), set, &ComposeParams::default(), &mut st, 0, 0)
    }

    fn base_inp(n: u8) -> CellInputs {
        CellInputs { luma_top: n, luma_bottom: n, e: 0, ex: 128, ey: 128, h: 0, chroma: None, dither: 0 }
    }

    #[test]
    fn color_lift_and_dither_default_off() {
        let p = ComposeParams::default();
        assert_eq!((p.lift_color, p.dither), (0, 0));
    }

    #[test]
    fn oriented_edge_glyph_per_bin() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        let expect_ascii = ['-', '\\', '\\', '|', '|', '/', '/', '-'];
        let expect_uni = ['─', '╲', '╲', '│', '│', '╱', '╱', '─'];
        for k in 0..8usize {
            let (ex, ey) = exy(11.25 + k as f64 * 22.5, 200.0);
            let inp = CellInputs { e: 200, ex, ey, ..base_inp(100) };
            assert_eq!(cell_with(&inp, &ascii).glyph(), expect_ascii[k], "ascii bin {k}");
            assert_eq!(cell_with(&inp, &uni).glyph(), expect_uni[k], "unicode bin {k}");
        }
    }

    #[test]
    fn edge_subposition_variants() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        let (ex, ey) = exy(0.0, 200.0);
        let top = CellInputs { luma_top: 200, luma_bottom: 20, e: 200, ex, ey, h: 0, chroma: None, dither: 0 };
        assert_eq!(cell_with(&top, &ascii).glyph(), '=');
        assert_eq!(cell_with(&top, &uni).glyph(), '‾');
        let bot = CellInputs { luma_top: 20, luma_bottom: 200, ..top };
        assert_eq!(cell_with(&bot, &ascii).glyph(), '_');
        assert_eq!(cell_with(&bot, &uni).glyph(), '_');
    }

    #[test]
    fn junction_when_bins_conflict() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        let inp = CellInputs { e: 60, ex: 128 + 15, ey: 128, ..base_inp(100) };
        assert_eq!(cell_with(&inp, &ascii).glyph(), '+');
        let strong = CellInputs { e: 180, ex: 128 + 40, ey: 128, ..base_inp(100) };
        assert_eq!(cell_with(&strong, &ascii).glyph(), '#');
    }

    #[test]
    fn low_coherence_suppresses_edge() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        let inp = CellInputs { e: 200, ex: 131, ey: 126, ..base_inp(100) };
        let cell = cell_with(&inp, &ascii);
        assert_eq!(cell.glyph(), ascii.base.glyph(3), "falls back to base ramp");
    }

    #[test]
    fn near_white_base_beats_edge() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        let (ex, ey) = exy(90.0, 200.0);
        let inp = CellInputs { e: 200, ex, ey, ..base_inp(255) };
        assert_eq!(cell_with(&inp, &ascii).glyph(), '@', "FEATURE-MAP §5: edge never overrides near-white");
    }

    #[test]
    fn edge_dual_threshold_over_time() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        let (lut, params) = (ident(), ComposeParams::default());
        let mut st = HysteresisState::new(1, 1);
        let (ex, ey) = exy(90.0, 120.0);
        let at = |e: u8, st: &mut HysteresisState| {
            let inp = CellInputs { e, ex, ey, ..base_inp(100) };
            compose_cell(&inp, &lut, &ascii, &params, st, 0, 0).glyph()
        };
        assert_eq!(at(40, &mut st), '|', "above T_on: edge turns on");
        assert_eq!(at(20, &mut st), '|', "T_off < e < T_on holds while was_edge");
        assert_eq!(at(10, &mut st), ascii.base.glyph(3), "below T_off: edge drops");
        assert_eq!(at(20, &mut st), ascii.base.glyph(3), "T_off alone cannot re-arm");
        assert_eq!(at(40, &mut st), '|');
        st.reset();
        assert_eq!(at(20, &mut st), ascii.base.glyph(3), "after cut, T_on required again");
    }

    #[test]
    fn highlight_gate_and_boost() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        let inp = CellInputs { h: h_flags::HIGHLIGHT, ..base_inp(100) };
        let cell = cell_with(&inp, &ascii);
        assert_eq!(cell.glyph(), '+');
        assert_eq!(cell.fg, Rgb::gray(138), "gray(100) boosted 25% toward white");
        let bright = CellInputs { h: h_flags::HIGHLIGHT, ..base_inp(220) };
        assert_eq!(cell_with(&bright, &ascii).glyph(), ascii.base.glyph(6));
    }

    #[test]
    fn deep_shadow_clamps_to_darkest() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        let inp = CellInputs { h: h_flags::DEEP_SHADOW, ..base_inp(200) };
        let mut st = HysteresisState::new(1, 1);
        let cell =
            compose_cell(&inp, &ident(), &ascii, &ComposeParams::default(), &mut st, 0, 0);
        assert_eq!(cell.glyph(), ' ');
        assert_eq!(st.cell(0, 0).idx, 0, "clamp is the tracked state");
    }

    #[test]
    fn halfblock_pair_top_and_bottom() {
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        let top = CellInputs { luma_top: 200, luma_bottom: 20, ..base_inp(0) };
        let cell = cell_with(&top, &uni);
        assert_eq!((cell.glyph(), cell.fg, cell.bg), ('▀', Rgb::gray(200), Rgb::gray(20)));
        let bot = CellInputs { luma_top: 20, luma_bottom: 200, ..base_inp(0) };
        let cell = cell_with(&bot, &uni);
        assert_eq!((cell.glyph(), cell.fg, cell.bg), ('▄', Rgb::gray(200), Rgb::gray(20)));
        let color = CellInputs { chroma: Some(Rgb::new(200, 100, 50)), ..top };
        let cell = cell_with(&color, &uni);
        assert_eq!(cell.glyph(), '▀');
        assert_eq!(cell.fg, Rgb::new(255, 181, 90));
        assert_eq!(cell.bg, Rgb::new(36, 18, 9));
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        assert_eq!(cell_with(&top, &ascii).glyph(), '"');
        assert_eq!(cell_with(&bot, &ascii).glyph(), '_');
    }

    #[test]
    fn quadrant_from_pair_plus_orientation() {
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        let (ex, ey) = exy(45.0, 24.0);
        let inp = CellInputs { luma_top: 200, luma_bottom: 20, e: 24, ex, ey, h: 0, chroma: None, dither: 0 };
        assert_eq!(cell_with(&inp, &uni).glyph(), '▝');
        let flipped = CellInputs { luma_top: 20, luma_bottom: 200, ..inp };
        assert_eq!(cell_with(&flipped, &uni).glyph(), '▖');
        let (ex, ey) = exy(90.0, 24.0);
        let v = CellInputs { ex, ey, ..inp };
        assert_eq!(cell_with(&v, &uni).glyph(), '▀');
    }

    #[test]
    fn lsb_noise_orientation_never_picks_quadrant() {
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        for e in [0u8, 1] {
            for (ex, ey) in [(129u8, 128u8), (127, 128), (128, 129), (129, 127)] {
                let inp = CellInputs { luma_top: 200, luma_bottom: 20, e, ex, ey, h: 0, chroma: None, dither: 0 };
                let cell = cell_with(&inp, &uni);
                assert_eq!(
                    cell.glyph(),
                    '▀',
                    "e={e} exy=({ex},{ey}): noise must fall through to the half-block"
                );
            }
        }
        let p = ComposeParams::default();
        let e = p.quad_e_on + 1;
        let (ex, ey) = exy(45.0, f64::from(e));
        let inp = CellInputs { luma_top: 200, luma_bottom: 20, e, ex, ey, h: 0, chroma: None, dither: 0 };
        assert_eq!(cell_with(&inp, &uni).glyph(), '▝', "arms one LSB above the noise floor");
    }

    #[test]
    fn fine_diagonal_band_still_refines_to_quadrants() {
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        let p = ComposeParams::default();
        assert!(p.quad_e_on < p.edge_t_off, "the floor must sit below the edge gate");
        for e in 3u8..=15 {
            let (ex, ey) = exy(45.0, f64::from(e));
            let up = CellInputs { luma_top: 200, luma_bottom: 20, e, ex, ey, h: 0, chroma: None, dither: 0 };
            assert_eq!(cell_with(&up, &uni).glyph(), '▝', "E={e} diagonal lost its quadrant");
            let (ex, ey) = exy(135.0, f64::from(e));
            let dn = CellInputs { ex, ey, ..up };
            assert_eq!(cell_with(&dn, &uni).glyph(), '▘', "E={e} diagonal lost its quadrant");
        }
    }

    #[test]
    fn quadrant_floor_is_dither_stable() {
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        let (lut, p) = (ident(), ComposeParams::default());
        let mut st = HysteresisState::new(1, 1);
        let at = |e: u8, st: &mut HysteresisState| {
            let (ex, ey) = exy(45.0, 8.0);
            let inp = CellInputs { luma_top: 200, luma_bottom: 20, e, ex, ey, h: 0, chroma: None, dither: 0 };
            compose_cell(&inp, &lut, &uni, &p, st, 0, 0).glyph()
        };
        assert_eq!(at(p.quad_e_on + 1, &mut st), '▝');
        for _ in 0..8 {
            assert_eq!(at(p.quad_e_on, &mut st), '▝', "hold threshold absorbs the dip");
            assert_eq!(at(p.quad_e_on + 1, &mut st), '▝');
        }
        assert_eq!(at(p.quad_e_off, &mut st), '▀', "below the hold threshold: half-block");
        assert_eq!(at(p.quad_e_on, &mut st), '▀', "the hold threshold cannot re-arm");
        assert_eq!(at(p.quad_e_on + 1, &mut st), '▝');
        st.reset();
        assert_eq!(at(p.quad_e_on, &mut st), '▀', "after a cut, arming is required again");
    }

    #[test]
    fn braille_replaces_edge_glyphs_only() {
        let braille = select_palettes(GlyphTier::BrailleVerified, ColorDepth::True, 100);
        assert!(braille.braille);
        let (ex, ey) = exy(11.25, 200.0);
        let edge = CellInputs { e: 200, ex, ey, ..base_inp(100) };
        let cell = cell_with(&edge, &braille);
        assert_eq!(cell.glyph(), crate::palette::braille_glyph(0x36), "H-mid dot mask");
        let flat = cell_with(&base_inp(255), &braille);
        assert!(!('\u{2800}'..='\u{28FF}').contains(&flat.glyph()));
    }

    #[test]
    fn frame_backcompat_y_only_is_pure_base() {
        let vp = compute_viewport(40, 12, 2.0).unwrap();
        let (vc, vr) = (vp.cols as usize, vp.rows as usize);
        let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, vp.cols);
        let luma2: Vec<u8> = (0..vc * 2 * vr).map(|i| (i % 251) as u8).collect();
        let planes = FramePlanes { luma2: &luma2, e: None, ex: None, ey: None, h: None, chroma: None };
        let mut st = HysteresisState::new(vp.cols, vp.rows);
        let mut grid: Grid<Cell> = Grid::new(40, 12);
        compose_frame(&planes, &vp, &ident(), &set, &ComposeParams::default(), &mut st, &mut grid);
        for r in 0..vr {
            for c in 0..vc {
                let lt = luma2[2 * r * vc + c];
                let lb = luma2[(2 * r + 1) * vc + c];
                let n = ((lt as u16 + lb as u16 + 1) >> 1) as u8;
                let cell = grid.get(vp.pad_left + c as u16, vp.pad_top + r as u16);
                if lt.abs_diff(lb) >= 64 {
                    assert!(matches!(cell.glyph(), '▀' | '▄'), "({c},{r})");
                } else {
                    assert_eq!(cell.glyph(), set.base.glyph(hysteresis_idx(n, set.base.len(), crate::hysteresis::IDX_UNSET, crate::hysteresis::IDX_HYST_Q8)), "({c},{r})");
                    assert_eq!(cell.fg, Rgb::gray(n));
                }
            }
        }
        for r in 0..12u16 {
            for c in 0..40u16 {
                let in_vp = c >= vp.pad_left
                    && c < vp.pad_left + vp.cols
                    && r >= vp.pad_top
                    && r < vp.pad_top + vp.rows;
                if !in_vp {
                    assert_eq!(grid.get(c, r), Cell::BLANK);
                }
            }
        }
    }

    #[test]
    fn layer_mask_tags_winning_layers() {
        let vp = compute_viewport(40, 12, 2.0).unwrap();
        let (vc, vr) = (vp.cols as usize, vp.rows as usize);
        let set = select_palettes(GlyphTier::Ascii, ColorDepth::True, vp.cols);
        let luma2 = vec![100u8; vc * 2 * vr];
        let mut e = vec![0u8; vc * vr];
        let mut ex = vec![128u8; vc * vr];
        let mut ey = vec![128u8; vc * vr];
        let mut h = vec![0u8; vc * vr];
        let (exb, eyb) = exy(90.0, 200.0);
        e[1] = 200;
        ex[1] = exb;
        ey[1] = eyb;
        h[2] = h_flags::HIGHLIGHT;
        h[3] = h_flags::DEEP_SHADOW;
        let planes = FramePlanes {
            luma2: &luma2,
            e: Some(&e),
            ex: Some(&ex),
            ey: Some(&ey),
            h: Some(&h),
            chroma: None,
        };
        let mut st = HysteresisState::new(vp.cols, vp.rows);
        let mut grid: Grid<Cell> = Grid::new(40, 12);
        let mut mask: Grid<u8> = Grid::new(40, 12);
        mask.fill(0xEE);
        compose_frame_masked(
            &planes, &vp, &ident(), &set, &ComposeParams::default(), &mut st, &mut grid, &mut mask,
        );
        let at = |c: u16| mask.get(vp.pad_left + c, vp.pad_top);
        assert_eq!(at(0), layer::BASE);
        assert_eq!(at(1), layer::EDGE);
        assert_eq!(at(2), layer::HIGHLIGHT);
        assert_eq!(at(3), layer::SHADOW);
        assert_eq!(mask.get(0, 11), layer::BASE, "pads are BASE");
        assert_eq!(grid.get(vp.pad_left + 1, vp.pad_top).glyph(), '|');

        let mut st2 = HysteresisState::new(vp.cols, vp.rows);
        let mut grid2: Grid<Cell> = Grid::new(40, 12);
        compose_frame(&planes, &vp, &ident(), &set, &ComposeParams::default(), &mut st2, &mut grid2);
        assert_eq!(grid.as_slice(), grid2.as_slice());
    }

    #[test]
    fn structure_layer_tagged() {
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        let mut st = HysteresisState::new(1, 1);
        let inp = CellInputs { luma_top: 200, luma_bottom: 20, ..base_inp(0) };
        let (cell, won) =
            compose_cell_layer(&inp, &ident(), &uni, &ComposeParams::default(), &mut st, 0, 0);
        assert_eq!(cell.glyph(), '▀');
        assert_eq!(won, layer::STRUCTURE);
    }

    #[test]
    #[should_panic(expected = "layer mask != grid dims")]
    fn masked_frame_dim_mismatch_panics() {
        let vp = compute_viewport(40, 12, 2.0).unwrap();
        let luma2 = vec![0u8; vp.cols as usize * 2 * vp.rows as usize];
        let planes = FramePlanes { luma2: &luma2, e: None, ex: None, ey: None, h: None, chroma: None };
        let set = select_palettes(GlyphTier::Ascii, ColorDepth::True, vp.cols);
        let mut st = HysteresisState::new(vp.cols, vp.rows);
        let mut grid: Grid<Cell> = Grid::new(40, 12);
        let mut mask: Grid<u8> = Grid::new(39, 12);
        compose_frame_masked(
            &planes, &vp, &ident(), &set, &ComposeParams::default(), &mut st, &mut grid, &mut mask,
        );
    }

    #[test]
    #[should_panic(expected = "hysteresis state size")]
    fn frame_state_size_mismatch_panics() {
        let vp = compute_viewport(40, 12, 2.0).unwrap();
        let luma2 = vec![0u8; vp.cols as usize * 2 * vp.rows as usize];
        let planes = FramePlanes { luma2: &luma2, e: None, ex: None, ey: None, h: None, chroma: None };
        let set = select_palettes(GlyphTier::Ascii, ColorDepth::True, vp.cols);
        let mut st = HysteresisState::new(1, 1);
        let mut grid: Grid<Cell> = Grid::new(40, 12);
        compose_frame(&planes, &vp, &ident(), &set, &ComposeParams::default(), &mut st, &mut grid);
    }
}
