use std::io::Cursor;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use auto_ascii::pipeline::Player;
use auto_ascii_core::{
    Cell, ColorDepth, ComposeParams, FramePlanes, GlyphTier, Grid, HysteresisState, Resampler,
    compose_frame, compute_viewport, h_flags, select_palettes,
};
use auto_ascii_format::header::plane_id;
use auto_ascii_format::{Meta, PlaneRef, AsciiReader, AsciiWriter, WriterOptions};
use auto_ascii_term::{Backend, ColorTier, SimBackend};

const BASE_W: u16 = 480;
const BASE_H: u16 = 270;
const FRAMES: u32 = 96;
const GRID: (u16, u16) = (300, 80);

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
                e[i] = mag;
                ex[i] = 128 + (mag >> 1);
            } else if (y + frame / 2) % 30 < 3 {
                e[i] = mag;
                ex[i] = 128 - (mag >> 1);
            } else if (x + y + frame) % 40 < 3 {
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
        factory_version: "auto-ascii-player-bench".to_owned(),
        source: "synthetic-480x270-m3".to_owned(),
        palette_hints: Vec::new(),
    };
    let mut writer =
        AsciiWriter::new(Cursor::new(Vec::new()), opts, &meta).expect("valid bench writer options");
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

fn bench_decode(c: &mut Criterion) {
    let asset = build_synth_asset();
    let mut reader = AsciiReader::open(&asset).expect("bench asset opens");
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

fn bench_present(c: &mut Criterion) {
    let (cols, rows) = GRID;
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

fn bench_e2e_frame(c: &mut Criterion) {
    let asset = build_synth_asset();
    let reader = AsciiReader::open(&asset).expect("bench asset opens");
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
