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
use crate::probe;
use crate::render::FramePainter;
use crate::restore;

/// A reply burst is contiguous; once the input has been quiet this long the
/// fragment is over and real keys pass again.
const STRAGGLER_QUIET: Duration = Duration::from_millis(150);
/// How long a lone ESC is held back waiting for a split DCS intro (the
/// terminal's `ESC` and `P…` landing in separate reads) before it is
/// flushed as real input. Costs armed sessions ≤ this much Esc-quit
/// latency — imperceptible, and armed sessions are the degraded-probe
/// exception, never the steady state.
const ESC_HOLD: Duration = STRAGGLER_QUIET;

/// Filters late probe-reply fragments out of the interactive event stream
/// (M1 review low 2; hardened at M3 for the M2-low findings). Armed only
/// when [`probe::volley_stragglers_possible`] says the volley timed out
/// without its DA1 sentinel — and then for the WHOLE session: a reply from
/// a slow terminal/mux chain can land seconds later, and the old 2 s
/// disarm window let such bursts surface as key events (digits = the 0–9
/// seek bindings). Shape recognition is cheap, so armed sessions keep it.
///
/// Why events, not bytes: crossterm owns stdin once the session starts. It
/// silently absorbs straggling CSI replies (unknown finals error out its
/// parser; `CSI ? … c` becomes an internal PrimaryDeviceAttributes event),
/// but a DCS reply — XTVERSION `ESC P > | text ST`, XTGETTCAP
/// `ESC P 1 + r … ST` — is tokenized as Alt+P, then the payload as PLAIN
/// CHARACTER KEYS, then Alt+`\`. When the burst is SPLIT across reads the
/// intro degrades further: a lone `ESC` (tokenized as the Esc key — which
/// maps to Quit!) followed by a plain `P`. This state machine recognizes
/// both shapes: Alt+P — or a briefly-held Esc completed by `P` — opens a
/// discard span that ends at the ST (`Alt+\`) or after a quiet gap. A held
/// Esc not completed by `P` is flushed as real input (quit still works).
struct StragglerFilter {
    /// Shape filtering active (session-long); `false` = disarmed fast path.
    armed: bool,
    /// Inside a DCS reply fragment — discard key events until ST/quiet gap.
    in_reply: bool,
    last_discard: Instant,
    /// Lone ESC held back (split-intro candidate) since this instant.
    pending_esc: Option<Instant>,
}

/// What [`StragglerFilter::filter`] decided for one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Filtered {
    /// A previously-held lone ESC turned out to be real input — deliver an
    /// Esc key (→ Quit) BEFORE handling the current event.
    flush_esc: bool,
    /// The current event is a reply fragment (or a held ESC): swallow it.
    discard: bool,
}

impl Filtered {
    const PASS: Filtered = Filtered { flush_esc: false, discard: false };
}

impl StragglerFilter {
    fn new(armed: bool, now: Instant) -> StragglerFilter {
        StragglerFilter { armed, in_reply: false, last_discard: now, pending_esc: None }
    }

    /// Classify one crossterm event. Non-key events and disarmed sessions
    /// always pass untouched (a mid-fragment SIGWINCH must never be lost).
    fn filter(&mut self, ev: &CtEvent, now: Instant) -> Filtered {
        if !self.armed {
            return Filtered::PASS;
        }
        let CtEvent::Key(k) = ev else { return Filtered::PASS };
        if k.kind == KeyEventKind::Release {
            return Filtered::PASS;
        }
        // A held ESC past its hold window is real input regardless of what
        // this event turns out to be.
        let mut flush_esc = false;
        if let Some(t0) = self.pending_esc
            && now.duration_since(t0) > ESC_HOLD
        {
            self.pending_esc = None;
            flush_esc = true;
        }
        if self.in_reply {
            if now.duration_since(self.last_discard) > STRAGGLER_QUIET {
                self.in_reply = false; // burst over; classify afresh below
            } else {
                self.last_discard = now;
                // ST (ESC \ → Alt+'\') closes the fragment; an ESC-split ST
                // surfaces as Esc + '\' — both swallowed here, the quiet
                // gap closes the span either way.
                if k.modifiers.contains(KeyModifiers::ALT) && k.code == KeyCode::Char('\\') {
                    self.in_reply = false;
                }
                return Filtered { flush_esc, discard: true };
            }
        }
        if self.pending_esc.take().is_some() {
            // Fresh held ESC: a following `P` (crossterm gives it SHIFT,
            // not ALT — the ESC byte was consumed separately) completes a
            // split DCS intro, `[` a split CSI reply (crossterm absorbs
            // CSI replies only when the whole sequence lands in one read —
            // split, the payload surfaces as plain chars). Anything else
            // means the ESC was a real key. CSI replies have no ST; the
            // quiet gap closes those spans.
            if matches!(k.code, KeyCode::Char('P' | 'p' | '[')) {
                self.in_reply = true;
                self.last_discard = now;
                return Filtered { flush_esc, discard: true };
            }
            flush_esc = true;
        }
        // Single-read DCS intro: crossterm tokenizes `ESC P` as Alt+P.
        if k.modifiers.contains(KeyModifiers::ALT) && matches!(k.code, KeyCode::Char('P' | 'p'))
        {
            self.in_reply = true;
            self.last_discard = now;
            return Filtered { flush_esc, discard: true };
        }
        // Lone ESC: hold it briefly — it may be the split intro's first
        // half. `take_expired_esc` (or the next event) resolves it.
        if k.code == KeyCode::Esc {
            self.pending_esc = Some(now);
            return Filtered { flush_esc, discard: true };
        }
        Filtered { flush_esc, discard: false }
    }

