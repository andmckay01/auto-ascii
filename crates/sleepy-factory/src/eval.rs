//! `sleepy-factory eval` — the M2 agent socket (PLAN §5/§6, item B).
//!
//! For every video in `--corpus`: build (or reuse) the asset, then drive the
//! REAL player pipeline (`sleepy_player::pipeline::Player`, extracted to a
//! lib at M2 exactly so this driver measures the renderer and not a
//! reimplementation) headlessly against `SimBackend`, collecting the §6
//! metrics:
//!
//! - **downscale-SSIM** at the truecolor tier: grid rasterized through the
//!   conservative ink-coverage table, viewport-cropped, compared against the
//!   source luma normalized by eval-owned per-frame percentiles (independent
//!   of the factory's NORM levels — no self-grading; see [`frame_ssim`]) and
//!   downscaled through the player's own resampler (sampled every
//!   `eval.ssim_every` frames);
//! - **asset structure**: shot/cut counts, keyframe count, asset bytes —
//!   the factory-tunable gate (shot thresholds, keyframe cadence, encode
//!   profile regress HERE even when render metrics stay flat);
//! - **flicker** (glyph switches/cell/s) on static segments — the NORM cut
//!   table splits the stream so scene cuts never count as flicker;
//! - **damage rate + bytes/frame** per tier (truecolor / 256 / mono) from
//!   `FrameStats`, in pure diff mode (damage is meaningless under
//!   invalidate-every-frame);
//! - **per-stage frame times** (decode/resample/compose/present).
//!
//! Output: `--out` JSON ([`EvalReport`], deterministic layout), optional
//! `--baseline` compare (per-metric tolerances from params.toml, nonzero
//! exit on breach) and an optional `--html` contact sheet — self-contained,
//! base64-embedded PNGs, source frame vs rasterized render at
//! `eval.contact_frames` timestamps per clip (PLAN §6 "the human loop").
//!
//! Assets are cached under `--cache-dir` keyed by
//! `(input sha256, build-params sha256, pipeline source fingerprint)` —
//! eval-only knobs never invalidate the cache
//! ([`Params::build_fingerprint`]), but any code change to sleepy-factory
//! or slpy-format DOES ([`PIPELINE_FINGERPRINT`], M2 review fix: an M3
//! extract-stage change must never be measured against stale cached
//! assets built by older code).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use memmap2::Mmap;
use sleepy_player::pipeline::Player;
use slpy_eval::{
    ClipMetrics, ClipReport, CompareReport, CoverageTable, EvalReport, FlickerAccum, GrayImage,
    RasterOptions, Stage, StageAccum, aggregate_frame_stats, compare_reports, downscale_ssim,
    rasterize,
};
use slpy_format::SlpyReader;
use slpy_format::header::plane_id;
use slpy_term::{Backend, ColorTier, SimBackend};

use crate::build::{self, BuildArgs};
use crate::ffmpeg::BoxErr;
use crate::params::Params;
use crate::sha256::{sha256_file, sha256_hex};

/// Video extensions scanned in the corpus directory (non-recursive).
const VIDEO_EXTS: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "m4v"];

/// Tiers measured per clip: full metrics at truecolor, damage/bytes at the
/// degraded tiers (M2 item B contract). Tags are the `ColorTier` canonical
/// forms used across the JSON schema.
const TIERS: &[(ColorTier, &str)] =
    &[(ColorTier::True, "truecolor"), (ColorTier::C256, "256"), (ColorTier::Mono, "mono")];

/// FNV-1a 64 hash of every `.rs` source file in sleepy-factory + slpy-format
/// (computed by build.rs at compile time). Part of the asset cache key: a
/// factory/format code change invalidates cached corpus assets so eval never
/// measures stale pipeline output (M2 review fix). Over-invalidation (e.g. an
/// eval-driver-only edit) merely costs a rebuild — the safe direction.
const PIPELINE_FINGERPRINT: &str = env!("SLPY_PIPELINE_FINGERPRINT");

/// Cache file name for one (clip, params, code) combination.
fn asset_cache_name(name: &str, input_sha: &str, params_sha: &str) -> String {
    format!("{name}-{}-{}-{PIPELINE_FINGERPRINT}.slpy", &input_sha[..12], &params_sha[..12])
}

