//! The home folder and the naming rules around it (PLAN-M6-M8 §0.2, §2).
//!
//! `~/auto-ascii/` — override `AUTO_ASCII_HOME` — with three visible
//! subfolders created on demand: `library/` (processed clips + provenance
//! sidecars), `compositions/` (`<name>.toml` timelines) and `exports/`
//! (flattened compositions). Visible and discoverable beats a dotfile
//! cache: an agent can `ls` it and a human can find what the agent made.

use std::io;
use std::path::{Path, PathBuf};

use auto_ascii::{Composition, timecode};

use crate::BoxErr;

/// The resolved home folder. Construct with [`Home::resolve`]; nothing here
/// touches the filesystem until [`Home::create`].
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// `$AUTO_ASCII_HOME` if set and non-empty, else `~/auto-ascii`.
    pub fn resolve() -> Result<Home, BoxErr> {
        if let Some(dir) = std::env::var_os("AUTO_ASCII_HOME")
            && !dir.is_empty()
        {
            return Ok(Home { root: PathBuf::from(dir) });
        }
        // An EMPTY `HOME` is no home, the same way an empty
        // `AUTO_ASCII_HOME` is none: `PathBuf::from("").join("auto-ascii")`
        // is the relative path `auto-ascii`, which would quietly build a
        // library in whatever directory the agent happened to run in.
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

    /// Create the root and all three subfolders. Idempotent.
    pub fn create(&self) -> io::Result<()> {
        for dir in [self.library(), self.compositions(), self.exports()] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// The home folder itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<home>/library` — processed clips and their sidecars.
    pub fn library(&self) -> PathBuf {
        self.root.join("library")
    }

    /// `<home>/compositions` — M8 timelines.
    pub fn compositions(&self) -> PathBuf {
        self.root.join("compositions")
    }

    /// `<home>/exports` — flattened compositions.
    pub fn exports(&self) -> PathBuf {
        self.root.join("exports")
    }

    /// Where the clip named `name` lives (whether or not it exists yet).
    pub fn clip_path(&self, name: &str) -> PathBuf {
        self.library().join(format!("{name}.ascii"))
    }

    /// Resolve a `<clip>` argument per §2, in this order: an existing path
    /// wins, else `library/<spec>.ascii`, else `library/<kebab>.ascii`.
    ///
    /// The kebab fallback is a convenience, not the rule: clips are named
    /// by their file stem verbatim, so `list` and `info` always agree.
    /// It exists because an agent that saw `My Clip.mp4` go in will often
    /// ask for `My Clip` back.
    pub fn resolve_clip(&self, spec: &str) -> Result<PathBuf, BoxErr> {
        let as_path = Path::new(spec);
        if as_path.is_file() {
            return Ok(as_path.to_path_buf());
        }
        // `clip-a` and `clip-a.ascii` name the same clip: the library adds
        // the extension, so carrying one in would ask for `clip-a.ascii.ascii`.
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

    /// Where the composition named `name` lives (whether or not it exists
    /// yet).
    pub fn composition_path(&self, name: &str) -> PathBuf {
        self.compositions().join(format!("{name}.toml"))
    }

    /// Where the export of the composition named `name` lands by default.
    pub fn export_path(&self, name: &str) -> PathBuf {
        self.exports().join(format!("{name}.ascii"))
    }

    /// Resolve a `<name>` argument for `compose …`, the same three steps
    /// [`resolve_clip`](Home::resolve_clip) takes: an existing path wins
    /// (a `.toml` may live anywhere), else `compositions/<name>.toml`,
    /// else `compositions/<kebab>.toml` — where `<name>` is `spec` without
    /// a trailing `.toml`, so `demo` and `demo.toml` are one composition.
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

    /// Resolve a `play` argument, which is a clip OR a composition
    /// (PLAN-M6-M8 §3). Clips are tried first, so every M7 spelling still
    /// means what it meant; a `.toml` is always a composition.
    pub fn resolve_playable(&self, spec: &str) -> Result<Target, BoxErr> {
        let as_path = Path::new(spec);
        if as_path.is_file() {
            // The facade decides what a composition file looks like, so
            // the player, `headless-dump` and this agree — including that
            // `Demo.TOML` is one, which the file systems this ships to
            // would hand us either way.
            return Ok(if Composition::is_toml_path(as_path) {
                Target::Composition(as_path.to_path_buf())
            } else {
                Target::Clip(as_path.to_path_buf())
            });
        }
        // Each folder strips only the extension IT adds, so `demo.toml`
        // asks `compositions/` for `demo.toml` and `clip.ascii` asks
        // `library/` for `clip.ascii`.
        let (clip_name, comp_name) =
            (library_name(spec, "ascii"), library_name(spec, "toml"));
        let in_library = self.clip_path(clip_name);
        let in_compositions = self.composition_path(comp_name);
        // The kebab forms are the same convenience `resolve_clip` documents:
        // an agent that saw `My Clip.mp4` go in asks for `My Clip` back.
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

/// What a `play` argument named: one clip, or a timeline of them.
pub enum Target {
    /// An `.ascii` asset.
    Clip(PathBuf),
    /// A composition `.toml`.
    Composition(PathBuf),
}

/// The sidecar that belongs to an asset: the same path with a `.json`
/// extension, so it works for library clips and loose paths alike.
pub fn sidecar_path(asset: &Path) -> PathBuf {
    asset.with_extension("json")
}

/// `spec` without the extension its folder adds back: `clip-a.ascii` and
/// `clip-a` are one clip, `demo.toml` and `demo` one composition.
///
/// Only that one extension is stripped, and only as a SUFFIX — a clip
/// really called `clip.final` keeps its dot, and `demo.toml` never asks
/// the folder for `demo.toml.toml`.
pub fn library_name<'a>(spec: &'a str, ext: &str) -> &'a str {
    spec.strip_suffix(&format!(".{ext}")).unwrap_or(spec)
}

/// Kebab-case a file stem or a `--name`: lowercase, every run of
/// non-alphanumerics collapsed to one `-`, leading/trailing `-` trimmed.
///
/// Applied to `--name` as well as to the derived default, deliberately —
/// it is the documented naming rule (the agent guide's rule 1), and it also
/// means a name can never contain a path separator or `..`.
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

/// The clip name of an asset already on disk: its file stem verbatim
/// (lossy only when the OS string is not UTF-8), NOT kebab-cased.
///
/// Kebab-casing belongs to `import`, which chooses the name; every reader
/// must report what is actually in `library/`, or `list` and `info` would
/// disagree about the same file.
pub fn stem_of(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The kebab-cased stem of `path`, or an error naming the flag that fixes
/// it — a file called `.mp4` or `%%%.mov` has no usable name. `import`
/// only: see [`stem_of`] for reading a name back.
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

/// The default name of a `cut`: the source clip's name with the slice's
/// two timestamps made file-name-safe — `apple-1984` cut `0:05`..`0:20`
/// becomes `apple-1984-0m05s-0m20s` (PLAN-M6-M8 §3). Kebab-cased like
/// every other name on the way in, so a cut of a hand-dropped
/// `My Clip.ascii` still lands inside `library/`.
pub fn cut_name(from: &str, in_secs: f64, out_secs: f64) -> String {
    kebab_case(&format!("{from}-{}-{}", time_tag(in_secs), time_tag(out_secs)))
}

/// One timestamp as a file-name component: the shared formatter's `M:SS`
/// (or `H:MM:SS`) with its colons spelled out — `0:05` -> `0m05s`,
/// `1:01:01` -> `1h01m01s`. Sub-second parts are truncated, exactly as
/// `format_mmss` truncates them, so two cuts of one clip that differ only
/// inside a second collide by name and want `--name`.
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

/// Format a Unix timestamp as RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Hand-rolled rather than a `chrono`/`time` dependency: the sidecar needs
/// exactly one direction of exactly one format, and the workspace's
/// no-new-deps rule applies to the CLI too. The date split is Howard
/// Hinnant's `civil_from_days`, which is exact for every proleptic
/// Gregorian date — leap years and centuries included.
pub fn rfc3339_utc(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let sod = unix_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Days since the Unix epoch → (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift to an era starting 0000-03-01 so leap day is the last day of
    // the year and every 400-year era has the same 146097-day shape.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // day of era, [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year, [0, 365]
    let mp = (5 * doy + 2) / 153; // month, March = 0
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
        // No name can escape the library folder.
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
        // Only that folder's extension, and only at the end.
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
        // The source name goes through the same kebab rule as --name, so a
        // cut of a hand-dropped file cannot escape the library either.
        assert_eq!(cut_name("My Clip", 1.0, 2.0), "my-clip-0m01s-0m02s");
    }

    /// Readers report the stem verbatim, so `list` and `info` agree even
    /// for a file a human dropped into `library/` by hand.
    #[test]
    fn stems_are_read_back_verbatim() {
        assert_eq!(stem_of(Path::new("/tmp/lib/My Clip.ascii")), "My Clip");
        assert_eq!(stem_of(Path::new("/tmp/lib/clip-a.ascii")), "clip-a");
        assert_eq!(stem_of(Path::new("/tmp/lib")), "lib");
    }

    #[test]
    fn rfc3339_matches_date_u() {
        // Reference values from `date -u -r N`.
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(rfc3339_utc(1_758_326_400), "2025-09-20T00:00:00Z");
        // Both kinds of leap year: 2000 is one, and 2024 is the ordinary case.
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        // Last second of a year, first of the next.
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
