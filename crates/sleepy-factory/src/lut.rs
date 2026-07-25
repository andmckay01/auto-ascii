//! Luma transfer + level-normalization LUTs (PLAN §5 stages 3+5, M0 subset).
//!
//! The M0 factory folds the whole per-pixel transform into ONE 256-entry u8
//! LUT applied on pass 2:
//!
//! ```text
//!   sRGB gray byte -> linear light -> CIE L* (0..100 -> 0..255)
//!                  -> global p2/p98 level stretch (approved M0 simplification:
//!                     baked into the plane; NORM + per-shot levels at M1/M3)
//! ```
//!
//! Floats appear only while *building* the 256-entry tables at startup —
//! pass 1/pass 2 pixel loops are pure table lookups, so the factory output is
//! byte-deterministic for identical input (PLAN §4 golden requirement).

/// Lower percentile for the global level stretch (PLAN §5 stage 5: p2).
pub const LEVELS_LO_PCT: u64 = 2;
/// Upper percentile for the global level stretch (PLAN §5 stage 5: p98).
pub const LEVELS_HI_PCT: u64 = 98;

/// Global luma levels in the L* (0..=255) domain, from the pass-1 histogram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Levels {
    /// p2 in L* units — maps to output 0.
    pub lo: u8,
    /// p98 in L* units — maps to output 255.
    pub hi: u8,
}

/// 256-entry sRGB-gray-byte → L\* table (L\* 0..100 scaled to 0..255,
/// round-to-nearest). Monotone non-decreasing; 0 → 0 and 255 → 255.
pub fn srgb_to_lstar_lut() -> [u8; 256] {
    let mut lut = [0u8; 256];
    for (g, out) in lut.iter_mut().enumerate() {
        let c = g as f64 / 255.0;
        // sRGB EOTF (IEC 61966-2-1).
        let lin = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        // CIE L* from relative luminance Y (white Y = 1).
        const EPS: f64 = 216.0 / 24389.0; // (6/29)^3
        const KAPPA: f64 = 24389.0 / 27.0;
        let lstar = if lin > EPS { 116.0 * lin.cbrt() - 16.0 } else { KAPPA * lin };
        *out = (lstar * 255.0 / 100.0).round().clamp(0.0, 255.0) as u8;
    }
    lut
}

/// p2/p98 of a 256-bin histogram (nearest-rank, rank = ⌈N·p/100⌉ clamped to
/// ≥ 1 — pure integer math, deterministic). Returns `None` for an empty
/// histogram (the zero-frame edge case is rejected before this is reached).
pub fn percentile_levels(hist: &[u64; 256]) -> Option<Levels> {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return None;
    }
    let rank = |pct: u64| -> u64 { ((total * pct).div_ceil(100)).max(1) };
    let value_at = |target: u64| -> u8 {
        let mut cum = 0u64;
        for (v, &count) in hist.iter().enumerate() {
            cum += count;
            if cum >= target {
                return v as u8;
            }
        }
        255
    };
    Some(Levels { lo: value_at(rank(LEVELS_LO_PCT)), hi: value_at(rank(LEVELS_HI_PCT)) })
}

/// The combined pass-2 LUT: sRGB gray byte → level-stretched L\* byte.
/// `(l - lo) · 255 / (hi - lo)`, round-half-up integer math, saturating at
/// 0/255 outside [lo, hi]. Degenerate levels (`hi <= lo`, e.g. a constant-
/// luma input) skip the stretch and pass L\* through unchanged — still
/// deterministic, never a divide-by-zero.
pub fn build_output_lut(lstar: &[u8; 256], levels: Levels) -> [u8; 256] {
    if levels.hi <= levels.lo {
        return *lstar;
    }
    let lo = levels.lo as u32;
    let span = levels.hi as u32 - lo;
    let mut lut = [0u8; 256];
    for (g, out) in lut.iter_mut().enumerate() {
        let l = lstar[g] as u32;
        *out = if l <= lo {
            0
        } else if l >= levels.hi as u32 {
            255
        } else {
            (((l - lo) * 255 + span / 2) / span) as u8
        };
    }
    lut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lstar_lut_monotone_with_exact_endpoints() {
        let lut = srgb_to_lstar_lut();
        assert_eq!(lut[0], 0);
        assert_eq!(lut[255], 255);
        assert!(lut.windows(2).all(|w| w[0] <= w[1]), "L* LUT must be monotone");
        // Mid-gray sanity: sRGB 119 ≈ linear 0.184 ≈ L* 50 → ~128.
        assert!((120..=135).contains(&lut[119]), "lut[119] = {}", lut[119]);
    }

    #[test]
    fn percentiles_nearest_rank() {
        // 100 samples: 2 at value 10, 96 at value 100, 2 at value 200.
        let mut hist = [0u64; 256];
        hist[10] = 2;
        hist[100] = 96;
        hist[200] = 2;
        let lv = percentile_levels(&hist).unwrap();
        // rank(p2) = 2 → cum reaches 2 at value 10; rank(p98) = 98 → value 100.
        assert_eq!(lv, Levels { lo: 10, hi: 100 });
    }

    #[test]
    fn percentiles_empty_and_degenerate() {
        assert_eq!(percentile_levels(&[0u64; 256]), None);

        let mut hist = [0u64; 256];
        hist[77] = 12345; // constant input: p2 == p98
        let lv = percentile_levels(&hist).unwrap();
        assert_eq!(lv, Levels { lo: 77, hi: 77 });
        // Degenerate levels fall back to plain L* (no stretch, no div-by-zero).
        let lstar = srgb_to_lstar_lut();
        assert_eq!(build_output_lut(&lstar, lv), lstar);
    }

    #[test]
    fn output_lut_stretches_to_full_range() {
        let lstar = srgb_to_lstar_lut();
        let lv = Levels { lo: 40, hi: 200 };
        let lut = build_output_lut(&lstar, lv);
        assert!(lut.windows(2).all(|w| w[0] <= w[1]), "output LUT must be monotone");
        assert_eq!(lut[0], 0);
        assert_eq!(lut[255], 255);
        // Everything at/below lo saturates to 0; at/above hi to 255.
        let g_lo = lstar.iter().rposition(|&l| l <= lv.lo).unwrap();
        let g_hi = lstar.iter().position(|&l| l >= lv.hi).unwrap();
        assert_eq!(lut[g_lo], 0);
        assert_eq!(lut[g_hi], 255);
    }
}
