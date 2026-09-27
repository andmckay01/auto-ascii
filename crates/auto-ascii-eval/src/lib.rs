//! Evaluation metrics, report schemas and deterministic fixture support.

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
