//! M2 item B integration tests via the real `auto-ascii-factory` binary:
//! params.toml plumbing (`params --dump`, --params overrides, validation),
//! the determinism guard (default-params build byte-pinned against the
//! committed pipeline output on a fixture this file writes itself —
//! reproducible WITHOUT the corpus and WITHOUT a pinned ffmpeg), and the
//! `eval` agent socket (tiny synthetic corpus → metrics JSON with every §6
//! metric populated + self-contained HTML contact sheet + baseline compare
//! gating with nonzero exit on breach).
//!
//! This box guarantees ffmpeg + sha256sum or shasum on PATH (Linux CI,
//! macOS dev boxes).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Self-cleaning per-test scratch dir (no tempfile dep in the workspace).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p =
            std::env::temp_dir().join(format!("auto-ascii-factory-m2-{tag}-{}", std::process::id()));
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

fn factory(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_auto-ascii-factory"));
    for a in args {
        cmd.arg(a.as_ref());
    }
    cmd.output().expect("failed to run auto-ascii-factory binary")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `sha256sum` (Linux CI) or `shasum -a 256` (macOS ships shasum, not
/// sha256sum); both print the digest as the first whitespace-separated token.
fn sha256_of(path: &Path) -> String {
    let out = match Command::new("sha256sum").arg(path).output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Command::new("shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()
            .expect("sha256sum or shasum must be on PATH"),
        Err(e) => panic!("failed to run sha256sum: {e}"),
    };
    assert!(out.status.success(), "sha256 failed on {}", path.display());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("sha256 output")
        .to_string()
}

/// The committed repo-root params.toml (also embedded in the binary).
fn repo_params_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../params.toml")
}

// ---------------------------------------------------------------------------
// params.toml plumbing
// ---------------------------------------------------------------------------

#[test]
fn params_dump_matches_committed_file_and_merges_overrides() {
    // `params --dump` == the committed repo-root file, modulo TOML noise.
    let out = factory(&[&"params", &"--dump"]);
    assert!(out.status.success(), "params --dump failed:\n{}", stderr_of(&out));
    let dumped: toml::Value =
        toml::from_str(&String::from_utf8_lossy(&out.stdout)).expect("dump must be valid TOML");
    let committed: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo_params_path()).unwrap()).unwrap();
    assert_eq!(
        dumped, committed,
        "embedded defaults drifted from the committed params.toml"
    );

    // A partial --params file overrides exactly the named keys.
    let dir = TempDir::new("dump");
    let p = dir.path("override.toml");
    std::fs::write(&p, "[build]\nkeyframe_ivl = 12\n\n[eval.tolerances]\nssim_max_drop = 0.5\n")
        .unwrap();
    let out = factory(&[&"params", &"--dump", &"--params", &p]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let merged: toml::Value =
        toml::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(merged["build"]["keyframe_ivl"].as_integer(), Some(12));
    assert_eq!(merged["eval"]["tolerances"]["ssim_max_drop"].as_float(), Some(0.5));
    // Untouched keys keep the committed defaults.
    assert_eq!(merged["build"]["fps"], committed["build"]["fps"]);
    assert_eq!(merged["shots"], committed["shots"]);

    // `params` without --dump is a clean error, not a silent no-op.
    let out = factory(&[&"params"]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("--dump"), "stderr:\n{}", stderr_of(&out));
}

#[test]
fn params_validation_rejects_degenerate_geometry() {
    let dir = TempDir::new("badparams");
    let input = dir.path("unused.mp4"); // rejected before any I/O on it

    // params file with the M1-review base_w == 1 case.
    let p = dir.path("bad.toml");
    std::fs::write(&p, "[build]\nbase_w = 1\n").unwrap();
    let out = factory(&[&"build", &input, &"-o", &dir.path("x.ascii"), &"--params", &p]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("even and >= 2"),
        "stderr should state the §4 geometry rule:\n{}",
        stderr_of(&out)
    );

    // CLI --res odd dims: same rule, friendliest error first (clap-level).
    let out = factory(&[&"build", &input, &"-o", &dir.path("x.ascii"), &"--res", &"479x270"]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("even"), "stderr:\n{}", stderr_of(&out));

    // Unknown keys (typos) are hard errors — an agent sweep must not no-op.
    let p2 = dir.path("typo.toml");
    std::fs::write(&p2, "[build]\nfsp = 30\n").unwrap();
    let out = factory(&[&"params", &"--dump", &"--params", &p2]);
    assert!(!out.status.success());
}

// ---------------------------------------------------------------------------
// Determinism guard (M2 item B: "default-params asset output must stay
// byte-identical to the current pipeline output")
// ---------------------------------------------------------------------------

