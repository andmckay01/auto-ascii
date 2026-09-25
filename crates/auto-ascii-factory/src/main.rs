use std::path::PathBuf;
use std::process::ExitCode;

use auto_ascii_factory::{build, effective_params, eval, font_table, parse_res, sweep};
use clap::{Parser, Subcommand};
use auto_ascii_format::{
    CHUNK_HEADER_SIZE, ChunkHeader, AsciiHeader, AsciiReader, TAG_FRAM, frame_flags, header_flags,
    plane_id,
};

#[derive(Parser)]
#[command(name = "auto-ascii-factory", version, about = "Distill video into ASCI feature assets")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build an asset: `auto-ascii-factory build <in.mp4> -o out.ascii
    /// [--ss T] [--t T] [--fps 30] [--res 480x270]`.
    /// Emits ASCI v1: Y, E, Ex, Ey, H and C planes, temporal delta +
    /// keyframes, NORM per-shot levels + cut flags (applied at runtime by
    /// the player).
    Build {
        /// Input video (any ffmpeg-readable container).
        input: PathBuf,
        /// Output .ascii path.
        #[arg(short, long)]
        output: PathBuf,
        /// Start offset in seconds (ffmpeg `-ss`, input seeking).
        #[arg(long, value_parser = parse_ss)]
        ss: Option<f64>,
        /// Duration limit in seconds (ffmpeg `-t`).
        #[arg(long = "t", value_parser = parse_t)]
        t: Option<f64>,
        /// Output frame rate override (default: params.toml `build.fps`).
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..=1000))]
        fps: Option<u16>,
        /// Stored plane resolution override as WxH (default: params.toml
        /// `build.base_w/base_h`). Dimensions must be even and >= 2: the
        /// chroma plane C is stored at half res.
        #[arg(long, value_parser = parse_res)]
        res: Option<(u16, u16)>,
        /// Tunables file (every tunable lives in params.toml — the agent
        /// socket). Missing keys keep the embedded defaults.
        #[arg(long)]
        params: Option<PathBuf>,
    },
    /// Print header, chunks, sizes, per-plane value stats; verify CRCs.
    Inspect {
        /// Asset to inspect.
        asset: PathBuf,
        /// Dump decoded planes of sampled frames into this directory as
        /// PGM/PPM images (Y/E as gray, Ex/Ey as bias-128 gray, H as a
        /// flag map, C as color) for eyeball review.
        #[arg(long)]
        dump_planes: Option<PathBuf>,
        /// Frame indices for --dump-planes and the stats sampler
        /// (repeatable; default: 4 frames spread over the asset).
        #[arg(long)]
        frame: Vec<u32>,
    },
    /// Inspect the effective tunables: `auto-ascii-factory params --dump`
    /// prints the merged config (embedded defaults + --params file) as TOML.
    Params {
        /// Tunables file to merge over the embedded defaults.
        #[arg(long)]
        params: Option<PathBuf>,
        /// Print the effective config to stdout.
        #[arg(long)]
        dump: bool,
    },
    /// The agent socket: build every video in --corpus
    /// (cached by input+params sha), run the player pipeline headlessly,
    /// emit metrics JSON (+ HTML contact sheet), optionally compare against
    /// a baseline (nonzero exit on tolerance breach).
    Eval {
        /// Directory of corpus videos (non-recursive; mp4/mov/mkv/webm/avi).
        #[arg(long)]
        corpus: PathBuf,
        /// Tunables file (see `build --params`).
        #[arg(long)]
        params: Option<PathBuf>,
        /// Baseline metrics JSON to compare against (runs/base.json).
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Output metrics JSON path (e.g. runs/X.json).
        #[arg(long)]
        out: PathBuf,
        /// Optional self-contained HTML contact sheet path (runs/X.html).
        #[arg(long)]
        html: Option<PathBuf>,
        /// Optional review-reel HTML path (human sign-off artifact):
        /// per clip, >=4 source|render timestamp rows with per-frame
        /// metrics plus an animated GIF of the rasterized render.
        #[arg(long)]
        reel: Option<PathBuf>,
        /// Asset cache directory, keyed by (input sha, params sha).
        #[arg(long, default_value = "runs/cache")]
        cache_dir: PathBuf,
        /// Ink-coverage table for the SSIM rasterizer: a built-in
        /// name (conservative, dejavu-sans-mono, liberation-mono,
        /// ubuntu-mono, noto-sans-mono) or a path to a `auto-ascii-factory
        /// font-table` TOML. Default: conservative (the committed baseline's
        /// table — absolute SSIM is only comparable within one table).
        #[arg(long)]
        font_table: Option<String>,
    },
    /// Rasterize every glyph the 8 shipped palettes can emit through a
    /// monospace font at 64x128 px and write a deterministic
    /// ink-coverage table (TOML). The committed tables under fonts/ are
    /// generated this way (see fonts/README.md); `--font-table NAME|PATH`
    /// consumes them in the player and in `eval`.
    FontTable {
        /// Monospace font file (.ttf/.otf). Omit with --conservative.
        font: Option<PathBuf>,
        /// Output table path (e.g. fonts/dejavu-sans-mono.toml).
        #[arg(short, long)]
        output: PathBuf,
        /// Table name recorded in the file (default: the font file stem).
        #[arg(long)]
        name: Option<String>,
        /// Emit the built-in conservative (ASCII-repertoire) table in the
        /// same format instead of rasterizing a font.
        #[arg(long)]
        conservative: bool,
    },
    /// Parameter sweep: run eval per combo of the
    /// axes declared in --grid (values within an axis travel together; axes
    /// cross), score each combo (default 0.4*ssim + 0.4*edgeF1 -
    /// 0.2*flicker/2.0) and emit ranked results JSON + a leaderboard HTML.
    /// Combos share the eval asset cache, so factory-identical combos never
    /// rebuild assets.
    Sweep {
        /// Directory of corpus videos (see `eval --corpus`).
        #[arg(long)]
        corpus: PathBuf,
        /// Base tunables file every combo starts from (see `build --params`).
        #[arg(long)]
        params: Option<PathBuf>,
        /// Sweep spec: axes of param overrides + optional [score] weights.
        #[arg(long)]
        grid: PathBuf,
        /// Output directory: combo-NN.json + sweep.json + leaderboard.html.
        #[arg(long)]
        out: PathBuf,
        /// Asset cache directory shared with `eval`.
        #[arg(long, default_value = "runs/cache")]
        cache_dir: PathBuf,
    },
}

