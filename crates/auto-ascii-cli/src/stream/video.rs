//! Picture without a file: ffmpeg scales the video track to the plane size
//! at a constant frame rate as raw rgb24 on a pipe, each frame becomes the
//! factory's six feature planes, and a bounded queue back-pressures the pipe.

use std::cell::Cell;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::thread::JoinHandle;

use auto_ascii_factory::live::LiveExtractor;
use auto_ascii_factory::params::Params;
use auto_ascii_format::PlaneLevels;

use super::decode::{DecodeCtx, Msg, STOPPED, input_args, run_ffmpeg};
use super::ytdlp::Track;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fps {
    pub num: u32,
    pub den: u32,
}

impl Fps {
    pub fn value(self) -> f64 {
        f64::from(self.num) / f64::from(self.den.max(1))
    }

    pub fn filter_arg(self) -> String {
        if self.den == 1 { self.num.to_string() } else { format!("{}/{}", self.num, self.den) }
    }

    pub fn pts(self, idx: u32) -> f64 {
        f64::from(idx) * f64::from(self.den) / f64::from(self.num.max(1))
    }
}

pub fn stream_fps(source: Option<f64>, max: u16) -> Fps {
    let max = f64::from(max.max(1));
    let Some(src) = source.filter(|f| f.is_finite() && *f > 0.0) else {
        return Fps { num: max as u32, den: 1 };
    };
    let mut f = src;
    while f > max + 0.01 {
        f /= 2.0;
    }
    for base in [24.0, 30.0, 60.0] {
        let ntsc = base * 1000.0 / 1001.0;
        if (f - ntsc).abs() < 0.02 {
            return Fps { num: base as u32 * 1000, den: 1001 };
        }
    }
    Fps { num: (f.round() as u32).max(1), den: 1 }
}

pub fn plane_dims(width: Option<u32>, height: Option<u32>, base_w: u16, base_h: u16) -> (u16, u16, u16, u16) {
    let (w, h) = match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (f64::from(w), f64::from(h)),
        _ => (16.0, 9.0),
    };
    let area = f64::from(base_w) * f64::from(base_h);
    let even = |v: f64| ((v / 2.0).round() as u32).clamp(1, 2048) as u16 * 2;
    let pw = even((area * w / h).sqrt());
    let ph = even((area * h / w).sqrt());
    let g = gcd(w as u64, h as u64).max(1);
    let (an, ad) = ((w as u64 / g), (h as u64 / g));
    let (an, ad) = if an > u64::from(u16::MAX) || ad > u64::from(u16::MAX) {
        (u64::from(pw), u64::from(ph))
    } else {
        (an, ad)
    };
    (pw, ph, an as u16, ad as u16)
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

pub struct Frame {
    pub idx: u32,
    pub pts: f64,
    pub y: Vec<u8>,
    pub e: Vec<u8>,
    pub ex: Vec<u8>,
    pub ey: Vec<u8>,
    pub h: Vec<u8>,
    pub c: Vec<u8>,
    pub levels: Option<PlaneLevels>,
    pub shot_start: u32,
}

impl Frame {
    fn blank() -> Frame {
        Frame {
            idx: 0,
            pts: 0.0,
            y: Vec::new(),
            e: Vec::new(),
            ex: Vec::new(),
            ey: Vec::new(),
            h: Vec::new(),
            c: Vec::new(),
            levels: None,
            shot_start: 0,
        }
    }

    pub fn live(&self) -> auto_ascii::pipeline::LiveFrame<'_> {
        auto_ascii::pipeline::LiveFrame {
            y: &self.y,
            e: &self.e,
            ex: &self.ex,
            ey: &self.ey,
            h: &self.h,
            c: &self.c,
            levels: self.levels,
            shot_start: self.shot_start,
        }
    }
}

fn copy(dst: &mut Vec<u8>, src: &[u8]) {
    dst.clear();
    dst.extend_from_slice(src);
}

pub fn ffmpeg_args(track: &Track, ss: Option<f64>, w: u16, h: u16, fps: Fps) -> Vec<String> {
    let mut args = input_args(track, ss);
    for a in ["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"] {
        args.push(a.to_string());
    }
    args.push(auto_ascii_factory::ffmpeg::rawvideo_filter(w, h, &fps.filter_arg()));
    for a in ["-f", "rawvideo", "-"] {
        args.push(a.to_string());
    }
    args
}

pub struct VideoJob {
    pub ctx: DecodeCtx,
    pub tracks: Vec<Track>,
    pub w: u16,
    pub h: u16,
    pub fps: Fps,
    pub params: Params,
    pub tx: Sender<Msg>,
    pub frames: SyncSender<Frame>,
    pub recycle: Receiver<Frame>,
    pub queued: Arc<AtomicUsize>,
}

