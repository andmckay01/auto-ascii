//! The library folder's data model (PLAN-M6-M8 §2).
//!
//! One JSON shape serves `import`, `cut`, `info` and each element of
//! `list`: the **sidecar**. Writers fill it in; the readers rebuild the
//! `asset` block
//! from the ASCI header on disk (authoritative — the file is what plays)
//! and merge whatever provenance the sidecar remembers. A clip with no
//! sidecar is still a clip, so `source`/`created_unix`/`created` are
//! nullable and that case is `null`, never a missing key.
//!
//! Reads are deliberately forgiving in three directions. Sidecars are
//! parsed through [`Provenance`], which wants nothing but the fields it
//! uses, so a hand-written `{"source": {...}}` loads. Inside `source`
//! only a video's `path` is required — `{"source": {"path": "/a.mp4"}}`
//! describes a clip partially rather than failing, and what is missing
//! reads back as `null`/`(unknown)`. And [`list`] never aborts on one bad
//! entry: a truncated file, a directory, a name that is not UTF-8 or an
//! unparseable sidecar becomes an entry carrying an `error` string, and the
//! rest of the library still lists. `info` and `play`, which name ONE clip,
//! stay strict — there the failure is the answer.

use std::path::{Path, PathBuf};

use auto_ascii_format::AsciiReader;
use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::BoxErr;
use crate::home::{Home, sidecar_path};

/// `library/<name>.json` — one clip's provenance, and the object every
/// `--json` command prints.
#[derive(Clone, Debug, Serialize)]
pub struct Sidecar {
    /// Library name: the asset's file stem, verbatim.
    pub name: String,
    /// Where the clip came from; `null` when no sidecar was found.
    pub source: Option<Source>,
    /// What is in the `.ascii` file, read from its header rather than
    /// cached; `null` when the file could not be read (see `error`).
    pub asset: Option<AssetInfo>,
    /// Import time, seconds since the Unix epoch; `null` without a sidecar.
    pub created_unix: Option<u64>,
    /// The same instant as RFC 3339 UTC; `null` without a sidecar.
    pub created: Option<String>,
    /// Why this entry is incomplete. Absent — not `null` — when the clip
    /// read cleanly, which is every entry `import` ever writes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What a reader prints where a sidecar said nothing.
pub const UNKNOWN: &str = "(unknown)";

/// Where a clip came from: the video `import` ingested, or the slice
/// `cut` took out of another clip (PLAN-M6-M8 §3).
///
/// `kind` is what tells the two apart, and only a cut carries one — a
/// video source has none and never did, so every sidecar `import` has
/// ever written still loads. Everything except a video's `path` is
/// OPTIONAL on the way in: a sidecar an agent wrote by hand, or one a
/// future version trimmed, describes a clip PARTIALLY rather than not at
/// all, and the missing fields read back as `null` (JSON) or
/// `(unknown)` (the human tables).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RawSource", untagged)]
pub enum Source {
    /// `import`: the source video.
    Video {
        /// Absolute path at import time (the file may since have moved).
        path: String,
        /// SHA-256 of the source bytes — the honest "is this the same video".
        sha256: Option<String>,
        /// Source file size in bytes.
        bytes: Option<u64>,
    },
    /// `cut`: the slice, and the clip it came out of.
    Cut {
        /// Always `"cut"`.
        kind: String,
        /// The clip this is a slice of: its library name, or its path when
        /// it lives outside `library/` (see [`clip_ref`]).
        from: Option<String>,
        /// Slice start inside that clip, seconds.
        #[serde(rename = "in")]
        in_secs: Option<f64>,
        /// Slice end inside that clip, seconds (exclusive).
        #[serde(rename = "out")]
        out_secs: Option<f64>,
    },
}

/// A `source` object as it comes off disk: every key optional (serde
/// treats an absent `Option` field as `None`), unknown keys ignored.
/// [`Source`] is the same object with the rules applied.
#[derive(Deserialize)]
struct RawSource {
    kind: Option<String>,
    path: Option<String>,
    sha256: Option<String>,
    bytes: Option<u64>,
    from: Option<String>,
    #[serde(rename = "in")]
    in_secs: Option<f64>,
    #[serde(rename = "out")]
    out_secs: Option<f64>,
}

impl TryFrom<RawSource> for Source {
    type Error = String;