fn parse_ss(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|e| format!("bad --ss seconds: {e}"))?;
    if !v.is_finite() || v < 0.0 {
        return Err("--ss must be a finite number of seconds >= 0".into());
    }
    Ok(v)
}

fn parse_t(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|e| format!("bad --t seconds: {e}"))?;
    if !v.is_finite() || v <= 0.0 {
        return Err("--t must be a finite number of seconds > 0".into());
    }
    Ok(v)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Build { input, output, ss, t, fps, res, params } => {
            effective_params(params.as_deref(), fps, res).and_then(|params| {
                build::run(
                    &build::BuildArgs { input, output, ss, t, params },
                    &mut std::io::stderr(),
                )
                .map(|_report| ())
            })
        }
        Cmd::Inspect { asset, dump_planes, frame } => {
            cmd_inspect(&asset, dump_planes.as_deref(), &frame)
        }
        Cmd::Params { params, dump } => effective_params(params.as_deref(), None, None)
            .and_then(|p| {
                if dump {
                    print!("{}", p.dump());
                    Ok(())
                } else {
                    Err("params: nothing to do (use --dump to print the effective config)".into())
                }
            }),
        Cmd::Eval { corpus, params, baseline, out, html, reel, cache_dir, font_table } => {
            effective_params(params.as_deref(), None, None).and_then(|params| {
                eval::run(&eval::EvalArgs {
                    corpus,
                    params,
                    baseline,
                    out,
                    html,
                    reel,
                    cache_dir,
                    truecolor_only: false,
                    font_table,
                })
            })
        }
        Cmd::FontTable { font, output, name, conservative } => {
            font_table::run(&font_table::FontTableArgs { font, output, name, conservative })
        }
        Cmd::Sweep { corpus, params, grid, out, cache_dir } => {
            effective_params(params.as_deref(), None, None).and_then(|base| {
                sweep::run(&sweep::SweepArgs { corpus, base, grid, out_dir: out, cache_dir })
            })
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("auto-ascii-factory: {e}");
            ExitCode::FAILURE
        }
    }
}

