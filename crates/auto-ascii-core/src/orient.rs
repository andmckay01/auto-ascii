//! Orientation math (PLAN §3.3): direction bins from the doubled-angle field
//! and the coherence test — sign/comparison only, **no atan2, no floats**.
//!
//! The asset stores `Ex = mag·cos 2θg`, `Ey = mag·sin 2θg` as bias-128 u8 at
//! half scale (`ex = 128 + round(mag·cos 2θg / 2)` so ±mag fits the byte;
//! contract shared with factory `features.rs`/`edges.rs`), where θg is the
//! **gradient** direction in image coordinates (x right, **y down**). The
//! functions here operate on the edge-**tangent** doubled vector — the
//! gradient one negated (doubling turns the 90° tangent rotation into a sign
//! flip); `compose_cell` performs that negation when debiasing.
//! π-periodicity is exactly why the doubled-angle form resamples linearly.
//!
//! Bins: 2θ quantized to 8 octants by sign/|x|-vs-|y| comparisons → 8 edge
//! orientation bins of 22.5° over θ ∈ [0°, 180°). Bin switching carries an
//! 8° (in θ) hysteresis guard implemented with two precomputed Q14 boundary
//! vectors per bin and integer cross products (§3.5 "orientation bin + 8°
//! guard").

/// Sentinel for "no previous bin" (fresh cell / after scene-cut reset).
pub const BIN_UNSET: u8 = 0xFF;

/// Debias a stored Ex/Ey byte to a signed component.
#[inline]
pub fn debias(v: u8) -> i32 {
    v as i32 - 128
}

/// Octant of the doubled angle: bin k ⇔ 2θ ∈ [45k°, 45(k+1)°), i.e. edge
/// orientation θ ∈ [22.5k°, 22.5(k+1)°). Pure sign/comparison tests.
/// `(0, 0)` maps to bin 0 (callers gate on coherence first).
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

/// Q14 boundary vectors of each bin's *expanded* sector in 2θ space:
/// `[45k − 16°, 45(k+1) + 16°]` — ±16° in 2θ = the ±8° guard in θ. Sector
/// width 77° < 180°, so membership is the AND of two cross-product signs.
const GUARD_LO: [(i64, i64); 8] = [
    (15749, -4516),   // −16°
    (14330, 7943),    //  29°
    (4516, 15749),    //  74°
    (-7943, 14330),   // 119°
    (-15749, 4516),   // 164°
    (-14330, -7943),  // 209°
    (-4516, -15749),  // 254°
    (7943, -14330),   // 299°
];
const GUARD_HI: [(i64, i64); 8] = [
    (7943, 14330),    //  61°
    (-4516, 15749),   // 106°
    (-14330, 7943),   // 151°
    (-15749, -4516),  // 196°
    (-7943, -14330),  // 241°
    (4516, -15749),   // 286°
    (14330, -7943),   // 331°
    (15749, 4516),    //  16°
];

#[inline]
fn cross(a: (i64, i64), b: (i64, i64)) -> i64 {
    a.0 * b.1 - a.1 * b.0
}

/// Direction bin with the 8° hysteresis guard: keep `prev` while the vector
/// stays within `prev`'s sector expanded by 8° of θ on both sides; switch
/// (via [`octant_bin`]) only strictly beyond the guard. `prev` ≥ 8 (e.g.
/// [`BIN_UNSET`]) means no history.
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

/// Coherence test (PLAN §3.3): coherence = |(Ex, Ey)| / max(E, 1), where the
/// stored components are half scale, so coherence = 2·|(dx,dy)| / e. Returns
/// whether coherence ≥ `t_q8`/256 — compared squared, no sqrt:
/// `(2·|v|·256)² ≥ (e·t)²  ⇔  mag²·262144 ≥ e²·t²`.
#[inline]
pub fn coherence_at_least(dx: i32, dy: i32, e: u8, t_q8: u8) -> bool {
    let mag2 = (dx as i64) * (dx as i64) + (dy as i64) * (dy as i64);
    let e = e.max(1) as i64;
    let t = t_q8 as i64;
    mag2 * 262144 >= e * e * t * t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Integer doubled-angle vector for an edge orientation θ (test-only float).
    fn vec2t(theta_deg: f64, mag: f64) -> (i32, i32) {
        let a = (2.0 * theta_deg).to_radians();
        ((mag * a.cos()).round() as i32, (mag * a.sin()).round() as i32)
    }

    #[test]
    fn octants_cover_all_orientations() {
        // Bin centers: θ = 11.25° + k·22.5° must land in bin k.
        for k in 0..8u8 {
            let (dx, dy) = vec2t(11.25 + k as f64 * 22.5, 1000.0);
            assert_eq!(octant_bin(dx, dy), k, "center of bin {k}");
        }
    }

    #[test]
    fn guard_holds_within_8_degrees() {
        // Bin 0's upper θ boundary is 22.5°. 2.5°–8° past it: keep bin 0.
        for theta in [23.0, 25.0, 27.0, 30.0] {
            let (dx, dy) = vec2t(theta, 1000.0);
            assert_eq!(octant_bin(dx, dy), 1, "sanity: raw octant flipped");
            assert_eq!(bin_with_guard(dx, dy, 0), 0, "θ={theta} inside guard");
        }
        // Strictly beyond 8°: must switch.
        let (dx, dy) = vec2t(32.5, 1000.0);
        assert_eq!(bin_with_guard(dx, dy, 0), 1);
    }

    #[test]
    fn guard_is_symmetric_downward() {
        // From bin 1 drifting back toward bin 0 (boundary θ = 22.5°).
        let (dx, dy) = vec2t(19.0, 1000.0); // 3.5° back inside bin 0
        assert_eq!(bin_with_guard(dx, dy, 1), 1);
        let (dx, dy) = vec2t(10.0, 1000.0); // 12.5° past — switch
        assert_eq!(bin_with_guard(dx, dy, 1), 0);
    }

    #[test]
    fn guard_wraps_at_pi() {
        // Bin 7 spans θ [157.5°, 180°); its expanded sector crosses 2θ = 0.
        let (dx, dy) = vec2t(178.0, 1000.0);
        assert_eq!(bin_with_guard(dx, dy, 7), 7);
        let (dx, dy) = vec2t(3.0, 1000.0); // θ wraps past 180° → bin 0 raw,
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

    #[test]
    fn coherence_thresholds() {
        // Fully coherent: |v| = e/2 → coherence 1.0.
        assert!(coherence_at_least(100, 0, 200, 255));
        // Half coherent: |v| = e/4 → coherence 0.5 = 128/256.
        assert!(coherence_at_least(50, 0, 200, 128));
        assert!(!coherence_at_least(50, 0, 200, 129));
        // Cancelled directions inside the cell → tiny |v| → suppressed.
        assert!(!coherence_at_least(3, 2, 200, 96));
        // e = 0 clamps to 1 (never divides by zero).
        assert!(coherence_at_least(1, 0, 0, 255));
    }
}
