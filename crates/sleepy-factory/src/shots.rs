//! Shot detection + per-shot levels (PLAN §5 stages 2 + 5, M1 subset).
//!
//! Boundary test: sum-of-absolute-differences between consecutive frames'
//! 256-bin L\* histograms, normalized against the maximum possible SAD
//! (2·npx, fully disjoint histograms), thresholded, debounced by a minimum
//! shot length. Every honored boundary is a hard cut at M1 (histogram delta
//! finds cuts, not fades) and is flagged as such for the player's hysteresis
//! reset (PLAN §3.5).
//!
//! Levels: each shot pools its frames' histograms and takes p2/p98 once —
//! one constant level pair per shot is the "temporally stable within shot"
//! 80/20 (PLAN §5 stage 5: per-frame levels pump, global levels waste range).

use crate::lut::{self, Levels};

/// Cut threshold in thousandths of the maximum possible histogram SAD.
/// 300 = 0.30: real hard cuts land ~0.5–1.2, in-shot motion ~0.02–0.15.
/// M2: the effective value comes from params.toml (`shots.sad_threshold_milli`);
/// this constant is the embedded default and the unit-test anchor.
pub const SHOT_SAD_THRESHOLD_MILLI: u64 = 300;

/// Minimum shot length in frames: a boundary is honored only once the
/// current shot is at least this long (debounces flashes/strobes).
/// M2: params.toml `shots.min_shot_frames` overrides.
pub const MIN_SHOT_FRAMES: u32 = 8;

/// 256-bin histogram of one luma plane.
pub fn luma_histogram(luma: &[u8]) -> [u64; 256] {
    let mut hist = [0u64; 256];
    for &v in luma {
        hist[v as usize] += 1;
    }
    hist
}

/// One detected shot, ready to become a NORM record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shot {
    pub first_frame: u32,
    /// True when the shot begins at a detected hard cut (never on shot 0).
    pub cut: bool,
    /// Pooled p2/p98 of the shot's stored L\* luma.
    pub levels: Levels,
}

/// Online single-pass detector: feed per-frame histograms in order, then
/// [`finish`](ShotDetector::finish). Pure integer math — deterministic.
pub struct ShotDetector {
    npx: u64,
    threshold_milli: u64,
    min_shot_frames: u32,
    /// Percentiles for the pooled per-shot levels (params.toml `[levels]`).
    lo_pct: u64,
    hi_pct: u64,
    frames: u32,
    prev_hist: [u64; 256],
    shot_start: u32,
    shot_cut: bool,
    shot_hist: [u64; 256],
    done: Vec<Shot>,
}

impl ShotDetector {
    /// All tunables explicit — the values come from params.toml `[shots]` +
    /// `[levels]` (M2 item B; the constants above are the embedded defaults,
    /// re-exported through `params::Params::default`). `npx` = pixels per
    /// luma plane (normalizes the SAD).
    pub fn with_params(
        npx: u64,
        threshold_milli: u64,
        min_shot_frames: u32,
        lo_pct: u64,
        hi_pct: u64,
    ) -> ShotDetector {
        assert!(npx > 0, "empty planes have no shots");
        assert!(lo_pct < hi_pct && hi_pct <= 100, "params validation upholds this");
        ShotDetector {
            npx,
            threshold_milli,
            min_shot_frames,
            lo_pct,
            hi_pct,
            frames: 0,
            prev_hist: [0; 256],
            shot_start: 0,
            shot_cut: false,
            shot_hist: [0; 256],
            done: Vec::new(),
        }
    }

    /// Feed the next frame's luma histogram.
    pub fn push(&mut self, hist: &[u64; 256]) {
        if self.frames > 0 {
            let sad: u64 = hist.iter().zip(&self.prev_hist).map(|(a, b)| a.abs_diff(*b)).sum();
            let shot_len = self.frames - self.shot_start;
            // Normalized compare without division: sad/(2·npx) ≥ thr/1000.
            if 1000 * sad >= 2 * self.npx * self.threshold_milli
                && shot_len >= self.min_shot_frames
            {
                self.close_shot();
                self.shot_start = self.frames;
                self.shot_cut = true;
            }
        }
        for (pooled, &count) in self.shot_hist.iter_mut().zip(hist) {
            *pooled += count;
        }
        self.prev_hist = *hist;
        self.frames += 1;
    }

