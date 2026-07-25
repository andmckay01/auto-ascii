//! Terminal capability probe (PLAN §3.1, M1).
//!
//! Passive signals (`COLORTERM`, `TERM`, `TERM_PROGRAM`) are hints, not
//! truth. The active volley goes out in ONE write on the tty:
//! XTVERSION (`CSI > 0 q`), DECRQM 2026 (`CSI ? 2026 $ p` — never a
//! hardcoded support table), XTGETTCAP `RGB`, `CSI 16 t` (cell px → cell
//! aspect), then **DA1 (`CSI c`) last as sentinel** — when its reply
//! arrives, everything that was going to answer has answered. Replies are
//! parsed until the DA1 answer or a deadline (default 200 ms); `!isatty`
//! or silence → conservative default (256-color, ASCII glyphs). Never hangs.
//!
//! Results are cached at `$XDG_CACHE_HOME/sleepytime/caps` keyed on
//! `(TERM, TERM_PROGRAM, COLORTERM, tmux?)` — COLORTERM is part of the key
//! (M1 review low 3: a COLORTERM-stripped run, e.g. under a pipe wrapper,
//! must not poison later runs where COLORTERM proves truecolor), and a
//! cache hit can only *upgrade* the passive evidence of the current run,
//! never downgrade it. Escape hatches: a forced tier (`--tier`) overrides
//! the detected color tier, `--no-query` skips the volley entirely,
//! `no_cache` bypasses the cache (tests).
//!
//! Straggler hygiene (M1 review low 2): replies still in flight at the
//! deadline are consumed by a bounded quiet-gap grace drain (only when the
//! terminal was already mid-answer — silent terminals return at the
//! deadline unchanged), and [`volley_stragglers_possible`] tells the
//! backend's event decoder to filter any reply fragments that arrive later
//! still, so probe bytes never surface as key events (digits are seek
//! bindings!).
//!
//! Capability tiers are color depth + glyph repertoire only — no
//! throughput/latency classification (Scope amendment).

use std::fs;
use std::io;
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::ansi::write_all_fd;
use crate::caps::{Caps, ColorTier, GlyphFlags, GlyphSupportTier};

/// Default reply deadline (PLAN §3.1: 150–250 ms local).
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// Grace-drain quiet gap: once the deadline passed with a reply already in
/// flight (some bytes arrived, no DA1 yet), keep consuming until the input
/// has been quiet this long. Only extends the probe when bytes actually
/// arrived — a silent terminal still returns at the deadline exactly.
const STRAGGLER_QUIET: Duration = Duration::from_millis(150);
/// Hard cap on the grace drain past the deadline (never hangs, PLAN §3.1).
const STRAGGLER_GRACE_CAP: Duration = Duration::from_millis(1000);

/// True when the last volley this process wrote timed out without its DA1
/// sentinel — reply bytes may still arrive on stdin *after* [`probe_caps`]
/// returned. `AnsiBackend` reads this to arm its event-decoder straggler
/// filter (M1 review low 2: late reply fragments must never surface as key
/// events). Reset on every probe; cache hits / `--no-query` / `!isatty`
/// never leave stragglers (no volley was written).
static VOLLEY_STRAGGLERS: AtomicBool = AtomicBool::new(false);

/// See [`VOLLEY_STRAGGLERS`].
pub(crate) fn volley_stragglers_possible() -> bool {
    VOLLEY_STRAGGLERS.load(Ordering::Relaxed)
}

/// The active volley, sent as ONE write (PLAN §3.1). Order matters: DA1 last
/// as sentinel.
pub const VOLLEY: &[u8] = b"\x1b[>0q\x1b[?2026$p\x1bP+q524742\x1b\\\x1b[16t\x1b[c";

/// Options for [`probe_caps`] — the `--tier` / `--no-query` / `--no-cache`
/// escape hatches plus test knobs.
#[derive(Clone, Debug)]
pub struct ProbeOptions {
    /// `--tier`: force the color tier (applied last, overrides detection).
    pub forced_tier: Option<ColorTier>,
    /// `--no-query`: never write the volley; passive env hints only.
    pub no_query: bool,
    /// Bypass the cache entirely (no read, no write).
    pub no_cache: bool,
    /// Reply deadline for the volley.
    pub timeout: Duration,
    /// Cache directory override; `None` → `$XDG_CACHE_HOME/sleepytime`
    /// (fallback `~/.cache/sleepytime`).
    pub cache_dir: Option<PathBuf>,
}

