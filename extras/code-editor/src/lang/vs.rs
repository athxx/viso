//! `.vs` highlighted by the compiler's own tokenizer.
//!
//! A line is lexed by [`viso_lexer::Lexer`], the one the compiler parses
//! with, so what is a comment, a string or a number here is what it is to
//! the compiler. Only two tokens reach past a line's end — a block comment,
//! which nests, and a raw string — so a line's end state is which of those
//! it is still in, with the comment's depth or the string's hashes; a line
//! that starts in one finds where it ends by the same rule and lexes the
//! rest as the compiler would.
//!
//! The token kinds then name what a name is from the tokens around it, as a
//! reader would: a call, a type, a field, a path's module, an attribute, the
//! context words that are keywords where they start a declaration.

use std::ops::Range;

use viso_lexer::{LexError, Lexer, SyntaxKind};

use crate::highlight::{Language, Token, TokenKind};

/// In a block comment, its depth in the high bits.
const BLOCK: u32 = 1;
/// In a raw string, its hash count in the high bits.
const RAW: u32 = 2;

/// The `.vs` language.
#[derive(Debug, Clone, Copy, Default)]
pub struct Vs;

impl Language for Vs {
    fn name(&self) -> &str {
        "Viso"
    }

    fn line(&self, state: u32, line: &str, out: &mut Vec<Token>) -> u32 {
        let mut at = 0;
        match state & 3 {
            BLOCK => match block_end(line, (state >> 2).max(1)) {
                Ok(end) => {
                    out.push(Token::new(end, TokenKind::Comment));
                    at = end;
                }
                Err(depth) => {
                    out.push(Token::new(line.len(), TokenKind::Comment));
                    return BLOCK | (depth << 2);
                }
            },
            RAW => match raw_end(line, (state >> 2) as usize) {
                Some(end) => {
                    out.push(Token::new(end, TokenKind::String));
                    at = end;
                }
                None => {
                    out.push(Token::new(line.len(), TokenKind::String));
                    return state;
                }
            },
            _ => {}
        }
        lex(&line[at..], out)
    }

    fn line_comment(&self) -> Option<&str> {
        Some("//")
    }
}

/// Where a block comment `depth` deep at the start of `line` ends, past its
/// `*/`; or the depth it is at the line's end.
fn block_end(line: &str, mut depth: u32) -> Result<usize, u32> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return Ok(i);
            }
        } else {
            i += 1;
        }
    }
    Err(depth)
}

/// Where a raw string with `hashes` hashes at the start of `line` ends,
/// past its closing quote and hashes.
fn raw_end(line: &str, hashes: usize) -> Option<usize> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"'
            && b[i + 1..]
                .iter()
                .take(hashes)
                .filter(|&&c| c == b'#')
                .count()
                == hashes
        {
            return Some(i + 1 + hashes);
        }
        i += 1;
    }
    None
}

/// One token of a line, as the lexer saw it.
struct Lexed {
    kind: SyntaxKind,
    range: Range<usize>,
    error: Option<LexError>,
}

/// Lexes `text`, from a line's normal state, into `out`; returns the state
/// its end is in.
fn lex(text: &str, out: &mut Vec<Token>) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let mut lexer = Lexer::new(text);
    let mut toks = Vec::with_capacity(text.len() / 3 + 1);
    loop {
        let t = lexer.next_token();
        if t.kind == SyntaxKind::Eof {
            break;
        }
        toks.push(Lexed {
            kind: t.kind,
            range: t.range.start().to_usize()..t.range.end().to_usize(),
            error: t.error,
        });
    }
    // The significant tokens either side of each, for what a name is.
    let sig: Vec<usize> = (0..toks.len())
        .filter(|&i| !toks[i].kind.is_trivia())
        .collect();
    let mut p = 0;
    for (i, t) in toks.iter().enumerate() {
        while p < sig.len() && sig[p] < i {
            p += 1;
        }
        let prev = p.checked_sub(1).map(|q| toks[sig[q]].kind);
        let after = if sig.get(p) == Some(&i) { p + 1 } else { p };
        let next = sig
            .get(after)
            .map(|&n| (toks[n].kind, &text[toks[n].range.clone()]));
        let spelled = &text[t.range.clone()];
        match t.kind {
            SyntaxKind::StringLiteral
            | SyntaxKind::StringStart
            | SyntaxKind::StringMiddle
            | SyntaxKind::StringEnd => string(spelled, out),
            kind => out.push(Token::new(
                t.range.len(),
                classify(kind, spelled, prev, next),
            )),
        }
    }
    // A token running on past the line.
    match toks.last() {
        Some(t)
            if t.kind == SyntaxKind::BlockComment
                && t.error == Some(LexError::UnterminatedBlockComment) =>
        {
            let depth = match block_end(&text[t.range.start + 2..], 1) {
                Err(d) => d,
                Ok(_) => 1,
            };
            BLOCK | (depth << 2)
        }
        Some(t)
            if t.kind == SyntaxKind::RawStringLiteral
                && t.error == Some(LexError::UnterminatedRawString) =>
        {
            let hashes = text[t.range.start + 1..]
                .bytes()
                .take_while(|&b| b == b'#')
                .count() as u32;
            RAW | (hashes.min(255) << 2)
        }
        _ => 0,
    }
}

