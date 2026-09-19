//! End-to-end metric-chain test over synthetic data (no corpus, PLAN §6):
//! source plane → engine resample → ramp compose (player-style) → coverage
//! rasterize → downscale-SSIM, plus the report/compare loop those metrics
//! feed.

use auto_ascii_core::{Cell, Grid, Rgb, ramp};
use auto_ascii_eval::{
    ClipMetrics, ClipReport, CoverageTable, EvalReport, FlickerAccum, GrayImage, RasterOptions,
    Tolerances, compare_reports, downscale_ssim, rasterize,
};

const SRC_W: u16 = 480;
const SRC_H: u16 = 270;

/// Deterministic synthetic source: smooth diagonal gradient with a bright
/// disk — structure at both scales.
fn synthetic_source() -> Vec<u8> {
    let mut data = Vec::with_capacity(SRC_W as usize * SRC_H as usize);
    for y in 0..SRC_H as i32 {
        for x in 0..SRC_W as i32 {
            let grad = (x * 160 / (SRC_W as i32 - 1)) + (y * 60 / (SRC_H as i32 - 1));
            let (dx, dy) = (x - 340, y - 90);
            let disk = if dx * dx + dy * dy < 45 * 45 { 60 } else { 0 };
            data.push((grad + disk).clamp(0, 255) as u8);
        }
    }
    data
}

/// Player-style L0 compose: resample source luma to the grid, ramp glyph +
/// gray fg per cell (the M0/M1 render path in miniature).
fn compose(src: &[u8], cols: u16, rows: u16, invert: bool) -> Grid<Cell> {
    let mut rs = auto_ascii_core::Resampler::build(SRC_W, SRC_H, cols, rows);
    let mut luma = vec![0u8; cols as usize * rows as usize];
    rs.apply(src, &mut luma);
    let r = ramp::base_ramp_for_cols(cols);
    let mut grid: Grid<Cell> = Grid::new(cols, rows);
    for row in 0..rows {
        for col in 0..cols {
            let mut n = luma[row as usize * cols as usize + col as usize];
            if invert {
                n = 255 - n;
            }
            grid.set(col, row, Cell::new(ramp::ramp_glyph(r, n), Rgb::gray(n), Rgb::BLACK));
        }
    }
    grid
}

#[test]
fn downscale_ssim_full_chain_scores_sane_and_orders_renders() {
    let src = synthetic_source();
    let table = CoverageTable::conservative();
    let opts = RasterOptions::default(); // 1×2 px per cell (1:2 aspect)

    let good = rasterize(&compose(&src, 150, 40, false), table, &opts);
    let bad = rasterize(&compose(&src, 150, 40, true), table, &opts);
    assert_eq!((good.w(), good.h()), (150, 80));

    let s_good = downscale_ssim(&good, &src, SRC_W, SRC_H);
    let s_bad = downscale_ssim(&bad, &src, SRC_W, SRC_H);

    assert!(s_good > 0.5, "faithful render scores structurally: {s_good}");
    assert!(s_good < 1.0);
    // Inversion flips window covariances but SSIM's C2 stabilizer softens the
    // penalty in low-variance windows — a clear ordering margin, not a chasm.
    assert!(
        s_good > s_bad + 0.15,
        "inverted render must score clearly lower: good {s_good} vs bad {s_bad}"
    );
}

#[test]
fn static_compose_has_zero_flicker_across_frames() {
    let src = synthetic_source();
    let mut acc = FlickerAccum::new();
    for _ in 0..10 {
        acc.push(&compose(&src, 100, 30, false));
    }
    assert_eq!(acc.switches(), 0);
    assert_eq!(acc.score(30.0), Some(0.0));
}

#[test]
fn report_compare_loop_catches_ssim_regression() {
    let src = synthetic_source();
    let table = CoverageTable::conservative();
    let opts = RasterOptions::default();

    let clip = |grid: &Grid<Cell>| {
        let img = rasterize(grid, table, &opts);
        ClipReport {
            name: "synthetic".into(),
            frames: 1,
            fps: 30.0,
            grid_cols: grid.cols(),
            grid_rows: grid.rows(),
            metrics: ClipMetrics {
                ssim: Some(downscale_ssim(&img, &src, SRC_W, SRC_H)),
                ..ClipMetrics::default()
            },
        }
    };

    let mut baseline = EvalReport::new("test");
    baseline.clips.push(clip(&compose(&src, 150, 40, false)));
    let mut regressed = EvalReport::new("test");
    regressed.clips.push(clip(&compose(&src, 150, 40, true)));

    let tol = Tolerances::default();
    let same = compare_reports(&baseline, &baseline, &tol);
    assert!(same.pass);

    let cmp = compare_reports(&regressed, &baseline, &tol);
    assert!(!cmp.pass, "inverted render must trip the ssim tolerance");
    assert_eq!(cmp.failures().next().unwrap().metric, "ssim");

    // And the whole loop survives a JSON roundtrip (the runs/*.json path).
    let back = EvalReport::from_json(&baseline.to_json()).unwrap();
    assert_eq!(back, baseline);
}

#[test]
fn viewport_crop_excludes_letterbox_from_the_metric() {
    // A letterboxed grid: BLANK pad rows above/below the composed viewport.
    let src = synthetic_source();
    let table = CoverageTable::conservative();
    let opts = RasterOptions::default();

    let inner = compose(&src, 80, 20, false);
    let mut padded: Grid<Cell> = Grid::new(80, 24); // 2 pad rows top + bottom
    for row in 0..20u16 {
        for col in 0..80u16 {
            padded.set(col, row + 2, inner.get(col, row));
        }
    }

    let full = rasterize(&padded, table, &opts);
    let cropped = full.crop(0, 4, 80, 40); // viewport region in px (1×2 per cell)
    let direct = rasterize(&inner, table, &opts);
    assert_eq!(cropped, direct);

    let s_cropped = downscale_ssim(&cropped, &src, SRC_W, SRC_H);
    let s_full = downscale_ssim(&full, &src, SRC_W, SRC_H);
    assert!(
        s_cropped > s_full,
        "pads penalize the uncropped comparison: cropped {s_cropped} vs full {s_full}"
    );
}

#[test]
fn gray_image_from_raw_wraps_source_planes() {
    let src = synthetic_source();
    let img = GrayImage::from_raw(SRC_W, SRC_H, src.clone());
    assert_eq!(img.as_slice(), &src[..]);
}
