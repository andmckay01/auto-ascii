//! The one coherent error type of the facade (M4 item A): thiserror-style
//! layering over [`SlpyError`] and `std::io::Error`, hand-rolled to keep the
//! embedder dependency tree at zero beyond the engine itself (matching the
//! slpy-format convention).

use std::fmt;
use std::path::PathBuf;

use slpy_format::SlpyError;

/// Everything a `Player` or [`crate::RenderSession`] can fail with.
///
/// Layering: container-level failures carry the underlying [`SlpyError`]
/// (reachable through [`std::error::Error::source`] for `anyhow`-style chain
/// printing); OS failures carry the `std::io::Error`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Opening or memory-mapping the asset file failed.
    Io {
        /// The asset path as given.
        path: PathBuf,
        /// The underlying OS error.
        source: std::io::Error,
    },
    /// The file is not a valid SLPY container (bad magic, truncation,
    /// unsupported version, CRC mismatch, ...).
    Format {
        /// The asset path as given.
        path: PathBuf,
        /// The container-level failure.
        source: SlpyError,
    },
    /// The container parsed but cannot be played (no luma plane, zero
    /// frames, corrupt fps).
    Asset(&'static str),
    /// Decoding one plane of one frame failed mid-playback.
    Decode {
        /// The asset frame index being decoded.
        frame: u32,
        /// The SLPY plane id (PLAN §4 registry) being decoded.
        plane: u8,
        /// The container-level failure.
        source: SlpyError,
    },
    /// Invalid builder/session configuration (e.g. an fps cap ≤ 0, a seek
    /// past the end of the asset, a frame index out of range).
    Config(String),
    /// Entering or driving the terminal session failed (stdout is not a
    /// TTY, raw mode rejected, write error).
    Terminal(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "cannot open {}: {source}", path.display()),
            Error::Format { path, source } => {
                write!(f, "{} is not a valid SLPY asset: {source}", path.display())
            }
            Error::Asset(msg) => write!(f, "unplayable asset: {msg}"),
            Error::Decode { frame, plane, source } => {
                write!(f, "decoding plane {plane} of frame {frame}: {source}")
            }
            Error::Config(msg) => write!(f, "invalid configuration: {msg}"),
            Error::Terminal(e) => write!(
                f,
                "cannot enter terminal session (headless? use RenderSession or --sim): {e}"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Format { source, .. } => Some(source),
            Error::Decode { source, .. } => Some(source),
            Error::Terminal(source) => Some(source),
            Error::Asset(_) | Error::Config(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn display_and_source_chain() {
        let e = Error::Decode { frame: 7, plane: 2, source: SlpyError::BadFrameIndex(7) };
        assert_eq!(e.to_string(), "decoding plane 2 of frame 7: frame index 7 out of range");
        assert!(e.source().is_some(), "SlpyError must be reachable via source()");
        let e = Error::Asset("asset has zero frames");
        assert!(e.source().is_none());
        assert_eq!(e.to_string(), "unplayable asset: asset has zero frames");
    }
}
