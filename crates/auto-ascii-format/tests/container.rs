//! Container acceptance tests (PLAN §4/§6/§7): byte-golden determinism
//! with a committed hash, roundtrip, corrupt-CRC detection, truncated-file
//! detection, forward-compat chunk semantics. The default profile under
//! test is the M1 one (temporal delta, keyframe 60); M1-specific behavior
//! (seek, NORM, chroma, hostile inputs) lives in `tests/m1_format.rs`.

use std::io::Cursor;
use std::sync::OnceLock;

use auto_ascii_format::{
    CHUNK_HEADER_SIZE, ChunkHeader, Meta, PlaneRef, AsciiError, AsciiReader, AsciiWriter,
    TAG_FIDX, TAG_FRAM, TAG_META, TAG_TRLR, WriterOptions, chunk_flags, codec, filter,
    header_flags, plane_id,
};

// ---------------------------------------------------------------------------
// Minimal SHA-256 (FIPS 180-4) — test-only, so the crate keeps its PLAN §8
// dependency set (zstd/crc32fast/ciborium/serde only). Verified against
// known vectors below.
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (wi, word) in w.iter_mut().zip(block.chunks_exact(4)) {
            *wi = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (&k, &wi) in K.iter().zip(w.iter()) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k)
                .wrapping_add(wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (hs, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *hs = hs.wrapping_add(v);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[test]
fn sha256_known_vectors() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

// ---------------------------------------------------------------------------
// Synthetic generator (PLAN §6: the deterministic synthetic plane generator
// is the regression baseline): gradient + moving checkerboard, 480×270,
// pure integer math.
// ---------------------------------------------------------------------------

const W: usize = 480;
const H: usize = 270;
const FRAMES: u32 = 6;

fn synth_frame(f: u32) -> Vec<u8> {
    let mut plane = vec![0u8; W * H];
    for (y, row) in plane.chunks_exact_mut(W).enumerate() {
        for (x, px) in row.iter_mut().enumerate() {
            let grad = ((x * 255) / (W - 1)) as u8;
            let vgrad = ((y * 128) / (H - 1)) as u8;
            let checker =
                if ((x / 16) + (y / 16) + f as usize).is_multiple_of(2) { 40u8 } else { 0u8 };
            *px = grad
                .wrapping_add(vgrad)
                .wrapping_add(checker)
                .wrapping_add((f * 7) as u8);
        }
    }
    plane
}

fn test_meta() -> Meta {
    Meta {
        factory_version: "auto-ascii-format-test-0.1.0".to_string(),
        source: "synthetic-gradient-checkerboard".to_string(),
        palette_hints: Vec::new(),
    }
}

fn encode_default() -> Vec<u8> {
    let writer =
        AsciiWriter::new(Cursor::new(Vec::new()), WriterOptions::default(), &test_meta()).unwrap();
    let mut writer = writer;
    for f in 0..FRAMES {
        let plane = synth_frame(f);
        writer
            .write_frame(&[PlaneRef { id: plane_id::Y, data: &plane }])
            .unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// Encoded once, shared by the read-side tests (zstd-19 × 6 frames is the
/// expensive part).
fn golden_bytes() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(encode_default)
}

/// Walk chunks with the public codec types (integration tests can't see the
/// reader's internals). Assumes CRCs present (the default profile).
fn find_chunk(bytes: &[u8], tag: [u8; 4]) -> (usize, ChunkHeader) {
    let mut pos = 64usize;
    while pos < bytes.len() {
        let ch =
            ChunkHeader::from_bytes(bytes[pos..pos + CHUNK_HEADER_SIZE].try_into().unwrap());
        if ch.tag == tag {
            return (pos, ch);
        }
        pos += CHUNK_HEADER_SIZE + ch.size as usize + 4;
    }
    panic!("chunk {:?} not found", String::from_utf8_lossy(&tag));
}

// ---------------------------------------------------------------------------
// (4) M0 acceptance: byte-golden determinism + committed hash.
// ---------------------------------------------------------------------------

/// Committed golden (M0 acceptance (4), re-baselined at M1 — deliberate
/// format change: version_minor 0→1 and the default profile is now temporal
/// delta with keyframe interval 60). Depends on the synthetic input, the
/// frozen wire layout, and pinned zstd (0.13.3 / libzstd 1.5.7) at level 19.
/// If it moves without a deliberate format change, the writer leaked
/// nondeterminism — do not just re-commit the hash.
///
/// RE-PINNED 2026-09-19 (slpy/sleepy -> auto-ascii rename), previous value
/// bdccde10…. Three deliberate causes, and the old and new goldens were
/// diffed chunk-by-chunk to prove there is no fourth:
///   - magic `SLPY` -> `ASCI` (4 B) and TRLR payload `SLPY_END` -> `ASCI_END`;
///   - this test's own META `factory_version` label, `slpy-format-test-0.1.0`
///     -> `auto-ascii-format-test-0.1.0`: +6 chars, and +1 more because CBOR
///     needs a 2-byte length header past 23 chars, so META grows 95 -> 102 B;
///   - every FIDX frame offset and the header's index_offset therefore shift
///     by exactly +7. Verified: all six shift by +7, none by anything else.
///
/// All six FRAM payloads — the compressed plane bytes — are BYTE-IDENTICAL,
/// same sizes, same contents. The writer and zstd are untouched; only the
/// container's identity and this fixture's own label moved.
const GOLDEN_SHA256: &str = "367528656d60fe4b2e2dbcc8238b4b2dd8703fd54462b4e5df5150e192dea2e5";

#[test]
fn golden_encode_twice_is_byte_identical_and_hash_committed() {
    let first = golden_bytes();
    let second = encode_default();
    assert_eq!(first, &second[..], "writer output differs between identical runs");
    assert_eq!(
        sha256_hex(first),
        GOLDEN_SHA256,
        "ASCI byte golden moved (len = {})",
        first.len()
    );
}

// ---------------------------------------------------------------------------
// Roundtrip: header fields, META, every frame decodes to the source plane.
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_header_meta_planes() {
    let bytes = golden_bytes();
    let mut reader = AsciiReader::open(bytes).unwrap();

    let h = reader.header();
    assert_eq!(h.version_major, 1);
    assert_eq!(h.flags, header_flags::INDEX_PRESENT | header_flags::CRCS_PRESENT);
    assert_eq!((h.fps_num, h.fps_den), (30, 1));
    assert_eq!((h.base_w, h.base_h), (480, 270));
    assert_eq!((h.aspect_num, h.aspect_den), (16, 9));
    assert_eq!(h.frame_count, FRAMES);
    assert_eq!(h.plane_count, 1);
    assert_eq!(h.codec, codec::ZSTD);
    assert_eq!(h.filter, filter::TEMPORAL_DELTA); // M1 default profile
    assert_eq!(h.keyframe_ivl, 60);
    assert_eq!(h.plane_ids, [plane_id::Y, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(reader.frame_count(), FRAMES);

    assert_eq!(reader.meta().unwrap(), test_meta());
    assert_eq!(reader.plane_dims(plane_id::Y), Some((480, 270)));
    assert_eq!(reader.plane_dims(plane_id::E), None);

    let mut dst = vec![0u8; W * H];
    for f in 0..FRAMES {
        let n = reader.decode_plane_into(f, plane_id::Y, &mut dst).unwrap();
        assert_eq!(n, W * H);
        assert_eq!(dst, synth_frame(f), "frame {f} decoded != source");
    }

    reader.verify().unwrap();
}

#[test]
fn no_crc_profile_roundtrips() {
    let opts = WriterOptions { with_crc: false, ..WriterOptions::default() };
    let mut writer = AsciiWriter::new(Cursor::new(Vec::new()), opts, &test_meta()).unwrap();
    let plane = synth_frame(0);
    writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &plane }]).unwrap();
    let bytes = writer.finish().unwrap().into_inner();

    let mut reader = AsciiReader::open(&bytes).unwrap();
    assert_eq!(reader.header().flags & header_flags::CRCS_PRESENT, 0);
    let mut dst = vec![0u8; W * H];
    reader.decode_plane_into(0, plane_id::Y, &mut dst).unwrap();
    assert_eq!(dst, plane);
    reader.verify().unwrap(); // structural walk still runs without CRCs
}

// ---------------------------------------------------------------------------
// Corruption: CRC mismatch is detected by verify().
// ---------------------------------------------------------------------------

#[test]
fn corrupt_fram_payload_fails_crc_verify() {
    let mut bytes = golden_bytes().to_vec();
    let (off, ch) = find_chunk(&bytes, TAG_FRAM);
    assert!(ch.is_required());
    // Flip a byte in the middle of the FRAM payload (compressed data).
    let target = off + CHUNK_HEADER_SIZE + ch.size as usize / 2;
    bytes[target] ^= 0xFF;

    // open() is the cheap structural pass — it does not hash payloads.
    let reader = AsciiReader::open(&bytes).unwrap();
    match reader.verify() {
        Err(AsciiError::CrcMismatch { tag }) => assert_eq!(tag, TAG_FRAM),
        other => panic!("expected CrcMismatch(FRAM), got {other:?}"),
    }
}

#[test]
fn corrupt_meta_payload_fails_crc_verify() {
    let mut bytes = golden_bytes().to_vec();
    let (off, _) = find_chunk(&bytes, TAG_META);
    bytes[off + CHUNK_HEADER_SIZE] ^= 0x01;
    let reader = AsciiReader::open(&bytes).unwrap();
    match reader.verify() {
        Err(AsciiError::CrcMismatch { tag }) => assert_eq!(tag, TAG_META),
        other => panic!("expected CrcMismatch(META), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Truncation: missing TRLR ⇒ Truncated ⇒ factory rerun (PLAN §4).
// ---------------------------------------------------------------------------

#[test]
fn missing_trlr_is_truncated() {
    let bytes = golden_bytes();
    let (trlr_off, _) = find_chunk(bytes, TAG_TRLR);
    // Cut the whole TRLR chunk (header + "ASCI_END" + crc).
    let cut = &bytes[..trlr_off];
    assert!(matches!(AsciiReader::open(cut), Err(AsciiError::Truncated)));
}

#[test]
fn mid_chunk_truncation_is_truncated() {
    let bytes = golden_bytes();
    assert!(matches!(
        AsciiReader::open(&bytes[..bytes.len() - 3]),
        Err(AsciiError::Truncated)
    ));
    // Also: cut inside the FRAM stream (before FIDX).
    let (fidx_off, _) = find_chunk(bytes, TAG_FIDX);
    assert!(matches!(
        AsciiReader::open(&bytes[..fidx_off + 5]),
        Err(AsciiError::Truncated)
    ));
}

// ---------------------------------------------------------------------------
// Forward compat (PLAN §4): unknown required = hard error; unknown
// non-required = skipped by size. Major version bump = rejected.
// ---------------------------------------------------------------------------

#[test]
fn unknown_required_chunk_is_rejected() {
    let mut bytes = golden_bytes().to_vec();
    let (off, _) = find_chunk(&bytes, TAG_META);
    bytes[off..off + 4].copy_from_slice(b"XREQ");
    bytes[off + 4] = chunk_flags::REQUIRED;
    match AsciiReader::open(&bytes) {
        Err(AsciiError::UnknownRequiredChunk(tag)) => assert_eq!(&tag, b"XREQ"),
        Err(other) => panic!("expected UnknownRequiredChunk, got {other:?}"),
        Ok(_) => panic!("expected UnknownRequiredChunk, got Ok"),
    }
}

#[test]
fn unknown_optional_chunk_is_skipped() {
    let mut bytes = golden_bytes().to_vec();
    let (off, _) = find_chunk(&bytes, TAG_META);
    bytes[off..off + 4].copy_from_slice(b"XOPT");
    bytes[off + 4] = 0;
    // Open succeeds (chunk skipped by size); frames still decode.
    let mut reader = AsciiReader::open(&bytes).unwrap();
    let mut dst = vec![0u8; W * H];
    reader.decode_plane_into(0, plane_id::Y, &mut dst).unwrap();
    assert_eq!(dst, synth_frame(0));
    // meta_offset now points at a non-META chunk — surfaced on demand.
    assert!(matches!(reader.meta(), Err(AsciiError::Corrupt(_))));
}

#[test]
fn future_major_version_is_rejected() {
    let mut bytes = golden_bytes().to_vec();
    bytes[4..6].copy_from_slice(&2u16.to_le_bytes());
    assert!(matches!(
        AsciiReader::open(&bytes),
        Err(AsciiError::UnsupportedVersion { found: 2, supported: 1 })
    ));
}

// ---------------------------------------------------------------------------
// API misuse errors.
// ---------------------------------------------------------------------------

#[test]
fn bad_frame_index_and_bad_plane_id() {
    let mut reader = AsciiReader::open(golden_bytes()).unwrap();
    let mut dst = vec![0u8; W * H];
    assert!(matches!(
        reader.decode_plane_into(FRAMES, plane_id::Y, &mut dst),
        Err(AsciiError::BadFrameIndex(f)) if f == FRAMES
    ));
    assert!(matches!(
        reader.decode_plane_into(0, plane_id::E, &mut dst),
        Err(AsciiError::BadPlaneId(id)) if id == plane_id::E
    ));
}

#[test]
fn writer_rejects_mismatched_planes() {
    let mut writer =
        AsciiWriter::new(Cursor::new(Vec::new()), WriterOptions::default(), &test_meta()).unwrap();
    let plane = synth_frame(0);
    // Wrong id.
    assert!(writer.write_frame(&[PlaneRef { id: plane_id::E, data: &plane }]).is_err());
    // Wrong length.
    assert!(writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &plane[..100] }]).is_err());
    // Wrong count.
    assert!(writer.write_frame(&[]).is_err());
    // Still usable after rejected inputs.
    writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &plane }]).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(AsciiReader::open(&bytes).unwrap().frame_count(), 1);
}

