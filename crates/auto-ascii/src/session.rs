//! [`RenderSession`] — the terminal-free embedding entry (M4 item A).
//!
//! For callers who own their event loop and their output layer: a game
//! engine, a GUI widget, a test harness, a web canvas. No terminal, no
//! clock, no signals — you ask for a frame at a grid size, you get back a
//! composed [`Grid`]`<`[`Cell`]`>` of glyphs + RGB colors to draw however
//! you like.

use std::path::Path;

use auto_ascii_core::{Cell, Codec, ColorDepth, FontTable, Grid};

use crate::composition::Composition;
use crate::deck::{ClipDeck, DeckConfig};
use crate::error::Error;
use crate::PaletteChoice;

/// Resolve a `--font-table NAME|PATH` spec (PLAN §3.4, M5): a committed
/// built-in table by name, else a path to a `auto-ascii-factory font-table`
/// TOML. Shared by [`RenderSession::set_font_table`] and
/// `PlayerBuilder::font_table`.
pub(crate) fn load_font_table(spec: &str) -> Result<FontTable, Error> {
    if let Some(t) = FontTable::builtin(spec) {
        return Ok(t.clone());
    }
    let path = Path::new(spec);
    if !path.is_file() {
        return Err(Error::Config(format!(
            "font table {spec:?} is neither a built-in table ({}) nor a file",
            auto_ascii_core::BUILTIN_FONT_TABLES.join(", ")
        )));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("font table {spec}: {e}")))?;
    FontTable::parse(&text).map_err(|e| Error::Config(format!("font table {spec}: {e}")))
}

/// A terminal-free render session over one ASCI asset.
///
/// Opens the asset once (memory-mapped, decoded lazily per frame) and turns
/// `(frame_idx, cols, rows)` into a composed cell grid:
///
/// ```
/// use auto_ascii::RenderSession;
/// # // The doctest renders a synthetic test asset instead of "intro.ascii".
/// # let path = std::env::temp_dir().join("auto-ascii-doc-session.ascii");
/// # let fixture = auto_ascii_eval::fixtures::Fixture::GradientMotion;
/// # std::fs::write(&path, auto_ascii_eval::fixtures::build_fixture(fixture)).unwrap();
///
/// let mut session = RenderSession::open(&path)?;     // "intro.ascii"
/// let grid = session.render(0, 120, 40)?;
/// for row in 0..grid.rows() {
///     let line: String = grid.row(row).iter().map(|c| c.glyph()).collect();
///     println!("{line}");
/// }
/// # assert!(grid.as_slice().iter().any(|c| c.glyph() != ' '), "asset rendered");
/// # std::fs::remove_file(&path).unwrap();
/// # Ok::<(), auto_ascii::Error>(())
/// ```
///
/// # Temporal-state semantics
///
/// The engine keeps per-cell temporal state (ramp-index hysteresis, edge
/// on/off memory, orientation bins — the flicker killers, PLAN §3.5) keyed
/// to the frame sequence you feed it:
///
/// * **Monotonic advance** (`frame_idx` ≥ the previous call's, skips
///   allowed) — full quality. This is normal playback, including
///   latest-frame-wins frame dropping.
/// * **Backward jump** (`frame_idx` < the previous call's) — the session
///   automatically resets all temporal state before rendering: a seek is a
///   temporal discontinuity, and stale state would ghost pre-seek edges
///   into the landing frame. The landing frame is rendered cold (exactly
///   what a fresh session would produce); hysteresis re-converges within a
///   frame or two.
/// * **Grid size change** — state is reallocated and reset for the new
///   grid, same as a terminal resize.
///
/// Rendering the *same* `frame_idx` twice returns the same grid without
/// re-decoding.
///
/// # Compositions
///
/// [`open_composition`](RenderSession::open_composition) (and
/// [`from_composition`](RenderSession::from_composition)) opens a stitch of
/// clips instead of one asset (PLAN-M6-M8 §3). Everything above is
/// unchanged: `frame_idx` counts frames on the COMPOSITION's timeline at
/// its own [`fps`](RenderSession::fps), each clip decodes from its own
/// mapping, a clip switch resets temporal state exactly like a backward
/// jump, and a gap between clips renders an all-blank grid.
pub struct RenderSession {
    /// The clip decks' render state. Declared first so its borrows die
    /// before anything it depends on.
    deck: ClipDeck,
    /// The timeline. A plain asset is a one-clip composition whose frame
    /// mapping is the identity (integer, exact) — one path, one source of
    /// truth for fps, aspect and frame count.
    comp: Composition,
    /// Frame index of the last successful render (backward-jump detection).
    last_frame: Option<u32>,
    /// The chosen repertoire, kept so the font-table veto can re-resolve it.
    palette: PaletteChoice,
    /// Optional §3.4 font coverage table: its repertoire vetoes `palette`.
    font_table: Option<FontTable>,
}

