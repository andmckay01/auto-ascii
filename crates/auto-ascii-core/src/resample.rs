//! Separable box resampler with precomputed Q8 tap tables (PLAN §3.3).
//!
//! Planes live at 480×270 u8 (chroma 240×135 at M1+). Per output cell we box-
//! average a fractional source rect, done as two 1-D passes: H-pass into a
//! shared `u16` buffer, V-pass with `u32` accumulator, `>> 16` out. No floats
//! in the hot loop; fixed trip counts autovectorize under `-O3` — no hand SIMD
//! (80/20) unless profiling shows > 2 ms.
//!
//! Tables are rebuilt on resize only (~50 µs, < 4 KB). Upscale degrades
//! naturally to 1–2 linear taps (bilinear); same code, no branch.
//!
//! Table construction is pure integer rational arithmetic (no floats anywhere
//! in this module), so tap tables and output are byte-deterministic across
//! platforms — a golden-test requirement (PLAN §4/§6).
//!
//! M3 note (PLAN §3.3): luma is resampled at `Vc × 2·Vr` through this exact
//! code path (half-block fills / subposition glyphs); M0 uses `Vc × Vr`.

/// Tap table entry for one output coordinate along one axis (PLAN §3.3).
///
/// Q8 fixed point: the `ntaps` weights sum to 256. Deviation from the PLAN
/// sketch (`w: [u16]` inline / digest's `w[MAXTAP]`): weights live in a shared
/// pool inside [`Resampler`] and `w_off` indexes it — a fixed `MAXTAP` cannot
/// cover extreme downscales (480 source columns → a 1-col viewport is a legal
/// fuzz case, PLAN §6), while a pool keeps `Tap1D` a fixed-size POD. Recorded
/// in docs/INTERFACES.md.
///
/// `ntaps` is `u16` (widened from the scaffold's `u8`, recorded in
/// docs/INTERFACES.md): the same 480→1 fuzz case needs 480 taps in one run, which
/// overflows `u8`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tap1D {
    /// First source index covered by this output sample.
    pub src_start: u16,
    /// Number of consecutive source samples (≥ 1).
    pub ntaps: u16,
    /// Offset of this entry's `ntaps` Q8 weights in the resampler's weight pool.
    pub w_off: u32,
}

/// Precomputed separable resampler for one (src, dst) dimension pair
/// (PLAN §3.3). Build once per resize per plane geometry; `apply` per frame.
#[derive(Clone, Debug)]
pub struct Resampler {
    taps_x: Vec<Tap1D>,
    taps_y: Vec<Tap1D>,
    /// Shared Q8 weight pool referenced by `Tap1D::w_off`; each run sums to 256.
    weights: Vec<u16>,
    /// Shared H-pass intermediate buffer (`dst_w × src_h` u16), reused across
    /// frames — no per-frame allocation (PLAN §6).
    hbuf: Vec<u16>,
    src_w: u16,
    src_h: u16,
    dst_w: u16,
    dst_h: u16,
}

/// Append the box tap run for every output index along one axis.
///
/// Output interval `i` covers source span `[i·src/dst, (i+1)·src/dst)`. All
/// arithmetic is exact integer rational, scaled by `dst`: source pixel `s`
/// spans `[s·dst, (s+1)·dst)` and the output interval spans
/// `[i·src, (i+1)·src)` (length `src`). Weights are assigned by the cumulative
/// method — `w_j = floor(cum_{j+1}·256/src) − floor(cum_j·256/src)` — which
/// guarantees every run sums to exactly 256 with no float in sight.
fn build_axis(src: u16, dst: u16, taps: &mut Vec<Tap1D>, weights: &mut Vec<u16>) {
    let src = src.max(1) as u64;
    let dst = dst.max(1) as u64;
    taps.reserve(dst as usize);
    for i in 0..dst {
        let left = i * src; // scaled by dst
        let right = (i + 1) * src;
        let s0 = left / dst; // floor(left / dst)
        let s1 = right.div_ceil(dst); // exclusive end, ceil(right / dst)
        debug_assert!(s1 > s0 && s1 <= src);
        let w_off = weights.len() as u32;
        let mut cum: u64 = 0; // cumulative overlap, scaled by dst
        let mut prev_q: u64 = 0; // floor(cum * 256 / span)
        let span = right - left; // == src
        for s in s0..s1 {
            let lo = left.max(s * dst);
            let hi = right.min((s + 1) * dst);
            cum += hi - lo;
            let q = cum * 256 / span;
            weights.push((q - prev_q) as u16);
            prev_q = q;
        }
        debug_assert_eq!(prev_q, 256);
        taps.push(Tap1D {
            src_start: s0 as u16,
            ntaps: (s1 - s0) as u16,
            w_off,
        });
    }
}

