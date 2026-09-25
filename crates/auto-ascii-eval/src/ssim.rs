//! Windowed SSIM (downscale-SSIM, step 2).
//!
//! Implements mean SSIM exactly as specified in the reference paper:
//! Wang, Bovik, Sheikh & Simoncelli, *"Image Quality Assessment: From Error
//! Visibility to Structural Similarity"*, IEEE Trans. Image Processing 13(4),
//! 2004 — **11×11 Gaussian window, σ = 1.5**, K1 = 0.01, K2 = 0.03, L = 255.
//! The Gaussian window is chosen over the 8×8 uniform window for the reason
//! the paper gives (§III.B): uniform windows produce blocking artifacts in
//! the quality map; the Gaussian is the de-facto standard every reference
//! implementation uses, so our scores stay comparable to other tooling.
//!
//! Windows are "valid"-mode (no padding, like the reference MATLAB
//! implementation's `filter2(..., 'valid')`). Images smaller than 11 px in
//! either dimension fall back to a single uniform window over the whole
//! image — grids that small are below MIN_COLS×MIN_ROWS anyway, the fallback
//! just keeps the metric total on degenerate fuzz-sized inputs.

use crate::raster::GrayImage;

/// Window side (Wang et al. 2004).
pub const SSIM_WINDOW: usize = 11;
/// Gaussian σ (Wang et al. 2004).
pub const SSIM_SIGMA: f64 = 1.5;
/// Stabilizer K1 (paper default).
pub const SSIM_K1: f64 = 0.01;
/// Stabilizer K2 (paper default).
pub const SSIM_K2: f64 = 0.03;
/// Dynamic range for 8-bit images.
pub const SSIM_L: f64 = 255.0;

fn c1() -> f64 {
    (SSIM_K1 * SSIM_L) * (SSIM_K1 * SSIM_L)
}

fn c2() -> f64 {
    (SSIM_K2 * SSIM_L) * (SSIM_K2 * SSIM_L)
}

fn gaussian_kernel() -> [f64; SSIM_WINDOW] {
    let mut k = [0.0; SSIM_WINDOW];
    let c = (SSIM_WINDOW / 2) as f64;
    let mut sum = 0.0;
    for (i, v) in k.iter_mut().enumerate() {
        let d = i as f64 - c;
        *v = (-d * d / (2.0 * SSIM_SIGMA * SSIM_SIGMA)).exp();
        sum += *v;
    }
    for v in &mut k {
        *v /= sum;
    }
    k
}

fn filter_valid(src: &[f64], w: usize, h: usize, k: &[f64; SSIM_WINDOW]) -> Vec<f64> {
    let ow = w - SSIM_WINDOW + 1;
    let oh = h - SSIM_WINDOW + 1;
    let mut hpass = vec![0.0; ow * h];
    for row in 0..h {
        let s = &src[row * w..(row + 1) * w];
        let d = &mut hpass[row * ow..(row + 1) * ow];
        for (x, dv) in d.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (t, &kv) in k.iter().enumerate() {
                acc += kv * s[x + t];
            }
            *dv = acc;
        }
    }
    let mut out = vec![0.0; ow * oh];
    for y in 0..oh {
        for x in 0..ow {
            let mut acc = 0.0;
            for (t, &kv) in k.iter().enumerate() {
                acc += kv * hpass[(y + t) * ow + x];
            }
            out[y * ow + x] = acc;
        }
    }
    out
}

