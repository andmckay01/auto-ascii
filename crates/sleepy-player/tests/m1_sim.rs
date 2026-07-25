//! M1 player integration tests via the real binary (headless `--sim` path):
//! tier byte checks through `--sim-dump`, seek-vs-sequential byte identity,
//! runtime NORM application, chroma fg, and probe-related no-hang behavior.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use slpy_format::header::plane_id;
use slpy_format::{Meta, PlaneLevels, PlaneRef, ShotRecord, SlpyWriter, WriterOptions, norm_flags};

/// Self-cleaning temp file (no tempfile dep — pinned workspace dep set).
struct TmpFile(PathBuf);

impl TmpFile {
    fn new(name: &str) -> TmpFile {
        let mut p = std::env::temp_dir();
        p.push(format!("sleepy-player-m1-{}-{name}", std::process::id()));
        TmpFile(p)
    }
}

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn meta() -> Meta {
    Meta {
        factory_version: "test".into(),
        source: "synthetic".into(),
        palette_hints: vec![],
    }
}

/// Delta asset (keyframe interval 5) with a moving gradient — Y only.
fn write_delta_asset(path: &PathBuf, frames: u32) {
    let opts = WriterOptions { zstd_level: 3, keyframe_ivl: 5, ..WriterOptions::default() };
    let (w, h) = (opts.base_w as usize, opts.base_h as usize);
    let file = fs::File::create(path).unwrap();
    let mut writer = SlpyWriter::new(std::io::BufWriter::new(file), opts, &meta()).unwrap();
    let mut plane = vec![0u8; w * h];
    for f in 0..frames {
        for (i, px) in plane.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            *px = ((x + 2 * y + 11 * f as usize) & 0xff) as u8;
        }
        writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &plane }]).unwrap();
    }
    writer.finish().unwrap().into_inner().unwrap();
}

/// Y+C asset: constant luma 128, solid red chroma (RGB565 0xF800) — the fg
/// must come from C, not from the gray luma.
fn write_chroma_asset(path: &PathBuf, frames: u32) {
    let opts = WriterOptions {
        zstd_level: 3,
        plane_ids: vec![plane_id::Y, plane_id::C],
        ..WriterOptions::default()
    };
    let (w, h) = (opts.base_w as usize, opts.base_h as usize);
    let file = fs::File::create(path).unwrap();
    let mut writer = SlpyWriter::new(std::io::BufWriter::new(file), opts, &meta()).unwrap();
    let luma = vec![128u8; w * h];
    let chroma: Vec<u8> = 0xF800u16
        .to_le_bytes()
        .into_iter()
        .cycle()
        .take((w / 2) * (h / 2) * 2)
        .collect();
    for _ in 0..frames {
        writer
            .write_frame(&[
                PlaneRef { id: plane_id::Y, data: &luma },
                PlaneRef { id: plane_id::C, data: &chroma },
            ])
            .unwrap();
    }
    writer.finish().unwrap().into_inner().unwrap();
}

/// Two-shot NORM asset: constant luma 100 everywhere; shot 0 (frames 0–1)
/// levels (0,200) → n=128, shot 1 (frames 2–3, CUT) levels (0,100) → n=255.
/// Identity (no NORM) would give n=100 — a distinct glyph on the coarse ramp.
fn write_norm_asset(path: &PathBuf) {
    let opts = WriterOptions { zstd_level: 3, ..WriterOptions::default() };
    let (w, h) = (opts.base_w as usize, opts.base_h as usize);
    let file = fs::File::create(path).unwrap();
    let mut writer = SlpyWriter::new(std::io::BufWriter::new(file), opts, &meta()).unwrap();
    let mk = |first_frame: u32, flags: u8, p98: u8| {
        let mut levels = [PlaneLevels::default(); 8];
        levels[0] = PlaneLevels { p2: 0, p98 };
        ShotRecord { first_frame, flags, levels }
    };
    writer
        .write_norm(&[mk(0, 0, 200), mk(2, norm_flags::CUT, 100)])
        .unwrap();
    let luma = vec![100u8; w * h];
    for _ in 0..4 {
        writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &luma }]).unwrap();
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

