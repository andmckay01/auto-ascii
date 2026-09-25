//! `letters` — printable characters for texture and edges.
//!
//! Where [`pixels`](super::pixels) paints a low-resolution picture out of
//! shade and half-block cells, `letters` draws with type: a luminance ramp of
//! letters, digits and punctuation ordered by measured ink, directional ASCII
//! strokes on edges, and a top-/bottom-heavy glyph variant where a cell's two
//! halves disagree (the character-set stand-in for a half-block). Solid shapes
//! are kept for the densest fill only — `▓`/`█`, and `▀`/`▄` where a
//! near-white half meets a dark one — and only on tiers that can draw them;
//! on the ASCII tier every glyph is printable ASCII.
//!
//! Color is the same chroma pipeline as pixels (fg = the cell's chroma
//! sample, gray fallback; bg stays black), and so is temporal stability: the
//! ramp index, edge gate and orientation bin ride the same [`CellState`]
//! hysteresis, and the half-variant choice gets its own dual threshold in
//! the codec-private flag bits.
//!
//! **Ramp order.** Coverage was measured the way `auto-ascii-factory
//! font-table` does (antialiased ink over the advance-scaled cell) on Menlo
//! and SF Mono, and each ramp below is monotonic in both. Glyphs are picked
//! for *even* top/bottom mass so the base ramp reads as tone, not shape; the
//! stroke glyphs `| / \ - _` are kept out of it so an edge never looks like
//! texture and texture never looks like an edge.

use crate::cell::{Cell, Rgb};
use crate::codec::GlyphCodec;
use crate::compose::{CellInputs, ComposeParams, boost, h_flags, layer, shade};
use crate::hysteresis::{CellState, IDX_UNSET, cell_flags, edge_gate};
use crate::orient::{bin_with_guard, coherence_at_least, debias};
use crate::palette::{GlyphClass, PaletteSet, subpos};

/// Base ramp, darkest first — 16 steps of printable ASCII, monotonic in
/// measured ink (Menlo coverage 0 → 0.196).
pub const LETTERS_RAMP: &[char] = &[
    ' ', '.', ':', ';', '+', 'c', 'x', 'n', 'o', 'e', 'S', 'G', 'D', '8', 'B', 'M',
];

/// Ink-in-the-top-half variant of each [`LETTERS_RAMP`] step (same index,
/// similar total ink), used when the top tap is decisively brighter.
pub const LETTERS_TOP: &[char] = &[
    ' ', '\'', '\'', '"', '"', '7', 'T', 'Y', 'F', 'P', 'P', 'P', 'M', 'M', 'M', 'M',
];

/// Ink-in-the-bottom-half variant of each [`LETTERS_RAMP`] step.
pub const LETTERS_BOTTOM: &[char] = &[
    ' ', '.', '.', ',', ',', 'u', 'u', 'a', 'a', 'w', 'g', 'g', 'g', 'g', 'g', 'g',
];

/// Dense fill for near-white cells on tiers that can draw blocks — where no
/// printable glyph carries enough ink (the densest letter inks about a fifth
/// of the cell, `█` all of it).
pub const LETTERS_FILL: char = '█';

/// Tone at/above which a cell fills instead of lettering. Decided on the
/// plain tone, not the ramp's contrast curve, so it is a brightness
/// threshold in the picture's own terms: 236 keeps fill to true highlights
/// (a lit window, a white core) and off ordinary lit skin, where isolated
/// blocks read as speckle rather than light.
pub const LETTERS_FILL_MIN: u8 = 236;

/// Once filled, a cell stays filled down to this tone — the fill's own dual
/// threshold. It reads the CURRENT tone, not the held one: the held tone
/// trails a slow brightening by up to its deadband, which left a white core
/// lettered inside a ragged ring of blocks.
const FILL_HOLD: u8 = 208;

/// Top/bottom-heavy variants of the fill.
const FILL_TOP: char = '▀';
const FILL_BOTTOM: char = '▄';

