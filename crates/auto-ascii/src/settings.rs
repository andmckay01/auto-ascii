//! Per-video dial and style sidecar I/O.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
        compose.lift_color = self.compose.lift_color;
        compose.dither = self.compose.dither;
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
        let _ = writeln!(out, "lift_color = {}", self.compose.lift_color);
        let _ = writeln!(out, "dither = {}", self.compose.dither);
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
            if key == "style" {
                let name = val.trim_matches(['"', '\'']);
                out.style = Style::from_name(name).unwrap_or_default();
            } else if key == "lift_color" || key == "dither" {
                let v: u8 = val.parse()
                    .map_err(|_| err(format!("{key} must be an integer 0..=255, got {val}")))?;
                if key == "dither" {
                    if v > 2 {
                        return Err(err("dither must be in 0..=2".into()));
                    }
                    out.compose.dither = v;
                } else {
                    out.compose.lift_color = v;
                }
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
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let tmp = path.with_extension(format!("toml.{}.{nanos}.tmp", std::process::id()));
        match self.write_then_rename(&tmp, &path) {
            Ok(()) => Ok(path),
            Err(source) => Err(Error::Io { path, source }),
        }
    }

    fn write_then_rename(&self, tmp: &Path, path: &Path) -> std::io::Result<()> {
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(tmp)?
            .write_all(self.to_toml().as_bytes());
        let renamed = written.and_then(|()| std::fs::rename(tmp, path));
        if renamed.is_err() {
            let _ = std::fs::remove_file(tmp);
        }
        renamed
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

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn shadow_options_round_trip_and_validate() {
        let s = VideoSettings::parse("shadow_lift = 160\nlift_color = 255\ndither = 2\n").unwrap();
        assert_eq!((s.compose.shadow_lift, s.compose.lift_color, s.compose.dither), (160, 255, 2));
        assert_eq!(s.persisted(), s);
        assert_eq!(VideoSettings::parse(&s.to_toml()), Ok(s));
        for bad in ["lift_color = 256", "dither = 3", "dither = -1", "dither = noise"] {
            assert!(VideoSettings::parse(bad).is_err(), "{bad}");
        }
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
    fn a_codec_key_is_just_an_unknown_key() {
        assert_eq!(VideoSettings::parse("codec = \"letters\"\n").unwrap().style, Style::Ascii);
        let old = VideoSettings::parse("codec = \"letters\"\nshadow_lift = 64\n").unwrap();
        assert_eq!((old.style, old.compose.shadow_lift), (Style::Ascii, 64));
        assert!(!old.to_toml().contains("codec"), "writes use `style`: {}", old.to_toml());
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
        let temps: Vec<String> = names(&dir)
            .into_iter()
            .filter(|name| name.starts_with("My Clip.player.toml.") && name.ends_with(".tmp"))
            .collect();
        assert!(temps.is_empty(), "the temp file is renamed away: {temps:?}");

        let again = VideoSettings { style: Style::Pixels, ..s };
        again.save(&asset).unwrap();
        assert_eq!(VideoSettings::load(&asset).unwrap(), Some(again));

        std::fs::write(&path, "shadow_lift = nope\n").unwrap();
        let e = VideoSettings::load(&asset).unwrap_err();
        assert!(matches!(e, Error::Config(_)), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn saving_never_writes_through_a_symlink_planted_at_the_old_temp_name() {
        let root = std::env::temp_dir().join(format!("auto-ascii-settings-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (clip_dir, home) = (root.join("clip"), root.join("home"));
        std::fs::create_dir_all(&clip_dir).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let victim = home.join(".zshrc");
        std::fs::write(&victim, "export PATH=/usr/bin\n").unwrap();
        std::os::unix::fs::symlink("../home/.zshrc", clip_dir.join("clip.player.toml.tmp")).unwrap();

        let asset = clip_dir.join("clip.ascii");
        let path = turned().save(&asset).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "export PATH=/usr/bin\n");
        assert_eq!(path, clip_dir.join("clip.player.toml"));
        assert!(std::fs::symlink_metadata(&path).unwrap().is_file(), "a regular file, not a link");
        assert_eq!(VideoSettings::load(&asset).unwrap(), Some(turned()));
        assert_eq!(names(&clip_dir), ["clip.player.toml", "clip.player.toml.tmp"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_planted_at_the_exact_temp_name_is_refused_not_followed() {
        let dir = std::env::temp_dir().join(format!("auto-ascii-settings-exact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim.rc");
        std::fs::write(&victim, "export PATH=/usr/bin\n").unwrap();
        let (tmp, path) = (dir.join("clip.player.toml.1.2.tmp"), dir.join("clip.player.toml"));
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();

        let e = turned().write_then_rename(&tmp, &path).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "export PATH=/usr/bin\n");
        assert!(std::fs::symlink_metadata(&tmp).unwrap().file_type().is_symlink(), "the planted link is left alone");
        assert!(!path.exists(), "nothing was renamed into place");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_save_removes_its_temp_file() {
        let dir = std::env::temp_dir().join(format!("auto-ascii-settings-failed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("clip.player.toml")).unwrap();
        let e = turned().save(&dir.join("clip.ascii")).unwrap_err();
        assert!(matches!(e, Error::Io { .. }), "{e}");
        assert_eq!(names(&dir), ["clip.player.toml"], "only the directory that blocked the rename");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
