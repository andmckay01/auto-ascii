//! Versioned evaluation report schema and JSON serialization.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::stats::{DamageStats, StageTimesMs};

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    pub schema_version: u32,
    pub generator: String,
    pub clips: Vec<ClipReport>,
}

impl EvalReport {
    pub fn new(generator: impl Into<String>) -> EvalReport {
        EvalReport { schema_version: SCHEMA_VERSION, generator: generator.into(), clips: Vec::new() }
    }

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipReport {
    pub name: String,
    pub frames: u32,
    pub fps: f64,
    pub grid_cols: u16,
    pub grid_rows: u16,
    pub metrics: ClipMetrics,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipMetrics {
    pub ssim: Option<f64>,
    pub flicker_switches_per_cell_sec: Option<f64>,
    pub edge_f1: Option<f64>,
    pub edge_precision: Option<f64>,
    pub edge_recall: Option<f64>,
    pub shot_count: Option<u32>,
    pub cut_count: Option<u32>,
    pub keyframe_count: Option<u32>,
    pub asset_bytes: Option<u64>,
    pub damage_by_tier: BTreeMap<String, DamageStats>,
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
