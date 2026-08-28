//! M4 item D — the **`TERM=linux` legibility floor** (PLAN §7 M4 accept:
//! "`TERM=linux` legible with palette 8").
//!
//! The Linux console is the worst terminal we ship to and the one the owner
//! can least easily inspect from a desktop session: 16 ANSI colors, a CP437
//! font, no `CSI 16 t` (so cell aspect falls back to 2.0), no synchronized
//! output. `crates/slpy-term/tests/terminal_identity.rs` pins what the probe
//! must *conclude* there; this file pins what we then *draw*:
//!
//! 1. a committed glyph-grid golden, rendered through the real
//!    `auto_ascii::pipeline::Player` at the caps the console produces, and
//! 2. legibility assertions that a golden alone cannot express — glyph
//!    repertoire (CP437-safe: no glyph the console font lacks), ink coverage,
//!    tonal range, and a 16-color escape stream with no truecolor SGR in it.
//!
//! Re-bless deliberately: `SLPY_UPDATE_GOLDENS=1 cargo test -p auto-ascii
//! --test linux_console_golden`, then review the diff.

use std::path::PathBuf;

use auto_ascii::pipeline::{Player, color_depth};
use auto_ascii::{Cell, Grid, PaletteChoice};
use slpy_eval::fixtures::{Fixture, build_fixture};
use slpy_format::SlpyReader;
use slpy_term::{
    Backend, Caps, ColorTier, GlyphFlags, GlyphSupportTier, SimBackend,
};

/// A full-screen console: 80×24 is the kernel VT's default text mode.
const COLS: u16 = 80;
const ROWS: u16 = 24;
/// Mid-motion gradient frame (same fixture family as the other goldens).
const FRAME: u32 = 10;

/// Exactly what `probe_caps` concludes for `TERM=linux` (pinned by
/// `slpy-term/tests/terminal_identity.rs::linux_console_identity`): 16
/// colors, CP437 repertoire, no synchronized output, no cell-pixel report.
fn linux_console_caps() -> Caps {
    Caps {
        color: ColorTier::C16,
        glyphs: GlyphFlags::ASCII.with(GlyphFlags::BLOCKS),
        glyph_support: GlyphSupportTier::Cp437,
        sync_2026: false,
        cells: (COLS, ROWS),
        cell_px: None,
        can_query: true,
    }
}

/// Render the fixture frame exactly as the player would on that console:
/// the caps go through the facade's own palette resolution, so a change in
/// `PaletteChoice::Auto`'s Caps mapping shows up here.
fn console_grid() -> Grid<Cell> {
    console_grids(Fixture::GradientMotion, &[FRAME]).pop().unwrap()
}

/// The same render for a whole list of frames of any fixture — the
/// repertoire floor below needs coverage, not one frame's luck.
fn console_grids(fixture: Fixture, frames: &[u32]) -> Vec<Grid<Cell>> {
    let caps = linux_console_caps();
    let glyph_tier = PaletteChoice::Auto.resolve_for_caps(&caps);
    assert_eq!(
        glyph_tier,
        slpy_core::GlyphTier::Ascii,
        "CP437 support must resolve to the ASCII floor (palette 8 base ramp)"
    );

    let asset = build_fixture(fixture);
    let reader = SlpyReader::open(&asset).expect("fixture asset is valid");
    let mut player = Player::new(
        reader,
        slpy_core::DEFAULT_CELL_ASPECT, // no CSI 16 t on the console → 2.0
        false,
        color_depth(caps.color),
        glyph_tier,
    )
    .expect("player over the fixture");
    let mut backend = SimBackend::new(COLS, ROWS);
    backend.set_caps(caps);
    player.reflow(&mut backend, COLS, ROWS);
    frames
        .iter()
        .map(|&f| {
            player.render_present(&mut backend, f).expect("render");
            backend.take_output();
            player.grid().clone()
        })
        .collect()
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens").join(name)
}

/// Human-reviewable text form: framed rows so trailing spaces survive
/// editors, plus the fg color index per row is deliberately NOT included —
/// the escape-stream assertions below cover color, and the point of this
/// golden is that the *picture* stays legible in glyphs alone.
fn render_text(grid: &Grid<Cell>) -> String {
    let mut s = String::new();
    s.push_str("TERM=linux console — 80x24, 16 colors, CP437/ASCII floor, frame 10\n");
    let rule = |s: &mut String| {
        s.push('+');
        for _ in 0..grid.cols() {
            s.push('-');
        }
        s.push_str("+\n");
    };
    rule(&mut s);
    for row in 0..grid.rows() {
        s.push('|');
        for cell in grid.row(row) {
            s.push(cell.glyph());
        }
        s.push_str("|\n");
    }
    rule(&mut s);
    s
}