/// The generated fixture itself. A GENERATOR pin, not an ffmpeg pin: the
/// bytes come from `synth_fixture` below, so no ffmpeg upgrade can move it.
/// Pinned so an edit to the generator fails HERE with a clear message instead
/// of as a mystery asset-sha mismatch below.
const FIXTURE_AVI_SHA: &str = "4c9a29d515e2184a3f592e30912dedbdb3e453901233136a249d8c56e382cd5d";
/// Committed pipeline output for the fixture at embedded-default params.
/// Re-baselining this constant is a deliberate act (it means the default
/// factory output changed for every user). History:
/// - M2 (b8e83ee8…): M1 Y+C pipeline pinned at the params.toml refactor.
/// - M3 (439e6ac8…): DELIBERATE re-pin — the factory now emits the full
///   §4 plane set (Y+E+Ex+Ey+H+C) with temporal EMA on Y/E/Ex/Ey/C, so
///   every default build's bytes changed by design (PLAN §5 stages 3–4;
///   INTERFACES note 18). Verified: two consecutive builds byte-identical
///   before pinning; corpus plane dumps eyeballed (edges trace contours).
///   RE-PINNED 2026-08-31: `[build].zstd_level` 19 -> 15 (audited sweep on
///   600 real frames — +0.91% asset bytes for a 2.84x faster build). zstd is
///   LOSSLESS, so this changes only the container's compressed bytes: the
///   decoded planes, and therefore every quality metric, are bit-identical
///   at any level. Verified before pinning: three consecutive builds all
///   produced e5bc340e…, and the L15 and L19 assets decode to identical
///   plane bytes.
///   RE-PINNED 2026-09-19: the slpy/sleepy -> auto-ascii rename changed the
///   format's identity bytes — magic `SLPY` -> `ASCI` and the TRLR payload
///   `SLPY_END` -> `ASCI_END` (both fixed-width, so no offset moved). The
///   const itself was `FIXTURE_SLPY_SHA` before this. Verified before pinning:
///   three consecutive builds all produced b00e3ecb…, and — the stronger
///   check — reverting exactly those 16 bytes in the new asset (4 magic,
///   8 trailer payload, 4 trailer CRC) reproduces the previous pin
///   e5bc340e… EXACTLY. So every compressed plane byte is bit-identical and
///   this is a pure container-identity re-pin, not a pipeline change. Every
///   render golden and insta snapshot passed unchanged through the rename.
///   RE-PINNED 2026-09-20: the fixture is now a Rust-written raw BGR24 AVI
///   instead of an ffmpeg lavfi/libx264 mp4, so the pin no longer depends on
///   the ffmpeg build — the old mp4 pin died on ffmpeg 9.0.2, which renders
///   testsrc2 and encodes it differently from the box the pin was taken on.
///   The PIPELINE is unchanged; only the input bytes are, so the asset sha
///   necessarily moved with them. Verified before pinning: three consecutive
///   default builds of the new fixture all produced 7b301c1b… on macOS/aarch64.
///   Cross-platform (Linux) confirmation is pending.
const FIXTURE_ASSET_SHA: &str = "7b301c1b2d649cf9ba43ac46010c5b4cb00aab892a404049a3671bdd7a59fb6f";

// Fixture geometry: exactly the default `[build]` grid and rate, which is
// what makes the ingest a passthrough (see `synth_fixture`).
const FIX_W: usize = 480;
const FIX_H: usize = 270;
const FIX_FPS: u32 = 30;
const FIX_FRAMES: usize = 30;
/// One uncompressed BGR24 frame. The row stride (1440) is a multiple of 4, so
/// DIB rows need no padding and every `00db` chunk is even-sized — the RIFF
/// pad byte never applies anywhere in this file.
const FIX_FRAME_BYTES: u32 = (FIX_W * FIX_H * 3) as u32;
/// Frame of the hard scene change (gives shot detection exactly one cut).
const FIX_CUT: usize = 15;

/// One rectangle in the pattern: x, y, w, h + RGB.
type FixRect = (i32, i32, i32, i32, (u8, u8, u8));

fn le32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}

fn le16(out: &mut Vec<u8>, n: u16) {
    out.extend_from_slice(&n.to_le_bytes());
}

/// `fourcc` + little-endian payload size + payload.
fn riff_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(fourcc);
    le32(out, payload.len() as u32);
    out.extend_from_slice(payload);
}

/// Integer HSV at full saturation: `hue` walks six 256-wide segments of the
/// colour wheel, `val` scales 0..=255. Integer-only, like everything else in
/// the generator — no float rounding to differ between hosts.
fn hue_rgb(hue: u32, val: u32) -> (u8, u8, u8) {
    let (seg, t) = (hue / 256 % 6, hue % 256);
    let (r, g, b) = match seg {
        0 => (255, t, 0),
        1 => (255 - t, 255, 0),
        2 => (0, 255, t),
        3 => (0, 255 - t, 255),
        4 => (t, 0, 255),
        _ => (255, 0, 255 - t),
    };
    ((r * val / 255) as u8, (g * val / 255) as u8, (b * val / 255) as u8)
}

fn put_px(rgb: &mut [u8], x: usize, y: usize, c: (u8, u8, u8)) {
    let i = (y * FIX_W + x) * 3;
    rgb[i] = c.0;
    rgb[i + 1] = c.1;
    rgb[i + 2] = c.2;
}

