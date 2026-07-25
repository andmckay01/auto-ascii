//! M1 format acceptance (PLAN §4 / §7 M1): temporal delta + keyframe
//! roundtrip across hard cuts, FIDX seek == sequential decode, NORM runtime
//! levels, chroma plane C, delta-vs-intra compression, and hostile-input
//! rejection (clean errors, never panics).

use std::io::Cursor;
use std::sync::OnceLock;

use slpy_format::{
    CHUNK_HEADER_SIZE, ChunkHeader, Meta, PlaneLevels, PlaneRef, ShotRecord, SlpyError,
    SlpyReader, SlpyWriter, TAG_FRAM, TAG_NORM, WriterOptions, filter, frame_flags, norm_flags,
    plane_id,
};

// ---------------------------------------------------------------------------
// Synthetic 3-shot sequence with hard cuts (PLAN §6: deterministic synthetic
// planes are the regression baseline). Small dims keep zstd cheap.
// ---------------------------------------------------------------------------

const W: u16 = 64;
const H: u16 = 36;
const Y_LEN: usize = 64 * 36;
const C_LEN: usize = 32 * 18 * 2; // RGB565, half res
const FRAMES: u32 = 150;
const IVL: u8 = 7;
/// Hard cuts: shot 0 = [0,40), shot 1 = [40,90), shot 2 = [90,150).
const CUTS: [u32; 2] = [40, 90];

fn shot_of(f: u32) -> u8 {
    CUTS.iter().filter(|&&c| f >= c).count() as u8
}

/// Luma: shot-dependent base + slow coherent motion within the shot.
fn synth_y(f: u32) -> Vec<u8> {
    let s = shot_of(f);
    let fx = f as usize;
    let mut v = vec![0u8; Y_LEN];
    for (y, row) in v.chunks_exact_mut(W as usize).enumerate() {
        for (x, px) in row.iter_mut().enumerate() {
            let grad = (x * 4) as u8 ^ s.wrapping_mul(85);
            let stripe =
                if ((x + fx / 2) / 8 + (y + fx / 3) / 4).is_multiple_of(2) { 40u8 } else { 0 };
            *px = grad
                .wrapping_add((y * 3) as u8)
                .wrapping_add(stripe)
                .wrapping_add(s.wrapping_mul(71));
        }
    }
    v
}

/// Chroma: RGB565 field, shot-dependent hue, slow drift.
fn synth_c(f: u32) -> Vec<u8> {
    let s = shot_of(f);
    let fx = f as usize;
    let mut v = vec![0u8; C_LEN];
    for i in 0..C_LEN / 2 {
        let (x, y) = (i % 32, i / 32);
        let r = ((x + fx / 4) & 0x1F) as u16;
        let g = ((y * 2 + s as usize * 13) & 0x3F) as u16;
        let b = ((x + y + s as usize * 7) & 0x1F) as u16;
        let px = (r << 11) | (g << 5) | b;
        v[2 * i..2 * i + 2].copy_from_slice(&px.to_le_bytes());
    }
    v
}

fn lv(pairs: &[(u8, u8)]) -> [PlaneLevels; 8] {
    let mut out = [PlaneLevels::default(); 8];
    for (slot, &(p2, p98)) in out.iter_mut().zip(pairs) {
        *slot = PlaneLevels { p2, p98 };
    }
    out
}

/// NORM records matching the synthetic cuts (levels indexed by plane
/// POSITION in `plane_ids`: 0 = Y, 1 = C).
fn test_shots() -> Vec<ShotRecord> {
    vec![
        ShotRecord { first_frame: 0, flags: 0, levels: lv(&[(10, 240), (5, 250)]) },
        ShotRecord { first_frame: 40, flags: norm_flags::CUT, levels: lv(&[(30, 220), (8, 245)]) },
        ShotRecord { first_frame: 90, flags: norm_flags::CUT, levels: lv(&[(2, 180), (0, 255)]) },
    ]
}

