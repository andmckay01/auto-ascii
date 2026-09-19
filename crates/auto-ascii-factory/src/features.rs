//! Per-frame feature-plane orchestration (PLAN §5 stages 3–4, M3): one rgb24
//! frame in → the six quantized ASCI planes out (Y, E, Ex, Ey, H, C — PLAN
//! §4 registry order).
//!
//! ## Stage order (one frame)
//!
//! ```text
//!   rgb24 ─ L*luts ─→ raw luma ─ EMA(α_y) ─→ Y (stored)
//!                                   │
//!                     Scharr → doubled-angle field → 2× orientation-aware
//!                     bilateral → cap → hysteresis  (edges.rs, from Y)
//!                                   │
//!                        E ─ EMA(α_e) ─→ E (stored)
//!                    vx,vy ─ EMA(α_e) ─ quantize ─→ Ex, Ey (stored)
//!                                   │
//!                     top-hat + shadow percentile ─→ H (stored, from Y)
//!   rgb24 ─ 2×2 avg ─→ r,g,b ─ EMA(α_c) ─ pack565 ─→ C (stored)
//! ```
//!
//! Downstream stages read the EMA'd Y (not raw luma): edges must trace the
//! plane the player renders, and H/E inherit the EMA's temporal stability
//! at the source instead of chasing it afterwards. All EMAs reset at shot
//! cuts (PLAN §5 stage 4); pass 1 replicates the same reset schedule for
//! its levels pooling (build.rs), which keeps NORM levels equal to the
//! stored-plane percentiles.
//!
//! ## Quantization (factory⇄player wire contract, PLAN §4)
//!
//! - `Y`, `E`: u8 as computed (E ≈ L\* contrast, see edges.rs).
//! - `Ex/Ey`: `128 + (v >> 1)` — the ±255 doubled-angle components halved
//!   into bias-128 u8. Decode: `v ≈ (byte − 128) · 2`; coherence per §3.3
//!   is `2·|(Ex−128, Ey−128)| / E`.
//! - `H`: bit0 highlight, bit1 deep shadow (highlights.rs).
//! - `C`: RGB565 LE from the EMA'd channel planes.
//!
//! ## Memory strategy (PLAN context: planes are ~130 KB/frame — stream!)
//!
//! Everything here is O(plane), allocated ONCE at construction and reused
//! for every frame: ~1.8 MB of edge scratch (edges.rs), ~0.9 MB of EMA
//! accumulators (7 × Q8 i32 planes), ~1.3 MB of luma/chroma/output
//! staging — ≈ 4 MB total at 480×270, independent of clip length. Frames
//! stream straight into the ASCI writer; no plane is ever accumulated
//! across frames (the EMA state is the only cross-frame memory).

use crate::edges::{EdgeConfig, EdgeExtractor};
use crate::extract::{Extractor, pack_rgb565};
use crate::highlights::{HighlightConfig, HighlightExtractor};
use crate::params::Params;
use crate::temporal::EmaPlane;

pub struct FeatureExtractor {
    extractor: Extractor,
    edges: EdgeExtractor,
    edge_cfg: EdgeConfig,
    highlights: HighlightExtractor,
    hi_cfg: HighlightConfig,

    ema_y: EmaPlane,
    ema_e: EmaPlane,
    ema_vx: EmaPlane,
    ema_vy: EmaPlane,
    ema_r: EmaPlane,
    ema_g: EmaPlane,
    ema_b: EmaPlane,

    luma_raw: Vec<u8>,
    vx_s: Vec<i16>,
    vy_s: Vec<i16>,
    cr: Vec<u8>,
    cg: Vec<u8>,
    cb: Vec<u8>,
    cr_s: Vec<u8>,
    cg_s: Vec<u8>,
    cb_s: Vec<u8>,

    // The six wire planes of the current frame (PLAN §4).
    y: Vec<u8>,
    e: Vec<u8>,
    ex_q: Vec<u8>,
    ey_q: Vec<u8>,
    h: Vec<u8>,
    c: Vec<u8>,
}

