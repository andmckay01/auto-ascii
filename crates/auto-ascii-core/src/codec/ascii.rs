//! `ascii` — printable ASCII only, on the terminal's own background.
//!
//! The same drawing as [`letters`](super::letters) — a ramp ordered by
//! measured ink, a top-/bottom-heavy variant where a cell's two halves
//! disagree, directional strokes on edges — with every solid shape and every
//! background taken away. On every tier and palette selection each glyph is
//! printable ASCII `0x20..=0x7E` and each cell carries
//! [`attrs::DEFAULT_BG`], so the painter never sets a background color and the
//! terminal's own shows through (letterbox pads and composition gaps too, via
//! [`GlyphCodec::PAD`]).
//!
//! Tone therefore lives in two places only: how much of the cell the glyph
//! inks, and how bright its color is. The glyph comes from the held tone
//! through an 18-step ramp that tops out in the densest glyphs (`#`, `D`,
//! `8`, `B`, `@`, `@` from `TONE_TOP` up); the ink target runs linearly in
//! tone above mid-gray and is bent up below it, so midtones ink sooner. The
//! color is the cell's chroma sample (gray fallback), hue kept: its
//! brightness `y` starts lifted to `y·(1 + (1 − y)²)` (at most 4×) and, over
//! held tone `LIT_FROM` to `LIT_FULL`, rises to full brightness, since a
//! glyph inks at most about a quarter of its cell and has no background to
//! carry the picture; from `HI_FROM` up it runs toward white (seven eighths
//! of the way at 255), so a highlight outshines the lit surface around it.
//! Tone past mid-gray is carried by ink alone. Stability follows letters on untinted tiers:
//! the displayed tone is held within a deadband of `9/32 × idx_hyst_q8` tone
//! units, a lit cell is held down to half the black floor, and the
//! top-/bottom-heavy choice reuses letters' dual threshold. The output is the
//! same on every tier; the backend quantizes the color.
//!
//! **Ramp order.** Coverage was measured on JetBrains Mono 2.304 Regular
//! (Ghostty's bundled default) as antialiased ink over the advance ×
//! (ascent + descent) cell, rasterized with CoreText; [`ASCII_INK`] is that
//! coverage relative to `@`. The ramp is strictly increasing there; Menlo
//! swaps two near-ties (`+`/`r` and `#`/`D`, each within 0.003). The stroke
//! glyphs `| / \ - _ = X` stay out of every ramp table, as in letters.
//!
//! **Design constants.** The glyph tables, [`ASCII_INK`] and the thresholds
//! below (`BLACK_FLOOR`, `FLOOR_HOLD`, `TONE_TOP`, `GAIN_MAX_Q8`, `LIT_FROM`,
//! `LIT_FULL`, `HI_FROM`, `HI_WHITE_Q8`, the curves
//! in `value_table` and `step_table` and the deadband factor in `held_tone`)
//! are this codec's DATA, pinned by its tests and goldens; what a viewer
//! tunes stays in `ComposeParams`, exactly as for letters.

use crate::cell::{Cell, Rgb, attrs};
use crate::codec::GlyphCodec;
use crate::codec::letters::{EDGE, HALF_MASK, HALF_NONE, HALF_SHIFT, HALF_TOP, JUNCTION, half_variant};
use crate::compose::{CellInputs, ComposeParams, boost, h_flags, layer, shade};
use crate::hysteresis::{CellState, IDX_UNSET, cell_flags, edge_gate};
use crate::orient::{bin_with_guard, coherence_at_least, debias};
use crate::palette::{ASCII_HIGHLIGHT, GlyphClass, PaletteSet, subpos};

/// Base ramp, darkest first — 18 steps of printable ASCII, strictly
/// increasing in measured ink.
pub const ASCII_RAMP: &[char] = &[
    ' ', '.', ':', ';', '+', 'r', 'c', 'x', 'n', 'o', 'e', 'a', 'S', '#', 'D', '8', 'B', '@',
];