fn test_meta() -> Meta {
    Meta {
        factory_version: "slpy-format-m1-test".to_string(),
        source: "synthetic-3-shot".to_string(),
        palette_hints: Vec::new(),
    }
}

fn m1_opts(filter_: u8) -> WriterOptions {
    WriterOptions {
        base_w: W,
        base_h: H,
        plane_ids: vec![plane_id::Y, plane_id::C],
        filter: filter_,
        keyframe_ivl: IVL,
        zstd_level: 3, // determinism is golden-tested elsewhere; keep M1 tests fast
        ..WriterOptions::default()
    }
}

fn build(filter_: u8) -> Vec<u8> {
    let mut writer =
        SlpyWriter::new(Cursor::new(Vec::new()), m1_opts(filter_), &test_meta()).unwrap();
    writer.write_norm(&test_shots()).unwrap();
    for f in 0..FRAMES {
        let y = synth_y(f);
        let c = synth_c(f);
        writer
            .write_frame(&[
                PlaneRef { id: plane_id::Y, data: &y },
                PlaneRef { id: plane_id::C, data: &c },
            ])
            .unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn delta_asset() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| build(filter::TEMPORAL_DELTA))
}

/// Chunk-walk helper (CRC-on profile: 16-B header + payload + 4-B CRC).
fn find_chunk(bytes: &[u8], tag: [u8; 4]) -> (usize, ChunkHeader) {
    let mut pos = 64usize;
    while pos < bytes.len() {
        let ch = ChunkHeader::from_bytes(bytes[pos..pos + CHUNK_HEADER_SIZE].try_into().unwrap());
        if ch.tag == tag {
            return (pos, ch);
        }
        pos += CHUNK_HEADER_SIZE + ch.size as usize + 4;
    }
    panic!("chunk {:?} not found", String::from_utf8_lossy(&tag));
}

// ---------------------------------------------------------------------------
// Delta + keyframe roundtrip across cuts, both planes, keyframe cadence.
// ---------------------------------------------------------------------------

#[test]
fn delta_roundtrip_with_cuts_and_chroma() {
    let mut reader = SlpyReader::open(delta_asset()).unwrap();
    let h = reader.header();
    assert_eq!(h.filter, filter::TEMPORAL_DELTA);
    assert_eq!(h.keyframe_ivl, IVL);
    assert_eq!(h.plane_count, 2);
    assert_eq!(reader.plane_dims(plane_id::Y), Some((W, H)));
    assert_eq!(reader.plane_dims(plane_id::C), Some((W / 2, H / 2)));

    // Sequential decode into standing buffers (the play path, PLAN §3.6):
    // every frame must reproduce the raw source exactly, including across
    // the hard cuts at 40/90 (deltas are content-agnostic byte arithmetic).
    let mut y = vec![0u8; Y_LEN];
    let mut c = vec![0u8; C_LEN];
    for f in 0..FRAMES {
        assert_eq!(reader.decode_plane_into(f, plane_id::Y, &mut y).unwrap(), Y_LEN);
        assert_eq!(reader.decode_plane_into(f, plane_id::C, &mut c).unwrap(), C_LEN);
        assert_eq!(y, synth_y(f), "Y frame {f} decoded != source");
        assert_eq!(c, synth_c(f), "C frame {f} decoded != source");
        // Keyframe cadence: every IVL frames from 0 (PLAN §4).
        assert_eq!(reader.is_keyframe(f).unwrap(), f % u32::from(IVL) == 0, "frame {f}");
    }
    reader.verify().unwrap();
}

// ---------------------------------------------------------------------------
// Seek (M1 acceptance 2): random frames via FIDX land byte-identical to
// sequential decode; nearest-keyframe binary search is exact.
// ---------------------------------------------------------------------------

