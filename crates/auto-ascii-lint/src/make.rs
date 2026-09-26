//! Make directives, literal define bodies and shell recipe comment extraction.

use crate::{Comment, Kind, extract::syntax_comments};
use anyhow::{Result, bail};

fn continued(line: &str) -> bool {
    line.trim_end_matches(['\r', '\n'])
        .bytes()
        .rev()
        .take_while(|&b| b == b'\\')
        .count()
        % 2
        == 1
}

fn hash(line: &str) -> Result<Option<usize>> {
    let bytes = line.as_bytes();
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'#' && !escaped {
            return Ok(Some(i));
        }
        if b == b'$' && i + 1 < bytes.len() {
            if matches!(bytes[i + 1], b'(' | b'{') {
                i = variable_end(bytes, i)?;
            } else {
                i += 2;
            }
            escaped = false;
            continue;
        }
        escaped = b == b'\\' && !escaped;
        i += 1;
    }
    Ok(None)
}

fn variable_end(bytes: &[u8], start: usize) -> Result<usize> {
    let open = bytes[start + 1];
    let close = if open == b'(' { b')' } else { b'}' };
    let mut depth = 1;
    let mut end = start + 2;
    while end < bytes.len() && depth > 0 {
        if bytes[end] == open {
            depth += 1;
        }
        if bytes[end] == close {
            depth -= 1;
        }
        end += 1;
    }
    if depth != 0 {
        bail!("unterminated Make variable reference");
    }
    Ok(end)
}

fn shell(source: &str, start: usize, end: usize, prefix: u8, inline: bool) -> Result<Vec<Comment>> {
    let mut bytes = source.as_bytes()[start..end].to_vec();
    let mut row_start = !inline;
    let mut i = 0;
    while i < bytes.len() {
        if row_start {
            if bytes[i] == prefix {
                bytes[i] = b' ';
            }
            let mut j = i;
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'@' | b'-' | b'+') {
                bytes[j] = b' ';
                j += 1;
            }
            row_start = false;
        }
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'$' {
                bytes[i] = b' ';
                i += 2;
                continue;
            }
            if matches!(bytes[i + 1], b'(' | b'{') {
                let j = variable_end(&bytes, i)?;
                for byte in &mut bytes[i..j] {
                    if *byte != b'\n' {
                        *byte = b'x';
                    }
                }
                i = j;
                continue;
            }
        }
        row_start = bytes[i] == b'\n';
        i += 1;
    }
    let masked = String::from_utf8(bytes)?;
    Ok(syntax_comments(&masked, tree_sitter_bash::LANGUAGE.into())?
        .into_iter()
        .map(|c| Comment::new(source, start + c.start, start + c.end, Kind::Hash))
        .collect())
}

pub fn extract(source: &str) -> Result<Vec<Comment>> {
    let lines: Vec<_> = source.split_inclusive('\n').collect();
    let mut offsets = vec![0];
    for line in &lines {
        offsets.push(offsets.last().unwrap() + line.len());
    }
    let mut prefix = b'\t';
    let mut defines = 0;
    let mut oneshell = false;
    let mut found = Vec::new();
    let mut row = 0;
    while row < lines.len() {
        let line = lines[row];
        if defines > 0 {
            let trimmed = line.trim();
            if trimmed.starts_with("define ") {
                defines += 1;
            }
            if trimmed == "endef"
                || trimmed.strip_prefix("endef").is_some_and(|tail| {
                    tail.starts_with(char::is_whitespace) && tail.trim_start().starts_with('#')
                })
            {
                defines -= 1;
                if defines == 0
                    && let Some(at) = hash(line)?
                {
                    found.push(Comment::new(
                        source,
                        offsets[row] + at,
                        offsets[row + 1],
                        Kind::Hash,
                    ));
                }
            }
            row += 1;
            continue;
        }
        if line.as_bytes().first() == Some(&prefix) {
            let start = row;
            row += 1;
            while row < lines.len()
                && (continued(lines[row - 1])
                    || oneshell && lines[row].as_bytes().first() == Some(&prefix))
            {
                row += 1;
            }
            found.extend(shell(source, offsets[start], offsets[row], prefix, false)?);
            continue;
        }
        let start = row;
        row += 1;
        while row < lines.len() && continued(lines[row - 1]) {
            row += 1;
        }
        let logical = &source[offsets[start]..offsets[row]];
        let comment_at = hash(logical)?;
        let code = &logical[..comment_at.unwrap_or(logical.len())];
        let inline = code.find(':').and_then(|colon| {
            (!code[..colon].contains('='))
                .then(|| code[colon + 1..].find(';').map(|semi| colon + 2 + semi))
                .flatten()
        });
        if let Some(inline) = inline {
            found.extend(shell(
                source,
                offsets[start] + inline,
                offsets[row],
                prefix,
                true,
            )?);
        } else if let Some(at) = comment_at {
            found.push(Comment::new(
                source,
                offsets[start] + at,
                offsets[row],
                Kind::Hash,
            ));
        }
        let code = code.trim();
        if code == ".ONESHELL:" {
            oneshell = true;
        }
        if code.starts_with(".RECIPEPREFIX") {
            let (_, value) = code
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("unsupported .RECIPEPREFIX directive"))?;
            let value = value.trim();
            if value.contains(['$', '\\']) || !value.is_ascii() {
                bail!(".RECIPEPREFIX must have a literal ASCII value");
            }
            prefix = value.bytes().next().unwrap_or(b'\t');
        }
        let directive = code
            .strip_prefix("override ")
            .or_else(|| code.strip_prefix("export "))
            .unwrap_or(code);
        if directive.starts_with("define ") {
            defines = 1;
        }
    }
    if defines != 0 {
        bail!("unterminated Make define body");
    }
    Ok(found)
}
