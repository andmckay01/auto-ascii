//! META chunk payload (PLAN §4): a CBOR map — the ONLY serde/CBOR surface in
//! the container; everything else is hand-rolled fixed layout.

use serde::{Deserialize, Serialize};

/// META contents (PLAN §4: "palette hints, per-plane gamma, provenance,
/// factory version. Unknown keys ignored.").
///
/// Determinism rules (PLAN §4 byte-golden requirement):
/// - NO wall-clock timestamps, hostnames, usernames or absolute paths — the
///   writer must produce byte-identical output for identical input.
/// - Struct fields serialize in declaration order via ciborium; additions are
///   append-only (readers ignore unknown keys, `serde(default)` on new fields).
///
/// M0 carries provenance + factory version only; palette hints and per-plane
/// gamma gain fields at M1/M3.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// Factory crate version that produced the asset (provenance).
    pub factory_version: String,
    /// Source identity: file NAME (not path) of the input video.
    pub source: String,
    /// Palette hint keys (PLAN §3.4), e.g. `"ascii/base/fine"`. Empty at M0.
    #[serde(default)]
    pub palette_hints: Vec<String>,
}
