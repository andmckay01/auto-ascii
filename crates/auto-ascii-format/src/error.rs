//! Error type for ASCI reading/writing.

use std::fmt;

pub type Result<T> = std::result::Result<T, AsciiError>;

/// ASCI container error. Reader policy: unknown non-required chunks are
/// skipped by size; unknown *required* chunks and an unsupported major
/// version are hard errors; a missing TRLR means truncation.
#[derive(Debug)]
pub enum AsciiError {
    Io(std::io::Error),
    /// First 4 bytes are not `"ASCI"`.
    BadMagic,
    /// `version_major` above what this reader supports.
    UnsupportedVersion { found: u16, supported: u16 },
    /// File ends before a complete header/chunk/payload, or the TRLR chunk is
    /// missing.
    Truncated,
    /// A required chunk (flags bit0) with an unknown tag.
    UnknownRequiredChunk([u8; 4]),
    /// Per-chunk CRC32 mismatch.
    CrcMismatch { tag: [u8; 4] },
    /// Frame index out of range.
    BadFrameIndex(u32),
    /// Plane id not present in this asset's `plane_ids` registry entry.
    BadPlaneId(u8),
    /// Structural corruption with a static description.
    Corrupt(&'static str),
    /// META CBOR decode failure: malformed CBOR, not a schema mismatch
    /// (unknown keys are ignored).
    BadMeta,
}

impl fmt::Display for AsciiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsciiError::Io(e) => write!(f, "io error: {e}"),
            AsciiError::BadMagic => write!(f, "not a ASCI file (bad magic)"),
            AsciiError::UnsupportedVersion { found, supported } => {
                write!(f, "unsupported ASCI major version {found} (reader supports <= {supported})")
            }
            AsciiError::Truncated => write!(f, "truncated ASCI file (missing/short data or TRLR)"),
            AsciiError::UnknownRequiredChunk(tag) => {
                write!(f, "unknown required chunk {:?}", String::from_utf8_lossy(tag))
            }
            AsciiError::CrcMismatch { tag } => {
                write!(f, "CRC mismatch in chunk {:?}", String::from_utf8_lossy(tag))
            }
            AsciiError::BadFrameIndex(i) => write!(f, "frame index {i} out of range"),
            AsciiError::BadPlaneId(p) => write!(f, "plane id {p} not in this asset"),
            AsciiError::Corrupt(msg) => write!(f, "corrupt ASCI file: {msg}"),
            AsciiError::BadMeta => write!(f, "malformed META CBOR"),
        }
    }
}

impl std::error::Error for AsciiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AsciiError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for AsciiError {
    fn from(e: std::io::Error) -> AsciiError {
        AsciiError::Io(e)
    }
}
