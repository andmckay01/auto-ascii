//! `ascii` — printable ASCII only, over a capped shade.
//!
//! The same drawing as [`letters`](super::letters) — a ramp ordered by
//! measured ink, a top-/bottom-heavy variant where a cell's two halves
//! disagree, directional strokes on edges — with every solid shape taken
//! away. On every tier and palette selection each glyph is printable ASCII
//! `0x20..=0x7E`. Where letters' tint would light the cell, `ascii` paints a
//! background **shade** instead, and only ever a shade: on truecolor and
//! 256-color a cell's background starts as its chroma sample (a gray of the
//! held tone when there is none) times letters' coverage curve of the held
//! tone times 0.6; over held tone `PALE_FROM` to `PALE_FULL` a pale color's
//! shade runs toward neutral at its own brightest channel, as far as the
//! color is pale (its least channel over its greatest), hue kept. It is then
//! held to [`backing_within_cap`] — no channel above [`SHADE_CEIL`] (a glyph
//! cell) or [`SHADE_BLANK_CEIL`] (a space), and at most
//! [`SHADE_CONTRAST_Q8`]/256 of the glyph's [`luminance`]. A shade that
//! breaks the cap is scaled down, hue kept; one whose brightest channel ends
//! below [`SHADE_FLOOR`] would read as black and is not sent. Truecolor sends
//! that shade. 256-color sends the nearest of [`SHADES_256`] (OKLab distance,
//! the chroma plane counted double so hue weighs more than lightness) among
//! those within the cap against the glyph's color as quantized and
//! [`in_hue_family`] with it: a gray, or one of the six cube colors at level
//! 95 within 30° of the glyph's hue. The cube has no darker chromatic entry,
//! so dim shades land on grays. 16-color and mono paint no shade. Cells
//! without one carry [`attrs::DEFAULT_BG`], so the terminal's own background
//! shows through (letterbox pads and composition gaps too, via
//! [`GlyphCodec::PAD`]). [`cell_within_cap`] states the whole contract per
//! color depth.
//!
//! The glyph comes from the held tone through an 18-step ramp that tops out
//! in the densest glyphs (`#`, `D`, `8`, `B`, `@`); the ink target runs
//! linearly in tone from the black floor to `TONE_TOP`, bent up below
//! mid-gray so midtones ink sooner, and each tone takes the step of nearest
//! ink, so `@` starts at held tone 225, a little below `TONE_TOP`, and stays
//! for near-white. The glyph's color is the chroma sample, hue kept: over
//! held tone `LIT_FROM` to `LIT_FULL` its brightness rises from the sample's
//! own to full, since a glyph inks at most about a quarter of its cell; from
//! `HI_FROM` up it runs toward white (all the way at 255), so a highlight
//! outshines the lit surface around it. Stability follows letters on
//! untinted tiers: the displayed tone is held within a deadband of
//! `9/32 × idx_hyst_q8` tone units, a lit cell is held down to half the black
//! floor, the shade follows the held tone rather than the instantaneous one,
//! and the top-/bottom-heavy choice reuses letters' dual threshold. Glyph and
//! glyph color are the same on every tier; the backend quantizes the color.
//!
//! **Ramp order.** Coverage was measured on JetBrains Mono 2.304 Regular
//! (Ghostty's bundled default) as antialiased ink over the advance ×
//! (ascent + descent) cell, rasterized with CoreText; [`ASCII_INK`] is that
//! coverage relative to `@`. The ramp is strictly increasing there; Menlo
//! swaps two near-ties (`+`/`r` and `#`/`D`, each within 0.003). The stroke
//! glyphs `| / \ - _ = X` stay out of every ramp table, as in letters.
//!
//! **Design constants.** The glyph tables, [`ASCII_INK`] and the thresholds
//! below (`BLACK_FLOOR`, `FLOOR_HOLD`, `TONE_TOP`, `LIT_FROM`, `LIT_FULL`,
//! `HI_FROM`, `PALE_FROM`, `PALE_FULL`, `SHADE_Q8`, the shade cap and floor,
//! the 256-color entries and hue test, the curve in `step_table` and the
//! deadband factor in `held_tone`)
//! are this codec's DATA, pinned by its tests and goldens; what a viewer
//! tunes stays in `ComposeParams`, exactly as for letters.