impl Default for ProbeOptions {
    fn default() -> ProbeOptions {
        ProbeOptions {
            forced_tier: None,
            no_query: false,
            no_cache: false,
            timeout: DEFAULT_PROBE_TIMEOUT,
            cache_dir: None,
        }
    }
}

/// What the volley got back (parsed by [`ProbeParser`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeReplies {
    /// XTVERSION text (e.g. `kitty(0.32.2)`), for the future quirk table.
    pub xtversion: Option<String>,
    /// DECRPM `Ps` for mode 2026 (0 = not recognized, 1/2/3/4 = recognized).
    pub decrqm_2026: Option<u8>,
    /// XTGETTCAP `RGB`: `Some(true)` on a valid (`1+r`) reply.
    pub xtgettcap_rgb: Option<bool>,
    /// Cell size in px `(w, h)` from the `CSI 16 t` reply.
    pub cell_px: Option<(u16, u16)>,
    /// The DA1 sentinel answered — the volley is complete.
    pub da1: bool,
}

/// Incremental parser for volley replies — a tiny VT-reply state machine
/// (CSI + DCS + OSC-skip), tolerant of interleaved garbage. Pure: feed it
/// scripted byte streams in tests.
#[derive(Debug)]
pub struct ProbeParser {
    state: State,
    /// CSI params+intermediates (final byte excluded), capped.
    seq: Vec<u8>,
    /// Sequence overflowed the cap — still consumed, contents dropped.
    overflow: bool,
    replies: ProbeReplies,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Csi,
    Dcs,
    /// ESC seen inside DCS/OSC (possible ST).
    DcsEsc,
    Osc,
    OscEsc,
}

/// Cap on stored sequence bytes — anything longer is consumed but dropped.
const SEQ_CAP: usize = 256;

impl Default for ProbeParser {
    fn default() -> ProbeParser {
        ProbeParser::new()
    }
}

impl ProbeParser {
    pub fn new() -> ProbeParser {
        ProbeParser {
            state: State::Ground,
            seq: Vec::with_capacity(64),
            overflow: false,
            replies: ProbeReplies::default(),
        }
    }

    /// Parsed replies so far.
    pub fn replies(&self) -> &ProbeReplies {
        &self.replies
    }

    /// True once the DA1 sentinel reply has been seen.
    pub fn done(&self) -> bool {
        self.replies.da1
    }

