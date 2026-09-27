//! Media time for the picture, ported from the `yt-stream` branch's
//! `stream/clock.rs` and extended with seeks and loop wraps. `AudioClock` is
//! the track position the output has actually played: the cursor it restarted
//! from at the last applied seek, plus the frames consumed since, over the
//! sample rate, minus the output latency, held monotonic between seeks.
//! `MonotonicClock` is a pausable wall clock; `AudioClock::mark_ended` hands
//! over to one when the output stops calling back, from the current position.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub trait Clock {
    fn now(&self, at: Instant) -> f64;
    fn set_running(&mut self, running: bool, at: Instant);
    fn seek(&mut self, secs: f64, at: Instant);
    fn mark_ended(&mut self, _at: Instant) {}
}

#[derive(Clone, Copy, Debug)]
pub struct MonotonicClock {
    base: f64,
    since: Option<Instant>,
}

impl MonotonicClock {
    pub fn new() -> MonotonicClock {
        MonotonicClock { base: 0.0, since: None }
    }
}

impl Default for MonotonicClock {
    fn default() -> MonotonicClock {
        MonotonicClock::new()
    }
}

impl Clock for MonotonicClock {
    fn now(&self, at: Instant) -> f64 {
        self.base + self.since.map_or(0.0, |s| at.saturating_duration_since(s).as_secs_f64())
    }

    fn set_running(&mut self, running: bool, at: Instant) {
        match (running, self.since) {
            (true, None) => self.since = Some(at),
            (false, Some(_)) => {
                self.base = self.now(at);
                self.since = None;
            }
            _ => {}
        }
    }

    fn seek(&mut self, secs: f64, at: Instant) {
        self.base = secs;
        self.since = self.since.map(|_| at);
    }
}

#[derive(Debug, Default)]
struct SeekRequest {
    frame: Option<u64>,
    generation: u64,
}

#[derive(Debug)]
pub struct AudioShared {
    pub rate: u32,
    pub epoch: Instant,
    pub consumed: AtomicU64,
    pub last_chunk: AtomicU64,
    pub last_callback_ns: AtomicU64,
    pub latency_ns: AtomicU64,
    pub playing: AtomicBool,
    pub muted: AtomicBool,
    pub underruns: AtomicU64,
    pub callbacks: AtomicU64,
    pub lost: AtomicBool,
    pub base: AtomicU64,
    pub loop_len: AtomicU64,
    pub applied: AtomicU64,
    seek: Mutex<SeekRequest>,
}

impl AudioShared {
    pub fn new(rate: u32) -> Arc<AudioShared> {
        Arc::new(AudioShared {
            rate: rate.max(1),
            epoch: Instant::now(),
            consumed: AtomicU64::new(0),
            last_chunk: AtomicU64::new(0),
            last_callback_ns: AtomicU64::new(0),
            latency_ns: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            callbacks: AtomicU64::new(0),
            lost: AtomicBool::new(false),
            base: AtomicU64::new(0),
            loop_len: AtomicU64::new(0),
            applied: AtomicU64::new(0),
            seek: Mutex::new(SeekRequest::default()),
        })
    }

    pub fn record(&self, consumed: u64, at: Instant, latency: Option<Duration>) {
        self.consumed.fetch_add(consumed, Ordering::SeqCst);
        self.last_chunk.store(consumed, Ordering::SeqCst);
        let ns = at.saturating_duration_since(self.epoch).as_nanos() as u64;
        self.last_callback_ns.store(ns, Ordering::SeqCst);
        if let Some(l) = latency {
            self.latency_ns.store(l.as_nanos() as u64, Ordering::SeqCst);
        }
        self.callbacks.fetch_add(1, Ordering::SeqCst);
    }

    pub fn consumed(&self) -> u64 {
        self.consumed.load(Ordering::SeqCst)
    }

    pub fn request_seek(&self, frame: u64) -> u64 {
        let mut req = self.seek.lock().unwrap_or_else(|p| p.into_inner());
        req.generation += 1;
        req.frame = Some(frame);
        req.generation
    }

    pub fn take_seek(&self) {
        let Ok(mut req) = self.seek.try_lock() else {
            return;
        };
        if let Some(frame) = req.frame.take() {
            self.base.store(frame, Ordering::SeqCst);
            self.consumed.store(0, Ordering::SeqCst);
            self.last_chunk.store(0, Ordering::SeqCst);
            self.applied.store(req.generation, Ordering::SeqCst);
        }
    }

    pub fn cursor(&self) -> u64 {
        self.base.load(Ordering::SeqCst) + self.consumed()
    }

