//! `sleepy-player` — realtime terminal player (PLAN §3, M1).
//!
//! Frame loop (PLAN §3.6): drain events (resize → recompute viewport §3.2,
//! rebuild resampler §3.3, reset diff state, invalidate; digit keys 0–9 jump
//! to 0–90%) → wall-clock pacing with latest-frame-wins frame skipping (§3.6
//! step 2) → decode Y (+ C chroma on color tiers) with sequential delta rolls
//! or FIDX seek (§4) → resample to viewport → per-shot NORM levels folded
//! into a 256-entry LUT rebuilt on shot change (§3.5 auto-levels, no
//! per-frame pumping) → compose (base ramp glyph + chroma fg, §3.4/§3.5) →
//! `Backend::present`, with `--repaint full` (invalidate-every-frame) as the
//! default mode (PLAN §3.1/§7: one render path).
//!
//! The pipeline itself lives in [`sleepy_player::pipeline`] (M2: extracted
//! to the lib so `sleepy-factory eval` reuses it verbatim); this binary owns
//! the CLI, the clock and the tty.
//!
//! Startup runs the capability probe (PLAN §3.1 DA1-sentinel volley) unless
//! `--sim`, `--tier` or `--no-query` — the escape hatches skip the volley
//! entirely and rely on passive hints (plus the forced tier).
//!
//! Never run interactively without a TTY; headless verification uses
//! `--sim COLSxROWS:NFRAMES` (SimBackend), which renders N frames as fast as
//! possible and prints one JSON line of stats. `--sim-tier` selects the
//! simulated color tier and `--sim-dump PATH` captures the raw escape stream
//! for byte-level tier checks; `--sim-resize [COLSxROWS]` injects a resize
//! event at frame N/2 to prove reflow.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use memmap2::Mmap;
use sleepy_player::pipeline::Player;
use slpy_format::SlpyReader;
use slpy_term::{
    AnsiBackend, Backend, ColorTier, Event, ProbeOptions, SimBackend, probe_caps,
};

/// Repaint mode (PLAN §3.1: one render path — "full" is diff with
/// `invalidate()` every frame, the M0 kitty-target default per §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RepaintMode {
    /// Invalidate every frame → full escape stream each present (M0 default).
    Full,
    /// Pure diff: only damaged cells are rewritten; invalidate on resize only.
    Diff,
}

#[derive(Parser)]
#[command(name = "sleepy-player", version, about = "Play SLPY assets in the terminal (PLAN §3)")]
struct Cli {
    /// SLPY asset (mmap'd read-only via memmap2, PLAN §8).
    asset: PathBuf,

    /// Repaint mode (PLAN §7: M0 default is invalidate-every-frame).
    #[arg(long, value_enum, default_value_t = RepaintMode::Full)]
    repaint: RepaintMode,

    /// Loop playback instead of exiting at the last frame.
    #[arg(long = "loop")]
    loop_playback: bool,

    /// Cap the presentation rate below the asset fps (frames are still
    /// selected by wall clock, so capping skips asset frames — it never
    /// slows the video down).
    #[arg(long, value_name = "FPS")]
    fps_cap: Option<f64>,

    /// Cell aspect override `cell_h_px / cell_w_px` (PLAN §3.2). Default:
    /// derived from the terminal's reported cell pixel size, else 2.0.
    #[arg(long, value_name = "RATIO")]
    cell_aspect: Option<f64>,

    /// Stop after N seconds (interactive benching); default: play to end.
    #[arg(long, value_name = "SECS")]
    duration_secs: Option<f64>,

    /// Start at TIMESTAMP — plain seconds ("42.5") or colon form ("1:30",
    /// "0:01:30.5"). Uses the FIDX seek path (keyframe binary search +
    /// ≤ keyframe_ivl−1 delta rolls, PLAN §4). Interactively, keys 0–9 also
    /// jump to 0–90% of the asset.
    #[arg(long, value_name = "TIMESTAMP")]
    seek: Option<String>,

    /// Force the color tier (truecolor|256|16|mono) — skips the probe volley
    /// entirely (PLAN §3.1 escape hatch; passive env hints still fill the
    /// glyph repertoire).
    #[arg(long, value_name = "TIER")]
    tier: Option<ColorTier>,