use crate::cell::{Cell, Rgb, attrs};
use crate::codec::GlyphCodec;
use crate::codec::letters::{EDGE, HALF_MASK, HALF_NONE, HALF_SHIFT, HALF_TOP, JUNCTION, half_variant};
use crate::compose::{CellInputs, ComposeParams, boost, h_flags, layer, shade};
use crate::hysteresis::{CellState, IDX_UNSET, cell_flags, edge_gate};
use crate::orient::{bin_with_guard, coherence_at_least, debias};
use crate::quant::{ansi256_to_rgb, rgb_to_256};
use crate::palette::{ASCII_HIGHLIGHT, ColorDepth, GlyphClass, PaletteSet, subpos};

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

const TONE_TOP: u8 = 240;

const LIT_FROM: u8 = 48;

const LIT_FULL: u8 = 176;

const HI_FROM: u8 = 160;

const PALE_FROM: u8 = 128;

const PALE_FULL: u8 = 192;

/// Brightest channel a glyph cell's backing shade may have: 96 of 255,
/// 38% of full.
pub const SHADE_CEIL: u8 = 96;

/// Brightest channel the backing of a space may have: the black floor, so
/// no blank cell is brighter than the dimmest tone `ascii` draws a glyph for.
pub const SHADE_BLANK_CEIL: u8 = BLACK_FLOOR;

/// A glyph cell's backing may have at most this fraction (Q8, 96/256 =
/// 0.375) of its glyph's relative luminance.
pub const SHADE_CONTRAST_Q8: u32 = 96;

/// A backing whose brightest channel is below this is not sent: it reads as
/// black on any display, and 8 is the darkest xterm gray.
pub const SHADE_FLOOR: u8 = 8;

/// A chromatic 256-color backing is within 30° of OKLab hue of the glyph's
/// color as quantized: cos² of that angle.
pub const SHADE_HUE_COS2: f32 = 0.75;

/// Least OKLab chroma a quantized glyph color needs to have a hue family.
pub const SHADE_HUE_MIN_CHROMA: f32 = 0.03;

const SHADE_Q8: u32 = 154;

/// The xterm-256 entries a backing can land on under [`SHADE_CEIL`]: the
/// gray ramp through 88, the cube's gray at 95, and the six cube colors
/// whose levels are all 0 or 95. Each is its own quantization.
pub const SHADES_256: [Rgb; 16] = [
    Rgb::gray(8),
    Rgb::gray(18),
    Rgb::gray(28),
    Rgb::gray(38),
    Rgb::gray(48),
    Rgb::gray(58),
    Rgb::gray(68),
    Rgb::gray(78),
    Rgb::gray(88),
    Rgb::gray(95),
    Rgb::new(95, 0, 0),
    Rgb::new(0, 95, 0),
    Rgb::new(0, 0, 95),
    Rgb::new(95, 95, 0),
    Rgb::new(95, 0, 95),
    Rgb::new(0, 95, 95),
];

const SHADES_256_LAB: [Lab; 16] = shades_lab();