/// Eval-owned percentiles for the SSIM reference normalization. Fixed here —
/// deliberately NOT params `[levels]` and NOT the factory's NORM output: the
/// reference must stay independent of the tunables under test, or the metric
/// grades the factory against its own damage (M2 review fix — the SSIM
/// source side previously ran through `player.levels_lut()`, so a params
/// change that destroyed shot detection barely moved the score).
const SSIM_REF_LO_PCT: u64 = 2;
const SSIM_REF_HI_PCT: u64 = 98;

pub struct EvalArgs {
    pub corpus: PathBuf,
    pub params: Params,
    pub baseline: Option<PathBuf>,
    pub out: PathBuf,
    pub html: Option<PathBuf>,
    pub cache_dir: PathBuf,
}

/// One contact-sheet snapshot (truecolor tier).
struct Snap {
    frame: u32,
    secs: f64,
    ssim: f64,
    /// Viewport-cropped render raster (1×2 px/cell).
    raster: GrayImage,
    /// PNGs filled in after the render pass (ffmpeg subprocess).
    src_png: Vec<u8>,
    render_png: Vec<u8>,
}

struct ClipEval {
    report: ClipReport,
    snaps: Vec<Snap>,
}

pub fn run(args: &EvalArgs) -> Result<(), BoxErr> {
    let clips = discover_corpus(&args.corpus)?;
    std::fs::create_dir_all(&args.cache_dir)
        .map_err(|e| format!("create {}: {e}", args.cache_dir.display()))?;
    let params_sha = sha256_hex(args.params.build_fingerprint().as_bytes());

    let mut evals: Vec<ClipEval> = Vec::new();
    for (name, path) in &clips {
        eprintln!("eval: clip {name} ({})", path.display());
        evals.push(eval_clip(name, path, args, &params_sha)?);
    }

    let mut report = EvalReport::new(format!("sleepy-factory {}", env!("CARGO_PKG_VERSION")));
    report.clips = evals.iter().map(|e| e.report.clone()).collect();

    // Baseline compare (computed before writing so the HTML can show deltas;
    // the JSON + HTML are still written on a breach — the artifacts are the
    // evidence — and the breach is the exit code).
    let compare = match &args.baseline {
        None => None,
        Some(bp) => {
            let text = std::fs::read_to_string(bp)
                .map_err(|e| format!("read baseline {}: {e}", bp.display()))?;
            let baseline = EvalReport::from_json(&text)
                .map_err(|e| format!("parse baseline {}: {e}", bp.display()))?;
            Some(compare_reports(&report, &baseline, &args.params.eval.tolerances))
        }
    };

    if let Some(parent) = args.out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&args.out, report.to_json())
        .map_err(|e| format!("write {}: {e}", args.out.display()))?;
    eprintln!("eval: wrote {}", args.out.display());

    if let Some(html_path) = &args.html {
        if let Some(parent) = html_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let html = render_html(&report, &evals, compare.as_ref());
        std::fs::write(html_path, html)
            .map_err(|e| format!("write {}: {e}", html_path.display()))?;
        eprintln!("eval: wrote {}", html_path.display());
    }

    if let Some(cmp) = &compare {
        print_compare(cmp);
        if !cmp.pass {
            let failures = cmp.failures().count() + cmp.notes.len();
            return Err(format!(
                "baseline compare FAILED: {failures} finding(s) out of tolerance (see above)"
            )
            .into());
        }
    }
    Ok(())
}

/// Corpus scan: sorted by file name for deterministic clip order.
fn discover_corpus(dir: &Path) -> Result<Vec<(String, PathBuf)>, BoxErr> {
    if !dir.is_dir() {
        return Err(format!("--corpus {} is not a directory", dir.display()).into());
    }
    let mut clips = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let path = entry.map_err(|e| format!("read {}: {e}", dir.display()))?.path();
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        if path.is_file()
            && let Some(ext) = ext
            && VIDEO_EXTS.contains(&ext.as_str())
        {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            clips.push((name, path));
        }
    }
    if clips.is_empty() {
        return Err(format!(
            "no videos ({}) found in {}",
            VIDEO_EXTS.join("/"),
            dir.display()
        )
        .into());
    }
    clips.sort();
    Ok(clips)
}

