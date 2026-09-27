use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use std::fmt::Write as _;

use auto_ascii_core::{Codec, ComposeParams};
use auto_ascii_term::{AnsiBackend, Backend, ColorTier, ProbeOptions, probe_caps};

use crate::audio::{SinkChoice, Sound, SoundOptions, Soundtrack};
use crate::composition::Composition;
use crate::deck::{ClipDeck, DeckConfig};
use crate::error::Error;
use crate::pipeline::ProgressContext;
use crate::settings::VideoSettings;
use crate::{PaletteChoice, pipeline};

/// Repaint mode. There is one render path: [`Full`](RepaintMode::Full) is
/// the diff renderer with `invalidate()` every frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepaintMode {
    /// Repaint every cell every frame (the default; right for local
    /// GPU-accelerated terminals).
    #[default]
    Full,
    /// Pure diff: only damaged cells are rewritten; full repaint on resize
    /// only. Fewer output bytes, right for slower terminals.
    Diff,
}

/// Floor for [`fps_cap`](PlayerBuilder::fps_cap): caps below 1 fps are
/// rejected at [`build`](PlayerBuilder::build) with [`Error::Config`]. The
/// pacing loop sleeps `1/cap` seconds between presents, so a tiny positive
/// cap (e.g. `1e-9`) would freeze input handling for years, and small
/// enough values would overflow `Duration::from_secs_f64` mid-session.
pub const MIN_FPS_CAP: f64 = 1.0;

pub use crate::pipeline::SCRUB_STEP_SECS;

const DIAL_OVERLAY_HIDE_AFTER: Duration = Duration::from_millis(2500);

/// A renderer knob adjustable during playback: `d` selects, `[`/`]` turns it.
///
/// Every dial is a [`ComposeParams`] field — a render-time setting, not part
/// of the asset — so turning one re-renders the asset already in memory
/// instead of rebuilding it. One asset serves every setting; nothing is baked
/// in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dial {
    /// Open the shadows so a dark subject clears the first ramp steps instead
    /// of sharing a glyph with black. First in the cycle: it is the one that
    /// rescues detail that is otherwise simply absent.
    ShadowLift,
    /// Edge gate on-threshold. LOWER draws more edges — the dial is inverted
    /// on screen so that turning it up means "more edges", which is what a
    /// person turning a dial expects.
    EdgeStrength,
    /// Ramp-index hysteresis width: wider = stickier cells = less flicker,
    /// narrower = more responsive.
    Hysteresis,
}

impl Dial {
    /// Cycle order. Shadow lift leads deliberately (see the variant docs).
    pub const ALL: [Dial; 3] = [Dial::ShadowLift, Dial::EdgeStrength, Dial::Hysteresis];

