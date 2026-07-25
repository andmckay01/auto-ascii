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
use slpy_core::ramp::{base_ramp_for_cols, ramp_glyph};
use slpy_core::{Cell, Grid, Resampler, Rgb, Viewport, compute_viewport};
use slpy_format::{PlaneLevels, SlpyReader};
use slpy_format::header::plane_id;
use slpy_term::{
    AnsiBackend, Backend, ColorTier, Event, FrameStats, Key, ProbeOptions, SimBackend, probe_caps,
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

/// Per-stage wall-time accumulators (ns) for the `--sim` JSON report.
#[derive(Default)]
struct StageNs {
    decode: u64,
    resample: u64,
    compose: u64,
    present: u64,
}

/// Result of one event-queue drain.
struct Drained {
    quit: bool,
    /// Digit key 0–9 → jump to that ×10% of the asset (interactive seek).
    jump_digit: Option<u8>,
}

/// The frame pipeline: decode → resample → NORM levels → compose into a
/// term-sized grid. All buffers are (re)allocated only in `new`/`reflow` —
/// the hot loop is allocation-free (PLAN §6 discipline).
struct Player<'a> {
    reader: SlpyReader<'a>,
    frame_count: u32,
    src_w: u16,
    src_h: u16,
    /// C plane dims when the asset has chroma (half res, PLAN §4).
    chroma_dims: Option<(u16, u16)>,
    /// Decode + composite chroma (asset has C and the tier shows color;
    /// mono keeps glyph-only and skips the C subblocks entirely, PLAN §4).
    use_chroma: bool,
    cell_aspect: f64,
    repaint_full: bool,
    ramp: &'static [char],
    vp: Option<Viewport>,
    resampler: Option<Resampler>,
    chroma_resampler: Option<Resampler>,
    /// Decoded Y plane, `src_w × src_h` — the standing delta double buffer.
    luma_src: Vec<u8>,
    /// Resampled luma, `vp.cols × vp.rows`.
    luma_dst: Vec<u8>,
    /// Decoded C plane (RGB565 LE) — the chroma delta double buffer.
    chroma_src: Vec<u8>,
    /// C unpacked to 8-bit channels at chroma res (resampler inputs).
    cr_src: Vec<u8>,
    cg_src: Vec<u8>,
    cb_src: Vec<u8>,
    /// Resampled chroma channels, `vp.cols × vp.rows`.
    cr_dst: Vec<u8>,
    cg_dst: Vec<u8>,
    cb_dst: Vec<u8>,
    /// Per-shot NORM levels folded into one LUT (PLAN §3.5: n =
    /// clamp((L − shot_lo) · shot_inv_range)); rebuilt only on shot change.
    levels_lut: [u8; 256],
    /// `first_frame` of the shot `levels_lut` was built for (`None` = the
    /// identity LUT for assets without NORM).
    lut_shot: Option<u32>,
    /// Frame currently decoded in `luma_src`/`chroma_src` — drives the
    /// sequential-roll vs FIDX-seek decode policy on delta assets.
    loaded: Option<u32>,
    /// Full terminal grid (viewport + letterbox pads).
    grid: Grid<Cell>,
    stage: StageNs,
}

