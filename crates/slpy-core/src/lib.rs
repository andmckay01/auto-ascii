//! `slpy-core` — the pure engine (PLAN §2): viewport/letterbox math (§3.2),
//! separable resampler (§3.3), base ramp palettes (§3.4), and the `Cell`/`Grid`
//! primitives (§3.1).
//!
//! No dependencies beyond `std`; no terminal, no clock, no I/O — everything in
//! this crate is deterministic and golden-testable (PLAN §8).
//!
//! Hot-path discipline (PLAN §6): `Grid::resize` (called from `Backend::resize`)
//! is the ONLY allocation point in the hot path.

pub mod cell;
pub mod compose;
pub mod grid;
pub mod ramp;
pub mod resample;
pub mod viewport;

pub use cell::{Cell, Rgb};
pub use compose::compose_luma;
pub use grid::Grid;
pub use resample::{Resampler, Tap1D};
pub use viewport::{DEFAULT_CELL_ASPECT, MIN_COLS, MIN_ROWS, Viewport, compute_viewport};
