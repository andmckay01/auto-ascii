//! Terminal input and output backend interface.

use auto_ascii_core::{Cell, Grid};

use crate::caps::{Caps, FrameStats};
use crate::event::EventQueue;

pub trait Backend {
    fn caps(&self) -> &Caps;

    fn events(&mut self) -> &mut EventQueue;

    fn present(&mut self, grid: &Grid<Cell>) -> FrameStats;

    fn invalidate(&mut self);

    fn resize(&mut self, cols: u16, rows: u16);

    fn shutdown(&mut self);
}
