//! `auto-ascii-factory eval` — the agent socket.
//!
//! For every video in `--corpus`: build (or reuse) the asset, then drive the
//! REAL player pipeline (`auto_ascii::pipeline::Player`, a library type so
//! this driver measures the renderer and not a reimplementation) headlessly
//! against `SimBackend`, collecting these metrics:
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
//! `eval.contact_frames` timestamps per clip, for human review.
//!
//! Assets are cached under `--cache-dir` keyed by
//! `(input sha256, build-params sha256, pipeline source fingerprint)` —
//! eval-only knobs never invalidate the cache
//! ([`Params::build_fingerprint`]), but any code change to auto-ascii-factory
//! or auto-ascii-format DOES ([`PIPELINE_FINGERPRINT`]), so an extract-stage
//! change is never measured against stale cached assets built by older
//! code.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use std::collections::BTreeMap;

use memmap2::Mmap;
use auto_ascii::pipeline::Player;
use auto_ascii_core::{DEFAULT_CELL_ASPECT, compute_viewport};
use auto_ascii_eval::{
    ClipMetrics, ClipReport, CompareReport, CoverageTable, EDGE_MATCH_TOLERANCE, EdgeMask,
    EdgeScore, EvalReport, FlickerAccum, GrayImage, RasterOptions, Stage, StageAccum,
    aggregate_frame_stats, canny_edge_truth, compare_reports, downscale_ssim, edge_cells_from_layers,
    edge_f1, rasterize,
};
use auto_ascii_format::AsciiReader;
use auto_ascii_format::header::plane_id;
use auto_ascii_term::{Backend, ColorTier, SimBackend};

use crate::build::{self, BuildArgs};
use crate::ffmpeg::BoxErr;
use crate::params::Params;
use crate::reel::{GIF_FPS, GIF_SECS, REEL_ROWS, ReelClip, ReelRow, encode_gray_gif, render_reel_html};
use crate::sha256::{sha256_file, sha256_hex};

const VIDEO_EXTS: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "m4v"];

const TIERS: &[(ColorTier, &str)] =
    &[(ColorTier::True, "truecolor"), (ColorTier::C256, "256"), (ColorTier::Mono, "mono")];

const PIPELINE_FINGERPRINT: &str = env!("ASCII_PIPELINE_FINGERPRINT");

fn asset_cache_name(name: &str, input_sha: &str, params_sha: &str) -> String {
    format!("{name}-{}-{}-{PIPELINE_FINGERPRINT}.ascii", &input_sha[..12], &params_sha[..12])
}

const SSIM_REF_LO_PCT: u64 = 2;
const SSIM_REF_HI_PCT: u64 = 98;

pub struct EvalArgs {
    pub corpus: PathBuf,
    pub params: Params,
    pub baseline: Option<PathBuf>,
    pub out: PathBuf,
    pub html: Option<PathBuf>,
    /// Review-reel HTML path (human sign-off artifact — see `reel.rs`).
    pub reel: Option<PathBuf>,
    pub cache_dir: PathBuf,
    /// Sweep mode: only the truecolor pass runs (it carries every
    /// gated render metric — ssim/flicker/edge-F1/stage times); the 256/mono
    /// damage passes are skipped. `auto-ascii-factory eval` always sets false —
    /// baseline reports keep full tier coverage.
    pub truecolor_only: bool,
    /// `--font-table NAME|PATH`: ink-coverage table for the SSIM rasterizer.
    /// `None` = the conservative default (the table baselines are scored
    /// with unless told otherwise).
    pub font_table: Option<String>,
}

pub(crate) fn resolve_font_table(spec: Option<&str>) -> Result<CoverageTable, BoxErr> {
    let Some(spec) = spec else {
        return Ok(CoverageTable::conservative().clone());
    };
    if spec == "conservative" {
        return Ok(CoverageTable::conservative().clone());
    }
    if let Some(t) = auto_ascii_core::FontTable::builtin(spec) {
        return Ok(CoverageTable::from_font_table(t));
    }
    let path = Path::new(spec);
    if path.is_file() {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read {spec}: {e}"))?;
        let t = auto_ascii_core::FontTable::parse(&text)
            .map_err(|e| format!("font table {spec}: {e}"))?;
        return Ok(CoverageTable::from_font_table(&t));
    }
    Err(format!(
        "--font-table {spec:?}: not a built-in table ({}) and no such file",
        auto_ascii_core::BUILTIN_FONT_TABLES.join(", ")
    )
    .into())
}

