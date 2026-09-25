//! **auto-ascii** — realtime ASCII-art video for terminals and embedders.
//!
//! An offline factory distills reference video into a resolution-independent
//! feature asset (`.ascii`); this crate maps that asset onto whatever cell
//! grid you have right now — glyph ramps, directional edge strokes,
//! highlights and half-blocks, with temporal hysteresis so nothing flickers
//! (PLAN §1). Assets never store glyphs; every glyph decision happens at
//! render time for *your* grid, *your* palette, *your* terminal.
//!
//! # 60-second quickstart
//!
//! Play an asset in the current terminal (probes capabilities, letterboxes,
//! handles resize, restores the terminal even on panic/Ctrl-C) — this one
//! needs the default `terminal` feature:
//!
// The fence is feature-gated so `cargo test --no-default-features --doc`
// (the pure-embedder configuration this same doc block advertises below)
// does not fail on a `Player` that is configured out. docs.rs builds with
// default features, so the rendered page always shows the runnable form.
#![cfg_attr(feature = "terminal", doc = "```no_run")]
#![cfg_attr(not(feature = "terminal"), doc = "```no_run,ignore")]
//! auto_ascii::Player::builder()
//!     .asset("intro.ascii")
//!     .looping(true)
//!     .build()?
//!     .run()?;
//! # Ok::<(), auto_ascii::Error>(())
//! ```
//!
//! Or skip the terminal entirely and drive your own output layer — a game
//! engine, a GUI, a test — with [`RenderSession`]:
//!
//! ```
//! use auto_ascii::RenderSession;
//! # // The doctest renders a synthetic test asset instead of "intro.ascii".
//! # let path = std::env::temp_dir().join("auto-ascii-doc-quickstart.ascii");
//! # let fixture = auto_ascii_eval::fixtures::Fixture::GradientMotion;
//! # std::fs::write(&path, auto_ascii_eval::fixtures::build_fixture(fixture)).unwrap();
//!
//! let mut session = RenderSession::open(&path)?;     // "intro.ascii"
//! let fps = session.fps();                           // drive your own clock
//! // your loop, your canvas — ask for any frame at any grid size:
//! let grid = session.render(/*frame*/ 0, /*cols*/ 120, /*rows*/ 40)?;
//! for row in 0..grid.rows() {
//!     for cell in grid.row(row) {
//!         let _ = (cell.glyph(), cell.fg); // char + auto_ascii::Rgb
//!     }
//! }
//! # assert_eq!((grid.cols(), grid.rows(), fps), (120, 40, 30.0));
//! # std::fs::remove_file(&path).unwrap();
//! # Ok::<(), auto_ascii::Error>(())
//! ```
//!
//! The crate's three runnable `examples/` are the same story at full size:
//! `simple-play` (12 lines, the whole player), `embedded-loop`
//! ([`RenderSession`] in a hand-rolled loop with a mid-run resize) and
//! `headless-dump` (frames to stdout as text, no terminal at all).
//!
//! # Cargo features
//!
//! | feature | default | provides |
//! |---|---|---|
//! | `bin` | **on** | the `auto-ascii-player` CLI binary (implies `terminal`) |
//! | `terminal` | via `bin` | `Player`/`PlayerBuilder` — the blocking terminal session |
//! | `compose` | **on** | reading composition `.toml` files (`toml` dep); the [`Composition`] type, its timeline and [`compose::export`] are always there |
//! | *(none)* | | [`RenderSession`] only: no crossterm, no clap — the pure-embedder build (`default-features = false`) |
//!
//! # What is deliberately NOT here
//!
//! Producing `.ascii` assets is the offline `auto-ascii-factory` binary's job
//! (see the repository README); the player never links video codecs. The
//! internal engine crates (`auto-ascii-core`, `auto-ascii-term`, `auto-ascii-format`) are
//! implementation details — the types you need ([`Grid`], [`Cell`],
//! [`Rgb`]) are re-exported here and nothing else is part of the public
//! contract.

// Every public item of the facade carries docs — enforced, not aspirational
// (M4 item C). `#[doc(hidden)]` items are exempt from this lint, which is
// exactly right: `pipeline` is the workspace harness contract, documented as
// such but not part of the embedding API.
#![deny(missing_docs)]

// The engine room: the exact frame pipeline shared by the CLI binary, the
// facade Player, RenderSession and the auto-ascii-factory eval harness (M2 item
// B: metrics must measure the real renderer). Hidden: it is the workspace
// harness contract, not the embedding API, and is exempt from facade semver.
#[doc(hidden)]
pub mod pipeline;

mod composition;
mod error;
mod session;

/// Flattening a [`Composition`] into one `.ascii` file (PLAN-M6-M8 §3).
/// Playback never needs this — compositions play virtually — but a single
/// file is sometimes what you want, and `auto-ascii cut` is an export of a
/// one-clip composition.
pub mod compose;

/// One decode pipeline per composition clip, created on demand. Hidden: the
/// workspace harness contract (the `--sim` path drives it), not the
/// embedding API — [`RenderSession`] and `Player` are the supported ways in.
#[doc(hidden)]
pub mod deck;

/// Timestamp parsing/formatting shared by every entry point that takes a
/// time (PLAN-M6-M8 §2) — `--seek`, `auto-ascii import --ss/--t`.
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

// The minimal embedding type set (M4 audit: what a simple project actually
// touches). Grid/Cell/Rgb are what RenderSession::render returns; ColorTier
// is the PlayerBuilder::tier argument. Everything else in the internal
// crates stays internal.
pub use auto_ascii_core::{Cell, Grid, Rgb};
/// Glyph codecs — how a cell's features become a glyph: `pixels` (the
/// default, picture-like) or `letters` (printable characters). Chosen with
/// [`RenderSession::set_codec`] or `PlayerBuilder::codec`, cycled live with
/// `/` in the player.
pub use auto_ascii_core::Codec;
#[cfg(feature = "terminal")]
pub use auto_ascii_term::ColorTier;

/// Glyph repertoire selection (PLAN §3.4 charset-tier axis of the 8 shipped
/// palettes). The color axis is chosen separately (probed terminal tier for
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

/// Resolve a `--font-table NAME|PATH` spec into a parsed coverage table
/// (PLAN §3.4, M5). Hidden: CLI/harness plumbing — embedders use
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

    /// Resolve with no terminal to probe (the [`RenderSession`] path):
    /// `Auto` means the Unicode-blocks tier.
    pub(crate) fn resolve_headless(self) -> auto_ascii_core::GlyphTier {
        match self {
            PaletteChoice::Auto | PaletteChoice::Unicode => auto_ascii_core::GlyphTier::UnicodeBlocks,
            PaletteChoice::Ascii => auto_ascii_core::GlyphTier::Ascii,
            PaletteChoice::Braille => auto_ascii_core::GlyphTier::BrailleVerified,
        }
    }
}
