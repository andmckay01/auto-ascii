//! M5 scrub UX (PLAN §7 M5 item D): Left/Right arrow seeks and the
//! transient bottom-row progress overlay.
//!
//! The acceptance surface tested here:
//! * arrows surface through the REAL event queue as coalesced ±5 s steps and
//!   reset hysteresis exactly like digit jumps (temporal discontinuity);
//! * the overlay never corrupts diff output — every presented frame across
//!   overlay show/hide parses as a valid escape stream, replaying the
//!   with-overlay diff stream reconstructs the SAME screen as an untouched
//!   reference player once the overlay hides, and the hide itself forces a
//!   full repaint (the diff baseline cannot keep describing overlay cells).

use sleepytime::pipeline::Player;
use slpy_core::{ColorDepth, GlyphTier};
use slpy_eval::fixtures::{Fixture, build_fixture};
use slpy_format::SlpyReader;
use slpy_term::{Event, Key, SimBackend};

fn player(bytes: &[u8], repaint_full: bool) -> Player<'_> {
    // Ascii tier keeps every emitted glyph single-byte, so the screen model
    // below can be strict about what it accepts.
    Player::new(SlpyReader::open(bytes).unwrap(), 2.0, repaint_full, ColorDepth::True, GlyphTier::Ascii)
        .unwrap()
}

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

/// The acceptance test (M5 accept 2): frames stay byte-valid across overlay
/// show/hide in PURE DIFF mode, the reconstructed screen matches a
/// no-overlay reference everywhere except the overlay row while visible,
/// the hide forces a full repaint, and afterwards the screens are identical
/// — the overlay left zero trace in the diff state.
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

    for f in 0..12u32 {
        if f == 4 {
            with.set_progress_overlay(true);
        }
        if f == 8 {
            with.set_progress_overlay(false); // auto-hide moment
        }
        let stats = with.render_present(&mut b_with, f).unwrap();
        s_with.apply(&b_with.take_output()); // panics on any invalid byte
        reference.render_present(&mut b_ref, f).unwrap();
        s_ref.apply(&b_ref.take_output());

        if f == 8 {
            assert_eq!(
                stats.cells_damaged,
                u32::from(cols) * u32::from(rows),
                "hiding the overlay must invalidate → full repaint"
            );
        }
        if (4..8).contains(&f) {
            let bottom = s_with.row_string(rows - 1);
            assert!(
                bottom.contains('[') && bottom.contains('%') && bottom.contains('/'),
                "overlay visible on the bottom row: {bottom:?}"
            );
            // Everything ABOVE the overlay row is untouched by it.
            for r in 0..rows - 1 {
                assert_eq!(s_with.row_string(r), s_ref.row_string(r), "row {r} at frame {f}");
            }
        } else {
            assert!(
                s_with.cells == s_ref.cells,
                "screens must be identical when the overlay is hidden (frame {f})"
            );
        }
    }
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

/// The public scrub-step contract (PLAN §7 M5: Left/Right = ±5 s). The
/// constant lives on the terminal Player, so it exists only with that
/// feature (the pure-embedder build has no key bindings to document).
#[cfg(feature = "terminal")]
#[test]
fn scrub_step_is_five_seconds() {
    assert_eq!(sleepytime::SCRUB_STEP_SECS, 5.0);
}
