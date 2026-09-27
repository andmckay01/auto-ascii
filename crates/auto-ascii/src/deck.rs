//! Resident clip pipelines, timeline frames and overlay dispatch.

use std::path::PathBuf;
use std::time::Instant;

use auto_ascii_core::{
    Cell, Codec, ColorDepth, ComposeParams, GlyphTier, Grid, MIN_COLS, MIN_ROWS,
};
use auto_ascii_format::AsciiReader;
use auto_ascii_term::{Backend, FrameStats};
use memmap2::Mmap;

use crate::composition::Located;
use crate::error::Error;
use crate::pipeline::{
    self, Drained, OverlayScale, ProgressContext, StageNs, UiRows, draw_dial_overlay,
    draw_enlarge_card, draw_hint_overlay, draw_info_overlay, draw_progress_overlay_clips,
};

fn add_stage(a: StageNs, b: StageNs) -> StageNs {
    StageNs {
        decode: a.decode + b.decode,
        resample: a.resample + b.resample,
        compose: a.compose + b.compose,
        present: a.present + b.present,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DeckConfig {
    pub cell_aspect: f64,
    pub repaint_full: bool,
    pub color: ColorDepth,
    pub glyph_tier: GlyphTier,
}

pub const MAX_LIVE_CLIPS: usize = 8;

struct Resident {
    player: pipeline::Player<'static>,
    #[expect(
        dead_code,
        reason = "owns the mapping `player` borrows; declared after it so it drops last"
    )]
    map: Mmap,
}

