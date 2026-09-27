//! `AnsiBackend` — the one real terminal backend, parameterized by `Caps`.
//! Writes to stdout; crossterm is used for raw mode / alt screen / event
//! polling ONLY, never per-cell commands.

use std::io;
#[cfg(unix)]
use std::mem::MaybeUninit;
use std::time::{Duration, Instant};

use crossterm::event::{Event as CtEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{cursor, terminal};
use auto_ascii_core::{Cell, Grid};

use crate::backend::Backend;
use crate::caps::{Caps, ColorTier, FrameStats};
use crate::event::{Event, EventQueue, Key};
use crate::probe;
use crate::render::FramePainter;
use crate::restore;

const STRAGGLER_QUIET: Duration = Duration::from_millis(150);
const ESC_HOLD: Duration = STRAGGLER_QUIET;

struct StragglerFilter {
    armed: bool,
    in_reply: bool,
    last_discard: Instant,
    pending_esc: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Filtered {
    flush_esc: bool,
    discard: bool,
}

impl Filtered {
    const PASS: Filtered = Filtered { flush_esc: false, discard: false };
}

impl StragglerFilter {
    fn new(armed: bool, now: Instant) -> StragglerFilter {
        StragglerFilter { armed, in_reply: false, last_discard: now, pending_esc: None }
    }

    fn filter(&mut self, ev: &CtEvent, now: Instant) -> Filtered {
        if !self.armed {
            return Filtered::PASS;
        }
        let CtEvent::Key(k) = ev else { return Filtered::PASS };
        if k.kind == KeyEventKind::Release {
            return Filtered::PASS;
        }
        let mut flush_esc = false;
        if let Some(t0) = self.pending_esc
            && now.duration_since(t0) > ESC_HOLD
        {
            self.pending_esc = None;
            flush_esc = true;
        }
        if self.in_reply {
            if now.duration_since(self.last_discard) > STRAGGLER_QUIET {
                self.in_reply = false;
            } else {
                self.last_discard = now;
                if k.modifiers.contains(KeyModifiers::ALT) && k.code == KeyCode::Char('\\') {
                    self.in_reply = false;
                }
                return Filtered { flush_esc, discard: true };
            }
        }
        if self.pending_esc.take().is_some() {
            if matches!(k.code, KeyCode::Char('P' | 'p' | '[')) {
                self.in_reply = true;
                self.last_discard = now;
                return Filtered { flush_esc, discard: true };
            }
            flush_esc = true;
        }
        if k.modifiers.contains(KeyModifiers::ALT) && matches!(k.code, KeyCode::Char('P' | 'p'))
        {
            self.in_reply = true;
            self.last_discard = now;
            return Filtered { flush_esc, discard: true };
        }
        if k.code == KeyCode::Esc {
            self.pending_esc = Some(now);
            return Filtered { flush_esc, discard: true };
        }
        Filtered { flush_esc, discard: false }
    }

    fn take_expired_esc(&mut self, now: Instant) -> bool {
        match self.pending_esc {
            Some(t0) if now.duration_since(t0) > ESC_HOLD => {
                self.pending_esc = None;
                true
            }
            _ => false,
        }
    }
}

/// ANSI escape-stream backend over stdout.
///
/// Present path: quantize to the caps color tier (truecolor passthrough /
/// xterm-256 / standard-16 / mono glyph-only), per-row diff against the
/// previous *quantized* grid, changed spans with the skip-vs-move heuristic,
/// SGR run-length elision, `?2026h…l` wrap when `Caps::sync_2026`, single
/// `write(2)` from a reused ≥64 KB buffer.
pub struct AnsiBackend {
    caps: Caps,
    events: EventQueue,
    painter: FramePainter,
    straggler: StragglerFilter,
    active: bool,
}

impl AnsiBackend {
    /// Enter the session: arm the process-global restore state (fd + pre-raw
    /// termios), then raw mode, alt screen `?1049h`, hide cursor `?25l`,
    /// autowrap off `?7l` in one write. Installs the restore hooks itself
    /// (idempotent), so panic, SIGINT, SIGTERM, SIGHUP and atexit always restore
    /// (pty-tested).
    ///
    /// The passed `caps.cells` is overridden by the real terminal size;
    /// `caps.cell_px` is filled from `TIOCGWINSZ` when the kernel reports
    /// pixel sizes.
    ///
    /// Errors if stdout is not a TTY. All four color tiers are supported; the
    /// tier lives in `caps.color`, normally from [`crate::probe_caps`].
    pub fn new(caps: Caps) -> io::Result<AnsiBackend> {
        Self::with_backdrop(caps, false)
    }

    /// [`new`](AnsiBackend::new), and with `backdrop` also set the terminal's
    /// default background to black for the session: [`crate::BACKDROP_SET`]
    /// right after the alt-screen enter, [`crate::BACKDROP_RESET`] on every
    /// restore path (orderly, Rust panic, SIGINT, SIGTERM, SIGHUP, atexit),
    /// exactly once. Cells that paint their own background look the same
    /// either way; cells that keep the terminal's (SGR 49) sit on black
    /// instead of the theme. Never set on [`ColorTier::Mono`]: nothing there
    /// paints a foreground, so the terminal's default one (black on a light
    /// theme) would vanish on black.
    #[cfg(unix)]
    pub fn with_backdrop(caps: Caps, backdrop: bool) -> io::Result<AnsiBackend> {
        let backdrop = backdrop && caps.color != ColorTier::Mono;
        let fd = libc::STDOUT_FILENO;
        if unsafe { libc::isatty(fd) } == 0 {
            return Err(io::Error::other(
                "stdout is not a TTY (AnsiBackend needs a terminal; use SimBackend headless)",
            ));
        }
        restore::install_restore_hooks();

        let mut saved = MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, saved.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        restore::arm(fd, unsafe { saved.assume_init() }, backdrop);

        if let Err(err) = Self::enter(fd, backdrop) {
            restore::restore_now();
            let _ = terminal::disable_raw_mode();
            return Err(err);
        }

        let (cols, rows) = terminal::size().unwrap_or(caps.cells);
        let mut caps = caps;
        caps.cells = (cols, rows);
        if caps.cell_px.is_none() {
            caps.cell_px = query_cell_px(fd);
        }

        Ok(AnsiBackend {
            caps,
            events: EventQueue::new(),
            painter: FramePainter::new(cols, rows),
            straggler: StragglerFilter::new(probe::volley_stragglers_possible(), Instant::now()),
            active: true,
        })
    }

    /// Windows session entry (compiled for the windows-gnu cross build, not
    /// tested on Windows). Same hygiene, different plumbing: crossterm owns
    /// raw mode and executes the enter-session commands through its
    /// ANSI-or-WinAPI layer (which also enables VT output processing on
    /// conhost), the restore path is the panic hook + orderly shutdown/Drop
    /// ([`crate::restore`], windows half), and frame bytes go to stdout via
    /// `std::io::Write`.
    ///
    /// Never probed: the caller's `caps` arrive passive-only
    /// ([`crate::probe_caps`] writes no volley on Windows), so `cell_px`
    /// stays `None` (aspect falls back to 2.0).
    #[cfg(windows)]
    pub fn with_backdrop(caps: Caps, backdrop: bool) -> io::Result<AnsiBackend> {
        let backdrop = backdrop && caps.color != ColorTier::Mono;
        use crossterm::tty::IsTty as _;
        if !io::stdout().is_tty() {
            return Err(io::Error::other(
                "stdout is not a TTY (AnsiBackend needs a terminal; use SimBackend headless)",
            ));
        }
        restore::install_restore_hooks();
        terminal::enable_raw_mode()?;
        restore::arm(backdrop);
        if let Err(err) = crossterm::execute!(
            io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide,
            terminal::DisableLineWrap
        )
        .and_then(|()| {
            use std::io::Write as _;
            let mut out = io::stdout();
            if backdrop {
                out.write_all(restore::BACKDROP_SET)?;
            }
            out.flush()
        }) {
            restore::restore_now();
            return Err(err);
        }

        let (cols, rows) = terminal::size().unwrap_or(caps.cells);
        let mut caps = caps;
        caps.cells = (cols, rows);

        Ok(AnsiBackend {
            caps,
            events: EventQueue::new(),
            painter: FramePainter::new(cols, rows),
            straggler: StragglerFilter::new(probe::volley_stragglers_possible(), Instant::now()),
            active: true,
        })
    }

    #[cfg(unix)]
    fn enter(fd: libc::c_int, backdrop: bool) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        let mut seq: Vec<u8> = Vec::with_capacity(64);
        crossterm::queue!(
            seq,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            terminal::DisableLineWrap
        )?;
        if backdrop {
            seq.extend_from_slice(restore::BACKDROP_SET);
        }
        write_all_fd(fd, &seq)
    }
}

#[cfg(unix)]
fn query_cell_px(fd: libc::c_int) -> Option<(u16, u16)> {
    let mut ws = MaybeUninit::<libc::winsize>::uninit();
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, ws.as_mut_ptr()) } != 0 {
        return None;
    }
    let ws = unsafe { ws.assume_init() };
    if ws.ws_col == 0 || ws.ws_row == 0 || ws.ws_xpixel == 0 || ws.ws_ypixel == 0 {
        return None;
    }
    Some((ws.ws_xpixel / ws.ws_col, ws.ws_ypixel / ws.ws_row))
}