/// Build-or-reuse the asset, then run the three tier passes and assemble the
/// clip report + contact snapshots.
fn eval_clip(
    name: &str,
    input: &Path,
    args: &EvalArgs,
    params_sha: &str,
) -> Result<ClipEval, BoxErr> {
    let params = &args.params;
    let input_sha = sha256_file(input).map_err(|e| format!("hash {}: {e}", input.display()))?;
    let asset_path = args.cache_dir.join(asset_cache_name(name, &input_sha, params_sha));
    if asset_path.is_file() {
        eprintln!("eval: cached asset {}", asset_path.display());
    } else {
        eprintln!("eval: building {}", asset_path.display());
        build::run(&BuildArgs {
            input: input.to_path_buf(),
            output: asset_path.clone(),
            ss: None,
            t: None,
            params: params.clone(),
        })?;
    }

    let file = std::fs::File::open(&asset_path)
        .map_err(|e| format!("open {}: {e}", asset_path.display()))?;
    // Safety: read-only private map of a file nothing mutates during eval
    // (same contract as the player's mmap).
    let mmap = unsafe { Mmap::map(&file) }
        .map_err(|e| format!("mmap {}: {e}", asset_path.display()))?;

    // Metadata pass: fps, frame budget, cut table, source luma dims.
    let reader = SlpyReader::open(&mmap)?;
    let header = reader.header();
    let fps = f64::from(header.fps_num) / f64::from(header.fps_den);
    let frames_total = reader.frame_count();
    if frames_total == 0 {
        return Err(format!("{name}: asset has zero frames").into());
    }
    let eval_frames = if params.eval.max_frames == 0 {
        frames_total
    } else {
        frames_total.min(params.eval.max_frames)
    };
    let cuts: BTreeSet<u32> =
        reader.shots().iter().filter(|s| s.is_cut()).map(|s| s.first_frame).collect();
    let shot_count = reader.shots().len() as u32;
    let (src_w, src_h) =
        reader.plane_dims(plane_id::Y).ok_or("asset has no Y plane")?;
    // Asset-structure metrics over the WHOLE asset (not just eval_frames):
    // the factory built all of it, so all of it is gated (M2 review fix).
    let mut keyframe_count = 0u32;
    for i in 0..frames_total {
        if reader.is_keyframe(i)? {
            keyframe_count += 1;
        }
    }
    drop(reader);

    let (cols, rows) = (params.eval.grid_cols, params.eval.grid_rows);
    let grid_cells = u32::from(cols) * u32::from(rows);

    // Contact snapshots: midpoints of contact_frames equal segments.
    let n_snaps = params.eval.contact_frames.min(eval_frames);
    let snap_frames: BTreeSet<u32> = (0..n_snaps)
        .map(|k| {
            ((u64::from(eval_frames) * (2 * u64::from(k) + 1)) / (2 * u64::from(n_snaps.max(1))))
                as u32
        })
        .collect();

    let mut metrics = ClipMetrics {
        shot_count: Some(shot_count),
        cut_count: Some(cuts.len() as u32),
        keyframe_count: Some(keyframe_count),
        asset_bytes: Some(mmap.len() as u64),
        ..ClipMetrics::default()
    };
    let mut snaps: Vec<Snap> = Vec::new();

    for &(tier, tag) in TIERS {
        let truecolor = tier == ColorTier::True;
        let reader = SlpyReader::open(&mmap)?;
        let mut backend = SimBackend::new(cols, rows);
        let mut caps = backend.caps().clone();
        caps.color = tier;
        backend.set_caps(caps);
        // Pure diff mode (repaint_full = false): damage rate is the metric
        // here, and invalidate-every-frame would pin it at 100%.
        let mut player =
            Player::new(reader, slpy_core::DEFAULT_CELL_ASPECT, false, tier != ColorTier::Mono)?;
        player.reflow(&mut backend, cols, rows);

        let table = CoverageTable::conservative();
        let ropts = RasterOptions::default();
        let mut frame_stats = Vec::with_capacity(eval_frames as usize);
        let mut stage_acc = StageAccum::new();
        let mut prev_stage = player.stage();
        // Flicker with cut segmentation: a fresh accumulator per shot means
        // the cut transition contributes no pairs (§6 "static segments").
        let mut flicker = FlickerAccum::new();
        let (mut fl_switches, mut fl_pairs) = (0u64, 0u64);
        let mut ssim_sum = 0.0f64;
        let mut ssim_n = 0u32;
        let mut normed: Vec<u8> = Vec::new();

        for i in 0..eval_frames {
            let fs = player.render_present(&mut backend, i)?;
            backend.take_output(); // count bytes via stats; don't hoard RAM
            frame_stats.push(fs);
            if !truecolor {
                continue;
            }

            let stage = player.stage();
            stage_acc.record(Stage::Decode, Duration::from_nanos(stage.decode - prev_stage.decode));
            stage_acc.record(
                Stage::Resample,
                Duration::from_nanos(stage.resample - prev_stage.resample),
            );
            stage_acc.record(Stage::Compose, Duration::from_nanos(stage.compose - prev_stage.compose));
            stage_acc.record(Stage::Present, Duration::from_nanos(stage.present - prev_stage.present));
            prev_stage = stage;

            if i > 0 && cuts.contains(&i) {
                fl_switches += flicker.switches();
                fl_pairs += flicker.cell_pairs();
                flicker = FlickerAccum::new();
            }
            flicker.push(player.grid());

            let want_sample = i.is_multiple_of(params.eval.ssim_every);
            let want_snap = snap_frames.contains(&i);
            if want_sample || want_snap {
                let (value, raster) = frame_ssim(&player, table, &ropts, src_w, src_h, &mut normed)?;
                if want_sample {
                    ssim_sum += value;
                    ssim_n += 1;
                }
                if want_snap {
                    snaps.push(Snap {
                        frame: i,
                        secs: f64::from(i) / fps,
                        ssim: value,
                        raster,
                        src_png: Vec::new(),
                        render_png: Vec::new(),
                    });
                }
            }
        }

        metrics
            .damage_by_tier
            .insert(tag.to_string(), aggregate_frame_stats(&frame_stats, grid_cells, fps));
        if truecolor {
            fl_switches += flicker.switches();
            fl_pairs += flicker.cell_pairs();
            metrics.ssim = (ssim_n > 0).then(|| ssim_sum / f64::from(ssim_n));
            metrics.flicker_switches_per_cell_sec =
                (fl_pairs > 0).then(|| fl_switches as f64 / fl_pairs as f64 * fps);
            metrics.stage_ms = Some(stage_acc.report());
        }
    }

    // Contact-sheet PNGs (ffmpeg subprocess — outside the timed passes).
    for snap in &mut snaps {
        snap.render_png = png_from_gray(&snap.raster)?;
        snap.src_png =
            png_source_frame(input, src_w, src_h, params.build.fps, snap.frame)?;
    }

    let d = &metrics.damage_by_tier["truecolor"];
    eprintln!(
        "eval: {name}: {eval_frames}/{frames_total} frames @ {fps} fps, grid {cols}x{rows} | \
         ssim {} | flicker {} | {shot_count} shot(s)/{} cut(s), {keyframe_count} keyframes | \
         truecolor {:.0} B/frame ({:.1}% damage)",
        metrics.ssim.map_or("n/a".into(), |v| format!("{v:.4}")),
        metrics
            .flicker_switches_per_cell_sec
            .map_or("n/a".into(), |v| format!("{v:.3}/cell/s")),
        cuts.len(),
        d.avg_bytes_per_frame,
        d.avg_damage_rate * 100.0,
    );

    Ok(ClipEval {
        report: ClipReport {
            name: name.to_string(),
            frames: eval_frames,
            fps,
            grid_cols: cols,
            grid_rows: rows,
            metrics,
        },
        snaps,
    })
}

