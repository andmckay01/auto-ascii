//! M5 scrub UX (PLAN §7 M5 item D): Left/Right arrow seeks and the
//! transient bottom-row progress overlay — plus the M6 key-hints row one
//! line above it (PLAN-M6-M8 §1).
//!
//! The acceptance surface tested here:
//! * arrows surface through the REAL event queue as coalesced ±5 s steps and
//!   reset hysteresis exactly like digit jumps (temporal discontinuity), and
//!   `?`/`h` surface as the hints toggle;
//! * neither overlay corrupts diff output — every presented frame across
//!   show/hide parses as a valid escape stream, replaying the with-overlay
//!   diff stream reconstructs the SAME screen as an untouched reference
//!   player once both rows hide, and EACH hide forces a full repaint (the
//!   diff baseline cannot keep describing overlay cells);
//! * the hints row drops whole items from the right as the terminal narrows,
//!   and the progress row's arrow block appears exactly at 64 columns.

use auto_ascii::pipeline::Player;
use auto_ascii_core::{ColorDepth, GlyphTier};
use auto_ascii_eval::fixtures::{Fixture, build_fixture};
use auto_ascii_format::AsciiReader;
use auto_ascii_term::{Event, Key, SimBackend};

fn player(bytes: &[u8], repaint_full: bool) -> Player<'_> {
    // Ascii tier keeps every emitted glyph single-byte, so the screen model
    // below can be strict about what it accepts.
    Player::new(AsciiReader::open(bytes).unwrap(), 2.0, repaint_full, ColorDepth::True, GlyphTier::Ascii)
        .unwrap()
}

/// One rendered grid row as text (M6 hint/progress row assertions).
fn grid_row(p: &Player<'_>, row: u16) -> String {
    p.grid().row(row).iter().map(|c| c.glyph()).collect()
}

// The key-hints row exactly as PLAN-M6-M8 §1 specifies it, at the widths the
// milestone calls out plus the 32-column floor: the full list at 80, then
// items leaving in drop order — `[ ] adjust`, `d dial`, `space pause`,
// `<- -> 5s` — while `? keys` survives all of them, because how to summon
// the legend back is what a cramped screen must still say. Written out in
// full rather than assembled, so a wording change has to be deliberate.
const HINTS_80: &str =
    " q quit   space pause   0-9 jump   <- -> 5s   d dial   [ ] adjust   ? keys      ";
const HINTS_64: &str = " q quit   space pause   0-9 jump   <- -> 5s   d dial   ? keys   ";
const HINTS_40: &str = " q quit   0-9 jump   <- -> 5s   ? keys  ";
const HINTS_32: &str = " q quit   0-9 jump   ? keys     ";

// ---------------------------------------------------------------------------
// A strict truecolor escape-stream interpreter: it accepts EXACTLY what the
// painter is specified to emit (CUP, truecolor SGR runs, the ?2026 wrap,
// printable ASCII) and panics on anything else — parsing success IS the
// byte-validity assertion. Applying frames in order reconstructs the screen.
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq)]
struct ScreenCell {
    ch: char,
    fg: (u8, u8, u8),
    bg: (u8, u8, u8),
}

struct Screen {
    cols: u16,
    rows: u16,
    cells: Vec<ScreenCell>,
    cur_col: u16,
    cur_row: u16,
    fg: (u8, u8, u8),
    bg: (u8, u8, u8),
}

impl Screen {
    fn new(cols: u16, rows: u16) -> Screen {
        Screen {
            cols,
            rows,
            cells: vec![
                ScreenCell { ch: ' ', fg: (0, 0, 0), bg: (0, 0, 0) };
                cols as usize * rows as usize
            ],
            cur_col: 0,
            cur_row: 0,
            fg: (0, 0, 0),
            bg: (0, 0, 0),
        }
    }

    fn row_string(&self, row: u16) -> String {
        (0..self.cols)
            .map(|c| self.cells[row as usize * self.cols as usize + c as usize].ch)
            .collect()
    }