    /// Never write the probe volley; passive env hints only (PLAN §3.1
    /// escape hatch for hostile PTYs).
    #[arg(long)]
    no_query: bool,

    /// Bypass the probe cache (no read, no write).
    #[arg(long)]
    no_cache: bool,

    /// Headless mode: render NFRAMES frames to SimBackend at COLSxROWS as
    /// fast as possible (no pacing), never touch the tty, print one JSON
    /// stats line (acceptance runs at 213x58:900 and 320x90:900).
    #[arg(long, value_name = "COLSxROWS:NFRAMES")]
    sim: Option<String>,

    /// With --sim: color tier of the simulated terminal (falls back to
    /// --tier; default truecolor) — headless tier byte checks.
    #[arg(long, value_name = "TIER", requires = "sim")]
    sim_tier: Option<ColorTier>,

    /// With --sim: write every presented frame's raw escape bytes to PATH
    /// (concatenated) for byte-level assertions on tier output.
    #[arg(long, value_name = "PATH", requires = "sim")]
    sim_dump: Option<PathBuf>,

    /// With --sim: inject a Resize event at frame NFRAMES/2 to the given
    /// size (default 100x40) — proves resize reflow; reported as
    /// `grid_after` in the JSON.
    #[arg(long, value_name = "COLSxROWS", requires = "sim",
          num_args = 0..=1, default_missing_value = "100x40")]
    sim_resize: Option<String>,
}

/// Parse "COLSxROWS" (e.g. "213x58").
fn parse_size(s: &str) -> Result<(u16, u16)> {
    let (c, r) = s.split_once(['x', 'X']).context("expected COLSxROWS")?;
    let cols: u16 = c.trim().parse().context("bad COLS")?;
    let rows: u16 = r.trim().parse().context("bad ROWS")?;
    if cols == 0 || rows == 0 {
        bail!("size must be at least 1x1");
    }
    Ok((cols, rows))
}

/// Parse "COLSxROWS:NFRAMES" (e.g. "213x58:900").
fn parse_sim_spec(s: &str) -> Result<((u16, u16), u64)> {
    let (size, n) = s
        .split_once(':')
        .context("expected COLSxROWS:NFRAMES (e.g. 213x58:900)")?;
    let size = parse_size(size)?;
    let nframes: u64 = n.trim().parse().context("bad NFRAMES")?;
    if nframes == 0 {
        bail!("NFRAMES must be >= 1");
    }
    Ok((size, nframes))
}

/// Parse a `--seek` timestamp: plain seconds ("42.5") or colon form
/// ("1:30", "0:01:30.5") — up to H:M:S, fractions allowed anywhere.
fn parse_timestamp(s: &str) -> Result<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        bail!("expected SECONDS, MM:SS or HH:MM:SS");
    }
    let mut secs = 0.0f64;
    for p in &parts {
        let v: f64 = p.trim().parse().with_context(|| format!("bad timestamp component {p:?}"))?;
        if !v.is_finite() || v < 0.0 {
            bail!("timestamp components must be finite and >= 0");
        }
        secs = secs * 60.0 + v;
    }
    Ok(secs)
}

/// Canonical tier tag for the `--sim` JSON line.
fn tier_tag(t: ColorTier) -> &'static str {
    match t {
        ColorTier::True => "truecolor",
        ColorTier::C256 => "256",
        ColorTier::C16 => "16",
        ColorTier::Mono => "mono",
    }
}

