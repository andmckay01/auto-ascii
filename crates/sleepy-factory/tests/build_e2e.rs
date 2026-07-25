//! End-to-end factory tests (M1): synthesize tiny inputs with
//! `ffmpeg -f lavfi -i testsrc2=...`, run the real `sleepy-factory build`
//! binary, then open the output with `slpy_format::SlpyReader` and assert
//! the SLPY v1 profile (Y+C planes, temporal delta + keyframes, NORM
//! per-shot levels measured but NOT baked into the planes), CRC pass,
//! shot/cut detection on a two-scene concat, and byte-determinism.
//! This box guarantees ffmpeg on PATH.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use slpy_format::{PlaneLevels, SlpyReader, codec, filter, header_flags, plane_id};

/// Self-cleaning per-test scratch dir (no tempfile dep in the workspace).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!("sleepy-factory-e2e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 2 s / 10 fps / 320x180 synthetic clip → exactly 20 frames at --fps 10.
fn synth_input(dir: &TempDir) -> PathBuf {
    let input = dir.path("in.mp4");
    let status = Command::new("ffmpeg")
        .args(["-y", "-nostdin", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=duration=2:size=320x180:rate=10")
        .args(["-pix_fmt", "yuv420p"])
        .arg(&input)
        .status()
        .expect("ffmpeg must be installed for factory e2e tests");
    assert!(status.success(), "ffmpeg lavfi synthesis failed");
    input
}

fn factory(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sleepy-factory"));
    for a in args {
        cmd.arg(a.as_ref());
    }
    cmd.output().expect("failed to run sleepy-factory binary")
}

fn build(input: &Path, output: &Path) -> Output {
    factory(&[&"build", &input, &"-o", &output, &"--fps", &"10"])
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Nearest-rank p2/p98 over a pooled 256-bin histogram — the same integer
/// formula as the factory's `lut::percentile_levels` (duplicated here because
/// `sleepy-factory` is a bin crate; keep the two in sync).
fn percentile_levels(hist: &[u64; 256]) -> PlaneLevels {
    let total: u64 = hist.iter().sum();
    assert!(total > 0);
    let rank = |pct: u64| -> u64 { (total * pct).div_ceil(100).max(1) };
    let value_at = |target: u64| -> u8 {
        let mut cum = 0u64;
        for (v, &count) in hist.iter().enumerate() {
            cum += count;
            if cum >= target {
                return v as u8;
            }
        }
        255
    };
    PlaneLevels { p2: value_at(rank(2)), p98: value_at(rank(98)) }
}

/// Decode Y sequentially over `frames` (delta assets need the standing
/// double buffer), pooling a histogram of the decoded (stored, unstretched)
/// bytes.
fn pooled_y_levels(
    reader: &mut SlpyReader<'_>,
    frames: std::ops::Range<u32>,
) -> PlaneLevels {
    let (w, h) = reader.plane_dims(plane_id::Y).unwrap();
    let mut plane = vec![0u8; w as usize * h as usize];
    let mut hist = [0u64; 256];
    // Roll from frame 0 so every delta frame sees its predecessor.
    for idx in 0..frames.end {
        let n = reader.decode_plane_into(idx, plane_id::Y, &mut plane).unwrap();
        assert_eq!(n, plane.len());
        if idx >= frames.start {
            for &v in &plane {
                hist[v as usize] += 1;
            }
        }
    }
    percentile_levels(&hist)
}

#[test]
fn full_build_roundtrip_and_determinism() {
    let dir = TempDir::new("roundtrip");
    let input = synth_input(&dir);
    let out_path = dir.path("out.slpy");

    let out = build(&input, &out_path);
    assert!(out.status.success(), "build failed:\n{}", stderr_of(&out));

    let bytes = std::fs::read(&out_path).unwrap();
    let mut reader = SlpyReader::open(&bytes).expect("built asset must open");

    // Header fields (PLAN §4 layout, M1 SLPY v1 profile).
    let h = reader.header().clone();
    // Minor 1 since the M1 slpy-format upgrade (additive, PLAN §4).
    assert_eq!((h.version_major, h.version_minor), (1, 1));
    assert_eq!((h.fps_num, h.fps_den), (10, 1));
    assert_eq!((h.base_w, h.base_h), (480, 270), "default --res is 480x270");
    assert_eq!((h.aspect_num, h.aspect_den), (16, 9));
    assert_eq!(h.frame_count, 20, "2 s @ 10 fps must yield exactly 20 frames");
    assert_eq!(h.plane_count, 2, "M1 emits Y + C");
    assert_eq!(h.plane_ids[0], plane_id::Y);
    assert_eq!(h.plane_ids[1], plane_id::C);
    assert_eq!(h.codec, codec::ZSTD);
    assert_eq!(h.filter, filter::TEMPORAL_DELTA, "M1 profile is delta+keyframes");
    assert_eq!(h.keyframe_ivl, 60);
    assert_ne!(h.flags & header_flags::INDEX_PRESENT, 0);
    assert_ne!(h.flags & header_flags::CRCS_PRESENT, 0);
    assert_eq!(reader.frame_count(), 20);
    assert!(reader.is_keyframe(0).unwrap(), "frame 0 of a delta asset is a keyframe");

    // CRC pass over every chunk.
    reader.verify().expect("CRC walk must pass on a fresh asset");

    // Meta determinism rules: crate version + file NAME only.
    let meta = reader.meta().unwrap();
    assert_eq!(meta.factory_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(meta.source, "in.mp4");

    // NORM present: continuous testsrc2 is exactly one shot, no cut flag.
    let shots = reader.shots().to_vec();
    assert_eq!(shots.len(), 1, "continuous testsrc2 must be a single shot");
    assert_eq!(shots[0].first_frame, 0);
    assert!(!shots[0].is_cut(), "shot 0 never carries a cut flag");

    // Levels are MEASURED into NORM, not baked into the plane: the stored
    // NORM p2/p98 must equal the percentiles of the decoded (raw L*) bytes.
    let stored = reader.norm_levels(0, plane_id::Y).expect("Y levels in NORM");
    let measured = pooled_y_levels(&mut reader, 0..20);
    assert_eq!(
        (stored.p2, stored.p98),
        (measured.p2, measured.p98),
        "NORM Y levels must match percentiles of the stored (unstretched) plane"
    );
    assert!(stored.p2 <= stored.p98);

    // Chroma plane C: half res, RGB565 (2 B/px), non-trivial content.
    let (cw, ch) = reader.plane_dims(plane_id::C).unwrap();
    assert_eq!((cw, ch), (240, 135), "C is half of 480x270");
    let mut c_plane = vec![0u8; cw as usize * ch as usize * 2];
    let n = reader.seek_plane_into(0, plane_id::C, &mut c_plane).unwrap();
    assert_eq!(n, c_plane.len());
    assert!(
        c_plane.iter().any(|&b| b != 0),
        "testsrc2 chroma must not be all zero"
    );

    // Seek lands byte-identical to the sequential roll (delta asset).
    let (w, h_px) = reader.plane_dims(plane_id::Y).unwrap();
    let mut seq = vec![0u8; w as usize * h_px as usize];
    for idx in 0..=13 {
        reader.decode_plane_into(idx, plane_id::Y, &mut seq).unwrap();
    }
    let mut sought = vec![0xAAu8; seq.len()]; // garbage prefill: dst must not matter
    reader.seek_plane_into(13, plane_id::Y, &mut sought).unwrap();
    assert_eq!(seq, sought, "seek_plane_into(13) must equal the sequential decode");

    // Byte-determinism (M0 acceptance 4): identical input twice → identical file.
    let out2_path = dir.path("out2.slpy");
    let out2 = build(&input, &out2_path);
    assert!(out2.status.success(), "second build failed:\n{}", stderr_of(&out2));
    let bytes2 = std::fs::read(&out2_path).unwrap();
    assert!(bytes == bytes2, "two builds of identical input differ (byte-determinism broken)");

    // inspect smoke: succeeds and reports the M1 additions on stdout.
    let ins = factory(&[&"inspect", &out_path]);
    assert!(ins.status.success(), "inspect failed:\n{}", stderr_of(&ins));
    let stdout = String::from_utf8_lossy(&ins.stdout);
    assert!(stdout.contains("frames:       20"), "inspect stdout:\n{stdout}");
    assert!(stdout.contains("planes:       2 [Y, C]"), "inspect stdout:\n{stdout}");
    assert!(stdout.contains("shots:        1 (0 cut-flagged)"), "inspect stdout:\n{stdout}");
    assert!(stdout.contains("keyframes:    1 of 20 frames"), "inspect stdout:\n{stdout}");
    assert!(stdout.contains("plane Y"), "inspect stdout:\n{stdout}");
    assert!(stdout.contains("plane C"), "inspect stdout:\n{stdout}");
    assert!(stdout.contains("compression:"), "inspect stdout:\n{stdout}");
    assert!(stdout.contains("integrity:    OK"), "inspect stdout:\n{stdout}");
}

/// Two-scene lavfi concat (testsrc2 | negate+darkened testsrc2) at 10 fps,
/// 15 frames per scene: shot detection must split at frame 15 with a cut
/// flag, and each shot's NORM levels must equal the percentiles of its own
/// decoded (unstretched) frames — the "no baked normalization, no cross-shot
/// pumping" contract (PLAN §5 stages 2+5, M1 acceptance 3).
#[test]
fn two_scene_concat_detects_cut_with_per_shot_levels() {
    let dir = TempDir::new("twoscene");
    let input = dir.path("two.mp4");
    let status = Command::new("ffmpeg")
        .args(["-y", "-nostdin", "-v", "error"])
        .args(["-f", "lavfi", "-i", "testsrc2=duration=1.5:size=320x180:rate=10"])
        .args(["-f", "lavfi", "-i", "testsrc2=duration=1.5:size=320x180:rate=10"])
        .args(["-filter_complex", "[1:v]negate,eq=brightness=-0.3[b];[0:v][b]concat=n=2:v=1:a=0[out]"])
        .args(["-map", "[out]", "-pix_fmt", "yuv420p"])
        .arg(&input)
        .status()
        .expect("ffmpeg must be installed for factory e2e tests");
    assert!(status.success(), "ffmpeg two-scene synthesis failed");

    let out_path = dir.path("two.slpy");
    let out = build(&input, &out_path);
    assert!(out.status.success(), "build failed:\n{}", stderr_of(&out));

    let bytes = std::fs::read(&out_path).unwrap();
    let mut reader = SlpyReader::open(&bytes).expect("built asset must open");
    reader.verify().expect("CRC walk must pass");
    assert_eq!(reader.frame_count(), 30);

    let shots = reader.shots().to_vec();
    assert_eq!(shots.len(), 2, "hard concat boundary must split into 2 shots");
    assert_eq!(shots[0].first_frame, 0);
    assert!(!shots[0].is_cut());
    assert_eq!(shots[1].first_frame, 15, "cut must land on the concat boundary");
    assert!(shots[1].is_cut(), "concat boundary must carry the CUT flag");
    assert_eq!(reader.shot_for_frame(14).unwrap().first_frame, 0);
    assert_eq!(reader.shot_for_frame(15).unwrap().first_frame, 15);

    // Per-shot levels: measured within each shot only, never baked. If the
    // factory stretched the planes, decoded percentiles would sit at the
    // rails and no longer match the stored (dark) scene-B levels.
    let lv_a = reader.norm_levels(0, plane_id::Y).unwrap();
    let lv_b = reader.norm_levels(15, plane_id::Y).unwrap();
    let got_a = pooled_y_levels(&mut reader, 0..15);
    let got_b = pooled_y_levels(&mut reader, 15..30);
    assert_eq!((lv_a.p2, lv_a.p98), (got_a.p2, got_a.p98), "shot A levels");
    assert_eq!((lv_b.p2, lv_b.p98), (got_b.p2, got_b.p98), "shot B levels");
    assert_ne!(
        (lv_a.p2, lv_a.p98),
        (lv_b.p2, lv_b.p98),
        "darkened scene B must get its own level pair (per-shot, not global)"
    );
}

#[test]
fn zero_frames_is_a_clean_error() {
    let dir = TempDir::new("zeroframes");
    let input = synth_input(&dir);
    let out_path = dir.path("out.slpy");

    // --ss far past the 2 s clip → ffmpeg decodes zero frames.
    let out = factory(&[&"build", &input, &"-o", &out_path, &"--fps", &"10", &"--ss", &"100"]);
    assert!(!out.status.success(), "zero-frame build must fail");
    assert!(
        stderr_of(&out).contains("zero frames"),
        "stderr should name the zero-frame case:\n{}",
        stderr_of(&out)
    );
    assert!(!out_path.exists(), "no output file may be left behind");
    assert!(!dir.path("out.slpy.part").exists(), "no .part temp file may be left behind");
}

#[test]
fn missing_input_fails_before_writing() {
    let dir = TempDir::new("missing");
    let out_path = dir.path("out.slpy");
    let out = factory(&[&"build", &dir.path("nope.mp4"), &"-o", &out_path]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("input not found"), "stderr:\n{}", stderr_of(&out));
    assert!(!out_path.exists());
}

#[test]
fn ss_and_t_trim_the_stream() {
    let dir = TempDir::new("trim");
    let input = synth_input(&dir);
    let out_path = dir.path("trim.slpy");

    // Take 1 s from the middle: 10 frames at 10 fps.
    let out = factory(&[
        &"build", &input, &"-o", &out_path, &"--fps", &"10", &"--ss", &"0.5", &"--t", &"1",
    ]);
    assert!(out.status.success(), "trimmed build failed:\n{}", stderr_of(&out));
    let bytes = std::fs::read(&out_path).unwrap();
    let reader = SlpyReader::open(&bytes).unwrap();
    assert_eq!(reader.frame_count(), 10, "-ss 0.5 -t 1 @10 fps must yield 10 frames");
    reader.verify().unwrap();
}

#[test]
fn custom_res_is_honored() {
    let dir = TempDir::new("res");
    let input = synth_input(&dir);
    let out_path = dir.path("res.slpy");

    let out = factory(&[
        &"build", &input, &"-o", &out_path, &"--fps", &"10", &"--res", &"320x180",
    ]);
    assert!(out.status.success(), "custom-res build failed:\n{}", stderr_of(&out));
    let bytes = std::fs::read(&out_path).unwrap();
    let mut reader = SlpyReader::open(&bytes).unwrap();
    assert_eq!((reader.header().base_w, reader.header().base_h), (320, 180));
    assert_eq!(reader.header().frame_count, 20);
    let mut plane = vec![0u8; 320 * 180];
    assert_eq!(reader.decode_plane_into(0, plane_id::Y, &mut plane).unwrap(), 320 * 180);
}
