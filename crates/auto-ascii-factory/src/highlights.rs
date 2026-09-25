//! H plane: top-hat highlight + percentile deep-shadow
//! flags, detected offline (far stabler than runtime thresholding) from the
//! stored (EMA'd) Y plane — the flags inherit the EMA's temporal stability
//! without ever EMA-ing bits.
//!
//! Wire contract: `bit0` = highlight, `bit1` = deep shadow.
//!
//! - **Highlight:** white top-hat `y − opening(y)` with a box structuring
//!   element of radius `tophat_radius`, flagged at `≥ tophat_thresh`. The
//!   opening erases every feature narrower than the SE, so the response
//!   isolates small bright accents (glints, eyes, speculars) and ignores
//!   large bright areas — those belong to the base ramp, not the highlight
//!   layer.
//! - **Deep shadow:** the darkest `shadow_pct`% of the frame, capped by the
//!   absolute ceiling `shadow_max_l` so bright scenes never flag midtones:
//!   `y ≤ min(percentile(shadow_pct), shadow_max_l)`.

use crate::edges::{dilate_box, erode_box};
use crate::lut::percentile_levels_pct;
use crate::shots::luma_histogram;

/// H plane flag bits (wire contract).
pub mod h_flags {
    /// bit0: small bright accent (top-hat).
    pub const HIGHLIGHT: u8 = 1;
    /// bit1: deep shadow (percentile + ceiling).
    pub const DEEP_SHADOW: u8 = 1 << 1;
}

/// Effective `[highlights]` config (validated params, native types).
#[derive(Clone, Copy, Debug)]
pub struct HighlightConfig {
    pub tophat_radius: usize,
    pub tophat_thresh: u8,
    pub shadow_pct: u64,
    pub shadow_max_l: u8,
}

impl HighlightConfig {
    pub fn from_params(p: &crate::params::HighlightsParams) -> HighlightConfig {
        HighlightConfig {
            tophat_radius: p.tophat_radius as usize,
            tophat_thresh: p.tophat_thresh as u8,
            shadow_pct: u64::from(p.shadow_pct),
            shadow_max_l: p.shadow_max_l as u8,
        }
    }
}

/// Reusable per-frame extractor (two scratch planes, allocated once).
pub struct HighlightExtractor {
    w: usize,
    h: usize,
    a: Vec<u8>,
    b: Vec<u8>,
}

impl HighlightExtractor {
    pub fn new(w: u16, h: u16) -> HighlightExtractor {
        let n = w as usize * h as usize;
        HighlightExtractor { w: w as usize, h: h as usize, a: vec![0; n], b: vec![0; n] }
    }

    /// Fill `out` with H flags for one (EMA'd) luma plane.
    pub fn run(&mut self, luma: &[u8], cfg: &HighlightConfig, out: &mut [u8]) {
        assert_eq!(luma.len(), self.w * self.h);
        assert_eq!(out.len(), luma.len());

        erode_box(luma, self.w, self.h, cfg.tophat_radius, &mut self.a, &mut self.b);
        let (eroded, opened) = (&self.b, &mut self.a);
        dilate_box(eroded, self.w, self.h, cfg.tophat_radius, out, opened);

        let hist = luma_histogram(luma);
        let p = percentile_levels_pct(&hist, cfg.shadow_pct, 100)
            .expect("non-empty plane has a histogram")
            .lo;
        let shadow_thr = p.min(cfg.shadow_max_l);

        for ((&y, &op), o) in luma.iter().zip(self.a.iter()).zip(out.iter_mut()) {
            let mut f = 0u8;
            if y.saturating_sub(op) >= cfg.tophat_thresh {
                f |= h_flags::HIGHLIGHT;
            }
            if y <= shadow_thr {
                f |= h_flags::DEEP_SHADOW;
            }
            *o = f;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 32;
    const H: usize = 32;

    fn cfg() -> HighlightConfig {
        HighlightConfig { tophat_radius: 3, tophat_thresh: 48, shadow_pct: 8, shadow_max_l: 40 }
    }

    fn run(luma: &[u8], cfg: &HighlightConfig) -> Vec<u8> {
        let mut hx = HighlightExtractor::new(W as u16, H as u16);
        let mut out = vec![0u8; W * H];
        hx.run(luma, cfg, &mut out);
        out
    }

    #[test]
    fn bright_spot_fires_bit0_only_at_the_spot() {
        let mut luma = vec![100u8; W * H];
        for y in 15..18 {
            for x in 15..18 {
                luma[y * W + x] = 250;
            }
        }
        let out = run(&luma, &cfg());
        for y in 0..H {
            for x in 0..W {
                let want = (15..18).contains(&x) && (15..18).contains(&y);
                let got = out[y * W + x] & h_flags::HIGHLIGHT != 0;
                assert_eq!(got, want, "highlight bit wrong at ({x},{y})");
                assert_eq!(out[y * W + x] & h_flags::DEEP_SHADOW, 0, "no shadow at 100 L*");
            }
        }
    }

    #[test]
    fn wide_bright_area_is_not_a_highlight() {
        let mut luma = vec![100u8; W * H];
        for y in 10..22 {
            for x in 10..22 {
                luma[y * W + x] = 250;
            }
        }
        let out = run(&luma, &cfg());
        let hits = out.iter().filter(|&&f| f & h_flags::HIGHLIGHT != 0).count();
        assert_eq!(hits, 0, "bright AREAS belong to the base ramp, not the highlight layer");
    }

    #[test]
    fn dark_region_fires_bit1_capped_by_ceiling() {
        let mut luma = vec![120u8; W * H];
        for v in luma.iter_mut().take(W * H / 4) {
            *v = 5;
        }
        let out = run(&luma, &cfg());
        for (i, &f) in out.iter().enumerate() {
            let want = luma[i] == 5;
            assert_eq!(f & h_flags::DEEP_SHADOW != 0, want, "shadow bit wrong at {i}");
        }

        let out = run(&vec![160u8; W * H], &cfg());
        assert!(out.iter().all(|&f| f & h_flags::DEEP_SHADOW == 0), "midtones flagged as shadow");
    }
}
