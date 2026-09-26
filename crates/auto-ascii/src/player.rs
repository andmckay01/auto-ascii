use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use std::fmt::Write as _;

use auto_ascii_core::{Codec, ComposeParams};
use auto_ascii_term::{AnsiBackend, Backend, ColorTier, ProbeOptions, probe_caps};

use crate::composition::Composition;
use crate::deck::{ClipDeck, DeckConfig};
use crate::error::Error;
use crate::pipeline::ProgressContext;
use crate::settings::VideoSettings;
use crate::{PaletteChoice, pipeline};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepaintMode {
    #[default]
    Full,
    Diff,
}

pub const MIN_FPS_CAP: f64 = 1.0;

pub use crate::pipeline::SCRUB_STEP_SECS;

const DIAL_OVERLAY_HIDE_AFTER: Duration = Duration::from_millis(2500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dial {
    ShadowLift,
    EdgeStrength,
    Hysteresis,
}

impl Dial {
    pub const ALL: [Dial; 3] = [Dial::ShadowLift, Dial::EdgeStrength, Dial::Hysteresis];

    pub fn label(self) -> &'static str {
        match self {
            Dial::ShadowLift => "shadow lift",
            Dial::EdgeStrength => "edge strength",
            Dial::Hysteresis => "hysteresis",
        }
    }

    pub fn step(self) -> i32 {
        match self {
            Dial::ShadowLift => 16,
            Dial::EdgeStrength => 4,
            Dial::Hysteresis => 16,
        }
    }

    pub fn max(self) -> u8 {
        match self {
            Dial::ShadowLift | Dial::Hysteresis => 255,
            Dial::EdgeStrength => 128,
        }
    }

    pub fn get(self, p: &ComposeParams) -> u8 {
        self.display_value(p)
    }

    pub(crate) fn display_value(self, p: &ComposeParams) -> u8 {
        match self {
            Dial::ShadowLift => p.shadow_lift,
            Dial::EdgeStrength => self.max().saturating_sub(p.edge_t_on),
            Dial::Hysteresis => p.idx_hyst_q8,
        }
    }

    pub fn param_key(self) -> &'static str {
        match self {
            Dial::ShadowLift => "shadow_lift",
            Dial::EdgeStrength => "edge_t_on",
            Dial::Hysteresis => "idx_hyst_q8",
        }
    }

    pub fn param(self, p: &ComposeParams) -> u8 {
        self.raw_param_value(p)
    }

    pub(crate) fn raw_param_value(self, p: &ComposeParams) -> u8 {
        match self {
            Dial::ShadowLift => p.shadow_lift,
            Dial::EdgeStrength => p.edge_t_on,
            Dial::Hysteresis => p.idx_hyst_q8,
        }
    }

    pub fn set_param(self, p: &mut ComposeParams, v: u8) {
        match self {
            Dial::ShadowLift => p.shadow_lift = v,
            Dial::EdgeStrength => p.edge_t_on = v.min(self.max()),
            Dial::Hysteresis => p.idx_hyst_q8 = v,
        }
    }

    pub fn turn(self, p: &mut ComposeParams, steps: i32) {
        let (cur, step, max) =
            (i32::from(self.display_value(p)), self.step(), i32::from(self.max()));
        let from = turn_origin_treating_the_top_as_a_stop(cur, step, max);
        let next = (from + steps * step).clamp(0, max) as u8;
        match self {
            Dial::ShadowLift => p.shadow_lift = next,
            Dial::EdgeStrength => p.edge_t_on = self.max() - next,
            Dial::Hysteresis => p.idx_hyst_q8 = next,
        }
    }
}

fn turn_origin_treating_the_top_as_a_stop(cur: i32, step: i32, max: i32) -> i32 {
    if cur == max { first_detent_at_or_above(max, step) } else { cur }
}

fn first_detent_at_or_above(value: i32, step: i32) -> i32 {
    (value + step - 1) / step * step
}

fn dial_after_cycle(idx: usize, presses: u32, readout_up: bool) -> usize {
    let advance = presses.saturating_sub(u32::from(!readout_up)) as usize;
    (idx + advance) % Dial::ALL.len()
}

#[derive(Debug)]
struct LiveSettings {
    forced: Option<Codec>,
    session: Option<Codec>,
    session_compose: Option<ComposeParams>,
    fronted: Option<usize>,
    clip_name: String,
    saved: Option<VideoSettings>,
    note: Option<&'static str>,
    problems: Vec<String>,
    compose: ComposeParams,
    codec: Codec,
}

impl LiveSettings {
    fn new(forced: Option<Codec>) -> LiveSettings {
        LiveSettings {
            forced,
            session: None,
            session_compose: None,
            fronted: None,
            clip_name: String::new(),
            saved: None,
            note: None,
            problems: Vec::new(),
            compose: ComposeParams::default(),
            codec: forced.unwrap_or_default(),
        }
    }

