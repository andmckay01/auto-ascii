//! `sleepy-player` — realtime terminal player (PLAN §3), M4: a thin CLI
//! over the `sleepytime` facade. Interactive playback is
//! [`sleepytime::Player`] verbatim — argv maps 1:1 onto
//! [`PlayerBuilder`](sleepytime::PlayerBuilder) options and NOTHING else
//! (no logic fork between bin and lib paths, M4 item B); this file owns
//! only argument parsing and the headless `--sim` harness.
//!
//! Never run interactively without a TTY; headless verification uses
//! `--sim COLSxROWS:NFRAMES` (SimBackend), which renders N frames as fast as
//! possible and prints one JSON line of stats. `--sim-tier` selects the
//! simulated color tier and `--sim-dump PATH` captures the raw escape stream
//! for byte-level tier checks; `--sim-resize [COLSxROWS]` injects a resize
//! event at frame N/2 to prove reflow. The `--sim` path drives the same
//! [`sleepytime::pipeline`] the facade Player runs.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use memmap2::Mmap;
use sleepytime::pipeline::{Player, color_depth};
use sleepytime::{PaletteChoice, RepaintMode};
use slpy_format::SlpyReader;
use slpy_term::{Backend, Caps, ColorTier, Event, SimBackend};

/// CLI face of [`sleepytime::RepaintMode`] (PLAN §3.1: one render path —
/// "full" is diff with `invalidate()` every frame, the M0 default per §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RepaintArg {
    /// Invalidate every frame → full escape stream each present (M0 default).
    Full,
    /// Pure diff: only damaged cells are rewritten; invalidate on resize only.
    Diff,
}

impl From<RepaintArg> for RepaintMode {
    fn from(r: RepaintArg) -> RepaintMode {
        match r {
            RepaintArg::Full => RepaintMode::Full,
            RepaintArg::Diff => RepaintMode::Diff,
        }
    }
}

/// Charset-tier override for palette selection (PLAN §3.4). `auto` derives
/// the tier from the probed `Caps.glyph_support`/`Caps.glyphs`; the explicit
/// values force it (e.g. `--palette braille` on a terminal whose font is
/// known-good — braille is never enabled from passive hints alone).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum PaletteArg {
    Auto,
    Ascii,
    Unicode,
    Braille,
}

impl From<PaletteArg> for PaletteChoice {
    fn from(p: PaletteArg) -> PaletteChoice {
        match p {
            PaletteArg::Auto => PaletteChoice::Auto,
            PaletteArg::Ascii => PaletteChoice::Ascii,
            PaletteArg::Unicode => PaletteChoice::Unicode,
            PaletteArg::Braille => PaletteChoice::Braille,
        }
    }
}

#[derive(Parser)]
#[command(name = "sleepy-player", version, about = "Play SLPY assets in the terminal (PLAN §3)")]
struct Cli {
    /// SLPY asset (mmap'd read-only via memmap2, PLAN §8).
    asset: PathBuf,

    /// Repaint mode (PLAN §7: M0 default is invalidate-every-frame).
    #[arg(long, value_enum, default_value_t = RepaintArg::Full)]
    repaint: RepaintArg,

    /// Loop playback instead of exiting at the last frame.
    #[arg(long = "loop")]
    loop_playback: bool,

    /// Cap the presentation rate below the asset fps (frames are still
    /// selected by wall clock, so capping skips asset frames — it never
    /// slows the video down). Minimum 1 fps.
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

    /// Skip the identity-keyed quirk table (M5, PLAN §3.1): take the probe
    /// replies at face value instead of applying the known per-terminal
    /// corrections (keyed on the XTVERSION reply, never on TERM). Implies
    /// the probe cache is not written.
    #[arg(long)]
    no_quirks: bool,

    /// Charset-tier override for palette selection (PLAN §3.4): auto (from
    /// the probed glyph repertoire), ascii, unicode (blocks/box-drawing) or
    /// braille (verified fonts only). Applies to interactive and --sim runs.
    #[arg(long, value_enum, default_value_t = PaletteArg::Auto)]
    palette: PaletteArg,

