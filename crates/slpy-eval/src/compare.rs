//! Baseline compare: per-metric tolerances → pass/fail + deltas (PLAN §6,
//! M2 acceptance: "a deliberate param regression trips the eval baseline
//! compare").
//!
//! Direction-aware: SSIM regresses downward; flicker, bytes, damage and
//! stage times regress upward. Improvements always pass — tolerances bound
//! regressions only. A metric present in the baseline but missing from the
//! current run fails (coverage must not silently shrink); a metric new in
//! the current run is informational only.

use serde::{Deserialize, Serialize};

use crate::report::EvalReport;
use crate::stats::Stage;

/// Per-metric regression tolerances. All fields have serde defaults so a
/// params.toml `[eval.tolerances]` table can override any subset (item B).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tolerances {
    /// Max allowed absolute SSIM drop (SSIM is 0..=1; absolute is stabler
    /// than relative near 1.0).
    pub ssim_max_drop: f64,
    /// Max allowed absolute increase in flicker switches/cell/s (the gate
    /// scale is "≤ 2", so absolute units are the natural tolerance).
    pub flicker_max_increase: f64,
    /// Max allowed fractional increase in avg bytes/frame per tier
    /// (0.20 = +20%).
    pub bytes_frac_max_increase: f64,
    /// Max allowed absolute increase in avg damage rate per tier
    /// (rates are 0..=1 fractions of the grid).
    pub damage_rate_max_increase: f64,
    /// Max allowed fractional increase in per-stage mean frame time.
    /// Wider than the criterion perf gates on purpose: eval-run stage timers
    /// are wall-clock on a noisy box; the perf gate (item E) is the precise
    /// instrument.
    pub stage_ms_frac_max_increase: f64,
    /// Max allowed absolute change (either direction) in `shot_count` and
    /// `cut_count`. Shot structure has no "better" direction — a params
    /// change that alters the cut roster is exactly what this gate exists
    /// to flag (M2 review fix: killed cut detection must fail the compare).
    pub shot_structure_max_delta: f64,
    /// Max allowed fractional DROP in `keyframe_count` (fewer keyframes =
    /// longer delta rolls = worse seeks, PLAN §4). Increases pass on their
    /// own; the size cost of extra keyframes is bounded by `asset_bytes`.
    pub keyframes_frac_max_drop: f64,
    /// Max allowed fractional increase in `asset_bytes` (encode-profile
    /// bloat, e.g. a zstd_level downgrade). Shrink always passes.
    pub asset_bytes_frac_max_increase: f64,
}

impl Default for Tolerances {
    fn default() -> Tolerances {
        Tolerances {
            ssim_max_drop: 0.02,
            flicker_max_increase: 0.5,
            bytes_frac_max_increase: 0.20,
            damage_rate_max_increase: 0.05,
            stage_ms_frac_max_increase: 0.50,
            shot_structure_max_delta: 0.0,
            keyframes_frac_max_drop: 0.0,
            asset_bytes_frac_max_increase: 0.20,
        }
    }
}

/// One compared metric.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricDelta {
    /// Clip name the metric belongs to.
    pub clip: String,
    /// Metric path, e.g. `"ssim"`, `"flicker"`, `"bytes_per_frame/256"`,
    /// `"damage_rate/truecolor"`, `"stage_ms/resample"`.
    pub metric: String,
    pub baseline: f64,
    pub current: f64,
    /// `current − baseline`.
    pub delta: f64,
    pub pass: bool,
}

/// Result of a baseline comparison.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompareReport {
    /// True iff every delta passed and no structural problem was found.
    pub pass: bool,
    pub deltas: Vec<MetricDelta>,
    /// Structural findings (schema mismatch, missing clips/metrics/tiers).
    pub notes: Vec<String>,
}

impl CompareReport {
    /// Deltas that failed their tolerance.
    pub fn failures(&self) -> impl Iterator<Item = &MetricDelta> {
        self.deltas.iter().filter(|d| !d.pass)
    }
}

