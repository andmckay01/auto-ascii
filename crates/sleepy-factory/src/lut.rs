//! Luma transfer LUTs + percentile levels (PLAN §5 stages 3+5, M1 subset).
//!
//! M1 change from M0: level normalization is NO LONGER baked into the plane.
//! The per-pixel transform is only sRGB → linear light → CIE L\* (folded into
//! [`LumaLut`]); per-shot p2/p98 levels are still *measured* here
//! ([`percentile_levels`]) but travel in the NORM chunk and are applied at
//! RUNTIME by the player (PLAN §4/§5 — replaces M0's baked global stretch).
//!
//! Floats appear only while *building* the tables at startup — the pass 1 /
//! pass 2 pixel loops are pure table lookups, so the factory output is
//! byte-deterministic for identical input (PLAN §4 golden requirement).

/// Lower percentile for the per-shot level stretch (PLAN §5 stage 5: p2).
pub const LEVELS_LO_PCT: u64 = 2;
/// Upper percentile for the per-shot level stretch (PLAN §5 stage 5: p98).
pub const LEVELS_HI_PCT: u64 = 98;

/// Luma levels in the L\* (0..=255) domain, from a pooled shot histogram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Levels {
    /// p2 in stored-L\* units — the player maps it to output 0.
    pub lo: u8,
    /// p98 in stored-L\* units — the player maps it to output 255.
    pub hi: u8,
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

/// rgb24 → L\* byte, as three table lookups + two adds (PLAN §5 stage 3:
/// sRGB→linear→L\*, now from full RGB since one rgb24 decode feeds both
/// planes at M1):
///
/// ```text
///   per channel: sRGB byte -> linear light × Rec.709 coefficient, Q16
///   sum (0..=65535) -> CIE L* (0..100 scaled to 0..=255) via a 64 Ki table
/// ```
pub struct LumaLut {
    /// Per-channel sRGB byte → coefficient-weighted linear light in Q16
    /// (`round(coef · linear(c) · 65535)`); the three tables sum to ≤ 65536
    /// (rounding), clamped at lookup.
    lin: [[u32; 256]; 3],
    /// 16-bit linear light → L\* byte.
    lstar: Box<[u8; 65536]>,
}

impl LumaLut {
    pub fn new() -> LumaLut {
        /// Rec.709 / sRGB relative-luminance coefficients (R, G, B).
        const COEF: [f64; 3] = [0.2126, 0.7152, 0.0722];
        let mut lin = [[0u32; 256]; 3];
        for (table, coef) in lin.iter_mut().zip(COEF) {
            for (v, out) in table.iter_mut().enumerate() {
                let c = v as f64 / 255.0;
                // sRGB EOTF (IEC 61966-2-1).
                let l = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
                *out = (coef * l * 65535.0).round() as u32;
            }
        }

        let mut lstar = vec![0u8; 1 << 16].into_boxed_slice();
        for (i, out) in lstar.iter_mut().enumerate() {
            let y = i as f64 / 65535.0;
            // CIE L* from relative luminance Y (white Y = 1).
            const EPS: f64 = 216.0 / 24389.0; // (6/29)^3
            const KAPPA: f64 = 24389.0 / 27.0;
            let l = if y > EPS { 116.0 * y.cbrt() - 16.0 } else { KAPPA * y };
            *out = (l * 255.0 / 100.0).round().clamp(0.0, 255.0) as u8;
        }
        let lstar: Box<[u8; 65536]> = lstar.try_into().expect("built with len 65536");
        LumaLut { lin, lstar }
    }

    /// L\* byte for one rgb24 pixel.
    #[inline]
    pub fn l_of(&self, r: u8, g: u8, b: u8) -> u8 {
        let y = self.lin[0][r as usize] + self.lin[1][g as usize] + self.lin[2][b as usize];
        self.lstar[y.min(65535) as usize]
    }
}

impl Default for LumaLut {
    fn default() -> LumaLut {
        LumaLut::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference gray-axis path (the M0 256-entry table): sRGB gray byte →
    /// linear → L\* byte. `LumaLut` must agree on r == g == b within ±1
    /// (its luminance detour through Q16 rounds independently).
    fn srgb_gray_to_lstar(g: u8) -> u8 {
        let c = g as f64 / 255.0;
        let lin = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        const EPS: f64 = 216.0 / 24389.0;
        const KAPPA: f64 = 24389.0 / 27.0;
        let lstar = if lin > EPS { 116.0 * lin.cbrt() - 16.0 } else { KAPPA * lin };
        (lstar * 255.0 / 100.0).round().clamp(0.0, 255.0) as u8
    }

    #[test]
    fn gray_axis_matches_reference_and_is_monotone() {
        let lut = LumaLut::new();
        assert_eq!(lut.l_of(0, 0, 0), 0);
        assert_eq!(lut.l_of(255, 255, 255), 255);
        let mut prev = 0u8;
        for g in 0..=255u8 {
            let l = lut.l_of(g, g, g);
            let want = srgb_gray_to_lstar(g);
            assert!(l.abs_diff(want) <= 1, "gray {g}: LumaLut {l} vs reference {want}");
            assert!(l >= prev, "gray axis must be monotone at {g}");
            prev = l;
        }
        // Mid-gray sanity: sRGB 119 ≈ linear 0.184 ≈ L* 50 → ~128.
        assert!((120..=135).contains(&lut.l_of(119, 119, 119)));
    }

    #[test]
    fn channel_luminance_ordering() {
        // Rec.709: green carries most luminance, blue least.
        let lut = LumaLut::new();
        let (r, g, b) = (lut.l_of(255, 0, 0), lut.l_of(0, 255, 0), lut.l_of(0, 0, 255));
        assert!(g > r && r > b, "expected G > R > B luminance, got r={r} g={g} b={b}");
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
        hist[77] = 12345; // constant input: p2 == p98 (player treats as identity)
        assert_eq!(percentile_levels(&hist), Some(Levels { lo: 77, hi: 77 }));
    }
}
