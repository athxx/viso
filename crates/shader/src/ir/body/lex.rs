//! Tokenizer for the built-in shading subset.
//!
//! Comments are tokens (the codegens carry them into every target), and every
//! token records whether a blank line separated it from the previous one, so a
//! printer can reproduce the source's statement grouping.

use crate::ir::body::BodyError;

/// A byte range plus the 1-based line/column of its first byte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the first character.
    pub start: u32,
    /// Byte offset one past the last character.
    pub end: u32,
    /// 1-based line.
    pub line: u32,
    /// 1-based column (in bytes).
    pub col: u32,
}

/// What a token is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// An identifier or keyword.
    Ident(String),
    /// A floating-point literal, verbatim (`0.5`, `1e-4`).
    Float(String),
    /// A signed integer literal, verbatim (`0`, `15`).
    Int(String),
    /// An unsigned integer literal with its `u` suffix, verbatim (`0u`,
    /// `0x9E3779B9u`).
    Uint(String),
    /// Punctuation or an operator.
    Punct(&'static str),
    /// A `//` line comment; the text after the slashes, one leading space
    /// removed.
    Comment(String),
    /// End of input.
    Eof,
}

/// One token with its location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The token.
    pub kind: TokenKind,
    /// Where it is.
    pub span: Span,
    /// At least one empty line precedes this token.
    pub blank_before: bool,
}

/// Longest-match-first operator table.
const PUNCT: &[&str] = &[
    "<<=", ">>=", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "<=",
    ">=", "==", "!=", "&&", "||", "(", ")", "{", "}", "[", "]", ";", ",", ".", "?", ":", "+", "-",
    "*", "/", "%", "&", "|", "^", "!", "~", "<", ">", "=",
];

/// Tokenize `src`. The final token is always [`TokenKind::Eof`].
pub fn lex(src: &str) -> Result<Vec<Token>, BodyError> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line = 1u32;
    let mut line_start = 0usize;
    let mut newlines = 0u32;

    loop {
        // Whitespace, counting newlines for blank-line detection.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            if bytes[i] == b'\n' {
                newlines += 1;
                line += 1;
                line_start = i + 1;
            }
            i += 1;
        }
        let blank_before = newlines >= 2;
        newlines = 0;
        let start = i;
        let span_at = |end: usize| Span {
            start: start as u32,
            end: end as u32,
            line,
            col: (start - line_start + 1) as u32,
        };
        if i >= bytes.len() {
            out.push(Token {
                kind: TokenKind::Eof,
                span: span_at(i),
                blank_before,
            });
            return Ok(out);
        }
        let c = bytes[i];
        let kind = if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            let text = &src[i + 2..j];
            i = j;
            TokenKind::Comment(text.strip_prefix(' ').unwrap_or(text).to_string())
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            return Err(BodyError::new(
                "S0101",
                span_at(i + 2),
                "block comments are not part of the shading subset; use `//`",
            ));
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            let text = &src[i..j];
            i = j;
            TokenKind::Ident(text.to_string())
        } else if c.is_ascii_digit()
            || (c == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            let (kind, end) =
                lex_number(src, i).map_err(|m| BodyError::new("S0102", span_at(i + 1), m))?;
            i = end;
            kind
        } else if let Some(p) = PUNCT.iter().find(|p| src[i..].starts_with(**p)) {
            i += p.len();
            TokenKind::Punct(p)
        } else {
            let ch = src[i..].chars().next().unwrap_or('?');
            return Err(BodyError::new(
                "S0103",
                span_at(i + ch.len_utf8()),
                format!("unexpected character `{ch}`"),
            ));
        };
        out.push(Token {
            kind,
            span: span_at(i),
            blank_before,
        });
    }
}

/// Lex one numeric literal starting at `i`, returning it and the end offset.
fn lex_number(src: &str, i: usize) -> Result<(TokenKind, usize), String> {
    let bytes = src.as_bytes();
    let mut j = i;
    if bytes[j] == b'0' && matches!(bytes.get(j + 1), Some(b'x' | b'X')) {
        j += 2;
        let digits = j;
        while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
            j += 1;
        }
        if j == digits {
            return Err("hex literal has no digits".into());
        }
        return if bytes.get(j) == Some(&b'u') {
            Ok((TokenKind::Uint(src[i..j + 1].to_string()), j + 1))
        } else {
            Ok((TokenKind::Int(src[i..j].to_string()), j))
        };
    }
    let mut is_float = false;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }
    if bytes.get(j) == Some(&b'.') {
        is_float = true;
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
    }
    if matches!(bytes.get(j), Some(b'e' | b'E')) {
        is_float = true;
        j += 1;
        if matches!(bytes.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let digits = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == digits {
            return Err("exponent has no digits".into());
        }
    }
    match bytes.get(j) {
        Some(b'f' | b'h') => Err("literal suffixes other than `u` are not portable".into()),
        Some(b'u') if !is_float => Ok((TokenKind::Uint(src[i..j + 1].to_string()), j + 1)),
        Some(c) if c.is_ascii_alphanumeric() || *c == b'_' => {
            Err("malformed numeric literal".into())
        }
        _ if is_float => Ok((TokenKind::Float(src[i..j].to_string()), j)),
        _ => Ok((TokenKind::Int(src[i..j].to_string()), j)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn literals_keep_their_spelling() {
        assert_eq!(
            kinds("0.5 1e-4 15 0u 0x9E3779B9u"),
            vec![
                TokenKind::Float("0.5".into()),
                TokenKind::Float("1e-4".into()),
                TokenKind::Int("15".into()),
                TokenKind::Uint("0u".into()),
                TokenKind::Uint("0x9E3779B9u".into()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn operators_match_longest_first() {
        assert_eq!(
            kinds("h ^= h >> 15; ++i <= R"),
            vec![
                TokenKind::Ident("h".into()),
                TokenKind::Punct("^="),
                TokenKind::Ident("h".into()),
                TokenKind::Punct(">>"),
                TokenKind::Int("15".into()),
                TokenKind::Punct(";"),
                TokenKind::Punct("++"),
                TokenKind::Ident("i".into()),
                TokenKind::Punct("<="),
                TokenKind::Ident("R".into()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn comments_and_blank_lines_are_recorded() {
        let toks = lex("a;\n\n// note\nb;").unwrap();
        assert_eq!(toks[2].kind, TokenKind::Comment("note".into()));
        assert!(toks[2].blank_before);
        assert!(!toks[3].blank_before);
        assert_eq!(toks[3].span.line, 4);
        assert_eq!(toks[3].span.col, 1);
    }

    #[test]
    fn malformed_input_is_a_spanned_error() {
        let e = lex("float x = 1.0f;").unwrap_err();
        assert_eq!(e.code, "S0102");
        assert_eq!(e.span.col, 11);
        assert_eq!(lex("a /* b */").unwrap_err().code, "S0101");
        assert_eq!(lex("a @ b").unwrap_err().code, "S0103");
    }
}
