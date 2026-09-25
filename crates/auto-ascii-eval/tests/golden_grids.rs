use auto_ascii_eval::fixtures::{Fixture, FixtureRenderer, GoldenPalette, build_fixture, snapshot};

const GRIDS: [(u16, u16); 4] = [(48, 12), (80, 24), (206, 58), (320, 90)];

fn snapshot_frame(fixture: Fixture) -> u32 {
    match fixture {
        Fixture::GradientMotion => 10,
        Fixture::HardCut => 40,
        Fixture::CheckerDrift => 7,
    }
}

fn golden_all_grids_and_palettes(fixture: Fixture) {
    let asset = build_fixture(fixture);
    let frame = snapshot_frame(fixture);
    for palette in GoldenPalette::ALL {
        for (cols, rows) in GRIDS {
            let mut renderer = FixtureRenderer::new(&asset, palette);
            renderer.reflow(cols, rows);
            let vp = renderer.viewport();
            let grid = renderer.render(frame);
            let title = format!("{} frame {frame}", fixture.name());
            let s = snapshot(&title, (cols, rows), palette, vp, grid);
            insta::assert_snapshot!(
                format!("{}_{cols}x{rows}_{}", fixture.name(), palette.name()),
                s
            );
        }
    }
}

#[test]
fn golden_gradient_motion() {
    golden_all_grids_and_palettes(Fixture::GradientMotion);
}

#[test]
fn golden_hard_cut() {
    golden_all_grids_and_palettes(Fixture::HardCut);
}

#[test]
fn golden_checker_drift() {
    golden_all_grids_and_palettes(Fixture::CheckerDrift);
}

#[test]
fn golden_frames_are_meaningful() {
    let asset = build_fixture(Fixture::HardCut);
    let reader = auto_ascii_format::AsciiReader::open(&asset).unwrap();
    let shot = reader.shot_for_frame(snapshot_frame(Fixture::HardCut)).unwrap();
    assert_eq!(shot.first_frame, auto_ascii_eval::fixtures::HARD_CUT_FRAME);
    assert!(shot.is_cut());

    let asset = build_fixture(Fixture::CheckerDrift);
    let mut r = FixtureRenderer::new(&asset, GoldenPalette::Ascii);
    r.reflow(80, 24);
    let grid = r.render(snapshot_frame(Fixture::CheckerDrift));
    let mid_row: Vec<char> = grid.row(10).iter().map(|c| c.glyph()).collect();
    assert!(
        mid_row.iter().any(|&g| g != ' ' && g != '@'),
        "checker must average to interior glyphs, got {mid_row:?}"
    );

    let mut uni = FixtureRenderer::new(&asset, GoldenPalette::Unicode);
    uni.reflow(206, 58);
    let has_halfblock = uni
        .render(snapshot_frame(Fixture::CheckerDrift))
        .as_slice()
        .iter()
        .any(|c| matches!(c.glyph(), '▀' | '▄'));
    assert!(has_halfblock, "unicode golden config must exercise half-blocks");

    let mut asc = FixtureRenderer::new(&asset, GoldenPalette::Ascii);
    asc.reflow(206, 58);
    let asc_grid = asc.render(snapshot_frame(Fixture::CheckerDrift));
    let has_subpos = asc_grid.as_slice().iter().any(|c| matches!(c.glyph(), '"' | '_'));
    assert!(has_subpos, "ascii golden config must exercise subposition glyphs");
    for c in asc_grid.as_slice() {
        let g = c.glyph();
        assert!(
            g == ' ' || g.is_ascii_graphic(),
            "ascii golden config emitted non-ASCII {g:?} (U+{:04X})",
            g as u32
        );
    }
}
