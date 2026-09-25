//! `auto-ascii-core` — the pure engine: viewport/letterbox math, the separable
//! resampler, the 8 ramp palettes, the three-layer compositor with hysteresis,
//! the glyph codecs that map a cell's features to a glyph ([`codec`]), and the
//! `Cell`/`Grid` primitives.
//!
//! No dependencies beyond `std`; no terminal, no clock, no I/O — everything in
//! this crate is deterministic and golden-testable.
//!
//! Hot-path discipline: `Grid::resize` (called from `Backend::resize`) and
//! `HysteresisState::resize` are the ONLY allocation points in the hot path.

pub mod cell;
pub mod codec;
pub mod compose;
pub mod font_table;
pub mod grid;
pub mod hysteresis;
pub mod orient;
pub mod palette;
pub mod ramp;
pub mod resample;
pub mod viewport;

pub use cell::{Cell, Rgb};
pub use codec::{Codec, GlyphCodec, compose_frame_codec};
pub use font_table::{BUILTIN_FONT_TABLES, FontTable};
pub use compose::{
    CellInputs, ComposeParams, FramePlanes, compose_cell, compose_cell_layer, compose_frame,
    compose_frame_masked, compose_luma, h_flags, layer,
};
pub use grid::Grid;
pub use hysteresis::{CellState, HysteresisState, IDX_HYST_Q8, edge_gate, hysteresis_idx};
pub use palette::{
    ColorDepth, DensityBand, EdgeLut, GlyphClass, GlyphTier, LayerRole, PaletteSet, RampView,
    select_palettes,
};
pub use resample::{Resampler, Tap1D};
pub use viewport::{
    DEFAULT_CELL_ASPECT, MIN_COLS, MIN_ROWS, Viewport, compute_viewport, compute_viewport_for,
};
