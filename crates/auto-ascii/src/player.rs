//! [`Player`] — the blocking terminal player (M4 item A, feature
//! `terminal`): probe the terminal, enter a session, play, restore. The
//! whole M0–M3 machinery behind two calls:
//!
//! ```no_run
//! auto_ascii::Player::builder().asset("intro.ascii").build()?.run()?;
//! # Ok::<(), auto_ascii::Error>(())
//! ```

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

/// Repaint mode (PLAN §3.1: one render path — [`Full`](RepaintMode::Full)
/// is the diff renderer with `invalidate()` every frame).
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

/// Documented floor for [`fps_cap`](PlayerBuilder::fps_cap) (M5 fix 3):
/// caps below 1 fps are rejected at [`build`](PlayerBuilder::build) with
/// [`Error::Config`]. The pacing loop sleeps `1/cap` seconds between
/// presents, so a tiny positive cap (e.g. `1e-9`) would freeze input
/// handling for years — and small enough values overflow
/// `Duration::from_secs_f64`, panicking only after the terminal session had
/// already started.
pub const MIN_FPS_CAP: f64 = 1.0;

// The scrub step now lives in `pipeline` — the progress and hints rows print
// it (M6, PLAN-M6-M8 §1) and that module builds without the `terminal`
// feature. Re-exported here so `auto_ascii::SCRUB_STEP_SECS` is unchanged.
pub use crate::pipeline::SCRUB_STEP_SECS;

/// How long the live-dial readout stays up after the last turn of the dial.
/// Longer than the seek overlay: you are watching the picture change while you
/// turn it, and the readout is the only thing telling you where you are.
const DIAL_OVERLAY_HIDE_AFTER: Duration = Duration::from_millis(2500);

/// A renderer knob adjustable during playback: `d` selects, `[`/`]` turns it.
///
/// Every dial is a [`ComposeParams`] field, and that is the point — those are
/// documented as excluded from the build fingerprint and the eval cache key,
/// so turning one re-renders the asset already in memory instead of rebuilding
/// it. One asset serves every setting; nothing is baked in.
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
            Dial::ShadowLift | Dial::Hysteresis => 255,
            Dial::EdgeStrength => 128,
        }
    }

    /// Current on-screen value (already un-inverted where the underlying
    /// field runs the other way).
    pub fn get(self, p: &ComposeParams) -> u8 {
        match self {
            Dial::ShadowLift => p.shadow_lift,
            Dial::EdgeStrength => self.max().saturating_sub(p.edge_t_on),
            Dial::Hysteresis => p.idx_hyst_q8,
        }
    }

    /// The `params.toml` `[compose]` key of the field this dial turns — also
    /// its key in a video's saved settings file (see [`crate::settings`]).
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
            Dial::Hysteresis => p.idx_hyst_q8 = v,
        }
    }

    /// Apply a signed number of steps, saturating at the dial's ends.
    ///
    /// The top is a stop, not a detent: 255 is no multiple of a 16 step, so
    /// counting down from it by `step()` would leave the grid the dial
    /// climbed on, and up N / down N would miss the start by one (255 → 239,
    /// not 240). A press away from the top counts from the detent just above
    /// `max()` instead, so every walk retraces its own steps.
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

/// Where `d` lands (M6 review fix): the FIRST press only REVEALS the
/// readout — you cannot cycle off a dial you cannot see, and
/// [`Dial::ShadowLift`] leads the cycle deliberately, so jumping straight
/// past it was wrong. Every press while the readout is up advances by one,
/// wrapping; presses coalesced into one drain still count past the reveal.
fn dial_after_cycle(idx: usize, presses: u32, readout_up: bool) -> usize {
    let advance = presses.saturating_sub(u32::from(!readout_up)) as usize;
    (idx + advance) % Dial::ALL.len()
}

/// The per-video settings as the run loop lives them: which clip is on top,
/// what is saved for it, the dials and codec in force, and the info row that
/// reports them. Split out of [`Player::run`] (which owns the only clock and
/// hard-wires `AnsiBackend`) so the front-load rules are testable.
///
/// Codec precedence when a clip fronts: the viewer's last `/` choice this
/// session, else the builder's `codec` (`--codec`), else the codec saved for
/// that video, else the default. A `/` press is therefore never undone at a
/// composition cut, and it outranks `--codec` from the moment it is made.
/// The dials always come from the fronted video's saved settings (or the
/// defaults): they are per video.
#[derive(Debug)]
struct LiveSettings {
    /// The builder's `codec` (`--codec`).
    forced: Option<Codec>,
    /// The codec `/` last picked — holds across clip switches.
    session: Option<Codec>,
    fronted: Option<usize>,
    clip_name: String,
    /// What is on disk for the fronted clip.
    saved: Option<VideoSettings>,
    /// A load or save that failed, shown in the info row until the next
    /// clip switch or successful save.
    note: Option<&'static str>,
    /// Every settings failure, printed to stderr once the terminal is back.
    problems: Vec<String>,
    compose: ComposeParams,
    codec: Codec,
}

impl LiveSettings {
    fn new(forced: Option<Codec>) -> LiveSettings {
        LiveSettings {
            forced,
            session: None,
            fronted: None,
            clip_name: String::new(),
            saved: None,
            note: None,
            problems: Vec::new(),
            compose: ComposeParams::default(),
            codec: forced.unwrap_or_default(),
        }
    }