/// Hard-edged filled rectangle (no anti-aliasing), clipped to the frame.
fn fill_rect(rgb: &mut [u8], x0: i32, y0: i32, w: i32, h: i32, c: (u8, u8, u8)) {
    for y in y0.max(0)..(y0 + h).min(FIX_H as i32) {
        for x in x0.max(0)..(x0 + w).min(FIX_W as i32) {
            put_px(rgb, x as usize, y as usize, c);
        }
    }
}

/// One frame of the synthetic clip, top-down packed RGB24.
///
/// Deliberately edge-rich and colourful so every §4 plane gets real content:
/// a smooth gradient background (Y), hard rectangle and 45° stripe edges
/// (E + the Ex/Ey orientation field), a pure-white square (H highlights),
/// saturated primaries against a hue ramp (C), and a hard cut at frame 15
/// (shot detection sees one cut).
fn fix_frame_rgb(f: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; FIX_W * FIX_H * 3];
    let cut = f >= FIX_CUT;
    let phase = (f % FIX_CUT) * 4; // 4 px/frame, restarting after the cut
    let step = phase as i32;

    // Background: a luma gradient along one axis, a hue ramp along the other.
    // The cut swaps the two axes AND drops to a dark, narrow luma band: the
    // §5 stage-2 detector thresholds the SAD of 256-bin L* HISTOGRAMS, which
    // a mere transpose would leave untouched (it is position-blind). Scene B
    // barely overlaps scene A's luma range, so the cut is unmissable — and
    // its shadows give the H plane's deep-shadow bit real work too.
    for y in 0..FIX_H {
        for x in 0..FIX_W {
            let (hue, val) = if cut {
                (x * 1536 / FIX_W, 10 + y * 50 / FIX_H)
            } else {
                (y * 1536 / FIX_H, 40 + x * 200 / FIX_W)
            };
            put_px(&mut rgb, x, y, hue_rgb(hue as u32, val as u32));
        }
    }

    // A band of diagonal stripes: constant x+y is a 45° edge.
    let band = if cut { 20..70 } else { 120..170 };
    for y in band {
        for x in 0..FIX_W {
            let lit = ((x + y + phase) / 12).is_multiple_of(2);
            put_px(&mut rgb, x, y, if lit { (245, 245, 30) } else { (20, 20, 80) });
        }
    }

    // Saturated rectangles sliding 4 px/frame, alternating direction so the
    // motion field is not a single global pan.
    let rects: &[FixRect] = if cut {
        &[
            (250, 90, 90, 70, (255, 32, 0)),
            (30, 170, 80, 60, (0, 224, 64)),
            (150, 120, 60, 110, (32, 64, 255)),
        ]
    } else {
        &[
            (20, 30, 80, 70, (255, 0, 32)),
            (200, 60, 70, 50, (0, 255, 96)),
            (330, 180, 90, 70, (64, 0, 255)),
        ]
    };
    for (i, &(x, y, w, h, c)) in rects.iter().enumerate() {
        let dx = if i.is_multiple_of(2) { step } else { -step };
        fill_rect(&mut rgb, x + dx, y, w, h, c);
    }

    // A small pure-white square — the H plane's top-hat target.
    let (hx, hy) = if cut { (380 - step, 200) } else { (60 + step, 215) };
    fill_rect(&mut rgb, hx, hy, 24, 24, (255, 255, 255));
    rgb
}

