//! Shot-boundary detection and per-shot level pooling.

use crate::lut::{self, Levels};

pub const SHOT_SAD_THRESHOLD_MILLI: u64 = 300;

const MILLI: u64 = 1000;
const DISJOINT_HISTOGRAM_SAD_PER_PIXEL: u64 = 2;

pub const MIN_SHOT_FRAMES: u32 = 8;

pub fn luma_histogram(luma: &[u8]) -> [u64; 256] {
    let mut hist = [0u64; 256];
    for &v in luma {
        hist[v as usize] += 1;
    }
    hist
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shot {
    pub first_frame: u32,
    pub cut: bool,
    pub levels: Levels,
}

pub struct ShotDetector {
    npx: u64,
    threshold_milli: u64,
    min_shot_frames: u32,
    lo_pct: u64,
    hi_pct: u64,
    frames: u32,
    prev_hist: [u64; 256],
    shot_start: u32,
    shot_cut: bool,
    shot_hist: [u64; 256],
    done: Vec<Shot>,
    frame_pending: bool,
}

impl ShotDetector {
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
            frame_pending: false,
        }
    }

    pub fn boundary(&mut self, hist: &[u64; 256]) -> bool {
        debug_assert!(!self.frame_pending, "boundary twice without pool");
        self.frame_pending = true;
        let mut cut = false;
        if self.frames > 0 {
            let sad: u64 = hist.iter().zip(&self.prev_hist).map(|(a, b)| a.abs_diff(*b)).sum();
            let shot_len = self.frames - self.shot_start;
            if MILLI * sad >= DISJOINT_HISTOGRAM_SAD_PER_PIXEL * self.npx * self.threshold_milli
                && shot_len >= self.min_shot_frames
            {
                self.close_shot();
                self.shot_start = self.frames;
                self.shot_cut = true;
                cut = true;
            }
        }
        self.prev_hist = *hist;
        cut
    }

    pub fn pool(&mut self, hist: &[u64; 256]) {
        debug_assert!(self.frame_pending, "pool without boundary");
        self.frame_pending = false;
        for (pooled, &count) in self.shot_hist.iter_mut().zip(hist) {
            *pooled += count;
        }
        self.frames += 1;
    }

    pub fn open_levels(&self) -> Option<Levels> {
        lut::percentile_levels_pct(&self.shot_hist, self.lo_pct, self.hi_pct)
    }

    pub fn open_shot_start(&self) -> u32 {
        self.shot_start
    }

    pub fn finish(mut self) -> Vec<Shot> {
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

    fn default_detector() -> ShotDetector {
        ShotDetector::with_params(NPX, SHOT_SAD_THRESHOLD_MILLI, MIN_SHOT_FRAMES, 2, 98)
    }

    trait Push {
        fn push(&mut self, hist: &[u64; 256]) -> bool;
    }

    impl Push for ShotDetector {
        fn push(&mut self, hist: &[u64; 256]) -> bool {
            let cut = self.boundary(hist);
            self.pool(hist);
            cut
        }
    }

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
            det.push(&solid(200));
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
        assert_eq!(shots[0].levels, Levels { lo: 10, hi: 200 });
    }

    #[test]
    fn push_reports_the_cut_frame_and_split_pooling_matches() {
        let mut det = default_detector();
        for i in 0..20 {
            let cut = det.push(&solid(if i < 10 { 10 } else { 200 }));
            assert_eq!(cut, i == 10, "cut flag wrong at frame {i}");
        }

        let mut det = default_detector();
        for i in 0..20 {
            let raw = solid(if i < 10 { 10 } else { 200 });
            let cut = det.boundary(&raw);
            assert_eq!(cut, i == 10);
            det.pool(&solid(77));
        }
        let shots = det.finish();
        assert_eq!(shots.len(), 2);
        assert_eq!(shots[0].levels, Levels { lo: 77, hi: 77 }, "levels follow the pooled hist");
        assert_eq!(shots[1].levels, Levels { lo: 77, hi: 77 });
    }

    #[test]
    fn disjoint_histograms_reach_exactly_the_normalized_maximum() {
        let cuts_at = |threshold_milli| {
            let mut det = ShotDetector::with_params(NPX, threshold_milli, 1, 2, 98);
            det.push(&solid(10));
            det.push(&solid(200))
        };
        assert!(cuts_at(MILLI), "fully disjoint histograms score 1.0");
        assert!(!cuts_at(MILLI + 1), "nothing scores above 1.0");
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "boundary twice without pool")]
    fn boundary_twice_without_pool_is_a_protocol_error() {
        let mut det = default_detector();
        det.boundary(&solid(10));
        det.boundary(&solid(10));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "pool without boundary")]
    fn pool_without_boundary_is_a_protocol_error() {
        let mut det = default_detector();
        det.push(&solid(10));
        det.pool(&solid(10));
    }

    #[test]
    fn sub_threshold_drift_does_not_split() {
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