    /// Short label for the on-screen readout.
    pub fn label(self) -> &'static str {
        match self {
            Dial::ShadowLift => "shadow lift",
            Dial::EdgeStrength => "edge strength",
            Dial::Hysteresis => "hysteresis",
        }
    }

    /// The readout label for the dial's current value: [`label`](Dial::label),
    /// marked when the dial sits at its floor, its default or its top, so a
    /// turn that cannot go further says so.
    pub fn readout(self, p: &ComposeParams) -> &'static str {
        let v = self.get(p);
        let (floor, default) = (v == 0, v == self.get(&ComposeParams::default()));
        match (self, floor, default, v == self.max()) {
            (Dial::ShadowLift, true, true, _) => "shadow lift (floor, default)",
            (Dial::ShadowLift, true, false, _) => "shadow lift (floor)",
            (Dial::ShadowLift, _, true, _) => "shadow lift (default)",
            (Dial::ShadowLift, _, _, true) => "shadow lift (max)",
            (Dial::EdgeStrength, true, _, _) => "edge strength (floor)",
            (Dial::EdgeStrength, _, true, _) => "edge strength (default)",
            (Dial::EdgeStrength, _, _, true) => "edge strength (max)",
            (Dial::Hysteresis, true, _, _) => "hysteresis (floor)",
            (Dial::Hysteresis, _, true, true) => "hysteresis (default, max)",
            (Dial::Hysteresis, _, true, false) => "hysteresis (default)",
            (Dial::Hysteresis, _, _, true) => "hysteresis (max)",
            _ => self.label(),
        }
    }

    /// How far one `[`/`]` press moves it. Sized so a dial crosses its useful
    /// range in roughly a dozen presses rather than a hundred.
    pub fn step(self) -> i32 {
        match self {
            Dial::ShadowLift => 16,
            Dial::EdgeStrength => 4,
            Dial::Hysteresis => 16,
        }
    }

    /// Upper bound of the on-screen scale.
    pub fn max(self) -> u8 {
        match self {
            Dial::ShadowLift => 255,
            Dial::Hysteresis => auto_ascii_core::hysteresis::IDX_HYST_MAX_Q8,
            Dial::EdgeStrength => 128,
        }
    }

    /// Current on-screen value (already un-inverted where the underlying
    /// field runs the other way).
    pub fn get(self, p: &ComposeParams) -> u8 {
        match self {
            Dial::ShadowLift => p.shadow_lift,
            Dial::EdgeStrength => self.max().saturating_sub(p.edge_t_on),
            Dial::Hysteresis => p.idx_hyst_q8.min(self.max()),
        }
    }

    /// The `params.toml` `[compose]` key of the field this dial turns — also
    /// its key in a video's saved settings file (`<asset>.player.toml`).
    pub fn param_key(self) -> &'static str {
        match self {
            Dial::ShadowLift => "shadow_lift",
            Dial::EdgeStrength => "edge_t_on",
            Dial::Hysteresis => "idx_hyst_q8",
        }
    }

    /// The raw field value (not inverted — [`get`](Dial::get) is the
    /// on-screen reading).
    pub fn param(self, p: &ComposeParams) -> u8 {
        match self {
            Dial::ShadowLift => p.shadow_lift,
            Dial::EdgeStrength => p.edge_t_on,
            Dial::Hysteresis => p.idx_hyst_q8,
        }
    }

    /// Set the raw field, clamped to what the dial can reach — a saved or
    /// hand-edited value outside the scale would leave the readout pinned
    /// at an end while the picture sat somewhere the dial cannot return to.
    pub fn set_param(self, p: &mut ComposeParams, v: u8) {
        match self {
            Dial::ShadowLift => p.shadow_lift = v,
            Dial::EdgeStrength => p.edge_t_on = v.min(self.max()),
            Dial::Hysteresis => p.idx_hyst_q8 = v.min(self.max()),
        }
    }

    /// Apply a signed number of steps, saturating at the dial's ends.
    ///
    /// The top is a stop, not a detent. Where `max()` is no multiple of
    /// `step()` (shadow lift's 255 against 16), counting down from it would
    /// leave the grid the dial climbed on, and up N / down N would miss the
    /// start by one (255 → 239, not 240). A press away from the top counts
    /// from the detent just above `max()` instead, so every walk retraces its
    /// own steps.
    pub fn turn(self, p: &mut ComposeParams, steps: i32) {
        let (cur, step, max) = (i32::from(self.get(p)), self.step(), i32::from(self.max()));
        let from = if cur == max { (max + step - 1) / step * step } else { cur };
        let next = (from + steps * step).clamp(0, max) as u8;
        match self {
            Dial::ShadowLift => p.shadow_lift = next,
            Dial::EdgeStrength => p.edge_t_on = self.max() - next,
            Dial::Hysteresis => p.idx_hyst_q8 = next,
        }
    }
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
        let current = self.current();
        self.note.unwrap_or(match self.saved {
            Some(s) if s == current => "saved",
            None if current == VideoSettings::default() => "default",
            _ => "s to save",
        })
    }

    fn write_info(&self, out: &mut String, sound: Sound) {
        out.clear();
        let _ = write!(
            out,
            " {}   codec: {}   settings: {}   sound: {} ",
            self.clip_name,
            self.codec.name(),
            self.status(),
            sound.label()
        );
    }
}

const OVERLAY_HIDE_AFTER: Duration = Duration::from_millis(1000);

#[allow(missing_docs)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    Ended,
    Quit,
}

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

const AUDIO_FRAME_EPS: f64 = 1e-6;

#[derive(Debug)]
struct MediaClock {
    transport: Transport,
    sound: Option<Soundtrack>,
    fps: f64,
}

impl MediaClock {
    fn new(base_frame: u64, now: Instant, sound: Option<Soundtrack>, fps: f64) -> MediaClock {
        let mut clock = MediaClock { transport: Transport::new(base_frame, now), sound, fps };
        if let Some(s) = &mut clock.sound {
            s.seek(base_frame as f64 / fps, now);
            s.set_running(true, now);
        }
        clock
    }

    fn paused(&self) -> bool {
        self.transport.paused
    }

    fn frame(&self, comp: &Composition, now: Instant) -> u64 {
        match &self.sound {
            Some(s) if !self.transport.paused => comp.frame_after(0, s.now_secs(now) + AUDIO_FRAME_EPS),
            _ => comp.frame_after(self.transport.base_frame, self.transport.elapsed_secs(now)),
        }
    }