pub struct ClipDeck {
    residents: Vec<Option<Resident>>,
    paths: Vec<PathBuf>,
    dims: Vec<Option<(u16, u16)>>,
    used: Vec<u64>,
    activations: u64,
    active: Option<usize>,
    size: (u16, u16),
    cfg: DeckConfig,
    compose_params: Option<ComposeParams>,
    codec: Codec,
    progress_visible: bool,
    hint_visible: bool,
    info: Option<String>,
    paused: bool,
    dial: Option<(&'static str, u8, u8)>,
    progress_ctx: Option<ProgressContext>,
    layer_mask: bool,
    repaint_pending: bool,
    carried: StageNs,
    blank: Grid<Cell>,
    blank_ui: UiRows,
}

impl std::fmt::Debug for ClipDeck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipDeck")
            .field("clips", &self.paths.len())
            .field("open", &self.live_clips())
            .field("active", &self.active)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl ClipDeck {
    pub fn new(paths: Vec<PathBuf>, cfg: DeckConfig) -> ClipDeck {
        let n = paths.len();
        ClipDeck {
            residents: (0..n).map(|_| None).collect(),
            paths,
            dims: vec![None; n],
            used: vec![0; n],
            activations: 0,
            active: None,
            size: (0, 0),
            cfg,
            compose_params: None,
            codec: Codec::default(),
            progress_visible: false,
            hint_visible: false,
            info: None,
            paused: false,
            dial: None,
            progress_ctx: None,
            layer_mask: false,
            repaint_pending: false,
            carried: StageNs::default(),
            blank: Grid::new(0, 0),
            blank_ui: UiRows::NONE,
        }
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.size = (cols, rows);
    }

    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    fn activate(&mut self, idx: usize) -> Result<(), Error> {
        self.open(idx)?;
        self.activations += 1;
        self.used[idx] = self.activations;
        if self.active != Some(idx) {
            self.player(idx).reset_temporal_state();
            self.active = Some(idx);
            self.repaint_pending = true;
        }
        if self.dims[idx] != Some(self.size) {
            let (cols, rows) = self.size;
            self.player(idx).reflow_grid(cols, rows);
            self.dims[idx] = Some(self.size);
        }
        Ok(())
    }

    pub fn reset_active(&mut self) {
        if let Some(idx) = self.active {
            self.player(idx).reset_temporal_state();
        }
    }

    pub fn render_at(&mut self, located: Option<Located>) -> Result<(), Error> {
        let Some(loc) = located else {
            self.compose_gap();
            return Ok(());
        };
        self.activate(loc.clip_idx)?;
        self.apply_sticky(loc.clip_idx);
        self.player(loc.clip_idx).render_grid(loc.local_frame)
    }

    pub fn present_at<B: Backend>(
        &mut self,
        backend: &mut B,
        located: Option<Located>,
    ) -> Result<FrameStats, Error> {
        let Some(loc) = located else {
            self.compose_gap();
            if self.cfg.repaint_full || std::mem::take(&mut self.repaint_pending) {
                backend.invalidate();
            }
            let t = Instant::now();
            let stats = backend.present(&self.blank);
            self.carried.present += t.elapsed().as_nanos() as u64;
            return Ok(stats);
        };
        self.activate(loc.clip_idx)?;
        self.apply_sticky(loc.clip_idx);
        if std::mem::take(&mut self.repaint_pending) {
            backend.invalidate();
        }
        self.player(loc.clip_idx).render_present(backend, loc.local_frame)
    }

    pub fn showing(&self) -> &Grid<Cell> {
        match self.active.and_then(|idx| self.residents[idx].as_ref()) {
            Some(resident) => resident.player.grid(),
            None => &self.blank,
        }
    }

    pub fn ui_rows(&self) -> UiRows {
        match self.active.and_then(|idx| self.residents[idx].as_ref()) {
            Some(resident) => resident.player.ui_rows(),
            None => self.blank_ui,
        }
    }

    pub fn drain_events<B: Backend>(&mut self, backend: &mut B) -> Drained {
        let (drained, resize) = pipeline::drain_backend_events(backend);
        if let Some((cols, rows)) = resize {
            self.set_size(cols, rows);
            backend.resize(cols, rows);
            if let Some(idx) = self.active {
                self.player(idx).reflow_grid(cols, rows);
                self.dims[idx] = Some(self.size);
            }
            backend.invalidate();
        }
        if drained.requests_temporal_reset() {
            self.reset_active();
        }
        drained
    }

    pub fn set_progress_overlay(&mut self, visible: bool) {
        self.mark_hidden_during_gap(self.progress_visible, visible);
        self.progress_visible = visible;
    }

    pub fn set_hint_overlay(&mut self, visible: bool) {
        self.mark_hidden_during_gap(self.hint_visible, visible);
        self.hint_visible = visible;
    }

    pub fn set_info_overlay(&mut self, info: Option<&str>) {
        match info {
            None => {
                let was = self.info.take().is_some();
                self.mark_hidden_during_gap(was && self.hint_visible, false);
            }
            Some(new) => match &mut self.info {
                Some(cur) if cur == new => {}
                Some(cur) => new.clone_into(cur),
                slot => *slot = Some(new.to_owned()),
            },
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn set_dial_overlay(&mut self, dial: Option<(&'static str, u8, u8)>) {
        self.mark_hidden_during_gap(self.dial.is_some(), dial.is_some());
        self.dial = dial;
    }

    pub fn set_progress_context(&mut self, ctx: Option<ProgressContext>) {
        self.progress_ctx = ctx;
    }

    pub fn set_compose_params(&mut self, params: ComposeParams) {
        self.compose_params = Some(params);
        for player in self.players_mut() {
            player.set_compose_params(params);
        }
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    pub fn set_codec(&mut self, codec: Codec) {
        self.codec = codec;
        for player in self.players_mut() {
            player.set_codec(codec);
        }
    }

    pub fn set_glyph_tier(&mut self, glyph_tier: GlyphTier) {
        self.cfg.glyph_tier = glyph_tier;
        for (idx, resident) in self.residents.iter_mut().enumerate() {
            if let Some(Resident { player: p, .. }) = resident {
                p.set_glyph_tier_for_next_reflow(glyph_tier);
                p.reset_temporal_state();
                self.dims[idx] = None;
            }
        }
    }

    pub fn set_cell_aspect(&mut self, cell_aspect: f64) {
        self.cfg.cell_aspect = cell_aspect;
        for (idx, resident) in self.residents.iter_mut().enumerate() {
            if let Some(Resident { player: p, .. }) = resident {
                p.set_cell_aspect_for_next_reflow(cell_aspect);
                self.dims[idx] = None;
            }
        }
    }

    pub fn enable_layer_mask(&mut self) {
        self.layer_mask = true;
        for player in self.players_mut() {
            player.enable_layer_mask();
        }
    }

    pub fn layer_mask(&self) -> Option<&Grid<u8>> {
        self.active
            .and_then(|idx| self.residents[idx].as_ref())
            .and_then(|resident| resident.player.layer_mask())
    }

    pub fn stage(&self) -> StageNs {
        self.residents
            .iter()
            .flatten()
            .fold(self.carried, |acc, r| add_stage(acc, r.player.stage()))
    }

    pub fn live_clips(&self) -> usize {
        self.residents.iter().filter(|r| r.is_some()).count()
    }

    fn players_mut(&mut self) -> impl Iterator<Item = &mut pipeline::Player<'static>> {
        self.residents.iter_mut().flatten().map(|r| &mut r.player)
    }

    fn evict_one(&mut self, keep: usize) -> bool {
        let victim = self
            .residents
            .iter()
            .enumerate()
            .filter(|(i, resident)| *i != keep && resident.is_some())
            .min_by_key(|(i, _)| self.used[*i])
            .map(|(i, _)| i);
        let Some(i) = victim else { return false };
        if let Some(resident) = self.residents[i].take() {
            self.carried = add_stage(self.carried, resident.player.stage());
        }
        self.dims[i] = None;
        if self.active == Some(i) {
            self.active = None;
        }
        true
    }

    fn open(&mut self, idx: usize) -> Result<(), Error> {
        if self.residents.get(idx).is_none() {
            return Err(Error::Config(format!("no clip {idx} in this composition")));
        }
        if self.residents[idx].is_some() {
            return Ok(());
        }
        while self.live_clips() >= MAX_LIVE_CLIPS && self.evict_one(idx) {}
        let path = self.paths[idx].clone();
        let file = std::fs::File::open(&path)
            .map_err(|source| Error::Io { path: path.clone(), source })?;
        let map = unsafe { Mmap::map(&file) }
            .map_err(|source| Error::Io { path: path.clone(), source })?;
        let bytes: &'static [u8] =
            unsafe { std::slice::from_raw_parts(map.as_ptr(), map.len()) };
        let reader = AsciiReader::open(bytes)
            .map_err(|source| Error::Format { path: path.clone(), source })?;
        let mut player = pipeline::Player::new(
            reader,
            self.cfg.cell_aspect,
            self.cfg.repaint_full,
            self.cfg.color,
            self.cfg.glyph_tier,
        )?;
        if let Some(params) = self.compose_params {
            player.set_compose_params(params);
        }
        player.set_codec(self.codec);
        if self.layer_mask {
            player.enable_layer_mask();
        }
        self.residents[idx] = Some(Resident { player, map });
        Ok(())
    }

    fn player(&mut self, idx: usize) -> &mut pipeline::Player<'static> {
        &mut self.residents[idx].as_mut().expect("clip is open").player
    }

    fn apply_sticky(&mut self, idx: usize) {
        let (progress, hints, dial, ctx) =
            (self.progress_visible, self.hint_visible, self.dial, self.progress_ctx);
        let paused = self.paused;
        let info = self.info.as_deref();
        let player = &mut self.residents[idx].as_mut().expect("clip is open").player;
        player.set_progress_overlay(progress);
        player.set_hint_overlay(hints);
        player.set_info_overlay(info);
        player.set_dial_overlay(dial);
        player.set_progress_context(ctx);
        player.set_paused(paused);
    }

    fn mark_hidden_during_gap(&mut self, was: bool, now: bool) {
        if was && !now && self.active.is_none() {
            self.repaint_pending = true;
        }
    }

    fn compose_gap(&mut self) {
        if self.active.is_some() {
            self.active = None;
            self.repaint_pending = true;
        }
        let (cols, rows) = self.size;
        if (self.blank.cols(), self.blank.rows()) != (cols, rows) {
            self.blank.resize(cols, rows);
        }
        let tiny = cols < MIN_COLS || rows < MIN_ROWS;
        if tiny {
            draw_enlarge_card(&mut self.blank, self.codec.pad());
        } else {
            self.blank.fill(self.codec.pad());
        }
        let scale = OverlayScale::for_grid(cols, rows, self.cfg.glyph_tier);
        let mut ui = UiRows::NONE;
        if self.progress_visible
            && let Some(ctx) = self.progress_ctx
        {
            ui |= draw_progress_overlay_clips(
                &mut self.blank,
                ctx.frame,
                ctx.frame_count,
                ctx.fps(),
                ctx.clip,
                self.paused,
                scale,
            );
        }
        if let Some((label, value, max)) = self.dial {
            ui |= draw_dial_overlay(&mut self.blank, label, value, max, scale);
        }
        if self.hint_visible && !tiny {
            ui |= draw_hint_overlay(&mut self.blank, scale);
            if let Some(info) = &self.info {
                ui |= draw_info_overlay(&mut self.blank, info, scale);
            }
        }
        self.blank_ui = ui;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_ascii_core::{DEFAULT_CELL_ASPECT, GlyphTier};
    use auto_ascii_term::SimBackend;
    use auto_ascii_eval::fixtures::{Fixture, build_fixture};

    struct Clips(PathBuf, Vec<PathBuf>);

    impl Clips {
        fn new(tag: &str, n: usize) -> Clips {
            let dir = std::env::temp_dir()
                .join(format!("auto-ascii-deck-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            let bytes = build_fixture(Fixture::GradientMotion);
            let paths = (0..n)
                .map(|i| {
                    let path = dir.join(format!("clip-{i}.ascii"));
                    std::fs::write(&path, &bytes).expect("write clip");
                    path
                })
                .collect();
            Clips(dir, paths)
        }
    }

    impl Drop for Clips {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn at(idx: usize) -> Option<Located> {
        Some(Located { clip_idx: idx, local_frame: 7 })
    }

    fn headless() -> DeckConfig {
        DeckConfig {
            cell_aspect: DEFAULT_CELL_ASPECT,
            repaint_full: false,
            color: ColorDepth::True,
            glyph_tier: GlyphTier::UnicodeBlocks,
        }
    }

    #[test]
    fn live_clips_are_capped_and_the_oldest_goes_first() {
        let clips = Clips::new("evict", 12);
        let mut deck = ClipDeck::new(clips.1.clone(), headless());
        deck.set_size(80, 24);

        deck.render_at(at(0)).unwrap();
        let before = deck.showing().as_slice().to_vec();

        for idx in 1..clips.1.len() {
            deck.render_at(at(idx)).unwrap();
            assert!(
                deck.live_clips() <= MAX_LIVE_CLIPS,
                "clip {idx}: {} pipelines resident",
                deck.live_clips()
            );
        }
        assert_eq!(deck.live_clips(), MAX_LIVE_CLIPS, "the cap is reached, not exceeded");

        deck.render_at(at(0)).unwrap();
        assert_eq!(deck.showing().as_slice(), &before[..], "a re-opened clip is itself");
        assert!(deck.live_clips() <= MAX_LIVE_CLIPS);
    }

    #[test]
    fn stage_time_survives_eviction_and_counts_gaps() {
        let clips = Clips::new("stage", 12);
        let mut deck = ClipDeck::new(clips.1.clone(), headless());
        let mut backend = SimBackend::new(80, 24);
        deck.set_size(80, 24);

        let mut last = StageNs::default();
        for idx in 0..clips.1.len() {
            deck.present_at(&mut backend, at(idx)).unwrap();
            backend.take_output();
            let now = deck.stage();
            assert!(
                now.decode >= last.decode && now.compose >= last.compose,
                "clip {idx}: stage time went backwards over an eviction"
            );
            last = now;
        }
        assert!(deck.live_clips() <= MAX_LIVE_CLIPS, "clips were evicted during the walk");
        assert!(last.decode > 0 && last.compose > 0, "the walk did real work: {last:?}");

        let before = deck.stage().present;
        for _ in 0..8 {
            deck.present_at(&mut backend, None).unwrap();
            backend.take_output();
        }
        assert!(deck.stage().present > before, "gap presents are not being timed");
    }

    #[test]
    fn a_tiny_gap_keeps_the_progress_row_under_the_card() {
        let clips = Clips::new("tinygap", 1);
        let mut deck = ClipDeck::new(clips.1.clone(), headless());
        deck.set_size(20, 8);
        deck.set_progress_overlay(true);
        deck.set_hint_overlay(true);
        deck.set_progress_context(Some(ProgressContext {
            frame: 30,
            frame_count: 150,
            fps_num: 30,
            fps_den: 1,
            clip: None,
        }));

        deck.render_at(None).unwrap();
        let grid = deck.showing();
        let row = |r: u16| -> String { (0..20).map(|c| grid.get(c, r).glyph()).collect() };
        assert!(row(3).contains("AUTO-ASCII"), "the card is drawn: {:?}", row(3));
        let bottom = row(7);
        assert!(bottom.contains("0:01 / 0:05"), "the progress row survives: {bottom:?}");
        assert!(row(6).trim().is_empty(), "the hints row stays off the card: {:?}", row(6));
    }

    #[test]
    fn a_gap_scales_the_overlay_text_like_a_clip() {
        let clips = Clips::new("biggap", 1);
        for (cols, rows, big) in [(320u16, 90u16, true), (213, 58, false)] {
            let mut deck = ClipDeck::new(clips.1.clone(), headless());
            deck.set_size(cols, rows);
            deck.set_hint_overlay(true);
            deck.set_info_overlay(Some(" gap   codec: pixels "));
            deck.render_at(None).unwrap();
            let grid = deck.showing();
            let row = |r: u16| -> String { (0..cols).map(|c| grid.get(c, r).glyph()).collect() };
            if big {
                for r in rows - 9..rows - 3 {
                    assert!(row(r).chars().all(|c| " ▀▄█".contains(c)), "row {r}: {:?}", row(r));
                }
                assert!(row(rows - 5).contains('█'), "big hint text drawn");
                assert!(row(rows - 10).trim().is_empty(), "nothing above the info band");
            } else {
                assert!(row(rows - 2).contains("v controls"));
                assert!(row(rows - 3).ends_with(" 213x58 cells "), "{:?}", row(rows - 3));
            }
        }
    }
}
