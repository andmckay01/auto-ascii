//! `sleepy-factory` — offline asset factory (PLAN §5).
//!
//! M0 pipeline (PLAN §7 + approved simplification): ffmpeg subprocess
//! (`-vf scale=W:H:flags=area,fps=N,format=gray -f rawvideo -` piped, never
//! libav bindings) → sRGB→linear→L* luma → global p2/p98 level normalization
//! BAKED into the plane at encode time (NORM chunk + per-shot levels arrive
//! at M1/M3) → intra zstd FRAM stream via `slpy_format::SlpyWriter`.
//! `ffprobe -print_format json` supplies metadata (`serde_json`).
//!
//! CLI shape per PLAN §5; `eval` and `sweep` land at M2 with slpy-eval.
//! Progress/diagnostics go to stderr; stdout stays clean (inspect's report is
//! the one stdout product).

mod build;
mod ffmpeg;
mod lut;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use slpy_format::{SlpyReader, header_flags, plane_id};

#[derive(Parser)]
#[command(name = "sleepy-factory", version, about = "Distill video into SLPY feature assets (PLAN §5)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build an asset: `sleepy-factory build <in.mp4> -o out.slpy
    /// [--ss T] [--t T] [--fps 30] [--res 480x270]`.
    /// M0 emits luma-only SLPY-lite (intra, zstd, baked global p2/p98 levels).
    Build {
        /// Input video (any ffmpeg-readable container).
        input: PathBuf,
        /// Output .slpy path.
        #[arg(short, long)]
        output: PathBuf,
        /// Start offset in seconds (ffmpeg `-ss`, input seeking).
        #[arg(long, value_parser = parse_ss)]
        ss: Option<f64>,
        /// Duration limit in seconds (ffmpeg `-t`).
        #[arg(long = "t", value_parser = parse_t)]
        t: Option<f64>,
        /// Output frame rate (fps filter; header fps_num, fps_den = 1).
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u16).range(1..=1000))]
        fps: u16,
        /// Stored plane resolution as WxH (PLAN §4 base res).
        #[arg(long, default_value = "480x270", value_parser = parse_res)]
        res: (u16, u16),
        /// Tunables file (PLAN §5: every tunable lives in params.toml — the
        /// agent socket). Accepted but unused at M0; defaults are compiled in.
        #[arg(long)]
        params: Option<PathBuf>,
    },
    /// Print header, chunks, sizes; verify CRCs (PLAN §5 CLI shape).
    Inspect {
        /// Asset to inspect.
        asset: PathBuf,
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

fn parse_res(s: &str) -> Result<(u16, u16), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("bad --res {s:?}: expected WxH, e.g. 480x270"))?;
    let w: u16 = w.trim().parse().map_err(|e| format!("bad --res width: {e}"))?;
    let h: u16 = h.trim().parse().map_err(|e| format!("bad --res height: {e}"))?;
    if w == 0 || h == 0 {
        return Err("--res dimensions must be nonzero".into());
    }
    Ok((w, h))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Build { input, output, ss, t, fps, res, params } => {
            if let Some(p) = &params {
                eprintln!(
                    "note: --params {} is accepted but unused at M0 (tunables land at M2)",
                    p.display()
                );
            }
            build::run(&build::BuildArgs { input, output, ss, t, fps, res })
        }
        Cmd::Inspect { asset } => cmd_inspect(&asset),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sleepy-factory: {e}");
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

/// Header + structural walk + full CRC check via `SlpyReader::open`/`verify`
/// (PLAN §5). M0 has no mmap here — `fs::read` is fine for an offline tool.
fn cmd_inspect(asset: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(asset).map_err(|e| format!("read {}: {e}", asset.display()))?;
    let reader = SlpyReader::open(&bytes)?;
    let h = reader.header();

    println!("{}: SLPY v{}.{}", asset.display(), h.version_major, h.version_minor);
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
        let payload = h.index_offset.saturating_sub(u64::from(slpy_format::HEADER_SIZE));
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

    reader.verify()?;
    println!("  integrity:    OK (all chunk CRCs verified, TRLR present)");
    Ok(())
}
