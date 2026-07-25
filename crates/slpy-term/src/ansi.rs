//! `AnsiBackend` — the one real terminal backend, parameterized by `Caps`
//! (PLAN §3.1). Writes to stdout; crossterm is used for raw mode / alt screen
//! / event polling ONLY, never per-cell commands (PLAN §8).

use std::io;
use std::mem::MaybeUninit;
use std::time::{Duration, Instant};

use crossterm::event::{Event as CtEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{cursor, terminal};
use slpy_core::{Cell, Grid};

use crate::backend::Backend;
use crate::caps::{Caps, ColorTier, FrameStats};
use crate::event::{Event, EventQueue, Key};
use crate::render::FramePainter;
use crate::restore;

/// ANSI escape-stream backend over stdout (PLAN §3.1).
///
/// Present path (PLAN §3.6 step 6): quantize to the caps color tier
/// (truecolor passthrough / xterm-256 / standard-16 / mono glyph-only),
/// per-row diff against the previous *quantized* grid, changed spans with
/// the skip-vs-move heuristic, SGR run-length elision, `?2026h…l` wrap when
/// `Caps::sync_2026`, single `write(2)` from a reused ≥64 KB buffer.
pub struct AnsiBackend {
    caps: Caps,
    events: EventQueue,
    /// Shared diff/assembly pipeline — identical code to `SimBackend`.
    painter: FramePainter,
    /// Session entered and not yet restored (`shutdown` flips this once).
    active: bool,
}

impl AnsiBackend {
    /// Enter the session (PLAN §3.1 session hygiene): arm the process-global
    /// restore state (fd + pre-raw termios), then raw mode, alt screen
    /// `?1049h`, hide cursor `?25l`, autowrap off `?7l` in one write.
    /// Installs the restore hooks itself (idempotent), so panic, SIGINT,
    /// SIGTERM and atexit always restore (M0 acceptance 3, pty-tested).
    ///
    /// The passed `caps.cells` is overridden by the real terminal size;
    /// `caps.cell_px` is filled from `TIOCGWINSZ` when the kernel reports
    /// pixel sizes (PLAN §3.2).
    ///
    /// Errors if stdout is not a TTY. All four color tiers are supported
    /// (M1); the tier lives in `caps.color`, normally from
    /// [`crate::probe_caps`].
    pub fn new(caps: Caps) -> io::Result<AnsiBackend> {
        let fd = libc::STDOUT_FILENO;
        if unsafe { libc::isatty(fd) } == 0 {
            return Err(io::Error::other(
                "stdout is not a TTY (AnsiBackend needs a terminal; use SimBackend headless)",
            ));
        }
        restore::install_restore_hooks();

        // Save cooked termios BEFORE any terminal mutation, then arm the
        // global restore state so there is no window where a signal leaves
        // the terminal hosed.
        let mut saved = MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, saved.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        restore::arm(fd, unsafe { saved.assume_init() });

        if let Err(err) = Self::enter(fd) {
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
            active: true,
        })
    }

    /// Raw mode via crossterm, then alt screen + hide cursor + autowrap off
    /// queued through crossterm's Commands and flushed as one write.
    fn enter(fd: libc::c_int) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        let mut seq: Vec<u8> = Vec::with_capacity(32);
        crossterm::queue!(
            seq,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            terminal::DisableLineWrap
        )?;
        write_all_fd(fd, &seq)
    }
}

/// Cell pixel size from `TIOCGWINSZ`, when the terminal reports pixel fields
/// (PLAN §3.2: drives cell aspect; fallback 2.0 handled by the caller).
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

/// Paint into the shared painter, then attempt the single `write(2)` to `fd`
/// (PLAN §3.6 step 6). A failed or partial write invalidates the painter:
/// `paint` has already committed the new grid as the diff baseline, so
/// without a forced full repaint the bytes the terminal never received would
/// never be re-emitted — in `--repaint diff` mode the screen would stay
/// stale/garbled until the next resize. Invalidate-on-failure makes the next
/// `present` self-healing in every mode; the drop is still reported via
/// `FrameStats::dropped`.
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

/// Loop `write(2)` handling EINTR and partial writes. One logical write per
/// frame (PLAN §3.6 step 6); the kernel may still split it. Shared with the
/// probe volley (one-write, PLAN §3.1).
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

