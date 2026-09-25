//! Versioned JSON report schema — one metrics JSON per eval run.
//!
//! This is the machine half of the agent socket: the factory's
//! `eval` subcommand emits an [`EvalReport`] to `runs/*.json`, an
//! orchestrating agent reads it, edits params.toml, and loops. Recorded
//! baselines (e.g. `runs/base.json`) are compared with
//! [`crate::compare_reports`].
//!
//! Schema rules:
//! - `schema_version` bumps on any breaking field change; additive optional
//!   fields don't bump it (readers ignore unknown fields, `Option`/default
//!   fields tolerate absence — same posture as the ASCI chunk registry).
//! - No timestamps or host info in the report body: a rerun on identical
//!   inputs must produce identical JSON (determinism guard friendliness).
//!   Provenance belongs in the run *filename* and the git history.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::stats::{DamageStats, StageTimesMs};

/// Current report schema version. v2 adds the edge-F1 metric family
/// (`edge_f1`/`edge_precision`/`edge_recall`) to `ClipMetrics` as a
/// deliberate renderer-generation marker — the fields themselves are
/// additive, so v1 reports still deserialize (serde defaults);
/// [`crate::compare_reports`] accepts an OLDER-versioned baseline with an
/// informational note and fails only on a NEWER/unknown baseline version.
pub const SCHEMA_VERSION: u32 = 2;

/// One eval run over a corpus (or a single clip).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    pub schema_version: u32,
    /// Producing tool, e.g. `"auto-ascii-eval 0.1.0"` — informational.
    pub generator: String,
    pub clips: Vec<ClipReport>,
}

impl EvalReport {
    pub fn new(generator: impl Into<String>) -> EvalReport {
        EvalReport { schema_version: SCHEMA_VERSION, generator: generator.into(), clips: Vec::new() }
    }

    /// Deterministic pretty JSON (BTreeMap keys are sorted; field order is
    /// declaration order), newline-terminated.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("EvalReport serializes");
        s.push('\n');
        s
    }

    pub fn from_json(s: &str) -> Result<EvalReport, serde_json::Error> {
        serde_json::from_str(s)
    }

    pub fn clip(&self, name: &str) -> Option<&ClipReport> {
        self.clips.iter().find(|c| c.name == name)
    }
}

/// Per-clip results.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipReport {
    /// Stable clip identifier (corpus file stem).
    pub name: String,
    /// Frames evaluated.
    pub frames: u32,
    /// Playback rate the time-based metrics were computed at.
    pub fps: f64,
    /// Terminal grid the render ran on.
    pub grid_cols: u16,
    pub grid_rows: u16,
    pub metrics: ClipMetrics,
}

/// The per-clip metric set. Every field is optional/defaultable so partial
/// runs (e.g. a tier sweep without SSIM) still serialize, and future metrics
/// are additive.
///
/// The asset-structure fields (`shot_count`/`cut_count`/`keyframe_count`/
/// `asset_bytes`) exist so the baseline compare sees FACTORY-tunable
/// regressions, not only render quality: a shot threshold that kills cut
/// detection, a keyframe cadence change or a zstd downgrade must trip the
/// gate, and render-side metrics alone are nearly blind to them because the
/// player normalizes through whatever levels the factory wrote.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipMetrics {
    /// Downscale-SSIM, mean over sampled frames (higher is better).
    pub ssim: Option<f64>,
    /// Glyph switches per cell per second on static segments (lower is
    /// better; gate ≤ 2).
    pub flicker_switches_per_cell_sec: Option<f64>,
    /// Edge F1 vs source Canny at grid resolution (higher is
    /// better) — mean over sampled frames, 1-cell tolerance ring
    /// ([`crate::edge`] documents thresholds + empty-frame conventions).
    pub edge_f1: Option<f64>,
    /// Mean cell-level edge precision over the same samples (informational —
    /// only `edge_f1` is gated by the baseline compare).
    pub edge_precision: Option<f64>,
    /// Mean cell-level edge recall over the same samples (informational).
    pub edge_recall: Option<f64>,
    /// NORM shot records in the asset (factory shot detection output).
    pub shot_count: Option<u32>,
    /// CUT-flagged shots (hard cuts the player resets hysteresis on).
    pub cut_count: Option<u32>,
    /// Keyframes in the asset (FIDX roster) — drops when `keyframe_ivl`
    /// inflates, taking seek latency with it.
    pub keyframe_count: Option<u32>,
    /// Asset file size in bytes (encode-profile bloat detector).
    pub asset_bytes: Option<u64>,
    /// Damage/bytes aggregation keyed by tier name
    /// (`"truecolor" | "256" | "16" | "mono"` — `ColorTier` canonical forms).
    pub damage_by_tier: BTreeMap<String, DamageStats>,
    /// Per-stage frame times.
    pub stage_ms: Option<StageTimesMs>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::aggregate_frame_stats;
    use auto_ascii_term::FrameStats;

    fn sample_report() -> EvalReport {
        let mut r = EvalReport::new("auto-ascii-eval test");
        let stats =
            [FrameStats { bytes: 1200, cells_damaged: 40, write_ns: 1_500_000, dropped: false }];
        let mut metrics = ClipMetrics {
            ssim: Some(0.8125),
            flicker_switches_per_cell_sec: Some(0.25),
            edge_f1: Some(0.5625),
            edge_precision: Some(0.75),
            edge_recall: Some(0.45),
            ..ClipMetrics::default()
        };
        metrics.damage_by_tier.insert("truecolor".into(), aggregate_frame_stats(&stats, 2400, 30.0));
        r.clips.push(ClipReport {
            name: "grass".into(),
            frames: 194,
            fps: 30.0,
            grid_cols: 300,
            grid_rows: 80,
            metrics,
        });
        r
    }

    #[test]
    fn json_roundtrip_is_lossless() {
        let r = sample_report();
        let json = r.to_json();
        assert!(json.contains("\"schema_version\": 2"));
        assert!(json.contains("\"edge_f1\": 0.5625"));
        assert!(json.ends_with('\n'));
        let back = EvalReport::from_json(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn v1_report_still_deserializes() {
        let json = r#"{
            "schema_version": 1,
            "generator": "auto-ascii-factory 0.1.0",
            "clips": [{
                "name": "grass", "frames": 194, "fps": 30.0,
                "grid_cols": 300, "grid_rows": 80,
                "metrics": { "ssim": 0.64 }
            }]
        }"#;
        let r = EvalReport::from_json(json).unwrap();
        assert_eq!(r.schema_version, 1);
        let m = &r.clips[0].metrics;
        assert_eq!(m.ssim, Some(0.64));
        assert_eq!((m.edge_f1, m.edge_precision, m.edge_recall), (None, None, None));
    }

    #[test]
    fn serialization_is_deterministic() {
        assert_eq!(sample_report().to_json(), sample_report().to_json());
    }

    #[test]
    fn missing_optional_metrics_deserialize_as_default() {
        let json = r#"{
            "schema_version": 1,
            "generator": "x",
            "clips": [{
                "name": "c", "frames": 1, "fps": 30.0,
                "grid_cols": 80, "grid_rows": 24,
                "metrics": {}
            }]
        }"#;
        let r = EvalReport::from_json(json).unwrap();
        assert_eq!(r.clips[0].metrics, ClipMetrics::default());
        assert!(r.clip("c").is_some());
        assert!(r.clip("missing").is_none());
    }
}