    /// The one rule: `"kind": "cut"` makes it a cut, anything else is a
    /// video source and needs a `path` to be about anything at all.
    fn try_from(raw: RawSource) -> Result<Source, String> {
        if raw.kind.as_deref() == Some("cut") {
            return Ok(Source::Cut {
                kind: "cut".to_string(),
                from: raw.from,
                in_secs: raw.in_secs,
                out_secs: raw.out_secs,
            });
        }
        match raw.path {
            Some(path) => Ok(Source::Video { path, sha256: raw.sha256, bytes: raw.bytes }),
            None => Err(format!(
                "source needs a \"path\" (a video) or \"kind\": \"cut\"{}",
                match raw.kind {
                    Some(kind) => format!(" — this one says \"kind\": {kind:?}"),
                    None => String::new(),
                }
            )),
        }
    }
}

impl Source {
    /// A cut's provenance, with the `kind` discriminator set in one place.
    pub fn cut(from: String, in_secs: f64, out_secs: f64) -> Source {
        Source::Cut {
            kind: "cut".to_string(),
            from: Some(from),
            in_secs: Some(in_secs),
            out_secs: Some(out_secs),
        }
    }

    /// The one line `list` puts in its last column.
    pub fn summary(&self) -> String {
        match self {
            Source::Video { path, .. } => path.clone(),
            Source::Cut { from, in_secs, out_secs, .. } => {
                let from = from.as_deref().unwrap_or(UNKNOWN);
                match (in_secs, out_secs) {
                    (Some(start), Some(end)) => {
                        format!("cut of {from} [{start:.2}s, {end:.2}s)")
                    }
                    // A cut that forgot its own slice is still a cut.
                    _ => format!("cut of {from} (slice {UNKNOWN})"),
                }
            }
        }
    }
}

/// An ASCI asset's header facts, plus where it is and how big it is.
#[derive(Clone, Debug, Serialize)]
pub struct AssetInfo {
    /// Absolute path to the `.ascii` file.
    pub path: String,
    /// File size in bytes.
    pub bytes: u64,
    /// Frame count from the header.
    pub frames: u32,
    /// `fps_num / fps_den`.
    pub fps: f64,
    /// `frames / fps`.
    pub duration_secs: f64,
    /// Stored plane width.
    pub base_w: u16,
    /// Stored plane height.
    pub base_h: u16,
}

/// The only part of a sidecar we read back. Every field is optional and
/// unknown keys are ignored, so a hand-written file carrying just a
/// `source` block loads, and a sidecar written by a future version that
/// added fields still loads here.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Provenance {
    /// See [`Sidecar::source`].
    pub source: Option<Source>,
    /// See [`Sidecar::created_unix`].
    pub created_unix: Option<u64>,
    /// See [`Sidecar::created`].
    pub created: Option<String>,
}

/// Read an asset's header (mmap + [`AsciiReader`] — no decode, no copy).
pub fn asset_info(path: &Path) -> Result<AssetInfo, BoxErr> {
    // Checked explicitly so a directory named `x.ascii` fails as itself
    // rather than as whatever mmap happens to say about a directory fd.
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()).into());
    }
    let file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let bytes = file.metadata().map_err(|e| format!("stat {}: {e}", path.display()))?.len();
    // Safety: read-only private map of a file nothing mutates while we hold
    // it — the same contract the player's mmap assumes.
    let mmap = unsafe { Mmap::map(&file) }.map_err(|e| format!("mmap {}: {e}", path.display()))?;
    let reader = AsciiReader::open(&mmap)
        .map_err(|e| format!("{} is not a valid ASCI asset: {e}", path.display()))?;
    let h = reader.header();
    let fps = if h.fps_den == 0 { 0.0 } else { f64::from(h.fps_num) / f64::from(h.fps_den) };
    Ok(AssetInfo {
        path: absolute(path),
        bytes,
        frames: h.frame_count,
        fps,
        duration_secs: if fps > 0.0 { f64::from(h.frame_count) / fps } else { 0.0 },
        base_w: h.base_w,
        base_h: h.base_h,
    })
}

/// Provenance from `<asset>.json`. A missing sidecar is not an error (the
/// asset is still a clip); an unparseable one is.
pub fn read_provenance(asset: &Path) -> Result<Provenance, BoxErr> {
    let path = sidecar_path(asset);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} is not a valid sidecar: {e}", path.display()).into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Provenance::default()),
        Err(e) => Err(format!("read {}: {e}", path.display()).into()),
    }
}

