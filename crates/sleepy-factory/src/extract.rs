//! rgb24 → feature planes (PLAN §5 stage 3, M1 subset): full-res L\* luma
//! (plane Y) and half-res RGB565 chroma (plane C) from ONE decoded stream.
//!
//! **C plane wire format (factory⇄player contract, PLAN §4):** one
//! little-endian u16 per pixel at (base_w/2) × (base_h/2), packed
//! `bits 15..11 = R5 | 10..5 = G6 | 4..0 = B5`, produced by a 2×2
//! area-average downsample (per-channel sum + 2 >> 2, round-half-up) of the
//! rgb24 frame. Half res is invisible at cell granularity (PLAN §4).

use crate::lut::LumaLut;

/// Stateless per-frame plane extractor (tables built once per build).
pub struct Extractor {
    lut: LumaLut,
    w: usize,
    h: usize,
}

impl Extractor {
    pub fn new(w: u16, h: u16) -> Extractor {
        Extractor { lut: LumaLut::new(), w: w as usize, h: h as usize }
    }

    /// Fill the Y plane (`w × h` bytes): per-pixel sRGB→linear→L\*
    /// ([`LumaLut`]). No level stretch — levels live in NORM at M1.
    pub fn luma(&self, rgb: &[u8], out: &mut [u8]) {
        debug_assert_eq!(rgb.len(), self.w * self.h * 3);
        debug_assert_eq!(out.len(), self.w * self.h);
        for (dst, px) in out.iter_mut().zip(rgb.chunks_exact(3)) {
            *dst = self.lut.l_of(px[0], px[1], px[2]);
        }
    }

    /// Fill the C plane (`(w/2) × (h/2) × 2` bytes): 2×2 area average per
    /// channel, RGB565-packed, little-endian (module docs). `build` rejects
    /// odd `--res`, so every source pixel lands in exactly one 2×2 block.
    pub fn chroma(&self, rgb: &[u8], out: &mut [u8]) {
        let (cw, ch) = (self.w / 2, self.h / 2);
        debug_assert_eq!(rgb.len(), self.w * self.h * 3);
        debug_assert_eq!(out.len(), cw * ch * 2);
        let stride = self.w * 3;
        for cy in 0..ch {
            let row0 = &rgb[2 * cy * stride..][..stride];
            let row1 = &rgb[(2 * cy + 1) * stride..][..stride];
            let out_row = &mut out[cy * cw * 2..][..cw * 2];
            for cx in 0..cw {
                let o = 2 * cx * 3;
                let avg = |c: usize| -> u16 {
                    let sum = u16::from(row0[o + c])
                        + u16::from(row0[o + 3 + c])
                        + u16::from(row1[o + c])
                        + u16::from(row1[o + 3 + c]);
                    (sum + 2) >> 2
                };
                let (r, g, b) = (avg(0), avg(1), avg(2));
                let packed = ((r & 0xF8) << 8) | ((g & 0xFC) << 3) | (b >> 3);
                out_row[cx * 2..cx * 2 + 2].copy_from_slice(&packed.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(w: usize, h: usize, rgb: [u8; 3]) -> Vec<u8> {
        rgb.iter().copied().cycle().take(w * h * 3).collect()
    }

    #[test]
    fn luma_matches_lut_per_pixel() {
        let ex = Extractor::new(4, 2);
        let mut frame = solid_frame(4, 2, [200, 100, 50]);
        frame[0..3].copy_from_slice(&[0, 0, 0]);
        frame[3..6].copy_from_slice(&[255, 255, 255]);
        let mut out = vec![0u8; 8];
        ex.luma(&frame, &mut out);
        let lut = LumaLut::new();
        assert_eq!(out[0], 0);
        assert_eq!(out[1], 255);
        assert!(out[2..].iter().all(|&v| v == lut.l_of(200, 100, 50)));
    }

    #[test]
    fn chroma_packs_solid_color_rgb565_le() {
        let ex = Extractor::new(4, 4);
        let frame = solid_frame(4, 4, [255, 128, 8]);
        let mut out = vec![0u8; 2 * 2 * 2];
        ex.chroma(&frame, &mut out);
        // r=255→31<<11, g=128→32<<5, b=8→1: 0xF841 little-endian = [0x41, 0xF8].
        let expected = ((255u16 & 0xF8) << 8) | ((128u16 & 0xFC) << 3) | (8u16 >> 3);
        assert_eq!(expected, 0xFC01); // r5=31 g6=32 b5=1
        for px in out.chunks_exact(2) {
            assert_eq!(u16::from_le_bytes([px[0], px[1]]), expected);
        }
    }

    #[test]
    fn chroma_area_average_rounds_half_up() {
        // One 2×2 block: reds 0,1,2,3 → sum 6 → (6+2)>>2 = 2; g/b constant.
        let ex = Extractor::new(2, 2);
        let mut frame = vec![0u8; 12];
        for (i, px) in frame.chunks_exact_mut(3).enumerate() {
            px[0] = i as u8;
            px[1] = 100;
            px[2] = 200;
        }
        let mut out = vec![0u8; 2];
        ex.chroma(&frame, &mut out);
        let packed = u16::from_le_bytes([out[0], out[1]]);
        assert_eq!(packed >> 11, u16::from(2u8 >> 3)); // r5 from avg red 2
        assert_eq!((packed >> 5) & 0x3F, u16::from(100u8 >> 2)); // g6
        assert_eq!(packed & 0x1F, u16::from(200u8 >> 3)); // b5
    }
}