    /// Assert the terminal's font by ink-coverage table (PLAN §3.4, M5): a
    /// built-in name (conservative, dejavu-sans-mono, liberation-mono,
    /// ubuntu-mono, noto-sans-mono) or a path to a `sleepy-factory
    /// font-table` TOML. The table's recorded repertoire vetoes the palette
    /// tier (braille -> unicode -> ascii) so a font missing e.g. box-drawing
    /// diagonals degrades instead of drawing missing-glyph boxes. Applies to
    /// interactive and --sim runs.
    #[arg(long, value_name = "NAME|PATH")]
    font_table: Option<String>,

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

    /// Headless scrub-latency benchmark (M5 acceptance: seek < 50 ms on the
    /// full asset): perform N random seeks — each one a hysteresis reset +
    /// FIDX keyframe seek + delta rolls + resample + compose + present to a
    /// 300x80 SimBackend, exactly the interactive scrub path — and print one
    /// JSON line with p50/p95/max latency in ms. Deterministic seek
    /// sequence (seeded LCG); never touches the tty.
    #[arg(long, value_name = "N", conflicts_with = "sim")]
    bench_seek: Option<u32>,
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

    // Winning-layer counts (M3): the §3.4 priority decision, summed over
    // every rendered cell — headless visibility into which layers actually
    // fire ("layers" in the JSON line).
    player.enable_layer_mask();
    let mut layer_counts = [0u64; 5];

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
        if let Some(mask) = player.layer_mask() {
            for &l in mask.as_slice() {
                if let Some(c) = layer_counts.get_mut(l as usize) {
                    *c += 1;
                }
            }
        }
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
         \"layers\":{{\"base\":{},\"edge\":{},\"highlight\":{},\"shadow\":{},\
         \"structure\":{}}},\"grid_after\":\"{gc}x{gr}\"}}",
        tier_tag(tier),
        ms(stage.decode),
        ms(stage.resample),
        ms(stage.compose),
        ms(stage.present),
        layer_counts[0],
        layer_counts[1],
        layer_counts[2],
        layer_counts[3],
        layer_counts[4],
    );
    Ok(())
}