impl FeatureExtractor {
    /// `w`/`h` = base plane dims (even, ≥ 2 — the §4 geometry term);
    /// `params` must be validated.
    pub fn new(w: u16, h: u16, params: &Params) -> FeatureExtractor {
        let n = w as usize * h as usize;
        let cn = (w as usize / 2) * (h as usize / 2);
        let ay = params.temporal.ema_alpha_y_milli;
        let ae = params.temporal.ema_alpha_e_milli;
        let ac = params.temporal.ema_alpha_c_milli;
        FeatureExtractor {
            extractor: Extractor::new(w, h),
            edges: EdgeExtractor::new(w, h),
            edge_cfg: EdgeConfig::from_params(&params.edges),
            highlights: HighlightExtractor::new(w, h),
            hi_cfg: HighlightConfig::from_params(&params.highlights),
            ema_y: EmaPlane::new(n, ay),
            ema_e: EmaPlane::new(n, ae),
            ema_vx: EmaPlane::new(n, ae),
            ema_vy: EmaPlane::new(n, ae),
            ema_r: EmaPlane::new(cn, ac),
            ema_g: EmaPlane::new(cn, ac),
            ema_b: EmaPlane::new(cn, ac),
            luma_raw: vec![0; n],
            vx_s: vec![0; n],
            vy_s: vec![0; n],
            cr: vec![0; cn],
            cg: vec![0; cn],
            cb: vec![0; cn],
            cr_s: vec![0; cn],
            cg_s: vec![0; cn],
            cb_s: vec![0; cn],
            y: vec![0; n],
            e: vec![0; n],
            ex_q: vec![0; n],
            ey_q: vec![0; n],
            h: vec![0; n],
            c: vec![0; cn * 2],
        }
    }

    /// Extract all six planes from one rgb24 frame. `cut` resets every EMA
    /// first (PLAN §5 stage 4: never blend across a hard cut).
    pub fn process(&mut self, rgb: &[u8], cut: bool) {
        if cut {
            self.ema_y.reset();
            self.ema_e.reset();
            self.ema_vx.reset();
            self.ema_vy.reset();
            self.ema_r.reset();
            self.ema_g.reset();
            self.ema_b.reset();
        }

        // Y: L* then temporal EMA — the stored plane feeds everything else.
        self.extractor.luma(rgb, &mut self.luma_raw);
        self.ema_y.apply_u8(&self.luma_raw, &mut self.y);

        // E / Ex / Ey from the stored Y.
        self.edges.run(&self.y, &self.edge_cfg);
        self.ema_e.apply_u8(self.edges.e(), &mut self.e);
        self.ema_vx.apply_i16(self.edges.vx(), &mut self.vx_s);
        self.ema_vy.apply_i16(self.edges.vy(), &mut self.vy_s);
        for (o, &v) in self.ex_q.iter_mut().zip(self.vx_s.iter()) {
            *o = (128 + i32::from(v >> 1)).clamp(0, 255) as u8;
        }
        for (o, &v) in self.ey_q.iter_mut().zip(self.vy_s.iter()) {
            *o = (128 + i32::from(v >> 1)).clamp(0, 255) as u8;
        }

        // H from the stored Y (flags are not EMA'd — their input is).
        self.highlights.run(&self.y, &self.hi_cfg, &mut self.h);

        // C: per-channel EMA between the 2×2 average and the 565 packing.
        self.extractor.chroma_channels(rgb, &mut self.cr, &mut self.cg, &mut self.cb);
        self.ema_r.apply_u8(&self.cr, &mut self.cr_s);
        self.ema_g.apply_u8(&self.cg, &mut self.cg_s);
        self.ema_b.apply_u8(&self.cb, &mut self.cb_s);
        pack_rgb565(&self.cr_s, &self.cg_s, &self.cb_s, &mut self.c);
    }

    pub fn y(&self) -> &[u8] {
        &self.y
    }

    pub fn e(&self) -> &[u8] {
        &self.e
    }

    pub fn ex(&self) -> &[u8] {
        &self.ex_q
    }

    pub fn ey(&self) -> &[u8] {
        &self.ey_q
    }

    pub fn h(&self) -> &[u8] {
        &self.h
    }

