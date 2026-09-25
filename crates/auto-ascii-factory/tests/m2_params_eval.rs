use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn repo_params_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../params.toml")
}

#[test]
fn params_dump_matches_committed_file_and_merges_overrides() {
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
    assert_eq!(merged["build"]["fps"], committed["build"]["fps"]);
    assert_eq!(merged["shots"], committed["shots"]);

    let out = factory(&[&"params"]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("--dump"), "stderr:\n{}", stderr_of(&out));
}

#[test]
fn params_validation_rejects_degenerate_geometry() {
    let dir = TempDir::new("badparams");
    let input = dir.path("unused.mp4");

    let p = dir.path("bad.toml");
    std::fs::write(&p, "[build]\nbase_w = 1\n").unwrap();
    let out = factory(&[&"build", &input, &"-o", &dir.path("x.ascii"), &"--params", &p]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("even and >= 2"),
        "stderr should state the §4 geometry rule:\n{}",
        stderr_of(&out)
    );

    let out = factory(&[&"build", &input, &"-o", &dir.path("x.ascii"), &"--res", &"479x270"]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("even"), "stderr:\n{}", stderr_of(&out));

    let p2 = dir.path("typo.toml");
    std::fs::write(&p2, "[build]\nfsp = 30\n").unwrap();
    let out = factory(&[&"params", &"--dump", &"--params", &p2]);
    assert!(!out.status.success());
}

const FIXTURE_AVI_SHA: &str = "4c9a29d515e2184a3f592e30912dedbdb3e453901233136a249d8c56e382cd5d";
const FIXTURE_ASSET_SHA: &str = "7b301c1b2d649cf9ba43ac46010c5b4cb00aab892a404049a3671bdd7a59fb6f";

const FIX_W: usize = 480;
const FIX_H: usize = 270;
const FIX_FPS: u32 = 30;
const FIX_FRAMES: usize = 30;
const FIX_CUT: usize = 15;

type FixRect = (i32, i32, i32, i32, (u8, u8, u8));

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

fn fill_rect(rgb: &mut [u8], x0: i32, y0: i32, w: i32, h: i32, c: (u8, u8, u8)) {
    for y in y0.max(0)..(y0 + h).min(FIX_H as i32) {
        for x in x0.max(0)..(x0 + w).min(FIX_W as i32) {
            put_px(rgb, x as usize, y as usize, c);
        }
    }
}

fn fix_frame_rgb(f: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; FIX_W * FIX_H * 3];
    let cut = f >= FIX_CUT;
    let phase = (f % FIX_CUT) * 4;
    let step = phase as i32;

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

    let band = if cut { 20..70 } else { 120..170 };
    for y in band {
        for x in 0..FIX_W {
            let lit = ((x + y + phase) / 12).is_multiple_of(2);
            put_px(&mut rgb, x, y, if lit { (245, 245, 30) } else { (20, 20, 80) });
        }
    }

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

    let (hx, hy) = if cut { (380 - step, 200) } else { (60 + step, 215) };
    fill_rect(&mut rgb, hx, hy, 24, 24, (255, 255, 255));
    rgb
}

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

