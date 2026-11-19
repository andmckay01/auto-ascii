//! Pixels glyph style and per-cell layer selection.
//! One ramp step inside a quarter of the shared dial is ignored, and deep
//! shadow targets the darkest step. A step past that band waits for the frame
//! slot of a fixed interleaved-gradient phase, at most five frames. Two or
//! more steps snap. Dial 0 is plain quantisation.

use crate::cell::{Cell, Rgb};
use crate::style::GlyphStyle;
use crate::compose::{CellInputs, ComposeParams, boost, h_flags, layer, shade};
use crate::hysteresis::{CellState, cell_flags, edge_gate, pixels_ramp_idx};
use crate::orient::{bin_with_guard, coherence_at_least, debias};
use crate::palette::{
    BRAILLE_EDGE, GlyphClass, PaletteSet, SUBPOS_GLYPHS, SubPos, braille_glyph, quadrant_for,
    subpos,
};

#[inline]
fn ramp_phase(col: u16, row: u16) -> u8 {
    crate::hysteresis::ramp_phase(col, row)
}

pub struct Pixels;

impl GlyphStyle for Pixels {
    const NAME: &'static str = "pixels";

    #[inline]
    fn cell(
        inp: &CellInputs,
        lut: &[u8; 256],
        set: &PaletteSet,
        params: &ComposeParams,
        s: &mut CellState,
    ) -> (Cell, u8) {
        Self::cell_at(inp, lut, set, params, 0, 0, 0, s)
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn cell_at(
        inp: &CellInputs,
        lut: &[u8; 256],
        set: &PaletteSet,
        params: &ComposeParams,
        col: u16,
        row: u16,
        frame: u8,
        s: &mut CellState,
    ) -> (Cell, u8) {
        let lt = lut[inp.luma_top as usize];
        let lb = lut[inp.luma_bottom as usize];
        let n = ((lt as u16 + lb as u16 + 1) >> 1) as u8;
        let len = set.base.len();
        let deep_shadow = inp.h & h_flags::DEEP_SHADOW != 0;

        let idx = pixels_ramp_idx(
            if deep_shadow { 0 } else { n },
            len,
            s.idx,
            params.idx_hyst_q8 as u32,
            ramp_phase(col, row),
            frame,
            &mut s.tone_candidate,
            &mut s.tone_age,
        );
        s.idx = idx;

        let was_edge = s.flags & cell_flags::WAS_EDGE != 0;
        let edge_on = edge_gate(inp.e, was_edge, params.edge_t_on, params.edge_t_off);
        if edge_on {
            s.flags |= cell_flags::WAS_EDGE;
        } else {
            s.flags &= !cell_flags::WAS_EDGE;
        }

        let quad_on = set.quadrant && {
            let was_quad = s.flags & cell_flags::WAS_QUADRANT != 0;
            let on = edge_gate(inp.e, was_quad, params.quad_e_on, params.quad_e_off);
            if on {
                s.flags |= cell_flags::WAS_QUADRANT;
            } else {
                s.flags &= !cell_flags::WAS_QUADRANT;
            }
            on
        };

        let fg = inp.chroma.unwrap_or(Rgb::gray(n));
        let dx = -debias(inp.ex);
        let dy = -debias(inp.ey);
        let plain_idx = ((n as u32 * len as u32) >> 8).min(len as u32 - 1);
        let near_white = ((plain_idx + 1) << 8) > params.edge_white_cut_q8 as u32 * len as u32;

        if edge_on && !near_white && coherence_at_least(dx, dy, inp.e, params.coh_min_q8) {
            let g = if coherence_at_least(dx, dy, inp.e, params.coh_dir_q8) {
                let bin = bin_with_guard(dx, dy, s.bin);
                s.bin = bin;
                let class = GlyphClass::from_bin(bin) as usize;
                let sub = subpos(lt, lb, params.halfblock_min_delta) as usize;
                if set.braille {
                    braille_glyph(BRAILLE_EDGE.by_class[class][sub])
                } else {
                    set.edge.by_class[class][sub]
                }
            } else if set.braille {
                braille_glyph(BRAILLE_EDGE.junction)
            } else if inp.e >= params.edge_strong {
                set.edge.junction_strong
            } else {
                set.edge.junction
            };
            return (Cell::new(g, fg, Rgb::BLACK), layer::EDGE);
        }

        if deep_shadow {
            let glyph = set.base.glyph(idx);
            let won = if idx == 0 { layer::SHADOW } else { layer::BASE };
            return (Cell::new(glyph, fg, Rgb::BLACK), won);
        }

        let hi_cut = ((len as u32 * params.hi_cut_q8 as u32) >> 8).max(1);
        if inp.h & h_flags::HIGHLIGHT != 0 && (idx as u32) < hi_cut {
            let hlen = set.highlight.len() as u32;
            let hidx = ((idx as u32 * hlen) / hi_cut).min(hlen - 1) as u8;
            return (Cell::new(set.highlight.glyph(hidx), boost(fg), Rgb::BLACK), layer::HIGHLIGHT);
        }

        if lt.abs_diff(lb) >= params.halfblock_min_delta {
            if set.halfblock {
                let m = n.max(1);
                let (ctop, cbot) = match inp.chroma {
                    Some(c) => (shade(c, lt, m), shade(c, lb, m)),
                    None => (Rgb::gray(lt), Rgb::gray(lb)),
                };
                if quad_on && coherence_at_least(dx, dy, inp.e, params.coh_dir_q8) {
                    let bin = bin_with_guard(dx, dy, s.bin);
                    s.bin = bin;
                    if let Some(q) = quadrant_for(GlyphClass::from_bin(bin), lt >= lb) {
                        let (f, b) = if lt >= lb { (ctop, cbot) } else { (cbot, ctop) };
                        return (Cell::new(q, f, b), layer::STRUCTURE);
                    }
                }
                return if lt >= lb {
                    (Cell::new('▀', ctop, cbot), layer::STRUCTURE)
                } else {
                    (Cell::new('▄', cbot, ctop), layer::STRUCTURE)
                };
            }
            if set.subpos {
                let g = if lt >= lb {
                    SUBPOS_GLYPHS[SubPos::Top as usize]
                } else {
                    SUBPOS_GLYPHS[SubPos::Bottom as usize]
                };
                return (Cell::new(g, fg, Rgb::BLACK), layer::STRUCTURE);
            }
        }

        (Cell::new(set.base.glyph(idx), fg, Rgb::BLACK), layer::BASE)
    }
}

#[cfg(test)]
mod tests {
    use super::Pixels;
    use crate::compose::{CellInputs, ComposeParams, compose_cell, h_flags};
    use crate::hysteresis::{CellState, HysteresisState};
    use crate::palette::{ColorDepth, GlyphTier, select_palettes};
    use crate::style::GlyphStyle;