#[test]
fn linux_console_golden() {
    let text = render_text(&console_grid());
    let path = golden_path("linux_console_80x24_f10.txt");
    if std::env::var_os("SLPY_UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("missing golden {} ({e}); bless with SLPY_UPDATE_GOLDENS=1", path.display())
    });
    assert_eq!(text, want, "console render diverged from {}", path.display());
}

/// Legibility floor 1 — **repertoire**: every glyph must be printable in the
/// console font. The ASCII tier's ramps, edge LUT and subposition glyphs are
/// all ASCII (PLAN §3.4 palettes 1–4, 8), which is a strict subset of CP437,
/// so nothing here may leave 0x20..=0x7E.
///
/// Swept across all three fixtures and many frames, **and** asserted to have
/// actually reached the sub-cell subposition branch — an M4 review finding:
/// the single-frame version of this test passed only because that one
/// gradient frame happened to contain no subposition cells, while the
/// shipping ASCII path was emitting U+203E OVERLINE (not a CP437 code point:
/// CP437's 0xEE is U+00AF MACRON) wherever the branch did fire. The data-side
/// pin is `slpy_core::palette`'s `every_ascii_tier_glyph_is_ascii`; this is
/// the same guarantee observed through the real player at real console caps.
#[test]
fn every_glyph_is_console_printable() {
    let mut saw_subpos = false;
    for fixture in Fixture::ALL {
        let frames: Vec<u32> = (0..40).collect();
        for (i, grid) in console_grids(fixture, &frames).into_iter().enumerate() {
            for row in 0..grid.rows() {
                for (col, cell) in grid.row(row).iter().enumerate() {
                    let g = cell.glyph();
                    assert!(
                        g == ' ' || g.is_ascii_graphic(),
                        "non-CP437-safe glyph {g:?} (U+{:04X}) at {col},{row} \
                         ({} frame {i})",
                        g as u32,
                        fixture.name()
                    );
                    // M5 fix 6: the witness must be '"' SPECIFICALLY — the
                    // Top slot of `slpy_core::palette::SUBPOS_GLYPHS`, which
                    // ONLY the subposition branch emits. '_' is ambiguous:
                    // the ASCII edge LUT emits it too (PLAN §3.4 palette 3),
                    // so a '_' witness could come entirely from edge cells
                    // while the subposition branch never ran.
                    saw_subpos |= g == '"';
                }
            }
        }
    }
    assert!(
        saw_subpos,
        "vacuous test: no frame emitted the '\"' top-subposition glyph, so the \
         repertoire assertion proved nothing about the subposition branch \
         (a '_' witness would not do — the edge LUT emits '_' as well)"
    );
}

/// Legibility floor 2 — **the picture is actually there**: enough ink to
/// read, enough distinct ramp steps to see tone, and letterbox pads that are
/// genuinely blank (80×24 → 80×23 viewport, one padded row, PLAN §3.2).
#[test]
fn console_frame_is_legible() {
    let grid = console_grid();
    let total = usize::from(grid.cols()) * usize::from(grid.rows());
    let nonblank = grid.as_slice().iter().filter(|c| c.glyph() != ' ').count();
    let coverage = nonblank as f64 / total as f64;
    assert!(
        coverage > 0.5,
        "console frame is too empty to read: {:.1}% non-blank",
        coverage * 100.0
    );

    let mut glyphs: Vec<char> = grid.as_slice().iter().map(|c| c.glyph()).collect();
    glyphs.sort_unstable();
    glyphs.dedup();
    assert!(
        glyphs.len() >= 5,
        "flat picture: only {} distinct glyphs ({glyphs:?})",
        glyphs.len()
    );

    // The bottom row is letterbox pad at 80×24 and must be untouched.
    assert!(
        grid.row(ROWS - 1).iter().all(|c| *c == Cell::BLANK),
        "letterbox pad row is not blank"
    );
}

/// Legibility floor 3 — **the wire**: on the console the painter may emit
/// only the standard 16-color SGRs (30–37/90–97 fg, 40–47/100–107 bg), never
/// `38;2` truecolor or `38;5` palette indices, and never a `?2026` wrap
/// (the console does not know the mode).
#[test]
fn console_stream_is_16_color_only() {
    let grid = console_grid();
    let mut sim = SimBackend::new(COLS, ROWS);
    sim.set_caps(linux_console_caps());
    let stats = sim.present(&grid);
    assert_eq!(
        stats.cells_damaged,
        u32::from(COLS) * u32::from(ROWS),
        "first present must be a full repaint"
    );
    let out = sim.take_output();
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("38;2"), "truecolor SGR on a 16-color console");
    assert!(!text.contains("38;5"), "256-color SGR on a 16-color console");
    assert!(!text.contains("\u{1b}[?2026"), "synchronized-output wrap without support");
    assert!(!out.is_empty(), "the console got no frame at all");
}