pub fn spawn(job: VideoJob) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let VideoJob { ctx, tracks, w, h, fps, params, tx, frames, recycle, queued } = job;
        let frame_size = usize::from(w) * usize::from(h) * 3;
        let mut rgb = vec![0u8; frame_size];
        let mut live = LiveExtractor::new(w, h, &params);
        let out = Cell::new(0u32);
        let result = run_ffmpeg(
            &ctx,
            &tracks,
            "video",
            &tx,
            |track, _| {
                let done = out.get();
                let ss = (done > 0).then(|| fps.pts(done));
                ffmpeg_args(track, ss, w, h, fps)
            },
            |stdout| loop {
                let mut filled = 0;
                while filled < frame_size {
                    match stdout.read(&mut rgb[filled..]) {
                        Ok(0) => return Ok(()),
                        Ok(n) => filled += n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e.to_string()),
                    }
                }
                let info = live.push(&rgb);
                let f = live.features();
                let mut frame = recycle.try_recv().unwrap_or_else(|_| Frame::blank());
                let idx = out.get();
                frame.idx = idx;
                frame.pts = fps.pts(idx);
                copy(&mut frame.y, f.y());
                copy(&mut frame.e, f.e());
                copy(&mut frame.ex, f.ex());
                copy(&mut frame.ey, f.ey());
                copy(&mut frame.h, f.h());
                copy(&mut frame.c, f.c());
                frame.levels = info.levels;
                frame.shot_start = info.shot_start;
                if idx == 0 {
                    let _ = tx.send(Msg::FirstVideo);
                }
                queued.fetch_add(1, Ordering::SeqCst);
                if frames.send(frame).is_err() {
                    queued.fetch_sub(1, Ordering::SeqCst);
                    return Err(STOPPED.into());
                }
                out.set(idx + 1);
            },
        );
        match result {
            Ok(()) => {
                let _ = tx.send(Msg::VideoEof);
            }
            Err(e) if e == STOPPED => {}
            Err(e) => {
                let _ = tx.send(Msg::Failed(format!("video: {e}")));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rates_stay_at_or_under_the_cap_and_keep_ntsc_exact() {
        assert_eq!(stream_fps(Some(15.0), 30), Fps { num: 15, den: 1 });
        assert_eq!(stream_fps(Some(30.0), 30), Fps { num: 30, den: 1 });
        assert_eq!(stream_fps(Some(29.97), 30), Fps { num: 30000, den: 1001 });
        assert_eq!(stream_fps(Some(23.976), 30), Fps { num: 24000, den: 1001 });
        assert_eq!(stream_fps(Some(60.0), 30), Fps { num: 30, den: 1 });
        assert_eq!(stream_fps(Some(59.94), 30), Fps { num: 30000, den: 1001 });
        assert_eq!(stream_fps(Some(50.0), 30), Fps { num: 25, den: 1 });
        assert_eq!(stream_fps(None, 30), Fps { num: 30, den: 1 });
        assert_eq!(stream_fps(Some(f64::NAN), 30), Fps { num: 30, den: 1 });
        let ntsc = Fps { num: 30000, den: 1001 };
        assert_eq!(ntsc.filter_arg(), "30000/1001");
        assert!((ntsc.pts(30) - 1.001).abs() < 1e-12);
    }

    #[test]
    fn planes_keep_the_factory_area_and_the_source_shape() {
        assert_eq!(plane_dims(Some(1920), Some(1080), 480, 270), (480, 270, 16, 9));
        assert_eq!(plane_dims(Some(320), Some(240), 480, 270), (416, 312, 4, 3));
        assert_eq!(plane_dims(Some(1080), Some(1920), 480, 270), (270, 480, 9, 16));
        assert_eq!(plane_dims(None, None, 480, 270), (480, 270, 16, 9));
        let (w, h, _, _) = plane_dims(Some(3840), Some(1600), 480, 270);
        assert!(w % 2 == 0 && h % 2 == 0);
    }

    #[test]
    fn video_args_scale_to_the_planes_at_a_constant_rate() {
        let track = Track {
            url: "https://v.example/395".into(),
            format_id: "395".into(),
            protocol: "https".into(),
            headers: Vec::new(),
        };
        let args = ffmpeg_args(&track, None, 416, 312, Fps { num: 15, den: 1 });
        assert_eq!(
            args[args.len() - 10..].join(" "),
            "-map 0:v:0 -an -sn -dn -vf scale=416:312:flags=area,fps=15,format=rgb24 -f rawvideo -"
        );
    }
}