fn synth_fixture(dir: &TempDir) -> PathBuf {
    let input = dir.path("fixture.avi");
    auto_ascii_eval::fixtures::write_bgr24_avi(
        &input,
        FIX_W as u32,
        FIX_H as u32,
        FIX_FPS,
        (0..FIX_FRAMES).map(fix_frame_bgr_bottom_up),
    )
    .unwrap();
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

    let params_copy = dir.path("params-copy.toml");
    std::fs::copy(repo_params_path(), &params_copy).unwrap();
    let out_filed = dir.path("filed.ascii");
    let out = factory(&[&"build", &input, &"-o", &out_filed, &"--params", &params_copy]);
    assert!(out.status.success(), "build failed:\n{}", stderr_of(&out));
    assert_eq!(sha256_of(&out_filed), FIXTURE_ASSET_SHA, "--params file path diverged");
}

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

    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &params, &"--out", &out_json,
        &"--html", &out_html, &"--reel", &out_reel, &"--cache-dir", &cache,
    ]);
    assert!(out.status.success(), "eval failed:\n{}", stderr_of(&out));

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_json).unwrap()).unwrap();
    assert_eq!(json["schema_version"], 2);
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
        let bytes = |t: &str| tiers[t]["avg_bytes_per_frame"].as_f64().unwrap();
        assert!(bytes("truecolor") > bytes("256") && bytes("256") > bytes("mono"));
        for stage in ["decode", "resample", "compose", "present"] {
            assert!(
                m["stage_ms"][stage]["frames"].as_u64().unwrap() > 0,
                "stage {stage} unpopulated"
            );
        }
    }
    let flicker_b = clips[1]["metrics"]["flicker_switches_per_cell_sec"].as_f64().unwrap();
    assert!(flicker_b < 5.0, "cut leaked into flicker: {flicker_b}");
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

    let html = std::fs::read_to_string(&out_html).unwrap();
    assert!(html.contains("<title>auto-ascii eval</title>"));
    assert!(html.contains("clip-a") && html.contains("clip-b"));
    let pngs = html.matches("data:image/png;base64,iVBOR").count();
    assert!(pngs >= 8, "expected >= 8 embedded PNGs (source+render x snaps x clips), got {pngs}");
    assert!(!html.contains("http://") && !html.contains("https://"), "must be self-contained");
    assert!(html.contains("edge F1 vs source Canny"), "contact sheet reports edge F1");

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

    let out2_json = dir.path("runs/run2.json");
    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &params, &"--baseline", &out_json,
        &"--out", &out2_json, &"--cache-dir", &cache,
    ]);
    assert!(out.status.success(), "self-baseline compare must pass:\n{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("cached asset"), "second run must hit the asset cache");
    assert!(stderr_of(&out).contains("baseline: PASS"));

    let mut tampered: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_json).unwrap()).unwrap();
    tampered["clips"][0]["metrics"]["ssim"] = serde_json::json!(0.999);
    tampered["clips"][1]["metrics"]["damage_by_tier"]["truecolor"]["avg_bytes_per_frame"] =
        serde_json::json!(1000.0);
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
    assert!(err.contains("cached asset"), "eval knobs must not rebuild assets:\n{err}");

    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &params, &"--baseline", &out_json,
        &"--out", &dir.path("runs/run5.json"), &"--cache-dir", &cache,
    ]);
    assert!(out.status.success(), "reverted params must pass:\n{}", stderr_of(&out));

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

    let kf600 = dir.path("p-kf600.toml");
    std::fs::write(&kf600, "[build]\nkeyframe_ivl = 600\n").unwrap();
    let out = factory(&[
        &"eval", &"--corpus", &corpus, &"--params", &kf600, &"--baseline", &out_json,
        &"--out", &dir.path("runs/run8.json"), &"--cache-dir", &cache,
    ]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("1..=255"), "stderr:\n{}", stderr_of(&out));
}

#[test]
fn sweep_ranks_combos_and_reuses_the_asset_cache() {
    let dir = TempDir::new("sweep");
    let corpus = synth_corpus(&dir);
    let params = dir.path("p.toml");
    std::fs::write(&params, EVAL_PARAMS).unwrap();
    let cache = dir.path("cache");
    let out_dir = dir.path("sweeps/edge");

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
    let sane = results.iter().find(|r| r["combo"].as_str().unwrap().contains("=32")).unwrap();
    let absurd = results.iter().find(|r| r["combo"].as_str().unwrap().contains("=240")).unwrap();
    assert!(
        sane["edge_f1_mean"].as_f64().unwrap() >= absurd["edge_f1_mean"].as_f64().unwrap(),
        "sane {} vs absurd {}",
        sane["edge_f1_mean"],
        absurd["edge_f1_mean"]
    );
    for r in results.iter().take(2) {
        assert_eq!(r["clips"].as_array().unwrap().len(), 2, "both clips scored");
        let rep = r["report"].as_str().unwrap();
        let combo_report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out_dir.join(rep)).unwrap()).unwrap();
        assert_eq!(combo_report["schema_version"], 2);
        let tiers =
            combo_report["clips"][0]["metrics"]["damage_by_tier"].as_object().unwrap();
        assert_eq!(tiers.len(), 1, "sweep runs the truecolor pass only");
        assert!(tiers.contains_key("truecolor"));
    }

    let html = std::fs::read_to_string(out_dir.join("leaderboard.html")).unwrap();
    assert!(html.contains("<title>auto-ascii sweep leaderboard</title>"));
    assert!(html.contains("skipped:"));
    assert!(!html.contains("http://") && !html.contains("https://"), "must be self-contained");

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