/// Directional strokes per `[GlyphClass][SubPos]` (top / mid / bottom).
const EDGE: [[char; 3]; 4] = [
    ['-', '-', '_'],    // H: a horizontal boundary; `_` when the ink sits low
    ['\\', '\\', '\\'], // DiagDown
    ['|', '|', '|'],    // V
    ['/', '/', '/'],    // DiagUp
];
/// Orientation conflict inside the cell (coherence in the junction band).
/// One glyph at every magnitude, like the Unicode tier's `┼`: a strong/weak
/// pair swaps glyphs whenever E dithers across `edge_strong`, and that swap
/// measured as a real share of letters' edge flicker.
const JUNCTION: char = '+';

/// Codec-private flag bits (see `cell_flags`): the held half variant.
const HALF_SHIFT: u8 = 2;
const HALF_MASK: u8 = 0b11 << HALF_SHIFT;
const HALF_NONE: u8 = 0;
const HALF_TOP: u8 = 1;
const HALF_BOTTOM: u8 = 2;
/// Codec-private flag bit: the cell was filled last frame.
const WAS_FILL: u8 = 1 << 4;

/// Hold threshold for the half variant, as a fraction of the arm threshold
/// (`halfblock_min_delta`) in Q8: once a cell reads top- or bottom-heavy it
/// keeps that shape until the taps close to within 5/8 of the arm distance,
/// so a pair dithering around the threshold cannot swap glyphs every frame.
const HALF_HOLD_Q8: u16 = 160;

/// Ink gain on the glyph color, Q8 (384 = up to 1.5×). A glyph inks a fifth
/// of its cell at most where a pixels block inks all of it, so at the same
/// color a lettered region reads several times darker; lifting the color
/// toward full value (hue kept, capped at 255) hands luminance to the
/// glyph's DENSITY, which is what the ramp is for.
const INK_GAIN_Q8: u32 = 384;

/// Scale `c` by up to [`INK_GAIN_Q8`], stopping where its brightest channel
/// reaches 255 — the hue survives, the value rises.
#[inline]
fn ink(c: Rgb) -> Rgb {
    let m = c.r.max(c.g).max(c.b) as u32;
    if m == 0 {
        return c;
    }
    let g = INK_GAIN_Q8.min((255 << 8) / m);
    let s = |v: u8| ((v as u32 * g) >> 8).min(255) as u8;
    Rgb::new(s(c.r), s(c.g), s(c.b))
}

/// Tone hysteresis: the luma a cell last DISPLAYED, held until the input
/// moves more than a deadband away from it.
///
/// `hysteresis_idx` holds a cell for a fraction of one ramp step — right for
/// pixels' 6–8 step ramps, but a letters step is a third of that in luma, so
/// the same fraction let a cell change glyph at roughly twice pixels' rate on
/// the same video noise. Holding the luma instead makes the band independent
/// of ramp length (and of the contrast curve, which would otherwise stretch
/// it in the shadows). It is measured in steps of the tier's PIXELS ramp
/// (`ref_len` steps): 13/8 × `hyst_q8` of one such step (≈ 43 luma units
/// at the default 160 on the Unicode tier). At 1× the sample clips still
/// switched ~1.3× as often as pixels — a finer ramp turns more real motion
/// into glyph changes — and 13/8 lands within ±3.5% of pixels'
/// switches/cell/s on four sample clips (dark, bright, faces, high motion)
/// at 120x40 and 200x56, and below pixels on the ASCII tier, while a fresh
/// cell still quantizes at letters' full tonal resolution. The hysteresis
/// dial still scales it and 0 still turns it off.
///
/// The held value lives in `CellState::idx` (this codec's own reading of
/// that byte; a codec switch resets it), capped at 254 so it can never be
/// mistaken for [`IDX_UNSET`].
#[inline]
fn held_tone(n: u8, prev: u8, hyst_q8: u8, ref_len: u8) -> u8 {
    let band = (hyst_q8 as u16 * 13 / 8 / ref_len.max(1) as u16) as u8;
    let h = if prev == IDX_UNSET || n.abs_diff(prev) > band { n } else { prev };
    h.min(IDX_UNSET - 1)
}

