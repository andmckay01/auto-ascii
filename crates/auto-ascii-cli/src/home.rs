use std::io;
use std::path::{Path, PathBuf};

use auto_ascii::{Composition, timecode};

use crate::BoxErr;

pub struct Home {
    root: PathBuf,
}

impl Home {
    pub fn resolve() -> Result<Home, BoxErr> {
        if let Some(dir) = std::env::var_os("AUTO_ASCII_HOME")
            && !dir.is_empty()
        {
            return Ok(Home { root: PathBuf::from(dir) });
        }
        let home = [
            std::env::var_os("HOME"),
            std::env::var_os("USERPROFILE"),
        ]
        .into_iter()
        .flatten()
        .find(|dir| !dir.is_empty())
        .ok_or("cannot find your home directory (set AUTO_ASCII_HOME)")?;
        Ok(Home { root: PathBuf::from(home).join("auto-ascii") })
    }

    pub fn create(&self) -> io::Result<()> {
        for dir in [self.library(), self.compositions(), self.exports()] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn library(&self) -> PathBuf {
        self.root.join("library")
    }

    pub fn compositions(&self) -> PathBuf {
        self.root.join("compositions")
    }

    pub fn exports(&self) -> PathBuf {
        self.root.join("exports")
    }

    pub fn clip_path(&self, name: &str) -> PathBuf {
        self.library().join(format!("{name}.ascii"))
    }

    pub fn resolve_clip(&self, spec: &str) -> Result<PathBuf, BoxErr> {
        let as_path = Path::new(spec);
        if as_path.is_file() {
            return Ok(as_path.to_path_buf());
        }
        let name = library_name(spec, "ascii");
        let in_library = self.clip_path(name);
        if in_library.is_file() {
            return Ok(in_library);
        }
        let kebab = kebab_case(name);
        if !kebab.is_empty() && kebab != name {
            let kebabbed = self.clip_path(&kebab);
            if kebabbed.is_file() {
                return Ok(kebabbed);
            }
        }
        Err(format!(
            "no clip {spec:?}: not a file here, and {} does not exist \
             (`auto-ascii list` shows the library)",
            in_library.display()
        )
        .into())
    }

    pub fn composition_path(&self, name: &str) -> PathBuf {
        self.compositions().join(format!("{name}.toml"))
    }

    pub fn export_path(&self, name: &str) -> PathBuf {
        self.exports().join(format!("{name}.ascii"))
    }

    pub fn resolve_composition(&self, spec: &str) -> Result<PathBuf, BoxErr> {
        let as_path = Path::new(spec);
        if as_path.is_file() {
            return Ok(as_path.to_path_buf());
        }
        let name = library_name(spec, "toml");
        let in_home = self.composition_path(name);
        if in_home.is_file() {
            return Ok(in_home);
        }
        let kebab = kebab_case(name);
        if !kebab.is_empty() && kebab != name {
            let kebabbed = self.composition_path(&kebab);
            if kebabbed.is_file() {
                return Ok(kebabbed);
            }
        }
        Err(format!(
            "no composition {spec:?}: not a file here, and {} does not exist \
             (`auto-ascii compose new {name}` starts one)",
            in_home.display()
        )
        .into())
    }

    pub fn resolve_playable(&self, spec: &str) -> Result<Target, BoxErr> {
        let as_path = Path::new(spec);
        if as_path.is_file() {
            return Ok(if Composition::is_toml_path(as_path) {
                Target::Composition(as_path.to_path_buf())
            } else {
                Target::Clip(as_path.to_path_buf())
            });
        }
        let (clip_name, comp_name) =
            (library_name(spec, "ascii"), library_name(spec, "toml"));
        let in_library = self.clip_path(clip_name);
        let in_compositions = self.composition_path(comp_name);
        let (clip_kebab, comp_kebab) = (kebab_case(clip_name), kebab_case(comp_name));
        let kebabbed = (!clip_kebab.is_empty()
            && (clip_kebab != clip_name || comp_kebab != comp_name))
            .then(|| (self.clip_path(&clip_kebab), self.composition_path(&comp_kebab)));
        for (clip, composition) in
            std::iter::once((&in_library, &in_compositions)).chain(
                kebabbed.iter().map(|(c, k)| (c, k)),
            )
        {
            if clip.is_file() {
                return Ok(Target::Clip(clip.clone()));
            }
            if composition.is_file() {
                return Ok(Target::Composition(composition.clone()));
            }
        }
        Err(format!(
            "no clip or composition {spec:?}: not a file here, and neither {} nor {} \
             exists (`auto-ascii list` shows the library)",
            in_library.display(),
            in_compositions.display()
        )
        .into())
    }
}

pub enum Target {
    Clip(PathBuf),
    Composition(PathBuf),
}

pub fn sidecar_path(asset: &Path) -> PathBuf {
    asset.with_extension("json")
}

pub fn library_name<'a>(spec: &'a str, ext: &str) -> &'a str {
    spec.strip_suffix(&format!(".{ext}")).unwrap_or(spec)
}

