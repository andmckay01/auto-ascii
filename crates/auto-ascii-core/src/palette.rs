//! The 8 shipped palettes as data, plus `PaletteSet` selection.
//!
//! Selection is keyed **charset tier × layer role**: density (cell count)
//! selects ramp *length within* a config — the coarse/fine ASCII split — and
//! color depth selects an *effective ramp-length cap* (truecolor shorter, mono
//! longest) instead of duplicating ramp data.
//!
//! `auto-ascii-core` stays terminal-free: [`GlyphTier`] and [`ColorDepth`] are
//! Caps-shaped input enums the player derives from `auto-ascii-term`'s probe result
//! (`Caps.glyphs`/`Caps.glyph_support` → `GlyphTier`, `Caps.color` →
//! `ColorDepth`).

use crate::ramp::{ASCII_BASE_COARSE, ASCII_BASE_FINE, FINE_MIN_COLS};

/// Charset tier — the glyph repertoire the terminal's font is trusted to
/// render (derived from `Caps` by the caller).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlyphTier {
    /// ASCII repertoire only, `0x20..=0x7E` — including the `" - _`
    /// subposition triplet, so the whole tier is CP437-safe.
    Ascii,
    /// Unicode blocks/box-drawing trusted (half-blocks, quadrants, `╱╲`).
    UnicodeBlocks,
    /// Braille U+2800–28FF *verified* present (gates palette 7).
    BrailleVerified,
}

/// Color depth — mirrors `auto-ascii-term::ColorTier` variants without the
/// dependency. Drives the per-tier ramp-length caps (truecolor gets shorter
/// effective ramps because color carries luminance; mono the longest).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorDepth {
    True,
    C256,
    C16,
    Mono,
}

/// Density band (coarse below 70 viewport cols).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DensityBand {
    Coarse,
    Fine,
}

impl DensityBand {
    /// Band for a viewport width (video area cols, not terminal cols).
    #[inline]
    pub fn from_cols(viewport_cols: u16) -> DensityBand {
        if viewport_cols < FINE_MIN_COLS {
            DensityBand::Coarse
        } else {
            DensityBand::Fine
        }
    }
}

/// Layer role — the second axis of the palette selection key. `PaletteSet`
/// holds one choice per role; the enum names them for docs/tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayerRole {
    Base,
    Edge,
    Highlight,
    Detail,
}

/// Screen-space orientation class an 8-bin edge orientation collapses to.
///
/// Bins are 22.5° of edge-tangent angle θ in **image coordinates (y down)**;
/// θ ∈ (0°, 90°) descends to the right on screen, so bins 1–2 render as `\`
/// and bins 5–6 as `/` (see `orient.rs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlyphClass {
    /// θ near 0°/180° — horizontal stroke (bins 0, 7).
    H = 0,
    /// θ near 45° image-space — `\` on screen (bins 1, 2).
    DiagDown = 1,
    /// θ near 90° — vertical stroke (bins 3, 4).
    V = 2,
    /// θ near 135° image-space — `/` on screen (bins 5, 6).
    DiagUp = 3,
}

impl GlyphClass {
    /// Collapse an 8-bin orientation (see [`crate::orient::octant_bin`]) to
    /// its glyph class.
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

/// Vertical sub-cell position from the Vc×2Vr luma pair.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubPos {
    Top = 0,
    Mid = 1,
    Bottom = 2,
}

/// Classify the (top, bottom) luma pair: decisive when the halves differ by at
/// least `delta`, else `Mid`.
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

/// Orientation LUT for one edge palette: glyph per `[GlyphClass][SubPos]`,
/// plus junction glyphs for conflicting bins (palettes 3 and 6).
#[derive(Debug, PartialEq, Eq)]
pub struct EdgeLut {
    pub by_class: [[char; 3]; 4],
    /// Emitted when orientation bins conflict inside the cell (coherence in
    /// the junction band).
    pub junction: char,
    /// Junction at strong edge magnitude (≥ `ComposeParams::edge_strong`).
    pub junction_strong: char,
}

