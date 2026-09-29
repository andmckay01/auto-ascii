//! The soundtrack end to end through the real binary under `--sim --sim-audio`:
//! discovery order, duration-mismatch rejection, `--mute`, `--no-audio`, the
//! silent fallback, audio-clock pacing, and no ffmpeg child outliving the run.
//! Skipped when ffmpeg is missing.
#![cfg(feature = "cli")]
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use auto_ascii::audio::source::Tools;
use auto_ascii_eval::fixtures::{Fixture, build_fixture};

struct Stage(PathBuf);

impl Stage {
    fn new(tag: &str) -> Option<Stage> {
        Tools::find().ok().or_else(|| {
            eprintln!("skipping: ffmpeg/ffprobe not found");
            None
        })?;
        let dir = std::env::temp_dir().join(format!("auto-ascii-sound-e2e-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("clip.ascii"), build_fixture(Fixture::GradientMotion)).unwrap();
        Some(Stage(dir))
    }

    fn sine(&self, name: &str, secs: f64) {
        let tools = Tools::find().unwrap();
        let status = Command::new(tools.ffmpeg)
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:sample_rate=44100:duration={secs}"))
            .args(["-ac", "2", "-c:a", "aac"])
            .arg(self.0.join(name))
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn sim(&self, extra: &[&str]) -> (Output, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
            .arg("play")
            .arg(self.0.join("clip.ascii"))
            .args(["--sim", "80x24:15"])
            .args(extra)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{stdout}\n{stderr}");
        assert!(no_child_left(&self.0), "an ffmpeg child outlived the player");
        (out, stdout, stderr)
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn no_child_left(dir: &Path) -> bool {
    let out = Command::new("pgrep").args(["-f"]).arg(dir).output().unwrap();
    out.stdout.is_empty()
}

fn field<'a>(json: &'a str, key: &str) -> &'a str {
    let at = json.find(&format!("\"{key}\":")).unwrap_or_else(|| panic!("{key} missing: {json}"));
    let rest = &json[at + key.len() + 3..];
    let end = rest.find([',', '}']).unwrap();
    rest[..end].trim_matches('"')
}

fn audio(stdout: &str) -> &str {
    let at = stdout.find("\"audio\":").unwrap_or_else(|| panic!("no audio object: {stdout}"));
    &stdout[at..]
}

#[test]
fn sim_audio_plays_the_sidecar_through_the_null_sink_on_the_audio_clock() {
    let Some(stage) = Stage::new("on") else { return };
    stage.sine("clip.m4a", 2.4);
    stage.sine("source.mp4", 2.4);
    let (_, stdout, stderr) = stage.sim(&["--sim-audio"]);
    let a = audio(&stdout);
    assert_eq!(field(a, "sound"), "on", "{a}");
    assert_eq!(field(a, "clock"), "audio", "{a}");
    assert!(field(a, "source").ends_with("clip.m4a"), "the stem beats source.mp4: {a}");
    assert_eq!(field(a, "device"), "null (real-time paced)");
    assert_eq!(field(stdout.as_str(), "frames"), "15");
    let clock: f64 = field(a, "clock_secs").parse().unwrap();
    let wall: f64 = field(a, "wall_secs").parse().unwrap();
    assert!((0.3..=0.55).contains(&clock), "the last frame was picked 14 ticks in: {clock} ({a})");
    assert!(clock <= wall + 0.05, "the audio clock never runs ahead of real time: {a}");
    assert!(field(a, "callbacks").parse::<u64>().unwrap() > 10, "{a}");
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn mute_keeps_the_audio_clock_and_no_audio_falls_back_to_the_wall() {
    let Some(stage) = Stage::new("flags") else { return };
    stage.sine("clip.m4a", 2.4);
    let (_, stdout, _) = stage.sim(&["--sim-audio", "--mute"]);
    let a = audio(&stdout);
    assert_eq!((field(a, "sound"), field(a, "clock")), ("off", "audio"), "{a}");
    let (_, stdout, stderr) = stage.sim(&["--sim-audio", "--no-audio"]);
    let a = audio(&stdout);
    assert_eq!((field(a, "sound"), field(a, "clock")), ("none", "wall"), "{a}");
    assert!(stderr.is_empty(), "--no-audio is not a problem: {stderr}");
}

#[test]
fn no_track_or_a_different_cut_plays_silently() {
    let Some(stage) = Stage::new("fallback") else { return };
    let (_, stdout, stderr) = stage.sim(&["--sim-audio"]);
    let a = audio(&stdout);
    assert_eq!((field(a, "sound"), field(a, "clock")), ("none", "wall"), "{a}");
    assert!(stderr.is_empty(), "{stderr}");
    stage.sine("source.mp4", 12.0);
    let (_, stdout, stderr) = stage.sim(&["--sim-audio"]);
    let a = audio(&stdout);
    assert_eq!(field(a, "sound"), "none", "a 12 s source is not this 2.4 s clip: {a}");
    assert!(stderr.contains("auto-ascii: sound:") && stderr.contains("source.mp4") && stderr.contains("not played"), "{stderr}");
}

#[test]
fn plain_sim_is_untouched_by_a_sidecar() {
    let Some(stage) = Stage::new("plain") else { return };
    stage.sine("clip.m4a", 2.4);
    let (_, stdout, stderr) = stage.sim(&[]);
    assert!(!stdout.contains("\"audio\""), "{stdout}");
    assert!(stdout.contains("\"frames\":15"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
}
