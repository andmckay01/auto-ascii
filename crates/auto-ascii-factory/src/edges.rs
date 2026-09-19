//! Edge extraction (PLAN §5 stage 3, M3): Scharr gradients on the stored
//! (EMA'd) L\* plane → doubled-angle orientation field → two passes of
//! orientation-aware bilateral smoothing → hysteresis-thresholded
//! **unthinned** edge magnitude (thinning breaks under resampling, PLAN §5).
//!
//! ## The doubled-angle field (PLAN §3.3 / §4)
//!
//! Orientation is π-periodic; averaging angles is wrong. We store the
//! doubled-angle vector, which is linear under averaging/resampling:
//!
//! ```text
//!   vx = m·cos 2θg = m·(gx² − gy²) / (gx² + gy²)
//!   vy = m·sin 2θg = m·(2·gx·gy)   / (gx² + gy²)
//! ```
//!
//! computed **rationally** (no trig, no floats — byte-determinism), where
//! `θg` is the GRADIENT direction in a y-down raster and `m` the shifted
//! Scharr magnitude. The edge *tangent* doubled-angle vector is `−(vx, vy)`
//! (doubling turns the 90° tangent rotation into a negation) — the player's
//! LUT accounts for that; the asset stores the gradient convention.
//! Canonical bins (§3.3 sign tests): vertical edge → `vx ≈ +m`; horizontal
//! edge → `vx ≈ −m`; the two diagonals → `vy ≈ ±m`.
//!
//! ## Orientation-aware bilateral smoothing (2 passes)
//!
//! Pragmatist cut of Kang-style ETF (PLAN §5): each pass replaces a pixel's
//! vector with a weighted average over a `(2r+1)²` window; the weight is a
//! spatial falloff times `max(dot(v_c, v_n), 0)` — magnitude-proportional
//! (strong edges dominate) and alignment-gated (perpendicular doubled
//! vectors have negative dot and contribute nothing, so structure corners
//! don't smear). Zero-vector centers instead take magnitude-weighted
//! neighborhoods, letting coherent orientation grow into weak pixels.
//!
//! Like real ETF, the passes smooth the ORIENTATION only. E itself stays
//! the *local* Scharr magnitude — spatially smoothing E would smear thin
//! contours into plateaus (the exact failure thinning-free extraction must
//! avoid); E's "smoothed" in PLAN §4 is delivered temporally by the EMA
//! stage. After the passes the field is re-capped to `|v| ≤ E` per pixel:
//! coherent neighborhoods keep full magnitude, incoherent ones (window
//! cancellation) store a shorter vector — exactly the §3.3 coherence
//! signal, now baked in per pixel and preserved by resampling.
//!
//! E is hysteresis-thresholded (strong ≥ `t_hi` seeds, `t_lo..t_hi` kept
//! when 8-connected to a seed), never thinned; suppressed pixels zero the
//! vector too, so cell-level direction bins never aggregate sub-threshold
//! noise.

/// Effective `[edges]` config (validated params, narrowed to native types).
#[derive(Clone, Copy, Debug)]
pub struct EdgeConfig {
    pub scharr_shift: u32,
    pub passes: u32,
    pub radius: usize,
    pub t_hi: u8,
    pub t_lo: u8,
}

impl EdgeConfig {
    pub fn from_params(p: &crate::params::EdgesParams) -> EdgeConfig {
        EdgeConfig {
            scharr_shift: p.scharr_shift,
            passes: p.bilateral_passes,
            radius: p.bilateral_radius as usize,
            t_hi: p.t_hi as u8,
            t_lo: p.t_lo as u8,
        }
    }
}

/// Reusable per-frame edge extractor: all buffers allocated once at build
/// start (~1.8 MB at 480×270 — see the features.rs memory note).
pub struct EdgeExtractor {
    w: usize,
    h: usize,
    gx: Vec<i16>,
    gy: Vec<i16>,
    /// Doubled-angle field, ping-pong pair (±255 domain).
    vx: Vec<i16>,
    vy: Vec<i16>,
    vx2: Vec<i16>,
    vy2: Vec<i16>,
    /// Local Scharr magnitude (E before thresholding). Fixed across the
    /// smoothing passes (module docs: orientation smooths, E stays local).
    mag: Vec<u8>,
    /// Dilated-magnitude activity guard: pixels whose whole bilateral window
    /// is zero-magnitude are skipped (most of a flat frame).
    active: Vec<u8>,
    tmp: Vec<u8>,
    keep: Vec<u8>,
    stack: Vec<u32>,
}

