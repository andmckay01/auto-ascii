//! `letters` — printable characters for texture and edges.
//!
//! Where [`pixels`](super::pixels) paints a low-resolution picture out of
//! shade and half-block cells, `letters` draws with type: a luminance ramp of
//! letters, digits and punctuation ordered by measured ink, directional ASCII
//! strokes on edges, and a top-/bottom-heavy glyph variant where a cell's two
//! halves disagree (the character-set stand-in for a half-block). Solid shapes
//! are kept for light only — `█` for near-white cells, `▀`/`▄` where a lit
//! half meets a dark one — and only on tiers that can draw them; on the
//! ASCII tier every glyph is printable ASCII.
//!
//! Color starts from the same chroma sample as pixels (gray fallback). A
//! glyph inks only a fifth of its cell, so where the palette allows a tinted
//! background (`PaletteSet::bg_tint`: truecolor and 256-color) the cell's
//! tone rides the background too: fg and bg are the chroma scaled by a
//! coverage curve of the tone — bg at 0.6×, the glyph brighter, solid blocks
//! at pixels' own colors — so a cell averages to what pixels draws there and
//! a dark-but-lit face keeps its shape. On 16-color and mono the background
//! stays black and the glyph takes a ≤1.5× value gain. Temporal stability
//! is pixels' too: the ramp index, edge gate and orientation bin ride the
//! same [`CellState`] hysteresis, the black floor holds a lit cell down to
//! half its threshold, and the half-variant choice gets its own dual
//! threshold in the codec-private flag bits.
//!
//! **Ramp order.** Coverage was measured the way `auto-ascii-factory
//! font-table` does (antialiased ink over the advance-scaled cell) on Menlo
//! and SF Mono, and each ramp below is monotonic in both. Glyphs are picked
//! for *even* top/bottom mass so the base ramp reads as tone, not shape; the
//! stroke glyphs `| / \ - _ = X` are kept out of every ramp table so an edge
//! never looks like texture and texture never looks like an edge.
//!
//! **Design constants.** The glyph tables and the handful of thresholds
//! below (`LETTERS_FILL_MIN`, `FILL_HOLD`, `HALF_BLOCK_MIN_IDX`,
//! `HALF_HOLD_Q8`, `INK_GAIN_Q8`, `FG_MIN_Q8`, `FG_Q8`, `BG_Q8`,
//! `TONE_STEPS`, `BLACK_FLOOR`, `FLOOR_HOLD`, the coverage curve in `paint`
//! and the deadband factor in `held_tone`) are
//! this codec's DATA, in the same sense as the palettes in `palette.rs`: they
//! define what `letters` looks like and are pinned by its unit tests and
//! goldens. What a viewer or the eval sweep tunes stays in `ComposeParams`
//! (`params.toml [compose]`), which every codec reads — the hysteresis dial
//! scales the deadband here, the edge dials gate the strokes, shadow lift
//! bends the LUT — so no letters threshold is a second home for a tunable.

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

const FILL_HOLD: u8 = 208;

const FILL_TOP: char = '▀';
const FILL_BOTTOM: char = '▄';

const HALF_BLOCK_MIN_IDX: usize = 10;

const EDGE: [[char; 3]; 4] = [
    ['=', '-', '_'],
    ['\\', '\\', '\\'],
    ['|', '|', '|'],
    ['/', '/', '/'],
];
const JUNCTION: char = 'X';

const HALF_SHIFT: u8 = 2;
const HALF_MASK: u8 = 0b11 << HALF_SHIFT;
const HALF_NONE: u8 = 0;
const HALF_TOP: u8 = 1;
const HALF_BOTTOM: u8 = 2;
const WAS_FILL: u8 = 1 << 4;

const HALF_HOLD_Q8: u16 = 160;

const INK_GAIN_Q8: u32 = 384;

const FG_MIN_Q8: u32 = 192;
const FG_Q8: u32 = 512;
const BG_Q8: u32 = 154;

