//! Where an asset's sound comes from and how it reaches memory: sidecar
//! discovery beside the asset, the ffprobe length check, and the background
//! ffmpeg decode to interleaved s16 into a budget-bounded [`Pcm`] slab.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::output::{Pcm, Track};

pub const SIDECAR_EXTS: [&str; 12] =
    ["m4a", "mp4", "aac", "mp3", "wav", "flac", "ogg", "opus", "mov", "m4v", "mkv", "webm"];

pub const FOLDER_SOURCE: &str = "source.mp4";

pub const PCM_BUDGET_BYTES: usize = 512 << 20;

pub const MISMATCH_MIN_SECS: f64 = 3.0;

pub const MISMATCH_FRACTION: f64 = 0.1;

const TOOL_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackSource {
    Sidecar(PathBuf),
}

impl TrackSource {
    pub fn path(&self) -> &Path {
        match self {
            TrackSource::Sidecar(p) => p,
        }
    }
}

pub fn discover(asset: &Path) -> Option<TrackSource> {
    let dir = asset.parent().unwrap_or(Path::new(""));
    let stem = asset.file_stem()?;
    let sidecars = SIDECAR_EXTS.iter().map(|ext| {
        let mut name = OsString::from(stem);
        name.push(".");
        name.push(ext);
        dir.join(name)
    });
    sidecars
        .chain(std::iter::once(dir.join(FOLDER_SOURCE)))
        .find(|p| p.is_file() && p.as_path() != asset)
        .map(TrackSource::Sidecar)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

pub fn find_tool_in(name: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

impl Tools {
    pub fn find() -> Result<Tools, String> {
        let dirs = || {
            let path = std::env::var_os("PATH").unwrap_or_default();
            std::env::split_paths(&path).chain(TOOL_DIRS.map(PathBuf::from)).collect::<Vec<_>>()
        };
        let ffmpeg = find_tool_in("ffmpeg", dirs()).ok_or("ffmpeg not found on PATH or in /opt/homebrew/bin")?;
        let ffprobe = find_tool_in("ffprobe", dirs()).ok_or("ffprobe not found on PATH or in /opt/homebrew/bin")?;
        Ok(Tools { ffmpeg, ffprobe })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Probe {
    pub has_audio: bool,
    pub secs: Option<f64>,
}

pub fn parse_probe(text: &str) -> Probe {
    let mut probe = Probe { has_audio: false, secs: None };
    for line in text.lines() {
        match line.trim().split_once('=') {
            Some(("codec_type", "audio")) => probe.has_audio = true,
            Some(("duration", v)) if probe.secs.is_none() => {
                probe.secs = v.parse::<f64>().ok().filter(|s| s.is_finite() && *s > 0.0);
            }
            _ => {}
        }
    }
    probe
}

pub fn probe(tools: &Tools, path: &Path) -> Result<Probe, String> {
    let out = Command::new(&tools.ffprobe)
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries"])
        .args(["stream=codec_type,duration:format=duration", "-of", "default=nw=1"])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("running ffprobe: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("ffprobe could not read it: {}", last_line(&err)));
    }
    Ok(parse_probe(&String::from_utf8_lossy(&out.stdout)))
}

pub fn duration_mismatch(video_secs: f64, audio_secs: f64) -> bool {
    (audio_secs - video_secs).abs() > (video_secs * MISMATCH_FRACTION).max(MISMATCH_MIN_SECS)
}

pub fn capacity_frames(audio_secs: f64, rate: u32) -> usize {
    ((audio_secs + 1.0) * f64::from(rate)).ceil() as usize
}

pub fn ffmpeg_args(path: &Path, rate: u32, channels: usize) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-nostdin", "-v", "error", "-i"].map(OsString::from).to_vec();
    args.push(path.as_os_str().to_owned());
    for a in ["-map", "0:a:0", "-vn", "-sn", "-dn", "-ac"] {
        args.push(a.into());
    }
    args.push(channels.to_string().into());
    args.push("-ar".into());
    args.push(rate.to_string().into());
    for a in ["-f", "s16le", "-"] {
        args.push(a.into());
    }
    args
}

fn last_line(text: &str) -> String {
    text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no message").trim().to_string()
}

pub struct DecodeJob {
    pub ffmpeg: PathBuf,
    pub path: PathBuf,
    pub rate: u32,
    pub channels: usize,
    pub capacity_frames: usize,
    pub track: Arc<Track>,
    pub notes: Arc<Mutex<Vec<String>>>,
}

pub struct Decoder {
    child: Arc<Mutex<Option<Child>>>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder").field("running", &self.thread.is_some()).finish_non_exhaustive()
    }
}

const STDERR_KEEP: usize = 4096;

pub fn spawn(job: DecodeJob) -> Decoder {
    let child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let cancel = Arc::new(AtomicBool::new(false));
    let (slot, stop) = (child.clone(), cancel.clone());
    let thread = std::thread::spawn(move || decode(job, &slot, &stop));
    Decoder { child, cancel, thread: Some(thread) }
}

fn decode(job: DecodeJob, slot: &Mutex<Option<Child>>, cancel: &AtomicBool) {
    let pcm = job.track.publish(Pcm::new(job.rate, job.channels, job.capacity_frames));
    let note = |msg: String| job.notes.lock().unwrap_or_else(|p| p.into_inner()).push(msg);
    let name = job.path.display();
    let (stdout, stderr) = {
        let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
        if cancel.load(Ordering::SeqCst) {
            pcm.finish();
            return;
        }
        let spawned = Command::new(&job.ffmpeg)
            .args(ffmpeg_args(&job.path, job.rate, pcm.channels))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                job.track.fail();
                note(format!("{name}: running ffmpeg: {e}"));
                return;
            }
        };
        let io = (child.stdout.take(), child.stderr.take());
        *guard = Some(child);
        io
    };
    let full = std::thread::scope(|s| {
        let err = s.spawn(move || {
            let mut kept = Vec::new();
            if let Some(mut e) = stderr {
                let mut buf = [0u8; 1024];
                while let Ok(n) = e.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let room = STDERR_KEEP.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
            String::from_utf8_lossy(&kept).into_owned()
        });
        let full = stdout.is_some_and(|out| read_pcm(out, pcm));
        if full && let Some(c) = slot.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            let _ = c.kill();
        }
        (full, err.join().unwrap_or_default())
    });
    let (full, err) = full;
    let status = slot.lock().unwrap_or_else(|p| p.into_inner()).take().map(|mut c| c.wait());
    let secs = pcm.ready_frames() as f64 / f64::from(pcm.rate);
    if cancel.load(Ordering::SeqCst) || full {
        pcm.finish();
        return;
    }
    match status {
        Some(Ok(s)) if s.success() => pcm.finish(),
        _ if pcm.ready_frames() == 0 => {
            job.track.fail();
            note(format!("{name}: decode failed: {}", last_line(&err)));
        }
        _ => {
            pcm.finish();
            note(format!("{name}: decode stopped after {secs:.1} s: {}", last_line(&err)));
        }
    }
}

