//! `auto-ascii-eval` — quantitative metrics for the eval harness (PLAN §6, M2).
//!
//! Library-only: pure measurement primitives plus the versioned JSON report
//! schema. The driver that builds assets, runs the player against
//! `SimBackend` and writes `runs/*.json` + HTML contact sheets is
//! `auto-ascii-factory eval` (PLAN §5, M2 item B) — this crate deliberately does
//! no I/O beyond serde.
//!
//! The measurement chain for the §6 quality metric:
//!
//! ```text
//! Grid<Cell> ── rasterize (ink-coverage · fg/bg luma) ──▶ GrayImage
//! source Y  ── auto-ascii-core Resampler (same box-average) ──▶ GrayImage
//!                       └────────── ssim ──────────┘
//! ```
//!
//! Everything here is deterministic for fixed inputs; scores land in
//! `runs/` JSON (data, not test goldens).
//!
//! The [`fixtures`] module (M2 items C/D) generates the deterministic
//! synthetic ASCI assets that the committed goldens and the resize fuzzer
//! run against — pure integer plane generators through `AsciiWriter`, all in
//! memory (the repo rule: committed tests never depend on the corpus mp4s).

pub mod compare;
pub mod coverage;
pub mod edge;
pub mod fixtures;
pub mod flicker;
pub mod raster;
pub mod report;
pub mod ssim;
pub mod stats;

pub use compare::{CompareReport, MetricDelta, Tolerances, compare_reports};
pub use coverage::{CONSERVATIVE_COVERAGE, CoverageTable};
pub use edge::{
    CANNY_HIGH, CANNY_LOW, EDGE_MATCH_TOLERANCE, EdgeMask, EdgeScore, canny_edge_truth,
    edge_cells_from_layers, edge_f1,
};
pub use flicker::FlickerAccum;
pub use raster::{GrayImage, RasterOptions, luma8, rasterize};
pub use report::{ClipMetrics, ClipReport, EvalReport, SCHEMA_VERSION};
pub use ssim::{SSIM_SIGMA, SSIM_WINDOW, downscale_ssim, ssim};
pub use stats::{
    DamageStats, Stage, StageAccum, StageStat, StageTimesMs, aggregate_frame_stats,
};