    pub fn track_frame(&self, pos: u64) -> u64 {
        match self.loop_len.load(Ordering::SeqCst) {
            0 => pos,
            len => pos % len,
        }
    }

    pub fn last_callback(&self) -> Option<Instant> {
        (self.callbacks.load(Ordering::SeqCst) > 0).then(|| {
            self.epoch + Duration::from_nanos(self.last_callback_ns.load(Ordering::SeqCst))
        })
    }
}

#[derive(Debug)]
pub struct AudioClock {
    shared: Arc<AudioShared>,
    ended: Option<MonotonicClock>,
    high: Cell<f64>,
    pending: u64,
}

impl AudioClock {
    pub fn new(shared: Arc<AudioShared>) -> AudioClock {
        AudioClock { shared, ended: None, high: Cell::new(0.0), pending: 0 }
    }

    pub fn samples_time(&self, at: Instant) -> f64 {
        let s = &self.shared;
        let rate = f64::from(s.rate);
        let consumed = s.consumed.load(Ordering::SeqCst);
        let chunk = s.last_chunk.load(Ordering::SeqCst).min(consumed);
        let cb = s.epoch + Duration::from_nanos(s.last_callback_ns.load(Ordering::SeqCst));
        let settled = (consumed - chunk) as f64 / rate;
        let within = at.saturating_duration_since(cb).as_secs_f64().min(chunk as f64 / rate);
        let latency = s.latency_ns.load(Ordering::SeqCst) as f64 / 1e9;
        s.base.load(Ordering::SeqCst) as f64 / rate + settled + within - latency
    }

    pub fn is_ended(&self) -> bool {
        self.ended.is_some()
    }

    pub fn is_settled(&self) -> bool {
        self.shared.applied.load(Ordering::SeqCst) >= self.pending
    }
}

impl Clock for AudioClock {
    fn now(&self, at: Instant) -> f64 {
        if let Some(wall) = &self.ended {
            return wall.now(at);
        }
        if !self.is_settled() {
            return self.high.get();
        }
        let t = self.samples_time(at).max(self.high.get());
        self.high.set(t);
        t
    }

    fn set_running(&mut self, running: bool, at: Instant) {
        self.shared.playing.store(running, Ordering::SeqCst);
        if let Some(wall) = &mut self.ended {
            wall.set_running(running, at);
        }
    }

    fn seek(&mut self, secs: f64, at: Instant) {
        let secs = secs.max(0.0);
        let frame = (secs * f64::from(self.shared.rate)).round() as u64;
        self.pending = self.shared.request_seek(frame);
        self.high.set(secs);
        if let Some(wall) = &mut self.ended {
            wall.seek(secs, at);
        }
    }

