//! Layer compositor (PLAN §3.4/§3.5).
//!
//! [`compose_luma`] is the M0 L0-only path (kept verbatim — M0/M2 goldens and
//! the player's legacy path depend on it). [`compose_frame`]/[`compose_cell`]
//! are the M3 three-layer compositor: base luminance, edge/contour, highlight
//! — **priority/override composition, one glyph per cell, never blended**
//! (§3.4). No dithering in v1 (§3.5). Nothing here allocates; all temporal
//! state lives in [`HysteresisState`], (re)allocated only via its
//! `new`/`resize` (PLAN §6 hot-path discipline).

use crate::cell::{Cell, Rgb};
use crate::grid::Grid;
use crate::hysteresis::{HysteresisState, cell_flags, edge_gate, hysteresis_idx};
use crate::orient::{bin_with_guard, coherence_at_least, debias};
use crate::palette::{
    BRAILLE_EDGE, GlyphClass, PaletteSet, SUBPOS_GLYPHS, SubPos, braille_glyph, quadrant_for,
    subpos,
};
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

/// H-plane flag bits (PLAN §4 plane registry: bit0 highlight, bit1 deep shadow).
pub mod h_flags {
    pub const HIGHLIGHT: u8 = 1;
    pub const DEEP_SHADOW: u8 = 1 << 1;
}

/// Per-cell winning-layer ids — render metadata (M3). A `Grid<u8>` of these
/// (the **LayerMask**, filled by [`compose_frame_masked`]) makes the §3.4
/// priority decision observable per cell; the eval harness reads it for the
/// §6 edge-F1 prediction side ("cells where the edge layer won"). Values are
/// data, not bitflags — exactly one layer wins (§3.4 override, never blend).
pub mod layer {
    /// L0 base ramp won (also letterbox pads and Y-only back-compat cells).
    pub const BASE: u8 = 0;
    /// L1 edge/contour won (directional, junction or braille edge glyph).
    pub const EDGE: u8 = 1;
    /// L2 highlight won (H bit0 gate).
    pub const HIGHLIGHT: u8 = 2;
    /// Deep-shadow clamp won (H bit1 → darkest step).
    pub const SHADOW: u8 = 3;
    /// Sub-cell vertical structure won (half-block / quadrant / `" _`
    /// subposition — §3.3's Vc×2Vr payoff, not an edge-layer decision).
    pub const STRUCTURE: u8 = 4;
}

/// Per-cell compositor inputs (§3.5): the two vertical luma taps from the
/// Vc×2Vr plane, edge magnitude + doubled-angle orientation, H flags, and the
/// cell's chroma sample (`None` = gray path: mono tier or Y-only asset).
#[derive(Clone, Copy, Debug)]
pub struct CellInputs {
    pub luma_top: u8,
    pub luma_bottom: u8,
    /// Resampled edge magnitude (E plane, unthinned).
    pub e: u8,
    /// Doubled-angle components, bias-128 half scale (see `orient.rs`).
    pub ex: u8,
    pub ey: u8,
    /// H flags (see [`h_flags`]).
    pub h: u8,
    pub chroma: Option<Rgb>,
}