/// The ramp's contrast curve: `(n + n²/256) / 2` — endpoints fixed, the
/// middle pulled down (128 → 96), monotonic, integer-only.
#[inline]
fn tone(n: u8) -> u8 {
    ((n as u32 * (256 + n as u32)) >> 9) as u8
}

/// The `letters` codec. See the module docs.
pub struct Letters;

/// Which half (if any) carries the cell's ink this frame — dual threshold
/// on `|top − bottom|`, remembered in `flags`.
#[inline]
fn half_variant(lt: u8, lb: u8, arm: u8, flags: &mut u8) -> u8 {
    let held = (*flags & HALF_MASK) >> HALF_SHIFT;
    let hold = ((arm as u16 * HALF_HOLD_Q8) >> 8) as u8;
    let dir = if lt >= lb { HALF_TOP } else { HALF_BOTTOM };
    let d = lt.abs_diff(lb);
    let v = if d >= arm.max(1) || (held == dir && d >= hold.max(1)) { dir } else { HALF_NONE };
    *flags = (*flags & !HALF_MASK) | (v << HALF_SHIFT);
    v
}

impl GlyphCodec for Letters {
    const NAME: &'static str = "letters";

    /// Layer priority mirrors pixels (§3.4): edge → deep shadow →
    /// highlight → half variant (STRUCTURE) → base ramp.
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
        let deep_shadow = inp.h & h_flags::DEEP_SHADOW != 0;
        // Hold the displayed luma (see `held_tone`); deep shadow clamps to
        // black and the clamp IS the held state.
        let h = if deep_shadow {
            0
        } else {
            held_tone(n, s.idx, params.idx_hyst_q8, set.base.len())
        };
        s.idx = h;
        // Glyph texture separates tones far more weakly than a shade block
        // does (mid letters all read as one grey weight), so the ramp index
        // rides a contrast curve: mid-darks drop to sparse punctuation, lit
        // areas climb into the dense letters. Glyph choice only — the color
        // still carries the plain tone.
        let t = tone(h);
        // Blocks only where the tier can draw them (set.halfblock ⇔ Unicode).
        // Tracked every frame, like the edge gate, whichever layer wins.
        let half = half_variant(lt, lb, params.halfblock_min_delta, &mut s.flags);
        // Fill is judged where the ink sits: the bright half when the cell
        // draws a half variant (a near-white half over a dark one is `▀`/`▄`),
        // the whole cell otherwise.
        let ink_tone = if half == HALF_NONE { n } else { lt.max(lb) };
        let dense = set.halfblock
            && !deep_shadow
            && (ink_tone >= LETTERS_FILL_MIN || (s.flags & WAS_FILL != 0 && ink_tone >= FILL_HOLD));
        if dense {
            s.flags |= WAS_FILL;
        } else {
            s.flags &= !WAS_FILL;
        }
        let len = LETTERS_RAMP.len() as u8;

        let idx = ((t as u32 * len as u32) >> 8).min(len as u32 - 1) as u8;

        let was_edge = s.flags & cell_flags::WAS_EDGE != 0;
        let edge_on = edge_gate(inp.e, was_edge, params.edge_t_on, params.edge_t_off);
        if edge_on {
            s.flags |= cell_flags::WAS_EDGE;
        } else {
            s.flags &= !cell_flags::WAS_EDGE;
        }

