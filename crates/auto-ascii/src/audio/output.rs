//! Sound out of memory: a lock-free i16 slab the decode thread publishes by
//! frame count, and the cpal or null-sink output that pulls frames at the
//! shared read cursor, wraps at the loop length, writes zeros while muted or
//! undecoded, and reports what it consumed to the shared clock state.

use std::sync::atomic::{AtomicBool, AtomicI16, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::clock::AudioShared;

#[derive(Debug)]
pub struct Pcm {
    pub rate: u32,
    pub channels: usize,
    samples: Box<[AtomicI16]>,
    ready: AtomicUsize,
    done: AtomicBool,
}

impl Pcm {
    pub fn new(rate: u32, channels: usize, capacity_frames: usize) -> Pcm {
        let channels = channels.clamp(1, 2);
        let samples = std::iter::repeat_with(|| AtomicI16::new(0))
            .take(capacity_frames * channels)
            .collect();
        Pcm { rate: rate.max(1), channels, samples, ready: AtomicUsize::new(0), done: AtomicBool::new(false) }
    }

    pub fn from_samples(rate: u32, channels: usize, samples: &[i16]) -> Pcm {
        let pcm = Pcm::new(rate, channels, samples.len() / channels.clamp(1, 2));
        pcm.push(samples);
        pcm.finish();
        pcm
    }

    pub fn capacity_frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    pub fn ready_frames(&self) -> usize {
        self.ready.load(Ordering::Acquire)
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn bytes(&self) -> usize {
        self.samples.len() * 2
    }

    pub fn push(&self, samples: &[i16]) -> usize {
        let start = self.ready.load(Ordering::Relaxed);
        let frames = (samples.len() / self.channels).min(self.capacity_frames() - start);
        let dst = &self.samples[start * self.channels..(start + frames) * self.channels];
        for (d, s) in dst.iter().zip(samples) {
            d.store(*s, Ordering::Relaxed);
        }
        self.ready.store(start + frames, Ordering::Release);
        frames
    }

    pub fn finish(&self) {
        self.done.store(true, Ordering::Release);
    }

    pub fn read_frame(&self, idx: u64, out: &mut [f32]) -> bool {
        let ready = self.ready_frames() as u64;
        if idx >= ready {
            out.fill(0.0);
            return !self.is_done();
        }
        let i = idx as usize * self.channels;
        let left = f32::from(self.samples[i].load(Ordering::Relaxed)) / 32768.0;
        let right = match self.channels {
            1 => left,
            _ => f32::from(self.samples[i + 1].load(Ordering::Relaxed)) / 32768.0,
        };
        for (c, o) in out.iter_mut().enumerate() {
            *o = match c {
                0 => left,
                1 => right,
                _ => 0.0,
            };
        }
        false
    }
}

#[derive(Debug, Default)]
pub struct Track {
    pcm: OnceLock<Pcm>,
    failed: AtomicBool,
}

impl Track {
    pub fn new() -> Arc<Track> {
        Arc::new(Track::default())
    }

    pub fn with_pcm(pcm: Pcm) -> Arc<Track> {
        let track = Track::new();
        track.publish(pcm);
        track
    }

    pub fn publish(&self, pcm: Pcm) -> &Pcm {
        self.pcm.get_or_init(|| pcm)
    }

    pub fn pcm(&self) -> Option<&Pcm> {
        self.pcm.get()
    }

    pub fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        if let Some(pcm) = self.pcm() {
            pcm.finish();
        }
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub struct Output {
    pub track: Arc<Track>,
    pub shared: Arc<AudioShared>,
    pub channels: usize,
}

impl Output {
    pub fn fill(&self, out: &mut [f32], at: Instant, latency: Option<Duration>) {
        let shared = &self.shared;
        shared.take_seek();
        let channels = self.channels.max(1);
        let frames = out.len() / channels;
        if !shared.playing.load(Ordering::SeqCst) {
            out.fill(0.0);
            shared.record(0, at, latency);
            return;
        }
        let start = shared.cursor();
        let muted = shared.muted.load(Ordering::SeqCst);
        let pcm = self.track.pcm();
        let mut starved = pcm.is_none() && !self.track.failed();
        for (f, frame) in out.chunks_exact_mut(channels).enumerate() {
            match pcm {
                Some(pcm) if !muted => {
                    starved |= pcm.read_frame(shared.track_frame(start + f as u64), frame);
                }
                _ => frame.fill(0.0),
            }
        }
        out[frames * channels..].fill(0.0);
        shared.record(frames as u64, at, latency);
        if starved && !muted {
            shared.underruns.fetch_add(1, Ordering::SeqCst);
        }
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
                output.fill(&mut buf[..frames * output.channels.max(1)], now, None);
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

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sink").field("null", &self._null.is_some()).finish_non_exhaustive()
    }
}

impl Sink {
    pub fn null(sink: NullSink) -> Sink {
        Sink {
            _null: Some(sink),
            #[cfg(feature = "audio")]
            _device: None,
        }
    }

    pub fn detached() -> Sink {
        Sink {
            _null: None,
            #[cfg(feature = "audio")]
            _device: None,
        }
    }

    #[cfg(feature = "audio")]
    pub fn device(sink: device::DeviceSink) -> Sink {
        Sink { _null: None, _device: Some(sink) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceFormat {
    pub rate: u32,
    pub channels: usize,
    pub name: String,
}

#[cfg(feature = "audio")]
pub mod device {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{ErrorKind, FromSample, SampleFormat, SizedSample};

    use super::super::clock::AudioShared;
    use super::{DeviceFormat, Output};

    pub fn on_stream_error(shared: &AudioShared, kind: ErrorKind) {
        match kind {
            ErrorKind::DeviceNotAvailable | ErrorKind::HostUnavailable | ErrorKind::StreamInvalidated => {
                shared.lost.store(true, Ordering::SeqCst);
            }
            ErrorKind::Xrun => {
                shared.underruns.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
    }

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
        let lost = output.shared.clone();
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
                move |err: cpal::Error| on_stream_error(&lost, err.kind()),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: f64) -> Instant {
        base + Duration::from_secs_f64(secs)
    }

    fn ramp(frames: usize) -> Pcm {
        let samples: Vec<i16> = (0..frames).flat_map(|i| [i as i16, -(i as i16)]).collect();
        Pcm::from_samples(1000, 2, &samples)
    }

    fn output(track: Arc<Track>, rate: u32) -> Output {
        Output { track, shared: AudioShared::new(rate), channels: 2 }
    }

    fn first_left(buf: &[f32]) -> i32 {
        (buf[0] * 32768.0).round() as i32
    }

    #[test]
    fn the_slab_publishes_whole_frames_and_never_grows() {
        let pcm = Pcm::new(1000, 2, 3);
        assert_eq!(pcm.push(&[1, 2, 3]), 1, "a half frame waits for its partner");
        assert_eq!(pcm.ready_frames(), 1);
        assert_eq!(pcm.push(&[5, 6, 7, 8, 9, 10]), 2);
        assert_eq!(pcm.push(&[11, 12]), 0, "the budget is fixed up front");
        assert_eq!((pcm.capacity_frames(), pcm.bytes()), (3, 12));
        let mut out = [0.0f32; 2];
        assert!(!pcm.read_frame(1, &mut out));
        assert_eq!(out.map(|s| (s * 32768.0).round() as i32), [5, 6]);
        assert!(pcm.read_frame(3, &mut out), "past what is decoded, still decoding: starved");
        pcm.finish();
        assert!(!pcm.read_frame(3, &mut out), "past the end of a finished track: silence");
        assert_eq!(out, [0.0, 0.0]);
    }

    #[test]
    fn channels_are_mapped_to_the_device() {
        let mono = Pcm::from_samples(1000, 1, &[16384]);
        let mut quad = [9.0f32; 4];
        mono.read_frame(0, &mut quad);
        assert_eq!(quad, [0.5, 0.5, 0.0, 0.0]);
        let stereo = Pcm::from_samples(1000, 2, &[16384, -16384]);
        let mut one = [9.0f32; 1];
        stereo.read_frame(0, &mut one);
        assert_eq!(one, [0.5]);
    }

    #[test]
    fn output_consumes_only_while_playing() {
        let out = output(Track::with_pcm(ramp(200)), 100);
        let t0 = out.shared.epoch;
        let mut buf = [1.0f32; 40];
        out.fill(&mut buf, t0, None);
        assert!(buf.iter().all(|&s| s == 0.0));
        assert_eq!(out.shared.consumed(), 0, "paused output is silence and consumes nothing");
        out.shared.playing.store(true, Ordering::SeqCst);
        out.fill(&mut buf, at(t0, 0.2), None);
        assert_eq!(out.shared.consumed(), 20);
        out.fill(&mut buf, at(t0, 0.4), None);
        assert_eq!(first_left(&buf), 20, "the cursor carries on where it stopped");
    }

    #[test]
    fn muting_keeps_consuming_and_unmuting_lands_in_place() {
        let out = output(Track::with_pcm(ramp(1000)), 1000);
        let t0 = out.shared.epoch;
        out.shared.playing.store(true, Ordering::SeqCst);
        let mut buf = [0.0f32; 20];
        out.fill(&mut buf, t0, None);
        out.shared.muted.store(true, Ordering::SeqCst);
        for i in 1..=5 {
            out.fill(&mut buf, at(t0, f64::from(i) * 0.01), None);
            assert!(buf.iter().all(|&s| s == 0.0), "muted output is zeros");
        }
        assert_eq!(out.shared.consumed(), 60, "and the cursor still advances");
        out.shared.muted.store(false, Ordering::SeqCst);
        out.fill(&mut buf, at(t0, 0.06), None);
        assert_eq!(first_left(&buf), 60, "unmute is immediate and in sync");
        assert_eq!(out.shared.underruns.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_seek_moves_the_read_cursor_and_a_loop_wraps_it() {
        let out = output(Track::with_pcm(ramp(1000)), 1000);
        let t0 = out.shared.epoch;
        out.shared.playing.store(true, Ordering::SeqCst);
        out.shared.request_seek(700);
        let mut buf = [0.0f32; 20];
        out.fill(&mut buf, t0, None);
        assert_eq!(first_left(&buf), 700);
        out.shared.loop_len.store(710, Ordering::SeqCst);
        out.fill(&mut buf, at(t0, 0.01), None);
        assert_eq!(first_left(&buf), 0, "frame 710 wraps to the top of the loop");
    }

    #[test]
    fn a_track_still_decoding_plays_silence_and_counts_the_starve() {
        let track = Track::new();
        let out = output(track.clone(), 1000);
        let t0 = out.shared.epoch;
        out.shared.playing.store(true, Ordering::SeqCst);
        let mut buf = [1.0f32; 20];
        out.fill(&mut buf, t0, None);
        assert!(buf.iter().all(|&s| s == 0.0));
        assert_eq!(out.shared.consumed(), 10, "the clock keeps moving while the track loads");
        assert_eq!(out.shared.underruns.load(Ordering::SeqCst), 1);
        track.publish(ramp(100));
        out.fill(&mut buf, at(t0, 0.01), None);
        assert_eq!(first_left(&buf), 10, "sound comes in at the cursor, in sync");
    }

    fn until(cond: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        cond()
    }

    #[cfg(feature = "audio")]
    #[test]
    fn only_a_gone_device_retires_the_stream() {
        use cpal::ErrorKind;
        let shared = AudioShared::new(1000);
        for kind in [ErrorKind::DeviceChanged, ErrorKind::RealtimeDenied, ErrorKind::DeviceBusy] {
            device::on_stream_error(&shared, kind);
            assert!(!shared.lost.load(Ordering::SeqCst), "{kind:?}: the stream keeps playing");
        }
        device::on_stream_error(&shared, ErrorKind::Xrun);
        assert!(!shared.lost.load(Ordering::SeqCst), "a glitch is not a lost device");
        assert_eq!(shared.underruns.load(Ordering::SeqCst), 1, "it is counted as an underrun");
        for kind in [ErrorKind::DeviceNotAvailable, ErrorKind::HostUnavailable, ErrorKind::StreamInvalidated] {
            let shared = AudioShared::new(1000);
            device::on_stream_error(&shared, kind);
            assert!(shared.lost.load(Ordering::SeqCst), "{kind:?} retires the stream");
        }
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
        let out = output(Track::with_pcm(ramp(30_000)), 10_000);
        let shared = out.shared.clone();
        shared.playing.store(true, Ordering::SeqCst);
        let started = Instant::now();
        let sink = NullSink::start(out);
        assert!(until(|| shared.consumed() >= 100), "the null sink never pulled");
        drop(sink);
        let elapsed = started.elapsed().as_secs_f64();
        let consumed = shared.consumed();
        assert!(consumed as f64 <= elapsed * 10_000.0 + 1.0, "{consumed} frames in {elapsed:.3}s is faster than real time");
        let after = shared.callbacks.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(shared.callbacks.load(Ordering::SeqCst), after, "dropping the sink stops it");
    }
}