// Fixed tone scale: preserve the eight-step ASCII look on every tier.
// The pixels palette must not change letters' black floor or deadband.
const TONE_STEPS: u16 = 8;
const BLACK_FLOOR: u8 = (256 / TONE_STEPS) as u8;
const FLOOR_HOLD: u8 = BLACK_FLOOR / 2;

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

#[inline]
fn scaled(c: Rgb, k: u32) -> Rgb {
    let s = |v: u8| ((v as u32 * k) >> 16).min(255) as u8;
    Rgb::new(s(c.r), s(c.g), s(c.b))
}

#[inline]
fn paint(c: Option<Rgb>, n: u8, set: &PaletteSet) -> (Rgb, Rgb) {
    let c = c.unwrap_or(Rgb::gray(n));
    if !set.bg_tint {
        return (ink(c), Rgb::BLACK);
    }
    let f = BLACK_FLOOR as u32;
    let x = ((n as u32).saturating_sub(f) << 8) / (255 - f);
    let cov = (x + ((x * x * (768 - 2 * x)) >> 16)) >> 1;
    let gain = FG_MIN_Q8 + ((cov * (FG_Q8 - FG_MIN_Q8)) >> 8);
    (scaled(c, gain << 8), scaled(c, cov * BG_Q8))
}

#[inline]
fn held_tone(n: u8, prev: u8, hyst_q8: u8) -> u8 {
    let band = (hyst_q8 as u16 * 5 / 2 / TONE_STEPS) as u8;
    let h = if prev == IDX_UNSET || n.abs_diff(prev) > band { n } else { prev };
    h.min(IDX_UNSET - 1)
}

#[inline]
fn tone(n: u8) -> u8 {
    ((n as u32 * (256 + n as u32)) >> 9) as u8
}