impl<'a> Player<'a> {
    fn new(
        reader: SlpyReader<'a>,
        cell_aspect: f64,
        repaint_full: bool,
        want_color: bool,
    ) -> Result<Player<'a>> {
        let (src_w, src_h) = reader
            .plane_dims(plane_id::Y)
            .context("asset has no Y (luma) plane")?;
        let frame_count = reader.frame_count();
        if frame_count == 0 {
            bail!("asset has zero frames");
        }
        let chroma_dims = reader.plane_dims(plane_id::C);
        let use_chroma = want_color && chroma_dims.is_some();
        let chroma_len = chroma_dims.map_or(0, |(w, h)| w as usize * h as usize);
        let mut levels_lut = [0u8; 256];
        build_levels_lut(&mut levels_lut, None); // identity until NORM says otherwise
        Ok(Player {
            reader,
            frame_count,
            src_w,
            src_h,
            chroma_dims,
            use_chroma,
            cell_aspect,
            repaint_full,
            ramp: base_ramp_for_cols(0),
            vp: None,
            resampler: None,
            chroma_resampler: None,
            luma_src: vec![0; src_w as usize * src_h as usize],
            luma_dst: Vec::new(),
            chroma_src: vec![0; if use_chroma { chroma_len * 2 } else { 0 }],
            cr_src: vec![0; if use_chroma { chroma_len } else { 0 }],
            cg_src: vec![0; if use_chroma { chroma_len } else { 0 }],
            cb_src: vec![0; if use_chroma { chroma_len } else { 0 }],
            cr_dst: Vec::new(),
            cg_dst: Vec::new(),
            cb_dst: Vec::new(),
            levels_lut,
            lut_shot: None,
            loaded: None,
            grid: Grid::new(0, 0),
            stage: StageNs::default(),
        })
    }

    /// Resize path (PLAN §3.6 step 1): backend + grid realloc, viewport
    /// recompute, resampler tap rebuilds, invalidate. The next rendered frame
    /// lands on the new grid (M0 acceptance 2).
    fn reflow<B: Backend>(&mut self, backend: &mut B, cols: u16, rows: u16) {
        backend.resize(cols, rows);
        self.grid.resize(cols, rows);
        self.vp = compute_viewport(cols, rows, self.cell_aspect);
        if let Some(vp) = self.vp {
            self.ramp = base_ramp_for_cols(vp.cols);
            self.resampler = Some(Resampler::build(self.src_w, self.src_h, vp.cols, vp.rows));
            let cells = vp.cols as usize * vp.rows as usize;
            self.luma_dst.resize(cells, 0);
            if self.use_chroma {
                let (cw, ch) = self.chroma_dims.expect("use_chroma implies C dims");
                self.chroma_resampler = Some(Resampler::build(cw, ch, vp.cols, vp.rows));
                self.cr_dst.resize(cells, 0);
                self.cg_dst.resize(cells, 0);
                self.cb_dst.resize(cells, 0);
            }
        } else {
            // Below 32x9 (PLAN §3.2): "enlarge terminal" card until regrown.
            self.resampler = None;
            self.chroma_resampler = None;
        }
        backend.invalidate();
    }

    /// Drain the event queue (PLAN §3.6 step 1): Quit wins, resizes coalesce
    /// to the latest and trigger one reflow, digit keys report a jump.
    fn drain_events<B: Backend>(&mut self, backend: &mut B) -> Drained {
        let mut resize: Option<(u16, u16)> = None;
        let mut jump_digit = None;
        while let Some(ev) = backend.events().pop() {
            match ev {
                Event::Quit => return Drained { quit: true, jump_digit: None },
                Event::Resize(c, r) => resize = Some((c, r)),
                Event::Key(Key::Char(c @ '0'..='9')) => jump_digit = Some(c as u8 - b'0'),
                Event::Key(_) => {}
            }
        }
        if let Some((c, r)) = resize {
            self.reflow(backend, c, r);
        }
        Drained { quit: false, jump_digit }
    }

    /// Bring `luma_src` (+ `chroma_src`) to `frame_idx`. Sequential
    /// successors roll one delta via `decode_plane_into` (the standing
    /// double buffer, PLAN §3.6 step 3); anything else — startup, `--seek`,
    /// digit jumps, latest-frame-wins skips, loop wrap — goes through
    /// `seek_plane_into` (FIDX keyframe bsearch + delta rolls, PLAN §4).
    /// Frame-skipping on delta assets MUST NOT use plain decode (INTERFACES).
    fn load_frame(&mut self, frame_idx: u32) -> Result<()> {
        if self.loaded == Some(frame_idx) {
            return Ok(()); // paced repeat: planes already current
        }
        let sequential = frame_idx > 0 && self.loaded == Some(frame_idx - 1);
        if sequential {
            self.reader
                .decode_plane_into(frame_idx, plane_id::Y, &mut self.luma_src)
                .with_context(|| format!("decoding frame {frame_idx}"))?;
            if self.use_chroma {
                self.reader
                    .decode_plane_into(frame_idx, plane_id::C, &mut self.chroma_src)
                    .with_context(|| format!("decoding chroma of frame {frame_idx}"))?;
            }
        } else {
            self.reader
                .seek_plane_into(frame_idx, plane_id::Y, &mut self.luma_src)
                .with_context(|| format!("seeking to frame {frame_idx}"))?;
            if self.use_chroma {
                self.reader
                    .seek_plane_into(frame_idx, plane_id::C, &mut self.chroma_src)
                    .with_context(|| format!("seeking chroma to frame {frame_idx}"))?;
            }
        }
        self.loaded = Some(frame_idx);
        Ok(())
    }

    /// Rebuild the levels LUT iff `frame_idx` entered a different shot
    /// (PLAN §3.5 per-shot auto-levels from NORM — per-frame rebuilds would
    /// pump; per-shot is the contract). Assets without NORM keep identity.
    fn update_levels(&mut self, frame_idx: u32) {
        let shot = self.reader.shot_for_frame(frame_idx).map(|s| s.first_frame);
        if shot != self.lut_shot {
            build_levels_lut(&mut self.levels_lut, self.reader.norm_levels(frame_idx, plane_id::Y));
            self.lut_shot = shot;
        }
    }

    /// Decode → resample → NORM levels → compose → present one asset frame
    /// (PLAN §3.6 steps 3–6). Renders the "enlarge terminal" card when the
    /// terminal is below the 32x9 minimum.
    fn render_present<B: Backend>(&mut self, backend: &mut B, frame_idx: u32) -> Result<FrameStats> {
        if self.vp.is_some() && self.resampler.is_some() {
            let t = Instant::now();
            self.load_frame(frame_idx)?;
            self.stage.decode += t.elapsed().as_nanos() as u64;

            let t = Instant::now();
            self.update_levels(frame_idx);
            let resampler = self.resampler.as_mut().expect("checked above");
            resampler.apply(&self.luma_src, &mut self.luma_dst);
            // Runtime per-shot normalization (M1: replaces M0's baked-in
            // stretch). Applied post-resample: the map is monotone linear,
            // so order is equivalent — and 24k lookups beat 130k.
            for v in &mut self.luma_dst {
                *v = self.levels_lut[*v as usize];
            }
            if self.use_chroma {
                unpack_rgb565(&self.chroma_src, &mut self.cr_src, &mut self.cg_src, &mut self.cb_src);
                let cres = self.chroma_resampler.as_mut().expect("use_chroma implies resampler");
                cres.apply(&self.cr_src, &mut self.cr_dst);
                cres.apply(&self.cg_src, &mut self.cg_dst);
                cres.apply(&self.cb_src, &mut self.cb_dst);
            }
            self.stage.resample += t.elapsed().as_nanos() as u64;

            let t = Instant::now();
            let vp = self.vp.expect("checked above");
            let chroma = self
                .use_chroma
                .then(|| (&self.cr_dst[..], &self.cg_dst[..], &self.cb_dst[..]));
            compose_cells(&self.luma_dst, chroma, &vp, self.ramp, &mut self.grid);
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

/// Fold per-shot p2/p98 NORM levels into a 256-entry LUT (PLAN §3.5:
/// `n = clamp((L − shot_lo) · shot_inv_range)`, rounded). `None` levels or a
/// degenerate span (p98 ≤ p2 — flat shot, or the (0,0) rows of unused plane
/// slots / NORM-less assets) → identity, keeping M0 assets byte-identical.
fn build_levels_lut(lut: &mut [u8; 256], levels: Option<PlaneLevels>) {
    match levels {
        Some(PlaneLevels { p2, p98 }) if p98 > p2 => {
            let lo = u32::from(p2);
            let span = u32::from(p98) - lo;
            for (v, out) in lut.iter_mut().enumerate() {
                let v = v as u32;
                *out = if v <= lo {
                    0
                } else if v >= lo + span {
                    255
                } else {
                    (((v - lo) * 255 + span / 2) / span) as u8
                };
            }
        }
        _ => {
            for (v, out) in lut.iter_mut().enumerate() {
                *out = v as u8;
            }
        }
    }
}

/// Unpack little-endian RGB565 (factory C plane contract, PLAN §4) into
/// three 8-bit channel planes, expanding with bit replication
/// (`r8 = r5<<3 | r5>>2` etc. — 0x1f → 255, canonical).
fn unpack_rgb565(src: &[u8], r: &mut [u8], g: &mut [u8], b: &mut [u8]) {
    for (i, px) in src.chunks_exact(2).enumerate() {
        let v = u16::from_le_bytes([px[0], px[1]]);
        let r5 = (v >> 11) as u8;
        let g6 = ((v >> 5) & 0x3f) as u8;
        let b5 = (v & 0x1f) as u8;
        r[i] = (r5 << 3) | (r5 >> 2);
        g[i] = (g6 << 2) | (g6 >> 4);
        b[i] = (b5 << 3) | (b5 >> 2);
    }
}

/// Fill `out` from normalized luma + optional resampled chroma channels,
/// letterboxed per `vp` (PLAN §3.4/§3.5): base ramp glyph from luma, fg
/// sampled from the chroma plane (area-resampled) on color tiers, gray fg
/// fallback for luma-only assets / mono. [`Cell::BLANK`] pads. Never
/// allocates; `out` must already be term-grid-sized (PLAN §6 discipline).
fn compose_cells(
    luma: &[u8],
    chroma: Option<(&[u8], &[u8], &[u8])>,
    vp: &Viewport,
    ramp: &[char],
    out: &mut Grid<Cell>,
) {
    let vc = vp.cols as usize;
    let vr = vp.rows as usize;
    assert_eq!(out.cols(), vp.cols + vp.pad_left + vp.pad_right, "grid cols != viewport + pads");
    assert_eq!(out.rows(), vp.rows + vp.pad_top + vp.pad_bottom, "grid rows != viewport + pads");
    assert!(luma.len() >= vc * vr, "luma plane smaller than viewport");
    if let Some((r, g, b)) = chroma {
        assert!(r.len() >= vc * vr && g.len() >= vc * vr && b.len() >= vc * vr);
    }
    assert!(!ramp.is_empty(), "empty ramp");

    out.fill(Cell::BLANK);
    let pad_left = vp.pad_left as usize;
    for row in 0..vr {
        let base = row * vc;
        let src = &luma[base..base + vc];
        let drow = &mut out.row_mut(vp.pad_top + row as u16)[pad_left..pad_left + vc];
        for (i, (cell, &n)) in drow.iter_mut().zip(src).enumerate() {
            let fg = match chroma {
                Some((r, g, b)) => Rgb::new(r[base + i], g[base + i], b[base + i]),
                None => Rgb::gray(n),
            };
            *cell = Cell::new(ramp_glyph(ramp, n), fg, Rgb::BLACK);
        }
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
        let frame_idx = ((u64::from(start_frame) + i) % u64::from(player.frame_count)) as u32;
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
    println!(
        "{{\"fps\":{fps:.2},\"frames\":{rendered},\"bytes_total\":{bytes_total},\
         \"avg_bytes_per_frame\":{avg:.1},\"tier\":\"{}\",\"stage_ms\":{{\"decode\":{:.1},\
         \"resample\":{:.1},\"compose\":{:.1},\"present\":{:.1}}},\
         \"grid_after\":\"{gc}x{gr}\"}}",
        tier_tag(tier),
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
    let frame_count = u64::from(player.frame_count);
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

    #[test]
    fn levels_lut_identity_without_norm() {
        let mut lut = [0u8; 256];
        build_levels_lut(&mut lut, None);
        assert!(lut.iter().enumerate().all(|(i, &v)| v as usize == i));
        // Degenerate spans are identity too (unused (0,0) slots, flat shots).
        build_levels_lut(&mut lut, Some(PlaneLevels { p2: 0, p98: 0 }));
        assert!(lut.iter().enumerate().all(|(i, &v)| v as usize == i));
        build_levels_lut(&mut lut, Some(PlaneLevels { p2: 200, p98: 100 }));
        assert!(lut.iter().enumerate().all(|(i, &v)| v as usize == i));
    }

    #[test]
    fn levels_lut_stretches_and_clamps() {
        let mut lut = [0u8; 256];
        build_levels_lut(&mut lut, Some(PlaneLevels { p2: 50, p98: 200 }));
        assert_eq!(lut[0], 0);
        assert_eq!(lut[50], 0);
        assert_eq!(lut[200], 255);
        assert_eq!(lut[255], 255);
        assert_eq!(lut[125], 128); // midpoint → mid gray (rounded)
        // Monotone non-decreasing everywhere.
        assert!(lut.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn rgb565_unpack_is_canonical() {
        // Solid red 0xF800, solid green 0x07E0, solid blue 0x001F, white.
        let src: Vec<u8> = [0xF800u16, 0x07E0, 0x001F, 0xFFFF]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let (mut r, mut g, mut b) = (vec![0u8; 4], vec![0u8; 4], vec![0u8; 4]);
        unpack_rgb565(&src, &mut r, &mut g, &mut b);
        assert_eq!((r[0], g[0], b[0]), (255, 0, 0));
        assert_eq!((r[1], g[1], b[1]), (0, 255, 0));
        assert_eq!((r[2], g[2], b[2]), (0, 0, 255));
        assert_eq!((r[3], g[3], b[3]), (255, 255, 255));
    }

    #[test]
    fn compose_cells_chroma_fg_and_blank_pads() {
        let vp = compute_viewport(213, 58, 2.0).unwrap(); // 206x58, pads 3/4
        let cells = vp.cols as usize * vp.rows as usize;
        let luma = vec![128u8; cells];
        let (r, g, b) = (vec![200u8; cells], vec![10u8; cells], vec![30u8; cells]);
        let mut grid: Grid<Cell> = Grid::new(213, 58);
        compose_cells(
            &luma,
            Some((&r, &g, &b)),
            &vp,
            base_ramp_for_cols(vp.cols),
            &mut grid,
        );
        assert_eq!(grid.get(0, 0), Cell::BLANK, "left pad blank");
        let c = grid.get(vp.pad_left, 0);
        assert_eq!(c.fg, Rgb::new(200, 10, 30), "fg from chroma, not gray");
        assert_eq!(c.bg, Rgb::BLACK);
        // Luma-only fallback keeps the M0 gray fg.
        let mut grid2: Grid<Cell> = Grid::new(213, 58);
        compose_cells(&luma, None, &vp, base_ramp_for_cols(vp.cols), &mut grid2);
        assert_eq!(grid2.get(vp.pad_left, 0).fg, Rgb::gray(128));
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