        let fg = ink(inp.chroma.unwrap_or(Rgb::gray(n)));
        let dx = -debias(inp.ex);
        let dy = -debias(inp.ey);
        // Pixels' near-white veto EXACTLY — plain tone over the tier's pixels
        // ramp — so both codecs agree on where an edge may draw. Measured on
        // the curved index over letters' longer ramp instead, the veto only
        // bit near n ≈ 247 and strokes dithered on and off across bright fire.
        let base_len = set.base.len() as u32;
        let plain_idx = ((n as u32 * base_len) >> 8).min(base_len - 1);
        let near_white = ((plain_idx + 1) << 8) > params.edge_white_cut_q8 as u32 * base_len;

        if edge_on && !near_white && coherence_at_least(dx, dy, inp.e, params.coh_min_q8) {
            let g = if coherence_at_least(dx, dy, inp.e, params.coh_dir_q8) {
                let bin = bin_with_guard(dx, dy, s.bin);
                s.bin = bin;
                let class = GlyphClass::from_bin(bin) as usize;
                EDGE[class][subpos(lt, lb, params.halfblock_min_delta) as usize]
            } else {
                JUNCTION
            };
            return (Cell::new(g, fg, Rgb::BLACK), layer::EDGE);
        }

        if deep_shadow {
            return (Cell::new(LETTERS_RAMP[0], fg, Rgb::BLACK), layer::SHADOW);
        }

        let hi_cut = ((len as u32 * params.hi_cut_q8 as u32) >> 8).max(1);
        if inp.h & h_flags::HIGHLIGHT != 0 && (idx as u32) < hi_cut {
            let hlen = set.highlight.len() as u32;
            let hidx = ((idx as u32 * hlen) / hi_cut).min(hlen - 1) as u8;
            return (Cell::new(set.highlight.glyph(hidx), boost(fg), Rgb::BLACK), layer::HIGHLIGHT);
        }

        let i = idx as usize;
        if half != HALF_NONE && i > 0 {
            // The ink sits in the bright half, so it takes that half's shade.
            let top = half == HALF_TOP;
            let g = match (dense, top) {
                (true, true) => FILL_TOP,
                (true, false) => FILL_BOTTOM,
                (false, true) => LETTERS_TOP[i],
                (false, false) => LETTERS_BOTTOM[i],
            };
            let c = ink(match inp.chroma {
                Some(c) => shade(c, lt.max(lb), n.max(1)),
                None => Rgb::gray(lt.max(lb)),
            });
            return (Cell::new(g, c, Rgb::BLACK), layer::STRUCTURE);
        }

        let g = if dense { LETTERS_FILL } else { LETTERS_RAMP[i] };
        (Cell::new(g, fg, Rgb::BLACK), layer::BASE)
    }
}