#[test]
fn seek_matches_sequential_at_50_random_frames() {
    let mut reader = SlpyReader::open(delta_asset()).unwrap();

    // Sequential reference decode of the whole asset.
    let mut ref_y: Vec<Vec<u8>> = Vec::new();
    let mut ref_c: Vec<Vec<u8>> = Vec::new();
    let mut y = vec![0u8; Y_LEN];
    let mut c = vec![0u8; C_LEN];
    for f in 0..FRAMES {
        reader.decode_plane_into(f, plane_id::Y, &mut y).unwrap();
        reader.decode_plane_into(f, plane_id::C, &mut c).unwrap();
        ref_y.push(y.clone());
        ref_c.push(c.clone());
    }

    // 50 deterministic pseudo-random targets (LCG — no RNG dep).
    let mut state = 0x1234_5678_9abc_def0u64;
    for _ in 0..50 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let f = ((state >> 33) as u32) % FRAMES;

        let key = reader.nearest_keyframe_at_or_before(f).unwrap();
        assert_eq!(key, f - f % u32::from(IVL), "keyframe search for {f}");

        // dst starts as garbage: seek must not depend on prior state.
        let mut sy = vec![0xAAu8; Y_LEN];
        let mut sc = vec![0xAAu8; C_LEN];
        assert_eq!(reader.seek_plane_into(f, plane_id::Y, &mut sy).unwrap(), Y_LEN);
        assert_eq!(reader.seek_plane_into(f, plane_id::C, &mut sc).unwrap(), C_LEN);
        assert_eq!(sy, ref_y[f as usize], "seek Y at {f} != sequential decode");
        assert_eq!(sc, ref_c[f as usize], "seek C at {f} != sequential decode");
    }
}

// ---------------------------------------------------------------------------
// NORM roundtrip + typed lookup API (M1 acceptance 3 format side).
// ---------------------------------------------------------------------------

#[test]
fn norm_roundtrip_and_lookup() {
    let reader = SlpyReader::open(delta_asset()).unwrap();
    let shots = test_shots();
    assert_eq!(reader.shots(), &shots[..]);

    assert!(!reader.shots()[0].is_cut());
    assert!(reader.shots()[1].is_cut() && reader.shots()[2].is_cut());

    for (f, want) in [(0, 0), (39, 0), (40, 40), (89, 40), (90, 90), (149, 90)] {
        assert_eq!(reader.shot_for_frame(f).unwrap().first_frame, want, "frame {f}");
    }

    assert_eq!(reader.plane_index(plane_id::Y), Some(0));
    assert_eq!(reader.plane_index(plane_id::C), Some(1));
    assert_eq!(reader.plane_index(plane_id::E), None);

    assert_eq!(reader.norm_levels(45, plane_id::Y), Some(PlaneLevels { p2: 30, p98: 220 }));
    assert_eq!(reader.norm_levels(45, plane_id::C), Some(PlaneLevels { p2: 8, p98: 245 }));
    assert_eq!(reader.norm_levels(139, plane_id::Y), Some(PlaneLevels { p2: 2, p98: 180 }));
    assert_eq!(reader.norm_levels(45, plane_id::E), None); // plane not in asset
}

#[test]
fn asset_without_norm_has_empty_shots() {
    let mut writer =
        SlpyWriter::new(Cursor::new(Vec::new()), m1_opts(filter::TEMPORAL_DELTA), &test_meta())
            .unwrap();
    let (y, c) = (synth_y(0), synth_c(0));
    writer
        .write_frame(&[PlaneRef { id: plane_id::Y, data: &y }, PlaneRef { id: plane_id::C, data: &c }])
        .unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let reader = SlpyReader::open(&bytes).unwrap();
    assert!(reader.shots().is_empty());
    assert_eq!(reader.shot_for_frame(0), None);
    assert_eq!(reader.norm_levels(0, plane_id::Y), None);
}

// ---------------------------------------------------------------------------
// Intra profile (M0 semantics) still writes/reads: back-compat path.
// ---------------------------------------------------------------------------