/// Collect the parameter strings of every SGR (`ESC [ … m`) in a stream.
fn sgr_params(stream: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < stream.len() {
        if stream[i] == 0x1b && stream[i + 1] == b'[' {
            let mut j = i + 2;
            while j < stream.len() && !(0x40..=0x7e).contains(&stream[j]) {
                j += 1;
            }
            if j < stream.len() && stream[j] == b'm' {
                out.push(String::from_utf8_lossy(&stream[i + 2..j]).into_owned());
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Split a full-repaint dump into per-frame chunks: every frame's escape
/// stream starts at row 0 with `CUP 1;1` and CUP is only emitted at span
/// starts, so the delimiter appears exactly once per frame.
fn split_frames(stream: &[u8]) -> Vec<Vec<u8>> {
    let delim = b"\x1b[1;1H";
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut i = 0;
    let mut cur: Option<Vec<u8>> = None;
    while i < stream.len() {
        if stream[i..].starts_with(delim) {
            if let Some(f) = cur.take() {
                frames.push(f);
            }
            cur = Some(Vec::new());
            i += delim.len();
        } else {
            if let Some(f) = cur.as_mut() {
                f.push(stream[i]);
            }
            i += 1;
        }
    }
    if let Some(f) = cur.take() {
        frames.push(f);
    }
    frames
}

#[test]
fn tier_256_emits_only_indexed_sgr() {
    let asset = TmpFile::new("t256.slpy");
    write_chroma_asset(&asset.0, 3);
    let dump = TmpFile::new("t256.bin");
    let (ok, _, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--sim",
        "80x24:3",
        "--sim-tier",
        "256",
        "--sim-dump",
        dump.0.to_str().unwrap(),
    ]);
    assert!(ok, "player failed: {stderr}");
    let bytes = fs::read(&dump.0).unwrap();
    let sgrs = sgr_params(&bytes);
    assert!(!sgrs.is_empty(), "expected colored output");
    for p in &sgrs {
        // Every SGR is built solely from 38;5;N / 48;5;N runs (M1 acceptance
        // 5: 256-color output contains only SGR 38;5;N sequences).
        let toks: Vec<&str> = p.split(';').collect();
        let mut k = 0;
        while k < toks.len() {
            assert!(
                (toks[k] == "38" || toks[k] == "48") && toks.get(k + 1) == Some(&"5"),
                "non-indexed SGR {p:?} on 256 tier"
            );
            let n: u16 = toks[k + 2].parse().expect("SGR index");
            assert!(n <= 255);
            k += 3;
        }
        assert!(!p.contains(";2;"), "truecolor SGR {p:?} leaked into 256 tier");
    }
}

#[test]
fn tier_mono_emits_no_sgr_and_truecolor_uses_chroma_fg() {
    let asset = TmpFile::new("tmono.slpy");
    write_chroma_asset(&asset.0, 3);

    let dump = TmpFile::new("tmono.bin");
    let (ok, _, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--sim",
        "80x24:3",
        "--sim-tier",
        "mono",
        "--sim-dump",
        dump.0.to_str().unwrap(),
    ]);
    assert!(ok, "player failed: {stderr}");
    let bytes = fs::read(&dump.0).unwrap();
    assert!(!bytes.is_empty(), "mono still paints glyphs");
    assert!(
        sgr_params(&bytes).is_empty(),
        "mono must contain no color SGR at all (M1 acceptance 5)"
    );

    // Truecolor on the same asset: fg comes from the C plane (solid red
    // 0xF800 → 255,0,0), not from the M0 gray-from-luma path.
    let dump_t = TmpFile::new("ttrue.bin");
    let (ok, _, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--sim",
        "80x24:3",
        "--sim-dump",
        dump_t.0.to_str().unwrap(),
    ]);
    assert!(ok, "player failed: {stderr}");
    let text = fs::read(&dump_t.0).unwrap();
    let sgrs = sgr_params(&text);
    assert!(sgrs.iter().any(|p| p.contains("38;2;255;0;0")), "chroma fg missing: {sgrs:?}");
    assert!(
        !sgrs.iter().any(|p| p.contains("38;2;128;128;128")),
        "gray-from-luma fg must be replaced by chroma"
    );
}

#[test]
fn seek_lands_byte_identical_to_sequential() {
    let asset = TmpFile::new("seek.slpy");
    write_delta_asset(&asset.0, 23); // keyframes at 0,5,10,15,20

    // Sequential ground truth: frames 0..=17 (17 = keyframe 15 + 2 deltas).
    let dump_a = TmpFile::new("seq.bin");
    let (ok, _, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--sim",
        "80x24:18",
        "--sim-dump",
        dump_a.0.to_str().unwrap(),
    ]);
    assert!(ok, "sequential run failed: {stderr}");

    // Cold seek straight to frame 17 (17.5/30 s → floor(17.5) = 17).
    let dump_b = TmpFile::new("seek.bin");
    let (ok, _, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--seek",
        "0.58333333",
        "--sim",
        "80x24:1",
        "--sim-dump",
        dump_b.0.to_str().unwrap(),
    ]);
    assert!(ok, "seek run failed: {stderr}");

    let frames_a = split_frames(&fs::read(&dump_a.0).unwrap());
    let frames_b = split_frames(&fs::read(&dump_b.0).unwrap());
    assert_eq!(frames_a.len(), 18);
    assert_eq!(frames_b.len(), 1);
    // Full-repaint frames reset SGR state, so byte equality is exact.
    assert_eq!(
        frames_a[17], frames_b[0],
        "seek must land byte-identical to sequential decode (M1 acceptance 2)"
    );
    assert_ne!(frames_a[16], frames_b[0], "sanity: neighbor frames differ");
}

