//! Zoom discoverability (docs/research/zoom.md): the controls overlay says
//! how many cells the picture gets and, on a narrow terminal, that zooming
//! out buys more; on a wide one its text grows so it stays readable.
//!
//! * the info row carries `WxH cells` through the real `Player`, with the
//!   zoom hint on the row above below `ZOOM_HINT_MAX_COLS` and not from it;
//! * big text only on block tiers from `BIG_OVERLAY_MIN_COLS` x
//!   `BIG_OVERLAY_MIN_ROWS`, three 3-row bands at the bottom — everything
//!   above them is the no-overlay picture, and hiding them is a full
//!   repaint back to it (the overlay-hide contract, at the new size);
//! * no panic from 1x1 up with every overlay on (the resize fuzz drives the
//!   same rows through random storms).

use auto_ascii::pipeline::{BIG_OVERLAY_MIN_COLS, OverlayScale, Player, ZOOM_HINT_MAX_COLS};
use auto_ascii_core::{Cell, ColorDepth, GlyphTier, Grid};
use auto_ascii_eval::fixtures::{Fixture, build_fixture};
use auto_ascii_format::AsciiReader;
use auto_ascii_term::SimBackend;

fn player(bytes: &[u8], tier: GlyphTier) -> Player<'_> {
    // Diff mode, so a hide has to invalidate on its own.
    Player::new(AsciiReader::open(bytes).unwrap(), 2.0, false, ColorDepth::True, tier).unwrap()
}

fn row(grid: &Grid<Cell>, r: u16) -> String {
    grid.row(r).iter().map(|c| c.glyph()).collect()
}

const INFO: &str = " The Architect   codec: letters   settings: default ";

/// The picture with no overlay at all, for the "rows above are untouched"
/// comparisons.
fn reference(bytes: &[u8], tier: GlyphTier, cols: u16, rows: u16, frame: u32) -> Vec<Cell> {
    let mut b = SimBackend::new(cols, rows);
    let mut p = player(bytes, tier);
    p.reflow(&mut b, cols, rows);
    p.render_present(&mut b, frame).unwrap();
    p.grid().as_slice().to_vec()
}

/// The info row at the sizes a viewer moves through while zooming: the
/// size readout everywhere, the hint only while the grid is narrow.
#[test]
fn info_row_reads_the_grid_size_and_hints_at_zoom_while_narrow() {
    let asset = build_fixture(Fixture::GradientMotion);
    for (cols, rows) in [(80u16, 24u16), (120, 40), (159, 45), (160, 45), (213, 58)] {
        let mut backend = SimBackend::new(cols, rows);
        let mut p = player(&asset, GlyphTier::UnicodeBlocks);
        p.reflow(&mut backend, cols, rows);
        p.set_info_overlay(Some(INFO));
        p.set_hint_overlay(true);
        p.render_present(&mut backend, 0).unwrap();
        let g = p.grid();
        let info = row(g, rows - 3);
        assert!(info.starts_with(INFO), "{cols}x{rows}: {info:?}");
        assert!(info.ends_with(&format!(" {cols}x{rows} cells ")), "{cols}x{rows}: {info:?}");
        assert!(row(g, rows - 2).contains("v controls"), "hints below it");
        let hinted = row(g, rows - 4).contains("to zoom out");
        assert_eq!(hinted, cols < ZOOM_HINT_MAX_COLS, "{cols}x{rows}: {:?}", row(g, rows - 4));

        // Everything above the overlay is the plain picture.
        let reference = reference(&asset, GlyphTier::UnicodeBlocks, cols, rows, 0);
        let above = usize::from(cols) * usize::from(rows - if hinted { 4 } else { 3 });
        assert_eq!(&g.as_slice()[..above], &reference[..above], "{cols}x{rows}");
    }
}

/// From 240x36 on a block tier the three rows draw as big text, the rows
/// above them are untouched, and hiding them repaints the whole screen back
/// to the plain picture. The ASCII tier keeps one-cell text at any size.
#[test]
fn big_text_on_wide_grids_and_a_clean_hide() {
    let asset = build_fixture(Fixture::GradientMotion);
    let (cols, rows) = (320u16, 90u16);
    assert!(cols >= BIG_OVERLAY_MIN_COLS);
    let mut backend = SimBackend::new(cols, rows);
    let mut p = player(&asset, GlyphTier::UnicodeBlocks);
    p.reflow(&mut backend, cols, rows);
    p.set_progress_overlay(true);
    p.set_hint_overlay(true);
    p.set_info_overlay(Some(INFO));
    p.render_present(&mut backend, 3).unwrap();
    backend.take_output();

    let g = p.grid();
    let bands = 3 * OverlayScale::Big.line_rows();
    for r in rows - bands..rows {
        let text = row(g, r);
        assert!(text.chars().all(|c| " ▀▄█".contains(c)), "row {r} is not big text: {text:?}");
        assert!(text.contains(['▀', '▄', '█']), "row {r} is empty");
    }
    let reference = reference(&asset, GlyphTier::UnicodeBlocks, cols, rows, 3);
    let above = usize::from(cols) * usize::from(rows - bands);
    assert_eq!(&g.as_slice()[..above], &reference[..above], "rows above the bands untouched");

    p.set_progress_overlay(false);
    p.set_hint_overlay(false);
    let stats = p.render_present(&mut backend, 3).unwrap();
    assert_eq!(stats.cells_damaged, u32::from(cols) * u32::from(rows), "hide → full repaint");
    assert_eq!(p.grid().as_slice(), &reference[..], "hidden → the plain picture");

    // Same grid on the ASCII tier: the one-cell rows, printable ASCII.
    let mut backend = SimBackend::new(cols, rows);
    let mut p = player(&asset, GlyphTier::Ascii);
    p.reflow(&mut backend, cols, rows);
    p.set_hint_overlay(true);
    p.set_info_overlay(Some(INFO));
    p.render_present(&mut backend, 3).unwrap();
    assert!(row(p.grid(), rows - 3).ends_with(" 320x90 cells "));
    assert!(row(p.grid(), rows - 2).is_ascii());
}

/// Every overlay on, every tier, from 1x1 through the tier thresholds: the
/// player renders without panicking, and below 32x9 the enlarge card still
/// owns the screen apart from the bottom row.
#[test]
fn every_overlay_on_tiny_and_threshold_grids() {
    let asset = build_fixture(Fixture::GradientMotion);
    let sizes = [
        (1u16, 1u16), (2, 1), (1, 2), (10, 3), (10, 4), (31, 9), (32, 8), (32, 9), (80, 24),
        (239, 36), (240, 35), (240, 36), (241, 37), (1000, 1000),
    ];
    for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks] {
        for (cols, rows) in sizes {
            let mut backend = SimBackend::new(cols, rows);
            let mut p = player(&asset, tier);
            p.reflow(&mut backend, cols, rows);
            p.set_progress_overlay(true);
            p.set_dial_overlay(Some(("edge on", 32, 255)));
            p.set_hint_overlay(true);
            p.set_info_overlay(Some(INFO));
            p.render_present(&mut backend, 1).unwrap();
            assert_eq!((p.grid().cols(), p.grid().rows()), (cols, rows));
            if (cols, rows) == (10, 3) {
                // Too small to play: the card, with the dial row under it.
                assert!(row(p.grid(), 2).starts_with(" edge on"), "{:?}", row(p.grid(), 2));
            }
        }
    }
}
