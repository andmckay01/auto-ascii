#![cfg(feature = "cli")]

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use auto_ascii_eval::fixtures::{Fixture, build_fixture};

const FIX_W: u32 = 160;
const FIX_H: u32 = 90;
const FIX_FPS: u32 = 30;
const FIX_FRAMES: u32 = 12;

const PLAY_IS_INTERACTIVE: &str =
    "play is interactive; run it without --json (or add --sim for one JSON stats line)";

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("auto-ascii-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        Scratch(root)
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    fn library(&self) -> PathBuf {
        self.home().join("library")
    }

    fn compositions(&self) -> PathBuf {
        self.home().join("compositions")
    }

    fn clip(&self, fixture: Fixture) -> PathBuf {
        std::fs::create_dir_all(self.library()).unwrap();
        let path = self.library().join(format!("{}.ascii", fixture.name()));
        std::fs::write(&path, build_fixture(fixture)).unwrap();
        path
    }

    fn demo(&self) -> PathBuf {
        self.clip(Fixture::GradientMotion);
        self.clip(Fixture::HardCut);
        ok(&cli(self, &["compose", "new", "demo"]));
        ok(&cli(self, &["compose", "add", "demo", "gradient-motion"]));
        ok(&cli(
            self,
            &[
                "compose", "add", "demo", "hard-cut", "--in", "0:00.5", "--out", "0:01.5",
                "--at", "0:04",
            ],
        ));
        self.compositions().join("demo.toml")
    }

    fn video(&self, name: &str) -> PathBuf {
        let path = self.0.join("src").join(format!("{name}.avi"));
        auto_ascii_eval::fixtures::write_bgr24_avi(
            &path,
            FIX_W,
            FIX_H,
            FIX_FPS,
            (0..FIX_FRAMES).map(fixture_frame),
        )
        .unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture_frame(f: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((FIX_W * FIX_H * 3) as usize);
    for row in 0..FIX_H {
        let y = FIX_H - 1 - row;
        for x in 0..FIX_W {
            let (r, g, b) = if x / 8 == (f + y / 32) % 20 {
                (250u8, 250, 250)
            } else {
                (
                    (x * 255 / (FIX_W - 1)) as u8,
                    (y * 255 / (FIX_H - 1)) as u8,
                    (f * 20) as u8,
                )
            };
            out.extend_from_slice(&[b, g, r]);
        }
    }
    out
}

fn cli(scratch: &Scratch, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env("AUTO_ASCII_HOME", scratch.home())
        .args(args)
        .output()
        .expect("failed to run the auto-ascii binary")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "command failed:\n{}", stderr_of(out));
    stdout_of(out)
}

fn json_of(out: &Output) -> serde_json::Value {
    let text = ok(out);
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("stdout is not one JSON value ({e}):\n{text}"))
}

#[test]
fn home_prints_and_creates_the_three_folders() {
    let s = Scratch::new("home");
    assert!(!s.home().exists());

    let out = cli(&s, &["home"]);
    assert_eq!(ok(&out).trim_end(), s.home().display().to_string());
    for sub in ["library", "compositions", "exports"] {
        assert!(s.home().join(sub).is_dir(), "{sub}/ was not created");
    }

    let v = json_of(&cli(&s, &["--json", "home"]));
    assert_eq!(v["home"], s.home().display().to_string());
    assert_eq!(v["library"], s.library().display().to_string());
    assert_eq!(v["compositions"], s.home().join("compositions").display().to_string());
    assert_eq!(v["exports"], s.home().join("exports").display().to_string());
}

#[test]
fn import_writes_the_clip_and_a_sidecar() {
    let s = Scratch::new("import");
    let video = s.video("clip-a");

    let out = cli(&s, &["import", video.to_str().unwrap()]);
    let stdout = ok(&out);
    assert!(stdout.starts_with("imported clip-a\n"), "stdout:\n{stdout}");
    assert!(stdout.contains("frames:       12 (0.40s @ 30 fps)"), "stdout:\n{stdout}");
    assert!(stdout.contains("base res:     480x270"), "stdout:\n{stdout}");
    assert!(!stdout.contains("wrote "), "factory lines leaked to stdout:\n{stdout}");
    assert!(stderr_of(&out).contains("pass 1/2:"), "stderr:\n{}", stderr_of(&out));

    let asset = s.library().join("clip-a.ascii");
    let sidecar = s.library().join("clip-a.json");
    assert!(asset.is_file(), "no asset at {}", asset.display());
    assert!(sidecar.is_file(), "no sidecar at {}", sidecar.display());

    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(v["name"], "clip-a");
    assert_eq!(v["asset"]["frames"], 12);
    assert_eq!(v["asset"]["fps"], 30.0);
    assert_eq!(v["asset"]["duration_secs"], 0.4);
    assert_eq!(v["asset"]["base_w"], 480);
    assert_eq!(v["asset"]["base_h"], 270);
    assert_eq!(
        v["asset"]["bytes"].as_u64().unwrap(),
        std::fs::metadata(&asset).unwrap().len()
    );
    assert!(
        v["asset"]["path"].as_str().unwrap().ends_with("clip-a.ascii"),
        "asset path: {}",
        v["asset"]["path"]
    );
    assert!(
        v["source"]["path"].as_str().unwrap().ends_with("clip-a.avi"),
        "source path: {}",
        v["source"]["path"]
    );
    assert_eq!(
        v["source"]["bytes"].as_u64().unwrap(),
        std::fs::metadata(&video).unwrap().len()
    );
    let sha = v["source"]["sha256"].as_str().unwrap();
    assert_eq!(sha.len(), 64, "sha256: {sha}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "sha256: {sha}");
    let created = v["created"].as_str().unwrap();
    assert!(v["created_unix"].as_u64().unwrap() > 1_700_000_000, "created_unix: {v}");
    assert!(created.ends_with('Z') && created.len() == 20, "created: {created}");

    let out = cli(&s, &["--json", "import", video.to_str().unwrap(), "--name", "My Clip"]);
    let printed = json_of(&out);
    assert_eq!(printed["name"], "my-clip", "--name must be kebab-cased");
    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(s.library().join("my-clip.json")).unwrap()).unwrap();
    assert_eq!(printed, on_disk);
}

#[test]
fn import_collision_needs_force() {
    let s = Scratch::new("force");
    let video = s.video("clip-a");
    let arg = video.to_str().unwrap();
    ok(&cli(&s, &["import", arg]));
    let first = std::fs::read(s.library().join("clip-a.ascii")).unwrap();

    let out = cli(&s, &["import", arg]);
    assert!(!out.status.success(), "a name collision must fail");
    let err = stderr_of(&out);
    assert!(err.contains("already exists") && err.contains("--force"), "stderr:\n{err}");
    assert!(stdout_of(&out).is_empty(), "stdout:\n{}", stdout_of(&out));
    assert!(!err.contains("pass 1/2:"), "the build ran anyway:\n{err}");

    let out = cli(&s, &["--json", "import", arg]);
    assert!(!out.status.success());
    let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert!(v["error"].as_str().unwrap().contains("already exists"), "{v}");
    assert!(stdout_of(&out).is_empty());

    ok(&cli(&s, &["import", arg, "--force"]));
    assert_eq!(std::fs::read(s.library().join("clip-a.ascii")).unwrap(), first);
}

#[test]
fn bad_timecode_fails_cleanly_in_both_modes() {
    let s = Scratch::new("timecode");
    let video = s.video("clip-a");
    let arg = video.to_str().unwrap();

    let out = cli(&s, &["import", arg, "--ss", "nope"]);
    assert!(!out.status.success(), "a bad --ss must exit nonzero");
    assert_eq!(out.status.code(), Some(1), "exit code");
    let err = stderr_of(&out);
    assert!(err.contains("--ss \"nope\""), "stderr:\n{err}");
    assert!(err.contains("bad timestamp component"), "stderr:\n{err}");
    assert!(stdout_of(&out).is_empty(), "stdout:\n{}", stdout_of(&out));
    assert!(!s.library().join("clip-a.ascii").exists(), "a rejected import built anyway");

    let out = cli(&s, &["--json", "import", arg, "--ss", "1:2:3:4"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_of(&out).is_empty());
    let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert!(
        v["error"].as_str().unwrap().contains("expected SECONDS, MM:SS or HH:MM:SS"),
        "{v}"
    );
}

#[test]
fn force_keeps_the_old_provenance_when_the_rebuild_fails() {
    let s = Scratch::new("stale");
    let video = s.video("clip-a");
    ok(&cli(&s, &["import", video.to_str().unwrap()]));
    let asset = s.library().join("clip-a.ascii");
    let sidecar = s.library().join("clip-a.json");
    let before = std::fs::read(&asset).unwrap();
    let provenance = std::fs::read(&sidecar).unwrap();

    let junk = s.0.join("src/junk.avi");
    std::fs::write(&junk, b"this is not a video").unwrap();
    let out = cli(&s, &["import", junk.to_str().unwrap(), "--name", "clip-a", "--force"]);
    assert!(!out.status.success(), "a junk input must fail");

    assert_eq!(std::fs::read(&asset).unwrap(), before, "the old clip changed");
    assert_eq!(std::fs::read(&sidecar).unwrap(), provenance, "provenance was dropped");
    let clips = json_of(&cli(&s, &["--json", "list"]));
    assert_eq!(clips[0]["name"], "clip-a");
    assert_eq!(clips[0]["asset"]["frames"], 12);
    assert!(
        clips[0]["source"]["path"].as_str().unwrap().ends_with("clip-a.avi"),
        "the surviving sidecar still describes the surviving clip: {}",
        clips[0]
    );

    let other = s.video("clip-b");
    ok(&cli(&s, &["import", other.to_str().unwrap(), "--name", "clip-a", "--force"]));
    let after = json_of(&cli(&s, &["--json", "info", "clip-a"]));
    assert!(
        after["source"]["path"].as_str().unwrap().ends_with("clip-b.avi"),
        "provenance did not follow the new bytes: {after}"
    );
}

#[test]
fn list_merges_sidecars_and_still_shows_orphans() {
    let s = Scratch::new("list");
    let video = s.video("clip-a");
    ok(&cli(&s, &["import", video.to_str().unwrap()]));
    std::fs::copy(s.library().join("clip-a.ascii"), s.library().join("orphan.ascii")).unwrap();

    let v = json_of(&cli(&s, &["--json", "list"]));
    let clips = v.as_array().expect("list --json is an array");
    assert_eq!(clips.len(), 2, "{v}");
    assert_eq!(clips[0]["name"], "clip-a");
    assert_eq!(clips[1]["name"], "orphan", "clips are sorted by name");
    assert!(clips[0]["source"]["path"].as_str().unwrap().ends_with("clip-a.avi"));
    assert_eq!(clips[1]["source"], serde_json::Value::Null);
    assert_eq!(clips[1]["created_unix"], serde_json::Value::Null);
    assert_eq!(clips[1]["created"], serde_json::Value::Null);
    assert_eq!(clips[1]["asset"]["frames"], 12);
    assert_eq!(clips[1]["asset"]["fps"], 30.0);

    let stdout = ok(&cli(&s, &["list"]));
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines[0].starts_with("name"), "header row:\n{stdout}");
    assert!(lines[0].contains("duration") && lines[0].contains("source"), "{stdout}");
    assert!(lines[1].starts_with("clip-a "), "{stdout}");
    assert!(lines[1].contains("0:00") && lines[1].contains("30") && lines[1].contains("12"));
    assert!(lines[2].starts_with("orphan "), "{stdout}");
    assert!(lines[2].trim_end().ends_with('-'), "an orphan's source column is -:\n{stdout}");
}