/// The `letters` codec. See the module docs.
pub struct Letters;

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

    /// Layer priority mirrors pixels: edge → deep shadow → highlight → half
    /// variant (STRUCTURE) → base ramp.
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
        let was_half = (s.flags & HALF_MASK) >> HALF_SHIFT;
        let half = half_variant(lt, lb, params.halfblock_min_delta, &mut s.flags);
        let ink_tone = if half == HALF_NONE { n } else { lt.max(lb) };
        let prev = if half == was_half { s.idx } else { IDX_UNSET };
        let floor = if prev != IDX_UNSET && prev >= BLACK_FLOOR { FLOOR_HOLD } else { BLACK_FLOOR };
        let h = if deep_shadow || ink_tone < floor {
            0
        } else {
            held_tone(ink_tone, prev, params.idx_hyst_q8)
        };
        s.idx = h;
        let dense = set.halfblock
            && !deep_shadow
            && (ink_tone >= LETTERS_FILL_MIN || (s.flags & WAS_FILL != 0 && ink_tone >= FILL_HOLD));
        if dense {
            s.flags |= WAS_FILL;
        } else {
            s.flags &= !WAS_FILL;
        }
        let len = LETTERS_RAMP.len() as u8;
        let black = h < BLACK_FLOOR;
        let idx = if black {
            0
        } else {
            ((tone(h) as u32 * len as u32) >> 8).min(len as u32 - 1) as u8
        };

        let was_edge = s.flags & cell_flags::WAS_EDGE != 0;
        let edge_on = edge_gate(inp.e, was_edge, params.edge_t_on, params.edge_t_off);
        if edge_on {
            s.flags |= cell_flags::WAS_EDGE;
        } else {
            s.flags &= !cell_flags::WAS_EDGE;
        }

        let (fg, bg) = paint(inp.chroma, n, set);
        let dx = -debias(inp.ex);
        let dy = -debias(inp.ey);
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
            return (Cell::new(g, fg, bg), layer::EDGE);
        }

        if deep_shadow {
            return (Cell::new(LETTERS_RAMP[0], fg, Rgb::BLACK), layer::SHADOW);
        }

        let hi_cut = ((len as u32 * params.hi_cut_q8 as u32) >> 8).max(1);
        if inp.h & h_flags::HIGHLIGHT != 0 && (idx as u32) < hi_cut {
            let hlen = set.highlight.len() as u32;
            let hidx = ((idx as u32 * hlen) / hi_cut).min(hlen - 1) as u8;
            return (Cell::new(set.highlight.glyph(hidx), boost(fg), bg), layer::HIGHLIGHT);
        }

        let i = idx as usize;
        if half != HALF_NONE && i > 0 {
            let top = half == HALF_TOP;
            let block = dense || (set.halfblock && i >= HALF_BLOCK_MIN_IDX);
            let g = match (block, top) {
                (true, true) => FILL_TOP,
                (true, false) => FILL_BOTTOM,
                (false, true) => LETTERS_TOP[i],
                (false, false) => LETTERS_BOTTOM[i],
            };
            let (lit, dim) = (lt.max(lb), lt.min(lb));
            let (fg, bg) = if set.bg_tint && block {
                match inp.chroma {
                    Some(c) => (shade(c, lit, n.max(1)), shade(c, dim, n.max(1))),
                    None => (Rgb::gray(lit), Rgb::gray(dim)),
                }
            } else if set.bg_tint {
                (paint(inp.chroma, lit, set).0, paint(inp.chroma, dim, set).1)
            } else {
                let c = inp.chroma.map_or(Rgb::gray(lit), |c| shade(c, lit, n.max(1)));
                (ink(c), Rgb::BLACK)
            };
            return (Cell::new(g, fg, bg), layer::STRUCTURE);
        }

        if dense {
            let c = if set.bg_tint { inp.chroma.unwrap_or(Rgb::gray(n)) } else { fg };
            return (Cell::new(LETTERS_FILL, c, bg), layer::BASE);
        }
        (Cell::new(LETTERS_RAMP[i], fg, bg), layer::BASE)
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
        for g in EDGE.iter().flatten().chain([&JUNCTION]) {
            for table in [LETTERS_RAMP, LETTERS_TOP, LETTERS_BOTTOM] {
                assert!(!table.contains(g), "{g:?} is an edge stroke");
            }
        }
    }

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

    #[test]
    fn tone_deadband_holds_small_wobble() {
        let (_, uni) = sets();
        let band = held_tone(0, 100, ComposeParams::default().idx_hyst_q8);
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
        assert_eq!(cold(&inp(255, 180), &uni), FILL_TOP);
        assert_eq!(cold(&inp(180, 255), &uni), FILL_BOTTOM);
        assert_ne!(cold(&inp(255, 180), &ascii), FILL_TOP, "never on the ascii tier");
    }

    #[test]
    fn half_variants_carry_the_lit_halfs_tone() {
        let (ascii, uni) = sets();
        let g = cold(&inp(200, 0), &ascii);
        assert!(LETTERS_TOP[HALF_BLOCK_MIN_IDX..].contains(&g), "lit top reads dense: {g:?}");
        assert_eq!(cold(&inp(200, 0), &uni), FILL_TOP);
        assert_eq!(cold(&inp(0, 200), &uni), FILL_BOTTOM);
        let g = cold(&inp(110, 20), &uni);
        assert!(LETTERS_TOP.contains(&g) && g != FILL_TOP, "{g:?}");
    }

    #[test]
    fn near_black_is_blank() {
        let (ascii, uni) = sets();
        for set in [&uni, &ascii] {
            let f = BLACK_FLOOR;
            assert_eq!(cold(&inp(f - 1, f - 1), set), ' ', "just under the floor");
            assert_ne!(cold(&inp(f + 2, f + 2), set), ' ', "just over it");
        }
    }

    #[test]
    fn tone_floor_and_hold_are_independent_of_tier() {
        let (ascii, _) = sets();
        for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
            for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
                for cols in [40, 100] {
                    let set = select_palettes(tier, color, cols);
                    let mut reference = HysteresisState::new(1, 1);
                    let mut st = HysteresisState::new(1, 1);
                    for n in [31, 34, 120, 100, 140, 110, 150, 200, 40, 0, 0] {
                        assert_eq!(glyph(&inp(n, n), &set, &mut st),
                            glyph(&inp(n, n), &ascii, &mut reference),
                            "tone {n}, {tier:?}, {color:?}, {cols} cols");
                    }
                }
            }
        }
    }

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
        assert_eq!(glyph(&inp(100 + arm - 10, 100), &ascii, &mut st), flat);
    }

    fn cold_cell(i: &CellInputs, set: &PaletteSet) -> Cell {
        let mut st = HysteresisState::new(1, 1);
        Letters::cell(i, &ident(), set, &ComposeParams::default(), st.cell_mut(0, 0)).0
    }

    fn cell_at(n: u8, chroma: Option<Rgb>, set: &PaletteSet) -> Cell {
        cold_cell(&CellInputs { chroma, ..inp(n, n) }, set)
    }

    fn luma(c: Rgb) -> u32 {
        (77 * c.r as u32 + 150 * c.g as u32 + 29 * c.b as u32) >> 8
    }

    #[test]
    fn tinted_background_carries_the_tone() {
        let skin = Rgb::new(200, 150, 120);
        for color in [ColorDepth::True, ColorDepth::C256] {
            let set = select_palettes(GlyphTier::UnicodeBlocks, color, 100);
            assert_eq!(cell_at(BLACK_FLOOR - 1, Some(skin), &set).bg, Rgb::BLACK, "black under the floor");
            let mut last = 0;
            for n in (BLACK_FLOOR as u16 + 8..LETTERS_FILL_MIN as u16).step_by(16) {
                let c = cell_at(n as u8, Some(skin), &set);
                let (f, b) = (luma(c.fg), luma(c.bg));
                assert!(b > last, "tone {n}: bg {b} must rise with tone");
                assert!(f >= 2 * b, "tone {n}: the glyph ({f}) must stand off its bg ({b})");
                assert!(f * 4 >= luma(skin) * 3 || f == 255, "tone {n}: glyph stays visible");
                last = b;
            }
        }
    }

    #[test]
    fn untinted_tiers_keep_a_black_background() {
        for color in [ColorDepth::C16, ColorDepth::Mono] {
            let set = select_palettes(GlyphTier::UnicodeBlocks, color, 100);
            for n in [40, 120, 200, 250] {
                let c = cell_at(n, Some(Rgb::new(90, 120, 60)), &set);
                assert_eq!(c.bg, Rgb::BLACK, "tone {n}, {color:?}");
                assert_eq!(c.fg, ink(Rgb::new(90, 120, 60)), "tone {n}, {color:?}");
            }
        }
    }

    #[test]
    fn solid_blocks_take_pixels_colors() {
        let (_, uni) = sets();
        let c = Rgb::new(180, 200, 190);
        assert_eq!(cell_at(250, Some(c), &uni).fg, c, "a full block is the chroma itself");
        let cell = cold_cell(&CellInputs { chroma: Some(c), ..inp(255, 180) }, &uni);
        assert_eq!(cell.glyph(), FILL_TOP);
        assert_eq!((cell.fg, cell.bg), (shade(c, 255, 218), shade(c, 180, 218)), "pixels' half-block pair");
    }

    #[test]
    fn black_floor_holds_a_lit_cell() {
        let (_, uni) = sets();
        let mut st = HysteresisState::new(1, 1);
        assert_ne!(glyph(&inp(40, 40), &uni, &mut st), ' ');
        assert_ne!(glyph(&inp(FLOOR_HOLD, FLOOR_HOLD), &uni, &mut st), ' ', "held down to FLOOR_HOLD");
        assert_eq!(glyph(&inp(FLOOR_HOLD - 1, FLOOR_HOLD - 1), &uni, &mut st), ' ');
        let f = BLACK_FLOOR - 1;
        assert_eq!(glyph(&inp(f, f), &uni, &mut st), ' ', "no re-arm under the floor");
    }

    #[test]
    fn edges_are_ascii_strokes() {
        let (_, uni) = sets();
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