/// Downscale-SSIM for the player's current frame (PLAN §6): rasterize the
/// grid through the coverage table, crop the viewport (pads are not scored),
/// and compare against the source luma normalized by EVAL-OWNED per-frame
/// p2/p98 percentiles ([`SSIM_REF_LO_PCT`]/[`SSIM_REF_HI_PCT`]).
///
/// The reference is deliberately independent of the factory's NORM levels
/// (M2 review fix — no self-grading): a healthy build's per-shot levels sit
/// close to the per-frame percentiles, so SSIM stays high; a params change
/// that kills shot detection or degrades `[levels]` leaves the render
/// mis-stretched against a still-correct reference and the score drops.
/// The stretch itself is still not scored — both sides are normalized, just
/// not by the same knob under test.
fn frame_ssim(
    player: &Player<'_>,
    table: &CoverageTable,
    ropts: &RasterOptions,
    src_w: u16,
    src_h: u16,
    normed: &mut Vec<u8>,
) -> Result<(f64, GrayImage), BoxErr> {
    let vp = player
        .viewport()
        .ok_or("eval grid is below the 32x9 viewport minimum")?;
    let raster = rasterize(player.grid(), table, ropts);
    let cropped = raster.crop(
        vp.pad_left * ropts.cell_w_px,
        vp.pad_top * ropts.cell_h_px,
        vp.cols * ropts.cell_w_px,
        vp.rows * ropts.cell_h_px,
    );
    let mut lut = [0u8; 256];
    sleepy_player::pipeline::build_levels_lut(&mut lut, reference_levels(player.luma_src()));
    normed.clear();
    normed.extend(player.luma_src().iter().map(|&v| lut[v as usize]));
    let value = downscale_ssim(&cropped, normed, src_w, src_h);
    Ok((value, cropped))
}

