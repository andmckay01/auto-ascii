//! META chunk payload: a CBOR map — the ONLY serde/CBOR surface in the
//! container; everything else is hand-rolled fixed layout.

use serde::{Deserialize, Serialize};

/// META contents: provenance (factory version, source name) and palette
/// hints. Unknown keys are ignored when reading.
///
/// Determinism rules:
/// - NO wall-clock timestamps, hostnames, usernames or absolute paths — the
///   writer must produce byte-identical output for identical input.
/// - Struct fields serialize in declaration order via ciborium; additions are
///   append-only (readers ignore unknown keys, `serde(default)` on new fields).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// Factory crate version that produced the asset (provenance).
    pub factory_version: String,
    /// Source identity: file NAME (not path) of the input video.
    pub source: String,
    /// Palette hint keys, e.g. `"ascii/base/fine"`; empty when absent.
    #[serde(default)]
    pub palette_hints: Vec<String>,
}