const LINEAR: [u16; 256] = [
    0, 20, 40, 60, 80, 99, 119, 139, 159, 179, 199, 219, 241, 264, 288, 313,
    340, 367, 396, 427, 458, 491, 526, 562, 599, 637, 677, 718, 761, 805, 851, 898,
    947, 997, 1048, 1101, 1156, 1212, 1270, 1330, 1391, 1453, 1517, 1583, 1651, 1720, 1790, 1863,
    1937, 2013, 2090, 2170, 2250, 2333, 2418, 2504, 2592, 2681, 2773, 2866, 2961, 3058, 3157, 3258,
    3360, 3464, 3570, 3678, 3788, 3900, 4014, 4129, 4247, 4366, 4488, 4611, 4736, 4864, 4993, 5124,
    5257, 5392, 5530, 5669, 5810, 5953, 6099, 6246, 6395, 6547, 6700, 6856, 7014, 7174, 7335, 7500,
    7666, 7834, 8004, 8177, 8352, 8528, 8708, 8889, 9072, 9258, 9445, 9635, 9828, 10022, 10219, 10417,
    10619, 10822, 11028, 11235, 11446, 11658, 11873, 12090, 12309, 12530, 12754, 12980, 13209, 13440, 13673, 13909,
    14146, 14387, 14629, 14874, 15122, 15371, 15623, 15878, 16135, 16394, 16656, 16920, 17187, 17456, 17727, 18001,
    18277, 18556, 18837, 19121, 19407, 19696, 19987, 20281, 20577, 20876, 21177, 21481, 21787, 22096, 22407, 22721,
    23038, 23357, 23678, 24002, 24329, 24658, 24990, 25325, 25662, 26001, 26344, 26688, 27036, 27386, 27739, 28094,
    28452, 28813, 29176, 29542, 29911, 30282, 30656, 31033, 31412, 31794, 32179, 32567, 32957, 33350, 33745, 34143,
    34544, 34948, 35355, 35764, 36176, 36591, 37008, 37429, 37852, 38278, 38706, 39138, 39572, 40009, 40449, 40891,
    41337, 41785, 42236, 42690, 43147, 43606, 44069, 44534, 45002, 45473, 45947, 46423, 46903, 47385, 47871, 48359,
    48850, 49344, 49841, 50341, 50844, 51349, 51858, 52369, 52884, 53401, 53921, 54445, 54971, 55500, 56032, 56567,
    57105, 57646, 58190, 58737, 59287, 59840, 60396, 60955, 61517, 62082, 62650, 63221, 63795, 64372, 64952, 65535,
];

const STEP: [u8; 256] = step_table();

const fn unit(n: usize) -> u32 {
    let span = (TONE_TOP - BLACK_FLOOR) as u32;
    let x = n as u32 - BLACK_FLOOR as u32;
    (if x < span { x } else { span }) * 255 / span
}

#[derive(Clone, Copy)]
struct Lab {
    l: f32,
    a: f32,
    b: f32,
}

const fn cbrt(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut y = f32::from_bits(x.to_bits() / 3 + 0x2a51_4067);
    let mut i = 0;
    while i < 4 {
        y = (2.0 * y + x / (y * y)) / 3.0;
        i += 1;
    }
    y
}

