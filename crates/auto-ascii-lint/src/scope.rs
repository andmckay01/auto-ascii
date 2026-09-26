//! Worktree source scope and relative path selection.

use crate::Language;
use anyhow::{Result, bail};
use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::process::Command;

pub fn relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

pub fn language(path: &str) -> Option<Language> {
    let path = Path::new(path);
    if path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy().to_lowercase();
        name == ".env"
            || name.starts_with(".env.") && name != ".env.example"
            || name.contains("secret")
            || name.starts_with("credentials")
    }) {
        return None;
    }
    match path.file_name()?.to_str()? {
        "Cargo.lock" => return None,
        ".gitignore" => return Some(Language::Gitignore),
        "Makefile" | "GNUmakefile" | "makefile" => return Some(Language::Make),
        _ => {}
    }
    match path.extension()?.to_str()? {
        "rs" => Some(Language::Rust),
        "sh" => Some(Language::Shell),
        "py" => Some(Language::Python),
        "toml" => Some(Language::Toml),
        _ => None,
    }
}

pub fn in_scope(path: &str, prefixes: &[String]) -> bool {
    prefixes.is_empty()
        || prefixes.iter().any(|p| {
            let p = p.trim_end_matches('/');
            path == p
                || path
                    .strip_prefix(p)
                    .is_some_and(|tail| tail.starts_with('/'))
        })
}

pub fn path_metadata(root: &Path, path: &str) -> Result<Option<std::fs::Metadata>> {
    if !relative_path(path) {
        bail!("invalid worktree path: {path}");
    }
    let mut full = root.to_owned();
    let mut metadata = None;
    for component in Path::new(path).components() {
        full.push(component);
        let next = match full.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if next.file_type().is_symlink() {
            bail!(
                "scoped source symlink refused: {path} (component {})",
                full.display()
            );
        }
        metadata = Some(next);
    }
    Ok(metadata)
}

pub fn worktree_files(root: &Path, prefixes: &[String]) -> Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()?;
    if !output.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let names = String::from_utf8(output.stdout)?;
    let mut paths = BTreeSet::new();
    for path in names.split('\0').filter(|p| !p.is_empty()) {
        if language(path).is_some()
            && in_scope(path, prefixes)
            && path_metadata(root, path)?.is_some_and(|metadata| metadata.is_file())
        {
            paths.insert(path.to_owned());
        }
    }
    Ok(paths.into_iter().collect())
}