    fn lut() -> [u8; 256] {
        core::array::from_fn(|i| i as u8)
    }

    fn tone(n: u8) -> CellInputs {
        CellInputs {
            luma_top: n,
            luma_bottom: n,
            e: 0,
            ex: 128,
            ey: 128,
            h: 0,
            chroma: None,
            dither: 0,
        }
    }

    fn glyph(n: u8, col: u16, row: u16, frame: u8, state: &mut CellState) -> char {
        let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 32);
        Pixels::cell_at(&tone(n), &lut(), &set, &ComposeParams::default(), col, row, frame, state)
            .0
            .glyph()
    }

    fn component(mask: &[bool], cols: usize) -> usize {
        let rows = mask.len() / cols;
        let mut seen = vec![false; mask.len()];
        let mut best = 0usize;
        for i in 0..mask.len() {
            if !mask[i] || seen[i] {
                continue;
            }
            let mut stack = vec![i];
            seen[i] = true;
            let mut n = 0usize;
            while let Some(cur) = stack.pop() {
                n += 1;
                let (r, c) = (cur / cols, cur % cols);
                for (rr, cc) in [(r.wrapping_sub(1), c), (r + 1, c), (r, c.wrapping_sub(1)), (r, c + 1)] {
                    if rr < rows && cc < cols {
                        let j = rr * cols + cc;
                        if mask[j] && !seen[j] {
                            seen[j] = true;
                            stack.push(j);
                        }
                    }
                }
            }
            best = best.max(n);
        }
        best
    }

