//! M2 item E (PLAN §6): criterion perf gates over the REAL player pipeline —
//! decode (delta roll @480×270, all six planes since M3), resample
//! (480×270 → 300×80), compose (the §3.5 three-layer compositor), present-
//! to-SimBackend (300×80 truecolor + 256), and the end-to-end frame.
//!
//! Thresholds are committed at `perf/thresholds.toml` (measured 3-run
//! median-of-medians ×1.15); `scripts/perf-gate.sh` runs these benches and
//! compares criterion's `estimates.json` medians against them (nonzero exit
//! on breach). Bench IDs here and threshold keys there must stay in sync.
//!
//! The input is a deterministic synthetic asset at PRODUCTION geometry —
//! M3: the full §4 plane set (480×270 Y/E/Ex/Ey/H + 240×135 RGB565 C,
//! temporal delta, keyframe 60) so decode/compose gate the real M3 frame
//! cost — the repo rule: nothing committed may depend on the corpus mp4s.

use std::io::Cursor;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use sleepytime::pipeline::Player;
use slpy_core::{
    Cell, ColorDepth, ComposeParams, FramePlanes, GlyphTier, Grid, HysteresisState, Resampler,
    compose_frame, compute_viewport, h_flags, select_palettes,
};
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

/// Synthetic feature planes (M3): a drifting grid of contours — 3-px
/// vertical stripes every 24 px and 3-px horizontal stripes every 30 rows —
/// with the factory's gradient doubled-angle encoding (INTERFACES note 18:
/// vertical edge → Ex > 128, horizontal → Ex < 128), plus sparse highlight
/// blocks and a deep-shadow band. Edge density ~20% of pixels: a busy frame,
/// so the compose gate sits at the expensive end of real content.
fn feature_planes(frame: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let (w, h) = (u32::from(BASE_W), u32::from(BASE_H));
    let n = (w * h) as usize;
    let (mut e, mut ex, mut ey, mut hp) =
        (vec![0u8; n], vec![128u8; n], vec![128u8; n], vec![0u8; n]);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let mag = 200u8;
            if (x + frame) % 24 < 3 {
                // Vertical contour: gradient along +x → cos 2θg = +1.
                e[i] = mag;
                ex[i] = 128 + (mag >> 1);
            } else if (y + frame / 2) % 30 < 3 {
                // Horizontal contour: gradient along y → cos 2θg = −1.
                e[i] = mag;
                ex[i] = 128 - (mag >> 1);
            } else if (x + y + frame) % 40 < 3 {
                // '\'-diagonal contour: gradient at 135° → sin 2θg = −1.
                e[i] = mag;
                ey[i] = 128 - (mag >> 1);
            }
            if hash2((x >> 3) + frame / 4, y >> 3).is_multiple_of(97) {
                hp[i] |= h_flags::HIGHLIGHT;
            }
            if y >= h - 30 {
                hp[i] |= h_flags::DEEP_SHADOW;
            }
        }
    }
    (e, ex, ey, hp)
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

/// Build the in-memory SLPY asset (all six §4 planes). zstd level 3 (not the
/// factory's 19) keeps bench setup fast; zstd DECODE speed is essentially
/// level-independent and the thresholds are calibrated on this same input.
fn build_synth_asset() -> Vec<u8> {
    let opts = WriterOptions {
        plane_ids: vec![
            plane_id::Y,
            plane_id::E,
            plane_id::EX,
            plane_id::EY,
            plane_id::H,
            plane_id::C,
        ],
        zstd_level: 3,
        ..WriterOptions::default()
    };
    let meta = Meta {
        factory_version: "sleepy-player-bench".to_owned(),
        source: "synthetic-480x270-m3".to_owned(),
        palette_hints: Vec::new(),
    };
    let mut writer =
        SlpyWriter::new(Cursor::new(Vec::new()), opts, &meta).expect("valid bench writer options");
    for frame in 0..FRAMES {
        let y = luma_plane(frame);
        let (e, ex, ey, hp) = feature_planes(frame);
        let c = chroma_plane(frame);
        writer
            .write_frame(&[
                PlaneRef { id: plane_id::Y, data: &y },
                PlaneRef { id: plane_id::E, data: &e },
                PlaneRef { id: plane_id::EX, data: &ex },
                PlaneRef { id: plane_id::EY, data: &ey },
                PlaneRef { id: plane_id::H, data: &hp },
                PlaneRef { id: plane_id::C, data: &c },
            ])
            .expect("valid bench frame");
    }
    writer.finish().expect("bench asset finish").into_inner()
}

