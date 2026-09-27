//! Luma transfer lookup tables and histogram percentile levels.

pub const LEVELS_LO_PCT: u64 = 2;
pub const LEVELS_HI_PCT: u64 = 98;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Levels {
    pub lo: u8,
    pub hi: u8,
}

pub fn percentile_levels_pct(hist: &[u64; 256], lo_pct: u64, hi_pct: u64) -> Option<Levels> {
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
    Some(Levels { lo: value_at(rank(lo_pct)), hi: value_at(rank(hi_pct)) })
}

pub struct LumaLut {
    lin: [[u32; 256]; 3],
    lstar: Box<[u8; 65536]>,
}

impl LumaLut {
    pub fn new() -> LumaLut {
        const COEF: [f64; 3] = [0.2126, 0.7152, 0.0722];
        let mut lin = [[0u32; 256]; 3];
        for (table, coef) in lin.iter_mut().zip(COEF) {
            for (v, out) in table.iter_mut().enumerate() {
                let c = v as f64 / 255.0;
                let l = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
                *out = (coef * l * 65535.0).round() as u32;
            }
        }

        let mut lstar = vec![0u8; 1 << 16].into_boxed_slice();
        for (i, out) in lstar.iter_mut().enumerate() {
            let y = i as f64 / 65535.0;
            const EPS: f64 = 216.0 / 24389.0;
            const KAPPA: f64 = 24389.0 / 27.0;
            let l = if y > EPS { 116.0 * y.cbrt() - 16.0 } else { KAPPA * y };
            *out = (l * 255.0 / 100.0).round().clamp(0.0, 255.0) as u8;
        }
        let lstar: Box<[u8; 65536]> = lstar.try_into().expect("built with len 65536");
        LumaLut { lin, lstar }
    }

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
        assert!((120..=135).contains(&lut.l_of(119, 119, 119)));
    }

    #[test]
    fn channel_luminance_ordering() {
        let lut = LumaLut::new();
        let (r, g, b) = (lut.l_of(255, 0, 0), lut.l_of(0, 255, 0), lut.l_of(0, 0, 255));
        assert!(g > r && r > b, "expected G > R > B luminance, got r={r} g={g} b={b}");
    }

    #[test]
    fn percentiles_nearest_rank() {
        let mut hist = [0u64; 256];
        hist[10] = 2;
        hist[100] = 96;
        hist[200] = 2;
        let lv = percentile_levels_pct(&hist, LEVELS_LO_PCT, LEVELS_HI_PCT).unwrap();
        assert_eq!(lv, Levels { lo: 10, hi: 100 });
        let wide = percentile_levels_pct(&hist, 1, 100).unwrap();
        assert_eq!(wide, Levels { lo: 10, hi: 200 });
    }

    #[test]
    fn percentiles_empty_and_degenerate() {
        assert_eq!(percentile_levels_pct(&[0u64; 256], 2, 98), None);

        let mut hist = [0u64; 256];
        hist[77] = 12345;
        assert_eq!(percentile_levels_pct(&hist, 2, 98), Some(Levels { lo: 77, hi: 77 }));
    }
}
