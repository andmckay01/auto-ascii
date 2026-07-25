//! End-to-end player tests against the real binary via `--sim` (the M0
//! headless acceptance path — this box has no TTY/kitty; PLAN §7).
//!
//! A tiny synthetic SLPY asset is written with `SlpyWriter`, then the
//! `sleepy-player` binary is driven with `CARGO_BIN_EXE_sleepy-player`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use slpy_format::{Meta, PlaneRef, SlpyWriter, WriterOptions, header::plane_id};

/// Self-cleaning temp file (no tempfile dep — pinned workspace dep set).
struct TmpFile(PathBuf);

impl TmpFile {
    fn new(name: &str) -> TmpFile {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "sleepy-player-test-{}-{name}",
            std::process::id()
        ));
        TmpFile(p)
    }
}

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Write a small synthetic luma-only asset: `frames` frames of a moving
/// gradient at 480x270 (fast zstd level — determinism is slpy-format's test
/// concern, not this one's).
fn write_test_asset(path: &PathBuf, frames: u32) {
    let opts = WriterOptions { zstd_level: 3, ..WriterOptions::default() };
    let (w, h) = (opts.base_w as usize, opts.base_h as usize);
    let meta = Meta {
        factory_version: "test".into(),
        source: "synthetic".into(),
        palette_hints: vec![],
    };
    let file = fs::File::create(path).unwrap();
    let mut writer = SlpyWriter::new(std::io::BufWriter::new(file), opts, &meta).unwrap();
    let mut plane = vec![0u8; w * h];
    for f in 0..frames {
        for (i, px) in plane.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            *px = ((x + y + 7 * f as usize) & 0xff) as u8;
        }
        writer
            .write_frame(&[PlaneRef { id: plane_id::Y, data: &plane }])
            .unwrap();
    }
    writer.finish().unwrap().into_inner().unwrap();
}

fn run_player(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sleepy-player"))
        .args(args)
        .output()
        .expect("spawn sleepy-player");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Pull a bare (unquoted) JSON scalar out of the one-line stats report.
fn json_field<'a>(json: &'a str, key: &str) -> &'a str {
    let pat = format!("\"{key}\":");
    let start = json.find(&pat).unwrap_or_else(|| panic!("no {key} in {json}")) + pat.len();
    let rest = &json[start..];
    let end = rest
        .find([',', '}'])
        .unwrap_or_else(|| panic!("unterminated {key} in {json}"));
    rest[..end].trim_matches('"')
}

#[test]
fn sim_renders_all_frames_and_reports_stats() {
    let asset = TmpFile::new("stats.slpy");
    write_test_asset(&asset.0, 12);

    let (ok, stdout, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--sim",
        "80x24:30", // 30 > 12 frames: wraps modulo frame_count
    ]);
    assert!(ok, "player failed: {stderr}");
    let line = stdout.lines().last().unwrap();
    assert_eq!(json_field(line, "frames"), "30");
    assert_eq!(json_field(line, "grid_after"), "80x24");
    assert!(json_field(line, "fps").parse::<f64>().unwrap() > 0.0);
    assert!(json_field(line, "bytes_total").parse::<u64>().unwrap() > 0);
    assert!(json_field(line, "avg_bytes_per_frame").parse::<f64>().unwrap() > 0.0);
    // stage_ms block present with all four stages
    for stage in ["decode", "resample", "compose", "present"] {
        assert!(
            json_field(line, stage).parse::<f64>().unwrap() >= 0.0,
            "missing stage {stage}: {line}"
        );
    }
    // M3: winning-layer counts (the §3.4 priority decision, observable
    // headlessly). Y-only asset: every rendered cell is base or sub-cell
    // structure; edge/highlight/shadow must be zero (auto-disabled planes).
    let mut layer_total = 0u64;
    for layer in ["base", "edge", "highlight", "shadow", "structure"] {
        layer_total += json_field(line, layer).parse::<u64>().unwrap();
    }
    assert_eq!(layer_total, 30 * 80 * 24, "layer counts must cover every cell");
    assert_eq!(json_field(line, "edge"), "0", "Y-only asset cannot compose edges");
    assert_eq!(json_field(line, "highlight"), "0");
    assert_eq!(json_field(line, "shadow"), "0");
}

#[test]
fn sim_resize_reflows_mid_run() {
    let asset = TmpFile::new("resize.slpy");
    write_test_asset(&asset.0, 8);

    // Explicit resize target.
    let (ok, stdout, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--sim",
        "213x58:20",
        "--sim-resize",
        "320x90",
    ]);
    assert!(ok, "player failed: {stderr}");
    let line = stdout.lines().last().unwrap();
    assert_eq!(json_field(line, "frames"), "20");
    assert_eq!(json_field(line, "grid_after"), "320x90");

    // Flag with no value uses the default (100x40).
    let (ok, stdout, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--sim",
        "80x24:10",
        "--sim-resize",
    ]);
    assert!(ok, "player failed: {stderr}");
    let line = stdout.lines().last().unwrap();
    assert_eq!(json_field(line, "grid_after"), "100x40");
}

#[test]
fn sim_resize_below_minimum_renders_card_without_panic() {
    let asset = TmpFile::new("tiny.slpy");
    write_test_asset(&asset.0, 4);

    let (ok, stdout, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--sim",
        "40x12:8",
        "--sim-resize",
        "20x5", // below MIN_COLS x MIN_ROWS -> "enlarge terminal" card path
    ]);
    assert!(ok, "player failed: {stderr}");
    let line = stdout.lines().last().unwrap();
    assert_eq!(json_field(line, "frames"), "8");
    assert_eq!(json_field(line, "grid_after"), "20x5");
}

#[test]
fn diff_repaint_mode_emits_fewer_bytes_on_static_content() {
    let asset = TmpFile::new("diff.slpy");
    // One unique frame rendered repeatedly (loop wraps modulo 1): after the
    // first paint, diff mode should emit ~0 bytes; full mode repaints.
    write_test_asset(&asset.0, 1);

    let run = |mode: &str| -> u64 {
        let (ok, stdout, stderr) = run_player(&[
            asset.0.to_str().unwrap(),
            "--sim",
            "80x24:10",
            "--repaint",
            mode,
        ]);
        assert!(ok, "player failed: {stderr}");
        json_field(stdout.lines().last().unwrap(), "bytes_total")
            .parse()
            .unwrap()
    };
    let full = run("full");
    let diff = run("diff");
    assert!(
        diff < full / 2,
        "diff mode should emit far fewer bytes on a static clip: diff={diff} full={full}"
    );
}

#[test]
fn invalid_inputs_fail_cleanly() {
    // Missing asset.
    let (ok, _, stderr) = run_player(&["/nonexistent/nope.slpy", "--sim", "80x24:1"]);
    assert!(!ok);
    assert!(stderr.contains("opening"), "unexpected stderr: {stderr}");

    // Not a SLPY file.
    let junk = TmpFile::new("junk.slpy");
    fs::write(&junk.0, b"definitely not a slpy asset, but long enough to mmap")
        .unwrap();
    let (ok, _, stderr) = run_player(&[junk.0.to_str().unwrap(), "--sim", "80x24:1"]);
    assert!(!ok);
    assert!(stderr.contains("not a valid SLPY asset"), "unexpected stderr: {stderr}");

    // Bad --sim spec.
    let asset = TmpFile::new("spec.slpy");
    write_test_asset(&asset.0, 1);
    let (ok, _, _) = run_player(&[asset.0.to_str().unwrap(), "--sim", "80x24"]);
    assert!(!ok);
}