/// Edge-truth memo across sweep combos: the source Canny masks
/// depend only on (clip, ingest fps, grid, sampled frame set) — never on the
/// tunables under test — so a sweep computes them once per clip, not once
/// per combo. Keyed by every input that shapes the mask set.
pub type TruthCache = BTreeMap<String, BTreeMap<u32, EdgeMask>>;

struct Snap {
    frame: u32,
    secs: f64,
    ssim: f64,
    raster: GrayImage,
    src_png: Vec<u8>,
    render_png: Vec<u8>,
}

pub(crate) struct ClipEval {
    pub(crate) report: ClipReport,
    snaps: Vec<Snap>,
    reel: Option<ReelClip>,
}

pub fn run(args: &EvalArgs) -> Result<(), BoxErr> {
    let clips = discover_corpus(&args.corpus)?;
    std::fs::create_dir_all(&args.cache_dir)
        .map_err(|e| format!("create {}: {e}", args.cache_dir.display()))?;
    let params_sha = sha256_hex(args.params.build_fingerprint().as_bytes());

    let mut evals: Vec<ClipEval> = Vec::new();
    for (name, path) in &clips {
        eprintln!("eval: clip {name} ({})", path.display());
        evals.push(eval_clip(name, path, args, &params_sha, None)?);
    }

    let mut report = EvalReport::new(format!("auto-ascii-factory {}", env!("CARGO_PKG_VERSION")));
    report.clips = evals.iter().map(|e| e.report.clone()).collect();

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

    if let Some(reel_path) = &args.reel {
        if let Some(parent) = reel_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let reel_clips: Vec<ReelClip> = evals.iter_mut().filter_map(|e| e.reel.take()).collect();
        let html = render_reel_html(
            &reel_clips,
            &format!("auto-ascii-factory {}", env!("CARGO_PKG_VERSION")),
        );
        std::fs::write(reel_path, html)
            .map_err(|e| format!("write {}: {e}", reel_path.display()))?;
        eprintln!("eval: wrote review reel {}", reel_path.display());
    }

    if let Some(cmp) = &compare {
        print_compare(cmp);
        if !cmp.pass {
            let failures = cmp.failures().count()
                + cmp.notes.iter().filter(|n| !n.starts_with("info:")).count();
            return Err(format!(
                "baseline compare FAILED: {failures} finding(s) out of tolerance (see above)"
            )
            .into());
        }
    }
    Ok(())
}

