//! One ffmpeg child per track, reading the stream URL itself: shared input
//! arguments (yt-dlp's HTTP headers, reconnects, an I/O timeout, `-ss` to
//! resume), and a runner that moves to the next candidate track (the HLS
//! fallback) when the current one is refused with HTTP 403.

use std::io::Read;
use std::path::PathBuf;
use std::process::{ChildStdout, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use super::procs::Procs;
use super::ytdlp::{Media, Track};

pub const STOPPED: &str = "stopped";

#[derive(Clone)]
pub struct DecodeCtx {
    pub procs: Procs,
    pub cwd: PathBuf,
    pub ffmpeg: PathBuf,
    pub stop: Arc<AtomicBool>,
}

pub enum Msg {
    YtdlpStarted,
    EntryFound(String),
    Media(Box<Media>),
    DecoderSpawned,
    FirstAudio,
    FirstVideo,
    AudioEof,
    VideoEof,
    Fallback(String),
    Failed(String),
}

pub fn input_args(track: &Track, ss: Option<f64>) -> Vec<String> {
    let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-v", "error"].map(String::from).to_vec();
    let lower = track.url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        for a in [
            "-reconnect",
            "1",
            "-reconnect_streamed",
            "1",
            "-reconnect_on_network_error",
            "1",
            "-reconnect_delay_max",
            "4",
            "-rw_timeout",
            "15000000",
        ] {
            args.push(a.to_string());
        }
        let mut extra = String::new();
        for (k, v) in &track.headers {
            if k.eq_ignore_ascii_case("user-agent") {
                args.push("-user_agent".into());
                args.push(v.clone());
            } else if !k.contains(['\r', '\n']) && !v.contains(['\r', '\n']) {
                extra.push_str(&format!("{k}: {v}\r\n"));
            }
        }
        if !extra.is_empty() {
            args.push("-headers".into());
            args.push(extra);
        }
    }
    if let Some(ss) = ss.filter(|s| *s > 0.0) {
        args.push("-ss".into());
        args.push(format!("{ss:.3}"));
    }
    args.push("-i".into());
    args.push(track.url.clone());
    args
}

pub fn forbidden(stderr: &str) -> bool {
    stderr.contains("403") || stderr.contains("Forbidden")
}

fn tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let start = lines.len().saturating_sub(2);
    lines[start..].join(" / ")
}

