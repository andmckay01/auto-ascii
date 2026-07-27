//! The `Backend` trait (PLAN §3.1) — exactly two production impls
//! ([`crate::AnsiBackend`], [`crate::SimBackend`]) — and nothing else
//! (Scope amendment: no connectivity- or platform-specific backends).

use slpy_core::{Cell, Grid};

use crate::caps::{Caps, FrameStats};
use crate::event::EventQueue;

/// Terminal output + input abstraction (PLAN §3.1).
///
/// One render path (PLAN §3.1): there is no separate "full repaint mode" —
/// GPU-tier full repaint is simply `invalidate()` before every `present()`,
/// which is the M0 config default (PLAN §7). Diff-always means one code path,
/// tested everywhere.
pub trait Backend {
    fn caps(&self) -> &Caps;

    /// Event source drained once per frame (PLAN §3.6 step 1).
    fn events(&mut self) -> &mut EventQueue;

    /// Render a full grid: quantize to the caps color tier → diff against the
    /// previous quantized grid → span/SGR elision → ONE `write(2)`
    /// (PLAN §3.1, §3.6 step 6). Returns per-frame stats for the eval harness.
    fn present(&mut self, grid: &Grid<Cell>) -> FrameStats;

    /// Force a full repaint on the next `present` (drops the previous-grid
    /// diff baseline). M0 default mode calls this every frame (PLAN §7).
    fn invalidate(&mut self);

    /// Adopt a new terminal size. The ONLY allocation point in the hot path
    /// (PLAN §3.1, §6): reallocates diff buffers; implies `invalidate`.
    fn resize(&mut self, cols: u16, rows: u16);

    /// Restore the terminal (SGR 0, cursor show, autowrap on, main screen,
    /// cooked mode — PLAN §3.1 session hygiene). Idempotent; also runs from
    /// `Drop` and the signal path installed by
    /// [`crate::install_restore_hooks`].
    fn shutdown(&mut self);
}
