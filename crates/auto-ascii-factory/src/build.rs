//! `auto-ascii-factory build` — the two-pass pipeline, decode to encode:
//!
//! - **pass 1:** stream rgb24 frames from ffmpeg, extract L\* luma, detect
//!   shot boundaries on the RAW histograms (histogram SAD + min shot
//!   length, [`crate::shots`]) while pooling per-shot levels histograms of
//!   the EMA'd luma — the EMA'd plane is what pass 2 stores, so NORM
//!   p2/p98 describe the actual stored bytes. The pass-1 Y-EMA resets at
//!   exactly the honored boundaries, mirroring pass 2 (both passes decode
//!   identical frames, so the schedules match deterministically).
//! - **pass 2:** identical ffmpeg invocation; write NORM (per-shot levels +
//!   cut flags — applied at RUNTIME by the player), then per frame run
//!   [`crate::features::FeatureExtractor`] (L\* + Scharr →
//!   doubled-angle orientation smoothing → hysteresis-thresholded unthinned
//!   E → Ex/Ey; top-hat + shadow → H; per-plane temporal EMA reset at
//!   cuts; RGB565 chroma) and stream all six planes (Y, E, Ex, Ey, H, C —
//!   registry order) through [`AsciiWriter`] under the `[build]` encode
//!   profile (temporal delta, keyframes every `keyframe_ivl`, zstd at
//!   `zstd_level`, CRCs on).
//!
//! The asset is written to `<out>.part` and renamed into place only after a
//! successful `finish()` — a killed build never leaves a plausible-looking
//! truncated `.ascii` behind (an asset missing its TRLR needs a factory
//! rerun anyway; this just makes the common case obvious). Byte-deterministic: no
//! timestamps, fixed zstd level, LUT/integer-only pixel math end to end
//! (see features.rs for the fixed-point EMA and rational orientation math).
//! Memory: all per-frame state is O(plane) and allocated once — planes
//! stream to the writer, never accumulate (features.rs memory note).

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use auto_ascii_format::{
    Meta, PlaneLevels, PlaneRef, ShotRecord, AsciiWriter, WriterOptions, norm_flags, plane_id,
    plane_raw_size,
};

use crate::extract::Extractor;
use crate::features::FeatureExtractor;
use crate::ffmpeg::{BoxErr, DecodeParams, FrameStream, probe};
use crate::params::Params;
use crate::shots::{Shot, ShotDetector, luma_histogram};
use crate::temporal::EmaPlane;

pub struct BuildArgs {
    pub input: PathBuf,
    pub output: PathBuf,
    pub ss: Option<f64>,
    pub t: Option<f64>,
    /// Effective tunables (params.toml + CLI overrides, validated) —
    /// fps/res/encode profile/shot detection/levels all live here.
    pub params: Params,
}

/// What a finished build produced: the numbers
/// `auto-ascii import` records in its sidecar and prints as JSON, read off
/// the same values the human "wrote …" line reports. Nothing here needs the
/// asset reopened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuildReport {
    /// Frames encoded (== the ASCI header's `frame_count`).
    pub frames: u32,
    /// Output frame rate (the header stores it as `fps`/1).
    pub fps: f64,
    /// `frames / fps`.
    pub duration_secs: f64,
    /// Stored plane width (`[build].base_w` after CLI overrides).
    pub base_w: u16,
    /// Stored plane height.
    pub base_h: u16,
    /// Size of the written `.ascii` file.
    pub bytes: u64,
}

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

fn reduced_aspect(w: u16, h: u16) -> (u16, u16) {
    fn gcd(a: u16, b: u16) -> u16 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let g = gcd(w, h).max(1);
    (w / g, h / g)
}

fn shot_records(shots: &[Shot]) -> Vec<ShotRecord> {
    shots
        .iter()
        .map(|s| {
            let mut levels = [PlaneLevels::default(); 8];
            levels[0] = PlaneLevels { p2: s.levels.lo, p98: s.levels.hi };
            ShotRecord {
                first_frame: s.first_frame,
                flags: if s.cut { norm_flags::CUT } else { 0 },
                levels,
            }
        })
        .collect()
}