    /// True once per expired hold: the held ESC was real input after all —
    /// the caller must deliver it now. Poll this every pump even when no
    /// events arrive (an Esc-quit must not wait for the next keystroke).
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
    /// Late-probe-reply event filter (M1 review low 2).
    straggler: StragglerFilter,
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
            // Armed iff the probe volley timed out without its DA1 sentinel
            // this run — late reply bytes may still hit stdin (review low 2).
            straggler: StragglerFilter::new(probe::volley_stragglers_possible(), Instant::now()),
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
    /// then hands it to the caller (PLAN §3.6 step 1). Late probe-reply
    /// fragments are dropped by the straggler filter before mapping — probe
    /// bytes must never become key events (M1 review low 2), no matter how
    /// late or how split across reads they arrive (M2 low fix a).
    fn events(&mut self) -> &mut EventQueue {
        while let Ok(true) = crossterm::event::poll(Duration::ZERO) {
            match crossterm::event::read() {
                Ok(ev) => {
                    let verdict = self.straggler.filter(&ev, Instant::now());
                    if verdict.flush_esc {
                        // A held lone ESC proved real: deliver it first,
                        // with `map_event`'s Esc semantics (Quit).
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
        // A held ESC whose hold window lapsed with no follow-up is a real
        // Esc keypress — deliver it even on an otherwise idle pump.
        if self.straggler.take_expired_esc(Instant::now()) {
            self.events.push(Event::Quit);
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

    /// Exactly how crossterm tokenizes a straggling DCS reply: `ESC P` →
    /// Alt(+Shift)+P, payload → plain char keys, `ESC \` → Alt+'\'.
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

    /// Shorthand verdicts.
    fn discarded(f: &mut StragglerFilter, ev: &CtEvent, now: std::time::Instant) -> bool {
        let v = f.filter(ev, now);
        assert!(!v.flush_esc, "unexpected Esc flush for {ev:?}");
        v.discard
    }

    /// Regression (M1 review low 2): a late XTVERSION/XTGETTCAP reply —
    /// which crossterm surfaces as Alt+P + plain chars (digits = the 0–9
    /// seek bindings!) + Alt+'\' — must be discarded wholesale, while a real
    /// key after a quiet gap still passes.
    #[test]
    fn straggler_filter_discards_late_dcs_reply_fragments() {
        let t0 = std::time::Instant::now();
        let mut f = StragglerFilter::new(true, t0);

        // Whole burst arrives "at once" (same pump): every event discarded.
        let mut now = t0 + Duration::from_millis(300);
        for ev in dcs_fragment_events(">|kitty(0.32.2)") {
            assert!(discarded(&mut f, &ev, now), "reply fragment must be discarded: {ev:?}");
            now += Duration::from_millis(1);
        }

        // A second fragment (XTGETTCAP reply, digits everywhere) too.
        for ev in dcs_fragment_events("1+r524742=38") {
            assert!(discarded(&mut f, &ev, now), "reply fragment must be discarded: {ev:?}");
            now += Duration::from_millis(1);
        }

        // Real playback key AFTER a quiet gap: passes.
        let x = CtEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!discarded(&mut f, &x, now + Duration::from_millis(400)));
    }

    /// The quiet gap ends an unterminated fragment (reply cut mid-payload —
    /// e.g. the ST never arrives): user keys must not be eaten forever.
    #[test]
    fn straggler_filter_quiet_gap_ends_unterminated_fragment() {
        let t0 = std::time::Instant::now();
        let mut f = StragglerFilter::new(true, t0);
        let alt_p = CtEvent::Key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::ALT));
        let five = CtEvent::Key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE));

        let now = t0 + Duration::from_millis(100);
        assert!(discarded(&mut f, &alt_p, now));
        assert!(discarded(&mut f, &five, now + Duration::from_millis(10)), "payload digit eaten");
        // No ST ever arrives; after the quiet gap the same digit is real input.
        assert!(!discarded(&mut f, &five, now + Duration::from_millis(500)));
    }

    /// M2 low fix a, part 1: the SPLIT burst — the terminal's `ESC` and the
    /// `P…` payload land in separate reads, so crossterm tokenizes the intro
    /// as the Esc KEY (which maps to Quit!) followed by plain chars. The
    /// held ESC must not surface (no spurious quit), the payload must be
    /// discarded, and a real key after the quiet gap still passes.
    #[test]
    fn straggler_filter_swallows_split_esc_p_burst() {
        let t0 = std::time::Instant::now();
        let mut f = StragglerFilter::new(true, t0);
        let esc = CtEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // Lone ESC: held (discarded for now, no flush).
        let mut now = t0 + Duration::from_millis(300);
        assert!(discarded(&mut f, &esc, now));

        // 'P' + payload 60 ms later (next read): completes the DCS intro.
        now += Duration::from_millis(60);
        for c in "P>|kitty(0.32.2)".chars() {
            let m = if c.is_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
            let ev = CtEvent::Key(KeyEvent::new(KeyCode::Char(c), m));
            assert!(discarded(&mut f, &ev, now), "split-burst payload must be eaten: {c:?}");
            now += Duration::from_millis(1);
        }
        // Split ST too: lone ESC (held) then '\'.
        assert!(discarded(&mut f, &esc, now + Duration::from_millis(1)));
        let bs = CtEvent::Key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::NONE));
        assert!(discarded(&mut f, &bs, now + Duration::from_millis(2)));
        // The held ESC belonged to the ST — it must never flush as a quit.
        assert!(!f.take_expired_esc(now + Duration::from_secs(5)), "ST Esc must not flush");

