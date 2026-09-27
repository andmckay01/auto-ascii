//! `auto-ascii stream`: resolve the first video behind a link with yt-dlp,
//! pipe its video and audio through two ffmpeg children (nothing written to
//! disk), render the picture through the player's pipeline slaved to the
//! audio clock, and show a real-progress loader while buffering.

pub mod audio;
pub mod clock;
pub mod decode;
pub mod loader;
pub mod procs;
pub mod video;
pub mod ytdlp;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use auto_ascii::pipeline::{self, LiveSpec, drain_backend_events};
use auto_ascii::{Cell, Codec, ColorTier, Grid, PaletteChoice};
use auto_ascii_core::GlyphTier;
use auto_ascii_term::{AnsiBackend, Backend, ProbeOptions, SimBackend, probe_caps};

use clock::{AudioClock, AudioShared, Clock, MonotonicClock, Pick, pick_frame};
use decode::{DecodeCtx, Msg};
use loader::{LoadState, LoaderStyle, Progress, draw_loader, grid_text, percent};
use procs::{Procs, ProcsGuard, ScratchDir};
use video::{Fps, Frame};
use ytdlp::{Event, Input, Media, YtDlp};

use crate::BoxErr;

const START_BUFFER_SECS: f64 = 1.5;
const VIDEO_QUEUE_SECS: f64 = 2.5;
const AUDIO_RING_SECS: f64 = 4.0;
const STALL_SECS: f64 = 0.35;
const DEVICE_SILENT: Duration = Duration::from_millis(1500);
const LOADER_FRAME: Duration = Duration::from_millis(33);
const SIM_RATE: u32 = 48_000;
const SIM_CHANNELS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimSpec {
    pub cols: u16,
    pub rows: u16,
    pub secs: f64,
}

pub fn parse_sim(spec: &str) -> Result<SimSpec, String> {
    let (size, secs) = spec
        .split_once(':')
        .ok_or_else(|| format!("--sim {spec:?}: expected COLSxROWS:SECONDS (e.g. 120x40:25)"))?;
    let (c, r) = size
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("--sim {spec:?}: expected COLSxROWS before the colon"))?;
    let cols: u16 = c.trim().parse().map_err(|_| format!("--sim {spec:?}: bad COLS"))?;
    let rows: u16 = r.trim().parse().map_err(|_| format!("--sim {spec:?}: bad ROWS"))?;
    let secs: f64 = secs.trim().parse().map_err(|_| format!("--sim {spec:?}: bad SECONDS"))?;
    if cols == 0 || rows == 0 || !secs.is_finite() || secs <= 0.0 {
        return Err(format!("--sim {spec:?}: size and seconds must be positive"));
    }
    Ok(SimSpec { cols, rows, secs })
}

pub struct StreamArgs {
    pub input: String,
    pub codec: Codec,
    pub palette: PaletteChoice,
    pub max_height: u32,
    pub no_audio: bool,
    pub sim: Option<SimSpec>,
    pub sim_dump: Option<PathBuf>,
}