#[test]
fn norm_levels_apply_per_shot_at_runtime() {
    let asset = TmpFile::new("norm.slpy");
    write_norm_asset(&asset.0);

    let dump = TmpFile::new("norm.bin");
    let (ok, _, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--sim",
        "40x12:4",
        "--sim-dump",
        dump.0.to_str().unwrap(),
    ]);
    assert!(ok, "player failed: {stderr}");
    let frames = split_frames(&fs::read(&dump.0).unwrap());
    assert_eq!(frames.len(), 4);

    // Coarse ramp " .:-=+*#%@": luma 100 → identity glyph '-', shot-0
    // stretch (0,200) → n=128 → '+', shot-1 stretch (0,100) → n=255 → '@'.
    let glyphs = |f: &[u8]| -> Vec<char> {
        String::from_utf8_lossy(f)
            .chars()
            .filter(|c| " .:-=+*#%@".contains(*c) && *c != ' ')
            .collect()
    };
    for f in &frames[..2] {
        let g = glyphs(f);
        assert!(!g.is_empty() && g.iter().all(|&c| c == '+'), "shot 0 → '+': {g:?}");
    }
    for f in &frames[2..] {
        let g = glyphs(f);
        assert!(!g.is_empty() && g.iter().all(|&c| c == '@'), "shot 1 → '@': {g:?}");
    }
    // No per-frame pumping: frames inside one shot are byte-identical.
    assert_eq!(frames[0], frames[1], "stable levels within a shot");
    assert_eq!(frames[2], frames[3], "stable levels within a shot");
}

#[test]
fn seek_flag_validates_input() {
    let asset = TmpFile::new("badseek.slpy");
    write_delta_asset(&asset.0, 5);

    let (ok, _, stderr) =
        run_player(&[asset.0.to_str().unwrap(), "--seek", "9999", "--sim", "80x24:1"]);
    assert!(!ok);
    assert!(stderr.contains("past the end"), "unexpected stderr: {stderr}");

    let (ok, _, _) = run_player(&[asset.0.to_str().unwrap(), "--seek", "nope", "--sim", "80x24:1"]);
    assert!(!ok);

    // Valid colon form works.
    let (ok, _, stderr) =
        run_player(&[asset.0.to_str().unwrap(), "--seek", "0:00", "--sim", "80x24:1"]);
    assert!(ok, "colon timestamp failed: {stderr}");
}

#[test]
fn probe_flags_never_hang_headless() {
    let asset = TmpFile::new("probe.slpy");
    write_delta_asset(&asset.0, 3);

    // --help with piped stdio (M1 acceptance 4: never hangs piped output).
    let (ok, stdout, _) = run_player(&["--help"]);
    assert!(ok);
    for flag in ["--tier", "--no-query", "--no-cache", "--seek", "--sim-tier", "--sim-dump"] {
        assert!(stdout.contains(flag), "--help missing {flag}");
    }

    // Interactive mode on a pipe: probe returns conservative caps instantly
    // (no tty → no volley) and AnsiBackend then fails fast and cleanly.
    let (ok, _, stderr) = run_player(&[asset.0.to_str().unwrap(), "--duration-secs", "0.1"]);
    assert!(!ok);
    assert!(stderr.contains("cannot enter terminal session"), "unexpected stderr: {stderr}");

    // Escape hatches accepted headlessly; --tier doubles as the sim tier
    // fallback when --sim-tier is absent.
    let (ok, stdout, stderr) = run_player(&[
        asset.0.to_str().unwrap(),
        "--tier",
        "16",
        "--no-query",
        "--no-cache",
        "--sim",
        "80x24:2",
    ]);
    assert!(ok, "escape hatches failed: {stderr}");
    assert!(stdout.contains("\"tier\":\"16\""), "sim JSON tier: {stdout}");
}
