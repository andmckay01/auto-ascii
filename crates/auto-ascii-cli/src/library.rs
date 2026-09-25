use std::path::{Path, PathBuf};

use auto_ascii_format::AsciiReader;
use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::BoxErr;
use crate::home::{Home, sidecar_path};

#[derive(Clone, Debug, Serialize)]
pub struct Sidecar {
    pub name: String,
    pub source: Option<Source>,
    pub asset: Option<AssetInfo>,
    pub created_unix: Option<u64>,
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const UNKNOWN: &str = "(unknown)";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RawSource", untagged)]
pub enum Source {
    Video {
        path: String,
        sha256: Option<String>,
        bytes: Option<u64>,
    },
    Cut {
        kind: String,
        from: Option<String>,
        #[serde(rename = "in")]
        in_secs: Option<f64>,
        #[serde(rename = "out")]
        out_secs: Option<f64>,
    },
}

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
    pub fn cut(from: String, in_secs: f64, out_secs: f64) -> Source {
        Source::Cut {
            kind: "cut".to_string(),
            from: Some(from),
            in_secs: Some(in_secs),
            out_secs: Some(out_secs),
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Source::Video { path, .. } => path.clone(),
            Source::Cut { from, in_secs, out_secs, .. } => {
                let from = from.as_deref().unwrap_or(UNKNOWN);
                match (in_secs, out_secs) {
                    (Some(start), Some(end)) => {
                        format!("cut of {from} [{start:.2}s, {end:.2}s)")
                    }
                    _ => format!("cut of {from} (slice {UNKNOWN})"),
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AssetInfo {
    pub path: String,
    pub bytes: u64,
    pub frames: u32,
    pub fps: f64,
    pub duration_secs: f64,
    pub base_w: u16,
    pub base_h: u16,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Provenance {
    pub source: Option<Source>,
    pub created_unix: Option<u64>,
    pub created: Option<String>,
}

pub fn asset_info(path: &Path) -> Result<AssetInfo, BoxErr> {
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

pub fn read_provenance(asset: &Path) -> Result<Provenance, BoxErr> {
    let path = sidecar_path(asset);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} is not a valid sidecar: {e}", path.display()).into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Provenance::default()),
        Err(e) => Err(format!("read {}: {e}", path.display()).into()),
    }
}

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

pub fn describe_lenient(name: &str, asset: &Path) -> Sidecar {
    let (info, mut error) = match asset_info(asset) {
        Ok(info) => (Some(info), None),
        Err(e) => (None, Some(e.to_string())),
    };
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

pub fn list(home: &Home) -> Result<Vec<Sidecar>, BoxErr> {
    let dir = home.library();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", dir.display()).into()),
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
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

pub fn write_sidecar(asset: &Path, sidecar: &Sidecar) -> Result<PathBuf, BoxErr> {
    let path = sidecar_path(asset);
    let mut json = serde_json::to_string_pretty(sidecar)?;
    json.push('\n');
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

pub fn remove_sidecar(asset: &Path) -> Result<(), BoxErr> {
    let path = sidecar_path(asset);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove {}: {e}", path.display()).into()),
    }
}

pub fn clip_ref(home: &Home, path: &Path) -> String {
    let in_library =
        path.extension().is_some_and(|e| e == "ascii") && same_dir(path.parent(), &home.library());
    if in_library { crate::home::stem_of(path) } else { absolute(path) }
}

fn same_dir(dir: Option<&Path>, other: &Path) -> bool {
    match dir {
        None => false,
        Some(d) => match (std::fs::canonicalize(d), std::fs::canonicalize(other)) {
            (Ok(a), Ok(b)) => a == b,
            _ => d == other,
        },
    }
}

pub fn absolute(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