    /// Feed reply bytes; returns [`Self::done`] (DA1 seen).
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        for &b in bytes {
            self.step(b);
        }
        self.done()
    }

    fn begin(&mut self, state: State) {
        self.state = state;
        self.seq.clear();
        self.overflow = false;
    }

    fn push(&mut self, b: u8) {
        if self.seq.len() < SEQ_CAP {
            self.seq.push(b);
        } else {
            self.overflow = true;
        }
    }

    fn step(&mut self, b: u8) {
        match self.state {
            State::Ground => match b {
                0x1b => self.state = State::Esc,
                0x9b => self.begin(State::Csi),
                0x90 => self.begin(State::Dcs),
                _ => {}
            },
            State::Esc => match b {
                b'[' => self.begin(State::Csi),
                b'P' => self.begin(State::Dcs),
                b']' => self.begin(State::Osc),
                0x1b => {} // stay: ESC ESC
                _ => self.state = State::Ground,
            },
            State::Csi => match b {
                0x20..=0x3f => self.push(b),
                0x40..=0x7e => {
                    self.finish_csi(b);
                    self.state = State::Ground;
                }
                0x1b => self.state = State::Esc, // malformed: restart
                _ => {} // C0 controls inside CSI: ignore
            },
            State::Dcs => match b {
                0x1b => self.state = State::DcsEsc,
                0x9c => {
                    self.finish_dcs();
                    self.state = State::Ground;
                }
                _ => self.push(b),
            },
            State::DcsEsc => match b {
                b'\\' => {
                    self.finish_dcs();
                    self.state = State::Ground;
                }
                0x1b => {} // stay armed
                _ => {
                    // Not ST — treat as DCS payload noise and continue.
                    self.state = State::Dcs;
                }
            },
            State::Osc => match b {
                0x07 | 0x9c => self.state = State::Ground,
                0x1b => self.state = State::OscEsc,
                _ => {}
            },
            State::OscEsc => match b {
                b'\\' => self.state = State::Ground,
                0x1b => {}
                _ => self.state = State::Osc,
            },
        }
    }

    fn finish_csi(&mut self, final_byte: u8) {
        if self.overflow {
            return;
        }
        let body = std::mem::take(&mut self.seq);
        match final_byte {
            // DECRPM: CSI ? 2026 ; Ps $ y
            b'y' => {
                if let Some(rest) = body.strip_prefix(b"?2026;") {
                    let digits: Vec<u8> =
                        rest.iter().copied().take_while(u8::is_ascii_digit).collect();
                    if !digits.is_empty()
                        && let Ok(ps) = std::str::from_utf8(&digits).unwrap_or("").parse::<u8>()
                    {
                        self.replies.decrqm_2026 = Some(ps);
                    }
                }
            }
            // CSI 16 t reply: CSI 6 ; height ; width t
            b't' => {
                let parts: Vec<u32> = body
                    .split(|&c| c == b';')
                    .map(|p| std::str::from_utf8(p).ok().and_then(|s| s.parse().ok()))
                    .collect::<Option<Vec<u32>>>()
                    .unwrap_or_default();
                if parts.len() == 3 && parts[0] == 6 {
                    let (h, w) = (parts[1], parts[2]);
                    if (1..=u32::from(u16::MAX)).contains(&h)
                        && (1..=u32::from(u16::MAX)).contains(&w)
                    {
                        self.replies.cell_px = Some((w as u16, h as u16));
                    }
                }
            }
            // DA1 sentinel: CSI ? … c
            b'c' if body.first() == Some(&b'?') || body.is_empty() => {
                self.replies.da1 = true;
            }
            _ => {}
        }
    }

    fn finish_dcs(&mut self) {
        if self.overflow {
            return;
        }
        let body = std::mem::take(&mut self.seq);
        if let Some(rest) = body.strip_prefix(b">|") {
            // XTVERSION: DCS > | text ST
            self.replies.xtversion = Some(String::from_utf8_lossy(rest).into_owned());
        } else if body.starts_with(b"1+r") {
            // XTGETTCAP valid reply — confirm it is about the RGB cap
            // (hex "524742"), case-insensitively.
            let upper: Vec<u8> = body.iter().map(u8::to_ascii_uppercase).collect();
            if upper.windows(6).any(|w| w == b"524742") {
                self.replies.xtgettcap_rgb = Some(true);
            }
        } else if body.starts_with(b"0+r") && self.replies.xtgettcap_rgb.is_none() {
            self.replies.xtgettcap_rgb = Some(false);
        }
    }
}

/// Passive environment hints — read once, testable without touching the
/// process environment.
#[derive(Clone, Debug, Default)]
pub(crate) struct EnvHints {
    pub term: String,
    pub term_program: String,
    pub colorterm: String,
    /// Effective locale charset source (LC_ALL > LC_CTYPE > LANG).
    pub locale: String,
    pub tmux: bool,
}

impl EnvHints {
    pub(crate) fn from_env() -> EnvHints {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .map(var)
            .find(|v| !v.is_empty())
            .unwrap_or_default();
        let term = var("TERM");
        EnvHints {
            tmux: !var("TMUX").is_empty()
                || term.starts_with("tmux")
                || term.starts_with("screen"),
            term,
            term_program: var("TERM_PROGRAM"),
            colorterm: var("COLORTERM"),
            locale,
        }
    }
}

