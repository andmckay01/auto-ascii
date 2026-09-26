//! Rust comment tokens and literal prose documentation attributes.

use crate::{Comment, Kind, Language, shebang_end};
use anyhow::{Result, bail};
use ra_ap_rustc_lexer::{DocStyle, FrontmatterAllowed, LiteralKind, TokenKind, tokenize};

pub fn extract(source: &str) -> Result<Vec<Comment>> {
    let mut offset = shebang_end(source, Language::Rust);
    let mut comments = Vec::new();
    let mut significant = Vec::new();
    for token in tokenize(&source[offset..], FrontmatterAllowed::No) {
        let start = offset;
        offset += token.len as usize;
        let kind = match token.kind {
            TokenKind::LineComment { doc_style } => Some(match doc_style {
                Some(DocStyle::Inner) => Kind::InnerDoc,
                Some(DocStyle::Outer) => Kind::OuterDoc,
                None => Kind::Line,
            }),
            TokenKind::BlockComment {
                doc_style,
                terminated,
            } => {
                if !terminated {
                    bail!("unterminated Rust block comment at byte {start}");
                }
                Some(match doc_style {
                    Some(DocStyle::Inner) => Kind::InnerBlockDoc,
                    Some(DocStyle::Outer) => Kind::OuterBlockDoc,
                    None => Kind::Block,
                })
            }
            TokenKind::Literal { kind, .. } => {
                if matches!(
                    kind,
                    LiteralKind::Char { terminated: false }
                        | LiteralKind::Byte { terminated: false }
                        | LiteralKind::Str { terminated: false }
                        | LiteralKind::ByteStr { terminated: false }
                        | LiteralKind::CStr { terminated: false }
                        | LiteralKind::RawStr { n_hashes: None }
                        | LiteralKind::RawByteStr { n_hashes: None }
                        | LiteralKind::RawCStr { n_hashes: None }
                ) {
                    bail!("unterminated Rust literal at byte {start}");
                }
                None
            }
            _ => None,
        };
        if let Some(kind) = kind {
            comments.push(Comment::new(source, start, offset, kind));
        } else if token.kind != TokenKind::Whitespace {
            significant.push((token.kind, start, offset));
        }
    }
    for (i, &(kind, start, _)) in significant.iter().enumerate() {
        if kind != TokenKind::Pound {
            continue;
        }
        let mut open = i + 1;
        if significant
            .get(open)
            .is_some_and(|t| t.0 == TokenKind::Bang)
        {
            open += 1;
        }
        if !significant
            .get(open)
            .is_some_and(|t| t.0 == TokenKind::OpenBracket)
        {
            continue;
        }
        let mut depth = 0;
        let mut end = None;
        for (j, t) in significant.iter().enumerate().skip(open) {
            match t.0 {
                TokenKind::OpenBracket => depth += 1,
                TokenKind::CloseBracket => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(j);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            bail!("unterminated Rust attribute at byte {start}");
        };
        if prose_attribute(source, &significant[open + 1..end]) {
            comments.push(Comment::new(
                source,
                start,
                significant[end].2,
                Kind::DocAttribute,
            ));
        }
    }
    Ok(comments)
}

fn prose_attribute(source: &str, tokens: &[(TokenKind, usize, usize)]) -> bool {
    let Some(first) = tokens.first() else {
        return false;
    };
    let name = source[first.1..first.2].trim_start_matches("r#");
    if name == "doc" {
        return tokens.get(1).is_some_and(|t| t.0 == TokenKind::Eq);
    }
    if name != "cfg_attr" || tokens.get(1).is_none_or(|t| t.0 != TokenKind::OpenParen) {
        return false;
    }
    let mut depth = 0;
    let mut argument = None;
    for (i, token) in tokens.iter().enumerate().skip(2) {
        match token.0 {
            TokenKind::Comma | TokenKind::CloseParen if depth == 0 => {
                if let Some(start) = argument
                    && prose_attribute(source, &tokens[start..i])
                {
                    return true;
                }
                argument = Some(i + 1);
            }
            TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket => depth += 1,
            TokenKind::CloseParen | TokenKind::CloseBrace | TokenKind::CloseBracket => depth -= 1,
            _ => {}
        }
    }
    false
}
