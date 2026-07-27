//! [`Player`] — the blocking terminal player (M4 item A, feature
//! `terminal`): probe the terminal, enter a session, play, restore. The
//! whole M0–M3 machinery behind two calls:
//!
//! ```no_run
//! sleepytime::Player::builder().asset("intro.slpy").build()?.run()?;
//! # Ok::<(), sleepytime::Error>(())
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use memmap2::Mmap;
use slpy_format::SlpyReader;
use slpy_term::{AnsiBackend, Backend, ColorTier, ProbeOptions, probe_caps};

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
    no_cache: bool,
}

impl PlayerBuilder {
    /// Path of the SLPY asset to play. Required.
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
    /// slows the video down. Must be > 0 (checked at
    /// [`build`](PlayerBuilder::build)).
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

    /// Open and validate the asset (memory-mapped) and check the
    /// configuration. Does NOT touch the terminal — that happens in
    /// [`Player::run`], so a bad path or corrupt file fails cleanly before
    /// any screen state changes.
    pub fn build(self) -> Result<Player, Error> {
        let path = self.asset.clone().ok_or_else(|| {
            Error::Config("no asset path set (PlayerBuilder::asset is required)".into())
        })?;
        if let Some(cap) = self.fps_cap
            && (cap.is_nan() || cap <= 0.0)
        {
            return Err(Error::Config(format!("fps cap must be > 0 (got {cap})")));
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
        let reader = SlpyReader::open(&map)
            .map_err(|source| Error::Format { path: path.clone(), source })?;
        let header = reader.header();
        // Belt-and-braces: SlpyReader::open rejects zero fps since M1, but a
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
        drop(reader); // run() re-opens over the owned map (cheap: header parse)
        Ok(Player { map, cfg: self, path, asset_fps, start_frame })
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
}

/// Cell aspect: explicit override > terminal-reported cell pixel size > 2.0
/// (PLAN §3.2).
fn resolve_cell_aspect(flag: Option<f64>, cell_px: Option<(u16, u16)>) -> f64 {
    if let Some(a) = flag {
        return a;
    }
    match cell_px {
        Some((w, h)) if w > 0 && h > 0 => f64::from(h) / f64::from(w),
        _ => slpy_core::DEFAULT_CELL_ASPECT,
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
    /// ×10% of the asset.
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
            no_cache: self.cfg.no_cache,
            ..ProbeOptions::default()
        };
        let caps = probe_caps(&probe_opts);

        // AnsiBackend::new arms restore + installs panic/SIGINT/SIGTERM/
        // atexit hooks before touching the terminal (M0 acceptance 3,
        // pty-tested in slpy-term). Errors cleanly if stdout is not a TTY.
        let mut backend = AnsiBackend::new(caps).map_err(Error::Terminal)?;
        let reader = SlpyReader::open(&self.map)
            .map_err(|source| Error::Format { path: self.path.clone(), source })?;
        let aspect = resolve_cell_aspect(self.cfg.cell_aspect, backend.caps().cell_px);
        // Palette selection inputs from Caps (PLAN §3.4 key: charset tier ×
        // color depth; density falls out of the viewport at reflow).
        let depth = pipeline::color_depth(backend.caps().color);
        let glyphs = self.cfg.palette.resolve_for_caps(backend.caps());
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
            Some(cap) => cap.min(self.asset_fps), // > 0 checked at build()
            None => self.asset_fps,
        };
        let tick = Duration::from_secs_f64(1.0 / present_fps);
        let frame_count = u64::from(player.frame_count());
        let mut base_frame = u64::from(self.start_frame);
        let t0 = Instant::now(); // duration_secs origin (never reset by jumps)
        let mut clock = t0; // pacing origin, reset on 0–9 jumps
        let mut next_tick = t0;

        loop {
            let drained = player.drain_events(&mut backend);
            if drained.quit {
                break;
            }
            if let Some(d) = drained.jump_digit {
                // 0–9 → jump to d×10% (PLAN §3.6; decode goes through the
                // FIDX seek path automatically via the loaded-frame tracker,
                // and drain_events already reset the hysteresis state — a
                // seek must not ghost pre-seek edges/indices into the
                // landing frame).
                base_frame = frame_count * u64::from(d) / 10;
                clock = Instant::now();
            }
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
        let e = Player::builder().asset("/no/such/file.slpy").build().unwrap_err();
        assert!(matches!(e, Error::Io { .. }), "missing file: {e}");
        let e = Player::builder()
            .asset("/no/such/file.slpy")
            .fps_cap(0.0)
            .build()
            .unwrap_err();
        assert!(matches!(e, Error::Config(_)), "fps cap checked first: {e}");
        let e = Player::builder()
            .asset("/no/such/file.slpy")
            .cell_aspect(f64::NAN)
            .build()
            .unwrap_err();
        assert!(matches!(e, Error::Config(_)), "NaN cell aspect: {e}");
    }

    #[test]
    fn build_rejects_non_slpy_files() {
        // This manifest exists but is not an SLPY container.
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let e = Player::builder().asset(manifest).build().unwrap_err();
        assert!(matches!(e, Error::Format { .. }), "{e}");
    }
}
