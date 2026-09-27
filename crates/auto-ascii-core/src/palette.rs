//! Glyph palettes, repertoire enumeration and tier selection.

use crate::ramp::{ASCII_BASE_COARSE, ASCII_BASE_FINE, FINE_MIN_COLS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlyphTier {
    Ascii,
    UnicodeBlocks,
    BrailleVerified,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorDepth {
    True,
    C256,
    C16,
    Mono,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DensityBand {
    Coarse,
    Fine,
}

impl DensityBand {
    #[inline]
    pub fn from_cols(viewport_cols: u16) -> DensityBand {
        if viewport_cols < FINE_MIN_COLS {
            DensityBand::Coarse
        } else {
            DensityBand::Fine
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayerRole {
    Base,
    Edge,
    Highlight,
    Detail,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlyphClass {
    H = 0,
    DiagDown = 1,
    V = 2,
    DiagUp = 3,
}

impl GlyphClass {
    #[inline]
    pub fn from_bin(bin: u8) -> GlyphClass {
        match bin {
            0 | 7 => GlyphClass::H,
            1 | 2 => GlyphClass::DiagDown,
            3 | 4 => GlyphClass::V,
            _ => GlyphClass::DiagUp,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubPos {
    Top = 0,
    Mid = 1,
    Bottom = 2,
}

#[inline]
pub fn subpos(luma_top: u8, luma_bottom: u8, delta: u8) -> SubPos {
    let delta = delta.max(1);
    if luma_top >= luma_bottom.saturating_add(delta) {
        SubPos::Top
    } else if luma_bottom >= luma_top.saturating_add(delta) {
        SubPos::Bottom
    } else {
        SubPos::Mid
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct EdgeLut {
    pub by_class: [[char; 3]; 4],
    pub junction: char,
    pub junction_strong: char,
}

pub const ASCII_EDGE: EdgeLut = EdgeLut {
    by_class: [
        ['=', '-', '_'],
        ['\\', '\\', '\\'],
        ['|', '|', '|'],
        ['/', '/', '/'],
    ],
    junction: '+',
    junction_strong: '#',
};

pub const UNICODE_EDGE: EdgeLut = EdgeLut {
    by_class: [
        ['‾', '─', '_'],
        ['╲', '╲', '╲'],
        ['│', '│', '│'],
        ['╱', '╱', '╱'],
    ],
    junction: '┼',
    junction_strong: '┼',
};

pub const ASCII_HIGHLIGHT: &[char] = &[' ', '.', '+', '*'];

pub const UNICODE_BASE: &[char] = &[' ', '·', '░', '▒', '▓', '█'];

pub const UNICODE_QUADRANTS: &[char] = &['▖', '▘', '▝', '▗', '▀', '▄', '▌', '▐'];

pub const MONO_FALLBACK_BASE: &[char] = &[' ', '.', ':', 'c', 'o', 'O', '8', '@'];

pub const SUBPOS_GLYPHS: [char; 3] = ['"', '-', '_'];

#[derive(Debug, PartialEq, Eq)]
pub struct BrailleLut {
    pub by_class: [[u8; 3]; 4],
    pub junction: u8,
}

pub const BRAILLE_EDGE: BrailleLut = BrailleLut {
    by_class: [
        [0x09, 0x36, 0xC0],
        [0xA3, 0xA3, 0xA3],
        [0x47, 0x47, 0x47],
        [0x5C, 0x5C, 0x5C],
    ],
    junction: 0x57,
};

#[inline]
pub fn braille_glyph(mask: u8) -> char {
    char::from_u32(0x2800 + mask as u32).unwrap_or(' ')
}

#[inline]
pub fn quadrant_for(class: GlyphClass, top_bright: bool) -> Option<char> {
    match (class, top_bright) {
        (GlyphClass::DiagDown, true) => Some('▝'),
        (GlyphClass::DiagDown, false) => Some('▖'),
        (GlyphClass::DiagUp, true) => Some('▘'),
        (GlyphClass::DiagUp, false) => Some('▗'),
        _ => None,
    }
}

pub fn tier_glyphs(tier: GlyphTier) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
        for cols in [FINE_MIN_COLS - 1, FINE_MIN_COLS] {
            let set = select_palettes(tier, color, cols);
            out.extend_from_slice(set.base.glyphs());
            out.extend_from_slice(set.highlight.glyphs());
            out.extend(set.edge.by_class.iter().flatten().copied());
            out.push(set.edge.junction);
            out.push(set.edge.junction_strong);
            if set.subpos {
                out.extend_from_slice(&SUBPOS_GLYPHS);
            }
            if set.quadrant || set.halfblock {
                out.extend_from_slice(UNICODE_QUADRANTS);
            }
            if set.braille {
                let masks =
                    BRAILLE_EDGE.by_class.iter().flatten().copied().chain([BRAILLE_EDGE.junction]);
                out.extend(masks.map(braille_glyph));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

pub fn all_palette_glyphs() -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
        out.extend(tier_glyphs(tier));
    }
    out.sort_unstable();
    out.dedup();
    out
}

pub const RAMP_CAP_TRUE: u8 = 8;
pub const RAMP_CAP_256: u8 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RampView {
    glyphs: &'static [char],
    len: u8,
}

impl RampView {
    pub fn new(glyphs: &'static [char], cap: u8) -> RampView {
        assert!(!glyphs.is_empty(), "empty ramp");
        let full = glyphs.len().min(u8::MAX as usize) as u8;
        RampView { glyphs, len: full.min(cap).max(1) }
    }

    #[inline]
    pub fn len(&self) -> u8 {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn glyphs(&self) -> &'static [char] {
        self.glyphs
    }

    #[inline]
    pub fn glyph(&self, idx: u8) -> char {
        let idx = idx.min(self.len - 1) as usize;
        let glen = self.glyphs.len();
        if self.len as usize == glen || self.len == 1 {
            self.glyphs[if self.len == 1 { 0 } else { idx }]
        } else {
            self.glyphs[idx * (glen - 1) / (self.len as usize - 1)]
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PaletteSet {
    pub base: RampView,
    pub highlight: RampView,
    pub edge: &'static EdgeLut,
    pub halfblock: bool,
    pub quadrant: bool,
    pub braille: bool,
    pub subpos: bool,
    pub bg_tint: bool,
    /// The color depth this set was selected for, for codecs whose colors
    /// must hold a rule after the backend quantizes them.
    pub color: ColorDepth,
}

pub fn select_palettes(tier: GlyphTier, color: ColorDepth, viewport_cols: u16) -> PaletteSet {
    let density = DensityBand::from_cols(viewport_cols);
    let cap = match color {
        ColorDepth::True => RAMP_CAP_TRUE,
        ColorDepth::C256 => RAMP_CAP_256,
        ColorDepth::C16 | ColorDepth::Mono => u8::MAX,
    };
    let base_glyphs: &'static [char] = match color {
        ColorDepth::C16 | ColorDepth::Mono => MONO_FALLBACK_BASE,
        _ => match tier {
            GlyphTier::Ascii => match density {
                DensityBand::Coarse => ASCII_BASE_COARSE,
                DensityBand::Fine => ASCII_BASE_FINE,
            },
            _ => UNICODE_BASE,
        },
    };
    let unicode = !matches!(tier, GlyphTier::Ascii);
    PaletteSet {
        base: RampView::new(base_glyphs, cap),
        highlight: RampView::new(ASCII_HIGHLIGHT, cap),
        edge: if unicode { &UNICODE_EDGE } else { &ASCII_EDGE },
        halfblock: unicode,
        quadrant: unicode,
        braille: matches!(tier, GlyphTier::BrailleVerified) && density == DensityBand::Fine,
        subpos: !unicode,
        bg_tint: matches!(color, ColorDepth::True | ColorDepth::C256),
        color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(chars: &[char]) -> String {
        chars.iter().collect()
    }

    #[test]
    fn palette_table_exact() {
        assert_eq!(s(ASCII_BASE_COARSE), " .:-=+*#%@");
        assert_eq!(s(ASCII_BASE_FINE), " .,:;i1tfLCG08@");
        assert_eq!(s(ASCII_HIGHLIGHT), " .+*");
        assert_eq!(s(UNICODE_BASE), " ·░▒▓█");
        assert_eq!(s(UNICODE_QUADRANTS), "▖▘▝▗▀▄▌▐");
        assert_eq!(s(MONO_FALLBACK_BASE), " .:coO8@");
        assert_eq!(SUBPOS_GLYPHS.iter().collect::<String>(), "\"-_");
    }

    #[test]
    fn every_ascii_tier_glyph_is_ascii() {
        let printable = |ch: char, what: &str| {
            assert!(
                ch == ' ' || ch.is_ascii_graphic(),
                "{what}: {ch:?} (U+{:04X}) is not CP437-safe ASCII",
                ch as u32
            );
        };
        for ch in SUBPOS_GLYPHS {
            printable(ch, "SUBPOS_GLYPHS");
        }
        for ch in ASCII_EDGE.by_class.iter().flatten().copied() {
            printable(ch, "ASCII_EDGE.by_class");
        }
        printable(ASCII_EDGE.junction, "ASCII_EDGE.junction");
        printable(ASCII_EDGE.junction_strong, "ASCII_EDGE.junction_strong");

        for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
            for cols in [40u16, 200] {
                let set = select_palettes(GlyphTier::Ascii, color, cols);
                assert!(!set.halfblock && !set.quadrant && !set.braille && set.subpos);
                assert_eq!(set.edge, &ASCII_EDGE, "ascii tier must use palette 3");
                for &ch in set.base.glyphs() {
                    printable(ch, "base ramp");
                }
                for &ch in set.highlight.glyphs() {
                    printable(ch, "highlight ramp");
                }
            }
        }
    }

    #[test]
    fn edge_lut_inventories_exact() {
        let inv = |lut: &EdgeLut| {
            let mut set: Vec<char> = lut
                .by_class
                .iter()
                .flatten()
                .copied()
                .chain([lut.junction, lut.junction_strong])
                .collect();
            set.sort_unstable();
            set.dedup();
            set
        };
        let mut ascii_plan: Vec<char> = "-/|\\_=+#".chars().collect();
        ascii_plan.sort_unstable();
        assert_eq!(inv(&ASCII_EDGE), ascii_plan);
        let mut uni_plan: Vec<char> = "─│╱╲‾_┼".chars().collect();
        uni_plan.sort_unstable();
        assert_eq!(inv(&UNICODE_EDGE), uni_plan);
    }

    #[test]
    fn braille_masks_never_solid() {
        let masks = BRAILLE_EDGE
            .by_class
            .iter()
            .flatten()
            .copied()
            .chain([BRAILLE_EDGE.junction]);
        for m in masks {
            assert!(m.count_ones() <= 5, "mask {m:#04x} too solid");
            let ch = braille_glyph(m);
            assert!(('\u{2800}'..='\u{28FF}').contains(&ch));
        }
    }

    #[test]
    fn quadrants_come_from_palette_5() {
        for class in [GlyphClass::DiagDown, GlyphClass::DiagUp] {
            for bright in [true, false] {
                let q = quadrant_for(class, bright).unwrap();
                assert!(UNICODE_QUADRANTS.contains(&q));
            }
        }
        assert_eq!(quadrant_for(GlyphClass::H, true), None);
        assert_eq!(quadrant_for(GlyphClass::V, false), None);
    }

    #[test]
    fn ramp_view_caps_and_spreads() {
        let v = RampView::new(ASCII_BASE_FINE, RAMP_CAP_TRUE);
        assert_eq!(v.len(), 8);
        assert_eq!(v.glyph(0), ' ');
        assert_eq!(v.glyph(7), '@');
        let full = RampView::new(ASCII_BASE_FINE, u8::MAX);
        assert_eq!(full.len(), 15);
        assert_eq!(full.glyph(14), '@');
        assert_eq!(full.glyph(3), ':');
    }

    #[test]
    fn selection_matrix() {
        let p = select_palettes(GlyphTier::Ascii, ColorDepth::True, 60);
        assert_eq!(p.base.glyphs(), ASCII_BASE_COARSE);
        assert_eq!(p.base.len(), 8);
        assert!(p.subpos && !p.halfblock && !p.quadrant && !p.braille);
        assert_eq!(p.edge, &ASCII_EDGE);

        let p = select_palettes(GlyphTier::Ascii, ColorDepth::C256, 200);
        assert_eq!(p.base.glyphs(), ASCII_BASE_FINE);
        assert_eq!(p.base.len(), RAMP_CAP_256);

        let p = select_palettes(GlyphTier::Ascii, ColorDepth::Mono, 200);
        assert_eq!(p.base.glyphs(), MONO_FALLBACK_BASE);
        assert_eq!(p.base.len(), 8);
        let p = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::C16, 200);
        assert_eq!(p.base.glyphs(), MONO_FALLBACK_BASE);

        let p = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, 100);
        assert_eq!(p.base.glyphs(), UNICODE_BASE);
        assert!(p.halfblock && p.quadrant && !p.braille && !p.subpos);
        assert_eq!(p.edge, &UNICODE_EDGE);

        let p = select_palettes(GlyphTier::BrailleVerified, ColorDepth::C256, 100);
        assert!(p.braille);
        let p = select_palettes(GlyphTier::BrailleVerified, ColorDepth::C256, 60);
        assert!(!p.braille, "coarse density must not enable braille");
    }

    #[test]
    fn bg_tint_only_where_a_dim_background_survives() {
        for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
            for (color, tint) in [
                (ColorDepth::True, true),
                (ColorDepth::C256, true),
                (ColorDepth::C16, false),
                (ColorDepth::Mono, false),
            ] {
                assert_eq!(select_palettes(tier, color, 100).bg_tint, tint, "{tier:?} {color:?}");
            }
        }
    }

    #[test]
    fn glyph_enumeration_covers_all_palette_data() {
        let all = all_palette_glyphs();
        assert!(all.windows(2).all(|w| w[0] < w[1]), "sorted + deduped");
        let expect = |chars: &[char]| {
            for &ch in chars {
                assert!(all.contains(&ch), "{ch:?} missing from all_palette_glyphs");
            }
        };
        expect(ASCII_BASE_COARSE);
        expect(ASCII_BASE_FINE);
        let e: Vec<char> = ASCII_EDGE
            .by_class
            .iter()
            .flatten()
            .copied()
            .chain([ASCII_EDGE.junction, ASCII_EDGE.junction_strong])
            .collect();
        expect(&e);
        expect(ASCII_HIGHLIGHT);
        expect(UNICODE_BASE);
        expect(UNICODE_QUADRANTS);
        let e: Vec<char> = UNICODE_EDGE
            .by_class
            .iter()
            .flatten()
            .copied()
            .chain([UNICODE_EDGE.junction, UNICODE_EDGE.junction_strong])
            .collect();
        expect(&e);
        let b: Vec<char> = BRAILLE_EDGE
            .by_class
            .iter()
            .flatten()
            .copied()
            .chain([BRAILLE_EDGE.junction])
            .map(braille_glyph)
            .collect();
        expect(&b);
        expect(MONO_FALLBACK_BASE);
        expect(&SUBPOS_GLYPHS);

        let mut union: Vec<char> = ASCII_BASE_COARSE
            .iter()
            .chain(ASCII_BASE_FINE)
            .chain(ASCII_HIGHLIGHT)
            .chain(UNICODE_BASE)
            .chain(UNICODE_QUADRANTS)
            .chain(MONO_FALLBACK_BASE)
            .chain(&SUBPOS_GLYPHS)
            .copied()
            .chain(ASCII_EDGE.by_class.iter().flatten().copied())
            .chain([ASCII_EDGE.junction, ASCII_EDGE.junction_strong])
            .chain(UNICODE_EDGE.by_class.iter().flatten().copied())
            .chain([UNICODE_EDGE.junction, UNICODE_EDGE.junction_strong])
            .chain(b)
            .collect();
        union.sort_unstable();
        union.dedup();
        assert_eq!(all, union);

        for ch in tier_glyphs(GlyphTier::Ascii) {
            assert!(ch == ' ' || ch.is_ascii_graphic(), "{ch:?} not ASCII");
        }
        let uni = tier_glyphs(GlyphTier::UnicodeBlocks);
        let braille = tier_glyphs(GlyphTier::BrailleVerified);
        for ch in &uni {
            assert!(braille.contains(ch), "braille tier must be a superset");
        }
        assert!(braille.iter().any(|c| ('\u{2800}'..='\u{28FF}').contains(c)));
        assert!(!uni.iter().any(|c| ('\u{2800}'..='\u{28FF}').contains(c)));
    }

    #[test]
    fn subpos_classification() {
        assert_eq!(subpos(200, 20, 64), SubPos::Top);
        assert_eq!(subpos(20, 200, 64), SubPos::Bottom);
        assert_eq!(subpos(120, 130, 64), SubPos::Mid);
        assert_eq!(subpos(128, 128, 0), SubPos::Mid);
    }
}