    fn seek_to(&mut self, frame: u64, now: Instant) {
        self.transport.seek_to(frame, now);
        if let Some(s) = &mut self.sound {
            s.seek(frame as f64 / self.fps, now);
        }
    }

    fn toggle_pause(&mut self, frozen: u64, now: Instant) {
        self.transport.toggle_pause(frozen, now);
        let (paused, base) = (self.transport.paused, self.transport.base_frame);
        if let Some(s) = &mut self.sound {
            if !paused {
                s.seek(base as f64 / self.fps, now);
            }
            s.set_running(!paused, now);
        }
    }

    fn sound(&self) -> Sound {
        self.sound.as_ref().map_or(Sound::None, Soundtrack::sound)
    }

    fn toggle_mute(&mut self) -> Sound {
        self.sound.as_mut().map_or(Sound::None, Soundtrack::toggle_mute)
    }

    fn poll(&mut self, now: Instant) -> bool {
        self.sound.as_mut().is_some_and(|s| s.poll(now))
    }

    fn finish(&mut self) -> Vec<String> {
        self.sound.take().map(Soundtrack::finish).unwrap_or_default()
    }
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

/// Builder for [`Player`] — see [`Player::builder`]. Every option has a
/// sensible default; only [`asset`](PlayerBuilder::asset) is required.
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
    no_backdrop: bool,
    font_table: Option<String>,
    codec: Option<Codec>,
    mute: bool,
    no_audio: bool,
}

impl PlayerBuilder {
    /// Path of the ASCI asset to play. Required, unless
    /// [`composition`](PlayerBuilder::composition) is set instead.
    pub fn asset(mut self, path: impl Into<PathBuf>) -> Self {
        self.asset = Some(path.into());
        self
    }

    /// Path of a composition `.toml` to play instead of one asset. The
    /// timeline is the composition's: `--seek`, the digits, the arrows,
    /// `--loop` and the progress row all count its frames, clips switch
    /// decoders at their boundaries and a gap plays black. Bare library
    /// names inside the file resolve through
    /// [`Composition::default_library_dir`].
    ///
    /// Mutually exclusive with [`asset`](PlayerBuilder::asset).
    pub fn composition(mut self, path: impl Into<PathBuf>) -> Self {
        self.composition = Some(path.into());
        self
    }

    /// Glyph repertoire (default [`PaletteChoice::Auto`]: derived from the
    /// probed terminal capabilities).
    pub fn palette(mut self, palette: PaletteChoice) -> Self {
        self.palette = palette;
        self
    }

    /// Force the color tier instead of probing (`Some(..)` also skips the
    /// probe volley entirely). `None` (the default) probes.
    pub fn tier(mut self, tier: Option<ColorTier>) -> Self {
        self.tier = tier;
        self
    }

    /// Repaint mode (default [`RepaintMode::Full`]).
    pub fn repaint(mut self, repaint: RepaintMode) -> Self {
        self.repaint = repaint;
        self
    }

    /// Cap the presentation rate below the asset fps. Frames are still
    /// selected by wall clock, so capping skips asset frames — it never
    /// slows the video down. Must be at least [`MIN_FPS_CAP`] (1 fps),
    /// checked at [`build`](PlayerBuilder::build): the pacing loop sleeps
    /// `1/cap` seconds between presents, so sub-1 caps would leave quit keys
    /// and resizes unserviced for arbitrarily long (and tiny values would
    /// overflow `Duration`).
    pub fn fps_cap(mut self, fps: f64) -> Self {
        self.fps_cap = Some(fps);
        self
    }

    /// Loop playback instead of returning at the last frame (default off).
    pub fn looping(mut self, looping: bool) -> Self {
        self.looping = looping;
        self
    }

    /// Override the cell aspect `cell_h_px / cell_w_px` used by the
    /// letterbox math. Default: the terminal's reported cell pixel size,
    /// else 2.0.
    pub fn cell_aspect(mut self, cell_aspect: f64) -> Self {
        self.cell_aspect = Some(cell_aspect);
        self
    }

    /// Start playback this many seconds in (FIDX keyframe seek).
    /// Must land inside the asset (checked at [`build`](PlayerBuilder::build)).
    pub fn seek_secs(mut self, secs: f64) -> Self {
        self.seek_secs = Some(secs);
        self
    }