    /// Clip `idx` (file `path`) is on top. A clip coming to the front loads
    /// its saved settings (the defaults when it has none, or when the file
    /// does not parse — that is reported, not fatal). Returns whether the
    /// fronted clip changed, i.e. whether the caller must push the new
    /// dials and codec to the deck.
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
        self.compose = start.compose;
        self.codec = self.session.or(self.forced).unwrap_or(start.codec);
        true
    }

    /// `presses` of `/`: advance the codec and make it the session's choice.
    fn cycle(&mut self, presses: u32) {
        for _ in 0..presses {
            self.codec = self.codec.next();
        }
        self.session = Some(self.codec);
    }

    /// `s`: save the dials and codec for the fronted clip at `path`.
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

    /// The info row's settings word.
    fn status(&self) -> &'static str {
        let current = self.current();
        self.note.unwrap_or(match self.saved {
            Some(s) if s == current => "saved",
            None if current == VideoSettings::default() => "default",
            _ => "s to save",
        })
    }

    /// Rewrite the info row into `out` (a reused buffer).
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

/// How long the bottom-row progress overlay stays up after a seek before it
/// auto-hides (M5 scrub UX; hiding forces a clean full repaint of the
/// overlay row — see `pipeline::Player::set_progress_overlay`).
const OVERLAY_HIDE_AFTER: Duration = Duration::from_millis(1000);

/// Where the run loop is in the asset and whether it is moving (M6 pause).
///
/// Asset time is `base_frame` plus the wall time since `clock`; pausing
/// stops contributing that second term and parks the frozen frame in
/// `base_frame`, so resuming only has to repoint `clock` at the resume
/// instant — playback continues from the frame on screen instead of
/// jumping over everything the pause lasted for. Seeks repoint both, paused
/// or not, which is why a jump while frozen simply changes the frozen
/// frame. Split out of [`Player::run`] so the arithmetic is testable: the
/// loop itself owns the only clock and hard-wires `AnsiBackend`.
#[derive(Debug)]
struct Transport {
    /// Frame the pacing clock counts from.
    base_frame: u64,
    /// Pacing origin — repointed by every seek and by every resume, never
    /// by `duration_secs` (that stays wall clock, see [`Player::run`]).
    clock: Instant,
    paused: bool,
}

impl Transport {
    fn new(base_frame: u64, now: Instant) -> Transport {
        Transport { base_frame, clock: now, paused: false }
    }

    /// Seconds of asset time since the pacing origin — zero while paused,
    /// which is the whole of what freezing the picture means here.
    fn elapsed_secs(&self, now: Instant) -> f64 {
        if self.paused { 0.0 } else { now.duration_since(self.clock).as_secs_f64() }
    }

    /// Repoint to `frame` and restart the pacing clock (every 0–9 jump and
    /// arrow scrub). Paused stays paused: the frozen frame just moves.
    fn seek_to(&mut self, frame: u64, now: Instant) {
        self.base_frame = frame;
        self.clock = now;
    }

    /// Space: freeze at `frozen` (see [`freeze_target`]), or resume from
    /// wherever the freeze left us.
    fn toggle_pause(&mut self, frozen: u64, now: Instant) {
        if self.paused {
            self.clock = now; // the wall clock ran; asset time did not
            self.paused = false;
        } else {
            self.base_frame = frozen;
            self.paused = true;
        }
    }
}

/// Which frame Space freezes on: the one last PRESENTED, when there is one
/// (M6 pause).
///
/// The clock's answer is the wrong one. The run loop presents at
/// `--fps-cap` and only then sleeps, so at a 1 fps cap on a 30 fps asset
/// the clock has moved ~30 frames past the picture by the time the next
/// drain sees the press — and freezing there would jump the picture
/// forward a second at the exact moment the viewer asked it to stop.
/// Before the first present there is nothing on screen yet, so the clock
/// is all we have.
fn freeze_target(presented: Option<u64>, clock_frame: u64) -> u64 {
    presented.unwrap_or(clock_frame)
}

/// When the bottom-row progress overlay is on screen (M5 scrub UX + M6
/// pause). A seek shows it for [`OVERLAY_HIDE_AFTER`]; a pause holds it up
/// with no deadline at all, because that row is what tells a frozen picture
/// apart from a stalled one; resuming restarts the full timeout from the
/// resume instant. Split out of [`Player::run`] for the same reason as
/// [`HintState`] — the loop owns the only clock.
#[derive(Debug, Default)]
struct ProgressTimer {
    /// When the row auto-hides; `None` is either "not on screen" or
    /// "paused, so there is no deadline to reach".
    until: Option<Instant>,
}

