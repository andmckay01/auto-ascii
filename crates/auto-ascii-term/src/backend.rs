//! The `Backend` trait — exactly two production impls ([`crate::AnsiBackend`],
//! [`crate::SimBackend`]) and nothing else: no connectivity- or
//! platform-specific backends.

use auto_ascii_core::{Cell, Grid};

use crate::caps::{Caps, FrameStats};
use crate::event::EventQueue;

/// Terminal output + input abstraction.
///
/// One render path: there is no separate "full repaint mode" — a full
/// repaint is simply `invalidate()` before every `present()`. Diff-always
/// means one code path, tested everywhere.
pub trait Backend {
    fn caps(&self) -> &Caps;

    /// Event source, drained once per frame.
    fn events(&mut self) -> &mut EventQueue;

    /// Render a full grid: quantize to the caps color tier → diff against the
    /// previous quantized grid → span/SGR elision → ONE `write(2)`. Returns
    /// per-frame stats.
    fn present(&mut self, grid: &Grid<Cell>) -> FrameStats;

    /// Force a full repaint on the next `present` (drops the previous-grid
    /// diff baseline).
    fn invalidate(&mut self);

    /// Adopt a new terminal size. The ONLY allocation point in the hot path:
    /// reallocates diff buffers; implies `invalidate`.
    fn resize(&mut self, cols: u16, rows: u16);

    /// Restore the terminal (SGR 0, cursor show, autowrap on, main screen,
    /// cooked mode). Idempotent; also runs from `Drop` and the signal path
    /// installed by [`crate::install_restore_hooks`].
    fn shutdown(&mut self);
}