/// Compositor tunables (§3.5). Every field is a `params.toml` candidate — the
/// defaults here are the untuned baseline the M3 eval loop starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComposeParams {
    /// Edge gate on-threshold (strict `e > T_on`).
    pub edge_t_on: u8,
    /// Edge gate hold-threshold (strict `e > T_off` while `was_edge`).
    pub edge_t_off: u8,
    /// Coherence below this (Q8) suppresses the edge layer entirely.
    pub coh_min_q8: u8,
    /// Coherence at/above this (Q8) draws directional glyphs; the band
    /// between `coh_min_q8` and this draws the junction glyph (bins conflict
    /// — cancelled doubled-angle vectors, §3.3 "conflict handling falls out
    /// free").
    pub coh_dir_q8: u8,
    /// Highlight gate: overrides only while `idx < len·hi_cut_q8/256` (§3.5
    /// `idx < HI_CUT` — highlights extend range on dark-mid cells only).
    pub hi_cut_q8: u8,
    /// Edge suppression on near-white base (§3.4 "edge wins if gated on and
    /// base isn't near-white"): suppressed when `(idx+1)·256/len` exceeds
    /// this. 240 = exactly the top ramp step at every shipped length.
    pub edge_white_cut_q8: u8,
    /// `|top − bottom|` at/above this is "large" (§3.5): half-block /
    /// subposition / quadrant paths and the edge top/bottom subposition.
    pub halfblock_min_delta: u8,
    /// Edge magnitude at/above this upgrades an ASCII junction `+` to `#`.
    pub edge_strong: u8,
    /// Quadrant-refinement **noise floor**, arm threshold (strict
    /// `e > quad_e_on`). Coherence divides by `max(e, 1)`, so at a resampled
    /// E of 0–1 a 1-LSB resample-noise `(Ex, Ey)` vector reads as "perfectly
    /// coherent" and would steer the cell to a noise-driven corner quadrant
    /// (M3 review). This is deliberately a *noise* floor and not the edge
    /// gate's hold threshold: resampled E is heavily diluted by the box
    /// average (corpus source E mean ≈ 3.6, 8.95% nonzero), so a genuine fine
    /// diagonal — source E ≈ 100 across one or two pixels of a 3×6 source box
    /// — lands near cell E 6–11, exactly the band quadrants exist to serve.
    pub quad_e_on: u8,
    /// Quadrant-refinement noise floor, hold threshold (strict
    /// `e > quad_e_off` while the gate was on last frame). The pair is a
    /// Canny-style dual threshold for the same reason the edge gate is one:
    /// a single hard threshold on this noisy plane lets a cell dithering
    /// across it alternate quadrant/half-block every frame.
    pub quad_e_off: u8,
    /// Ramp-index hysteresis width in Q8 fractions of one step. The §3.5
    /// spec nominal is "boundary ± 0.35·step" = 90 ([`crate::IDX_HYST_Q8`]);
    /// promoted to a tunable at M3 Tune — wider = stickier cells (less
    /// flicker), narrower = more responsive.
    pub idx_hyst_q8: u8,
    /// Shadow lift: how far to bend the tone curve toward the shadows when the
    /// NORM levels LUT is built. `0` = off (the plain linear per-shot window);
    /// `255` = a full square-root curve. See
    /// [`build_levels_lut`](../../auto_ascii/pipeline/fn.build_levels_lut.html).
    ///
    /// **Why this exists.** The per-shot NORM window ([`PlaneLevels`] p2→0,
    /// p98→255) is *linear*, so it cannot move a dark subject relative to a
    /// bright one — normalizing a shot of fire leaves a face at the same
    /// fraction of the range wherever the endpoints land. That matters more
    /// here than in a continuous-tone renderer: a glyph ramp has only 8–16
    /// steps, so a subject below the first step is not merely dark, it is *the
    /// same glyph as black* and disappears. Lifting buys it a step.
    ///
    /// Applied to the 256-entry LUT, so it costs nothing per pixel and rides
    /// on top of the per-shot normalization instead of replacing it. Endpoints
    /// stay pinned (0→0, 255→255): this redistributes the middle, it does not
    /// wash the picture out.
    pub shadow_lift: u8,
}

impl Default for ComposeParams {
    fn default() -> ComposeParams {
        ComposeParams {
            // Edge thresholds live on the FACTORY's E scale (INTERFACES
            // note 18: E ≈ L* contrast, hysteresis seeds at t_hi 28, corpus
            // max ~130) after box-average dilution to cell resolution — NOT
            // on a full 0..255 synthetic scale. Re-anchored at M3
            // integration from a corpus sweep (fine-texture clip F1 0.72 @ T_on 32 with
            // precision 0.77 — the coherence gates carry noise suppression;
            // 0.52 @ 40, 0.00 @ the original 96).
            edge_t_on: 32,
            edge_t_off: 16,
            coh_min_q8: 96,       // 0.375
            coh_dir_q8: 160,      // 0.625
            hi_cut_q8: 160,       // 0.625 of the ramp
            edge_white_cut_q8: 240,
            halfblock_min_delta: 64,
            edge_strong: 96,      // ascii '+' → '#', same E scale
            // Noise floor only (M4 review): 1-LSB resample noise cannot
            // exceed E = 1, so `> 2` arms on genuine ink and `> 1` holds it
            // — the whole E ∈ [2, 15] fine-diagonal band stays served.
            quad_e_on: 2,
            quad_e_off: 1,
            // M3 Tune: 160 (0.625·step) from the committed corpus sweep —
            // fine-texture flicker 2.313 → 1.651 (fixes the ≤ 2 gate breach) with
            // edge F1 invariant (the near-white veto rides the plain index)
            // and mean corpus ssim flat. The §3.5 spec nominal 0.35·step
            // remains `hysteresis::IDX_HYST_Q8` (= 90).
            idx_hyst_q8: 160,
            // Off by default: the shipped corpus was tuned and signed off
            // without it, and every committed golden assumes the plain linear
            // window. It is a per-clip choice, not a global look change.
            shadow_lift: 0,
        }
    }
}

