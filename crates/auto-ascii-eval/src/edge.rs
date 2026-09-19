//! Edge F1 (PLAN §6): cell-level precision/recall/F1 of the renderer's edge
//! layer against **Canny on the SOURCE frame at grid resolution**.
//!
//! Ground truth is deliberately *never* the factory's own E plane (no
//! self-grading): the driver feeds the raw source luma (the fps-normalized
//! ingest stream, before any factory tunable touches it), this module
//! downscales it to the viewport grid through auto-ascii-core's own [`Resampler`]
//! (the same box-average semantics the player uses) and runs
//! [`imageproc::edges::canny`] on the result ([`canny_edge_truth`]).
//!
//! Prediction is the set of cells where the compositor's **edge layer won**
//! — observable through the render-metadata `LayerMask` (a `Grid<u8>` of
//! `auto_ascii_core::compose::layer` ids filled by `compose_frame_masked`;
//! [`edge_cells_from_layers`] crops the viewport and selects
//! [`layer::EDGE`]).
//!
//! Matching uses a **1-cell tolerance ring** ([`EDGE_MATCH_TOLERANCE`],
//! Chebyshev distance): glyph quantization means a contour legitimately
//! lands one cell off the downscaled Canny ridge, and the renderer's edge
//! magnitude is unthinned by design (PLAN §4) — off-by-one cells count as
//! hits on both the precision and the recall side. Scores are NaN-free by
//! construction (see [`edge_f1`] for the empty-mask conventions).

use crate::raster::GrayImage;
use imageproc::edges::canny;
use auto_ascii_core::compose::layer;
use auto_ascii_core::{Grid, Resampler, Viewport};

/// Canny hysteresis thresholds (imageproc: Sobel gradient magnitude on the
/// internally Gaussian-blurred image, σ = 1.4). Fixed and eval-owned — like
/// the SSIM reference percentiles they must stay independent of the params
/// under test. Chosen on the M3 corpus at the 300×80 reference grid so truth
/// density lands in the "real contours" band (~4–10% of cells): low 60 /
/// high 140 keeps the sheep outlines and silhouette limbs while dropping
/// grass micro-texture.
pub const CANNY_LOW: f32 = 60.0;
pub const CANNY_HIGH: f32 = 140.0;

/// Tolerance ring radius (cells, Chebyshev) used by the report metric.
pub const EDGE_MATCH_TOLERANCE: u16 = 1;

/// A binary cell mask at grid resolution (row-major, `w × h`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeMask {
    w: u16,
    h: u16,
    bits: Vec<bool>,
}

impl EdgeMask {
    /// All-false mask. Zero-sized masks are legal (empty frame conventions
    /// apply — see [`edge_f1`]).
    pub fn new(w: u16, h: u16) -> EdgeMask {
        EdgeMask { w, h, bits: vec![false; w as usize * h as usize] }
    }

    pub fn w(&self) -> u16 {
        self.w
    }

    pub fn h(&self) -> u16 {
        self.h
    }

    pub fn get(&self, x: u16, y: u16) -> bool {
        assert!(x < self.w && y < self.h, "EdgeMask::get({x},{y}) out of {}x{}", self.w, self.h);
        self.bits[y as usize * self.w as usize + x as usize]
    }

    pub fn set(&mut self, x: u16, y: u16, v: bool) {
        assert!(x < self.w && y < self.h, "EdgeMask::set({x},{y}) out of {}x{}", self.w, self.h);
        self.bits[y as usize * self.w as usize + x as usize] = v;
    }

    /// Number of set cells.
    pub fn count(&self) -> u32 {
        self.bits.iter().map(|&b| u32::from(b)).sum()
    }

    /// Any set cell within Chebyshev distance `tol` of `(x, y)`?
    fn hit_near(&self, x: u16, y: u16, tol: u16) -> bool {
        let x0 = x.saturating_sub(tol);
        let y0 = y.saturating_sub(tol);
        let x1 = (x + tol).min(self.w - 1);
        let y1 = (y + tol).min(self.h - 1);
        (y0..=y1).any(|yy| (x0..=x1).any(|xx| self.get(xx, yy)))
    }
}

