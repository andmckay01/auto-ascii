//! ASCI container error values.

use std::fmt;

pub type Result<T> = std::result::Result<T, AsciiError>;

#[derive(Debug)]
pub enum AsciiError {
    Io(std::io::Error),
    BadMagic,
    UnsupportedVersion { found: u16, supported: u16 },
    Truncated,
    UnknownRequiredChunk([u8; 4]),
    CrcMismatch { tag: [u8; 4] },
    BadFrameIndex(u32),
    BadPlaneId(u8),
    Corrupt(&'static str),
    BadMeta,
}

impl fmt::Display for AsciiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsciiError::Io(e) => write!(f, "io error: {e}"),
            AsciiError::BadMagic => write!(f, "not an .ascii file (bad magic)"),
            AsciiError::UnsupportedVersion { found, supported } => {
                write!(f, "unsupported .ascii major version {found} (reader supports <= {supported})")
            }
            AsciiError::Truncated => write!(f, "truncated .ascii file (missing/short data or TRLR)"),
            AsciiError::UnknownRequiredChunk(tag) => {
                write!(f, "unknown required chunk {:?}", String::from_utf8_lossy(tag))
            }
            AsciiError::CrcMismatch { tag } => {
                write!(f, "CRC mismatch in chunk {:?}", String::from_utf8_lossy(tag))
            }
            AsciiError::BadFrameIndex(i) => write!(f, "frame index {i} out of range"),
            AsciiError::BadPlaneId(p) => write!(f, "plane id {p} not in this asset"),
            AsciiError::Corrupt(msg) => write!(f, "corrupt .ascii file: {msg}"),
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
