//! End-to-end factory tests (task spec): synthesize a tiny input with
//! `ffmpeg -f lavfi -i testsrc2=...`, run the real `sleepy-factory build`
//! binary, then open the output with `slpy_format::SlpyReader` and assert
//! header fields, frame count, CRC pass, normalization spread, and
//! byte-determinism (M0 acceptance 4). This box guarantees ffmpeg on PATH.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use slpy_format::{SlpyReader, codec, filter, header_flags, plane_id};

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

#[test]
fn full_build_roundtrip_and_determinism() {
    let dir = TempDir::new("roundtrip");
    let input = synth_input(&dir);
    let out_path = dir.path("out.slpy");

    let out = build(&input, &out_path);
    assert!(out.status.success(), "build failed:\n{}", stderr_of(&out));

    let bytes = std::fs::read(&out_path).unwrap();
    let mut reader = SlpyReader::open(&bytes).expect("built asset must open");

    // Header fields (PLAN §4 layout, M0 SLPY-lite profile).
    let h = reader.header().clone();
    assert_eq!((h.version_major, h.version_minor), (1, 0));
    assert_eq!((h.fps_num, h.fps_den), (10, 1));
    assert_eq!((h.base_w, h.base_h), (480, 270), "default --res is 480x270");
    assert_eq!((h.aspect_num, h.aspect_den), (16, 9));
    assert_eq!(h.frame_count, 20, "2 s @ 10 fps must yield exactly 20 frames");
    assert_eq!(h.plane_count, 1);
    assert_eq!(h.plane_ids[0], plane_id::Y);
    assert_eq!(h.codec, codec::ZSTD);
    assert_eq!(h.filter, filter::INTRA);
    assert_ne!(h.flags & header_flags::INDEX_PRESENT, 0);
    assert_ne!(h.flags & header_flags::CRCS_PRESENT, 0);
    assert_eq!(reader.frame_count(), 20);

    // CRC pass over every chunk.
    reader.verify().expect("CRC walk must pass on a fresh asset");

    // Meta determinism rules: crate version + file NAME only.
    let meta = reader.meta().unwrap();
    assert_eq!(meta.factory_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(meta.source, "in.mp4");

    // Baked global p2/p98 normalization must spread the histogram: the 2%
    // tails saturate to 0/255, so the global min/max sit at the rails.
    let (w, h_px) = reader.plane_dims(plane_id::Y).unwrap();
    let mut plane = vec![0u8; w as usize * h_px as usize];
    let (mut min, mut max) = (u8::MAX, u8::MIN);
    for idx in 0..reader.frame_count() {
        let n = reader.decode_plane_into(idx, plane_id::Y, &mut plane).unwrap();
        assert_eq!(n, plane.len());
        for &v in &plane {
            min = min.min(v);
            max = max.max(v);
        }
    }
    assert!(min < 10, "normalized luma min {min} not < 10 (levels not baked?)");
    assert!(max > 245, "normalized luma max {max} not > 245 (levels not baked?)");

    // Byte-determinism (M0 acceptance 4): identical input twice → identical file.
    let out2_path = dir.path("out2.slpy");
    let out2 = build(&input, &out2_path);
    assert!(out2.status.success(), "second build failed:\n{}", stderr_of(&out2));
    let bytes2 = std::fs::read(&out2_path).unwrap();
    assert!(bytes == bytes2, "two builds of identical input differ (byte-determinism broken)");

    // inspect smoke: succeeds and reports the CRC walk on stdout.
    let ins = factory(&[&"inspect", &out_path]);
    assert!(ins.status.success(), "inspect failed:\n{}", stderr_of(&ins));
    let stdout = String::from_utf8_lossy(&ins.stdout);
    assert!(stdout.contains("frames:       20"), "inspect stdout:\n{stdout}");
    assert!(stdout.contains("integrity:    OK"), "inspect stdout:\n{stdout}");
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