/// Ground truth: Canny on the source luma **downscaled to grid resolution**
/// (PLAN §6 "source Canny at grid resolution").
///
/// `src` is the raw source luma at `src_w × src_h` (the factory's ingest
/// scale, e.g. 480×270); the downscale to `grid_w × grid_h` viewport cells
/// runs through auto-ascii-core's [`Resampler`] — the identical box-average the
/// player applies, so truth and prediction see the same spatial quantization
/// (grid cells are ~1:2 anisotropic; Canny at cell granularity is the metric
/// definition, not a raster-space edge map). Thresholds: [`CANNY_LOW`] /
/// [`CANNY_HIGH`], documented above.
///
/// # Panics
/// If `src` is shorter than `src_w × src_h` or any dimension is zero.
pub fn canny_edge_truth(
    src: &[u8],
    src_w: u16,
    src_h: u16,
    grid_w: u16,
    grid_h: u16,
) -> EdgeMask {
    assert!(src_w > 0 && src_h > 0 && grid_w > 0 && grid_h > 0, "degenerate dims");
    assert!(src.len() >= src_w as usize * src_h as usize, "source luma too short");
    let mut small = vec![0u8; grid_w as usize * grid_h as usize];
    Resampler::build(src_w, src_h, grid_w, grid_h)
        .apply(&src[..src_w as usize * src_h as usize], &mut small);
    let img = image::GrayImage::from_raw(u32::from(grid_w), u32::from(grid_h), small)
        .expect("buffer sized above");
    let edges = canny(&img, CANNY_LOW, CANNY_HIGH);
    let mut mask = EdgeMask::new(grid_w, grid_h);
    for (i, px) in edges.into_raw().iter().enumerate() {
        mask.bits[i] = *px != 0;
    }
    mask
}

/// Prediction: viewport cells whose winning layer was the edge layer, from a
/// render-metadata `LayerMask` (`Grid<u8>` of `auto_ascii_core::compose::layer`
/// ids at full terminal dimensions — pads are cropped here).
///
/// # Panics
/// If the grid is smaller than the viewport + pads.
pub fn edge_cells_from_layers(layers: &Grid<u8>, vp: &Viewport) -> EdgeMask {
    assert!(
        layers.cols() >= vp.pad_left + vp.cols && layers.rows() >= vp.pad_top + vp.rows,
        "layer mask smaller than viewport"
    );
    let mut mask = EdgeMask::new(vp.cols, vp.rows);
    for y in 0..vp.rows {
        let row = layers.row(vp.pad_top + y);
        for x in 0..vp.cols {
            if row[(vp.pad_left + x) as usize] == layer::EDGE {
                mask.set(x, y, true);
            }
        }
    }
    mask
}

/// Cell-level edge score. All fields are finite for every input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeScore {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    /// Ground-truth edge cells in the frame.
    pub truth_cells: u32,
    /// Predicted (edge-layer-won) cells in the frame.
    pub predicted_cells: u32,
}

