//! `sleepy-player` — realtime terminal player (PLAN §3, M0 subset).
//!
//! M0 frame loop (PLAN §3.6): drain events (resize → recompute viewport §3.2,
//! rebuild resampler §3.3, reset diff state, invalidate) → wall-clock pacing
//! with latest-frame-wins frame skipping (§3.6 step 2) →
//! `SlpyReader::decode_plane_into` (Y) → resample to viewport → `compose_luma`
//! (base ramp glyph + truecolor gray fg, §3.4/§7) → `Backend::present`, with
//! `--repaint full` (invalidate-every-frame) as the default mode (PLAN §3.1/§7:
//! full repaint is just `invalidate()` each frame — one render path).
//!
//! Never run interactively without a TTY; headless verification uses
//! `--sim COLSxROWS:NFRAMES` (SimBackend — the M0 fps acceptance path at
//! 213x58 and 320x90), which renders N frames as fast as possible and prints
//! one JSON line of stats. `--sim-resize [COLSxROWS]` injects a resize event
//! at frame N/2 to prove reflow.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use memmap2::Mmap;
use slpy_core::ramp::base_ramp_for_cols;
use slpy_core::{Cell, Grid, Resampler, Rgb, Viewport, compose_luma, compute_viewport};
use slpy_format::SlpyReader;
use slpy_format::header::plane_id;
use slpy_term::{AnsiBackend, Backend, Caps, Event, FrameStats, SimBackend};

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

    /// Headless mode: render NFRAMES frames to SimBackend at COLSxROWS as
    /// fast as possible (no pacing), never touch the tty, print one JSON
    /// stats line (M0 acceptance 1: 213x58:900 and 320x90:900).
    #[arg(long, value_name = "COLSxROWS:NFRAMES")]
    sim: Option<String>,

    /// With --sim: inject a Resize event at frame NFRAMES/2 to the given
    /// size (default 100x40) — proves resize reflow; reported as
    /// `grid_after` in the JSON.
    #[arg(long, value_name = "COLSxROWS", requires = "sim",
          num_args = 0..=1, default_missing_value = "100x40")]
    sim_resize: Option<String>,
}

/// Per-stage wall-time accumulators (ns) for the `--sim` JSON report.
#[derive(Default)]
struct StageNs {
    decode: u64,
    resample: u64,
    compose: u64,
    present: u64,
}

enum Flow {
    Continue,
    Quit,
}

/// The frame pipeline: decode → resample → compose into a term-sized grid.
/// All buffers are (re)allocated only in `reflow` — the hot loop is
/// allocation-free (PLAN §6 discipline).
struct Player<'a> {
    reader: SlpyReader<'a>,
    frame_count: u32,
    src_w: u16,
    src_h: u16,
    cell_aspect: f64,
    repaint_full: bool,
    ramp: &'static [char],
    vp: Option<Viewport>,
    resampler: Option<Resampler>,
    /// Decoded Y plane, `src_w × src_h`.
    luma_src: Vec<u8>,
    /// Resampled luma, `vp.cols × vp.rows`.
    luma_dst: Vec<u8>,
    /// Full terminal grid (viewport + letterbox pads).
    grid: Grid<Cell>,
    stage: StageNs,
}