    fn sgr(&mut self, params: &str) {
        let nums: Vec<u32> = params
            .split(';')
            .map(|p| p.parse().unwrap_or_else(|_| panic!("bad SGR param {p:?} in {params:?}")))
            .collect();
        let mut i = 0;
        while i < nums.len() {
            match nums[i] {
                38 | 48 => {
                    assert_eq!(nums.get(i + 1), Some(&2), "truecolor SGR must be ;2; {params:?}");
                    assert!(i + 4 < nums.len(), "truncated 38/48;2 SGR: {params:?}");
                    let rgb =
                        (nums[i + 2] as u8, nums[i + 3] as u8, nums[i + 4] as u8);
                    if nums[i] == 38 {
                        self.fg = rgb;
                    } else {
                        self.bg = rgb;
                    }
                    i += 5;
                }
                other => panic!("unexpected SGR code {other} (truecolor painter): {params:?}"),
            }
        }
    }

    /// Apply one presented frame's bytes. Panics on any byte or escape
    /// sequence the painter is not specified to produce.
    fn apply(&mut self, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == 0x1b {
                assert_eq!(bytes.get(i + 1), Some(&b'['), "ESC not starting CSI at {i}");
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                assert!(j < bytes.len(), "unterminated CSI at {i}");
                let params = std::str::from_utf8(&bytes[i + 2..j]).expect("CSI params utf8");
                match bytes[j] {
                    b'H' => {
                        let (r, c) = params.split_once(';').expect("CUP row;col");
                        let row: u16 = r.parse().expect("CUP row");
                        let col: u16 = c.parse().expect("CUP col");
                        assert!(row >= 1 && row <= self.rows, "CUP row {row} off-screen");
                        assert!(col >= 1 && col <= self.cols, "CUP col {col} off-screen");
                        self.cur_row = row - 1;
                        self.cur_col = col - 1;
                    }
                    b'm' => self.sgr(params),
                    b'h' | b'l' => {
                        assert_eq!(params, "?2026", "only the sync wrap may toggle modes");
                    }
                    other => panic!("unexpected CSI final {:?}", other as char),
                }
                i = j + 1;
            } else {
                assert!(
                    (0x20..=0x7e).contains(&b),
                    "non-ASCII/control byte 0x{b:02x} outside an escape at {i}"
                );
                assert!(self.cur_col < self.cols, "glyph written past the row end");
                let idx = self.cur_row as usize * self.cols as usize + self.cur_col as usize;
                self.cells[idx] = ScreenCell { ch: b as char, fg: self.fg, bg: self.bg };
                self.cur_col += 1;
                i += 1;
            }
        }
    }
}

/// The acceptance test (M5 accept 2, extended by PLAN-M6-M8 §1): frames stay
/// byte-valid across show/hide in PURE DIFF mode, the reconstructed screen
/// matches a no-overlay reference everywhere except the two overlay rows
/// while they are visible, EACH hide forces a full repaint, and afterwards
/// the screens are identical — the overlays left zero trace in the diff
/// state. The two rows hide on separate frames on purpose: each hide path
/// has to schedule its own invalidate, not lean on the other one's.
#[test]
fn overlay_show_hide_never_corrupts_diff_output() {
    let asset = build_fixture(Fixture::GradientMotion);
    let (cols, rows) = (80u16, 24u16);

    let mut with = player(&asset, false); // pure diff — the corruptible mode
    let mut reference = player(&asset, false);
    let mut b_with = SimBackend::new(cols, rows);
    let mut b_ref = SimBackend::new(cols, rows);
    with.reflow(&mut b_with, cols, rows);
    reference.reflow(&mut b_ref, cols, rows);

    let mut s_with = Screen::new(cols, rows);
    let mut s_ref = Screen::new(cols, rows);

    for f in 0..14u32 {
        if f == 4 {
            with.set_progress_overlay(true);
            with.set_hint_overlay(true); // the hints ride with it (M6)
        }
        if f == 8 {
            with.set_progress_overlay(false); // auto-hide moment
        }
        if f == 11 {
            with.set_hint_overlay(false); // the hints follow, one hide later
        }
        let stats = with.render_present(&mut b_with, f).unwrap();
        s_with.apply(&b_with.take_output()); // panics on any invalid byte
        reference.render_present(&mut b_ref, f).unwrap();
        s_ref.apply(&b_ref.take_output());

        if f == 8 || f == 11 {
            assert_eq!(
                stats.cells_damaged,
                u32::from(cols) * u32::from(rows),
                "hiding an overlay must invalidate → full repaint (frame {f})"
            );
        }
        if (4..11).contains(&f) {
            let hints = s_with.row_string(rows - 2);
            assert_eq!(hints, HINTS_80, "hints row at frame {f}");
            let bottom = s_with.row_string(rows - 1);
            if f < 8 {
                assert!(
                    bottom.contains('[') && bottom.contains('%') && bottom.contains('/'),
                    "overlay visible on the bottom row: {bottom:?}"
                );
            } else {
                // Progress gone, hints still up: the bottom row is the
                // picture again, repainted from a clean baseline.
                assert_eq!(bottom, s_ref.row_string(rows - 1), "bottom row at frame {f}");
            }
            // Everything ABOVE the two overlay rows is untouched by them.
            for r in 0..rows - 2 {
                assert_eq!(s_with.row_string(r), s_ref.row_string(r), "row {r} at frame {f}");
            }
        } else {
            assert!(
                s_with.cells == s_ref.cells,
                "screens must be identical when the overlays are hidden (frame {f})"
            );
        }
    }
}