#[test]
fn intra_profile_reads_and_seeks_order_independent() {
    let bytes = build(filter::INTRA);
    let mut reader = SlpyReader::open(&bytes).unwrap();
    assert_eq!(reader.header().filter, filter::INTRA);

    // Every frame keyframe-flagged; random access needs no rolling.
    for f in [0u32, 1, 73, 149] {
        assert!(reader.is_keyframe(f).unwrap());
        assert_eq!(reader.nearest_keyframe_at_or_before(f).unwrap(), f);
        let mut y = vec![0xAAu8; Y_LEN];
        reader.decode_plane_into(f, plane_id::Y, &mut y).unwrap();
        assert_eq!(y, synth_y(f), "intra direct decode at {f}");
        let mut y2 = vec![0x55u8; Y_LEN];
        reader.seek_plane_into(f, plane_id::Y, &mut y2).unwrap();
        assert_eq!(y2, y, "intra seek at {f}");
    }
}

// ---------------------------------------------------------------------------
// Delta + zstd measurably smaller than intra + zstd on the same input
// (M1 acceptance 1, format side; factory profile zstd-19, keyframe 60).
// ---------------------------------------------------------------------------

#[test]
fn delta_plus_zstd_beats_intra() {
    const RW: usize = 480;
    const RH: usize = 270;
    const N: u32 = 30;

    // Temporally coherent full-res luma: static gradient + a moving band —
    // most inter-frame deltas are zero, like real EMA-smoothed footage.
    fn frame(f: u32) -> Vec<u8> {
        let mut v = vec![0u8; RW * RH];
        for (y, row) in v.chunks_exact_mut(RW).enumerate() {
            for (x, px) in row.iter_mut().enumerate() {
                let grad = ((x * 255) / (RW - 1)) as u8;
                let vgrad = ((y * 128) / (RH - 1)) as u8;
                let band = if (x + f as usize) % 120 < 30 && y / 32 % 2 == 0 { 50u8 } else { 0 };
                *px = grad.wrapping_add(vgrad).wrapping_add(band);
            }
        }
        v
    }

    let encode = |filter_: u8| -> usize {
        let opts = WriterOptions { filter: filter_, ..WriterOptions::default() };
        let mut writer = SlpyWriter::new(Cursor::new(Vec::new()), opts, &test_meta()).unwrap();
        for f in 0..N {
            let plane = frame(f);
            writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &plane }]).unwrap();
        }
        writer.finish().unwrap().into_inner().len()
    };

    let intra = encode(filter::INTRA);
    let delta = encode(filter::TEMPORAL_DELTA);
    println!("intra={intra} B, delta={delta} B, ratio={:.2}x", intra as f64 / delta as f64);
    assert!(
        (delta as f64) < intra as f64 * 0.8,
        "delta ({delta} B) must be measurably smaller than intra ({intra} B)"
    );
}

// ---------------------------------------------------------------------------
// Writer misuse: NORM ordering rules; rejected frames must not desync the
// delta reference.
// ---------------------------------------------------------------------------

#[test]
fn write_norm_ordering_rules() {
    let shots = test_shots();

    // After a frame: rejected.
    let mut w =
        SlpyWriter::new(Cursor::new(Vec::new()), m1_opts(filter::TEMPORAL_DELTA), &test_meta())
            .unwrap();
    let (y, c) = (synth_y(0), synth_c(0));
    w.write_frame(&[PlaneRef { id: plane_id::Y, data: &y }, PlaneRef { id: plane_id::C, data: &c }])
        .unwrap();
    assert!(w.write_norm(&shots).is_err());

    // Twice: rejected.
    let mut w =
        SlpyWriter::new(Cursor::new(Vec::new()), m1_opts(filter::TEMPORAL_DELTA), &test_meta())
            .unwrap();
    w.write_norm(&shots).unwrap();
    assert!(w.write_norm(&shots).is_err());

    // Empty / first_frame != 0 / non-increasing: rejected.
    let mut w =
        SlpyWriter::new(Cursor::new(Vec::new()), m1_opts(filter::TEMPORAL_DELTA), &test_meta())
            .unwrap();
    assert!(w.write_norm(&[]).is_err());
    assert!(w.write_norm(&shots[1..]).is_err()); // starts at 40
    let unsorted = vec![shots[0], shots[2], shots[1]];
    assert!(w.write_norm(&unsorted).is_err());
}

