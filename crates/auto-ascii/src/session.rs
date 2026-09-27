use std::path::Path;

use auto_ascii_core::{Cell, Codec, ColorDepth, FontTable, Grid};

use crate::composition::Composition;
use crate::deck::{ClipDeck, DeckConfig};
use crate::error::Error;
use crate::PaletteChoice;

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

pub struct RenderSession {
    deck: ClipDeck,
    comp: Composition,
    last_frame: Option<u32>,
    palette: PaletteChoice,
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
    pub fn open(path: impl AsRef<Path>) -> Result<RenderSession, Error> {
        RenderSession::from_composition(Composition::single(path.as_ref()))
    }

    #[cfg(feature = "compose")]
    pub fn open_composition(
        path: impl AsRef<Path>,
        library_dir: Option<&Path>,
    ) -> Result<RenderSession, Error> {
        let comp = Composition::from_toml_file(path, library_dir)?;
        RenderSession::from_composition(comp)
    }

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

    fn apply_glyph_tier(&mut self) {
        let mut tier = self.palette.resolve_headless();
        if let Some(t) = &self.font_table {
            tier = t.veto_tier(tier);
        }
        self.deck.set_glyph_tier(tier);
    }

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
        let located = self.comp.locate_frame(frame_idx);
        let backward = self.last_frame.is_some_and(|last| frame_idx < last);
        self.deck.set_size(cols, rows);
        if backward {
            self.deck.reset_active();
        }
        self.deck.render_at(located)?;
        self.last_frame = Some(frame_idx);
        Ok(self.deck.showing())
    }

    pub fn fps(&self) -> f64 {
        self.comp.fps()
    }

    pub fn frame_count(&self) -> u32 {
        self.comp.frame_count()
    }

    pub fn aspect(&self) -> f64 {
        self.comp.aspect()
    }

    pub fn set_palette(&mut self, palette: PaletteChoice) {
        self.palette = palette;
        self.apply_glyph_tier();
    }

    pub fn set_font_table(&mut self, name_or_path: Option<&str>) -> Result<(), Error> {
        self.font_table = match name_or_path {
            None => None,
            Some(spec) => Some(load_font_table(spec)?),
        };
        self.apply_glyph_tier();
        Ok(())
    }

    pub fn set_codec(&mut self, codec: Codec) {
        self.deck.set_codec(codec);
    }

    pub fn codec(&self) -> Codec {
        self.deck.codec()
    }

    pub fn set_cell_aspect(&mut self, cell_aspect: f64) -> Result<(), Error> {
        if !cell_aspect.is_finite() || cell_aspect <= 0.0 {
            return Err(Error::Config(format!(
                "cell aspect must be finite and > 0 (got {cell_aspect})"
            )));
        }
        self.deck.set_cell_aspect(cell_aspect);
        Ok(())
    }
}

fn headless_deck_config() -> DeckConfig {
    DeckConfig {
        cell_aspect: auto_ascii_core::DEFAULT_CELL_ASPECT,
        repaint_full: false,
        color: ColorDepth::True,
        glyph_tier: PaletteChoice::Auto.resolve_headless(),
    }
}
