use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use auto_ascii_eval::fixtures::{Fixture, build_fixture};

const FIX_W: u32 = 160;
const FIX_H: u32 = 90;
const FIX_FPS: u32 = 30;
const FIX_FRAMES: u32 = 12;

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
        broken["error"].as_str().unwrap().contains("not a valid ASCI asset"),
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
    assert_eq!(v["error"], "play is interactive; run it without --json");
    assert!(!s.home().exists(), "play --json touched the home folder");

    let out = cli(&s, &["--json", "compose", "play", "demo"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_of(&out).is_empty(), "stdout:\n{}", stdout_of(&out));
    let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert_eq!(v["error"], "play is interactive; run it without --json");
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
        v["error"].as_str().unwrap().contains("not a valid ASCI asset"),
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
    assert!(!err.contains("not a valid ASCI asset"), "stderr:\n{err}");
}
