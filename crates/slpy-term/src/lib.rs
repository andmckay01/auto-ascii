//! `slpy-term` — terminal backend layer (PLAN §2, §3.1).
//!
//! "Pluggable backends" ≠ N backend structs: terminals differ by capability
//! *data*, not code shape. One [`AnsiBackend`] parameterized by [`Caps`], plus
//! [`SimBackend`] (in-memory, throttleable) for tests/benches. The [`Backend`]
//! trait exists for those two and a future ConPTY oddity — nothing more.
//!
//! M0 scope (PLAN §7): truecolor gray fg, diff renderer with
//! invalidate-every-frame as the config default (one render path — full
//! repaint is just `invalidate()` each frame, §3.1). The capability probe
//! (DA1-sentinel volley, cache, `--tier`/`--no-query`) lands at M1; M0 uses
//! [`Caps::default`] or explicit construction.

pub mod ansi;
pub mod backend;
pub mod caps;
pub mod event;
mod render;
pub mod restore;
pub mod sim;

pub use ansi::AnsiBackend;
pub use backend::Backend;
pub use caps::{Caps, ColorTier, FrameStats, GlyphFlags, GlyphSupportTier, Throughput};
pub use event::{Event, EventQueue, Key};
pub use restore::{RESTORE_SEQ, install_restore_hooks};
pub use sim::SimBackend;