/// A string's text and its escapes, a literal brace (`{{`) and a hole's
/// brace among them.
fn string(text: &str, out: &mut Vec<Token>) {
    let b = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        let end = match b[i] {
            b'\\' if i + 1 < b.len() => escape_end(text, i),
            brace @ (b'{' | b'}') => i + if b.get(i + 1) == Some(&brace) { 2 } else { 1 },
            _ => {
                i += 1;
                continue;
            }
        };
        if i > start {
            out.push(Token::new(i - start, TokenKind::String));
        }
        out.push(Token::new(end - i, TokenKind::Escape));
        start = end;
        i = end;
    }
    if b.len() > start {
        out.push(Token::new(b.len() - start, TokenKind::String));
    }
}

/// The end of the escape at byte `at` of `text`, its backslash.
fn escape_end(text: &str, at: usize) -> usize {
    let rest = &text[at + 1..];
    let n = match rest.as_bytes().first() {
        Some(b'x') => 3,
        Some(b'u') if rest.as_bytes().get(1) == Some(&b'{') => {
            rest.find('}').map_or(rest.len(), |i| i + 1)
        }
        Some(_) => rest.chars().next().map_or(1, char::len_utf8),
        None => 0,
    };
    (at + 1 + n).min(text.len())
}

/// What token `kind`, spelled `text`, is to a reader, between the
/// significant tokens `prev` and `next`.
fn classify(
    kind: SyntaxKind,
    text: &str,
    prev: Option<SyntaxKind>,
    next: Option<(SyntaxKind, &str)>,
) -> TokenKind {
    use SyntaxKind as K;
    match kind {
        K::Whitespace => TokenKind::Text,
        K::LineComment | K::BlockComment => TokenKind::Comment,
        K::DocComment | K::ModuleDocComment => TokenKind::DocComment,
        K::IntLiteral | K::FloatLiteral | K::UnitLiteral => TokenKind::Number,
        K::StringLiteral | K::RawStringLiteral => TokenKind::String,
        K::CharLiteral => TokenKind::Char,
        K::ColorLiteral => TokenKind::Constant,
        K::TrueKw | K::FalseKw | K::NoneKw | K::SelfValueKw | K::SelfTypeKw => TokenKind::Builtin,
        K::Ident | K::RawIdent => name(text, prev, next),
        K::LParen | K::RParen | K::LBrace | K::RBrace | K::LBracket | K::RBracket => {
            TokenKind::Bracket
        }
        K::Comma | K::Semi | K::Colon | K::ColonColon | K::Dot | K::QuestionDot => {
            TokenKind::Punctuation
        }
        K::At => TokenKind::Attribute,
        K::Error => TokenKind::Invalid,
        k if k.is_keyword() => TokenKind::Keyword,
        _ => TokenKind::Operator,
    }
}