impl std::fmt::Debug for RenderSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderSession")
            .field("fps", &self.comp.fps())
            .field("aspect", &self.comp.aspect())
            .field("frame_count", &self.comp.frame_count())
            .field("clips", &self.deck.len())
            .field("last_frame", &self.last_frame)
            .finish_non_exhaustive()
    }
}

impl RenderSession {
    /// Open an ASCI asset for terminal-free rendering.
    ///
    /// The file is memory-mapped read-only and opened as a container
    /// (header, TRLR, chunk roll, frame index), so a corrupt or truncated
    /// asset fails HERE rather than at the first frame; frames are decoded
    /// on demand in [`render`](Self::render). Defaults:
    /// [`PaletteChoice::Auto`] (Unicode blocks — there is no terminal to
    /// probe) and cell aspect 2.0 (see
    /// [`set_cell_aspect`](Self::set_cell_aspect)).
    ///
    /// One asset is a one-clip composition internally, so this and
    /// [`from_composition`](Self::from_composition) are the same code path
    /// — the clip's frames map to composition frames one for one.
    pub fn open(path: impl AsRef<Path>) -> Result<RenderSession, Error> {
        RenderSession::from_composition(Composition::single(path.as_ref()))
    }

    /// Open a composition TOML for terminal-free rendering (PLAN-M6-M8 §3).
    ///
    /// `library_dir` is where a bare library NAME in the file resolves
    /// (`<library>/<name>.ascii`); pass
    /// [`Composition::default_library_dir`] for the usual
    /// `$AUTO_ASCII_HOME/library` rule, or `None` to accept only paths.
    ///
    /// # Errors
    /// [`Error::Config`] for a malformed file (every clip-level message
    /// names the clip index) and [`Error::Io`]/[`Error::Format`] when the
    /// file or one of its clips cannot be read.
    #[cfg(feature = "compose")]
    pub fn open_composition(
        path: impl AsRef<Path>,
        library_dir: Option<&Path>,
    ) -> Result<RenderSession, Error> {
        let comp = Composition::from_toml_file(path, library_dir)?;
        RenderSession::from_composition(comp)
    }

    /// Render a [`Composition`] built in memory — what
    /// [`open_composition`](RenderSession::open_composition) is on top of,
    /// and what a caller who assembled the clips itself wants.
    ///
    /// Resolving the composition (reading each clip's header) happens here
    /// if the caller has not done it already; the clips' decode pipelines
    /// are built lazily, as the timeline reaches them.
    pub fn from_composition(mut comp: Composition) -> Result<RenderSession, Error> {
        if !comp.is_resolved() {
            comp.resolve()?;
        }
        let paths = comp.clips().iter().map(|c| c.path.clone()).collect();
        Ok(RenderSession {
            deck: ClipDeck::new(paths, headless_deck_config()),
            comp,
            last_frame: None,
            palette: PaletteChoice::Auto,
            font_table: None,
        })
    }

    /// Re-resolve the glyph tier from `palette` through the font-table veto
    /// and reset the temporal state (a repertoire change makes every
    /// remembered ramp index stale, same as [`set_palette`](Self::set_palette)).
    fn apply_glyph_tier(&mut self) {
        let mut tier = self.palette.resolve_headless();
        if let Some(t) = &self.font_table {
            tier = t.veto_tier(tier);
        }
        // Every clip, present and future: the deck resets their temporal
        // state and drops their reflow (palettes are density-keyed and are
        // rebuilt there).
        self.deck.set_glyph_tier(tier);
    }

    /// Compose asset frame `frame_idx` into a `cols × rows` cell grid and
    /// return it. The grid is letterboxed to the asset's aspect (blank
    /// pads); below the 32×9 minimum it renders a centered "enlarge
    /// terminal" card. The returned borrow is valid until the next call.
    ///
    /// See the type-level docs for the temporal-state semantics of
    /// out-of-order `frame_idx` values.
    pub fn render(
        &mut self,
        frame_idx: u32,
        cols: u16,
        rows: u16,
    ) -> Result<&Grid<Cell>, Error> {
        if frame_idx >= self.comp.frame_count() {
            return Err(Error::Config(format!(
                "frame index {frame_idx} out of range (asset has {} frames)",
                self.comp.frame_count()
            )));
        }
        // The timeline says which clip is on top and which of ITS frames
        // that is — for one asset, frame f, exactly.
        let located = self.comp.locate_frame(frame_idx);
        let backward = self.last_frame.is_some_and(|last| frame_idx < last);
        self.deck.set_size(cols, rows);
        if backward {
            self.deck.reset_active(); // backward jump: no ghosting
        }
        // render_at reflows on a size change, switches clips (which resets
        // the one it fronts) and paints a gap black.
        self.deck.render_at(located)?;
        // Only now: a failed render must not move the cursor a later
        // backward-jump test reads.
        self.last_frame = Some(frame_idx);
        Ok(self.deck.showing())
    }