#[test]
fn rejected_frame_does_not_desync_delta_state() {
    let mut writer =
        SlpyWriter::new(Cursor::new(Vec::new()), m1_opts(filter::TEMPORAL_DELTA), &test_meta())
            .unwrap();
    let (y0, c0) = (synth_y(0), synth_c(0));
    let (y1, c1) = (synth_y(1), synth_c(1));
    writer
        .write_frame(&[PlaneRef { id: plane_id::Y, data: &y0 }, PlaneRef { id: plane_id::C, data: &c0 }])
        .unwrap();
    // Rejected mid-frame (second plane wrong id): prev must stay untouched.
    assert!(
        writer
            .write_frame(&[
                PlaneRef { id: plane_id::Y, data: &y1 },
                PlaneRef { id: plane_id::E, data: &c1 },
            ])
            .is_err()
    );
    writer
        .write_frame(&[PlaneRef { id: plane_id::Y, data: &y1 }, PlaneRef { id: plane_id::C, data: &c1 }])
        .unwrap();
    let bytes = writer.finish().unwrap().into_inner();

    let mut reader = SlpyReader::open(&bytes).unwrap();
    let mut y = vec![0u8; Y_LEN];
    reader.decode_plane_into(0, plane_id::Y, &mut y).unwrap();
    assert_eq!(y, y0);
    reader.decode_plane_into(1, plane_id::Y, &mut y).unwrap();
    assert_eq!(y, y1, "delta chain desynced by a rejected frame");
}

#[test]
fn zero_frame_asset_is_openable() {
    let writer =
        SlpyWriter::new(Cursor::new(Vec::new()), m1_opts(filter::TEMPORAL_DELTA), &test_meta())
            .unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let mut reader = SlpyReader::open(&bytes).unwrap();
    assert_eq!(reader.frame_count(), 0);
    let mut y = vec![0u8; Y_LEN];
    assert!(matches!(
        reader.decode_plane_into(0, plane_id::Y, &mut y),
        Err(SlpyError::BadFrameIndex(0))
    ));
    assert!(matches!(reader.is_keyframe(0), Err(SlpyError::BadFrameIndex(0))));
    assert!(matches!(
        reader.nearest_keyframe_at_or_before(0),
        Err(SlpyError::BadFrameIndex(0))
    ));
}

// ---------------------------------------------------------------------------
// Hostile inputs (M1 acceptance + M0 adversarial-review regressions):
// clean SlpyError, never a panic.
// ---------------------------------------------------------------------------

#[test]
fn hostile_header_fields_are_clean_errors() {
    // (offset, bytes-to-write, what it corrupts)
    let cases: &[(usize, &[u8], &str)] = &[
        (16, &[0, 0], "fps_num = 0 (regression: player Duration panic)"),
        (18, &[0, 0], "fps_den = 0"),
        (20, &[0, 0], "base_w = 0 (regression: downstream assert panic)"),
        (22, &[0, 0], "base_h = 0"),
        (32, &[0], "plane_count = 0"),
        (32, &[9], "plane_count = 9"),
        (33, &[0], "codec = raw (unsupported)"),
        (34, &[5], "filter = 5 (unknown)"),
        (35, &[0], "keyframe_ivl = 0"),
        (36, &[0], "plane id 0 in registry"),
        (37, &[1], "duplicate plane id in registry"),
        // index_offset near u64::MAX: the bounds check must use checked_add —
        // a wrapping `io + CHUNK_HEADER_SIZE` passed and open() panicked at
        // the FIDX header slice (confirmed adversarial-review regression).
        (44, &u64::MAX.to_le_bytes(), "index_offset = u64::MAX (checked_add regression)"),
        (44, &(u64::MAX - 8).to_le_bytes(), "index_offset = u64::MAX - 8"),
    ];
    for &(off, patch, what) in cases {
        let mut bytes = delta_asset().to_vec();
        bytes[off..off + patch.len()].copy_from_slice(patch);
        match SlpyReader::open(&bytes) {
            Err(SlpyError::Corrupt(_)) => {}
            Err(other) => panic!("{what}: expected Corrupt, got {other:?}"),
            Ok(_) => panic!("{what}: expected Corrupt, got Ok"),
        }
    }
}

