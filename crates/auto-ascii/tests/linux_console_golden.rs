use std::path::PathBuf;

use auto_ascii::pipeline::{Player, color_depth};
use auto_ascii::{Cell, Grid, PaletteChoice};
use auto_ascii_eval::fixtures::{Fixture, build_fixture};
use auto_ascii_format::AsciiReader;
use auto_ascii_term::{
    Backend, Caps, ColorTier, GlyphFlags, GlyphSupportTier, SimBackend,
};

const COLS: u16 = 80;
const ROWS: u16 = 24;
const FRAME: u32 = 10;

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

fn console_grid() -> Grid<Cell> {
    console_grids(Fixture::GradientMotion, &[FRAME]).pop().unwrap()
}

fn console_grids(fixture: Fixture, frames: &[u32]) -> Vec<Grid<Cell>> {
    let caps = linux_console_caps();
    let glyph_tier = PaletteChoice::Auto.resolve_for_caps(&caps);
    assert_eq!(
        glyph_tier,
        auto_ascii_core::GlyphTier::Ascii,
        "CP437 support must resolve to the ASCII floor (palette 8 base ramp)"
    );

    let asset = build_fixture(fixture);
    let reader = AsciiReader::open(&asset).expect("fixture asset is valid");
    let mut player = Player::new(
        reader,
        auto_ascii_core::DEFAULT_CELL_ASPECT,
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
    if std::env::var_os("ASCII_UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("missing golden {} ({e}); bless with ASCII_UPDATE_GOLDENS=1", path.display())
    });
    assert_eq!(text, want, "console render diverged from {}", path.display());
}

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

    assert!(
        grid.row(ROWS - 1).iter().all(|c| *c == Cell::BLANK),
        "letterbox pad row is not blank"
    );
}

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
