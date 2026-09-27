//! The player's soundtrack: find the asset's sound ([`source::discover`]),
//! decode it to memory in the background, play it through an output (cpal
//! under the `audio` feature, or a real-time null sink for `--sim` and tests)
//! and hand the picture its media time from what that output has actually
//! played ([`clock::AudioClock`]). Seeks move the output's read cursor, pause
//! stops consumption, a loop wraps the cursor at the video's length, and mute
//! keeps consuming so the clock never breaks. Outages are recoverable: when
//! the output stops calling back (not counting a freeze of the whole process)
//! the clock hands over to a pausable wall clock from the current position,
//! the HUD reads `sound: wait`, and the first callback after it re-seeks the
//! sound to the wall position and makes it master again. A lost device is
//! dropped and the default output re-opened through a [`Reopener`] every
//! [`REOPEN_EVERY`]. Only a failed decode is permanent (`sound: none`). Every
//! fallback is silent playback, never an error; what happened is reported as
//! notes after exit. Single assets only: compositions play silently.

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

pub const REOPEN_EVERY: Duration = Duration::from_secs(2);

pub const SIM_RATE: u32 = 48_000;

pub const SIM_CHANNELS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    On,
    Off,
    Wait,
    None,
}

impl Sound {
    pub fn label(self) -> &'static str {
        match self {
            Sound::On => "on",
            Sound::Off => "off",
            Sound::Wait => "wait",
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

type ReopenFn = dyn FnMut(&DeviceFormat, Output) -> Result<Sink, String>;

pub struct Reopener(Box<ReopenFn>);

impl std::fmt::Debug for Reopener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reopener")
    }
}

impl Reopener {
    pub fn new(f: impl FnMut(&DeviceFormat, Output) -> Result<Sink, String> + 'static) -> Reopener {
        Reopener(Box::new(f))
    }

    pub fn for_choice(choice: &SinkChoice) -> Reopener {
        let choice = choice.clone();
        Reopener::new(move |want, output| {
            let (opened, got) = open_sink(choice.clone())?;
            if (got.rate, got.channels) != (want.rate, want.channels) {
                return Err(format!(
                    "the default output is now {} Hz x{}, the track was decoded for {} Hz x{}",
                    got.rate, got.channels, want.rate, want.channels
                ));
            }
            start_sink(opened, output)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outage {
    Live,
    Stalled { callbacks: u64 },
    Lost { next_try: Instant, noted: bool },
    Failed,
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
    outage: Outage,
    reopen: Option<Reopener>,
    armed: Instant,
    last_poll: Option<Instant>,
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
        let reopen = Reopener::for_choice(&sink);
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
        let mut opening = Soundtrack::start(found, tools.ffmpeg, video_secs, opts, opened, format, capacity);
        if let Some(st) = opening.soundtrack.as_mut() {
            st.set_reopen(reopen);
        }
        opening
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
            outage: Outage::Live,
            reopen: None,
            armed: Instant::now(),
            last_poll: None,
        }
    }

    pub fn set_reopen(&mut self, reopen: Reopener) {
        self.reopen = Some(reopen);
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
        self.outage == Outage::Live
    }

    pub fn sound(&self) -> Sound {
        match (self.outage, self.shared.muted.load(Ordering::SeqCst)) {
            (Outage::Failed, _) => Sound::None,
            (Outage::Stalled { .. } | Outage::Lost { .. }, _) => Sound::Wait,
            (Outage::Live, true) => Sound::Off,
            (Outage::Live, false) => Sound::On,
        }
    }

    pub fn toggle_mute(&mut self) -> Sound {
        if self.outage != Outage::Failed {
            self.shared.muted.fetch_xor(true, Ordering::SeqCst);
        }
        self.sound()
    }

    pub fn poll(&mut self, at: Instant) -> bool {
        let frozen = self.last_poll.is_some_and(|p| at.saturating_duration_since(p) > DEVICE_SILENT);
        self.last_poll = Some(at);
        if frozen {
            self.armed = at;
        }
        if self.outage == Outage::Failed {
            return false;
        }
        if self.track.failed() {
            self.clock.mark_ended(at);
            self.sink = None;
            self.outage = Outage::Failed;
            self.note(format!("{}: the track could not be decoded; continued silently", self.format.name));
            return true;
        }
        if self.shared.lost.swap(false, Ordering::SeqCst) && !matches!(self.outage, Outage::Lost { .. }) {
            self.clock.mark_ended(at);
            self.sink = None;
            self.outage = Outage::Lost { next_try: at + REOPEN_EVERY, noted: false };
            self.note(format!(
                "{}: the audio output was lost; continued silently, re-opening it every {} s",
                self.format.name,
                REOPEN_EVERY.as_secs()
            ));
            return true;
        }
        match self.outage {
            Outage::Live => {
                let heard = self.shared.last_callback().unwrap_or(self.started).max(self.started).max(self.armed);
                if at.saturating_duration_since(heard) <= DEVICE_SILENT {
                    return false;
                }
                self.clock.mark_ended(at);
                self.outage = Outage::Stalled { callbacks: self.shared.callbacks.load(Ordering::SeqCst) };
                self.note(format!(
                    "{}: the audio output stopped calling back; continued silently until it resumed",
                    self.format.name
                ));
                true
            }
            Outage::Stalled { callbacks } if self.shared.callbacks.load(Ordering::SeqCst) > callbacks => {
                self.clock.unmark(at);
                self.outage = Outage::Live;
                self.armed = at;
                true
            }
            Outage::Lost { next_try, noted } if at >= next_try => {
                let output = self.output();
                let result = match self.reopen.as_mut() {
                    Some(r) => (r.0)(&self.format, output),
                    None => Err("no way to re-open it".into()),
                };
                match result {
                    Ok(sink) => {
                        self.sink = Some(sink);
                        self.outage = Outage::Stalled { callbacks: self.shared.callbacks.load(Ordering::SeqCst) };
                        self.armed = at;
                    }
                    Err(e) => {
                        if !noted {
                            self.note(format!("{}: re-opening the audio output failed ({e}); still retrying", self.format.name));
                        }
                        self.outage = Outage::Lost { next_try: at + REOPEN_EVERY, noted: true };
                    }
                }
                false
            }
            _ => false,
        }
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
            clock: if self.clock.is_ended() { "wall" } else { "audio" },
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