/// Header facts + provenance for one named clip, strictly: anything wrong
/// with either file is the answer. `info` and `play` use this.
pub fn describe(name: &str, asset: &Path) -> Result<Sidecar, BoxErr> {
    let info = asset_info(asset)?;
    let provenance = read_provenance(asset)?;
    Ok(Sidecar {
        name: name.to_string(),
        source: provenance.source,
        asset: Some(info),
        created_unix: provenance.created_unix,
        created: provenance.created,
        error: None,
    })
}

/// The same, but total: whatever could be read is filled in and whatever
/// could not becomes `error`. [`list`] uses this so one broken file cannot
/// hide the rest of the library.
pub fn describe_lenient(name: &str, asset: &Path) -> Sidecar {
    let (info, mut error) = match asset_info(asset) {
        Ok(info) => (Some(info), None),
        Err(e) => (None, Some(e.to_string())),
    };
    // The asset's own failure is the more useful one, so it wins the slot.
    let provenance = match read_provenance(asset) {
        Ok(p) => p,
        Err(e) => {
            error.get_or_insert_with(|| e.to_string());
            Provenance::default()
        }
    };
    Sidecar {
        name: name.to_string(),
        source: provenance.source,
        asset: info,
        created_unix: provenance.created_unix,
        created: provenance.created,
        error,
    }
}

/// Every `library/*.ascii`, sorted by file name. Never fails on the
/// contents: only an unreadable library DIRECTORY is an error, and a
/// missing one is simply an empty library.
pub fn list(home: &Home) -> Result<Vec<Sidecar>, BoxErr> {
    let dir = home.library();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // Nothing imported yet is an empty library, not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", dir.display()).into()),
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        // A single unreadable directory entry carries no path to report
        // against, so skip it rather than sink the listing.
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "ascii") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths
        .iter()
        .map(|path| {
            let stem = path.file_stem().unwrap_or_default();
            match stem.to_str() {
                Some(name) => describe_lenient(name, path),
                // A name we cannot spell is a name nothing else can look
                // up, so say so instead of listing an unusable key.
                None => Sidecar {
                    name: stem.to_string_lossy().into_owned(),
                    source: None,
                    asset: None,
                    created_unix: None,
                    created: None,
                    error: Some("clip name is not valid UTF-8; rename the file".into()),
                },
            }
        })
        .collect())
}

/// Write `sidecar` to `library/<name>.json`, pretty-printed (an agent may
/// well read it with `cat`) and newline-terminated.
pub fn write_sidecar(asset: &Path, sidecar: &Sidecar) -> Result<PathBuf, BoxErr> {
    let path = sidecar_path(asset);
    let mut json = serde_json::to_string_pretty(sidecar)?;
    json.push('\n');
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Remove `library/<name>.json` if it is there. Missing is success.
pub fn remove_sidecar(asset: &Path) -> Result<(), BoxErr> {
    let path = sidecar_path(asset);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove {}: {e}", path.display()).into()),
    }
}

/// How a resolved clip is REFERRED TO from a composition's `asset` or a
/// cut's provenance: its library name when it is `library/<name>.ascii`,
/// else its absolute path (PLAN-M6-M8 §3).
///
/// The two forms are the two halves of the schema's own rule — an `asset`
/// resolves as a path first and as `<library>/<asset>.ascii` second — so a
/// name written here always reads back as the same file, wherever the
/// composition lives.
pub fn clip_ref(home: &Home, path: &Path) -> String {
    let in_library =
        path.extension().is_some_and(|e| e == "ascii") && same_dir(path.parent(), &home.library());
    if in_library { crate::home::stem_of(path) } else { absolute(path) }
}

/// Whether `dir` is the folder `other` names, through symlinks when both
/// exist — a temp `AUTO_ASCII_HOME` is a symlinked path on macOS, and a
/// textual comparison would say no.
fn same_dir(dir: Option<&Path>, other: &Path) -> bool {
    match dir {
        None => false,
        Some(d) => match (std::fs::canonicalize(d), std::fs::canonicalize(other)) {
            (Ok(a), Ok(b)) => a == b,
            _ => d == other,
        },
    }
}

/// Absolute display path, falling back to what we were given when the file
/// cannot be canonicalized (it may not exist yet).
pub fn absolute(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
