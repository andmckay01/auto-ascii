//! Sound without a file: ffmpeg decodes the audio track to interleaved f32
//! on a pipe, a bounded ring back-pressures that pipe, and an output (cpal,
//! or a null sink paced in real time for `--sim`) pulls from the ring and
//! reports what it consumed to the shared clock state.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::clock::AudioShared;
use super::decode::{DecodeCtx, Msg, STOPPED, run_ffmpeg};
use super::ytdlp::Track;

pub struct Ring {
    inner: Mutex<RingInner>,
    space: Condvar,
    cap: usize,
}

struct RingInner {
    buf: VecDeque<f32>,
    eof: bool,
    closed: bool,
}

impl Ring {
    pub fn new(cap: usize) -> Arc<Ring> {
        Arc::new(Ring {
            inner: Mutex::new(RingInner { buf: VecDeque::with_capacity(cap), eof: false, closed: false }),
            space: Condvar::new(),
            cap: cap.max(1),
        })
    }

    pub fn push(&self, mut samples: &[f32]) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        while !samples.is_empty() {
            if inner.closed {
                return false;
            }
            let room = self.cap - inner.buf.len();
            if room == 0 {
                inner = self.space.wait(inner).unwrap_or_else(|p| p.into_inner());
                continue;
            }
            let n = room.min(samples.len());
            inner.buf.extend(&samples[..n]);
            samples = &samples[n..];
        }
        true
    }

    pub fn pop_mapped(&self, out: &mut [f32], out_channels: usize, src_channels: usize) -> usize {
        let out_channels = out_channels.max(1);
        let src_channels = src_channels.max(1);
        let frames = out.len() / out_channels;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let avail = inner.buf.len() / src_channels;
        let take = frames.min(avail);
        for f in 0..take {
            let frame = &mut out[f * out_channels..(f + 1) * out_channels];
            let mut src = [0.0f32; 2];
            for s in src.iter_mut().take(src_channels.min(2)) {
                *s = inner.buf.pop_front().unwrap_or(0.0);
            }
            for _ in 2..src_channels {
                inner.buf.pop_front();
            }
            for (c, o) in frame.iter_mut().enumerate() {
                *o = match (src_channels, c) {
                    (1, 0 | 1) => src[0],
                    (_, 0) => src[0],
                    (_, 1) => src[1],
                    _ => 0.0,
                };
            }
        }
        drop(inner);
        if take > 0 {
            self.space.notify_all();
        }
        for o in &mut out[take * out_channels..] {
            *o = 0.0;
        }
        take
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).buf.len()
    }

    pub fn set_eof(&self) {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).eof = true;
    }

    pub fn eof(&self) -> bool {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).eof
    }

    pub fn close(&self) {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).closed = true;
        self.space.notify_all();
    }
}

pub struct Output {
    pub ring: Arc<Ring>,
    pub shared: Arc<AudioShared>,
    pub channels: usize,
    pub src_channels: usize,
}

impl Output {
    pub fn fill(&self, out: &mut [f32], at: Instant, latency: Option<Duration>) {
        let frames = (out.len() / self.channels.max(1)) as u64;
        if !self.shared.playing.load(Ordering::SeqCst) {
            out.fill(0.0);
            self.shared.record(0, 0, at, latency, false);
            return;
        }
        let took = self.ring.pop_mapped(out, self.channels, self.src_channels) as u64;
        self.shared.record(took, frames, at, latency, self.ring.eof());
    }
}

pub struct NullSink {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

const NULL_CHUNK: u64 = 4096;

pub fn frames_due(elapsed: Duration, rate: u64, pulled: u64, max: u64) -> u64 {
    let due = (elapsed.as_secs_f64() * rate as f64) as u64;
    due.saturating_sub(pulled).min(max)
}

impl NullSink {
    pub fn start(output: Output) -> NullSink {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let rate = u64::from(output.shared.rate);
            let t0 = Instant::now();
            let mut pulled: u64 = 0;
            let mut buf = vec![0.0f32; NULL_CHUNK as usize * output.channels.max(1)];
            while !flag.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
                let now = Instant::now();
                let frames = frames_due(now.duration_since(t0), rate, pulled, NULL_CHUNK) as usize;
                if frames == 0 {
                    continue;
                }
                pulled += frames as u64;
                output.fill(&mut buf[..frames * output.channels], now, None);
            }
        });
        NullSink { stop, thread: Some(thread) }
    }
}

impl Drop for NullSink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub struct Sink {
    _null: Option<NullSink>,
    #[cfg(feature = "audio")]
    _device: Option<device::DeviceSink>,
}

impl Sink {
    pub fn null(sink: NullSink) -> Sink {
        Sink {
            _null: Some(sink),
            #[cfg(feature = "audio")]
            _device: None,
        }
    }

    #[cfg(feature = "audio")]
    pub fn device(sink: device::DeviceSink) -> Sink {
        Sink { _null: None, _device: Some(sink) }
    }
}

