//! Base ASCII glyph ramps and viewport-density selection.

pub const ASCII_BASE_COARSE: &[char] =
    &[' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

pub const ASCII_BASE_FINE: &[char] = &[
    ' ', '.', ',', ':', ';', 'i', '1', 't', 'f', 'L', 'C', 'G', '0', '8', '@',
];

pub const FINE_MIN_COLS: u16 = 70;

#[inline]
pub fn base_ramp_for_cols(viewport_cols: u16) -> &'static [char] {
    if viewport_cols < FINE_MIN_COLS {
        ASCII_BASE_COARSE
    } else {
        ASCII_BASE_FINE
    }
}

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
