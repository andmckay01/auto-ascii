//! `sleepy-factory` — offline asset factory (PLAN §5).
//!
//! M1 pipeline (PLAN §5/§7): ffmpeg subprocess (`-vf
//! scale=W:H:flags=area,fps=N,format=rgb24 -f rawvideo -` piped, never libav
//! bindings) → pass 1: sRGB→linear→L* luma, shot detection (histogram SAD +
//! min shot length) and per-shot p2/p98 levels → pass 2: NORM chunk (levels
//! applied at RUNTIME by the player — nothing baked into the planes) + Y
//! (L*, full res) + C (RGB565, half res, 2×2 area average) planes through
//! the SLPY v1 writer (temporal delta + keyframes, zstd-19, CRCs).
//! `ffprobe -print_format json` supplies metadata (`serde_json`).
//!
//! CLI shape per PLAN §5; `eval` and `sweep` land at M2 with slpy-eval.
//! Progress/diagnostics go to stderr; stdout stays clean (inspect's report is
//! the one stdout product).

mod build;
mod extract;
mod ffmpeg;
mod lut;
mod shots;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use slpy_format::{
    CHUNK_HEADER_SIZE, ChunkHeader, SlpyHeader, SlpyReader, TAG_FRAM, frame_flags, header_flags,
    plane_id,
};

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
    /// M1 emits SLPY v1: Y + C planes, temporal delta + keyframes, NORM
    /// per-shot levels + cut flags (applied at runtime by the player).
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
        /// Stored plane resolution as WxH (PLAN §4 base res). Dimensions
        /// must be even: the chroma plane C is stored at half res.
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
    if !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err("--res dimensions must be even (chroma plane C is stored at half res)".into());
    }
    Ok((w, h))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Build { input, output, ss, t, fps, res, params } => {
            if let Some(p) = &params {
                eprintln!(
                    "note: --params {} is accepted but unused at M1 (tunables land at M2)",
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

/// Per-plane and keyframe totals from a manual FRAM walk (wire layout frozen
/// in PLAN §4 / slpy-format write.rs: `frame_idx u32 | flags u8 | [id u8 |
/// comp u32 | raw u32 | data | pad-to-64] × planes`). The walk reads only
/// chunk and subblock headers — payload bytes are skipped by size.
struct FramStats {
    frames: u64,
    keyframes: u64,
    /// `(plane_id, compressed bytes, raw bytes)` in first-seen order.
    per_plane: Vec<(u8, u64, u64)>,
}

fn walk_frames(bytes: &[u8], h: &SlpyHeader) -> Result<FramStats, Box<dyn std::error::Error>> {
    let crc_len: usize = if h.flags & header_flags::CRCS_PRESENT != 0 { 4 } else { 0 };
    let end = usize::try_from(h.index_offset).map_err(|_| "index_offset exceeds file")?;
    if end > bytes.len() {
        return Err("index_offset exceeds file".into());
    }
    let mut stats = FramStats { frames: 0, keyframes: 0, per_plane: Vec::new() };
    let mut pos = slpy_format::HEADER_SIZE as usize;
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
                // Subblocks are padded to 64-B alignment (PLAN §4).
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

    // M1 additions: shots/cuts, keyframe count, per-plane sizes, ratio vs raw.
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

    reader.verify()?;
    println!("  integrity:    OK (all chunk CRCs verified, TRLR present)");
    Ok(())
}
