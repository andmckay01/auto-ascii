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
use super::{SinkChoice, Sound, SoundOptions, Soundtrack};

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
    assert!(st.poll(at(t0, 0.2)), "a cpal error callback retires the output");
    assert_eq!(st.sound(), Sound::None);
    assert!((st.now_secs(at(t0, 1.2)) - 1.1).abs() < 1e-9, "silently on the wall from where it was");

    let mut st = Soundtrack::with_track(Track::with_pcm(ramp(5000)), 5.0, opts(), fmt(1000));
    let out = st.output();
    let t0 = st.shared().epoch;
    st.set_running(true, t0);
    out.fill(&mut buf, at(t0, 0.1), None);
    assert!(!st.poll(at(t0, 1.5)));
    assert!(st.poll(at(t0, 1.7)), "no callback for longer than DEVICE_SILENT: retired, no hang");
    assert!(st.now_secs(at(t0, 2.7)) > st.now_secs(at(t0, 1.7)) + 0.99);
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
    assert_eq!([Sound::On, Sound::Off, Sound::None].map(Sound::label), ["on", "off", "none"]);
}