/// `n / d` rounded half away from zero, `d > 0`.
#[inline]
fn div_round(n: i64, d: i64) -> i64 {
    if n >= 0 { (n + d / 2) / d } else { (n - d / 2) / d }
}

impl EdgeExtractor {
    pub fn new(w: u16, h: u16) -> EdgeExtractor {
        let n = w as usize * h as usize;
        EdgeExtractor {
            w: w as usize,
            h: h as usize,
            gx: vec![0; n],
            gy: vec![0; n],
            vx: vec![0; n],
            vy: vec![0; n],
            vx2: vec![0; n],
            vy2: vec![0; n],
            mag: vec![0; n],
            active: vec![0; n],
            tmp: vec![0; n],
            keep: vec![0; n],
            stack: Vec::new(),
        }
    }

    /// Extract from one luma plane. Results via [`e`](EdgeExtractor::e) /
    /// [`vx`](EdgeExtractor::vx) / [`vy`](EdgeExtractor::vy) (pre-EMA).
    pub fn run(&mut self, luma: &[u8], cfg: &EdgeConfig) {
        assert_eq!(luma.len(), self.w * self.h);
        self.scharr(luma);
        self.build_field(cfg.scharr_shift);
        if cfg.passes > 0 {
            // Activity guard, once: E is fixed across passes, and a pass
            // never creates vectors outside dilate(E-support, radius).
            let (mag, tmp, active) = (&self.mag, &mut self.tmp, &mut self.active);
            dilate_box(mag, self.w, self.h, cfg.radius, tmp, active);
            for _ in 0..cfg.passes {
                self.bilateral_pass(cfg.radius);
                std::mem::swap(&mut self.vx, &mut self.vx2);
                std::mem::swap(&mut self.vy, &mut self.vy2);
            }
        }
        self.cap_field();
        hysteresis_mask(
            &self.mag, self.w, self.h, cfg.t_hi, cfg.t_lo, &mut self.keep, &mut self.stack,
        );
        for i in 0..self.mag.len() {
            if self.keep[i] == 0 {
                self.mag[i] = 0;
                self.vx[i] = 0;
                self.vy[i] = 0;
            }
        }
    }

    /// Thresholded, unthinned edge magnitude (pre-EMA).
    pub fn e(&self) -> &[u8] {
        &self.mag
    }

    /// Doubled-angle x component (±255, gradient convention, pre-EMA).
    pub fn vx(&self) -> &[i16] {
        &self.vx
    }

    /// Doubled-angle y component (±255, gradient convention, pre-EMA).
    pub fn vy(&self) -> &[i16] {
        &self.vy
    }

    /// Scharr 3×3 (PLAN §5: better rotational symmetry than Sobel), border
    /// pixels replicate-clamped. `|gx|,|gy| ≤ 16·255 = 4080` fits i16.
    fn scharr(&mut self, luma: &[u8]) {
        let (w, h) = (self.w, self.h);
        for y in 0..h {
            let ym = y.saturating_sub(1);
            let yp = (y + 1).min(h - 1);
            for x in 0..w {
                let xm = x.saturating_sub(1);
                let xp = (x + 1).min(w - 1);
                let px = |yy: usize, xx: usize| i32::from(luma[yy * w + xx]);
                let (a, b, c) = (px(ym, xm), px(ym, x), px(ym, xp));
                let (d, f) = (px(y, xm), px(y, xp));
                let (g, hh, i) = (px(yp, xm), px(yp, x), px(yp, xp));
                let idx = y * w + x;
                self.gx[idx] = (3 * (c - a) + 10 * (f - d) + 3 * (i - g)) as i16;
                self.gy[idx] = (3 * (g - a) + 10 * (hh - b) + 3 * (i - c)) as i16;
            }
        }
    }

    /// Magnitude + rational doubled-angle vector per pixel (module docs).
    fn build_field(&mut self, shift: u32) {
        for i in 0..self.gx.len() {
            let gx = i64::from(self.gx[i]);
            let gy = i64::from(self.gy[i]);
            let den = gx * gx + gy * gy;
            if den == 0 {
                self.mag[i] = 0;
                self.vx[i] = 0;
                self.vy[i] = 0;
                continue;
            }
            let m = ((den as u64).isqrt() >> shift).min(255) as i64;
            self.mag[i] = m as u8;
            self.vx[i] = div_round(m * (gx * gx - gy * gy), den) as i16;
            self.vy[i] = div_round(m * (2 * gx * gy), den) as i16;
        }
    }