#[test]
fn hostile_norm_is_a_clean_error() {
    let (norm_off, _) = find_chunk(delta_asset(), TAG_NORM);
    let payload = norm_off + CHUNK_HEADER_SIZE;

    // Record 1 first_frame -> 0: not strictly increasing.
    let mut bytes = delta_asset().to_vec();
    bytes[payload + 24..payload + 28].copy_from_slice(&0u32.to_le_bytes());
    assert!(matches!(SlpyReader::open(&bytes), Err(SlpyError::Corrupt(_))));

    // Record 2 first_frame -> 60000: past frame_count.
    let mut bytes = delta_asset().to_vec();
    bytes[payload + 48..payload + 52].copy_from_slice(&60000u32.to_le_bytes());
    assert!(matches!(SlpyReader::open(&bytes), Err(SlpyError::Corrupt(_))));

    // Record 0 first_frame -> 1: NORM must start at frame 0.
    let mut bytes = delta_asset().to_vec();
    bytes[payload..payload + 4].copy_from_slice(&1u32.to_le_bytes());
    assert!(matches!(SlpyReader::open(&bytes), Err(SlpyError::Corrupt(_))));
}

#[test]
fn hostile_fidx_is_a_clean_error() {
    let bytes = delta_asset();
    let index_offset = u64::from_le_bytes(bytes[44..52].try_into().unwrap()) as usize;
    let entry0 = index_offset + CHUNK_HEADER_SIZE;

    // Frame 0 stripped of its keyframe flag: a delta asset with no starting
    // keyframe is undecodable.
    let mut b = bytes.to_vec();
    b[entry0 + 12] = 0;
    assert!(matches!(SlpyReader::open(&b), Err(SlpyError::Corrupt(_))));

    // Entry 0 offset -> u64::MAX: out of range.
    let mut b = bytes.to_vec();
    b[entry0..entry0 + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(SlpyReader::open(&b), Err(SlpyError::Corrupt(_))));

    // Entry 1 falsely flagged keyframe: FIDX/FRAM flag cross-check fires at
    // decode (open never touches FRAM payloads).
    let mut b = bytes.to_vec();
    let entry1 = entry0 + 16;
    b[entry1 + 12] = frame_flags::KEYFRAME;
    let mut reader = SlpyReader::open(&b).unwrap();
    let mut y = vec![0u8; Y_LEN];
    assert!(matches!(
        reader.decode_plane_into(1, plane_id::Y, &mut y),
        Err(SlpyError::Corrupt(_))
    ));
}

#[test]
fn hostile_fram_size_and_meta_offset_are_clean_errors() {
    // FRAM chunk size -> u64::MAX: decode must fail cleanly, not overflow.
    let (fram_off, _) = find_chunk(delta_asset(), TAG_FRAM);
    let mut b = delta_asset().to_vec();
    b[fram_off + 8..fram_off + 16].copy_from_slice(&u64::MAX.to_le_bytes());
    let mut reader = SlpyReader::open(&b).unwrap(); // open never touches FRAM
    let mut y = vec![0u8; Y_LEN];
    assert!(reader.decode_plane_into(0, plane_id::Y, &mut y).is_err());

    // meta_offset -> u64::MAX: meta() must fail cleanly, not overflow.
    let mut b = delta_asset().to_vec();
    b[52..60].copy_from_slice(&u64::MAX.to_le_bytes());
    let reader = SlpyReader::open(&b);
    match reader {
        Ok(r) => assert!(matches!(r.meta(), Err(SlpyError::Corrupt(_)))),
        Err(e) => assert!(matches!(e, SlpyError::Corrupt(_))),
    }
}