    /// Stop after this many seconds of WALL clock (default: play to end).
    /// Wall clock, not asset time: time spent paused (space) counts against
    /// the budget exactly as playing time does.
    pub fn duration_secs(mut self, secs: f64) -> Self {
        self.duration_secs = Some(secs);
        self
    }

    /// Never write the capability-probe volley; rely on passive env hints
    /// only (an escape hatch for hostile PTYs).
    pub fn no_query(mut self, no_query: bool) -> Self {
        self.no_query = no_query;
        self
    }

    /// Bypass the capability-probe cache (no read, no write).
    pub fn no_cache(mut self, no_cache: bool) -> Self {
        self.no_cache = no_cache;
        self
    }

    /// Skip the identity-keyed quirk table: the probe normally corrects
    /// known reply gaps after the volley — e.g. kitty's missing XTGETTCAP
    /// `RGB` entry, or plain xterm's "no direct color" answer overriding a
    /// stale `COLORTERM` — keyed on the terminal's own XTVERSION reply, never
    /// on `TERM`. This escape hatch takes the replies at face value: the
    /// probe cache is bypassed in both directions (cached entries hold
    /// quirk-adjusted results, so none is read, and a no-quirks result is
    /// never stored) and the volley always runs fresh.
    pub fn no_quirks(mut self, no_quirks: bool) -> Self {
        self.no_quirks = no_quirks;
        self
    }

    /// Leave the terminal's own default background alone. By default the
    /// session sets it to black (OSC 11) on entry and resets it to the
    /// terminal's configured one (OSC 111) on normal exit, errors, a Rust
    /// panic, SIGINT, SIGTERM, SIGHUP and atexit, so a codec that keeps the
    /// terminal background ([`Codec::Ascii`]) sits on black under any theme;
    /// pixels and letters paint every cell and look the same either way. The
    /// mono tier never sets it (see [`AnsiBackend::with_backdrop`]).
    pub fn no_backdrop(mut self, no_backdrop: bool) -> Self {
        self.no_backdrop = no_backdrop;
        self
    }

    /// Assert which font the terminal renders with, by ink-coverage table: a
    /// built-in name — `conservative`, `dejavu-sans-mono`, `liberation-mono`,
    /// `ubuntu-mono`, `noto-sans-mono` — or a path to a `auto-ascii-factory
    /// font-table` TOML. Terminals cannot be queried for their font, so this
    /// is user-asserted truth: the table's recorded repertoire vetoes the
    /// palette selection (a tier whose glyphs the font is missing degrades
    /// braille → unicode → ascii instead of drawing missing-glyph boxes).
    /// Resolved and validated at [`build`](PlayerBuilder::build).
    pub fn font_table(mut self, name_or_path: impl Into<String>) -> Self {
        self.font_table = Some(name_or_path.into());
        self
    }

    /// Start in this glyph [`Codec`] for every clip, overriding any codec
    /// saved in a video's settings (default: the saved one, else
    /// [`Codec::Pixels`]). `/` still cycles it during playback.
    pub fn codec(mut self, codec: Codec) -> Self {
        self.codec = Some(codec);
        self
    }

    #[allow(missing_docs)]
    pub fn mute(mut self, mute: bool) -> Self {
        self.mute = mute;
        self
    }

    #[allow(missing_docs)]
    pub fn no_audio(mut self, no_audio: bool) -> Self {
        self.no_audio = no_audio;
        self
    }

    /// Check the configuration and open every asset that will play — one
    /// for [`asset`](PlayerBuilder::asset), all of a
    /// [`composition`](PlayerBuilder::composition)'s clips — as a
    /// container, through a read-only mapping.
    ///
    /// Does NOT touch the terminal: that happens in [`Player::run`], so a
    /// bad path, a corrupt or truncated file, a clip with no picture in it,
    /// an impossible trim or a seek past the end all fail cleanly here,
    /// before any screen state changes. The decoders themselves are built
    /// in `run`, per clip, as the timeline reaches them.
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

/// A ready-to-run terminal player: every clip validated and its timeline
/// resolved, terminal not yet touched. Created by [`Player::builder`];
/// consumed by [`Player::run`].
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
    /// Start building a player.
    pub fn builder() -> PlayerBuilder {
        PlayerBuilder::default()
    }