/// Every glyph `letters` can emit on a tier with (`blocks`) or without
/// block drawing — the repertoire the allowed-glyph tests pin.
pub fn letters_glyphs(blocks: bool) -> Vec<char> {
    let mut out: Vec<char> = LETTERS_RAMP
        .iter()
        .chain(LETTERS_TOP)
        .chain(LETTERS_BOTTOM)
        .chain(EDGE.iter().flatten())
        .copied()
        .chain([JUNCTION])
        .chain(crate::palette::ASCII_HIGHLIGHT.iter().copied())
        .collect();
    if blocks {
        out.extend([LETTERS_FILL, FILL_TOP, FILL_BOTTOM]);
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hysteresis::HysteresisState;
    use crate::palette::{ColorDepth, GlyphTier, select_palettes};

    fn ident() -> [u8; 256] {
        core::array::from_fn(|i| i as u8)
    }

    fn inp(top: u8, bottom: u8) -> CellInputs {
        CellInputs { luma_top: top, luma_bottom: bottom, e: 0, ex: 128, ey: 128, h: 0, chroma: None }
    }

    fn glyph(i: &CellInputs, set: &PaletteSet, st: &mut HysteresisState) -> char {
        Letters::cell(i, &ident(), set, &ComposeParams::default(), st.cell_mut(0, 0)).0.glyph()
    }

    fn cold(i: &CellInputs, set: &PaletteSet) -> char {
        glyph(i, set, &mut HysteresisState::new(1, 1))
    }

    fn sets() -> (PaletteSet, PaletteSet) {
        (
            select_palettes(GlyphTier::Ascii, ColorDepth::True, 100),
            select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100),
        )
    }

    #[test]
    fn variant_tables_match_the_ramp() {
        assert_eq!(LETTERS_TOP.len(), LETTERS_RAMP.len());
        assert_eq!(LETTERS_BOTTOM.len(), LETTERS_RAMP.len());
        let (mut r, mut t, mut b) = (LETTERS_RAMP.to_vec(), LETTERS_TOP.to_vec(), LETTERS_BOTTOM.to_vec());
        r.sort_unstable();
        r.dedup();
        assert_eq!(r.len(), LETTERS_RAMP.len(), "every base step is a distinct glyph");
        t.dedup();
        b.dedup();
        assert!(t.len() > 4 && b.len() > 4, "the variants carry real shape range");
        // Texture never looks like an edge: no stroke glyph in the base ramp.
        for g in EDGE.iter().flatten() {
            assert!(!LETTERS_RAMP.contains(g), "{g:?} is an edge stroke");
        }
    }

    /// The ramp is monotonic in tone on both tiers; blocks appear only for
    /// true highlights, and only where the tier can draw them.
    #[test]
    fn ramp_is_monotonic_and_fill_is_highlights_only() {
        let (ascii, uni) = sets();
        for set in [&ascii, &uni] {
            let pos = |g: char| LETTERS_RAMP.iter().position(|&r| r == g);
            let mut last = 0;
            for n in (0..LETTERS_FILL_MIN).step_by(3) {
                let g = cold(&inp(n, n), set);
                let p = pos(g).unwrap_or_else(|| panic!("tone {n} -> {g:?} off the ramp"));
                assert!(p >= last, "tone {n}: {g:?} darker than the step before");
                last = p;
            }
            assert_eq!(cold(&inp(0, 0), set), ' ');
        }
        assert_eq!(cold(&inp(255, 255), &ascii), 'M', "ascii tops out in letters");
        assert_eq!(cold(&inp(255, 255), &uni), LETTERS_FILL, "unicode fills a highlight");
        let below = cold(&inp(LETTERS_FILL_MIN - 1, LETTERS_FILL_MIN - 1), &uni);
        assert!(LETTERS_RAMP[LETTERS_RAMP.len() - 2..].contains(&below), "{below:?}");
        // The contrast curve: mid-gray lands below the middle of the ramp.
        let mid = LETTERS_RAMP.iter().position(|&g| g == cold(&inp(128, 128), &uni)).unwrap();
        assert!(mid < LETTERS_RAMP.len() / 2, "mid-gray at step {mid}");
    }

    #[test]
    fn fill_holds_down_to_its_hold_threshold() {
        let (_, uni) = sets();
        let mut st = HysteresisState::new(1, 1);
        assert_eq!(glyph(&inp(240, 240), &uni, &mut st), LETTERS_FILL);
        assert_eq!(glyph(&inp(FILL_HOLD, FILL_HOLD), &uni, &mut st), LETTERS_FILL, "held");
        assert_ne!(glyph(&inp(FILL_HOLD - 1, FILL_HOLD - 1), &uni, &mut st), LETTERS_FILL);
        assert_ne!(glyph(&inp(FILL_HOLD, FILL_HOLD), &uni, &mut st), LETTERS_FILL, "no re-arm");
    }

    /// The displayed tone holds inside the deadband and jumps past it; the
    /// hysteresis dial at 0 turns the hold off.
    #[test]
    fn tone_deadband_holds_small_wobble() {
        let (_, uni) = sets();
        let band = held_tone(0, 100, ComposeParams::default().idx_hyst_q8, uni.base.len());
        assert_eq!(band, 0, "a 100-unit move always updates");
        let mut st = HysteresisState::new(1, 1);
        let at = |n: u8, st: &mut HysteresisState| glyph(&inp(n, n), &uni, st);
        let start = at(120, &mut st);
        for wobble in [100u8, 140, 110, 150, 125] {
            assert_eq!(at(wobble, &mut st), start, "±30 around 120 is noise");
        }
        assert_ne!(at(200, &mut st), start, "a real change moves the glyph");
        let off = ComposeParams { idx_hyst_q8: 0, ..ComposeParams::default() };
        let mut st = HysteresisState::new(1, 1);
        let g = |n: u8, st: &mut HysteresisState| {
            Letters::cell(&inp(n, n), &ident(), &uni, &off, st.cell_mut(0, 0)).0.glyph()
        };
        g(120, &mut st);
        assert_eq!(g(150, &mut st), cold(&inp(150, 150), &uni), "dial at 0: no hold");
    }

    #[test]
    fn half_variants_follow_the_bright_half() {
        let (ascii, uni) = sets();
        let g = cold(&inp(200, 40), &ascii);
        assert!(LETTERS_TOP.contains(&g), "{g:?} is top-heavy");
        let g = cold(&inp(40, 200), &ascii);
        assert!(LETTERS_BOTTOM.contains(&g), "{g:?} is bottom-heavy");
        // A near-white half over a dark one on a block tier: the half block.
        assert_eq!(cold(&inp(255, 180), &uni), FILL_TOP);
        assert_eq!(cold(&inp(180, 255), &uni), FILL_BOTTOM);
        assert_ne!(cold(&inp(255, 180), &ascii), FILL_TOP, "never on the ascii tier");
    }

    /// The half variant arms at `halfblock_min_delta` and holds down to 5/8
    /// of it: a pair dithering around the arm threshold keeps its shape.
    #[test]
    fn half_variant_is_dither_stable() {
        let (ascii, _) = sets();
        let mut st = HysteresisState::new(1, 1);
        let arm = ComposeParams::default().halfblock_min_delta;
        let top = glyph(&inp(100 + arm, 100), &ascii, &mut st);
        assert!(LETTERS_TOP.contains(&top));
        for _ in 0..4 {
            assert_eq!(glyph(&inp(100 + arm - 10, 100), &ascii, &mut st), top, "held below arm");
            assert_eq!(glyph(&inp(100 + arm, 100), &ascii, &mut st), top);
        }
        let flat = glyph(&inp(100 + 20, 100), &ascii, &mut st);
        assert!(LETTERS_RAMP.contains(&flat), "released to the ramp: {flat:?}");
        // Re-arming from flat needs the full arm distance again.
        assert_eq!(glyph(&inp(100 + arm - 10, 100), &ascii, &mut st), flat);
    }

    #[test]
    fn edges_are_ascii_strokes() {
        let (_, uni) = sets();
        // Image-space θ (y down) per bin centre, encoded as the asset's
        // gradient convention (tangent negated), as in compose's tests.
        let expect = ['-', '\\', '\\', '|', '|', '/', '/', '-'];
        for (k, want) in expect.iter().enumerate() {
            let a = (2.0 * (11.25 + k as f64 * 22.5)).to_radians();
            let ex = (128.0 - 100.0 * a.cos()).round() as u8;
            let ey = (128.0 - 100.0 * a.sin()).round() as u8;
            let i = CellInputs { e: 200, ex, ey, ..inp(100, 100) };
            assert_eq!(cold(&i, &uni), *want, "bin {k}");
        }
    }

    #[test]
    fn repertoire_is_printable_ascii_plus_three_blocks() {
        for g in letters_glyphs(false) {
            assert!(g == ' ' || g.is_ascii_graphic(), "{g:?}");
        }
        let extra: Vec<char> =
            letters_glyphs(true).into_iter().filter(|g| !(*g == ' ' || g.is_ascii_graphic())).collect();
        assert_eq!(extra, vec!['▀', '▄', '█']);
    }
}