impl<'a> Player<'a> {
    fn new(reader: SlpyReader<'a>, cell_aspect: f64, repaint_full: bool) -> Result<Player<'a>> {
        let (src_w, src_h) = reader
            .plane_dims(plane_id::Y)
            .context("asset has no Y (luma) plane")?;
        let frame_count = reader.frame_count();
        if frame_count == 0 {
            bail!("asset has zero frames");
        }
        Ok(Player {
            reader,
            frame_count,
            src_w,
            src_h,
            cell_aspect,
            repaint_full,
            ramp: base_ramp_for_cols(0),
            vp: None,
            resampler: None,
            luma_src: vec![0; src_w as usize * src_h as usize],
            luma_dst: Vec::new(),
            grid: Grid::new(0, 0),
            stage: StageNs::default(),
        })
    }

    /// Resize path (PLAN §3.6 step 1): backend + grid realloc, viewport
    /// recompute, resampler tap rebuild, invalidate. The next rendered frame
    /// lands on the new grid (M0 acceptance 2).
    fn reflow<B: Backend>(&mut self, backend: &mut B, cols: u16, rows: u16) {
        backend.resize(cols, rows);
        self.grid.resize(cols, rows);
        self.vp = compute_viewport(cols, rows, self.cell_aspect);
        if let Some(vp) = self.vp {
            self.ramp = base_ramp_for_cols(vp.cols);
            self.resampler = Some(Resampler::build(self.src_w, self.src_h, vp.cols, vp.rows));
            self.luma_dst.resize(vp.cols as usize * vp.rows as usize, 0);
        } else {
            // Below 32x9 (PLAN §3.2): "enlarge terminal" card until regrown.
            self.resampler = None;
        }
        backend.invalidate();
    }

    /// Drain the event queue (PLAN §3.6 step 1): Quit wins, resizes coalesce
    /// to the latest and trigger one reflow.
    fn drain_events<B: Backend>(&mut self, backend: &mut B) -> Flow {
        let mut resize: Option<(u16, u16)> = None;
        while let Some(ev) = backend.events().pop() {
            match ev {
                Event::Quit => return Flow::Quit,
                Event::Resize(c, r) => resize = Some((c, r)),
                Event::Key(_) => {}
            }
        }
        if let Some((c, r)) = resize {
            self.reflow(backend, c, r);
        }
        Flow::Continue
    }

    /// Decode → resample → compose → present one asset frame (PLAN §3.6
    /// steps 3–6). Renders the "enlarge terminal" card when the terminal is
    /// below the 32x9 minimum.
    fn render_present<B: Backend>(&mut self, backend: &mut B, frame_idx: u32) -> Result<FrameStats> {
        if let (Some(vp), Some(resampler)) = (self.vp, self.resampler.as_mut()) {
            let t = Instant::now();
            self.reader
                .decode_plane_into(frame_idx, plane_id::Y, &mut self.luma_src)
                .with_context(|| format!("decoding frame {frame_idx}"))?;
            self.stage.decode += t.elapsed().as_nanos() as u64;

            let t = Instant::now();
            resampler.apply(&self.luma_src, &mut self.luma_dst);
            self.stage.resample += t.elapsed().as_nanos() as u64;

            let t = Instant::now();
            compose_luma(&self.luma_dst, &vp, self.ramp, &mut self.grid);
            self.stage.compose += t.elapsed().as_nanos() as u64;
        } else {
            draw_enlarge_card(&mut self.grid);
        }

        if self.repaint_full {
            backend.invalidate();
        }
        let t = Instant::now();
        let stats = backend.present(&self.grid);
        self.stage.present += t.elapsed().as_nanos() as u64;
        Ok(stats)
    }
}

/// Centered "enlarge terminal" card (PLAN §3.2, below 32x9).
fn draw_enlarge_card(grid: &mut Grid<Cell>) {
    grid.fill(Cell::BLANK);
    let (cols, rows) = (grid.cols(), grid.rows());
    if cols == 0 || rows == 0 {
        return;
    }
    let lines: [&str; 2] = ["SLEEPYTIME", "enlarge terminal (min 32x9)"];
    let top = rows.saturating_sub(lines.len() as u16) / 2;
    for (i, line) in lines.iter().enumerate() {
        let row = top + i as u16;
        if row >= rows {
            break;
        }
        let n = (line.len() as u16).min(cols); // ASCII-only card text
        let left = (cols - n) / 2;
        for (j, ch) in line.chars().take(n as usize).enumerate() {
            grid.set(left + j as u16, row, Cell::new(ch, Rgb::gray(220), Rgb::BLACK));
        }
    }
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

/// Headless acceptance path: N frames as fast as possible against SimBackend,
/// one JSON stats line on stdout (never touches the tty).
fn run_sim(mut player: Player<'_>, spec: &str, sim_resize: Option<&str>) -> Result<()> {
    let ((cols, rows), nframes) = parse_sim_spec(spec)?;
    let resize_to = sim_resize.map(parse_size).transpose()?;

    let mut backend = SimBackend::new(cols, rows);
    player.reflow(&mut backend, cols, rows);

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
        if matches!(player.drain_events(&mut backend), Flow::Quit) {
            break;
        }
        let frame_idx = (i % u64::from(player.frame_count)) as u32;
        let stats = player.render_present(&mut backend, frame_idx)?;
        bytes_total += u64::from(stats.bytes);
        backend.take_output(); // bytes are counted; don't hold 900 frames in RAM
        rendered += 1;
    }
    let wall_s = t0.elapsed().as_secs_f64();

    let fps = if wall_s > 0.0 { rendered as f64 / wall_s } else { 0.0 };
    let avg = if rendered > 0 { bytes_total as f64 / rendered as f64 } else { 0.0 };
    let ms = |ns: u64| ns as f64 / 1e6;
    let (gc, gr) = backend.caps().cells;
    println!(
        "{{\"fps\":{fps:.2},\"frames\":{rendered},\"bytes_total\":{bytes_total},\
         \"avg_bytes_per_frame\":{avg:.1},\"stage_ms\":{{\"decode\":{:.1},\
         \"resample\":{:.1},\"compose\":{:.1},\"present\":{:.1}}},\
         \"grid_after\":\"{gc}x{gr}\"}}",
        ms(player.stage.decode),
        ms(player.stage.resample),
        ms(player.stage.compose),
        ms(player.stage.present),
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
fn run_interactive(reader: SlpyReader<'_>, cli: &Cli, asset_fps: f64) -> Result<()> {
    // AnsiBackend::new arms restore + installs panic/SIGINT/SIGTERM/atexit
    // hooks before touching the terminal (M0 acceptance 3, pty-tested in
    // slpy-term). Errors cleanly if stdout is not a TTY.
    let mut backend = AnsiBackend::new(Caps::default())
        .context("cannot enter terminal session (headless? use --sim COLSxROWS:NFRAMES)")?;
    let aspect = resolve_cell_aspect(cli.cell_aspect, backend.caps().cell_px);
    let mut player = Player::new(reader, aspect, cli.repaint == RepaintMode::Full)?;
    let (cols, rows) = backend.caps().cells;
    player.reflow(&mut backend, cols, rows);

    let present_fps = match cli.fps_cap {
        Some(cap) if cap > 0.0 => cap.min(asset_fps),
        Some(_) => bail!("--fps-cap must be > 0"),
        None => asset_fps,
    };
    let tick = Duration::from_secs_f64(1.0 / present_fps);
    let start = Instant::now();
    let mut next_tick = start;
    let frame_count = u64::from(player.frame_count);

    loop {
        if matches!(player.drain_events(&mut backend), Flow::Quit) {
            break;
        }
        let elapsed = start.elapsed();
        if let Some(d) = cli.duration_secs
            && elapsed.as_secs_f64() >= d
        {
            break;
        }
        // Pacing (§3.6 step 2): target frame by wall clock — if we fell
        // behind, this skips asset frames (latest-frame-wins, never queued).
        let mut target = (elapsed.as_secs_f64() * asset_fps) as u64;
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
    if header.fps_den == 0 {
        bail!("corrupt header: fps_den == 0");
    }
    let asset_fps = f64::from(header.fps_num) / f64::from(header.fps_den);

    if let Some(spec) = cli.sim.as_deref() {
        let aspect = cli.cell_aspect.unwrap_or(slpy_core::DEFAULT_CELL_ASPECT);
        let player = Player::new(reader, aspect, cli.repaint == RepaintMode::Full)?;
        return run_sim(player, spec, cli.sim_resize.as_deref());
    }
    run_interactive(reader, &cli, asset_fps)
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
    fn cell_aspect_resolution() {
        assert_eq!(resolve_cell_aspect(Some(1.5), Some((10, 20))), 1.5);
        assert_eq!(resolve_cell_aspect(None, Some((10, 21))), 2.1);
        assert_eq!(resolve_cell_aspect(None, Some((0, 20))), 2.0);
        assert_eq!(resolve_cell_aspect(None, None), 2.0);
    }

    #[test]
    fn enlarge_card_fits_tiny_grids() {
        for (c, r) in [(1u16, 1u16), (10, 2), (31, 8), (80, 24)] {
            let mut g = Grid::new(c, r);
            draw_enlarge_card(&mut g); // must never panic / go OOB
            assert_eq!(g.cols(), c);
        }
        let mut g = Grid::new(40, 9);
        draw_enlarge_card(&mut g);
        let mid: String = (0..40).map(|col| g.get(col, 3).glyph()).collect();
        assert!(mid.contains("SLEEPYTIME"), "card text missing: {mid:?}");
    }
}
