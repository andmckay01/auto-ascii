//! `sleepy-factory build` — the M0 two-pass pipeline (PLAN §5 stages
//! 1 + 3(luma) + 5(global levels) + 6, approved M0 simplification):
//!
//! - **pass 1:** stream gray frames from ffmpeg, accumulate a global 256-bin
//!   histogram (transformed to the L\* domain for the percentiles) and learn
//!   the exact frame count.
//! - **pass 2:** identical ffmpeg invocation, apply the combined
//!   sRGB→linear→L\*→p2/p98 LUT per byte, stream Y planes into
//!   [`SlpyWriter`] (intra, zstd-19, CRCs on).
//!
//! The asset is written to `<out>.part` and renamed into place only after a
//! successful `finish()` — a killed build never leaves a plausible-looking
//! truncated `.slpy` behind (PLAN §4: missing TRLR ⇒ factory rerun anyway;
//! this just makes the common case obvious). Byte-deterministic: no
//! timestamps, fixed zstd level, LUT-only pixel math.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use slpy_format::{Meta, PlaneRef, SlpyWriter, WriterOptions, plane_id};

use crate::ffmpeg::{BoxErr, DecodeParams, FrameStream, probe};
use crate::lut;

pub struct BuildArgs {
    pub input: PathBuf,
    pub output: PathBuf,
    pub ss: Option<f64>,
    pub t: Option<f64>,
    pub fps: u16,
    pub res: (u16, u16),
}

/// Run one full decode pass, feeding every frame to `on_frame`. Returns the
/// frame count. Error precedence: ffmpeg's own nonzero exit (with its stderr)
/// beats a pipe-side short read; an `on_frame` failure aborts ffmpeg and is
/// reported as ours.
fn stream_frames(
    params: &DecodeParams<'_>,
    mut on_frame: impl FnMut(&[u8]) -> Result<(), BoxErr>,
) -> Result<u64, BoxErr> {
    let mut stream = FrameStream::spawn(params)?;
    let mut buf = vec![0u8; stream.frame_size()];
    let mut frames = 0u64;
    let pipe_err: Option<BoxErr> = loop {
        match stream.next_frame(&mut buf) {
            Ok(true) => {
                if let Err(e) = on_frame(&buf) {
                    stream.abort();
                    return Err(e);
                }
                frames += 1;
            }
            Ok(false) => break None,
            Err(e) => break Some(e),
        }
    };
    // ffmpeg's own failure is the root cause when both went wrong.
    stream.finish()?;
    match pipe_err {
        Some(e) => Err(e),
        None => Ok(frames),
    }
}

