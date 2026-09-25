//! `pixels` — the three-layer compositor as a codec (the default).
//!
//! Base ramp from the tier's palette, the directional edge layer, deep-shadow
//! clamp, highlights, and — on Unicode tiers — half-block `▀▄` (fg, bg) pairs
//! and quadrant refinement, which is what makes it read as a low-resolution
//! picture. See [`crate::compose::compose_cell`] for the layer-priority
//! contract.

use crate::cell::{Cell, Rgb};
use crate::codec::GlyphCodec;
use crate::compose::{CellInputs, ComposeParams, boost, h_flags, layer, shade};
use crate::hysteresis::{CellState, cell_flags, edge_gate, hysteresis_idx};
use crate::orient::{bin_with_guard, coherence_at_least, debias};
use crate::palette::{
    BRAILLE_EDGE, GlyphClass, PaletteSet, SUBPOS_GLYPHS, SubPos, braille_glyph, quadrant_for,
    subpos,
};

/// The `pixels` codec. See the module docs.
pub struct Pixels;

impl GlyphCodec for Pixels {
    const NAME: &'static str = "pixels";

    /// Per-cell selection. Layer priority (override, never blend): edge
    /// (gated + coherent, base not near-white) → deep-shadow clamp (H bit1 →
    /// darkest step) → highlight (H bit0, `idx < len·hi_cut_q8/256`) →
    /// half-block/quadrant (unicode) or `" - _` subposition (ascii) when
    /// `|top−bottom|` is large → base ramp. Foreground is always the chroma
    /// sample (gray fallback); the backend quantizes.
    #[inline]
    fn cell(
        inp: &CellInputs,
        lut: &[u8; 256],
        set: &PaletteSet,
        params: &ComposeParams,
        s: &mut CellState,
    ) -> (Cell, u8) {
        let lt = lut[inp.luma_top as usize];
        let lb = lut[inp.luma_bottom as usize];
        let n = ((lt as u16 + lb as u16 + 1) >> 1) as u8;
        let len = set.base.len();
        let deep_shadow = inp.h & h_flags::DEEP_SHADOW != 0;

        let mut idx = hysteresis_idx(n, len, s.idx, params.idx_hyst_q8 as u32);
        if deep_shadow {
            idx = 0;
        }
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
            return (Cell::new(set.base.glyph(0), fg, Rgb::BLACK), layer::SHADOW);
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