/// The same frame as the DIB stores it: rows bottom-up (last image row
/// first), pixels B,G,R.
fn fix_frame_bgr_bottom_up(f: usize) -> Vec<u8> {
    let rgb = fix_frame_rgb(f);
    let stride = FIX_W * 3;
    let mut out = Vec::with_capacity(rgb.len());
    for y in (0..FIX_H).rev() {
        for px in rgb[y * stride..(y + 1) * stride].chunks_exact(3) {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    out
}

/// Write the determinism fixture: a minimal RIFF AVI holding 30 frames of
/// uncompressed 24-bit BGR (BI_RGB) at 480x270, 30 fps.
///
/// Written from Rust rather than synthesised with `ffmpeg -f lavfi -i
/// testsrc2 ...` on purpose. The mp4 this replaced depended on testsrc2's
/// renderer, on libx264 AND on swscale's YUV->RGB, none of which are
/// bit-stable across ffmpeg majors — so the pin broke on the first ffmpeg
/// upgrade, with nothing about the factory having changed. This file is
/// already at the default `[build]` geometry and rate, so the factory's
/// `scale=480:270:flags=area,fps=30,format=rgb24` ingest is a passthrough
/// scale, a 1:1 fps filter and an exact BGR->RGB byte permutation: every
/// ffmpeg build decodes the identical frames.
fn synth_fixture(dir: &TempDir) -> PathBuf {
    // movi: one `00db` chunk per frame, each with an idx1 entry. idx1
    // offsets are relative to the `movi` fourcc itself (4 for the first).
    let mut movi = Vec::with_capacity(4 + FIX_FRAMES * (8 + FIX_FRAME_BYTES as usize));
    movi.extend_from_slice(b"movi");
    let mut idx1 = Vec::with_capacity(FIX_FRAMES * 16);
    for f in 0..FIX_FRAMES {
        let offset = movi.len() as u32;
        riff_chunk(&mut movi, b"00db", &fix_frame_bgr_bottom_up(f));
        idx1.extend_from_slice(b"00db");
        le32(&mut idx1, 0x10); // dwFlags = AVIIF_KEYFRAME
        le32(&mut idx1, offset);
        le32(&mut idx1, FIX_FRAME_BYTES);
    }

    // hdrl { avih, LIST strl { strh, strf } }.
    let mut avih = Vec::with_capacity(56); // MainAVIHeader
    le32(&mut avih, 1_000_000 / FIX_FPS); // dwMicroSecPerFrame
    le32(&mut avih, FIX_FRAME_BYTES * FIX_FPS); // dwMaxBytesPerSec
    le32(&mut avih, 0); // dwPaddingGranularity
    le32(&mut avih, 0x10); // dwFlags = AVIF_HASINDEX
    le32(&mut avih, FIX_FRAMES as u32); // dwTotalFrames
    le32(&mut avih, 0); // dwInitialFrames
    le32(&mut avih, 1); // dwStreams
    le32(&mut avih, FIX_FRAME_BYTES); // dwSuggestedBufferSize
    le32(&mut avih, FIX_W as u32); // dwWidth
    le32(&mut avih, FIX_H as u32); // dwHeight
    for _ in 0..4 {
        le32(&mut avih, 0); // dwReserved[4]
    }

    let mut strh = Vec::with_capacity(56); // AVIStreamHeader
    strh.extend_from_slice(b"vids"); // fccType
    strh.extend_from_slice(b"DIB "); // fccHandler = uncompressed DIB
    le32(&mut strh, 0); // dwFlags
    le16(&mut strh, 0); // wPriority
    le16(&mut strh, 0); // wLanguage
    le32(&mut strh, 0); // dwInitialFrames
    le32(&mut strh, 1); // dwScale
    le32(&mut strh, FIX_FPS); // dwRate => 30/1 fps
    le32(&mut strh, 0); // dwStart
    le32(&mut strh, FIX_FRAMES as u32); // dwLength
    le32(&mut strh, FIX_FRAME_BYTES); // dwSuggestedBufferSize
    le32(&mut strh, 0xFFFF_FFFF); // dwQuality = default
    le32(&mut strh, 0); // dwSampleSize
    for v in [0, 0, FIX_W as u16, FIX_H as u16] {
        le16(&mut strh, v); // rcFrame, four i16
    }

    let mut strf = Vec::with_capacity(40); // BITMAPINFOHEADER
    le32(&mut strf, 40); // biSize
    le32(&mut strf, FIX_W as u32); // biWidth
    le32(&mut strf, FIX_H as u32); // biHeight > 0 => bottom-up rows
    le16(&mut strf, 1); // biPlanes
    le16(&mut strf, 24); // biBitCount
    le32(&mut strf, 0); // biCompression = BI_RGB
    le32(&mut strf, FIX_FRAME_BYTES); // biSizeImage
    for _ in 0..4 {
        le32(&mut strf, 0); // bi{X,Y}PelsPerMeter, biClrUsed, biClrImportant
    }

    let mut strl = Vec::new();
    strl.extend_from_slice(b"strl");
    riff_chunk(&mut strl, b"strh", &strh);
    riff_chunk(&mut strl, b"strf", &strf);
    let mut hdrl = Vec::new();
    hdrl.extend_from_slice(b"hdrl");
    riff_chunk(&mut hdrl, b"avih", &avih);
    riff_chunk(&mut hdrl, b"LIST", &strl);

    // RIFF `AVI ` { LIST hdrl, LIST movi, idx1 }.
    let mut body = Vec::with_capacity(4 + 8 + hdrl.len() + 8 + movi.len() + 8 + idx1.len());
    body.extend_from_slice(b"AVI ");
    riff_chunk(&mut body, b"LIST", &hdrl);
    riff_chunk(&mut body, b"LIST", &movi);
    riff_chunk(&mut body, b"idx1", &idx1);
    let mut avi = Vec::with_capacity(8 + body.len());
    riff_chunk(&mut avi, b"RIFF", &body);

    let input = dir.path("fixture.avi");
    std::fs::write(&input, &avi).unwrap();
    input
}

#[test]
fn default_params_build_is_byte_pinned() {
    let dir = TempDir::new("detguard");
    let input = synth_fixture(&dir);
    assert_eq!(
        sha256_of(&input),
        FIXTURE_AVI_SHA,
        "synth_fixture no longer writes the pinned bytes — the generator was \
         edited; re-baseline BOTH constants deliberately"
    );

    // Flagless build == embedded defaults == the committed pipeline bytes.
    let out_default = dir.path("default.ascii");
    let out = factory(&[&"build", &input, &"-o", &out_default]);
    assert!(out.status.success(), "build failed:\n{}", stderr_of(&out));
    assert_eq!(
        sha256_of(&out_default),
        FIXTURE_ASSET_SHA,
        "default-params factory output changed byte-wise — the params.toml \
         defaults no longer reproduce the committed pipeline (M2 determinism \
         guard). If deliberate, re-baseline FIXTURE_ASSET_SHA."
    );

    // --params <copy of the committed file> must be byte-identical too:
    // file-loaded params and embedded params are the same config.
    let params_copy = dir.path("params-copy.toml");
    std::fs::copy(repo_params_path(), &params_copy).unwrap();
    let out_filed = dir.path("filed.ascii");
    let out = factory(&[&"build", &input, &"-o", &out_filed, &"--params", &params_copy]);
    assert!(out.status.success(), "build failed:\n{}", stderr_of(&out));
    assert_eq!(sha256_of(&out_filed), FIXTURE_ASSET_SHA, "--params file path diverged");
}

/// Acceptance 8 (corpus integration half — the committed guard above runs
/// without the corpus): rebuilding the grass clip with pure defaults must be
/// byte-identical to the committed assets/ copy. Ignored by default: needs
/// the local-only corpus and a full 194-frame zstd-19 build (~1 min).
/// Run: `cargo test -p auto-ascii-factory --test m2_params_eval -- --ignored`
#[test]
#[ignore = "needs local corpus (gitignored) + ~1 min build"]
fn grass_rebuild_matches_assets_copy() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let input = root.join("corpus/prepared/grass-field-windy-mirror.mp4");
    let committed = root.join("assets/grass-field-windy-mirror.ascii");
    if !input.is_file() || !committed.is_file() {
        eprintln!("skipping: corpus/assets not present on this checkout");
        return;
    }
    let dir = TempDir::new("grass");
    let rebuilt = dir.path("grass.ascii");
    let out = factory(&[&"build", &input, &"-o", &rebuilt]);
    assert!(out.status.success(), "grass rebuild failed:\n{}", stderr_of(&out));
    assert_eq!(
        sha256_of(&rebuilt),
        sha256_of(&committed),
        "default-params grass rebuild is not byte-identical to assets/ copy"
    );
}

// ---------------------------------------------------------------------------
// eval: JSON + HTML + baseline gating on a tiny synthetic corpus
// ---------------------------------------------------------------------------

/// Two lavfi clips: continuous (1 shot) + two-scene concat (1 cut).
fn synth_corpus(dir: &TempDir) -> PathBuf {
    let corpus = dir.path("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    let status = Command::new("ffmpeg")
        .args(["-y", "-nostdin", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=duration=2:size=320x180:rate=10")
        .args(["-pix_fmt", "yuv420p"])
        .arg(corpus.join("clip-a.mp4"))
        .status()
        .expect("ffmpeg must be installed");
    assert!(status.success());
    let status = Command::new("ffmpeg")
        .args(["-y", "-nostdin", "-v", "error"])
        .args(["-f", "lavfi", "-i", "testsrc2=duration=1.5:size=320x180:rate=10"])
        .args(["-f", "lavfi", "-i", "testsrc2=duration=1.5:size=320x180:rate=10"])
        .args([
            "-filter_complex",
            "[1:v]negate,eq=brightness=-0.3[b];[0:v][b]concat=n=2:v=1:a=0[out]",
        ])
        .args(["-map", "[out]", "-pix_fmt", "yuv420p"])
        .arg(corpus.join("clip-b.mp4"))
        .status()
        .expect("ffmpeg must be installed");
    assert!(status.success());
    corpus
}

/// Eval params: fast fps, a short keyframe cadence (so keyframe_count is a
/// live metric on 20–30-frame clips — the keyframe-regression drill below
/// needs headroom to DROP), denser SSIM sampling, and a huge stage-time
/// tolerance — stage means over ~20 debug frames on a loaded 4-core box are
/// far noisier than the default +50%; the perf gate (item E) is the precise
/// instrument, this test gates the deterministic metrics.
const EVAL_PARAMS: &str = "[build]\nfps = 10\nkeyframe_ivl = 4\n\n\
                           [eval]\nssim_every = 5\n\n\
                           [eval.tolerances]\nstage_ms_frac_max_increase = 1000.0\n";

#[test]
fn eval_emits_metrics_json_html_and_gates_on_baseline() {
    let dir = TempDir::new("eval");
    let corpus = synth_corpus(&dir);
    let params = dir.path("p.toml");
    std::fs::write(&params, EVAL_PARAMS).unwrap();
    let cache = dir.path("cache");
    let out_json = dir.path("runs/run1.json");
    let out_html = dir.path("runs/run1.html");
    let out_reel = dir.path("runs/run1-reel.html");

    // --- run 1: builds assets, emits JSON + HTML + review reel ------------
    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &params, &"--out", &out_json,
        &"--html", &out_html, &"--reel", &out_reel, &"--cache-dir", &cache,
    ]);
    assert!(out.status.success(), "eval failed:\n{}", stderr_of(&out));

    // JSON: every §6 metric populated for both clips.
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_json).unwrap()).unwrap();
    assert_eq!(json["schema_version"], 2); // M3 bump: edge-F1 metric family
    let clips = json["clips"].as_array().unwrap();
    assert_eq!(clips.len(), 2, "both corpus clips must be evaluated");
    assert_eq!(clips[0]["name"], "clip-a");
    assert_eq!(clips[1]["name"], "clip-b");
    for clip in clips {
        let m = &clip["metrics"];
        let ssim = m["ssim"].as_f64().expect("ssim populated");
        assert!(ssim > 0.0 && ssim <= 1.0, "ssim in (0,1]: {ssim}");
        let flicker = m["flicker_switches_per_cell_sec"].as_f64().expect("flicker populated");
        assert!(flicker >= 0.0);
        // M3 edge-F1 family: populated (layer mask observed + source Canny
        // truth computed) and in range. testsrc2 is edge-rich, so truth is
        // nonempty; the score itself depends on the renderer's edge layer.
        for k in ["edge_f1", "edge_precision", "edge_recall"] {
            let v = m[k].as_f64().unwrap_or_else(|| panic!("{k} unpopulated"));
            assert!((0.0..=1.0).contains(&v), "{k} out of range: {v}");
        }
        let tiers = m["damage_by_tier"].as_object().unwrap();
        for tier in ["truecolor", "256", "mono"] {
            let d = tiers.get(tier).unwrap_or_else(|| panic!("tier {tier} missing"));
            assert!(d["avg_bytes_per_frame"].as_f64().unwrap() > 0.0);
            assert!(d["frames"].as_u64().unwrap() > 0);
        }
        // Truecolor must be the most expensive stream (SGR 38;2 vs 38;5/none).
        let bytes = |t: &str| tiers[t]["avg_bytes_per_frame"].as_f64().unwrap();
        assert!(bytes("truecolor") > bytes("256") && bytes("256") > bytes("mono"));
        for stage in ["decode", "resample", "compose", "present"] {
            assert!(
                m["stage_ms"][stage]["frames"].as_u64().unwrap() > 0,
                "stage {stage} unpopulated"
            );
        }
    }
    // clip-b has a hard cut; the eval segments flicker on it, so both clips
    // stay far below catastrophe even though the concat flips every cell.
    let flicker_b = clips[1]["metrics"]["flicker_switches_per_cell_sec"].as_f64().unwrap();
    assert!(flicker_b < 5.0, "cut leaked into flicker: {flicker_b}");
    // Asset-structure metrics (M2 review fix): populated for every clip,
    // and clip-b's splice is visible as a cut in the report.
    for clip in clips {
        let m = &clip["metrics"];
        assert!(m["shot_count"].as_u64().unwrap() >= 1);
        assert!(m["keyframe_count"].as_u64().unwrap() >= 2, "keyframe_ivl=4 on 20+ frames");
        assert!(m["asset_bytes"].as_u64().unwrap() > 0);
    }
    assert_eq!(clips[0]["metrics"]["cut_count"].as_u64(), Some(0), "clip-a is one shot");
    assert!(
        clips[1]["metrics"]["cut_count"].as_u64().unwrap() >= 1,
        "clip-b's concat splice must be a detected cut"
    );

    // HTML contact sheet: self-contained, embedded PNGs, both clips.
    let html = std::fs::read_to_string(&out_html).unwrap();
    assert!(html.contains("<title>auto-ascii eval</title>"));
    assert!(html.contains("clip-a") && html.contains("clip-b"));
    let pngs = html.matches("data:image/png;base64,iVBOR").count();
    assert!(pngs >= 8, "expected >= 8 embedded PNGs (source+render x snaps x clips), got {pngs}");
    assert!(!html.contains("http://") && !html.contains("https://"), "must be self-contained");
    assert!(html.contains("edge F1 vs source Canny"), "contact sheet reports edge F1");

    // Review reel (M3 sign-off artifact): self-contained, per-clip animated
    // GIF + >= 4 source|render timestamp rows with metric strips.
    let reel = std::fs::read_to_string(&out_reel).unwrap();
    assert!(reel.contains("<title>auto-ascii review reel</title>"));
    assert!(reel.contains("clip-a") && reel.contains("clip-b"));
    assert_eq!(reel.matches("data:image/gif;base64,").count(), 2, "one GIF per clip");
    let reel_pngs = reel.matches("data:image/png;base64,iVBOR").count();
    assert!(reel_pngs >= 2 * 2 * 4, "expected >= 4 rows x 2 imgs x 2 clips, got {reel_pngs}");
    assert!(reel.contains("flicker-to-date") && reel.contains("edge F1"));
    assert!(!reel.contains("http://") && !reel.contains("https://"), "reel must be self-contained");
    for chunk in reel.split("src=\"").skip(1) {
        assert!(chunk.starts_with("data:"), "reel has a non-data: src");
    }

    // --- run 2: cached assets + self-baseline PASS, exit 0 ----------------
    let out2_json = dir.path("runs/run2.json");
    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &params, &"--baseline", &out_json,
        &"--out", &out2_json, &"--cache-dir", &cache,
    ]);
    assert!(out.status.success(), "self-baseline compare must pass:\n{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("cached asset"), "second run must hit the asset cache");
    assert!(stderr_of(&out).contains("baseline: PASS"));

    // --- tampered baseline: gate trips, nonzero exit ----------------------
    let mut tampered: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_json).unwrap()).unwrap();
    tampered["clips"][0]["metrics"]["ssim"] = serde_json::json!(0.999); // ssim "regressed"
    tampered["clips"][1]["metrics"]["damage_by_tier"]["truecolor"]["avg_bytes_per_frame"] =
        serde_json::json!(1000.0); // bytes "exploded"
    let tampered_path = dir.path("runs/tampered.json");
    std::fs::write(&tampered_path, tampered.to_string()).unwrap();
    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &params, &"--baseline", &tampered_path,
        &"--out", &dir.path("runs/run3.json"), &"--cache-dir", &cache,
    ]);
    assert!(!out.status.success(), "tolerance breach must exit nonzero");
    let err = stderr_of(&out);
    assert!(err.contains("baseline compare FAILED"), "stderr:\n{err}");
    assert!(err.contains("FAIL clip-a/ssim"), "stderr:\n{err}");
    assert!(err.contains("FAIL clip-b/bytes_per_frame/truecolor"), "stderr:\n{err}");

    // --- deliberate param regression trips the gate (M2 acceptance 4):
    // a params.toml edit that renders on a much larger grid inflates
    // bytes/frame deterministically past the +20% tolerance for every tier
    // vs the 300x80 baseline — nonzero exit, then "reverted" (params files
    // are per-run inputs; the good file is untouched).
    let regressed = dir.path("p-regressed.toml");
    std::fs::write(
        &regressed,
        "[build]\nfps = 10\nkeyframe_ivl = 4\n\n\
         [eval]\nssim_every = 5\ngrid_cols = 420\ngrid_rows = 112\n\n\
         [eval.tolerances]\nstage_ms_frac_max_increase = 1000.0\n",
    )
    .unwrap();
    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &regressed, &"--baseline", &out_json,
        &"--out", &dir.path("runs/run4.json"), &"--cache-dir", &cache,
    ]);
    assert!(!out.status.success(), "param regression must trip the baseline compare");
    let err = stderr_of(&out);
    assert!(err.contains("FAIL clip-a/bytes_per_frame/truecolor"), "stderr:\n{err}");
    assert!(err.contains("baseline compare FAILED"), "stderr:\n{err}");
    // Asset cache was NOT invalidated: eval-only knobs are excluded from the
    // params fingerprint (no rebuild happened on the regressed run).
    assert!(err.contains("cached asset"), "eval knobs must not rebuild assets:\n{err}");

    // Reverted params pass again (the drill leaves the tree green).
    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &params, &"--baseline", &out_json,
        &"--out", &dir.path("runs/run5.json"), &"--cache-dir", &cache,
    ]);
    assert!(out.status.success(), "reverted params must pass:\n{}", stderr_of(&out));

    // --- FACTORY-tunable regressions trip the gate (M2 review fix — these
    // were previously invisible: the SSIM reference self-graded through the
    // factory's own levels and no shot/keyframe/size metric existed).
    //
    // (a) a shot threshold that kills cut detection: clip-b's splice
    // vanishes from the NORM roster → shot_count/cut_count fail.
    let killed_cuts = dir.path("p-killed-cuts.toml");
    std::fs::write(
        &killed_cuts,
        format!("{EVAL_PARAMS}\n[shots]\nsad_threshold_milli = 100000\n"),
    )
    .unwrap();
    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &killed_cuts, &"--baseline", &out_json,
        &"--out", &dir.path("runs/run6.json"), &"--cache-dir", &cache,
    ]);
    assert!(!out.status.success(), "killed cut detection must trip the baseline compare");
    let err = stderr_of(&out);
    assert!(err.contains("FAIL clip-b/cut_count"), "stderr:\n{err}");
    assert!(err.contains("FAIL clip-b/shot_count"), "stderr:\n{err}");

    // (b) an inflated keyframe cadence: keyframe_count drops → fails.
    let sparse_keys = dir.path("p-sparse-keys.toml");
    std::fs::write(
        &sparse_keys,
        EVAL_PARAMS.replace("keyframe_ivl = 4", "keyframe_ivl = 200"),
    )
    .unwrap();
    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &sparse_keys, &"--baseline", &out_json,
        &"--out", &dir.path("runs/run7.json"), &"--cache-dir", &cache,
    ]);
    assert!(!out.status.success(), "keyframe cadence regression must trip the compare");
    let err = stderr_of(&out);
    assert!(err.contains("FAIL clip-a/keyframe_count"), "stderr:\n{err}");

    // (c) keyframe_ivl = 600 (the PLAN acceptance-4 drill value) is a clean
    // params range error naming the u8 wire limit — not a serde type error,
    // not a silent build.
    let kf600 = dir.path("p-kf600.toml");
    std::fs::write(&kf600, "[build]\nkeyframe_ivl = 600\n").unwrap();
    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &kf600, &"--baseline", &out_json,
        &"--out", &dir.path("runs/run8.json"), &"--cache-dir", &cache,
    ]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("1..=255"), "stderr:\n{}", stderr_of(&out));
}