impl Resampler {
    /// Build tap tables mapping a `src_w × src_h` u8 plane onto `dst_w × dst_h`.
    ///
    /// Called on resize only (PLAN §3.6 step 1); ~50 µs, < 4 KB. All dims are
    /// clamped ≥ 1. Box weights are exact Q8 (each axis run sums to 256), so
    /// output is deterministic across platforms — a golden-test requirement.
    pub fn build(src_w: u16, src_h: u16, dst_w: u16, dst_h: u16) -> Resampler {
        let (src_w, src_h) = (src_w.max(1), src_h.max(1));
        let (dst_w, dst_h) = (dst_w.max(1), dst_h.max(1));
        let mut taps_x = Vec::new();
        let mut taps_y = Vec::new();
        let mut weights = Vec::new();
        build_axis(src_w, dst_w, &mut taps_x, &mut weights);
        build_axis(src_h, dst_h, &mut taps_y, &mut weights);
        Resampler {
            taps_x,
            taps_y,
            weights,
            hbuf: vec![0u16; dst_w as usize * src_h as usize],
            src_w,
            src_h,
            dst_w,
            dst_h,
        }
    }

    /// Resample one u8 plane. `src.len()` must be `src_w × src_h`,
    /// `dst.len()` at least `dst_w × dst_h`. Zero allocation; H-pass into the
    /// internal shared `u16` buffer, V-pass accumulates in `u32`, rounds and
    /// shifts out (`(acc + 0x8000) >> 16`). No floats.
    ///
    /// Range safety: each Q8 run sums to exactly 256, so H-pass values fit
    /// `u16` (≤ 256·255 = 65 280) and the V-pass accumulator fits `u32`
    /// (≤ 256·65 280 + 0x8000), and the shifted result is always ≤ 255 —
    /// no clamp needed.
    pub fn apply(&mut self, src: &[u8], dst: &mut [u8]) {
        let sw = self.src_w as usize;
        let sh = self.src_h as usize;
        let dw = self.dst_w as usize;
        let dh = self.dst_h as usize;
        assert_eq!(src.len(), sw * sh, "src plane size mismatch");
        assert!(dst.len() >= dw * dh, "dst buffer too small");

        // Disjoint field borrows: taps/weights read-only, hbuf written.
        let taps_x = &self.taps_x;
        let taps_y = &self.taps_y;
        let weights = &self.weights;
        let hbuf = &mut self.hbuf;

        // H-pass: each source row → dst_w u16 samples.
        for y in 0..sh {
            let srow = &src[y * sw..y * sw + sw];
            let hrow = &mut hbuf[y * dw..y * dw + dw];
            for (h, t) in hrow.iter_mut().zip(taps_x) {
                let run = &weights[t.w_off as usize..t.w_off as usize + t.ntaps as usize];
                let px = &srow[t.src_start as usize..t.src_start as usize + t.ntaps as usize];
                let mut acc: u32 = 0;
                for (&w, &p) in run.iter().zip(px) {
                    acc += w as u32 * p as u32;
                }
                *h = acc as u16; // ≤ 65 280
            }
        }

        // V-pass: dst_h × dst_w, u32 accumulator, round + shift out.
        for (y, t) in taps_y.iter().enumerate().take(dh) {
            let run = &weights[t.w_off as usize..t.w_off as usize + t.ntaps as usize];
            let drow = &mut dst[y * dw..y * dw + dw];
            for x in 0..dw {
                let mut acc: u32 = 0;
                for (j, &w) in run.iter().enumerate() {
                    let sy = t.src_start as usize + j;
                    acc += w as u32 * hbuf[sy * dw + x] as u32;
                }
                drow[x] = ((acc + 0x8000) >> 16) as u8;
            }
        }
    }