/// Ink coverage of each [`ASCII_RAMP`] step in Q8 of the densest glyph's
/// (JetBrains Mono: `@` inks 0.283 of its cell).
pub const ASCII_INK: &[u8] = &[0, 27, 50, 65, 90, 107, 133, 137, 142, 150, 158, 167, 177, 190, 196, 212, 218, 255];

/// Ink-in-the-top-half variant of each [`ASCII_RAMP`] step, used when the
/// top tap is decisively brighter.
pub const ASCII_TOP: &[char] = &[
    ' ', '\'', '\'', '\'', '"', '"', '"', '"', 'T', 'T', 'Y', 'Y', 'F', '7', '7', 'P', 'P', 'M',
];

/// Ink-in-the-bottom-half variant of each [`ASCII_RAMP`] step.
pub const ASCII_BOTTOM: &[char] = &[
    ' ', '.', '.', ',', ',', ',', 'v', 'v', 'u', 'u', 'u', 'a', 'a', 'a', 'w', 'w', 'g', 'g',
];

const BLACK_FLOOR: u8 = 24;

const FLOOR_HOLD: u8 = 12;

const TONE_TOP: u8 = 216;

const GAIN_MAX_Q8: u32 = 1024;

const LIT_FROM: u8 = 24;

const LIT_FULL: u8 = 128;

const HI_FROM: u8 = 160;

const HI_WHITE_Q8: u32 = 224;

const STEP: [u8; 256] = step_table();

const VALUE: [u8; 256] = value_table();

const fn unit(n: usize) -> u32 {
    let span = (TONE_TOP - BLACK_FLOOR) as u32;
    let x = n as u32 - BLACK_FLOOR as u32;
    (if x < span { x } else { span }) * 255 / span
}

const fn value_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut m = 0;
    while m < 256 {
        let d = 255 - m as u32;
        t[m] = (m as u32 + m as u32 * d * d / (255 * 255)) as u8;
        m += 1;
    }
    t
}

const fn step_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let lo = ASCII_INK[1] as u32;
    let mut n = BLACK_FLOOR as usize;
    while n < 256 {
        let u = unit(n);
        let mut want = if u < 128 { u + (128 - u) * u / 254 } else { u };
        if want < lo {
            want = lo;
        }
        let mut i = 1;
        while i + 1 < ASCII_INK.len() && ASCII_INK[i] as u32 + ASCII_INK[i + 1] as u32 <= 2 * want {
            i += 1;
        }
        t[n] = i as u8;
        n += 1;
    }
    t
}

#[inline]
const fn put(g: char, fg: Rgb) -> Cell {
    Cell { ch: g as u32, fg, bg: Rgb::BLACK, attrs: attrs::DEFAULT_BG }
}

#[inline]
fn tint(c: Rgb, h: u8) -> Rgb {
    let m = c.r.max(c.g).max(c.b) as u32;
    if m == 0 {
        return c;
    }
    let lift = GAIN_MAX_Q8.min(((VALUE[m as usize] as u32) << 8) / m);
    let full = (255 << 8) / m;
    let b = (h.clamp(LIT_FROM, LIT_FULL) - LIT_FROM) as u32 * 256 / (LIT_FULL - LIT_FROM) as u32;
    let g = lift + (((full.max(lift) - lift) * b) >> 8);
    let w = (h.saturating_sub(HI_FROM) as u32 * HI_WHITE_Q8) / (255 - HI_FROM) as u32;
    let s = |x: u8| {
        let v = ((x as u32 * g) >> 8).min(255);
        (v + (((255 - v) * w) >> 8)) as u8
    };
    Rgb::new(s(c.r), s(c.g), s(c.b))
}

#[inline]
fn held_tone(n: u8, prev: u8, hyst_q8: u8) -> u8 {
    let band = (hyst_q8 as u16 * 9 / 32) as u8;
    let h = if prev == IDX_UNSET || n.abs_diff(prev) > band { n } else { prev };
    h.min(IDX_UNSET - 1)
}

/// The `ascii` codec. See the module docs.
pub struct Ascii;

impl GlyphCodec for Ascii {
    const NAME: &'static str = "ascii";

    const PAD: Cell = put(' ', Rgb::WHITE);

