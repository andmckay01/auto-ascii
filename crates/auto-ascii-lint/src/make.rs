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

fn directive(code: &str) -> Option<(&str, &str)> {
    let (keyword, tail) = code.split_once(char::is_whitespace).unwrap_or((code, ""));
    matches!(
        keyword,
        "define"
            | "endef"
            | "ifdef"
            | "ifndef"
            | "ifeq"
            | "ifneq"
            | "else"
            | "endif"
            | "include"
            | "-include"
            | "sinclude"
            | "override"
            | "export"
    )
    .then_some((keyword, tail.trim_start()))
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

fn inline_recipe(code: &str) -> Result<Option<usize>> {
    let bytes = code.as_bytes();
    let mut target = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'$' if i + 1 < bytes.len() => {
                i = if matches!(bytes[i + 1], b'(' | b'{') {
                    variable_end(bytes, i)?
                } else {
                    i + 2
                };
            }
            b'=' => return Ok(None),
            b';' => return Ok(target.then_some(i + 1)),
            b':' => {
                target = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    Ok(None)
}

fn recipe_end(lines: &[&str], mut row: usize, prefix: u8, oneshell: bool) -> usize {
    while row < lines.len()
        && (continued(lines[row - 1])
            || oneshell
                && (lines[row].as_bytes().first() == Some(&prefix)
                    || lines[row].trim().is_empty()
                    || lines[row].trim_start().starts_with('#')))
    {
        row += 1;
    }
    row
}

fn shell(source: &str, start: usize, end: usize, prefix: u8, inline: bool) -> Result<Vec<Comment>> {
    let mut bytes = source.as_bytes()[start..end].to_vec();
    let mut found = Vec::new();
    let mut offset = 0;
    let mut continuation = false;
    let mut make_comment = None;
    for (row, line) in source[start..end].split_inclusive('\n').enumerate() {
        if make_comment.is_none()
            && !(continuation || row == 0 && inline)
            && line.as_bytes().first() != Some(&prefix)
            && line.trim_start().starts_with('#')
        {
            make_comment = Some(offset + line.find('#').unwrap());
        }
        continuation = continued(line);
        if let Some(at) = make_comment {
            for byte in &mut bytes[offset..offset + line.len()] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
            if !continuation || offset + line.len() == bytes.len() {
                found.push(Comment::new(
                    source,
                    start + at,
                    start + offset + line.len(),
                    Kind::Hash,
                ));
                make_comment = None;
            }
        }
        offset += line.len();
    }
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
    found.extend(
        syntax_comments(&masked, tree_sitter_bash::LANGUAGE.into())?
            .into_iter()
            .map(|c| Comment::new(source, start + c.start, start + c.end, Kind::Hash)),
    );
    Ok(found)
}

pub fn extract(source: &str) -> Result<Vec<Comment>> {
    let (_, oneshell) = extract_mode(source, false, false)?;
    Ok(extract_mode(source, oneshell, true)?.0)
}

fn extract_mode(source: &str, oneshell: bool, parse_recipes: bool) -> Result<(Vec<Comment>, bool)> {
    let lines: Vec<_> = source.split_inclusive('\n').collect();
    let mut offsets = vec![0];
    for line in &lines {
        offsets.push(offsets.last().unwrap() + line.len());
    }
    let mut prefix = b'\t';
    let mut defines = 0;
    let mut has_oneshell = oneshell;
    let mut found = Vec::new();
    let mut row = 0;
    while row < lines.len() {
        let line = lines[row];
        if defines > 0 {
            if line.as_bytes().first() == Some(&prefix) {
                row += 1;
                continue;
            }
            let trimmed = line.trim();
            let token = directive(trimmed);
            if matches!(token, Some(("define", _))) {
                defines += 1;
            }
            if token.is_some_and(|(keyword, tail)| {
                keyword == "endef" && (tail.is_empty() || tail.starts_with('#'))
            }) {
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
            row = recipe_end(&lines, row + 1, prefix, oneshell);
            if parse_recipes {
                found.extend(shell(source, offsets[start], offsets[row], prefix, false)?);
            }
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
        if directive(code.trim()).is_none()
            && let Some(inline) = inline_recipe(code)?
        {
            row = recipe_end(&lines, row, prefix, oneshell);
            if parse_recipes {
                found.extend(shell(
                    source,
                    offsets[start] + inline,
                    offsets[row],
                    prefix,
                    true,
                )?);
            }
        } else if let Some(at) = comment_at {
            found.push(Comment::new(
                source,
                offsets[start] + at,
                offsets[row],
                Kind::Hash,
            ));
        }
        let code = code.trim();
        if code
            .split_once(':')
            .is_some_and(|(target, tail)| target.trim() == ".ONESHELL" && tail.trim().is_empty())
        {
            has_oneshell = true;
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
        let mut token = directive(code);
        while let Some(("override" | "export", tail)) = token {
            token = directive(tail);
        }
        if matches!(token, Some(("define", _))) {
            defines = 1;
        }
    }
    if defines != 0 {
        bail!("unterminated Make define body");
    }
    Ok((found, has_oneshell))
}
