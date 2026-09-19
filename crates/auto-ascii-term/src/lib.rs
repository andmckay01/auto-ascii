//! `auto-ascii-term` — terminal backend layer (PLAN §2, §3.1).
//!
//! "Pluggable backends" ≠ N backend structs: terminals differ by capability
//! *data*, not code shape. One [`AnsiBackend`] parameterized by [`Caps`], plus
//! [`SimBackend`] (in-memory, throttleable) for tests/benches. The [`Backend`]
//! trait exists for exactly those two — nothing more.
//!
//! M1 scope (PLAN §7): capability probe ([`probe_caps`] — passive env hints,
//! then the DA1-sentinel volley, cached; `--tier`/`--no-query` escape
//! hatches), color tiers truecolor/256/16/mono with quantize-BEFORE-diff
//! (§3.1), DEC 2026 synchronized-output wrap when DECRQM confirmed it, and
//! the diff renderer with invalidate-every-frame as the config default (one
//! render path — full repaint is just `invalidate()` each frame, §3.1).
//! Capability tiers are color depth + glyph repertoire only (Scope
//! amendment: no throughput/connectivity classification).
//!
//! M4 (item B): everything that touches a real terminal — [`AnsiBackend`],
//! the probe volley, the restore hooks — is gated behind the default-on
//! `session` feature (crossterm + libc). Without it the crate is pure data +
//! code: the [`Backend`] trait, [`Caps`] types, [`SimBackend`], quantizer and
//! diff renderer — exactly what a terminal-free embedder build needs.

#[cfg(feature = "session")]
pub mod ansi;
pub mod backend;
pub mod caps;
pub mod event;
#[cfg(feature = "session")]
pub mod probe;
pub mod quant;
#[cfg(feature = "session")]
pub mod quirks;
mod render;
#[cfg(feature = "session")]
pub mod restore;
pub mod sim;

#[cfg(feature = "session")]
pub use ansi::AnsiBackend;
pub use backend::Backend;
pub use caps::{Caps, ColorTier, FrameStats, GlyphFlags, GlyphSupportTier};
pub use event::{Event, EventQueue, Key};
#[cfg(feature = "session")]
pub use probe::{DEFAULT_PROBE_TIMEOUT, ProbeOptions, ProbeParser, ProbeReplies, probe_caps};
#[cfg(feature = "session")]
pub use restore::{RESTORE_SEQ, install_restore_hooks};
pub use sim::SimBackend;