/// Palette 3 — `ascii/edge`: `- / | \` + `_ = + #` (junction).
///
/// ASCII has no overline glyph, so the horizontal top-subposition slot uses
/// `=` (the closest raised-ink horizontal in the palette's glyph set); `+` is
/// the junction, `#` the strong junction.
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

/// Palette 6 — `unicode/edge`: `─ │ ╱ ╲ ‾ _ ┼` (8-dir + junction).
/// `‾`/`_` are the horizontal top/bottom subposition variants.
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

/// Palette 4 — `ascii/highlight`: `" .+*"`. Shared by all tiers — there is no
/// separate unicode highlight ramp.
pub const ASCII_HIGHLIGHT: &[char] = &[' ', '.', '+', '*'];

/// Palette 5 (base half) — `unicode/base`: `" ·░▒▓█"`.
pub const UNICODE_BASE: &[char] = &[' ', '·', '░', '▒', '▓', '█'];

/// Palette 5 (quadrant half) — `▖▘▝▗▀▄▌▐` from the 2×2 pattern.
///
/// The runtime samples luma at Vc×2Vr only, so the 2×2 pattern is
/// *reconstructed* from the vertical pair plus the dominant orientation —
/// diagonal orientations pick corner quadrants via [`quadrant_for`]; `▀▄` are
/// the half-block path; `▌▐` are unreachable (a left–right split needs
/// 2Vc×2Vr sampling) and are listed for palette completeness.
pub const UNICODE_QUADRANTS: &[char] = &['▖', '▘', '▝', '▗', '▀', '▄', '▌', '▐'];

/// Palette 8 — `mono-fallback/base` for 16-color / no-color / Linux console:
/// `" .:coO8@"`, CP437-safe.
pub const MONO_FALLBACK_BASE: &[char] = &[' ', '.', ':', 'c', 'o', 'O', '8', '@'];

/// Subposition glyph triplet `" - _` for ASCII tiers, indexed by [`SubPos`].
/// `-` (mid) is never decisive in the base path.
///
/// ASCII has no overline (the same gap [`ASCII_EDGE`] works around), and
/// U+203E OVERLINE is not in CP437, so the Linux console cannot draw it. The
/// top slot uses `"` instead: the closest raised-ink ASCII glyph, and a match
/// for `_` by ink weight.
pub const SUBPOS_GLYPHS: [char; 3] = ['"', '-', '_'];

/// Braille orientation LUT — palette 7, `unicode/detail`: dot masks per
/// `[GlyphClass][SubPos]` + junction, rendered via [`braille_glyph`].
///
/// Dot bit layout (U+2800 offset): 0x01 r1c1, 0x02 r2c1, 0x04 r3c1, 0x08 r1c2,
/// 0x10 r2c2, 0x20 r3c2, 0x40 r4c1, 0x80 r4c2. Braille is **edge/texture
/// only, never solid fills** — every mask keeps ≤ 5 of 8 dots (unit-tested)
/// and the compositor only reaches it on edge-gated cells.
#[derive(Debug, PartialEq, Eq)]
pub struct BrailleLut {
    pub by_class: [[u8; 3]; 4],
    pub junction: u8,
}

/// See [`BrailleLut`].
pub const BRAILLE_EDGE: BrailleLut = BrailleLut {
    by_class: [
        [0x09, 0x36, 0xC0],
        [0xA3, 0xA3, 0xA3],
        [0x47, 0x47, 0x47],
        [0x5C, 0x5C, 0x5C],
    ],
    junction: 0x57,
};

/// Braille char for a dot mask: U+2800 + mask (always a valid scalar).
#[inline]
pub fn braille_glyph(mask: u8) -> char {
    char::from_u32(0x2800 + mask as u32).unwrap_or(' ')
}

