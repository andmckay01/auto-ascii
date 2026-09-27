//! Doubled-angle orientation bins, hysteresis guards and coherence math.

pub const BIN_UNSET: u8 = 0xFF;

#[inline]
pub fn debias(v: u8) -> i32 {
    v as i32 - 128
}

#[inline]
pub fn octant_bin(dx: i32, dy: i32) -> u8 {
    let (ax, ay) = (dx.abs(), dy.abs());
    match (dx >= 0, dy >= 0, ax >= ay) {
        (true, true, true) => 0,
        (true, true, false) => 1,
        (false, true, false) => 2,
        (false, true, true) => 3,
        (false, false, true) => 4,
        (false, false, false) => 5,
        (true, false, false) => 6,
        (true, false, true) => 7,
    }
}

const GUARD_LO: [(i64, i64); 8] = [
    (15749, -4516),
    (14330, 7943),
    (4516, 15749),
    (-7943, 14330),
    (-15749, 4516),
    (-14330, -7943),
    (-4516, -15749),
    (7943, -14330),
];
const GUARD_HI: [(i64, i64); 8] = [
    (7943, 14330),
    (-4516, 15749),
    (-14330, 7943),
    (-15749, -4516),
    (-7943, -14330),
    (4516, -15749),
    (14330, -7943),
    (15749, 4516),
];

#[inline]
fn cross(a: (i64, i64), b: (i64, i64)) -> i64 {
    a.0 * b.1 - a.1 * b.0
}

#[inline]
pub fn bin_with_guard(dx: i32, dy: i32, prev: u8) -> u8 {
    if dx == 0 && dy == 0 {
        return if prev < 8 { prev } else { 0 };
    }
    if prev < 8 {
        let v = (dx as i64, dy as i64);
        let p = prev as usize;
        if cross(GUARD_LO[p], v) >= 0 && cross(v, GUARD_HI[p]) >= 0 {
            return prev;
        }
    }
    octant_bin(dx, dy)
}

const HALF_SCALE_TO_FULL: i64 = 2;
const Q8_ONE: i64 = 256;
const COHERENCE_CROSS_SCALE_SQUARED: i64 =
    (HALF_SCALE_TO_FULL * Q8_ONE) * (HALF_SCALE_TO_FULL * Q8_ONE);

#[inline]
pub fn coherence_at_least(half_dx: i32, half_dy: i32, e: u8, t_q8: u8) -> bool {
    let (half_dx, half_dy) = (half_dx as i64, half_dy as i64);
    let half_mag_squared = half_dx * half_dx + half_dy * half_dy;
    let e = e.max(1) as i64;
    let t = t_q8 as i64;
    half_mag_squared * COHERENCE_CROSS_SCALE_SQUARED >= e * e * t * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec2t(theta_deg: f64, mag: f64) -> (i32, i32) {
        let a = (2.0 * theta_deg).to_radians();
        ((mag * a.cos()).round() as i32, (mag * a.sin()).round() as i32)
    }

    #[test]
    fn octants_cover_all_orientations() {
        for k in 0..8u8 {
            let (dx, dy) = vec2t(11.25 + k as f64 * 22.5, 1000.0);
            assert_eq!(octant_bin(dx, dy), k, "center of bin {k}");
        }
    }

    #[test]
    fn guard_holds_within_8_degrees() {
        for theta in [23.0, 25.0, 27.0, 30.0] {
            let (dx, dy) = vec2t(theta, 1000.0);
            assert_eq!(octant_bin(dx, dy), 1, "sanity: raw octant flipped");
            assert_eq!(bin_with_guard(dx, dy, 0), 0, "θ={theta} inside guard");
        }
        let (dx, dy) = vec2t(32.5, 1000.0);
        assert_eq!(bin_with_guard(dx, dy, 0), 1);
    }

    #[test]
    fn guard_is_symmetric_downward() {
        let (dx, dy) = vec2t(19.0, 1000.0);
        assert_eq!(bin_with_guard(dx, dy, 1), 1);
        let (dx, dy) = vec2t(10.0, 1000.0);
        assert_eq!(bin_with_guard(dx, dy, 1), 0);
    }

    #[test]
    fn guard_wraps_at_pi() {
        let (dx, dy) = vec2t(178.0, 1000.0);
        assert_eq!(bin_with_guard(dx, dy, 7), 7);
        let (dx, dy) = vec2t(3.0, 1000.0);
        assert_eq!(bin_with_guard(dx, dy, 7), 7, "3° past wrap is inside guard");
        let (dx, dy) = vec2t(12.0, 1000.0);
        assert_eq!(bin_with_guard(dx, dy, 7), 0, "12° past wrap switches");
    }

    #[test]
    fn unset_prev_takes_raw_octant() {
        let (dx, dy) = vec2t(50.0, 1000.0);
        assert_eq!(bin_with_guard(dx, dy, BIN_UNSET), octant_bin(dx, dy));
        assert_eq!(bin_with_guard(0, 0, BIN_UNSET), 0);
        assert_eq!(bin_with_guard(0, 0, 5), 5, "zero vector keeps history");
    }

    const GUARD_THETA_DEGREES: f64 = 8.0;
    const GUARD_VECTOR_Q14_SCALE: f64 = 16384.0;

    fn q14_unit_at_doubled_angle(degrees: f64) -> (i64, i64) {
        let a = degrees.to_radians();
        let q14 = |v: f64| (v * GUARD_VECTOR_Q14_SCALE).round() as i64;
        (q14(a.cos()), q14(a.sin()))
    }

    #[test]
    fn guard_vectors_widen_each_doubled_angle_sector_by_the_theta_guard() {
        let doubled_guard = 2.0 * GUARD_THETA_DEGREES;
        for k in 0..8 {
            let sector_start = 45.0 * k as f64;
            assert_eq!(GUARD_LO[k], q14_unit_at_doubled_angle(sector_start - doubled_guard));
            assert_eq!(GUARD_HI[k], q14_unit_at_doubled_angle(sector_start + 45.0 + doubled_guard));
        }
    }

    #[test]
    fn coherence_scale_is_the_squared_half_scale_q8_product() {
        assert_eq!(COHERENCE_CROSS_SCALE_SQUARED, 262_144);
    }

    #[test]
    fn coherence_thresholds() {
        assert!(coherence_at_least(100, 0, 200, 255));
        assert!(coherence_at_least(50, 0, 200, 128));
        assert!(!coherence_at_least(50, 0, 200, 129));
        assert!(!coherence_at_least(3, 2, 200, 96));
        assert!(coherence_at_least(1, 0, 0, 255));
    }
}