    /// Close the trailing shot and return all shots in frame order.
    /// Empty iff no frames were pushed.
    pub fn finish(mut self) -> Vec<Shot> {
        // There is always exactly one open shot once any frame was pushed
        // (a boundary opens the next shot with the frame that triggered it).
        if self.frames > 0 {
            self.close_shot();
        }
        self.done
    }

    fn close_shot(&mut self) {
        let levels = lut::percentile_levels_pct(&self.shot_hist, self.lo_pct, self.hi_pct)
            .expect("open shot has at least one frame");
        self.done.push(Shot { first_frame: self.shot_start, cut: self.shot_cut, levels });
        self.shot_hist = [0; 256];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NPX: u64 = 100;

    /// Detector at the embedded defaults (what `params.toml` ships).
    fn default_detector() -> ShotDetector {
        ShotDetector::with_params(NPX, SHOT_SAD_THRESHOLD_MILLI, MIN_SHOT_FRAMES, 2, 98)
    }

    /// All 100 pixels in one bin.
    fn solid(bin: usize) -> [u64; 256] {
        let mut h = [0u64; 256];
        h[bin] = NPX;
        h
    }

    #[test]
    fn static_input_is_one_shot_no_cut() {
        let mut det = default_detector();
        for _ in 0..20 {
            det.push(&solid(10));
        }
        let shots = det.finish();
        assert_eq!(
            shots,
            vec![Shot { first_frame: 0, cut: false, levels: Levels { lo: 10, hi: 10 } }]
        );
    }

    #[test]
    fn no_frames_no_shots() {
        assert!(default_detector().finish().is_empty());
    }

    #[test]
    fn hard_cut_splits_with_cut_flag_and_per_shot_levels() {
        let mut det = default_detector();
        for _ in 0..10 {
            det.push(&solid(10));
        }
        for _ in 0..10 {
            det.push(&solid(200)); // disjoint: SAD = 2·npx = 1.0 normalized
        }
        let shots = det.finish();
        assert_eq!(
            shots,
            vec![
                Shot { first_frame: 0, cut: false, levels: Levels { lo: 10, hi: 10 } },
                Shot { first_frame: 10, cut: true, levels: Levels { lo: 200, hi: 200 } },
            ]
        );
    }

    #[test]
    fn min_shot_length_debounces_early_cut() {
        // Cut fires at frame 3 < min_shot_frames → suppressed, single shot.
        let mut det = ShotDetector::with_params(NPX, SHOT_SAD_THRESHOLD_MILLI, 8, 2, 98);
        for _ in 0..3 {
            det.push(&solid(10));
        }
        for _ in 0..17 {
            det.push(&solid(200));
        }
        let shots = det.finish();
        assert_eq!(shots.len(), 1, "sub-min-length boundary must be merged");
        assert_eq!(shots[0].first_frame, 0);
        assert!(!shots[0].cut);
        // Pooled levels span both segments: p2 rank 40 of 2000 lands in bin
        // 10 (300 samples), p98 rank 1960 in bin 200.
        assert_eq!(shots[0].levels, Levels { lo: 10, hi: 200 });
    }

    #[test]
    fn sub_threshold_drift_does_not_split() {
        // 10% of pixels shift one bin: SAD = 20 → 0.10 normalized < 0.30.
        let base = solid(10);
        let mut shifted = solid(10);
        shifted[10] = 90;
        shifted[11] = 10;
        let mut det = default_detector();
        for i in 0..20 {
            det.push(if i % 2 == 0 { &base } else { &shifted });
        }
        assert_eq!(det.finish().len(), 1);
    }
}
