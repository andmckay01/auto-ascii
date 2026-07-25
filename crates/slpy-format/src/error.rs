//! Error type for SLPY reading/writing (PLAN §4 validation rules).

use std::fmt;

pub type Result<T> = std::result::Result<T, SlpyError>;

/// SLPY container error (PLAN §4). Reader policy: unknown non-required chunks
/// are skipped by size; unknown *required* chunks and a major-version mismatch
/// are hard errors; a missing TRLR means truncation → factory rerun.
#[derive(Debug)]
pub enum SlpyError {
    Io(std::io::Error),
    /// First 4 bytes are not `"SLPY"`.
    BadMagic,
    /// `version_major` above what this reader supports (PLAN §4 header).
    UnsupportedVersion { found: u16, supported: u16 },
    /// File ends before a complete header/chunk/payload; also the missing-TRLR
    /// case (PLAN §4: absence ⇒ truncated ⇒ factory rerun).
    Truncated,
    /// A required chunk (flags bit0) with an unknown tag (PLAN §4 compat).
    UnknownRequiredChunk([u8; 4]),
    /// Per-chunk CRC32 mismatch (PLAN §4 determinism).
    CrcMismatch { tag: [u8; 4] },
    /// Frame index out of range.
    BadFrameIndex(u32),
    /// Plane id not present in this asset's `plane_ids` registry entry.
    BadPlaneId(u8),
    /// Structural corruption with a static description.
    Corrupt(&'static str),
    /// META CBOR decode failure (unknown keys are ignored, PLAN §4 — this is
    /// malformed CBOR, not a schema mismatch).
    BadMeta,
}

impl fmt::Display for SlpyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SlpyError::Io(e) => write!(f, "io error: {e}"),
            SlpyError::BadMagic => write!(f, "not a SLPY file (bad magic)"),
            SlpyError::UnsupportedVersion { found, supported } => {
                write!(f, "unsupported SLPY major version {found} (reader supports <= {supported})")
            }
            SlpyError::Truncated => write!(f, "truncated SLPY file (missing/short data or TRLR)"),
            SlpyError::UnknownRequiredChunk(tag) => {
                write!(f, "unknown required chunk {:?}", String::from_utf8_lossy(tag))
            }
            SlpyError::CrcMismatch { tag } => {
                write!(f, "CRC mismatch in chunk {:?}", String::from_utf8_lossy(tag))
            }
            SlpyError::BadFrameIndex(i) => write!(f, "frame index {i} out of range"),
            SlpyError::BadPlaneId(p) => write!(f, "plane id {p} not in this asset"),
            SlpyError::Corrupt(msg) => write!(f, "corrupt SLPY file: {msg}"),
            SlpyError::BadMeta => write!(f, "malformed META CBOR"),
        }
    }
}

impl std::error::Error for SlpyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SlpyError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SlpyError {
    fn from(e: std::io::Error) -> SlpyError {
        SlpyError::Io(e)
    }
}
