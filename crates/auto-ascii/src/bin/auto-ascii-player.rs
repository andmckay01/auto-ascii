use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use auto_ascii::deck::{ClipDeck, DeckConfig};
use auto_ascii::pipeline::color_depth;
use auto_ascii::{Codec, Composition, PaletteChoice, RepaintMode};
use auto_ascii_term::{Backend, Caps, ColorTier, Event, SimBackend};

/// CLI face of [`auto_ascii::RepaintMode`] (one render path — "full" is diff
/// with `invalidate()` every frame, the default).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RepaintArg {
    /// Invalidate every frame → full escape stream each present (default).
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

/// Charset-tier override for palette selection. `auto` derives
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
#[command(name = "auto-ascii-player", version, about = "Play ASCI assets in the terminal")]
struct Cli {
    /// ASCI asset (mmap'd read-only via memmap2), or a composition `.toml` —
    /// a stitch of clips played virtually, on one timeline. Bare library
    /// names inside a composition resolve under
    /// `$AUTO_ASCII_HOME/library` (default `~/auto-ascii/library`).
    asset: PathBuf,

    /// Repaint mode (default: invalidate every frame).
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

    /// Cell aspect override `cell_h_px / cell_w_px`. Default:
    /// derived from the terminal's reported cell pixel size, else 2.0.
    #[arg(long, value_name = "RATIO")]
    cell_aspect: Option<f64>,

    /// Stop after N seconds (interactive benching); default: play to end.
    #[arg(long, value_name = "SECS")]
    duration_secs: Option<f64>,

    /// Start at TIMESTAMP — plain seconds ("42.5") or colon form ("1:30",
    /// "0:01:30.5"). Uses the FIDX seek path (keyframe binary search +
    /// ≤ keyframe_ivl−1 delta rolls). Interactively, keys 0–9 also
    /// jump to 0–90% of the asset.
    #[arg(long, value_name = "TIMESTAMP")]
    seek: Option<String>,

    /// Force the color tier (truecolor|256|16|mono) — skips the probe volley
    /// entirely (an escape hatch; passive env hints still fill the glyph
    /// repertoire).
    #[arg(long, value_name = "TIER")]
    tier: Option<ColorTier>,

    /// Never write the probe volley; passive env hints only (an escape
    /// hatch for hostile PTYs).
    #[arg(long)]
    no_query: bool,

    /// Bypass the probe cache (no read, no write).
    #[arg(long)]
    no_cache: bool,

    /// Skip the identity-keyed quirk table: take the probe
    /// replies at face value instead of applying the known per-terminal
    /// corrections (keyed on the XTVERSION reply, never on TERM). Implies
    /// the probe cache is not written.
    #[arg(long)]
    no_quirks: bool,

    /// Keep the terminal's own default background. By default the player
    /// sets it to black for the session (OSC 11, reset with OSC 111 on
    /// exit), so the ascii codec, which paints no background, sits on black
    /// under any theme.
    #[arg(long)]
    no_backdrop: bool,

    /// Charset-tier override for palette selection: auto (from
    /// the probed glyph repertoire), ascii, unicode (blocks/box-drawing) or
    /// braille (verified fonts only). Applies to interactive and --sim runs.
    #[arg(long, value_enum, default_value_t = PaletteArg::Auto)]
    palette: PaletteArg,

    /// Assert the terminal's font by ink-coverage table: a
    /// built-in name (conservative, dejavu-sans-mono, liberation-mono,
    /// ubuntu-mono, noto-sans-mono) or a path to a `auto-ascii-factory
    /// font-table` TOML. The table's recorded repertoire vetoes the palette
    /// tier (braille -> unicode -> ascii) so a font missing e.g. box-drawing
    /// diagonals degrades instead of drawing missing-glyph boxes. Applies to
    /// interactive and --sim runs.
    #[arg(long, value_name = "NAME|PATH")]
    font_table: Option<String>,

    #[arg(long, value_name = "NAME", value_parser = parse_codec, help = codec_help())]
    codec: Option<Codec>,

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

