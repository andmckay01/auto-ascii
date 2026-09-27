//! Soundtrack tests: the silent fallbacks, the mute toggle, the hand-off to
//! the wall clock, and one real decode of a synthetic ffmpeg sine through the
//! null sink (skipped when ffmpeg is missing). Nothing here opens an audio
//! device: every soundtrack is built on the null sink or driven by hand.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::output::{DeviceFormat, Pcm, Track};
use super::source::{TrackSource, Tools};
use super::{Reopener, SinkChoice, Sound, SoundOptions, Soundtrack, REOPEN_EVERY};

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("auto-ascii-sound-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn sine(&self, name: &str, secs: f64) -> Option<PathBuf> {
        let tools = Tools::find().ok().or_else(|| {
            eprintln!("skipping: ffmpeg/ffprobe not found");
            None
        })?;
        let out = self.0.join(name);
        let status = Command::new(tools.ffmpeg)
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:sample_rate=48000:duration={secs}"))
            .args(["-ac", "2", "-c:a", "aac"])
            .arg(&out)
            .status()
            .unwrap();
        assert!(status.success(), "ffmpeg could not write the test sine");
        Some(out)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fmt(rate: u32) -> DeviceFormat {
    DeviceFormat { rate, channels: 2, name: "test output".into() }
}

fn at(base: Instant, secs: f64) -> Instant {
    base + Duration::from_secs_f64(secs)
}

fn ramp(frames: usize) -> Pcm {
    let samples: Vec<i16> = (0..frames).flat_map(|i| [(i % 30_000) as i16 + 1, 0]).collect();
    Pcm::from_samples(1000, 2, &samples)
}

fn opts() -> SoundOptions {
    SoundOptions::default()
}

#[test]
fn no_audio_never_looks_for_a_track() {
    let d = Dir::new("no-audio");
    let asset = d.file("clip.ascii", b"");
    d.file("clip.m4a", b"not audio at all");
    let opening = Soundtrack::open(&asset, 2.0, SoundOptions { no_audio: true, ..opts() }, SinkChoice::null());
    assert!(opening.soundtrack.is_none());
    assert!(opening.notes.is_empty(), "--no-audio is not a problem to report: {:?}", opening.notes);
}

#[test]
fn no_track_plays_silently_without_a_note() {
    let d = Dir::new("none");
    let asset = d.file("clip.ascii", b"");
    d.file("other.m4a", b"x");
    let opening = Soundtrack::open(&asset, 2.0, opts(), SinkChoice::null());
    assert!(opening.soundtrack.is_none());
    assert!(opening.notes.is_empty(), "{:?}", opening.notes);
}

#[test]
fn an_unreadable_sidecar_plays_silently_and_says_why() {
    let d = Dir::new("garbage");
    let asset = d.file("clip.ascii", b"");
    d.file("clip.m4a", b"definitely not an m4a");
    if Tools::find().is_err() {
        return;
    }
    let opening = Soundtrack::open(&asset, 2.0, opts(), SinkChoice::null());
    assert!(opening.soundtrack.is_none());
    assert_eq!(opening.notes.len(), 1, "{:?}", opening.notes);
    assert!(opening.notes[0].contains("clip.m4a") && opening.notes[0].contains("played silently"), "{:?}", opening.notes);
}

#[test]
fn no_output_device_plays_silently_and_says_so() {
    let d = Dir::new("no-device");
    let asset = d.file("clip.ascii", b"");
    let Some(_) = d.sine("clip.m4a", 1.0) else { return };
    let opening = Soundtrack::open(&asset, 1.0, opts(), SinkChoice::Unavailable("no default audio output device".into()));
    assert!(opening.soundtrack.is_none());
    assert_eq!(opening.notes, ["no audio output (no default audio output device); played silently"]);
}

#[test]
fn a_different_cut_is_rejected_and_reported() {
    let d = Dir::new("mismatch");
    let asset = d.file("1.ascii", b"");
    let Some(_) = d.sine("source.mp4", 8.0) else { return };
    let opening = Soundtrack::open(&asset, 1.0, opts(), SinkChoice::null());
    assert!(opening.soundtrack.is_none(), "an 8 s track is not this 1 s clip's sound");
    assert_eq!(opening.notes.len(), 1);
    assert!(opening.notes[0].contains("source.mp4") && opening.notes[0].contains("not played"), "{:?}", opening.notes);
}

#[test]
fn a_real_track_decodes_in_the_background_and_drives_the_clock() {
    let d = Dir::new("real");
    let asset = d.file("clip.ascii", b"");
    let Some(m4a) = d.sine("clip.m4a", 1.5) else { return };
    let opening = Soundtrack::open(&asset, 1.5, opts(), SinkChoice::null());
    assert!(opening.notes.is_empty(), "{:?}", opening.notes);
    let mut st = opening.soundtrack.expect("the sidecar plays");
    assert_eq!(st.source(), m4a);
    assert_eq!(st.sound(), Sound::On, "on by default when a track exists");
    assert!(st.wait_decoded(Duration::from_secs(20)), "the decode thread finished");
    let stats = st.stats();
    assert!(stats.decoded, "{stats:?}");
    assert!((stats.decoded_secs - 1.5).abs() < 0.1, "{stats:?}");
    assert_eq!((stats.rate, stats.channels, stats.clock), (48_000, 2, "audio"));

    let t0 = Instant::now();
    st.seek(0.25, t0);
    st.set_running(true, t0);
    std::thread::sleep(Duration::from_millis(300));
    let now = Instant::now();
    let t = st.now_secs(now);
    let wall = now.duration_since(t0).as_secs_f64();
    assert!(t >= 0.25 && t <= 0.25 + wall + 0.01, "the clock is what the null sink played: {t} after {wall}");
    assert!(t > 0.25 + wall - 0.1, "and it keeps up with real time: {t} after {wall}");
    assert!(!st.poll(now), "a live output is not retired");
    assert!(st.finish().is_empty());
}

#[test]
fn m_mutes_and_unmutes_without_moving_the_clock() {
    let mut st = Soundtrack::with_track(Track::with_pcm(ramp(5000)), 5.0, opts(), fmt(1000));
    let out = st.output();
    let t0 = st.shared().epoch;
    st.set_running(true, t0);
    let mut buf = [0.0f32; 20];
    let mut unmuted_at = None;
    for i in 1..=100 {
        let t = f64::from(i) * 0.01;
        if i == 30 {
            assert_eq!(st.toggle_mute(), Sound::Off);
        }
        if i == 60 {
            assert_eq!(st.toggle_mute(), Sound::On);
            unmuted_at = Some(st.shared().cursor());
        }
        out.fill(&mut buf, at(t0, t), None);
        let silent = buf.iter().all(|&s| s == 0.0);
        assert_eq!(silent, (30..60).contains(&i), "callback {i}: muted output is zeros, the rest is sound");
        assert!((st.now_secs(at(t0, t)) - (t - 0.01)).abs() < 1e-9, "the clock advances through the mute");
    }
    let resumed = unmuted_at.unwrap();
    assert_eq!(resumed, 590, "unmute picks up at the cursor the clock is at");
}

#[test]
fn a_decode_failure_hands_the_clock_to_the_wall() {
    let d = Dir::new("decode-fail");
    let fake = d.file("ffmpeg", b"#!/bin/sh\necho 'Invalid data found when processing input' >&2\nexit 1\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let track = d.file("clip.m4a", b"x");
    let opening = Soundtrack::start(
        TrackSource::Sidecar(track),
        fake,
        3.0,
        opts(),
        super::Opened::Null,
        fmt(8000),
        24_000,
    );
    let mut st = opening.soundtrack.unwrap();
    let t0 = Instant::now();
    st.set_running(true, t0);
    assert!(st.wait_decoded(Duration::from_secs(10)));
    std::thread::sleep(Duration::from_millis(50));
    let pos = st.now_secs(Instant::now());
    assert!(st.poll(Instant::now()), "the failure retires the output");
    assert_eq!(st.sound(), Sound::None);
    assert_eq!(st.stats().clock, "wall");
    let later = st.now_secs(Instant::now() + Duration::from_millis(500));
    assert!(later >= pos + 0.45, "the wall clock carries on from the current position: {pos} -> {later}");
    assert_eq!(st.toggle_mute(), Sound::None, "m does nothing without sound");
    let notes = st.finish();
    assert!(notes.iter().any(|n| n.contains("decode failed: Invalid data found")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("could not be decoded; continued silently")), "{notes:?}");
}

#[test]
fn a_lost_or_stalled_device_hands_the_clock_to_the_wall() {
    let mut st = Soundtrack::with_track(Track::with_pcm(ramp(5000)), 5.0, opts(), fmt(1000));
    let out = st.output();
    let t0 = st.shared().epoch;
    st.set_running(true, t0);
    let mut buf = [0.0f32; 200];
    out.fill(&mut buf, at(t0, 0.1), None);
    assert!(!st.poll(at(t0, 0.15)));
    st.shared().lost.store(true, Ordering::SeqCst);
    assert!(st.poll(at(t0, 0.2)), "a cpal error callback drops the stream");
    assert_eq!(st.sound(), Sound::Wait, "waiting for a device, not gone for good");
    assert!((st.now_secs(at(t0, 1.2)) - 1.1).abs() < 1e-9, "silently on the wall from where it was");
    assert!(!st.poll(at(t0, 5.0)), "no reopener: it stays silent without a new note");
    assert_eq!(st.sound(), Sound::Wait);

    let mut st = Soundtrack::with_track(Track::with_pcm(ramp(5000)), 5.0, opts(), fmt(1000));
    let out = st.output();
    let t0 = st.shared().epoch;
    st.set_running(true, t0);
    out.fill(&mut buf, at(t0, 0.1), None);
    assert!(!st.poll(at(t0, 1.5)));
    assert!(st.poll(at(t0, 1.61)), "no callback for longer than DEVICE_SILENT: the wall takes over, no hang");
    assert_eq!((st.sound(), st.stats().clock), (Sound::Wait, "wall"));
    assert!(st.now_secs(at(t0, 2.5)) > st.now_secs(at(t0, 1.61)) + 0.85);
    out.fill(&mut buf, at(t0, 2.5), None);
    assert!(st.poll(at(t0, 2.51)), "and hands back when the output calls again");
    assert_eq!((st.sound(), st.stats().clock), (Sound::On, "audio"));
    let notes = st.finish();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("stopped calling back"), "{notes:?}");
}

#[test]
fn audio_shorter_than_the_video_keeps_time_and_resyncs_on_a_seek_back() {
    let mut st = Soundtrack::with_track(Track::with_pcm(ramp(1000)), 3.0, opts(), fmt(1000));
    let out = st.output();
    let t0 = st.shared().epoch;
    st.set_running(true, t0);
    let mut buf = [0.0f32; 20];
    for i in 1..=250 {
        out.fill(&mut buf, at(t0, f64::from(i) * 0.01), None);
    }
    assert!(buf.iter().all(|&s| s == 0.0), "past the end of the sound: silence");
    assert!((st.now_secs(at(t0, 2.5)) - 2.49).abs() < 1e-9, "the picture keeps its pace past the sound");
    assert_eq!(st.sound(), Sound::On);
    assert!(!st.poll(at(t0, 2.5)));
    assert_eq!(st.shared().underruns.load(Ordering::SeqCst), 0, "a finished track is not starving");
    st.seek(0.5, at(t0, 2.5));
    out.fill(&mut buf, at(t0, 2.51), None);
    assert_eq!((buf[0] * 32768.0).round() as i32, 501, "back inside the sound, it plays in place");
    assert!((st.now_secs(at(t0, 2.515)) - 0.505).abs() < 1e-9);
}

#[test]
fn sound_states_read_on_off_none() {
    assert_eq!(
        [Sound::On, Sound::Off, Sound::Wait, Sound::None].map(Sound::label),
        ["on", "off", "wait", "none"]
    );
}

fn tick(st: &mut Soundtrack, out: &super::output::Output, from: f64, to: f64, calls: bool, high: &mut f64) {
    let t0 = st.shared().epoch;
    let mut buf = [0.0f32; 20];
    let mut t = from;
    while t <= to + 1e-9 {
        if calls {
            out.fill(&mut buf, at(t0, t), None);
        }
        st.poll(at(t0, t));
        let now = st.now_secs(at(t0, t));
        assert!(now + 1e-9 >= *high, "the clock went backwards at {t:.2}: {high} -> {now}");
        *high = now;
        t += 0.01;
    }
}

#[test]
fn a_stalled_output_that_resumes_returns_to_the_audio_clock_in_sync() {
    let mut st = Soundtrack::with_track(Track::with_pcm(ramp(5000)), 5.0, opts(), fmt(1000));
    let out = st.output();
    let t0 = st.shared().epoch;
    st.set_running(true, t0);
    let mut high = 0.0;
    tick(&mut st, &out, 0.01, 0.10, true, &mut high);
    tick(&mut st, &out, 0.11, 1.80, false, &mut high);
    assert_eq!(st.stats().clock, "wall", "a stalled output hands the picture to the wall");
    let stalled_at = high;
    tick(&mut st, &out, 1.81, 2.00, true, &mut high);
    assert_eq!(st.stats().clock, "audio", "callbacks came back: the audio clock is master again");
    assert_ne!(st.sound(), Sound::None, "and the sound is back on");
    assert!(high > stalled_at + 0.15, "the picture kept moving through the stall and after it");
    let wall_expected = 0.10 + (2.00 - 1.61);
    assert!((st.now_secs(at(t0, 2.0)) - wall_expected).abs() < 1.0 / 30.0, "re-seeked to the wall position: {} vs {wall_expected}", st.now_secs(at(t0, 2.0)));
    let mut buf = [0.0f32; 20];
    out.fill(&mut buf, at(t0, 2.01), None);
    let heard = (buf[0] * 32768.0).round() as f64 / 1000.0;
    assert!((heard - st.now_secs(at(t0, 2.01))).abs() < 1.0 / 30.0, "the sound plays where the picture is: {heard}");
}

#[test]
fn a_frozen_process_is_not_a_stalled_device() {
    let mut st = Soundtrack::with_track(Track::with_pcm(ramp(5000)), 5.0, opts(), fmt(1000));
    let out = st.output();
    let t0 = st.shared().epoch;
    st.set_running(true, t0);
    let mut high = 0.0;
    tick(&mut st, &out, 0.01, 0.10, true, &mut high);
    assert!(!st.poll(at(t0, 3.10)), "the first poll after a 3 s freeze does not stall the output");
    assert_eq!(st.stats().clock, "audio");
    tick(&mut st, &out, 3.11, 3.50, true, &mut high);
    assert_eq!((st.stats().clock, st.sound()), ("audio", Sound::On), "never stalled across the freeze");
}

#[test]
fn m_during_an_outage_is_honoured_on_recovery() {
    let mut st = Soundtrack::with_track(Track::with_pcm(ramp(5000)), 5.0, opts(), fmt(1000));
    let out = st.output();
    let t0 = st.shared().epoch;
    st.set_running(true, t0);
    let mut high = 0.0;
    tick(&mut st, &out, 0.01, 0.10, true, &mut high);
    tick(&mut st, &out, 0.11, 1.80, false, &mut high);
    st.toggle_mute();
    tick(&mut st, &out, 1.81, 2.00, true, &mut high);
    assert_eq!(st.sound(), Sound::Off, "muted during the outage, muted after it");
    let mut buf = [1.0f32; 20];
    out.fill(&mut buf, at(t0, 2.01), None);
    assert!(buf.iter().all(|&s| s == 0.0));
    st.toggle_mute();
    assert_eq!(st.sound(), Sound::On);
}

#[test]
fn a_lost_device_is_reopened_after_the_retry_interval_and_plays_in_sync() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut st = Soundtrack::with_track(Track::with_pcm(ramp(8000)), 8.0, opts(), fmt(1000));
    let first = st.output();
    let t0 = st.shared().epoch;
    st.set_running(true, t0);
    let handed: Rc<RefCell<Vec<(u32, usize, super::output::Output)>>> = Rc::default();
    let sink = handed.clone();
    st.set_reopen(Reopener::new(move |want, out| {
        sink.borrow_mut().push((want.rate, want.channels, out));
        Ok(super::output::Sink::detached())
    }));
    let mut high = 0.0;
    tick(&mut st, &first, 0.01, 0.30, true, &mut high);
    st.shared().lost.store(true, Ordering::SeqCst);
    assert!(st.poll(at(t0, 0.31)));
    let retry = 0.31 + REOPEN_EVERY.as_secs_f64();
    tick(&mut st, &first, 0.32, retry - 0.02, false, &mut high);
    assert!(handed.borrow().is_empty(), "no retry before the interval");
    assert_eq!(st.sound(), Sound::Wait);
    st.poll(at(t0, retry + 0.001));
    assert_eq!(handed.borrow().len(), 1, "re-opened once the interval passed");
    let (rate, channels, reopened) = handed.borrow_mut().pop().unwrap();
    assert_eq!((rate, channels), (1000, 2), "at the track's own rate and layout");
    assert_eq!(st.sound(), Sound::Wait, "waiting for the new stream's first callback");
    tick(&mut st, &reopened, retry + 0.01, retry + 0.30, true, &mut high);
    assert_eq!((st.sound(), st.stats().clock), (Sound::On, "audio"));
    let expected = 0.30 + (retry + 0.30 - 0.31);
    let now = st.now_secs(at(t0, retry + 0.30));
    assert!((now - expected).abs() < 1.0 / 30.0, "in sync after the reopen: {now} vs {expected}");
    let mut buf = [0.0f32; 20];
    reopened.fill(&mut buf, at(t0, retry + 0.31), None);
    let heard = (buf[0] * 32768.0).round() as f64 / 1000.0;
    assert!((heard - now).abs() < 1.0 / 30.0, "the new stream plays where the picture is: {heard} vs {now}");
}

