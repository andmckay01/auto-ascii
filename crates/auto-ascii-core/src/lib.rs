//! Pure rendering engine: cell grids, viewport fitting, resampling, palettes and glyph styles.

pub mod cell;
pub mod compose;
pub mod font_table;
pub mod grid;
pub mod hysteresis;
pub mod orient;
pub mod palette;
pub mod quant;
pub mod ramp;
pub mod resample;
pub mod style;
pub mod viewport;

pub use cell::{Cell, Rgb};
pub use style::{GlyphStyle, Style, compose_frame_style};
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

