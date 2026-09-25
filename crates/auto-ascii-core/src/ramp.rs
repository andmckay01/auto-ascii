//! Base ASCII ramps — palettes 1 and 2 of the 8 shipped palettes, embedded as
//! data (the full set lives in [`crate::palette`]).
//!
//! Ramps map normalized luma (0..=255 after p2/p98 levels) to a glyph. Density
//! selects ramp length *within* a config — it never multiplies the palette set.

/// Palette 1 — `ascii/base/coarse`, used below [`FINE_MIN_COLS`] viewport
/// columns: `" .:-=+*#%@"`.
pub const ASCII_BASE_COARSE: &[char] =
    &[' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

/// Palette 2 — `ascii/base/fine`, used at ≥ [`FINE_MIN_COLS`] viewport columns:
/// `" .,:;i1tfLCG08@"` (15 steps).
pub const ASCII_BASE_FINE: &[char] = &[
    ' ', '.', ',', ':', ';', 'i', '1', 't', 'f', 'L', 'C', 'G', '0', '8', '@',
];

/// Density boundary between coarse and fine base ramps, in viewport columns.
pub const FINE_MIN_COLS: u16 = 70;

/// Select the base ramp for a viewport width (coarse < 70 cols, fine ≥ 70).
/// `viewport_cols` is the video area width, not the terminal width.
#[inline]
pub fn base_ramp_for_cols(viewport_cols: u16) -> &'static [char] {
    if viewport_cols < FINE_MIN_COLS {
        ASCII_BASE_COARSE
    } else {
        ASCII_BASE_FINE
    }
}

/// Map normalized luma `n` (0..=255) to a ramp glyph: `ramp[(n·len) >> 8]`.
/// Pure index mapping — hysteresis on the index is the caller's job.
#[inline]
pub fn ramp_glyph(ramp: &[char], n: u8) -> char {
    debug_assert!(!ramp.is_empty());
    ramp[(n as usize * ramp.len()) >> 8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramps_match_plan() {
        assert_eq!(ASCII_BASE_COARSE.iter().collect::<String>(), " .:-=+*#%@");
        assert_eq!(
            ASCII_BASE_FINE.iter().collect::<String>(),
            " .,:;i1tfLCG08@"
        );
    }

    #[test]
    fn selector_and_mapping() {
        assert_eq!(base_ramp_for_cols(69), ASCII_BASE_COARSE);
        assert_eq!(base_ramp_for_cols(70), ASCII_BASE_FINE);
        assert_eq!(ramp_glyph(ASCII_BASE_COARSE, 0), ' ');
        assert_eq!(ramp_glyph(ASCII_BASE_COARSE, 255), '@');
        assert_eq!(ramp_glyph(ASCII_BASE_FINE, 255), '@');
    }
}
