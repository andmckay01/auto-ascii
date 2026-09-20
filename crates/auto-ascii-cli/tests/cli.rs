//! `auto-ascii` end-to-end through the real binary (PLAN-M6-M8 §2 accept).
//!
//! Every test points `AUTO_ASCII_HOME` at its own temp dir, so the suite
//! never touches the developer's `~/auto-ascii` and the cases run in
//! parallel. `import` runs the genuine ffmpeg ingest over a Rust-written
//! raw BGR24 AVI (`auto_ascii_eval::fixtures::write_bgr24_avi`, the same
//! writer the factory's determinism guard uses) — no corpus, and no
//! dependence on how a particular ffmpeg build renders a synthetic source.
//!
//! This box guarantees ffmpeg on PATH (Linux CI, macOS dev boxes).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Fixture geometry: small enough that a build is ~a second, and 160 is a
/// multiple of 4 so the DIB rows need no padding.
const FIX_W: u32 = 160;
const FIX_H: u32 = 90;
const FIX_FPS: u32 = 30;
const FIX_FRAMES: u32 = 12;

/// Self-cleaning per-test scratch dir (no tempfile dep in the workspace):
/// `home/` is `AUTO_ASCII_HOME`, `src/` holds the input videos.
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

    /// Write the fixture AVI as `src/<name>.avi` and return its path.
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

