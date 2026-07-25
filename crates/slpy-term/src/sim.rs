//! `SimBackend` — in-memory backend with a throttleable writer
//! (PLAN §3.1, §6). All headless verification runs against this: M0 fps
//! acceptance at 213×58 / 320×90, and from M2 the 2 MB/s ≥ 24 fps hard gate.

use std::time::Instant;

use slpy_core::{Cell, Grid};

use crate::backend::Backend;
use crate::caps::{Caps, FrameStats};
use crate::event::{Event, EventQueue};
use crate::render::FramePainter;

/// In-memory `Backend`: runs the exact same diff → spans → SGR-elide pipeline
/// as `AnsiBackend` (the shared crate-private painter), but the write lands
/// in a byte buffer, optionally paced to a simulated link speed
/// (PLAN §6 "SimBackend @ 2 MB/s").
pub struct SimBackend {
    caps: Caps,
    events: EventQueue,
    /// Shared diff/assembly pipeline — identical code to `AnsiBackend`.
    painter: FramePainter,
    /// Captured escape-stream output (per-palette golden streams, PLAN §6).
    out: Vec<u8>,
    /// Simulated link speed; `None` = unthrottled.
    throughput_bps: Option<u64>,
}

impl SimBackend {
    /// Unthrottled sim at `cols × rows` with [`Caps::default`] (truecolor).
    pub fn new(cols: u16, rows: u16) -> SimBackend {
        let caps = Caps { cells: (cols, rows), ..Caps::default() };
        SimBackend {
            caps,
            events: EventQueue::new(),
            painter: FramePainter::new(cols, rows),
            out: Vec::new(),
            throughput_bps: None,
        }
    }

    /// Throttle the simulated writer to `bytes_per_sec` (`None` or 0 =
    /// unlimited). `present` accounts the simulated drain time in
    /// `FrameStats::write_ns` — no real sleeping — so pacing/governor logic
    /// is testable headlessly (PLAN §6).
    pub fn set_throughput(&mut self, bytes_per_sec: Option<u64>) {
        self.throughput_bps = bytes_per_sec;
    }

    /// Inject an event (resize storms, quit) — the test-side input channel.
    pub fn push_event(&mut self, ev: Event) {
        self.events.push(ev);
    }

    /// Take and clear the captured output bytes written so far.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
}

impl Backend for SimBackend {
    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn events(&mut self) -> &mut EventQueue {
        &mut self.events
    }

    /// Same pipeline as `AnsiBackend::present`, output captured not written
    /// (PLAN §3.1). Throttled drain time reported via `write_ns`.
    fn present(&mut self, grid: &Grid<Cell>) -> FrameStats {
        let cells_damaged = self.painter.paint(grid, self.caps.color);
        let frame = &self.painter.buf;
        let start = Instant::now();
        self.out.extend_from_slice(frame);
        let write_ns = match self.throughput_bps {
            Some(bps) if bps > 0 => ((frame.len() as u128 * 1_000_000_000) / u128::from(bps)) as u64,
            _ => start.elapsed().as_nanos() as u64,
        };
        FrameStats {
            bytes: frame.len() as u32,
            cells_damaged,
            write_ns,
            dropped: false,
        }
    }

    fn invalidate(&mut self) {
        self.painter.invalidate();
    }

    /// Programmatic size change (mid-run resize simulation): updates
    /// `caps.cells`, reallocates the diff baseline, implies invalidate.
    fn resize(&mut self, cols: u16, rows: u16) {
        self.caps.cells = (cols, rows);
        self.painter.resize(cols, rows);
    }

    fn shutdown(&mut self) {
        // Nothing to restore in-memory.
    }
}