    #[test]
    fn constant_tone_plus_small_noise_does_not_change_glyphs() {
        let mut state = 0xA5A5_1234u32;
        let mut noise = || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            ((state >> 24) % 9) as i16 - 4
        };
        for base in [64u8, 128, 200] {
            let mut cells = vec![CellState::default(); 64];
            let mut prev = vec!['\0'; 64];
            for frame in 0..60u8 {
                let n = (i16::from(base) + noise()).clamp(0, 255) as u8;
                for row in 0..8u16 {
                    for col in 0..8u16 {
                        let i = row as usize * 8 + col as usize;
                        let g = glyph(n, col, row, frame, &mut cells[i]);
                        if frame > 0 {
                            assert_eq!(g, prev[i], "base {base} frame {frame} at {col},{row}");
                        }
                        prev[i] = g;
                    }
                }
            }
        }
    }

    #[test]
    fn a_flat_one_step_crossing_dissolves_across_frames() {
        const N: usize = 16;
        let mut cells = vec![CellState::default(); N * N];
        let mut prev = vec!['\0'; N * N];
        for i in 0..N * N {
            let col = (i % N) as u16;
            let row = (i / N) as u16;
            prev[i] = glyph(120, col, row, 0, &mut cells[i]);
        }
        let mut seen = [0u16; 8];
        let mut worst = 0usize;
        for frame in 1..=6u8 {
            let mut changed = vec![false; N * N];
            for i in 0..N * N {
                let col = (i % N) as u16;
                let row = (i / N) as u16;
                let g = glyph(140, col, row, frame, &mut cells[i]);
                if g != prev[i] {
                    assert_eq!(super::ramp_phase(col, row) % 3, frame % 3, "frame {frame} released another phase");
                }
                changed[i] = g != prev[i];
                prev[i] = g;
            }
            seen[frame as usize] = changed.iter().filter(|c| **c).count() as u16;
            worst = worst.max(component(&changed, N));
        }
        assert_eq!(seen[1], 0, "the first frame of a one-step crossing still holds");
        assert!(seen[2] > 0 && seen[3] > 0 && seen[4] > 0, "phases land on different frames: {seen:?}");
        assert_eq!(seen[5], 0, "every cell has stepped by frame 4");
        assert!(worst <= 4, "largest same-frame block was {worst}");
    }

    #[test]
    fn cell_by_cell_grid_releases_all_three_phase_classes() {
        const N: usize = 16;
        let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 32);
        let params = ComposeParams::default();
        let table = lut();
        let mut st = HysteresisState::new(N as u16, N as u16);
        let mut prev = vec!['\0'; N * N];
        let mut seen = [0u16; 8];
        let mut residues = [false; 3];
        let mut worst = 0usize;
        for step in 0..=6u8 {
            let residue = st.begin_frame();
            assert_eq!(residue, step % 3, "the clock steps 0, 1, 2 once per frame");
            let n = if step == 0 { 120 } else { 140 };
            let mut changed = vec![false; N * N];
            for i in 0..N * N {
                let col = (i % N) as u16;
                let row = (i / N) as u16;
                let g = compose_cell(&tone(n), &table, &set, &params, &mut st, col, row).glyph();
                if step > 0 && g != prev[i] {
                    assert_eq!(
                        (super::ramp_phase(col, row) & 15) % 3,
                        residue,
                        "step {step} released another class at {col},{row}",
                    );
                    residues[residue as usize] = true;
                }
                changed[i] = step > 0 && g != prev[i];
                prev[i] = g;
            }
            if step > 0 {
                seen[step as usize] = changed.iter().filter(|c| **c).count() as u16;
                worst = worst.max(component(&changed, N));
            }
        }
        assert_eq!(seen[1], 0, "the first frame of a one-step crossing still holds");
        assert!(
            seen[2] > 0 && seen[3] > 0 && seen[4] > 0,
            "phases land on different frames: {seen:?}",
        );
        assert_eq!(seen[5], 0, "every cell has stepped by the fourth crossing frame");
        assert_eq!(seen[6], 0, "a later frame does not release again");
        assert_eq!(residues, [true, true, true], "residues 0, 1 and 2 each released a class");
        assert!(worst <= 4, "largest same-frame block was {worst}");
    }

    #[test]
    fn deep_shadow_cancels_a_pending_ramp_step() {
        let mut state = CellState::default();
        assert_eq!(glyph(20, 0, 0, 0, &mut state), ' ');
        assert_eq!(glyph(50, 0, 0, 1, &mut state), ' ', "one frame past the narrow band is not enough");
        let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 32);
        let shadowed = CellInputs { h: h_flags::DEEP_SHADOW, ..tone(50) };
        let g = Pixels::cell_at(
            &shadowed,
            &lut(),
            &set,
            &ComposeParams::default(),
            0,
            0,
            2,
            &mut state,
        )
        .0;
        assert_eq!(g.glyph(), ' ');
        assert_eq!(glyph(50, 0, 0, 3, &mut state), ' ', "shadow cleared the confirm");
        assert_eq!(glyph(50, 0, 0, 4, &mut state), ' ', "frame 4 is not phase 0");
        assert_eq!(glyph(50, 0, 0, 6, &mut state), '·');
    }

    #[test]
    fn a_shadow_field_of_middle_dots_dissolves_across_frames() {
        const N: usize = 16;
        let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 32);
        let params = ComposeParams::default();
        let table = lut();
        let shadow = CellInputs { h: h_flags::DEEP_SHADOW, ..tone(50) };
        let mut cells = vec![CellState::default(); N * N];
        let mut prev = vec!['\0'; N * N];
        for i in 0..N * N {
            let col = (i % N) as u16;
            let row = (i / N) as u16;
            prev[i] = glyph(50, col, row, 0, &mut cells[i]);
            assert_eq!(prev[i], '·', "50 quantises to middle dot");
        }
        let mut seen = [0u16; 8];
        let mut worst = 0usize;
        for frame in 1..=6u8 {
            let mut changed = vec![false; N * N];
            for i in 0..N * N {
                let col = (i % N) as u16;
                let row = (i / N) as u16;
                let g = Pixels::cell_at(&shadow, &table, &set, &params, col, row, frame, &mut cells[i])
                    .0
                    .glyph();
                if g != prev[i] {
                    assert_eq!(g, ' ', "shadow only steps toward the darkest glyph");
                    assert_eq!(
                        super::ramp_phase(col, row) % 3,
                        frame % 3,
                        "frame {frame} inked another phase"
                    );
                }
                changed[i] = g != prev[i];
                prev[i] = g;
            }
            seen[frame as usize] = changed.iter().filter(|c| **c).count() as u16;
            worst = worst.max(component(&changed, N));
        }
        assert_eq!(seen[1], 0, "the first shadow frame still holds middle dot");
        assert!(seen[2] > 0 && seen[3] > 0 && seen[4] > 0, "phases land on different frames: {seen:?}");
        assert_eq!(seen[5], 0, "every cell has reached space by frame 4");
        assert!(worst <= 4, "largest same-frame shadow block was {worst}");
    }

    #[test]
    fn a_one_frame_shadow_flicker_does_not_ink() {
        let mut state = CellState::default();
        assert_eq!(glyph(50, 0, 0, 0, &mut state), '·');
        let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 32);
        let shadow = CellInputs { h: h_flags::DEEP_SHADOW, ..tone(50) };
        let held = Pixels::cell_at(
            &shadow,
            &lut(),
            &set,
            &ComposeParams::default(),
            0,
            0,
            1,
            &mut state,
        )
        .0
        .glyph();
        assert_eq!(held, '·', "one shadow frame is not enough to leave middle dot");
        assert_eq!(glyph(50, 0, 0, 2, &mut state), '·', "the flicker cancels when the flag drops");
    }

    #[test]
    fn a_deep_jump_into_shadow_snaps() {
        let mut state = CellState::default();
        assert_eq!(glyph(200, 0, 0, 0, &mut state), '▓');
        let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 32);
        let shadow = CellInputs { h: h_flags::DEEP_SHADOW, ..tone(200) };
        let g = Pixels::cell_at(&shadow, &lut(), &set, &ComposeParams::default(), 0, 0, 1, &mut state)
            .0
            .glyph();
        assert_eq!(g, ' ', "two or more ramp steps into shadow still snap");
    }

    #[test]
    fn cell_without_coordinates_matches_the_origin_phase() {
        let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 32);
        let params = ComposeParams::default();
        let mut origin = CellState::default();
        let mut plain = CellState::default();
        let table = lut();
        for n in [120u8, 140, 140, 140, 140] {
            let inp = tone(n);
            let a = Pixels::cell(&inp, &table, &set, &params, &mut plain).0;
            let b = Pixels::cell_at(&inp, &table, &set, &params, 0, 0, 0, &mut origin).0;
            assert_eq!(a, b);
        }
    }
}