/// The hints row at four widths (PLAN-M6-M8 §1 + the M6 review fix): the
/// full list fits at 80; 64 has dropped `[ ] adjust`; a 40-column terminal
/// has also dropped `d dial` and `space pause`, and a 32-column one (the
/// minimum playable width) the arrows too — but `? keys` is still there at
/// every width, so the legend can always be summoned back. The row is
/// painted to its full width throughout, so no stale picture cell survives
/// underneath it.
#[test]
fn hint_row_drops_whole_items_as_the_terminal_narrows() {
    let asset = build_fixture(Fixture::GradientMotion);
    for (cols, want) in [(80u16, HINTS_80), (64, HINTS_64), (40, HINTS_40), (32, HINTS_32)] {
        let rows = 24u16;
        let mut backend = SimBackend::new(cols, rows);
        let mut p = player(&asset, true);
        p.reflow(&mut backend, cols, rows);
        p.set_hint_overlay(true);
        p.render_present(&mut backend, 0).unwrap();
        backend.take_output();
        assert_eq!(grid_row(&p, rows - 2), want, "hints row at {cols} columns");
        assert_eq!(want.len(), cols as usize, "the row is painted to its full width");
        assert!(want.contains("? keys"), "the summon hint survives every drop");
    }
}

/// M6 review fix: the hints row never lands on the enlarge card. At 30x4 the
/// terminal is below the 32x9 minimum, so the card is what `render_grid`
/// draws — and `rows-2` is exactly where the card's "enlarge terminal" line
/// sits. Raising the row must therefore change nothing at all.
#[test]
fn hint_row_stays_off_the_enlarge_card() {
    let asset = build_fixture(Fixture::GradientMotion);
    let (cols, rows) = (30u16, 4u16);

    let mut backend = SimBackend::new(cols, rows);
    let mut p = player(&asset, true);
    p.reflow(&mut backend, cols, rows);
    p.set_hint_overlay(true);
    p.render_present(&mut backend, 0).unwrap();
    backend.take_output();

    let mut b_ref = SimBackend::new(cols, rows);
    let mut reference = player(&asset, true);
    reference.reflow(&mut b_ref, cols, rows);
    reference.render_present(&mut b_ref, 0).unwrap();
    b_ref.take_output();

    assert!(grid_row(&p, rows - 2).contains("enlarge"), "the card's message is on rows-2");
    assert_eq!(
        p.grid().as_slice(),
        reference.grid().as_slice(),
        "a raised hints row must not touch the enlarge card"
    );
}

/// The progress row's arrow block (PLAN-M6-M8 §1) appears exactly at
/// `PROGRESS_HINT_MIN_COLS` = 64 and is absent one column below, where the
/// row is the M5 layout to the byte — a narrow terminal spends its columns on
/// the timecode and the bar instead.
#[test]
fn progress_row_gains_the_arrow_block_at_64_columns() {
    let asset = build_fixture(Fixture::GradientMotion);
    let rows = 24u16;
    let row_at = |cols: u16| {
        let mut backend = SimBackend::new(cols, rows);
        let mut p = player(&asset, true);
        p.reflow(&mut backend, cols, rows);
        p.set_progress_overlay(true);
        p.render_present(&mut backend, 0).unwrap();
        backend.take_output();
        grid_row(&p, rows - 1)
    };

    let wide = row_at(64);
    assert!(wide.starts_with(" <- 5s -> "), "arrow block at 64 columns: {wide:?}");
    assert!(wide.contains('[') && wide.ends_with("% "), "bar and percent survive: {wide:?}");
    assert_eq!(wide.len(), 64, "the row is still painted edge to edge");

    let narrow = row_at(63);
    assert!(!narrow.contains('<'), "no arrow block below the threshold: {narrow:?}");
    assert!(narrow.starts_with(" 0:00 / "), "M5 layout starts at the timecode: {narrow:?}");
    assert_eq!(narrow.len(), 63);
}

