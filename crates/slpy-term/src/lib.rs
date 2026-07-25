//! `slpy-term` — terminal backend layer (PLAN §2, §3.1).
//!
//! "Pluggable backends" ≠ N backend structs: terminals differ by capability
//! *data*, not code shape. One [`AnsiBackend`] parameterized by [`Caps`], plus
//! [`SimBackend`] (in-memory, throttleable) for tests/benches. The [`Backend`]
//! trait exists for those two and a future ConPTY oddity — nothing more.
//!
//! M1 scope (PLAN §7): capability probe ([`probe_caps`] — passive env hints,
//! then the DA1-sentinel volley, cached; `--tier`/`--no-query` escape
//! hatches), color tiers truecolor/256/16/mono with quantize-BEFORE-diff
//! (§3.1), DEC 2026 synchronized-output wrap when DECRQM confirmed it, and
//! the diff renderer with invalidate-every-frame as the config default (one
//! render path — full repaint is just `invalidate()` each frame, §3.1).
//! Capability tiers are color depth + glyph repertoire only (Scope
//! amendment: no throughput/connectivity classification).

pub mod ansi;
pub mod backend;
pub mod caps;
pub mod event;
pub mod probe;
pub mod quant;
mod render;
pub mod restore;
pub mod sim;

pub use ansi::AnsiBackend;
pub use backend::Backend;
pub use caps::{Caps, ColorTier, FrameStats, GlyphFlags, GlyphSupportTier};
pub use event::{Event, EventQueue, Key};
pub use probe::{DEFAULT_PROBE_TIMEOUT, ProbeOptions, ProbeParser, ProbeReplies, probe_caps};
pub use restore::{RESTORE_SEQ, install_restore_hooks};
pub use sim::SimBackend;