    fn front(&mut self, idx: usize, path: &Path) -> bool {
        if self.fronted == Some(idx) {
            return false;
        }
        self.fronted = Some(idx);
        self.clip_name = path
            .file_stem()
            .map_or_else(|| path.display().to_string(), |s| s.to_string_lossy().into_owned());
        (self.saved, self.note) = match VideoSettings::load(path) {
            Ok(s) => (s, None),
            Err(e) => {
                self.problem(e.to_string());
                (None, Some("unreadable"))
            }
        };
        let start = self.saved.unwrap_or_default();
        self.compose = self.session_compose.unwrap_or(start.compose);
        self.codec = self.session.or(self.forced).unwrap_or(start.codec);
        true
    }

    fn cycle(&mut self, presses: u32) {
        for _ in 0..presses {
            self.codec = self.codec.next();
        }
        self.session = Some(self.codec);
    }

    fn turn(&mut self, dial: Dial, steps: i32) {
        dial.turn(&mut self.compose, steps);
        self.session_compose = Some(self.compose);
    }

    fn save(&mut self, path: &Path) {
        let current = self.current();
        match current.save(path) {
            Ok(_) => (self.saved, self.note) = (Some(current), None),
            Err(e) => {
                self.problem(e.to_string());
                self.note = Some("save failed");
            }
        }
    }

    fn problem(&mut self, msg: String) {
        if !self.problems.contains(&msg) {
            self.problems.push(msg);
        }
    }

    fn current(&self) -> VideoSettings {
        VideoSettings { compose: self.compose, codec: self.codec }
    }

    fn status(&self) -> &'static str {
        let current = self.current().persisted();
        self.note.unwrap_or(match self.saved.map(|s| s.persisted()) {
            Some(s) if s == current => "saved",
            None if current == VideoSettings::default() => "default",
            _ => "s to save",
        })
    }

    fn write_info(&self, out: &mut String) {
        out.clear();
        let _ = write!(
            out,
            " {}   codec: {}   settings: {} ",
            self.clip_name,
            self.codec.name(),
            self.status()
        );
    }
}

const OVERLAY_HIDE_AFTER: Duration = Duration::from_millis(1000);

#[derive(Debug)]
struct Transport {
    base_frame: u64,
    clock: Instant,
    paused: bool,
}

impl Transport {
    fn new(base_frame: u64, now: Instant) -> Transport {
        Transport { base_frame, clock: now, paused: false }
    }

    fn elapsed_secs(&self, now: Instant) -> f64 {
        if self.paused { 0.0 } else { now.duration_since(self.clock).as_secs_f64() }
    }

    fn seek_to(&mut self, frame: u64, now: Instant) {
        self.base_frame = frame;
        self.clock = now;
    }

    fn toggle_pause(&mut self, frozen: u64, now: Instant) {
        if self.paused {
            self.clock = now;
            self.paused = false;
        } else {
            self.base_frame = frozen;
            self.paused = true;
        }
    }
}

fn freeze_target(presented: Option<u64>, clock_frame: u64) -> u64 {
    presented.unwrap_or(clock_frame)
}

#[derive(Debug, Default)]
struct ProgressTimer {
    until: Option<Instant>,
}

impl ProgressTimer {
    fn visible(&mut self, now: Instant, restart: bool, paused: bool) -> bool {
        if paused {
            self.until = None;
            return true;
        }
        if restart {
            self.until = Some(now + OVERLAY_HIDE_AFTER);
        } else if self.until.is_some_and(|t| now >= t) {
            self.until = None;
        }
        self.until.is_some()
    }
}

#[derive(Debug, Default)]
struct RepaintGate {
    skipped: u64,
}

impl RepaintGate {
    fn should_paint(&mut self, paused: bool, dirty: bool) -> bool {
        if paused && !dirty {
            self.skipped += 1;
            return false;
        }
        self.skipped = 0;
        true
    }
}

const HINT_STARTUP_SHOW_FOR: Duration = Duration::from_millis(3000);

#[derive(Debug)]
struct HintState {
    startup_until: Instant,
    sticky: bool,
}

impl HintState {
    fn new(now: Instant) -> HintState {
        HintState { startup_until: now + HINT_STARTUP_SHOW_FOR, sticky: false }
    }

    fn visible(&mut self, now: Instant, toggle: bool, overlays_up: bool) -> bool {
        let in_startup = now < self.startup_until;
        if !toggle {
            return self.sticky || overlays_up || in_startup;
        }
        self.startup_until = now;
        self.sticky = if self.sticky {
            false
        } else {
            !in_startup
        };
        self.sticky || overlays_up
    }
}

