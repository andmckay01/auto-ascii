//! Input events: `Resize(cols, rows) | Key | Quit`.

use std::collections::VecDeque;

/// Minimal key model — deliberately not crossterm's `KeyEvent`: the pub API
/// stays dependency-free above `std` types (crossterm is an implementation
/// detail of `AnsiBackend`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// Ctrl-modified letter, lowercase (`Ctrl('c')`).
    Ctrl(char),
    Esc,
    /// Left arrow — scrub back 5 s.
    Left,
    /// Right arrow — scrub forward 5 s.
    Right,
}

/// Backend event. `Resize` carries the new `(cols, rows)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Resize(u16, u16),
    Key(Key),
    Quit,
}

/// FIFO event queue drained once per frame. Backends push (from crossterm
/// polls or test scripts); the caller pops until empty.
#[derive(Debug, Default)]
pub struct EventQueue {
    q: VecDeque<Event>,
}

impl EventQueue {
    pub fn new() -> EventQueue {
        EventQueue::default()
    }

    pub fn push(&mut self, ev: Event) {
        self.q.push_back(ev);
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.q.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.q.is_empty()
    }

    pub fn clear(&mut self) {
        self.q.clear();
    }
}