/// `?` and `h` surface through the REAL event queue as the hints toggle
/// (PLAN-M6-M8 §1), collapsed to one flag per drain, and they move nothing
/// else: the row is chrome, not a seek or a dial.
#[test]
fn question_mark_and_h_report_the_hints_toggle() {
    let asset = build_fixture(Fixture::GradientMotion);
    let mut backend = SimBackend::new(80, 24);
    let mut p = player(&asset, true);
    p.reflow(&mut backend, 80, 24);

    for key in ['?', 'h'] {
        backend.push_event(Event::Key(Key::Char(key)));
        let d = p.drain_events(&mut backend);
        assert!(d.toggle_hints, "{key} must report the hints toggle");
        assert_eq!((d.jump_digit, d.seek_steps, d.dial_cycle, d.dial_delta), (None, 0, 0, 0));
    }

    // Repeats inside one drain collapse — a held key must not flicker the row.
    backend.push_event(Event::Key(Key::Char('?')));
    backend.push_event(Event::Key(Key::Char('h')));
    backend.push_event(Event::Key(Key::Char('?')));
    assert!(p.drain_events(&mut backend).toggle_hints);

    // Unbound keys still report nothing.
    backend.push_event(Event::Key(Key::Char('x')));
    assert!(!p.drain_events(&mut backend).toggle_hints);
}

/// Space through the REAL event queue (M6 pause): one flag per drain,
/// collapsed like `?`, and it moves nothing else — pausing is a transport
/// change, not a seek or a dial.
#[test]
fn space_reports_the_pause_toggle() {
    let asset = build_fixture(Fixture::GradientMotion);
    let mut backend = SimBackend::new(80, 24);
    let mut p = player(&asset, true);
    p.reflow(&mut backend, 80, 24);

    backend.push_event(Event::Key(Key::Char(' ')));
    let d = p.drain_events(&mut backend);
    assert!(d.toggle_pause, "space must report the pause toggle");
    assert_eq!((d.jump_digit, d.seek_steps, d.dial_cycle, d.dial_delta), (None, 0, 0, 0));
    assert!(!d.toggle_hints);

    // A key repeat inside one drain is one intent, not a pause/resume stutter.
    backend.push_event(Event::Key(Key::Char(' ')));
    backend.push_event(Event::Key(Key::Char(' ')));
    assert!(p.drain_events(&mut backend).toggle_pause);

    // Quit still wins over a queued space.
    backend.push_event(Event::Key(Key::Char(' ')));
    backend.push_event(Event::Quit);
    let d = p.drain_events(&mut backend);
    assert!(d.quit);
    assert!(!d.toggle_pause);
}

