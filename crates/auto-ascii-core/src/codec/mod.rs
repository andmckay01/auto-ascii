//! Glyph codecs — how a cell's features become a glyph and its colors.
//!
//! The asset stores features (luma taps, edge magnitude and orientation,
//! highlight/shadow flags, chroma), never glyphs (PLAN §4). A **codec** is one
//! complete answer to "which glyph, in which colors, for these features":
//!
//! | codec | module | looks like |
//! |---|---|---|
//! | `pixels` | [`pixels`] | the §3.4/§3.5 compositor: tier ramps, half-blocks and quadrants on Unicode tiers — a low-res picture |
//! | `letters` | [`letters`] | printable characters ordered by ink, directional ASCII strokes on edges, `▓`/`█` only for the densest fill |
//!
//! Every codec reads the same [`CellInputs`] through the same NORM LUT and
//! the same [`PaletteSet`] (the §3.4 selection still tells a codec which
//! repertoire the terminal can draw), writes the same [`CellState`] temporal
//! memory, and reports the same [`layer`](crate::layer) ids — so the
//! pipeline, the hysteresis resets, the layer mask and every overlay work
//! unchanged whichever one is active.
//!
//! # Adding a codec
//!
//! One module implementing [`GlyphCodec`], then register it HERE and nowhere
//! else: a [`Codec`] variant, its place in [`Codec::ALL`] (the `/` cycle
//! order), and one arm in each of [`Codec::name`] and [`compose_frame_codec`]
//! — the compiler rejects a variant missing either arm, and
//! `registry_lists_every_variant` below rejects one missing from `ALL`.
//!
//! # Cost
//!
//! Dispatch is once per FRAME, not per cell: [`compose_frame_codec`] picks the
//! codec with one `match` and runs a per-cell loop monomorphized for it, so
//! the pixels path is the exact pre-codec loop (the perf gates and every
//! golden pin that).

pub mod letters;
pub mod pixels;

use crate::cell::Cell;
use crate::compose::{CellInputs, ComposeParams, FramePlanes, frame_impl};
use crate::grid::Grid;
use crate::hysteresis::{CellState, HysteresisState};
use crate::palette::PaletteSet;
use crate::viewport::Viewport;

/// One glyph codec: a pure per-cell mapping from features to a [`Cell`] plus
/// the winning [`layer`](crate::layer) id. See the module docs.
pub trait GlyphCodec {
    /// Registry name — what the `/` key shows, what `--codec` takes and what
    /// the per-video settings file stores. Lowercase ASCII, stable forever
    /// (saved files carry it).
    const NAME: &'static str;

    /// Compose one viewport cell. `lut` is the per-shot NORM LUT (shadow
    /// lift folded in), `set` the §3.4 palette selection for this tier and
    /// density, `state` this cell's temporal memory. Must not allocate.
    fn cell(
        inp: &CellInputs,
        lut: &[u8; 256],
        set: &PaletteSet,
        params: &ComposeParams,
        state: &mut CellState,
    ) -> (Cell, u8);
}

/// The codec registry: every glyph codec the player can switch to live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Codec {
    /// The §3.4/§3.5 compositor ([`pixels::Pixels`]) — the default, and what
    /// every committed golden renders.
    #[default]
    Pixels,
    /// Printable characters for texture and edges ([`letters::Letters`]).
    Letters,
}

impl Codec {
    /// Every registered codec in `/` cycle order (the default first).
    pub const ALL: [Codec; 2] = [Codec::Pixels, Codec::Letters];

    /// Registry name (see [`GlyphCodec::NAME`]).
    pub fn name(self) -> &'static str {
        match self {
            Codec::Pixels => pixels::Pixels::NAME,
            Codec::Letters => letters::Letters::NAME,
        }
    }

    /// Look a codec up by its registry name (exact, lowercase).
    pub fn from_name(name: &str) -> Option<Codec> {
        Codec::ALL.into_iter().find(|c| c.name() == name)
    }

    /// The next codec in [`Codec::ALL`], wrapping — one `/` press.
    pub fn next(self) -> Codec {
        let i = Codec::ALL.iter().position(|&c| c == self).unwrap_or(0);
        Codec::ALL[(i + 1) % Codec::ALL.len()]
    }

    /// Every registry name, comma-separated — for error messages and help.
    pub fn names() -> String {
        Codec::ALL.map(Codec::name).join(", ")
    }
}

/// [`compose_frame`](crate::compose_frame) through `codec`, optionally
/// recording the winning layer per cell into `mask` (as
/// [`compose_frame_masked`](crate::compose_frame_masked)). `Codec::Pixels`
/// is byte-identical to the plain entry points. Never allocates.
///
/// # Panics
/// As [`compose_frame_masked`](crate::compose_frame_masked).
#[allow(clippy::too_many_arguments)] // compose_frame_masked + the codec it dispatches on
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
        Codec::Pixels => frame_impl::<pixels::Pixels>(planes, vp, lut, set, params, state, out, mask),
        Codec::Letters => {
            frame_impl::<letters::Letters>(planes, vp, lut, set, params, state, out, mask)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` is the one list the compiler cannot check: a variant missing
    /// from it would be unreachable by `/` and unparseable from a saved
    /// file. The match forces this test to be revisited with every variant.
    #[test]
    fn registry_lists_every_variant() {
        for c in Codec::ALL {
            match c {
                Codec::Pixels | Codec::Letters => {}
            }
        }
        assert_eq!(Codec::ALL.len(), 2, "a new variant must join Codec::ALL");
        assert_eq!(Codec::ALL[0], Codec::default(), "the default leads the cycle");
    }

    #[test]
    fn names_round_trip_and_are_unique() {
        for c in Codec::ALL {
            assert_eq!(Codec::from_name(c.name()), Some(c));
            assert!(c.name().bytes().all(|b| b.is_ascii_lowercase()), "{}", c.name());
        }
        assert_eq!(Codec::from_name("Pixels"), None, "names are exact");
        assert_eq!(Codec::from_name(""), None);
        assert_eq!(Codec::names(), "pixels, letters");
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
    }
}
