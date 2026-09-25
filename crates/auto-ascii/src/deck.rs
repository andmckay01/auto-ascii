//! [`ClipDeck`] — one decode pipeline per composition clip, created on
//! first use (M8, PLAN-M6-M8 §3).
//!
//! A composition plays virtually: nothing is re-encoded, so an unbounded
//! stitch costs one mmap and one [`pipeline::Player`] per clip that is
//! actually reached. The deck owns those, forwards the render calls to
//! whichever clip is on top, and carries the presentation state
//! (overlays, dials, compose params) ACROSS a clip switch — the run loop
//! sets it once and the next clip inherits it.
//!
//! Three callers share this: [`crate::RenderSession`] (no backend),
//! `crate::Player`'s run loop, and the `auto-ascii-player --sim` harness.
//! Single assets go through it too, as a one-clip composition, so there is
//! exactly one clip-switch implementation in the tree.

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
    self, Drained, OverlayScale, ProgressContext, StageNs, draw_dial_overlay,
    draw_enlarge_card, draw_hint_overlay, draw_info_overlay, draw_progress_overlay_clips,
};

/// Add two per-stage accumulators.
fn add_stage(a: StageNs, b: StageNs) -> StageNs {
    StageNs {
        decode: a.decode + b.decode,
        resample: a.resample + b.resample,
        compose: a.compose + b.compose,
        present: a.present + b.present,
    }
}

/// How every clip player in a deck is built (the §3.4 selection inputs the
/// caller already resolved: probed caps for the terminal player, headless
/// defaults for [`crate::RenderSession`]).
#[derive(Clone, Copy, Debug)]
pub struct DeckConfig {
    /// §3.2 cell aspect `cell_h_px / cell_w_px`.
    pub cell_aspect: f64,
    /// Invalidate before every present (PLAN §3.1 `RepaintMode::Full`).
    pub repaint_full: bool,
    /// Color depth of the output medium.
    pub color: ColorDepth,
    /// Glyph repertoire (already through any font-table veto).
    pub glyph_tier: GlyphTier,
}

/// How many clip pipelines stay live at once. Each one holds decode
/// buffers sized for its base resolution plus the asset's FIDX — order
/// megabytes for a 480×270 six-plane clip — and PLAN-M6-M8 §0.4 promises a
/// composition can stitch an UNBOUNDED number of clips, so the deck keeps
/// the most recently fronted few and drops the rest. Eight covers any
/// plausible cut rate at the player's ~2 s scrub granularity while keeping
/// the resident cost flat; re-opening an evicted clip is the ordinary lazy
/// path, and it comes back cold — which is the temporal reset a return to
/// a clip wants anyway.
pub const MAX_LIVE_CLIPS: usize = 8;

/// One [`pipeline::Player`] per clip, opened on demand (module docs).
pub struct ClipDeck {
    /// Each borrows `maps[i]` (see SAFETY in [`ClipDeck::open`]). Declared
    /// BEFORE `maps` so declaration-order drop kills every borrow before
    /// the mapping it points into.
    players: Vec<Option<pipeline::Player<'static>>>,
    /// The mapping behind each opened player. Cleared only by
    /// [`ClipDeck::evict_one`], and only AFTER the player borrowing it.
    maps: Vec<Option<Mmap>>,
    paths: Vec<PathBuf>,
    /// Grid dims each player was last reflowed to (`None` = never).
    dims: Vec<Option<(u16, u16)>>,
    /// `activations` value when each clip was last fronted — the LRU key.
    used: Vec<u64>,
    /// Monotonic activation counter (never wraps in any real session: one
    /// tick per fronted frame at 60 fps is 9.7 billion years to overflow).
    activations: u64,
    /// The clip on top right now; `None` during a gap and before the first
    /// activation.
    active: Option<usize>,
    size: (u16, u16),
    cfg: DeckConfig,
    /// Presentation state re-applied to whichever player is fronted, so a
    /// clip switch never drops an overlay or a turned dial.
    compose_params: Option<ComposeParams>,
    /// The glyph codec every clip composes with (the `/` key).
    codec: Codec,
    progress_visible: bool,
    hint_visible: bool,
    /// The controls overlay's info row (see `pipeline::draw_info_overlay`).
    info: Option<String>,
    /// M6 pause: how the progress row PRINTS while the run loop holds the
    /// picture — re-applied on every clip, like the overlays themselves.
    paused: bool,
    dial: Option<(&'static str, u8, u8)>,
    progress_ctx: Option<ProgressContext>,
    layer_mask: bool,
    /// A clip switch, a gap transition or an overlay hide during a gap: the
    /// diff baseline describes a picture that is no longer there, so the
    /// next present must repaint in full.
    repaint_pending: bool,
    /// Stage time that no live player can account for any more: whatever an
    /// evicted clip had spent, plus every gap present. Without this a
    /// `--sim` run over a long composition would report a stage budget that
    /// shrinks as clips are dropped.
    carried: StageNs,
    /// The all-blank grid a gap frame presents (PLAN-M6-M8 §3: "a gap is
    /// black"), with the overlays drawn on top.
    blank: Grid<Cell>,
}

impl std::fmt::Debug for ClipDeck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipDeck")
            .field("clips", &self.paths.len())
            .field("open", &self.players.iter().filter(|p| p.is_some()).count())
            .field("active", &self.active)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl ClipDeck {
    /// A deck over `paths` (a composition's clips, in file order). Nothing
    /// is opened until [`activate`](ClipDeck::activate).
    pub fn new(paths: Vec<PathBuf>, cfg: DeckConfig) -> ClipDeck {
        let n = paths.len();
        ClipDeck {
            players: (0..n).map(|_| None).collect(),
            maps: (0..n).map(|_| None).collect(),
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
        }
    }

    /// Clips in the deck.
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the deck has no clips (never, for a resolved composition).
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Adopt a new grid size. Players are reflowed lazily, when they are
    /// next fronted — a composition may hold clips that never play.
    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.size = (cols, rows);
    }

