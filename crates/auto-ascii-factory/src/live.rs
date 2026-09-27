//! One rgb24 frame at a time into the six feature planes, for a source that
//! cannot be read twice: shot cuts come from the online detector and the
//! levels from the running shot's pooled histogram, instead of build's pass 1.

use auto_ascii_format::PlaneLevels;

use crate::extract::Extractor;
use crate::features::FeatureExtractor;
use crate::params::Params;
use crate::shots::{ShotDetector, luma_histogram};

pub struct LiveExtractor {
    extractor: Extractor,
    features: FeatureExtractor,
    detector: ShotDetector,
    luma: Vec<u8>,
    frames: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveInfo {
    pub cut: bool,
    pub shot_start: u32,
    pub levels: Option<PlaneLevels>,
}

impl LiveExtractor {
    pub fn new(w: u16, h: u16, params: &Params) -> LiveExtractor {
        let npx = w as usize * h as usize;
        LiveExtractor {
            extractor: Extractor::new(w, h),
            features: FeatureExtractor::new(w, h, params),
            detector: ShotDetector::with_params(
                npx as u64,
                params.shots.sad_threshold_milli,
                params.shots.min_shot_frames,
                params.levels.lo_pct,
                params.levels.hi_pct,
            ),
            luma: vec![0; npx],
            frames: 0,
        }
    }

    pub fn push(&mut self, rgb: &[u8]) -> LiveInfo {
        self.extractor.luma(rgb, &mut self.luma);
        let cut = self.detector.boundary(&luma_histogram(&self.luma));
        self.features.process(rgb, cut);
        self.detector.pool(&luma_histogram(self.features.y()));
        self.frames += 1;
        let levels = self
            .detector
            .open_levels()
            .map(|l| PlaneLevels { p2: l.lo, p98: l.hi });
        LiveInfo { cut, shot_start: self.detector.open_shot_start(), levels }
    }

    pub fn frames(&self) -> u32 {
        self.frames
    }

    pub fn features(&self) -> &FeatureExtractor {
        &self.features
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u16 = 32;
    const H: u16 = 18;

    fn frame(v: u8, t: u32) -> Vec<u8> {
        let mut rgb = vec![v; W as usize * H as usize * 3];
        for (i, px) in rgb.chunks_exact_mut(3).enumerate() {
            px[0] = px[0].wrapping_add(((i as u32 + t) % 17) as u8);
        }
        rgb
    }

    fn clip() -> Vec<Vec<u8>> {
        (0..12).map(|t| frame(20, t)).chain((0..12).map(|t| frame(220, t))).collect()
    }

    #[test]
    fn planes_match_the_build_extractor_on_the_same_cut_schedule() {
        let params = Params::default();
        let mut live = LiveExtractor::new(W, H, &params);
        let mut reference = FeatureExtractor::new(W, H, &params);
        let mut cuts = Vec::new();
        for (i, rgb) in clip().iter().enumerate() {
            let info = live.push(rgb);
            if info.cut {
                cuts.push(i);
            }
            reference.process(rgb, info.cut);
            let (a, b) = (live.features(), &reference);
            assert_eq!((a.y(), a.e(), a.ex()), (b.y(), b.e(), b.ex()), "frame {i}");
            assert_eq!((a.ey(), a.h(), a.c()), (b.ey(), b.h(), b.c()), "frame {i}");
        }
        assert_eq!(cuts, vec![12], "the hard cut is found online");
        assert_eq!(live.frames(), 24);
    }

    #[test]
    fn levels_follow_the_open_shot_and_restart_at_a_cut() {
        let params = Params::default();
        let mut live = LiveExtractor::new(W, H, &params);
        let frames = clip();
        let first = live.push(&frames[0]);
        assert_eq!(first.shot_start, 0);
        assert!(!first.cut);
        let dark = first.levels.expect("one frame pooled");
        let mut last = first;
        for rgb in &frames[1..] {
            last = live.push(rgb);
        }
        assert_eq!(last.shot_start, 12);
        let bright = last.levels.expect("the open shot has frames");
        assert!(bright.p2 > dark.p98, "{bright:?} vs {dark:?}");
    }
}
