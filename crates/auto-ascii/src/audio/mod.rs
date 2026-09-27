//! The player's soundtrack: find the asset's sound ([`source::discover`]),
//! decode it to memory in the background, play it through an output (cpal
//! under the `audio` feature, or a real-time null sink for `--sim` and tests)
//! and hand the picture its media time from what that output has actually
//! played ([`clock::AudioClock`]). Seeks move the output's read cursor, pause
//! stops consumption, a loop wraps the cursor at the video's length, and mute
//! keeps consuming so the clock never breaks. When the output stops calling
//! back or the decode fails, the clock hands over to a pausable wall clock
//! from the current position and the HUD reads `sound: none`. Every fallback
//! is silent playback, never an error; what happened is reported as notes
//! after exit. Single assets only: compositions play silently.

pub mod clock;
pub mod output;
pub mod source;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clock::{AudioClock, AudioShared, Clock};
use output::{DeviceFormat, NullSink, Output, Sink, Track};
use source::{Decoder, TrackSource};

pub const DEVICE_SILENT: Duration = Duration::from_millis(1500);

pub const SIM_RATE: u32 = 48_000;

pub const SIM_CHANNELS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    On,
    Off,
    None,
}

impl Sound {
    pub fn label(self) -> &'static str {
        match self {
            Sound::On => "on",
            Sound::Off => "off",
            Sound::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SoundOptions {
    pub no_audio: bool,
    pub muted: bool,
    pub looping: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SinkChoice {
    Device,
    Null(DeviceFormat),
    Unavailable(String),
}

impl SinkChoice {
    pub fn null() -> SinkChoice {
        SinkChoice::Null(DeviceFormat {
            rate: SIM_RATE,
            channels: SIM_CHANNELS,
            name: "null (real-time paced)".into(),
        })
    }
}

enum Opened {
    #[cfg(feature = "audio")]
    Device(Box<output::device::Opened>),
    Null,
}

fn open_sink(choice: SinkChoice) -> Result<(Opened, DeviceFormat), String> {
    match choice {
        SinkChoice::Null(format) => Ok((Opened::Null, format)),
        SinkChoice::Unavailable(why) => Err(why),
        #[cfg(feature = "audio")]
        SinkChoice::Device => {
            output::device::open().map(|(opened, format)| (Opened::Device(Box::new(opened)), format))
        }
        #[cfg(not(feature = "audio"))]
        SinkChoice::Device => Err("built without the `audio` feature".into()),
    }
}

fn start_sink(opened: Opened, output: Output) -> Result<Sink, String> {
    match opened {
        Opened::Null => Ok(Sink::null(NullSink::start(output))),
        #[cfg(feature = "audio")]
        Opened::Device(opened) => output::device::start(*opened, output).map(Sink::device),
    }
}

#[derive(Debug, Default)]
pub struct Opening {
    pub soundtrack: Option<Soundtrack>,
    pub notes: Vec<String>,
}

impl Opening {
    fn silent(note: Option<String>) -> Opening {
        Opening { soundtrack: None, notes: note.into_iter().collect() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SoundStats {
    pub source: PathBuf,
    pub device: String,
    pub rate: u32,
    pub channels: usize,
    pub decoded_secs: f64,
    pub decoded: bool,
    pub callbacks: u64,
    pub underruns: u64,
    pub clock: &'static str,
}

#[derive(Debug)]
pub struct Soundtrack {
    sink: Option<Sink>,
    decoder: Option<Decoder>,
    shared: Arc<AudioShared>,
    track: Arc<Track>,
    clock: AudioClock,
    source: TrackSource,
    format: DeviceFormat,
    loop_secs: Option<f64>,
    started: Instant,
    notes: Arc<Mutex<Vec<String>>>,
}

impl Soundtrack {
    pub fn open(asset: &Path, video_secs: f64, opts: SoundOptions, sink: SinkChoice) -> Opening {
        if opts.no_audio {
            return Opening::silent(None);
        }
        let Some(found) = source::discover(asset) else {
            return Opening::silent(None);
        };
        let name = found.path().display().to_string();
        let tools = match source::Tools::find() {
            Ok(t) => t,
            Err(e) => return Opening::silent(Some(format!("{name}: {e}; played silently"))),
        };
        let probe = match source::probe(&tools, found.path()) {
            Ok(p) => p,
            Err(e) => return Opening::silent(Some(format!("{name}: {e}; played silently"))),
        };
        let Some(audio_secs) = probe.secs.filter(|_| probe.has_audio) else {
            return Opening::silent(Some(format!("{name}: no audio stream; played silently")));
        };
        if source::duration_mismatch(video_secs, audio_secs) {
            return Opening::silent(Some(format!(
                "{name}: {audio_secs:.1} s of sound does not match the {video_secs:.1} s video \
                 (a different cut?); not played"
            )));
        }
        let (opened, format) = match open_sink(sink) {
            Ok(o) => o,
            Err(e) => return Opening::silent(Some(format!("no audio output ({e}); played silently"))),
        };
        let stored = format.channels.clamp(1, 2);
        let capacity = source::capacity_frames(audio_secs, format.rate);
        if capacity.saturating_mul(stored * 2) > source::PCM_BUDGET_BYTES {
            return Opening::silent(Some(format!(
                "{name}: {audio_secs:.0} s of sound is over the {} MiB in-memory budget; played silently",
                source::PCM_BUDGET_BYTES >> 20
            )));
        }
        Soundtrack::start(found, tools.ffmpeg, video_secs, opts, opened, format, capacity)
    }

    fn start(
        found: TrackSource,
        ffmpeg: PathBuf,
        video_secs: f64,
        opts: SoundOptions,
        opened: Opened,
        format: DeviceFormat,
        capacity: usize,
    ) -> Opening {
        let track = Track::new();
        let mut soundtrack = Soundtrack::with_track(track.clone(), video_secs, opts, format);
        match start_sink(opened, soundtrack.output()) {
            Ok(s) => soundtrack.sink = Some(s),
            Err(e) => return Opening::silent(Some(format!("no audio output ({e}); played silently"))),
        }
        soundtrack.started = Instant::now();
        soundtrack.decoder = Some(source::spawn(source::DecodeJob {
            ffmpeg,
            path: found.path().to_path_buf(),
            rate: soundtrack.format.rate,
            channels: soundtrack.format.channels.clamp(1, 2),
            capacity_frames: capacity,
            track,
            notes: soundtrack.notes.clone(),
        }));
        soundtrack.source = found;
        Opening { soundtrack: Some(soundtrack), notes: Vec::new() }
    }

    pub fn with_track(track: Arc<Track>, video_secs: f64, opts: SoundOptions, format: DeviceFormat) -> Soundtrack {
        let shared = AudioShared::new(format.rate);
        let loop_len = (video_secs * f64::from(shared.rate)).round() as u64;
        if opts.looping && loop_len > 0 {
            shared.loop_len.store(loop_len, Ordering::SeqCst);
        }
        shared.muted.store(opts.muted, Ordering::SeqCst);
        Soundtrack {
            sink: None,
            decoder: None,
            clock: AudioClock::new(shared.clone()),
            loop_secs: (opts.looping && loop_len > 0).then(|| loop_len as f64 / f64::from(shared.rate)),
            shared,
            track,
            source: TrackSource::Sidecar(PathBuf::new()),
            format,
            started: Instant::now(),
            notes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn output(&self) -> Output {
        Output { track: self.track.clone(), shared: self.shared.clone(), channels: self.format.channels.max(1) }
    }

    pub fn shared(&self) -> &Arc<AudioShared> {
        &self.shared
    }

    pub fn source(&self) -> &Path {
        self.source.path()
    }

    pub fn now_secs(&self, at: Instant) -> f64 {
        let t = self.clock.now(at);
        match self.loop_secs {
            Some(len) => t.rem_euclid(len),
            None => t,
        }
    }

    pub fn seek(&mut self, secs: f64, at: Instant) {
        self.clock.seek(secs, at);
    }

    pub fn set_running(&mut self, running: bool, at: Instant) {
        self.clock.set_running(running, at);
    }

    pub fn is_live(&self) -> bool {
        !self.clock.is_ended()
    }

    pub fn sound(&self) -> Sound {
        match (self.is_live(), self.shared.muted.load(Ordering::SeqCst)) {
            (false, _) => Sound::None,
            (true, true) => Sound::Off,
            (true, false) => Sound::On,
        }
    }

    pub fn toggle_mute(&mut self) -> Sound {
        if self.is_live() {
            self.shared.muted.fetch_xor(true, Ordering::SeqCst);
        }
        self.sound()
    }

    pub fn poll(&mut self, at: Instant) -> bool {
        if !self.is_live() {
            return false;
        }
        let heard = self.shared.last_callback().unwrap_or(self.started).max(self.started);
        let why = if self.track.failed() {
            "the track could not be decoded"
        } else if self.shared.lost.load(Ordering::SeqCst) {
            "the audio output reported an error"
        } else if at.saturating_duration_since(heard) > DEVICE_SILENT {
            "the audio output stopped calling back"
        } else {
            return false;
        };
        self.retire(why, at);
        true
    }

    fn retire(&mut self, why: &str, at: Instant) {
        self.clock.mark_ended(at);
        self.sink = None;
        self.note(format!("{}: {why}; continued silently", self.format.name));
    }

    fn note(&self, msg: String) {
        let mut notes = self.notes.lock().unwrap_or_else(|p| p.into_inner());
        if !notes.contains(&msg) {
            notes.push(msg);
        }
    }

    pub fn stats(&self) -> SoundStats {
        let (decoded_secs, decoded) = match self.track.pcm() {
            Some(p) => (p.ready_frames() as f64 / f64::from(p.rate), p.is_done()),
            None => (0.0, false),
        };
        SoundStats {
            source: self.source.path().to_path_buf(),
            device: self.format.name.clone(),
            rate: self.format.rate,
            channels: self.format.channels,
            decoded_secs,
            decoded,
            callbacks: self.shared.callbacks.load(Ordering::SeqCst),
            underruns: self.shared.underruns.load(Ordering::SeqCst),
            clock: if self.is_live() { "audio" } else { "wall" },
        }
    }

    pub fn wait_decoded(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.decoder.as_ref().is_none_or(Decoder::is_finished) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    pub fn finish(mut self) -> Vec<String> {
        self.sink = None;
        if let Some(mut d) = self.decoder.take() {
            d.stop();
        }
        std::mem::take(&mut *self.notes.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

#[cfg(test)]
mod tests;