/// Passive tier + glyph hints (PLAN §3.1: hints, not truth). Used directly
/// under `--no-query`, and as the base the volley upgrades.
fn apply_passive(caps: &mut Caps, h: &EnvHints) {
    let term = h.term.to_ascii_lowercase();
    caps.color = if h.colorterm.eq_ignore_ascii_case("truecolor")
        || h.colorterm.eq_ignore_ascii_case("24bit")
        || matches!(h.term_program.as_str(), "iTerm.app" | "WezTerm" | "ghostty")
        || term.contains("direct")
        || term.contains("truecolor")
    {
        ColorTier::True
    } else if term == "linux" {
        ColorTier::C16
    } else if term == "dumb" {
        ColorTier::Mono
    } else if term.contains("256") {
        ColorTier::C256
    } else if term.contains("16color") {
        ColorTier::C16
    } else {
        // Conservative default (PLAN §9.4).
        ColorTier::C256
    };

    // Glyph repertoire: locale/terminal driven; braille stays unset until
    // verified (PLAN §3.4 palette 7 — verified-support only).
    if term == "linux" {
        caps.glyphs = GlyphFlags::ASCII.with(GlyphFlags::BLOCKS);
        caps.glyph_support = GlyphSupportTier::Cp437;
    } else {
        let loc = h.locale.to_ascii_lowercase();
        if loc.contains("utf-8") || loc.contains("utf8") {
            caps.glyphs = GlyphFlags::ASCII
                .with(GlyphFlags::BLOCKS)
                .with(GlyphFlags::BOX_DRAWING);
            caps.glyph_support = GlyphSupportTier::UnicodeCore;
        } else {
            caps.glyphs = GlyphFlags::ASCII;
            caps.glyph_support = GlyphSupportTier::AsciiOnly;
        }
    }
}

/// Probe terminal capabilities (PLAN §3.1). Never hangs, never blocks
/// longer than `opts.timeout` (+ a non-blocking drain): `!isatty` on
/// stdin/stdout or a silent terminal (no DA1) → conservative default
/// (256-color, ASCII glyphs). See module docs for the full flow.
pub fn probe_caps(opts: &ProbeOptions) -> Caps {
    let in_fd = libc::STDIN_FILENO;
    let out_fd = libc::STDOUT_FILENO;
    let tty =
        unsafe { libc::isatty(in_fd) } == 1 && unsafe { libc::isatty(out_fd) } == 1;

    // Conservative base (PLAN §9.4): 256-color, ASCII glyphs.
    let mut caps = Caps {
        color: ColorTier::C256,
        glyphs: GlyphFlags::ASCII,
        glyph_support: GlyphSupportTier::AsciiOnly,
        sync_2026: false,
        cells: (80, 24),
        cell_px: None,
        can_query: false,
    };

    if !tty {
        if let Some(t) = opts.forced_tier {
            caps.color = t;
        }
        return caps;
    }

    if let Some((cols, rows)) = term_cells(out_fd) {
        caps.cells = (cols, rows);
    }
    caps.cell_px = winsize_cell_px(out_fd);
    caps.can_query = !opts.no_query;

    let hints = EnvHints::from_env();
    VOLLEY_STRAGGLERS.store(false, Ordering::Relaxed);
    if opts.no_query {
        apply_passive(&mut caps, &hints);
    } else {
        let key = cache_key(&hints);
        let cached = if opts.no_cache { None } else { cache_load(opts, &key) };
        if let Some(hit) = cached {
            apply_passive(&mut caps, &hints);
            apply_cache_hit(&mut caps, hit);
        } else {
            match run_volley(in_fd, out_fd, opts.timeout) {
                Ok(replies) if replies.da1 => {
                    // The sentinel answered: passive hints + reply upgrades.
                    apply_passive(&mut caps, &hints);
                    if replies.xtgettcap_rgb == Some(true) {
                        caps.color = ColorTier::True;
                    }
                    caps.sync_2026 = matches!(replies.decrqm_2026, Some(1..=4));
                    if replies.cell_px.is_some() {
                        caps.cell_px = replies.cell_px;
                    }
                    if !opts.no_cache {
                        cache_store(
                            opts,
                            &key,
                            &CacheEntry { color: caps.color, sync_2026: caps.sync_2026 },
                        );
                    }
                }
                // Timeout / silence / error: stay on the conservative
                // default (PLAN §3.1 "!isatty or silence → dumb tier";
                // never cache a non-answer). The volley went out but its
                // sentinel never came back — reply bytes may yet arrive, so
                // arm the backend's straggler filter.
                _ => VOLLEY_STRAGGLERS.store(true, Ordering::Relaxed),
            }
        }
    }

    if let Some(t) = opts.forced_tier {
        caps.color = t;
    }
    caps
}