/// Corner quadrant for a diagonal edge through the cell (palette 5).
///
/// Image-space reasoning (y down): a `\` stroke (DiagDown) separates the
/// screen upper-right from the lower-left — bright top half ⇒ upper-right
/// bright ⇒ `▝`; bright bottom ⇒ `▖`. A `/` stroke (DiagUp) separates
/// upper-left from lower-right ⇒ `▘` / `▗`. Horizontal/vertical classes
/// return `None` (the half-block / base paths handle them; left–right splits
/// need 2Vc sampling — see [`UNICODE_QUADRANTS`]).
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

/// Every distinct glyph the compositor can emit at charset tier `tier`,
/// enumerated **from the palette data itself** (never hardcoded lists):
/// [`select_palettes`] is walked over all four color depths × both density
/// bands, collecting the full backing ramps, the edge LUT (both junctions),
/// the subposition triplet, the quadrant/half-block set and the reachable
/// braille masks. Sorted by codepoint, deduplicated.
///
/// This is the repertoire a font must cover for the tier to render without
/// missing-glyph boxes — the font-coverage-table generator and the
/// `--font-table` repertoire veto both consume it.
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

/// Union of [`tier_glyphs`] over every charset tier — every glyph any of the
/// 8 shipped palettes can put on screen. Sorted, deduplicated.
pub fn all_palette_glyphs() -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
        out.extend(tier_glyphs(tier));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Effective ramp-length cap on truecolor tiers (color carries luminance, so
/// ramps stay short and smooth).
pub const RAMP_CAP_TRUE: u8 = 8;
/// Effective ramp-length cap on 256-color tiers.
pub const RAMP_CAP_256: u8 = 12;

/// A ramp with a per-tier *effective length*: the same `&'static` glyph data
/// serves every color tier, `len` just quantizes coarser. Indices 0..len
/// spread over the full glyph range with exact endpoints (0 → first,
/// len−1 → last).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RampView {
    glyphs: &'static [char],
    len: u8,
}

impl RampView {
    /// View over `glyphs` capped to `cap` steps (min 1; empty ramps are a bug).
    pub fn new(glyphs: &'static [char], cap: u8) -> RampView {
        assert!(!glyphs.is_empty(), "empty ramp");
        let full = glyphs.len().min(u8::MAX as usize) as u8;
        RampView { glyphs, len: full.min(cap).max(1) }
    }

    /// Effective step count (hysteresis indices run 0..len).
    #[inline]
    pub fn len(&self) -> u8 {
        self.len
    }

    /// Never true — kept for clippy's `len`-without-`is_empty` convention.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Backing glyph data (full, uncapped).
    #[inline]
    pub fn glyphs(&self) -> &'static [char] {
        self.glyphs
    }

    /// Glyph for effective index `idx` (clamped to len−1).
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

/// The per-role palette choices for one (charset tier × color depth × density)
/// configuration — the compositor's lookup surface.
#[derive(Clone, Copy, Debug)]
pub struct PaletteSet {
    /// L0 base ramp (role [`LayerRole::Base`]).
    pub base: RampView,
    /// L2 highlight ramp (role [`LayerRole::Highlight`], palette 4).
    pub highlight: RampView,
    /// L1 edge orientation LUT (role [`LayerRole::Edge`], palette 3 or 6).
    pub edge: &'static EdgeLut,
    /// Half-block `▀▄` (fg,bg) pairs available (unicode tiers).
    pub halfblock: bool,
    /// Quadrant refinement of half-blocks available (palette 5).
    pub quadrant: bool,
    /// Braille detail replaces edge glyphs (role [`LayerRole::Detail`],
    /// palette 7 — BrailleVerified tier at fine density only).
    pub braille: bool,
    /// ASCII `" - _` subposition glyphs active (ascii tiers).
    pub subpos: bool,
    /// A dim cell background survives quantization as a tint of the cell's
    /// own color (truecolor and 256-color; 16-color would snap it to a
    /// palette hue and mono drops it).
    pub bg_tint: bool,
}

/// Select the palette configuration (keyed by charset tier × layer role;
/// density picks ramp length within the config; color depth caps ramp length).
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