/// M6 pause on screen: the progress row reads ` PAUSED ` where the
/// percentage goes and `|` where the bar head goes, in printable ASCII the
/// strict screen model above accepts. The row is a pure function of the
/// paused flag, so it persists frame after frame with zero damage in diff
/// mode — nothing in the render path can time it out, which is what lets the
/// run loop suspend the 1 s deadline (`ProgressTimer`, unit-tested in
/// player.rs). Resuming restores the percentage, and hiding the row still
/// forces the full repaint.
#[test]
fn paused_progress_row_reads_paused_and_persists() {
    let asset = build_fixture(Fixture::GradientMotion);
    let (cols, rows) = (80u16, 24u16);
    let mut backend = SimBackend::new(cols, rows);
    let mut p = player(&asset, false); // pure diff — the corruptible mode
    p.reflow(&mut backend, cols, rows);
    let mut screen = Screen::new(cols, rows);

    p.set_progress_overlay(true);
    p.set_paused(true);
    p.render_present(&mut backend, 12).unwrap();
    screen.apply(&backend.take_output()); // panics on any invalid byte
    let paused = screen.row_string(rows - 1);
    assert!(paused.ends_with(" PAUSED "), "the percent block reads PAUSED: {paused:?}");
    assert!(paused.contains("=|"), "the bar head is a bar, not an arrow: {paused:?}");
    assert!(!paused.contains('%'), "no percentage while frozen: {paused:?}");

    // Frozen: the same frame, the same row, no damage at all — the row
    // cannot age out on its own however long the pause lasts.
    for _ in 0..4 {
        let stats = p.render_present(&mut backend, 12).unwrap();
        screen.apply(&backend.take_output());
        assert_eq!(stats.cells_damaged, 0, "a frozen frame redraws nothing");
        assert_eq!(screen.row_string(rows - 1), paused, "and the row does not move");
    }

    // A seek while frozen: the row follows the new frame, still PAUSED.
    p.render_present(&mut backend, 40).unwrap();
    screen.apply(&backend.take_output());
    let moved = screen.row_string(rows - 1);
    assert!(moved.ends_with(" PAUSED "), "still frozen after a seek: {moved:?}");
    assert_ne!(moved, paused, "but the timecode moved with the frame");

    // Resume: the percentage is back, and the hide still repaints in full.
    p.set_paused(false);
    p.render_present(&mut backend, 40).unwrap();
    screen.apply(&backend.take_output());
    let playing = screen.row_string(rows - 1);
    assert!(playing.contains('%') && playing.contains("=>"), "resumed row: {playing:?}");
    p.set_progress_overlay(false);
    let stats = p.render_present(&mut backend, 40).unwrap();
    screen.apply(&backend.take_output());
    assert_eq!(
        stats.cells_damaged,
        u32::from(cols) * u32::from(rows),
        "hiding the row after a pause still forces the full repaint"
    );
}

/// Arrow keys through the REAL event queue: coalesced net steps, quit
/// priority, and the same cold-start hysteresis guarantee as digit seeks —
/// the landing frame is byte-identical to a fresh player's render of it.
#[test]
fn arrow_scrub_reports_steps_and_resets_state() {
    let asset = build_fixture(Fixture::GradientMotion);
    let mut backend = SimBackend::new(80, 24);
    let mut p = player(&asset, true);
    p.reflow(&mut backend, 80, 24);

    // Warm the temporal state well past the landing frame.
    for f in 0..10 {
        p.render_present(&mut backend, f).unwrap();
        backend.take_output();
    }

    // Two Lefts and a Right coalesce to net −1 (−5 s).
    backend.push_event(Event::Key(Key::Left));
    backend.push_event(Event::Key(Key::Left));
    backend.push_event(Event::Key(Key::Right));
    let d = p.drain_events(&mut backend);
    assert!(!d.quit);
    assert_eq!(d.seek_steps, -1, "Left+Left+Right must coalesce to -1");
    assert_eq!(d.jump_digit, None);

    // drain_events already reset hysteresis: the landing frame equals a
    // cold start (the digit-jump rule, extended to arrows).
    p.render_present(&mut backend, 2).unwrap();
    backend.take_output();
    let mut cold = player(&asset, true);
    cold.reflow(&mut backend, 80, 24);
    cold.render_present(&mut backend, 2).unwrap();
    backend.take_output();
    assert_eq!(
        p.grid().as_slice(),
        cold.grid().as_slice(),
        "arrow-seek landing frame must be byte-identical to a cold start"
    );

    // Quit still wins over queued arrows.
    backend.push_event(Event::Key(Key::Right));
    backend.push_event(Event::Quit);
    let d = p.drain_events(&mut backend);
    assert!(d.quit);
    assert_eq!(d.seek_steps, 0);
}

/// The public scrub-step contract (PLAN §7 M5: Left/Right = ±5 s). Since M6
/// the constant lives in `pipeline` (the overlays print it, and that module
/// builds without the `terminal` feature); only the facade re-export is
/// gated, because the pure-embedder build has no key bindings to document.
#[cfg(feature = "terminal")]
#[test]
fn scrub_step_is_five_seconds() {
    assert_eq!(auto_ascii::SCRUB_STEP_SECS, 5.0);
}