/// Per-frame p2/p98 of the raw source luma (nearest-rank on a 256-bin
/// histogram) — the SSIM reference normalization. `None` on an empty plane;
/// a flat frame yields a degenerate span, which `build_levels_lut` treats as
/// identity (matching the player's own convention).
fn reference_levels(luma: &[u8]) -> Option<slpy_format::PlaneLevels> {
    if luma.is_empty() {
        return None;
    }
    let mut hist = [0u64; 256];
    for &v in luma {
        hist[v as usize] += 1;
    }
    let total = luma.len() as u64;
    let rank = |pct: u64| -> u8 {
        let target = (total * pct).div_ceil(100).max(1);
        let mut cum = 0u64;
        for (v, &n) in hist.iter().enumerate() {
            cum += n;
            if cum >= target {
                return v as u8;
            }
        }
        255
    };
    Some(slpy_format::PlaneLevels { p2: rank(SSIM_REF_LO_PCT), p98: rank(SSIM_REF_HI_PCT) })
}

// ---------------------------------------------------------------------------
// PNG plumbing — ffmpeg subprocess (PLAN §8: ffmpeg strictly as CLI; the
// factory already owns that dependency, so no image crate is needed).
// ---------------------------------------------------------------------------

/// Run ffmpeg with `-v error`, feeding `stdin_data` when given, capturing
/// stdout. stderr is drained on a thread (same deadlock rule as ffmpeg.rs).
fn ffmpeg_capture(extra_args: &[&str], stdin_data: Option<&[u8]>) -> Result<Vec<u8>, BoxErr> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error"]);
    cmd.args(extra_args);
    cmd.stdin(if stdin_data.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to run ffmpeg (is it installed and on PATH?): {e}"))?;

    let stdin_thread = stdin_data.map(|data| {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
            // dropped: closes the pipe so ffmpeg sees EOF
        })
    });
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let mut out = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout was piped")
        .read_to_end(&mut out)?;
    if let Some(t) = stdin_thread {
        let _ = t.join();
    }
    let err = stderr_thread.join().unwrap_or_default();
    let status = child.wait()?;
    if !status.success() {
        return Err(format!(
            "ffmpeg exited with {status}: {}",
            String::from_utf8_lossy(&err).trim()
        )
        .into());
    }
    if out.is_empty() {
        return Err("ffmpeg produced no output bytes".into());
    }
    Ok(out)
}

