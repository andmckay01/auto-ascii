//! The home folder and the naming rules around it (PLAN-M6-M8 §0.2, §2).
//!
//! `~/auto-ascii/` — override `AUTO_ASCII_HOME` — with three visible
//! subfolders created on demand: `library/` (processed clips + provenance
//! sidecars), `compositions/` (M8 timelines) and `exports/` (flattened
//! compositions). Visible and discoverable beats a dotfile cache: an agent
//! can `ls` it and a human can find what the agent made.

use std::io;
use std::path::{Path, PathBuf};

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
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
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
        let in_library = self.clip_path(spec);
        if in_library.is_file() {
            return Ok(in_library);
        }
        let kebab = kebab_case(spec);
        if !kebab.is_empty() && kebab != spec {
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
}

/// The sidecar that belongs to an asset: the same path with a `.json`
/// extension, so it works for library clips and loose paths alike.
pub fn sidecar_path(asset: &Path) -> PathBuf {
    asset.with_extension("json")
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
