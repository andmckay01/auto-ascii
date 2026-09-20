//! [`Player`] — the blocking terminal player (M4 item A, feature
//! `terminal`): probe the terminal, enter a session, play, restore. The
//! whole M0–M3 machinery behind two calls:
//!
//! ```no_run
//! auto_ascii::Player::builder().asset("intro.ascii").build()?.run()?;
//! # Ok::<(), auto_ascii::Error>(())
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use memmap2::Mmap;
use auto_ascii_core::ComposeParams;
use auto_ascii_format::AsciiReader;
use auto_ascii_term::{AnsiBackend, Backend, ColorTier, ProbeOptions, probe_caps};

use crate::error::Error;
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

    /// Apply a signed number of steps, saturating at the dial's ends.
    pub fn turn(self, p: &mut ComposeParams, steps: i32) {
        let cur = i32::from(self.get(p));
        let next = (cur + steps * self.step()).clamp(0, i32::from(self.max())) as u8;
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

/// How long the bottom-row progress overlay stays up after a seek before it
/// auto-hides (M5 scrub UX; hiding forces a clean full repaint of the
/// overlay row — see `pipeline::Player::set_progress_overlay`).
const OVERLAY_HIDE_AFTER: Duration = Duration::from_millis(1000);

/// How long the key-hints row stays up at start-up (M6, PLAN-M6-M8 §1).
/// Long enough to read six items, short enough that it is gone before anyone
/// settles into the picture — after that the row is earned, not given.
const HINT_STARTUP_SHOW_FOR: Duration = Duration::from_millis(3000);

/// Visibility policy for the key-hints row (M6, PLAN-M6-M8 §1): it rides
/// with whichever transient overlay is up, shows for
/// [`HINT_STARTUP_SHOW_FOR`] at start-up, and is pinned open by `?`/`h`
/// until the next press. Split out of the run loop so the timing rules are
/// testable — [`Player::run`] owns the only clock and hard-wires
/// `AnsiBackend`, so there is nothing headless to drive it through.
#[derive(Debug)]
struct HintState {
    /// When the start-up window lapses. Cut short by the first `?`/`h`
    /// press (a deliberate press ends the freebie) and never re-armed.
    startup_until: Instant,
    /// The pin `?`/`h` last set — the OPPOSITE of what was on screen when it
    /// was pressed, so one key both summons and dismisses the row.
    sticky: bool,
}

impl HintState {
    fn new(now: Instant) -> HintState {
        HintState { startup_until: now + HINT_STARTUP_SHOW_FOR, sticky: false }
    }

    /// One run-loop step: fold in this drain's `?`/`h` press and report
    /// whether the row belongs on screen now. `overlays_up` is true while the
    /// progress or dial overlay is visible — the hints ride along with them,
    /// since a viewer touching those keys is exactly who wants the legend.
    ///
    /// A press toggles against what is ON SCREEN, not against `sticky`
    /// alone: pressing `?` while the start-up row (or an overlay's
    /// ride-along) is up must dismiss it rather than silently pin it for the
    /// rest of playback and leave the next press reading inverted. The
    /// overlays keep their veto either way — the row cannot be dismissed out
    /// from under the bar it belongs to, it just does not stick once that
    /// bar goes.
    fn visible(&mut self, now: Instant, toggle: bool, overlays_up: bool) -> bool {
        let showing = self.sticky || overlays_up || now < self.startup_until;
        if !toggle {
            return showing;
        }
        self.startup_until = now; // a deliberate press ends the start-up window
        self.sticky = !showing;
        self.sticky || overlays_up
    }
}

/// Builder for [`Player`] — see [`Player::builder`]. Every option has a
/// sensible default; only [`asset`](PlayerBuilder::asset) is required.
#[derive(Debug, Default)]
#[must_use = "call .build() to open the asset"]
pub struct PlayerBuilder {
    asset: Option<PathBuf>,
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
}

impl PlayerBuilder {
    /// Path of the ASCI asset to play. Required.
    pub fn asset(mut self, path: impl Into<PathBuf>) -> Self {
        self.asset = Some(path.into());
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

    /// Stop after this many seconds of wall clock (default: play to end).
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

    /// Open and validate the asset (memory-mapped) and check the
    /// configuration. Does NOT touch the terminal — that happens in
    /// [`Player::run`], so a bad path or corrupt file fails cleanly before
    /// any screen state changes.
    pub fn build(self) -> Result<Player, Error> {
        let path = self.asset.clone().ok_or_else(|| {
            Error::Config("no asset path set (PlayerBuilder::asset is required)".into())
        })?;
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
        let file = std::fs::File::open(&path)
            .map_err(|source| Error::Io { path: path.clone(), source })?;
        // SAFETY: read-only private map of a file we never mutate through
        // this mapping; assets are not truncated mid-playback (the same
        // assumption every mmap'd reader makes).
        let map = unsafe { Mmap::map(&file) }
            .map_err(|source| Error::Io { path: path.clone(), source })?;
        let reader = AsciiReader::open(&map)
            .map_err(|source| Error::Format { path: path.clone(), source })?;
        let header = reader.header();
        // Belt-and-braces: AsciiReader::open rejects zero fps since M1, but a
        // zero here would reach Duration::from_secs_f64(1/0.0) and panic.
        if header.fps_num == 0 || header.fps_den == 0 {
            return Err(Error::Asset("corrupt header: fps_num or fps_den == 0"));
        }
        let asset_fps = f64::from(header.fps_num) / f64::from(header.fps_den);
        let start_frame = match self.seek_secs {
            Some(secs) if secs.is_finite() && secs >= 0.0 => {
                let frame = (secs * asset_fps).floor();
                if frame >= f64::from(reader.frame_count()) {
                    return Err(Error::Config(format!(
                        "seek {secs}s is past the end of the asset ({} frames @ {asset_fps} fps)",
                        reader.frame_count()
                    )));
                }
                frame as u32
            }
            Some(secs) => {
                return Err(Error::Config(format!("seek must be finite and >= 0 (got {secs}s)")));
            }
            None => 0,
        };
        // Resolve --font-table early (M5 §3.4): a bad name/path/table is a
        // config error before any terminal state changes.
        let font_table = match &self.font_table {
            None => None,
            Some(spec) => Some(crate::session::load_font_table(spec)?),
        };
        drop(reader); // run() re-opens over the owned map (cheap: header parse)
        Ok(Player { map, cfg: self, path, asset_fps, start_frame, font_table })
    }
}

/// A ready-to-run terminal player: asset opened and validated, terminal not
/// yet touched. Created by [`Player::builder`]; consumed by
/// [`Player::run`].
#[derive(Debug)]
pub struct Player {
    map: Mmap,
    cfg: PlayerBuilder,
    path: PathBuf,
    asset_fps: f64,
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
    /// cycles the live [`Dial`]s and `[`/`]` turn the selected one. Every
    /// seek flashes a bottom-row progress overlay that auto-hides after ~1 s.
    /// A key-hints row sits above it whenever an overlay is up, for the first
    /// few seconds of playback, and for as long as `?` (or `h`) pins it open
    /// (M6, PLAN-M6-M8 §1).
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
        let reader = AsciiReader::open(&self.map)
            .map_err(|source| Error::Format { path: self.path.clone(), source })?;
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
        let mut player = pipeline::Player::new(
            reader,
            aspect,
            self.cfg.repaint == RepaintMode::Full,
            depth,
            glyphs,
        )?;
        let (cols, rows) = backend.caps().cells;
        player.reflow(&mut backend, cols, rows);

        let present_fps = match self.cfg.fps_cap {
            Some(cap) => cap.min(self.asset_fps), // >= MIN_FPS_CAP checked at build()
            None => self.asset_fps,
        };
        let tick = Duration::from_secs_f64(1.0 / present_fps);
        let frame_count = u64::from(player.frame_count());
        let mut base_frame = u64::from(self.start_frame);
        let t0 = Instant::now(); // duration_secs origin (never reset by jumps)
        let mut clock = t0; // pacing origin, reset on 0–9 jumps + arrow scrubs
        let mut next_tick = t0;
        // M5 scrub UX: the transient progress overlay auto-hides this long
        // after the last seek.
        let mut overlay_until: Option<Instant> = None;
        // Live dials (M6 tuning UX): `d` selects, `[`/`]` turns. Starts from
        // whatever the CLI/params handed the pipeline, so a --shadow-lift on
        // the command line is simply the dial's opening position.
        let mut dial_idx: usize = 0;
        let mut compose = player.compose_params();
        let mut dial_until: Option<Instant> = None;
        // M6 key hints: the legend row above the overlay row (PLAN-M6-M8 §1).
        let mut hints = HintState::new(t0);

        loop {
            let drained = player.drain_events(&mut backend);
            if drained.quit {
                break;
            }
            let mut sought = false;
            if let Some(d) = drained.jump_digit {
                // 0–9 → jump to d×10% (PLAN §3.6; decode goes through the
                // FIDX seek path automatically via the loaded-frame tracker,
                // and drain_events already reset the hysteresis state — a
                // seek must not ghost pre-seek edges/indices into the
                // landing frame).
                base_frame = frame_count * u64::from(d) / 10;
                clock = Instant::now();
                sought = true;
            }
            if drained.seek_steps != 0 {
                // Left/Right → ±SCRUB_STEP_SECS from the frame currently on
                // the clock (post-digit-jump if both landed in one drain),
                // clamped to the asset; same FIDX-seek + state-reset
                // machinery as digit jumps.
                let pos = base_frame + (clock.elapsed().as_secs_f64() * self.asset_fps) as u64;
                let pos = if self.cfg.looping { pos % frame_count } else { pos };
                let delta =
                    (f64::from(drained.seek_steps) * SCRUB_STEP_SECS * self.asset_fps) as i64;
                let landing = (pos.min(frame_count - 1) as i64 + delta)
                    .clamp(0, frame_count as i64 - 1) as u64;
                base_frame = landing;
                clock = Instant::now();
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
                    dial.turn(&mut compose, drained.dial_delta);
                    // Renderer-only: re-tunes the asset already in memory, no
                    // rebuild, no temporal reset.
                    player.set_compose_params(compose);
                }
                player.set_dial_overlay(Some((dial.label(), dial.get(&compose), dial.max())));
                dial_until = Some(Instant::now() + DIAL_OVERLAY_HIDE_AFTER);
            } else if dial_until.is_some_and(|t| Instant::now() >= t) {
                player.set_dial_overlay(None);
                dial_until = None;
            }
            if sought {
                player.set_progress_overlay(true);
                overlay_until = Some(Instant::now() + OVERLAY_HIDE_AFTER);
            } else if overlay_until.is_some_and(|t| Instant::now() >= t) {
                // Auto-hide (~1 s): set_progress_overlay(false) schedules the
                // backend invalidate that repaints the row under the overlay.
                player.set_progress_overlay(false);
                overlay_until = None;
            }
            // Both `*_until` options are now exactly "that overlay is on
            // screen", so the hints row can simply ride with them; hiding it
            // goes through the same invalidate contract (PLAN-M6-M8 §1).
            let overlays_up = overlay_until.is_some() || dial_until.is_some();
            let show_hints = hints.visible(Instant::now(), drained.toggle_hints, overlays_up);
            player.set_hint_overlay(show_hints);
            if let Some(dur) = self.cfg.duration_secs
                && t0.elapsed().as_secs_f64() >= dur
            {
                break;
            }
            // Pacing (§3.6 step 2): target frame by wall clock — if we fell
            // behind, this skips asset frames (latest-frame-wins, never
            // queued).
            let mut target = base_frame + (clock.elapsed().as_secs_f64() * self.asset_fps) as u64;
            if target >= frame_count {
                if self.cfg.looping {
                    target %= frame_count;
                } else {
                    break;
                }
            }
            player.render_present(&mut backend, target as u32)?;

            next_tick += tick;
            let now = Instant::now();
            if next_tick > now {
                std::thread::sleep(next_tick - now);
            } else {
                next_tick = now; // behind: render immediately, drop the deficit
            }
        }
        backend.shutdown();
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

    /// M6 key hints (PLAN-M6-M8 §1): the run loop's visibility policy —
    /// start-up window, ride-along with the overlays, sticky `?`/`h`. Driven
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

        // `?`/`h` pins it open until the next press — timers do not override.
        assert!(hints.visible(late, true, false), "? pins the row open");
        assert!(hints.visible(late, false, false), "and it stays pinned");
        assert!(!hints.visible(late, true, false), "a second ? unpins it");
    }

    /// M6 review fix: `?`/`h` toggles against what is ON SCREEN. A press
    /// inside the start-up window dismisses the row and ends the window,
    /// instead of silently pinning it for the rest of playback and leaving
    /// the next press reading inverted; a press while an overlay is up
    /// cannot dismiss the ride-along, but must not leave the row pinned once
    /// that overlay lapses.
    #[test]
    fn hint_toggle_reads_what_is_on_screen_not_the_pin() {
        let t0 = Instant::now();
        let mut hints = HintState::new(t0);
        assert!(hints.visible(t0, false, false), "the start-up row is up");
        assert!(!hints.visible(t0, true, false), "? during start-up dismisses it");
        let tick = t0 + Duration::from_millis(1);
        assert!(!hints.visible(tick, false, false), "and the window does not bring it back");
        assert!(hints.visible(tick, true, false), "the next ? summons it again");

        // A press while an overlay is up: the ride-along still wins for as
        // long as the overlay lives, but the pin left behind is "hidden".
        let mut hints = HintState::new(t0);
        let late = t0 + HINT_STARTUP_SHOW_FOR + Duration::from_secs(60);
        assert!(hints.visible(late, false, true), "an overlay pulls the row up");
        assert!(hints.visible(late, true, true), "? cannot dismiss the ride-along");
        assert!(!hints.visible(late, false, false), "and it does not stick after it");
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
