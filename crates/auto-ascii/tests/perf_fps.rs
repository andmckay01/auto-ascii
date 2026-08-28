//! M2 item E (PLAN §6, amended): the UNTHROTTLED end-to-end SimBackend fps
//! gate — the real player pipeline must sustain ≥ 24 effective fps at
//! 300×80 (throttled-link gates are descoped per the Scope amendment).
//!
//! The measured number on the reference box is in the hundreds even under
//! the dev profile (the workspace pins opt-level 3 for the hot crates); the
//! assertion stays at the 24 fps contract so this never flaps — precise
//! regression tracking is `perf/thresholds.toml` + scripts/perf-gate.sh
//! (criterion medians, item E).
//!
//! Input: deterministic synthetic asset at production geometry (480×270 Y +
//! 240×135 RGB565 C, temporal delta, keyframe 60) — committed gates must be
//! reproducible without the corpus (repo rule).

use std::io::Cursor;
use std::time::Instant;

use auto_ascii::pipeline::Player;
use slpy_format::header::plane_id;
use slpy_format::{Meta, PlaneRef, SlpyReader, SlpyWriter, WriterOptions};
use slpy_term::SimBackend;

const BASE_W: u16 = 480;
const BASE_H: u16 = 270;
const FRAMES: u32 = 96;

fn hash2(x: u32, y: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

fn tri(p: u32) -> u8 {
    let m = p % 512;
    if m < 256 { m as u8 } else { (511 - m) as u8 }
}

/// Same generator family as benches/pipeline.rs: drifting gradient + coarse
/// translating noise — dense, structured frame deltas at production res.
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

fn build_synth_asset() -> Vec<u8> {
    let opts = WriterOptions {
        plane_ids: vec![plane_id::Y, plane_id::C],
        zstd_level: 3, // setup speed; decode cost is level-independent
        ..WriterOptions::default()
    };
    let meta = Meta {
        factory_version: "sleepy-player-perf-test".to_owned(),
        source: "synthetic-480x270".to_owned(),
        palette_hints: Vec::new(),
    };
    let mut writer =
        SlpyWriter::new(Cursor::new(Vec::new()), opts, &meta).expect("valid writer options");
    for frame in 0..FRAMES {
        let y = luma_plane(frame);
        let c = chroma_plane(frame);
        writer
            .write_frame(&[
                PlaneRef { id: plane_id::Y, data: &y },
                PlaneRef { id: plane_id::C, data: &c },
            ])
            .expect("valid frame");
    }
    writer.finish().expect("finish").into_inner()
}

/// PLAN §6 hard gate (amended: unthrottled): ≥ 24 effective fps at 300×80,
/// default `--repaint full` mode, truecolor tier, chroma composited — the
/// most expensive steady-state configuration.
#[test]
fn unthrottled_sim_sustains_24fps_at_300x80() {
    let asset = build_synth_asset();
    let reader = SlpyReader::open(&asset).expect("synthetic asset opens");
    let mut player = Player::new(
        reader,
        2.0,
        true,
        slpy_core::ColorDepth::True,
        slpy_core::GlyphTier::UnicodeBlocks,
    )
    .expect("player");
    let mut backend = SimBackend::new(300, 80);
    player.reflow(&mut backend, 300, 80);
    assert!(player.viewport().is_some(), "300x80 must yield a viewport");

    // Warm-up lap (page in the asset, settle allocations), then measure.
    for i in 0..FRAMES {
        player.render_present(&mut backend, i).expect("warm-up frame");
        backend.take_output();
    }

    let frames = 480u32; // 5 laps: sequential rolls + periodic loop-wrap seeks
    let t0 = Instant::now();
    let mut bytes_total = 0u64;
    for i in 0..frames {
        let stats = player.render_present(&mut backend, i % FRAMES).expect("frame");
        bytes_total += u64::from(stats.bytes);
        backend.take_output();
    }
    let wall = t0.elapsed().as_secs_f64();
    let fps = f64::from(frames) / wall;

    let stage = player.stage();
    println!(
        "unthrottled sim @300x80 truecolor: {fps:.1} fps ({frames} frames in {wall:.3} s, \
         {:.1} KiB/frame, cumulative stage ms decode={:.1} resample={:.1} compose={:.1} \
         present={:.1})",
        bytes_total as f64 / f64::from(frames) / 1024.0,
        stage.decode as f64 / 1e6,
        stage.resample as f64 / 1e6,
        stage.compose as f64 / 1e6,
        stage.present as f64 / 1e6,
    );
    assert!(
        fps >= 24.0,
        "end-to-end unthrottled SimBackend fps gate: {fps:.1} fps < 24 fps at 300x80"
    );
}