/// Grayscale raster → PNG bytes.
fn png_from_gray(img: &GrayImage) -> Result<Vec<u8>, BoxErr> {
    let size = format!("{}x{}", img.w(), img.h());
    ffmpeg_capture(
        &[
            "-f", "rawvideo", "-pixel_format", "gray", "-video_size", &size, "-i", "-",
            "-frames:v", "1", "-f", "image2pipe", "-c:v", "png", "-",
        ],
        Some(img.as_slice()),
    )
}

/// Source frame `frame_idx` of the fps-normalized, base-res-scaled stream —
/// the exact `scale=W:H:flags=area,fps=N` chain the factory ingested
/// (ffmpeg.rs), so the contact sheet compares what the asset actually saw.
fn png_source_frame(
    input: &Path,
    w: u16,
    h: u16,
    fps: u16,
    frame_idx: u32,
) -> Result<Vec<u8>, BoxErr> {
    let vf = format!("scale={w}:{h}:flags=area,fps={fps},select=eq(n\\,{frame_idx})");
    let input = input.to_string_lossy();
    ffmpeg_capture(
        &[
            "-nostdin", "-i", &input, "-map", "0:v:0", "-vf", &vf,
            "-frames:v", "1", "-f", "image2pipe", "-c:v", "png", "-",
        ],
        None,
    )
}

// ---------------------------------------------------------------------------
// Baseline compare output
// ---------------------------------------------------------------------------

fn print_compare(cmp: &CompareReport) {
    for note in &cmp.notes {
        eprintln!("baseline: NOTE  {note}");
    }
    for d in &cmp.deltas {
        eprintln!(
            "baseline: {} {}/{}: {:.6} -> {:.6} ({:+.6})",
            if d.pass { "ok  " } else { "FAIL" },
            d.clip,
            d.metric,
            d.baseline,
            d.current,
            d.delta
        );
    }
    eprintln!(
        "baseline: {} ({} metrics compared, {} failed, {} notes)",
        if cmp.pass { "PASS" } else { "FAIL" },
        cmp.deltas.len(),
        cmp.failures().count(),
        cmp.notes.len()
    );
}

// ---------------------------------------------------------------------------
// HTML contact sheet (PLAN §6 "the human loop") — fully self-contained:
// inline CSS, base64 data: URIs, no external requests.
// ---------------------------------------------------------------------------

/// Standard base64 (RFC 4648, with padding) — 20 lines beat a dependency.
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn fmt_opt(v: Option<f64>, digits: usize) -> String {
    v.map_or("n/a".into(), |v| format!("{v:.digits$}"))
}