/// Precision/recall/F1 with a Chebyshev tolerance ring of `tol` cells
/// (off-by-`tol` counts as a hit on BOTH sides; the report uses
/// [`EDGE_MATCH_TOLERANCE`] = 1).
///
/// NaN-safe empty-mask conventions (documented contract):
/// - no truth, no prediction → precision = recall = f1 = 1.0 (nothing to
///   find, nothing drawn — perfect agreement; e.g. a flat black frame);
/// - no truth, some prediction → precision = 0 (every prediction false),
///   recall = 1 (vacuous), f1 = 0;
/// - some truth, no prediction → precision = 1 (vacuous), recall = 0, f1 = 0.
///
/// # Panics
/// On dimension mismatch.
pub fn edge_f1(truth: &EdgeMask, pred: &EdgeMask, tol: u16) -> EdgeScore {
    assert_eq!(
        (truth.w, truth.h),
        (pred.w, pred.h),
        "edge_f1: truth {}x{} vs pred {}x{}",
        truth.w,
        truth.h,
        pred.w,
        pred.h
    );
    let truth_cells = truth.count();
    let predicted_cells = pred.count();

    let matched_in = |from: &EdgeMask, against: &EdgeMask| -> u32 {
        let mut n = 0u32;
        for y in 0..from.h {
            for x in 0..from.w {
                if from.get(x, y) && against.hit_near(x, y, tol) {
                    n += 1;
                }
            }
        }
        n
    };

    let precision = if predicted_cells == 0 {
        1.0
    } else {
        f64::from(matched_in(pred, truth)) / f64::from(predicted_cells)
    };
    let recall = if truth_cells == 0 {
        1.0
    } else {
        f64::from(matched_in(truth, pred)) / f64::from(truth_cells)
    };
    let f1 = if truth_cells == 0 && predicted_cells == 0 {
        1.0
    } else if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    EdgeScore { precision, recall, f1, truth_cells, predicted_cells }
}