    /// Headless scrub-latency benchmark: perform N random seeks — each one a
    /// hysteresis reset +
    /// FIDX keyframe seek + delta rolls + resample + compose + present to a
    /// 300x80 SimBackend, exactly the interactive scrub path — and print one
    /// JSON line with p50/p95/max latency in ms. Deterministic seek
    /// sequence (seeded LCG); never touches the tty.
    #[arg(long, value_name = "N", conflicts_with = "sim")]
    bench_seek: Option<u32>,
}

fn parse_codec(name: &str) -> std::result::Result<Codec, String> {
    Codec::from_name(name)
        .ok_or_else(|| format!("unknown codec {name:?} (known: {})", Codec::names(", ")))
}

fn codec_help() -> String {
    format!(
        "Glyph codec — how cell features become glyphs: {} (default {}). \
         Interactively it overrides the codec saved for a video until `/` \
         picks another; with --sim it is the codec the run renders in",
        Codec::names(", "),
        Codec::default().name()
    )
}

fn parse_size(s: &str) -> Result<(u16, u16)> {
    let (c, r) = s.split_once(['x', 'X']).context("expected COLSxROWS")?;
    let cols: u16 = c.trim().parse().context("bad COLS")?;
    let rows: u16 = r.trim().parse().context("bad ROWS")?;
    if cols == 0 || rows == 0 {
        bail!("size must be at least 1x1");
    }
    Ok((cols, rows))
}

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

fn open_timeline(path: &Path) -> Result<Composition> {
    let mut comp = if Composition::is_toml_path(path) {
        Composition::from_toml_file(path, Composition::default_library_dir().as_deref())
            .with_context(|| format!("reading composition {}", path.display()))?
    } else {
        Composition::single(path)
    };
    comp.resolve()?;
    Ok(comp)
}

fn tier_tag(t: ColorTier) -> &'static str {
    match t {
        ColorTier::True => "truecolor",
        ColorTier::C256 => "256",
        ColorTier::C16 => "16",
        ColorTier::Mono => "mono",
    }
}

