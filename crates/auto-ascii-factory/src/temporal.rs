//! Temporal EMA on feature planes: sensor-noise
//! suppression, the first line of anti-flicker defense. Reset at shot cuts —
//! blending across a hard cut ghosts the old scene into the new one.
//!
//! Pure integer fixed point (byte-determinism): the accumulator
//! holds `value · 256` (Q8) per pixel, alpha is Q8 derived from the
//! params.toml milli value. `alpha_milli = 1000` (Q8 256) is an exact
//! passthrough, so "EMA off" costs nothing in precision. Update rule:
//!
//! ```text
//!   acc += (alpha_q8 · ((new << 8) − acc) + 128) >> 8   // round-half-up
//!   out  = (acc + 128) >> 8                             // Q8 → value
//! ```
//!
//! The state converges to within one Q8 LSB of a held input, which rounds to
//! the exact input byte — a static scene reaches a byte-stable plane (the
//! flicker gate's premise). One `EmaPlane` per stored plane channel; the
//! accumulator is the ONLY cross-frame state in the factory's extract stage
//! (O(plane), never O(frames) — see the memory note in `features.rs`).

/// One plane's EMA state.
pub struct EmaPlane {
    acc: Vec<i32>,
    alpha_q8: i32,
    primed: bool,
}

impl EmaPlane {
    /// `alpha_milli` per params.toml `[temporal]` (validated 1..=1000).
    pub fn new(len: usize, alpha_milli: u32) -> EmaPlane {
        let alpha_q8 = ((alpha_milli * 256 + 500) / 1000).clamp(1, 256) as i32;
        EmaPlane { acc: vec![0; len], alpha_q8, primed: false }
    }

    /// Forget all state; the next apply primes from its input verbatim
    /// (shot-cut reset).
    pub fn reset(&mut self) {
        self.primed = false;
    }

    /// Smooth an unsigned plane: updates the state and writes the quantized
    /// result. `src`/`out` lengths must equal the constructed plane length.
    pub fn apply_u8(&mut self, src: &[u8], out: &mut [u8]) {
        assert_eq!(src.len(), self.acc.len());
        assert_eq!(out.len(), self.acc.len());
        if !self.primed {
            for ((a, &s), o) in self.acc.iter_mut().zip(src).zip(out.iter_mut()) {
                *a = i32::from(s) << 8;
                *o = s;
            }
            self.primed = true;
            return;
        }
        for ((a, &s), o) in self.acc.iter_mut().zip(src).zip(out.iter_mut()) {
            let delta = (i32::from(s) << 8) - *a;
            *a += (self.alpha_q8 * delta + 128) >> 8;
            *o = ((*a + 128) >> 8).clamp(0, 255) as u8;
        }
    }

    /// Smooth a signed plane (the pre-quantization Ex/Ey fields, ±255).
    pub fn apply_i16(&mut self, src: &[i16], out: &mut [i16]) {
        assert_eq!(src.len(), self.acc.len());
        assert_eq!(out.len(), self.acc.len());
        if !self.primed {
            for ((a, &s), o) in self.acc.iter_mut().zip(src).zip(out.iter_mut()) {
                *a = i32::from(s) << 8;
                *o = s;
            }
            self.primed = true;
            return;
        }
        for ((a, &s), o) in self.acc.iter_mut().zip(src).zip(out.iter_mut()) {
            let delta = (i32::from(s) << 8) - *a;
            *a += (self.alpha_q8 * delta + 128) >> 8;
            *o = ((*a + 128) >> 8) as i16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_1000_is_exact_passthrough() {
        let mut ema = EmaPlane::new(3, 1000);
        let mut out = [0u8; 3];
        ema.apply_u8(&[0, 137, 255], &mut out);
        assert_eq!(out, [0, 137, 255]);
        ema.apply_u8(&[255, 3, 0], &mut out);
        assert_eq!(out, [255, 3, 0], "alpha 256/256 must not lag at all");
    }

    #[test]
    fn half_alpha_converges_geometrically_and_settles_exactly() {
        let mut ema = EmaPlane::new(1, 500);
        let mut out = [0u8; 1];
        ema.apply_u8(&[100], &mut out);
        assert_eq!(out, [100], "first frame primes verbatim");
        ema.apply_u8(&[200], &mut out);
        assert_eq!(out, [150]);
        ema.apply_u8(&[200], &mut out);
        assert_eq!(out, [175]);
        for _ in 0..40 {
            ema.apply_u8(&[200], &mut out);
        }
        assert_eq!(out, [200]);
        ema.apply_u8(&[200], &mut out);
        assert_eq!(out, [200], "settled state must be byte-stable");
    }

    #[test]
    fn reset_reprimes_from_the_next_frame() {
        let mut ema = EmaPlane::new(2, 500);
        let mut out = [0u8; 2];
        ema.apply_u8(&[10, 240], &mut out);
        ema.reset();
        ema.apply_u8(&[200, 20], &mut out);
        assert_eq!(out, [200, 20], "post-cut frame must carry zero pre-cut state");
        ema.apply_u8(&[100, 20], &mut out);
        assert_eq!(out, [150, 20], "blending resumes from the re-primed state");
    }

    #[test]
    fn signed_planes_blend_and_settle_symmetrically() {
        let mut ema = EmaPlane::new(2, 500);
        let mut out = [0i16; 2];
        ema.apply_i16(&[-200, 200], &mut out);
        assert_eq!(out, [-200, 200]);
        ema.apply_i16(&[0, 0], &mut out);
        assert_eq!(out, [-100, 100]);
        for _ in 0..40 {
            ema.apply_i16(&[255, -255], &mut out);
        }
        assert_eq!(out, [255, -255], "must settle on signed extremes exactly");
    }
}
