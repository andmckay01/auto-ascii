//! Terminal capabilities, probing and rendering backends.

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
