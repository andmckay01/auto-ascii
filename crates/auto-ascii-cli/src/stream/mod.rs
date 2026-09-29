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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use auto_ascii::pipeline::{self, LiveSpec, drain_backend_events};
use auto_ascii::{Cell, ColorTier, Grid, PaletteChoice, Style};
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
const VIDEO_BEHIND_SECS: f64 = 2.0;
const TAIL_WAIT_MAX: Duration = Duration::from_secs(2);
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

pub struct Programs {
    pub ffmpeg: PathBuf,
    pub ffmpeg_ca_file: Option<PathBuf>,
    pub ytdlp: PathBuf,
    pub ytdlp_update: Option<crate::deps::Update>,
}

pub struct StreamArgs {
    pub input: String,
    pub style: Style,
    pub palette: PaletteChoice,
    pub max_height: u32,
    pub no_audio: bool,
    pub sim: Option<SimSpec>,
    pub sim_dump: Option<PathBuf>,
    pub cookies_from_browser: Option<String>,
}

pub trait Discard {
    fn discard_output(&mut self) {}
}

impl Discard for AnsiBackend {}

impl Discard for SimBackend {
    fn discard_output(&mut self) {
        drop(self.take_output());
    }
}

static SIGNALLED: AtomicI32 = AtomicI32::new(0);
static SIGNALS: AtomicU32 = AtomicU32::new(0);
static PREV_HANDLER: AtomicUsize = AtomicUsize::new(0);
static SCRATCH_PATH: AtomicPtr<std::ffi::c_char> = AtomicPtr::new(std::ptr::null_mut());

const EMERGENCY_AFTER: u32 = 3;

#[cfg(unix)]
extern "C" fn on_signal(sig: libc::c_int) {
    let _ = SIGNALLED.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
    if SIGNALS.fetch_add(1, Ordering::SeqCst) + 1 >= EMERGENCY_AFTER {
        emergency_exit(sig);
    }
}

#[cfg(unix)]
fn emergency_exit(sig: libc::c_int) {
    procs::kill_registered_groups();
    let scratch = SCRATCH_PATH.load(Ordering::SeqCst);
    unsafe {
        if !scratch.is_null() {
            libc::rmdir(scratch);
        }
        let prev = PREV_HANDLER.load(Ordering::SeqCst);
        if prev != libc::SIG_DFL && prev != libc::SIG_IGN && prev != libc::SIG_ERR {
            let restore: extern "C" fn(libc::c_int) = std::mem::transmute(prev);
            restore(sig);
        }
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

pub fn default_signals() {
    #[cfg(unix)]
    unsafe {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::signal(sig, libc::SIG_DFL);
        }
    }
}

fn install_signal_handlers(scratch: &Path) {
    SIGNALLED.store(0, Ordering::SeqCst);
    SIGNALS.store(0, Ordering::SeqCst);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if let Ok(c) = std::ffi::CString::new(scratch.as_os_str().as_bytes()) {
            let old = SCRATCH_PATH.swap(c.into_raw(), Ordering::SeqCst);
            if !old.is_null() {
                drop(unsafe { std::ffi::CString::from_raw(old) });
            }
        }
        unsafe {
            let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            let prev = libc::signal(libc::SIGTERM, handler);
            if prev != handler {
                PREV_HANDLER.store(prev, Ordering::SeqCst);
            }
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGHUP, handler);
        }
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
    loader_peak: u8,
}

struct Shared {
    procs: Procs,
    stop: Arc<AtomicBool>,
    cwd: PathBuf,
    ffmpeg: PathBuf,
    ca_file: Option<PathBuf>,
    ytdlp: PathBuf,
    ytdlp_update: Option<crate::deps::Update>,
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
    period: f64,
}

fn resolve_cell_aspect(cell_px: Option<(u16, u16)>) -> f64 {
    match cell_px {
        Some((w, h)) if w > 0 && h > 0 => f64::from(h) / f64::from(w),
        _ => auto_ascii_core::DEFAULT_CELL_ASPECT,
    }
}

