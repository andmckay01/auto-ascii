//! M4 item A: `RenderSession` — the terminal-free embedder entry — against
//! the deterministic M2 fixture assets. Covers the documented temporal-state
//! contract (monotonic advance = full quality, backward jump = automatic
//! cold reset), the resize path, palette/cell-aspect knobs and the error
//! surface. No terminal, no backend anywhere in this file — exactly what an
//! embedding project sees.

use std::fs;
use std::path::PathBuf;

use slpy_eval::fixtures::{FIXTURE_FRAMES, Fixture, build_fixture};
use sleepytime::{Cell, Error, Grid, PaletteChoice, RenderSession};

/// Self-cleaning temp file (no tempfile dep — pinned workspace dep set).
struct TmpFile(PathBuf);

impl TmpFile {
    fn with_fixture(fixture: Fixture, tag: &str) -> TmpFile {
        let mut p = std::env::temp_dir();
        p.push(format!("sleepytime-session-{}-{tag}.slpy", std::process::id()));
        fs::write(&p, build_fixture(fixture)).expect("write fixture asset");
        TmpFile(p)
    }
}

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn glyphs(grid: &Grid<Cell>) -> String {
    (0..grid.rows())
        .flat_map(|r| grid.row(r).iter().map(|c| c.glyph()))
        .collect()
}

#[test]
fn open_exposes_asset_metadata() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "meta");
    let session = RenderSession::open(&f.0).expect("open fixture");
    assert_eq!(session.frame_count(), FIXTURE_FRAMES);
    assert!((session.fps() - 30.0).abs() < 1e-9, "fixtures are 30 fps");
    assert!((session.aspect() - 16.0 / 9.0).abs() < 1e-9, "16:9 asset");
}

#[test]
fn render_fills_requested_grid_deterministically() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "grid");
    let mut session = RenderSession::open(&f.0).unwrap();
    let first = glyphs(session.render(0, 80, 24).unwrap());
    let grid = session.render(0, 80, 24).unwrap();
    assert_eq!((grid.cols(), grid.rows()), (80, 24));
    assert_eq!(glyphs(grid), first, "same frame twice = identical grid");
    // A fixture frame is not blank.
    assert!(first.chars().any(|c| c != ' '), "rendered content expected");
}

/// The documented backward-jump contract: after a jump the landing frame is
/// EXACTLY what a cold session produces (no pre-seek ghosting).
#[test]
fn backward_jump_resets_to_cold_render() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "jump");
    let mut warm = RenderSession::open(&f.0).unwrap();
    // Monotonic advance with skips (latest-frame-wins shape).
    for idx in [0u32, 1, 2, 5, 40] {
        warm.render(idx, 100, 30).unwrap();
    }
    let jumped = warm.render(3, 100, 30).unwrap().as_slice().to_vec(); // backward: reset

    let mut cold = RenderSession::open(&f.0).unwrap();
    let cold_cells = cold.render(3, 100, 30).unwrap().as_slice().to_vec();
    assert_eq!(jumped, cold_cells, "post-jump frame == cold-start frame, cell for cell");
}

#[test]
fn resize_reflows_and_below_minimum_renders_card() {
    let f = TmpFile::with_fixture(Fixture::CheckerDrift, "resize");
    let mut session = RenderSession::open(&f.0).unwrap();
    assert_eq!(session.render(0, 80, 24).unwrap().cols(), 80);
    let grid = session.render(1, 213, 58).unwrap();
    assert_eq!((grid.cols(), grid.rows()), (213, 58), "grid tracks the call");
    // Below the 32x9 minimum: the enlarge card, never a panic.
    let tiny = session.render(2, 10, 3).unwrap();
    assert_eq!((tiny.cols(), tiny.rows()), (10, 3));
    // And back up again.
    assert_eq!(session.render(3, 80, 24).unwrap().cols(), 80);
}

#[test]
fn palette_and_cell_aspect_knobs() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "knobs");
    let mut session = RenderSession::open(&f.0).unwrap();
    session.set_palette(PaletteChoice::Ascii);
    let ascii = glyphs(session.render(0, 80, 24).unwrap());
    // The ascii tier is ASCII, full stop — no `|| c == '‾'` escape clause
    // (M4 review: U+203E is not CP437 and boxes out on the Linux console).
    assert!(ascii.is_ascii(), "ascii tier: {ascii:?}");
    // Square cells at 80x24: R = 16/9, height-limited -> a 43x24 viewport
    // with pad_left = 18 (PLAN §3.2 math at a=1.0) — the left pad columns
    // must be blank cells.
    session.set_cell_aspect(1.0).unwrap();
    let square = session.render(0, 80, 24).unwrap();
    for col in 0..18u16 {
        for row in 0..square.rows() {
            assert_eq!(square.get(col, row), Cell::BLANK, "pad at ({col},{row})");
        }
    }
    assert!(session.set_cell_aspect(0.0).is_err());
    assert!(session.set_cell_aspect(f64::NAN).is_err());
}

#[test]
fn error_surface_is_coherent() {
    // Out-of-range frame index.
    let f = TmpFile::with_fixture(Fixture::HardCut, "errors");
    let mut session = RenderSession::open(&f.0).unwrap();
    let e = session.render(FIXTURE_FRAMES, 80, 24).unwrap_err();
    assert!(matches!(e, Error::Config(_)), "{e}");
    // Missing file.
    let e = RenderSession::open("/no/such/asset.slpy").unwrap_err();
    assert!(matches!(e, Error::Io { .. }), "{e}");
    // Present but not SLPY.
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let e = RenderSession::open(manifest).unwrap_err();
    assert!(matches!(e, Error::Format { .. }), "{e}");
}