    /// One orientation-aware bilateral pass `(vx,vy) → (vx2,vy2)` within
    /// the precomputed activity region.
    fn bilateral_pass(&mut self, r: usize) {
        let (w, h) = (self.w, self.h);
        let max_d2 = 2 * (r * r) as i64;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if self.active[i] == 0 {
                    self.vx2[i] = 0;
                    self.vy2[i] = 0;
                    continue;
                }
                let cx = i64::from(self.vx[i]);
                let cy = i64::from(self.vy[i]);
                let center_nonzero = cx != 0 || cy != 0;
                let (mut ax, mut ay, mut aw) = (0i64, 0i64, 0i64);
                for dy in -(r as isize)..=r as isize {
                    let yy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                    for dx in -(r as isize)..=r as isize {
                        let xx = (x as isize + dx).clamp(0, w as isize - 1) as usize;
                        let j = yy * w + xx;
                        let nx = i64::from(self.vx[j]);
                        let ny = i64::from(self.vy[j]);
                        if nx == 0 && ny == 0 {
                            continue;
                        }
                        // Alignment/magnitude term (module docs): dot for
                        // oriented centers, |v_n|² fill-in for zero centers.
                        let a = if center_nonzero {
                            let dot = cx * nx + cy * ny;
                            if dot <= 0 {
                                continue;
                            }
                            dot
                        } else {
                            nx * nx + ny * ny
                        };
                        // Linear spatial falloff in d² (integer, monotone).
                        let ws = (max_d2 + 1 - (dx * dx + dy * dy) as i64).max(1);
                        let wn = ws * a;
                        ax += wn * nx;
                        ay += wn * ny;
                        aw += wn;
                    }
                }
                if aw == 0 {
                    self.vx2[i] = 0;
                    self.vy2[i] = 0;
                } else {
                    self.vx2[i] = div_round(ax, aw).clamp(-255, 255) as i16;
                    self.vy2[i] = div_round(ay, aw).clamp(-255, 255) as i16;
                }
            }
        }
    }

    /// Re-cap the smoothed field to `|v| ≤ E` per pixel (module docs): the
    /// passes may bleed strong magnitudes around, but the stored vector
    /// never claims more edge energy than the local magnitude — and pixels
    /// with no local edge store no orientation at all.
    fn cap_field(&mut self) {
        for i in 0..self.mag.len() {
            let m = i64::from(self.mag[i]);
            if m == 0 {
                self.vx[i] = 0;
                self.vy[i] = 0;
                continue;
            }
            let x = i64::from(self.vx[i]);
            let y = i64::from(self.vy[i]);
            let n2 = x * x + y * y;
            if n2 > m * m {
                let n = i64::try_from((n2 as u64).isqrt()).expect("norm fits i64");
                self.vx[i] = div_round(x * m, n) as i16;
                self.vy[i] = div_round(y * m, n) as i16;
            }
        }
    }
}

/// Canny-style dual-threshold keep mask, 8-connected flood fill from strong
/// seeds — but NO thinning (PLAN §5: thinning breaks under resampling).
/// `keep[i] = 1` iff `mag[i] ≥ t_lo` and connected to some `mag ≥ t_hi`.
pub fn hysteresis_mask(
    mag: &[u8],
    w: usize,
    h: usize,
    t_hi: u8,
    t_lo: u8,
    keep: &mut Vec<u8>,
    stack: &mut Vec<u32>,
) {
    keep.clear();
    keep.resize(mag.len(), 0);
    stack.clear();
    for (i, &m) in mag.iter().enumerate() {
        if m >= t_hi && keep[i] == 0 {
            keep[i] = 1;
            stack.push(i as u32);
            while let Some(p) = stack.pop() {
                let (px, py) = (p as usize % w, p as usize / w);
                for dy in -1isize..=1 {
                    for dx in -1isize..=1 {
                        let (nx, ny) = (px as isize + dx, py as isize + dy);
                        if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                            continue;
                        }
                        let q = ny as usize * w + nx as usize;
                        if keep[q] == 0 && mag[q] >= t_lo {
                            keep[q] = 1;
                            stack.push(q as u32);
                        }
                    }
                }
            }
        }
    }
}

/// Separable box max filter (radius `r`), replicate borders: `src → dst`
/// via `tmp`. Used as the bilateral activity guard and by highlights.rs.
pub fn dilate_box(src: &[u8], w: usize, h: usize, r: usize, tmp: &mut [u8], dst: &mut [u8]) {
    row_extrema::<true>(src, w, h, r, tmp);
    col_extrema::<true>(tmp, w, h, r, dst);
}

