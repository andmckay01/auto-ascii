//! Glyph style interface, registry and frame dispatch.

use crate::cell::Cell;
use crate::compose::{CellInputs, ComposeParams, FramePlanes, frame_impl};
use crate::grid::Grid;
use crate::hysteresis::{CellState, HysteresisState};
use crate::palette::PaletteSet;
use crate::viewport::Viewport;

pub trait GlyphStyle {
    const NAME: &'static str;

    const PAD: Cell = Cell::BLANK;

    fn cell(
        inp: &CellInputs,
        lut: &[u8; 256],
        set: &PaletteSet,
        params: &ComposeParams,
        state: &mut CellState,
    ) -> (Cell, u8);
}

macro_rules! registry {
    ($(#[$doc:meta])* $first:ident => $fmod:ident::$fty:ident,
     $($(#[$vdoc:meta])* $variant:ident => $module:ident::$ty:ident),* $(,)?) => {
        pub mod $fmod;
        $(pub mod $module;)*

        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        pub enum Style {
            $(#[$doc])*
            $first,
            $($(#[$vdoc])* $variant,)*
        }

        impl Style {
            pub const ALL: [Style; 1 $(+ registry!(@one $variant))*] =
                [Style::$first, $(Style::$variant),*];

            pub fn name(self) -> &'static str {
                match self {
                    Style::$first => <$fmod::$fty as GlyphStyle>::NAME,
                    $(Style::$variant => <$module::$ty as GlyphStyle>::NAME,)*
                }
            }

            pub fn pad(self) -> Cell {
                match self {
                    Style::$first => <$fmod::$fty as GlyphStyle>::PAD,
                    $(Style::$variant => <$module::$ty as GlyphStyle>::PAD,)*
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub fn compose_frame_style(
            style: Style,
            planes: &FramePlanes<'_>,
            vp: &Viewport,
            lut: &[u8; 256],
            set: &PaletteSet,
            params: &ComposeParams,
            state: &mut HysteresisState,
            out: &mut Grid<Cell>,
            mask: Option<&mut Grid<u8>>,
        ) {
            if let Some(m) = &mask {
                assert_eq!((m.cols(), m.rows()), (out.cols(), out.rows()), "layer mask != grid dims");
            }
            match style {
                Style::$first => {
                    frame_impl::<$fmod::$fty>(planes, vp, lut, set, params, state, out, mask)
                }
                $(Style::$variant => {
                    frame_impl::<$module::$ty>(planes, vp, lut, set, params, state, out, mask)
                })*
            }
        }
    };
    (@one $x:ident) => { 1 };
}

registry! {
    Pixels => pixels::Pixels,
    Letters => letters::Letters,
    #[default]
    Ascii => ascii::Ascii,
}

impl Style {
    pub fn from_name(name: &str) -> Option<Style> {
        Style::ALL.into_iter().find(|c| c.name() == name)
    }

    pub fn next(self) -> Style {
        let i = Style::ALL.iter().position(|&c| c == self).unwrap_or(0);
        Style::ALL[(i + 1) % Style::ALL.len()]
    }

    pub fn names(sep: &str) -> String {
        Style::ALL.map(Style::name).join(sep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_complete_and_the_default_is_ascii() {
        assert_eq!(Style::default(), Style::Ascii);
        assert!(Style::ALL.contains(&Style::default()), "the default is in the cycle");
        for (i, a) in Style::ALL.iter().enumerate() {
            assert!(Style::ALL[i + 1..].iter().all(|b| b != a), "{a:?} listed twice");
        }
    }

    #[test]
    fn names_round_trip_and_are_unique() {
        for c in Style::ALL {
            assert_eq!(Style::from_name(c.name()), Some(c));
            assert!(c.name().bytes().all(|b| b.is_ascii_lowercase()), "{}", c.name());
        }
        assert_eq!(Style::from_name("Pixels"), None, "names are exact");
        assert_eq!(Style::from_name(""), None);
        assert_eq!(Style::names(", "), "pixels, letters, ascii");
        assert_eq!(Style::names("|"), "pixels|letters|ascii");
    }

    #[test]
    fn only_ascii_pads_on_the_terminal_background() {
        assert_eq!(Style::Pixels.pad(), Cell::BLANK);
        assert_eq!(Style::Letters.pad(), Cell::BLANK);
        let pad = Style::Ascii.pad();
        assert_eq!((pad.glyph(), pad.bg, pad.attrs), (' ', crate::Rgb::BLACK, crate::cell::attrs::DEFAULT_BG));
    }

    #[test]
    fn next_cycles_through_all_and_wraps() {
        let mut c = Style::ALL[0];
        let mut seen = Vec::new();
        for _ in 0..Style::ALL.len() {
            seen.push(c);
            c = c.next();
        }
        assert_eq!(seen, Style::ALL.to_vec(), "one press per style, in registry order");
        assert_eq!(c, Style::ALL[0], "and back to the start");
        assert_eq!(Style::ALL, [Style::Pixels, Style::Letters, Style::Ascii]);
        assert_eq!(Style::Pixels.next(), Style::Letters);
        assert_eq!(Style::Letters.next(), Style::Ascii);
        assert_eq!(Style::Ascii.next(), Style::Pixels);
    }
}
