//! Per-video player settings: the live dials and the glyph codec a viewer
//! saved for one video with `s` during playback, restored the next time that
//! video fronts.
//!
//! The file sits beside the asset, the same way `auto-ascii import` keeps a
//! clip's `.json` sidecar: `clip.ascii` → `clip.player.toml`, so the settings
//! travel with the video and work for library clips and loose paths alike.
//! It is a small TOML file in `params.toml`'s own `[compose]` vocabulary —
//! one `key = value` line per [`Dial`] plus `codec`:
//!
//! ```toml
//! codec = "letters"
//! shadow_lift = 64
//! edge_t_on = 32
//! idx_hyst_q8 = 160
//! ```
//!
//! Every key is optional and a missing one keeps its default, which is the
//! whole backward-compatibility story: a file saved before a setting existed
//! (one with no `codec` line, say) loads with that setting at its default
//! (`pixels`). Unknown keys — and a codec name this build does not know —
//! are ignored, so a newer player's file still loads in an older one.
//! Hand-parsed (the same line-based subset `FontTable` reads) so the facade
//! takes no TOML dependency for it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use auto_ascii_core::{Codec, ComposeParams};

use crate::error::Error;
use crate::player::Dial;

/// Suffix that replaces the asset's extension: `clip.ascii` → `clip.player.toml`.
pub const EXTENSION: &str = "player.toml";

// This line-based subset accepts both TOML string quote styles. A hash
// inside a string (including after an escaped quote) is part of its value.
fn without_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (i, ch) in line.char_indices() {
        if escaped {
            escaped = false;
        } else if quote == Some('"') && ch == '\\' {
            escaped = true;
        } else if Some(ch) == quote {
            quote = None;
        } else if quote.is_none() {
            match ch {
                '"' | '\'' => quote = Some(ch),
                '#' => return &line[..i],
                _ => {}
            }
        }
    }
    line
}

/// What one video's settings file holds. `compose` carries the dial fields
/// (see [`Dial::param_key`]); its other fields are always the defaults — they
/// are not viewer-adjustable, so they are neither written nor read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoSettings {
    pub compose: ComposeParams,
    pub codec: Codec,
}

impl VideoSettings {
    /// Where the settings for `asset` live.
    pub fn path_for(asset: &Path) -> PathBuf {
        asset.with_extension(EXTENSION)
    }

    /// Serialize — every dial and the codec, one line each, after a header
    /// comment saying what the file is.
    pub fn to_toml(&self) -> String {
        let mut out = String::from(
            "# auto-ascii player settings for this video, saved with `s` during\n\
             # playback. Keys are params.toml [compose] names; a missing key keeps\n\
             # its default.\n",
        );
        let _ = writeln!(out, "codec = \"{}\"", self.codec.name());
        for dial in Dial::ALL {
            let _ = writeln!(out, "{} = {}", dial.param_key(), dial.param(&self.compose));
        }
        out
    }

    /// Parse a settings file. Missing keys keep their defaults; unknown keys
    /// and unknown codec names are ignored (see the module docs).
    ///
    /// # Errors
    /// A message naming the line for malformed syntax or an out-of-range
    /// dial value.
    pub fn parse(text: &str) -> Result<VideoSettings, String> {
        let mut out = VideoSettings::default();
        for (ln, raw) in text.lines().enumerate() {
            let line = without_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let err = |m: String| format!("line {}: {m}", ln + 1);
            let Some((key, val)) = line.split_once('=') else {
                return Err(err(format!("expected `key = value`, got {line:?}")));
            };
            let (key, val) = (key.trim(), val.trim());
            if key == "codec" {
                let name = val.trim_matches(['"', '\'']);
                out.codec = Codec::from_name(name).unwrap_or_default();
            } else if let Some(dial) = Dial::ALL.into_iter().find(|d| d.param_key() == key) {
                let v: u8 = val
                    .parse()
                    .map_err(|_| err(format!("{key} must be an integer 0..=255, got {val}")))?;
                dial.set_param(&mut out.compose, v);
            }
        }
        Ok(out)
    }