static SIGNALLED: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn on_signal(sig: libc::c_int) {
    if SIGNALLED.swap(sig, Ordering::SeqCst) != 0 {
        procs::kill_registered_groups();
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
}

fn install_signal_handlers() {
    #[cfg(unix)]
    unsafe {
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reason {
    Eof,
    Quit,
    Signal,
    TimeLimit,
    Error,
}

impl Reason {
    fn name(self) -> &'static str {
        match self {
            Reason::Eof => "eof",
            Reason::Quit => "quit",
            Reason::Signal => "signal",
            Reason::TimeLimit => "time_limit",
            Reason::Error => "error",
        }
    }
}

#[derive(Default)]
struct Stats {
    entry_url: Option<String>,
    media: Option<Media>,
    plane: Option<(u16, u16)>,
    fps: Option<Fps>,
    frames_rendered: u64,
    frames_dropped: u64,
    frames_decoded: u64,
    max_drift: f64,
    stages: Vec<(&'static str, u8)>,
    rebuffers: u32,
    notes: Vec<String>,
    audio_rate: Option<u32>,
    audio_device: Option<String>,
    started_at: Option<f64>,
    loader_dump: Option<(u8, String)>,
    picture_dump: Option<(u32, f64, String)>,
    samples_consumed: u64,
    underruns: u64,
}

struct Shared {
    procs: Procs,
    stop: Arc<AtomicBool>,
    cwd: PathBuf,
    ffmpeg: PathBuf,
}

enum AudioPlan {
    Off(Option<String>),
    Sim,
    #[cfg(feature = "audio")]
    Device(Box<audio::device::Opened>, audio::DeviceFormat),
}

fn open_audio(no_audio: bool, sim: bool) -> AudioPlan {
    if no_audio {
        return AudioPlan::Off(None);
    }
    if sim {
        return AudioPlan::Sim;
    }
    #[cfg(feature = "audio")]
    {
        match audio::device::open() {
            Ok((opened, format)) => AudioPlan::Device(Box::new(opened), format),
            Err(e) => AudioPlan::Off(Some(format!("audio: {e}; playing without sound"))),
        }
    }
    #[cfg(not(feature = "audio"))]
    {
        AudioPlan::Off(Some("audio: built without the `audio` feature; playing without sound".into()))
    }
}

struct Audio {
    ring: Arc<audio::Ring>,
    shared: Arc<AudioShared>,
    rate: u32,
    src_channels: usize,
    _sink: Option<audio::Sink>,
    thread: Option<JoinHandle<()>>,
}

struct Pipes {
    frames: Receiver<Frame>,
    recycle: Sender<Frame>,
    queued: Arc<AtomicUsize>,
    video_thread: Option<JoinHandle<()>>,
    target_frames: usize,
}

fn resolve_cell_aspect(cell_px: Option<(u16, u16)>) -> f64 {
    match cell_px {
        Some((w, h)) if w > 0 && h > 0 => f64::from(h) / f64::from(w),
        _ => auto_ascii_core::DEFAULT_CELL_ASPECT,
    }
}

fn next_codec(codec: Codec, presses: u32) -> Codec {
    let at = Codec::ALL.iter().position(|c| *c == codec).unwrap_or(0);
    Codec::ALL[(at + presses as usize) % Codec::ALL.len()]
}

pub fn run(args: &StreamArgs) -> Result<(), BoxErr> {
    SIGNALLED.store(0, Ordering::SeqCst);
    let scratch = ScratchDir::new().map_err(|e| format!("creating a private temp dir: {e}"))?;
    let scratch_path = scratch.path().to_path_buf();
    let procs = Procs::new();
    let guard = ProcsGuard(procs.clone());
    let shared = Shared {
        procs: procs.clone(),
        stop: Arc::new(AtomicBool::new(false)),
        cwd: scratch_path.clone(),
        ffmpeg: PathBuf::from("ffmpeg"),
    };
    let t0 = Instant::now();
    let plan = open_audio(args.no_audio, args.sim.is_some());

    let (reason, stats, error, scratch_entries) = match args.sim {
        Some(sim) => {
            install_signal_handlers();
            let mut backend = SimBackend::new(sim.cols, sim.rows);
            let mut caps = backend.caps().clone();
            caps.color = ColorTier::True;
            backend.set_caps(caps);
            backend.resize(sim.cols, sim.rows);
            let (reason, stats, error) = session(&mut backend, args, &shared, plan, t0, Some(sim.secs));
            (reason, stats, error, scratch.entries())
        }
        None => {
            let caps = probe_caps(&ProbeOptions::default());
            let mut backend = AnsiBackend::new(caps).map_err(|e| format!("terminal: {e}"))?;
            install_signal_handlers();
            let (reason, stats, error) = session(&mut backend, args, &shared, plan, t0, None);
            let entries = scratch.entries();
            shared.stop.store(true, Ordering::SeqCst);
            procs.shutdown();
            backend.shutdown();
            (reason, stats, error, entries)
        }
    };
    shared.stop.store(true, Ordering::SeqCst);
    drop(guard);
    let alive = procs.alive();
    drop(scratch);
    let removed = !scratch_path.exists();

    if args.sim.is_some() {
        let line = sim_json(&stats, reason, error.as_deref(), alive, procs.spawned(), removed, scratch_entries, t0);
        crate::emit(&format!("{line}\n"));
        if let Some(path) = &args.sim_dump {
            write_dump(path, &stats)?;
        }
    } else {
        for note in &stats.notes {
            eprintln!("auto-ascii: {note}");
        }
    }
    let signal = SIGNALLED.load(Ordering::SeqCst);
    match (error, reason) {
        (Some(e), _) => Err(e.into()),
        (None, Reason::Signal) => Err(format!("stopped by signal {signal}").into()),
        _ => Ok(()),
    }
}

fn spawn_resolver(args: &StreamArgs, shared: &Shared, tx: Sender<Msg>) -> JoinHandle<()> {
    let input = Input::classify(&args.input);
    let max_height = args.max_height;
    let ytdlp = YtDlp::new(YtDlp::program_from_env(), shared.procs.clone(), &shared.cwd);
    let stop = shared.stop.clone();
    std::thread::spawn(move || {
        let result = ytdlp.resolve(&input, max_height, &mut |e| {
            let _ = tx.send(match e {
                Event::Started => Msg::YtdlpStarted,
                Event::EntryFound(url) => Msg::EntryFound(url),
            });
        });
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let _ = tx.send(match result {
            Ok(media) => Msg::Media(Box::new(media)),
            Err(e) => Msg::Failed(e),
        });
    })
}

fn start_audio(
    plan: AudioPlan,
    media: &Media,
    shared: &Shared,
    tx: &Sender<Msg>,
    stats: &mut Stats,
) -> Option<Audio> {
    if media.audio.is_empty() {
        if !matches!(plan, AudioPlan::Off(_)) {
            stats.notes.push("audio: this video has no audio track; playing without sound".into());
        }
        return None;
    }
    let (rate, channels) = match &plan {
        AudioPlan::Off(note) => {
            if let Some(n) = note {
                stats.notes.push(n.clone());
            }
            return None;
        }
        AudioPlan::Sim => (SIM_RATE, SIM_CHANNELS),
        #[cfg(feature = "audio")]
        AudioPlan::Device(_, format) => (format.rate, format.channels.max(1)),
    };
    let src_channels = channels.min(2);
    let ring = audio::Ring::new((AUDIO_RING_SECS * f64::from(rate)) as usize * src_channels);
    let shared_clock = AudioShared::new(rate);
    let output = audio::Output {
        ring: ring.clone(),
        shared: shared_clock.clone(),
        channels,
        src_channels,
    };
    let sink = match plan {
        AudioPlan::Off(_) => return None,
        AudioPlan::Sim => {
            stats.audio_device = Some("null (sim, real-time paced)".into());
            audio::Sink::null(audio::NullSink::start(output))
        }
        #[cfg(feature = "audio")]
        AudioPlan::Device(opened, format) => match audio::device::start(*opened, output) {
            Ok(s) => {
                stats.audio_device = Some(format.name.clone());
                audio::Sink::device(s)
            }
            Err(e) => {
                stats.notes.push(format!("audio: {e}; playing without sound"));
                return None;
            }
        },
    };
    stats.audio_rate = Some(rate);
    let thread = audio::spawn(audio::AudioJob {
        ctx: DecodeCtx {
            procs: shared.procs.clone(),
            cwd: shared.cwd.clone(),
            ffmpeg: shared.ffmpeg.clone(),
            stop: shared.stop.clone(),
        },
        tracks: media.audio.clone(),
        rate,
        channels: src_channels,
        ring: ring.clone(),
        tx: tx.clone(),
    });
    Some(Audio {
        ring,
        shared: shared_clock,
        rate,
        src_channels,
        _sink: Some(sink),
        thread: Some(thread),
    })
}

fn start_video(media: &Media, shared: &Shared, tx: &Sender<Msg>, stats: &mut Stats) -> Result<(Pipes, LiveSpec), String> {
    let params = auto_ascii_factory::effective_params(None, None, None).map_err(|e| e.to_string())?;
    let (w, h, an, ad) = video::plane_dims(media.width, media.height, params.build.base_w, params.build.base_h);
    let fps = video::stream_fps(media.fps, params.build.fps);
    let capacity = ((VIDEO_QUEUE_SECS * fps.value()).ceil() as usize).max(4);
    let start_secs = media.duration.map_or(START_BUFFER_SECS, |d| d.min(START_BUFFER_SECS));
    let target_frames = ((start_secs * fps.value()).ceil() as usize).clamp(1, capacity - 1);
    let (frame_tx, frame_rx) = mpsc::sync_channel(capacity);
    let (recycle_tx, recycle_rx) = mpsc::channel();
    let queued = Arc::new(AtomicUsize::new(0));
    let thread = video::spawn(video::VideoJob {
        ctx: DecodeCtx {
            procs: shared.procs.clone(),
            cwd: shared.cwd.clone(),
            ffmpeg: shared.ffmpeg.clone(),
            stop: shared.stop.clone(),
        },
        tracks: media.video.clone(),
        w,
        h,
        fps,
        params,
        tx: tx.clone(),
        frames: frame_tx,
        recycle: recycle_rx,
        queued: queued.clone(),
    });
    stats.plane = Some((w, h));
    stats.fps = Some(fps);
    let frame_count = media.duration.map_or(u32::MAX, |d| (d * fps.value()).ceil().min(f64::from(u32::MAX)) as u32);
    let spec = LiveSpec { w, h, aspect_num: an, aspect_den: ad, fps: fps.value(), frame_count };
    Ok((
        Pipes { frames: frame_rx, recycle: recycle_tx, queued, video_thread: Some(thread), target_frames },
        spec,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Loading,
    Playing,
    Rebuffering,
}

struct Loop {
    stats: Stats,
    load: LoadState,
    progress: Progress,
    phase: Phase,
    codec: Codec,
    glyph_tier: GlyphTier,
    color: ColorTier,
    loader: Grid<Cell>,
    showing_loader: bool,
    pending: VecDeque<Frame>,
    video_eof: bool,
    audio_eof: bool,
    stall_since: Option<Instant>,
    last_pts: Option<f64>,
    device_lost: bool,
}

impl Loop {
    fn stage(&mut self, name: &'static str) {
        if !self.stats.stages.iter().any(|(n, _)| *n == name) {
            let pct = self.progress.update(percent(&self.load));
            self.stats.stages.push((name, pct));
        }
    }

    fn draw_loader<B: Backend>(&mut self, backend: &mut B, pct: u8, t0: Instant) {
        let (cols, rows) = backend.caps().cells;
        if self.loader.cols() != cols || self.loader.rows() != rows {
            self.loader.resize(cols, rows);
        }
        let style = LoaderStyle::for_codec(self.codec, self.glyph_tier, self.color);
        let tick = (t0.elapsed().as_millis() / LOADER_FRAME.as_millis()) as u32;
        let title = self.stats.media.as_ref().map(|m| m.title.as_str());
        draw_loader(&mut self.loader, pct, Some(tick), &style, title);
        if !self.showing_loader {
            backend.invalidate();
            self.showing_loader = true;
        }
        backend.present(&self.loader);
        if self.stats.loader_dump.as_ref().is_none_or(|(p, _)| *p < 40 && pct >= *p) {
            self.stats.loader_dump = Some((pct, grid_text(&self.loader)));
        }
    }

    fn recycle(&self, pipes: &Pipes, frame: Frame) {
        let _ = pipes.recycle.send(frame);
    }
}

fn session<B: Backend>(
    backend: &mut B,
    args: &StreamArgs,
    shared: &Shared,
    plan: AudioPlan,
    t0: Instant,
    limit: Option<f64>,
) -> (Reason, Stats, Option<String>) {
    let (tx, rx) = mpsc::channel::<Msg>();
    let resolver = spawn_resolver(args, shared, tx.clone());
    let color = backend.caps().color;
    let glyph_tier = args.palette.resolve_for_caps(backend.caps());
    let depth = pipeline::color_depth(color);
    let cell_aspect = resolve_cell_aspect(backend.caps().cell_px);
    let mut lp = Loop {
        stats: Stats::default(),
        load: LoadState::default(),
        progress: Progress::default(),
        phase: Phase::Loading,
        codec: args.codec,
        glyph_tier,
        color,
        loader: Grid::new(0, 0),
        showing_loader: false,
        pending: VecDeque::new(),
        video_eof: false,
        audio_eof: false,
        stall_since: None,
        last_pts: None,
        device_lost: false,
    };
    let mut plan = Some(plan);
    let mut player: Option<pipeline::Player<'static>> = None;
    let mut pipes: Option<Pipes> = None;
    let mut audio: Option<Audio> = None;
    let mut clock: Option<Box<dyn Clock>> = None;
    let mut error: Option<String> = None;
    let mut dump_at: Option<f64> = None;

    let reason = 'run: loop {
        if SIGNALLED.load(Ordering::SeqCst) != 0 {
            break Reason::Signal;
        }
        if limit.is_some_and(|l| t0.elapsed().as_secs_f64() >= l) {
            break Reason::TimeLimit;
        }
        let drained = match player.as_mut() {
            Some(p) => p.drain_events(backend),
            None => {
                let (d, resize) = drain_backend_events(backend);
                if let Some((c, r)) = resize {
                    backend.resize(c, r);
                }
                d
            }
        };
        if drained.quit {
            break Reason::Quit;
        }
        if drained.codec_cycle > 0 {
            lp.codec = next_codec(lp.codec, drained.codec_cycle);
            if let Some(p) = player.as_mut() {
                p.set_codec(lp.codec);
            }
            lp.showing_loader = false;
        }

        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::YtdlpStarted => {
                    lp.load.ytdlp_started = true;
                    lp.stage("ytdlp_started");
                }
                Msg::EntryFound(url) => {
                    lp.stats.entry_url = Some(url);
                    lp.load.entry_found = true;
                    lp.stage("entry_found");
                }
                Msg::Media(media) => {
                    lp.load.media_ready = true;
                    lp.stage("media_ready");
                    let a = start_audio(plan.take().unwrap_or(AudioPlan::Off(None)), &media, shared, &tx, &mut lp.stats);
                    let (p, spec) = match start_video(&media, shared, &tx, &mut lp.stats) {
                        Ok(v) => v,
                        Err(e) => {
                            error = Some(e);
                            break 'run Reason::Error;
                        }
                    };
                    let mut live = match pipeline::Player::live(&spec, cell_aspect, true, depth, glyph_tier) {
                        Ok(l) => l,
                        Err(e) => {
                            error = Some(e.to_string());
                            break 'run Reason::Error;
                        }
                    };
                    live.set_codec(lp.codec);
                    let (cols, rows) = backend.caps().cells;
                    live.reflow(backend, cols, rows);
                    player = Some(live);
                    clock = Some(match &a {
                        Some(a) => Box::new(AudioClock::new(a.shared.clone())),
                        None => {
                            lp.load.first_audio = true;
                            lp.load.audio_fill = 1.0;
                            lp.audio_eof = true;
                            Box::new(MonotonicClock::new())
                        }
                    });
                    audio = a;
                    pipes = Some(p);
                    lp.stats.media = Some(*media);
                }
                Msg::DecoderSpawned => {
                    lp.load.decoders_spawned = true;
                    lp.stage("decoders_spawned");
                }
                Msg::FirstAudio => {
                    lp.load.first_audio = true;
                    lp.stage("first_audio");
                }
                Msg::FirstVideo => {
                    lp.load.first_video = true;
                    lp.stage("first_video");
                }
                Msg::AudioEof => lp.audio_eof = true,
                Msg::VideoEof => lp.video_eof = true,
                Msg::Fallback(note) => lp.stats.notes.push(note),
                Msg::Failed(e) => {
                    error = Some(e);
                    break 'run Reason::Error;
                }
            }
        }

        let now_i = Instant::now();
        let (video_fill, audio_fill) = match &pipes {
            Some(p) => {
                let have = p.queued.load(Ordering::SeqCst) + lp.pending.len();
                let v = if lp.video_eof { 1.0 } else { have as f64 / p.target_frames as f64 };
                let a = match &audio {
                    Some(a) if !lp.audio_eof && !a.ring.eof() => {
                        let target = START_BUFFER_SECS * f64::from(a.rate) * a.src_channels as f64;
                        a.ring.len() as f64 / target
                    }
                    _ => 1.0,
                };
                (v, a)
            }
            None => (0.0, 0.0),
        };

        match lp.phase {
            Phase::Loading => {
                lp.load.video_fill = video_fill;
                lp.load.audio_fill = audio_fill;
                let pct = lp.progress.update(percent(&lp.load));
                if pct >= 100 {
                    lp.stage("buffered");
                    lp.phase = Phase::Playing;
                    lp.stats.started_at = Some(t0.elapsed().as_secs_f64());
                    if let Some(c) = clock.as_mut() {
                        c.set_running(true, now_i);
                    }
                    lp.showing_loader = false;
                    continue;
                }
                lp.draw_loader(backend, pct, t0);
                std::thread::sleep(Duration::from_millis(15));
            }
            Phase::Rebuffering => {
                let pct = percent(&LoadState::rebuffering(audio_fill, video_fill));
                if pct >= 100 {
                    lp.phase = Phase::Playing;
                    lp.stall_since = None;
                    if let Some(c) = clock.as_mut() {
                        c.set_running(true, now_i);
                    }
                    lp.showing_loader = false;
                    continue;
                }
                lp.draw_loader(backend, pct, t0);
                std::thread::sleep(Duration::from_millis(15));
            }
            Phase::Playing => {
                let (Some(p), Some(c), Some(pl)) = (pipes.as_ref(), clock.as_mut(), player.as_mut()) else {
                    continue;
                };
                let now = c.now(now_i);
                while lp.pending.back().is_none_or(|f| f.pts <= now) {
                    match p.frames.try_recv() {
                        Ok(f) => {
                            p.queued.fetch_sub(1, Ordering::SeqCst);
                            lp.stats.frames_decoded += 1;
                            lp.pending.push_back(f);
                        }
                        Err(_) => break,
                    }
                }
                let pts: Vec<f64> = lp.pending.iter().map(|f| f.pts).collect();
                let mut wait_for = Duration::from_millis(10);
                match pick_frame(&pts, now) {
                    Pick::Show { index, dropped } => {
                        for _ in 0..dropped {
                            if let Some(f) = lp.pending.pop_front() {
                                lp.recycle(p, f);
                            }
                        }
                        lp.stats.frames_dropped += dropped as u64;
                        let frame = lp.pending.pop_front().expect("pick_frame indexes pending");
                        debug_assert_eq!(index, dropped);
                        if let Err(e) = pl.load_live(frame.idx, &frame.live()) {
                            error = Some(e.to_string());
                            break 'run Reason::Error;
                        }
                        if lp.showing_loader {
                            backend.invalidate();
                            lp.showing_loader = false;
                        }
                        if let Err(e) = pl.render_present(backend, frame.idx) {
                            error = Some(e.to_string());
                            break 'run Reason::Error;
                        }
                        let drift = (c.now(Instant::now()) - frame.pts).abs();
                        lp.stats.max_drift = lp.stats.max_drift.max(drift);
                        lp.stats.frames_rendered += 1;
                        lp.last_pts = Some(frame.pts);
                        lp.stall_since = None;
                        let want_dump = dump_at.get_or_insert_with(|| {
                            lp.stats.media.as_ref().and_then(|m| m.duration).map_or(5.0, |d| (d / 2.0).min(5.0))
                        });
                        if lp.stats.picture_dump.is_none() && frame.pts >= *want_dump {
                            lp.stats.picture_dump = Some((frame.idx, frame.pts, grid_text(pl.grid())));
                        }
                        lp.recycle(p, frame);
                        if let Some(next) = lp.pending.front() {
                            wait_for = Duration::from_secs_f64((next.pts - now).clamp(0.001, 0.010));
                        }
                    }
                    Pick::Wait => {
                        if let Some(next) = lp.pending.front() {
                            wait_for = Duration::from_secs_f64((next.pts - now).clamp(0.001, 0.010));
                        }
                    }
                    Pick::Empty => {}
                }
                let video_done = lp.video_eof && lp.pending.is_empty() && p.queued.load(Ordering::SeqCst) == 0;
                if let Some(a) = &audio
                    && !lp.device_lost
                    && device_silent(&a.shared, now_i)
                {
                    lp.device_lost = true;
                    lp.stats.notes.push("audio: the output device stopped playing; continuing without sound".into());
                    c.mark_ended(now_i);
                }
                let audio_done = match &audio {
                    Some(a) => lp.device_lost || ((lp.audio_eof || a.ring.eof()) && a.ring.len() == 0),
                    None => true,
                };
                if video_done && audio_done {
                    if let Some(a) = &audio {
                        let latency = a.shared.latency_ns.load(Ordering::SeqCst);
                        std::thread::sleep(Duration::from_nanos(latency).min(Duration::from_millis(500)));
                    }
                    break Reason::Eof;
                }
                let audio_starved = audio.as_ref().is_some_and(|a| {
                    !audio_done && !lp.audio_eof && !a.ring.eof() && a.ring.len() < a.src_channels * 64
                });
                let video_starved = !lp.video_eof
                    && lp.pending.is_empty()
                    && p.queued.load(Ordering::SeqCst) == 0
                    && lp.last_pts.is_none_or(|l| now - l > STALL_SECS);
                if audio_starved || video_starved {
                    let since = *lp.stall_since.get_or_insert(now_i);
                    if now_i.duration_since(since).as_secs_f64() >= STALL_SECS {
                        lp.phase = Phase::Rebuffering;
                        lp.stats.rebuffers += 1;
                        c.set_running(false, now_i);
                        continue;
                    }
                } else {
                    lp.stall_since = None;
                }
                if audio_done && audio.is_some() {
                    c.mark_ended(now_i);
                }
                std::thread::sleep(wait_for);
            }
        }
    };

    if let Some(a) = &audio {
        lp.stats.samples_consumed = a.shared.consumed();
        lp.stats.underruns = a.shared.underruns.load(Ordering::SeqCst);
        a.shared.playing.store(false, Ordering::SeqCst);
    }
    shared.stop.store(true, Ordering::SeqCst);
    if let Some(a) = &audio {
        a.ring.close();
    }
    let mut stats = lp.stats;
    drop(lp.pending);
    if let Some(mut p) = pipes.take() {
        let thread = p.video_thread.take();
        drop(p);
        shared.procs.shutdown();
        if let Some(t) = thread {
            let _ = t.join();
        }
    }
    shared.procs.shutdown();
    if let Some(t) = audio.take().and_then(|mut a| a.thread.take()) {
        let _ = t.join();
    }
    drop(rx);
    let _ = resolver.join();
    stats.frames_decoded = stats.frames_decoded.max(stats.frames_rendered + stats.frames_dropped);
    (reason, stats, error)
}

fn device_silent(shared: &AudioShared, at: Instant) -> bool {
    let last = shared.epoch + Duration::from_nanos(shared.last_callback_ns.load(Ordering::SeqCst));
    at.saturating_duration_since(last) > DEVICE_SILENT
}

fn json_f(v: f64) -> serde_json::Value {
    serde_json::Value::from((v * 1000.0).round() / 1000.0)
}

#[allow(clippy::too_many_arguments)]
fn sim_json(
    stats: &Stats,
    reason: Reason,
    error: Option<&str>,
    alive: usize,
    spawned: usize,
    removed: bool,
    scratch_entries: usize,
    t0: Instant,
) -> String {
    let media = stats.media.as_ref();
    let stages: Vec<serde_json::Value> = stats
        .stages
        .iter()
        .map(|(n, p)| serde_json::json!({ "stage": n, "percent": p }))
        .collect();
    let obj = serde_json::json!({
        "id": media.map(|m| m.id.clone()),
        "title": media.map(|m| m.title.clone()),
        "entry_url": stats.entry_url,
        "duration_secs": media.and_then(|m| m.duration),
        "fps": stats.fps.map(|f| json_f(f.value())),
        "source": media.map(|m| format!("{}x{}", m.width.unwrap_or(0), m.height.unwrap_or(0))),
        "formats": media.map(|m| serde_json::json!({
            "video": m.video.first().map(|t| t.format_id.clone()),
            "audio": m.audio.first().map(|t| t.format_id.clone()),
        })),
        "plane": stats.plane.map(|(w, h)| format!("{w}x{h}")),
        "frames_decoded": stats.frames_decoded,
        "frames_rendered": stats.frames_rendered,
        "frames_dropped": stats.frames_dropped,
        "max_drift_ms": json_f(stats.max_drift * 1000.0),
        "audio_rate": stats.audio_rate,
        "audio_sink": stats.audio_device,
        "audio_samples_consumed": stats.samples_consumed,
        "audio_underrun_callbacks": stats.underruns,
        "loader": stages,
        "playback_started_secs": stats.started_at.map(json_f),
        "rebuffers": stats.rebuffers,
        "notes": stats.notes,
        "children_spawned": spawned,
        "children_alive": alive,
        "temp_files_written": scratch_entries,
        "temp_dir_removed": removed,
        "exit_reason": reason.name(),
        "error": error,
        "wall_secs": json_f(t0.elapsed().as_secs_f64()),
    });
    obj.to_string()
}

fn write_dump(path: &Path, stats: &Stats) -> Result<(), BoxErr> {
    let mut text = String::new();
    match &stats.loader_dump {
        Some((pct, grid)) => text.push_str(&format!("== loader ({pct}%) ==\n{grid}")),
        None => text.push_str("== loader (not shown) ==\n"),
    }
    match &stats.picture_dump {
        Some((idx, pts, grid)) => text.push_str(&format!("== picture (frame {idx}, {pts:.2}s) ==\n{grid}")),
        None => text.push_str("== picture (none rendered) ==\n"),
    }
    std::fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_specs_parse() {
        assert_eq!(parse_sim("120x40:25"), Ok(SimSpec { cols: 120, rows: 40, secs: 25.0 }));
        assert_eq!(parse_sim("80X24:2.5"), Ok(SimSpec { cols: 80, rows: 24, secs: 2.5 }));
        for bad in ["120x40", "120:25", "0x40:5", "120x40:0", "120x40:-1", "axb:1", "120x40:nan"] {
            assert!(parse_sim(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_device_that_stops_calling_back_is_noticed() {
        let shared = AudioShared::new(48_000);
        let t0 = shared.epoch;
        assert!(!device_silent(&shared, t0 + Duration::from_millis(1000)));
        assert!(device_silent(&shared, t0 + Duration::from_millis(1600)));
        shared.record(480, 480, t0 + Duration::from_millis(1550), None, false);
        assert!(!device_silent(&shared, t0 + Duration::from_millis(1600)), "a fresh callback resets it");
    }

    #[test]
    fn slash_cycles_every_codec() {
        assert_eq!(next_codec(Codec::Ascii, Codec::ALL.len() as u32), Codec::Ascii);
        let first = Codec::ALL[0];
        assert_eq!(next_codec(first, 1), Codec::ALL[1]);
    }
}