/// What a name is.
fn name(text: &str, prev: Option<SyntaxKind>, next: Option<(SyntaxKind, &str)>) -> TokenKind {
    use SyntaxKind as K;
    let text = text.strip_prefix("r#").unwrap_or(text);
    let next_kind = next.map(|(k, _)| k);
    if prev == Some(K::At) {
        return TokenKind::Attribute;
    }
    if matches!(prev, Some(K::FnKw | K::TaskKw | K::ActionKw)) {
        return TokenKind::Function;
    }
    let member = matches!(prev, Some(K::Dot | K::QuestionDot));
    // A context word starting a declaration or a clause.
    if !member
        && SyntaxKind::contextual_keyword(text).is_some()
        && matches!(
            next_kind,
            Some(K::Ident | K::RawIdent | K::LBrace | K::LParen | K::StringLiteral)
        )
        && !matches!(prev, Some(K::Colon | K::Eq))
    {
        return TokenKind::Keyword;
    }
    let upper = text.chars().next().is_some_and(char::is_uppercase);
    if next_kind == Some(K::LParen) {
        return if upper {
            TokenKind::Type
        } else {
            TokenKind::Function
        };
    }
    if member {
        return TokenKind::Property;
    }
    if next_kind == Some(K::ColonColon) {
        return if upper {
            TokenKind::Type
        } else {
            TokenKind::Namespace
        };
    }
    if upper {
        let shouting = text.len() > 1
            && text
                .chars()
                .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_');
        return if shouting {
            TokenKind::Constant
        } else {
            TokenKind::Type
        };
    }
    // A field set in a block: `gap: 8dp`.
    if next_kind == Some(K::Colon)
        && prev.is_none_or(|p| matches!(p, K::LBrace | K::Semi | K::Comma))
    {
        return TokenKind::Property;
    }
    TokenKind::Variable
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each byte's coarse class as the whole-file tokenizer sees it.
    fn coarse(kind: SyntaxKind) -> u8 {
        use SyntaxKind as K;
        match kind {
            K::LineComment | K::BlockComment | K::DocComment | K::ModuleDocComment => 1,
            K::StringLiteral
            | K::StringStart
            | K::StringMiddle
            | K::StringEnd
            | K::RawStringLiteral => 2,
            K::Whitespace => 0,
            _ => 3,
        }
    }

    fn coarse_kind(kind: TokenKind) -> u8 {
        match kind {
            TokenKind::Comment | TokenKind::DocComment => 1,
            TokenKind::String | TokenKind::Escape => 2,
            TokenKind::Text => 0,
            _ => 3,
        }
    }

    /// Highlights `source` a line at a time and checks every byte's class
    /// against the tokenizer run over the whole of it.
    fn agrees(source: &str) {
        let mut whole = vec![0u8; source.len()];
        for t in viso_lexer::tokenize(source) {
            let r = t.range.start().to_usize()..t.range.end().to_usize();
            let c = coarse(t.kind);
            for b in &mut whole[r] {
                *b = c;
            }
        }
        let mut state = 0;
        let mut at = 0;
        for line in source.split('\n') {
            let mut tokens = Vec::new();
            state = Vs.line(state, line, &mut tokens);
            let mut p = at;
            for t in &tokens {
                for i in p..p + t.len as usize {
                    // Whitespace inside a comment or string is either.
                    let got = coarse_kind(t.kind);
                    let want = whole[i];
                    let space = source.as_bytes()[i].is_ascii_whitespace();
                    assert!(
                        got == want || (space && (got == 0 || want == 0)),
                        "byte {i} {:?}: {got} vs {want}",
                        &source[i..(i + 8).min(source.len())]
                    );
                }
                p += t.len as usize;
            }
            assert_eq!(p, at + line.len(), "tokens cover the line {line:?}");
            at += line.len() + 1;
        }
    }

    #[test]
    fn line_states_agree_with_the_whole_file_tokenizer() {
        agrees(
            "component A {\n    /* one /* two\n still */ in one\n */ state x = r#\"raw\n\"still\" raw\"# ;\n    let s = \"a\\n\\u{41}b\"; // done\n}\n",
        );
        for source in [
            include_str!("../../../../crates/dsl/tests/usability/04-search.vs"),
            include_str!("../../../../crates/dsl/tests/native/colony.vs"),
            include_str!("../../../../crates/dsl/src/resolve/prelude.vs"),
        ] {
            agrees(source);
        }
    }

    #[test]
    fn names_are_told_apart_by_their_neighbours() {
        let mut out = Vec::new();
        let line = "    state count: Int = clamp(model.total, MAX_N); @inline fn go() {}";
        Vs.line(0, line, &mut out);
        let mut at = 0;
        let mut kinds = Vec::new();
        for t in &out {
            let text = &line[at..at + t.len as usize];
            if !text.trim().is_empty() {
                kinds.push((text, t.kind));
            }
            at += t.len as usize;
        }
        let of = |w: &str| kinds.iter().find(|(t, _)| *t == w).map(|(_, k)| *k);
        assert_eq!(of("state"), Some(TokenKind::Keyword));
        assert_eq!(of("count"), Some(TokenKind::Variable));
        assert_eq!(of("Int"), Some(TokenKind::Type));
        assert_eq!(of("clamp"), Some(TokenKind::Function));
        assert_eq!(of("model"), Some(TokenKind::Variable));
        assert_eq!(of("total"), Some(TokenKind::Property));
        assert_eq!(of("MAX_N"), Some(TokenKind::Constant));
        assert_eq!(of("inline"), Some(TokenKind::Attribute));
        assert_eq!(of("go"), Some(TokenKind::Function));
        assert_eq!(of("fn"), Some(TokenKind::Keyword));
    }
}
