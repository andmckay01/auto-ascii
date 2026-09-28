//! Per-video dial and style sidecar I/O.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use auto_ascii_core::{ComposeParams, Style};

use crate::error::Error;
use crate::player::Dial;

pub const EXTENSION: &str = "player.toml";

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoSettings {
    pub compose: ComposeParams,
    pub style: Style,
}

impl VideoSettings {
    pub fn path_for(asset: &Path) -> PathBuf {
        asset.with_extension(EXTENSION)
    }

    pub fn persisted(&self) -> VideoSettings {
        let mut compose = ComposeParams::default();
        for dial in Dial::ALL {
            dial.set_param(&mut compose, dial.raw_param_value(&self.compose));
        }
        VideoSettings { compose, style: self.style }
    }

    pub fn to_toml(&self) -> String {
        let mut out = String::from(
            "# auto-ascii player settings for this video, saved with `s` during\n\
             # playback. Keys are params.toml [compose] names; a missing key keeps\n\
             # its default.\n",
        );
        let _ = writeln!(out, "style = \"{}\"", self.style.name());
        for dial in Dial::ALL {
            let _ = writeln!(out, "{} = {}", dial.param_key(), dial.raw_param_value(&self.compose));
        }
        out
    }

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
            if key == "style" || key == "codec" {
                let name = val.trim_matches(['"', '\'']);
                out.style = Style::from_name(name).unwrap_or_default();
            } else if let Some(dial) = Dial::ALL.into_iter().find(|d| d.param_key() == key) {
                let v: u8 = val
                    .parse()
                    .map_err(|_| err(format!("{key} must be an integer 0..=255, got {val}")))?;
                dial.set_param(&mut out.compose, v);
            }
        }
        Ok(out)
    }

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
        VideoSettings { compose, style: Style::Letters }
    }

    #[test]
    fn hysteresis_settings_up_to_255_no_longer_clamp() {
        let s = VideoSettings::parse("idx_hyst_q8 = 255\n").unwrap();
        assert_eq!(s.compose.idx_hyst_q8, 255);
    }

    #[test]
    fn text_round_trip_keeps_every_dial_and_the_style() {
        let s = turned();
        assert_ne!(s.compose, ComposeParams::default(), "the fixture moved the dials");
        assert_eq!(VideoSettings::parse(&s.to_toml()), Ok(s));
        let d = VideoSettings::default();
        assert_eq!(VideoSettings::parse(&d.to_toml()), Ok(d));
    }

    #[test]
    fn a_round_trip_keeps_exactly_the_persisted_projection() {
        let mut s = turned();
        s.compose.edge_t_off = s.compose.edge_t_off.wrapping_add(1);
        s.compose.coh_min_q8 = s.compose.coh_min_q8.wrapping_add(7);
        let persisted = s.persisted();
        assert_eq!(s.to_toml(), persisted.to_toml(), "non-dial fields are not written");
        assert_eq!(VideoSettings::parse(&s.to_toml()), Ok(persisted));
        let dials = |v: &VideoSettings| Dial::ALL.map(|d| d.param(&v.compose));
        assert_eq!(dials(&persisted), dials(&s));
        assert_eq!(persisted, turned().persisted(), "only non-dial fields differed");
        assert_eq!(turned().persisted(), turned(), "dial-only settings are their own projection");
    }

    #[test]
    fn ascii_style_round_trips_as_text_and_on_disk() {
        let s = VideoSettings { style: Style::Ascii, ..turned() };
        assert!(s.to_toml().contains("style = \"ascii\"\n"), "{}", s.to_toml());
        assert_eq!(VideoSettings::parse(&s.to_toml()), Ok(s));
        assert_eq!(VideoSettings::parse("style = ascii").unwrap().style, Style::Ascii);
        assert_eq!(VideoSettings::parse("style = 'ascii' # mine").unwrap().style, Style::Ascii);

        let dir = std::env::temp_dir().join(format!("auto-ascii-settings-ascii-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let asset = dir.join("clip.ascii");
        s.save(&asset).unwrap();
        assert_eq!(VideoSettings::load(&asset).unwrap(), Some(s));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_saved_with_the_old_codec_key_still_load() {
        assert_eq!(VideoSettings::parse("codec = \"letters\"\n").unwrap().style, Style::Letters);
        assert_eq!(VideoSettings::parse("codec = 'ascii' # old build").unwrap().style, Style::Ascii);
        let old = VideoSettings::parse("codec = \"letters\"\nshadow_lift = 64\n").unwrap();
        assert_eq!((old.style, old.compose.shadow_lift), (Style::Letters, 64));
        assert!(!old.to_toml().contains("codec"), "new writes use `style`: {}", old.to_toml());
        assert_eq!(VideoSettings::parse(&old.to_toml()), Ok(old));
    }

    #[test]
    fn a_file_without_style_defaults_to_ascii() {
        let old = "shadow_lift = 64\nedge_t_on = 40\nidx_hyst_q8 = 96\n";
        let s = VideoSettings::parse(old).unwrap();
        assert_eq!(s.style, Style::Ascii);
        assert_eq!(
            (s.compose.shadow_lift, s.compose.edge_t_on, s.compose.idx_hyst_q8),
            (64, 40, 96)
        );
        assert_eq!(VideoSettings::parse(""), Ok(VideoSettings::default()));
    }

    #[test]
    fn unknown_keys_and_styles_are_ignored_but_bad_values_are_not() {
        let fwd = "# newer build\nstyle = \"hieroglyphs\"\nsparkle = 9\nshadow_lift = 16\n";
        let s = VideoSettings::parse(fwd).unwrap();
        assert_eq!((s.style, s.compose.shadow_lift), (Style::Ascii, 16));
        assert_eq!(VideoSettings::parse("style = letters").unwrap().style, Style::Letters);
        for bad in ["shadow_lift = 300", "shadow_lift = x", "shadow_lift"] {
            let e = VideoSettings::parse(bad).unwrap_err();
            assert!(e.starts_with("line 1:"), "{bad}: {e}");
        }
    }

    #[test]
    fn trailing_comments_preserve_style_and_numeric_values() {
        let s = VideoSettings::parse(
            "style = \"letters\" # favorite\nshadow_lift = 64 # brighter\n\
             edge_t_on = 40 # less edge\nidx_hyst_q8 = 96 # responsive\n\
             future = 'a # value' # ignored\n",
        ).unwrap();
        assert_eq!(s.style, Style::Letters);
        assert_eq!((s.compose.shadow_lift, s.compose.edge_t_on, s.compose.idx_hyst_q8), (64, 40, 96));
        let old = VideoSettings::parse("shadow_lift = 64 # old file\nfuture = 1 # ignored").unwrap();
        assert_eq!((old.style, old.compose.shadow_lift), (Style::Ascii, 64));
        assert_eq!(VideoSettings::parse("style = 'letters' # literal").unwrap().style, Style::Letters);
        assert_eq!(VideoSettings::parse("style = \"letters#future\" # unknown").unwrap().style, Style::Ascii);
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

        let again = VideoSettings { style: Style::Pixels, ..s };
        again.save(&asset).unwrap();
        assert_eq!(VideoSettings::load(&asset).unwrap(), Some(again));

        std::fs::write(&path, "shadow_lift = nope\n").unwrap();
        let e = VideoSettings::load(&asset).unwrap_err();
        assert!(matches!(e, Error::Config(_)), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