    /// Frames per second the asset was authored at — drive your clock with
    /// this (frame to show at time `t` is `(t * fps()) as u32`).
    pub fn fps(&self) -> f64 {
        self.comp.fps()
    }

    /// Total frames in the asset — or on the composition's timeline at
    /// [`fps`](RenderSession::fps) (always > 0).
    pub fn frame_count(&self) -> u32 {
        self.comp.frame_count()
    }

    /// The asset's intended picture aspect ratio, width / height, from the
    /// ASCI header's `aspect_num/den` (16:9 assets return ≈1.778; degenerate
    /// zero fields fall back to 16:9). This is the exact ratio the letterbox
    /// inside [`render`](Self::render) targets (M5 fix 2: the viewport
    /// tracks the asset's aspect, not a hard-coded 16:9); it is exposed for
    /// embedders sizing their own viewport.
    pub fn aspect(&self) -> f64 {
        self.comp.aspect()
    }

    /// Set the glyph repertoire for subsequent renders. [`PaletteChoice::Auto`]
    /// means Unicode blocks here (no terminal to probe). Changing the
    /// palette resets temporal state — remembered ramp indices are stale
    /// under a different ramp.
    pub fn set_palette(&mut self, palette: PaletteChoice) {
        self.palette = palette;
        self.apply_glyph_tier();
    }

    /// Assert which font the output medium renders with, by ink-coverage
    /// table (PLAN §3.4 `--font-table`): a built-in name — `conservative`,
    /// `dejavu-sans-mono`, `liberation-mono`, `ubuntu-mono`,
    /// `noto-sans-mono` — or a path to a `auto-ascii-factory font-table` TOML.
    /// `None` clears it.
    ///
    /// The table's recorded repertoire then *vetoes* the palette choice:
    /// a tier whose glyphs the font is missing degrades (braille →
    /// unicode → ascii) instead of rendering missing-glyph boxes. Like
    /// [`set_palette`](Self::set_palette), this resets temporal state.
    ///
    /// # Errors
    /// [`Error::Config`] when the spec is neither a built-in name nor a
    /// readable, parseable table file.
    pub fn set_font_table(&mut self, name_or_path: Option<&str>) -> Result<(), Error> {
        self.font_table = match name_or_path {
            None => None,
            Some(spec) => Some(load_font_table(spec)?),
        };
        self.apply_glyph_tier();
        Ok(())
    }

    /// Choose the glyph codec for subsequent renders — how each cell's
    /// features become a glyph (see [`Codec`]). The default,
    /// [`Codec::Pixels`], is the classic picture-like mapping; switching
    /// resets temporal state, like [`set_palette`](Self::set_palette).
    pub fn set_codec(&mut self, codec: Codec) {
        self.deck.set_codec(codec);
    }

    /// The glyph codec in force.
    pub fn codec(&self) -> Codec {
        self.deck.codec()
    }

    /// Set the cell aspect ratio `cell_h / cell_w` used by the letterbox
    /// math (PLAN §3.2). Terminal fonts are ≈2.0 (the default); pass 1.0
    /// if your cells are square (e.g. a texture atlas of square tiles).
    ///
    /// # Errors
    /// [`Error::Config`] unless `0 < cell_aspect` and it is finite.
    pub fn set_cell_aspect(&mut self, cell_aspect: f64) -> Result<(), Error> {
        if !cell_aspect.is_finite() || cell_aspect <= 0.0 {
            return Err(Error::Config(format!(
                "cell aspect must be finite and > 0 (got {cell_aspect})"
            )));
        }
        // Every clip, present and future; viewport math is recomputed at
        // the reflow the deck schedules for each of them.
        self.deck.set_cell_aspect(cell_aspect);
        Ok(())
    }
}

/// How [`RenderSession`] builds every clip pipeline: truecolor + Unicode
/// defaults (it always composes full RGB cells — the embedder owns any
/// quantization — and `Auto` has no `Caps` to consult, so it resolves to
/// the Unicode-blocks tier), cell aspect 2.0, no repaint mode (a terminal
/// concern, unused here).
fn headless_deck_config() -> DeckConfig {
    DeckConfig {
        cell_aspect: auto_ascii_core::DEFAULT_CELL_ASPECT,
        repaint_full: false,
        color: ColorDepth::True,
        glyph_tier: PaletteChoice::Auto.resolve_headless(),
    }
}
