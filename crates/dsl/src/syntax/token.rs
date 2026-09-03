//! The flat [`Token`] the lexer emits, and the [`LexError`] it attaches when a
//! byte sequence is malformed but recoverable.
//!
//! A token is deliberately tiny: a [`SyntaxKind`], its byte [`TextRange`], and
//! an optional boxed-free error tag. It owns no text — the spelling is recovered
//! by slicing the original source with [`Token::text`], so a token vector for a
//! large file stays compact and cache-friendly. Trivia (whitespace, comments)
//! are emitted as ordinary tokens in the stream, keeping the sequence lossless;
//! attaching them to nodes as leading/trailing trivia happens later, in the
//! green-tree builder.

use super::kind::SyntaxKind;
use super::span::TextRange;
use crate::diag::Diagnostic;

/// One lexed token: what it is, where it is, and whether it was malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// The token's classification.
    pub kind: SyntaxKind,
    /// The token's byte span in the source. For [`SyntaxKind::Eof`] this is an
    /// empty range at the end of input.
    pub range: TextRange,
    /// A recoverable lexical error this token carries, if any. The token still
    /// has a `kind` and `range` (recovery keeps the stream lossless); the error
    /// drives a diagnostic. `None` for well-formed tokens (the common case).
    pub error: Option<LexError>,
}

impl Token {
    /// A well-formed token of `kind` spanning `range`.
    #[inline]
    pub const fn new(kind: SyntaxKind, range: TextRange) -> Token {
        Token {
            kind,
            range,
            error: None,
        }
    }

    /// A token of `kind` spanning `range` that carries a recoverable `error`.
    #[inline]
    pub const fn with_error(kind: SyntaxKind, range: TextRange, error: LexError) -> Token {
        Token {
            kind,
            range,
            error: Some(error),
        }
    }

    /// The token's source spelling, sliced from the `source` it was lexed from.
    ///
    /// `source` must be the exact string passed to the lexer; the range is a
    /// byte span into it. Slicing is `O(1)` and allocation-free.
    #[inline]
    pub fn text<'s>(&self, source: &'s str) -> &'s str {
        &source[self.range.as_usize()]
    }

    /// Whether this token is trivia (whitespace or a comment).
    #[inline]
    pub fn is_trivia(&self) -> bool {
        self.kind.is_trivia()
    }
}

/// A recoverable lexical error, attached to the [`Token`] it was found on.
///
/// Each variant maps to a stable diagnostic code (see [`LexError::code`]) so the
/// diagnostics layer (spec section 30) can render a primary span plus a note without
/// re-deriving the cause. The lexer never aborts on these — it records the error,
/// classifies the span as best it can, and keeps going, so a single malformed
/// token never truncates the token stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LexError {
    /// A `/* ... */` block comment ran to end of input with no closing `*/`.
    UnterminatedBlockComment,
    /// A `"..."` string ran to end of input (or end of line) with no closing
    /// quote.
    UnterminatedString,
    /// An `r#"..."#` raw string ran to end of input without its closing
    /// `"#...#` of matching hash count.
    UnterminatedRawString,
    /// A `'...'` character literal was never closed.
    UnterminatedChar,
    /// A `'...'` character literal held zero or more than one character.
    MalformedChar,
    /// A backslash escape used a letter that is not a recognized escape
    /// (`\n \r \t \0 \\ \" \' \x \u`).
    InvalidEscape,
    /// A `\xNN` byte escape had fewer than two hex digits or a non-hex digit.
    InvalidByteEscape,
    /// A `\u{...}` escape was malformed (missing braces, no digits, a non-hex
    /// digit, or a value that is not a Unicode scalar).
    InvalidUnicodeEscape,
    /// A `#...` color literal was not one of `#RGB` / `#RGBA` / `#RRGGBB` /
    /// `#RRGGBBAA`, or contained a non-hex digit.
    InvalidColor,
    /// A numeric literal misused digit separators — a leading, trailing, or
    /// doubled `_`, or a `_` adjacent to the radix prefix or the decimal point.
    MalformedNumericSeparator,
    /// A radix-prefixed integer (`0x` / `0o` / `0b`) had no digits after the
    /// prefix, or a digit outside its radix.
    MalformedIntLiteral,
    /// A raw string opener used more than 255 hashes (the format's limit).
    TooManyRawStringHashes,
    /// A lone `\r` not paired into a `\r\n` newline appeared in source text.
    BareCarriageReturn,
    /// A NUL byte (`U+0000`) appeared in the source.
    NulInSource,
    /// An identifier mixes scripts in a way known to be visually confusable
    /// (a cheap heuristic now; full Unicode confusable analysis lands with the
    /// symbol table). Warning severity — the token is still a valid identifier.
    ConfusableIdent,
    /// A byte or character the lexer could not begin any token with.
    UnexpectedCharacter,
}