#[test]
fn writer_rejects_bad_options() {
    let cases = [
        WriterOptions { plane_ids: vec![], ..WriterOptions::default() },
        WriterOptions { plane_ids: vec![plane_id::Y, plane_id::Y], ..WriterOptions::default() },
        WriterOptions { plane_ids: vec![0], ..WriterOptions::default() },
        // Unknown plane id: the writer has no geometry for it (M1).
        WriterOptions { plane_ids: vec![200], ..WriterOptions::default() },
        WriterOptions { codec: codec::LZ4, ..WriterOptions::default() },
        WriterOptions { filter: 2, ..WriterOptions::default() },
        WriterOptions { keyframe_ivl: 0, ..WriterOptions::default() },
        WriterOptions { base_w: 0, ..WriterOptions::default() },
        WriterOptions { base_h: 0, ..WriterOptions::default() },
        // M2 review fix 1: base dims must be even and >= 2 — odd/degenerate
        // dims give the C plane a zero dimension (base_w == 1 → C width 0)
        // and panicked the player's resampler.
        WriterOptions { base_w: 1, ..WriterOptions::default() },
        WriterOptions { base_h: 1, ..WriterOptions::default() },
        WriterOptions { base_w: 479, ..WriterOptions::default() },
        WriterOptions { base_h: 269, ..WriterOptions::default() },
        // M0 adversarial-review regression: zero fps must be rejected at the
        // source (player Duration::from_secs_f64(1/0.0) panic).
        WriterOptions { fps_num: 0, ..WriterOptions::default() },
        WriterOptions { fps_den: 0, ..WriterOptions::default() },
    ];
    for opts in cases {
        assert!(
            AsciiWriter::new(Cursor::new(Vec::new()), opts.clone(), &test_meta()).is_err(),
            "options accepted but should be rejected: {opts:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Wire-freeze spot checks: subblock 64-B alignment and FIDX placement.
// ---------------------------------------------------------------------------

#[test]
fn fram_payload_is_subblock_aligned_and_indexed() {
    let bytes = golden_bytes();
    let (off, ch) = find_chunk(bytes, TAG_FRAM);
    let payload = &bytes[off + CHUNK_HEADER_SIZE..off + CHUNK_HEADER_SIZE + ch.size as usize];
    // frame_idx 0, keyframe flag set.
    assert_eq!(&payload[..4], &[0, 0, 0, 0]);
    assert_eq!(payload[4], 1);
    // Single Y subblock, padded to a 64-B multiple.
    let comp_size = u32::from_le_bytes(payload[6..10].try_into().unwrap()) as usize;
    let raw_size = u32::from_le_bytes(payload[10..14].try_into().unwrap()) as usize;
    assert_eq!(payload[5], plane_id::Y);
    assert_eq!(raw_size, W * H);
    let sub = 9 + comp_size;
    let padded = sub.div_ceil(64) * 64;
    assert_eq!(payload.len(), 5 + padded, "subblock not padded to 64-B alignment");
    assert!(payload[5 + sub..].iter().all(|&b| b == 0), "pad bytes must be zero");

    // header.index_offset points at the FIDX chunk header.
    let (fidx_off, fidx_ch) = find_chunk(bytes, TAG_FIDX);
    let index_offset = u64::from_le_bytes(bytes[44..52].try_into().unwrap());
    assert_eq!(index_offset, fidx_off as u64);
    assert_eq!(fidx_ch.size, u64::from(FRAMES) * 16);
    // First FIDX row points back at the first FRAM chunk.
    let row = &bytes[fidx_off + CHUNK_HEADER_SIZE..fidx_off + CHUNK_HEADER_SIZE + 16];
    assert_eq!(u64::from_le_bytes(row[..8].try_into().unwrap()), off as u64);
}