#[test]
fn list_of_an_untouched_home_is_empty() {
    let s = Scratch::new("emptylist");
    assert_eq!(json_of(&cli(&s, &["--json", "list"])), serde_json::json!([]));
    assert!(ok(&cli(&s, &["list"])).contains("no clips in"));
}

#[test]
fn list_survives_broken_entries() {
    let s = Scratch::new("brokenlist");
    let video = s.video("clip-a");
    ok(&cli(&s, &["import", video.to_str().unwrap()]));
    let lib = s.library();
    std::fs::write(lib.join("broken.ascii"), [0u8; 19]).unwrap();
    std::fs::create_dir(lib.join("adir.ascii")).unwrap();
    std::fs::copy(lib.join("clip-a.ascii"), lib.join("z-corrupt.ascii")).unwrap();
    std::fs::write(lib.join("z-corrupt.json"), b"{ not json at all").unwrap();

    let out = cli(&s, &["--json", "list"]);
    assert_eq!(out.status.code(), Some(0), "a broken entry must not fail the listing");
    let v = json_of(&out);
    let clips = v.as_array().unwrap();
    let by_name = |n: &str| {
        clips.iter().find(|c| c["name"] == n).unwrap_or_else(|| panic!("{n} missing from {v}"))
    };
    assert_eq!(clips.len(), 4, "{v}");

    let dir = by_name("adir");
    assert_eq!(dir["asset"], serde_json::Value::Null);
    assert!(dir["error"].as_str().unwrap().contains("is not a file"), "{dir}");

    let broken = by_name("broken");
    assert_eq!(broken["asset"], serde_json::Value::Null);
    assert!(
        broken["error"].as_str().unwrap().contains("not a valid .ascii asset"),
        "{broken}"
    );

    let corrupt = by_name("z-corrupt");
    assert_eq!(corrupt["asset"]["frames"], 12);
    assert_eq!(corrupt["source"], serde_json::Value::Null);
    assert!(corrupt["error"].as_str().unwrap().contains("not a valid sidecar"), "{corrupt}");

    let good = by_name("clip-a");
    assert_eq!(good["asset"]["frames"], 12);
    assert!(good.get("error").is_none(), "{good}");

    let out = cli(&s, &["list"]);
    let stdout = ok(&out);
    let row = stdout.lines().find(|l| l.starts_with("broken ")).expect(&stdout);
    assert_eq!(row.matches('?').count(), 4, "row: {row}");
    assert!(row.contains("! "), "row: {row}");
    assert!(stdout.lines().any(|l| l.starts_with("clip-a ") && l.contains("12")), "{stdout}");
}