pub struct DeviceFormat {
    pub rate: u32,
    pub channels: usize,
    pub name: String,
}

#[cfg(feature = "audio")]
pub mod device {
    use std::sync::Arc;
    use std::time::Instant;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SampleFormat, SizedSample};

    use super::{DeviceFormat, Output};

    pub struct DeviceSink {
        stream: cpal::Stream,
    }

    pub struct Opened {
        device: cpal::Device,
        config: cpal::SupportedStreamConfig,
    }

    pub fn open() -> Result<(Opened, DeviceFormat), String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no default audio output device")?;
        let config = device
            .default_output_config()
            .map_err(|e| format!("audio output config: {e}"))?;
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "default output".to_string());
        let format = DeviceFormat {
            rate: config.sample_rate(),
            channels: usize::from(config.channels()),
            name,
        };
        Ok((Opened { device, config }, format))
    }

    fn build<T>(opened: &Opened, output: Output) -> Result<cpal::Stream, String>
    where
        T: SizedSample + FromSample<f32>,
    {
        let config = opened.config.config();
        let output = Arc::new(output);
        let mut scratch: Vec<f32> = Vec::new();
        opened
            .device
            .build_output_stream::<T, _, _>(
                config,
                move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                    let ts = info.timestamp();
                    let latency = ts.playback.duration_since(ts.callback);
                    if scratch.len() < data.len() {
                        scratch.resize(data.len(), 0.0);
                    }
                    let buf = &mut scratch[..data.len()];
                    output.fill(buf, Instant::now(), Some(latency));
                    for (d, s) in data.iter_mut().zip(buf.iter()) {
                        *d = T::from_sample(*s);
                    }
                },
                |_err| {},
                None,
            )
            .map_err(|e| format!("audio output stream: {e}"))
    }

    pub fn start(opened: Opened, output: Output) -> Result<DeviceSink, String> {
        let stream = match opened.config.sample_format() {
            SampleFormat::F32 => build::<f32>(&opened, output)?,
            SampleFormat::I16 => build::<i16>(&opened, output)?,
            SampleFormat::U16 => build::<u16>(&opened, output)?,
            SampleFormat::I32 => build::<i32>(&opened, output)?,
            other => return Err(format!("unsupported audio sample format {other}")),
        };
        stream.play().map_err(|e| format!("audio output start: {e}"))?;
        Ok(DeviceSink { stream })
    }

    impl Drop for DeviceSink {
        fn drop(&mut self) {
            let _ = self.stream.pause();
        }
    }
}

pub fn ffmpeg_args(track: &Track, ss: Option<f64>, rate: u32, channels: usize) -> Vec<String> {
    let mut args = super::decode::input_args(track, ss);
    for a in ["-map", "0:a:0", "-vn", "-sn", "-dn", "-ac"] {
        args.push(a.to_string());
    }
    args.push(channels.to_string());
    args.push("-ar".into());
    args.push(rate.to_string());
    for a in ["-f", "f32le", "-"] {
        args.push(a.to_string());
    }
    args
}

pub struct AudioJob {
    pub ctx: DecodeCtx,
    pub tracks: Vec<Track>,
    pub rate: u32,
    pub channels: usize,
    pub ring: Arc<Ring>,
    pub tx: Sender<Msg>,
}

