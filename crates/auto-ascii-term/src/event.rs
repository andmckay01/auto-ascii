//! Terminal input event types and FIFO event queues.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Esc,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Resize(u16, u16),
    Key(Key),
    Quit,
}

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
