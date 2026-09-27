//! Public terminal player and terminal-free rendering facade.

#[doc(hidden)]
pub mod pipeline;

#[doc(hidden)]
pub mod audio;

mod composition;
mod error;
mod session;

pub mod compose;

#[doc(hidden)]
pub mod deck;

pub mod timecode;

#[cfg(feature = "terminal")]
mod player;

#[cfg(feature = "terminal")]
#[doc(hidden)]
pub mod settings;

pub use composition::{
    Clip, ClipMark, ClipSpan, Composition, Located, Overlap, SCHEMA_VERSION, Span,
};
pub use error::Error;
pub use session::RenderSession;

#[cfg(feature = "terminal")]
pub use player::{Dial, MIN_FPS_CAP, Player, PlayerBuilder, RepaintMode, SCRUB_STEP_SECS, Stopped};

pub use auto_ascii_core::{Cell, Grid, Rgb};
pub use auto_ascii_core::Codec;
#[cfg(feature = "terminal")]
pub use auto_ascii_term::ColorTier;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaletteChoice {
    #[default]
    Auto,
    Ascii,
    Unicode,
    Braille,
}

#[doc(hidden)]
pub fn load_font_table(spec: &str) -> Result<auto_ascii_core::FontTable, Error> {
    session::load_font_table(spec)
}

impl PaletteChoice {
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