/// Terminal size in cells from `TIOCGWINSZ`.
fn term_cells(fd: libc::c_int) -> Option<(u16, u16)> {
    let ws = winsize(fd)?;
    if ws.ws_col == 0 || ws.ws_row == 0 {
        return None;
    }
    Some((ws.ws_col, ws.ws_row))
}

/// Cell pixel size from `TIOCGWINSZ` pixel fields, when reported.
fn winsize_cell_px(fd: libc::c_int) -> Option<(u16, u16)> {
    let ws = winsize(fd)?;
    if ws.ws_col == 0 || ws.ws_row == 0 || ws.ws_xpixel == 0 || ws.ws_ypixel == 0 {
        return None;
    }
    Some((ws.ws_xpixel / ws.ws_col, ws.ws_ypixel / ws.ws_row))
}

fn winsize(fd: libc::c_int) -> Option<libc::winsize> {
    let mut ws = MaybeUninit::<libc::winsize>::uninit();
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, ws.as_mut_ptr()) } != 0 {
        return None;
    }
    Some(unsafe { ws.assume_init() })
}

/// Restores the saved termios on drop — the volley can never leave the
/// terminal in raw mode, whatever path returns.
struct TermiosGuard {
    fd: libc::c_int,
    saved: libc::termios,
}

impl Drop for TermiosGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
        }
    }
}

/// One write out, poll-read replies until DA1 or deadline, then a final
/// non-blocking drain so no stray reply bytes are left on stdin.
fn run_volley(in_fd: libc::c_int, out_fd: libc::c_int, timeout: Duration) -> io::Result<ProbeReplies> {
    // Reply-readable termios: no echo, no canonical buffering; VMIN=0/VTIME=0
    // with poll() doing the waiting. Restored by the guard on every path.
    let mut saved = MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(in_fd, saved.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let saved = unsafe { saved.assume_init() };
    let mut raw = saved;
    raw.c_lflag &= !(libc::ICANON | libc::ECHO);
    raw.c_iflag &= !(libc::IXON | libc::ICRNL);
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(in_fd, libc::TCSANOW, &raw) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let _guard = TermiosGuard { fd: in_fd, saved };

    write_all_fd(out_fd, VOLLEY)?;

    let mut parser = ProbeParser::new();
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 512];
    let mut got_bytes = false;
    loop {
        let now = Instant::now();
        if parser.done() || now >= deadline {
            break;
        }
        let remain_ms = (deadline - now).as_millis().min(i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd { fd: in_fd, events: libc::POLLIN, revents: 0 };
        let rc = unsafe { libc::poll(&mut pfd, 1, remain_ms.max(1)) };
        if rc < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if rc == 0 {
            break; // deadline
        }
        let n = unsafe { libc::read(in_fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            got_bytes = true;
            parser.feed(&buf[..n as usize]);
        } else if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        } else {
            break; // EOF or hard read error
        }
    }

    // Grace drain (M1 review low 2): the deadline hit while the terminal was
    // mid-answer (bytes arrived but no DA1). Those in-flight reply bytes
    // MUST NOT leak into the app's input stream, where digits would trigger
    // the 0–9 seek bindings — keep consuming until the input goes quiet for
    // STRAGGLER_QUIET or the hard cap lapses. A late DA1 inside this window
    // still upgrades caps (the replies are perfectly valid, just slow).
    // Silent terminals (`!got_bytes`) skip this entirely: the probe returns
    // at the deadline exactly as before.
    if got_bytes && !parser.done() {
        let cap = deadline + STRAGGLER_GRACE_CAP;
        while Instant::now() < cap {
            let mut pfd = libc::pollfd { fd: in_fd, events: libc::POLLIN, revents: 0 };
            let rc = unsafe {
                libc::poll(&mut pfd, 1, STRAGGLER_QUIET.as_millis() as i32)
            };
            if rc < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if rc == 0 {
                break; // quiet gap: nothing more in flight
            }
            let n = unsafe { libc::read(in_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                if parser.feed(&buf[..n as usize]) {
                    break; // late DA1: the volley is complete after all
                }
            } else if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            } else {
                break;
            }
        }
    }

    // Final zero-timeout drain: consume any straggler reply bytes so they
    // never leak into the application's input stream.
    loop {
        let mut pfd = libc::pollfd { fd: in_fd, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut pfd, 1, 0) } <= 0 {
            break;
        }
        let n = unsafe { libc::read(in_fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        parser.feed(&buf[..n as usize]);
    }

    Ok(parser.replies().clone())
}

// ---------------------------------------------------------------------------
// Cache: $XDG_CACHE_HOME/sleepytime/caps, keyed on
// (TERM, TERM_PROGRAM, COLORTERM, tmux?)
// ---------------------------------------------------------------------------

const CACHE_FILE: &str = "caps";
/// Bumped 1 → 2 when COLORTERM joined the key (M1 review low 3) — v1 lines
/// are ignored on load and dropped on the next store.
const CACHE_VERSION: &str = "2";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CacheEntry {
    color: ColorTier,
    sync_2026: bool,
}

/// Color-tier ordering for the no-downgrade rule (higher = more capable).
fn tier_rank(t: ColorTier) -> u8 {
    match t {
        ColorTier::Mono => 0,
        ColorTier::C16 => 1,
        ColorTier::C256 => 2,
        ColorTier::True => 3,
    }
}

/// Merge a cache hit over freshly applied passive hints (M1 review low 3):
/// the cached tier may only *upgrade* what this run's passive evidence
/// already proves — a stale entry must never downgrade below COLORTERM/
/// TERM-derived truth. `sync_2026` is cache-only knowledge (passive hints
/// can never prove it), so the cached value is taken as-is.
fn apply_cache_hit(caps: &mut Caps, hit: CacheEntry) {
    if tier_rank(hit.color) > tier_rank(caps.color) {
        caps.color = hit.color;
    }
    caps.sync_2026 = hit.sync_2026;
}

fn cache_key(h: &EnvHints) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '|'], "_");
    format!(
        "{}|{}|{}|{}",
        clean(&h.term),
        clean(&h.term_program),
        clean(&h.colorterm),
        u8::from(h.tmux)
    )
}