pub fn kebab_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

pub fn stem_of(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn name_for(path: &Path) -> Result<String, BoxErr> {
    let stem = path.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    let name = kebab_case(&stem);
    if name.is_empty() {
        return Err(format!(
            "cannot derive a clip name from {} — pass --name",
            path.display()
        )
        .into());
    }
    Ok(name)
}

pub fn cut_name(from: &str, in_secs: f64, out_secs: f64) -> String {
    kebab_case(&format!("{from}-{}-{}", time_tag(in_secs), time_tag(out_secs)))
}

fn time_tag(secs: f64) -> String {
    let text = timecode::format_mmss(secs);
    let hours = text.matches(':').count() == 2;
    let mut out = String::with_capacity(text.len() + 1);
    let mut seen = 0;
    for ch in text.chars() {
        if ch == ':' {
            seen += 1;
            out.push(if hours && seen == 1 { 'h' } else { 'm' });
        } else {
            out.push(ch);
        }
    }
    out.push('s');
    out
}

pub fn rfc3339_utc(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let sod = unix_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_rules() {
        assert_eq!(kebab_case("clip"), "clip");
        assert_eq!(kebab_case("Apple 1984"), "apple-1984");
        assert_eq!(kebab_case("My Clip (final).mp4"), "my-clip-final-mp4");
        assert_eq!(kebab_case("__already-kebab__"), "already-kebab");
        assert_eq!(kebab_case("a___b"), "a-b");
        assert_eq!(kebab_case("  spaced  out  "), "spaced-out");
        assert_eq!(kebab_case("café"), "caf");
        assert_eq!(kebab_case("../../etc/passwd"), "etc-passwd");
        assert_eq!(kebab_case("%%%"), "");
    }

    #[test]
    fn names_come_from_the_stem() {
        assert_eq!(name_for(Path::new("/tmp/My Clip.mp4")).unwrap(), "my-clip");
        assert_eq!(name_for(Path::new("clip.final.mov")).unwrap(), "clip-final");
        assert!(name_for(Path::new("/tmp/%%%.mov")).is_err());
    }

    #[test]
    fn a_folders_own_extension_is_stripped() {
        assert_eq!(library_name("clip-a", "ascii"), "clip-a");
        assert_eq!(library_name("clip-a.ascii", "ascii"), "clip-a");
        assert_eq!(library_name("demo.toml", "toml"), "demo");
        assert_eq!(library_name("demo.toml", "ascii"), "demo.toml");
        assert_eq!(library_name("clip.final", "ascii"), "clip.final");
        assert_eq!(library_name("toml.demo", "toml"), "toml.demo");
        assert_eq!(library_name(".ascii", "ascii"), "");
    }

    #[test]
    fn cut_names_carry_the_slice() {
        assert_eq!(cut_name("apple-1984", 5.0, 20.0), "apple-1984-0m05s-0m20s");
        assert_eq!(cut_name("clip", 0.0, 2.4), "clip-0m00s-0m02s");
        assert_eq!(cut_name("clip", 3661.0, 3725.0), "clip-1h01m01s-1h02m05s");
        assert_eq!(cut_name("My Clip", 1.0, 2.0), "my-clip-0m01s-0m02s");
    }

    #[test]
    fn stems_are_read_back_verbatim() {
        assert_eq!(stem_of(Path::new("/tmp/lib/My Clip.ascii")), "My Clip");
        assert_eq!(stem_of(Path::new("/tmp/lib/clip-a.ascii")), "clip-a");
        assert_eq!(stem_of(Path::new("/tmp/lib")), "lib");
    }

    #[test]
    fn rfc3339_matches_date_u() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(rfc3339_utc(1_758_326_400), "2025-09-20T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_767_225_599), "2025-12-31T23:59:59Z");
        assert_eq!(rfc3339_utc(1_767_225_600), "2026-01-01T00:00:00Z");
    }

    #[test]
    fn sidecars_sit_beside_their_asset() {
        assert_eq!(
            sidecar_path(Path::new("/tmp/lib/clip.ascii")),
            Path::new("/tmp/lib/clip.json")
        );
    }
}
