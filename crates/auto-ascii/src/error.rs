use std::fmt;
use std::path::PathBuf;

use auto_ascii_format::AsciiError;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Format {
        path: PathBuf,
        source: AsciiError,
    },
    Asset(&'static str),
    Decode {
        frame: u32,
        plane: u8,
        source: AsciiError,
    },
    Config(String),
    Terminal(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, .. } => write!(f, "cannot open {}", path.display()),
            Error::Format { path, .. } => {
                write!(f, "{} is not a valid .ascii asset", path.display())
            }
            Error::Asset(msg) => write!(f, "unplayable asset: {msg}"),
            Error::Decode { frame, plane, .. } => {
                write!(f, "decoding plane {plane} of frame {frame}")
            }
            Error::Config(msg) => write!(f, "invalid configuration: {msg}"),
            Error::Terminal(_) => write!(
                f,
                "cannot enter terminal session (headless? use RenderSession or --sim)"
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
        let e = Error::Decode { frame: 7, plane: 2, source: AsciiError::BadFrameIndex(7) };
        assert_eq!(e.to_string(), "decoding plane 2 of frame 7");
        let src = e.source().expect("AsciiError must be reachable via source()");
        assert_eq!(src.to_string(), "frame index 7 out of range");
        let e = Error::Asset("asset has zero frames");
        assert!(e.source().is_none());
        assert_eq!(e.to_string(), "unplayable asset: asset has zero frames");
    }

    #[test]
    fn display_never_repeats_the_source() {
        let errors = [
            Error::Io {
                path: "/tmp/x.ascii".into(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            },
            Error::Format { path: "/tmp/x.ascii".into(), source: AsciiError::BadFrameIndex(3) },
            Error::Decode { frame: 7, plane: 2, source: AsciiError::BadFrameIndex(7) },
            Error::Terminal(std::io::Error::other("not a tty")),
        ];
        for e in &errors {
            let display = e.to_string();
            let cause = e.source().expect("layered variant").to_string();
            assert!(
                !display.contains(&cause),
                "Display {display:?} embeds its source {cause:?} — chain printers \
                 would show the cause twice"
            );
        }
    }
}