/// Run the two-pass build. Every human progress/info line goes to `info`
/// (the bin hands it `stderr`; a `--json` caller hands it stderr too and
/// keeps stdout for the JSON object). The indicatif bars always draw on
/// stderr and are cleared before any line is written.
pub fn run(args: &BuildArgs, info: &mut dyn Write) -> Result<BuildReport, BoxErr> {
    if !args.input.is_file() {
        return Err(format!("input not found: {}", args.input.display()).into());
    }
    args.params.validate()?;
    let (w, h) = (args.params.build.base_w, args.params.build.base_h);
    let fps = args.params.build.fps;

    let probed = probe(&args.input)?;
    writeln!(
        info,
        "input: {} ({}x{}, {}) -> {}x{} @ {} fps, ASCI v1 Y+E+Ex+Ey+H+C (delta+zstd, NORM per-shot levels)",
        args.input.display(),
        probed.width,
        probed.height,
        probed.duration_secs.map_or_else(|| "unknown duration".into(), |d| format!("{d:.2}s")),
        w,
        h,
        fps
    )?;

    let params = DecodeParams { input: &args.input, ss: args.ss, t: args.t, fps, w, h };
    let extractor = Extractor::new(w, h);

    let pb = spinner("pass 1/2: shot detection + per-shot levels");
    let npx = w as usize * h as usize;
    let mut luma = vec![0u8; npx];
    let mut luma_ema = vec![0u8; npx];
    let mut ema_y = EmaPlane::new(npx, args.params.temporal.ema_alpha_y_milli);
    let mut detector = ShotDetector::with_params(
        npx as u64,
        args.params.shots.sad_threshold_milli,
        args.params.shots.min_shot_frames,
        args.params.levels.lo_pct,
        args.params.levels.hi_pct,
    );
    let frames = stream_frames(&params, |rgb| {
        extractor.luma(rgb, &mut luma);
        if detector.boundary(&luma_histogram(&luma)) {
            ema_y.reset();
        }
        ema_y.apply_u8(&luma, &mut luma_ema);
        detector.pool(&luma_histogram(&luma_ema));
        pb.inc(1);
        Ok(())
    })?;
    pb.finish_and_clear();
    if frames == 0 {
        return Err("ffmpeg produced zero frames (is --ss/--t outside the input's duration?)".into());
    }
    let shots = detector.finish();
    let cuts = shots.iter().filter(|s| s.cut).count();
    writeln!(
        info,
        "pass 1/2: {frames} frames, {} shot{} ({cuts} cut{}), Y levels per shot -> NORM",
        shots.len(),
        if shots.len() == 1 { "" } else { "s" },
        if cuts == 1 { "" } else { "s" },
    )?;

    let part = {
        let mut os = args.output.as_os_str().to_os_string();
        os.push(".part");
        PathBuf::from(os)
    };
    let result = encode_pass(args, &params, &shots, frames, &part);
    if let Err(e) = result {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, &args.output)
        .map_err(|e| format!("rename {} -> {}: {e}", part.display(), args.output.display()))?;

    let size = fs::metadata(&args.output).map(|m| m.len()).unwrap_or(0);
    writeln!(
        info,
        "wrote {} ({frames} frames, {:.1} KiB, {:.1} KiB/frame)",
        args.output.display(),
        size as f64 / 1024.0,
        size as f64 / 1024.0 / frames as f64
    )?;
    Ok(BuildReport {
        frames: u32::try_from(frames).map_err(|_| "frame count exceeds the u32 wire field")?,
        fps: f64::from(fps),
        duration_secs: frames as f64 / f64::from(fps),
        base_w: w,
        base_h: h,
        bytes: size,
    })
}

fn encode_pass(
    args: &BuildArgs,
    params: &DecodeParams<'_>,
    shots: &[Shot],
    expected_frames: u64,
    part: &Path,
) -> Result<(), BoxErr> {
    let (w, h) = (args.params.build.base_w, args.params.build.base_h);
    let (aspect_num, aspect_den) = reduced_aspect(w, h);
    let opts = WriterOptions {
        fps_num: args.params.build.fps,
        fps_den: 1,
        base_w: w,
        base_h: h,
        aspect_num,
        aspect_den,
        plane_ids: vec![
            plane_id::Y,
            plane_id::E,
            plane_id::EX,
            plane_id::EY,
            plane_id::H,
            plane_id::C,
        ],
        zstd_level: args.params.build.zstd_level,
        keyframe_ivl: u8::try_from(args.params.build.keyframe_ivl)
            .expect("keyframe_ivl validated to 1..=255"),
        ..WriterOptions::default()
    };
    let meta = Meta {
        factory_version: env!("CARGO_PKG_VERSION").to_string(),
        source: args
            .input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        palette_hints: Vec::new(),
    };

    let file = File::create(part).map_err(|e| format!("create {}: {e}", part.display()))?;
    let mut writer = AsciiWriter::new(BufWriter::new(file), opts, &meta)?;
    writer.write_norm(&shot_records(shots))?;

    let pb = ProgressBar::new(expected_frames).with_message("pass 2/2: encoding");
    pb.set_style(
        ProgressStyle::with_template(
            "{msg} [{bar:32}] {pos}/{len} frames ({per_sec}, eta {eta})",
        )
        .expect("static template")
        .progress_chars("=> "),
    );

    let cut_frames: Vec<u32> =
        shots.iter().filter(|s| s.cut).map(|s| s.first_frame).collect();
    let mut features = FeatureExtractor::new(w, h, &args.params);
    debug_assert_eq!(
        features.y().len(),
        plane_raw_size(w, h, plane_id::Y).expect("Y is known"),
        "feature planes must match the wire geometry"
    );
    let mut frame_idx = 0u32;
    let encoded = stream_frames(params, |rgb| {
        features.process(rgb, cut_frames.binary_search(&frame_idx).is_ok());
        frame_idx += 1;
        writer.write_frame(&[
            PlaneRef { id: plane_id::Y, data: features.y() },
            PlaneRef { id: plane_id::E, data: features.e() },
            PlaneRef { id: plane_id::EX, data: features.ex() },
            PlaneRef { id: plane_id::EY, data: features.ey() },
            PlaneRef { id: plane_id::H, data: features.h() },
            PlaneRef { id: plane_id::C, data: features.c() },
        ])?;
        pb.inc(1);
        Ok(())
    })?;
    pb.finish_and_clear();

    if encoded != expected_frames {
        return Err(format!(
            "frame count changed between passes (pass 1: {expected_frames}, pass 2: {encoded})"
        )
        .into());
    }

    writer.finish()?;
    Ok(())
}
