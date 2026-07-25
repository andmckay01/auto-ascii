//! Cell-grid goldens (PLAN §6, M2 item C): insta snapshots of the rendered
//! `Grid<Cell>` for 3 synthetic fixture assets × grids 80×24 / 206×58 /
//! 320×90 × 3 palettes (ascii-coarse, ascii-fine, mono glyph-only) —
//! 27 snapshots. Serialization: glyph grid verbatim + FNV-1a 64 fg digest
//! per row ([`slpy_eval::fixtures::snapshot`]).
//!
//! Reproducible without the corpus (repo rule): fixtures are pure integer
//! generators through `SlpyWriter`. Re-bless deliberately with
//! `INSTA_UPDATE=always cargo test -p slpy-eval --test golden_grids`.

use slpy_eval::fixtures::{Fixture, FixtureRenderer, GoldenPalette, build_fixture, snapshot};

/// The §6 grid set (PLAN worked examples: 80×24 → 80×23 letterbox,
/// 206×58 exact, 320×90 exact).
const GRIDS: [(u16, u16); 3] = [(80, 24), (206, 58), (320, 90)];

/// One representative frame per fixture: mid-motion for gradient/checker,
/// and — the point of the hard-cut fixture — a frame INSIDE scene B, so the
/// golden pins the second shot's NORM levels, not just scene A's.
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
        let mut renderer = FixtureRenderer::new(&asset, palette);
        for (cols, rows) in GRIDS {
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

/// Non-snapshot sanity riders on the same renders: the hard-cut golden frame
/// really sits in shot 2 (distinct normalization), and the checkerboard is
/// box-averaged (mid-gray glyphs appear at non-integer scale), not aliased
/// to only the two extremes.
#[test]
fn golden_frames_are_meaningful() {
    // Hard cut: frame 40 is past the CUT boundary.
    let asset = build_fixture(Fixture::HardCut);
    let reader = slpy_format::SlpyReader::open(&asset).unwrap();
    let shot = reader.shot_for_frame(snapshot_frame(Fixture::HardCut)).unwrap();
    assert_eq!(shot.first_frame, slpy_eval::fixtures::HARD_CUT_FRAME);
    assert!(shot.is_cut());

    // Checker at 80×24: the 192→80 box average must produce interior ramp
    // glyphs, not just the ' '/darkest and '@'/brightest extremes.
    let asset = build_fixture(Fixture::CheckerDrift);
    let mut r = FixtureRenderer::new(&asset, GoldenPalette::AsciiCoarse);
    r.reflow(80, 24);
    let grid = r.render(snapshot_frame(Fixture::CheckerDrift));
    let mid_row: Vec<char> = grid.row(10).iter().map(|c| c.glyph()).collect();
    assert!(
        mid_row.iter().any(|&g| g != ' ' && g != '@'),
        "checker must average to interior glyphs, got {mid_row:?}"
    );
}
