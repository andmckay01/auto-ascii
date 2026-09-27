//! Language extraction for shell, TOML, Python and ignore patterns.

use crate::{Comment, Kind, Language, shebang_end};
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::process::{Command, Stdio};

pub fn extract(source: &str, lang: Language) -> Result<Vec<Comment>> {
    match lang {
        Language::Rust => crate::rust::extract(source),
        Language::Shell => Ok(syntax_comments(source, tree_sitter_bash::LANGUAGE.into())?
            .into_iter()
            .filter(|c| c.start >= shebang_end(source, lang))
            .collect()),
        Language::Toml => syntax_comments(source, tree_sitter_toml_ng::LANGUAGE.into()),
        Language::Python => python(source),
        Language::Make => crate::make::extract(source),
        Language::Gitignore => {
            let mut offset = 0;
            let mut found = Vec::new();
            for line in source.split_inclusive('\n') {
                let bom = if offset == 0 {
                    line.len() - line.trim_start_matches('\u{feff}').len()
                } else {
                    0
                };
                if line[bom..].starts_with('#') {
                    found.push(Comment::new(
                        source,
                        offset + bom,
                        offset + line.len(),
                        Kind::Hash,
                    ));
                }
                offset += line.len();
            }
            Ok(found)
        }
    }
}

pub fn syntax_comments(source: &str, language: tree_sitter::Language) -> Result<Vec<Comment>> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language)?;
    let bom = source.len() - source.trim_start_matches('\u{feff}').len();
    let tree = parser
        .parse(&source[bom..], None)
        .context("parser returned no tree")?;
    let mut found = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            bail!(
                "syntax extraction error at line {} ({})",
                node.start_position().row + 1,
                node.kind()
            );
        }
        let comment = node.kind() == "comment";
        if comment {
            found.push(Comment::new(
                source,
                node.start_byte() + bom,
                node.end_byte() + bom,
                Kind::Hash,
            ));
        }
        if !comment && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(found);
            }
        }
    }
}

fn python(source: &str) -> Result<Vec<Comment>> {
    let source_without_bom = source.trim_start_matches('\u{feff}');
    let bom = source.len() - source_without_bom.len();
    let mut child = Command::new("python3")
        .args(["-I", "-B", "-c", include_str!("python.py")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Python extraction requires python3 on PATH")?;
    child
        .stdin
        .take()
        .context("Python stdin missing")?
        .write_all(source_without_bom.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "Python extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let atoms: Vec<(usize, usize, bool)> = serde_json::from_slice(&output.stdout)?;
    Ok(atoms
        .into_iter()
        .filter(|(start, _, _)| start + bom >= shebang_end(source, Language::Python))
        .map(|(start, end, doc)| {
            Comment::new(
                source,
                start + bom,
                end + bom,
                if doc { Kind::Docstring } else { Kind::Hash },
            )
        })
        .collect())
}