// ---------------------------------------------------------------------------
// sweep: ranked combos + leaderboard on the tiny synthetic corpus (M3 Tune)
// ---------------------------------------------------------------------------

#[test]
fn sweep_ranks_combos_and_reuses_the_asset_cache() {
    let dir = TempDir::new("sweep");
    let corpus = synth_corpus(&dir);
    let params = dir.path("p.toml");
    std::fs::write(&params, EVAL_PARAMS).unwrap();
    let cache = dir.path("cache");
    let out_dir = dir.path("sweeps/edge");

    // One renderer-only axis (2 sane combos + 1 that fails validation) plus
    // an absurd-T_on combo: [compose] is excluded from the build fingerprint,
    // so every combo after the first must hit the asset cache.
    let grid = dir.path("grid.toml");
    std::fs::write(
        &grid,
        "[[axes]]\nname = \"edge-runtime\"\nvalues = [\n\
         { \"compose.edge_t_on\" = 32, \"compose.edge_t_off\" = 16 },\n\
         { \"compose.edge_t_on\" = 240, \"compose.edge_t_off\" = 120 },\n\
         { \"compose.edge_t_on\" = 8, \"compose.edge_t_off\" = 16 },\n]\n",
    )
    .unwrap();

    let out = factory(&[
        &"sweep", &"--corpus", &corpus, &"--params", &params, &"--grid", &grid,
        &"--out", &out_dir, &"--cache-dir", &cache,
    ]);
    assert!(out.status.success(), "sweep failed:\n{}", stderr_of(&out));
    let err = stderr_of(&out);
    assert!(err.contains("3 combo(s)"), "stderr:\n{err}");
    assert!(err.contains("cached asset"), "combos 2+ must reuse the asset cache:\n{err}");
    assert!(err.contains("SKIPPED"), "t_off > t_on combo must be a recorded skip:\n{err}");

    // sweep.json: ranked, skip sinks to the tail, per-clip rows present.
    let sweep: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out_dir.join("sweep.json")).unwrap())
            .unwrap();
    assert_eq!(sweep["schema_version"], 1);
    let results = sweep["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    let scores: Vec<Option<f64>> = results.iter().map(|r| r["score"].as_f64()).collect();
    assert!(scores[0].is_some() && scores[1].is_some(), "two scored combos");
    assert!(scores[0].unwrap() >= scores[1].unwrap(), "ranked best-first");
    assert!(scores[2].is_none(), "skipped combo sinks to the tail");
    assert!(results[2]["skip_reason"].as_str().unwrap().contains("edge_t_off"));
    // The absurd T_on=240 gate must not beat the sane default (edge layer
    // dies => edge_f1 term collapses) — the aesthetic-regression mechanism
    // the drill rides.
    let sane = results.iter().find(|r| r["combo"].as_str().unwrap().contains("=32")).unwrap();
    let absurd = results.iter().find(|r| r["combo"].as_str().unwrap().contains("=240")).unwrap();
    assert!(
        sane["edge_f1_mean"].as_f64().unwrap() >= absurd["edge_f1_mean"].as_f64().unwrap(),
        "sane {} vs absurd {}",
        sane["edge_f1_mean"],
        absurd["edge_f1_mean"]
    );
    // Per-clip rows + per-combo EvalReports on disk.
    for r in results.iter().take(2) {
        assert_eq!(r["clips"].as_array().unwrap().len(), 2, "both clips scored");
        let rep = r["report"].as_str().unwrap();
        let combo_report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out_dir.join(rep)).unwrap()).unwrap();
        assert_eq!(combo_report["schema_version"], 2);
        // Sweep mode is truecolor-only (damage tiers trimmed deliberately).
        let tiers =
            combo_report["clips"][0]["metrics"]["damage_by_tier"].as_object().unwrap();
        assert_eq!(tiers.len(), 1, "sweep runs the truecolor pass only");
        assert!(tiers.contains_key("truecolor"));
    }

    // Leaderboard: self-contained, one row per combo, skip labeled.
    let html = std::fs::read_to_string(out_dir.join("leaderboard.html")).unwrap();
    assert!(html.contains("<title>auto-ascii sweep leaderboard</title>"));
    assert!(html.contains("skipped:"));
    assert!(!html.contains("http://") && !html.contains("https://"), "must be self-contained");

    // Typo'd param path: hard error naming the path (agent sweeps must not
    // silently no-op — the params.toml contract).
    let bad = dir.path("grid-typo.toml");
    std::fs::write(
        &bad,
        "[[axes]]\nname = \"x\"\nvalues = [ { \"compose.edge_t_onn\" = 32 } ]\n",
    )
    .unwrap();
    let out = factory(&[
        &"sweep", &"--corpus", &corpus, &"--params", &params, &"--grid", &bad,
        &"--out", &dir.path("sweeps/typo"), &"--cache-dir", &cache,
    ]);
    assert!(!out.status.success(), "typo'd path must fail");
    assert!(stderr_of(&out).contains("edge_t_onn"), "stderr:\n{}", stderr_of(&out));
}