pub fn run_ffmpeg(
    ctx: &DecodeCtx,
    tracks: &[Track],
    kind: &str,
    tx: &Sender<Msg>,
    mut args: impl FnMut(&Track, usize) -> Vec<String>,
    mut read: impl FnMut(&mut ChildStdout) -> Result<(), String>,
) -> Result<(), String> {
    for (i, track) in tracks.iter().enumerate() {
        if ctx.stop.load(Ordering::SeqCst) {
            return Err(STOPPED.into());
        }
        let mut cmd = Command::new(&ctx.ffmpeg);
        cmd.args(args(track, i)).current_dir(&ctx.cwd);
        let spawned = ctx.procs.spawn(&mut cmd, "ffmpeg")?;
        let _ = tx.send(Msg::DecoderSpawned);
        let mut stderr = spawned.stderr;
        let drain = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.by_ref().take(256 * 1024).read_to_end(&mut buf);
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            buf
        });
        let mut stdout = spawned.stdout;
        let result = read(&mut stdout);
        drop(stdout);
        if result.is_err() {
            ctx.procs.kill(spawned.id);
        }
        let status = ctx.procs.reap(spawned.id);
        let err = String::from_utf8_lossy(&drain.join().unwrap_or_default()).into_owned();
        if ctx.stop.load(Ordering::SeqCst) {
            return Err(STOPPED.into());
        }
        result?;
        let refused = forbidden(&err);
        if refused && i + 1 < tracks.len() {
            let next = &tracks[i + 1];
            let _ = tx.send(Msg::Fallback(format!(
                "{kind}: format {} was refused (HTTP 403); continuing from format {} ({})",
                track.format_id, next.format_id, next.protocol
            )));
            continue;
        }
        let ok = status.is_some_and(|s| s.success());
        if ok && !refused {
            return Ok(());
        }
        let what = status.map_or_else(|| "was stopped".to_string(), |s| format!("exited with {s}"));
        let detail = tail(&err);
        return Err(if detail.is_empty() {
            format!("ffmpeg {what}")
        } else {
            format!("ffmpeg {what}: {detail}")
        });
    }
    Err(format!("no {kind} stream URL to play"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::procs::ScratchDir;
    use std::sync::mpsc;

    fn track(url: &str, id: &str) -> Track {
        Track {
            url: url.into(),
            format_id: id.into(),
            protocol: if url.contains("m3u8") { "m3u8_native".into() } else { "https".into() },
            headers: vec![
                ("User-Agent".into(), "UA/1".into()),
                ("Accept".into(), "*/*".into()),
                ("Bad".into(), "x\r\ninjected: 1".into()),
            ],
        }
    }

    #[test]
    fn inputs_carry_headers_reconnects_and_the_resume_point() {
        let args = input_args(&track("https://v.example/395", "395"), Some(12.5));
        let at = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].as_str());
        assert_eq!(at("-user_agent"), Some("UA/1"));
        assert_eq!(at("-headers"), Some("Accept: */*\r\n"), "CR/LF-bearing headers are dropped");
        assert_eq!(at("-reconnect"), Some("1"));
        assert_eq!(at("-rw_timeout"), Some("15000000"));
        assert_eq!(at("-ss"), Some("12.500"));
        assert_eq!(args.last().map(String::as_str), Some("https://v.example/395"));
        assert!(args.iter().position(|a| a == "-ss") < args.iter().position(|a| a == "-i"));
        let fresh = input_args(&track("https://v.example/395", "395"), None);
        assert!(!fresh.contains(&"-ss".to_string()));
    }

    #[test]
    fn a_403_moves_to_the_next_track_and_other_failures_surface() {
        let scratch = ScratchDir::new().unwrap();
        let fake = scratch.path().join("ffmpeg");
        std::fs::write(
            &fake,
            "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\ncase \"$last\" in\n*dash*) echo 'Server returned 403 Forbidden (access denied)' >&2; exit 1;;\n*m3u8*) printf 'ok'; exit 0;;\n*) echo 'Connection timed out' >&2; exit 1;;\nesac\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let procs = Procs::new();
        let ctx = DecodeCtx {
            procs: procs.clone(),
            cwd: scratch.path().to_path_buf(),
            ffmpeg: fake,
            stop: Arc::new(AtomicBool::new(false)),
        };
        let (tx, rx) = mpsc::channel();
        let tracks = [track("https://v.example/dash", "395"), track("https://m.example/x.m3u8", "229")];
        let mut got = Vec::new();
        let mut used = Vec::new();
        let result = run_ffmpeg(&ctx, &tracks, "video", &tx, |t, i| {
            used.push(i);
            vec![t.url.clone()]
        }, |out| {
            out.read_to_end(&mut got).map_err(|e| e.to_string())?;
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(used, [0, 1]);
        assert_eq!(got, b"ok");
        let notes: Vec<String> = rx
            .try_iter()
            .filter_map(|m| match m {
                Msg::Fallback(n) => Some(n),
                _ => None,
            })
            .collect();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("403") && notes[0].contains("229"), "{notes:?}");

        let other = [track("https://v.example/plain", "18")];
        let err = run_ffmpeg(&ctx, &other, "video", &tx, |t, _| vec![t.url.clone()], |out| {
            let mut sink = Vec::new();
            out.read_to_end(&mut sink).map_err(|e| e.to_string())?;
            Ok(())
        })
        .unwrap_err();
        assert!(err.starts_with("ffmpeg exited with") && err.ends_with("Connection timed out"), "{err}");
        assert_eq!(procs.running(), 0);
        assert_eq!(procs.alive(), 0);
    }
}