    /// Layer priority mirrors letters: edge → deep shadow → highlight → half
    /// variant (STRUCTURE) → base ramp.
    #[inline]
    fn cell(
        inp: &CellInputs,
        lut: &[u8; 256],
        _set: &PaletteSet,
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
        let i = STEP[h as usize] as usize;
        let len = ASCII_RAMP.len() as u32;

        let was_edge = s.flags & cell_flags::WAS_EDGE != 0;
        let edge_on = edge_gate(inp.e, was_edge, params.edge_t_on, params.edge_t_off);
        if edge_on {
            s.flags |= cell_flags::WAS_EDGE;
        } else {
            s.flags &= !cell_flags::WAS_EDGE;
        }

        let fg = tint(inp.chroma.unwrap_or(Rgb::gray(n)), h);
        let dx = -debias(inp.ex);
        let dy = -debias(inp.ey);
        let plain_idx = ((n as u32 * len) >> 8).min(len - 1);
        let near_white = ((plain_idx + 1) << 8) > params.edge_white_cut_q8 as u32 * len;

        if edge_on && !near_white && coherence_at_least(dx, dy, inp.e, params.coh_min_q8) {
            let g = if coherence_at_least(dx, dy, inp.e, params.coh_dir_q8) {
                let bin = bin_with_guard(dx, dy, s.bin);
                s.bin = bin;
                let class = GlyphClass::from_bin(bin) as usize;
                EDGE[class][subpos(lt, lb, params.halfblock_min_delta) as usize]
            } else {
                JUNCTION
            };
            return (put(g, fg), layer::EDGE);
        }

        if deep_shadow {
            return (put(ASCII_RAMP[0], fg), layer::SHADOW);
        }

        let hi_cut = ((len * params.hi_cut_q8 as u32) >> 8).max(1);
        if inp.h & h_flags::HIGHLIGHT != 0 && (i as u32) < hi_cut {
            let hlen = ASCII_HIGHLIGHT.len() as u32;
            let hidx = ((i as u32 * hlen) / hi_cut).min(hlen - 1) as usize;
            return (put(ASCII_HIGHLIGHT[hidx], boost(fg)), layer::HIGHLIGHT);
        }

        if half != HALF_NONE && i > 0 {
            let g = if half == HALF_TOP { ASCII_TOP[i] } else { ASCII_BOTTOM[i] };
            let lit = lt.max(lb);
            return (put(g, tint(inp.chroma.map_or(Rgb::gray(lit), |c| shade(c, lit, n.max(1))), h)), layer::STRUCTURE);
        }

        (put(ASCII_RAMP[i], fg), layer::BASE)
    }
}