    pub fn c(&self) -> &[u8] {
        &self.c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlights::h_flags;

    const W: u16 = 64;
    const H: u16 = 64;

    /// Solid sRGB gray frame.
    fn flat(v: u8) -> Vec<u8> {
        vec![v; W as usize * H as usize * 3]
    }

    /// Bright disc (sRGB 230 gray) on dark ground, center (`cx`, 32), r=14.
    fn disc(cx: i32) -> Vec<u8> {
        let mut rgb = vec![40u8; W as usize * H as usize * 3];
        for y in 0..i32::from(H) {
            for x in 0..i32::from(W) {
                let (dx, dy) = (x - cx, y - 32);
                if dx * dx + dy * dy <= 14 * 14 {
                    let o = (y as usize * W as usize + x as usize) * 3;
                    rgb[o..o + 3].copy_from_slice(&[230, 230, 230]);
                }
            }
        }
        rgb
    }

    fn fx() -> FeatureExtractor {
        FeatureExtractor::new(W, H, &Params::default())
    }

    #[test]
    fn cut_resets_all_temporal_state() {
        // A cut must make frame B come out exactly as if the extractor had
        // never seen frame A — for every plane.
        let (a, b) = (disc(20), disc(40));

        let mut fresh = fx();
        fresh.process(&b, false);

        let mut cut = fx();
        cut.process(&a, false);
        cut.process(&b, true);
        assert_eq!(cut.y(), fresh.y(), "Y must not blend across a cut");
        assert_eq!(cut.e(), fresh.e(), "E must not blend across a cut");
        assert_eq!(cut.ex(), fresh.ex(), "Ex must not blend across a cut");
        assert_eq!(cut.ey(), fresh.ey(), "Ey must not blend across a cut");
        assert_eq!(cut.h(), fresh.h(), "H must not differ across a cut");
        assert_eq!(cut.c(), fresh.c(), "C must not blend across a cut");

        // ...and WITHOUT the cut flag the same pair genuinely blends
        // (otherwise the reset test proves nothing).
        let mut blend = fx();
        blend.process(&a, false);
        blend.process(&b, false);
        assert_ne!(blend.y(), fresh.y(), "no-cut Y must carry EMA history");
        assert_ne!(blend.e(), fresh.e(), "no-cut E must carry EMA history");
    }

    #[test]
    fn moving_disc_leaves_a_decaying_edge_trail_ema() {
        // Disc jumps 20 → 40: the new rim is strong immediately; the old
        // rim fades over the following frames (Y-ghost + E-EMA compound —
        // the player's dual edge threshold rides this decay); a cut wipes
        // it instantly.
        let mut f = fx();
        f.process(&disc(20), false);
        let e_before = f.e().to_vec();
        f.process(&disc(40), false);

        let w = W as usize;
        let old_rim = 32 * w + (20 - 14); // left rim of the old disc
        let new_rim = 32 * w + (40 + 14); // right rim of the new disc
        assert!(e_before[old_rim] > 0, "old rim must exist on frame 1");
        assert!(f.e()[new_rim] > 0, "new rim must be present immediately");
        let ghost1 = f.e()[old_rim];
        assert!(
            ghost1 > 0 && ghost1 < e_before[old_rim],
            "old rim must decay, not vanish or persist: {ghost1} vs {}",
            e_before[old_rim]
        );
        // Trail keeps decaying monotonically while the disc holds still.
        f.process(&disc(40), false);
        let ghost2 = f.e()[old_rim];
        assert!(ghost2 < ghost1, "trail must keep decaying: {ghost2} vs {ghost1}");

        // Flat interior of the new disc carries no edge energy.
        assert_eq!(f.e()[32 * w + 40], 0, "disc interior must stay clean");

        // Same jump ACROSS A CUT: zero trail, full new rim at once.
        let mut f = fx();
        f.process(&disc(20), false);
        f.process(&disc(40), true);
        assert_eq!(f.e()[old_rim], 0, "a cut must leave no edge trail");
        assert!(f.e()[new_rim] > 0);
    }

    #[test]
    fn highlight_and_shadow_flags_fire_on_the_right_pixels() {
        // Mid-gray frame with a 3×3 glint and a dark band.
        let mut rgb = flat(120);
        let w = W as usize;
        for y in 30..33 {
            for x in 30..33 {
                let o = (y * w + x) * 3;
                rgb[o..o + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        for y in 0..8 {
            for x in 0..w {
                let o = (y * w + x) * 3;
                rgb[o..o + 3].copy_from_slice(&[8, 8, 8]);
            }
        }
        let mut f = fx();
        f.process(&rgb, false);
        assert_ne!(f.h()[31 * w + 31] & h_flags::HIGHLIGHT, 0, "glint must flag bit0");
        assert_eq!(f.h()[10 * w + 10] & h_flags::HIGHLIGHT, 0, "flat gray is no highlight");
        assert_ne!(f.h()[4 * w + 4] & h_flags::DEEP_SHADOW, 0, "dark band must flag bit1");
        assert_eq!(f.h()[40 * w + 40] & h_flags::DEEP_SHADOW, 0, "midtone is no shadow");
    }

    #[test]
    fn ex_ey_quantization_is_bias_128_halved() {
        // A vertical contour: field vx ≈ +m (§3.3) → stored Ex ≈ 128 + m/2,
        // Ey ≈ 128; empty regions store exactly (128, 128).
        let mut rgb = flat(40);
        let w = W as usize;
        for y in 0..H as usize {
            for x in 32..w {
                let o = (y * w + x) * 3;
                rgb[o..o + 3].copy_from_slice(&[230, 230, 230]);
            }
        }
        let mut f = fx();
        f.process(&rgb, false);
        let i = 32 * w + 32; // on the contour
        assert!(f.e()[i] > 0);
        let ex = i32::from(f.ex()[i]);
        let ey = i32::from(f.ey()[i]);
        assert!(ex - 128 > 0, "vertical contour must store Ex > 128, got {ex}");
        assert!(
            (ex - 128).abs_diff(i32::from(f.e()[i]) / 2) <= 1,
            "|Ex − 128| must be E/2 on a clean contour: ex {ex} e {}",
            f.e()[i]
        );
        assert!((ey - 128).abs() <= 8, "vertical contour keeps Ey near bias: {ey}");
        assert_eq!((f.ex()[0], f.ey()[0]), (128, 128), "no-edge pixels store the bias");
    }
}