/// Convenience for tests/drivers: mask from a grayscale image (nonzero = edge).
pub fn mask_from_gray(img: &GrayImage) -> EdgeMask {
    let mut mask = EdgeMask::new(img.w(), img.h());
    for (i, &v) in img.as_slice().iter().enumerate() {
        mask.bits[i] = v != 0;
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_mask(w: u16, h: u16, x0: u16, y0: u16, x1: u16, y1: u16) -> EdgeMask {
        // Rectangle OUTLINE cells (inclusive corners).
        let mut m = EdgeMask::new(w, h);
        for x in x0..=x1 {
            m.set(x, y0, true);
            m.set(x, y1, true);
        }
        for y in y0..=y1 {
            m.set(x0, y, true);
            m.set(x1, y, true);
        }
        m
    }

    /// Synthetic known-edge fixture: a drawn rectangle's Canny truth matched
    /// by an exact-match renderer scores a perfect 1.0.
    #[test]
    fn drawn_rectangle_exact_match_is_perfect() {
        // Source image 4× the grid: filled bright rectangle on dark ground.
        let (sw, sh, gw, gh) = (128u16, 80u16, 32u16, 20u16);
        let mut src = vec![20u8; sw as usize * sh as usize];
        for y in 24..56usize {
            for x in 32..96usize {
                src[y * sw as usize + x] = 220;
            }
        }
        let truth = canny_edge_truth(&src, sw, sh, gw, gh);
        assert!(truth.count() > 0, "canny found no edges on a hard rectangle");
        // Truth cells hug the rectangle border (8..=24 × 6..=14 in grid
        // coords) within one cell — sanity that thresholds see the contour.
        for y in 0..gh {
            for x in 0..gw {
                if truth.get(x, y) {
                    let near_v = (7..=9).contains(&x) || (23..=25).contains(&x);
                    let near_h = (5..=7).contains(&y) || (13..=15).contains(&y);
                    assert!(
                        (near_v && (5..=15).contains(&y)) || (near_h && (7..=25).contains(&x)),
                        "stray truth cell at ({x},{y})"
                    );
                }
            }
        }
        // Exact-match renderer: prediction == truth → perfect at tol 0 and 1.
        let s0 = edge_f1(&truth, &truth, 0);
        assert_eq!((s0.precision, s0.recall, s0.f1), (1.0, 1.0, 1.0));
        let s1 = edge_f1(&truth, &truth, EDGE_MATCH_TOLERANCE);
        assert_eq!(s1.f1, 1.0);
        assert_eq!(s1.truth_cells, truth.count());
        assert_eq!(s1.predicted_cells, truth.count());
    }

    /// Off-by-one prediction: 0 at tol 0 unless overlapping, but still a
    /// perfect 1.0 under the 1-cell tolerance ring.
    #[test]
    fn shifted_by_one_is_perfect_under_tolerance_ring() {
        let truth = rect_mask(32, 20, 8, 5, 24, 14);
        let shifted = rect_mask(32, 20, 9, 6, 25, 15); // +1 in both axes
        let s1 = edge_f1(&truth, &shifted, EDGE_MATCH_TOLERANCE);
        assert_eq!((s1.precision, s1.recall, s1.f1), (1.0, 1.0, 1.0));
        // Two cells apart breaks the ring.
        let far = rect_mask(32, 20, 10, 7, 26, 16);
        let s2 = edge_f1(&truth, &far, EDGE_MATCH_TOLERANCE);
        assert!(s2.f1 < 1.0, "tol 1 must not absorb a 2-cell shift");
        // …but tol 2 does (ring radius is honored exactly).
        assert_eq!(edge_f1(&truth, &far, 2).f1, 1.0);
    }

    /// Empty-mask conventions: every combination is defined and NaN-free.
    #[test]
    fn empty_masks_are_nan_safe() {
        let empty = EdgeMask::new(16, 10);
        let some = rect_mask(16, 10, 4, 3, 10, 7);

        let both = edge_f1(&empty, &empty, 1);
        assert_eq!((both.precision, both.recall, both.f1), (1.0, 1.0, 1.0));

        let no_truth = edge_f1(&empty, &some, 1);
        assert_eq!((no_truth.precision, no_truth.recall, no_truth.f1), (0.0, 1.0, 0.0));

        let no_pred = edge_f1(&some, &empty, 1);
        assert_eq!((no_pred.precision, no_pred.recall, no_pred.f1), (1.0, 0.0, 0.0));

        for s in [both, no_truth, no_pred] {
            assert!(s.precision.is_finite() && s.recall.is_finite() && s.f1.is_finite());
        }
        // Zero true edges on a real frame: flat source → no Canny edges.
        let flat = vec![128u8; 64 * 36];
        let truth = canny_edge_truth(&flat, 64, 36, 32, 18);
        assert_eq!(truth.count(), 0);
        assert_eq!(edge_f1(&truth, &EdgeMask::new(32, 18), 1).f1, 1.0);
    }

    #[test]
    fn partial_overlap_scores_between() {
        let truth = rect_mask(32, 20, 8, 5, 24, 14);
        // Prediction: only the top half of the outline.
        let mut pred = EdgeMask::new(32, 20);
        for x in 8..=24 {
            pred.set(x, 5, true);
        }
        let s = edge_f1(&truth, &pred, 1);
        assert_eq!(s.precision, 1.0, "every predicted cell lies on the truth");
        assert!(s.recall > 0.0 && s.recall < 1.0);
        assert!(s.f1 > 0.0 && s.f1 < 1.0);
    }

    #[test]
    fn layer_mask_crops_viewport_and_selects_edge_layer() {
        let vp = Viewport { cols: 6, rows: 4, pad_left: 2, pad_right: 1, pad_top: 1, pad_bottom: 0 };
        let mut layers: Grid<u8> = Grid::new(9, 5);
        layers.fill(layer::BASE);
        layers.set(2, 1, layer::EDGE); // viewport (0,0)
        layers.set(4, 2, layer::EDGE); // viewport (2,1)
        layers.set(3, 3, layer::HIGHLIGHT); // highlight is not an edge
        layers.set(0, 0, layer::EDGE); // in the pads: cropped away
        let mask = edge_cells_from_layers(&layers, &vp);
        assert_eq!((mask.w(), mask.h()), (6, 4));
        assert_eq!(mask.count(), 2);
        assert!(mask.get(0, 0) && mask.get(2, 1));
    }

    #[test]
    #[should_panic(expected = "edge_f1")]
    fn dim_mismatch_panics() {
        edge_f1(&EdgeMask::new(4, 4), &EdgeMask::new(5, 4), 1);
    }
}