fn next_style(style: Style, presses: u32) -> Style {
    let at = Style::ALL.iter().position(|c| *c == style).unwrap_or(0);
    Style::ALL[(at + presses as usize) % Style::ALL.len()]
}

pub fn run(programs: &Programs, args: &StreamArgs) -> Result<(), BoxErr> {
    let scratch = ScratchDir::new().map_err(|e| format!("creating a private temp dir: {e}"))?;
    let scratch_path = scratch.path().to_path_buf();
    let procs = Procs::new();
    let guard = ProcsGuard(procs.clone());
    let shared = Shared {
        procs: procs.clone(),
        stop: Arc::new(AtomicBool::new(false)),
        cwd: scratch_path.clone(),
        ffmpeg: programs.ffmpeg.clone(),
        ca_file: programs.ffmpeg_ca_file.clone(),
        ytdlp: programs.ytdlp.clone(),
        ytdlp_update: programs.ytdlp_update.clone(),
    };
    let t0 = Instant::now();
    let plan = open_audio(args.no_audio, args.sim.is_some());

    let (reason, stats, error, scratch_entries) = match args.sim {
        Some(sim) => {
            install_signal_handlers(&scratch_path);
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
            install_signal_handlers(&scratch_path);
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
    let ytdlp = YtDlp::new(shared.ytdlp.clone(), shared.procs.clone(), &shared.cwd)
        .cookies_from_browser(args.cookies_from_browser.as_deref());
    let stop = shared.stop.clone();
    let update = shared.ytdlp_update.clone();
    std::thread::spawn(move || {
        let notes = tx.clone();
        let stopped = || stop.load(Ordering::SeqCst);
        let guarded = update.map(|update| {
            move || if stopped() { Ok(None) } else { update(&stopped) }
        });
        let result = ytdlp.resolve_or_update(
            &input,
            max_height,
            &mut |e| {
                let _ = tx.send(match e {
                    Event::Started => Msg::YtdlpStarted,
                    Event::EntryFound(url) => Msg::EntryFound(url),
                });
            },
            guarded.as_ref().map(|g| g as &(dyn Fn() -> Result<Option<String>, String> + Send + Sync)),
            &mut |note| {
                let _ = notes.send(Msg::Fallback(note));
            },
        );
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
            ca_file: shared.ca_file.clone(),
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
            ca_file: shared.ca_file.clone(),
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
        Pipes {
            frames: frame_rx,
            recycle: recycle_tx,
            queued,
            video_thread: Some(thread),
            target_frames,
            period: 1.0 / fps.value(),
        },
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
    style: Style,
    glyph_tier: GlyphTier,
    color: ColorTier,
    loader: Grid<Cell>,
    showing_loader: bool,
    pending: VecDeque<Frame>,
    video_eof: bool,
    audio_eof: bool,
    audio_starved_since: Option<Instant>,
    newest_pts: Option<f64>,
    device_lost: bool,
    ending_at: Option<Instant>,
}

impl Loop {
    fn stage(&mut self, name: &'static str) {
        if !self.stats.stages.iter().any(|(n, _)| *n == name) {
            let pct = self.progress.update(percent(&self.load));
            self.stats.stages.push((name, pct));
        }
    }

    fn draw_loader<B: Backend + Discard>(&mut self, backend: &mut B, pct: u8, t0: Instant) {
        let (cols, rows) = backend.caps().cells;
        if self.loader.cols() != cols || self.loader.rows() != rows {
            self.loader.resize(cols, rows);
        }
        let style = LoaderStyle::for_style(self.style, self.glyph_tier, self.color);
        let tick = (t0.elapsed().as_millis() / LOADER_FRAME.as_millis()) as u32;
        let title = self.stats.media.as_ref().map(|m| m.title.as_str());
        draw_loader(&mut self.loader, pct, Some(tick), &style, title);
        if !self.showing_loader {
            backend.invalidate();
            self.showing_loader = true;
        }
        backend.present(&self.loader);
        backend.discard_output();
        self.stats.loader_peak = self.stats.loader_peak.max(pct);
        if self.stats.loader_dump.as_ref().is_none_or(|(p, _)| *p < 40 && pct >= *p) {
            self.stats.loader_dump = Some((pct, grid_text(&self.loader)));
        }
    }

    fn recycle(&self, pipes: &Pipes, frame: Frame) {
        let _ = pipes.recycle.send(frame);
    }
}

fn session<B: Backend + Discard>(
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
        style: args.style,
        glyph_tier,
        color,
        loader: Grid::new(0, 0),
        showing_loader: false,
        pending: VecDeque::new(),
        video_eof: false,
        audio_eof: false,
        audio_starved_since: None,
        newest_pts: None,
        device_lost: false,
        ending_at: None,
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
        if drained.style_cycle > 0 {
            lp.style = next_style(lp.style, drained.style_cycle);
            if let Some(p) = player.as_mut() {
                p.set_style(lp.style);
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
                    live.set_style(lp.style);
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
                Msg::AudioEof => {
                    lp.audio_eof = true;
                    if !lp.load.first_audio && let Some(a) = audio.take() {
                        a.ring.close();
                        drop(a);
                        lp.stats.notes.push("audio: the audio track was empty; playing without sound".into());
                        lp.load.first_audio = true;
                        clock = Some(Box::new(MonotonicClock::new()));
                    }
                }
                Msg::VideoEof => {
                    if !lp.load.first_video {
                        error = Some("video: the stream ended before its first frame".into());
                        break 'run Reason::Error;
                    }
                    lp.video_eof = true;
                }
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
                    lp.draw_loader(backend, 100, t0);
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
                let video_fill = match (&pipes, &clock) {
                    (Some(p), Some(c)) if !lp.video_eof => {
                        let now = c.now(now_i);
                        while lp.pending.len() < p.target_frames {
                            match p.frames.try_recv() {
                                Ok(f) => {
                                    p.queued.fetch_sub(1, Ordering::SeqCst);
                                    lp.stats.frames_decoded += 1;
                                    lp.newest_pts = Some(f.pts);
                                    if f.pts + 1e-9 < now {
                                        lp.stats.frames_dropped += 1;
                                        lp.recycle(p, f);
                                    } else {
                                        lp.pending.push_back(f);
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        lp.pending.len() as f64 / p.target_frames as f64
                    }
                    _ => video_fill,
                };
                let pct = percent(&LoadState::rebuffering(audio_fill, video_fill));
                if pct >= 100 {
                    lp.draw_loader(backend, 100, t0);
                    lp.phase = Phase::Playing;
                    lp.audio_starved_since = None;
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
                            lp.newest_pts = Some(f.pts);
                            lp.pending.push_back(f);
                        }
                        Err(_) => break,
                    }
                }
                let pts: Vec<f64> = lp.pending.iter().map(|f| f.pts).collect();
                let mut wait_for = Duration::from_millis(10);
                match pick_frame(&pts, now, p.period) {
                    Pick::Stale { dropped } => {
                        for _ in 0..dropped {
                            if let Some(f) = lp.pending.pop_front() {
                                lp.recycle(p, f);
                            }
                        }
                        lp.stats.frames_dropped += dropped as u64;
                        continue;
                    }
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
                        backend.discard_output();
                        let drift = (c.now(Instant::now()) - frame.pts).abs();
                        lp.stats.max_drift = lp.stats.max_drift.max(drift);
                        lp.stats.frames_rendered += 1;
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
                    let deadline = *lp.ending_at.get_or_insert_with(|| match &audio {
                        Some(a) if !lp.device_lost => tail_deadline(&a.shared).min(now_i + TAIL_WAIT_MAX),
                        _ => now_i,
                    });
                    if now_i >= deadline {
                        break Reason::Eof;
                    }
                    std::thread::sleep((deadline - now_i).min(Duration::from_millis(10)));
                    continue;
                }
                let audio_starved = audio.as_ref().is_some_and(|a| {
                    !audio_done && !lp.audio_eof && !a.ring.eof() && a.ring.len() < a.src_channels * 64
                });
                let audio_starved_for = if audio_starved {
                    let since = *lp.audio_starved_since.get_or_insert(now_i);
                    Some(now_i.duration_since(since).as_secs_f64())
                } else {
                    lp.audio_starved_since = None;
                    None
                };
                let video_behind = (!lp.video_eof && lp.pending.is_empty() && p.queued.load(Ordering::SeqCst) == 0)
                    .then(|| now - lp.newest_pts.unwrap_or(0.0));
                let audio_plays = audio.is_some() && !audio_done;
                if stall_action(audio_starved_for, video_behind, audio_plays) == Stall::Rebuffer {
                    lp.phase = Phase::Rebuffering;
                    lp.stats.rebuffers += 1;
                    c.set_running(false, now_i);
                    continue;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stall {
    Continue,
    Rebuffer,
}

fn stall_action(audio_starved_for: Option<f64>, video_behind: Option<f64>, audio_plays: bool) -> Stall {
    let video_limit = if audio_plays { VIDEO_BEHIND_SECS } else { STALL_SECS };
    if audio_starved_for.is_some_and(|s| s >= STALL_SECS) || video_behind.is_some_and(|b| b >= video_limit) {
        Stall::Rebuffer
    } else {
        Stall::Continue
    }
}

fn tail_deadline(shared: &AudioShared) -> Instant {
    let callback = shared.epoch + Duration::from_nanos(shared.last_callback_ns.load(Ordering::SeqCst));
    let chunk = shared.last_chunk.load(Ordering::SeqCst) as f64 / f64::from(shared.rate);
    callback + Duration::from_secs_f64(chunk) + Duration::from_nanos(shared.latency_ns.load(Ordering::SeqCst))
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
        "loader_peak_percent": stats.loader_peak,
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
    fn stalls_rebuffer_only_when_the_sound_cannot_carry_on() {
        assert_eq!(stall_action(None, None, true), Stall::Continue);
        assert_eq!(stall_action(Some(0.1), None, true), Stall::Continue, "a blip of audio underrun");
        assert_eq!(stall_action(Some(STALL_SECS), None, true), Stall::Rebuffer);
        assert_eq!(stall_action(None, Some(1.0), true), Stall::Continue, "the sound plays on; the picture catches up");
        assert_eq!(stall_action(None, Some(VIDEO_BEHIND_SECS), true), Stall::Rebuffer, "too far behind: re-buffer both");
        assert_eq!(stall_action(None, Some(0.5), false), Stall::Rebuffer, "silent: the picture is the only clock");
        assert_eq!(stall_action(None, Some(0.1), false), Stall::Continue);
    }

    #[test]
    fn the_audio_tail_is_waited_for_until_it_has_played() {
        let shared = AudioShared::new(48_000);
        let cb = shared.epoch + Duration::from_secs(1);
        shared.record(4_800, 4_800, cb, Some(Duration::from_millis(30)), true);
        assert_eq!(tail_deadline(&shared), cb + Duration::from_millis(130), "the last chunk plus the output latency");
    }

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

    struct Local {
        scratch: ScratchDir,
    }

    impl Local {
        fn new() -> Local {
            Local { scratch: ScratchDir::new().unwrap() }
        }

        fn dir(&self) -> &Path {
            self.scratch.path()
        }

        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.dir().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        }

        fn media(&self, secs: u32, with_audio: bool) -> PathBuf {
            let path = self.dir().join(if with_audio { "av.mkv" } else { "v.mkv" });
            let mut cmd = std::process::Command::new(real_ffmpeg());
            cmd.args(["-nostdin", "-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i"])
                .arg(format!("testsrc2=size=160x90:rate=30:duration={secs}"));
            if with_audio {
                cmd.args(["-f", "lavfi", "-i"])
                    .arg(format!("sine=frequency=440:sample_rate=48000:duration={secs}"));
                cmd.args(["-c:a", "pcm_s16le"]);
            }
            let out = cmd.args(["-c:v", "ffv1"]).arg(&path).output().expect("ffmpeg runs");
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            path
        }

        fn ytdlp(&self, secs: u32, video: &Path, audio: Option<&Path>) -> PathBuf {
            let mut parts = vec![serde_json::json!({
                "format_id": "v", "protocol": "file", "url": video, "vcodec": "ffv1", "acodec": "none",
                "height": 90,
            })];
            if let Some(a) = audio {
                parts.push(serde_json::json!({
                    "format_id": "a", "protocol": "file", "url": a, "vcodec": "none", "acodec": "pcm_s16le",
                }));
            }
            let json = serde_json::json!({
                "_type": "video", "id": "local", "title": "Local", "duration": secs, "fps": 30,
                "width": 160, "height": 90, "requested_formats": parts,
            });
            self.script("yt-dlp", &format!("cat <<'JSON'\n{json}\nJSON"))
        }

        fn run(&self, ytdlp: PathBuf, ffmpeg: PathBuf, limit: f64) -> (Reason, Stats, Option<String>, SimBackend, Duration) {
            let procs = Procs::new();
            let guard = ProcsGuard(procs.clone());
            let shared = Shared {
                procs: procs.clone(),
                stop: Arc::new(AtomicBool::new(false)),
                cwd: self.dir().to_path_buf(),
                ffmpeg,
                ca_file: None,
                ytdlp,
                ytdlp_update: None,
            };
            let args = StreamArgs {
                input: "https://www.youtube.com/watch?v=localfixture".into(),
                style: Style::Ascii,
                palette: PaletteChoice::Auto,
                max_height: 480,
                no_audio: false,
                sim: Some(SimSpec { cols: 60, rows: 20, secs: limit }),
                sim_dump: None,
                cookies_from_browser: None,
            };
            let mut backend = SimBackend::new(60, 20);
            let t0 = Instant::now();
            let (reason, stats, error) = session(&mut backend, &args, &shared, AudioPlan::Sim, t0, Some(limit));
            let took = t0.elapsed();
            drop(guard);
            assert_eq!(procs.alive(), 0, "children outlived the session");
            (reason, stats, error, backend, took)
        }
    }

    fn real_ffmpeg() -> PathBuf {
        auto_ascii::tools::Lookup::from_env().program(auto_ascii::tools::Tool::Ffmpeg)
    }

    #[test]
    fn sim_output_is_discarded_after_every_present() {
        let local = Local::new();
        let video = local.media(1, false);
        let ytdlp = local.ytdlp(1, &video, None);
        let (reason, stats, error, mut backend, _) = local.run(ytdlp, real_ffmpeg(), 20.0);
        assert_eq!((reason, error), (Reason::Eof, None));
        assert!(stats.frames_rendered > 20);
        let kept = backend.take_output().len();
        assert_eq!(kept, 0, "the simulator kept {kept} bytes of presented frames");
    }

    #[test]
    fn the_loader_draws_its_completed_bar_before_playback() {
        let local = Local::new();
        let video = local.media(1, false);
        let ytdlp = local.ytdlp(1, &video, None);
        let (reason, stats, _, _, _) = local.run(ytdlp, real_ffmpeg(), 20.0);
        assert_eq!(reason, Reason::Eof);
        assert_eq!(stats.loader_peak, 100, "the 100% bar was never presented");
    }

    #[test]
    fn an_audio_track_that_ends_before_its_first_sample_plays_silently() {
        let local = Local::new();
        let av = local.media(1, true);
        let ytdlp = local.ytdlp(1, &av, Some(&av));
        let real = real_ffmpeg();
        let ffmpeg = local.script(
            "ffmpeg",
            &format!("case \"$*\" in *0:a:0*) exit 0;; esac\nexec '{}' \"$@\"", real.display()),
        );
        let (reason, stats, error, _, _) = local.run(ytdlp, ffmpeg, 20.0);
        assert_eq!((reason, error), (Reason::Eof, None), "an empty audio stream stalled the loader");
        assert!(stats.frames_rendered > 20);
        assert!(stats.notes.iter().any(|n| n.contains("audio track was empty")), "{:?}", stats.notes);
    }

    #[test]
    fn a_video_track_with_no_frames_is_a_clean_error() {
        let local = Local::new();
        let av = local.media(1, true);
        let ytdlp = local.ytdlp(1, &av, Some(&av));
        let real = real_ffmpeg();
        let ffmpeg = local.script(
            "ffmpeg",
            &format!("case \"$*\" in *0:v:0*) exit 0;; esac\nexec '{}' \"$@\"", real.display()),
        );
        let (reason, _, error, _, _) = local.run(ytdlp, ffmpeg, 20.0);
        assert_eq!(reason, Reason::Error, "an empty video stream stalled the loader");
        assert_eq!(error.as_deref(), Some("video: the stream ended before its first frame"));
    }

    #[test]
    fn audio_and_video_play_in_sync_to_the_end() {
        let local = Local::new();
        let av = local.media(2, true);
        let ytdlp = local.ytdlp(2, &av, Some(&av));
        let (reason, stats, error, _, took) = local.run(ytdlp, real_ffmpeg(), 30.0);
        assert_eq!((reason, error), (Reason::Eof, None));
        assert_eq!(stats.frames_rendered + stats.frames_dropped, 60, "every frame shown or dropped");
        assert!(stats.frames_rendered >= 50, "{} rendered", stats.frames_rendered);
        let period = 1.0 / 30.0;
        assert!(stats.max_drift < 2.0 * period, "drift {:.4}s", stats.max_drift);
        let consumed = stats.samples_consumed as f64 / 48_000.0;
        assert!((consumed - 2.0).abs() < 0.05, "{consumed:.3}s of audio played");
        assert_eq!(stats.underruns, 0);
        assert_eq!(stats.rebuffers, 0);
        let started = stats.started_at.unwrap();
        assert!(took.as_secs_f64() >= started + 2.0, "exited before the audio tail played ({took:?})");
    }

    #[test]
    fn a_brief_video_stall_keeps_the_audio_playing() {
        let local = Local::new();
        let av = local.media(5, true);
        let ytdlp = local.ytdlp(5, &av, Some(&av));
        let real = real_ffmpeg();
        let frame = 480 * 270 * 3;
        let ffmpeg = local.script(
            "ffmpeg",
            &format!(
                "case \"$*\" in *0:v:0*) '{r}' \"$@\" | {{ head -c {n}; sleep 3; cat; }}; exit 0;; esac\nexec '{r}' \"$@\"",
                r = real.display(),
                n = frame * 60
            ),
        );
        let (reason, stats, error, _, _) = local.run(ytdlp, ffmpeg, 30.0);
        assert_eq!((reason, error), (Reason::Eof, None));
        assert_eq!(stats.rebuffers, 0, "a ~1 s picture-only stall paused the sound");
        assert!(stats.frames_dropped >= 10, "the picture caught up by dropping: {}", stats.frames_dropped);
        assert_eq!(stats.underruns, 0);
        let consumed = stats.samples_consumed as f64 / 48_000.0;
        assert!((consumed - 5.0).abs() < 0.05, "{consumed:.3}s of audio played");
    }

    #[test]
    fn slash_cycles_every_style() {
        assert_eq!(next_style(Style::Ascii, Style::ALL.len() as u32), Style::Ascii);
        let first = Style::ALL[0];
        assert_eq!(next_style(first, 1), Style::ALL[1]);
    }

    impl Local {
        fn media_split(&self, video_secs: u32, audio_secs: u32) -> PathBuf {
            let path = self.dir().join(format!("split-{video_secs}-{audio_secs}.mkv"));
            let mut cmd = std::process::Command::new(real_ffmpeg());
            cmd.args(["-nostdin", "-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i"])
                .arg(format!("testsrc2=size=160x90:rate=30:duration={video_secs}"))
                .args(["-f", "lavfi", "-i"])
                .arg(format!("sine=frequency=440:sample_rate=48000:duration={audio_secs}"))
                .args(["-c:a", "pcm_s16le", "-c:v", "ffv1"])
                .arg(&path);
            let out = cmd.output().expect("ffmpeg runs");
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            path
        }
    }

    #[test]
    fn video_outlives_audio_and_still_plays_to_the_end() {
        let local = Local::new();
        let av = local.media_split(3, 1);
        let ytdlp = local.ytdlp(3, &av, Some(&av));
        let (reason, stats, error, _, took) = local.run(ytdlp, real_ffmpeg(), 30.0);
        assert_eq!((reason, error), (Reason::Eof, None), "did not reach a clean end");
        assert_eq!(stats.frames_rendered + stats.frames_dropped, 90, "every frame shown or dropped");
        assert!(stats.frames_rendered >= 80, "{} rendered", stats.frames_rendered);
        assert_eq!(stats.rebuffers, 0, "audio EOF was mistaken for starvation");
        assert_eq!(stats.underruns, 0);
        let period = 1.0 / 30.0;
        assert!(stats.max_drift < 2.0 * period, "drift {:.4}s", stats.max_drift);
        let consumed = stats.samples_consumed as f64 / 48_000.0;
        assert!((consumed - 1.0).abs() < 0.05, "{consumed:.3}s of audio played");
        let started = stats.started_at.unwrap();
        let wall = took.as_secs_f64();
        assert!(wall >= started + 3.0 - 0.1, "exited early: {wall:.2}s (started {started:.2}s)");
        assert!(wall <= started + 3.0 + 0.6, "exited late: {wall:.2}s (started {started:.2}s)");
    }

    fn stall_after(local: &Local, with_audio: bool) -> (Reason, Stats, Option<String>) {
        let media = if with_audio { local.media_split(5, 1) } else { local.media(5, false) };
        let ytdlp = local.ytdlp(5, &media, with_audio.then_some(media.as_path()));
        let real = real_ffmpeg();
        let frame = 480 * 270 * 3;
        let throttle = local.dir().join("throttle.py");
        std::fs::write(
            &throttle,
            "import sys,time\nn=int(sys.argv[1]);i=0\ninp=sys.stdin.buffer;out=sys.stdout.buffer\nwhile True:\n    b=inp.read(n)\n    if not b: break\n    out.write(b);out.flush();i+=1\n    if i==60: time.sleep(3)\n    elif i>60: time.sleep(0.05)\n",
        )
        .unwrap();
        let ffmpeg = local.script(
            "ffmpeg",
            &format!(
                "case \"$*\" in *0:v:0*) '{r}' \"$@\" | python3 '{t}' {frame}; exit 0;; esac\nexec '{r}' \"$@\"",
                r = real.display(),
                t = throttle.display(),
            ),
        );
        let (reason, stats, error, _, _) = local.run(ytdlp, ffmpeg, 40.0);
        (reason, stats, error)
    }

    #[test]
    fn a_picture_stall_after_the_sound_ended_rebuffers_like_a_silent_stream() {
        let silent = Local::new();
        let (reason, base, error) = stall_after(&silent, false);
        assert_eq!((reason, error), (Reason::Eof, None));
        let ended = Local::new();
        let (reason, stats, error) = stall_after(&ended, true);
        assert_eq!((reason, error), (Reason::Eof, None));
        eprintln!(
            "silent: rendered {} dropped {} rebuffers {} | audio-ended: rendered {} dropped {} rebuffers {}",
            base.frames_rendered, base.frames_dropped, base.rebuffers, stats.frames_rendered, stats.frames_dropped, stats.rebuffers
        );
        assert_eq!(base.rebuffers, 1, "the silent baseline re-buffers once");
        assert!(base.frames_rendered >= 110, "silent baseline: {} rendered", base.frames_rendered);
        assert_eq!(stats.frames_rendered + stats.frames_dropped, 150);
        assert_eq!(stats.rebuffers, base.rebuffers, "the same stall should re-buffer the same way");
        assert!(
            stats.frames_rendered >= base.frames_rendered - 15,
            "after the sound ended the re-buffer kept the clock running: {} rendered, {} dropped (silent stream: {} rendered, {} dropped)",
            stats.frames_rendered,
            stats.frames_dropped,
            base.frames_rendered,
            base.frames_dropped
        );
    }
}