/// Headless acceptance path: N frames as fast as possible against SimBackend,
/// one JSON stats line on stdout (never touches the tty, never probes).
fn run_sim(mut player: Player<'_>, cli: &Cli, tier: ColorTier, start_frame: u32) -> Result<()> {
    let spec = cli.sim.as_deref().expect("run_sim requires --sim");
    let ((cols, rows), nframes) = parse_sim_spec(spec)?;
    let resize_to = cli.sim_resize.as_deref().map(parse_size).transpose()?;

    let mut backend = SimBackend::new(cols, rows);
    let mut caps = backend.caps().clone();
    caps.color = tier;
    backend.set_caps(caps);
    player.reflow(&mut backend, cols, rows);

    let mut dump = cli
        .sim_dump
        .as_ref()
        .map(|p| {
            std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))
        })
        .transpose()?;

    let resize_at = nframes / 2;
    let mut bytes_total: u64 = 0;
    let mut rendered: u64 = 0;
    let t0 = Instant::now();
    for i in 0..nframes {
        if let Some((rc, rr)) = resize_to
            && i == resize_at
        {
            // Through the event path — same code the interactive loop runs.
            backend.push_event(Event::Resize(rc, rr));
        }
        if player.drain_events(&mut backend).quit {
            break;
        }
        let frame_idx =
            ((u64::from(start_frame) + i) % u64::from(player.frame_count())) as u32;
        let stats = player.render_present(&mut backend, frame_idx)?;
        bytes_total += u64::from(stats.bytes);
        // Bytes are counted; don't hold 900 frames in RAM.
        let out = backend.take_output();
        if let Some(f) = dump.as_mut() {
            f.write_all(&out)?;
        }
        rendered += 1;
    }
    let wall_s = t0.elapsed().as_secs_f64();

    let fps = if wall_s > 0.0 { rendered as f64 / wall_s } else { 0.0 };
    let avg = if rendered > 0 { bytes_total as f64 / rendered as f64 } else { 0.0 };
    let ms = |ns: u64| ns as f64 / 1e6;
    let (gc, gr) = backend.caps().cells;
    let stage = player.stage();
    println!(
        "{{\"fps\":{fps:.2},\"frames\":{rendered},\"bytes_total\":{bytes_total},\
         \"avg_bytes_per_frame\":{avg:.1},\"tier\":\"{}\",\"stage_ms\":{{\"decode\":{:.1},\
         \"resample\":{:.1},\"compose\":{:.1},\"present\":{:.1}}},\
         \"grid_after\":\"{gc}x{gr}\"}}",
        tier_tag(tier),
        ms(stage.decode),
        ms(stage.resample),
        ms(stage.compose),
        ms(stage.present),
    );
    Ok(())
}

/// Cell aspect: explicit flag > terminal-reported cell pixel size > 2.0
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