#[cfg(unix)]
fn present_to_fd(
    painter: &mut FramePainter,
    grid: &Grid<Cell>,
    tier: ColorTier,
    sync_2026: bool,
    fd: libc::c_int,
) -> FrameStats {
    let cells_damaged = painter.paint(grid, tier, sync_2026);
    let frame = &painter.buf;
    let start = Instant::now();
    let ok = frame.is_empty() || write_all_fd(fd, frame).is_ok();
    let stats = FrameStats {
        bytes: frame.len() as u32,
        cells_damaged,
        write_ns: start.elapsed().as_nanos() as u64,
        dropped: !ok,
    };
    if !ok {
        painter.invalidate();
    }
    stats
}

#[cfg(windows)]
fn present_to_stdout(
    painter: &mut FramePainter,
    grid: &Grid<Cell>,
    tier: ColorTier,
    sync_2026: bool,
) -> FrameStats {
    use std::io::Write as _;
    let cells_damaged = painter.paint(grid, tier, sync_2026);
    let frame = &painter.buf;
    let start = Instant::now();
    let mut out = io::stdout().lock();
    let ok = frame.is_empty() || out.write_all(frame).and_then(|()| out.flush()).is_ok();
    let stats = FrameStats {
        bytes: frame.len() as u32,
        cells_damaged,
        write_ns: start.elapsed().as_nanos() as u64,
        dropped: !ok,
    };
    if !ok {
        painter.invalidate();
    }
    stats
}

