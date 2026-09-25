//! `pixels` — the §3.4/§3.5 three-layer compositor as a codec (the default).
//!
//! This is the mapping every committed golden renders, moved here verbatim
//! when codecs became pluggable: base ramp from the tier's palette, the
//! directional edge layer, deep-shadow clamp, highlights, and — on Unicode
//! tiers — half-block `▀▄` (fg, bg) pairs and quadrant refinement, which is
//! what makes it read as a low-resolution picture. See
//! [`crate::compose::compose_cell`] for the layer-priority contract.

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

    /// §3.5 per-cell selection. Layer priority (§3.4, override never
    /// blend): edge (gated + coherent, base not near-white) → deep-shadow
    /// clamp (H bit1 → darkest step) → highlight (H bit0, `idx < HI_CUT`) →
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
        // Per-shot normalization LUT (§3.5 line 2) on each vertical tap.
        let lt = lut[inp.luma_top as usize];
        let lb = lut[inp.luma_bottom as usize];
        let n = ((lt as u16 + lb as u16 + 1) >> 1) as u8;
        let len = set.base.len();
        let deep_shadow = inp.h & h_flags::DEEP_SHADOW != 0;

        // Ramp-index hysteresis (§3.5 line 3); deep shadow clamps to darkest and
        // the clamp IS the tracked state (leaving shadow climbs back from 0).
        let mut idx = hysteresis_idx(n, len, s.idx, params.idx_hyst_q8 as u32);
        if deep_shadow {
            idx = 0;
        }
        s.idx = idx;

        // Temporal dual-threshold edge gate (§3.5 line 4) — magnitude memory is
        // kept even when orientation coherence suppresses drawing this frame.
        let was_edge = s.flags & cell_flags::WAS_EDGE != 0;
        let edge_on = edge_gate(inp.e, was_edge, params.edge_t_on, params.edge_t_off);
        if edge_on {
            s.flags |= cell_flags::WAS_EDGE;
        } else {
            s.flags &= !cell_flags::WAS_EDGE;
        }

        // Quadrant-refinement magnitude gate — the same Canny-style dual
        // threshold shape as the edge gate, an order of magnitude lower (see
        // `ComposeParams::quad_e_on`). Evaluated here rather than inside the
        // quadrant branch so the memory tracks E on every frame, including ones
        // where another layer wins the cell. Skipped entirely on palettes with
        // no quadrants (every ASCII tier), where the flag is never read; a
        // palette change goes through `reflow` → `HysteresisState::resize`,
        // which resets all state, so no stale bit can survive into a set that
        // does read it.
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
        // The asset stores the GRADIENT doubled-angle convention (factory
        // edges.rs / PLAN §4); the edge-tangent doubled vector is its negation
        // (doubling turns the 90° tangent rotation into a sign flip). Decode to
        // tangent here — all orientation bins/glyph classes are tangent-space.
        let dx = -debias(inp.ex);
        let dy = -debias(inp.ey);
        // §3.4: edge never overrides a near-white base cell. Deliberately the
        // PLAIN quantized index (M3 Tune): "near-white" is the cell's
        // instantaneous brightness, not the hysteresis-held display index —
        // riding `idx` here coupled edge recall to `idx_hyst_q8` (wider
        // stickiness held bright cells at the top step past bright edges,
        // measurably dropping edge F1 on the high-contrast clips).
        let plain_idx = ((n as u32 * len as u32) >> 8).min(len as u32 - 1);
        let near_white = ((plain_idx + 1) << 8) > params.edge_white_cut_q8 as u32 * len as u32;

        // L1 edge/contour.
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

        // Deep shadow (H bit1): clamp to the darkest ramp step.
        if deep_shadow {
            return (Cell::new(set.base.glyph(0), fg, Rgb::BLACK), layer::SHADOW);
        }

        // L2 highlight (H bit0), gated to dark-mid base (§3.5 `idx < HI_CUT`);
        // the gate range spreads over the whole highlight ramp.
        let hi_cut = ((len as u32 * params.hi_cut_q8 as u32) >> 8).max(1);
        if inp.h & h_flags::HIGHLIGHT != 0 && (idx as u32) < hi_cut {
            let hlen = set.highlight.len() as u32;
            let hidx = ((idx as u32 * hlen) / hi_cut).min(hlen - 1) as u8;
            return (Cell::new(set.highlight.glyph(hidx), boost(fg), Rgb::BLACK), layer::HIGHLIGHT);
        }

        // Sub-cell vertical structure (§3.3/§3.5): the Vc×2Vr taps disagree.
        if lt.abs_diff(lb) >= params.halfblock_min_delta {
            if set.halfblock {
                let m = n.max(1);
                let (ctop, cbot) = match inp.chroma {
                    Some(c) => (shade(c, lt, m), shade(c, lb, m)),
                    None => (Rgb::gray(lt), Rgb::gray(lb)),
                };
                // Quadrant refinement (palette 5): coherent diagonal orientation
                // below the edge gate — see `palette::quadrant_for` for the
                // vertical-pair + dominant-orientation approximation.
                //
                // Magnitude floor (M3 review low, re-tuned at M4 review):
                // coherence divides by max(e, 1), so at resampled E of 0–1 a
                // 1-LSB resample-noise (Ex,Ey) vector is "perfectly coherent"
                // and would steer the cell to a noise-driven corner quadrant.
                // `quad_on` is the dual-threshold NOISE floor for that — not the
                // edge gate's hold threshold, which would take the whole
                // E ∈ [2, 15] fine-diagonal band with it (see
                // `ComposeParams::quad_e_on`). Below the floor the plain
                // half-block is the honest glyph.
                if quad_on && coherence_at_least(dx, dy, inp.e, params.coh_dir_q8) {
                    let bin = bin_with_guard(dx, dy, s.bin);
                    s.bin = bin;
                    if let Some(q) = quadrant_for(GlyphClass::from_bin(bin), lt >= lb) {
                        let (f, b) = if lt >= lb { (ctop, cbot) } else { (cbot, ctop) };
                        return (Cell::new(q, f, b), layer::STRUCTURE);
                    }
                }
                // Half-block (fg, bg) vertical pixel pair — bg is load-bearing
                // (§3.1). `▀` paints the top tap as fg; `▄` keeps bg the darker
                // top when the bottom is the bright half.
                return if lt >= lb {
                    (Cell::new('▀', ctop, cbot), layer::STRUCTURE)
                } else {
                    (Cell::new('▄', cbot, ctop), layer::STRUCTURE)
                };
            }
            if set.subpos {
                // ASCII tiers: `"` / `_` when the bright half is decisive (`-` is
                // the mid slot, never decisive here — §3.3).
                let g = if lt >= lb {
                    SUBPOS_GLYPHS[SubPos::Top as usize]
                } else {
                    SUBPOS_GLYPHS[SubPos::Bottom as usize]
                };
                return (Cell::new(g, fg, Rgb::BLACK), layer::STRUCTURE);
            }
        }

        // L0 base ramp.
        (Cell::new(set.base.glyph(idx), fg, Rgb::BLACK), layer::BASE)
    }
}