#[derive(Debug, Default)]
#[must_use = "call .build() to open the asset"]
pub struct PlayerBuilder {
    asset: Option<PathBuf>,
    composition: Option<PathBuf>,
    palette: PaletteChoice,
    tier: Option<ColorTier>,
    repaint: RepaintMode,
    fps_cap: Option<f64>,
    looping: bool,
    cell_aspect: Option<f64>,
    seek_secs: Option<f64>,
    duration_secs: Option<f64>,
    no_query: bool,
    no_quirks: bool,
    no_cache: bool,
    font_table: Option<String>,
    codec: Option<Codec>,
}

impl PlayerBuilder {
    pub fn asset(mut self, path: impl Into<PathBuf>) -> Self {
        self.asset = Some(path.into());
        self
    }

    pub fn composition(mut self, path: impl Into<PathBuf>) -> Self {
        self.composition = Some(path.into());
        self
    }

    pub fn palette(mut self, palette: PaletteChoice) -> Self {
        self.palette = palette;
        self
    }

    pub fn tier(mut self, tier: Option<ColorTier>) -> Self {
        self.tier = tier;
        self
    }

    pub fn repaint(mut self, repaint: RepaintMode) -> Self {
        self.repaint = repaint;
        self
    }

    pub fn fps_cap(mut self, fps: f64) -> Self {
        self.fps_cap = Some(fps);
        self
    }

    pub fn looping(mut self, looping: bool) -> Self {
        self.looping = looping;
        self
    }

    pub fn cell_aspect(mut self, cell_aspect: f64) -> Self {
        self.cell_aspect = Some(cell_aspect);
        self
    }

    pub fn seek_secs(mut self, secs: f64) -> Self {
        self.seek_secs = Some(secs);
        self
    }

    pub fn duration_secs(mut self, secs: f64) -> Self {
        self.duration_secs = Some(secs);
        self
    }

    pub fn no_query(mut self, no_query: bool) -> Self {
        self.no_query = no_query;
        self
    }

    pub fn no_cache(mut self, no_cache: bool) -> Self {
        self.no_cache = no_cache;
        self
    }

    pub fn no_quirks(mut self, no_quirks: bool) -> Self {
        self.no_quirks = no_quirks;
        self
    }

    pub fn font_table(mut self, name_or_path: impl Into<String>) -> Self {
        self.font_table = Some(name_or_path.into());
        self
    }

    pub fn codec(mut self, codec: Codec) -> Self {
        self.codec = Some(codec);
        self
    }

    pub fn build(self) -> Result<Player, Error> {
        let path = match (&self.asset, &self.composition) {
            (Some(_), Some(_)) => {
                return Err(Error::Config(
                    "set either an asset or a composition, not both".into(),
                ));
            }
            (Some(asset), None) => asset.clone(),
            (None, Some(comp)) => comp.clone(),
            (None, None) => {
                return Err(Error::Config(
                    "no asset path set (PlayerBuilder::asset is required)".into(),
                ));
            }
        };
        if let Some(cap) = self.fps_cap
            && (cap.is_nan() || cap < MIN_FPS_CAP)
        {
            return Err(Error::Config(format!(
                "fps cap must be >= {MIN_FPS_CAP} (got {cap}): the pacing loop sleeps \
                 1/cap seconds between presents"
            )));
        }
        if let Some(a) = self.cell_aspect
            && (!a.is_finite() || a <= 0.0)
        {
            return Err(Error::Config(format!("cell aspect must be finite and > 0 (got {a})")));
        }
        let mut comp = match &self.composition {
            None => Composition::single(&path),
            Some(file) => Composition::from_toml_file(
                file,
                Composition::default_library_dir().as_deref(),
            )?,
        };
        comp.resolve()?;
        let start_frame = match self.seek_secs {
            Some(secs) => comp.frame_at_secs(secs)?,
            None => 0,
        };
        let font_table = match &self.font_table {
            None => None,
            Some(spec) => Some(crate::session::load_font_table(spec)?),
        };
        Ok(Player { comp, cfg: self, start_frame, font_table })
    }
}

#[derive(Debug)]
pub struct Player {
    comp: Composition,
    cfg: PlayerBuilder,
    start_frame: u32,
    font_table: Option<auto_ascii_core::FontTable>,
}

fn resolve_cell_aspect(flag: Option<f64>, cell_px: Option<(u16, u16)>) -> f64 {
    if let Some(a) = flag {
        return a;
    }
    match cell_px {
        Some((w, h)) if w > 0 && h > 0 => f64::from(h) / f64::from(w),
        _ => auto_ascii_core::DEFAULT_CELL_ASPECT,
    }
}