#[cfg(unix)]
pub(crate) fn write_all_fd(fd: libc::c_int, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

fn map_event(ev: CtEvent) -> Option<Event> {
    match ev {
        CtEvent::Resize(cols, rows) => Some(Event::Resize(cols, rows)),
        CtEvent::Key(k) => {
            if k.kind == KeyEventKind::Release {
                return None;
            }
            match k.code {
                KeyCode::Esc => Some(Event::Quit),
                KeyCode::Left => Some(Event::Key(Key::Left)),
                KeyCode::Right => Some(Event::Key(Key::Right)),
                KeyCode::Char(c) => {
                    let lower = c.to_ascii_lowercase();
                    if k.modifiers.contains(KeyModifiers::CONTROL) {
                        if lower == 'c' {
                            Some(Event::Quit)
                        } else {
                            Some(Event::Key(Key::Ctrl(lower)))
                        }
                    } else if lower == 'q' {
                        Some(Event::Quit)
                    } else {
                        Some(Event::Key(Key::Char(c)))
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

impl Backend for AnsiBackend {
    fn caps(&self) -> &Caps {
        &self.caps
    }

    /// Pumps all pending crossterm events (zero-timeout poll) into the queue,
    /// then hands it to the caller. Late probe-reply fragments are dropped by
    /// the straggler filter before mapping — probe bytes must never become
    /// key events, no matter how late or how split across reads they arrive.
    fn events(&mut self) -> &mut EventQueue {
        while let Ok(true) = crossterm::event::poll(Duration::ZERO) {
            match crossterm::event::read() {
                Ok(ev) => {
                    let verdict = self.straggler.filter(&ev, Instant::now());
                    if verdict.flush_esc {
                        self.events.push(Event::Quit);
                    }
                    if verdict.discard {
                        continue;
                    }
                    if let Some(mapped) = map_event(ev) {
                        self.events.push(mapped);
                    }
                }
                Err(_) => break,
            }
        }
        if self.straggler.take_expired_esc(Instant::now()) {
            self.events.push(Event::Quit);
        }
        &mut self.events
    }

    /// Diff → spans → SGR elide → ONE write. On a failed/partial write the
    /// painter is invalidated so the next frame is a full repaint — a dropped
    /// frame must not poison the diff baseline.
    #[cfg(unix)]
    fn present(&mut self, grid: &Grid<Cell>) -> FrameStats {
        present_to_fd(
            &mut self.painter,
            grid,
            self.caps.color,
            self.caps.sync_2026,
            libc::STDOUT_FILENO,
        )
    }

    /// Windows present: same painter, frame bytes through `std::io::Write`
    /// on stdout (VT processing was enabled by crossterm at session entry).
    #[cfg(windows)]
    fn present(&mut self, grid: &Grid<Cell>) -> FrameStats {
        present_to_stdout(&mut self.painter, grid, self.caps.color, self.caps.sync_2026)
    }

    fn invalidate(&mut self) {
        self.painter.invalidate();
    }

    /// The ONLY allocation point in the hot path.
    fn resize(&mut self, cols: u16, rows: u16) {
        self.caps.cells = (cols, rows);
        self.painter.resize(cols, rows);
    }

    /// Restore terminal (also invoked from `Drop` and the signal path).
    /// `restore_now` atomically consumes the armed session, so the restore
    /// bytes are emitted exactly once no matter how many paths fire.
    fn shutdown(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        restore::restore_now();
        let _ = terminal::disable_raw_mode();
    }
}

impl Drop for AnsiBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn quit_keys_map_to_quit() {
        for ev in [
            CtEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            CtEvent::Key(KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::NONE)),
            CtEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            CtEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        ] {
            assert_eq!(map_event(ev), Some(Event::Quit));
        }
    }

    #[test]
    fn other_keys_pass_through() {
        assert_eq!(
            map_event(CtEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))),
            Some(Event::Key(Key::Char('x')))
        );
        assert_eq!(
            map_event(CtEvent::Key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL))),
            Some(Event::Key(Key::Ctrl('z')))
        );
        assert_eq!(map_event(CtEvent::Resize(213, 58)), Some(Event::Resize(213, 58)));
        assert_eq!(
            map_event(CtEvent::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))),
            Some(Event::Key(Key::Left))
        );
        assert_eq!(
            map_event(CtEvent::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))),
            Some(Event::Key(Key::Right))
        );
        assert_eq!(map_event(CtEvent::FocusGained), None);
        assert_eq!(
            map_event(CtEvent::Key(KeyEvent::new_with_kind(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ))),
            None
        );
    }

    #[test]
    fn dropped_frame_invalidates_diff_baseline() {
        use auto_ascii_core::Rgb;

        unsafe fn set_nonblock(fd: libc::c_int) {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            assert!(flags >= 0);
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) }, 0);
        }

        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (rd, wr) = (fds[0], fds[1]);
        unsafe {
            set_nonblock(rd);
            set_nonblock(wr);
        }
        let drain = |rd: libc::c_int| {
            let mut sink = [0u8; 4096];
            while unsafe { libc::read(rd, sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
        };

        let cell = |ch: char, v: u8| Cell::new(ch, Rgb::gray(v), Rgb::BLACK);
        let mut grid: Grid<Cell> = Grid::new(8, 3);
        grid.fill(cell('.', 128));

        let mut painter = FramePainter::new(8, 3);
        let first = present_to_fd(&mut painter, &grid, ColorTier::True, false, wr);
        assert!(!first.dropped);
        assert_eq!(first.cells_damaged, 24);
        drain(rd);

        let junk = [b'x'; 4096];
        loop {
            let n = unsafe { libc::write(wr, junk.as_ptr().cast(), junk.len()) };
            if n <= 0 {
                break;
            }
        }
        while unsafe { libc::write(wr, junk.as_ptr().cast(), 1) } == 1 {}

        grid.set(3, 1, cell('@', 250));
        let dropped = present_to_fd(&mut painter, &grid, ColorTier::True, false, wr);
        assert!(dropped.dropped, "full pipe must report a dropped frame");
        assert_eq!(dropped.cells_damaged, 1);

        drain(rd);
        let heal = present_to_fd(&mut painter, &grid, ColorTier::True, false, wr);
        assert!(!heal.dropped);
        assert_eq!(
            heal.cells_damaged, 24,
            "present after a dropped frame must be a full repaint, not a no-op diff"
        );
        assert!(heal.bytes > 0);

        drain(rd);
        let idle = present_to_fd(&mut painter, &grid, ColorTier::True, false, wr);
        assert_eq!(idle.cells_damaged, 0);
        assert_eq!(idle.bytes, 0);
        assert!(!idle.dropped);

        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
    }

    fn dcs_fragment_events(payload: &str) -> Vec<CtEvent> {
        let mut evs = vec![CtEvent::Key(KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ))];
        evs.extend(payload.chars().map(|c| {
            let m = if c.is_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
            CtEvent::Key(KeyEvent::new(KeyCode::Char(c), m))
        }));
        evs.push(CtEvent::Key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT)));
        evs
    }

    fn discarded(f: &mut StragglerFilter, ev: &CtEvent, now: std::time::Instant) -> bool {
        let v = f.filter(ev, now);
        assert!(!v.flush_esc, "unexpected Esc flush for {ev:?}");
        v.discard
    }

    #[test]
    fn straggler_filter_discards_late_dcs_reply_fragments() {
        let t0 = std::time::Instant::now();
        let mut f = StragglerFilter::new(true, t0);

        let mut now = t0 + Duration::from_millis(300);
        for ev in dcs_fragment_events(">|kitty(0.32.2)") {
            assert!(discarded(&mut f, &ev, now), "reply fragment must be discarded: {ev:?}");
            now += Duration::from_millis(1);
        }

        for ev in dcs_fragment_events("1+r524742=38") {
            assert!(discarded(&mut f, &ev, now), "reply fragment must be discarded: {ev:?}");
            now += Duration::from_millis(1);
        }

        let x = CtEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!discarded(&mut f, &x, now + Duration::from_millis(400)));
    }

    #[test]
    fn straggler_filter_quiet_gap_ends_unterminated_fragment() {
        let t0 = std::time::Instant::now();
        let mut f = StragglerFilter::new(true, t0);
        let alt_p = CtEvent::Key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::ALT));
        let five = CtEvent::Key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE));

        let now = t0 + Duration::from_millis(100);
        assert!(discarded(&mut f, &alt_p, now));
        assert!(discarded(&mut f, &five, now + Duration::from_millis(10)), "payload digit eaten");
        assert!(!discarded(&mut f, &five, now + Duration::from_millis(500)));
    }

    #[test]
    fn straggler_filter_swallows_split_esc_p_burst() {
        let t0 = std::time::Instant::now();
        let mut f = StragglerFilter::new(true, t0);
        let esc = CtEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        let mut now = t0 + Duration::from_millis(300);
        assert!(discarded(&mut f, &esc, now));

        now += Duration::from_millis(60);
        for c in "P>|kitty(0.32.2)".chars() {
            let m = if c.is_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
            let ev = CtEvent::Key(KeyEvent::new(KeyCode::Char(c), m));
            assert!(discarded(&mut f, &ev, now), "split-burst payload must be eaten: {c:?}");
            now += Duration::from_millis(1);
        }
        assert!(discarded(&mut f, &esc, now + Duration::from_millis(1)));
        let bs = CtEvent::Key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::NONE));
        assert!(discarded(&mut f, &bs, now + Duration::from_millis(2)));
        assert!(!f.take_expired_esc(now + Duration::from_secs(5)), "ST Esc must not flush");

        let x = CtEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!discarded(&mut f, &x, now + Duration::from_millis(400)));
    }

    #[test]
    fn straggler_filter_swallows_split_csi_reply() {
        let t0 = std::time::Instant::now();
        let mut f = StragglerFilter::new(true, t0);
        let esc = CtEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        let mut now = t0 + Duration::from_millis(200);
        assert!(discarded(&mut f, &esc, now));
        now += Duration::from_millis(40);
        for c in "[?62;c".chars() {
            let ev = CtEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            assert!(discarded(&mut f, &ev, now), "split CSI payload must be eaten: {c:?}");
            now += Duration::from_millis(1);
        }
        assert!(!f.take_expired_esc(now + Duration::from_secs(5)), "reply Esc must not flush");
        let five = CtEvent::Key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE));
        assert!(!discarded(&mut f, &five, now + Duration::from_millis(400)));
    }

    #[test]
    fn straggler_filter_flushes_real_esc() {
        let t0 = std::time::Instant::now();
        let esc = CtEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let x = CtEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        let mut f = StragglerFilter::new(true, t0);
        assert!(discarded(&mut f, &esc, t0 + Duration::from_millis(10)));
        assert!(!f.take_expired_esc(t0 + Duration::from_millis(100)), "still inside the hold");
        assert!(f.take_expired_esc(t0 + Duration::from_millis(200)));
        assert!(!f.take_expired_esc(t0 + Duration::from_millis(201)), "flushes exactly once");

        let mut f = StragglerFilter::new(true, t0);
        assert!(discarded(&mut f, &esc, t0 + Duration::from_millis(10)));
        let v = f.filter(&x, t0 + Duration::from_millis(50));
        assert_eq!(v, Filtered { flush_esc: true, discard: false });
    }

    #[test]
    fn straggler_filter_scope_is_armed_sessions_for_life() {
        let t0 = std::time::Instant::now();
        let alt_p = CtEvent::Key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::ALT));
        let digit = CtEvent::Key(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::NONE));

        let mut off = StragglerFilter::new(false, t0);
        assert!(!discarded(&mut off, &alt_p, t0 + Duration::from_millis(10)));
        let esc = CtEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!discarded(&mut off, &esc, t0 + Duration::from_millis(11)));

        let mut f = StragglerFilter::new(true, t0);
        let now = t0 + Duration::from_millis(50);
        assert!(discarded(&mut f, &alt_p, now));
        assert!(!discarded(&mut f, &CtEvent::Resize(100, 40), now + Duration::from_millis(1)));

        let mut f = StragglerFilter::new(true, t0);
        let late = t0 + Duration::from_secs(600);
        assert!(discarded(&mut f, &alt_p, late));
        assert!(discarded(&mut f, &digit, late + Duration::from_millis(1)));
        assert!(!discarded(&mut f, &digit, late + Duration::from_millis(400)));
    }

    #[test]
    fn non_tty_stdout_rejected() {
        if unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1 {
            return;
        }
        assert!(AnsiBackend::new(Caps::default()).is_err());
    }
}
