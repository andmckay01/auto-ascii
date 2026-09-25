//! Capability data and per-frame stats.

/// Color tier the backend quantizes to before diffing: truecolor
/// passthrough, xterm-256 cube+gray, standard 16, or glyph-only mono.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorTier {
    True,
    C256,
    C16,
    Mono,
}

impl std::str::FromStr for ColorTier {
    type Err = String;

    /// `--tier` forced-tier parsing. Canonical forms: `truecolor` | `256` |
    /// `16` | `mono`; common aliases accepted.
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

/// Glyph repertoire bitflags. Plain `u8` newtype — no bitflags dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct GlyphFlags(pub u8);

impl GlyphFlags {
    pub const ASCII: GlyphFlags = GlyphFlags(1);
    /// Unicode block/quadrant fills (`▀▄█░▒▓` …).
    pub const BLOCKS: GlyphFlags = GlyphFlags(1 << 1);
    /// Box-drawing / directional strokes (`─│╱╲` …).
    pub const BOX_DRAWING: GlyphFlags = GlyphFlags(1 << 2);
    /// Braille U+2800–U+28FF (verified support only).
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

/// Font-coverage trust tier: which glyph repertoires we believe the user's
/// font actually renders. Terminals can't be queried for fonts, so this is
/// conservative data, overridable via `--font-table`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlyphSupportTier {
    AsciiOnly,
    /// CP437-safe superset (Linux console).
    Cp437,
    /// Common Unicode: blocks + box drawing, no braille.
    UnicodeCore,
    /// Full repertoire including verified braille.
    UnicodeFull,
}

/// Terminal capabilities. Produced by [`crate::probe_caps`] (DA1-sentinel
/// volley + cache) or constructed directly. Capability tiers are color
/// depth + glyph repertoire only — no throughput/connectivity classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caps {
    pub color: ColorTier,
    pub glyphs: GlyphFlags,
    /// Trusted font repertoire tier (see [`GlyphSupportTier`]).
    pub glyph_support: GlyphSupportTier,
    /// DECRQM 2026 synchronized-output support (never a hardcoded table).
    pub sync_2026: bool,
    /// Terminal size in cells `(cols, rows)`.
    pub cells: (u16, u16),
    /// Cell size in px `(w, h)` from `CSI 16 t`, if known — drives cell
    /// aspect; `None` → aspect fallback 2.0.
    pub cell_px: Option<(u16, u16)>,
    /// False when probing is unsafe/pointless (`--no-query`, `!isatty`).
    pub can_query: bool,
}

impl Default for Caps {
    /// Kitty-class local terminal: truecolor, ASCII glyphs only, no sync
    /// assumed, 80×24 until resized.
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

/// Per-`present` accounting: feeds frame stats and damage/bytes metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Bytes written to the terminal for this frame.
    pub bytes: u32,
    /// Cells that survived quantize-then-diff and were emitted.
    pub cells_damaged: u32,
    /// Wall time of the single `write(2)` (or simulated write), nanoseconds.
    pub write_ns: u64,
    /// Frame dropped (write would block / behind schedule).
    pub dropped: bool,
}
