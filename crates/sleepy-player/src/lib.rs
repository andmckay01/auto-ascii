//! `sleepy-player` library surface.
//!
//! The binary is the product (PLAN §2); this lib target exists so
//! `sleepy-factory eval` (M2 item B) can drive the *exact* player frame
//! pipeline headlessly against `SimBackend` — the metrics harness must
//! measure the real renderer, never a reimplementation. The full embeddable
//! API is M4 scope; until then [`pipeline`] is the only module and its
//! surface is registered in INTERFACES.md.

pub mod pipeline;