/// Interactive loop (PLAN §3.6): wall-clock pacing, latest-frame-wins.
fn run_interactive(reader: SlpyReader<'_>, cli: &Cli, asset_fps: f64, start_frame: u32) -> Result<()> {
    // Capability probe (PLAN §3.1) BEFORE the backend touches the terminal —
    // the volley manages its own termios. `--tier` and `--no-query` skip the
    // volley entirely (escape hatches); `--tier` still wins inside probe_caps.
    let probe_opts = ProbeOptions {
        forced_tier: cli.tier,
        no_query: cli.no_query || cli.tier.is_some(),
        no_cache: cli.no_cache,
        ..ProbeOptions::default()
    };
    let caps = probe_caps(&probe_opts);

    // AnsiBackend::new arms restore + installs panic/SIGINT/SIGTERM/atexit
    // hooks before touching the terminal (M0 acceptance 3, pty-tested in
    // slpy-term). Errors cleanly if stdout is not a TTY.
    let mut backend = AnsiBackend::new(caps)
        .context("cannot enter terminal session (headless? use --sim COLSxROWS:NFRAMES)")?;
    let aspect = resolve_cell_aspect(cli.cell_aspect, backend.caps().cell_px);
    let want_color = backend.caps().color != ColorTier::Mono;
    let mut player = Player::new(reader, aspect, cli.repaint == RepaintMode::Full, want_color)?;
    let (cols, rows) = backend.caps().cells;
    player.reflow(&mut backend, cols, rows);

    let present_fps = match cli.fps_cap {
        Some(cap) if cap > 0.0 => cap.min(asset_fps),
        Some(_) => bail!("--fps-cap must be > 0"),
        None => asset_fps,
    };
    let tick = Duration::from_secs_f64(1.0 / present_fps);
    let frame_count = u64::from(player.frame_count());
    let mut base_frame = u64::from(start_frame);
    let t0 = Instant::now(); // --duration-secs origin (never reset by jumps)
    let mut clock = t0; // pacing origin, reset on 0–9 jumps
    let mut next_tick = t0;

    loop {
        let drained = player.drain_events(&mut backend);
        if drained.quit {
            break;
        }
        if let Some(d) = drained.jump_digit {
            // 0–9 → jump to d×10% (PLAN §3.6; decode goes through the FIDX
            // seek path automatically via the loaded-frame tracker).
            base_frame = frame_count * u64::from(d) / 10;
            clock = Instant::now();
        }
        if let Some(dur) = cli.duration_secs
            && t0.elapsed().as_secs_f64() >= dur
        {
            break;
        }
        // Pacing (§3.6 step 2): target frame by wall clock — if we fell
        // behind, this skips asset frames (latest-frame-wins, never queued).
        let mut target = base_frame + (clock.elapsed().as_secs_f64() * asset_fps) as u64;
        if target >= frame_count {
            if cli.loop_playback {
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    let file = std::fs::File::open(&cli.asset)
        .with_context(|| format!("opening {}", cli.asset.display()))?;
    // Safety: read-only private map of a file we never mutate through this
    // mapping; M0 contract is that assets are not truncated mid-playback
    // (the same assumption every mmap'd reader makes).
    let mmap = unsafe { Mmap::map(&file) }
        .with_context(|| format!("mmap {}", cli.asset.display()))?;
    let reader = SlpyReader::open(&mmap)
        .with_context(|| format!("{} is not a valid SLPY asset", cli.asset.display()))?;

    let header = reader.header();
    // Belt-and-braces: SlpyReader::open rejects zero fps since M1, but a
    // zero fps_num here would reach Duration::from_secs_f64(1/0.0) and panic.
    if header.fps_num == 0 || header.fps_den == 0 {
        bail!("corrupt header: fps_num or fps_den == 0");
    }
    let asset_fps = f64::from(header.fps_num) / f64::from(header.fps_den);

    let start_frame: u32 = match cli.seek.as_deref() {
        Some(ts) => {
            let secs = parse_timestamp(ts).with_context(|| format!("--seek {ts:?}"))?;
            let frame = (secs * asset_fps).floor();
            if frame >= f64::from(reader.frame_count()) {
                bail!(
                    "--seek {ts} is past the end of the asset ({} frames @ {asset_fps} fps)",
                    reader.frame_count()
                );
            }
            frame as u32
        }
        None => 0,
    };

    if cli.sim.is_some() {
        let aspect = cli.cell_aspect.unwrap_or(slpy_core::DEFAULT_CELL_ASPECT);
        let tier = cli.sim_tier.or(cli.tier).unwrap_or(ColorTier::True);
        let player = Player::new(
            reader,
            aspect,
            cli.repaint == RepaintMode::Full,
            tier != ColorTier::Mono,
        )?;
        return run_sim(player, &cli, tier, start_frame);
    }
    run_interactive(reader, &cli, asset_fps, start_frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_and_spec_parsing() {
        assert_eq!(parse_size("213x58").unwrap(), (213, 58));
        assert_eq!(parse_size("320X90").unwrap(), (320, 90));
        assert!(parse_size("0x9").is_err());
        assert!(parse_size("213").is_err());
        assert_eq!(parse_sim_spec("213x58:900").unwrap(), ((213, 58), 900));
        assert!(parse_sim_spec("213x58").is_err());
        assert!(parse_sim_spec("213x58:0").is_err());
    }

    #[test]
    fn timestamp_parsing() {
        assert_eq!(parse_timestamp("42").unwrap(), 42.0);
        assert_eq!(parse_timestamp("42.5").unwrap(), 42.5);
        assert_eq!(parse_timestamp("1:30").unwrap(), 90.0);
        assert_eq!(parse_timestamp("0:01:30.5").unwrap(), 90.5);
        assert_eq!(parse_timestamp("2:00:00").unwrap(), 7200.0);
        assert!(parse_timestamp("").is_err());
        assert!(parse_timestamp("1:2:3:4").is_err());
        assert!(parse_timestamp("-5").is_err());
        assert!(parse_timestamp("abc").is_err());
    }

    #[test]
    fn cell_aspect_resolution() {
        assert_eq!(resolve_cell_aspect(Some(1.5), Some((10, 20))), 1.5);
        assert_eq!(resolve_cell_aspect(None, Some((10, 21))), 2.1);
        assert_eq!(resolve_cell_aspect(None, Some((0, 20))), 2.0);
        assert_eq!(resolve_cell_aspect(None, None), 2.0);
    }
}