fn render_html(report: &EvalReport, evals: &[ClipEval], compare: Option<&CompareReport>) -> String {
    let mut h = String::with_capacity(1 << 20);
    h.push_str(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>sleepytime eval</title>\n<style>\n\
         body{font-family:system-ui,sans-serif;margin:2rem auto;max-width:1100px;\
         background:#14151a;color:#d8dae2;line-height:1.45}\n\
         h1{font-size:1.5rem}h2{font-size:1.2rem;border-bottom:1px solid #33363f;\
         padding-bottom:.3rem;margin-top:2.5rem}\n\
         table{border-collapse:collapse;margin:.6rem 0;font-size:.85rem;\
         font-variant-numeric:tabular-nums}\n\
         th,td{border:1px solid #33363f;padding:.25rem .6rem;text-align:right}\n\
         th{background:#1d1f26;text-align:center}td:first-child{text-align:left}\n\
         .pass{color:#7fd48a}.fail{color:#ff6b6b;font-weight:700}\n\
         .banner{padding:.5rem .8rem;border-radius:6px;margin:1rem 0;font-weight:700}\n\
         .banner.pass{background:#15321b}.banner.fail{background:#3a1518}\n\
         .snaps{display:flex;flex-wrap:wrap;gap:1rem;margin:.8rem 0}\n\
         figure{margin:0;background:#1d1f26;padding:.5rem;border-radius:6px}\n\
         figcaption{font-size:.75rem;color:#9aa0ae;margin-bottom:.4rem}\n\
         img{display:block;width:480px;max-width:100%;image-rendering:pixelated}\n\
         .meta{color:#9aa0ae;font-size:.85rem}\n\
         </style>\n</head>\n<body>\n",
    );
    h.push_str("<h1>sleepytime eval — contact sheet</h1>\n");
    h.push_str(&format!(
        "<p class=\"meta\">{} | schema v{} | {} clip(s)</p>\n",
        html_escape(&report.generator),
        report.schema_version,
        report.clips.len()
    ));

    if let Some(cmp) = compare {
        let (class, label) = if cmp.pass { ("pass", "PASS") } else { ("fail", "FAIL") };
        h.push_str(&format!(
            "<div class=\"banner {class}\">baseline compare: {label} \
             ({} metrics, {} failed, {} notes)</div>\n",
            cmp.deltas.len(),
            cmp.failures().count(),
            cmp.notes.len()
        ));
        for note in &cmp.notes {
            h.push_str(&format!("<p class=\"fail\">note: {}</p>\n", html_escape(note)));
        }
    }

    for eval in evals {
        let c = &eval.report;
        let m = &c.metrics;
        h.push_str(&format!("<h2>{}</h2>\n", html_escape(&c.name)));
        h.push_str(&format!(
            "<p class=\"meta\">{} frames @ {} fps on a {}x{} grid</p>\n",
            c.frames, c.fps, c.grid_cols, c.grid_rows
        ));

        // Headline metrics.
        h.push_str("<table><tr><th>metric</th><th>value</th></tr>");
        h.push_str(&format!(
            "<tr><td>downscale-SSIM (truecolor)</td><td>{}</td></tr>",
            fmt_opt(m.ssim, 4)
        ));
        h.push_str(&format!(
            "<tr><td>flicker (glyph switches/cell/s)</td><td>{}</td></tr>",
            fmt_opt(m.flicker_switches_per_cell_sec, 3)
        ));
        let fmt_count = |v: Option<u32>| v.map_or("n/a".into(), |v| v.to_string());
        h.push_str(&format!(
            "<tr><td>shots / cuts</td><td>{} / {}</td></tr>",
            fmt_count(m.shot_count),
            fmt_count(m.cut_count)
        ));
        h.push_str(&format!(
            "<tr><td>keyframes</td><td>{}</td></tr>",
            fmt_count(m.keyframe_count)
        ));
        h.push_str(&format!(
            "<tr><td>asset size</td><td>{}</td></tr>",
            m.asset_bytes
                .map_or("n/a".into(), |b| format!("{:.2} MiB", b as f64 / (1024.0 * 1024.0)))
        ));
        h.push_str("</table>\n");

        // Per-tier damage table.
        h.push_str(
            "<table><tr><th>tier</th><th>avg B/frame</th><th>max B/frame</th>\
             <th>avg damage</th><th>max damage</th><th>B/s @ fps</th></tr>",
        );
        for (tier, d) in &m.damage_by_tier {
            h.push_str(&format!(
                "<tr><td>{}</td><td>{:.0}</td><td>{}</td><td>{:.2}%</td><td>{:.2}%</td>\
                 <td>{:.0}</td></tr>",
                html_escape(tier),
                d.avg_bytes_per_frame,
                d.max_bytes_per_frame,
                d.avg_damage_rate * 100.0,
                d.max_damage_rate * 100.0,
                d.bytes_per_sec
            ));
        }
        h.push_str("</table>\n");

        // Stage times.
        if let Some(s) = &m.stage_ms {
            h.push_str(
                "<table><tr><th>stage</th><th>mean ms</th><th>max ms</th></tr>",
            );
            for stage in Stage::ALL {
                let st = s.stage(stage);
                h.push_str(&format!(
                    "<tr><td>{}</td><td>{:.3}</td><td>{:.3}</td></tr>",
                    stage.as_str(),
                    st.mean_ms,
                    st.max_ms
                ));
            }
            h.push_str("</table>\n");
        }

        // Deltas vs baseline for this clip.
        if let Some(cmp) = compare {
            let rows: Vec<_> = cmp.deltas.iter().filter(|d| d.clip == c.name).collect();
            if !rows.is_empty() {
                h.push_str(
                    "<table><tr><th>vs baseline</th><th>baseline</th><th>current</th>\
                     <th>delta</th><th>ok?</th></tr>",
                );
                for d in rows {
                    let (class, label) = if d.pass { ("pass", "ok") } else { ("fail", "FAIL") };
                    h.push_str(&format!(
                        "<tr><td>{}</td><td>{:.6}</td><td>{:.6}</td><td>{:+.6}</td>\
                         <td class=\"{class}\">{label}</td></tr>",
                        html_escape(&d.metric),
                        d.baseline,
                        d.current,
                        d.delta
                    ));
                }
                h.push_str("</table>\n");
            }
        }

        // Side-by-side snapshots.
        h.push_str("<div class=\"snaps\">\n");
        for snap in &eval.snaps {
            h.push_str(&format!(
                "<figure><figcaption>frame {} (t={:.2}s) — source (fps-normalized, base res)\
                 </figcaption><img alt=\"source frame {}\" \
                 src=\"data:image/png;base64,{}\"></figure>\n",
                snap.frame,
                snap.secs,
                snap.frame,
                base64(&snap.src_png)
            ));
            h.push_str(&format!(
                "<figure><figcaption>frame {} — rendered grid raster ({}x{} px, \
                 ssim {:.4})</figcaption><img alt=\"render of frame {}\" \
                 src=\"data:image/png;base64,{}\"></figure>\n",
                snap.frame,
                snap.raster.w(),
                snap.raster.h(),
                snap.ssim,
                snap.frame,
                base64(&snap.render_png)
            ));
        }
        h.push_str("</div>\n");
    }
    h.push_str("</body>\n</html>\n");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn html_escaping() {
        assert_eq!(html_escape("a<b>&c"), "a&lt;b&gt;&amp;c");
    }

    /// M2 review fix: the cache key carries all three identity components —
    /// input bytes, build params AND pipeline code — so a factory/format
    /// source change can never reuse an asset built by older code.
    #[test]
    fn cache_name_includes_pipeline_fingerprint() {
        assert_eq!(PIPELINE_FINGERPRINT.len(), 16, "FNV-1a 64 as hex");
        assert!(PIPELINE_FINGERPRINT.chars().all(|c| c.is_ascii_hexdigit()));
        let input_sha = "aaaaaaaaaaaabbbbbbbbbbbb";
        let params_sha = "ccccccccccccdddddddddddd";
        let name = asset_cache_name("clip", input_sha, params_sha);
        assert_eq!(
            name,
            format!("clip-aaaaaaaaaaaa-cccccccccccc-{PIPELINE_FINGERPRINT}.slpy")
        );
        // Each component changes the key.
        assert_ne!(asset_cache_name("clip", "e00000000000ffff", params_sha), name);
        assert_ne!(asset_cache_name("clip", input_sha, "e00000000000ffff"), name);
    }

    /// The SSIM reference percentiles are computed from the raw source luma
    /// (nearest-rank), independent of factory NORM output.
    #[test]
    fn reference_levels_nearest_rank() {
        assert_eq!(reference_levels(&[]), None);
        // Flat plane → degenerate span (identity downstream).
        let flat = reference_levels(&vec![128u8; 1000]).unwrap();
        assert_eq!((flat.p2, flat.p98), (128, 128));
        // Uniform 0..=255 spread: p2 ≈ 2%, p98 ≈ 98% of the range.
        let spread: Vec<u8> = (0..=255u16).flat_map(|v| [v as u8; 100]).collect();
        let lv = reference_levels(&spread).unwrap();
        assert!((4..=6).contains(&lv.p2), "p2 = {}", lv.p2);
        assert!((249..=251).contains(&lv.p98), "p98 = {}", lv.p98);
        // Outlier-heavy plane: the bright 2% tail is clipped (p98 = 100, not
        // 255); the dark tail sits exactly on the rank-20 boundary so
        // nearest-rank lands on it (p2 = 0).
        let mut plane = vec![0u8; 20]; // 2% dark outliers
        plane.extend(std::iter::repeat_n(100u8, 960));
        plane.extend(std::iter::repeat_n(255u8, 20)); // 2% bright outliers
        let lv = reference_levels(&plane).unwrap();
        assert_eq!((lv.p2, lv.p98), (0, 100));
    }
}
