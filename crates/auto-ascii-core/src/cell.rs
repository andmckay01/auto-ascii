//! `Cell` and `Rgb` — the render-grid POD types.

/// 24-bit truecolor value. 3-byte `#[repr(C)]` POD so `Cell` packs to 12 bytes.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb::new(0, 0, 0);
    pub const WHITE: Rgb = Rgb::new(255, 255, 255);

    #[inline]
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// Grayscale helper: `v` in all three channels.
    #[inline]
    pub const fn gray(v: u8) -> Rgb {
        Rgb { r: v, g: v, b: v }
    }
}

/// Cell attribute bits (`Cell::attrs`). Only [`attrs::NONE`] is defined; the
/// field is part of the frozen 12-byte layout.
pub mod attrs {
    pub const NONE: u8 = 0;
}

/// One terminal cell: 12-byte `#[repr(C)]` POD, memcmp-able.
///
/// `bg` is load-bearing: half-block cells are (fg, bg) vertical pixel pairs.
///
/// `ch` is the glyph as a `char` widened to `u32` (so the struct stays POD and
/// diffable with a plain byte compare once padding is zeroed by constructors).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: u32,
    pub fg: Rgb,
    pub bg: Rgb,
    pub attrs: u8,
}

const _: () = assert!(core::mem::size_of::<Cell>() == 12, "Cell must be 12 B (PLAN §3.1)");
const _: () = assert!(core::mem::align_of::<Cell>() == 4);

impl Cell {
    /// Space on black — the letterbox/pad fill and `Grid` default.
    pub const BLANK: Cell = Cell {
        ch: ' ' as u32,
        fg: Rgb::WHITE,
        bg: Rgb::BLACK,
        attrs: attrs::NONE,
    };

    #[inline]
    pub const fn new(ch: char, fg: Rgb, bg: Rgb) -> Cell {
        Cell { ch: ch as u32, fg, bg, attrs: attrs::NONE }
    }

    /// The glyph as a `char` (lossy: invalid scalar values render as space).
    #[inline]
    pub fn glyph(&self) -> char {
        char::from_u32(self.ch).unwrap_or(' ')
    }
}

impl Default for Cell {
    #[inline]
    fn default() -> Cell {
        Cell::BLANK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_is_12_byte_pod() {
        assert_eq!(core::mem::size_of::<Cell>(), 12);
        assert_eq!(core::mem::size_of::<Rgb>(), 3);
    }
}