fn plane_name(id: u8) -> &'static str {
    match id {
        plane_id::Y => "Y",
        plane_id::E => "E",
        plane_id::EX => "Ex",
        plane_id::EY => "Ey",
        plane_id::H => "H",
        plane_id::C => "C",
        _ => "?",
    }
}

struct FramStats {
    frames: u64,
    keyframes: u64,
    per_plane: Vec<(u8, u64, u64)>,
}

fn walk_frames(bytes: &[u8], h: &AsciiHeader) -> Result<FramStats, Box<dyn std::error::Error>> {
    let crc_len: usize = if h.flags & header_flags::CRCS_PRESENT != 0 { 4 } else { 0 };
    let end = usize::try_from(h.index_offset).map_err(|_| "index_offset exceeds file")?;
    if end > bytes.len() {
        return Err("index_offset exceeds file".into());
    }
    let mut stats = FramStats { frames: 0, keyframes: 0, per_plane: Vec::new() };
    let mut pos = auto_ascii_format::HEADER_SIZE as usize;
    while pos < end {
        let hdr_bytes: &[u8; CHUNK_HEADER_SIZE] = bytes
            .get(pos..pos + CHUNK_HEADER_SIZE)
            .and_then(|s| s.try_into().ok())
            .ok_or("truncated chunk header before FIDX")?;
        let ch = ChunkHeader::from_bytes(hdr_bytes);
        let size = usize::try_from(ch.size).map_err(|_| "chunk size exceeds file")?;
        let payload =
            bytes.get(pos + CHUNK_HEADER_SIZE..pos + CHUNK_HEADER_SIZE + size)
                .ok_or("truncated chunk payload")?;
        if ch.tag == TAG_FRAM {
            if payload.len() < 5 {
                return Err("FRAM payload shorter than its fixed header".into());
            }
            stats.frames += 1;
            if payload[4] & frame_flags::KEYFRAME != 0 {
                stats.keyframes += 1;
            }
            let mut off = 5usize;
            while off + 9 <= payload.len() {
                let id = payload[off];
                let comp =
                    u32::from_le_bytes(payload[off + 1..off + 5].try_into().unwrap()) as u64;
                let raw =
                    u32::from_le_bytes(payload[off + 5..off + 9].try_into().unwrap()) as u64;
                match stats.per_plane.iter_mut().find(|(pid, _, _)| *pid == id) {
                    Some((_, c, r)) => {
                        *c += comp;
                        *r += raw;
                    }
                    None => stats.per_plane.push((id, comp, raw)),
                }
                off += usize::try_from((9 + comp).div_ceil(64) * 64)
                    .map_err(|_| "subblock size overflow")?;
            }
        }
        pos += CHUNK_HEADER_SIZE + size + crc_len;
    }
    Ok(stats)
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn sample_frames(frame_count: u32) -> Vec<u32> {
    let n = frame_count;
    let want = 4u32.min(n);
    let mut out = Vec::new();
    for k in 0..want {
        let f = if want <= 1 { 0 } else { k * (n - 1) / (want - 1) };
        if out.last() != Some(&f) {
            out.push(f);
        }
    }
    out
}

fn plane_stats(
    reader: &mut AsciiReader<'_>,
    frames: &[u32],
) -> Result<(), Box<dyn std::error::Error>> {
    let h = reader.header().clone();
    println!(
        "  stats:        over {} sampled frame{} {frames:?}",
        frames.len(),
        if frames.len() == 1 { "" } else { "s" }
    );
    for &id in &h.plane_ids[..h.plane_count as usize] {
        let Some((pw, ph)) = reader.plane_dims(id) else { continue };
        let mut buf = vec![0u8; auto_ascii_format::plane_raw_size(h.base_w, h.base_h, id).unwrap()];
        let n = pw as usize * ph as usize;
        let (mut min, mut max, mut sum, mut nonzero) = (255u8, 0u8, 0u64, 0u64);
        let (mut bit0, mut bit1, mut dev_sum, mut dev_max) = (0u64, 0u64, 0u64, 0u8);
        for &f in frames {
            reader.seek_plane_into(f, id, &mut buf)?;
            for &v in &buf[..if id == plane_id::C { buf.len() } else { n }] {
                min = min.min(v);
                max = max.max(v);
                sum += u64::from(v);
                nonzero += u64::from(v != 0);
                bit0 += u64::from(v & 1 != 0);
                bit1 += u64::from(v & 2 != 0);
                let d = v.abs_diff(128);
                dev_sum += u64::from(d);
                dev_max = dev_max.max(d);
            }
        }
        let total = (frames.len() * if id == plane_id::C { buf.len() } else { n }) as u64;
        let pct = |c: u64| 100.0 * c as f64 / total.max(1) as f64;
        match id {
            plane_id::E => println!(
                "    E   min {min} mean {:.1} max {max}, nonzero {:.2}% (unthinned edge mass)",
                sum as f64 / total as f64,
                pct(nonzero)
            ),
            plane_id::EX | plane_id::EY => println!(
                "    {:<3} |v-128| mean {:.2} max {dev_max} (doubled-angle, bias 128)",
                plane_name(id),
                dev_sum as f64 / total as f64
            ),
            plane_id::H => println!(
                "    H   highlight {:.2}% deep-shadow {:.2}%",
                pct(bit0),
                pct(bit1)
            ),
            _ => println!(
                "    {:<3} min {min} mean {:.1} max {max}",
                plane_name(id),
                sum as f64 / total as f64
            ),
        }
    }
    Ok(())
}

fn dump_planes(
    reader: &mut AsciiReader<'_>,
    dir: &std::path::Path,
    frames: &[u32],
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let h = reader.header().clone();
    for &f in frames {
        for &id in &h.plane_ids[..h.plane_count as usize] {
            let Some((pw, ph)) = reader.plane_dims(id) else { continue };
            let mut buf =
                vec![0u8; auto_ascii_format::plane_raw_size(h.base_w, h.base_h, id).unwrap()];
            reader.seek_plane_into(f, id, &mut buf)?;
            let name = plane_name(id).to_ascii_lowercase();
            if id == plane_id::C {
                let mut rgb = Vec::with_capacity(pw as usize * ph as usize * 3);
                for px in buf.chunks_exact(2) {
                    let v = u16::from_le_bytes([px[0], px[1]]);
                    let (r, g, b) = ((v >> 11) as u8, ((v >> 5) & 0x3F) as u8, (v & 0x1F) as u8);
                    rgb.push((r << 3) | (r >> 2));
                    rgb.push((g << 2) | (g >> 4));
                    rgb.push((b << 3) | (b >> 2));
                }
                let path = dir.join(format!("f{f:05}-{name}.ppm"));
                std::fs::write(&path, [format!("P6\n{pw} {ph}\n255\n").as_bytes(), &rgb].concat())?;
            } else {
                if id == plane_id::H {
                    for v in &mut buf {
                        *v = match *v & 3 {
                            1 | 3 => 255,
                            2 => 90,
                            _ => 0,
                        };
                    }
                }
                let path = dir.join(format!("f{f:05}-{name}.pgm"));
                std::fs::write(&path, [format!("P5\n{pw} {ph}\n255\n").as_bytes(), &buf].concat())?;
            }
        }
    }
    eprintln!("dumped {} frame(s) x {} plane(s) to {}", frames.len(), h.plane_count, dir.display());
    Ok(())
}

fn cmd_inspect(
    asset: &std::path::Path,
    dump_dir: Option<&std::path::Path>,
    frames: &[u32],
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(asset).map_err(|e| format!("read {}: {e}", asset.display()))?;
    let reader = AsciiReader::open(&bytes)?;
    let h = reader.header();

    println!("{}: ASCI v{}.{}", asset.display(), h.version_major, h.version_minor);
    println!("  file size:    {} bytes", bytes.len());
    println!(
        "  flags:        {:#06x} (index: {}, crcs: {})",
        h.flags,
        h.flags & header_flags::INDEX_PRESENT != 0,
        h.flags & header_flags::CRCS_PRESENT != 0
    );
    println!("  fps:          {}/{}", h.fps_num, h.fps_den);
    println!("  base res:     {}x{}", h.base_w, h.base_h);
    println!("  aspect:       {}:{}", h.aspect_num, h.aspect_den);
    let secs = if h.fps_num > 0 {
        h.frame_count as f64 * h.fps_den as f64 / h.fps_num as f64
    } else {
        0.0
    };
    println!("  frames:       {} ({secs:.2}s)", h.frame_count);
    let names: Vec<&str> =
        h.plane_ids[..h.plane_count as usize].iter().map(|&id| plane_name(id)).collect();
    println!("  planes:       {} [{}]", h.plane_count, names.join(", "));
    println!("  codec/filter: {}/{} (0=raw 1=lz4 2=zstd / 0=intra 1=delta)", h.codec, h.filter);
    println!("  keyframe ivl: {}", h.keyframe_ivl);
    println!("  index_offset: {}", h.index_offset);
    println!("  meta_offset:  {}", h.meta_offset);
    if h.frame_count > 0 {
        let payload = h.index_offset.saturating_sub(u64::from(auto_ascii_format::HEADER_SIZE));
        println!(
            "  avg frame:    {:.1} KiB (chunked payload before FIDX)",
            payload as f64 / 1024.0 / h.frame_count as f64
        );
    }

    let meta = reader.meta()?;
    println!(
        "  meta:         factory {} | source {:?} | palette hints {:?}",
        meta.factory_version, meta.source, meta.palette_hints
    );

    let shots = reader.shots();
    if shots.is_empty() {
        println!("  shots:        none (no NORM chunk — pre-M1 asset)");
    } else {
        let cuts = shots.iter().filter(|s| s.is_cut()).count();
        let starts: Vec<String> =
            shots.iter().take(8).map(|s| s.first_frame.to_string()).collect();
        println!(
            "  shots:        {} ({cuts} cut-flagged) starting at [{}{}]",
            shots.len(),
            starts.join(", "),
            if shots.len() > 8 { ", ..." } else { "" }
        );
    }
    let stats = walk_frames(&bytes, h)?;
    if stats.frames != u64::from(h.frame_count) {
        return Err(format!(
            "FRAM walk found {} frames but the header says {}",
            stats.frames, h.frame_count
        )
        .into());
    }
    println!(
        "  keyframes:    {} of {} frames (interval {})",
        stats.keyframes, h.frame_count, h.keyframe_ivl
    );
    let mut raw_total = 0u64;
    for &(id, comp, raw) in &stats.per_plane {
        raw_total += raw;
        println!(
            "  plane {:<2}      {:.2} MiB compressed / {:.2} MiB raw ({:.1}x)",
            plane_name(id),
            mib(comp),
            mib(raw),
            raw as f64 / comp.max(1) as f64
        );
    }
    println!(
        "  compression:  {:.2} MiB raw planes -> {:.2} MiB file ({:.1}x vs raw)",
        mib(raw_total),
        mib(bytes.len() as u64),
        raw_total as f64 / bytes.len().max(1) as f64
    );

    if h.frame_count > 0 {
        let sampled = if frames.is_empty() {
            sample_frames(h.frame_count)
        } else {
            for &f in frames {
                if f >= h.frame_count {
                    return Err(format!(
                        "--frame {f} out of range (asset has {} frames)",
                        h.frame_count
                    )
                    .into());
                }
            }
            frames.to_vec()
        };
        let mut reader = AsciiReader::open(&bytes)?;
        plane_stats(&mut reader, &sampled)?;
        if let Some(dir) = dump_dir {
            dump_planes(&mut reader, dir, &sampled)?;
        }
    }

    reader.verify()?;
    println!("  integrity:    OK (all chunk CRCs verified, TRLR present)");
    Ok(())
}