fn global_ssim(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len() as f64;
    let (mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (&pa, &pb) in a.iter().zip(b) {
        let (x, y) = (pa as f64, pb as f64);
        sx += x;
        sy += y;
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
    }
    let (mx, my) = (sx / n, sy / n);
    let vx = sxx / n - mx * mx;
    let vy = syy / n - my * my;
    let cov = sxy / n - mx * my;
    ((2.0 * mx * my + c1()) * (2.0 * cov + c2()))
        / ((mx * mx + my * my + c1()) * (vx + vy + c2()))
}

/// Mean SSIM between two equal-sized grayscale images.
///
/// # Panics
/// On dimension mismatch or empty images.
pub fn ssim(a: &GrayImage, b: &GrayImage) -> f64 {
    assert_eq!((a.w(), a.h()), (b.w(), b.h()), "ssim: image dims mismatch");
    assert!(a.w() > 0 && a.h() > 0, "ssim: empty image");
    let (w, h) = (a.w() as usize, a.h() as usize);
    if w < SSIM_WINDOW || h < SSIM_WINDOW {
        return global_ssim(a.as_slice(), b.as_slice());
    }

    let n = w * h;
    let mut fx = vec![0.0f64; n];
    let mut fy = vec![0.0f64; n];
    let mut fxx = vec![0.0f64; n];
    let mut fyy = vec![0.0f64; n];
    let mut fxy = vec![0.0f64; n];
    for i in 0..n {
        let x = a.as_slice()[i] as f64;
        let y = b.as_slice()[i] as f64;
        fx[i] = x;
        fy[i] = y;
        fxx[i] = x * x;
        fyy[i] = y * y;
        fxy[i] = x * y;
    }

    let k = gaussian_kernel();
    let mx = filter_valid(&fx, w, h, &k);
    let my = filter_valid(&fy, w, h, &k);
    let mxx = filter_valid(&fxx, w, h, &k);
    let myy = filter_valid(&fyy, w, h, &k);
    let mxy = filter_valid(&fxy, w, h, &k);

    let (c1, c2) = (c1(), c2());
    let mut sum = 0.0;
    for i in 0..mx.len() {
        let (ux, uy) = (mx[i], my[i]);
        let vx = mxx[i] - ux * ux;
        let vy = myy[i] - uy * uy;
        let cov = mxy[i] - ux * uy;
        sum += ((2.0 * ux * uy + c1) * (2.0 * cov + c2))
            / ((ux * ux + uy * uy + c1) * (vx + vy + c2));
    }
    sum / mx.len() as f64
}

/// The downscale-SSIM entry point: resample the source luma plane to the
/// rendered raster's dimensions through the engine's own separable resampler
/// (same box-average semantics the player uses), then SSIM.
///
/// `rendered` should already be cropped to the viewport region
/// ([`GrayImage::crop`]) so letterbox pads don't enter the comparison.
///
/// # Panics
/// If `src_luma.len() != src_w · src_h`, or `rendered` is empty.
pub fn downscale_ssim(rendered: &GrayImage, src_luma: &[u8], src_w: u16, src_h: u16) -> f64 {
    assert!(rendered.w() > 0 && rendered.h() > 0, "downscale_ssim: empty render");
    let mut rs = auto_ascii_core::Resampler::build(src_w, src_h, rendered.w(), rendered.h());
    let mut dst = vec![0u8; rendered.w() as usize * rendered.h() as usize];
    rs.apply(src_luma, &mut dst);
    ssim(&GrayImage::from_raw(rendered.w(), rendered.h(), dst), rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn next_u8(&mut self) -> u8 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 56) as u8
        }
    }

    fn gradient(w: u16, h: u16) -> GrayImage {
        let mut data = Vec::with_capacity(w as usize * h as usize);
        for y in 0..h {
            for x in 0..w {
                let v = (x as u32 * 255 / (w as u32 - 1) + y as u32 * 255 / (h as u32 - 1)) / 2;
                data.push(v as u8);
            }
        }
        GrayImage::from_raw(w, h, data)
    }

    fn add_noise(img: &GrayImage, amp: i32, seed: u64) -> GrayImage {
        let mut rng = Lcg(seed);
        let data = img
            .as_slice()
            .iter()
            .map(|&v| {
                let n = (rng.next_u8() as i32 * 2 * amp / 255) - amp;
                (v as i32 + n).clamp(0, 255) as u8
            })
            .collect();
        GrayImage::from_raw(img.w(), img.h(), data)
    }

    #[test]
    fn ssim_of_identical_is_exactly_one() {
        let img = gradient(64, 48);
        assert_eq!(ssim(&img, &img), 1.0);
        let tiny = gradient(5, 5);
        assert_eq!(ssim(&tiny, &tiny), 1.0);
    }

    #[test]
    fn noise_lowers_ssim_monotonically() {
        let base = gradient(64, 64);
        let s8 = ssim(&base, &add_noise(&base, 8, 1));
        let s32 = ssim(&base, &add_noise(&base, 32, 1));
        let s96 = ssim(&base, &add_noise(&base, 96, 1));
        assert!(s8 < 1.0, "s8 = {s8}");
        assert!(s32 < s8, "s32 = {s32} !< s8 = {s8}");
        assert!(s96 < s32, "s96 = {s96} !< s32 = {s32}");
        assert!(s96 > -1.0 && s96 < 0.9);
    }

    #[test]
    fn structurally_unrelated_images_score_low() {
        let a = GrayImage::from_raw(32, 32, vec![0; 1024]);
        let b = GrayImage::from_raw(32, 32, vec![255; 1024]);
        assert!(ssim(&a, &b) < 0.05);
    }

    #[test]
    fn downscale_ssim_identity_dims_is_one() {
        let img = gradient(48, 27);
        let s = downscale_ssim(&img, img.as_slice(), 48, 27);
        assert_eq!(s, 1.0);
    }

    #[test]
    fn downscale_ssim_prefers_faithful_render() {
        let src = gradient(480, 270);
        let mut rs = auto_ascii_core::Resampler::build(480, 270, 96, 54);
        let mut faithful = vec![0u8; 96 * 54];
        rs.apply(src.as_slice(), &mut faithful);
        let inverted: Vec<u8> = faithful.iter().map(|&v| 255 - v).collect();

        let s_good = downscale_ssim(&GrayImage::from_raw(96, 54, faithful), src.as_slice(), 480, 270);
        let s_bad = downscale_ssim(&GrayImage::from_raw(96, 54, inverted), src.as_slice(), 480, 270);
        assert!(s_good > 0.99, "faithful render should score ≈1, got {s_good}");
        assert!(s_bad < s_good - 0.5, "inverted render must score far lower, got {s_bad}");
    }

    #[test]
    #[should_panic(expected = "dims mismatch")]
    fn rejects_dim_mismatch() {
        let a = GrayImage::new(4, 4);
        let b = GrayImage::new(5, 4);
        let _ = ssim(&a, &b);
    }
}