impl Player {
    pub fn builder() -> PlayerBuilder {
        PlayerBuilder::default()
    }

    pub fn run(self) -> Result<(), Error> {
        let probe_opts = ProbeOptions {
            forced_tier: self.cfg.tier,
            no_query: self.cfg.no_query || self.cfg.tier.is_some(),
            no_quirks: self.cfg.no_quirks,
            no_cache: self.cfg.no_cache,
            ..ProbeOptions::default()
        };
        let caps = probe_caps(&probe_opts);

        let mut backend = AnsiBackend::new(caps).map_err(Error::Terminal)?;
        let aspect = resolve_cell_aspect(self.cfg.cell_aspect, backend.caps().cell_px);
        let depth = pipeline::color_depth(backend.caps().color);
        let mut glyphs = self.cfg.palette.resolve_for_caps(backend.caps());
        if let Some(t) = &self.font_table {
            glyphs = t.veto_tier(glyphs);
        }
        let paths = self.comp.clips().iter().map(|c| c.path.clone()).collect();
        let mut deck = ClipDeck::new(
            paths,
            DeckConfig {
                cell_aspect: aspect,
                repaint_full: self.cfg.repaint == RepaintMode::Full,
                color: depth,
                glyph_tier: glyphs,
            },
        );
        let (cols, rows) = backend.caps().cells;
        backend.resize(cols, rows);
        deck.set_size(cols, rows);

        let asset_fps = self.comp.fps();
        let present_fps = match self.cfg.fps_cap {
            Some(cap) => cap.min(asset_fps),
            None => asset_fps,
        };
        let tick = Duration::from_secs_f64(1.0 / present_fps);
        let frame_count = u64::from(self.comp.frame_count());
        let clip_count = self.comp.clips().len();
        let (fps_num, fps_den) = self.comp.fps_ratio();
        let t0 = Instant::now();
        let mut transport = Transport::new(u64::from(self.start_frame), t0);
        let mut next_tick = t0;
        let mut progress = ProgressTimer::default();
        let mut dial_idx: usize = 0;
        let mut dial_until: Option<Instant> = None;
        let mut live = LiveSettings::new(self.cfg.codec);
        deck.set_codec(live.codec);
        let mut note_until: Option<Instant> = None;
        let mut info = String::new();
        let mut hints = HintState::new(t0);
        let mut gate = RepaintGate::default();
        let mut presented: Option<u64> = None;
        let (mut was_progress, mut was_hints) = (false, false);
        let mut was_dial = false;
        let mut was_size = deck.size();

        loop {
            let drained = deck.drain_events(&mut backend);
            if drained.quit {
                break;
            }
            let mut sought = false;
            let mut resumed = false;
            if drained.toggle_pause {
                let now = Instant::now();
                let elapsed = transport.elapsed_secs(now);
                let clock_frame = self.comp.frame_after(transport.base_frame, elapsed);
                let clock_frame =
                    if self.cfg.looping { clock_frame % frame_count } else { clock_frame };
                transport.toggle_pause(freeze_target(presented, clock_frame), now);
                resumed = !transport.paused;
                deck.set_paused(transport.paused);
            }
            if let Some(d) = drained.jump_digit {
                transport.seek_to(frame_count * u64::from(d) / 10, Instant::now());
                sought = true;
            }
            if drained.seek_steps != 0 {
                let now = Instant::now();
                let pos = self.comp.frame_after(transport.base_frame, transport.elapsed_secs(now));
                let pos = if self.cfg.looping { pos % frame_count } else { pos };
                let delta = (f64::from(drained.seek_steps) * SCRUB_STEP_SECS * asset_fps) as i64;
                let landing = (pos.min(frame_count - 1) as i64 + delta)
                    .clamp(0, frame_count as i64 - 1) as u64;
                transport.seek_to(landing, now);
                sought = true;
            }
            if drained.dial_cycle > 0 || drained.dial_delta != 0 {
                if drained.dial_cycle > 0 {
                    dial_idx = dial_after_cycle(dial_idx, drained.dial_cycle, dial_until.is_some());
                }
                let dial = Dial::ALL[dial_idx];
                if drained.dial_delta != 0 {
                    live.turn(dial, drained.dial_delta);
                    deck.set_compose_params(live.compose);
                }
                deck.set_dial_overlay(Some((dial.label(), dial.display_value(&live.compose), dial.max())));
                dial_until = Some(Instant::now() + DIAL_OVERLAY_HIDE_AFTER);
            } else if dial_until.is_some_and(|t| Instant::now() >= t) {
                deck.set_dial_overlay(None);
                dial_until = None;
            }
            if drained.codec_cycle > 0 {
                live.cycle(drained.codec_cycle);
                deck.set_codec(live.codec);
            }
            if drained.save
                && let Some(idx) = live.fronted
            {
                live.save(&self.comp.clips()[idx].path);
            }
            let resized = deck.size() != was_size;
            if drained.codec_cycle > 0 || drained.save || resized {
                note_until = Some(Instant::now() + DIAL_OVERLAY_HIDE_AFTER);
            } else if note_until.is_some_and(|t| Instant::now() >= t) {
                note_until = None;
            }
            let show_progress =
                progress.visible(Instant::now(), sought || resumed, transport.paused);
            deck.set_progress_overlay(show_progress);
            let overlays_up = show_progress || dial_until.is_some() || note_until.is_some();
            let show_hints = hints.visible(Instant::now(), drained.toggle_hints, overlays_up);
            deck.set_hint_overlay(show_hints);
            if let Some(dur) = self.cfg.duration_secs
                && t0.elapsed().as_secs_f64() >= dur
            {
                break;
            }
            let mut target = self
                .comp
                .frame_after(transport.base_frame, transport.elapsed_secs(Instant::now()));
            if target >= frame_count {
                if self.cfg.looping {
                    target %= frame_count;
                } else {
                    break;
                }
            }
            let located = self.comp.locate_frame(target as u32);
            if let Some(l) = located
                && live.front(l.clip_idx, &self.comp.clips()[l.clip_idx].path)
            {
                deck.set_compose_params(live.compose);
                deck.set_codec(live.codec);
                if dial_until.is_some() {
                    let dial = Dial::ALL[dial_idx];
                    deck.set_dial_overlay(Some((dial.label(), dial.display_value(&live.compose), dial.max())));
                }
            }
            live.write_info(&mut info);
            deck.set_info_overlay(Some(&info));
            if self.comp.is_stitch() {
                deck.set_progress_context(Some(ProgressContext {
                    frame: target as u32,
                    frame_count: self.comp.frame_count(),
                    fps_num,
                    fps_den,
                    clip: located.map(|l| (l.clip_idx + 1, clip_count)),
                }));
            }
            let dial_up = dial_until.is_some();
            let dirty = drained.toggle_pause
                || sought
                || drained.dial_cycle > 0
                || drained.dial_delta != 0
                || drained.codec_cycle > 0
                || drained.save
                || drained.toggle_hints
                || show_progress != was_progress
                || show_hints != was_hints
                || dial_up != was_dial
                || deck.size() != was_size;
            (was_progress, was_hints, was_dial, was_size) =
                (show_progress, show_hints, dial_up, deck.size());

            if gate.should_paint(transport.paused, dirty) {
                deck.present_at(&mut backend, located)?;
                presented = Some(target);
            }

            next_tick += tick;
            let now = Instant::now();
            if next_tick > now {
                std::thread::sleep(next_tick - now);
            } else {
                next_tick = now;
            }
        }
        backend.shutdown();
        for problem in &live.problems {
            eprintln!("auto-ascii-player: settings: {problem}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_aspect_resolution() {
        assert_eq!(resolve_cell_aspect(Some(1.5), Some((10, 20))), 1.5);
        assert_eq!(resolve_cell_aspect(None, Some((10, 21))), 2.1);
        assert_eq!(resolve_cell_aspect(None, Some((0, 20))), 2.0);
        assert_eq!(resolve_cell_aspect(None, None), 2.0);
    }

    #[test]
    fn build_validates_before_touching_the_terminal() {
        let e = Player::builder().build().unwrap_err();
        assert!(matches!(e, Error::Config(_)), "missing asset: {e}");
        let e = Player::builder().asset("/no/such/file.ascii").build().unwrap_err();
        assert!(matches!(e, Error::Io { .. }), "missing file: {e}");
        let e = Player::builder()
            .asset("/no/such/file.ascii")
            .fps_cap(0.0)
            .build()
            .unwrap_err();
        assert!(matches!(e, Error::Config(_)), "fps cap checked first: {e}");
        let e = Player::builder()
            .asset("/no/such/file.ascii")
            .fps_cap(f64::NAN)
            .build()
            .unwrap_err();
        assert!(matches!(e, Error::Config(_)), "NaN fps cap: {e}");
        let e = Player::builder()
            .asset("/no/such/file.ascii")
            .cell_aspect(f64::NAN)
            .build()
            .unwrap_err();
        assert!(matches!(e, Error::Config(_)), "NaN cell aspect: {e}");
    }

    #[test]
    fn fps_cap_floor_is_enforced_at_build() {
        for bad in [1e-9, f64::MIN_POSITIVE, 0.5, 0.999] {
            let e = Player::builder()
                .asset("/no/such/file.ascii")
                .fps_cap(bad)
                .build()
                .unwrap_err();
            assert!(matches!(e, Error::Config(_)), "fps_cap({bad}) must be Config: {e}");
            assert!(e.to_string().contains(">= 1"), "message names the floor: {e}");
        }
        let e = Player::builder()
            .asset("/no/such/file.ascii")
            .fps_cap(MIN_FPS_CAP)
            .build()
            .unwrap_err();
        assert!(matches!(e, Error::Io { .. }), "cap at the floor is valid: {e}");
    }

    #[test]
    fn build_resolves_font_table_before_the_terminal() {
        let mut path = std::env::temp_dir();
        path.push(format!("auto-ascii-player-font-{}.ascii", std::process::id()));
        std::fs::write(
            &path,
            auto_ascii_eval::fixtures::build_fixture(auto_ascii_eval::fixtures::Fixture::GradientMotion),
        )
        .unwrap();

        let e = Player::builder()
            .asset(&path)
            .font_table("comic-sans")
            .build()
            .unwrap_err();
        assert!(matches!(e, Error::Config(_)), "{e}");
        assert!(e.to_string().contains("ubuntu-mono"), "error lists built-ins: {e}");

        let p = Player::builder()
            .asset(&path)
            .font_table("liberation-mono")
            .build()
            .expect("built-in table name resolves at build()");
        assert_eq!(p.font_table.as_ref().map(|t| t.name()), Some("liberation-mono"));
        assert_eq!(
            p.font_table.unwrap().veto_tier(auto_ascii_core::GlyphTier::UnicodeBlocks),
            auto_ascii_core::GlyphTier::Ascii
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn build_rejects_non_ascii_files() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let e = Player::builder().asset(manifest).build().unwrap_err();
        assert!(matches!(e, Error::Format { .. }), "{e}");
    }

    #[test]
    fn build_rejects_a_truncated_asset() {
        let mut path = std::env::temp_dir();
        path.push(format!("auto-ascii-player-truncated-{}.ascii", std::process::id()));
        let bytes = auto_ascii_eval::fixtures::build_fixture(
            auto_ascii_eval::fixtures::Fixture::GradientMotion,
        );
        std::fs::write(&path, &bytes[..bytes.len() * 3 / 4]).unwrap();

        let e = Player::builder().asset(&path).build().unwrap_err();
        assert!(matches!(e, Error::Format { .. }), "{e}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hint_row_shows_at_start_up_then_rides_the_overlays() {
        let t0 = Instant::now();
        let mut hints = HintState::new(t0);

        assert!(hints.visible(t0, false, false), "hints show at start-up");
        let last = t0 + HINT_STARTUP_SHOW_FOR - Duration::from_millis(1);
        assert!(hints.visible(last, false, false), "still up inside the window");
        assert!(!hints.visible(t0 + HINT_STARTUP_SHOW_FOR, false, false), "window lapses");

        let late = t0 + HINT_STARTUP_SHOW_FOR + Duration::from_secs(60);
        assert!(hints.visible(late, false, true), "rides with a visible overlay");
        assert!(!hints.visible(late, false, false), "and leaves with it");

        assert!(hints.visible(late, true, false), "v pins the row open");
        assert!(hints.visible(late, false, false), "and it stays pinned");
        assert!(!hints.visible(late, true, false), "a second v unpins it");
    }

    #[test]
    fn hint_press_dismisses_the_start_up_row_and_pins_otherwise() {
        let t0 = Instant::now();
        let mut hints = HintState::new(t0);
        assert!(hints.visible(t0, false, false), "the start-up row is up");
        assert!(!hints.visible(t0, true, false), "v during start-up dismisses it");
        let tick = t0 + Duration::from_millis(1);
        assert!(!hints.visible(tick, false, false), "and the window does not bring it back");
        assert!(hints.visible(tick, true, false), "the next v summons it again");

        let mut hints = HintState::new(t0);
        let late = t0 + HINT_STARTUP_SHOW_FOR + Duration::from_secs(60);
        assert!(hints.visible(late, false, true), "an overlay pulls the row up");
        assert!(hints.visible(late, true, true), "v pins it while the bar is up");
        assert!(hints.visible(late, false, false), "and it stays after the bar goes");
        assert!(!hints.visible(late, true, false), "the next press unpins");
    }

    #[test]
    fn hints_can_be_pinned_while_the_progress_row_is_up() {
        let t0 = Instant::now();
        let mut hints = HintState::new(t0);
        let late = t0 + HINT_STARTUP_SHOW_FOR + Duration::from_secs(60);
        assert!(hints.visible(late, true, true), "v pins during a pause");
        assert!(hints.visible(late, false, true), "and stays pinned");
        assert!(hints.visible(late, true, true), "a second v unpins");
        assert!(!hints.visible(late, false, false), "gone once the bar goes");
        assert!(hints.visible(late, true, false), "v pins with nothing up");
        assert!(hints.visible(late, false, false), "and it stays");
    }

    #[test]
    fn pause_freezes_the_frame_and_resume_continues_from_it() {
        let mut path = std::env::temp_dir();
        path.push(format!("auto-ascii-pause-{}.ascii", std::process::id()));
        std::fs::write(
            &path,
            auto_ascii_eval::fixtures::build_fixture(
                auto_ascii_eval::fixtures::Fixture::GradientMotion,
            ),
        )
        .unwrap();
        let mut comp = Composition::single(&path);
        comp.resolve().expect("the fixture is a valid one-clip composition");
        let fps = comp.fps();
        let target =
            |tr: &Transport, now: Instant| comp.frame_after(tr.base_frame, tr.elapsed_secs(now));

        let t0 = Instant::now();
        let mut tr = Transport::new(0, t0);

        let at_pause = t0 + Duration::from_secs(1);
        let frozen = target(&tr, at_pause);
        assert_eq!(frozen, fps as u64, "one second of a {fps} fps asset");
        tr.toggle_pause(frozen, at_pause);
        assert!(tr.paused);

        let at_resume = at_pause + Duration::from_secs(2);
        assert_eq!(target(&tr, at_resume), frozen, "a pause freezes asset time");

        tr.toggle_pause(frozen, at_resume);
        assert!(!tr.paused);
        assert_eq!(target(&tr, at_resume), frozen, "resume continues, never skips");
        let two_on = at_resume + Duration::from_secs(2);
        assert_eq!(
            target(&tr, two_on),
            frozen + (2.0 * fps) as u64,
            "and then it advances at the asset rate from the frozen frame"
        );

        tr.toggle_pause(target(&tr, two_on), two_on);
        tr.seek_to(7, two_on);
        assert!(tr.paused, "a jump while paused does not resume playback");
        assert_eq!(target(&tr, two_on + Duration::from_secs(5)), 7, "it lands and holds");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn progress_row_timeout_is_suspended_while_paused() {
        let t0 = Instant::now();
        let mut row = ProgressTimer::default();
        let nearly = OVERLAY_HIDE_AFTER - Duration::from_millis(1);
        assert!(!row.visible(t0, false, false), "nothing has shown it yet");
        assert!(row.visible(t0, true, false), "a seek shows it");
        assert!(row.visible(t0 + nearly, false, false), "for the whole timeout");
        assert!(!row.visible(t0 + OVERLAY_HIDE_AFTER, false, false), "then it hides");

        let at_pause = t0 + Duration::from_secs(10);
        assert!(row.visible(at_pause, true, true), "pausing shows the row");
        let long_after = at_pause + OVERLAY_HIDE_AFTER * 5;
        assert!(row.visible(long_after, false, true), "with the deadline suspended");

        assert!(row.visible(long_after, true, false), "resume keeps it up");
        assert!(row.visible(long_after + nearly, false, false), "for a full timeout");
        assert!(!row.visible(long_after + OVERLAY_HIDE_AFTER, false, false), "then hides");
    }

    #[test]
    fn a_frozen_frame_is_painted_once_until_something_changes() {
        let mut gate = RepaintGate::default();
        for _ in 0..5 {
            assert!(gate.should_paint(false, false));
        }
        assert_eq!(gate.skipped, 0);

        assert!(gate.should_paint(true, true));
        for _ in 0..1000 {
            assert!(!gate.should_paint(true, false));
        }
        assert_eq!(gate.skipped, 1000);

        assert!(gate.should_paint(true, true));
        assert_eq!(gate.skipped, 0);
        assert!(!gate.should_paint(true, false));
        assert!(gate.should_paint(false, true));
        assert!(gate.should_paint(false, false));
    }

    #[test]
    fn pause_freezes_on_the_presented_frame_not_the_clock() {
        const FPS: f64 = 30.0;
        let t0 = Instant::now();
        let mut transport = Transport::new(0, t0);

        let press = t0 + Duration::from_secs(1);
        let clock_frame = (transport.elapsed_secs(press) * FPS) as u64;
        assert_eq!(clock_frame, 30, "the clock really is 30 frames ahead");
        transport.toggle_pause(freeze_target(Some(0), clock_frame), press);
        assert!(transport.paused);
        assert_eq!(transport.base_frame, 0, "frozen on what was on screen");
        assert_eq!(transport.elapsed_secs(press + Duration::from_secs(9)), 0.0);

        let resume = press + Duration::from_secs(9);
        transport.toggle_pause(freeze_target(Some(0), 0), resume);
        assert!(!transport.paused);
        assert_eq!(transport.base_frame, 0);
        let tick = resume + Duration::from_secs(1);
        assert_eq!((transport.elapsed_secs(tick) * FPS) as u64, 30, "one second on");

        assert_eq!(freeze_target(None, 17), 17);
    }

    fn two_clip_dir(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("auto-ascii-live-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("clip-a.ascii"), dir.join("clip-b.ascii"));
        std::fs::write(dir.join("clip-a.player.toml"), "codec = \"letters\"\nshadow_lift = 64\n")
            .unwrap();
        (dir, a, b)
    }

    #[test]
    fn saved_settings_load_per_clip_and_slash_survives_cuts() {
        let (dir, a, b) = two_clip_dir("cuts");
        let mut live = LiveSettings::new(None);
        assert!(live.front(0, &a), "first clip fronts");
        assert_eq!((live.codec, live.compose.shadow_lift, live.status()), (Codec::Letters, 64, "saved"));
        assert!(!live.front(0, &a), "same clip again is not a switch");
        assert!(live.front(1, &b));
        assert_eq!((live.codec, live.compose, live.status()), (Codec::Pixels, ComposeParams::default(), "default"));
        let mut info = String::new();
        live.write_info(&mut info);
        assert_eq!(info, " clip-b   codec: pixels   settings: default ");

        live.cycle(1);
        assert_eq!((live.codec, live.status()), (Codec::Letters, "s to save"));
        live.front(0, &a);
        assert_eq!((live.codec, live.compose.shadow_lift), (Codec::Letters, 64));
        live.cycle(1);
        live.front(1, &b);
        assert_eq!((live.codec, live.compose.shadow_lift), (Codec::Ascii, 0), "the / pick holds");

        live.save(&b);
        assert_eq!(live.status(), "saved");
        live.front(0, &a);
        live.front(1, &b);
        assert_eq!(live.saved, Some(VideoSettings { compose: ComposeParams::default(), codec: Codec::Ascii }));
        assert!(live.problems.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dial_turns_override_saved_values_across_cuts_and_wraps() {
        let (dir, a, b) = two_clip_dir("dial-cuts");
        let mut live = LiveSettings::new(None);
        live.front(0, &a);
        live.turn(Dial::ShadowLift, 1);
        live.turn(Dial::EdgeStrength, -2);
        live.turn(Dial::Hysteresis, 2);
        let turned = live.compose;
        assert_eq!(turned.shadow_lift, 80);
        for (idx, path) in [(1, &b), (0, &a), (1, &b)] {
            assert!(live.front(idx, path));
            assert_eq!(live.compose, turned, "session dials survive cuts and wrap");
            assert_eq!(live.status(), "s to save");
        }
        live.save(&b);
        assert_eq!(VideoSettings::load(&b).unwrap().unwrap().compose, turned);
        assert_eq!(live.status(), "saved");
        live.compose.edge_t_off = live.compose.edge_t_off.wrapping_add(1);
        assert_eq!(live.status(), "saved", "fields no dial persists never read as unsaved");
        live.compose = turned;
        assert_eq!(VideoSettings::load(&a).unwrap().unwrap().compose.shadow_lift, 64);
        live.front(0, &a);
        assert_eq!(live.compose, turned, "saving does not clear the session override");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codec_flag_beats_saved_until_slash_beats_it() {
        let (dir, a, b) = two_clip_dir("forced");
        let mut live = LiveSettings::new(Some(Codec::Pixels));
        live.front(0, &a);
        assert_eq!((live.codec, live.compose.shadow_lift), (Codec::Pixels, 64), "--codec over saved");
        live.cycle(1);
        live.front(1, &b);
        assert_eq!(live.codec, Codec::Letters, "/ over --codec, across the cut");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_sidecar_plays_defaults_and_is_reported() {
        let (dir, a, _) = two_clip_dir("bad");
        std::fs::write(dir.join("clip-a.player.toml"), "shadow_lift = lots\n").unwrap();
        let mut live = LiveSettings::new(None);
        live.front(0, &a);
        assert_eq!((live.codec, live.compose, live.status()), (Codec::Pixels, ComposeParams::default(), "unreadable"));
        assert_eq!(live.problems.len(), 1);
        assert!(live.problems[0].contains("clip-a.player.toml") && live.problems[0].contains("line 1"), "{:?}", live.problems);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_d_reveals_the_dial_before_it_cycles() {
        assert_eq!(dial_after_cycle(0, 1, false), 0, "first d shows shadow lift");
        assert_eq!(dial_after_cycle(0, 1, true), 1, "d cycles once the readout is up");
        assert_eq!(dial_after_cycle(0, 3, false), 2);
        assert_eq!(dial_after_cycle(2, 2, true), 1);
        assert_eq!(dial_after_cycle(1, 0, false), 1);
    }
}