pub(crate) fn discover_corpus(dir: &Path) -> Result<Vec<(String, PathBuf)>, BoxErr> {
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

pub(crate) fn eval_clip(
    name: &str,
    input: &Path,
    args: &EvalArgs,
    params_sha: &str,
    truth_cache: Option<&mut TruthCache>,
) -> Result<ClipEval, BoxErr> {
    let params = &args.params;
    let input_sha = sha256_file(input).map_err(|e| format!("hash {}: {e}", input.display()))?;
    let asset_path = args.cache_dir.join(asset_cache_name(name, &input_sha, params_sha));
    if asset_path.is_file() {
        eprintln!("eval: cached asset {}", asset_path.display());
    } else {
        eprintln!("eval: building {}", asset_path.display());
        build::run(
            &BuildArgs {
                input: input.to_path_buf(),
                output: asset_path.clone(),
                ss: None,
                t: None,
                params: params.clone(),
            },
            &mut std::io::stderr(),
        )?;
    }

    let file = std::fs::File::open(&asset_path)
        .map_err(|e| format!("open {}: {e}", asset_path.display()))?;
    // Safety: read-only private map of a file nothing mutates during eval
    // (same contract as the player's mmap).
    let mmap = unsafe { Mmap::map(&file) }
        .map_err(|e| format!("mmap {}: {e}", asset_path.display()))?;

    let reader = AsciiReader::open(&mmap)?;
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
    let mut keyframe_count = 0u32;
    for i in 0..frames_total {
        if reader.is_keyframe(i)? {
            keyframe_count += 1;
        }
    }
    drop(reader);

    let (cols, rows_dim) = (params.eval.grid_cols, params.eval.grid_rows);
    let grid_cells = u32::from(cols) * u32::from(rows_dim);
    let vp = compute_viewport(cols, rows_dim, DEFAULT_CELL_ASPECT);

    let midpoints = |n: u32| -> BTreeSet<u32> {
        (0..n.min(eval_frames))
            .map(|k| {
                ((u64::from(eval_frames) * (2 * u64::from(k) + 1))
                    / (2 * u64::from(n.min(eval_frames).max(1)))) as u32
            })
            .collect()
    };
    let snap_frames = midpoints(params.eval.contact_frames);
    let reel_on = args.reel.is_some();
    let reel_frames: BTreeSet<u32> = if reel_on { midpoints(REEL_ROWS) } else { BTreeSet::new() };
    let gif_stride = ((fps / f64::from(GIF_FPS)).round() as u32).max(1);
    let gif_max = (GIF_SECS * GIF_FPS) as usize;

    let mut truth_owned: Option<BTreeMap<u32, EdgeMask>> = None;
    let truth: &BTreeMap<u32, EdgeMask> = match vp {
        None => truth_owned.insert(BTreeMap::new()),
        Some(vp) => {
            let mut wanted: BTreeSet<u32> = (0..eval_frames)
                .filter(|i| i.is_multiple_of(params.eval.ssim_every))
                .collect();
            wanted.extend(reel_frames.iter().copied());
            match truth_cache {
                Some(cache) => {
                    let key = format!(
                        "{name}:{}x{}:{}fps:{}f:every{}:reel{}",
                        vp.cols, vp.rows, params.build.fps, eval_frames,
                        params.eval.ssim_every, reel_on
                    );
                    if !cache.contains_key(&key) {
                        let masks = source_edge_truth(
                            input, src_w, src_h, params.build.fps, &wanted, vp.cols, vp.rows,
                        )?;
                        cache.insert(key.clone(), masks);
                    }
                    &cache[&key]
                }
                None => truth_owned.insert(source_edge_truth(
                    input, src_w, src_h, params.build.fps, &wanted, vp.cols, vp.rows,
                )?),
            }
        }
    };

    let mut metrics = ClipMetrics {
        shot_count: Some(shot_count),
        cut_count: Some(cuts.len() as u32),
        keyframe_count: Some(keyframe_count),
        asset_bytes: Some(mmap.len() as u64),
        ..ClipMetrics::default()
    };
    let mut snaps: Vec<Snap> = Vec::new();
    struct ReelPending {
        frame: u32,
        secs: f64,
        ssim: Option<f64>,
        edge: Option<EdgeScore>,
        flicker_to_date: Option<f64>,
        raster: GrayImage,
    }
    let mut reel_pending: Vec<ReelPending> = Vec::new();
    let mut gif_rasters: Vec<GrayImage> = Vec::new();

    let coverage_table = resolve_font_table(args.font_table.as_deref())?;

    let tiers: &[(ColorTier, &str)] = if args.truecolor_only { &TIERS[..1] } else { TIERS };
    for &(tier, tag) in tiers {
        let truecolor = tier == ColorTier::True;
        let reader = AsciiReader::open(&mmap)?;
        let mut backend = SimBackend::new(cols, rows_dim);
        let mut caps = backend.caps().clone();
        caps.color = tier;
        backend.set_caps(caps);
        let mut player = Player::new(
            reader,
            DEFAULT_CELL_ASPECT,
            false,
            auto_ascii::pipeline::color_depth(tier),
            auto_ascii_core::GlyphTier::Ascii,
        )?;
        player.set_compose_params(params.compose.to_core());
        if truecolor {
            player.enable_layer_mask();
        }
        player.reflow(&mut backend, cols, rows_dim);

        let table = &coverage_table;
        let ropts = RasterOptions::default();
        let mut frame_stats = Vec::with_capacity(eval_frames as usize);
        let mut stage_acc = StageAccum::new();
        let mut prev_stage = player.stage();
        let mut flicker = FlickerAccum::new();
        let (mut fl_switches, mut fl_pairs) = (0u64, 0u64);
        let mut ssim_sum = 0.0f64;
        let mut ssim_n = 0u32;
        let mut normed: Vec<u8> = Vec::new();
        let (mut ef1_sum, mut ep_sum, mut er_sum) = (0.0f64, 0.0f64, 0.0f64);
        let mut edge_n = 0u32;

        for i in 0..eval_frames {
            let fs = player.render_present(&mut backend, i)?;
            backend.take_output();
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
            let want_reel = reel_on && reel_frames.contains(&i);
            let want_gif =
                reel_on && gif_rasters.len() < gif_max && i.is_multiple_of(gif_stride);

            let mut edge_score: Option<EdgeScore> = None;
            if (want_sample || want_reel)
                && let (Some(vp), Some(t)) = (vp.as_ref(), truth.get(&i))
                && let Some(layers) = player.layer_mask()
            {
                let pred = edge_cells_from_layers(layers, vp);
                let score = edge_f1(t, &pred, EDGE_MATCH_TOLERANCE);
                if want_sample {
                    ef1_sum += score.f1;
                    ep_sum += score.precision;
                    er_sum += score.recall;
                    edge_n += 1;
                }
                edge_score = Some(score);
            }

            if want_sample || want_snap || want_reel || want_gif {
                let raster = render_raster(&player, table, &ropts)?;
                let value = if want_sample || want_snap || want_reel {
                    frame_ssim(&player, &raster, src_w, src_h, &mut normed)
                } else {
                    0.0
                };
                if want_sample {
                    ssim_sum += value;
                    ssim_n += 1;
                }
                if want_snap {
                    snaps.push(Snap {
                        frame: i,
                        secs: f64::from(i) / fps,
                        ssim: value,
                        raster: raster.clone(),
                        src_png: Vec::new(),
                        render_png: Vec::new(),
                    });
                }
                if want_gif {
                    gif_rasters.push(raster.clone());
                }
                if want_reel {
                    let sw = fl_switches + flicker.switches();
                    let pairs = fl_pairs + flicker.cell_pairs();
                    reel_pending.push(ReelPending {
                        frame: i,
                        secs: f64::from(i) / fps,
                        ssim: Some(value),
                        edge: edge_score,
                        flicker_to_date: (pairs > 0)
                            .then(|| sw as f64 / pairs as f64 * fps),
                        raster,
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
            metrics.edge_f1 = (edge_n > 0).then(|| ef1_sum / f64::from(edge_n));
            metrics.edge_precision = (edge_n > 0).then(|| ep_sum / f64::from(edge_n));
            metrics.edge_recall = (edge_n > 0).then(|| er_sum / f64::from(edge_n));
            metrics.stage_ms = Some(stage_acc.report());
        }
    }

    for snap in &mut snaps {
        snap.render_png = png_from_gray(&snap.raster)?;
        snap.src_png =
            png_source_frame(input, src_w, src_h, params.build.fps, snap.frame)?;
    }

    let reel_clip = if reel_on {
        let mut rows = Vec::with_capacity(reel_pending.len());
        for p in reel_pending {
            rows.push(ReelRow {
                frame: p.frame,
                secs: p.secs,
                ssim: p.ssim,
                edge: p.edge,
                flicker_to_date: p.flicker_to_date,
                src_png: png_source_frame(input, src_w, src_h, params.build.fps, p.frame)?,
                render_png: png_from_gray(&p.raster)?,
            });
        }
        let (gif, gif_w, gif_h) = if gif_rasters.is_empty() {
            (Vec::new(), 0, 0)
        } else {
            let (w, h) = (gif_rasters[0].w(), gif_rasters[0].h());
            (encode_gray_gif(&gif_rasters, GIF_FPS)?, w, h)
        };
        Some(ReelClip {
            name: name.to_string(),
            fps,
            grid_cols: cols,
            grid_rows: rows_dim,
            frames: eval_frames,
            ssim_mean: metrics.ssim,
            edge_f1_mean: metrics.edge_f1,
            flicker: metrics.flicker_switches_per_cell_sec,
            gif,
            gif_w,
            gif_h,
            rows,
        })
    } else {
        None
    };

    let d = &metrics.damage_by_tier["truecolor"];
    eprintln!(
        "eval: {name}: {eval_frames}/{frames_total} frames @ {fps} fps, grid {cols}x{rows_dim} | \
         ssim {} | edge F1 {} | flicker {} | {shot_count} shot(s)/{} cut(s), \
         {keyframe_count} keyframes | truecolor {:.0} B/frame ({:.1}% damage)",
        metrics.ssim.map_or("n/a".into(), |v| format!("{v:.4}")),
        metrics.edge_f1.map_or("n/a".into(), |v| format!("{v:.4}")),
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
            grid_rows: rows_dim,
            metrics,
        },
        snaps,
        reel: reel_clip,
    })
}

fn render_raster(
    player: &Player<'_>,
    table: &CoverageTable,
    ropts: &RasterOptions,
) -> Result<GrayImage, BoxErr> {
    let vp = player
        .viewport()
        .ok_or("eval grid is below the 32x9 viewport minimum")?;
    let raster = rasterize(player.grid(), table, ropts);
    Ok(raster.crop(
        vp.pad_left * ropts.cell_w_px,
        vp.pad_top * ropts.cell_h_px,
        vp.cols * ropts.cell_w_px,
        vp.rows * ropts.cell_h_px,
    ))
}

fn source_edge_truth(
    input: &Path,
    src_w: u16,
    src_h: u16,
    fps: u16,
    wanted: &BTreeSet<u32>,
    grid_w: u16,
    grid_h: u16,
) -> Result<BTreeMap<u32, EdgeMask>, BoxErr> {
    let Some(&max_frame) = wanted.iter().next_back() else {
        return Ok(BTreeMap::new());
    };
    let vf = format!("scale={src_w}:{src_h}:flags=area,fps={fps},format=gray");
    let frames_arg = (u64::from(max_frame) + 1).to_string();
    let input_s = input.to_string_lossy();
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-nostdin", "-hide_banner", "-v", "error", "-i", &input_s, "-map", "0:v:0",
        "-vf", &vf, "-frames:v", &frames_arg, "-f", "rawvideo", "-",
    ]);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to run ffmpeg (is it installed and on PATH?): {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let frame_size = src_w as usize * src_h as usize;
    let mut buf = vec![0u8; frame_size];
    let mut masks = BTreeMap::new();
    'frames: for idx in 0..=max_frame {
        let mut filled = 0usize;
        while filled < frame_size {
            let n = stdout.read(&mut buf[filled..])?;
            if n == 0 {
                break 'frames;
            }
            filled += n;
        }
        if wanted.contains(&idx) {
            masks.insert(idx, canny_edge_truth(&buf, src_w, src_h, grid_w, grid_h));
        }
    }
    drop(stdout);
    let status = child.wait()?;
    let err = stderr_thread.join().unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "ffmpeg (edge truth) exited with {status}: {}",
            String::from_utf8_lossy(&err).trim()
        )
        .into());
    }
    Ok(masks)
}