/// Highlight fg boost (§3.4 "highlights may boost value"): +25% toward white.
#[inline]
fn boost(c: Rgb) -> Rgb {
    Rgb::new(
        c.r + ((255 - c.r) >> 2),
        c.g + ((255 - c.g) >> 2),
        c.b + ((255 - c.b) >> 2),
    )
}

/// Scale the cell chroma to one vertical tap's luma, preserving the cell mean
/// (`m` = mean of the two post-LUT taps): channel · l / m, clamped.
#[inline]
fn shade(c: Rgb, l: u8, m: u8) -> Rgb {
    let s = |v: u8| ((v as u32 * l as u32) / m as u32).min(255) as u8;
    Rgb::new(s(c.r), s(c.g), s(c.b))
}

/// §3.5 per-cell selection. `(col, row)` index into `state` (viewport cells).
///
/// Layer priority (§3.4, override never blend): edge (gated + coherent, base
/// not near-white) → deep-shadow clamp (H bit1 → darkest step) → highlight
/// (H bit0, `idx < HI_CUT`) → half-block/quadrant (unicode) or `" - _`
/// subposition (ascii) when `|top−bottom|` is large → base ramp. Foreground
/// is always the chroma sample (gray fallback); the backend quantizes.
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

/// [`compose_cell`] plus the winning [`layer`] id (render metadata — the
/// eval harness's edge-F1 prediction side). Same selection, same state
/// mutations; `compose_cell` is this function with the tag dropped.
pub fn compose_cell_layer(
    inp: &CellInputs,
    lut: &[u8; 256],
    set: &PaletteSet,
    params: &ComposeParams,
    state: &mut HysteresisState,
    col: u16,
    row: u16,
) -> (Cell, u8) {
    // Per-shot normalization LUT (§3.5 line 2) on each vertical tap.
    let lt = lut[inp.luma_top as usize];
    let lb = lut[inp.luma_bottom as usize];
    let n = ((lt as u16 + lb as u16 + 1) >> 1) as u8;
    let len = set.base.len();
    let deep_shadow = inp.h & h_flags::DEEP_SHADOW != 0;

    let s = state.cell_mut(col, row);

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

/// Resampled feature planes for one frame, all at viewport resolution
/// (`luma2` at Vc×2Vr, everything else Vc×Vr; oversized buffers are fine).
///
/// `None` planes auto-disable their layer — an M1-era Y(+C) asset composes
/// through the exact base path with no edge/highlight leakage (back-compat,
/// PLAN §4 plane-registry detection is the caller's job).
#[derive(Clone, Copy, Debug)]
pub struct FramePlanes<'a> {
    /// Luma at Vc × 2Vr (two vertical taps per cell, §3.3).
    pub luma2: &'a [u8],
    pub e: Option<&'a [u8]>,
    pub ex: Option<&'a [u8]>,
    pub ey: Option<&'a [u8]>,
    pub h: Option<&'a [u8]>,
    /// Chroma resampled to cell resolution, split channels (r, g, b).
    pub chroma: Option<(&'a [u8], &'a [u8], &'a [u8])>,
}

/// Full-frame §3.5 composition: [`compose_cell`] per viewport cell,
/// [`Cell::BLANK`] pads. The edge layer runs only when E, Ex AND Ey are all
/// present. Never allocates.
///
/// # Panics
/// If `out` doesn't match the viewport's terminal dimensions, if `state`
/// isn't sized `vp.cols × vp.rows`, or if any provided plane is short.
pub fn compose_frame(
    planes: &FramePlanes<'_>,
    vp: &Viewport,
    lut: &[u8; 256],
    set: &PaletteSet,
    params: &ComposeParams,
    state: &mut HysteresisState,
    out: &mut Grid<Cell>,
) {
    frame_impl(planes, vp, lut, set, params, state, out, None);
}

/// [`compose_frame`] that also records the winning [`layer`] id per cell
/// into `mask` — the **LayerMask** render metadata (M3). `mask` must match
/// `out`'s full terminal dimensions; pads are [`layer::BASE`]. Identical
/// cell output and state mutations to [`compose_frame`]; the eval driver
/// crops the viewport and selects [`layer::EDGE`] for the §6 edge-F1
/// prediction side.
///
/// # Panics
/// As [`compose_frame`], plus a `mask`/`out` dimension mismatch.
#[allow(clippy::too_many_arguments)] // mirrors compose_frame + the one output it exists for
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
    frame_impl(planes, vp, lut, set, params, state, out, Some(mask));
}

