//! Damage/bytes aggregation from backend `FrameStats` (damage rate and
//! bytes/frame per tier), and per-stage frame timers.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use auto_ascii_term::FrameStats;

/// Aggregated present-path statistics for one run at one tier — the JSON
/// form of a `Vec<FrameStats>` (one per presented frame).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DamageStats {
    /// Frames presented (including dropped ones).
    pub frames: u32,
    /// Frames flagged dropped by the backend.
    pub dropped_frames: u32,
    /// Total bytes written across all frames.
    pub bytes_total: u64,
    /// Mean bytes per frame.
    pub avg_bytes_per_frame: f64,
    /// Worst single frame.
    pub max_bytes_per_frame: u32,
    /// Mean fraction of grid cells damaged per frame (0..=1).
    pub avg_damage_rate: f64,
    /// Worst single-frame damage fraction (0..=1).
    pub max_damage_rate: f64,
    /// Mean simulated/real `write(2)` time, milliseconds.
    pub avg_write_ms: f64,
    /// `avg_bytes_per_frame · fps` — the sustained output byte rate.
    pub bytes_per_sec: f64,
}

/// Aggregate a run's per-frame stats. `grid_cells` is the full terminal grid
/// cell count (damage rates are relative to it); `fps` is the playback rate
/// used to express byte throughput.
///
/// An empty slice yields all-zero stats (frames = 0).
pub fn aggregate_frame_stats(stats: &[FrameStats], grid_cells: u32, fps: f64) -> DamageStats {
    let frames = stats.len() as u32;
    let mut dropped = 0u32;
    let mut bytes_total = 0u64;
    let mut max_bytes = 0u32;
    let mut damage_sum = 0.0f64;
    let mut max_damage = 0.0f64;
    let mut write_ns_total = 0u64;
    for s in stats {
        dropped += s.dropped as u32;
        bytes_total += s.bytes as u64;
        max_bytes = max_bytes.max(s.bytes);
        let rate = if grid_cells > 0 { s.cells_damaged as f64 / grid_cells as f64 } else { 0.0 };
        damage_sum += rate;
        max_damage = max_damage.max(rate);
        write_ns_total += s.write_ns;
    }
    let nf = if frames > 0 { frames as f64 } else { 1.0 };
    let avg_bytes_per_frame = bytes_total as f64 / nf;
    DamageStats {
        frames,
        dropped_frames: dropped,
        bytes_total,
        avg_bytes_per_frame,
        max_bytes_per_frame: max_bytes,
        avg_damage_rate: damage_sum / nf,
        max_damage_rate: max_damage,
        avg_write_ms: write_ns_total as f64 / nf / 1e6,
        bytes_per_sec: avg_bytes_per_frame * fps,
    }
}

/// The four player pipeline stages, as reported in its `--sim` JSON
/// (`stage_ms:{decode,resample,compose,present}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Decode,
    Resample,
    Compose,
    Present,
}

impl Stage {
    pub const ALL: [Stage; 4] = [Stage::Decode, Stage::Resample, Stage::Compose, Stage::Present];

    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Decode => "decode",
            Stage::Resample => "resample",
            Stage::Compose => "compose",
            Stage::Present => "present",
        }
    }
}

/// Per-stage timing summary, milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StageStat {
    /// Samples recorded for this stage.
    pub frames: u32,
    pub mean_ms: f64,
    pub max_ms: f64,
}

/// The JSON form of one run's stage timings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StageTimesMs {
    pub decode: StageStat,
    pub resample: StageStat,
    pub compose: StageStat,
    pub present: StageStat,
}

impl StageTimesMs {
    pub fn stage(&self, stage: Stage) -> &StageStat {
        match stage {
            Stage::Decode => &self.decode,
            Stage::Resample => &self.resample,
            Stage::Compose => &self.compose,
            Stage::Present => &self.present,
        }
    }
}

/// Streaming per-stage timer accumulator: `record` each stage's duration
/// every frame, then `report()` for the serializable summary.
#[derive(Clone, Debug, Default)]
pub struct StageAccum {
    agg: [(u32, u64, u64); 4],
}

impl StageAccum {
    pub fn new() -> StageAccum {
        StageAccum::default()
    }

    pub fn record(&mut self, stage: Stage, elapsed: Duration) {
        let slot = &mut self.agg[stage as usize];
        let ns = elapsed.as_nanos() as u64;
        slot.0 += 1;
        slot.1 += ns;
        slot.2 = slot.2.max(ns);
    }

    pub fn report(&self) -> StageTimesMs {
        let stat = |i: usize| {
            let (n, total, max) = self.agg[i];
            StageStat {
                frames: n,
                mean_ms: if n > 0 { total as f64 / n as f64 / 1e6 } else { 0.0 },
                max_ms: max as f64 / 1e6,
            }
        };
        StageTimesMs {
            decode: stat(Stage::Decode as usize),
            resample: stat(Stage::Resample as usize),
            compose: stat(Stage::Compose as usize),
            present: stat(Stage::Present as usize),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_crafted_frame_stats() {
        let stats = [
            FrameStats { bytes: 1000, cells_damaged: 50, write_ns: 2_000_000, dropped: false },
            FrameStats { bytes: 3000, cells_damaged: 100, write_ns: 4_000_000, dropped: true },
        ];
        let d = aggregate_frame_stats(&stats, 200, 30.0);
        assert_eq!(d.frames, 2);
        assert_eq!(d.dropped_frames, 1);
        assert_eq!(d.bytes_total, 4000);
        assert_eq!(d.avg_bytes_per_frame, 2000.0);
        assert_eq!(d.max_bytes_per_frame, 3000);
        assert_eq!(d.avg_damage_rate, (0.25 + 0.5) / 2.0);
        assert_eq!(d.max_damage_rate, 0.5);
        assert_eq!(d.avg_write_ms, 3.0);
        assert_eq!(d.bytes_per_sec, 60000.0);
    }

    #[test]
    fn empty_stats_are_all_zero() {
        let d = aggregate_frame_stats(&[], 200, 30.0);
        assert_eq!(d.frames, 0);
        assert_eq!(d.bytes_total, 0);
        assert_eq!(d.avg_bytes_per_frame, 0.0);
        assert_eq!(d.avg_damage_rate, 0.0);
    }

    #[test]
    fn stage_accum_means_and_maxes() {
        let mut acc = StageAccum::new();
        acc.record(Stage::Decode, Duration::from_micros(300));
        acc.record(Stage::Decode, Duration::from_micros(500));
        acc.record(Stage::Present, Duration::from_millis(2));
        let r = acc.report();
        assert_eq!(r.decode.frames, 2);
        assert_eq!(r.decode.mean_ms, 0.4);
        assert_eq!(r.decode.max_ms, 0.5);
        assert_eq!(r.present.frames, 1);
        assert_eq!(r.present.mean_ms, 2.0);
        assert_eq!(r.resample.frames, 0);
        assert_eq!(r.resample.mean_ms, 0.0);
        assert_eq!(r.stage(Stage::Compose).frames, 0);
    }
}