/// One fixture frame in DIB order (rows bottom-up, pixels B,G,R): a colour
/// gradient with a hard-edged bar sweeping across it, so the extracted
/// planes carry real luma, edge and chroma signal.
fn fixture_frame(f: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((FIX_W * FIX_H * 3) as usize);
    for row in 0..FIX_H {
        let y = FIX_H - 1 - row; // bottom-up
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

/// Parse the single JSON value a `--json` run must have put on stdout.
fn json_of(out: &Output) -> serde_json::Value {
    let text = ok(out);
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("stdout is not one JSON value ({e}):\n{text}"))
}

// ---------------------------------------------------------------------------
// home
// ---------------------------------------------------------------------------

#[test]
fn home_prints_and_creates_the_three_folders() {
    let s = Scratch::new("home");
    assert!(!s.home().exists());

    let out = cli(&s, &["home"]);
    assert_eq!(ok(&out).trim_end(), s.home().display().to_string());
    for sub in ["library", "compositions", "exports"] {
        assert!(s.home().join(sub).is_dir(), "{sub}/ was not created");
    }

    // JSON form names all four paths; a second run is a no-op.
    let v = json_of(&cli(&s, &["--json", "home"]));
    assert_eq!(v["home"], s.home().display().to_string());
    assert_eq!(v["library"], s.library().display().to_string());
    assert_eq!(v["compositions"], s.home().join("compositions").display().to_string());
    assert_eq!(v["exports"], s.home().join("exports").display().to_string());
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

#[test]
fn import_writes_the_clip_and_a_sidecar() {
    let s = Scratch::new("import");
    let video = s.video("clip-a");

    let out = cli(&s, &["import", video.to_str().unwrap()]);
    let stdout = ok(&out);
    assert!(stdout.starts_with("imported clip-a\n"), "stdout:\n{stdout}");
    assert!(stdout.contains("frames:       12 (0.40s @ 30 fps)"), "stdout:\n{stdout}");
    assert!(stdout.contains("base res:     480x270"), "stdout:\n{stdout}");
    // The factory's own progress/info lines belong to stderr, always.
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

    // --json import prints exactly the sidecar it wrote, and nothing else.
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
    // The collision is checked BEFORE ffmpeg runs: nothing was rebuilt.
    assert!(!err.contains("pass 1/2:"), "the build ran anyway:\n{err}");

    // The same failure under --json is one {"error": ...} object on stderr.
    let out = cli(&s, &["--json", "import", arg]);
    assert!(!out.status.success());
    let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert!(v["error"].as_str().unwrap().contains("already exists"), "{v}");
    assert!(stdout_of(&out).is_empty());

    // --force rebuilds in place (same input ⇒ same bytes, byte-deterministic).
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

/// `--force` drops the old provenance BEFORE rebuilding, so a build that
/// fails leaves the clip visibly unrecorded rather than described by a
/// sidecar for bytes that were never written.
#[test]
fn force_clears_stale_provenance_even_when_the_build_fails() {
    let s = Scratch::new("stale");
    let video = s.video("clip-a");
    ok(&cli(&s, &["import", video.to_str().unwrap()]));
    let asset = s.library().join("clip-a.ascii");
    let sidecar = s.library().join("clip-a.json");
    let before = std::fs::read(&asset).unwrap();
    assert!(sidecar.is_file());

    // A file ffmpeg cannot read: the ingest fails after the sidecar is gone.
    let junk = s.0.join("src/junk.avi");
    std::fs::write(&junk, b"this is not a video").unwrap();
    let out = cli(&s, &["import", junk.to_str().unwrap(), "--name", "clip-a", "--force"]);
    assert!(!out.status.success(), "a junk input must fail");

    assert!(!sidecar.exists(), "stale provenance survived a failed --force");
    // `<out>.part` means the old asset is untouched, and still lists.
    assert_eq!(std::fs::read(&asset).unwrap(), before);
    let clips = json_of(&cli(&s, &["--json", "list"]));
    assert_eq!(clips[0]["name"], "clip-a");
    assert_eq!(clips[0]["asset"]["frames"], 12);
    assert_eq!(clips[0]["source"], serde_json::Value::Null);
}

// ---------------------------------------------------------------------------
// list / info
// ---------------------------------------------------------------------------

#[test]
fn list_merges_sidecars_and_still_shows_orphans() {
    let s = Scratch::new("list");
    let video = s.video("clip-a");
    ok(&cli(&s, &["import", video.to_str().unwrap()]));
    // An asset with no sidecar beside it: still a clip, just no provenance.
    std::fs::copy(s.library().join("clip-a.ascii"), s.library().join("orphan.ascii")).unwrap();

    let v = json_of(&cli(&s, &["--json", "list"]));
    let clips = v.as_array().expect("list --json is an array");
    assert_eq!(clips.len(), 2, "{v}");
    assert_eq!(clips[0]["name"], "clip-a");
    assert_eq!(clips[1]["name"], "orphan", "clips are sorted by name");
    assert!(clips[0]["source"]["path"].as_str().unwrap().ends_with("clip-a.avi"));
    // The orphan's header facts are real; its provenance is explicitly null.
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

/// One bad file must not hide the library. Every broken shape still gets a
/// row, carries an `error`, and leaves the exit code at 0.
#[test]
fn list_survives_broken_entries() {
    let s = Scratch::new("brokenlist");
    let video = s.video("clip-a");
    ok(&cli(&s, &["import", video.to_str().unwrap()]));
    let lib = s.library();
    // Too short to even hold an ASCI header.
    std::fs::write(lib.join("broken.ascii"), [0u8; 19]).unwrap();
    // A directory that happens to be named like a clip.
    std::fs::create_dir(lib.join("adir.ascii")).unwrap();
    // A valid asset whose sidecar is not JSON.
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

    // A corrupt sidecar loses only the provenance: the header still reads.
    let corrupt = by_name("z-corrupt");
    assert_eq!(corrupt["asset"]["frames"], 12);
    assert_eq!(corrupt["source"], serde_json::Value::Null);
    assert!(corrupt["error"].as_str().unwrap().contains("not a valid sidecar"), "{corrupt}");

    // The healthy clip is untouched, and carries no `error` key at all.
    let good = by_name("clip-a");
    assert_eq!(good["asset"]["frames"], 12);
    assert!(good.get("error").is_none(), "{good}");

    // Human table: `?` where the numbers would be, the reason last.
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

    // A path resolves too, and beats the library lookup.
    let by_path = s.library().join("clip-a.ascii");
    let w = json_of(&cli(&s, &["--json", "info", by_path.to_str().unwrap()]));
    assert_eq!(v, w);

    let out = cli(&s, &["info", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("no clip \"nope\""), "stderr:\n{}", stderr_of(&out));
}

/// Sidecars are read for provenance only, so a hand-written one that
/// names nothing but a source loads — the header facts come off the asset
/// either way.
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
    // Not in the file, so null — and the asset block still came off disk.
    assert_eq!(v["created_unix"], serde_json::Value::Null);
    assert_eq!(v["asset"]["frames"], 12);
    assert!(v.get("error").is_none(), "{v}");
}

// ---------------------------------------------------------------------------
// agent-guide / play
// ---------------------------------------------------------------------------

#[test]
fn agent_guide_prints_the_committed_file() {
    let s = Scratch::new("guide");
    let stdout = ok(&cli(&s, &["agent-guide"]));
    assert!(stdout.starts_with("# auto-ascii for agents\n"), "stdout:\n{stdout:.60}");
    assert!(stdout.contains("## Composition schema"), "stdout:\n{stdout}");
    // include_str! pins it to the file; prove the two are the same bytes.
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/AGENT-GUIDE.md");
    assert_eq!(stdout, std::fs::read_to_string(committed).unwrap());

    let v = json_of(&cli(&s, &["--json", "agent-guide"]));
    assert!(v["guide"].as_str().unwrap().starts_with("# auto-ascii for agents"));
}

#[test]
fn play_says_what_it_cannot_do_yet() {
    let s = Scratch::new("play");
    // The M8 seam: a .toml is a composition, not a broken asset.
    let out = cli(&s, &["play", "demo.toml"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr_of(&out);
    assert!(err.contains("composition") && err.contains("M8"), "stderr:\n{err}");

    let out = cli(&s, &["play", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("no clip \"nope\""), "stderr:\n{}", stderr_of(&out));
}

/// The player owns stdout for its whole run, so there is no stdout left to
/// put one JSON value on. Refused before any terminal session starts.
#[test]
fn play_refuses_json_up_front() {
    let s = Scratch::new("playjson");
    let out = cli(&s, &["--json", "play", "anything"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_of(&out).is_empty(), "stdout:\n{}", stdout_of(&out));
    let v: serde_json::Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert_eq!(v["error"], "play is interactive; run it without --json");
    // The refusal beats even the resolve step: no home folder was needed.
    assert!(!s.home().exists(), "play --json touched the home folder");
}

/// `agent-guide` is pure output, so it must not need a home folder — or a
/// HOME at all, which is the state a sandboxed agent often runs in.
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
