//! M2 item B integration tests via the real `sleepy-factory` binary:
//! params.toml plumbing (`params --dump`, --params overrides, validation),
//! the determinism guard (default-params build byte-pinned against the
//! committed pipeline output on a synthetic lavfi fixture — reproducible
//! WITHOUT the corpus), and the `eval` agent socket (tiny synthetic corpus →
//! metrics JSON with every §6 metric populated + self-contained HTML contact
//! sheet + baseline compare gating with nonzero exit on breach).
//!
//! This box guarantees ffmpeg + sha256sum on PATH (Linux CI).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Self-cleaning per-test scratch dir (no tempfile dep in the workspace).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p =
            std::env::temp_dir().join(format!("sleepy-factory-m2-{tag}-{}", std::process::id()));
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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sleepy-factory"));
    for a in args {
        cmd.arg(a.as_ref());
    }
    cmd.output().expect("failed to run sleepy-factory binary")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn sha256_of(path: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("sha256sum must be on PATH (Linux CI box)");
    assert!(out.status.success(), "sha256sum failed on {}", path.display());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum output")
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
    let out = factory(&[&"build", &input, &"-o", &dir.path("x.slpy"), &"--params", &p]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("even and >= 2"),
        "stderr should state the §4 geometry rule:\n{}",
        stderr_of(&out)
    );

    // CLI --res odd dims: same rule, friendliest error first (clap-level).
    let out = factory(&[&"build", &input, &"-o", &dir.path("x.slpy"), &"--res", &"479x270"]);
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

/// lavfi fixture (no corpus dependency). Pinned so an ffmpeg upgrade that
/// changes testsrc2/encoder bytes fails HERE with a clear message instead of
/// as a mystery asset-sha mismatch below.
const FIXTURE_MP4_SHA: &str = "a0a8d8fa401e499fb2ea04d6b00ddbc1d4e79c8dd2a728e035cf6609e69f3454";
/// Committed pipeline output for the fixture at embedded-default params.
/// Re-baselining this constant is a deliberate act (it means the default
/// factory output changed for every user). History:
/// - M2 (b8e83ee8…): M1 Y+C pipeline pinned at the params.toml refactor.
/// - M3 (439e6ac8…): DELIBERATE re-pin — the factory now emits the full
///   §4 plane set (Y+E+Ex+Ey+H+C) with temporal EMA on Y/E/Ex/Ey/C, so
///   every default build's bytes changed by design (PLAN §5 stages 3–4;
///   INTERFACES note 18). Verified: two consecutive builds byte-identical
///   before pinning; corpus plane dumps eyeballed (edges trace contours).
const FIXTURE_SLPY_SHA: &str = "439e6ac8933f4fc711480579d3ba416c841ed35770d6cf0b4f0cf26d18e2aab5";

fn synth_fixture(dir: &TempDir) -> PathBuf {
    let input = dir.path("fixture.mp4");
    let status = Command::new("ffmpeg")
        .args(["-y", "-nostdin", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=duration=1:size=480x270:rate=30")
        .args(["-pix_fmt", "yuv420p"])
        .arg(&input)
        .status()
        .expect("ffmpeg must be installed");
    assert!(status.success(), "ffmpeg lavfi synthesis failed");
    input
}

#[test]
fn default_params_build_is_byte_pinned() {
    let dir = TempDir::new("detguard");
    let input = synth_fixture(&dir);
    assert_eq!(
        sha256_of(&input),
        FIXTURE_MP4_SHA,
        "the ffmpeg on this box renders the lavfi fixture differently — \
         re-baseline BOTH constants deliberately"
    );

    // Flagless build == embedded defaults == the committed pipeline bytes.
    let out_default = dir.path("default.slpy");
    let out = factory(&[&"build", &input, &"-o", &out_default]);
    assert!(out.status.success(), "build failed:\n{}", stderr_of(&out));
    assert_eq!(
        sha256_of(&out_default),
        FIXTURE_SLPY_SHA,
        "default-params factory output changed byte-wise — the params.toml \
         defaults no longer reproduce the committed pipeline (M2 determinism \
         guard). If deliberate, re-baseline FIXTURE_SLPY_SHA."
    );

    // --params <copy of the committed file> must be byte-identical too:
    // file-loaded params and embedded params are the same config.
    let params_copy = dir.path("params-copy.toml");
    std::fs::copy(repo_params_path(), &params_copy).unwrap();
    let out_filed = dir.path("filed.slpy");
    let out = factory(&[&"build", &input, &"-o", &out_filed, &"--params", &params_copy]);
    assert!(out.status.success(), "build failed:\n{}", stderr_of(&out));
    assert_eq!(sha256_of(&out_filed), FIXTURE_SLPY_SHA, "--params file path diverged");
}

/// Acceptance 8 (corpus integration half — the committed guard above runs
/// without the corpus): rebuilding the grass clip with pure defaults must be
/// byte-identical to the committed assets/ copy. Ignored by default: needs
/// the local-only corpus and a full 194-frame zstd-19 build (~1 min).
/// Run: `cargo test -p sleepy-factory --test m2_params_eval -- --ignored`
#[test]
#[ignore = "needs local corpus (gitignored) + ~1 min build"]
fn grass_rebuild_matches_assets_copy() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let input = root.join("corpus/prepared/grass-field-windy-mirror.mp4");
    let committed = root.join("assets/grass-field-windy-mirror.slpy");
    if !input.is_file() || !committed.is_file() {
        eprintln!("skipping: corpus/assets not present on this checkout");
        return;
    }
    let dir = TempDir::new("grass");
    let rebuilt = dir.path("grass.slpy");
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
