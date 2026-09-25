use auto_ascii::pipeline::Player;
use auto_ascii_eval::fixtures::{Fixture, FixtureRenderer, GoldenPalette, build_fixture};
use auto_ascii_format::AsciiReader;
use auto_ascii_term::SimBackend;

const TERMS: &[(u16, u16)] = &[
    (80, 24),
    (48, 12),
    (206, 58),
    (60, 18),
    (320, 90),
    (213, 58),
    (20, 5),
    (80, 24),
];

const FRAMES: &[u32] = &[0, 1, 2, 36, 37, 12];

fn assert_grid_parity(fixture: Fixture, palette: GoldenPalette) {
    let asset = build_fixture(fixture);
    let reader = AsciiReader::open(&asset).expect("fixture asset is valid");
    let (tier, depth) = palette.config();
    let mut player = Player::new(reader, auto_ascii_core::DEFAULT_CELL_ASPECT, false, depth, tier)
        .expect("player over the fixture");
    let mut backend = SimBackend::new(80, 24);

    for &(cols, rows) in TERMS {
        player.reflow(&mut backend, cols, rows);
        let mut reference = FixtureRenderer::new(&asset, palette);
        reference.reflow(cols, rows);

        if player.viewport().is_none() {
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