/// M5 scrub-latency benchmark: N deterministic random seeks through the
/// EXACT interactive scrub machinery — hysteresis reset (the drain_events
/// discontinuity rule) + FIDX keyframe bsearch + ≤ keyframe_ivl−1 delta
/// rolls + resample + compose + present — timed end to end per seek.
/// Acceptance (PLAN §7 M5): p50/p95 < 50 ms on the full 856 MB asset.
fn run_bench_seek(mut player: Player<'_>, seeks: u32) -> Result<()> {
    const COLS: u16 = 300;
    const ROWS: u16 = 80; // the PLAN §3.6 reference grid
    if seeks == 0 {
        bail!("--bench-seek needs N >= 1");
    }
    let mut backend = SimBackend::new(COLS, ROWS);
    player.reflow(&mut backend, COLS, ROWS);
    // One warmup render: builds tap tables' caches and pages in the header/
    // FIDX region; every timed seek below still decodes cold frame data.
    player.render_present(&mut backend, 0)?;
    backend.take_output();

    let frames = u64::from(player.frame_count());
    let mut lat_ms: Vec<f64> = Vec::with_capacity(seeks as usize);
    let mut rng: u64 = 0x5EED_F00D_D15C_0B01; // fixed seed: reproducible run
    for _ in 0..seeks {
        // xorshift64* — deterministic frame sequence, no dependency.
        rng ^= rng >> 12;
        rng ^= rng << 25;
        rng ^= rng >> 27;
        let frame = ((rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) % frames) as u32;
        let t = Instant::now();
        player.reset_temporal_state(); // scrub = temporal discontinuity
        player.render_present(&mut backend, frame)?;
        lat_ms.push(t.elapsed().as_secs_f64() * 1e3);
        backend.take_output();
    }
    lat_ms.sort_by(|a, b| a.partial_cmp(b).expect("latencies are finite"));
    let pick = |q: f64| lat_ms[((lat_ms.len() - 1) as f64 * q).round() as usize];
    println!(
        "{{\"seeks\":{seeks},\"grid\":\"{COLS}x{ROWS}\",\"frames\":{frames},\
         \"p50_ms\":{:.2},\"p95_ms\":{:.2},\"max_ms\":{:.2}}}",
        pick(0.50),
        pick(0.95),
        lat_ms[lat_ms.len() - 1],
    );
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let seek_secs = cli
        .seek
        .as_deref()
        .map(|ts| parse_timestamp(ts).with_context(|| format!("--seek {ts:?}")))
        .transpose()?;

    if cli.sim.is_some() || cli.bench_seek.is_some() {
        // Headless harness paths: drive the pipeline directly (the facade
        // Player is a terminal session by definition).
        let file = std::fs::File::open(&cli.asset)
            .with_context(|| format!("opening {}", cli.asset.display()))?;
        // Safety: read-only private map of a file we never mutate through
        // this mapping; M0 contract is that assets are not truncated
        // mid-playback (the same assumption every mmap'd reader makes).
        let mmap = unsafe { Mmap::map(&file) }
            .with_context(|| format!("mmap {}", cli.asset.display()))?;
        let reader = SlpyReader::open(&mmap)
            .with_context(|| format!("{} is not a valid SLPY asset", cli.asset.display()))?;

        let header = reader.header();
        // Belt-and-braces: SlpyReader::open rejects zero fps since M1.
        if header.fps_num == 0 || header.fps_den == 0 {
            bail!("corrupt header: fps_num or fps_den == 0");
        }
        let asset_fps = f64::from(header.fps_num) / f64::from(header.fps_den);
        let start_frame: u32 = match seek_secs {
            Some(secs) => {
                let frame = (secs * asset_fps).floor();
                if frame >= f64::from(reader.frame_count()) {
                    bail!(
                        "--seek is past the end of the asset ({} frames @ {asset_fps} fps)",
                        reader.frame_count()
                    );
                }
                frame as u32
            }
            None => 0,
        };

        let aspect = cli.cell_aspect.unwrap_or(slpy_core::DEFAULT_CELL_ASPECT);
        let tier = cli.sim_tier.or(cli.tier).unwrap_or(ColorTier::True);
        // --sim never probes: palette auto derives from the SimBackend's
        // default Caps (ascii repertoire); --palette overrides; the
        // --font-table repertoire veto applies exactly as interactively.
        let mut glyphs = PaletteChoice::from(cli.palette).resolve_for_caps(&Caps::default());
        if let Some(spec) = &cli.font_table {
            glyphs = sleepytime::load_font_table(spec)?.veto_tier(glyphs);
        }
        let player = Player::new(
            reader,
            aspect,
            cli.repaint == RepaintArg::Full,
            color_depth(tier),
            glyphs,
        )?;
        if let Some(n) = cli.bench_seek {
            return run_bench_seek(player, n);
        }
        return run_sim(player, &cli, tier, start_frame);
    }

    // Interactive path: argv → PlayerBuilder, then the facade owns the
    // probe, the session, the pacing loop and the restore (no logic here).
    let mut builder = sleepytime::Player::builder()
        .asset(&cli.asset)
        .palette(cli.palette.into())
        .tier(cli.tier)
        .repaint(cli.repaint.into())
        .looping(cli.loop_playback)
        .no_query(cli.no_query)
        .no_quirks(cli.no_quirks)
        .no_cache(cli.no_cache);
    if let Some(cap) = cli.fps_cap {
        builder = builder.fps_cap(cap);
    }
    if let Some(a) = cli.cell_aspect {
        builder = builder.cell_aspect(a);
    }
    if let Some(secs) = seek_secs {
        builder = builder.seek_secs(secs);
    }
    if let Some(dur) = cli.duration_secs {
        builder = builder.duration_secs(dur);
    }
    if let Some(spec) = &cli.font_table {
        builder = builder.font_table(spec.as_str());
    }
    builder.build()?.run()?;
    Ok(())
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
}