    /// Play the asset: probe the terminal (a DA1-sentinel query volley,
    /// unless a tier is forced or [`no_query`](PlayerBuilder::no_query) is
    /// set), enter the session (alt screen, raw mode, hidden cursor, black
    /// backdrop unless [`no_backdrop`](PlayerBuilder::no_backdrop)), run
    /// the frame loop — paced by the soundtrack's audio clock when one plays,
    /// else by the wall clock — and restore the terminal — also on
    /// panic, SIGINT, SIGTERM and SIGHUP (the restore hooks are armed before the
    /// screen is touched).
    ///
    /// Blocks until the asset ends (unless [`looping`](PlayerBuilder::looping)),
    /// the configured [`duration`](PlayerBuilder::duration_secs) elapses, or
    /// the user quits (`q` / `Esc` / `Ctrl-C`). Keys `0`–`9` jump to that
    /// ×10% of the asset; `←`/`→` scrub ±[`SCRUB_STEP_SECS`] (5 s); `d`
    /// cycles the live [`Dial`]s and `[`/`]` turn the selected one; `/`
    /// cycles the glyph [`Codec`]; `s` saves the dials and codec as this
    /// video's settings (`<asset>.player.toml`, loaded whenever the video
    /// fronts); `m` turns the soundtrack on and off; space freezes the
    /// picture and resumes it from the frozen frame (jumps and scrubs still
    /// work while frozen, and stay frozen).
    /// Saved dials load per clip until the first dial turn. That turn keeps
    /// the full current compose settings for this session across cuts and
    /// wraps, overriding later clips' saved dials, just as `/` keeps its
    /// codec choice. Saving writes only the current clip's settings.
    /// Every seek flashes a bottom-row progress overlay that auto-hides
    /// after ~1 s. A key-hints row sits above it whenever an overlay is up,
    /// for the first few seconds of playback, and for as long as `v` pins it
    /// open. Above it the info row names the clip, the codec and the grid
    /// size, with a zoom-out hint on narrow terminals; a resize raises both
    /// for a moment, so zooming the terminal reads out the new size.
    ///
    /// # Errors
    /// [`Error::Terminal`] when stdout is not a TTY (headless callers want
    /// [`crate::RenderSession`]); [`Error::Decode`] on mid-playback asset
    /// corruption.
    pub fn run(self) -> Result<(), Error> {
        self.play().map(|_| ())
    }