pub fn spawn(job: AudioJob) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let AudioJob { ctx, tracks, rate, channels, ring, tx } = job;
        let frame_bytes = 4 * channels.max(1);
        let samples_out = std::cell::Cell::new(0u64);
        let mut first = true;
        let mut carry: Vec<u8> = Vec::new();
        let mut floats: Vec<f32> = Vec::new();
        let mut buf = vec![0u8; 32 * 1024];
        let result = run_ffmpeg(
            &ctx,
            &tracks,
            "audio",
            &tx,
            |track, _| {
                let done = samples_out.get();
                let ss = (done > 0).then(|| done as f64 / f64::from(rate));
                ffmpeg_args(track, ss, rate, channels)
            },
            |stdout| {
                carry.clear();
                loop {
                    let n = match stdout.read(&mut buf) {
                        Ok(0) => return Ok(()),
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e.to_string()),
                    };
                    if first {
                        first = false;
                        let _ = tx.send(Msg::FirstAudio);
                    }
                    carry.extend_from_slice(&buf[..n]);
                    let whole = carry.len() / frame_bytes * frame_bytes;
                    floats.clear();
                    floats.extend(
                        carry[..whole].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    );
                    carry.drain(..whole);
                    if !ring.push(&floats) {
                        return Err(STOPPED.into());
                    }
                    samples_out.set(samples_out.get() + (whole / frame_bytes) as u64);
                }
            },
        );
        match result {
            Ok(()) => {
                ring.set_eof();
                let _ = tx.send(Msg::AudioEof);
            }
            Err(e) if e == STOPPED => {}
            Err(e) => {
                ring.set_eof();
                let _ = tx.send(Msg::Failed(format!("audio: {e}")));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_is_bounded_and_back_pressures_the_producer() {
        let ring = Ring::new(8);
        let producer = {
            let ring = ring.clone();
            std::thread::spawn(move || ring.push(&[1.0; 20]))
        };
        assert!(until(|| ring.len() == 8), "the producer never filled the ring");
        assert_eq!(ring.len(), 8, "never grows past its capacity");
        assert!(!producer.is_finished(), "20 samples cannot fit in 8: the producer waits for room");
        let mut out = [0.0f32; 8];
        let mut got = 0;
        while got < 20 {
            got += 2 * ring.pop_mapped(&mut out, 2, 2);
            std::thread::yield_now();
        }
        assert!(producer.join().unwrap());
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn close_releases_a_blocked_producer() {
        let ring = Ring::new(4);
        let producer = {
            let ring = ring.clone();
            std::thread::spawn(move || ring.push(&[0.5; 10]))
        };
        assert!(until(|| ring.len() == 4), "the producer never blocked");
        ring.close();
        assert!(!producer.join().unwrap(), "a closed ring refuses the rest");
    }

    #[test]
    fn channels_are_mapped_and_underruns_pad_silence() {
        let ring = Ring::new(64);
        ring.push(&[0.1, 0.2, 0.3, 0.4]);
        let mut out = [9.0f32; 12];
        assert_eq!(ring.pop_mapped(&mut out, 4, 2), 2);
        assert_eq!(out, [0.1, 0.2, 0.0, 0.0, 0.3, 0.4, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        ring.push(&[0.7, 0.8]);
        let mut stereo = [9.0f32; 4];
        assert_eq!(ring.pop_mapped(&mut stereo, 2, 1), 2);
        assert_eq!(stereo, [0.7, 0.7, 0.8, 0.8]);
    }

    #[test]
    fn output_consumes_only_while_playing() {
        let ring = Ring::new(1000);
        ring.push(&[0.25; 200]);
        let shared = AudioShared::new(100);
        let out = Output { ring: ring.clone(), shared: shared.clone(), channels: 2, src_channels: 2 };
        let mut buf = [1.0f32; 40];
        out.fill(&mut buf, Instant::now(), None);
        assert!(buf.iter().all(|&s| s == 0.0));
        assert_eq!((shared.consumed(), ring.len()), (0, 200), "paused output is silence and consumes nothing");
        shared.playing.store(true, Ordering::SeqCst);
        out.fill(&mut buf, Instant::now(), None);
        assert_eq!((shared.consumed(), ring.len()), (20, 160));
    }

    fn until(cond: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::yield_now();
        }
        cond()
    }

    #[test]
    fn null_sink_pacing_is_elapsed_time_times_the_rate() {
        let ms = Duration::from_millis;
        assert_eq!(frames_due(ms(0), 48_000, 0, 4096), 0);
        assert_eq!(frames_due(ms(10), 48_000, 0, 4096), 480);
        assert_eq!(frames_due(ms(20), 48_000, 480, 4096), 480, "only what is newly due");
        assert_eq!(frames_due(ms(1000), 48_000, 480, 4096), 4096, "a late wake-up pulls one chunk at most");
        assert_eq!(frames_due(ms(5), 48_000, 480, 4096), 0, "never ahead of the wall clock");
    }

    #[test]
    fn the_null_sink_consumes_but_never_faster_than_real_time() {
        let ring = Ring::new(100_000);
        ring.push(&vec![0.0; 100_000]);
        let shared = AudioShared::new(10_000);
        shared.playing.store(true, Ordering::SeqCst);
        let started = Instant::now();
        let sink = NullSink::start(Output { ring, shared: shared.clone(), channels: 2, src_channels: 2 });
        assert!(until(|| shared.consumed() >= 100), "the null sink never pulled");
        drop(sink);
        let elapsed = started.elapsed().as_secs_f64();
        let consumed = shared.consumed();
        assert!(consumed as f64 <= elapsed * 10_000.0 + 1.0, "{consumed} frames in {elapsed:.3}s is faster than real time");
    }

    #[test]
    fn audio_args_decode_the_first_audio_stream_to_f32() {
        let track = Track {
            url: "https://a.example/251".into(),
            format_id: "251".into(),
            protocol: "https".into(),
            headers: vec![("User-Agent".into(), "UA/1".into())],
        };
        let args = ffmpeg_args(&track, Some(2.5), 48000, 2);
        let joined = args.join(" ");
        assert!(joined.ends_with("-i https://a.example/251 -map 0:a:0 -vn -sn -dn -ac 2 -ar 48000 -f f32le -"), "{joined}");
        assert!(joined.contains("-ss 2.500 -protocol_whitelist http,https,tcp,tls,crypto,httpproxy -i"), "{joined}");
    }
}
