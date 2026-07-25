//! Color quantization for the non-truecolor tiers (PLAN §3.1 "quantize
//! before diff"). Pure math, no I/O: RGB → xterm 6×6×6 cube + grayscale ramp
//! (256 tier) or nearest of the standard 16 (16 tier), plus the inverse
//! canonical-RGB tables. The painter quantizes cells to *canonical* tier RGB
//! before diffing, so cells that quantize equal are byte-equal PODs and
//! produce zero damage; emission re-derives the palette index from the
//! canonical RGB (exact-match roundtrip, asserted in tests).

use slpy_core::Rgb;

/// xterm 6×6×6 color-cube component levels (indices 16–231).
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Standard 16 ANSI colors, xterm defaults (indices 0–15). SGR 30–37/90–97.
const ANSI16: [Rgb; 16] = [
    Rgb::new(0, 0, 0),       // 0 black
    Rgb::new(205, 0, 0),     // 1 red
    Rgb::new(0, 205, 0),     // 2 green
    Rgb::new(205, 205, 0),   // 3 yellow
    Rgb::new(0, 0, 238),     // 4 blue
    Rgb::new(205, 0, 205),   // 5 magenta
    Rgb::new(0, 205, 205),   // 6 cyan
    Rgb::new(229, 229, 229), // 7 white
    Rgb::new(127, 127, 127), // 8 bright black
    Rgb::new(255, 0, 0),     // 9 bright red
    Rgb::new(0, 255, 0),     // 10 bright green
    Rgb::new(255, 255, 0),   // 11 bright yellow
    Rgb::new(92, 92, 255),   // 12 bright blue
    Rgb::new(255, 0, 255),   // 13 bright magenta
    Rgb::new(0, 255, 255),   // 14 bright cyan
    Rgb::new(255, 255, 255), // 15 bright white
];

#[inline]
fn dist2(a: Rgb, b: Rgb) -> u32 {
    let dr = i32::from(a.r) - i32::from(b.r);
    let dg = i32::from(a.g) - i32::from(b.g);
    let db = i32::from(a.b) - i32::from(b.b);
    (dr * dr + dg * dg + db * db) as u32
}

/// Nearest 6×6×6 cube level index (0–5) for one component — exact nearest
/// against `CUBE_LEVELS` (midpoints 47.5 / 115 / 155 / 195 / 235).
#[inline]
fn cube_component(c: u8) -> u8 {
    if c < 48 {
        0
    } else if c < 115 {
        1
    } else {
        ((u16::from(c) - 35) / 40) as u8
    }
}

/// Map RGB to the nearest xterm-256 index, considering both the 6×6×6 cube
/// (16–231) and the 24-step grayscale ramp (232–255, values 8–238). Never
/// returns 0–15 (those vary per user theme; the cube+ramp are stable).
pub fn rgb_to_256(c: Rgb) -> u8 {
    // Cube candidate.
    let (ri, gi, bi) = (cube_component(c.r), cube_component(c.g), cube_component(c.b));
    let cube_idx = 16 + 36 * ri + 6 * gi + bi;
    let cube_rgb = Rgb::new(
        CUBE_LEVELS[ri as usize],
        CUBE_LEVELS[gi as usize],
        CUBE_LEVELS[bi as usize],
    );
    // Gray-ramp candidate: values 8 + 10i, i = 0..24 (nearest by rounding).
    let avg = (u16::from(c.r) + u16::from(c.g) + u16::from(c.b)) / 3;
    let gi = if avg < 8 { 0 } else { ((avg - 8 + 5) / 10).min(23) } as u8;
    let gray_idx = 232 + gi;
    let gv = 8 + 10 * gi;
    let gray_rgb = Rgb::gray(gv);

    if dist2(c, gray_rgb) < dist2(c, cube_rgb) { gray_idx } else { cube_idx }
}

/// Map RGB to the nearest of the standard 16 ANSI colors (squared-distance,
/// lowest index wins ties).
pub fn rgb_to_16(c: Rgb) -> u8 {
    let mut best = 0u8;
    let mut best_d = u32::MAX;
    for (i, &pal) in ANSI16.iter().enumerate() {
        let d = dist2(c, pal);
        if d < best_d {
            best_d = d;
            best = i as u8;
        }
    }
    best
}

/// Canonical RGB for an xterm-256 index (0–15 use the standard-16 table).
pub fn ansi256_to_rgb(n: u8) -> Rgb {
    match n {
        0..=15 => ANSI16[n as usize],
        16..=231 => {
            let x = n - 16;
            Rgb::new(
                CUBE_LEVELS[(x / 36) as usize],
                CUBE_LEVELS[((x / 6) % 6) as usize],
                CUBE_LEVELS[(x % 6) as usize],
            )
        }
        _ => Rgb::gray(8 + 10 * (n - 232)),
    }
}

/// Canonical RGB for a standard-16 index (panics if `n > 15`).
pub fn ansi16_to_rgb(n: u8) -> Rgb {
    ANSI16[n as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quantizer goldens: known RGB → known 256 indices (M1 acceptance 5).
    #[test]
    fn golden_rgb_to_256() {
        for (rgb, idx) in [
            (Rgb::new(0, 0, 0), 16u8),        // cube black
            (Rgb::new(255, 255, 255), 231),   // cube white
            (Rgb::new(255, 0, 0), 196),       // pure red → cube corner
            (Rgb::new(0, 255, 0), 46),        // pure green
            (Rgb::new(0, 0, 255), 21),        // pure blue
            (Rgb::new(95, 135, 175), 67),     // exact cube point 1,2,3
            (Rgb::new(8, 8, 8), 232),         // exact gray-ramp bottom
            (Rgb::new(238, 238, 238), 255),   // exact gray-ramp top
            (Rgb::new(128, 128, 128), 244),   // mid gray → ramp value 128
            (Rgb::new(100, 100, 100), 241),   // near ramp value 98
        ] {
            assert_eq!(rgb_to_256(rgb), idx, "rgb_to_256({rgb:?})");
        }
    }

    /// Quantizer goldens: known RGB → known 16 indices.
    #[test]
    fn golden_rgb_to_16() {
        for (rgb, idx) in [
            (Rgb::new(0, 0, 0), 0u8),
            (Rgb::new(255, 255, 255), 15),
            (Rgb::new(205, 0, 0), 1),
            (Rgb::new(255, 0, 0), 9),
            (Rgb::new(0, 255, 255), 14),
            (Rgb::new(127, 127, 127), 8),
            (Rgb::new(230, 230, 230), 7),
            (Rgb::new(255, 255, 0), 11),
        ] {
            assert_eq!(rgb_to_16(rgb), idx, "rgb_to_16({rgb:?})");
        }
    }

    /// The painter stores canonical RGB and re-derives the index at emission:
    /// quantizing a canonical color must return its own index, exactly.
    #[test]
    fn canonical_roundtrip_256() {
        for n in 16..=255u8 {
            assert_eq!(rgb_to_256(ansi256_to_rgb(n)), n, "index {n}");
        }
    }

    #[test]
    fn canonical_roundtrip_16() {
        for n in 0..16u8 {
            assert_eq!(rgb_to_16(ansi16_to_rgb(n)), n, "index {n}");
        }
    }

    #[test]
    fn cube_component_midpoints() {
        assert_eq!(cube_component(47), 0);
        assert_eq!(cube_component(48), 1);
        assert_eq!(cube_component(114), 1);
        assert_eq!(cube_component(115), 2);
        assert_eq!(cube_component(255), 5);
    }
}