/// Separable box min filter (radius `r`), replicate borders.
pub fn erode_box(src: &[u8], w: usize, h: usize, r: usize, tmp: &mut [u8], dst: &mut [u8]) {
    row_extrema::<false>(src, w, h, r, tmp);
    col_extrema::<false>(tmp, w, h, r, dst);
}

fn row_extrema<const MAX: bool>(src: &[u8], w: usize, h: usize, r: usize, dst: &mut [u8]) {
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let out = &mut dst[y * w..(y + 1) * w];
        for (x, o) in out.iter_mut().enumerate() {
            let lo = x.saturating_sub(r);
            let hi = (x + r).min(w - 1);
            let win = &row[lo..=hi];
            *o = if MAX {
                *win.iter().max().expect("nonempty window")
            } else {
                *win.iter().min().expect("nonempty window")
            };
        }
    }
}

fn col_extrema<const MAX: bool>(src: &[u8], w: usize, h: usize, r: usize, dst: &mut [u8]) {
    for y in 0..h {
        let lo = y.saturating_sub(r);
        let hi = (y + r).min(h - 1);
        for x in 0..w {
            let mut v = src[lo * w + x];
            for yy in lo + 1..=hi {
                let s = src[yy * w + x];
                v = if MAX { v.max(s) } else { v.min(s) };
            }
            dst[y * w + x] = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 48;
    const H: usize = 48;

    fn cfg() -> EdgeConfig {
        EdgeConfig { scharr_shift: 4, passes: 2, radius: 2, t_hi: 28, t_lo: 12 }
    }

    fn run(luma: &[u8]) -> EdgeExtractor {
        let mut ex = EdgeExtractor::new(W as u16, H as u16);
        ex.run(luma, &cfg());
        ex
    }

    /// Bars of period 16 along `axis(x, y)`, values 30/220.
    fn bars(f: impl Fn(usize, usize) -> usize) -> Vec<u8> {
        let mut l = vec![0u8; W * H];
        for y in 0..H {
            for x in 0..W {
                l[y * W + x] = if (f(x, y) / 8).is_multiple_of(2) { 30 } else { 220 };
            }
        }
        l
    }

    /// Kept pixels away from the raster border (border Scharr is clamped).
    fn kept_interior(ex: &EdgeExtractor) -> Vec<usize> {
        (0..W * H)
            .filter(|&i| {
                let (x, y) = (i % W, i / W);
                ex.e()[i] > 0 && (4..W - 4).contains(&x) && (4..H - 4).contains(&y)
            })
            .collect()
    }

    #[test]
    fn vertical_bars_peak_on_contours_with_positive_vx() {
        let ex = run(&bars(|x, _| x));
        let kept = kept_interior(&ex);
        assert!(kept.len() > 100, "vertical contours must survive, kept {}", kept.len());
        for &i in &kept {
            let x = i % W;
            // E peaks ON the bar boundaries (multiples of 8), never deep
            // inside a flat bar: magnitude is never spatially smoothed, so
            // only the 2-px two-sided Scharr support responds.
            let d = (x % 8).min(8 - x % 8);
            assert!(d <= 2, "edge pixel {x} is {d} px from any contour");
            // §3.3 sign test: vertical edge → gradient horizontal →
            // cos 2θ = +1: vx strongly positive, vy near zero.
            assert!(
                ex.vx()[i] > 0 && ex.vx()[i] > 2 * ex.vy()[i].abs(),
                "vertical bar bin broken at {i}: vx {} vy {}",
                ex.vx()[i],
                ex.vy()[i]
            );
        }
        // Flat bar interiors carry no edge at all.
        for y in 4..H - 4 {
            assert_eq!(ex.e()[y * W + 4], 0, "bar interior must stay clean at row {y}");
        }
    }

    #[test]
    fn horizontal_bars_have_negative_vx() {
        let ex = run(&bars(|_, y| y));
        let kept = kept_interior(&ex);
        assert!(kept.len() > 100);
        for &i in &kept {
            // Horizontal edge → gradient vertical → cos 2θ = −1.
            assert!(
                ex.vx()[i] < 0 && -ex.vx()[i] > 2 * ex.vy()[i].abs(),
                "horizontal bar bin broken at {i}: vx {} vy {}",
                ex.vx()[i],
                ex.vy()[i]
            );
        }
    }

    #[test]
    fn diagonal_bars_split_by_vy_sign() {
        // x+y bars: contours along "/" (gradient at 45°, sin 2θ = +1).
        let ex = run(&bars(|x, y| x + y));
        let kept = kept_interior(&ex);
        assert!(kept.len() > 100);
        for &i in &kept {
            assert!(
                ex.vy()[i] > 0 && ex.vy()[i] > 2 * ex.vx()[i].abs(),
                "\"/\" diagonal bin broken at {i}: vx {} vy {}",
                ex.vx()[i],
                ex.vy()[i]
            );
        }
        // x−y bars: contours along "\" (gradient at −45°, sin 2θ = −1).
        let ex = run(&bars(|x, y| x + 4 * H - y));
        let kept = kept_interior(&ex);
        assert!(kept.len() > 100);
        for &i in &kept {
            assert!(
                ex.vy()[i] < 0 && -ex.vy()[i] > 2 * ex.vx()[i].abs(),
                "\"\\\" diagonal bin broken at {i}: vx {} vy {}",
                ex.vx()[i],
                ex.vy()[i]
            );
        }
    }

    #[test]
    fn disc_rim_traces_the_contour_with_rotating_orientation() {
        // Bright disc r=16 on dark ground: E must ring the rim only.
        let mut luma = vec![30u8; W * H];
        let (cx, cy, r) = (24i32, 24i32, 16i32);
        for y in 0..H {
            for x in 0..W {
                let (dx, dy) = (x as i32 - cx, y as i32 - cy);
                if dx * dx + dy * dy <= r * r {
                    luma[y * W + x] = 220;
                }
            }
        }
        let ex = run(&luma);
        let mut ring = 0;
        for i in 0..W * H {
            if ex.e()[i] == 0 {
                continue;
            }
            let (x, y) = ((i % W) as f64, (i / W) as f64);
            let dist = ((x - 24.0).powi(2) + (y - 24.0).powi(2)).sqrt();
            assert!(
                (dist - 16.0).abs() <= 4.0,
                "edge energy off the rim at ({x},{y}), dist {dist:.1}"
            );
            ring += 1;
        }
        assert!(ring >= 60, "rim ring too sparse: {ring} pixels");
        // Orientation rotates with the contour: right rim has a horizontal
        // gradient (vx > 0), top rim a vertical one (vx < 0).
        let right = (24 + 16) as usize + 24 * W;
        let top = 24 + (24 - 16) * W;
        assert!(ex.e()[right] > 0 && ex.vx()[right] > 0, "right rim: {}", ex.vx()[right]);
        assert!(ex.e()[top] > 0 && ex.vx()[top] < 0, "top rim: {}", ex.vx()[top]);
    }

    #[test]
    fn smoothing_keeps_orientation_coherent_along_a_line() {
        // A single vertical contour: after two bilateral passes every kept
        // pixel on the contour must agree on the bin (no speckle).
        let mut luma = vec![30u8; W * H];
        for y in 0..H {
            for x in 24..W {
                luma[y * W + x] = 220;
            }
        }
        let ex = run(&luma);
        let kept = kept_interior(&ex);
        assert!(!kept.is_empty());
        for &i in &kept {
            assert!(
                ex.vx()[i] > 0 && ex.vx()[i] > 4 * ex.vy()[i].abs(),
                "incoherent orientation on a straight contour at {i}"
            );
        }
    }

    #[test]
    fn hysteresis_keeps_connected_weak_drops_isolated_weak() {
        let (w, h) = (16usize, 5usize);
        let mut mag = vec![0u8; w * h];
        // A weak chain (t_lo..t_hi) touching one strong seed...
        for x in 2..10 {
            mag[2 * w + x] = 15;
        }
        mag[2 * w + 2] = 40; // seed
        // ...and an isolated weak pixel far from any seed.
        mag[4 * w + 14] = 15;
        let (mut keep, mut stack) = (Vec::new(), Vec::new());
        hysteresis_mask(&mag, w, h, 28, 12, &mut keep, &mut stack);
        for x in 2..10 {
            assert_eq!(keep[2 * w + x], 1, "seed-connected weak pixel {x} must survive");
        }
        assert_eq!(keep[4 * w + 14], 0, "isolated weak pixel must die");
        // Below t_lo never survives, even adjacent to the seed.
        assert_eq!(keep[2 * w + 1], 0);
    }

    #[test]
    fn unthinned_soft_edges_stay_wide() {
        // A 2-px luminance ramp: both flanking columns respond; no thinning
        // may collapse them to one (PLAN §5: unthinned).
        let mut luma = vec![30u8; W * H];
        for y in 0..H {
            luma[y * W + 24] = 125;
            for x in 25..W {
                luma[y * W + x] = 220;
            }
        }
        let ex = run(&luma);
        let row = 24 * W;
        let width = (20..30).filter(|&x| ex.e()[row + x] > 0).count();
        assert!(width >= 2, "soft edge must keep its full unthinned width, got {width}");
    }
}
