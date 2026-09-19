//! Cell-grid goldens (PLAN §6, M2 item C; re-keyed at M3): insta snapshots
//! of the rendered `Grid<Cell>` for 3 synthetic fixture assets × grids
//! 48×12 / 80×24 / 206×58 / 320×90 × 3 palette configs (ascii / unicode /
//! mono glyph-only) — 36 snapshots. Serialization: glyph grid verbatim +
//! FNV-1a 64 fg digest per row ([`auto_ascii_eval::fixtures::snapshot`]).
//!
//! M3 re-key rationale: palette configs are now the player's own selection
//! key (charset tier × color depth through `select_palettes`) instead of
//! hand-forced ramps, so the old ascii-coarse/ascii-fine axis collapsed
//! into `ascii` (density falls out of the viewport) and `unicode` joined
//! (half-blocks/quadrants are M3 acceptance surface). 48×12 keeps the
//! coarse density band covered (viewport 42 cols < 70).
//!
//! Reproducible without the corpus (repo rule): fixtures are pure integer
//! generators through `AsciiWriter`. Re-bless deliberately with
//! `INSTA_UPDATE=always cargo test -p auto-ascii-eval --test golden_grids`.

use auto_ascii_eval::fixtures::{Fixture, FixtureRenderer, GoldenPalette, build_fixture, snapshot};

/// The §6 grid set (PLAN worked examples: 80×24 → 80×23 letterbox,
/// 206×58 exact, 320×90 exact) + 48×12 (tier-golden size, coarse density).
const GRIDS: [(u16, u16); 4] = [(48, 12), (80, 24), (206, 58), (320, 90)];

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
        for (cols, rows) in GRIDS {
            // Fresh renderer per grid: goldens pin a deterministic render of
            // `frame` with no cross-grid hysteresis history (each starts
            // from reset state, like the player right after a reflow).
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

/// Non-snapshot sanity riders on the same renders: the hard-cut golden frame
/// really sits in shot 2 (distinct normalization), the checkerboard is
/// box-averaged (interior glyphs, not aliased extremes), and the M3
/// acceptance surface is demonstrably active — half-blocks on the unicode
/// tier, `"`/`_` subposition glyphs on the ascii tier (PLAN §3.3/§3.5), and
/// the ascii tier stays inside its ASCII repertoire (M4 review).
#[test]
fn golden_frames_are_meaningful() {
    // Hard cut: frame 40 is past the CUT boundary.
    let asset = build_fixture(Fixture::HardCut);
    let reader = auto_ascii_format::AsciiReader::open(&asset).unwrap();
    let shot = reader.shot_for_frame(snapshot_frame(Fixture::HardCut)).unwrap();
    assert_eq!(shot.first_frame, auto_ascii_eval::fixtures::HARD_CUT_FRAME);
    assert!(shot.is_cut());

    // Checker at 80×24: the 192→80 box average must produce interior ramp
    // glyphs, not just the blank/darkest and brightest extremes.
    let asset = build_fixture(Fixture::CheckerDrift);
    let mut r = FixtureRenderer::new(&asset, GoldenPalette::Ascii);
    r.reflow(80, 24);
    let grid = r.render(snapshot_frame(Fixture::CheckerDrift));
    let mid_row: Vec<char> = grid.row(10).iter().map(|c| c.glyph()).collect();
    assert!(
        mid_row.iter().any(|&g| g != ' ' && g != '@'),
        "checker must average to interior glyphs, got {mid_row:?}"
    );

    // M3 acceptance 4 (goldens demonstrate sub-cell structure): at 206×58
    // the checker's vertical taps split 2-px blocks, so the unicode config
    // must emit half-block pairs and the ascii config `"`/`_` subposition
    // glyphs somewhere in the viewport.
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
    // …and every glyph in that same render is CP437-safe ASCII. The
    // subposition assertion above is what makes this non-vacuous: it proves
    // the branch that used to emit U+203E OVERLINE actually ran here.
    for c in asc_grid.as_slice() {
        let g = c.glyph();
        assert!(
            g == ' ' || g.is_ascii_graphic(),
            "ascii golden config emitted non-ASCII {g:?} (U+{:04X})",
            g as u32
        );
    }
}