/// PLAN §3.6 step 3: sequential delta roll of all six planes through the
/// standing double buffers (exactly the player's `load_frame` sequential
/// path; the 1-in-96 wrap goes through the keyframe-0 seek, like `--loop`).
fn bench_decode(c: &mut Criterion) {
    let asset = build_synth_asset();
    let mut reader = SlpyReader::open(&asset).expect("bench asset opens");
    let y_len = usize::from(BASE_W) * usize::from(BASE_H);
    const IDS: [u8; 5] = [plane_id::Y, plane_id::E, plane_id::EX, plane_id::EY, plane_id::H];
    let mut planes = vec![vec![0u8; y_len]; 5];
    let mut cc = vec![0u8; y_len / 4 * 2];
    for (buf, id) in planes.iter_mut().zip(IDS) {
        reader.seek_plane_into(0, id, buf).unwrap();
    }
    reader.seek_plane_into(0, plane_id::C, &mut cc).unwrap();
    let mut frame = 0u32;
    c.bench_function("decode_delta_roll_480x270", |b| {
        b.iter(|| {
            frame += 1;
            if frame >= FRAMES {
                frame = 0;
                for (buf, id) in planes.iter_mut().zip(IDS) {
                    reader.seek_plane_into(0, id, buf).unwrap();
                }
                reader.seek_plane_into(0, plane_id::C, &mut cc).unwrap();
            } else {
                for (buf, id) in planes.iter_mut().zip(IDS) {
                    reader.decode_plane_into(frame, id, buf).unwrap();
                }
                reader.decode_plane_into(frame, plane_id::C, &mut cc).unwrap();
            }
            (planes[0][0], cc[0])
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

/// Resampled cell-res inputs for the compose/present benches: luma at
/// Vc×2Vr plus features/chroma at Vc×Vr, through the player's own
/// resamplers (H re-thresholded the way the pipeline does).
struct CellPlanes {
    luma2: Vec<u8>,
    e: Vec<u8>,
    ex: Vec<u8>,
    ey: Vec<u8>,
    h: Vec<u8>,
    cr: Vec<u8>,
    cg: Vec<u8>,
    cb: Vec<u8>,
}

fn cell_planes(frame: u32, vc: u16, vr: u16) -> CellPlanes {
    let cells = usize::from(vc) * usize::from(vr);
    let mut luma_rs = Resampler::build(BASE_W, BASE_H, vc, 2 * vr);
    let mut feat_rs = Resampler::build(BASE_W, BASE_H, vc, vr);
    let mut out = CellPlanes {
        luma2: vec![0; cells * 2],
        e: vec![0; cells],
        ex: vec![0; cells],
        ey: vec![0; cells],
        h: vec![0; cells],
        cr: vec![0; cells],
        cg: vec![0; cells],
        cb: vec![0; cells],
    };
    luma_rs.apply(&luma_plane(frame), &mut out.luma2);
    let (e, ex, ey, hp) = feature_planes(frame);
    feat_rs.apply(&e, &mut out.e);
    feat_rs.apply(&ex, &mut out.ex);
    feat_rs.apply(&ey, &mut out.ey);
    let hl: Vec<u8> = hp.iter().map(|&h| if h & 1 != 0 { 255 } else { 0 }).collect();
    let sh: Vec<u8> = hp.iter().map(|&h| if h & 2 != 0 { 255 } else { 0 }).collect();
    let (mut hld, mut shd) = (vec![0u8; cells], vec![0u8; cells]);
    feat_rs.apply(&hl, &mut hld);
    feat_rs.apply(&sh, &mut shd);
    for i in 0..cells {
        out.h[i] = u8::from(hld[i] >= 64) | (u8::from(shd[i] >= 128) << 1);
    }
    // Chroma: unpack + resample (channel planes at half res → cell res).
    let cplane = chroma_plane(frame);
    let clen = cplane.len() / 2;
    let (mut r8, mut g8, mut b8) = (vec![0u8; clen], vec![0u8; clen], vec![0u8; clen]);
    for (i, px) in cplane.chunks_exact(2).enumerate() {
        let v = u16::from_le_bytes([px[0], px[1]]);
        let r5 = (v >> 11) as u8;
        let g6 = ((v >> 5) & 0x3f) as u8;
        let b5 = (v & 0x1f) as u8;
        r8[i] = (r5 << 3) | (r5 >> 2);
        g8[i] = (g6 << 2) | (g6 >> 4);
        b8[i] = (b5 << 3) | (b5 >> 2);
    }
    let mut crs = Resampler::build(BASE_W / 2, BASE_H / 2, vc, vr);
    crs.apply(&r8, &mut out.cr);
    crs.apply(&g8, &mut out.cg);
    crs.apply(&b8, &mut out.cb);
    out
}

/// PLAN §3.4/§3.5: the three-layer compositor over the letterboxed viewport
/// inside the 300×80 grid — full plane set, unicode palette, hysteresis
/// state carried across iterations. Two alternating input frames keep the
/// hysteresis from settling into an all-hold steady state.
fn bench_compose(c: &mut Criterion) {
    let (cols, rows) = GRID;
    let vp = compute_viewport(cols, rows, 2.0).expect("300x80 is far above the 32x9 minimum");
    let a = cell_planes(7, vp.cols, vp.rows);
    let b2 = cell_planes(8, vp.cols, vp.rows);
    let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, vp.cols);
    let params = ComposeParams::default();
    let lut: [u8; 256] = core::array::from_fn(|i| i as u8);
    let mut state = HysteresisState::new(vp.cols, vp.rows);
    let mut grid: Grid<Cell> = Grid::new(cols, rows);
    let mut flip = false;
    c.bench_function("compose_300x80", |bch| {
        bch.iter(|| {
            flip = !flip;
            let p = if flip { &a } else { &b2 };
            let planes = FramePlanes {
                luma2: &p.luma2,
                e: Some(&p.e),
                ex: Some(&p.ex),
                ey: Some(&p.ey),
                h: Some(&p.h),
                chroma: Some((&p.cr, &p.cg, &p.cb)),
            };
            compose_frame(&planes, &vp, &lut, &set, &params, &mut state, &mut grid);
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
    // A realistic composed M3 frame (same content as bench_compose).
    let vp = compute_viewport(cols, rows, 2.0).expect("300x80 viewport");
    let p = cell_planes(7, vp.cols, vp.rows);
    let set = select_palettes(GlyphTier::UnicodeBlocks, ColorDepth::True, vp.cols);
    let lut: [u8; 256] = core::array::from_fn(|i| i as u8);
    let mut state = HysteresisState::new(vp.cols, vp.rows);
    let mut grid: Grid<Cell> = Grid::new(cols, rows);
    let planes = FramePlanes {
        luma2: &p.luma2,
        e: Some(&p.e),
        ex: Some(&p.ex),
        ey: Some(&p.ey),
        h: Some(&p.h),
        chroma: Some((&p.cr, &p.cg, &p.cb)),
    };
    compose_frame(&planes, &vp, &lut, &set, &ComposeParams::default(), &mut state, &mut grid);

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
    let mut player =
        Player::new(reader, 2.0, true, ColorDepth::True, GlyphTier::UnicodeBlocks)
            .expect("bench player");
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
    // committed thresholds carry 15% headroom on top; verified 3× green).
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