fn frame_ssim(
    player: &Player<'_>,
    cropped: &GrayImage,
    src_w: u16,
    src_h: u16,
    normed: &mut Vec<u8>,
) -> f64 {
    let mut lut = [0u8; 256];
    auto_ascii::pipeline::build_levels_lut(&mut lut, reference_levels(player.luma_src()));
    normed.clear();
    normed.extend(player.luma_src().iter().map(|&v| lut[v as usize]));
    downscale_ssim(cropped, normed, src_w, src_h)
}

fn reference_levels(luma: &[u8]) -> Option<auto_ascii_format::PlaneLevels> {
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
    Some(auto_ascii_format::PlaneLevels { p2: rank(SSIM_REF_LO_PCT), p98: rank(SSIM_REF_HI_PCT) })
}

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

pub(crate) fn base64(data: &[u8]) -> String {
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

pub(crate) fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn fmt_opt(v: Option<f64>, digits: usize) -> String {
    v.map_or("n/a".into(), |v| format!("{v:.digits$}"))
}

fn render_html(report: &EvalReport, evals: &[ClipEval], compare: Option<&CompareReport>) -> String {
    let mut h = String::with_capacity(1 << 20);
    h.push_str(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>auto-ascii eval</title>\n<style>\n\
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
    h.push_str("<h1>auto-ascii eval — contact sheet</h1>\n");
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
            let class = if note.starts_with("info:") { "meta" } else { "fail" };
            h.push_str(&format!("<p class=\"{class}\">note: {}</p>\n", html_escape(note)));
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

        h.push_str("<table><tr><th>metric</th><th>value</th></tr>");
        h.push_str(&format!(
            "<tr><td>downscale-SSIM (truecolor)</td><td>{}</td></tr>",
            fmt_opt(m.ssim, 4)
        ));
        h.push_str(&format!(
            "<tr><td>flicker (glyph switches/cell/s)</td><td>{}</td></tr>",
            fmt_opt(m.flicker_switches_per_cell_sec, 3)
        ));
        h.push_str(&format!(
            "<tr><td>edge F1 vs source Canny (precision / recall)</td>\
             <td>{} ({} / {})</td></tr>",
            fmt_opt(m.edge_f1, 4),
            fmt_opt(m.edge_precision, 4),
            fmt_opt(m.edge_recall, 4)
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

    #[test]
    fn cache_name_includes_pipeline_fingerprint() {
        assert_eq!(PIPELINE_FINGERPRINT.len(), 16, "FNV-1a 64 as hex");
        assert!(PIPELINE_FINGERPRINT.chars().all(|c| c.is_ascii_hexdigit()));
        let input_sha = "aaaaaaaaaaaabbbbbbbbbbbb";
        let params_sha = "ccccccccccccdddddddddddd";
        let name = asset_cache_name("clip", input_sha, params_sha);
        assert_eq!(
            name,
            format!("clip-aaaaaaaaaaaa-cccccccccccc-{PIPELINE_FINGERPRINT}.ascii")
        );
        assert_ne!(asset_cache_name("clip", "e00000000000ffff", params_sha), name);
        assert_ne!(asset_cache_name("clip", input_sha, "e00000000000ffff"), name);
    }

    #[test]
    fn reference_levels_nearest_rank() {
        assert_eq!(reference_levels(&[]), None);
        let flat = reference_levels(&vec![128u8; 1000]).unwrap();
        assert_eq!((flat.p2, flat.p98), (128, 128));
        let spread: Vec<u8> = (0..=255u16).flat_map(|v| [v as u8; 100]).collect();
        let lv = reference_levels(&spread).unwrap();
        assert!((4..=6).contains(&lv.p2), "p2 = {}", lv.p2);
        assert!((249..=251).contains(&lv.p98), "p98 = {}", lv.p98);
        let mut plane = vec![0u8; 20];
        plane.extend(std::iter::repeat_n(100u8, 960));
        plane.extend(std::iter::repeat_n(255u8, 20));
        let lv = reference_levels(&plane).unwrap();
        assert_eq!((lv.p2, lv.p98), (0, 100));
    }

    #[test]
    fn font_table_resolution() {
        let t = resolve_font_table(None).unwrap();
        assert_eq!(t.len(), 95, "default = conservative constants");
        assert!(t.coverage('\u{2588}').is_none());
        let t = resolve_font_table(Some("conservative")).unwrap();
        assert_eq!(t.len(), 95);
        let t = resolve_font_table(Some("dejavu-sans-mono")).unwrap();
        assert!(t.coverage('\u{2588}').unwrap() > 0.9, "per-font: measured block ink");
        let e = resolve_font_table(Some("papyrus")).unwrap_err().to_string();
        assert!(e.contains("ubuntu-mono"), "lists builtins: {e}");
        let mut p = std::env::temp_dir();
        p.push(format!("auto-ascii-eval-ft-{}.toml", std::process::id()));
        std::fs::write(&p, "name = \"f\"\nmissing = []\n[[glyphs]]\nch = \"a\"\ncoverage = 0.5\n")
            .unwrap();
        let t = resolve_font_table(Some(p.to_str().unwrap())).unwrap();
        assert_eq!(t.coverage('a'), Some(0.5));
        let _ = std::fs::remove_file(&p);
    }
}
