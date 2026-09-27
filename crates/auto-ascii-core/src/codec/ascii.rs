//! Printable ASCII glyph codec over a capped background shade.

use crate::cell::{Cell, Rgb, attrs};
use crate::codec::GlyphCodec;
use crate::codec::letters::{
    EDGE, HALF_MASK, HALF_NONE, HALF_SHIFT, HALF_TOP, JUNCTION, coverage, half_variant, tint,
};
use crate::compose::{CellInputs, ComposeParams, boost, h_flags, layer};
use crate::hysteresis::{CellState, IDX_UNSET, cell_flags, edge_gate};
use crate::orient::{bin_with_guard, coherence_at_least, debias};
use crate::quant::{ansi256_to_rgb, rgb_to_256};
use crate::palette::{ASCII_HIGHLIGHT, ColorDepth, GlyphClass, PaletteSet, subpos};

pub const ASCII_RAMP: &[char] = &[
    ' ', '.', ':', ';', '+', 'r', 'c', 'x', 'n', 'o', 'e', 'a', 'S', '#', 'D', '8', 'B', '@',
];

pub const ASCII_INK: &[u8] = &[0, 27, 50, 65, 90, 107, 133, 137, 142, 150, 158, 167, 177, 190, 196, 212, 218, 255];

pub const ASCII_TOP: &[char] = &[
    ' ', '\'', '\'', '\'', '"', '"', '"', '"', 'T', 'T', 'Y', 'Y', 'F', '7', '7', 'P', 'P', 'M',
];

pub const ASCII_BOTTOM: &[char] = &[
    ' ', '.', '.', ',', ',', ',', 'v', 'v', 'u', 'u', 'u', 'a', 'a', 'a', 'w', 'w', 'g', 'g',
];

const BLACK_FLOOR: u8 = 32;
const FLOOR_HOLD: u8 = BLACK_FLOOR / 2;
const INK_FROM: u8 = 24;

const SETTLE_MAX_FRAMES: u8 = 32;
const CONVERGE_MAX_FRAMES: u8 = 64;
const CONVERGE_CLEAR: u8 = 4;
const FLOOR_SETTLE_FRAMES: u8 = 4;
const SETTLE_DOWN: u8 = 0x80;
const SETTLE_SLOW: u8 = 0x40;
const SETTLE_COUNT: u8 = 0x3F;
const ACTIVITY_SHIFT: u8 = 4;
const ACTIVE: u8 = 9;

const TONE_TOP: u8 = 240;

pub const SHADE_CEIL: u8 = 154;

pub const SHADE_BLANK_CEIL: u8 = 0;

pub const SHADE_CONTRAST_Q8: u32 = 96;

pub const SHADE_FLOOR: u8 = 1;

pub const SHADE_HUE_COS2: f32 = 0.75;

pub const SHADE_HUE_MIN_CHROMA: f32 = 0.03;

const SHADE_Q8: u32 = 154;

pub const SHADES_256: [Rgb; 41] = [
    Rgb::new(8, 8, 8),
    Rgb::new(18, 18, 18),
    Rgb::new(28, 28, 28),
    Rgb::new(38, 38, 38),
    Rgb::new(48, 48, 48),
    Rgb::new(58, 58, 58),
    Rgb::new(68, 68, 68),
    Rgb::new(78, 78, 78),
    Rgb::new(88, 88, 88),
    Rgb::new(98, 98, 98),
    Rgb::new(108, 108, 108),
    Rgb::new(118, 118, 118),
    Rgb::new(128, 128, 128),
    Rgb::new(138, 138, 138),
    Rgb::new(148, 148, 148),
    Rgb::new(0, 0, 95),
    Rgb::new(0, 0, 135),
    Rgb::new(0, 95, 0),
    Rgb::new(0, 95, 95),
    Rgb::new(0, 95, 135),
    Rgb::new(0, 135, 0),
    Rgb::new(0, 135, 95),
    Rgb::new(0, 135, 135),
    Rgb::new(95, 0, 0),
    Rgb::new(95, 0, 95),
    Rgb::new(95, 0, 135),
    Rgb::new(95, 95, 0),
    Rgb::new(95, 95, 95),
    Rgb::new(95, 95, 135),
    Rgb::new(95, 135, 0),
    Rgb::new(95, 135, 95),
    Rgb::new(95, 135, 135),
    Rgb::new(135, 0, 0),
    Rgb::new(135, 0, 95),
    Rgb::new(135, 0, 135),
    Rgb::new(135, 95, 0),
    Rgb::new(135, 95, 95),
    Rgb::new(135, 95, 135),
    Rgb::new(135, 135, 0),
    Rgb::new(135, 135, 95),
    Rgb::new(135, 135, 135),
];