impl LexError {
    /// The stable diagnostic code for this error (spec section 30). The string is part
    /// of the diagnostic contract and must stay stable across releases; the
    /// `Lex` prefix namespaces the lexer's codes.
    pub const fn code(self) -> &'static str {
        match self {
            LexError::UnterminatedBlockComment => "Lex0001",
            LexError::UnterminatedString => "Lex0002",
            LexError::UnterminatedRawString => "Lex0003",
            LexError::UnterminatedChar => "Lex0004",
            LexError::MalformedChar => "Lex0005",
            LexError::InvalidEscape => "Lex0006",
            LexError::InvalidByteEscape => "Lex0007",
            LexError::InvalidUnicodeEscape => "Lex0008",
            LexError::InvalidColor => "Lex0009",
            LexError::MalformedNumericSeparator => "Lex0010",
            LexError::MalformedIntLiteral => "Lex0011",
            LexError::TooManyRawStringHashes => "Lex0012",
            LexError::BareCarriageReturn => "Lex0013",
            LexError::NulInSource => "Lex0014",
            LexError::ConfusableIdent => "Lex0015",
            LexError::UnexpectedCharacter => "Lex0016",
        }
    }

    /// A short, human-readable description for the diagnostic message.
    pub const fn message(self) -> &'static str {
        match self {
            LexError::UnterminatedBlockComment => "unterminated block comment",
            LexError::UnterminatedString => "unterminated string literal",
            LexError::UnterminatedRawString => "unterminated raw string literal",
            LexError::UnterminatedChar => "unterminated character literal",
            LexError::MalformedChar => "character literal must contain exactly one character",
            LexError::InvalidEscape => "unknown character escape",
            LexError::InvalidByteEscape => "`\\x` escape needs exactly two hex digits",
            LexError::InvalidUnicodeEscape => "malformed `\\u{...}` unicode escape",
            LexError::InvalidColor => {
                "color literal must be `#RGB`, `#RGBA`, `#RRGGBB`, or `#RRGGBBAA`"
            }
            LexError::MalformedNumericSeparator => "misplaced `_` digit separator",
            LexError::MalformedIntLiteral => "integer literal has no valid digits",
            LexError::TooManyRawStringHashes => "raw string uses more than 255 hashes",
            LexError::BareCarriageReturn => "bare carriage return is not allowed",
            LexError::NulInSource => "null byte is not allowed in source",
            LexError::ConfusableIdent => "identifier mixes visually confusable scripts",
            LexError::UnexpectedCharacter => "unexpected character",
        }
    }

    /// Whether this error is a warning (the token is still usable) rather than a
    /// hard error. Only [`LexError::ConfusableIdent`] is a warning today.
    #[inline]
    pub const fn is_warning(self) -> bool {
        matches!(self, LexError::ConfusableIdent)
    }

    /// Lifts this lexical error, at `range`, into the shared [`Diagnostic`], picking
    /// its [`Severity`] from [`LexError::is_warning`]. Lex errors ride on the green
    /// tokens themselves; this is the uniform way a caller assembling a unit's full
    /// diagnostic list folds them in alongside parse and resolve diagnostics.
    pub fn to_diagnostic(self, range: TextRange) -> Diagnostic {
        if self.is_warning() {
            Diagnostic::warning(self.code(), range, self.message())
        } else {
            Diagnostic::error(self.code(), range, self.message())
        }
    }
}