impl ProgressTimer {
    /// One run-loop step. `restart` is true on a seek or a resume — both
    /// want the whole timeout from now. Returns whether the row belongs on
    /// screen, which is also what the hints row rides on.
    fn visible(&mut self, now: Instant, restart: bool, paused: bool) -> bool {
        if paused {
            self.until = None; // suspended, not expired
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

/// Whether a frozen picture has to be painted again (M6 pause).
///
/// A paused loop still ticks — it has to, or it would stop answering the
/// keyboard — but the frame on screen is already correct. Re-running
/// resample → compose → present for it costs most of a core and, under
/// `RepaintMode::Full`, tens of megabytes a second of escape stream for a
/// picture that is not moving. So while paused, a tick paints only if
/// something actually changed: a key, a seek, a resize, an overlay
/// appearing or timing out. Playing always paints, because the picture is
/// moving by definition.
#[derive(Debug, Default)]
struct RepaintGate {
    /// Consecutive ticks skipped — diagnostics, and what the test reads to
    /// prove the loop is idling rather than spinning.
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

/// How long the key-hints row stays up at start-up (M6, PLAN-M6-M8 §1).
/// Long enough to read six items, short enough that it is gone before anyone
/// settles into the picture — after that the row is earned, not given.
const HINT_STARTUP_SHOW_FOR: Duration = Duration::from_millis(3000);

/// Visibility policy for the key-hints row (M6, PLAN-M6-M8 §1): it rides
/// with whichever transient overlay is up, shows for
/// [`HINT_STARTUP_SHOW_FOR`] at start-up, and is pinned open by `v` until
/// the next press. Split out of the run loop so the timing rules are
/// testable — [`Player::run`] owns the only clock and hard-wires
/// `AnsiBackend`, so there is nothing headless to drive it through.
#[derive(Debug)]
struct HintState {
    /// When the start-up window lapses. Cut short by the first `v` press
    /// (a deliberate press ends the freebie) and never re-armed.
    startup_until: Instant,
    /// The pin `v` last set — the OPPOSITE of what was on screen when it was
    /// pressed, so one key both summons and dismisses the row.
    sticky: bool,
}

impl HintState {
    fn new(now: Instant) -> HintState {
        HintState { startup_until: now + HINT_STARTUP_SHOW_FOR, sticky: false }
    }

    /// One run-loop step: fold in this drain's `v` press and report
    /// whether the row belongs on screen now. `overlays_up` is true while the
    /// progress or dial overlay is visible — the hints ride along with them,
    /// since a viewer touching those keys is exactly who wants the legend.
    ///
    /// A press reads the PIN first, the start-up window second: pinned →
    /// unpin, start-up freebie → dismiss it, anything else → pin. Toggling
    /// against "is the row on screen" instead looks right until an overlay
    /// is up for a long time — during a pause the progress row never goes
    /// away, so `v` could only ever unpin, and a press would silently take
    /// down a legend the viewer had pinned. The overlays keep their veto
    /// either way: the row cannot be dismissed out from under the bar it
    /// belongs to, it just stops riding along once that bar goes.
    fn visible(&mut self, now: Instant, toggle: bool, overlays_up: bool) -> bool {
        let in_startup = now < self.startup_until;
        if !toggle {
            return self.sticky || overlays_up || in_startup;
        }
        self.startup_until = now; // a deliberate press ends the start-up window
        self.sticky = if self.sticky {
            false // pinned: the press takes it down
        } else {
            // Not pinned. The start-up row is a freebie, so a press there
            // means "go away". Anything else — an overlay's ride-along, or
            // nothing on screen at all — means "stay up", which the old
            // rule could not express: during a pause the progress row is up
            // for minutes, so `v` could only ever fail to pin.
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
    font_table: Option<String>,
    codec: Option<Codec>,
}

impl PlayerBuilder {
    /// Path of the ASCI asset to play. Required, unless
    /// [`composition`](PlayerBuilder::composition) is set instead.
    pub fn asset(mut self, path: impl Into<PathBuf>) -> Self {
        self.asset = Some(path.into());
        self
    }

    /// Path of a composition `.toml` to play instead of one asset
    /// (PLAN-M6-M8 §3). The timeline is the composition's: `--seek`, the
    /// digits, the arrows, `--loop` and the progress row all count its
    /// frames, clips switch decoders at their boundaries and a gap plays
    /// black. Bare library names inside the file resolve through
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
    /// probe volley entirely — the PLAN §3.1 escape hatch). `None` (the
    /// default) probes.
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
    /// and resizes unserviced for arbitrarily long (and tiny values overflow
    /// `Duration`, an M5 fix-3 panic after the terminal session started).
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
    /// letterbox math (PLAN §3.2). Default: the terminal's reported cell
    /// pixel size, else 2.0.
    pub fn cell_aspect(mut self, cell_aspect: f64) -> Self {
        self.cell_aspect = Some(cell_aspect);
        self
    }

    /// Start playback this many seconds in (FIDX keyframe seek, PLAN §4).
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
    /// only (PLAN §3.1 escape hatch for hostile PTYs).
    pub fn no_query(mut self, no_query: bool) -> Self {
        self.no_query = no_query;
        self
    }

    /// Bypass the capability-probe cache (no read, no write).
    pub fn no_cache(mut self, no_cache: bool) -> Self {
        self.no_cache = no_cache;
        self
    }

    /// Skip the identity-keyed quirk table (M5, PLAN §3.1): the probe
    /// normally corrects known reply gaps after the volley — e.g. kitty's
    /// missing XTGETTCAP `RGB` entry, or plain xterm's "no direct color"
    /// answer overriding a stale `COLORTERM` — keyed on the terminal's own
    /// XTVERSION reply, never on `TERM`. This escape hatch takes the replies
    /// at face value: the probe cache is bypassed in both directions (cached
    /// entries embed quirk adjustments, so reading one would serve exactly
    /// the corrected result this flag disables; and a no-quirks result is
    /// never stored, because cached entries must be the fully adjusted
    /// truth) and the volley always runs fresh.
    pub fn no_quirks(mut self, no_quirks: bool) -> Self {
        self.no_quirks = no_quirks;
        self
    }

    /// Assert which font the terminal renders with, by ink-coverage table
    /// (PLAN §3.4 `--font-table`): a built-in name — `conservative`,
    /// `dejavu-sans-mono`, `liberation-mono`, `ubuntu-mono`,
    /// `noto-sans-mono` — or a path to a `auto-ascii-factory font-table` TOML.
    /// Terminals cannot be queried for their font, so this is user-asserted
    /// truth: the table's recorded repertoire vetoes the palette selection
    /// (a tier whose glyphs the font is missing degrades braille → unicode
    /// → ascii instead of drawing missing-glyph boxes). Resolved and
    /// validated at [`build`](PlayerBuilder::build).
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
        // Single assets are a one-clip composition (PLAN-M6-M8 §3): one
        // timeline, one time→frame function, no second code path. Resolving
        // reads every clip's header — an unreadable, empty or luma-less clip
        // fails HERE, before the terminal is touched.
        let mut comp = match &self.composition {
            None => Composition::single(&path),
            Some(file) => Composition::from_toml_file(
                file,
                Composition::default_library_dir().as_deref(),
            )?,
        };
        comp.resolve()?;
        // One bound check for every entry point (`--seek` here, in `--sim`
        // and in `--bench-seek`): the timeline owns it.
        let start_frame = match self.seek_secs {
            Some(secs) => comp.frame_at_secs(secs)?,
            None => 0,
        };
        // Resolve --font-table early (M5 §3.4): a bad name/path/table is a
        // config error before any terminal state changes.
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
    /// The timeline, resolved. A plain asset is a one-clip composition, so
    /// the run loop has exactly one time→frame path (PLAN-M6-M8 §3).
    comp: Composition,
    cfg: PlayerBuilder,
    start_frame: u32,
    /// Parsed §3.4 font coverage table (repertoire veto), from
    /// [`PlayerBuilder::font_table`].
    font_table: Option<auto_ascii_core::FontTable>,
}

/// Cell aspect: explicit override > terminal-reported cell pixel size > 2.0
/// (PLAN §3.2).
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

    /// Play the asset: probe the terminal (PLAN §3.1 DA1-sentinel volley,
    /// unless a tier is forced or [`no_query`](PlayerBuilder::no_query) is
    /// set), enter the session (alt screen, raw mode, hidden cursor), run
    /// the wall-clock-paced frame loop, and restore the terminal — also on
    /// panic, SIGINT and SIGTERM (the restore hooks are armed before the
    /// screen is touched).
    ///
    /// Blocks until the asset ends (unless [`looping`](PlayerBuilder::looping)),
    /// the configured [`duration`](PlayerBuilder::duration_secs) elapses, or
    /// the user quits (`q` / `Esc` / `Ctrl-C`). Keys `0`–`9` jump to that
    /// ×10% of the asset; `←`/`→` scrub ±[`SCRUB_STEP_SECS`] (5 s); `d`
    /// cycles the live [`Dial`]s and `[`/`]` turn the selected one; `/`
    /// cycles the glyph [`Codec`]; `s` saves the dials and codec as this
    /// video's settings (`<asset>.player.toml`, loaded whenever the video
    /// fronts); space
    /// freezes the picture and resumes it from the frozen frame (jumps and
    /// scrubs still work while frozen, and stay frozen). Every
    /// seek flashes a bottom-row progress overlay that auto-hides after ~1 s.
    /// A key-hints row sits above it whenever an overlay is up, for the first
    /// few seconds of playback, and for as long as `v` pins it open (M6,
    /// PLAN-M6-M8 §1).
    ///
    /// # Errors
    /// [`Error::Terminal`] when stdout is not a TTY (headless callers want
    /// [`crate::RenderSession`]); [`Error::Decode`] on mid-playback asset
    /// corruption.
    pub fn run(self) -> Result<(), Error> {
        // Capability probe BEFORE the backend touches the terminal — the
        // volley manages its own termios. A forced tier or no_query skips
        // the volley entirely (escape hatches); the forced tier still wins
        // inside probe_caps.
        let probe_opts = ProbeOptions {
            forced_tier: self.cfg.tier,
            no_query: self.cfg.no_query || self.cfg.tier.is_some(),
            no_quirks: self.cfg.no_quirks,
            no_cache: self.cfg.no_cache,
            ..ProbeOptions::default()
        };
        let caps = probe_caps(&probe_opts);

        // AnsiBackend::new arms restore + installs panic/SIGINT/SIGTERM/
        // atexit hooks before touching the terminal (M0 acceptance 3,
        // pty-tested in auto-ascii-term). Errors cleanly if stdout is not a TTY.
        let mut backend = AnsiBackend::new(caps).map_err(Error::Terminal)?;
        let aspect = resolve_cell_aspect(self.cfg.cell_aspect, backend.caps().cell_px);
        // Palette selection inputs from Caps (PLAN §3.4 key: charset tier ×
        // color depth; density falls out of the viewport at reflow).
        let depth = pipeline::color_depth(backend.caps().color);
        let mut glyphs = self.cfg.palette.resolve_for_caps(backend.caps());
        // §3.4 font-table repertoire veto: the user asserted a font; degrade
        // any tier whose glyph surface that font cannot render.
        if let Some(t) = &self.font_table {
            glyphs = t.veto_tier(glyphs);
        }
        // One decode pipeline per clip, built as the timeline reaches them
        // (a single asset is one clip, opened at the first render).
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
        backend.resize(cols, rows); // implies invalidate; clips reflow as they front
        deck.set_size(cols, rows);

        let asset_fps = self.comp.fps();
        let present_fps = match self.cfg.fps_cap {
            Some(cap) => cap.min(asset_fps), // >= MIN_FPS_CAP checked at build()
            None => asset_fps,
        };
        let tick = Duration::from_secs_f64(1.0 / present_fps);
        let frame_count = u64::from(self.comp.frame_count());
        let clip_count = self.comp.clips().len();
        let (fps_num, fps_den) = self.comp.fps_ratio();
        let t0 = Instant::now(); // duration_secs origin (never reset by jumps)
        // Asset position + whether it is advancing (M6 pause). `t0` stays
        // the wall-clock origin: `duration_secs` is a wall-clock budget by
        // definition, so a pause spends it like playback does.
        let mut transport = Transport::new(u64::from(self.start_frame), t0);
        let mut next_tick = t0;
        // M5 scrub UX: the transient progress overlay auto-hides this long
        // after the last seek — and stays up for as long as a pause lasts.
        let mut progress = ProgressTimer::default();
        // Live dials (M6 tuning UX): `d` selects, `[`/`]` turns. Starts from
        // whatever the CLI/params handed the pipeline, so a --shadow-lift on
        // the command line is simply the dial's opening position.
        let mut dial_idx: usize = 0;
        // The dials open where the pipeline starts: nothing on this path
        // overrides the §3.5 defaults (the eval driver is the only caller
        // that does, and it never builds a terminal Player).
        let mut dial_until: Option<Instant> = None;
        // The dials, the glyph codec (`/` cycles it) and the per-video
        // settings (`s` saves dials + codec beside the clip; they load as
        // each clip fronts) — see LiveSettings for the precedence rules.
        let mut live = LiveSettings::new(self.cfg.codec);
        deck.set_codec(live.codec);
        // `/` and `s` raise the controls overlay for as long as a dial turn
        // does — the info row in it is where their answer shows.
        let mut note_until: Option<Instant> = None;
        let mut info = String::new();
        // M6 key hints: the legend row above the overlay row (PLAN-M6-M8 §1).
        let mut hints = HintState::new(t0);
        // M6 pause: a frozen picture is painted once, not every tick.
        let mut gate = RepaintGate::default();
        let mut presented: Option<u64> = None; // last frame actually painted
        let (mut was_progress, mut was_hints) = (false, false);
        let mut was_dial = false;
        let mut was_size = deck.size();

        loop {
            let drained = deck.drain_events(&mut backend);
            if drained.quit {
                break;
            }
            let mut sought = false;
            // Space first: a seek landing in the same drain then moves the
            // frozen frame, which is the order that reads right either way.
            let mut resumed = false;
            if drained.toggle_pause {
                let now = Instant::now();
                let elapsed = transport.elapsed_secs(now);
                let clock_frame = self.comp.frame_after(transport.base_frame, elapsed);
                let clock_frame =
                    if self.cfg.looping { clock_frame % frame_count } else { clock_frame };
                // Freeze on the picture, not on the clock (see freeze_target).
                transport.toggle_pause(freeze_target(presented, clock_frame), now);
                resumed = !transport.paused;
                deck.set_paused(transport.paused);
            }
            if let Some(d) = drained.jump_digit {
                // 0–9 → jump to d×10% (PLAN §3.6; decode goes through the
                // FIDX seek path automatically via the loaded-frame tracker,
                // and drain_events already reset the hysteresis state — a
                // seek must not ghost pre-seek edges/indices into the
                // landing frame).
                transport.seek_to(frame_count * u64::from(d) / 10, Instant::now());
                sought = true;
            }
            if drained.seek_steps != 0 {
                // Left/Right → ±SCRUB_STEP_SECS from the frame currently on
                // the clock (post-digit-jump if both landed in one drain),
                // clamped to the asset; same FIDX-seek + state-reset
                // machinery as digit jumps.
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
                    // Read BEFORE this drain arms it below: the first `d` on a
                    // hidden readout shows the current dial, it does not cycle.
                    dial_idx = dial_after_cycle(dial_idx, drained.dial_cycle, dial_until.is_some());
                }
                let dial = Dial::ALL[dial_idx];
                if drained.dial_delta != 0 {
                    dial.turn(&mut live.compose, drained.dial_delta);
                    // Renderer-only: re-tunes the asset already in memory, no
                    // rebuild. The pipeline resets its hysteresis memory on
                    // the change, so the next frame IS the new position.
                    deck.set_compose_params(live.compose);
                }
                deck.set_dial_overlay(Some((dial.label(), dial.get(&live.compose), dial.max())));
                dial_until = Some(Instant::now() + DIAL_OVERLAY_HIDE_AFTER);
            } else if dial_until.is_some_and(|t| Instant::now() >= t) {
                deck.set_dial_overlay(None);
                dial_until = None;
            }
            if drained.codec_cycle > 0 {
                live.cycle(drained.codec_cycle);
                // Renderer-only, like a dial: the pipeline resets its
                // temporal state on the change, so the next frame is a cold
                // start in the new codec.
                deck.set_codec(live.codec);
            }
            if drained.save
                && let Some(idx) = live.fronted
            {
                live.save(&self.comp.clips()[idx].path);
            }
            if drained.codec_cycle > 0 || drained.save {
                note_until = Some(Instant::now() + DIAL_OVERLAY_HIDE_AFTER);
            } else if note_until.is_some_and(|t| Instant::now() >= t) {
                note_until = None;
            }
            // Hiding goes through set_progress_overlay(false), which
            // schedules the backend invalidate that repaints the row
            // underneath (the M5 diff contract).
            let show_progress =
                progress.visible(Instant::now(), sought || resumed, transport.paused);
            deck.set_progress_overlay(show_progress);
            // Both of these are exactly "that overlay is on screen", so the
            // hints row rides with them — including for the whole of a pause
            // (PLAN-M6-M8 §1).
            let overlays_up = show_progress || dial_until.is_some() || note_until.is_some();
            let show_hints = hints.visible(Instant::now(), drained.toggle_hints, overlays_up);
            deck.set_hint_overlay(show_hints);
            if let Some(dur) = self.cfg.duration_secs
                && t0.elapsed().as_secs_f64() >= dur
            {
                break;
            }
            // Pacing (§3.6 step 2): target frame by wall clock — if we fell
            // behind, this skips asset frames (latest-frame-wins, never
            // queued).
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
            // Composition time → the clip on top and its own frame. A plain
            // asset is one clip and maps frame to frame.
            let located = self.comp.locate_frame(target as u32);
            // A clip coming to the front brings its own saved settings (or
            // the defaults): they are per video, so a stitch re-tunes at the
            // cut. A gap keeps whatever was showing.
            if let Some(l) = located
                && live.front(l.clip_idx, &self.comp.clips()[l.clip_idx].path)
            {
                deck.set_compose_params(live.compose);
                deck.set_codec(live.codec);
            }
            // The controls overlay's info row: what is playing, in which
            // codec, and whether that is what is saved for it. Rebuilt in a
            // reused buffer; the deck copies it only when the text changes.
            live.write_info(&mut info);
            deck.set_info_overlay(Some(&info));
            if self.comp.is_stitch() {
                // Only a composition retimes the row: a plain asset keeps
                // the M6 overlay, printed from its own frame counter.
                deck.set_progress_context(Some(ProgressContext {
                    frame: target as u32,
                    frame_count: self.comp.frame_count(),
                    fps_num,
                    fps_den,
                    clip: located.map(|l| (l.clip_idx + 1, clip_count)),
                }));
            }
            // Anything that can change what is on screen. While playing
            // the picture moves on its own, so this only decides whether a
            // FROZEN frame has to be painted again.
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
                // One call for both cases: the clip on top at its own
                // frame, or a gap painted black with the overlays still on
                // it.
                deck.present_at(&mut backend, located)?;
                presented = Some(target);
            }

            next_tick += tick;
            let now = Instant::now();
            if next_tick > now {
                std::thread::sleep(next_tick - now);
            } else {
                next_tick = now; // behind: render immediately, drop the deficit
            }
        }
        backend.shutdown();
        // The session could not say this while it owned the screen: a
        // settings file that would not parse (the video played on its
        // defaults) or would not save — say which, now the terminal is back.
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

    /// M5 fix 3 regression: a tiny positive cap used to pass the old `> 0`
    /// check and panic inside run() (`Duration::from_secs_f64(1/cap)`
    /// overflow / a years-long pacing sleep) AFTER the terminal session had
    /// started. The documented floor is [`MIN_FPS_CAP`] = 1 fps, enforced at
    /// build() where failure is still clean.
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
        // At the floor exactly, validation passes — the missing file (Io) is
        // the next check, proving the cap itself was accepted.
        let e = Player::builder()
            .asset("/no/such/file.ascii")
            .fps_cap(MIN_FPS_CAP)
            .build()
            .unwrap_err();
        assert!(matches!(e, Error::Io { .. }), "cap at the floor is valid: {e}");
    }

    /// M5 item B: `--font-table` is resolved and validated at build() — a
    /// bad name or unparseable file fails before any terminal state changes;
    /// built-in names resolve.
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
        // The veto input is the parsed repertoire (researched: Liberation
        // Mono has no ╱╲) — the run()-time tier degrade consumes this.
        assert_eq!(
            p.font_table.unwrap().veto_tier(auto_ascii_core::GlyphTier::UnicodeBlocks),
            auto_ascii_core::GlyphTier::Ascii
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn build_rejects_non_ascii_files() {
        // This manifest exists but is not an ASCI container.
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let e = Player::builder().asset(manifest).build().unwrap_err();
        assert!(matches!(e, Error::Format { .. }), "{e}");
    }

    /// A structurally corrupt asset must fail at `build()`, not once the
    /// terminal session is up: the header of this one parses perfectly and
    /// only the tail (FIDX, TRLR) is gone, so nothing short of opening the
    /// container catches it.
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

    /// M6 key hints (PLAN-M6-M8 §1): the run loop's visibility policy —
    /// start-up window, ride-along with the overlays, sticky `v`. Driven
    /// with synthetic instants because `run()` owns the only clock and
    /// hard-wires `AnsiBackend`: there is no seam to hand a SimBackend or a
    /// fake clock, and inventing one is out of scope here. What the row then
    /// DRAWS, and that hiding it forces the full repaint, is covered
    /// end-to-end in tests/scrub_overlay.rs.
    #[test]
    fn hint_row_shows_at_start_up_then_rides_the_overlays() {
        let t0 = Instant::now();
        let mut hints = HintState::new(t0);

        // Start-up window: up with no key pressed and no overlay on screen.
        assert!(hints.visible(t0, false, false), "hints show at start-up");
        let last = t0 + HINT_STARTUP_SHOW_FOR - Duration::from_millis(1);
        assert!(hints.visible(last, false, false), "still up inside the window");
        assert!(!hints.visible(t0 + HINT_STARTUP_SHOW_FOR, false, false), "window lapses");

        // Long after the window: an overlay pulls the row back for its life.
        let late = t0 + HINT_STARTUP_SHOW_FOR + Duration::from_secs(60);
        assert!(hints.visible(late, false, true), "rides with a visible overlay");
        assert!(!hints.visible(late, false, false), "and leaves with it");

        // `v` pins it open until the next press — timers do not override.
        assert!(hints.visible(late, true, false), "v pins the row open");
        assert!(hints.visible(late, false, false), "and it stays pinned");
        assert!(!hints.visible(late, true, false), "a second v unpins it");
    }

    /// `v` reads the pin first, then the start-up window: a press
    /// inside the start-up window dismisses that freebie and ends it,
    /// while a press with an overlay up (or nothing up) pins the row.
    #[test]
    fn hint_press_dismisses_the_start_up_row_and_pins_otherwise() {
        let t0 = Instant::now();
        let mut hints = HintState::new(t0);
        assert!(hints.visible(t0, false, false), "the start-up row is up");
        assert!(!hints.visible(t0, true, false), "v during start-up dismisses it");
        let tick = t0 + Duration::from_millis(1);
        assert!(!hints.visible(tick, false, false), "and the window does not bring it back");
        assert!(hints.visible(tick, true, false), "the next v summons it again");

        // A press while an overlay is up PINS (addendum 3b): the press
        // cannot mean "dismiss" — the overlay keeps the row up regardless —
        // so the only useful reading is "keep it once the bar goes".
        let mut hints = HintState::new(t0);
        let late = t0 + HINT_STARTUP_SHOW_FOR + Duration::from_secs(60);
        assert!(hints.visible(late, false, true), "an overlay pulls the row up");
        assert!(hints.visible(late, true, true), "v pins it while the bar is up");
        assert!(hints.visible(late, false, false), "and it stays after the bar goes");
        assert!(!hints.visible(late, true, false), "the next press unpins");
    }

    /// Addendum 3b regression: a pause holds the progress row up for as
    /// long as the viewer likes, so "toggle against what is on screen"
    /// left `v` unable to EVER pin the legend — and a press would silently
    /// unpin one that was already pinned. The pin is read first now.
    #[test]
    fn hints_can_be_pinned_while_the_progress_row_is_up() {
        let t0 = Instant::now();
        let mut hints = HintState::new(t0);
        let late = t0 + HINT_STARTUP_SHOW_FOR + Duration::from_secs(60);
        // Paused: the progress row is up indefinitely (ProgressTimer).
        assert!(hints.visible(late, true, true), "v pins during a pause");
        assert!(hints.visible(late, false, true), "and stays pinned");
        // A second press unpins — the row is still on screen only because
        // the paused progress row is, and it leaves with it.
        assert!(hints.visible(late, true, true), "a second v unpins");
        assert!(!hints.visible(late, false, false), "gone once the bar goes");
        // Nothing on screen at all: a press pins, as it always did.
        assert!(hints.visible(late, true, false), "v pins with nothing up");
        assert!(hints.visible(late, false, false), "and it stays");
    }

    /// M6 pause (run-loop transport): freezing parks the frame on screen and
    /// stops asset time; resuming continues FROM that frame instead of
    /// jumping over the wall time the pause cost. Driven with synthetic
    /// instants — `run()` owns the only clock — against the real
    /// `Composition::frame_after`, which is the loop's one time→frame
    /// expression, so the arithmetic under test is the shipping one.
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

        // Play for a second, then freeze on whatever is up.
        let at_pause = t0 + Duration::from_secs(1);
        let frozen = target(&tr, at_pause);
        assert_eq!(frozen, fps as u64, "one second of a {fps} fps asset");
        tr.toggle_pause(frozen, at_pause);
        assert!(tr.paused);

        // Two seconds of WALL clock pass: the picture does not move.
        let at_resume = at_pause + Duration::from_secs(2);
        assert_eq!(target(&tr, at_resume), frozen, "a pause freezes asset time");

        // Resume: the very next target is the frozen frame — not the frame
        // two seconds of wall clock later (frozen + 2·fps, 60 frames here).
        tr.toggle_pause(frozen, at_resume);
        assert!(!tr.paused);
        assert_eq!(target(&tr, at_resume), frozen, "resume continues, never skips");
        let two_on = at_resume + Duration::from_secs(2);
        assert_eq!(
            target(&tr, two_on),
            frozen + (2.0 * fps) as u64,
            "and then it advances at the asset rate from the frozen frame"
        );

        // A seek while frozen moves the frozen frame and stays frozen.
        tr.toggle_pause(target(&tr, two_on), two_on);
        tr.seek_to(7, two_on);
        assert!(tr.paused, "a jump while paused does not resume playback");
        assert_eq!(target(&tr, two_on + Duration::from_secs(5)), 7, "it lands and holds");
        let _ = std::fs::remove_file(&path);
    }

    /// M6 pause: the progress row's timeout. A seek shows the row for
    /// [`OVERLAY_HIDE_AFTER`]; pausing suspends the deadline so the row
    /// persists well past it; resuming restarts the full timeout from the
    /// resume instant rather than from the seek that preceded the pause.
    #[test]
    fn progress_row_timeout_is_suspended_while_paused() {
        let t0 = Instant::now();
        let mut row = ProgressTimer::default();
        let nearly = OVERLAY_HIDE_AFTER - Duration::from_millis(1);
        assert!(!row.visible(t0, false, false), "nothing has shown it yet");
        assert!(row.visible(t0, true, false), "a seek shows it");
        assert!(row.visible(t0 + nearly, false, false), "for the whole timeout");
        assert!(!row.visible(t0 + OVERLAY_HIDE_AFTER, false, false), "then it hides");

        // Paused: up, and still up long past the timeout.
        let at_pause = t0 + Duration::from_secs(10);
        assert!(row.visible(at_pause, true, true), "pausing shows the row");
        let long_after = at_pause + OVERLAY_HIDE_AFTER * 5;
        assert!(row.visible(long_after, false, true), "with the deadline suspended");

        // Resume: the timeout restarts from HERE.
        assert!(row.visible(long_after, true, false), "resume keeps it up");
        assert!(row.visible(long_after + nearly, false, false), "for a full timeout");
        assert!(!row.visible(long_after + OVERLAY_HIDE_AFTER, false, false), "then hides");
    }

    /// Addendum 2: a frozen picture is painted once. Ticking is how the
    /// loop keeps answering the keyboard, but re-composing and re-sending
    /// an unchanged frame at the cap rate is most of a core and tens of
    /// MB/s of escape stream for nothing.
    #[test]
    fn a_frozen_frame_is_painted_once_until_something_changes() {
        let mut gate = RepaintGate::default();
        // Playing: every tick paints, dirty or not — the picture moves.
        for _ in 0..5 {
            assert!(gate.should_paint(false, false));
        }
        assert_eq!(gate.skipped, 0);

        // The press that pauses is itself a change, so it paints...
        assert!(gate.should_paint(true, true));
        // ...and then the loop idles, however long the pause lasts.
        for _ in 0..1000 {
            assert!(!gate.should_paint(true, false));
        }
        assert_eq!(gate.skipped, 1000);

        // A seek, a resize, an overlay appearing or timing out: one paint
        // each, then idle again.
        assert!(gate.should_paint(true, true));
        assert_eq!(gate.skipped, 0);
        assert!(!gate.should_paint(true, false));
        // Resuming paints and keeps painting.
        assert!(gate.should_paint(false, true));
        assert!(gate.should_paint(false, false));
    }

    /// Addendum 3a: Space freezes on the frame the viewer is LOOKING at.
    /// With `--fps-cap 1` on a 30 fps asset the loop presents a frame and
    /// then sleeps a whole second, so the clock is ~30 frames ahead by the
    /// time the press is drained — freezing there jumped the picture
    /// forward a second at the moment it was asked to stop.
    #[test]
    fn pause_freezes_on_the_presented_frame_not_the_clock() {
        const FPS: f64 = 30.0;
        let t0 = Instant::now();
        let mut transport = Transport::new(0, t0);

        // One tick of a 1 fps cap: frame 0 was presented, then a second
        // passed before the next drain saw Space.
        let press = t0 + Duration::from_secs(1);
        let clock_frame = (transport.elapsed_secs(press) * FPS) as u64;
        assert_eq!(clock_frame, 30, "the clock really is 30 frames ahead");
        transport.toggle_pause(freeze_target(Some(0), clock_frame), press);
        assert!(transport.paused);
        assert_eq!(transport.base_frame, 0, "frozen on what was on screen");
        assert_eq!(transport.elapsed_secs(press + Duration::from_secs(9)), 0.0);

        // Resuming continues from the frozen frame, not from the clock.
        let resume = press + Duration::from_secs(9);
        transport.toggle_pause(freeze_target(Some(0), 0), resume);
        assert!(!transport.paused);
        assert_eq!(transport.base_frame, 0);
        let tick = resume + Duration::from_secs(1);
        assert_eq!((transport.elapsed_secs(tick) * FPS) as u64, 30, "one second on");

        // Before the first present there is nothing on screen, so the
        // clock is all there is to freeze on.
        assert_eq!(freeze_target(None, 17), 17);
    }

    /// Two clips of a stitch, one with saved settings, one without — the
    /// front-load path `run()` takes at every cut, driven headlessly.
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

        // `/` on clip b is the session's choice: it survives both cuts,
        // over clip a's saved codec — while the dials stay per video.
        live.cycle(1);
        assert_eq!((live.codec, live.status()), (Codec::Letters, "s to save"));
        live.front(0, &a);
        assert_eq!((live.codec, live.compose.shadow_lift), (Codec::Letters, 64));
        live.cycle(1);
        live.front(1, &b);
        assert_eq!((live.codec, live.compose.shadow_lift), (Codec::Pixels, 0), "the / pick holds");

        // `s` saves for the fronted clip; the next visit loads it.
        live.save(&b);
        assert_eq!(live.status(), "saved");
        live.front(0, &a);
        live.front(1, &b);
        assert_eq!(live.saved, Some(VideoSettings { compose: ComposeParams::default(), codec: Codec::Pixels }));
        assert!(live.problems.is_empty());
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

    /// M6 review fix: the first `d` REVEALS the dial readout rather than
    /// cycling past shadow lift; once the readout is up, `d` cycles as
    /// before. The index arithmetic is the only testable part of that path —
    /// the rest is `run()`'s hard-wired backend and clock.
    #[test]
    fn first_d_reveals_the_dial_before_it_cycles() {
        // Readout hidden: one `d` lands on the dial already selected.
        assert_eq!(dial_after_cycle(0, 1, false), 0, "first d shows shadow lift");
        // Readout up: the same press advances.
        assert_eq!(dial_after_cycle(0, 1, true), 1, "d cycles once the readout is up");
        // Presses coalesced in one drain still count past the reveal...
        assert_eq!(dial_after_cycle(0, 3, false), 2);
        // ...and the cycle wraps.
        assert_eq!(dial_after_cycle(2, 2, true), 1);
        // A drain with no `d` never moves the selection.
        assert_eq!(dial_after_cycle(1, 0, false), 1);
    }
}