const SHADES_256_LAB: [Lab; 41] = shades_lab();

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
    let span = (TONE_TOP - INK_FROM) as u32;
    let x = (n as u32).saturating_sub(INK_FROM as u32);
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

const fn shades_lab() -> [Lab; 41] {
    let mut t = [Lab { l: 0.0, a: 0.0, b: 0.0 }; 41];
    let mut i = 0;
    while i < SHADES_256.len() {
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
    if c2(glyph) < SHADE_HUE_MIN_CHROMA * SHADE_HUE_MIN_CHROMA {
        return true;
    }
    let dot = glyph.a * shade.a + glyph.b * shade.b;
    dot > 0.0 && dot * dot >= SHADE_HUE_COS2 * c2(glyph) * c2(shade)
}

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
    let mut n = FLOOR_HOLD as usize;
    while n < 256 {
        let mut want = unit(n);
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

#[inline]
pub fn luminance(c: Rgb) -> u32 {
    let l = |v: u8| LINEAR[v as usize] as u32;
    (13933 * l(c.r) + 46871 * l(c.g) + 4732 * l(c.b)) >> 16
}

pub fn backing_within_cap(glyph: char, fg: Rgb, bg: Rgb) -> bool {
    let top = bg.r.max(bg.g).max(bg.b);
    if glyph == ' ' {
        return top == SHADE_BLANK_CEIL;
    }
    top <= SHADE_CEIL && (luminance(bg) << 8) <= SHADE_CONTRAST_Q8 * luminance(fg)
}

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
    if glyph == ' ' { return None; }
    let top = c.r.max(c.g).max(c.b) as u32;
    let mut k = coverage(n) * SHADE_Q8;
    if top == 0 || k == 0 {
        return None;
    }
    let ceil = SHADE_CEIL as u32;
    if (top * k) >> 16 > ceil {
        k = (ceil << 16).div_ceil(top);
    }
    let limit = (SHADE_CONTRAST_Q8 * luminance(seen)) >> 8;
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
fn jump_band(n: u8, from: u8, hyst_q8: u8, flags: &mut u8) -> u8 {
    let seen = (n.abs_diff(from) / 4).min(15);
    let was = *flags >> ACTIVITY_SHIFT;
    let now = if seen > was { was + (seen - was).div_ceil(8) } else { was - (was - seen) / 8 };
    *flags = (*flags & ((1 << ACTIVITY_SHIFT) - 1)) | (now << ACTIVITY_SHIFT);
    let h = hyst_q8 as u32;
    let cut = now.saturating_sub(ACTIVE) as u32 * (h * 3 / 16);
    (h * 5 / 8).saturating_sub(cut).max(h / 4) as u8
}

#[inline]
fn held_tone(n: u8, prev: u8, hyst_q8: u8, s: &mut CellState) -> u8 {
    let n = n.min(IDX_UNSET - 1);
    let from = if s.tone_candidate == IDX_UNSET { prev } else { s.tone_candidate };
    if prev == IDX_UNSET || n.abs_diff(prev) > jump_band(n, from, hyst_q8, &mut s.flags) {
        s.tone_candidate = n;
        s.tone_age = 0;
        return n;
    }
    let smooth = if n >= from { from + (n - from).div_ceil(4) } else { from - (from - n).div_ceil(4) };
    s.tone_candidate = smooth;
    let floor_crossing = (n < FLOOR_HOLD) != (prev < FLOOR_HOLD);
    let down = smooth < prev;
    let inside = if down { smooth.saturating_add(CONVERGE_CLEAR) } else { smooth.saturating_sub(CONVERGE_CLEAR) };
    let (side, wait) = if floor_crossing {
        (if n < FLOOR_HOLD { SETTLE_DOWN } else { 0 }, (FLOOR_SETTLE_FRAMES - 1).min(hyst_q8 / 8))
    } else if STEP[smooth as usize] == STEP[prev as usize] {
        s.tone_age = 0;
        return prev;
    } else if smooth.abs_diff(prev) > hyst_q8 / 8 {
        (if down { SETTLE_DOWN } else { 0 }, (hyst_q8 / 4).min(SETTLE_MAX_FRAMES - 1))
    } else if STEP[inside as usize] != STEP[prev as usize] {
        (SETTLE_SLOW | if down { SETTLE_DOWN } else { 0 }, (hyst_q8 / 2).min(CONVERGE_MAX_FRAMES - 1))
    } else {
        s.tone_age = 0;
        return prev;
    };
    let count = if s.tone_age != 0 && s.tone_age & !SETTLE_COUNT == side { s.tone_age & SETTLE_COUNT } else { 0 };
    if count < wait {
        s.tone_age = side | (count + 1);
        return prev;
    }
    let h = if floor_crossing { n } else { smooth };
    s.tone_candidate = h;
    s.tone_age = 0;
    h
}

pub struct Ascii;

impl GlyphCodec for Ascii {
    const NAME: &'static str = "ascii";

    const PAD: Cell = put(' ', Rgb::WHITE, None);

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
        let color_tone = if deep_shadow { 0 } else { ink_tone };
        let lit = prev != IDX_UNSET && prev >= BLACK_FLOOR;
        let floor = if lit { FLOOR_HOLD } else { BLACK_FLOOR };
        let target = if color_tone < floor { 0 } else { color_tone };
        let h = held_tone(target, prev, params.idx_hyst_q8, s);
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

        let tone = if deep_shadow { 0 } else { n };
        let c = inp.chroma.unwrap_or(Rgb::gray(n));
        let fg = tint(c, tone);
        let cell = |g: char, fg: Rgb, base: Rgb, tone: u8| put(g, fg, backing(base, tone, g, fg, set.color));
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
            return (cell(g, fg, c, tone), layer::EDGE);
        }

        if deep_shadow {
            return (put(ASCII_RAMP[0], fg, None), layer::SHADOW);
        }

        let hi_cut = ((len * params.hi_cut_q8 as u32) >> 8).max(1);
        if inp.h & h_flags::HIGHLIGHT != 0 && (i as u32) < hi_cut {
            let hlen = ASCII_HIGHLIGHT.len() as u32;
            let hidx = ((i as u32 * hlen) / hi_cut).min(hlen - 1) as usize;
            return (cell(ASCII_HIGHLIGHT[hidx], boost(fg), c, tone), layer::HIGHLIGHT);
        }

        if half != HALF_NONE && i > 0 {
            let g = if half == HALF_TOP { ASCII_TOP[i] } else { ASCII_BOTTOM[i] };
            let lit = lt.max(lb);
            let base = inp.chroma.unwrap_or(Rgb::gray(lit));
            return (cell(g, tint(base, lit), base, lt.min(lb)), layer::STRUCTURE);
        }

        (cell(ASCII_RAMP[i], fg, c, tone), layer::BASE)
    }
}

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
                                    assert!((w[i] as u32 * m).abs_diff(v[i] as u32 * s) <= m, "{c:?}: scaled {k:?}");
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
        assert_eq!(lit, Rgb::gray(153), "letters integer tint stays below the ceiling");
        assert_eq!(backing(white, 255, ' ', white, ColorDepth::True), None);
        let dim = Rgb::gray(70);
        let under = backing(white, 255, 'x', dim, ColorDepth::True).unwrap();
        assert!(backing_within_cap('x', dim, under), "{under:?}");
        assert!(!backing_within_cap('x', dim, Rgb::gray(under.r + 2)), "the largest shade under the cap");
        let q = backing(white, 255, 'x', dim, ColorDepth::C256).unwrap();
        assert!(cell_within_cap(&put('x', dim, Some(q)), ColorDepth::C256), "{q:?}");
        assert_eq!(backing(white, 255, 'x', Rgb::BLACK, ColorDepth::True), None, "a black glyph: no shade");
        let faint = Rgb::new(40, 20, 10);
        assert_eq!(backing(faint, 32, 'x', white, ColorDepth::True), None, "a shade below the floor is not sent");
        let lit = backing(faint, 255, 'x', white, ColorDepth::True).unwrap();
        assert!(lit.r >= SHADE_FLOOR, "{lit:?}");
        for color in [ColorDepth::C16, ColorDepth::Mono] {
            assert_eq!(backing(white, 255, '@', white, color), None);
        }
        assert_eq!((SHADE_CEIL, SHADE_BLANK_CEIL, SHADE_CONTRAST_Q8, SHADE_FLOOR), (154, 0, 96, 1));
        assert_eq!((luminance(Rgb::BLACK), luminance(Rgb::WHITE)), (0, 65535));
    }

    #[test]
    fn tint_and_backing_match_letters_curve_with_per_channel_clip() {
        let colors = [Rgb::new(255, 212, 160), Rgb::new(40, 20, 10), Rgb::new(255, 40, 0), Rgb::new(20, 60, 255), Rgb::WHITE];
        for c in colors {
            let mut last = Rgb::BLACK;
            for n in 0..=255 {
                let x = ((n as u32).saturating_sub(32) << 8) / 223;
                let cov = (x + ((x*x*(768-2*x)) >> 16)) >> 1;
                let expected = scaled(c, cov * 154);
                let bg = backing(c, n, '@', Rgb::WHITE, ColorDepth::True);
                assert_eq!(bg.unwrap_or(Rgb::BLACK), expected);
                let gain = 192 + ((cov * 320) >> 8);
                let clip = |v: u8| ((v as u32 * gain) >> 8).min(255) as u8;
                let fg = tint(c, n);
                assert_eq!(fg, Rgb::new(clip(c.r), clip(c.g), clip(c.b)), "{c:?} {n}");
                assert!(fg.r >= last.r && fg.g >= last.g && fg.b >= last.b, "{c:?} {n}");
                last = fg;
            }
        }
        assert_eq!(backing(Rgb::new(40, 20, 10), 64, '@', Rgb::WHITE, ColorDepth::True), Some(Rgb::new(2, 1, 0)));
        assert_eq!(tint(Rgb::new(160, 120, 80), 255), Rgb::new(255, 240, 160));
        assert_eq!(tint(Rgb::new(255, 40, 0), 255), Rgb::new(255, 80, 0));
        assert_eq!(tint(Rgb::new(20, 60, 255), 255), Rgb::new(40, 120, 255));
        assert_eq!(tint(Rgb::new(40, 20, 10), 255), Rgb::new(80, 40, 20));
    }

    #[test]
    fn glyph_foreground_matches_letters_on_the_ascii_tier() {
        use crate::codec::letters::Letters;
        let chromas = [None, Some(Rgb::new(200, 150, 120)), Some(Rgb::WHITE), Some(Rgb::new(255, 40, 0)),
            Some(Rgb::new(20, 60, 255)), Some(Rgb::new(40, 20, 10)), Some(Rgb::new(90, 120, 60))];
        let tones = [(20, 20), (40, 40), (64, 64), (100, 100), (128, 128), (170, 170), (200, 200), (240, 240),
            (255, 255), (250, 60), (60, 250), (220, 30), (30, 220), (140, 90), (90, 140), (180, 110)];
        let (mut same, mut boosted) = (0, 0);
        for color in [ColorDepth::True, ColorDepth::C256] {
            let letters = select_palettes(GlyphTier::Ascii, color, 100);
            for chroma in chromas {
                for (t, b) in tones {
                    for e in [0, 200] {
                        for h in [0, h_flags::HIGHLIGHT] {
                            let i = CellInputs { e, ex: 40, ey: 128, h, chroma, ..inp(t, b) };
                            let p = ComposeParams::default();
                            let (a, la) = Ascii::cell(&i, &ident(), &letters, &p, &mut CellState::default());
                            let (l, ll) = Letters::cell(&i, &ident(), &letters, &p, &mut CellState::default());
                            if a.glyph() == ' ' || l.glyph() == ' ' {
                                continue;
                            }
                            let what = format!("{color:?} {i:?}: ascii {a:?} {la}, letters {l:?} {ll}");
                            if la == ll {
                                assert_eq!(a.fg, l.fg, "{what}");
                                assert_eq!(rgb_to_256(a.fg), rgb_to_256(l.fg), "{what}");
                                same += 1;
                            } else {
                                assert!(la == layer::HIGHLIGHT || ll == layer::HIGHLIGHT, "{what}");
                                let (hi, lo, other) =
                                    if la == layer::HIGHLIGHT { (a.fg, l.fg, ll) } else { (l.fg, a.fg, la) };
                                if other == layer::BASE {
                                    assert_eq!(hi, boost(lo), "only the highlight boost differs: {what}");
                                }
                                boosted += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(same > 500 && boosted * 10 < same, "{same} matched, {boosted} on differing highlight cuts");
    }

    #[test]
    fn a_clipped_white_glyph_keeps_a_warm_capped_shade() {
        let skin = Rgb::new(200, 150, 120);
        let i = CellInputs { chroma: Some(skin), ..inp(255, 255) };
        let t = cold(&i);
        assert_eq!(t.fg, Rgb::new(255, 255, 240));
        assert_eq!(t.bg, scaled(skin, coverage(255) * SHADE_Q8), "letters' bg: a scaled copy of the chroma");
        assert!(cell_within_cap(&t, ColorDepth::True), "{t:?}");
        let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::C256, 100);
        let q = step(&i, &set, &ComposeParams::default(), &mut HysteresisState::new(1, 1));
        assert_eq!(q.bg, Rgb::new(135, 95, 95), "the warm entry letters quantizes to: {q:?}");
        assert!(cell_within_cap(&q, ColorDepth::C256), "{q:?}");
        let gray = CellInputs { chroma: Some(Rgb::gray(200)), ..inp(255, 255) };
        let g = step(&gray, &set, &ComposeParams::default(), &mut HysteresisState::new(1, 1));
        assert!(neutral(g.bg), "a gray source keeps a gray shade: {g:?}");
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
                        let (seen, bg) = (oklab(ansi256_to_rgb(rgb_to_256(c.fg))), oklab(c.bg));
                        if seen.a * seen.a + seen.b * seen.b < SHADE_HUE_MIN_CHROMA * SHADE_HUE_MIN_CHROMA {
                            continue;
                        }
                        chromatic += 1;
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
        let bright = CellInputs { chroma: Some(Rgb::new(0, 255, 255)), ..inp(255, 255) };
        let c = step(&bright, &set, &ComposeParams::default(), &mut HysteresisState::new(1, 1));
        assert_eq!(c.bg, Rgb::new(0, 135, 135), "135 cube level is safe: {c:?}");
        let dim = CellInputs { chroma: Some(Rgb::new(200, 60, 0)), ..inp(60, 60) };
        let c = step(&dim, &set, &ComposeParams::default(), &mut HysteresisState::new(1, 1));
        assert!(c.attrs & attrs::DEFAULT_BG != 0 || neutral(c.bg), "a dim shade is nearest a gray: {c:?}");
    }

    #[test]
    fn default_glyph_hold_rejects_near_boundary_chatter() {
        let mut state = CellState::default();
        let width = ComposeParams::default().idx_hyst_q8;
        let mut held = held_tone(120, IDX_UNSET, width, &mut state);
        let mut changes = 0;
        for n in [159, 120].into_iter().cycle().take(200) {
            let next = held_tone(n, held, width, &mut state);
            changes += usize::from(next != held);
            held = next;
        }
        assert!(changes <= 1 && (120..=159).contains(&held), "nearby alternating samples must not flash glyphs");
        let far = held + (width as u16 * 5 / 8) as u8 + 1;
        assert_eq!(held_tone(far, held, width, &mut state), far, "large changes remain immediate");
    }

    #[test]
    fn glyph_tone_settles_and_floor_crossings_do_not_latch() {
        for (from, to) in [(0, 40), (40, 12), (120, 150), (150, 120), (200, 170)] {
            let mut st = HysteresisState::new(1, 1);
            glyph(&inp(from, from), &mut st);
            for _ in 0..40 {
                glyph(&inp(to, to), &mut st);
            }
            assert_eq!(step(&inp(to, to), &uni(), &ComposeParams::default(), &mut st), cold(&inp(to, to)));
        }
        let mut st = HysteresisState::new(1, 1);
        assert_eq!(glyph(&inp(0, 0), &mut st), ' ');
        for _ in 0..8 {
            assert_eq!(glyph(&inp(BLACK_FLOOR - 1, BLACK_FLOOR - 1), &mut st), ' ', "a cold cell stays blank under the floor");
        }
        for n in [32, 33, 34] {
            glyph(&inp(n, n), &mut st);
        }
        assert_ne!(glyph(&inp(35, 35), &mut st), ' ');
        for n in [31, 24, 20, FLOOR_HOLD] {
            assert_eq!(glyph(&inp(n, n), &mut st), '.', "a lit cell holds down to FLOOR_HOLD: {n}");
        }
        for n in [15, 14, 13] {
            glyph(&inp(n, n), &mut st);
        }
        assert_eq!(glyph(&inp(12, 12), &mut st), ' ');
        assert_eq!(glyph(&inp(BLACK_FLOOR - 1, BLACK_FLOOR - 1), &mut st), ' ', "no re-arm under the floor");
        let off = ComposeParams { idx_hyst_q8: 0, ..ComposeParams::default() };
        step(&inp(120, 120), &uni(), &off, &mut st);
        assert_eq!(step(&inp(150, 150), &uni(), &off, &mut st), cold(&inp(150, 150)));
    }

    #[test]
    fn near_black_is_blank() {
        assert_eq!((BLACK_FLOOR, FLOOR_HOLD, INK_FROM), (32, 16, 24));
        for chroma in [None, Some(Rgb::new(200, 150, 120))] {
            let f = BLACK_FLOOR;
            assert_eq!(cold(&CellInputs { chroma, ..inp(f - 1, f - 1) }).glyph(), ' ', "just under the floor");
            assert_eq!(cold(&CellInputs { chroma, ..inp(f, f) }).glyph(), '.', "the floor draws the lightest ink");
        }
        for (n, &i) in STEP.iter().enumerate().take(BLACK_FLOOR as usize + 1) {
            assert_eq!(i, u8::from(n >= FLOOR_HOLD as usize), "held tones draw '.', lower ones nothing: {n}");
        }
    }

    #[test]
    fn black_floor_holds_a_lit_cell() {
        for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
            let set = select_palettes(GlyphTier::Ascii, color, 100);
            let p = ComposeParams::default();
            let mut st = HysteresisState::new(1, 1);
            assert_ne!(step(&inp(40, 40), &set, &p, &mut st).glyph(), ' ');
            let held = step(&CellInputs { chroma: Some(Rgb::new(200, 150, 120)), ..inp(FLOOR_HOLD, FLOOR_HOLD) }, &set, &p, &mut st);
            assert_eq!(held.glyph(), '.', "{color:?}: held down to FLOOR_HOLD");
            assert_eq!(held.fg, Rgb::new(150, 112, 90), "{color:?}: letters' 0.75x ink");
            assert_eq!(held.attrs & attrs::DEFAULT_BG, attrs::DEFAULT_BG, "{color:?}: no shade under the floor");
            let mut cold_st = HysteresisState::new(1, 1);
            assert_eq!(step(&inp(FLOOR_HOLD, FLOOR_HOLD), &set, &p, &mut cold_st).glyph(), ' ', "{color:?}: never armed");
        }
    }

    #[test]
    fn a_tone_settled_under_the_floor_blanks_like_letters() {
        let mut st = HysteresisState::new(1, 1);
        assert_eq!(glyph(&inp(60, 60), &mut st), ':');
        let mut shown = Vec::new();
        for _ in 0..80 {
            shown.push(glyph(&inp(20, 20), &mut st));
        }
        assert_eq!(shown.last(), Some(&' '), "{shown:?}");
        let blank = shown.iter().position(|&g| g == ' ').unwrap();
        assert!(shown[blank..].iter().all(|&g| g == ' '), "no re-arm: {shown:?}");
        let mut held = HysteresisState::new(1, 1);
        glyph(&inp(40, 40), &mut held);
        for _ in 0..80 {
            assert_eq!(glyph(&inp(20, 20), &mut held), '.', "a held glyph at tone 40 keeps its ink");
        }
    }

    #[test]
    fn glyph_tone_is_monotone_and_converges_within_74_frames() {
        for width in [0, 16, 90, 128, 160, 255] {
            for from in 0..=254u8 {
                for to in 0..=254u8 {
                    let mut held = from;
                    let mut state = CellState::default();
                    for f in 0..100 {
                        let next = held_tone(to, held, width, &mut state);
                        assert!(next >= held.min(to) && next <= held.max(to));
                        assert!(f < 74 || next == held, "{from}->{to}, width {width}: moved at frame {f}");
                        held = next;
                    }
                    let edge = if held < to { to.saturating_sub(CONVERGE_CLEAR) } else { to.saturating_add(CONVERGE_CLEAR) };
                    let near = STEP[held as usize] == STEP[to as usize] || STEP[edge as usize] == STEP[held as usize];
                    assert!(near, "{from}->{to}, width {width}: held {held}");
                }
            }
        }
    }

    #[test]
    fn a_steady_tone_inside_the_margin_reaches_its_own_glyph() {
        let width = ComposeParams::default().idx_hyst_q8;
        let mut state = CellState::default();
        let mut held = held_tone(142, IDX_UNSET, width, &mut state);
        assert_ne!(STEP[142], STEP[158]);
        let mut settled = None;
        for f in 0..80 {
            held = held_tone(158, held, width, &mut state);
            if settled.is_none() && STEP[held as usize] == STEP[158] {
                settled = Some(f);
            }
        }
        assert!(settled.is_some_and(|f| f >= 63), "{settled:?}: waits twice the settle time, then converges");
        assert_eq!(held, 158);
    }

    #[test]
    fn active_cells_take_moderate_changes_at_once() {
        let width = ComposeParams::default().idx_hyst_q8;
        let mut quiet = CellState::default();
        let held = held_tone(200, IDX_UNSET, width, &mut quiet);
        assert_eq!(held_tone(150, held, width, &mut quiet), 200, "a quiet cell holds a 50-unit change");
        let mut active = CellState::default();
        let mut held = held_tone(200, IDX_UNSET, width, &mut active);
        for n in [40, 200].into_iter().cycle().take(8) {
            held = held_tone(n, held, width, &mut active);
        }
        assert_eq!(held, 200);
        assert_eq!(held_tone(150, held, width, &mut active), 150, "a busy cell's band narrows to h/4");
        assert_eq!(held_tone(135, 150, width, &mut active), 150, "small changes still wait");
    }

    #[test]
    fn noisy_steady_tone_settles_once() {
        let width = ComposeParams::default().idx_hyst_q8;
        let mut state = CellState::default();
        let mut held = held_tone(120, IDX_UNSET, width, &mut state);
        let mut changes = Vec::new();
        for (f, jitter) in [0u8, 7, 2, 9, 4, 11, 1, 6, 10, 3, 8, 5].into_iter().cycle().take(120).enumerate() {
            let next = held_tone(150 + jitter, held, width, &mut state);
            if next != held {
                changes.push(f);
            }
            held = next;
        }
        assert_eq!(changes.len(), 1, "one settle, no chatter: {changes:?}");
        assert!(changes[0] < 41, "settled at frame {}", changes[0]);
        assert!((150..=161).contains(&held) && STEP[held as usize] != STEP[120], "{held}");
    }

    #[test]
    fn shade_and_foreground_follow_current_tone_while_glyph_waits() {
        for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
            let set = select_palettes(GlyphTier::UnicodeBlocks, color, 200);
            let params = ComposeParams::default();
            let mut st = HysteresisState::new(1, 1);
            step(&inp(120, 120), &set, &params, &mut st);
            for n in [150, 140, 155, 139, 151] {
                let warm = step(&inp(n, n), &set, &params, &mut st);
                let cold = step(&inp(n, n), &set, &params, &mut HysteresisState::new(1, 1));
                assert_ne!(warm.glyph(), cold.glyph());
                assert_eq!((warm.fg, warm.bg, warm.attrs), (cold.fg, cold.bg, cold.attrs));
                assert!(cell_within_cap(&warm, color));
            }
        }
    }

    #[test]
    fn deep_shadow_edges_do_not_gain_a_backing_from_current_tone() {
        let input = CellInputs { e: 200, ex: 28, h: h_flags::DEEP_SHADOW, ..inp(150, 150) };
        let (cell, layer) = Ascii::cell(
            &input, &ident(), &uni(), &ComposeParams::default(), &mut CellState::default(),
        );
        assert_eq!(layer, layer::EDGE);
        assert_ne!(cell.glyph(), ' ');
        assert_eq!(cell.attrs & attrs::DEFAULT_BG, attrs::DEFAULT_BG);
        assert_eq!(cell.fg, Rgb::gray(112));
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
