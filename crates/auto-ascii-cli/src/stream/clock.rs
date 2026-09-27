//! Media time for the picture. `AudioClock` is the samples the output has
//! actually consumed over the sample rate, minus the output latency, so an
//! underrun stops it; `MonotonicClock` is a pausable wall clock for silent
//! runs. `pick_frame` is the pure newest-frame-not-after-now decision.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub trait Clock {
    fn now(&self, at: Instant) -> f64;
    fn set_running(&mut self, running: bool, at: Instant);
    fn mark_ended(&mut self, _at: Instant) {}
}

pub struct MonotonicClock {
    base: f64,
    since: Option<Instant>,
}

impl MonotonicClock {
    pub fn new() -> MonotonicClock {
        MonotonicClock { base: 0.0, since: None }
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
}

pub struct AudioShared {
    pub rate: u32,
    pub epoch: Instant,
    pub consumed: AtomicU64,
    pub last_chunk: AtomicU64,
    pub last_callback_ns: AtomicU64,
    pub latency_ns: AtomicU64,
    pub playing: AtomicBool,
    pub underruns: AtomicU64,
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
            underruns: AtomicU64::new(0),
        })
    }

    pub fn record(&self, consumed: u64, requested: u64, at: Instant, latency: Option<Duration>, drained: bool) {
        self.consumed.fetch_add(consumed, Ordering::SeqCst);
        self.last_chunk.store(consumed, Ordering::SeqCst);
        let ns = at.saturating_duration_since(self.epoch).as_nanos() as u64;
        self.last_callback_ns.store(ns, Ordering::SeqCst);
        if let Some(l) = latency {
            self.latency_ns.store(l.as_nanos() as u64, Ordering::SeqCst);
        }
        if consumed < requested && !drained && self.playing.load(Ordering::SeqCst) {
            self.underruns.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn consumed(&self) -> u64 {
        self.consumed.load(Ordering::SeqCst)
    }
}

pub struct AudioClock {
    shared: Arc<AudioShared>,
    ended: Option<(f64, Instant)>,
    high: Cell<f64>,
}

impl AudioClock {
    pub fn new(shared: Arc<AudioShared>) -> AudioClock {
        AudioClock { shared, ended: None, high: Cell::new(0.0) }
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
        (settled + within - latency).max(0.0)
    }
}

impl Clock for AudioClock {
    fn now(&self, at: Instant) -> f64 {
        let t = match self.ended {
            Some((t, from)) => t + at.saturating_duration_since(from).as_secs_f64(),
            None => self.samples_time(at),
        };
        let t = t.max(self.high.get());
        self.high.set(t);
        t
    }

    fn set_running(&mut self, running: bool, _at: Instant) {
        self.shared.playing.store(running, Ordering::SeqCst);
    }

    fn mark_ended(&mut self, at: Instant) {
        if self.ended.is_none() {
            self.ended = Some((self.now(at), at));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Empty,
    Wait,
    Show { index: usize, dropped: usize },
    Stale { dropped: usize },
}

pub fn pick_frame(pts: &[f64], now: f64, period: f64) -> Pick {
    let due = pts.partition_point(|&p| p <= now + 1e-9);
    let stale = pts.partition_point(|&p| p + period <= now + 1e-9);
    match (pts.is_empty(), due) {
        (true, _) => Pick::Empty,
        (false, 0) => Pick::Wait,
        (false, n) if stale >= n => Pick::Stale { dropped: n },
        (false, n) => Pick::Show { index: n - 1, dropped: n - 1 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: f64) -> Instant {
        base + Duration::from_secs_f64(secs)
    }

    #[test]
    fn pick_shows_the_newest_due_frame_and_drops_the_older_ones() {
        let pts = [0.0, 0.1, 0.2, 0.3];
        assert_eq!(pick_frame(&[], 5.0, 0.1), Pick::Empty);
        assert_eq!(pick_frame(&pts, -0.01, 0.1), Pick::Wait);
        assert_eq!(pick_frame(&pts, 0.0, 0.1), Pick::Show { index: 0, dropped: 0 });
        assert_eq!(pick_frame(&pts, 0.15, 0.1), Pick::Show { index: 1, dropped: 1 });
        assert_eq!(pick_frame(&pts, 0.35, 0.1), Pick::Show { index: 3, dropped: 3 });
        assert_eq!(pick_frame(&[1.0, 1.1], 0.5, 0.1), Pick::Wait, "the picture waits when it is ahead");
    }

    #[test]
    fn a_frame_whose_slot_has_passed_is_dropped_not_shown_late() {
        assert_eq!(pick_frame(&[0.0, 0.1, 0.2, 0.3], 9.0, 0.1), Pick::Stale { dropped: 4 });
        assert_eq!(pick_frame(&[2.0], 3.2, 1.0 / 30.0), Pick::Stale { dropped: 1 }, "1.2 s late: drop, never show");
        assert_eq!(pick_frame(&[2.0, 3.19], 3.2, 1.0 / 30.0), Pick::Show { index: 1, dropped: 1 });
        assert_eq!(pick_frame(&[2.0, 3.3], 3.2, 1.0 / 30.0), Pick::Stale { dropped: 1 }, "the next one is not due yet");
    }

    #[test]
    fn a_fake_clock_run_keeps_drift_under_one_frame_and_counts_drops() {
        let fps = 30.0;
        let period = 1.0 / fps;
        let mut queue: Vec<f64> = (0..300).map(|i| f64::from(i) / fps).collect();
        let steps = [0.004, 0.013, 0.021, 0.034, 0.05, 0.09, 0.002, 0.2, 0.017];
        let (mut now, mut shown, mut dropped, mut max_drift) = (0.0f64, 0usize, 0usize, 0.0f64);
        let mut k = 0;
        while !queue.is_empty() {
            match pick_frame(&queue, now, period) {
                Pick::Stale { dropped: d } => {
                    dropped += d;
                    queue.drain(..d);
                }
                Pick::Show { index, dropped: d } => {
                    let pts = queue[index];
                    max_drift = max_drift.max((now - pts).abs());
                    dropped += d;
                    shown += 1;
                    queue.drain(..=index);
                }
                Pick::Wait => assert!(queue[0] > now),
                Pick::Empty => unreachable!(),
            }
            now += steps[k % steps.len()];
            k += 1;
        }
        assert_eq!(shown + dropped, 300, "every frame is shown or counted as dropped");
        assert!(dropped > 0, "the 90 ms and 200 ms steps force drops");
        assert!(max_drift < period, "drift {max_drift} >= one frame period");
    }

    #[test]
    fn the_monotonic_clock_pauses_and_resumes() {
        let t0 = Instant::now();
        let mut c = MonotonicClock::new();
        assert_eq!(c.now(at(t0, 5.0)), 0.0, "not started");
        c.set_running(true, t0);
        assert!((c.now(at(t0, 1.5)) - 1.5).abs() < 1e-9);
        c.set_running(false, at(t0, 2.0));
        assert!((c.now(at(t0, 9.0)) - 2.0).abs() < 1e-9);
        c.set_running(true, at(t0, 10.0));
        assert!((c.now(at(t0, 10.5)) - 2.5).abs() < 1e-9);
    }

    #[test]
    fn the_audio_clock_advances_only_on_consumed_samples() {
        let shared = AudioShared::new(1000);
        let mut clock = AudioClock::new(shared.clone());
        let t0 = shared.epoch;
        clock.set_running(true, t0);
        assert_eq!(clock.now(at(t0, 3.0)), 0.0, "nothing consumed, no time");
        shared.record(100, 100, at(t0, 0.1), None, false);
        assert!((clock.now(at(t0, 0.1)) - 0.0).abs() < 1e-9, "the chunk starts playing at the callback");
        assert!((clock.now(at(t0, 0.15)) - 0.05).abs() < 1e-9, "interpolated inside the chunk");
        assert!((clock.now(at(t0, 5.0)) - 0.1).abs() < 1e-9, "never past what was consumed");
        shared.record(100, 100, at(t0, 0.2), Some(Duration::from_millis(40)), false);
        assert!((clock.now(at(t0, 0.25)) - 0.11).abs() < 1e-9, "output latency is subtracted");
    }

    #[test]
    fn an_underrun_freezes_the_audio_clock() {
        let shared = AudioShared::new(1000);
        let mut clock = AudioClock::new(shared.clone());
        let t0 = shared.epoch;
        clock.set_running(true, t0);
        shared.record(50, 50, at(t0, 0.05), None, false);
        let before = clock.now(at(t0, 0.2));
        shared.record(0, 50, at(t0, 0.25), None, false);
        shared.record(0, 50, at(t0, 0.3), None, false);
        assert_eq!(clock.now(at(t0, 1.0)), before, "starved output: the picture freezes too");
        assert_eq!(shared.underruns.load(Ordering::SeqCst), 2);
        shared.record(0, 50, at(t0, 0.35), None, true);
        assert_eq!(shared.underruns.load(Ordering::SeqCst), 2, "a drained stream is not an underrun");
        shared.record(10, 50, at(t0, 1.1), None, false);
        assert!((clock.now(at(t0, 1.2)) - 0.06).abs() < 1e-9, "partial chunk: only what played counts");
    }

    #[test]
    fn after_the_audio_drains_the_clock_runs_on_the_wall() {
        let shared = AudioShared::new(1000);
        let mut clock = AudioClock::new(shared.clone());
        let t0 = shared.epoch;
        shared.record(500, 500, at(t0, 0.5), None, false);
        clock.mark_ended(at(t0, 1.0));
        clock.mark_ended(at(t0, 1.5));
        assert!((clock.now(at(t0, 2.0)) - 1.5).abs() < 1e-9);
    }
}