    /// The grid size in force. [`drain_events`](ClipDeck::drain_events)
    /// adopts resizes itself, so a run loop watching for "did anything
    /// change?" reads this rather than the event it never sees.
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// Open clip `idx` if needed, front it, and reflow it to the current
    /// size. A switch resets that clip's temporal state and schedules a
    /// full repaint. Called for you by [`render_at`](ClipDeck::render_at)
    /// and [`present_at`](ClipDeck::present_at).
    ///
    /// Each clip owns its own [`pipeline::Player`], so hysteresis, NORM
    /// levels and decode buffers can never leak from one clip into the
    /// next — the switch itself starts fresh by construction. The explicit
    /// reset is for the other direction: coming BACK to a clip whose
    /// remembered ramp indices and edge memory describe the frame it
    /// showed before we left, which is a temporal discontinuity exactly
    /// like a seek (PLAN §3.5).
    fn activate(&mut self, idx: usize) -> Result<(), Error> {
        self.open(idx)?;
        self.activations += 1;
        self.used[idx] = self.activations; // most recently used: never evicted next
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

    /// Reset the fronted clip's temporal state (the backward-jump rule —
    /// a seek must not ghost pre-seek edges into the landing frame).
    pub fn reset_active(&mut self) {
        if let Some(idx) = self.active {
            self.player(idx).reset_temporal_state();
        }
    }

    /// Compose one composition frame — the clip [`Composition::locate_frame`]
    /// put on top at its own local frame, or a gap — without a backend
    /// ([`crate::RenderSession`]). Read the result with
    /// [`showing`](ClipDeck::showing).
    ///
    /// This is the whole time→picture dispatch, in one place: every caller
    /// hands it what `locate_frame` said and nothing decides anything twice.
    ///
    /// [`Composition::locate_frame`]: crate::Composition::locate_frame
    pub fn render_at(&mut self, located: Option<Located>) -> Result<(), Error> {
        let Some(loc) = located else {
            self.compose_gap(); // a gap is black (PLAN-M6-M8 §3)
            return Ok(());
        };
        self.activate(loc.clip_idx)?;
        self.apply_sticky(loc.clip_idx);
        self.player(loc.clip_idx).render_grid(loc.local_frame)
    }

    /// [`render_at`](ClipDeck::render_at) and present it. Gap frames are
    /// presented from the deck's own blank grid; a clip frame goes through
    /// its pipeline's present, so the diff/damage accounting is untouched.
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
            // Timed like any other present, so a composition's stage budget
            // still adds up over its gaps.
            let t = Instant::now();
            let stats = backend.present(&self.blank);
            self.carried.present += t.elapsed().as_nanos() as u64;
            return Ok(stats);
        };
        self.activate(loc.clip_idx)?;
        self.apply_sticky(loc.clip_idx);
        if std::mem::take(&mut self.repaint_pending) {
            backend.invalidate(); // clip switch / gap edge: repaint the baseline
        }
        self.player(loc.clip_idx).render_present(backend, loc.local_frame)
    }

    /// The grid on screen after the last [`render_at`](ClipDeck::render_at)
    /// or [`present_at`](ClipDeck::present_at): the fronted clip's, or the
    /// blank gap grid.
    pub fn showing(&self) -> &Grid<Cell> {
        match self.active.and_then(|idx| self.players[idx].as_ref()) {
            Some(player) => player.grid(),
            None => &self.blank,
        }
    }

    /// Drain the backend's events (PLAN §3.6 step 1) for the whole deck: a
    /// resize is adopted by the deck (and by the fronted player now, the
    /// rest when they are next fronted) and a seek resets the fronted
    /// clip's temporal state — the same contract as
    /// [`pipeline::Player::drain_events`], which is also what a gap frame
    /// needs when no clip is fronted at all.
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
        if drained.jump_digit.is_some() || drained.seek_steps != 0 {
            self.reset_active();
        }
        drained
    }

    /// Show/hide the bottom-row progress overlay on whichever clip plays.
    pub fn set_progress_overlay(&mut self, visible: bool) {
        self.mark_hidden_during_gap(self.progress_visible, visible);
        self.progress_visible = visible;
    }

    /// Show/hide the key-hints row (M6).
    pub fn set_hint_overlay(&mut self, visible: bool) {
        self.mark_hidden_during_gap(self.hint_visible, visible);
        self.hint_visible = visible;
    }

    /// Set (or clear) the info row drawn above the hints while they show —
    /// clip name, codec, settings state. Copies only when the text changes.
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

    /// Report playback as frozen in the progress row (M6 pause): `|` bar
    /// head, ` PAUSED ` where the percentage goes. The deck holds no
    /// transport state of its own — this is the run loop's flag, forwarded.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Show/clear the live-dial readout (M6).
    pub fn set_dial_overlay(&mut self, dial: Option<(&'static str, u8, u8)>) {
        self.mark_hidden_during_gap(self.dial.is_some(), dial.is_some());
        self.dial = dial;
    }

    /// Report the COMPOSITION's position in the progress row instead of the
    /// fronted clip's own (M8). `None` keeps the M6 reading.
    pub fn set_progress_context(&mut self, ctx: Option<ProgressContext>) {
        self.progress_ctx = ctx;
    }

    /// Retune the compositor on every clip, present and future (the live
    /// dials — a renderer change; each player resets its hysteresis memory
    /// when its params actually move, see `pipeline::Player::set_compose_params`).
    pub fn set_compose_params(&mut self, params: ComposeParams) {
        self.compose_params = Some(params);
        for player in self.players.iter_mut().flatten() {
            player.set_compose_params(params);
        }
    }

    /// The glyph codec in force.
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Switch the glyph codec on every clip, present and future (the `/`
    /// key; each player resets its temporal state when the codec actually
    /// changes, see `pipeline::Player::set_codec`).
    pub fn set_codec(&mut self, codec: Codec) {
        self.codec = codec;
        for player in self.players.iter_mut().flatten() {
            player.set_codec(codec);
        }
    }

    /// Re-key palette selection on every clip (takes effect at the next
    /// reflow, so every player's dims are dropped) and reset temporal state
    /// — remembered ramp indices are stale under a different ramp.
    pub fn set_glyph_tier(&mut self, glyph_tier: GlyphTier) {
        self.cfg.glyph_tier = glyph_tier;
        for (idx, player) in self.players.iter_mut().enumerate() {
            if let Some(p) = player {
                p.set_glyph_tier(glyph_tier);
                p.reset_temporal_state();
                self.dims[idx] = None;
            }
        }
    }

    /// Override the §3.2 cell aspect on every clip (next reflow).
    pub fn set_cell_aspect(&mut self, cell_aspect: f64) {
        self.cfg.cell_aspect = cell_aspect;
        for (idx, player) in self.players.iter_mut().enumerate() {
            if let Some(p) = player {
                p.set_cell_aspect(cell_aspect);
                self.dims[idx] = None;
            }
        }
    }

    /// Collect the winning-layer mask on every clip (the eval/`--sim` view).
    pub fn enable_layer_mask(&mut self) {
        self.layer_mask = true;
        for player in self.players.iter_mut().flatten() {
            player.enable_layer_mask();
        }
    }

    /// The fronted clip's layer mask for the last rendered frame.
    pub fn layer_mask(&self) -> Option<&Grid<u8>> {
        self.active
            .and_then(|idx| self.players[idx].as_ref())
            .and_then(pipeline::Player::layer_mask)
    }

    /// Per-stage wall times for the whole composition (`--sim`'s JSON
    /// line): every live clip, plus what evicted clips and gap presents
    /// already spent. Monotonic — dropping a clip never loses its time.
    pub fn stage(&self) -> StageNs {
        self.players.iter().flatten().fold(self.carried, |acc, p| add_stage(acc, p.stage()))
    }

    /// Clip pipelines resident right now — at most [`MAX_LIVE_CLIPS`].
    pub fn live_clips(&self) -> usize {
        self.players.iter().filter(|p| p.is_some()).count()
    }

    /// Drop the least recently fronted clip (never `keep`) so a new one can
    /// open within [`MAX_LIVE_CLIPS`]. `false` when there was nothing to
    /// drop.
    ///
    /// Order matters and is the whole safety argument: `players[i]` is
    /// cleared FIRST, so the borrow into `maps[i]` is gone before the
    /// mapping is unmapped one line later.
    fn evict_one(&mut self, keep: usize) -> bool {
        let victim = self
            .players
            .iter()
            .enumerate()
            .filter(|(i, player)| *i != keep && player.is_some())
            .min_by_key(|(i, _)| self.used[*i])
            .map(|(i, _)| i);
        let Some(i) = victim else { return false };
        // Keep its stage time before it goes — `stage()` reports the whole
        // composition, not just what is resident.
        if let Some(player) = &self.players[i] {
            self.carried = add_stage(self.carried, player.stage());
        }
        self.players[i] = None; // the borrow dies here...
        self.maps[i] = None; // ...before the mapping it pointed into
        self.dims[i] = None; // a re-opened player has never been reflowed
        if self.active == Some(i) {
            self.active = None; // whatever fronts next starts cold
        }
        true
    }

    /// Open clip `idx`: map the file, build its pipeline player, inherit
    /// the deck's sticky render state.
    fn open(&mut self, idx: usize) -> Result<(), Error> {
        if self.players.get(idx).is_none() {
            return Err(Error::Config(format!("no clip {idx} in this composition")));
        }
        if self.players[idx].is_some() {
            return Ok(());
        }
        while self.live_clips() >= MAX_LIVE_CLIPS && self.evict_one(idx) {}
        let path = self.paths[idx].clone();
        let file = std::fs::File::open(&path)
            .map_err(|source| Error::Io { path: path.clone(), source })?;
        // SAFETY: read-only private map of a file we never mutate through
        // this mapping; the standard mmap'd-reader assumption that the
        // asset is not truncated mid-use (the same contract RenderSession
        // and the player binary take).
        let map = unsafe { Mmap::map(&file) }
            .map_err(|source| Error::Io { path: path.clone(), source })?;
        // SAFETY of the 'static lifetime: `bytes` points into the OS
        // mapping owned by `map`, whose address is stable for the life of
        // that object (moving the `Mmap` handle — into `self.maps` below,
        // or with the Vec if it reallocates — moves a pointer, not the
        // mapping). The only consumer is the player stored at the same
        // index, and `players` is declared BEFORE `maps`, so Rust's
        // declaration-order drop guarantees every borrow dies before the
        // mapping is unmapped. The only other way a slot dies is
        // `evict_one`, which clears the player first for exactly the same
        // reason. The fake 'static never escapes this module: no method
        // here hands out the player itself.
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
        self.maps[idx] = Some(map);
        self.players[idx] = Some(player);
        Ok(())
    }

    /// The player at `idx` (opened by [`open`](ClipDeck::open) first).
    fn player(&mut self, idx: usize) -> &mut pipeline::Player<'static> {
        self.players[idx].as_mut().expect("clip is open")
    }

    /// Push the deck's presentation state onto the fronted player. Every
    /// setter is edge-aware, so re-applying an unchanged value is free and
    /// the hide→`invalidate()` contract still fires exactly once.
    fn apply_sticky(&mut self, idx: usize) {
        let (progress, hints, dial, ctx) =
            (self.progress_visible, self.hint_visible, self.dial, self.progress_ctx);
        let paused = self.paused;
        let info = self.info.as_deref();
        let player = self.players[idx].as_mut().expect("clip is open");
        player.set_progress_overlay(progress);
        player.set_hint_overlay(hints);
        player.set_info_overlay(info);
        player.set_dial_overlay(dial);
        player.set_progress_context(ctx);
        player.set_paused(paused);
    }

    /// An overlay that hides while a gap is on screen has no clip player to
    /// record the edge, so the deck repaints on its behalf.
    fn mark_hidden_during_gap(&mut self, was: bool, now: bool) {
        if was && !now && self.active.is_none() {
            self.repaint_pending = true;
        }
    }

    /// Paint the gap frame: black, the "enlarge terminal" card when the
    /// grid is below the §3.2 minimum, then the overlays — the same
    /// presentation rules `render_grid` applies to a real frame.
    fn compose_gap(&mut self) {
        if self.active.is_some() {
            self.active = None; // leaving a clip: the next one starts cold
            self.repaint_pending = true;
        }
        let (cols, rows) = self.size;
        if (self.blank.cols(), self.blank.rows()) != (cols, rows) {
            self.blank.resize(cols, rows);
        }
        // Below the §3.2 minimum a gap says what a picture frame would say
        // — and, exactly like `render_grid` over the card, the transient
        // rows still go on top: losing the scrub bar is how you get stuck
        // in a gap on a small terminal with nothing to steer by.
        let tiny = cols < MIN_COLS || rows < MIN_ROWS;
        if tiny {
            draw_enlarge_card(&mut self.blank);
        } else {
            self.blank.fill(Cell::BLANK);
        }
        let scale = OverlayScale::for_grid(cols, rows, self.cfg.glyph_tier);
        if self.progress_visible
            && let Some(ctx) = self.progress_ctx
        {
            draw_progress_overlay_clips(
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
            draw_dial_overlay(&mut self.blank, label, value, max, scale);
        }
        // The hints row is the one thing the card outranks: `rows-2` is
        // where its second line sits on a 4-row screen (the same gate
        // `render_grid` applies through `vp.is_some()`).
        if self.hint_visible && !tiny {
            draw_hint_overlay(&mut self.blank, scale);
            if let Some(info) = &self.info {
                draw_info_overlay(&mut self.blank, info, scale);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_ascii_core::{DEFAULT_CELL_ASPECT, GlyphTier};
    use auto_ascii_term::SimBackend;
    use auto_ascii_eval::fixtures::{Fixture, build_fixture};

    /// Self-cleaning temp folder of `n` identical fixture clips (no
    /// tempfile dep — pinned workspace dep set). Identical bytes on
    /// purpose: eviction is about how many pipelines are resident, and
    /// identical clips make "same frame after a round trip" exact.
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

    /// Frame 7 of clip `idx` — what `Composition::locate_frame` would hand
    /// the deck for a clip that is on top.
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

    /// PLAN-M6-M8 §0.4 promises an unbounded stitch, so the deck must not
    /// hold a pipeline per clip: the most recently fronted MAX_LIVE_CLIPS
    /// stay, the rest are dropped, and coming back re-opens cold.
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

        // Clip 0 was evicted eleven activations ago; returning re-opens it
        // through the ordinary lazy path and renders the same frame.
        deck.render_at(at(0)).unwrap();
        assert_eq!(deck.showing().as_slice(), &before[..], "a re-opened clip is itself");
        assert!(deck.live_clips() <= MAX_LIVE_CLIPS);
    }

    /// `--sim` reports one stage budget for the whole composition, so the
    /// deck's total must survive eviction (an evicted clip's time is kept)
    /// and count gap presents (nothing else times those).
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

        // A gap present is timed too, or a composition's budget would
        // quietly stop adding up across its gaps.
        let before = deck.stage().present;
        for _ in 0..8 {
            deck.present_at(&mut backend, None).unwrap();
            backend.take_output();
        }
        assert!(deck.stage().present > before, "gap presents are not being timed");
    }

    /// A gap below the §3.2 minimum shows the enlarge card AND keeps the
    /// transient rows — being stuck in a gap with no scrub bar is exactly
    /// when a small terminal needs one. The hints row is the exception:
    /// `rows-2` is the card's own second line.
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

    /// A gap draws the overlays at the clip's scale — the same
    /// `OverlayScale::for_grid` a playing clip uses — so the controls do not
    /// shrink back to one-cell text between clips on a zoomed-out terminal.
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
                // Hints band rows-6..rows-3, info band above it, in blocks.
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