fn read_pcm(mut out: impl Read, pcm: &Pcm) -> bool {
    let frame_bytes = 2 * pcm.channels;
    let mut buf = vec![0u8; 64 * 1024];
    let mut carry: Vec<u8> = Vec::new();
    let mut samples: Vec<i16> = Vec::new();
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) => return false,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        };
        carry.extend_from_slice(&buf[..n]);
        let whole = carry.len() / frame_bytes * frame_bytes;
        samples.clear();
        samples.extend(carry[..whole].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
        carry.drain(..whole);
        let frames = samples.len() / pcm.channels;
        if pcm.push(&samples) < frames {
            return true;
        }
    }
}

impl Decoder {
    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(c) = self.child.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            let _ = c.kill();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("auto-ascii-audio-src-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(d: &Path, name: &str) {
        std::fs::write(d.join(name), b"x").unwrap();
    }

    fn found(asset: &Path) -> Option<String> {
        discover(asset).map(|t| t.path().file_name().unwrap().to_string_lossy().into_owned())
    }

    #[test]
    fn discovery_prefers_the_stem_over_the_folder_source() {
        let d = dir("order");
        let asset = d.join("1.ascii");
        touch(&d, "1.ascii");
        assert_eq!(found(&asset), None, "nothing beside it: no track");
        touch(&d, "source.mp4");
        assert_eq!(found(&asset).as_deref(), Some("source.mp4"), "the folder source is the fallback");
        touch(&d, "1.webm");
        assert_eq!(found(&asset).as_deref(), Some("1.webm"), "any stem sidecar beats source.mp4");
        touch(&d, "1.mp4");
        assert_eq!(found(&asset).as_deref(), Some("1.mp4"));
        touch(&d, "1.m4a");
        assert_eq!(found(&asset).as_deref(), Some("1.m4a"), "m4a first");
        touch(&d, "2.m4a");
        assert_eq!(found(&d.join("2.ascii")).as_deref(), Some("2.m4a"), "per-asset stems");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn discovery_keeps_spaces_and_dots_in_the_stem() {
        let d = dir("names");
        touch(&d, "Darth Vader.ascii");
        touch(&d, "source.mp4");
        assert_eq!(found(&d.join("Darth Vader.ascii")).as_deref(), Some("source.mp4"));
        touch(&d, "Darth Vader.m4a");
        assert_eq!(found(&d.join("Darth Vader.ascii")).as_deref(), Some("Darth Vader.m4a"));
        touch(&d, "v1.2.mp4");
        assert_eq!(found(&d.join("v1.2.ascii")).as_deref(), Some("v1.2.mp4"));
        std::fs::create_dir_all(d.join("sub").join("x.m4a")).unwrap();
        assert_eq!(found(&d.join("sub").join("x.ascii")), None, "a directory is not a track");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_different_cut_is_a_mismatch_a_close_one_is_not() {
        assert!(duration_mismatch(30.0, 4799.9), "an 80-minute mix is not a 30 s slice");
        assert!(!duration_mismatch(273.3, 273.32), "same cut");
        assert!(!duration_mismatch(30.03, 30.0), "container rounding");
        assert!(!duration_mismatch(2.0, 1.2), "a short clip gets the absolute floor");
        assert!(duration_mismatch(2.0, 5.5));
        assert!(!duration_mismatch(7200.0, 6600.0), "long films get a proportional band");
        assert!(duration_mismatch(7200.0, 6400.0));
    }

    #[test]
    fn the_probe_reads_the_audio_stream_and_its_length() {
        let p = parse_probe("codec_type=audio\nduration=273.322086\nduration=273.322086\n");
        assert_eq!(p, Probe { has_audio: true, secs: Some(273.322086) });
        let p = parse_probe("codec_type=audio\nduration=N/A\nduration=12.5\n");
        assert_eq!(p, Probe { has_audio: true, secs: Some(12.5) }, "stream length unknown: the container's");
        assert_eq!(parse_probe("duration=4.0\n"), Probe { has_audio: false, secs: Some(4.0) });
        assert_eq!(parse_probe(""), Probe { has_audio: false, secs: None });
    }

    #[test]
    fn tools_resolve_from_the_given_dirs_in_order() {
        let a = dir("tools-a");
        let b = dir("tools-b");
        touch(&b, "ffmpeg");
        assert_eq!(find_tool_in("ffmpeg", [a.clone(), b.clone()]), Some(b.join("ffmpeg")));
        touch(&a, "ffmpeg");
        assert_eq!(find_tool_in("ffmpeg", [a.clone(), b.clone()]), Some(a.join("ffmpeg")));
        assert_eq!(find_tool_in("ffprobe", [a.clone(), b.clone()]), None);
        let _ = (std::fs::remove_dir_all(&a), std::fs::remove_dir_all(&b));
    }

    #[test]
    fn decode_args_ask_for_the_first_audio_stream_as_s16() {
        let args = ffmpeg_args(Path::new("/m/Clip One.m4a"), 48_000, 2);
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(joined[4], "/m/Clip One.m4a", "the path is one argument, spaces and all");
        assert!(joined.join(" ").ends_with("-map 0:a:0 -vn -sn -dn -ac 2 -ar 48000 -f s16le -"), "{joined:?}");
        assert_eq!(capacity_frames(2.0, 1000), 3000, "one second of slack past the probed length");
    }
}