const fn oklab(c: Rgb) -> Lab {
    let r = LINEAR[c.r as usize] as f32 / 65535.0;
    let g = LINEAR[c.g as usize] as f32 / 65535.0;
    let b = LINEAR[c.b as usize] as f32 / 65535.0;
    let l = cbrt(0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b);
    let m = cbrt(0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b);
    let s = cbrt(0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b);
    Lab {
        l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

const fn shades_lab() -> [Lab; 16] {
    let mut t = [Lab { l: 0.0, a: 0.0, b: 0.0 }; 16];
    let mut i = 0;
    while i < 16 {
        t[i] = oklab(SHADES_256[i]);
        i += 1;
    }
    t
}

#[inline]
fn neutral(c: Rgb) -> bool {
    c.r == c.g && c.g == c.b
}

#[inline]
fn hue_near(glyph: Lab, shade: Lab) -> bool {
    let c2 = |x: Lab| x.a * x.a + x.b * x.b;
    let dot = glyph.a * shade.a + glyph.b * shade.b;
    c2(glyph) >= SHADE_HUE_MIN_CHROMA * SHADE_HUE_MIN_CHROMA
        && dot > 0.0
        && dot * dot >= SHADE_HUE_COS2 * c2(glyph) * c2(shade)
}

/// Whether `shade` sits in the hue family of the glyph color `glyph`: a
/// neutral gray always does; a chromatic color does when `glyph` has at
/// least [`SHADE_HUE_MIN_CHROMA`] and the two OKLab hues are within 30°
/// ([`SHADE_HUE_COS2`]).
pub fn in_hue_family(glyph: Rgb, shade: Rgb) -> bool {
    neutral(shade) || hue_near(oklab(glyph), oklab(shade))
}

#[inline]
fn shade_distance(want: Lab, e: Lab) -> f32 {
    let (dl, da, db) = (want.l - e.l, want.a - e.a, want.b - e.b);
    dl * dl + 2.0 * (da * da + db * db)
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
const fn put(g: char, fg: Rgb, bg: Option<Rgb>) -> Cell {
    match bg {
        Some(bg) => Cell { ch: g as u32, fg, bg, attrs: 0 },
        None => Cell { ch: g as u32, fg, bg: Rgb::BLACK, attrs: attrs::DEFAULT_BG },
    }
}

/// Relative luminance of an sRGB color (decoded to linear light, Rec. 709
/// weights), in Q16: 0 for black, 65535 for white.
#[inline]
pub fn luminance(c: Rgb) -> u32 {
    let l = |v: u8| LINEAR[v as usize] as u32;
    (13933 * l(c.r) + 46871 * l(c.g) + 4732 * l(c.b)) >> 16
}

/// The cap every `ascii` backing shade obeys, checked on the colors the
/// terminal is sent (after the backend's quantization): a space's backing
/// has no channel above [`SHADE_BLANK_CEIL`]; a glyph's has no channel above
/// [`SHADE_CEIL`] and at most [`SHADE_CONTRAST_Q8`]/256 of the glyph's
/// [`luminance`]. A cell on the terminal's own background passes trivially.
pub fn backing_within_cap(glyph: char, fg: Rgb, bg: Rgb) -> bool {
    let top = bg.r.max(bg.g).max(bg.b);
    if glyph == ' ' {
        return top <= SHADE_BLANK_CEIL;
    }
    top <= SHADE_CEIL && (luminance(bg) << 8) <= SHADE_CONTRAST_Q8 * luminance(fg)
}

/// The whole background contract of an `ascii` cell rendered for `color`,
/// on the colors the codec emits: on 16-color and mono the terminal's own
/// background; on truecolor the terminal's own or a backing within
/// [`backing_within_cap`] whose brightest channel reaches [`SHADE_FLOOR`];
/// on 256-color the terminal's own or one of [`SHADES_256`] (quantized
/// unchanged) that is [`in_hue_family`] of, and within the cap against, the
/// glyph's color as quantized.
pub fn cell_within_cap(cell: &Cell, color: ColorDepth) -> bool {
    if cell.attrs & attrs::DEFAULT_BG != 0 {
        return true;
    }
    match color {
        ColorDepth::True => {
            cell.bg.r.max(cell.bg.g).max(cell.bg.b) >= SHADE_FLOOR
                && backing_within_cap(cell.glyph(), cell.fg, cell.bg)
        }
        ColorDepth::C256 => {
            let seen = ansi256_to_rgb(rgb_to_256(cell.fg));
            SHADES_256.contains(&cell.bg)
                && in_hue_family(seen, cell.bg)
                && backing_within_cap(cell.glyph(), seen, cell.bg)
        }
        ColorDepth::C16 | ColorDepth::Mono => false,
    }
}

#[inline]
fn scaled(c: Rgb, k: u32) -> Rgb {
    let s = |v: u8| ((v as u32 * k) >> 16) as u8;
    Rgb::new(s(c.r), s(c.g), s(c.b))
}

#[inline]
fn backing(c: Rgb, n: u8, glyph: char, fg: Rgb, color: ColorDepth) -> Option<Rgb> {
    let seen = match color {
        ColorDepth::True => fg,
        ColorDepth::C256 => ansi256_to_rgb(rgb_to_256(fg)),
        ColorDepth::C16 | ColorDepth::Mono => return None,
    };
    let want = ideal_backing(c, n, glyph, seen)?;
    if color == ColorDepth::True {
        return Some(want);
    }
    let (w, g) = (oklab(want), oklab(seen));
    SHADES_256
        .iter()
        .zip(&SHADES_256_LAB)
        .filter(|&(&e, &lab)| backing_within_cap(glyph, seen, e) && (neutral(e) || hue_near(g, lab)))
        .min_by(|x, y| shade_distance(w, *x.1).total_cmp(&shade_distance(w, *y.1)))
        .map(|(&e, _)| e)
}

#[inline]
fn ideal_backing(c: Rgb, n: u8, glyph: char, seen: Rgb) -> Option<Rgb> {
    let top = c.r.max(c.g).max(c.b) as u32;
    let f = BLACK_FLOOR as u32;
    let x = ((n as u32).saturating_sub(f) << 8) / (255 - f);
    let cov = (x + ((x * x * (768 - 2 * x)) >> 16)) >> 1;
    let mut k = cov * SHADE_Q8;
    if top == 0 || k == 0 {
        return None;
    }
    let blank = glyph == ' ';
    let ceil = if blank { SHADE_BLANK_CEIL } else { SHADE_CEIL } as u32;
    if (top * k) >> 16 > ceil {
        k = (ceil << 16).div_ceil(top);
    }
    let limit = if blank { u32::MAX } else { (SHADE_CONTRAST_Q8 * luminance(seen)) >> 8 };
    let c = pale_toward_neutral(c, n);
    let bg = if luminance(scaled(c, k)) <= limit {
        scaled(c, k)
    } else {
        let (mut lo, mut hi) = (0, k);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if luminance(scaled(c, mid)) <= limit { lo = mid } else { hi = mid }
        }
        scaled(c, lo)
    };
    (bg.r.max(bg.g).max(bg.b) >= SHADE_FLOOR).then_some(bg)
}

#[inline]
fn pale_toward_neutral(c: Rgb, n: u8) -> Rgb {
    let m = c.r.max(c.g).max(c.b) as u32;
    let lo = c.r.min(c.g).min(c.b) as u32;
    let ramp = (n.clamp(PALE_FROM, PALE_FULL) - PALE_FROM) as u32 * 256 / (PALE_FULL - PALE_FROM) as u32;
    let w = ((lo << 8) / m.max(1) * ramp) >> 8;
    let s = |v: u8| (v as u32 + (((m - v as u32) * w) >> 8)) as u8;
    Rgb::new(s(c.r), s(c.g), s(c.b))
}

#[inline]
fn tint(c: Rgb, h: u8) -> Rgb {
    let m = c.r.max(c.g).max(c.b) as u32;
    if m == 0 {
        return c;
    }
    let full = (255 << 8) / m;
    let b = (h.clamp(LIT_FROM, LIT_FULL) - LIT_FROM) as u32 * 256 / (LIT_FULL - LIT_FROM) as u32;
    let g = 256 + (((full - 256) * b) >> 8);
    let w = (h.saturating_sub(HI_FROM) as u32 * 256) / (255 - HI_FROM) as u32;
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

    const PAD: Cell = put(' ', Rgb::WHITE, None);

    /// Layer priority mirrors letters: edge → deep shadow → highlight → half
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
        let i = STEP[h as usize] as usize;
        let len = ASCII_RAMP.len() as u32;

        let was_edge = s.flags & cell_flags::WAS_EDGE != 0;
        let edge_on = edge_gate(inp.e, was_edge, params.edge_t_on, params.edge_t_off);
        if edge_on {
            s.flags |= cell_flags::WAS_EDGE;
        } else {
            s.flags &= !cell_flags::WAS_EDGE;
        }

        let c = inp.chroma.unwrap_or(Rgb::gray(n));
        let fg = tint(c, h);
        let base = inp.chroma.unwrap_or(Rgb::gray(h));
        let cell = |g: char, fg: Rgb, tone: u8| put(g, fg, backing(base, tone, g, fg, set.color));
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
            return (cell(g, fg, h), layer::EDGE);
        }

        if deep_shadow {
            return (put(ASCII_RAMP[0], fg, None), layer::SHADOW);
        }

        let hi_cut = ((len * params.hi_cut_q8 as u32) >> 8).max(1);
        if inp.h & h_flags::HIGHLIGHT != 0 && (i as u32) < hi_cut {
            let hlen = ASCII_HIGHLIGHT.len() as u32;
            let hidx = ((i as u32 * hlen) / hi_cut).min(hlen - 1) as usize;
            return (cell(ASCII_HIGHLIGHT[hidx], boost(fg), h), layer::HIGHLIGHT);
        }

        if half != HALF_NONE && i > 0 {
            let g = if half == HALF_TOP { ASCII_TOP[i] } else { ASCII_BOTTOM[i] };
            let lit = lt.max(lb);
            let fg = tint(inp.chroma.map_or(Rgb::gray(lit), |c| shade(c, lit, n.max(1))), h);
            return (cell(g, fg, lt.min(lb)), layer::STRUCTURE);
        }

        (cell(ASCII_RAMP[i], fg, h), layer::BASE)
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
        assert_eq!(cold(&inp(225, 225)).glyph(), '@', "@ starts at held tone 225");
        assert_eq!(cold(&inp(224, 224)).glyph(), 'B', "and not a step sooner");
        let mid = cold(&inp(128, 128)).glyph();
        assert!("rcxno".contains(mid), "mid-gray is a lowercase midtone: {mid:?}");
    }

    #[test]
    fn every_backing_is_a_capped_shade_of_the_glyph_color() {
        let skin = Rgb::new(200, 150, 120);
        for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
            for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
                let set = select_palettes(tier, color, 100);
                let mut st = HysteresisState::new(1, 1);
                let mut shaded = 0;
                for chroma in [Some(skin), Some(Rgb::WHITE), Some(Rgb::new(255, 40, 0)), Some(Rgb::new(20, 60, 255)), None] {
                    for (t, b, e, h) in [(0, 0, 0, 0), (120, 120, 0, 0), (250, 60, 0, 0), (60, 250, 0, 0),
                        (100, 100, 200, 0), (40, 40, 0, 1), (200, 200, 0, 2), (30, 30, 0, 2), (255, 255, 0, 0)] {
                        let i = CellInputs { e, ex: 40, ey: 128, h, chroma, ..inp(t, b) };
                        let c = step(&i, &set, &ComposeParams::default(), &mut st);
                        assert!(cell_within_cap(&c, color), "{tier:?} {color:?} {i:?}: {c:?}");
                        assert!((' '..='~').contains(&c.glyph()), "{:?}", c.glyph());
                        if c.attrs & attrs::DEFAULT_BG == 0 {
                            shaded += 1;
                            if color == ColorDepth::True
                                && let Some(k) = chroma
                            {
                                let m = k.r.max(k.g).max(k.b) as u32;
                                let s = c.bg.r.max(c.bg.g).max(c.bg.b) as u32;
                                let (v, w) = ([k.r, k.g, k.b], [c.bg.r, c.bg.g, c.bg.b]);
                                for i in 0..3 {
                                    assert!(w[i] as u32 + 1 >= v[i] as u32 * s / m, "{c:?}: between {k:?}'s hue and neutral");
                                    for j in 0..3 {
                                        assert!(v[i] < v[j] || w[i] >= w[j], "{c:?}: same hue as {k:?}");
                                    }
                                }
                            }
                            if color == ColorDepth::C256 {
                                assert!(SHADES_256.contains(&c.bg), "{c:?}");
                            }
                        }
                    }
                }
                let tinted = matches!(color, ColorDepth::True | ColorDepth::C256);
                assert_eq!(shaded > 0, tinted, "{tier:?} {color:?}: shade only where the tier carries it");
            }
        }
        assert_eq!(Ascii::PAD.attrs, attrs::DEFAULT_BG);
        assert_eq!(Ascii::PAD.glyph(), ' ');
    }

    #[test]
    fn the_cap_holds_where_it_binds() {
        let white = Rgb::WHITE;
        let lit = backing(white, 255, '@', white, ColorDepth::True).unwrap();
        assert_eq!(lit, Rgb::gray(SHADE_CEIL), "the brightest shade is the ceiling, never a pixel");
        let blank = backing(white, 255, ' ', white, ColorDepth::True).unwrap();
        assert_eq!(blank, Rgb::gray(SHADE_BLANK_CEIL), "a space gets at most the blank ceiling");
        let dim = Rgb::gray(70);
        let under = backing(white, 255, 'x', dim, ColorDepth::True).unwrap();
        assert!(backing_within_cap('x', dim, under), "{under:?}");
        assert!(!backing_within_cap('x', dim, Rgb::gray(under.r + 2)), "the largest shade under the cap");
        let q = backing(white, 255, 'x', dim, ColorDepth::C256).unwrap();
        assert!(cell_within_cap(&put('x', dim, Some(q)), ColorDepth::C256), "{q:?}");
        assert_eq!(backing(white, 255, 'x', Rgb::BLACK, ColorDepth::True), None, "a black glyph: no shade");
        let faint = Rgb::new(40, 20, 10);
        assert_eq!(backing(faint, 40, 'x', white, ColorDepth::True), None, "a shade below the floor is not sent");
        let lit = backing(faint, 255, 'x', white, ColorDepth::True).unwrap();
        assert!(lit.r >= SHADE_FLOOR, "{lit:?}");
        for color in [ColorDepth::C16, ColorDepth::Mono] {
            assert_eq!(backing(white, 255, '@', white, color), None);
        }
        assert_eq!((SHADE_CEIL, SHADE_BLANK_CEIL, SHADE_CONTRAST_Q8, SHADE_FLOOR), (96, 24, 96, 8));
        assert_eq!((luminance(Rgb::BLACK), luminance(Rgb::WHITE)), (0, 65535));
    }

    #[test]
    fn color_keeps_hue_and_orders_brightness() {
        let dim = cold(&CellInputs { chroma: Some(Rgb::new(40, 20, 10)), ..inp(LIT_FROM, LIT_FROM) }).fg;
        assert_eq!(dim, Rgb::new(40, 20, 10), "below LIT_FROM a glyph takes the sample's own color");
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
        let hi = at(HI_FROM);
        let (r, g, b) = (hi.r as u32, hi.g as u32, hi.b as u32);
        assert!((r * 3 / 4).abs_diff(g) <= 1 && (r / 2).abs_diff(b) <= 1, "no white yet at HI_FROM: {hi:?}");
        let core = at(255);
        assert!(core.r == 255 && core.b >= 253, "a highlight runs all the way to white: {core:?}");
        assert!(at(224).b > full.b && at(224).b < 255, "and gets there gradually: {:?}", at(224));
    }

    #[test]
    fn a_pale_shade_runs_toward_neutral_a_saturated_one_keeps_its_color() {
        let bg = |c: Rgb, n: u8| backing(c, n, '@', Rgb::WHITE, ColorDepth::True).unwrap();
        let pale = Rgb::new(255, 212, 160);
        let (low, high) = (bg(pale, PALE_FROM), bg(pale, 255));
        assert_eq!(high.r, SHADE_CEIL, "{high:?}");
        assert!(high.r - high.b < low.r - low.b && high.b > (160 * SHADE_CEIL as u32 / 255) as u8, "{low:?} -> {high:?}");
        assert!(high.r >= high.g && high.g >= high.b, "hue kept: {high:?}");
        let red = Rgb::new(255, 40, 0);
        let (r, s) = (bg(red, 255), bg(Rgb::new(255, 0, 0), 255));
        assert_eq!((s.g, s.b), (0, 0), "a pure color stays pure: {s:?}");
        assert!(r.g <= 16 && r.b == 0, "a saturated color barely moves: {r:?}");
    }

    #[test]
    fn shades_256_quantize_to_themselves_and_lab_is_sane() {
        for e in SHADES_256 {
            assert_eq!(ansi256_to_rgb(rgb_to_256(e)), e);
            assert!(e.r.max(e.g).max(e.b) <= SHADE_CEIL);
        }
        let (k, w) = (oklab(Rgb::BLACK), oklab(Rgb::WHITE));
        assert!(k.l.abs() < 1e-4 && (w.l - 1.0).abs() < 1e-3 && w.a.abs() < 1e-3 && w.b.abs() < 1e-3);
        let red = oklab(Rgb::new(255, 0, 0));
        assert!((red.l - 0.628).abs() < 2e-3 && (red.a - 0.225).abs() < 2e-3 && (red.b - 0.126).abs() < 2e-3);
        assert!((cbrt(0.001) - 0.1).abs() < 1e-6 && (cbrt(0.5) - 0.793_700_5).abs() < 1e-6);
    }

    #[test]
    fn a_256_color_shade_is_a_gray_or_in_the_glyphs_hue_family() {
        let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::C256, 100);
        let mut chromatic = 0;
        for r in (0..=255u16).step_by(51) {
            for g in (0..=255u16).step_by(51) {
                for b in (0..=255u16).step_by(51) {
                    let chroma = Rgb::new(r as u8, g as u8, b as u8);
                    for n in [40u8, 90, 130, 160, 200, 255] {
                        let i = CellInputs { chroma: Some(chroma), ..inp(n, n) };
                        let c = step(&i, &set, &ComposeParams::default(), &mut HysteresisState::new(1, 1));
                        assert!(cell_within_cap(&c, ColorDepth::C256), "{chroma:?} {n}: {c:?}");
                        if c.attrs & attrs::DEFAULT_BG != 0 || neutral(c.bg) {
                            continue;
                        }
                        chromatic += 1;
                        let (seen, bg) = (oklab(ansi256_to_rgb(rgb_to_256(c.fg))), oklab(c.bg));
                        let (hs, hb) = (seen.b.atan2(seen.a), bg.b.atan2(bg.a));
                        let d = (hs - hb).abs().min(core::f32::consts::TAU - (hs - hb).abs()).to_degrees();
                        assert!(d <= 30.0 + 1e-3, "{chroma:?} {n}: {c:?} is {d}° off the glyph's hue");
                    }
                }
            }
        }
        assert!(chromatic > 0, "saturated glyphs keep their hue family on 256-color");
        let cyan = CellInputs { chroma: Some(Rgb::new(0, 200, 200)), ..inp(160, 160) };
        let c = step(&cyan, &set, &ComposeParams::default(), &mut HysteresisState::new(1, 1));
        assert_eq!(c.bg, Rgb::new(0, 95, 95), "{c:?}");
        let dim = CellInputs { chroma: Some(Rgb::new(200, 60, 0)), ..inp(60, 60) };
        let c = step(&dim, &set, &ComposeParams::default(), &mut HysteresisState::new(1, 1));
        assert!(c.attrs & attrs::DEFAULT_BG != 0 || neutral(c.bg), "a dim shade is nearest a gray: {c:?}");
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
    fn glyph_and_color_are_the_same_on_every_tier() {
        for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
            for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
                for cols in [40, 100] {
                    let set = select_palettes(tier, color, cols);
                    let (mut a, mut b) = (HysteresisState::new(1, 1), HysteresisState::new(1, 1));
                    for n in [31, 34, 120, 100, 140, 110, 150, 200, 40, 0, 255] {
                        let i = CellInputs { chroma: Some(Rgb::new(90, 120, 60)), ..inp(n, n / 2) };
                        let p = ComposeParams::default();
                        let (x, y) = (step(&i, &set, &p, &mut a), step(&i, &uni(), &p, &mut b));
                        assert_eq!((x.glyph(), x.fg), (y.glyph(), y.fg), "{tier:?} {color:?}");
                        if color == ColorDepth::True {
                            assert_eq!(x, y, "{tier:?}: the glyph tier never changes the shade");
                        }
                    }
                }
            }
        }
    }
}