fn run_sim(
    comp: &Composition,
    mut deck: ClipDeck,
    cli: &Cli,
    tier: ColorTier,
    start_frame: u32,
) -> Result<()> {
    let spec = cli.sim.as_deref().expect("run_sim requires --sim");
    let ((cols, rows), nframes) = parse_sim_spec(spec)?;
    let resize_to = cli.sim_resize.as_deref().map(parse_size).transpose()?;

    let mut backend = SimBackend::new(cols, rows);
    let mut caps = backend.caps().clone();
    caps.color = tier;
    backend.set_caps(caps);
    backend.resize(cols, rows);
    deck.set_size(cols, rows);

    let mut dump = cli
        .sim_dump
        .as_ref()
        .map(|p| {
            std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))
        })
        .transpose()?;

    deck.enable_layer_mask();
    let mut layer_counts = [0u64; 5];

    let resize_at = nframes / 2;
    let mut bytes_total: u64 = 0;
    let mut rendered: u64 = 0;
    let t0 = Instant::now();
    for i in 0..nframes {
        if let Some((rc, rr)) = resize_to
            && i == resize_at
        {
            backend.push_event(Event::Resize(rc, rr));
        }
        if deck.drain_events(&mut backend).quit {
            break;
        }
        let frame_idx = ((u64::from(start_frame) + i) % u64::from(comp.frame_count())) as u32;
        let stats = deck.present_at(&mut backend, comp.locate_frame(frame_idx))?;
        bytes_total += u64::from(stats.bytes);
        if let Some(mask) = deck.layer_mask() {
            for &l in mask.as_slice() {
                if let Some(c) = layer_counts.get_mut(l as usize) {
                    *c += 1;
                }
            }
        }
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
    let stage = deck.stage();
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

fn run_bench_seek(comp: &Composition, mut deck: ClipDeck, seeks: u32) -> Result<()> {
    const COLS: u16 = 300;
    const ROWS: u16 = 80;
    if seeks == 0 {
        bail!("--bench-seek needs N >= 1");
    }
    let mut backend = SimBackend::new(COLS, ROWS);
    backend.resize(COLS, ROWS);
    deck.set_size(COLS, ROWS);
    deck.present_at(&mut backend, comp.locate_frame(0))?;
    backend.take_output();

    let frames = u64::from(comp.frame_count());
    let mut lat_ms: Vec<f64> = Vec::with_capacity(seeks as usize);
    let mut rng: u64 = 0x5EED_F00D_D15C_0B01;
    for _ in 0..seeks {
        rng ^= rng >> 12;
        rng ^= rng << 25;
        rng ^= rng >> 27;
        let frame = ((rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) % frames) as u32;
        let t = Instant::now();
        deck.reset_active();
        deck.present_at(&mut backend, comp.locate_frame(frame))?;
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
        .map(|ts| auto_ascii::timecode::parse(ts).with_context(|| format!("--seek {ts:?}")))
        .transpose()?;

    if cli.sim.is_some() || cli.bench_seek.is_some() {
        let comp = open_timeline(&cli.asset)
            .with_context(|| format!("opening {}", cli.asset.display()))?;
        let start_frame: u32 = match seek_secs {
            Some(secs) => comp.frame_at_secs(secs)?,
            None => 0,
        };

        let aspect = cli.cell_aspect.unwrap_or(auto_ascii_core::DEFAULT_CELL_ASPECT);
        let tier = cli.sim_tier.or(cli.tier).unwrap_or(ColorTier::True);
        let mut glyphs = PaletteChoice::from(cli.palette).resolve_for_caps(&Caps::default());
        if let Some(spec) = &cli.font_table {
            glyphs = auto_ascii::load_font_table(spec)?.veto_tier(glyphs);
        }
        let mut deck = ClipDeck::new(
            comp.clips().iter().map(|c| c.path.clone()).collect(),
            DeckConfig {
                cell_aspect: aspect,
                repaint_full: cli.repaint == RepaintArg::Full,
                color: color_depth(tier),
                glyph_tier: glyphs,
            },
        );
        deck.set_codec(cli.codec.unwrap_or_default());
        if let Some(n) = cli.bench_seek {
            return run_bench_seek(&comp, deck, n);
        }
        return run_sim(&comp, deck, &cli, tier, start_frame);
    }

    let source = auto_ascii::Player::builder();
    let source = if Composition::is_toml_path(&cli.asset) {
        source.composition(&cli.asset)
    } else {
        source.asset(&cli.asset)
    };
    let mut builder = source
        .palette(cli.palette.into())
        .tier(cli.tier)
        .repaint(cli.repaint.into())
        .looping(cli.loop_playback)
        .no_query(cli.no_query)
        .no_quirks(cli.no_quirks)
        .no_cache(cli.no_cache)
        .no_backdrop(cli.no_backdrop);
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
    if let Some(codec) = cli.codec {
        builder = builder.codec(codec);
    }
    builder.build()?.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_flag_parsing() {
        assert_eq!(parse_codec("pixels"), Ok(Codec::Pixels));
        assert_eq!(parse_codec("letters"), Ok(Codec::Letters));
        assert_eq!(parse_codec("ascii"), Ok(Codec::Ascii));
        let e = parse_codec("ASCII").unwrap_err();
        assert!(e.contains("pixels, letters, ascii"), "the error lists the registry: {e}");
    }

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
        use auto_ascii::timecode::parse;
        assert_eq!(parse("42").unwrap(), 42.0);
        assert_eq!(parse("42.5").unwrap(), 42.5);
        assert_eq!(parse("1:30").unwrap(), 90.0);
        assert_eq!(parse("0:01:30.5").unwrap(), 90.5);
        assert_eq!(parse("2:00:00").unwrap(), 7200.0);
        assert!(parse("").is_err());
        assert!(parse("1:2:3:4").is_err());
        assert!(parse("-5").is_err());
        assert!(parse("abc").is_err());
    }
}