fn spinner(msg: &'static str) -> ProgressBar {
    let pb = ProgressBar::new_spinner().with_message(msg);
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

/// Reduce `w:h` to the smallest integer aspect ratio for the header.
fn reduced_aspect(w: u16, h: u16) -> (u16, u16) {
    fn gcd(a: u16, b: u16) -> u16 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let g = gcd(w, h).max(1);
    (w / g, h / g)
}

pub fn run(args: &BuildArgs) -> Result<(), BoxErr> {
    if !args.input.is_file() {
        return Err(format!("input not found: {}", args.input.display()).into());
    }
    let (w, h) = args.res;

    // Stage 1 (PLAN §5): validate via ffprobe before spending a decode pass.
    let info = probe(&args.input)?;
    eprintln!(
        "input: {} ({}x{}, {}) -> {}x{} @ {} fps, luma-only SLPY-lite",
        args.input.display(),
        info.width,
        info.height,
        info.duration_secs.map_or_else(|| "unknown duration".into(), |d| format!("{d:.2}s")),
        w,
        h,
        args.fps
    );

    let params =
        DecodeParams { input: &args.input, ss: args.ss, t: args.t, fps: args.fps, w, h };

    // ---- pass 1: global histogram + frame count ----------------------------
    let pb = spinner("pass 1/2: scanning luma histogram");
    let mut hist = [0u64; 256];
    let frames = stream_frames(&params, |frame| {
        for &b in frame {
            hist[b as usize] += 1;
        }
        pb.inc(1);
        Ok(())
    })?;
    pb.finish_and_clear();
    if frames == 0 {
        return Err("ffmpeg produced zero frames (is --ss/--t outside the input's duration?)".into());
    }

    // Percentiles live in the L* domain: fold the raw-byte histogram through
    // the monotone sRGB→L* table, then p2/p98 over that.
    let lstar = lut::srgb_to_lstar_lut();
    let mut hist_l = [0u64; 256];
    for (g, &count) in hist.iter().enumerate() {
        hist_l[lstar[g] as usize] += count;
    }
    let levels = lut::percentile_levels(&hist_l).expect("frames > 0 implies nonempty histogram");
    let out_lut = lut::build_output_lut(&lstar, levels);
    eprintln!("pass 1/2: {frames} frames, L* levels p2={} p98={}", levels.lo, levels.hi);

    // ---- pass 2: LUT + encode ----------------------------------------------
    // Write to `<out>.part`, rename on success.
    let part = {
        let mut os = args.output.as_os_str().to_os_string();
        os.push(".part");
        PathBuf::from(os)
    };
    let result = encode_pass(args, &params, &out_lut, frames, &part);
    if result.is_err() {
        let _ = fs::remove_file(&part);
        return result;
    }
    fs::rename(&part, &args.output)
        .map_err(|e| format!("rename {} -> {}: {e}", part.display(), args.output.display()))?;

    let size = fs::metadata(&args.output).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "wrote {} ({frames} frames, {:.1} KiB, {:.1} KiB/frame)",
        args.output.display(),
        size as f64 / 1024.0,
        size as f64 / 1024.0 / frames as f64
    );
    Ok(())
}

fn encode_pass(
    args: &BuildArgs,
    params: &DecodeParams<'_>,
    out_lut: &[u8; 256],
    expected_frames: u64,
    part: &Path,
) -> Result<(), BoxErr> {
    let (w, h) = args.res;
    let (aspect_num, aspect_den) = reduced_aspect(w, h);
    let opts = WriterOptions {
        fps_num: args.fps,
        fps_den: 1,
        base_w: w,
        base_h: h,
        aspect_num,
        aspect_den,
        plane_ids: vec![plane_id::Y],
        ..WriterOptions::default() // M0 profile: zstd-19, intra, keyframe_ivl 60, CRCs on
    };
    let meta = Meta {
        factory_version: env!("CARGO_PKG_VERSION").to_string(),
        // File NAME only — absolute paths would break byte-determinism
        // across checkouts (Meta determinism rules).
        source: args
            .input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        palette_hints: Vec::new(),
    };

    let file = File::create(part).map_err(|e| format!("create {}: {e}", part.display()))?;
    let mut writer = SlpyWriter::new(BufWriter::new(file), opts, &meta)?;

    let pb = ProgressBar::new(expected_frames).with_message("pass 2/2: encoding");
    pb.set_style(
        ProgressStyle::with_template(
            "{msg} [{bar:32}] {pos}/{len} frames ({per_sec}, eta {eta})",
        )
        .expect("static template")
        .progress_chars("=> "),
    );

    let mut plane = vec![0u8; params.frame_size()];
    let encoded = stream_frames(params, |frame| {
        for (dst, &src) in plane.iter_mut().zip(frame) {
            *dst = out_lut[src as usize];
        }
        writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &plane }])?;
        pb.inc(1);
        Ok(())
    })?;
    pb.finish_and_clear();

    // Both passes run the identical ffmpeg command on the same file; a
    // mismatch means the input changed under us (or ffmpeg is nondeterministic
    // here) — either way the histogram no longer matches the frames.
    if encoded != expected_frames {
        return Err(format!(
            "frame count changed between passes (pass 1: {expected_frames}, pass 2: {encoded})"
        )
        .into());
    }

    writer.finish()?;
    Ok(())
}
