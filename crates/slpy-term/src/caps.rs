//! Capability data and per-frame stats (PLAN §3.1).

/// Color tier the backend quantizes to before diffing (PLAN §3.1
/// "quantize before diff"). M0 runs `True` only; 256/16/mono land at M1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorTier {
    True,
    C256,
    C16,
    Mono,
}

/// Glyph repertoire bitflags (PLAN §3.1 `GlyphFlags`). Plain `u8` newtype —
/// no bitflags dep (slpy-term keeps its dependency list minimal, PLAN §8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct GlyphFlags(pub u8);

impl GlyphFlags {
    pub const ASCII: GlyphFlags = GlyphFlags(1);
    /// Unicode block/quadrant fills (`▀▄█░▒▓` …).
    pub const BLOCKS: GlyphFlags = GlyphFlags(1 << 1);
    /// Box-drawing / directional strokes (`─│╱╲` …).
    pub const BOX_DRAWING: GlyphFlags = GlyphFlags(1 << 2);
    /// Braille U+2800–U+28FF (verified-support only, PLAN §3.4 palette 7).
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
/// font actually renders (PLAN §3.1 `glyph_support`, §3.4 coverage tables,
/// risk §9.5). Terminals can't be queried for fonts, so this is conservative
/// data, overridable via `--font-table`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlyphSupportTier {
    AsciiOnly,
    /// CP437-safe superset (Linux console, PLAN §3.4 palette 8).
    Cp437,
    /// Common Unicode: blocks + box drawing, no braille.
    UnicodeCore,
    /// Full repertoire including verified braille.
    UnicodeFull,
}

/// Link throughput class (PLAN §3.1): `Slow` flagged on SSH/tmux/ConPTY
/// detection; drives the governor's byte budget (§3.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Throughput {
    Fast,
    Normal,
    Slow,
}

/// Terminal capabilities (PLAN §3.1). Produced by the M1 probe
/// (DA1-sentinel volley + cache); M0 constructs it directly.
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
    /// Cell size in px `(w, h)` from `CSI 16 t`, if known — drives cell aspect
    /// (PLAN §3.2); `None` → aspect fallback 2.0.
    pub cell_px: Option<(u16, u16)>,
    pub throughput: Throughput,
    /// False when probing is unsafe/pointless (`--no-query`, `!isatty`).
    pub can_query: bool,
}

impl Default for Caps {
    /// M0 default: the kitty-class local target (PLAN §7) — truecolor, ASCII
    /// glyphs only (base ramps 1–2), no sync assumed, 80×24 until resized.
    fn default() -> Caps {
        Caps {
            color: ColorTier::True,
            glyphs: GlyphFlags::ASCII,
            glyph_support: GlyphSupportTier::AsciiOnly,
            sync_2026: false,
            cells: (80, 24),
            cell_px: None,
            throughput: Throughput::Fast,
            can_query: false,
        }
    }
}

/// Per-`present` accounting (PLAN §3.1): feeds the governor's EMA (§3.6 step 7)
/// and the eval harness's damage/bytes metrics (§6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Bytes written to the terminal for this frame.
    pub bytes: u32,
    /// Cells that survived quantize-then-diff and were emitted.
    pub cells_damaged: u32,
    /// Wall time of the single `write(2)` (or simulated write), nanoseconds.
    pub write_ns: u64,
    /// Frame dropped (write would block / behind schedule — PLAN §3.6 pacing).
    pub dropped: bool,
}