    #[allow(missing_docs)]
    pub fn play(self) -> Result<Stopped, Error> {
        let probe_opts = ProbeOptions {
            forced_tier: self.cfg.tier,
            no_query: self.cfg.no_query || self.cfg.tier.is_some(),
            no_quirks: self.cfg.no_quirks,
            no_cache: self.cfg.no_cache,
            ..ProbeOptions::default()
        };
        let caps = probe_caps(&probe_opts);

        let mut backend =
            AnsiBackend::with_backdrop(caps, !self.cfg.no_backdrop).map_err(Error::Terminal)?;
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
        let opening = match &self.cfg.composition {
            None => Soundtrack::open(
                &self.comp.clips()[0].path,
                self.comp.duration_secs(),
                SoundOptions { no_audio: self.cfg.no_audio, muted: self.cfg.mute, looping: self.cfg.looping },
                SinkChoice::Device,
            ),
            Some(_) => Default::default(),
        };
        let t0 = Instant::now();
        let mut clock = MediaClock::new(u64::from(self.start_frame), t0, opening.soundtrack, asset_fps);
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

        let stopped = loop {
            let drained = deck.drain_events(&mut backend);
            if drained.quit {
                break Ok(Stopped::Quit);
            }
            let mut sought = false;
            let mut resumed = false;
            if drained.toggle_pause {
                let now = Instant::now();
                let clock_frame = clock.frame(&self.comp, now);
                let clock_frame =
                    if self.cfg.looping { clock_frame % frame_count } else { clock_frame };
                clock.toggle_pause(freeze_target(presented, clock_frame), now);
                resumed = !clock.paused();
                deck.set_paused(clock.paused());
            }
            if let Some(d) = drained.jump_digit {
                clock.seek_to(frame_count * u64::from(d) / 10, Instant::now());
                sought = true;
            }
            if drained.seek_steps != 0 {
                let now = Instant::now();
                let pos = clock.frame(&self.comp, now);
                let pos = if self.cfg.looping { pos % frame_count } else { pos };
                let delta = (f64::from(drained.seek_steps) * SCRUB_STEP_SECS * asset_fps) as i64;
                let landing = (pos.min(frame_count - 1) as i64 + delta)
                    .clamp(0, frame_count as i64 - 1) as u64;
                clock.seek_to(landing, now);
                sought = true;
            }
            if drained.toggle_sound {
                clock.toggle_mute();
            }
            let sound_lost = clock.poll(Instant::now());
            if drained.dial_cycle > 0 || drained.dial_delta != 0 {
                if drained.dial_cycle > 0 {
                    dial_idx = dial_after_cycle(dial_idx, drained.dial_cycle, dial_until.is_some());
                }
                let dial = Dial::ALL[dial_idx];
                if drained.dial_delta != 0 {
                    live.turn(dial, drained.dial_delta);
                    deck.set_compose_params(live.compose);
                }
                deck.set_dial_overlay(Some((dial.readout(&live.compose), dial.get(&live.compose), dial.max())));
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
            if drained.codec_cycle > 0 || drained.save || drained.toggle_sound || resized {
                note_until = Some(Instant::now() + DIAL_OVERLAY_HIDE_AFTER);
            } else if note_until.is_some_and(|t| Instant::now() >= t) {
                note_until = None;
            }
            let show_progress =
                progress.visible(Instant::now(), sought || resumed, clock.paused());
            deck.set_progress_overlay(show_progress);
            let overlays_up = show_progress || dial_until.is_some() || note_until.is_some();
            let show_hints = hints.visible(Instant::now(), drained.toggle_hints, overlays_up);
            deck.set_hint_overlay(show_hints);
            if let Some(dur) = self.cfg.duration_secs
                && t0.elapsed().as_secs_f64() >= dur
            {
                break Ok(Stopped::Ended);
            }
            let mut target = clock.frame(&self.comp, Instant::now());
            if target >= frame_count {
                if self.cfg.looping {
                    target %= frame_count;
                } else {
                    break Ok(Stopped::Ended);
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
                    deck.set_dial_overlay(Some((dial.readout(&live.compose), dial.get(&live.compose), dial.max())));
                }
            }
            live.write_info(&mut info, clock.sound());
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
                || drained.toggle_sound
                || sound_lost
                || drained.toggle_hints
                || show_progress != was_progress
                || show_hints != was_hints
                || dial_up != was_dial
                || deck.size() != was_size;
            (was_progress, was_hints, was_dial, was_size) =
                (show_progress, show_hints, dial_up, deck.size());

            if gate.should_paint(clock.paused(), dirty) {
                if let Err(e) = deck.present_at(&mut backend, located) {
                    break Err(e);
                }
                presented = Some(target);
            }

            next_tick += tick;
            let now = Instant::now();
            if next_tick > now {
                std::thread::sleep(next_tick - now);
            } else {
                next_tick = now;
            }
        };
        let sound_notes = clock.finish();
        backend.shutdown();
        for problem in &live.problems {
            eprintln!("auto-ascii-player: settings: {problem}");
        }
        for note in opening.notes.iter().chain(&sound_notes) {
            eprintln!("auto-ascii-player: sound: {note}");
        }
        stopped
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
        live.write_info(&mut info, Sound::None);
        assert_eq!(info, " clip-b   codec: pixels   settings: default   sound: none ");

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
    fn readout_marks_the_floor_the_default_and_the_top() {
        let mut p = ComposeParams::default();
        assert_eq!(Dial::ShadowLift.readout(&p), "shadow lift (floor, default)");
        assert_eq!(Dial::EdgeStrength.readout(&p), "edge strength (default)");
        assert_eq!(Dial::Hysteresis.readout(&p), "hysteresis (default)");
        Dial::ShadowLift.turn(&mut p, 1);
        assert_eq!(Dial::ShadowLift.readout(&p), "shadow lift");
        Dial::ShadowLift.turn(&mut p, 99);
        assert_eq!(Dial::ShadowLift.readout(&p), "shadow lift (max)");
        Dial::EdgeStrength.turn(&mut p, -99);
        assert_eq!(Dial::EdgeStrength.readout(&p), "edge strength (floor)");
        Dial::Hysteresis.turn(&mut p, 99);
        assert_eq!(Dial::Hysteresis.readout(&p), "hysteresis (max)");
        Dial::Hysteresis.turn(&mut p, -99);
        assert_eq!(Dial::Hysteresis.readout(&p), "hysteresis (floor)");
    }

    fn fixture_comp(tag: &str) -> (Composition, PathBuf) {
        let path = std::env::temp_dir().join(format!("auto-ascii-sound-sync-{}-{tag}.ascii", std::process::id()));
        std::fs::write(
            &path,
            auto_ascii_eval::fixtures::build_fixture(auto_ascii_eval::fixtures::Fixture::GradientMotion),
        )
        .unwrap();
        let mut comp = Composition::single(&path);
        comp.resolve().unwrap();
        (comp, path)
    }

    const SYNC_RATE: u32 = 8000;

    fn sync_track(comp: &Composition, looping: bool) -> Soundtrack {
        let frames = (comp.duration_secs() * f64::from(SYNC_RATE)).round() as usize;
        let samples: Vec<i16> = (0..frames).flat_map(|i| [i as i16 + 1, 0]).collect();
        let track = crate::audio::output::Track::with_pcm(crate::audio::output::Pcm::from_samples(SYNC_RATE, 2, &samples));
        let format = crate::audio::output::DeviceFormat { rate: SYNC_RATE, channels: 2, name: "fake".into() };
        Soundtrack::with_track(track, comp.duration_secs(), SoundOptions { looping, ..SoundOptions::default() }, format)
    }

    #[test]
    fn info_row_names_the_sound_state() {
        let (dir, a, _) = two_clip_dir("sound-info");
        let mut live = LiveSettings::new(None);
        live.front(0, &a);
        let mut info = String::new();
        for (sound, want) in [(Sound::On, "on"), (Sound::Off, "off"), (Sound::None, "none")] {
            live.write_info(&mut info, sound);
            assert_eq!(info, format!(" clip-a   codec: letters   settings: saved   sound: {want} "));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builder_carries_the_sound_flags() {
        let b = Player::builder().mute(true);
        assert!(b.mute && !b.no_audio);
        let b = Player::builder().no_audio(true);
        assert!(b.no_audio && !b.mute);
        let b = Player::builder();
        assert!(!b.mute && !b.no_audio, "sound is on by default");
    }

    #[test]
    fn without_a_soundtrack_the_media_clock_is_the_wall_transport() {
        let (comp, path) = fixture_comp("wall");
        let t0 = Instant::now();
        let mut clock = MediaClock::new(5, t0, None, comp.fps());
        let tr = Transport::new(5, t0);
        for ms in [0u64, 10, 333, 1000, 2400] {
            let at = t0 + Duration::from_millis(ms);
            assert_eq!(clock.frame(&comp, at), comp.frame_after(tr.base_frame, tr.elapsed_secs(at)));
        }
        assert_eq!((clock.sound(), clock.toggle_mute()), (Sound::None, Sound::None), "m does nothing");
        clock.seek_to(40, t0 + Duration::from_secs(3));
        assert_eq!(clock.frame(&comp, t0 + Duration::from_secs(4)), 70);
        assert!(!clock.poll(t0) && clock.finish().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn m_mutes_through_the_media_clock_and_the_picture_keeps_moving() {
        let (comp, path) = fixture_comp("mute");
        let st = sync_track(&comp, false);
        let (out, shared) = (st.output(), st.shared().clone());
        let t0 = shared.epoch;
        let mut clock = MediaClock::new(0, t0, Some(st), comp.fps());
        assert_eq!(clock.sound(), Sound::On);
        let mut buf = [0.0f32; 160];
        let mut frames = Vec::new();
        for i in 1..=90 {
            let at = t0 + Duration::from_millis(i * 10);
            if i == 30 || i == 60 {
                clock.toggle_mute();
            }
            out.fill(&mut buf, at, None);
            assert_eq!(buf.iter().all(|&s| s == 0.0), (30..60).contains(&i), "callback {i}");
            frames.push(clock.frame(&comp, at));
        }
        assert_eq!(clock.sound(), Sound::On);
        assert!(frames.windows(2).all(|w| w[1] >= w[0]), "the picture never stalls or rewinds on m");
        assert_eq!(frames[89], 26, "0.89 s at 30 fps, mute or not");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audio_and_picture_stay_within_a_frame_across_seeks_pauses_and_many_wraps() {
        let (comp, path) = fixture_comp("sync");
        let (fps, count) = (comp.fps(), u64::from(comp.frame_count()));
        let st = sync_track(&comp, true);
        let (out, shared) = (st.output(), st.shared().clone());
        let t0 = shared.epoch;
        let rate = f64::from(SYNC_RATE);
        let (period, latency) = (0.010, 0.020);
        let mut clock = MediaClock::new(0, t0, Some(st), fps);
        let mut buf = vec![0.0f32; (period * rate) as usize * 2];
        let mut chunks: Vec<(f64, Option<u64>)> = Vec::new();
        let heard = |t: f64, chunks: &[(f64, Option<u64>)]| -> Option<f64> {
            let &(cb, start) = chunks.iter().rev().find(|(cb, _)| cb + latency <= t + 1e-12)?;
            let pos = start? as f64 + ((t - cb - latency) * rate).min(period * rate);
            Some(pos % (count as f64 / fps * rate) / rate)
        };
        let mut quiet_until = 0.0f64;
        let mut paused_at: Option<u64> = None;
        let mut presented = 0u64;
        let (mut worst, mut checked, mut worst_tail) = (0u64, 0u64, 0u64);
        let mut next_cb = period;
        let mut rng: u64 = 0x5EED;
        let end = 150.0;
        for ms in 1..=(end * 1000.0) as u64 {
            let t = ms as f64 / 1000.0;
            let at = t0 + Duration::from_secs_f64(t);
            if ms % 7300 == 0 && t < 90.0 {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let landing = (rng >> 33) % count;
                clock.seek_to(landing, at);
                paused_at = paused_at.map(|_| landing);
                quiet_until = t + latency + 2.0 * period;
            }
            if ms % 11_100 == 0 && t < 90.0 {
                let landing = (clock.frame(&comp, at) % count + 150) % count;
                clock.seek_to(landing, at);
                paused_at = paused_at.map(|_| landing);
                quiet_until = t + latency + 2.0 * period;
            }
            if ms == 33_000 || ms == 61_000 {
                clock.toggle_pause(freeze_target(Some(presented), 0), at);
                paused_at = Some(presented);
            }
            if ms == 35_500 || ms == 64_000 {
                presented = paused_at.unwrap_or(presented);
                clock.toggle_pause(presented, at);
                paused_at = None;
                quiet_until = t + latency + 2.0 * period;
            }
            if t + 1e-9 >= next_cb {
                out.fill(&mut buf, at, Some(Duration::from_secs_f64(latency)));
                let first = (buf[0] * 32768.0).round() as i64;
                chunks.push((t, (first > 0).then(|| first as u64 - 1)));
                next_cb += period;
            }
            let frame = clock.frame(&comp, at) % count;
            if let Some(frozen) = paused_at {
                assert_eq!(frame, frozen, "paused at {t:.3}: the picture holds");
                assert!(buf.iter().all(|&s| s == 0.0), "paused at {t:.3}: the output is silent");
                continue;
            }
            presented = frame;
            if t < quiet_until {
                continue;
            }
            let Some(h) = heard(t, &chunks) else { continue };
            let heard_frame = ((h + 1e-9) * fps) as u64 % count;
            let d = frame.abs_diff(heard_frame).min(count - frame.abs_diff(heard_frame));
            worst = worst.max(d);
            if t > 90.0 {
                worst_tail = worst_tail.max(d);
            }
            checked += 1;
        }
        let wraps = (end - 90.0) / (count as f64 / fps);
        assert!(wraps > 20.0, "{wraps} undisturbed wraps at the end");
        assert!(checked > 100_000, "{checked}");
        assert!(worst <= 1, "the picture drifted {worst} frames from the sound");
        assert!(worst_tail <= 1, "and {worst_tail} frames after {wraps:.0} wraps with no seek");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pause_freezes_the_audio_clock_and_resume_starts_the_sound_at_the_frozen_frame() {
        let (comp, path) = fixture_comp("pause");
        let st = sync_track(&comp, false);
        let (out, shared) = (st.output(), st.shared().clone());
        let t0 = shared.epoch;
        let mut clock = MediaClock::new(0, t0, Some(st), comp.fps());
        let mut buf = [0.0f32; 160];
        let at = |s: f64| t0 + Duration::from_secs_f64(s);
        for i in 1..=50 {
            out.fill(&mut buf, at(f64::from(i) * 0.01), None);
        }
        let frozen = clock.frame(&comp, at(0.5));
        clock.toggle_pause(frozen, at(0.5));
        let consumed = shared.consumed();
        for i in 51..=300 {
            out.fill(&mut buf, at(f64::from(i) * 0.01), None);
            assert_eq!(clock.frame(&comp, at(f64::from(i) * 0.01)), frozen);
        }
        assert!(shared.consumed() <= consumed + 80, "pause stops consumption (one chunk may be in flight)");
        clock.toggle_pause(frozen, at(3.0));
        out.fill(&mut buf, at(3.01), None);
        let first = (buf[0] * 32768.0).round() as u64 - 1;
        assert_eq!(first, (frozen as f64 / comp.fps() * f64::from(SYNC_RATE)).round() as u64, "the sound resumes at the frozen frame");
        assert_eq!(clock.frame(&comp, at(3.015)), frozen, "and the picture with it");
        for i in 302..=320 {
            out.fill(&mut buf, at(f64::from(i) * 0.01), None);
        }
        assert_eq!(clock.frame(&comp, at(3.2)), frozen + 5, "0.19 s of sound played since the resume");
        let _ = std::fs::remove_file(&path);
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