#[allow(clippy::too_many_arguments)] // internal: the two pub wrappers are the API
fn frame_impl(
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

    out.fill(Cell::BLANK);
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
            };
            let (cell, won) =
                compose_cell_layer(&inp, lut, set, params, state, c as u16, r as u16);
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

    // ---- M3 three-layer compositor (§3.5) ----

    use crate::palette::{ColorDepth, GlyphTier, select_palettes};

    fn ident() -> [u8; 256] {
        core::array::from_fn(|i| i as u8)
    }

    /// Bias-128 half-scale plane bytes for an edge whose *tangent* runs along
    /// image-space θ. The asset stores the GRADIENT doubled-angle convention
    /// = the tangent one negated (factory edges.rs), so encode with `−`.
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
        CellInputs { luma_top: n, luma_bottom: n, e: 0, ex: 128, ey: 128, h: 0, chroma: None }
    }

    /// Synthetic oriented edge → correct directional glyph for every bin.
    #[test]
    fn oriented_edge_glyph_per_bin() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        // Image-space θ (y down): bins 1–2 render '\', bins 5–6 '/'.
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
        let (ex, ey) = exy(0.0, 200.0); // horizontal stroke
        // Bright top tap → top subposition.
        let top = CellInputs { luma_top: 200, luma_bottom: 20, e: 200, ex, ey, h: 0, chroma: None };
        assert_eq!(cell_with(&top, &ascii).glyph(), '='); // ASCII overline surrogate
        assert_eq!(cell_with(&top, &uni).glyph(), '‾');
        let bot = CellInputs { luma_top: 20, luma_bottom: 200, ..top };
        assert_eq!(cell_with(&bot, &ascii).glyph(), '_');
        assert_eq!(cell_with(&bot, &uni).glyph(), '_');
    }

    #[test]
    fn junction_when_bins_conflict() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        // Coherence 2·15/60 = 0.5 — inside the junction band [0.375, 0.625);
        // e = 60 sits between T_on (32) and edge_strong (96) → '+'.
        let inp = CellInputs { e: 60, ex: 128 + 15, ey: 128, ..base_inp(100) };
        assert_eq!(cell_with(&inp, &ascii).glyph(), '+');
        // Same conflict at strong magnitude (2·40/180 ≈ 0.44, e ≥ 96) → '#'.
        let strong = CellInputs { e: 180, ex: 128 + 40, ey: 128, ..base_inp(100) };
        assert_eq!(cell_with(&strong, &ascii).glyph(), '#');
    }

    #[test]
    fn low_coherence_suppresses_edge() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        // Directions cancelled inside the cell: |v| ≈ 0 despite e = 200.
        let inp = CellInputs { e: 200, ex: 131, ey: 126, ..base_inp(100) };
        let cell = cell_with(&inp, &ascii);
        assert_eq!(cell.glyph(), ascii.base.glyph(3), "falls back to base ramp");
    }

    #[test]
    fn near_white_base_beats_edge() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        let (ex, ey) = exy(90.0, 200.0);
        let inp = CellInputs { e: 200, ex, ey, ..base_inp(255) };
        assert_eq!(cell_with(&inp, &ascii).glyph(), '@', "§3.4: edge never overrides near-white");
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
        // Defaults: T_on 32 / T_off 16 (factory E scale — see Default impl).
        assert_eq!(at(40, &mut st), '|', "above T_on: edge turns on");
        assert_eq!(at(20, &mut st), '|', "T_off < e < T_on holds while was_edge");
        assert_eq!(at(10, &mut st), ascii.base.glyph(3), "below T_off: edge drops");
        assert_eq!(at(20, &mut st), ascii.base.glyph(3), "T_off alone cannot re-arm");
        // Scene-cut reset clears was_edge memory too.
        assert_eq!(at(40, &mut st), '|');
        st.reset();
        assert_eq!(at(20, &mut st), ascii.base.glyph(3), "after cut, T_on required again");
    }

    #[test]
    fn highlight_gate_and_boost() {
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        // idx 3 < HI_CUT 5 (len 8 · 160/256) → highlight glyph, boosted fg.
        let inp = CellInputs { h: h_flags::HIGHLIGHT, ..base_inp(100) };
        let cell = cell_with(&inp, &ascii);
        assert_eq!(cell.glyph(), '+'); // hidx = 3·4/5 = 2 in " .+*"
        assert_eq!(cell.fg, Rgb::gray(138), "gray(100) boosted 25% toward white");
        // Bright base: gate closed, highlight never fires (no leak).
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
        // Chroma pair: scaled around the cell mean, clamped.
        let color = CellInputs { chroma: Some(Rgb::new(200, 100, 50)), ..top };
        let cell = cell_with(&color, &uni);
        assert_eq!(cell.glyph(), '▀');
        assert_eq!(cell.fg, Rgb::new(255, 181, 90));
        assert_eq!(cell.bg, Rgb::new(36, 18, 9));
        // ASCII tier gets `"`/`_` instead (§3.5 subposition path).
        let ascii = select_palettes(GlyphTier::Ascii, ColorDepth::True, 100);
        assert_eq!(cell_with(&top, &ascii).glyph(), '"');
        assert_eq!(cell_with(&bot, &ascii).glyph(), '_');
    }

    #[test]
    fn quadrant_from_pair_plus_orientation() {
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        // Coherent diagonal below the edge gate (e=24 ≤ T_on 32), bright top.
        let (ex, ey) = exy(45.0, 24.0);
        let inp = CellInputs { luma_top: 200, luma_bottom: 20, e: 24, ex, ey, h: 0, chroma: None };
        assert_eq!(cell_with(&inp, &uni).glyph(), '▝');
        let flipped = CellInputs { luma_top: 20, luma_bottom: 200, ..inp };
        assert_eq!(cell_with(&flipped, &uni).glyph(), '▖');
        // Vertical orientation can't pick a side (needs 2Vc) → half-block.
        let (ex, ey) = exy(90.0, 24.0);
        let v = CellInputs { ex, ey, ..inp };
        assert_eq!(cell_with(&v, &uni).glyph(), '▀');
    }

    /// M3 review low (regression): a 1-LSB resample-noise (Ex,Ey) vector
    /// over a near-zero E plane must NOT steer quadrant refinement —
    /// coherence divides by max(e, 1), so (129, 128) at e ∈ {0, 1} reads as
    /// "fully coherent" without a magnitude floor and picked a noise-driven
    /// corner quadrant instead of the half-block.
    #[test]
    fn lsb_noise_orientation_never_picks_quadrant() {
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        for e in [0u8, 1] {
            for (ex, ey) in [(129u8, 128u8), (127, 128), (128, 129), (129, 127)] {
                let inp = CellInputs { luma_top: 200, luma_bottom: 20, e, ex, ey, h: 0, chroma: None };
                let cell = cell_with(&inp, &uni);
                assert_eq!(
                    cell.glyph(),
                    '▀',
                    "e={e} exy=({ex},{ey}): noise must fall through to the half-block"
                );
            }
        }
        // The floor is a NOISE floor: the first magnitude above 1-LSB noise
        // already refines (arm threshold is strict `>`).
        let p = ComposeParams::default();
        let e = p.quad_e_on + 1;
        let (ex, ey) = exy(45.0, f64::from(e));
        let inp = CellInputs { luma_top: 200, luma_bottom: 20, e, ex, ey, h: 0, chroma: None };
        assert_eq!(cell_with(&inp, &uni).glyph(), '▝', "arms one LSB above the noise floor");
    }

    /// M4 review (regression): the floor must not eat the fine-diagonal band
    /// it exists to serve. Resampled E is diluted by the cell box average
    /// (corpus source E mean ≈ 3.6), so a genuine fine diagonal lands around
    /// cell E 6–11 — far below the edge gate's hold threshold (16), which an
    /// earlier fix used as the floor and which silently downgraded every one
    /// of these cells to a plain half-block.
    #[test]
    fn fine_diagonal_band_still_refines_to_quadrants() {
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        let p = ComposeParams::default();
        assert!(p.quad_e_on < p.edge_t_off, "the floor must sit below the edge gate");
        for e in 3u8..=15 {
            // Coherent 45°/135° strokes at the cell's own magnitude.
            let (ex, ey) = exy(45.0, f64::from(e));
            let up = CellInputs { luma_top: 200, luma_bottom: 20, e, ex, ey, h: 0, chroma: None };
            assert_eq!(cell_with(&up, &uni).glyph(), '▝', "E={e} diagonal lost its quadrant");
            let (ex, ey) = exy(135.0, f64::from(e));
            let dn = CellInputs { ex, ey, ..up };
            assert_eq!(cell_with(&dn, &uni).glyph(), '▘', "E={e} diagonal lost its quadrant");
        }
    }

    /// M4 review (regression): a single hard threshold on the E plane lets a
    /// cell whose resampled magnitude dithers across it alternate
    /// quadrant/half-block every frame — invisible to the ≤2 switches/cell/s
    /// flicker gate, which only sees the ascii-tier eval render. The floor is
    /// therefore a dual threshold: once armed, a 1-LSB dip holds the quadrant.
    #[test]
    fn quadrant_floor_is_dither_stable() {
        let uni = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        let (lut, p) = (ident(), ComposeParams::default());
        let mut st = HysteresisState::new(1, 1);
        let at = |e: u8, st: &mut HysteresisState| {
            let (ex, ey) = exy(45.0, 8.0); // steady coherent orientation
            let inp = CellInputs { luma_top: 200, luma_bottom: 20, e, ex, ey, h: 0, chroma: None };
            compose_cell(&inp, &lut, &uni, &p, st, 0, 0).glyph()
        };
        // Arm just above the floor, then dither across it: no switching.
        assert_eq!(at(p.quad_e_on + 1, &mut st), '▝');
        for _ in 0..8 {
            assert_eq!(at(p.quad_e_on, &mut st), '▝', "hold threshold absorbs the dip");
            assert_eq!(at(p.quad_e_on + 1, &mut st), '▝');
        }
        // Below the hold threshold it drops, and the arm threshold alone is
        // then required to come back (same shape as the edge gate).
        assert_eq!(at(p.quad_e_off, &mut st), '▀', "below the hold threshold: half-block");
        assert_eq!(at(p.quad_e_on, &mut st), '▀', "the hold threshold cannot re-arm");
        assert_eq!(at(p.quad_e_on + 1, &mut st), '▝');
        // Scene-cut reset clears the memory too.
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
        // Non-edge cells never emit braille (no solid fills — §3.4 row 7).
        let flat = cell_with(&base_inp(255), &braille);
        assert!(!('\u{2800}'..='\u{28FF}').contains(&flat.glyph()));
    }

    #[test]
    fn frame_backcompat_y_only_is_pure_base() {
        // M1-era Y(+C) asset: no E/Ex/Ey/H planes → base path only.
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
                    // Sub-cell structure may legitimately pick a half-block…
                    assert!(matches!(cell.glyph(), '▀' | '▄'), "({c},{r})");
                } else {
                    assert_eq!(cell.glyph(), set.base.glyph(hysteresis_idx(n, set.base.len(), crate::hysteresis::IDX_UNSET, crate::hysteresis::IDX_HYST_Q8)), "({c},{r})");
                    assert_eq!(cell.fg, Rgb::gray(n));
                }
            }
        }
        // Pads blank.
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

    /// The LayerMask mirrors the §3.4 priority decision cell-for-cell and
    /// the masked variant renders byte-identically to the unmasked one.
    #[test]
    fn layer_mask_tags_winning_layers() {
        let vp = compute_viewport(40, 12, 2.0).unwrap();
        let (vc, vr) = (vp.cols as usize, vp.rows as usize);
        let set = select_palettes(GlyphTier::Ascii, ColorDepth::True, vp.cols);
        // Flat mid-gray field…
        let luma2 = vec![100u8; vc * 2 * vr];
        let mut e = vec![0u8; vc * vr];
        let mut ex = vec![128u8; vc * vr];
        let mut ey = vec![128u8; vc * vr];
        let mut h = vec![0u8; vc * vr];
        // …with one strong vertical edge, one highlight, one deep shadow.
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
        mask.fill(0xEE); // poison: every cell must be written
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

        // Byte-identical to the unmasked path (fresh state: same decisions).
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