    fn mark_ended(&mut self, at: Instant) {
        if self.ended.is_none() {
            let mut wall = MonotonicClock { base: self.now(at), since: None };
            wall.set_running(self.shared.playing.load(Ordering::SeqCst), at);
            self.ended = Some(wall);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: f64) -> Instant {
        base + Duration::from_secs_f64(secs)
    }

    fn apply(shared: &AudioShared) {
        shared.take_seek();
    }

    #[test]
    fn the_monotonic_clock_pauses_resumes_and_seeks() {
        let t0 = Instant::now();
        let mut c = MonotonicClock::new();
        assert_eq!(c.now(at(t0, 5.0)), 0.0, "not started");
        c.set_running(true, t0);
        assert!((c.now(at(t0, 1.5)) - 1.5).abs() < 1e-9);
        c.set_running(false, at(t0, 2.0));
        assert!((c.now(at(t0, 9.0)) - 2.0).abs() < 1e-9);
        c.set_running(true, at(t0, 10.0));
        assert!((c.now(at(t0, 10.5)) - 2.5).abs() < 1e-9);
        c.seek(40.0, at(t0, 11.0));
        assert!((c.now(at(t0, 12.0)) - 41.0).abs() < 1e-9, "a seek rebases a running clock");
        c.set_running(false, at(t0, 12.0));
        c.seek(3.0, at(t0, 13.0));
        assert!((c.now(at(t0, 20.0)) - 3.0).abs() < 1e-9, "and holds a paused one");
    }

    #[test]
    fn the_audio_clock_advances_only_on_consumed_samples() {
        let shared = AudioShared::new(1000);
        let mut clock = AudioClock::new(shared.clone());
        let t0 = shared.epoch;
        clock.set_running(true, t0);
        assert_eq!(clock.now(at(t0, 3.0)), 0.0, "nothing consumed, no time");
        shared.record(100, at(t0, 0.1), None);
        assert!((clock.now(at(t0, 0.1)) - 0.0).abs() < 1e-9, "the chunk starts playing at the callback");
        assert!((clock.now(at(t0, 0.15)) - 0.05).abs() < 1e-9, "interpolated inside the chunk");
        assert!((clock.now(at(t0, 5.0)) - 0.1).abs() < 1e-9, "never past what was consumed");
        shared.record(100, at(t0, 0.2), Some(Duration::from_millis(40)));
        assert!((clock.now(at(t0, 0.25)) - 0.11).abs() < 1e-9, "output latency is subtracted");
    }

    #[test]
    fn a_seek_pins_the_clock_until_the_output_applies_it() {
        let shared = AudioShared::new(1000);
        let mut clock = AudioClock::new(shared.clone());
        let t0 = shared.epoch;
        clock.set_running(true, t0);
        shared.record(500, at(t0, 0.5), None);
        assert!((clock.now(at(t0, 1.0)) - 0.5).abs() < 1e-9);
        clock.seek(10.0, at(t0, 1.0));
        assert!(!clock.is_settled());
        assert_eq!(clock.now(at(t0, 1.3)), 10.0, "pinned at the target before the output hears it");
        apply(&shared);
        assert!(clock.is_settled());
        assert_eq!(shared.cursor(), 10_000, "the read cursor moved to the target");
        shared.record(100, at(t0, 1.4), Some(Duration::from_millis(30)));
        assert_eq!(clock.now(at(t0, 1.41)), 10.0, "held while the latency drains, never backwards");
        assert!((clock.now(at(t0, 1.48)) - 10.05).abs() < 1e-9);
        clock.seek(2.0, at(t0, 1.5));
        apply(&shared);
        shared.record(100, at(t0, 1.6), None);
        assert!((clock.now(at(t0, 1.65)) - 2.02).abs() < 1e-9, "a backward seek resets the high-water mark");
    }

    #[test]
    fn a_seek_waits_for_the_output_when_the_request_is_contended() {
        let shared = AudioShared::new(1000);
        let mut clock = AudioClock::new(shared.clone());
        clock.seek(4.0, shared.epoch);
        {
            let _held = shared.seek.lock().unwrap();
            shared.take_seek();
        }
        assert!(!clock.is_settled(), "a contended callback skips, it never blocks");
        shared.take_seek();
        assert!(clock.is_settled());
        assert_eq!(shared.base.load(Ordering::SeqCst), 4000);
    }

    #[test]
    fn loops_wrap_the_track_position_not_the_clock() {
        let shared = AudioShared::new(1000);
        shared.loop_len.store(2500, Ordering::SeqCst);
        assert_eq!(shared.track_frame(2499), 2499);
        assert_eq!(shared.track_frame(2500), 0);
        assert_eq!(shared.track_frame(2500 * 400 + 7), 7, "no drift across many wraps");
    }

    #[test]
    fn pausing_freezes_the_audio_clock() {
        let shared = AudioShared::new(1000);
        let mut clock = AudioClock::new(shared.clone());
        let t0 = shared.epoch;
        clock.set_running(true, t0);
        shared.record(100, at(t0, 0.1), None);
        clock.set_running(false, at(t0, 0.15));
        shared.record(0, at(t0, 0.2), None);
        let frozen = clock.now(at(t0, 0.2));
        for i in 3..30 {
            shared.record(0, at(t0, f64::from(i) * 0.1), None);
            assert_eq!(clock.now(at(t0, f64::from(i) * 0.1 + 0.05)), frozen, "paused: the clock must not move");
        }
    }

    #[test]
    fn after_the_output_stops_the_clock_runs_on_the_wall_and_still_seeks() {
        let shared = AudioShared::new(1000);
        let mut clock = AudioClock::new(shared.clone());
        let t0 = shared.epoch;
        clock.set_running(true, t0);
        shared.record(500, at(t0, 0.5), None);
        clock.mark_ended(at(t0, 1.0));
        clock.mark_ended(at(t0, 1.5));
        assert!(clock.is_ended());
        assert!((clock.now(at(t0, 2.0)) - 1.5).abs() < 1e-9);
        clock.set_running(false, at(t0, 2.0));
        assert!((clock.now(at(t0, 9.0)) - 1.5).abs() < 1e-9, "the wall clock pauses");
        clock.seek(0.25, at(t0, 9.0));
        clock.set_running(true, at(t0, 10.0));
        assert!((clock.now(at(t0, 10.5)) - 0.75).abs() < 1e-9, "and seeks");
    }
}