    /// The settings saved for `asset`, or `None` when it has none.
    ///
    /// # Errors
    /// [`Error::Io`] when the file exists but cannot be read,
    /// [`Error::Config`] when it does not parse.
    pub fn load(asset: &Path) -> Result<Option<VideoSettings>, Error> {
        let path = VideoSettings::path_for(asset);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::Io { path, source }),
        };
        VideoSettings::parse(&text)
            .map(Some)
            .map_err(|m| Error::Config(format!("{}: {m}", path.display())))
    }

    /// Write these settings for `asset` (through a temporary file renamed
    /// into place, so a crash never leaves half a file) and return the path.
    ///
    /// # Errors
    /// [`Error::Io`] when the folder is not writable.
    pub fn save(&self, asset: &Path) -> Result<PathBuf, Error> {
        let path = VideoSettings::path_for(asset);
        let tmp = path.with_extension("toml.tmp");
        let io = |source| Error::Io { path: path.clone(), source };
        std::fs::write(&tmp, self.to_toml()).map_err(io)?;
        std::fs::rename(&tmp, &path).map_err(io)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turned() -> VideoSettings {
        let mut compose = ComposeParams::default();
        Dial::ShadowLift.turn(&mut compose, 4);
        Dial::EdgeStrength.turn(&mut compose, -3);
        Dial::Hysteresis.turn(&mut compose, 2);
        VideoSettings { compose, codec: Codec::Letters }
    }

    #[test]
    fn text_round_trip_keeps_every_dial_and_the_codec() {
        let s = turned();
        assert_ne!(s.compose, ComposeParams::default(), "the fixture moved the dials");
        assert_eq!(VideoSettings::parse(&s.to_toml()), Ok(s));
        let d = VideoSettings::default();
        assert_eq!(VideoSettings::parse(&d.to_toml()), Ok(d));
    }

    #[test]
    fn ascii_codec_round_trips_as_text_and_on_disk() {
        let s = VideoSettings { codec: Codec::Ascii, ..turned() };
        assert!(s.to_toml().contains("codec = \"ascii\"\n"), "{}", s.to_toml());
        assert_eq!(VideoSettings::parse(&s.to_toml()), Ok(s));
        assert_eq!(VideoSettings::parse("codec = ascii").unwrap().codec, Codec::Ascii);
        assert_eq!(VideoSettings::parse("codec = 'ascii' # mine").unwrap().codec, Codec::Ascii);

        let dir = std::env::temp_dir().join(format!("auto-ascii-settings-ascii-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let asset = dir.join("clip.ascii");
        s.save(&asset).unwrap();
        assert_eq!(VideoSettings::load(&asset).unwrap(), Some(s));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_without_codec_defaults_to_pixels() {
        let old = "shadow_lift = 64\nedge_t_on = 40\nidx_hyst_q8 = 96\n";
        let s = VideoSettings::parse(old).unwrap();
        assert_eq!(s.codec, Codec::Pixels);
        assert_eq!(
            (s.compose.shadow_lift, s.compose.edge_t_on, s.compose.idx_hyst_q8),
            (64, 40, 96)
        );
        assert_eq!(VideoSettings::parse(""), Ok(VideoSettings::default()));
    }

    #[test]
    fn unknown_keys_and_codecs_are_ignored_but_bad_values_are_not() {
        let fwd = "# newer build\ncodec = \"hieroglyphs\"\nsparkle = 9\nshadow_lift = 16\n";
        let s = VideoSettings::parse(fwd).unwrap();
        assert_eq!((s.codec, s.compose.shadow_lift), (Codec::Pixels, 16));
        assert_eq!(VideoSettings::parse("codec = letters").unwrap().codec, Codec::Letters);
        for bad in ["shadow_lift = 300", "shadow_lift = x", "shadow_lift"] {
            let e = VideoSettings::parse(bad).unwrap_err();
            assert!(e.starts_with("line 1:"), "{bad}: {e}");
        }
    }

    #[test]
    fn trailing_comments_preserve_codec_and_numeric_values() {
        let s = VideoSettings::parse(
            "codec = \"letters\" # favorite\nshadow_lift = 64 # brighter\n\
             edge_t_on = 40 # less edge\nidx_hyst_q8 = 96 # responsive\n\
             future = 'a # value' # ignored\n",
        ).unwrap();
        assert_eq!(s.codec, Codec::Letters);
        assert_eq!((s.compose.shadow_lift, s.compose.edge_t_on, s.compose.idx_hyst_q8), (64, 40, 96));
        let old = VideoSettings::parse("shadow_lift = 64 # old file\nfuture = 1 # ignored").unwrap();
        assert_eq!((old.codec, old.compose.shadow_lift), (Codec::Pixels, 64));
        assert_eq!(VideoSettings::parse("codec = 'letters' # literal").unwrap().codec, Codec::Letters);
        assert_eq!(VideoSettings::parse("codec = \"letters#future\" # unknown").unwrap().codec, Codec::Pixels);
        for value in [r#""a # value""#, r#"'a # value'"#, r#""a \" # value""#, r#""a \\""#] {
            let line = format!("future = {value} # comment");
            assert_eq!(without_comment(&line).trim_end(), format!("future = {value}"));
        }
    }

    #[test]
    fn save_and_load_round_trip_beside_the_asset() {
        let dir = std::env::temp_dir().join(format!("auto-ascii-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let asset = dir.join("My Clip.ascii");
        assert_eq!(VideoSettings::load(&asset).unwrap(), None, "nothing saved yet");

        let s = turned();
        let path = s.save(&asset).unwrap();
        assert_eq!(path, dir.join("My Clip.player.toml"));
        assert_eq!(VideoSettings::load(&asset).unwrap(), Some(s));
        assert!(!dir.join("My Clip.player.toml.tmp").exists(), "the temp file is renamed away");

        let again = VideoSettings { codec: Codec::Pixels, ..s };
        again.save(&asset).unwrap();
        assert_eq!(VideoSettings::load(&asset).unwrap(), Some(again));

        std::fs::write(&path, "shadow_lift = nope\n").unwrap();
        let e = VideoSettings::load(&asset).unwrap_err();
        assert!(matches!(e, Error::Config(_)), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