/// crossterm event → engine event (crossterm types never leak, PLAN §8).
/// q / Esc / Ctrl-C map to `Quit`; other keys pass through; crossterm
/// surfaces SIGWINCH as `Resize` on unix.
fn map_event(ev: CtEvent) -> Option<Event> {
    match ev {
        CtEvent::Resize(cols, rows) => Some(Event::Resize(cols, rows)),
        CtEvent::Key(k) => {
            if k.kind == KeyEventKind::Release {
                return None;
            }
            match k.code {
                KeyCode::Esc => Some(Event::Quit),
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
    /// then hands it to the caller (PLAN §3.6 step 1).
    fn events(&mut self) -> &mut EventQueue {
        while let Ok(true) = crossterm::event::poll(Duration::ZERO) {
            match crossterm::event::read() {
                Ok(ev) => {
                    if let Some(mapped) = map_event(ev) {
                        self.events.push(mapped);
                    }
                }
                Err(_) => break,
            }
        }
        &mut self.events
    }

    /// Diff → spans → SGR elide → ONE write (PLAN §3.1, §3.6 step 6). On a
    /// failed/partial write the painter is invalidated so the next frame is a
    /// full repaint — a dropped frame must not poison the diff baseline.
    fn present(&mut self, grid: &Grid<Cell>) -> FrameStats {
        present_to_fd(
            &mut self.painter,
            grid,
            self.caps.color,
            self.caps.sync_2026,
            libc::STDOUT_FILENO,
        )
    }

    fn invalidate(&mut self) {
        self.painter.invalidate();
    }

    /// The ONLY allocation point in the hot path (PLAN §3.1).
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

    /// Regression (review finding, --repaint diff poisoning): a failed write
    /// in `present` must not leave the diff baseline claiming the frame
    /// reached the screen. `paint` commits `prev` before the write, so a
    /// dropped frame has to invalidate the painter — otherwise the dropped
    /// cells are never re-emitted and diff mode shows stale content until a
    /// resize. Forces the failure with a full O_NONBLOCK pipe (the classic
    /// shared-tty flag-leak scenario).
    #[test]
    fn dropped_frame_invalidates_diff_baseline() {
        use slpy_core::Rgb;

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

        // Frame 1: pipe has room — full first paint succeeds.
        let mut painter = FramePainter::new(8, 3);
        let first = present_to_fd(&mut painter, &grid, ColorTier::True, false, wr);
        assert!(!first.dropped);
        assert_eq!(first.cells_damaged, 24);
        drain(rd);

        // Fill the pipe to capacity so the next write fails with EAGAIN.
        let junk = [b'x'; 4096];
        loop {
            let n = unsafe { libc::write(wr, junk.as_ptr().cast(), junk.len()) };
            if n <= 0 {
                break;
            }
        }
        while unsafe { libc::write(wr, junk.as_ptr().cast(), 1) } == 1 {}

        // Frame 2: one changed cell, write fails → dropped, baseline must
        // not pretend the cell reached the screen.
        grid.set(3, 1, cell('@', 250));
        let dropped = present_to_fd(&mut painter, &grid, ColorTier::True, false, wr);
        assert!(dropped.dropped, "full pipe must report a dropped frame");
        assert_eq!(dropped.cells_damaged, 1);

        // Frame 3: pipe drained, SAME grid presented again. Without the
        // invalidate-on-drop fix this diffs against the poisoned baseline and
        // emits nothing; with it, the whole frame is re-emitted.
        drain(rd);
        let heal = present_to_fd(&mut painter, &grid, ColorTier::True, false, wr);
        assert!(!heal.dropped);
        assert_eq!(
            heal.cells_damaged, 24,
            "present after a dropped frame must be a full repaint, not a no-op diff"
        );
        assert!(heal.bytes > 0);

        // And the baseline is healthy again: an unchanged frame is a no-op.
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

    #[test]
    fn non_tty_stdout_rejected() {
        // Only meaningful when the test runner's stdout is not a terminal
        // (always true on CI/pipes); skip interactively so we never enter a
        // real session from a unit test.
        if unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1 {
            return;
        }
        assert!(AnsiBackend::new(Caps::default()).is_err());
    }
}