/// Compare a run against a committed baseline under the given tolerances.
pub fn compare_reports(
    current: &EvalReport,
    baseline: &EvalReport,
    tol: &Tolerances,
) -> CompareReport {
    let mut rep = CompareReport { pass: true, ..CompareReport::default() };

    if current.schema_version != baseline.schema_version {
        rep.notes.push(format!(
            "schema version mismatch: current {} vs baseline {}",
            current.schema_version, baseline.schema_version
        ));
        rep.pass = false;
        return rep;
    }

    for base_clip in &baseline.clips {
        let Some(cur_clip) = current.clip(&base_clip.name) else {
            rep.notes.push(format!("clip {:?} missing from current run", base_clip.name));
            rep.pass = false;
            continue;
        };
        let (bm, cm) = (&base_clip.metrics, &cur_clip.metrics);
        let clip = &base_clip.name;

        // SSIM: higher is better; tolerate an absolute drop.
        push_scalar(&mut rep, clip, "ssim", bm.ssim, cm.ssim, |b, c| c >= b - tol.ssim_max_drop);

        // Flicker: lower is better; tolerate an absolute increase.
        push_scalar(
            &mut rep,
            clip,
            "flicker",
            bm.flicker_switches_per_cell_sec,
            cm.flicker_switches_per_cell_sec,
            |b, c| c <= b + tol.flicker_max_increase,
        );

        // Asset structure (M2 review fix — factory-tunable regressions).
        // Shot/cut counts: directionless, bounded absolute delta (default 0:
        // any change to the cut roster is a deliberate re-baselining act).
        for (metric, b, c) in [
            ("shot_count", bm.shot_count, cm.shot_count),
            ("cut_count", bm.cut_count, cm.cut_count),
        ] {
            push_scalar(&mut rep, clip, metric, b.map(f64::from), c.map(f64::from), |b, c| {
                (c - b).abs() <= tol.shot_structure_max_delta
            });
        }
        // Keyframes: only a drop regresses (seek latency, PLAN §4).
        push_scalar(
            &mut rep,
            clip,
            "keyframe_count",
            bm.keyframe_count.map(f64::from),
            cm.keyframe_count.map(f64::from),
            |b, c| c >= b * (1.0 - tol.keyframes_frac_max_drop),
        );
        // Asset size: only bloat regresses.
        push_scalar(
            &mut rep,
            clip,
            "asset_bytes",
            bm.asset_bytes.map(|v| v as f64),
            cm.asset_bytes.map(|v| v as f64),
            |b, c| frac_increase_ok(b, c, tol.asset_bytes_frac_max_increase),
        );

        // Damage/bytes per tier.
        for (tier, base_d) in &bm.damage_by_tier {
            let Some(cur_d) = cm.damage_by_tier.get(tier) else {
                rep.notes.push(format!("clip {clip:?}: tier {tier:?} missing from current run"));
                rep.pass = false;
                continue;
            };
            push_scalar(
                &mut rep,
                clip,
                &format!("bytes_per_frame/{tier}"),
                Some(base_d.avg_bytes_per_frame),
                Some(cur_d.avg_bytes_per_frame),
                |b, c| frac_increase_ok(b, c, tol.bytes_frac_max_increase),
            );
            push_scalar(
                &mut rep,
                clip,
                &format!("damage_rate/{tier}"),
                Some(base_d.avg_damage_rate),
                Some(cur_d.avg_damage_rate),
                |b, c| c <= b + tol.damage_rate_max_increase,
            );
        }

        // Stage times.
        if let Some(base_s) = &bm.stage_ms {
            let Some(cur_s) = &cm.stage_ms else {
                rep.notes.push(format!("clip {clip:?}: stage_ms missing from current run"));
                rep.pass = false;
                continue;
            };
            for stage in Stage::ALL {
                let (b, c) = (base_s.stage(stage), cur_s.stage(stage));
                if b.frames == 0 {
                    continue; // stage never ran in the baseline — nothing to regress against
                }
                push_scalar(
                    &mut rep,
                    clip,
                    &format!("stage_ms/{}", stage.as_str()),
                    Some(b.mean_ms),
                    Some(c.mean_ms),
                    |b, c| frac_increase_ok(b, c, tol.stage_ms_frac_max_increase),
                );
            }
        }
    }
    rep
}

/// A fractional-increase bound that stays meaningful at a zero baseline:
/// zero → anything nonzero is treated as an unbounded regression.
fn frac_increase_ok(baseline: f64, current: f64, max_frac: f64) -> bool {
    if baseline <= 0.0 {
        current <= 0.0
    } else {
        current <= baseline * (1.0 + max_frac)
    }
}