    #[inline]
    pub fn src_dims(&self) -> (u16, u16) {
        (self.src_w, self.src_h)
    }

    #[inline]
    pub fn dst_dims(&self) -> (u16, u16) {
        (self.dst_w, self.dst_h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift64* — property-style tests without a rand dep.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545F4914F6CDD1D)
        }
        fn range(&mut self, lo: u16, hi: u16) -> u16 {
            lo + (self.next() % (hi - lo + 1) as u64) as u16
        }
    }

    fn check_tap_invariants(r: &Resampler) {
        let (sw, sh) = r.src_dims();
        let (dw, dh) = r.dst_dims();
        assert_eq!(r.taps_x.len(), dw as usize);
        assert_eq!(r.taps_y.len(), dh as usize);
        for (taps, src_len) in [(&r.taps_x, sw), (&r.taps_y, sh)] {
            for t in taps.iter() {
                assert!(t.ntaps >= 1);
                // OOB safety: run stays inside the source axis.
                assert!(t.src_start as usize + t.ntaps as usize <= src_len as usize);
                let run =
                    &r.weights[t.w_off as usize..t.w_off as usize + t.ntaps as usize];
                assert_eq!(run.iter().map(|&w| w as u32).sum::<u32>(), 256);
            }
        }
    }

    /// PLAN §3.3: Q8 weights sum to 256 for every output index — random sizes.
    #[test]
    fn weight_sums_are_256_for_random_sizes() {
        let mut rng = Rng(0x5EED_1234_ABCD_EF01);
        for _ in 0..200 {
            let sw = rng.range(1, 700);
            let sh = rng.range(1, 400);
            let dw = rng.range(1, 700);
            let dh = rng.range(1, 400);
            let r = Resampler::build(sw, sh, dw, dh);
            check_tap_invariants(&r);
        }
        // The extreme-downscale cases that motivated the pool + u16 ntaps.
        for (sw, sh, dw, dh) in [(480, 270, 1, 1), (480, 270, 3, 2), (1, 1, 480, 270)] {
            let r = Resampler::build(sw, sh, dw, dh);
            check_tap_invariants(&r);
        }
    }

    /// Uniform input must stay exactly uniform at any geometry (weights are
    /// exact Q8, so v·65536 + 0x8000 >> 16 == v).
    #[test]
    fn uniform_input_invariance() {
        let mut rng = Rng(0xDEAD_BEEF_0BAD_F00D);
        for &v in &[0u8, 1, 127, 128, 254, 255] {
            for _ in 0..30 {
                let sw = rng.range(1, 480);
                let sh = rng.range(1, 270);
                let dw = rng.range(1, 350);
                let dh = rng.range(1, 200);
                let mut r = Resampler::build(sw, sh, dw, dh);
                let src = vec![v; sw as usize * sh as usize];
                let mut dst = vec![0u8; dw as usize * dh as usize];
                r.apply(&src, &mut dst);
                assert!(
                    dst.iter().all(|&d| d == v),
                    "uniform {v} broke at {sw}x{sh}->{dw}x{dh}"
                );
            }
        }
    }

    /// Integer 2:1 downscale is an exact 2×2 box average.
    #[test]
    fn two_to_one_exact_box_average() {
        let sw = 6u16;
        let sh = 4u16;
        #[rustfmt::skip]
        let src: Vec<u8> = vec![
            10,  20,  0, 255,  8,  8,
            30,  40, 255, 0,  8,  8,
            100, 100, 50, 50,  1,  3,
            100, 100, 50, 54,  5,  7,
        ];
        let mut r = Resampler::build(sw, sh, 3, 2);
        let mut dst = vec![0u8; 6];
        r.apply(&src, &mut dst);
        // 2×2 block means: (10+20+30+40)/4=25, (0+255+255+0)/4=127.5→128 (round
        // half up), (8·4)/4=8, (100·4)/4=100, (50+50+50+54)/4=51, (1+3+5+7)/4=4.
        assert_eq!(dst, vec![25, 128, 8, 100, 51, 4]);
    }

    /// 480×270 → 240×135 (the real chroma geometry) stays an exact 2×2 mean.
    #[test]
    fn base_plane_halving_matches_reference() {
        let (sw, sh, dw, dh) = (480u16, 270u16, 240u16, 135u16);
        let src: Vec<u8> = (0..sw as usize * sh as usize)
            .map(|i| ((i * 7 + i / 480 * 13) % 251) as u8)
            .collect();
        let mut r = Resampler::build(sw, sh, dw, dh);
        let mut dst = vec![0u8; dw as usize * dh as usize];
        r.apply(&src, &mut dst);
        for y in 0..dh as usize {
            for x in 0..dw as usize {
                let s = |dx: usize, dy: usize| {
                    src[(2 * y + dy) * sw as usize + 2 * x + dx] as u32
                };
                let sum = s(0, 0) + s(1, 0) + s(0, 1) + s(1, 1);
                let expect = ((sum * 16384 + 0x8000) >> 16) as u8; // round(sum/4)
                assert_eq!(dst[y * dw as usize + x], expect, "at ({x},{y})");
            }
        }
    }

    /// Edge safety + range sanity on hostile geometries: no panic, no OOB,
    /// every output within [min, max] of the source (box averages can't
    /// overshoot, and rounding can't escape an integer-bounded interval).
    #[test]
    fn out_of_bounds_safety_and_range_at_edges() {
        let mut rng = Rng(0x0123_4567_89AB_CDEF);
        let geoms: &[(u16, u16, u16, u16)] = &[
            (480, 270, 1, 1),     // full collapse
            (480, 270, 1, 270),   // width collapse
            (480, 270, 480, 1),   // height collapse
            (1, 1, 320, 90),      // extreme upscale
            (2, 2, 321, 91),      // odd upscale
            (480, 270, 479, 269), // near-identity
            (480, 270, 481, 271), // just past identity
            (3, 3, 3, 3),         // identity
        ];
        for &(sw, sh, dw, dh) in geoms {
            let mut src = vec![0u8; sw as usize * sh as usize];
            for b in src.iter_mut() {
                *b = (rng.next() & 0xFF) as u8;
            }
            let (lo, hi) = (
                *src.iter().min().unwrap(),
                *src.iter().max().unwrap(),
            );
            let mut r = Resampler::build(sw, sh, dw, dh);
            check_tap_invariants(&r);
            let mut dst = vec![0u8; dw as usize * dh as usize];
            r.apply(&src, &mut dst);
            assert!(
                dst.iter().all(|&d| d >= lo && d <= hi),
                "range escape at {sw}x{sh}->{dw}x{dh}"
            );
        }
    }

    /// Identity geometry is a byte-for-byte copy.
    #[test]
    fn identity_is_exact() {
        let (w, h) = (33u16, 17u16);
        let src: Vec<u8> = (0..w as usize * h as usize)
            .map(|i| (i % 256) as u8)
            .collect();
        let mut r = Resampler::build(w, h, w, h);
        let mut dst = vec![0u8; src.len()];
        r.apply(&src, &mut dst);
        assert_eq!(src, dst);
    }

    /// apply() is deterministic and reusable (double-apply, same output) and
    /// dims are clamped ≥ 1.
    #[test]
    fn deterministic_and_clamped() {
        let r = Resampler::build(0, 0, 0, 0);
        assert_eq!(r.src_dims(), (1, 1));
        assert_eq!(r.dst_dims(), (1, 1));

        let src: Vec<u8> = (0..480 * 270).map(|i| (i % 253) as u8).collect();
        let mut r = Resampler::build(480, 270, 206, 58);
        let mut a = vec![0u8; 206 * 58];
        let mut b = vec![255u8; 206 * 58];
        r.apply(&src, &mut a);
        r.apply(&src, &mut b);
        assert_eq!(a, b);
    }
}