#[test]
fn a_failing_reopen_keeps_the_session_silent_and_retrying_with_one_note() {
    use std::cell::Cell;
    use std::rc::Rc;
    let mut st = Soundtrack::with_track(Track::with_pcm(ramp(5000)), 30.0, opts(), fmt(1000));
    let out = st.output();
    let t0 = st.shared().epoch;
    st.set_running(true, t0);
    let tries = Rc::new(Cell::new(0));
    let counter = tries.clone();
    st.set_reopen(Reopener::new(move |_, _| {
        counter.set(counter.get() + 1);
        Err("no default audio output device".into())
    }));
    let mut high = 0.0;
    tick(&mut st, &out, 0.01, 0.10, true, &mut high);
    st.shared().lost.store(true, Ordering::SeqCst);
    tick(&mut st, &out, 0.11, 10.0, false, &mut high);
    assert!((4..=5).contains(&tries.get()), "retried every {REOPEN_EVERY:?}: {}", tries.get());
    assert_eq!(st.sound(), Sound::Wait);
    assert!(high > 9.5, "the picture kept going on the wall: {high}");
    let notes = st.finish();
    assert_eq!(notes.len(), 2, "one note for the loss, one for the failing retries: {notes:?}");
    assert!(notes[1].contains("no default audio output device") && notes[1].contains("still retrying"), "{notes:?}");
}
