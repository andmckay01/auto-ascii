#![cfg(all(unix, feature = "bin"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Stage(PathBuf);

impl Stage {
    fn new(tag: &str, statuses: &[i32]) -> Stage {
        let dir = std::env::temp_dir()
            .join(format!("auto-ascii-play-with-sound-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let codes: String = statuses.iter().map(|c| format!("{c}\n")).collect();
        fs::write(dir.join("codes"), codes).unwrap();
        script(
            &dir.join("player"),
            &format!(
                "if [ \"$1\" = --help ]; then exec \"{real}\" --help; fi\n\
                 d=\"{dir}\"\n\
                 echo \"$*\" >> \"$d/runs\"\n\
                 code=$(head -n 1 \"$d/codes\")\n\
                 tail -n +2 \"$d/codes\" > \"$d/codes.next\" && mv \"$d/codes.next\" \"$d/codes\"\n\
                 if [ \"$code\" = 101 ]; then echo \"thread 'main' panicked at player.rs\" >&2; fi\n\
                 exit \"$code\"\n",
                real = env!("CARGO_BIN_EXE_auto-ascii-player"),
                dir = dir.display(),
            ),
        );
        Stage(dir)
    }

    fn launch(&self) -> Output {
        self.launch_with(&[])
    }

    fn launch_with(&self, before_flags: &[&str]) -> Output {
        Command::new("bash")
            .arg(launcher())
            .args([self.0.join("player"), "clip.ascii".into()])
            .args(before_flags)
            .args(["--style", "ascii"])
            .env("LOG", self.0.join("launcher.log"))
            .stdin(Stdio::null())
            .output()
            .expect("run bash")
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.0.join(name)).unwrap_or_default()
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn launcher() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/play-with-sound.command")
}

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn exits(log: &str) -> Vec<&str> {
    log.lines().filter_map(|l| l.split_whitespace().find(|w| w.starts_with("exit="))).collect()
}

#[test]
fn reaching_the_end_restarts_and_only_a_quit_stops() {
    let stage = Stage::new("loop", &[0, 0, 3]);
    let out = stage.launch();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stage.read("runs").lines().collect::<Vec<_>>(), ["clip.ascii --style ascii"; 3]);
    assert_eq!(exits(&stage.read("launcher.log")), ["exit=0", "exit=0", "exit=3"]);
}

#[test]
fn the_launcher_starts_no_audio_player_and_drops_the_old_audio_argument() {
    let text = fs::read_to_string(launcher()).unwrap();
    assert!(!text.contains("afplay") && !text.contains("AFPLAY"), "the player plays its own sound");
    let stage = Stage::new("legacy", &[3]);
    let out = stage.launch_with(&["clip.m4a"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stage.read("runs").lines().collect::<Vec<_>>(), ["clip.ascii --style ascii"]);
    assert!(stage.read("launcher.log").contains("ignoring audio argument clip.m4a"), "{}", stage.read("launcher.log"));
}

#[test]
fn an_unexpected_exit_is_logged_and_held_open() {
    let stage = Stage::new("panic", &[0, 101]);
    let out = stage.launch();
    assert_eq!(out.status.code(), Some(101), "{out:?}");
    let log = stage.read("launcher.log");
    assert_eq!(exits(&log), ["exit=0", "exit=101"]);
    assert!(log.contains("stderr: thread 'main' panicked at player.rs"), "{log}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("exited with status 101") && stdout.contains("Press Return"), "{stdout}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("panicked"), "{out:?}");
}

#[test]
fn a_player_without_the_quit_status_is_refused() {
    let stage = Stage::new("old", &[0]);
    script(&stage.0.join("player"), "echo 'Play ASCI assets in the terminal'\nexit 0\n");
    let out = stage.launch();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(stage.read("runs").is_empty(), "an old player must not start");
}
