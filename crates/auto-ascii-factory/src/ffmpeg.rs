//! FFmpeg and ffprobe subprocess and frame-stream plumbing.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;


pub type BoxErr = Box<dyn std::error::Error>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Programs {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

pub fn missing_tool(tool: &str, e: &std::io::Error) -> String {
    format!("failed to run {tool} (is it installed and on PATH?): {e}")
}

pub fn rawvideo_filter(w: u16, h: u16, fps: &str) -> String {
    format!("scale={w}:{h}:flags=area,fps={fps},format=rgb24")
}

#[derive(Clone, Debug)]
pub struct ProbeInfo {
    pub width: u64,
    pub height: u64,
    pub duration_secs: Option<f64>,
}

pub fn probe(ffprobe: &Path, input: &Path) -> Result<ProbeInfo, BoxErr> {
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(input)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| missing_tool("ffprobe", &e))?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe failed on {} ({}): {}",
            input.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }

    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("ffprobe emitted unparseable JSON: {e}"))?;
    let streams = v["streams"].as_array().ok_or("ffprobe JSON has no streams array")?;
    let video = streams
        .iter()
        .find(|s| s["codec_type"].as_str() == Some("video"))
        .ok_or_else(|| format!("no video stream in {}", input.display()))?;

    let parse_secs = |val: &serde_json::Value| val.as_str().and_then(|s| s.parse::<f64>().ok());
    Ok(ProbeInfo {
        width: video["width"].as_u64().unwrap_or(0),
        height: video["height"].as_u64().unwrap_or(0),
        duration_secs: parse_secs(&video["duration"]).or_else(|| parse_secs(&v["format"]["duration"])),
    })
}

#[derive(Clone, Debug)]
pub struct DecodeParams<'a> {
    pub ffmpeg: &'a Path,
    pub input: &'a Path,
    pub ss: Option<f64>,
    pub t: Option<f64>,
    pub fps: u16,
    pub w: u16,
    pub h: u16,
}

impl DecodeParams<'_> {
    pub fn frame_size(&self) -> usize {
        self.w as usize * self.h as usize * 3
    }
}

pub struct FrameStream {
    child: Child,
    stdout: ChildStdout,
    stderr_thread: JoinHandle<Vec<u8>>,
    frame_size: usize,
}

impl FrameStream {
    pub fn spawn(p: &DecodeParams<'_>) -> Result<FrameStream, BoxErr> {
        let mut cmd = Command::new(p.ffmpeg);
        cmd.args(["-nostdin", "-hide_banner", "-v", "error"]);
        if let Some(ss) = p.ss {
            cmd.arg("-ss").arg(ss.to_string());
        }
        if let Some(t) = p.t {
            cmd.arg("-t").arg(t.to_string());
        }
        cmd.arg("-i").arg(p.input);
        cmd.args(["-map", "0:v:0"]);
        cmd.arg("-vf").arg(rawvideo_filter(p.w, p.h, &p.fps.to_string()));
        cmd.args(["-f", "rawvideo", "-"]);
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| missing_tool("ffmpeg", &e))?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let mut stderr = child.stderr.take().expect("stderr was piped");
        let stderr_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            buf
        });
        Ok(FrameStream { child, stdout, stderr_thread, frame_size: p.frame_size() })
    }

    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    pub fn next_frame(&mut self, buf: &mut [u8]) -> Result<bool, BoxErr> {
        debug_assert_eq!(buf.len(), self.frame_size);
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = self.stdout.read(&mut buf[filled..])?;
            if n == 0 {
                return if filled == 0 {
                    Ok(false)
                } else {
                    Err(format!(
                        "short read from ffmpeg: EOF {filled}/{} bytes into a frame",
                        buf.len()
                    )
                    .into())
                };
            }
            filled += n;
        }
        Ok(true)
    }

    pub fn finish(mut self) -> Result<(), BoxErr> {
        drop(self.stdout);
        let status = self.child.wait()?;
        let stderr = self.stderr_thread.join().unwrap_or_default();
        if !status.success() {
            let msg = String::from_utf8_lossy(&stderr);
            let msg = msg.trim();
            return Err(format!(
                "ffmpeg exited with {status}{}{}",
                if msg.is_empty() { "" } else { ": " },
                msg
            )
            .into());
        }
        Ok(())
    }

    pub fn abort(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.stderr_thread.join();
    }
}