fn cache_dir(opts: &ProbeOptions) -> Option<PathBuf> {
    if let Some(dir) = &opts.cache_dir {
        return Some(dir.clone());
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })?;
    Some(base.join("sleepytime"))
}

fn tier_tag(t: ColorTier) -> &'static str {
    match t {
        ColorTier::True => "truecolor",
        ColorTier::C256 => "256",
        ColorTier::C16 => "16",
        ColorTier::Mono => "mono",
    }
}

fn cache_load(opts: &ProbeOptions, key: &str) -> Option<CacheEntry> {
    let path = cache_dir(opts)?.join(CACHE_FILE);
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let mut f = line.split('\t');
        if f.next() != Some(CACHE_VERSION) || f.next() != Some(key) {
            continue;
        }
        let color = f.next()?.parse::<ColorTier>().ok()?;
        let sync_2026 = match f.next()? {
            "1" => true,
            "0" => false,
            _ => return None,
        };
        return Some(CacheEntry { color, sync_2026 });
    }
    None
}

/// Best-effort write (cache failures are never user-visible): rewrite the
/// file with this key's line replaced.
fn cache_store(opts: &ProbeOptions, key: &str, entry: &CacheEntry) {
    let Some(dir) = cache_dir(opts) else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(CACHE_FILE);
    // Keep only current-version lines for other keys (stale-version lines
    // are unreadable anyway — self-cleaning).
    let mut lines: Vec<String> = fs::read_to_string(&path)
        .map(|t| {
            t.lines()
                .filter(|l| {
                    let mut f = l.split('\t');
                    f.next() == Some(CACHE_VERSION) && f.next() != Some(key)
                })
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    lines.push(format!(
        "{CACHE_VERSION}\t{key}\t{}\t{}",
        tier_tag(entry.color),
        u8::from(entry.sync_2026)
    ));
    let mut text = lines.join("\n");
    text.push('\n');
    let _ = fs::write(path, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(term: &str, program: &str, colorterm: &str, locale: &str) -> EnvHints {
        EnvHints {
            term: term.into(),
            term_program: program.into(),
            colorterm: colorterm.into(),
            locale: locale.into(),
            tmux: false,
        }
    }

    fn passive(h: &EnvHints) -> Caps {
        let mut caps = Caps {
            color: ColorTier::C256,
            glyphs: GlyphFlags::ASCII,
            glyph_support: GlyphSupportTier::AsciiOnly,
            sync_2026: false,
            cells: (80, 24),
            cell_px: None,
            can_query: true,
        };
        apply_passive(&mut caps, h);
        caps
    }

    #[test]
    fn passive_hints_map_to_tiers() {
        assert_eq!(
            passive(&hints("xterm-256color", "", "truecolor", "en_US.UTF-8")).color,
            ColorTier::True
        );
        assert_eq!(passive(&hints("xterm-256color", "", "", "C")).color, ColorTier::C256);
        assert_eq!(passive(&hints("linux", "", "", "C")).color, ColorTier::C16);
        assert_eq!(passive(&hints("dumb", "", "", "C")).color, ColorTier::Mono);
        assert_eq!(passive(&hints("xterm", "WezTerm", "", "C")).color, ColorTier::True);
        assert_eq!(passive(&hints("xterm-direct", "", "", "C")).color, ColorTier::True);
        assert_eq!(passive(&hints("xterm", "", "", "C")).color, ColorTier::C256);
    }

    #[test]
    fn passive_hints_map_glyph_tiers() {
        let utf8 = passive(&hints("xterm-256color", "", "", "en_US.UTF-8"));
        assert_eq!(utf8.glyph_support, GlyphSupportTier::UnicodeCore);
        assert!(utf8.glyphs.contains(GlyphFlags::BLOCKS));
        assert!(utf8.glyphs.contains(GlyphFlags::BOX_DRAWING));
        assert!(!utf8.glyphs.contains(GlyphFlags::BRAILLE), "braille is verified-only");

        let console = passive(&hints("linux", "", "", "en_US.UTF-8"));
        assert_eq!(console.glyph_support, GlyphSupportTier::Cp437);

        let ascii = passive(&hints("xterm-256color", "", "", "C"));
        assert_eq!(ascii.glyph_support, GlyphSupportTier::AsciiOnly);
        assert_eq!(ascii.glyphs, GlyphFlags::ASCII);
    }

    #[test]
    fn cache_roundtrip_and_key_isolation() {
        let dir = std::env::temp_dir().join(format!("slpy-cache-test-{}", std::process::id()));
        let opts = ProbeOptions { cache_dir: Some(dir.clone()), ..ProbeOptions::default() };

        let kitty = cache_key(&hints("xterm-kitty", "", "truecolor", ""));
        let tmux_key = cache_key(&EnvHints { tmux: true, ..hints("tmux-256color", "", "", "") });
        assert_ne!(kitty, tmux_key);

        assert_eq!(cache_load(&opts, &kitty), None);
        cache_store(&opts, &kitty, &CacheEntry { color: ColorTier::True, sync_2026: true });
        cache_store(&opts, &tmux_key, &CacheEntry { color: ColorTier::C256, sync_2026: false });
        assert_eq!(
            cache_load(&opts, &kitty),
            Some(CacheEntry { color: ColorTier::True, sync_2026: true })
        );
        assert_eq!(
            cache_load(&opts, &tmux_key),
            Some(CacheEntry { color: ColorTier::C256, sync_2026: false })
        );

        // Overwrite in place: same key, new value, no duplicate lines.
        cache_store(&opts, &kitty, &CacheEntry { color: ColorTier::C16, sync_2026: false });
        assert_eq!(
            cache_load(&opts, &kitty),
            Some(CacheEntry { color: ColorTier::C16, sync_2026: false })
        );
        let text = fs::read_to_string(dir.join(CACHE_FILE)).unwrap();
        assert_eq!(text.lines().count(), 2);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn cache_ignores_garbage_lines() {
        let dir = std::env::temp_dir().join(format!("slpy-cache-garbage-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(CACHE_FILE), "junk\n1\tkey\tnot-a-tier\t1\n9\tkey\ttruecolor\t1\n")
            .unwrap();
        let opts = ProbeOptions { cache_dir: Some(dir.clone()), ..ProbeOptions::default() };
        assert_eq!(cache_load(&opts, "key"), None);
        let _ = fs::remove_dir_all(dir);
    }

    /// M1 acceptance 4: `!isatty` → conservative default instantly (no
    /// volley, no hang) and `--tier` still wins. Only meaningful when the
    /// test runner's stdio is not a terminal (always true on CI/pipes);
    /// skipped interactively so a unit test never writes a volley to a real
    /// terminal.
    #[test]
    fn non_tty_is_conservative_and_forced_tier_wins() {
        let tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1
            && unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1;
        if tty {
            return;
        }
        let caps = probe_caps(&ProbeOptions::default());
        assert_eq!(caps.color, ColorTier::C256, "conservative default tier");
        assert_eq!(caps.glyphs, GlyphFlags::ASCII, "conservative default glyphs");
        assert!(!caps.can_query);
        assert!(!caps.sync_2026);

        let forced = probe_caps(&ProbeOptions {
            forced_tier: Some(ColorTier::Mono),
            ..ProbeOptions::default()
        });
        assert_eq!(forced.color, ColorTier::Mono, "--tier overrides everything");
    }

    /// Regression (M1 review low 3, part 1): COLORTERM is part of the cache
    /// key — a COLORTERM-stripped run (pipe wrappers, sudo, some multiplexer
    /// launchers) stores under a *different* key and can never poison a
    /// later run where COLORTERM proves truecolor.
    #[test]
    fn cache_key_includes_colorterm() {
        let with = cache_key(&hints("xterm-kitty", "", "truecolor", ""));
        let without = cache_key(&hints("xterm-kitty", "", "", ""));
        assert_ne!(with, without, "stripped-COLORTERM run must use its own cache slot");

        // And the stripped run's entry is invisible to the truecolor run.
        let dir = std::env::temp_dir().join(format!("slpy-cache-ct-{}", std::process::id()));
        let opts = ProbeOptions { cache_dir: Some(dir.clone()), ..ProbeOptions::default() };
        cache_store(&opts, &without, &CacheEntry { color: ColorTier::C256, sync_2026: false });
        assert_eq!(cache_load(&opts, &with), None, "poisoned key must not hit");
        let _ = fs::remove_dir_all(dir);
    }

    /// Regression (M1 review low 3, part 2): a cache hit may only upgrade —
    /// it must never downgrade below what fresh passive evidence proves in
    /// THIS run.
    #[test]
    fn cache_hit_never_downgrades_fresh_passive_evidence() {
        // Fresh run proves truecolor via COLORTERM; stale hit says C256.
        let mut caps = passive(&hints("xterm-256color", "", "truecolor", "en_US.UTF-8"));
        assert_eq!(caps.color, ColorTier::True);
        apply_cache_hit(&mut caps, CacheEntry { color: ColorTier::C256, sync_2026: true });
        assert_eq!(caps.color, ColorTier::True, "cache hit must not downgrade");
        assert!(caps.sync_2026, "sync_2026 is cache-only knowledge");

        // Passive says C256; the cached volley result proved truecolor →
        // the upgrade path (the whole point of the cache) still works.
        let mut caps = passive(&hints("xterm-256color", "", "", "en_US.UTF-8"));
        assert_eq!(caps.color, ColorTier::C256);
        apply_cache_hit(&mut caps, CacheEntry { color: ColorTier::True, sync_2026: false });
        assert_eq!(caps.color, ColorTier::True);
        assert!(!caps.sync_2026);

        // Equal tiers: unchanged.
        let mut caps = passive(&hints("linux", "", "", "C"));
        apply_cache_hit(&mut caps, CacheEntry { color: ColorTier::C16, sync_2026: false });
        assert_eq!(caps.color, ColorTier::C16);
    }

    #[test]
    fn tier_rank_orders_all_tiers() {
        assert!(tier_rank(ColorTier::True) > tier_rank(ColorTier::C256));
        assert!(tier_rank(ColorTier::C256) > tier_rank(ColorTier::C16));
        assert!(tier_rank(ColorTier::C16) > tier_rank(ColorTier::Mono));
    }

    #[test]
    fn volley_is_one_write_with_da1_last() {
        assert!(VOLLEY.ends_with(b"\x1b[c"), "DA1 must be the sentinel (last)");
        let da1_at = VOLLEY.len() - 3;
        for probe in [&b"\x1b[>0q"[..], b"\x1b[?2026$p", b"\x1bP+q524742\x1b\\", b"\x1b[16t"] {
            let pos = VOLLEY
                .windows(probe.len())
                .position(|w| w == probe)
                .expect("query missing from volley");
            assert!(pos < da1_at);
        }
    }
}
