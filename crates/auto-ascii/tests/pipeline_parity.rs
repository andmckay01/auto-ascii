//! FixtureRenderer ⇄ Player parity (M2 review fix; re-keyed at M3).
//!
//! The committed goldens (36 insta cell-grid snapshots in auto-ascii-eval, 4
//! per-tier escape-stream goldens in auto-ascii-term) render through
//! `auto_ascii_eval::fixtures::FixtureRenderer` — a replay of the player pipeline
//! on public APIs. That replica is only trustworthy if it provably matches
//! the shipping renderer, so this test pins them cell-for-cell (glyph AND
//! colors) across every fixture, every M3 palette config (ascii / unicode /
//! mono — the player's own `select_palettes` key), a grid sweep covering
//! all golden grid sizes + both density bands, sequential/seek decode, the
//! hard cut (shot-change hysteresis reset on both sides), and mid-run
//! reflows driven through ONE persistent `Player` (so `Player::reflow`
//! state transitions — palette reselection, hysteresis realloc,
//! chroma/luma2 dst reallocs — are what is being verified).
//!
//! Because the M3 compositor is stateful (hysteresis), the reference
//! renders the exact same frame sequence as the player — parity covers the
//! temporal state trajectory, not just single frames.
//!
//! If this test fails, the shipping pipeline and the golden harness have
//! diverged: fix the pipeline or update FixtureRenderer + re-bless the
//! goldens DELIBERATELY — never let them drift apart silently.

use auto_ascii::pipeline::Player;
use auto_ascii_eval::fixtures::{Fixture, FixtureRenderer, GoldenPalette, build_fixture};
use auto_ascii_format::AsciiReader;
use auto_ascii_term::SimBackend;

/// Terminal sweep: golden grids (48×12 / 80×24 / 206×58 / 320×90), a coarse
/// mid-size (60×18), the PLAN worked example (213×58) and one below-minimum
/// size (20×5).
const TERMS: &[(u16, u16)] = &[
    (80, 24),
    (48, 12),
    (206, 58),
    (60, 18),
    (320, 90),
    (213, 58),
    (20, 5),
    (80, 24), // return trip: reflow back after the sweep
];

/// Frames per term: sequential roll (0,1,2), the HardCut boundary via seek
/// (36 mid-GOP, 37 sequential), and a jump back (12).
const FRAMES: &[u32] = &[0, 1, 2, 36, 37, 12];

fn assert_grid_parity(fixture: Fixture, palette: GoldenPalette) {
    let asset = build_fixture(fixture);
    let reader = AsciiReader::open(&asset).expect("fixture asset is valid");
    let (tier, depth) = palette.config();
    // Diff mode; the color depth mirrors the tier (mono skips chroma).
    let mut player = Player::new(reader, auto_ascii_core::DEFAULT_CELL_ASPECT, false, depth, tier)
        .expect("player over the fixture");
    let mut backend = SimBackend::new(80, 24);

    for &(cols, rows) in TERMS {
        player.reflow(&mut backend, cols, rows);
        // Fresh renderer per term: a correct reference for THIS geometry.
        // The player carries its state across the sweep — reflow bugs (stale
        // palette, unresized dst buffers, unreset hysteresis) surface as
        // parity mismatches here.
        let mut reference = FixtureRenderer::new(&asset, palette);
        reference.reflow(cols, rows);

        if player.viewport().is_none() {
            // Below 32×9 the player draws the enlarge card (UI) while the
            // pipeline replica blanks — documented divergence; the geometry
            // agreement is still asserted.
            assert!(reference.viewport().is_none(), "viewport minimum must agree");
            continue;
        }
        assert_eq!(player.viewport(), reference.viewport(), "{fixture:?} {cols}x{rows}");

        for &frame in FRAMES {
            player
                .render_present(&mut backend, frame)
                .unwrap_or_else(|e| panic!("{fixture:?} {cols}x{rows} f{frame}: {e}"));
            let expect = reference.render(frame);
            let got = player.grid();
            assert_eq!(
                (got.cols(), got.rows()),
                (expect.cols(), expect.rows()),
                "{fixture:?} {cols}x{rows} f{frame}: grid dims"
            );
            for row in 0..got.rows() {
                assert_eq!(
                    got.row(row),
                    expect.row(row),
                    "{fixture:?} {palette:?} {cols}x{rows} f{frame} row {row}: \
                     Player and FixtureRenderer diverged — the committed goldens \
                     no longer pin the shipping renderer"
                );
            }
        }
    }
}

#[test]
fn player_matches_fixture_renderer_ascii() {
    for fixture in Fixture::ALL {
        assert_grid_parity(fixture, GoldenPalette::Ascii);
    }
}

#[test]
fn player_matches_fixture_renderer_unicode() {
    for fixture in Fixture::ALL {
        assert_grid_parity(fixture, GoldenPalette::Unicode);
    }
}

#[test]
fn player_matches_fixture_renderer_mono() {
    for fixture in Fixture::ALL {
        assert_grid_parity(fixture, GoldenPalette::MonoGlyphOnly);
    }
}
