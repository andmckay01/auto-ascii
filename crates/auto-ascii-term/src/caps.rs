//! Capability data and per-frame stats.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorTier {
    True,
    C256,
    C16,
    Mono,
}

impl std::str::FromStr for ColorTier {
    type Err = String;

    fn from_str(s: &str) -> Result<ColorTier, String> {
        match s.to_ascii_lowercase().as_str() {
            "truecolor" | "true" | "24bit" | "rgb" => Ok(ColorTier::True),
            "256" | "256color" | "c256" => Ok(ColorTier::C256),
            "16" | "16color" | "c16" | "ansi" => Ok(ColorTier::C16),
            "mono" | "none" | "off" => Ok(ColorTier::Mono),
            other => Err(format!(
                "unknown color tier {other:?} (expected truecolor|256|16|mono)"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct GlyphFlags(pub u8);

impl GlyphFlags {
    pub const ASCII: GlyphFlags = GlyphFlags(1);
    pub const BLOCKS: GlyphFlags = GlyphFlags(1 << 1);
    pub const BOX_DRAWING: GlyphFlags = GlyphFlags(1 << 2);
    pub const BRAILLE: GlyphFlags = GlyphFlags(1 << 3);

    #[inline]
    pub const fn contains(self, other: GlyphFlags) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline]
    pub const fn with(self, other: GlyphFlags) -> GlyphFlags {
        GlyphFlags(self.0 | other.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlyphSupportTier {
    AsciiOnly,
    Cp437,
    UnicodeCore,
    UnicodeFull,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caps {
    pub color: ColorTier,
    pub glyphs: GlyphFlags,
    pub glyph_support: GlyphSupportTier,
    pub sync_2026: bool,
    pub cells: (u16, u16),
    pub cell_px: Option<(u16, u16)>,
    pub can_query: bool,
}

impl Default for Caps {
    fn default() -> Caps {
        Caps {
            color: ColorTier::True,
            glyphs: GlyphFlags::ASCII,
            glyph_support: GlyphSupportTier::AsciiOnly,
            sync_2026: false,
            cells: (80, 24),
            cell_px: None,
            can_query: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub bytes: u32,
    pub cells_damaged: u32,
    pub write_ns: u64,
    pub dropped: bool,
}
