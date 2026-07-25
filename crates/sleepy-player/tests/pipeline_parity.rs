//! FixtureRenderer ⇄ Player parity (M2 review fix).
//!
//! The committed goldens (27 insta cell-grid snapshots in slpy-eval, 4
//! per-tier escape-stream goldens in slpy-term) render through
//! `slpy_eval::fixtures::FixtureRenderer` — a replay of the player pipeline
//! on public APIs. That replica is only trustworthy if it provably matches
//! the shipping renderer, so this test pins them cell-for-cell (glyph AND
//! colors) across every fixture, a grid sweep covering all golden grid
//! sizes + both ramp densities + the tier-golden size, color and mono
//! modes, sequential/seek decode, the hard cut, and mid-run reflows driven
//! through ONE persistent `Player` (so `Player::reflow` state transitions —
//! ramp reselection, chroma dst reallocs — are what is being verified).
//!
//! If this test fails, the shipping pipeline and the golden harness have
//! diverged: fix the pipeline or update FixtureRenderer + re-bless the
//! goldens DELIBERATELY — never let them drift apart silently.

use sleepy_player::pipeline::Player;
use slpy_core::ramp::FINE_MIN_COLS;
use slpy_eval::fixtures::{Fixture, FixtureRenderer, GoldenPalette, build_fixture};
use slpy_format::SlpyReader;
use slpy_term::SimBackend;

/// Terminal sweep: golden grids (80×24 / 206×58 / 320×90), the tier-golden
/// size (48×12, coarse ramp), a coarse mid-size (60×18), the PLAN worked
/// example (213×58) and one below-minimum size (20×5).
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

fn assert_grid_parity(fixture: Fixture, mono: bool) {
    let asset = build_fixture(fixture);
    let reader = SlpyReader::open(&asset).expect("fixture asset is valid");
    // Diff mode; want_color mirrors the tier (mono skips chroma entirely).
    let mut player = Player::new(reader, slpy_core::DEFAULT_CELL_ASPECT, false, !mono)
        .expect("player over the fixture");
    let mut backend = SimBackend::new(80, 24);

    for &(cols, rows) in TERMS {
        player.reflow(&mut backend, cols, rows);
        let palette = match player.viewport() {
            _ if mono => GoldenPalette::MonoGlyphOnly, // density-picked, like the player
            Some(vp) if vp.cols >= FINE_MIN_COLS => GoldenPalette::AsciiFine,
            _ => GoldenPalette::AsciiCoarse,
        };
        // Fresh renderer per term: a correct reference for THIS geometry.
        // The player carries its state across the sweep — reflow bugs (stale
        // ramp, unresized chroma dst) surface as parity mismatches here.
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
                    "{fixture:?} mono={mono} {cols}x{rows} f{frame} row {row}: \
                     Player and FixtureRenderer diverged — the committed goldens \
                     no longer pin the shipping renderer"
                );
            }
        }
    }
}

#[test]
fn player_matches_fixture_renderer_color() {
    for fixture in Fixture::ALL {
        assert_grid_parity(fixture, false);
    }
}

#[test]
fn player_matches_fixture_renderer_mono() {
    for fixture in Fixture::ALL {
        assert_grid_parity(fixture, true);
    }
}
