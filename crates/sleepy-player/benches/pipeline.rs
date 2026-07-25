//! M2 item E (PLAN §6): criterion perf gates over the REAL player pipeline —
//! decode (delta roll @480×270 Y+C), resample (480×270 → 300×80), compose,
//! present-to-SimBackend (300×80 truecolor + 256), and the end-to-end frame.
//!
//! Thresholds are committed at `perf/thresholds.toml`, calibrated ~25–30%
//! above the measured median on the reference box so the gate never flaps on
//! scheduler noise; `scripts/perf-gate.sh` runs these benches and compares
//! criterion's `estimates.json` medians against them (nonzero exit on
//! breach). Bench IDs here and threshold keys there must stay in sync.
//!
//! The input is a deterministic synthetic asset at PRODUCTION geometry
//! (480×270 Y + 240×135 RGB565 C, temporal delta, keyframe 60) — the repo
//! rule: nothing committed may depend on the corpus mp4s.

use std::io::Cursor;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use sleepy_player::pipeline::{Player, compose_cells};
use slpy_core::ramp::base_ramp_for_cols;
use slpy_core::{Cell, Grid, Resampler, compute_viewport};
use slpy_format::header::plane_id;
use slpy_format::{Meta, PlaneRef, SlpyReader, SlpyWriter, WriterOptions};
use slpy_term::{Backend, ColorTier, SimBackend};

/// Production plane geometry (PLAN §4).
const BASE_W: u16 = 480;
const BASE_H: u16 = 270;
/// Frames in the synthetic asset: > one keyframe group (interval 60), so the
/// sequential roll is almost entirely delta decodes, like real playback.
const FRAMES: u32 = 96;
/// The gate grid (PLAN §6: perf budgets are quoted @300×80 = 24k cells).
const GRID: (u16, u16) = (300, 80);

/// Integer hash (xorshift-multiply mix) — stationary per-block noise field.
fn hash2(x: u32, y: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

/// Triangle wave over a 512 period (smooth, pure integer).
fn tri(p: u32) -> u8 {
    let m = p % 512;
    if m < 256 { m as u8 } else { (511 - m) as u8 }
}

/// Y plane: drifting diagonal gradient + coarse noise blocks that translate
/// with time — frame deltas are dense but structured, so zstd sees realistic
/// deltas (neither trivially constant nor incompressible).
fn luma_plane(frame: u32) -> Vec<u8> {
    let (w, h) = (u32::from(BASE_W), u32::from(BASE_H));
    let mut plane = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let base = tri(2 * x + 3 * y + 5 * frame);
            let noise = (hash2((x >> 2) + frame, y >> 2) & 0x3F) as u8;
            plane.push(base.wrapping_add(noise));
        }
    }
    plane
}

/// C plane (half res, RGB565 little-endian — the factory contract, PLAN §4).
fn chroma_plane(frame: u32) -> Vec<u8> {
    let (cw, ch) = (u32::from(BASE_W) / 2, u32::from(BASE_H) / 2);
    let mut plane = Vec::with_capacity((cw * ch * 2) as usize);
    for cy in 0..ch {
        for cx in 0..cw {
            let r = u16::from(tri(3 * cx + 4 * frame)) >> 3;
            let g = u16::from(tri(2 * cy + 3 * frame + 85)) >> 2;
            let b = u16::from(tri(cx + cy + 2 * frame + 170)) >> 3;
            let px = (r << 11) | (g << 5) | b;
            plane.extend_from_slice(&px.to_le_bytes());
        }
    }
    plane
}

/// Build the in-memory SLPY asset. zstd level 3 (not the factory's 19) keeps
/// bench setup fast; zstd DECODE speed is essentially level-independent and
/// the thresholds are calibrated on this same input anyway.
fn build_synth_asset() -> Vec<u8> {
    let opts = WriterOptions {
        plane_ids: vec![plane_id::Y, plane_id::C],
        zstd_level: 3,
        ..WriterOptions::default()
    };
    let meta = Meta {
        factory_version: "sleepy-player-bench".to_owned(),
        source: "synthetic-480x270".to_owned(),
        palette_hints: Vec::new(),
    };
    let mut writer =
        SlpyWriter::new(Cursor::new(Vec::new()), opts, &meta).expect("valid bench writer options");
    for frame in 0..FRAMES {
        let y = luma_plane(frame);
        let c = chroma_plane(frame);
        writer
            .write_frame(&[
                PlaneRef { id: plane_id::Y, data: &y },
                PlaneRef { id: plane_id::C, data: &c },
            ])
            .expect("valid bench frame");
    }
    writer.finish().expect("bench asset finish").into_inner()
}

/// PLAN §3.6 step 3: sequential delta roll of Y + C through the standing
/// double buffers (exactly the player's `load_frame` sequential path; the
/// 1-in-96 wrap goes through the keyframe-0 seek, like `--loop`).
fn bench_decode(c: &mut Criterion) {
    let asset = build_synth_asset();
    let mut reader = SlpyReader::open(&asset).expect("bench asset opens");
    let y_len = usize::from(BASE_W) * usize::from(BASE_H);
    let mut y = vec![0u8; y_len];
    let mut cc = vec![0u8; y_len / 4 * 2];
    reader.seek_plane_into(0, plane_id::Y, &mut y).unwrap();
    reader.seek_plane_into(0, plane_id::C, &mut cc).unwrap();
    let mut frame = 0u32;
    c.bench_function("decode_delta_roll_480x270", |b| {
        b.iter(|| {
            frame += 1;
            if frame >= FRAMES {
                frame = 0;
                reader.seek_plane_into(0, plane_id::Y, &mut y).unwrap();
                reader.seek_plane_into(0, plane_id::C, &mut cc).unwrap();
            } else {
                reader.decode_plane_into(frame, plane_id::Y, &mut y).unwrap();
                reader.decode_plane_into(frame, plane_id::C, &mut cc).unwrap();
            }
            (y[0], cc[0])
        })
    });
}

