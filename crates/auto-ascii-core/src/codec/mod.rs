//! Glyph codec interface, registry and frame dispatch.

use crate::cell::Cell;
use crate::compose::{CellInputs, ComposeParams, FramePlanes, frame_impl};
use crate::grid::Grid;
use crate::hysteresis::{CellState, HysteresisState};
use crate::palette::PaletteSet;
use crate::viewport::Viewport;

pub trait GlyphCodec {
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
        pub enum Codec {
            $(#[$doc])*
            #[default]
            $first,
            $($(#[$vdoc])* $variant,)*
        }

        impl Codec {
            pub const ALL: [Codec; 1 $(+ registry!(@one $variant))*] =
                [Codec::$first, $(Codec::$variant),*];

            pub fn name(self) -> &'static str {
                match self {
                    Codec::$first => <$fmod::$fty as GlyphCodec>::NAME,
                    $(Codec::$variant => <$module::$ty as GlyphCodec>::NAME,)*
                }
            }

            pub fn pad(self) -> Cell {
                match self {
                    Codec::$first => <$fmod::$fty as GlyphCodec>::PAD,
                    $(Codec::$variant => <$module::$ty as GlyphCodec>::PAD,)*
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub fn compose_frame_codec(
            codec: Codec,
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
            match codec {
                Codec::$first => {
                    frame_impl::<$fmod::$fty>(planes, vp, lut, set, params, state, out, mask)
                }
                $(Codec::$variant => {
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
    Ascii => ascii::Ascii,
}

impl Codec {
    pub fn from_name(name: &str) -> Option<Codec> {
        Codec::ALL.into_iter().find(|c| c.name() == name)
    }

    pub fn next(self) -> Codec {
        let i = Codec::ALL.iter().position(|&c| c == self).unwrap_or(0);
        Codec::ALL[(i + 1) % Codec::ALL.len()]
    }

    pub fn names(sep: &str) -> String {
        Codec::ALL.map(Codec::name).join(sep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_complete_and_default_first() {
        assert_eq!(Codec::ALL[0], Codec::default(), "the default leads the cycle");
        for (i, a) in Codec::ALL.iter().enumerate() {
            assert!(Codec::ALL[i + 1..].iter().all(|b| b != a), "{a:?} listed twice");
        }
    }

    #[test]
    fn names_round_trip_and_are_unique() {
        for c in Codec::ALL {
            assert_eq!(Codec::from_name(c.name()), Some(c));
            assert!(c.name().bytes().all(|b| b.is_ascii_lowercase()), "{}", c.name());
        }
        assert_eq!(Codec::from_name("Pixels"), None, "names are exact");
        assert_eq!(Codec::from_name(""), None);
        assert_eq!(Codec::names(", "), "pixels, letters, ascii");
        assert_eq!(Codec::names("|"), "pixels|letters|ascii");
    }

    #[test]
    fn only_ascii_pads_on_the_terminal_background() {
        assert_eq!(Codec::Pixels.pad(), Cell::BLANK);
        assert_eq!(Codec::Letters.pad(), Cell::BLANK);
        let pad = Codec::Ascii.pad();
        assert_eq!((pad.glyph(), pad.bg, pad.attrs), (' ', crate::Rgb::BLACK, crate::cell::attrs::DEFAULT_BG));
    }

    #[test]
    fn next_cycles_through_all_and_wraps() {
        let mut c = Codec::default();
        let mut seen = Vec::new();
        for _ in 0..Codec::ALL.len() {
            seen.push(c);
            c = c.next();
        }
        assert_eq!(seen, Codec::ALL.to_vec(), "one press per codec, in registry order");
        assert_eq!(c, Codec::default(), "and back to the start");
        assert_eq!(Codec::ALL, [Codec::Pixels, Codec::Letters, Codec::Ascii]);
        assert_eq!(Codec::Pixels.next(), Codec::Letters);
        assert_eq!(Codec::Letters.next(), Codec::Ascii);
        assert_eq!(Codec::Ascii.next(), Codec::Pixels);
    }
}
