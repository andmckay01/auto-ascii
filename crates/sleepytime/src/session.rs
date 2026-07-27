//! [`RenderSession`] — the terminal-free embedding entry (M4 item A).
//!
//! For callers who own their event loop and their output layer: a game
//! engine, a GUI widget, a test harness, a web canvas. No terminal, no
//! clock, no signals — you ask for a frame at a grid size, you get back a
//! composed [`Grid`]`<`[`Cell`]`>` of glyphs + RGB colors to draw however
//! you like.

use std::path::Path;

use memmap2::Mmap;
use slpy_core::{Cell, ColorDepth, FontTable, Grid};
use slpy_format::SlpyReader;

use crate::error::Error;
use crate::{PaletteChoice, pipeline};

/// Resolve a `--font-table NAME|PATH` spec (PLAN §3.4, M5): a committed
/// built-in table by name, else a path to a `sleepy-factory font-table`
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
            slpy_core::BUILTIN_FONT_TABLES.join(", ")
        )));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("font table {spec}: {e}")))?;
    FontTable::parse(&text).map_err(|e| Error::Config(format!("font table {spec}: {e}")))
}

/// A terminal-free render session over one SLPY asset.
///
/// Opens the asset once (memory-mapped, decoded lazily per frame) and turns
/// `(frame_idx, cols, rows)` into a composed cell grid:
///
/// ```
/// use sleepytime::RenderSession;
/// # // The doctest renders a synthetic test asset instead of "intro.slpy".
/// # let path = std::env::temp_dir().join("sleepytime-doc-session.slpy");
/// # let fixture = slpy_eval::fixtures::Fixture::GradientMotion;
/// # std::fs::write(&path, slpy_eval::fixtures::build_fixture(fixture)).unwrap();
///
/// let mut session = RenderSession::open(&path)?;     // "intro.slpy"
/// let grid = session.render(0, 120, 40)?;
/// for row in 0..grid.rows() {
///     let line: String = grid.row(row).iter().map(|c| c.glyph()).collect();
///     println!("{line}");
/// }
/// # assert!(grid.as_slice().iter().any(|c| c.glyph() != ' '), "asset rendered");
/// # std::fs::remove_file(&path).unwrap();
/// # Ok::<(), sleepytime::Error>(())
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
pub struct RenderSession {
    /// Borrows the mapping owned by `map` (see SAFETY in [`open`]). Declared
    /// before `map` so it is dropped first — the borrow never dangles.
    inner: pipeline::Player<'static>,
    /// The memory-mapped asset backing `inner`. Never touched directly; it
    /// exists to keep the mapping alive and is dropped last (field order).
    _map: Mmap,
    fps: f64,
    aspect: f64,
    /// Grid dims of the last reflow; `None` forces a reflow on next render.
    dims: Option<(u16, u16)>,
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
            .field("fps", &self.fps)
            .field("aspect", &self.aspect)
            .field("frame_count", &self.inner.frame_count())
            .field("dims", &self.dims)
            .field("last_frame", &self.last_frame)
            .finish_non_exhaustive()
    }
}