/// Record one scalar metric comparison. Baseline `None` → skip silently
/// (nothing to regress against); baseline `Some` + current `None` → fail
/// with a note (metric disappeared).
fn push_scalar(
    rep: &mut CompareReport,
    clip: &str,
    metric: &str,
    baseline: Option<f64>,
    current: Option<f64>,
    ok: impl Fn(f64, f64) -> bool,
) {
    match (baseline, current) {
        (Some(b), Some(c)) => {
            let pass = ok(b, c);
            rep.pass &= pass;
            rep.deltas.push(MetricDelta {
                clip: clip.into(),
                metric: metric.into(),
                baseline: b,
                current: c,
                delta: c - b,
                pass,
            });
        }
        (Some(_), None) => {
            rep.notes.push(format!("clip {clip:?}: metric {metric:?} missing from current run"));
            rep.pass = false;
        }
        (None, _) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ClipMetrics, ClipReport, EvalReport};
    use crate::stats::{DamageStats, StageStat, StageTimesMs};

    fn damage(avg_bytes: f64, avg_rate: f64) -> DamageStats {
        DamageStats {
            frames: 100,
            dropped_frames: 0,
            bytes_total: (avg_bytes * 100.0) as u64,
            avg_bytes_per_frame: avg_bytes,
            max_bytes_per_frame: avg_bytes as u32 * 2,
            avg_damage_rate: avg_rate,
            max_damage_rate: avg_rate * 2.0,
            avg_write_ms: 0.1,
            bytes_per_sec: avg_bytes * 30.0,
        }
    }

    fn report(ssim: f64, flicker: f64, avg_bytes: f64, resample_ms: f64) -> EvalReport {
        let mut r = EvalReport::new("test");
        let mut metrics = ClipMetrics {
            ssim: Some(ssim),
            flicker_switches_per_cell_sec: Some(flicker),
            ..ClipMetrics::default()
        };
        metrics.damage_by_tier.insert("truecolor".into(), damage(avg_bytes, 0.2));
        metrics.stage_ms = Some(StageTimesMs {
            resample: StageStat { frames: 100, mean_ms: resample_ms, max_ms: resample_ms * 2.0 },
            ..StageTimesMs::default()
        });
        r.clips.push(ClipReport {
            name: "clip".into(),
            frames: 100,
            fps: 30.0,
            grid_cols: 300,
            grid_rows: 80,
            metrics,
        });
        r
    }

    #[test]
    fn identical_reports_pass_with_zero_deltas() {
        let base = report(0.8, 1.0, 20000.0, 0.5);
        let cmp = compare_reports(&base, &base, &Tolerances::default());
        assert!(cmp.pass, "{cmp:?}");
        assert!(cmp.deltas.iter().all(|d| d.delta == 0.0 && d.pass));
        assert!(cmp.notes.is_empty());
        // ssim + flicker + bytes + damage_rate + 1 recorded stage
        assert_eq!(cmp.deltas.len(), 5);
    }

    #[test]
    fn within_tolerance_and_improvements_pass() {
        let base = report(0.8, 1.0, 20000.0, 0.5);
        // Slightly worse ssim (−0.01 ≤ 0.02), better flicker, fewer bytes,
        // slightly slower resample (+20% ≤ +50%).
        let cur = report(0.79, 0.5, 15000.0, 0.6);
        let cmp = compare_reports(&cur, &base, &Tolerances::default());
        assert!(cmp.pass, "{cmp:?}");
    }

    #[test]
    fn ssim_regression_trips() {
        let base = report(0.8, 1.0, 20000.0, 0.5);
        let cur = report(0.75, 1.0, 20000.0, 0.5); // −0.05 > 0.02
        let cmp = compare_reports(&cur, &base, &Tolerances::default());
        assert!(!cmp.pass);
        let fail: Vec<_> = cmp.failures().collect();
        assert_eq!(fail.len(), 1);
        assert_eq!(fail[0].metric, "ssim");
        assert!((fail[0].delta - -0.05).abs() < 1e-12);
    }

    #[test]
    fn bytes_regression_trips() {
        let base = report(0.8, 1.0, 20000.0, 0.5);
        let cur = report(0.8, 1.0, 26000.0, 0.5); // +30% > +20%
        let cmp = compare_reports(&cur, &base, &Tolerances::default());
        assert!(!cmp.pass);
        assert_eq!(cmp.failures().next().unwrap().metric, "bytes_per_frame/truecolor");
    }

    #[test]
    fn flicker_and_stage_regressions_trip() {
        let base = report(0.8, 1.0, 20000.0, 0.5);
        let cur = report(0.8, 2.0, 20000.0, 0.9); // +1.0 > 0.5; +80% > +50%
        let cmp = compare_reports(&cur, &base, &Tolerances::default());
        assert!(!cmp.pass);
        let metrics: Vec<_> = cmp.failures().map(|d| d.metric.as_str()).collect();
        assert_eq!(metrics, ["flicker", "stage_ms/resample"]);
    }

    #[test]
    fn missing_clip_or_metric_fails() {
        let base = report(0.8, 1.0, 20000.0, 0.5);
        let mut cur = report(0.8, 1.0, 20000.0, 0.5);
        cur.clips[0].name = "renamed".into();
        let cmp = compare_reports(&cur, &base, &Tolerances::default());
        assert!(!cmp.pass);
        assert!(cmp.notes[0].contains("missing from current run"));

        let mut cur2 = report(0.8, 1.0, 20000.0, 0.5);
        cur2.clips[0].metrics.ssim = None;
        let cmp2 = compare_reports(&cur2, &base, &Tolerances::default());
        assert!(!cmp2.pass);
        assert!(cmp2.notes.iter().any(|n| n.contains("\"ssim\"")));
    }

    #[test]
    fn schema_mismatch_fails_fast() {
        let base = report(0.8, 1.0, 20000.0, 0.5);
        let mut cur = report(0.8, 1.0, 20000.0, 0.5);
        cur.schema_version = 2;
        let cmp = compare_reports(&cur, &base, &Tolerances::default());
        assert!(!cmp.pass);
        assert!(cmp.deltas.is_empty());
    }

    /// M2 review fix: asset-structure regressions (shot/cut roster,
    /// keyframe cadence, encode bloat) must trip the compare on their own,
    /// even when every render-side metric stays flat.
    #[test]
    fn asset_structure_regressions_trip() {
        let with_asset = |shots: u32, cuts: u32, keyframes: u32, bytes: u64| {
            let mut r = report(0.8, 1.0, 20000.0, 0.5);
            let m = &mut r.clips[0].metrics;
            m.shot_count = Some(shots);
            m.cut_count = Some(cuts);
            m.keyframe_count = Some(keyframes);
            m.asset_bytes = Some(bytes);
            r
        };
        let base = with_asset(9, 8, 15, 1_000_000);
        let tol = Tolerances::default();

        // Identical structure passes.
        assert!(compare_reports(&base, &base, &tol).pass);

        // Killed cut detection: 1 shot, 0 cuts — fails on both counts.
        let cur = with_asset(1, 0, 15, 1_000_000);
        let cmp = compare_reports(&cur, &base, &tol);
        assert!(!cmp.pass);
        let metrics: Vec<_> = cmp.failures().map(|d| d.metric.as_str()).collect();
        assert_eq!(metrics, ["shot_count", "cut_count"]);

        // Over-segmentation is a change too (directionless, default delta 0).
        assert!(!compare_reports(&with_asset(20, 19, 15, 1_000_000), &base, &tol).pass);

        // keyframe_ivl inflated: fewer keyframes fails; more passes alone.
        let cmp = compare_reports(&with_asset(9, 8, 3, 1_000_000), &base, &tol);
        assert_eq!(cmp.failures().map(|d| d.metric.as_str()).collect::<Vec<_>>(), ["keyframe_count"]);
        assert!(compare_reports(&with_asset(9, 8, 60, 1_000_000), &base, &tol).pass);

        // Encode bloat > +20% fails; shrink passes.
        let cmp = compare_reports(&with_asset(9, 8, 15, 1_300_000), &base, &tol);
        assert_eq!(cmp.failures().map(|d| d.metric.as_str()).collect::<Vec<_>>(), ["asset_bytes"]);
        assert!(compare_reports(&with_asset(9, 8, 15, 700_000), &base, &tol).pass);

        // Metrics absent from an OLD baseline are skipped (back-compat) but
        // present-in-baseline + missing-from-current still fails.
        let old_base = report(0.8, 1.0, 20000.0, 0.5);
        assert!(compare_reports(&base, &old_base, &tol).pass);
        let cmp = compare_reports(&old_base, &base, &tol);
        assert!(!cmp.pass);
        assert!(cmp.notes.iter().any(|n| n.contains("shot_count")));
    }

    #[test]
    fn zero_baseline_fraction_semantics() {
        assert!(frac_increase_ok(0.0, 0.0, 0.2));
        assert!(!frac_increase_ok(0.0, 1.0, 0.2));
        assert!(frac_increase_ok(100.0, 120.0, 0.2));
        assert!(!frac_increase_ok(100.0, 121.0, 0.2));
    }

    #[test]
    fn tolerances_partial_toml_style_override() {
        // serde(default) lets params.toml override any subset (item B glue).
        let t: Tolerances = serde_json::from_str(r#"{"ssim_max_drop": 0.001}"#).unwrap();
        assert_eq!(t.ssim_max_drop, 0.001);
        assert_eq!(t.bytes_frac_max_increase, Tolerances::default().bytes_frac_max_increase);
    }
}