/// Every glyph `ascii` can emit, on any tier — the repertoire the tests pin.
pub fn ascii_glyphs() -> Vec<char> {
    let mut out: Vec<char> = ASCII_RAMP
        .iter()
        .chain(ASCII_TOP)
        .chain(ASCII_BOTTOM)
        .chain(EDGE.iter().flatten())
        .chain(ASCII_HIGHLIGHT)
        .copied()
        .chain([JUNCTION])
        .collect();
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

    fn uni() -> PaletteSet {
        select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100)
    }

    fn step(i: &CellInputs, set: &PaletteSet, p: &ComposeParams, st: &mut HysteresisState) -> Cell {
        Ascii::cell(i, &ident(), set, p, st.cell_mut(0, 0)).0
    }

    fn glyph(i: &CellInputs, st: &mut HysteresisState) -> char {
        step(i, &uni(), &ComposeParams::default(), st).glyph()
    }

    fn cold(i: &CellInputs) -> Cell {
        step(i, &uni(), &ComposeParams::default(), &mut HysteresisState::new(1, 1))
    }

    #[test]
    fn tables_are_printable_ascii_and_ordered_by_ink() {
        for t in [ASCII_TOP, ASCII_BOTTOM] {
            assert_eq!(t.len(), ASCII_RAMP.len());
        }
        assert_eq!(ASCII_INK.len(), ASCII_RAMP.len());
        assert!(ASCII_INK.windows(2).all(|w| w[0] < w[1]), "ink strictly increasing");
        assert_eq!((ASCII_INK[0], ASCII_INK[ASCII_INK.len() - 1]), (0, 255));
        let mut r = ASCII_RAMP.to_vec();
        r.sort_unstable();
        r.dedup();
        assert_eq!(r.len(), ASCII_RAMP.len(), "every base step is a distinct glyph");
        for g in EDGE.iter().flatten().chain([&JUNCTION]) {
            for table in [ASCII_RAMP, ASCII_TOP, ASCII_BOTTOM] {
                assert!(!table.contains(g), "{g:?} is an edge stroke");
            }
        }
        for g in ascii_glyphs() {
            assert!((' '..='~').contains(&g), "{g:?}");
        }
    }

    #[test]
    fn ramp_is_monotonic_and_tops_out_dense() {
        let mut last = 0;
        for n in 0..=255u8 {
            let g = cold(&inp(n, n)).glyph();
            let p = ASCII_RAMP.iter().position(|&r| r == g).unwrap_or_else(|| panic!("{n} -> {g:?}"));
            assert!(p >= last, "tone {n}: {g:?} darker than the step before");
            last = p;
        }
        assert_eq!(cold(&inp(0, 0)).glyph(), ' ');
        assert_eq!(cold(&inp(BLACK_FLOOR - 1, BLACK_FLOOR - 1)).glyph(), ' ');
        assert_ne!(cold(&inp(BLACK_FLOOR, BLACK_FLOOR)).glyph(), ' ');
        assert_eq!(cold(&inp(255, 255)).glyph(), '@');
        assert_eq!(cold(&inp(TONE_TOP, TONE_TOP)).glyph(), '@');
        let mid = cold(&inp(128, 128)).glyph();
        assert!("rcxno".contains(mid), "mid-gray is a lowercase midtone: {mid:?}");
    }

    #[test]
    fn every_cell_keeps_the_terminal_background() {
        let skin = Rgb::new(200, 150, 120);
        for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
            for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
                let set = select_palettes(tier, color, 100);
                let mut st = HysteresisState::new(1, 1);
                for (t, b, e, h) in [(0, 0, 0, 0), (120, 120, 0, 0), (250, 60, 0, 0), (60, 250, 0, 0),
                    (100, 100, 200, 0), (40, 40, 0, 1), (200, 200, 0, 2), (255, 255, 0, 0)] {
                    let i = CellInputs { e, ex: 40, ey: 128, h, chroma: Some(skin), ..inp(t, b) };
                    let c = step(&i, &set, &ComposeParams::default(), &mut st);
                    assert_eq!((c.bg, c.attrs), (Rgb::BLACK, attrs::DEFAULT_BG), "{tier:?} {color:?}");
                    assert!((' '..='~').contains(&c.glyph()), "{:?}", c.glyph());
                }
            }
        }
        assert_eq!(Ascii::PAD.attrs, attrs::DEFAULT_BG);
        assert_eq!(Ascii::PAD.glyph(), ' ');
    }

    #[test]
    fn color_keeps_hue_lifts_darks_and_orders_brightness() {
        let dim = cold(&CellInputs { chroma: Some(Rgb::new(40, 20, 10)), ..inp(LIT_FROM, LIT_FROM) }).fg;
        assert!(dim.r > 40 && dim.r <= 160, "dark colors lift, at most 4x: {dim:?}");
        assert_eq!((dim.r / 2, dim.r / 4), (dim.g, dim.b), "hue kept: {dim:?}");
        let white = cold(&CellInputs { chroma: Some(Rgb::WHITE), ..inp(250, 250) }).fg;
        assert_eq!(white, Rgb::WHITE, "nothing clips");
        let mid = (LIT_FROM + LIT_FULL) / 2;
        let mut last = 0;
        for m in (8..=255u16).step_by(8) {
            let c = cold(&CellInputs { chroma: Some(Rgb::gray(m as u8)), ..inp(mid, mid) }).fg;
            assert!(c.r >= last, "value {m}: {} below {last}", c.r);
            last = c.r;
        }
        let lit = cold(&CellInputs { chroma: Some(Rgb::gray(120)), ..inp(mid, mid) }).fg.r;
        assert!(lit > 120 && 255 - lit >= 20, "a half-lit surface lifts but stays below white: {lit}");
        let skin = Rgb::new(160, 120, 80);
        let at = |n: u8| cold(&CellInputs { chroma: Some(skin), ..inp(n, n) }).fg;
        let mut last = 0;
        for n in (LIT_FROM..=LIT_FULL).step_by(8) {
            assert!(at(n).r > last, "tone {n} brightens the color: {:?}", at(n));
            last = at(n).r;
        }
        let full = at(LIT_FULL);
        assert!(full.r == 255 && full.r > full.g && full.g > full.b, "lit cells reach full brightness: {full:?}");
        assert_eq!(at(HI_FROM), full, "full brightness, hue kept, up to HI_FROM");
        let core = at(255);
        assert!(core.r == 255 && core.b > 200 && core.b > full.b, "a highlight runs toward white: {core:?}");
    }

    #[test]
    fn tone_deadband_and_floor_hold() {
        let mut st = HysteresisState::new(1, 1);
        let start = glyph(&inp(120, 120), &mut st);
        for wobble in [100u8, 140, 110, 150, 125] {
            assert_eq!(glyph(&inp(wobble, wobble), &mut st), start, "±30 around 120 is noise");
        }
        assert_ne!(glyph(&inp(200, 200), &mut st), start, "a real change moves the glyph");
        let mut st = HysteresisState::new(1, 1);
        assert_ne!(glyph(&inp(40, 40), &mut st), ' ');
        assert_ne!(glyph(&inp(FLOOR_HOLD, FLOOR_HOLD), &mut st), ' ', "held down to FLOOR_HOLD");
        assert_eq!(glyph(&inp(FLOOR_HOLD - 1, FLOOR_HOLD - 1), &mut st), ' ');
        assert_eq!(glyph(&inp(BLACK_FLOOR - 1, BLACK_FLOOR - 1), &mut st), ' ', "no re-arm");
        let off = ComposeParams { idx_hyst_q8: 0, ..ComposeParams::default() };
        let mut st = HysteresisState::new(1, 1);
        step(&inp(120, 120), &uni(), &off, &mut st);
        assert_eq!(step(&inp(150, 150), &uni(), &off, &mut st).glyph(), cold(&inp(150, 150)).glyph());
    }

    #[test]
    fn halves_and_edges_are_ascii_shapes() {
        let g = cold(&inp(220, 30)).glyph();
        assert!(ASCII_TOP[10..].contains(&g), "lit top reads dense: {g:?}");
        let g = cold(&inp(30, 220)).glyph();
        assert!(ASCII_BOTTOM[10..].contains(&g), "lit bottom reads dense: {g:?}");
        let expect = ['-', '\\', '\\', '|', '|', '/', '/', '-'];
        for (k, want) in expect.iter().enumerate() {
            let a = (2.0 * (11.25 + k as f64 * 22.5)).to_radians();
            let ex = (128.0 - 100.0 * a.cos()).round() as u8;
            let ey = (128.0 - 100.0 * a.sin()).round() as u8;
            let i = CellInputs { e: 200, ex, ey, ..inp(100, 100) };
            assert_eq!(cold(&i).glyph(), *want, "bin {k}");
        }
    }

    #[test]
    fn same_output_on_every_tier() {
        for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
            for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
                for cols in [40, 100] {
                    let set = select_palettes(tier, color, cols);
                    let (mut a, mut b) = (HysteresisState::new(1, 1), HysteresisState::new(1, 1));
                    for n in [31, 34, 120, 100, 140, 110, 150, 200, 40, 0, 255] {
                        let i = CellInputs { chroma: Some(Rgb::new(90, 120, 60)), ..inp(n, n / 2) };
                        let p = ComposeParams::default();
                        assert_eq!(step(&i, &set, &p, &mut a), step(&i, &uni(), &p, &mut b), "{tier:?} {color:?}");
                    }
                }
            }
        }
    }
}