impl RenderSession {
    /// Open an SLPY asset for terminal-free rendering.
    ///
    /// The file is memory-mapped read-only and validated (header, chunk
    /// structure); frames are decoded on demand in [`render`](Self::render).
    /// Defaults: [`PaletteChoice::Auto`] (Unicode blocks — there is no
    /// terminal to probe) and cell aspect 2.0 (see
    /// [`set_cell_aspect`](Self::set_cell_aspect)).
    pub fn open(path: impl AsRef<Path>) -> Result<RenderSession, Error> {
        let path = path.as_ref();
        let file = std::fs::File::open(path)
            .map_err(|source| Error::Io { path: path.into(), source })?;
        // SAFETY: read-only private map of a file we never mutate through
        // this mapping; the standard mmap'd-reader assumption that the asset
        // is not truncated mid-use (same contract as the player binary).
        let map = unsafe { Mmap::map(&file) }
            .map_err(|source| Error::Io { path: path.into(), source })?;

        // SAFETY of the 'static lifetime: `bytes` points into the OS mapping
        // owned by `map`, whose address is stable for the life of `map`
        // (moving the `Mmap` handle moves a pointer, not the mapping). The
        // only consumer is `inner`, stored in the same struct and declared
        // BEFORE `map`, so Rust's declaration-order drop guarantees `inner`
        // (and every slice it holds) dies before the mapping is unmapped.
        // The fake 'static never escapes: every accessor reborrows at &self.
        let bytes: &'static [u8] =
            unsafe { std::slice::from_raw_parts(map.as_ptr(), map.len()) };

        let reader = SlpyReader::open(bytes)
            .map_err(|source| Error::Format { path: path.into(), source })?;
        let header = reader.header();
        if header.fps_num == 0 || header.fps_den == 0 {
            return Err(Error::Asset("corrupt header: fps_num or fps_den == 0"));
        }
        let fps = f64::from(header.fps_num) / f64::from(header.fps_den);
        // Degenerate (zero) header aspect fields fall back to 16:9 — the
        // same normalization pipeline::Player::new applies for the viewport,
        // so aspect() always reports the ratio render() letterboxes to.
        let aspect = if header.aspect_num == 0 || header.aspect_den == 0 {
            16.0 / 9.0
        } else {
            f64::from(header.aspect_num) / f64::from(header.aspect_den)
        };
        // Truecolor + Unicode defaults: RenderSession always composes full
        // RGB cells (the embedder owns quantization, if any); Auto has no
        // Caps to consult, so it resolves to the Unicode-blocks tier.
        let inner = pipeline::Player::new(
            reader,
            slpy_core::DEFAULT_CELL_ASPECT,
            false, // repaint mode is a terminal concern; unused here
            ColorDepth::True,
            PaletteChoice::Auto.resolve_headless(),
        )?;
        Ok(RenderSession {
            inner,
            _map: map,
            fps,
            aspect,
            dims: None,
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
        self.inner.set_glyph_tier(tier);
        self.inner.reset_temporal_state();
        self.dims = None; // palettes are rebuilt at reflow (density-keyed)
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
        if frame_idx >= self.inner.frame_count() {
            return Err(Error::Config(format!(
                "frame index {frame_idx} out of range (asset has {} frames)",
                self.inner.frame_count()
            )));
        }
        if self.dims != Some((cols, rows)) {
            self.inner.reflow_grid(cols, rows);
            self.dims = Some((cols, rows));
        }
        if self.last_frame.is_some_and(|last| frame_idx < last) {
            self.inner.reset_temporal_state(); // backward jump: no ghosting
        }
        self.inner.render_grid(frame_idx)?;
        self.last_frame = Some(frame_idx);
        Ok(self.inner.grid())
    }

    /// Frames per second the asset was authored at — drive your clock with
    /// this (frame to show at time `t` is `(t * fps()) as u32`).
    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// Total frames in the asset (always > 0).
    pub fn frame_count(&self) -> u32 {
        self.inner.frame_count()
    }

    /// The asset's intended picture aspect ratio, width / height, from the
    /// SLPY header's `aspect_num/den` (16:9 assets return ≈1.778; degenerate
    /// zero fields fall back to 16:9). This is the exact ratio the letterbox
    /// inside [`render`](Self::render) targets (M5 fix 2: the viewport
    /// tracks the asset's aspect, not a hard-coded 16:9); it is exposed for
    /// embedders sizing their own viewport.
    pub fn aspect(&self) -> f64 {
        self.aspect
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
    /// `noto-sans-mono` — or a path to a `sleepy-factory font-table` TOML.
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
        self.inner.set_cell_aspect(cell_aspect);
        self.dims = None; // viewport math is recomputed at reflow
        Ok(())
    }
}
