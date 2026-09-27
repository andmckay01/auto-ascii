//! Source comment extraction and front matter enforcement.

mod extract;
mod make;
mod rust;
mod scope;

pub use scope::{in_scope, language, path_metadata, worktree_files};

use anyhow::{Result, bail};
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    Shell,
    Python,
    Toml,
    Make,
    Gitignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Line,
    InnerDoc,
    OuterDoc,
    Block,
    InnerBlockDoc,
    OuterBlockDoc,
    DocAttribute,
    Hash,
    Docstring,
}

impl Kind {
    fn clusters(self) -> bool {
        matches!(
            self,
            Self::Line | Self::InnerDoc | Self::OuterDoc | Self::Hash
        )
    }
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub end_line: usize,
    pub kind: Kind,
    pub standalone: bool,
}

impl Comment {
    pub fn new(source: &str, start: usize, end: usize, kind: Kind) -> Self {
        let end = source[..end].trim_end_matches(['\r', '\n']).len();
        let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
        Self {
            start,
            end,
            line: source[..start].bytes().filter(|&b| b == b'\n').count() + 1,
            end_line: source[..end].bytes().filter(|&b| b == b'\n').count() + 1,
            kind,
            standalone: source[line_start..start]
                .trim_start_matches('\u{feff}')
                .trim()
                .is_empty(),
        }
    }
}

pub fn scan(source: &str, lang: Language) -> Result<Vec<Comment>> {
    let mut atoms = extract::extract(source, lang)?;
    atoms.sort_by_key(|c| c.start);
    let mut clusters: Vec<Comment> = Vec::new();
    for c in atoms {
        if let Some(prev) = clusters.last_mut()
            && prev.kind == c.kind
            && c.kind.clusters()
            && prev.standalone
            && c.standalone
            && prev.end_line + 1 == c.line
            && source[prev.end..c.start].trim().is_empty()
        {
            prev.end = c.end;
            prev.end_line = c.end_line;
        } else {
            clusters.push(c);
        }
    }
    Ok(clusters)
}

pub fn shebang_end(source: &str, lang: Language) -> usize {
    let bom = source.len() - source.trim_start_matches('\u{feff}').len();
    let text = &source[bom..];
    if matches!(lang, Language::Rust | Language::Shell | Language::Python)
        && text
            .strip_prefix("#!")
            .is_some_and(|tail| tail.trim_start_matches([' ', '\t']).starts_with('/'))
    {
        bom + text.find('\n').unwrap_or(text.len())
    } else {
        bom
    }
}

pub fn violation(source: &str, c: &Comment, lang: Language) -> Option<String> {
    let header_kind = if lang == Language::Rust {
        Kind::InnerDoc
    } else {
        Kind::Hash
    };
    if c.kind != header_kind {
        return Some(
            match c.kind {
                Kind::DocAttribute => "prose doc attribute is forbidden",
                Kind::Docstring => "Python docstring is forbidden",
                _ => "only leading front matter is allowed",
            }
            .into(),
        );
    }
    let preamble = shebang_end(source, lang).min(c.start);
    if !source[preamble..c.start].trim().is_empty() {
        return Some(
            if c.standalone {
                "comment outside the leading file header"
            } else {
                "trailing comment"
            }
            .into(),
        );
    }
    let marker = if lang == Language::Rust { "//!" } else { "#" };
    let lines: Vec<_> = source[c.start..c.end]
        .lines()
        .map(|line| line.trim().strip_prefix(marker).unwrap_or(line).trim())
        .collect();
    let first = lines.iter().position(|l| !l.is_empty())?;
    let last = lines.iter().rposition(|l| !l.is_empty()).unwrap();
    let body = &lines[first..=last];
    if body.iter().any(|l| l.is_empty()) {
        return Some("file header has more than one paragraph".into());
    }
    if body.len() > 5 {
        return Some(format!(
            "file header has {} text lines; maximum is 5",
            body.len()
        ));
    }
    None
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowlist {
    pub entries: Vec<Exemption>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exemption {
    pub path: String,
    pub comment: String,
    pub reason: String,
}

impl Allowlist {
    pub fn parse(source: &str) -> Result<Self> {
        let list: Self = toml::from_str(source)?;
        let mut seen = BTreeSet::new();
        for entry in &list.entries {
            if entry.reason.trim().is_empty()
                || entry.comment.trim().is_empty()
                || !scope::relative_path(&entry.path)
                || language(&entry.path).is_none()
            {
                bail!(
                    "allowlist entries need a scoped relative path, exact comment and nonempty reason"
                );
            }
            if !seen.insert((&entry.path, &entry.comment)) {
                bail!("duplicate allowlist entry: {}", entry.path);
            }
        }
        Ok(list)
    }

    pub fn exempt(&self, path: &str, comment: &str, used: &mut BTreeSet<usize>) -> bool {
        if let Some(index) = self
            .entries
            .iter()
            .position(|e| e.path == path && e.comment == comment)
        {
            used.insert(index);
            true
        } else {
            false
        }
    }

    pub fn stale(&self, used: &BTreeSet<usize>, prefixes: &[String]) -> Vec<String> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(i, e)| in_scope(&e.path, prefixes) && !used.contains(i))
            .map(|(_, e)| format!("{}:1: stale allowlist entry: {}", e.path, e.reason))
            .collect()
    }
}