        // Real key after the quiet gap passes.
        let x = CtEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!discarded(&mut f, &x, now + Duration::from_millis(400)));
    }

    /// A split CSI reply (`ESC` | `[?62;c` across reads — crossterm only
    /// absorbs CSI replies that land whole) is the same hole: the held ESC
    /// completed by `[` opens a discard span, the quiet gap closes it (CSI
    /// has no ST).
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
        // Digit after the quiet gap is real input again.
        let five = CtEvent::Key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE));
        assert!(!discarded(&mut f, &five, now + Duration::from_millis(400)));
    }

    /// M2 low fix a, part 1b: a REAL lone Esc keypress in an armed session
    /// is delayed by at most the hold window, never lost — and an Esc
    /// followed by a non-`P` key flushes immediately.
    #[test]
    fn straggler_filter_flushes_real_esc() {
        let t0 = std::time::Instant::now();
        let esc = CtEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let x = CtEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        // Esc then silence: expires into a real Esc (the caller quits).
        let mut f = StragglerFilter::new(true, t0);
        assert!(discarded(&mut f, &esc, t0 + Duration::from_millis(10)));
        assert!(!f.take_expired_esc(t0 + Duration::from_millis(100)), "still inside the hold");
        assert!(f.take_expired_esc(t0 + Duration::from_millis(200)));
        assert!(!f.take_expired_esc(t0 + Duration::from_millis(201)), "flushes exactly once");

        // Esc then a real key inside the hold: Esc flushes with the key.
        let mut f = StragglerFilter::new(true, t0);
        assert!(discarded(&mut f, &esc, t0 + Duration::from_millis(10)));
        let v = f.filter(&x, t0 + Duration::from_millis(50));
        assert_eq!(v, Filtered { flush_esc: true, discard: false });
    }

    /// Resize events pass even mid-fragment (never drop a SIGWINCH) and a
    /// disarmed filter passes everything (probe saw its DA1 → no stragglers
    /// possible). M2 low fix a, part 2: there is NO disarm window anymore —
    /// a reply burst arriving minutes into an armed session is still
    /// swallowed (the old 2 s window let it through as key events).
    #[test]
    fn straggler_filter_scope_is_armed_sessions_for_life() {
        let t0 = std::time::Instant::now();
        let alt_p = CtEvent::Key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::ALT));
        let digit = CtEvent::Key(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::NONE));

        // Disarmed: nothing is ever discarded, Esc passes instantly.
        let mut off = StragglerFilter::new(false, t0);
        assert!(!discarded(&mut off, &alt_p, t0 + Duration::from_millis(10)));
        let esc = CtEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!discarded(&mut off, &esc, t0 + Duration::from_millis(11)));

        // Armed: resize passes mid-fragment.
        let mut f = StragglerFilter::new(true, t0);
        let now = t0 + Duration::from_millis(50);
        assert!(discarded(&mut f, &alt_p, now));
        assert!(!discarded(&mut f, &CtEvent::Resize(100, 40), now + Duration::from_millis(1)));

        // A burst 10 minutes in (way past the removed 2 s window) is still a
        // reply, digits included; a real key after the quiet gap passes.
        let mut f = StragglerFilter::new(true, t0);
        let late = t0 + Duration::from_secs(600);
        assert!(discarded(&mut f, &alt_p, late));
        assert!(discarded(&mut f, &digit, late + Duration::from_millis(1)));
        assert!(!discarded(&mut f, &digit, late + Duration::from_millis(400)));
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
