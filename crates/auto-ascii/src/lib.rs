//! **auto-ascii** — realtime ASCII-art video for terminals and embedders.
//!
//! An offline factory distills reference video into a resolution-independent
//! feature asset (`.ascii`); this crate maps that asset onto whatever cell
//! grid you have right now — glyph ramps, directional edge strokes,
//! highlights and half-blocks, with temporal hysteresis so nothing flickers.
//! Assets never store glyphs; every glyph decision happens at render time
//! for *your* grid, *your* palette, *your* terminal.

#[doc(hidden)]
pub mod pipeline;

mod composition;
mod error;
mod session;

/// Flattening a [`Composition`] into one `.ascii` file.
pub mod compose;

/// One decode pipeline per composition clip, created on demand. Hidden: the
/// workspace harness contract (the `--sim` path drives it), not the
/// embedding API — [`RenderSession`] and `Player` are the supported ways in.
#[doc(hidden)]
pub mod deck;

/// Timestamp parsing and formatting, shared by every entry point that takes
/// a time.
pub mod timecode;

#[cfg(feature = "terminal")]
mod player;

/// Per-video player settings (the dials and codec saved with `s`), kept in a
/// `<asset>.player.toml` beside each asset. Hidden: the player's persistence
/// contract, exposed for the workspace tests, not the embedding API.
#[cfg(feature = "terminal")]
#[doc(hidden)]
pub mod settings;

pub use composition::{
    Clip, ClipMark, ClipSpan, Composition, Located, Overlap, SCHEMA_VERSION, Span,
};
pub use error::Error;
pub use session::RenderSession;

#[cfg(feature = "terminal")]
pub use player::{Dial, MIN_FPS_CAP, Player, PlayerBuilder, RepaintMode, SCRUB_STEP_SECS};

pub use auto_ascii_core::{Cell, Grid, Rgb};
/// Glyph codecs — how a cell's features become a glyph: `pixels` (the
/// default, picture-like) or `letters` (printable characters). Chosen with
/// [`RenderSession::set_codec`] or `PlayerBuilder::codec`, cycled live with
/// `/` in the player.
pub use auto_ascii_core::Codec;
#[cfg(feature = "terminal")]
pub use auto_ascii_term::ColorTier;

/// Glyph repertoire selection — the charset-tier axis of the shipped
/// palettes. The color axis is chosen separately (probed terminal tier for
/// `Player`; always truecolor for [`RenderSession`] — embedders own any
/// quantization).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaletteChoice {
    /// Derive from the probed terminal capabilities (`Player`); Unicode
    /// blocks for [`RenderSession`] (nothing to probe). Braille is never
    /// chosen automatically — font support cannot be queried, only forced.
    #[default]
    Auto,
    /// Pure ASCII ramps + `- / | \` edge strokes. Renders everywhere.
    Ascii,
    /// Unicode blocks, box-drawing edges, half-block and quadrant fills.
    Unicode,
    /// Unicode plus braille edge/texture glyphs (U+2800–U+28FF) — only for
    /// fonts verified to cover them; never solid fills.
    Braille,
}

/// Resolve a `--font-table NAME|PATH` spec into a parsed coverage table.
/// Hidden: CLI/harness plumbing — embedders use
/// [`RenderSession::set_font_table`] / `PlayerBuilder::font_table`, which
/// wrap this and keep `auto_ascii_core::FontTable` out of the facade surface.
#[doc(hidden)]
pub fn load_font_table(spec: &str) -> Result<auto_ascii_core::FontTable, Error> {
    session::load_font_table(spec)
}

impl PaletteChoice {
    /// Resolve against probed terminal capabilities (the `Player` and
    /// `--sim` paths). Hidden: harness/CLI plumbing, not the embedding API.
    #[doc(hidden)]
    pub fn resolve_for_caps(self, caps: &auto_ascii_term::Caps) -> auto_ascii_core::GlyphTier {
        match self {
            PaletteChoice::Auto => pipeline::glyph_tier_from_caps(caps),
            _ => self.resolve_headless(),
        }
    }

    pub(crate) fn resolve_headless(self) -> auto_ascii_core::GlyphTier {
        match self {
            PaletteChoice::Auto | PaletteChoice::Unicode => auto_ascii_core::GlyphTier::UnicodeBlocks,
            PaletteChoice::Ascii => auto_ascii_core::GlyphTier::Ascii,
            PaletteChoice::Braille => auto_ascii_core::GlyphTier::BrailleVerified,
        }
    }
}