#[test]
fn info_reads_the_header_and_the_sidecar() {
    let s = Scratch::new("info");
    let video = s.video("clip-a");
    ok(&cli(&s, &["import", video.to_str().unwrap()]));

    let stdout = ok(&cli(&s, &["info", "clip-a"]));
    assert!(stdout.starts_with("clip-a\n"), "stdout:\n{stdout}");
    assert!(stdout.contains("frames:       12 (0.40s @ 30 fps)"), "stdout:\n{stdout}");
    assert!(stdout.contains("base res:     480x270"), "stdout:\n{stdout}");
    assert!(stdout.contains("clip-a.avi"), "the source line:\n{stdout}");
    assert!(stdout.contains("created:      "), "stdout:\n{stdout}");

    let v = json_of(&cli(&s, &["--json", "info", "clip-a"]));
    assert_eq!(v["name"], "clip-a");
    assert_eq!(v["asset"]["frames"], 12);

    let by_path = s.library().join("clip-a.ascii");
    let w = json_of(&cli(&s, &["--json", "info", by_path.to_str().unwrap()]));
    assert_eq!(v, w);

    let out = cli(&s, &["info", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("no clip \"nope\""), "stderr:\n{}", stderr_of(&out));
}

#[test]
fn a_hand_written_sidecar_loads() {
    let s = Scratch::new("handwritten");
    let video = s.video("clip-a");
    ok(&cli(&s, &["import", video.to_str().unwrap()]));
    std::fs::copy(s.library().join("clip-a.ascii"), s.library().join("by-hand.ascii")).unwrap();
    std::fs::write(
        s.library().join("by-hand.json"),
        br#"{"source": {"path": "/somewhere/else.mov", "sha256": "ab", "bytes": 7}}"#,
    )
    .unwrap();

    let v = json_of(&cli(&s, &["--json", "info", "by-hand"]));
    assert_eq!(v["name"], "by-hand");
    assert_eq!(v["source"]["path"], "/somewhere/else.mov");
    assert_eq!(v["source"]["bytes"], 7);
    assert_eq!(v["created_unix"], serde_json::Value::Null);
    assert_eq!(v["asset"]["frames"], 12);
    assert!(v.get("error").is_none(), "{v}");
}

#[test]
fn agent_guide_prints_the_committed_file() {
    let s = Scratch::new("guide");
    let stdout = ok(&cli(&s, &["agent-guide"]));
    assert!(stdout.starts_with("# auto-ascii for agents\n"), "stdout:\n{stdout:.60}");
    assert!(stdout.contains("## Composition schema"), "stdout:\n{stdout}");
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/AGENT-GUIDE.md");
    assert_eq!(stdout, std::fs::read_to_string(committed).unwrap());

    let v = json_of(&cli(&s, &["--json", "agent-guide"]));
    assert!(v["guide"].as_str().unwrap().starts_with("# auto-ascii for agents"));
}

#[test]
fn play_resolves_clips_and_compositions() {
    let s = Scratch::new("play");
    let out = cli(&s, &["play", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr_of(&out);
    assert!(err.contains("no clip or composition \"nope\""), "stderr:\n{err}");
    assert!(err.contains("nope.ascii") && err.contains("nope.toml"), "stderr:\n{err}");

    ok(&cli(&s, &["compose", "new", "demo"]));
    for arg in ["demo", s.compositions().join("demo.toml").to_str().unwrap()] {
        let out = cli(&s, &["play", arg]);
        assert_eq!(out.status.code(), Some(1), "play {arg}");
        assert!(
            stderr_of(&out).contains("has no clips"),
            "play {arg} stderr:\n{}",
            stderr_of(&out)
        );
    }

    s.clip(Fixture::GradientMotion);
    let out = cli(&s, &["play", "gradient-motion"]);
    assert!(!stderr_of(&out).contains("no clip or composition"), "{}", stderr_of(&out));
}

#[test]
fn play_refuses_json_up_front() {
    let s = Scratch::new("playjson");
    let out = cli(&s, &["--json", "play", "anything"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_of(&out).is_empty(), "stdout:\n{}", stdout_of(&out));
    let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert_eq!(v["error"], PLAY_IS_INTERACTIVE);
    assert!(!s.home().exists(), "play --json touched the home folder");

    let out = cli(&s, &["--json", "compose", "play", "demo"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_of(&out).is_empty(), "stdout:\n{}", stdout_of(&out));
    let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert_eq!(v["error"], PLAY_IS_INTERACTIVE);
    assert!(!s.home().exists(), "compose play --json touched the home folder");
}

#[test]
fn agent_guide_needs_no_home() {
    let out = Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env_remove("AUTO_ASCII_HOME")
        .arg("agent-guide")
        .output()
        .expect("failed to run the auto-ascii binary");
    assert!(out.status.success(), "stderr:\n{}", stderr_of(&out));
    assert!(stdout_of(&out).starts_with("# auto-ascii for agents\n"));
}

#[test]
fn cut_writes_the_slice_and_its_provenance() {
    let s = Scratch::new("cut");
    s.clip(Fixture::GradientMotion);

    let out = cli(&s, &["cut", "gradient-motion", "--in", "0:01", "--out", "0:02"]);
    let stdout = ok(&out);
    assert!(
        stdout.starts_with("cut gradient-motion-0m01s-0m02s from gradient-motion\n"),
        "stdout:\n{stdout}"
    );
    assert!(stdout.contains("frames:       30 (1.00s @ 30 fps)"), "stdout:\n{stdout}");
    assert!(stdout.contains("source:       cut of gradient-motion [1.00s, 2.00s)"), "{stdout}");

    let asset = s.library().join("gradient-motion-0m01s-0m02s.ascii");
    assert!(asset.is_file(), "no slice at {}", asset.display());
    let part = s.library().join("gradient-motion-0m01s-0m02s.ascii.part");
    assert!(!part.exists(), "the part file survived at {}", part.display());

    let sidecar = s.library().join("gradient-motion-0m01s-0m02s.json");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(v["name"], "gradient-motion-0m01s-0m02s");
    assert_eq!(v["source"]["kind"], "cut");
    assert_eq!(v["source"]["from"], "gradient-motion", "a library clip is named, not pathed");
    assert_eq!(v["source"]["in"], 1.0);
    assert_eq!(v["source"]["out"], 2.0);
    assert_eq!(v["asset"]["frames"], 30);
    assert_eq!(v["asset"]["fps"], 30.0);
    assert_eq!(v["asset"]["duration_secs"], 1.0);
    assert_eq!(
        v["asset"]["bytes"].as_u64().unwrap(),
        std::fs::metadata(&asset).unwrap().len()
    );
    assert!(v["created_unix"].as_u64().unwrap() > 1_700_000_000, "{v}");

    let listed = json_of(&cli(&s, &["--json", "list"]));
    let clips = listed.as_array().unwrap();
    assert_eq!(clips.len(), 2, "{listed}");
    let slice = clips.iter().find(|c| c["name"] == "gradient-motion-0m01s-0m02s").unwrap();
    assert_eq!(slice["asset"]["frames"], 30);
    let table = ok(&cli(&s, &["list"]));
    assert!(
        table.lines().any(|l| l.contains("cut of gradient-motion [1.00s, 2.00s)")),
        "list:\n{table}"
    );
}

#[test]
fn cut_json_is_the_sidecar_it_wrote() {
    let s = Scratch::new("cutjson");
    s.clip(Fixture::HardCut);

    let printed = json_of(&cli(
        &s,
        &["--json", "cut", "hard-cut", "--in", "0.5", "--out", "1", "--name", "My Slice"],
    ));
    assert_eq!(printed["name"], "my-slice", "--name is kebab-cased");
    assert_eq!(printed["asset"]["frames"], 15);
    assert_eq!(printed["source"]["kind"], "cut");
    assert_eq!(printed["source"]["in"], 0.5);
    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(s.library().join("my-slice.json")).unwrap())
            .unwrap();
    assert_eq!(printed, on_disk);

    let out = cli(&s, &["cut", "hard-cut", "--in", "0.5", "--out", "1", "--name", "my-slice"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("already exists"), "stderr:\n{}", stderr_of(&out));
    let before = std::fs::read(s.library().join("my-slice.ascii")).unwrap();
    ok(&cli(
        &s,
        &["cut", "hard-cut", "--in", "0.5", "--out", "1", "--name", "my-slice", "--force"],
    ));
    assert_eq!(std::fs::read(s.library().join("my-slice.ascii")).unwrap(), before);
}

#[test]
fn compose_new_then_add_writes_the_schema() {
    let s = Scratch::new("composenew");
    s.clip(Fixture::GradientMotion);
    s.clip(Fixture::HardCut);

    let stdout = ok(&cli(&s, &["compose", "new", "demo"]));
    assert!(stdout.starts_with("created demo\n"), "stdout:\n{stdout}");
    let path = s.compositions().join("demo.toml");
    assert!(path.is_file(), "no composition at {}", path.display());

    let added = ok(&cli(&s, &["compose", "add", "demo", "gradient-motion"]));
    assert!(added.starts_with("added gradient-motion to demo\n"), "stdout:\n{added}");
    ok(&cli(
        &s,
        &[
            "compose", "add", "demo", "hard-cut", "--in", "0:00.5", "--out", "0:01.5", "--at",
            "0:04",
        ],
    ));

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("schema = 1\nname = \"demo\"\n\n#"), "toml:\n{text}");
    assert!(
        text.ends_with(
            "\n[[clip]]\nasset = \"gradient-motion\"\n\
             \n[[clip]]\nasset = \"hard-cut\"\nin = \"0:00.5\"\nout = \"0:01.5\"\nat = \"0:04\"\n"
        ),
        "toml:\n{text}"
    );
    for key in ["# [[clip]]", "# asset =", "# in =", "# out =", "# at ="] {
        assert!(text.contains(key), "{key} missing from:\n{text}");
    }

    std::fs::write(&path, format!("{text}\n# mine\n")).unwrap();
    ok(&cli(&s, &["compose", "add", "demo", "gradient-motion", "--at", "0:06"]));
    let grown = std::fs::read_to_string(&path).unwrap();
    assert!(grown.starts_with(&format!("{text}\n# mine\n")), "toml:\n{grown}");
    assert!(
        grown.ends_with("\n[[clip]]\nasset = \"gradient-motion\"\nat = \"0:06\"\n"),
        "toml:\n{grown}"
    );
}

#[test]
fn compose_show_reports_the_timeline_and_its_gap() {
    let s = Scratch::new("composeshow");
    s.demo();

    let v = json_of(&cli(&s, &["--json", "compose", "show", "demo"]));
    assert_eq!(v["name"], "demo");
    assert_eq!(v["fps"], 30.0);
    assert_eq!(v["duration_secs"], 5.0, "{v}");
    assert_eq!(v["frame_count"], 150, "2.4 s + 1.6 s gap + 1 s slice at 30 fps");

    let clips = v["clips"].as_array().expect("clips is an array");
    assert_eq!(clips.len(), 2, "{v}");
    assert_eq!(clips[0]["index"], 0);
    assert_eq!(clips[0]["asset"], "gradient-motion");
    assert!(clips[0]["path"].as_str().unwrap().ends_with("gradient-motion.ascii"), "{v}");
    assert_eq!(clips[0]["in_secs"], 0.0);
    assert_eq!(clips[0]["out_secs"], 2.4, "no `out` reports the asset's own end");
    assert_eq!(clips[0]["at_secs"], serde_json::Value::Null, "no `at` is null, not 0");
    assert_eq!(clips[0]["start_secs"], 0.0);
    assert_eq!(clips[0]["end_secs"], 2.4);
    assert_eq!(clips[1]["index"], 1);
    assert_eq!(clips[1]["in_secs"], 0.5);
    assert_eq!(clips[1]["out_secs"], 1.5);
    assert_eq!(clips[1]["at_secs"], 4.0);
    assert_eq!(clips[1]["start_secs"], 4.0);
    assert_eq!(clips[1]["end_secs"], 5.0);

    let gaps = v["gaps"].as_array().expect("gaps is an array");
    assert_eq!(gaps.len(), 1, "{v}");
    assert_eq!(gaps[0]["start_secs"], 2.4);
    assert_eq!(gaps[0]["end_secs"], 4.0);
    assert!(v["overlaps"].as_array().unwrap().is_empty(), "{v}");

    let stdout = ok(&cli(&s, &["compose", "show", "demo"]));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "demo  2 clips  5.00s  150 frames @ 30 fps", "stdout:\n{stdout}");
    assert!(lines[1].contains("start") && lines[1].contains("fps"), "stdout:\n{stdout}");
    assert!(lines[2].contains("gradient-motion") && lines[2].contains("2.40"), "{stdout}");
    assert!(lines[3].contains("GAP") && lines[3].contains("4.00"), "stdout:\n{stdout}");
    assert!(lines[4].contains("hard-cut") && lines[4].contains("5.00"), "stdout:\n{stdout}");
}

#[test]
fn compose_show_marks_an_overlap() {
    let s = Scratch::new("composeoverlap");
    s.clip(Fixture::GradientMotion);
    s.clip(Fixture::HardCut);
    ok(&cli(&s, &["compose", "new", "demo"]));
    ok(&cli(&s, &["compose", "add", "demo", "gradient-motion"]));
    ok(&cli(&s, &["compose", "add", "demo", "hard-cut", "--at", "0:01"]));

    let v = json_of(&cli(&s, &["--json", "compose", "show", "demo"]));
    let overlaps = v["overlaps"].as_array().expect("overlaps is an array");
    assert_eq!(overlaps.len(), 1, "{v}");
    assert_eq!(overlaps[0]["start_secs"], 1.0);
    assert_eq!(overlaps[0]["end_secs"], 2.4);
    assert_eq!(overlaps[0]["under"], 0);
    assert_eq!(overlaps[0]["over"], 1, "the later-listed clip is on top");
    assert!(v["gaps"].as_array().unwrap().is_empty(), "{v}");

    let stdout = ok(&cli(&s, &["compose", "show", "demo"]));
    assert!(stdout.lines().any(|l| l.contains("OVERLAP #0")), "stdout:\n{stdout}");
    assert!(
        stdout.lines().any(|l| l.contains("gradient-motion") && l.contains("UNDER #1")),
        "stdout:\n{stdout}"
    );
}

#[test]
fn compose_show_marks_a_hidden_clip() {
    let s = Scratch::new("composehidden");
    s.clip(Fixture::GradientMotion);
    s.clip(Fixture::HardCut);
    ok(&cli(&s, &["compose", "new", "demo"]));
    ok(&cli(&s, &[
        "compose", "add", "demo", "gradient-motion", "--out", "0:00.5", "--at", "0:01",
    ]));
    ok(&cli(&s, &["compose", "add", "demo", "hard-cut", "--at", "0:00"]));

    let v = json_of(&cli(&s, &["--json", "compose", "show", "demo"]));
    let overlaps = v["overlaps"].as_array().unwrap();
    assert_eq!(overlaps.len(), 1, "{v}");
    assert_eq!(overlaps[0]["start_secs"], 1.0);
    assert_eq!(overlaps[0]["end_secs"], 1.5, "the cover spans clip 0 whole");

    let stdout = ok(&cli(&s, &["compose", "show", "demo"]));
    assert!(
        stdout.lines().any(|l| l.contains("gradient-motion") && l.contains("HIDDEN #1")),
        "stdout:\n{stdout}"
    );
    assert!(!stdout.contains("UNDER"), "a swallowed clip is HIDDEN, not UNDER:\n{stdout}");
    assert!(stdout.lines().any(|l| l.contains("hard-cut") && l.contains("OVERLAP #0")), "{stdout}");
}

#[test]
fn a_float_length_does_not_invent_an_overlap() {
    let s = Scratch::new("phantom");
    s.clip(Fixture::GradientMotion);
    s.clip(Fixture::HardCut);
    ok(&cli(&s, &["compose", "new", "demo"]));
    ok(&cli(&s, &[
        "compose", "add", "demo", "gradient-motion", "--in", "0.7", "--out", "1.0",
    ]));
    ok(&cli(&s, &["compose", "add", "demo", "hard-cut", "--at", "0.3"]));

    let v = json_of(&cli(&s, &["--json", "compose", "show", "demo"]));
    assert!(v["overlaps"].as_array().unwrap().is_empty(), "phantom overlap: {v}");
    assert!(v["gaps"].as_array().unwrap().is_empty(), "phantom gap: {v}");
    assert_eq!(v["clips"][0]["end_secs"], 0.3);
    assert_eq!(v["clips"][1]["start_secs"], 0.3);
    assert_eq!(v["duration_secs"], 2.7);
    assert_eq!(v["frame_count"], 81, "9 frames + 72 at 30 fps");

    let stdout = ok(&cli(&s, &["compose", "show", "demo"]));
    assert!(!stdout.contains("OVERLAP"), "stdout:\n{stdout}");
    assert!(!stdout.contains("UNDER"), "stdout:\n{stdout}");
    assert!(!stdout.contains("GAP"), "stdout:\n{stdout}");
}

#[test]
fn names_may_carry_their_folders_extension() {
    let s = Scratch::new("extensions");
    s.demo();

    let bare = json_of(&cli(&s, &["--json", "compose", "show", "demo"]));
    let dotted = json_of(&cli(&s, &["--json", "compose", "show", "demo.toml"]));
    assert_eq!(bare, dotted);
    let clip = json_of(&cli(&s, &["--json", "info", "gradient-motion"]));
    let clip_dotted = json_of(&cli(&s, &["--json", "info", "gradient-motion.ascii"]));
    assert_eq!(clip, clip_dotted);

    ok(&cli(&s, &["compose", "new", "second.toml"]));
    assert!(s.compositions().join("second.toml").is_file(), "compose new kept the extension");
    assert!(!s.compositions().join("second-toml.toml").exists());

    let out = cli(&s, &["compose", "show", "ghost.toml"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr_of(&out);
    assert!(err.contains("no composition \"ghost.toml\""), "stderr:\n{err}");
    assert!(err.contains("`auto-ascii compose new ghost`"), "stderr:\n{err}");
}

#[test]
fn a_rejected_cut_leaves_the_old_clip_whole() {
    let s = Scratch::new("cutreject");
    s.clip(Fixture::GradientMotion);
    ok(&cli(&s, &["cut", "gradient-motion", "--in", "0", "--out", "1", "--name", "slice"]));
    let asset = s.library().join("slice.ascii");
    let sidecar = s.library().join("slice.json");
    let bytes = std::fs::read(&asset).unwrap();
    let provenance = std::fs::read(&sidecar).unwrap();

    let out = cli(&s, &[
        "cut", "gradient-motion", "--in", "0", "--out", "9999", "--name", "slice", "--force",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("past the end"), "stderr:\n{}", stderr_of(&out));
    assert_eq!(std::fs::read(&asset).unwrap(), bytes, "the old slice changed");
    assert_eq!(std::fs::read(&sidecar).unwrap(), provenance, "the provenance was dropped");
    let v = json_of(&cli(&s, &["--json", "info", "slice"]));
    assert_eq!(v["source"]["kind"], "cut");
    assert_eq!(v["asset"]["frames"], 30);
}

#[test]
fn add_validates_before_it_appends() {
    let s = Scratch::new("addvalidate");
    s.clip(Fixture::GradientMotion);
    s.clip(Fixture::HardCut);
    ok(&cli(&s, &["compose", "new", "demo"]));
    ok(&cli(&s, &["compose", "add", "demo", "gradient-motion"]));
    let path = s.compositions().join("demo.toml");
    let sound = std::fs::read_to_string(&path).unwrap();

    let refuses = |args: &[&str], needle: &str| {
        let out = cli(&s, args);
        assert_eq!(out.status.code(), Some(1), "{args:?} should have failed");
        assert!(stdout_of(&out).is_empty(), "{args:?} stdout:\n{}", stdout_of(&out));
        let err = stderr_of(&out);
        assert!(err.contains(needle), "{args:?}: {err:?} lacks {needle:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), sound, "{args:?} edited the file");
    };
    refuses(
        &["compose", "add", "demo", "hard-cut", "--in", "5"],
        "must be greater than",
    );
    refuses(&["compose", "add", "demo", "hard-cut", "--out", "9"], "past the end");

    let work = s.0.join("src");
    std::fs::write(work.join("a"), b"").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env("AUTO_ASCII_HOME", s.home())
        .current_dir(&work)
        .args(["--json", "compose", "add", "demo", "a"])
        .output()
        .expect("failed to run the auto-ascii binary");
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert!(
        v["error"].as_str().unwrap().contains("not a valid .ascii asset"),
        "{v}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), sound, "the file was edited");

    std::fs::write(&path, format!("{sound}bogus = 1\n")).unwrap();
    let broken = std::fs::read_to_string(&path).unwrap();
    let out = cli(&s, &["--json", "compose", "add", "demo", "hard-cut"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert!(v["error"].as_str().unwrap().contains("unknown key \"bogus\""), "{v}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), broken, "the file was edited");
}

#[test]
fn concurrent_adds_both_land() {
    let s = Scratch::new("addrace");
    s.clip(Fixture::GradientMotion);
    ok(&cli(&s, &["compose", "new", "demo"]));
    let path = s.compositions().join("demo.toml");

    let children: Vec<_> = ["--at", "--in"]
        .iter()
        .map(|flag| {
            let value = if *flag == "--at" { "0:01" } else { "0:00.5" };
            Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
                .env("AUTO_ASCII_HOME", s.home())
                .args(["compose", "add", "demo", "gradient-motion", flag, value])
                .stdout(Stdio::null())
                .spawn()
                .expect("failed to run the auto-ascii binary")
        })
        .collect();
    for mut child in children {
        assert!(child.wait().expect("wait").success());
    }

    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("[[clip]]").count(), 3, "one commented, two added:\n{text}");
    assert!(text.contains("at = \"0:01\""), "toml:\n{text}");
    assert!(text.contains("in = \"0:00.5\""), "toml:\n{text}");
    let v = json_of(&cli(&s, &["--json", "compose", "show", "demo"]));
    assert_eq!(v["clips"].as_array().unwrap().len(), 2, "{v}");
}

#[test]
fn a_closed_pipe_ends_the_run_quietly() {
    let s = Scratch::new("epipe");
    std::fs::create_dir_all(s.library()).unwrap();
    for i in 0..700 {
        std::fs::write(s.library().join(format!("clip-{i:04}.ascii")), [0u8; 19]).unwrap();
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env("AUTO_ASCII_HOME", s.home())
        .arg("list")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run the auto-ascii binary");
    let mut first = String::new();
    {
        let stdout = child.stdout.as_mut().expect("piped stdout");
        std::io::BufReader::new(stdout).read_line(&mut first).expect("read one row");
    }
    drop(child.stdout.take());
    let out = child.wait_with_output().expect("wait");

    assert!(first.starts_with("name"), "first line: {first:?}");
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", stderr_of(&out));
    assert!(!stderr_of(&out).contains("panic"), "stderr:\n{}", stderr_of(&out));

    let mut child = Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env("AUTO_ASCII_HOME", s.home())
        .arg("agent-guide")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run the auto-ascii binary");
    drop(child.stdout.take());
    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", stderr_of(&out));
    assert!(!stderr_of(&out).contains("panic"), "stderr:\n{}", stderr_of(&out));
}

#[test]
fn a_partial_sidecar_still_describes_its_clip() {
    let s = Scratch::new("partial");
    let lib = s.library();
    s.clip(Fixture::GradientMotion);
    std::fs::write(
        lib.join("gradient-motion.json"),
        br#"{"source": {"path": "/videos/a.mp4"}}"#,
    )
    .unwrap();

    let v = json_of(&cli(&s, &["--json", "info", "gradient-motion"]));
    assert_eq!(v["source"]["path"], "/videos/a.mp4");
    assert_eq!(v["source"]["sha256"], serde_json::Value::Null);
    assert_eq!(v["source"]["bytes"], serde_json::Value::Null);
    assert_eq!(v["asset"]["frames"], 72, "the header is read either way");
    let text = ok(&cli(&s, &["info", "gradient-motion"]));
    assert!(text.contains("source:       /videos/a.mp4"), "stdout:\n{text}");
    assert!(text.contains("source sha:   (unknown)"), "stdout:\n{text}");
    assert!(text.contains("source bytes: (unknown)"), "stdout:\n{text}");

    std::fs::copy(lib.join("gradient-motion.ascii"), lib.join("slice.ascii")).unwrap();
    std::fs::write(lib.join("slice.json"), br#"{"source": {"kind": "cut"}}"#).unwrap();
    let v = json_of(&cli(&s, &["--json", "info", "slice"]));
    assert_eq!(v["source"]["kind"], "cut");
    assert_eq!(v["source"]["from"], serde_json::Value::Null);
    let table = ok(&cli(&s, &["list"]));
    assert!(table.contains("cut of (unknown)"), "list:\n{table}");

    std::fs::write(lib.join("slice.json"), br#"{"source": {"sha256": "ab"}}"#).unwrap();
    let out = cli(&s, &["info", "slice"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr_of(&out);
    assert!(err.contains("slice.json"), "stderr:\n{err}");
    assert!(err.contains("not a valid sidecar"), "stderr:\n{err}");
    let listed = json_of(&cli(&s, &["--json", "list"]));
    let row = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "slice")
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(row["asset"]["frames"], 72);
    assert!(row["error"].as_str().unwrap().contains("not a valid sidecar"), "{row}");
}

#[test]
fn compose_export_flattens_and_the_file_re_enters_the_library() {
    let s = Scratch::new("composeexport");
    s.demo();

    let v = json_of(&cli(&s, &["--json", "compose", "export", "demo"]));
    let out = s.home().join("exports").join("demo.ascii");
    assert!(out.is_file(), "no export at {}", out.display());
    assert!(v["path"].as_str().unwrap().ends_with("demo.ascii"), "{v}");
    assert_eq!(v["fps"], 30.0);
    assert_eq!(v["bytes"].as_u64().unwrap(), std::fs::metadata(&out).unwrap().len());
    assert!(v["shots"].as_u64().unwrap() >= 1, "{v}");
    assert!(v["cuts"].as_u64().unwrap() >= 1, "every clip boundary is a cut: {v}");

    let show = json_of(&cli(&s, &["--json", "compose", "show", "demo"]));
    assert_eq!(v["frames"], show["frame_count"], "export {v} vs show {show}");

    std::fs::copy(&out, s.library().join("demo-flat.ascii")).unwrap();
    let listed = json_of(&cli(&s, &["--json", "list"]));
    let flat = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "demo-flat")
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(flat["asset"]["frames"], show["frame_count"]);
    assert_eq!(flat["asset"]["fps"], 30.0);
    assert_eq!(flat["source"], serde_json::Value::Null, "no sidecar, still a clip");
    assert!(flat.get("error").is_none(), "{flat}");

    let again = cli(&s, &["compose", "export", "demo"]);
    assert_eq!(again.status.code(), Some(1));
    let err = stderr_of(&again);
    assert!(err.contains("already exists") && err.contains("--force"), "stderr:\n{err}");
    let stdout = ok(&cli(&s, &["compose", "export", "demo", "--force"]));
    assert!(stdout.starts_with("exported demo\n"), "stdout:\n{stdout}");
    assert!(stdout.contains("frames:       150 (5.00s @ 30 fps)"), "stdout:\n{stdout}");
    let elsewhere = s.0.join("src/flat.ascii");
    ok(&cli(&s, &["compose", "export", "demo", "-o", elsewhere.to_str().unwrap()]));
    assert!(elsewhere.is_file(), "no export at {}", elsewhere.display());
}

#[test]
fn compose_and_cut_errors_are_json_objects() {
    let s = Scratch::new("composeerr");
    s.clip(Fixture::GradientMotion);
    ok(&cli(&s, &["compose", "new", "demo"]));

    let fails = |args: &[&str], needle: &str| {
        let out = cli(&s, args);
        assert_eq!(out.status.code(), Some(1), "{args:?} should have failed");
        assert!(stdout_of(&out).is_empty(), "{args:?} stdout:\n{}", stdout_of(&out));
        let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim())
            .unwrap_or_else(|e| panic!("{args:?} stderr is not JSON ({e}):\n{}", stderr_of(&out)));
        let text = v["error"].as_str().unwrap_or_else(|| panic!("{args:?}: {v}"));
        assert!(text.contains(needle), "{args:?}: {text:?} lacks {needle:?}");
    };

    fails(&["--json", "compose", "add", "demo", "nope"], "no clip \"nope\"");
    fails(&["--json", "compose", "show", "ghost"], "no composition \"ghost\"");
    fails(&["--json", "compose", "new", "demo"], "already exists");
    fails(&["--json", "compose", "show", "demo"], "has no clips");
    fails(
        &["--json", "cut", "gradient-motion", "--in", "0:02", "--out", "0:01"],
        "must be later than",
    );
    fails(&["--json", "cut", "gradient-motion", "--in", "0", "--out", "nope"], "--out \"nope\"");
    fails(&["--json", "cut", "nope", "--in", "0", "--out", "1"], "no clip \"nope\"");
    fails(&["--json", "cut", "gradient-motion", "--in", "0", "--out", "9"], "past the end");
}

#[test]
fn compose_reads_a_toml_from_anywhere() {
    let s = Scratch::new("composepath");
    let clip = s.clip(Fixture::GradientMotion);
    let loose = s.0.join("src/loose.toml");
    std::fs::write(
        &loose,
        format!("schema = 1\n[[clip]]\nasset = \"{}\"\n", clip.display()),
    )
    .unwrap();

    let v = json_of(&cli(&s, &["--json", "compose", "show", loose.to_str().unwrap()]));
    assert_eq!(v["name"], "loose", "the file stem names an unnamed composition");
    assert_eq!(v["frame_count"], 72);
    assert_eq!(v["clips"][0]["start_secs"], 0.0);
    assert_eq!(v["clips"][0]["end_secs"], 2.4);

    let report = json_of(&cli(&s, &["--json", "compose", "export", loose.to_str().unwrap()]));
    assert_eq!(report["frames"], 72);
    assert!(s.home().join("exports").join("loose.ascii").is_file());
}

#[test]
fn usage_errors_keep_the_json_contract() {
    let s = Scratch::new("usage");
    let cases: [(&[&str], &str); 4] = [
        (&["--json", "cut", "clip", "--in", "0"], "--out"),
        (&["--json", "list", "--bogus"], "--bogus"),
        (&["--json", "import", "x.mp4", "--fps", "abc"], "abc"),
        (&["--json", "bogus"], "bogus"),
    ];
    for (args, needle) in cases {
        let out = cli(&s, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(stdout_of(&out).is_empty(), "{args:?} stdout:\n{}", stdout_of(&out));
        let v: serde_json::Value =
            serde_json::from_str(stderr_of(&out).trim()).unwrap_or_else(|e| {
                panic!("{args:?} stderr is not one JSON value ({e}):\n{}", stderr_of(&out))
            });
        let text = v["error"].as_str().unwrap_or_else(|| panic!("{args:?}: {v}"));
        assert!(text.contains(needle), "{args:?}: {text:?} lacks {needle:?}");
        assert!(!text.contains('\u{1b}'), "terminal styling leaked into JSON: {text:?}");
        assert!(!text.starts_with("error: "), "the key already says it: {text:?}");
    }

    let out = cli(&s, &["cut", "clip", "--in", "0"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("--out"), "stderr:\n{}", stderr_of(&out));
    assert!(stdout_of(&out).is_empty());
}

#[test]
fn help_and_version_are_not_errors() {
    let s = Scratch::new("help");
    for args in [&["--help"][..], &["--json", "--help"][..]] {
        let out = cli(&s, args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(stdout_of(&out).contains("Usage:"), "{args:?} stdout:\n{}", stdout_of(&out));
    }
    let out = cli(&s, &["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout_of(&out).starts_with("auto-ascii "), "stdout:\n{}", stdout_of(&out));
}

#[test]
fn an_empty_home_variable_is_no_home() {
    let out = Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env("HOME", "")
        .env("USERPROFILE", "")
        .env_remove("AUTO_ASCII_HOME")
        .arg("home")
        .output()
        .expect("failed to run the auto-ascii binary");
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_of(&out).is_empty(), "stdout:\n{}", stdout_of(&out));
    assert!(
        stderr_of(&out).contains("cannot find your home directory"),
        "stderr:\n{}",
        stderr_of(&out)
    );
}

#[test]
fn play_needs_the_header_not_the_sidecar() {
    let s = Scratch::new("playsidecar");
    s.clip(Fixture::GradientMotion);
    std::fs::write(s.library().join("gradient-motion.json"), b"{ truncated").unwrap();

    let out = cli(&s, &["info", "gradient-motion"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("not a valid sidecar"), "stderr:\n{}", stderr_of(&out));

    let out = cli(&s, &["play", "gradient-motion"]);
    assert!(
        !stderr_of(&out).contains("not a valid sidecar"),
        "a truncated sidecar blocked playback:\n{}",
        stderr_of(&out)
    );
}

#[test]
fn an_uppercase_toml_path_is_a_composition() {
    let s = Scratch::new("upperext");
    s.clip(Fixture::GradientMotion);
    let loose = s.0.join("src/Demo.TOML");
    std::fs::write(&loose, "schema = 1\n[[clip]]\nasset = \"ghost\"\n").unwrap();

    let out = cli(&s, &["play", loose.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr_of(&out);
    assert!(err.contains("ghost"), "stderr:\n{err}");
    assert!(!err.contains("not a valid .ascii asset"), "stderr:\n{err}");
}

#[test]
fn stream_help_lists_its_flags() {
    let s = Scratch::new("streamhelp");
    let text = ok(&cli(&s, &["stream", "--help"]));
    for flag in ["--style", "--palette", "--max-height", "--no-audio", "--cookies-from-browser", "URL|TERMS"] {
        assert!(text.contains(flag), "{flag} missing from:\n{text}");
    }
    assert!(text.contains("yt-dlp"), "{text}");
    assert!(!text.contains("--sim"), "the headless flags are advanced:\n{text}");
    let all = ok(&cli(&s, &["stream", "--help-all"]));
    assert!(all.contains("--sim <COLSxROWS:SECONDS>") && all.contains("--sim-dump"), "{all}");
}

#[test]
fn stream_refuses_json_without_sim() {
    let s = Scratch::new("streamjson");
    let out = cli(&s, &["--json", "stream", "https://www.youtube.com/watch?v=jNQXAC9IVRw"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_of(&out).is_empty(), "stdout:\n{}", stdout_of(&out));
    let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert_eq!(
        v["error"],
        "stream is interactive; run it without --json (or add --sim for one JSON stats line)"
    );
    assert!(!s.home().exists(), "stream --json touched the home folder");
    let out = cli(&s, &["stream", "zoo", "--sim-dump", "/tmp/x.txt"]);
    assert_eq!(out.status.code(), Some(2), "--sim-dump needs --sim");
}

fn fake_ytdlp(s: &Scratch, body: &str) -> PathBuf {
    let path = s.0.join("yt-dlp");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn stream_cli(s: &Scratch, ytdlp: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env("AUTO_ASCII_HOME", s.home())
        .env("AUTO_ASCII_YTDLP", ytdlp)
        .args(args)
        .output()
        .expect("failed to run the auto-ascii binary")
}

#[test]
fn stream_passes_browser_cookies_to_ytdlp_only_when_requested() {
    let s = Scratch::new("streamcookies");
    let log = s.0.join("argv.log");
    let ytdlp = fake_ytdlp(
        &s,
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\necho 'ERROR: test refusal' >&2\nexit 1",
            log.display()
        ),
    );
    for browser in [None, Some("firefox")] {
        let mut args = vec!["stream", "https://www.youtube.com/watch?v=localfixture", "--sim", "40x12:20"];
        if let Some(browser) = browser {
            args.extend(["--cookies-from-browser", browser]);
        }
        let out = stream_cli(&s, &ytdlp, &args);
        assert_eq!(out.status.code(), Some(1));
        assert!(stderr_of(&out).contains("test refusal"), "{}", stderr_of(&out));
        let logged = std::fs::read_to_string(&log).unwrap();
        let call: Vec<_> = logged.lines().collect();
        assert_eq!(call[0], "--ignore-config");
        if let Some(browser) = browser {
            let at = call.iter().position(|arg| *arg == "--cookies-from-browser").unwrap();
            assert_eq!(call[at + 1], browser);
            assert_eq!(call.iter().filter(|arg| arg.starts_with("--cookies")).count(), 1);
            assert!(at > 0 && at + 1 < call.len() - 2);
        } else {
            assert!(!call.iter().any(|arg| arg.starts_with("--cookies")), "{call:?}");
        }
        assert_eq!(call[call.len() - 2], "--");
    }
}

#[test]
fn stream_sim_plays_a_resolved_video_to_the_end_and_cleans_up() {
    let s = Scratch::new("streamsim");
    let video = s.0.join("src").join("long.avi");
    auto_ascii_eval::fixtures::write_bgr24_avi(&video, FIX_W, FIX_H, FIX_FPS, (0..45).map(fixture_frame)).unwrap();
    let json = serde_json::json!({
        "_type": "video", "id": "localfixture", "title": "Local fixture", "duration": 1.5,
        "fps": 30, "width": FIX_W, "height": FIX_H, "url": video, "format_id": "avi",
        "protocol": "file", "vcodec": "rawvideo", "acodec": "none",
    });
    let ytdlp = fake_ytdlp(&s, &format!("cat <<'JSON'\n{json}\nJSON"));
    let dump = s.0.join("dump.txt");
    let out = stream_cli(
        &s,
        &ytdlp,
        &[
            "stream",
            "https://www.youtube.com/watch?v=localfixture",
            "--sim",
            "60x20:30",
            "--sim-dump",
            dump.to_str().unwrap(),
        ],
    );
    let v = json_of(&out);
    assert_eq!(v["exit_reason"], "eof", "{v}");
    assert_eq!(v["id"], "localfixture");
    assert_eq!(v["error"], serde_json::Value::Null);
    let rendered = v["frames_rendered"].as_u64().unwrap();
    let dropped = v["frames_dropped"].as_u64().unwrap();
    assert!(rendered > 30, "{v}");
    assert_eq!(rendered + dropped, 45, "every decoded frame is shown or counted as dropped: {v}");
    assert!(v["max_drift_ms"].as_f64().unwrap() < 1000.0 / 30.0 + 15.0, "{v}");
    assert_eq!(v["children_spawned"], 2, "yt-dlp and one ffmpeg: {v}");
    assert_eq!(v["children_alive"], 0);
    assert_eq!(v["temp_dir_removed"], true);
    assert_eq!(v["temp_files_written"], 0, "nothing is written to disk: {v}");
    let stages: Vec<(String, u64)> = v["loader"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["stage"].as_str().unwrap().to_string(), s["percent"].as_u64().unwrap()))
        .collect();
    assert_eq!(stages.first().map(|s| s.1), Some(5), "{stages:?}");
    assert!(stages.contains(&("media_ready".to_string(), 30)), "{stages:?}");
    assert_eq!(stages.last(), Some(&("buffered".to_string(), 100)), "{stages:?}");
    assert!(stages.windows(2).all(|w| w[0].1 <= w[1].1), "{stages:?}");
    assert!(v["notes"].to_string().contains("no audio track"), "{v}");
    let text = std::fs::read_to_string(&dump).unwrap();
    assert!(text.starts_with("== loader ("), "{text}");
    assert!(text.contains("loading...") && text.contains("== picture (frame "), "{text}");
}

#[test]
fn stream_surfaces_a_yt_dlp_refusal_as_one_clean_line() {
    let s = Scratch::new("streamerr");
    let ytdlp = fake_ytdlp(
        &s,
        "echo 'ERROR: [youtube] abcDEF12345: Sign in to confirm your age. This video may be inappropriate for some users.' >&2\nexit 1",
    );
    let url = "https://www.youtube.com/watch?v=abcDEF12345";
    let out = stream_cli(&s, &ytdlp, &["--json", "stream", url, "--sim", "40x12:20"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_str(stdout_of(&out).trim()).unwrap();
    assert_eq!(v["exit_reason"], "error");
    assert_eq!(v["children_alive"], 0);
    assert_eq!(v["temp_dir_removed"], true);
    let reason = "yt-dlp: Sign in to confirm your age. This video may be inappropriate for some users.";
    let e: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert_eq!(e["error"], reason);

    let out = stream_cli(&s, &ytdlp, &["stream", url, "--sim", "40x12:20"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr_of(&out), format!("auto-ascii: {reason}\n"));

    let missing = s.0.join("no-such-yt-dlp");
    let out = stream_cli(&s, &missing, &["stream", url, "--sim", "40x12:20"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).starts_with("auto-ascii: failed to run yt-dlp (is it installed and on PATH?)"),
        "{}",
        stderr_of(&out)
    );
}

#[cfg(unix)]
#[test]
fn stream_survives_repeated_signals_and_still_cleans_up() {
    let s = Scratch::new("streamsig");
    let ytdlp = fake_ytdlp(&s, "exec sleep 30");
    let child = Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env("AUTO_ASCII_HOME", s.home())
        .env("AUTO_ASCII_YTDLP", &ytdlp)
        .args(["stream", "https://www.youtube.com/watch?v=abcDEF12345", "--sim", "40x12:60"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let kids = Command::new("pgrep").args(["-P", &pid.to_string()]).output().unwrap();
        if !kids.stdout.is_empty() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "yt-dlp never started");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    std::thread::sleep(std::time::Duration::from_millis(3));
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1), "killed outright instead of cleaning up: {:?}", out.status);
    let v: serde_json::Value = serde_json::from_str(stdout_of(&out).trim())
        .unwrap_or_else(|e| panic!("no stats line ({e}): {}", stdout_of(&out)));
    assert_eq!(v["exit_reason"], "signal");
    assert_eq!(v["children_alive"], 0);
    assert_eq!(v["temp_dir_removed"], true);
    assert_eq!(stderr_of(&out), "auto-ascii: stopped by signal 15\n");
}

#[cfg(unix)]
#[test]
fn a_third_signal_forces_an_exit_that_still_leaves_nothing_behind() {
    use std::os::unix::process::ExitStatusExt;
    let s = Scratch::new("streamsig3");
    let ytdlp = fake_ytdlp(&s, "exec sleep 30");
    let mut child = Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env("AUTO_ASCII_HOME", s.home())
        .env("AUTO_ASCII_YTDLP", &ytdlp)
        .args(["stream", "https://www.youtube.com/watch?v=abcDEF12345", "--sim", "40x12:60"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let kid: i32 = loop {
        let kids = Command::new("pgrep").args(["-P", &pid.to_string()]).output().unwrap();
        if let Some(first) = String::from_utf8_lossy(&kids.stdout).split_whitespace().next() {
            break first.parse().unwrap();
        }
        assert!(std::time::Instant::now() < deadline, "yt-dlp never started");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    unsafe {
        libc::kill(pid, libc::SIGSTOP);
        libc::kill(pid, libc::SIGHUP);
        libc::kill(pid, libc::SIGINT);
        libc::kill(pid, libc::SIGTERM);
        libc::kill(pid, libc::SIGCONT);
    }
    let status = child.wait().unwrap();
    assert!(status.signal().is_some(), "three signals must force the exit: {status:?}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while unsafe { libc::kill(kid, 0) } == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_ne!(unsafe { libc::kill(kid, 0) }, 0, "the forced exit left yt-dlp running");
    let prefix = format!("auto-ascii-stream-{pid}-");
    let left: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .collect();
    assert!(left.is_empty(), "the forced exit left its scratch dir: {left:?}");
}

fn toolless(s: &Scratch, args: &[&str]) -> (Output, PathBuf) {
    let empty = s.0.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let cache = s.0.join("cache");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_auto-ascii"));
    for var in ["AUTO_ASCII_FFMPEG", "AUTO_ASCII_FFPROBE", "AUTO_ASCII_YTDLP", "AUTO_ASCII_YES", "AUTO_ASCII_NO_DOWNLOAD"] {
        cmd.env_remove(var);
    }
    let out = cmd
        .env("AUTO_ASCII_HOME", s.home())
        .env("AUTO_ASCII_CACHE_DIR", &cache)
        .env("PATH", &empty)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to run the auto-ascii binary");
    (out, cache)
}

#[test]
fn doctor_reports_every_tool_missing_without_creating_the_cache() {
    let s = Scratch::new("doctor");
    let (out, cache) = toolless(&s, &["--json", "doctor"]);
    let v = json_of(&out);
    assert_eq!(v["bin_dir"], cache.join("bin").display().to_string());
    assert_eq!(v["cache_dir"], cache.display().to_string());
    let tools = v["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["ffmpeg", "ffprobe", "yt-dlp"]);
    for t in tools {
        assert_eq!(t["source"], "missing", "{t}");
        assert!(t["path"].is_null() && t["cached"].is_null(), "{t}");
    }
    assert!(!cache.exists(), "doctor only looks");

    let (out, _) = toolless(&s, &["doctor"]);
    let text = ok(&out);
    assert!(text.contains("\nffmpeg     missing  not found\n"), "{text}");
    assert!(text.contains("auto-ascii doctor --fetch --yes"), "{text}");
}

#[test]
fn a_missing_tool_without_yes_fails_before_any_download() {
    let s = Scratch::new("needyes");
    let video = s.0.join("src").join("clip.mp4");
    std::fs::write(&video, b"not really a video").unwrap();
    let (out, cache) = toolless(&s, &["--json", "import", video.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let e: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    let msg = e["error"].as_str().unwrap();
    assert!(msg.starts_with("ffmpeg and ffprobe not found; auto-ascii can download standalone builds"), "{msg}");
    for needle in ["--json never prompts", "--yes", "AUTO_ASCII_YES=1", "auto-ascii doctor --fetch --yes"] {
        assert!(msg.contains(needle), "{needle:?} missing from {msg}");
    }
    assert!(stdout_of(&out).is_empty());

    let (out, _) = toolless(&s, &["stream", "me at the zoo", "--sim", "40x12:5"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr_of(&out);
    assert!(err.starts_with("auto-ascii: yt-dlp and ffmpeg not found; "), "{err}");
    assert!(err.contains("a non-interactive run never prompts"), "{err}");
    assert!(stdout_of(&out).is_empty(), "no stats line: the stream never started");
    assert!(!cache.exists(), "nothing downloaded without consent");
}

#[test]
fn downloads_can_be_turned_off() {
    let s = Scratch::new("nodownload");
    let empty = s.0.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env("AUTO_ASCII_HOME", s.home())
        .env("AUTO_ASCII_CACHE_DIR", s.0.join("cache"))
        .env("AUTO_ASCII_NO_DOWNLOAD", "1")
        .env("AUTO_ASCII_YES", "1")
        .env_remove("AUTO_ASCII_YTDLP")
        .env_remove("AUTO_ASCII_FFMPEG")
        .env("PATH", &empty)
        .args(["stream", "zoo", "--sim", "40x12:5"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = stderr_of(&out);
    assert!(err.contains("AUTO_ASCII_NO_DOWNLOAD is set: install them yourself"), "{err}");
    assert!(!s.0.join("cache").exists());
}

fn json_line(out: &Output) -> serde_json::Value {
    let text = ok(out);
    assert_eq!(text.lines().count(), 1, "exactly one stdout line:\n{text}");
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not JSON ({e}):\n{text}"))
}

#[test]
fn import_to_a_path_writes_only_that_file() {
    let s = Scratch::new("importpath");
    let video = s.video("clip-a");
    let arg = video.to_str().unwrap();
    let dest = s.0.join("out.ascii");
    let dest_arg = dest.to_str().unwrap();

    let stdout = ok(&cli(&s, &["import", arg, "-o", dest_arg]));
    assert!(stdout.starts_with(&format!("wrote {dest_arg}\n")), "stdout:\n{stdout}");
    assert!(stdout.contains("frames:       12 (0.40s @ 30 fps)"), "stdout:\n{stdout}");
    assert!(dest.is_file());
    assert!(!s.0.join("out.json").exists(), "-o writes no sidecar");
    assert!(!s.home().exists(), "-o must not create the library");

    ok(&cli(&s, &["import", arg]));
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        std::fs::read(s.library().join("clip-a.ascii")).unwrap(),
        "both destinations build the same bytes"
    );

    let out = cli(&s, &["import", arg, "-o", dest_arg, "--fps", "10"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("already exists") && stderr_of(&out).contains("--force"));
    assert!(!stderr_of(&out).contains("pass 1/2:"), "the build ran anyway");

    let v = json_line(&cli(&s, &["--json", "import", arg, "-o", dest_arg, "--fps", "10", "--force"]));
    assert_eq!(v["frames"], 4);
    assert_eq!(v["fps"], 10.0);
    assert_eq!((v["base_w"].as_u64(), v["base_h"].as_u64()), (Some(480), Some(270)));
    assert_eq!(v["bytes"].as_u64().unwrap(), std::fs::metadata(&dest).unwrap().len());
    assert!(std::path::Path::new(v["path"].as_str().unwrap()).is_absolute(), "{v}");
    assert!(v["duration_secs"].as_f64().is_some(), "{v}");

    let out = cli(&s, &["import", arg, "-o", s.0.join("no/such/dir/x.ascii").to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("does not exist"), "{}", stderr_of(&out));

    let out = cli(&s, &["import", arg, "-o", dest_arg, "--name", "x"]);
    assert_eq!(out.status.code(), Some(2), "--name and -o conflict");
    let out = cli(&s, &["import", arg, "-o", dest_arg, "--force", "--t", "0"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("longer than zero"), "{}", stderr_of(&out));
}

#[test]
fn headless_play_prints_one_json_line_with_or_without_json() {
    let s = Scratch::new("playsim");
    s.clip(Fixture::GradientMotion);
    for args in [
        &["play", "gradient-motion", "--sim", "80x24:5"][..],
        &["--json", "play", "gradient-motion", "--sim", "80x24:5"],
        &["dev", "sim", "gradient-motion", "--sim", "80x24:5"],
    ] {
        let v = json_line(&cli(&s, args));
        assert_eq!(v["frames"], 5, "{args:?}");
        assert_eq!(v["grid_after"], "80x24", "{args:?}");
        assert_eq!(v["tier"], "truecolor", "{args:?}");
    }
    let v = json_line(&cli(&s, &["play", "gradient-motion", "--sim", "80x24:4", "--sim-tier", "mono", "--sim-resize"]));
    assert_eq!((v["tier"].as_str(), v["grid_after"].as_str()), (Some("mono"), Some("100x40")));

    for args in [
        &["play", "gradient-motion", "--bench-seek", "3"][..],
        &["dev", "bench-seek", "gradient-motion", "--bench-seek", "3"],
    ] {
        let v = json_line(&cli(&s, args));
        assert_eq!(v["seeks"], 3, "{args:?}");
        assert!(v["p95_ms"].as_f64().is_some(), "{args:?}: {v}");
    }

    s.demo();
    let v = json_line(&cli(&s, &["compose", "play", "demo", "--sim", "60x20:3"]));
    assert_eq!(v["frames"], 3);
    let v = json_line(&cli(&s, &["play", "demo", "--sim", "60x20:2"]));
    assert_eq!(v["frames"], 2);
}

#[test]
fn dev_commands_answer_in_text_or_json() {
    let s = Scratch::new("devjson");
    let clip = s.0.join("hard-cut.ascii");
    std::fs::write(&clip, build_fixture(Fixture::HardCut)).unwrap();
    let clip = clip.to_str().unwrap();

    let text = ok(&cli(&s, &["dev", "inspect", clip]));
    assert!(text.contains("integrity:    OK (all chunk CRCs verified, TRLR present)"), "{text}");
    let v = json_line(&cli(&s, &["--json", "dev", "inspect", clip, "--frame", "0"]));
    assert_eq!(v["integrity"], "ok");
    assert_eq!(v["sampled_frames"], serde_json::json!([0]));
    assert!(v["plane_stats"].as_array().is_some_and(|p| !p.is_empty()), "{v}");

    let text = ok(&cli(&s, &["dev", "params", "--dump"]));
    assert!(text.contains("[build]"), "{text}");
    let v = json_line(&cli(&s, &["dev", "params", "--dump", "--json"]));
    assert_eq!(v["build"]["fps"], 30);

    let table = s.0.join("t.toml");
    let v = json_line(&cli(&s, &["--json", "dev", "font-table", "--conservative", "-o", table.to_str().unwrap()]));
    assert_eq!(v["name"], "conservative");
    assert!(table.is_file());
    assert!(!s.home().exists(), "dev commands never touch the library");
}

#[test]
fn dev_inspect_quotes_meta_text_and_says_when_no_crcs_were_written() {
    use auto_ascii_format::{AsciiWriter, Meta, PlaneRef, WriterOptions, header::plane_id};

    let s = Scratch::new("inspectnocrc");
    let opts = WriterOptions { with_crc: false, ..WriterOptions::default() };
    let y = vec![0u8; usize::from(opts.base_w) * usize::from(opts.base_h)];
    let meta = Meta { factory_version: "evil\x1b]0;X\x07".into(), source: "synthetic".into(), palette_hints: vec![] };
    let mut writer = AsciiWriter::new(std::io::Cursor::new(Vec::new()), opts, &meta).unwrap();
    writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &y }]).unwrap();
    let clip = s.0.join("no-crc.ascii");
    std::fs::write(&clip, writer.finish().unwrap().into_inner()).unwrap();

    let text = ok(&cli(&s, &["dev", "inspect", clip.to_str().unwrap()]));
    assert!(text.contains("integrity:    OK (no chunk CRCs present, TRLR present)"), "{text}");
    assert!(text.contains(r#"factory "evil\u{1b}]0;X\u{7}" | source "synthetic""#), "{text}");
    assert!(!text.contains(['\x1b', '\x07']), "{text:?}");
}

#[test]
fn help_all_needs_no_target_and_no_home() {
    let s = Scratch::new("helpall");
    for args in [&["--help-all"][..], &["play", "--help-all"], &["compose", "play", "--help-all"], &["dev", "--help-all"]] {
        let text = ok(&cli(&s, args));
        assert!(text.contains("Usage: auto-ascii"), "{args:?}:\n{text}");
    }
    let text = ok(&cli(&s, &["play", "--help-all"]));
    assert!(text.contains("--bench-seek") && text.contains("--sim-audio"), "{text}");
    assert!(!s.home().exists(), "--help-all touched the home folder");
}

fn media_tools() -> Option<auto_ascii::audio::source::Tools> {
    auto_ascii::audio::source::Tools::find().ok().or_else(|| {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        None
    })
}

fn sounding_video(tools: &auto_ascii::audio::source::Tools, path: &Path) {
    let status = Command::new(&tools.ffmpeg)
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=160x90:rate=30:duration=2"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=2"])
        .args(["-c:a", "aac", "-pix_fmt", "yuv420p", "-shortest"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not make {}", path.display());
}

fn launch(folder: &Path, extra: &str) -> Output {
    Command::new(folder.join("play.command")).args(extra.split_whitespace()).current_dir("/").output().unwrap()
}

#[test]
fn add_turns_a_local_file_into_a_playable_folder() {
    let Some(tools) = media_tools() else { return };
    let s = Scratch::new("addfile");
    let video = s.0.join("src").join("Kiki's Clip.mkv");
    sounding_video(&tools, &video);
    let arg = video.to_str().unwrap();
    let lib = s.0.join("run");
    let lib_arg = lib.to_str().unwrap();

    let v = json_of(&cli(&s, &["--json", "add", arg, "--library", lib_arg]));
    let folder = lib.join("kiki-s-clip");
    assert_eq!(v["name"], "kiki-s-clip", "{v}");
    assert_eq!(v["title"], "Kiki's Clip");
    assert!(v["url"].is_null());
    for name in ["Kiki's Clip.ascii", "Kiki's Clip.m4a", "Kiki's Clip.json", "play.command", "distill.log"] {
        assert!(folder.join(name).is_file(), "{name} missing from {}", folder.display());
    }
    assert!(!folder.join("source.mp4").exists(), "a local file is not copied");
    assert!(!lib.join("kiki-s-clip.partial").exists(), "the staging folder was moved into place");
    assert!(matches!(v["asset"]["frames"].as_u64(), Some(60 | 61)), "{v}");
    assert_eq!((v["asset"]["base_w"].as_u64(), v["asset"]["base_h"].as_u64()), (Some(480), Some(270)));
    assert!((v["source_duration_secs"].as_f64().unwrap() - 2.0).abs() < 0.1, "{v}");
    assert!(v["source"]["path"].as_str().unwrap().ends_with("Kiki's Clip.mkv"), "{v}");
    assert!(v["soundtrack"].as_str().unwrap().ends_with("kiki-s-clip/Kiki's Clip.m4a"), "{v}");
    assert!(std::fs::read_to_string(folder.join("distill.log")).unwrap().contains("pass 1/2:"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(folder.join("play.command")).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "the launcher is executable");
    }

    let played = launch(&folder, "--sim 40x12:5 --no-audio");
    assert!(played.status.success(), "{}", stderr_of(&played));
    let stats: serde_json::Value = serde_json::from_slice(&played.stdout).unwrap();
    assert_eq!(stats["frames"], 5, "{stats}");

    let again = cli(&s, &["add", arg, "--library", lib_arg]);
    assert_eq!(again.status.code(), Some(1));
    assert!(stderr_of(&again).contains("already exists — pass --force to replace it"), "{}", stderr_of(&again));
    let text = ok(&cli(&s, &["add", arg, "--library", lib_arg, "--force"]));
    assert!(text.starts_with("added Kiki's Clip\n"), "{text}");
    assert!(text.contains("integrity OK, length matches the source"), "{text}");
    assert!(!text.contains("play:"), "a custom library is not the home library: {text}");

    let text = ok(&cli(&s, &["add", arg, "--title", "Demo Reel"]));
    assert!(text.contains("auto-ascii play demo-reel\n"), "{text}");
    assert!(s.library().join("demo-reel").join("Demo Reel.ascii").is_file());
    let listed = json_of(&cli(&s, &["--json", "list"]));
    let row = listed.as_array().unwrap().iter().find(|c| c["name"] == "demo-reel").expect("listed");
    assert!(matches!(row["asset"]["frames"].as_u64(), Some(60 | 61)), "{row}");
    let out = cli(&s, &["play", "Demo Reel", "--sim", "40x12:5", "--no-audio"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(json_of(&cli(&s, &["--json", "info", "demo-reel"]))["name"], "demo-reel");
}

#[test]
fn add_downloads_a_link_through_ytdlp_without_cookies() {
    let Some(tools) = media_tools() else { return };
    let s = Scratch::new("addlink");
    let fixture = s.0.join("src").join("fixture.mp4");
    sounding_video(&tools, &fixture);
    let log = s.0.join("argv.log");
    let meta = serde_json::json!({
        "_type": "video", "id": "fixture", "title": "Fake: Movie/Clip", "duration": 2,
        "url": "https://v.example/18", "format_id": "18", "vcodec": "avc1", "acodec": "mp4a", "height": 90,
    });
    let body = format!(
        r#"out=""
prev=""
for a in "$@"; do printf '%s\n' "$a" >> '{log}'; [ "$prev" = "-o" ] && out="$a"; prev="$a"; done
printf 'END_CALL\n' >> '{log}'
case "$*" in *--simulate*) cat <<'JSON'
{meta}
JSON
exit 0;; esac
echo "[download] Destination: $out"
cp '{fixture}' "$(printf '%s' "$out" | sed 's/%(ext)s/mp4/')""#,
        log = log.display(),
        fixture = fixture.display(),
    );
    let ytdlp = fake_ytdlp(&s, &body);
    let lib = s.0.join("run");
    let url = "https://www.youtube.com/watch?v=fixture";
    let out = stream_cli(&s, &ytdlp, &["--json", "add", url, "--library", lib.to_str().unwrap()]);
    let v = json_of(&out);
    let folder = lib.join("fake-movie-clip");
    assert_eq!(v["title"], "Fake- Movie-Clip", "{v}");
    assert_eq!(v["url"], url);
    for name in ["source.mp4", "download.log", "Fake- Movie-Clip.ascii", "Fake- Movie-Clip.m4a", "play.command"] {
        assert!(folder.join(name).is_file(), "{name} missing from {}", folder.display());
    }
    assert!(v["source"]["path"].as_str().unwrap().ends_with("fake-movie-clip/source.mp4"), "{v}");
    assert!(std::fs::read_to_string(folder.join("download.log")).unwrap().contains("[download] Destination: "));

    let logged = std::fs::read_to_string(&log).unwrap();
    let calls: Vec<Vec<&str>> =
        logged.split("END_CALL\n").filter(|c| !c.is_empty()).map(|c| c.lines().collect()).collect();
    assert_eq!(calls.len(), 2, "one metadata call, one download: {calls:?}");
    assert!(calls[0].contains(&"--simulate"), "{:?}", calls[0]);
    assert!(calls[1].contains(&"--no-playlist") && !calls[1].contains(&"--simulate"), "{:?}", calls[1]);
    for call in &calls {
        assert_eq!(call[0], "--ignore-config", "{call:?}");
        assert!(!call.iter().any(|a| a.contains("cookies")), "never browser cookies: {call:?}");
        assert_eq!(call[call.len() - 2..], ["--", url], "{call:?}");
    }
}

fn fake_ytdlp_for_page(s: &Scratch, entry_url: &str, download: &str) -> PathBuf {
    let entry = serde_json::json!({ "webpage_url": entry_url });
    let meta = serde_json::json!({
        "_type": "video", "id": "v", "title": "Evil", "duration": 2,
        "url": "https://v.example/18", "format_id": "18", "vcodec": "avc1", "acodec": "mp4a", "height": 90,
    });
    let body = format!(
        r#"case "$*" in *--flat-playlist*) cat <<'JSON'
{entry}
JSON
exit 0;; *--simulate*) cat <<'JSON'
{meta}
JSON
exit 0;; esac
{download}"#
    );
    fake_ytdlp(s, &body)
}

#[test]
fn add_scrubs_terminal_controls_from_what_a_remote_site_sends() {
    let Some(_) = media_tools() else { return };
    let s = Scratch::new("addescape");
    let download = format!(
        r#"printf '[download] Destination: source.mp4\r\n'
printf '[download]   1.0%% of 2.00MiB\r[download]  50.0%% of 2.00MiB {progress}\r\n'
printf 'frame=    1 fps=0.0\rframe=   60 fps=30\n' >&2
echo 'ERROR: [generic] v: Unable to download webpage: HTTP Error 404: {reason}' >&2
exit 1"#,
        progress = "\x1b]0;LINE\x07",
        reason = "\x1b]0;X\x07\u{9d}52;c;SGVsbG8=\u{9c}",
    );
    let ytdlp = fake_ytdlp_for_page(&s, "https://evil.example/v\x1b]0;URL\x07\nauto-ascii: forged", &download);
    let out = stream_cli(&s, &ytdlp, &["add", "https://evil.example/v"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = stderr_of(&out);
    assert!(!stderr.contains(['\x1b', '\x07', '\u{9c}', '\u{9d}']), "{stderr:?}");
    assert!(!stderr.lines().any(|line| line.starts_with("auto-ascii: forged")), "{stderr}");
    assert!(!stderr.contains("1.0% of") && !stderr.contains("frame=    1"), "overwritten progress: {stderr}");
    for scrubbed in [
        "auto-ascii: downloading https://evil.example/v?]0;URL??auto-ascii: forged (up to 1080p)\n",
        "[download] Destination: source.mp4\n",
        "[download]  50.0% of 2.00MiB ?]0;LINE?\n",
        "frame=   60 fps=30\n",
        "ERROR: [generic] v: Unable to download webpage: HTTP Error 404: ?]0;X??52;c;SGVsbG8=?\n",
        "auto-ascii: yt-dlp: Unable to download webpage: HTTP Error 404: ?]0;X??52;c;SGVsbG8=? (log: ",
    ] {
        assert!(stderr.contains(scrubbed), "{scrubbed:?} missing from {stderr:?}");
    }

    let out = stream_cli(&s, &ytdlp, &["--json", "add", "https://evil.example/v"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!stderr_of(&out).contains(['\u{9c}', '\u{9d}']), "{:?}", stderr_of(&out));
    let e: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    let error = e["error"].as_str().unwrap();
    let scrubbed = "yt-dlp: Unable to download webpage: HTTP Error 404: ?]0;X??52;c;SGVsbG8=? (log: ";
    assert!(error.starts_with(scrubbed) && !error.contains(char::is_control), "{error:?}");
}

#[test]
fn add_scrubs_the_link_a_page_chose_in_text_and_json() {
    let Some(tools) = media_tools() else { return };
    let s = Scratch::new("addlinkescape");
    let fixture = s.0.join("src").join("fixture.mp4");
    sounding_video(&tools, &fixture);
    let download = format!(
        r#"out=""
prev=""
for a in "$@"; do [ "$prev" = "-o" ] && out="$a"; prev="$a"; done
cp '{fixture}' "$(printf '%s' "$out" | sed 's/%(ext)s/mp4/')""#,
        fixture = fixture.display(),
    );
    let ytdlp = fake_ytdlp_for_page(&s, "https://evil.example/v\u{9d}52;c;SGVsbG8=\u{9c}\nforged: line", &download);

    let out = stream_cli(&s, &ytdlp, &["--json", "add", "https://evil.example/v"]);
    assert!(!stdout_of(&out).contains(['\u{9c}', '\u{9d}']), "{:?}", stdout_of(&out));
    assert_eq!(json_of(&out)["url"], "https://evil.example/v?52;c;SGVsbG8=?\nforged: line");

    let out = stream_cli(&s, &ytdlp, &["add", "https://evil.example/v", "--force"]);
    let text = ok(&out);
    let link = format!("  {:<14}{}\n", "link:", "https://evil.example/v?52;c;SGVsbG8=??forged: line");
    assert!(text.contains(&link), "{link:?} missing from {text}");
    assert!(!text.lines().any(|line| line.starts_with("forged")), "{text}");
    assert!(!text.contains(['\u{9c}', '\u{9d}']) && !stderr_of(&out).contains(['\u{9c}', '\u{9d}']), "{text:?}");
}

#[cfg(unix)]
#[test]
fn list_and_info_scrub_the_names_a_shipped_folder_brings() {
    let s = Scratch::new("shippedname");
    let hostile = "evil\x1b]0;X\x07\u{9d}52;c;SGVsbG8=\u{9c}\nforged";
    let folder = s.library().join(hostile);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(format!("{hostile}.ascii")), build_fixture(Fixture::HardCut)).unwrap();

    let listed = ok(&cli(&s, &["list"]));
    let shown = ok(&cli(&s, &["info", hostile]));
    for text in [&listed, &shown] {
        assert!(!text.contains(|c: char| c.is_control() && c != '\n'), "{text:?}");
        assert!(!text.lines().any(|line| line.starts_with("forged")), "{text}");
        assert!(text.contains("evil?]0;X??52;c;SGVsbG8=??forged"), "{text}");
    }
    assert!(shown.contains("evil?]0;X??52;c;SGVsbG8=??forged.ascii\n"), "the asset path: {shown}");
}

#[test]
fn add_refuses_what_it_would_clobber_and_leaves_nothing_behind_on_failure() {
    let Some(tools) = media_tools() else { return };
    let s = Scratch::new("addguard");
    let video = s.0.join("src").join("clip.mp4");
    sounding_video(&tools, &video);
    let arg = video.to_str().unwrap();

    ok(&cli(&s, &["import", arg, "--name", "flat"]));
    let out = cli(&s, &["add", arg, "--title", "flat"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("flat.ascii already exists — pass --force"), "{}", stderr_of(&out));
    ok(&cli(&s, &["add", arg, "--title", "flat", "--force"]));
    assert!(!s.library().join("flat.ascii").exists() && !s.library().join("flat.json").exists());
    let listed = json_of(&cli(&s, &["--json", "list"]));
    let names: Vec<&str> = listed.as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["flat"], "{listed}");

    let clip = s.library().join("flat").join("flat.ascii");
    let out = cli(&s, &["add", clip.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("is already a clip: auto-ascii play "), "{}", stderr_of(&out));

    let staged = s.library().join("inside.partial");
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::copy(&video, staged.join("source.mp4")).unwrap();
    std::fs::copy(&clip, staged.join("ghost.ascii")).unwrap();
    let out = cli(&s, &["add", staged.join("source.mp4").to_str().unwrap(), "--title", "inside"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("which add replaces; move it out first"), "{}", stderr_of(&out));
    assert!(staged.join("source.mp4").is_file(), "the input was not deleted");
    let listed = json_of(&cli(&s, &["--json", "list"]));
    assert_eq!(listed.as_array().unwrap().len(), 1, "a staging folder is not a clip: {listed}");
    assert_eq!(cli(&s, &["play", "inside.partial", "--sim", "40x12:5", "--no-audio"]).status.code(), Some(1));

    let broken = s.0.join("src").join("broken.mp4");
    std::fs::write(&broken, b"not a video").unwrap();
    let out = cli(&s, &["add", broken.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!s.library().join("broken.partial").exists(), "a failed add removes its staging folder");
    assert!(!s.library().join("broken").exists());
}