/// PLAN §3.3: separable Q8 box resample of the Y plane, 480×270 → 300×80.
fn bench_resample(c: &mut Criterion) {
    let src = luma_plane(7);
    let (cols, rows) = GRID;
    let mut rs = Resampler::build(BASE_W, BASE_H, cols, rows);
    let mut dst = vec![0u8; usize::from(cols) * usize::from(rows)];
    c.bench_function("resample_480x270_to_300x80", |b| {
        b.iter(|| {
            rs.apply(&src, &mut dst);
            dst[0]
        })
    });
}

/// PLAN §3.4/§3.5: compose the viewport (letterboxed inside the 300×80 grid)
/// from normalized luma + resampled chroma channels.
fn bench_compose(c: &mut Criterion) {
    let (cols, rows) = GRID;
    let vp = compute_viewport(cols, rows, 2.0).expect("300x80 is far above the 32x9 minimum");
    let cells = usize::from(vp.cols) * usize::from(vp.rows);
    let luma: Vec<u8> = (0..cells).map(|i| (i * 131) as u8).collect();
    let cr: Vec<u8> = (0..cells).map(|i| (i * 31) as u8).collect();
    let cg: Vec<u8> = (0..cells).map(|i| (i * 17) as u8).collect();
    let cb: Vec<u8> = (0..cells).map(|i| (i * 7) as u8).collect();
    let ramp = base_ramp_for_cols(vp.cols);
    let mut grid: Grid<Cell> = Grid::new(cols, rows);
    c.bench_function("compose_300x80", |b| {
        b.iter(|| {
            compose_cells(&luma, Some((&cr, &cg, &cb)), &vp, ramp, &mut grid);
            grid.get(vp.pad_left, vp.pad_top).ch
        })
    });
}

/// PLAN §3.6 step 6 in the default `--repaint full` mode: full-invalidate
/// quantize → diff → SGR-elide → simulated write. `take_output` is included
/// (it is part of every SimBackend frame in `--sim`/eval runs and keeps the
/// capture buffer from growing across iterations).
fn bench_present(c: &mut Criterion) {
    let (cols, rows) = GRID;
    // A realistic composed frame (same content as bench_compose).
    let vp = compute_viewport(cols, rows, 2.0).expect("300x80 viewport");
    let cells = usize::from(vp.cols) * usize::from(vp.rows);
    let luma: Vec<u8> = (0..cells).map(|i| (i * 131) as u8).collect();
    let cr: Vec<u8> = (0..cells).map(|i| (i * 31) as u8).collect();
    let cg: Vec<u8> = (0..cells).map(|i| (i * 17) as u8).collect();
    let cb: Vec<u8> = (0..cells).map(|i| (i * 7) as u8).collect();
    let mut grid: Grid<Cell> = Grid::new(cols, rows);
    compose_cells(&luma, Some((&cr, &cg, &cb)), &vp, base_ramp_for_cols(vp.cols), &mut grid);

    for (id, tier) in [
        ("present_truecolor_300x80", ColorTier::True),
        ("present_256_300x80", ColorTier::C256),
    ] {
        let mut backend = SimBackend::new(cols, rows);
        let mut caps = backend.caps().clone();
        caps.color = tier;
        backend.set_caps(caps);
        c.bench_function(id, |b| {
            b.iter(|| {
                backend.invalidate();
                let stats = backend.present(&grid);
                backend.take_output();
                stats.bytes
            })
        });
    }
}

/// The whole §3.6 frame: decode → resample → NORM → compose → present, via
/// the real `Player` against SimBackend at 300×80 truecolor, default
/// `--repaint full` mode — the per-frame cost the fps gate divides into.
fn bench_e2e_frame(c: &mut Criterion) {
    let asset = build_synth_asset();
    let reader = SlpyReader::open(&asset).expect("bench asset opens");
    let mut player = Player::new(reader, 2.0, true, true).expect("bench player");
    let (cols, rows) = GRID;
    let mut backend = SimBackend::new(cols, rows);
    player.reflow(&mut backend, cols, rows);
    let mut i = 0u32;
    c.bench_function("e2e_frame_300x80", |b| {
        b.iter(|| {
            let stats = player.render_present(&mut backend, i % FRAMES).expect("render");
            i += 1;
            backend.take_output();
            stats.bytes
        })
    });
}

fn config() -> Criterion {
    // Medians over 30 samples × 3 s are stable on this noisy 4-core box (the
    // committed thresholds carry 25–30% headroom on top; verified 3× green).
    Criterion::default()
        .sample_size(30)
        .measurement_time(Duration::from_secs(3))
        .warm_up_time(Duration::from_secs(1))
}

criterion_group! {
    name = pipeline;
    config = config();
    targets = bench_decode, bench_resample, bench_compose, bench_present, bench_e2e_frame
}
criterion_main!(pipeline);
