//! The streaming tokenizer: `&str` → a flat [`Vec<Token>`] ending in
//! [`SyntaxKind::Eof`], with trivia preserved and every malformed span carrying
//! a [`LexError`] instead of aborting.
//!
//! The lexer is a byte cursor with `char`-aware peeking. It applies the spec's
//! longest-match rules, and the awkward cases are handled explicitly and tested:
//! the `%` percent-suffix-vs-modulo split, `1..2` lexing as integer + range,
//! `r#ident` vs `r"raw string"`, nested block comments, and string/char escape
//! validation. It **never panics** on any input — truncated, malformed, or
//! arbitrary bytes all produce a token stream (the fuzz contract).
//!
//! A resumable [`LexState`] captures the only cross-token lexer state (nothing,
//! today — a `.vs` token never depends on lexer state carried across a token
//! boundary), so the incremental reparser can restart the lexer mid-file at a
//! token boundary and get an identical stream. It exists as an explicit seam so
//! the incremental path in `reparse` has a state type to thread even though the
//! current grammar's state is empty.

use super::kind::SyntaxKind;
use super::span::{TextRange, TextSize};
use super::token::{LexError, Token};

/// Tokenizes `source` in full, returning every token (including trivia) followed
/// by a final [`SyntaxKind::Eof`]. Never panics.
pub fn tokenize(source: &str) -> Vec<Token> {
    let mut lexer = Lexer::new(source);
    let mut out = Vec::new();
    loop {
        let tok = lexer.next_token();
        let is_eof = tok.kind == SyntaxKind::Eof;
        out.push(tok);
        if is_eof {
            break;
        }
    }
    out
}

/// Resumable cross-token lexer state.
///
/// The `.vs` grammar carries no state across token boundaries (comments and
/// strings are always closed within a single token), so this is currently empty.
/// It is a named type rather than `()` so the incremental reparser threads a
/// stable state seam that stays correct if a future stateful token (e.g. a
/// heredoc) is added.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LexState;

/// A byte cursor over the source with `char`-aware inspection.
///
/// Public only within the crate's `syntax` module; the outside world uses
/// [`tokenize`]. The cursor tracks a byte offset; token spans are `[start, pos)`.
pub struct Lexer<'s> {
    source: &'s str,
    bytes: &'s [u8],
    /// Current byte offset into `source`. Always on a UTF-8 char boundary.
    pos: usize,
}

impl<'s> Lexer<'s> {
    /// Creates a lexer over `source`, starting at offset 0.
    pub fn new(source: &'s str) -> Lexer<'s> {
        Lexer {
            source,
            bytes: source.as_bytes(),
            pos: 0,
        }
    }

    /// Creates a lexer positioned at byte `offset` with resumable `state`, for
    /// incremental reparsing. `offset` must be a char boundary.
    pub fn resume(source: &'s str, offset: TextSize, _state: LexState) -> Lexer<'s> {
        debug_assert!(source.is_char_boundary(offset.to_usize()));
        Lexer {
            source,
            bytes: source.as_bytes(),
            pos: offset.to_usize(),
        }
    }

    /// The current byte offset.
    #[inline]
    pub fn offset(&self) -> TextSize {
        TextSize::new(self.pos as u32)
    }

    /// The resumable state at the current position (always empty today).
    #[inline]
    pub fn state(&self) -> LexState {
        LexState
    }

    // --- Cursor primitives -------------------------------------------------

    /// The byte at the current position, or `None` at end of input.
    #[inline]
    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    /// The byte `n` positions ahead, or `None` past end of input.
    #[inline]
    fn peek_byte_at(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.pos + n).copied()
    }

    /// The `char` at the current position (decoding a UTF-8 sequence), or `None`
    /// at end of input.
    #[inline]
    fn peek_char(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    /// Advances past one `char`, returning it. Returns `None` at end of input.
    #[inline]
    fn bump(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    /// Advances past one ASCII byte known to be present. Debug-asserts the byte
    /// is ASCII so multi-byte boundaries are never split.
    #[inline]
    fn bump_ascii(&mut self) {
        debug_assert!(self.bytes[self.pos] < 0x80);
        self.pos += 1;
    }

    /// Builds a token spanning `[start, pos)` with `kind`, no error.
    #[inline]
    fn token(&self, kind: SyntaxKind, start: usize) -> Token {
        Token::new(kind, self.range(start))
    }

    /// Builds a token spanning `[start, pos)` with `kind` and a recoverable
    /// `error`.
    #[inline]
    fn token_err(&self, kind: SyntaxKind, start: usize, error: LexError) -> Token {
        Token::with_error(kind, self.range(start), error)
    }

    /// The range `[start, pos)`.
    #[inline]
    fn range(&self, start: usize) -> TextRange {
        TextRange::new(TextSize::new(start as u32), TextSize::new(self.pos as u32))
    }

    // --- Main dispatch -----------------------------------------------------

    /// Lexes and returns the next token, advancing the cursor. At end of input
    /// returns an empty [`SyntaxKind::Eof`] token (idempotently).
    pub fn next_token(&mut self) -> Token {
        let start = self.pos;
        let Some(b) = self.peek_byte() else {
            return Token::new(SyntaxKind::Eof, TextRange::empty(self.offset()));
        };

        match b {
            b' ' | b'\t' | b'\n' => self.whitespace(start),
            b'\r' => self.carriage_return(start),
            b'/' => self.slash_or_comment(start),
            b'"' => self.string(start),
            b'\'' => self.char_literal(start),
            b'#' => self.color(start),
            b'r' => self.raw_prefix_or_ident(start),
            b'0'..=b'9' => self.number(start),
            0 => {
                self.bump_ascii();
                self.token_err(SyntaxKind::Error, start, LexError::NulInSource)
            }
            _ if is_ident_start_byte(b) => self.ident_or_keyword(start),
            _ => self.punct_or_unknown(start),
        }
    }

    // --- Trivia ------------------------------------------------------------

    /// A run of spaces, tabs, and newlines. `\r` is handled separately so a bare
    /// `\r` can be flagged; a `\r\n` pair is absorbed here after the `\r`.
    fn whitespace(&mut self, start: usize) -> Token {
        while let Some(b) = self.peek_byte() {
            match b {
                b' ' | b'\t' | b'\n' => self.bump_ascii(),
                b'\r' if self.peek_byte_at(1) == Some(b'\n') => {
                    // Absorb a well-formed CRLF into the whitespace run.
                    self.bump_ascii();
                    self.bump_ascii();
                }
                _ => break,
            }
        }
        self.token(SyntaxKind::Whitespace, start)
    }

    /// A carriage return. `\r\n` is a valid newline (lexed as whitespace); a lone
    /// `\r` is flagged as an error but still consumed as whitespace so the stream
    /// stays lossless.
    fn carriage_return(&mut self, start: usize) -> Token {
        if self.peek_byte_at(1) == Some(b'\n') {
            self.bump_ascii();
            self.bump_ascii();
            // Continue consuming any adjacent whitespace into one run.
            return self.whitespace(start);
        }
        self.bump_ascii();
        self.token_err(SyntaxKind::Whitespace, start, LexError::BareCarriageReturn)
    }

    /// `/` may begin a line comment, block comment, doc comment, the `/=`
    /// operator, or a bare `/` division operator.
    fn slash_or_comment(&mut self, start: usize) -> Token {
        match self.peek_byte_at(1) {
            Some(b'/') => self.line_comment(start),
            Some(b'*') => self.block_comment(start),
            Some(b'=') => {
                self.bump_ascii();
                self.bump_ascii();
                self.token(SyntaxKind::SlashEq, start)
            }
            _ => {
                self.bump_ascii();
                self.token(SyntaxKind::Slash, start)
            }
        }
    }

    /// `//` line comment, or `///` doc / `//!` module-doc. The doc distinction is
    /// by the third byte; `////+` is an ordinary line comment (Rust's rule).
    fn line_comment(&mut self, start: usize) -> Token {
        self.bump_ascii(); // first '/'
        self.bump_ascii(); // second '/'
        let kind = match self.peek_byte() {
            Some(b'/') if self.peek_byte_at(1) != Some(b'/') => SyntaxKind::DocComment,
            Some(b'!') => SyntaxKind::ModuleDocComment,
            _ => SyntaxKind::LineComment,
        };
        while let Some(b) = self.peek_byte() {
            if b == b'\n' {
                break;
            }
            // Advance one whole char so multi-byte content is not split.
            self.bump();
        }
        self.token(kind, start)
    }

    /// `/* ... */`, nestable. An unterminated comment consumes to end of input
    /// and is flagged, keeping the stream lossless.
    fn block_comment(&mut self, start: usize) -> Token {
        self.bump_ascii(); // '/'
        self.bump_ascii(); // '*'
        let mut depth = 1usize;
        while depth > 0 {
            match self.peek_byte() {
                None => {
                    return self.token_err(
                        SyntaxKind::BlockComment,
                        start,
                        LexError::UnterminatedBlockComment,
                    );
                }
                Some(b'/') if self.peek_byte_at(1) == Some(b'*') => {
                    self.bump_ascii();
                    self.bump_ascii();
                    depth += 1;
                }
                Some(b'*') if self.peek_byte_at(1) == Some(b'/') => {
                    self.bump_ascii();
                    self.bump_ascii();
                    depth -= 1;
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
        self.token(SyntaxKind::BlockComment, start)
    }

    // --- Identifiers & keywords -------------------------------------------

    /// An identifier or keyword. The dispatch byte test (`is_ident_start_byte`)
    /// accepts any non-ASCII lead byte, which is coarser than the char-level
    /// [`is_ident_start`] rule; so a non-ASCII, non-letter char (say `¡`) can
    /// reach here without being a real identifier start. Guard that case by
    /// consuming exactly one char as an `Error`, guaranteeing forward progress
    /// (otherwise the tail loop consumes nothing and the tokenizer spins).
    fn ident_or_keyword(&mut self, start: usize) -> Token {
        if self.peek_char().is_none_or(|ch| !is_ident_start(ch)) {
            self.bump(); // one whole char, never zero-width
            return self.token_err(SyntaxKind::Error, start, LexError::UnexpectedCharacter);
        }
        self.bump_identifier_tail();
        let text = &self.source[start..self.pos];
        let kind = SyntaxKind::from_ident(text);
        if kind == SyntaxKind::Ident && is_confusable(text) {
            return self.token_err(SyntaxKind::Ident, start, LexError::ConfusableIdent);
        }
        self.token(kind, start)
    }

    /// Consumes identifier-continue characters from the current position.
    fn bump_identifier_tail(&mut self) {
        while let Some(ch) = self.peek_char() {
            if is_ident_continue(ch) {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    /// A leading `r`: either a raw identifier `r#name`, a raw string `r"..."` /
    /// `r#"..."#`, or an ordinary identifier that merely starts with `r`.
    fn raw_prefix_or_ident(&mut self, start: usize) -> Token {
        match self.peek_byte_at(1) {
            // `r"..."` — raw string with zero hashes.
            Some(b'"') => {
                self.bump_ascii(); // 'r'
                self.raw_string(start, 0)
            }
            // `r#` — either `r#"..."#` raw string or `r#ident` raw identifier.
            Some(b'#') => {
                // Count the hashes after `r`.
                let mut n = 0usize;
                while self.peek_byte_at(1 + n) == Some(b'#') {
                    n += 1;
                }
                match self.peek_byte_at(1 + n) {
                    Some(b'"') => {
                        // Raw string: consume `r` and the hashes, then the body.
                        self.bump_ascii(); // 'r'
                        for _ in 0..n {
                            self.bump_ascii();
                        }
                        self.raw_string(start, n)
                    }
                    _ if n == 1 => {
                        // `r#ident` raw identifier: `r`, one `#`, then an ident.
                        self.bump_ascii(); // 'r'
                        self.bump_ascii(); // '#'
                        self.bump_identifier_tail();
                        self.token(SyntaxKind::RawIdent, start)
                    }
                    _ => {
                        // `r##...` not followed by `"` and not a single-hash raw
                        // ident: treat the `r` as an ordinary identifier start.
                        self.ident_or_keyword(start)
                    }
                }
            }
            // Just an identifier beginning with `r`.
            _ => self.ident_or_keyword(start),
        }
    }

    /// A raw string body: reads until a `"` followed by exactly `hashes` `#`s.
    /// The opening `r`/hashes/quote handling differs by caller; here the cursor
    /// is positioned just before the opening `"`.
    fn raw_string(&mut self, start: usize, hashes: usize) -> Token {
        if hashes > 255 {
            // Over the format's hash limit; still consume the opener's quote and
            // scan to end so the stream stays lossless.
            if self.peek_byte() == Some(b'"') {
                self.bump_ascii();
            }
            while self.bump().is_some() {}
            return self.token_err(
                SyntaxKind::RawStringLiteral,
                start,
                LexError::TooManyRawStringHashes,
            );
        }
        // Consume the opening quote.
        debug_assert_eq!(self.peek_byte(), Some(b'"'));
        self.bump_ascii();
        loop {
            match self.peek_byte() {
                None => {
                    return self.token_err(
                        SyntaxKind::RawStringLiteral,
                        start,
                        LexError::UnterminatedRawString,
                    );
                }
                Some(b'"') => {
                    // A closing quote is only real if followed by `hashes` `#`s.
                    let mut matched = 0usize;
                    while self.peek_byte_at(1 + matched) == Some(b'#') && matched < hashes {
                        matched += 1;
                    }
                    if matched == hashes {
                        self.bump_ascii(); // '"'
                        for _ in 0..hashes {
                            self.bump_ascii();
                        }
                        return self.token(SyntaxKind::RawStringLiteral, start);
                    }
                    // Not the real closer; consume the quote and keep scanning.
                    self.bump_ascii();
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    // --- Strings & chars ---------------------------------------------------

    /// A `"..."` string with escapes. Unterminated at end of line or input is
    /// flagged; escape errors are flagged but scanning continues to the close.
    fn string(&mut self, start: usize) -> Token {
        self.bump_ascii(); // opening '"'
        let mut error: Option<LexError> = None;
        loop {
            match self.peek_byte() {
                None => {
                    return self.token_err(
                        SyntaxKind::StringLiteral,
                        start,
                        error.unwrap_or(LexError::UnterminatedString),
                    );
                }
                Some(b'"') => {
                    self.bump_ascii();
                    return match error {
                        Some(e) => self.token_err(SyntaxKind::StringLiteral, start, e),
                        None => self.token(SyntaxKind::StringLiteral, start),
                    };
                }
                Some(b'\n') => {
                    // A newline inside a string closes nothing; the string is
                    // unterminated on this line. Do not consume the newline (it
                    // is its own whitespace token), so recovery resumes cleanly.
                    return self.token_err(
                        SyntaxKind::StringLiteral,
                        start,
                        error.unwrap_or(LexError::UnterminatedString),
                    );
                }
                Some(b'\\') => {
                    if let Some(e) = self.string_escape() {
                        error.get_or_insert(e);
                    }
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    /// Validates and consumes one escape sequence beginning at the `\`. Returns a
    /// [`LexError`] if malformed (the sequence is still consumed for recovery).
    fn string_escape(&mut self) -> Option<LexError> {
        self.bump_ascii(); // backslash
        match self.peek_byte() {
            None => Some(LexError::InvalidEscape),
            Some(b'n' | b'r' | b't' | b'0' | b'\\' | b'"' | b'\'') => {
                self.bump_ascii();
                None
            }
            Some(b'x') => {
                self.bump_ascii();
                self.byte_escape()
            }
            Some(b'u') => {
                self.bump_ascii();
                self.unicode_escape()
            }
            Some(_) => {
                // Consume one char so a stray escape does not stall scanning.
                self.bump();
                Some(LexError::InvalidEscape)
            }
        }
    }

    /// A `\xNN` escape: exactly two hex digits must follow the already-consumed
    /// `\x`.
    fn byte_escape(&mut self) -> Option<LexError> {
        let mut ok = true;
        for _ in 0..2 {
            match self.peek_byte() {
                Some(b) if b.is_ascii_hexdigit() => self.bump_ascii(),
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            None
        } else {
            Some(LexError::InvalidByteEscape)
        }
    }

    /// A `\u{...}` escape: `{`, one-to-six hex digits forming a Unicode scalar,
    /// then `}`. The `\u` is already consumed.
    fn unicode_escape(&mut self) -> Option<LexError> {
        if self.peek_byte() != Some(b'{') {
            return Some(LexError::InvalidUnicodeEscape);
        }
        self.bump_ascii(); // '{'
        let mut value: u32 = 0;
        let mut digits = 0usize;
        let mut overflow = false;
        while let Some(b) = self.peek_byte() {
            if b.is_ascii_hexdigit() {
                self.bump_ascii();
                digits += 1;
                if digits <= 6 {
                    value = value * 16 + hex_value(b);
                } else {
                    overflow = true;
                }
            } else {
                break;
            }
        }
        let closed = self.peek_byte() == Some(b'}');
        if closed {
            self.bump_ascii(); // '}'
        }
        if !closed || digits == 0 || overflow || char::from_u32(value).is_none() {
            Some(LexError::InvalidUnicodeEscape)
        } else {
            None
        }
    }

    /// A `'c'` character literal. Exactly one character (or a single valid
    /// escape) between the quotes; anything else is flagged.
    fn char_literal(&mut self, start: usize) -> Token {
        self.bump_ascii(); // opening quote
        let mut error: Option<LexError> = None;
        let mut count = 0usize;
        loop {
            match self.peek_byte() {
                None | Some(b'\n') => {
                    return self.token_err(
                        SyntaxKind::CharLiteral,
                        start,
                        error.unwrap_or(LexError::UnterminatedChar),
                    );
                }
                Some(b'\'') => {
                    self.bump_ascii();
                    let err = error.or(if count == 1 {
                        None
                    } else {
                        Some(LexError::MalformedChar)
                    });
                    return match err {
                        Some(e) => self.token_err(SyntaxKind::CharLiteral, start, e),
                        None => self.token(SyntaxKind::CharLiteral, start),
                    };
                }
                Some(b'\\') => {
                    if let Some(e) = self.string_escape() {
                        error.get_or_insert(e);
                    }
                    count += 1;
                }
                Some(_) => {
                    self.bump();
                    count += 1;
                }
            }
        }
    }

    // --- Color -------------------------------------------------------------

    /// A `#RGB` / `#RGBA` / `#RRGGBB` / `#RRGGBBAA` color literal. The `#` is at
    /// the cursor. Non-hex or a wrong digit count is flagged (span still taken).
    fn color(&mut self, start: usize) -> Token {
        self.bump_ascii(); // '#'
        let digits_start = self.pos;
        while let Some(b) = self.peek_byte() {
            if b.is_ascii_hexdigit() {
                self.bump_ascii();
            } else {
                break;
            }
        }
        let n = self.pos - digits_start;
        // A trailing identifier-continue char means this was not a clean color.
        let clean_boundary = match self.peek_char() {
            Some(ch) => !is_ident_continue(ch),
            None => true,
        };
        if matches!(n, 3 | 4 | 6 | 8) && clean_boundary {
            self.token(SyntaxKind::ColorLiteral, start)
        } else {
            // Consume any trailing ident chars so the bad token is one span.
            self.bump_identifier_tail();
            self.token_err(SyntaxKind::ColorLiteral, start, LexError::InvalidColor)
        }
    }

    // --- Numbers -----------------------------------------------------------

    /// A numeric literal: integer, float, or unit literal. The first byte is a
    /// digit. Handles radix prefixes, digit separators, the `1..2` range rule,
    /// numeric suffixes, and the `%` percent-vs-modulo disambiguation.
    fn number(&mut self, start: usize) -> Token {
        // Radix-prefixed integers: 0x / 0o / 0b.
        if self.peek_byte() == Some(b'0')
            && let Some(radix) = self.peek_byte_at(1).and_then(radix_of)
        {
            self.bump_ascii(); // '0'
            self.bump_ascii(); // radix letter
            return self.radix_integer(start, radix);
        }

        // Decimal integer part.
        let sep_err_int = self.bump_decimal_digits();

        // A `.` may start a fraction — but only if followed by a digit. `1.` and
        // `1..2` do NOT make a float: `.` alone / `..` are operators, so `1..2`
        // lexes as Int + `..`, and `1.method()` as Int + `.` (spec forces `1.0`).
        let mut is_float = false;
        if self.peek_byte() == Some(b'.')
            && self.peek_byte_at(1).is_some_and(|b| b.is_ascii_digit())
        {
            is_float = true;
            self.bump_ascii(); // '.'
            self.bump_decimal_digits();
        }

        // An exponent `e`/`E` with optional sign makes it a float.
        if matches!(self.peek_byte(), Some(b'e' | b'E')) {
            let sign = matches!(self.peek_byte_at(1), Some(b'+' | b'-'));
            let digit_at = if sign { 2 } else { 1 };
            if self
                .peek_byte_at(digit_at)
                .is_some_and(|b| b.is_ascii_digit())
            {
                is_float = true;
                self.bump_ascii(); // 'e'/'E'
                if sign {
                    self.bump_ascii();
                }
                self.bump_decimal_digits();
            }
        }

        // A `%` suffix makes a UnitLiteral, but ONLY when it does not begin a
        // modulo: `50%` (percent) vs `50%3` / `50%x` (modulo). The `%` is a
        // percent suffix only if the char after it is not ident-continue/digit.
        if self.peek_byte() == Some(b'%') {
            let after = self.peek_byte_at(1);
            let is_modulo = match after {
                Some(b) if b.is_ascii_digit() => true,
                Some(b) => is_ident_start_byte(b),
                None => false,
            };
            if !is_modulo {
                self.bump_ascii(); // '%'
                return self.finish_number(start, SyntaxKind::UnitLiteral, sep_err_int);
            }
            // Otherwise leave `%` for the operator lexer.
        }

        // A unit or type suffix: an identifier immediately following the digits
        // (e.g. `12px`, `1u32`, `3.0f32`). We classify it as UnitLiteral so the
        // parser/HIR can split the numeric body from the suffix; a bare int/float
        // with no suffix stays Int/Float.
        if self.peek_char().is_some_and(is_ident_start) {
            self.bump_identifier_tail();
            return self.finish_number(start, SyntaxKind::UnitLiteral, sep_err_int);
        }

        let kind = if is_float {
            SyntaxKind::FloatLiteral
        } else {
            SyntaxKind::IntLiteral
        };
        self.finish_number(start, kind, sep_err_int)
    }

    /// Emits a numeric token, attaching a separator error if one was seen.
    fn finish_number(&self, start: usize, kind: SyntaxKind, sep_err: Option<LexError>) -> Token {
        match sep_err {
            Some(e) => self.token_err(kind, start, e),
            None => self.token(kind, start),
        }
    }

    /// A radix integer body after the `0x`/`0o`/`0b` prefix. Requires at least
    /// one valid digit; separators follow the same misplacement rules.
    fn radix_integer(&mut self, start: usize, radix: u32) -> Token {
        let mut any_digit = false;
        let mut prev_sep = true; // a `_` right after the prefix is misplaced
        let mut sep_err: Option<LexError> = None;
        while let Some(b) = self.peek_byte() {
            if b == b'_' {
                if prev_sep {
                    sep_err.get_or_insert(LexError::MalformedNumericSeparator);
                }
                prev_sep = true;
                self.bump_ascii();
            } else if (b as char).is_digit(radix) {
                any_digit = true;
                prev_sep = false;
                self.bump_ascii();
            } else if b.is_ascii_alphanumeric() {
                // A digit outside the radix (e.g. `0b12`) or a stray alnum.
                any_digit = true;
                prev_sep = false;
                sep_err.get_or_insert(LexError::MalformedIntLiteral);
                self.bump_ascii();
            } else {
                break;
            }
        }
        if prev_sep && self.bytes.get(self.pos.wrapping_sub(1)) == Some(&b'_') {
            sep_err.get_or_insert(LexError::MalformedNumericSeparator);
        }
        // Allow a trailing type suffix (`0xFFu8`) — handled by the alnum branch
        // above folding into digits; keep classification as IntLiteral.
        if !any_digit {
            sep_err.get_or_insert(LexError::MalformedIntLiteral);
        }
        self.finish_number(start, SyntaxKind::IntLiteral, sep_err)
    }

    /// Consumes a run of decimal digits and `_` separators, returning a
    /// separator error if a `_` was leading, trailing, or doubled. Does not
    /// consume a `.` or suffix.
    fn bump_decimal_digits(&mut self) -> Option<LexError> {
        let mut sep_err: Option<LexError> = None;
        let mut prev_was_sep = false;
        let mut prev_was_digit = false;
        while let Some(b) = self.peek_byte() {
            if b.is_ascii_digit() {
                prev_was_sep = false;
                prev_was_digit = true;
                self.bump_ascii();
            } else if b == b'_' {
                // A `_` must sit between two digits: not leading, not doubled.
                if !prev_was_digit || prev_was_sep {
                    sep_err.get_or_insert(LexError::MalformedNumericSeparator);
                }
                prev_was_sep = true;
                self.bump_ascii();
            } else {
                break;
            }
        }
        // A trailing `_` (e.g. `1_`) is misplaced.
        if prev_was_sep {
            sep_err.get_or_insert(LexError::MalformedNumericSeparator);
        }
        sep_err
    }

    // --- Punctuation -------------------------------------------------------

    /// A delimiter or operator, or an unknown character. Longest-match over the
    /// multi-byte operators (spec section 13).
    fn punct_or_unknown(&mut self, start: usize) -> Token {
        let b0 = self.bytes[self.pos];
        let b1 = self.peek_byte_at(1);
        let b2 = self.peek_byte_at(2);

        // A helper closure would borrow `self`; instead advance inline per arm.
        macro_rules! emit {
            ($len:expr, $kind:expr) => {{
                for _ in 0..$len {
                    self.bump_ascii();
                }
                return self.token($kind, start);
            }};
        }

        match b0 {
            b'(' => emit!(1, SyntaxKind::LParen),
            b')' => emit!(1, SyntaxKind::RParen),
            b'{' => emit!(1, SyntaxKind::LBrace),
            b'}' => emit!(1, SyntaxKind::RBrace),
            b'[' => emit!(1, SyntaxKind::LBracket),
            b']' => emit!(1, SyntaxKind::RBracket),
            b',' => emit!(1, SyntaxKind::Comma),
            b';' => emit!(1, SyntaxKind::Semi),
            b'@' => emit!(1, SyntaxKind::At),
            b'~' => emit!(1, SyntaxKind::Tilde),
            b':' => match b1 {
                Some(b':') => emit!(2, SyntaxKind::ColonColon),
                _ => emit!(1, SyntaxKind::Colon),
            },
            b'+' => match b1 {
                Some(b'=') => emit!(2, SyntaxKind::PlusEq),
                _ => emit!(1, SyntaxKind::Plus),
            },
            b'-' => match b1 {
                Some(b'=') => emit!(2, SyntaxKind::MinusEq),
                Some(b'>') => emit!(2, SyntaxKind::Arrow),
                _ => emit!(1, SyntaxKind::Minus),
            },
            b'*' => match b1 {
                Some(b'=') => emit!(2, SyntaxKind::StarEq),
                _ => emit!(1, SyntaxKind::Star),
            },
            b'%' => match b1 {
                Some(b'=') => emit!(2, SyntaxKind::PercentEq),
                _ => emit!(1, SyntaxKind::Percent),
            },
            b'^' => match b1 {
                Some(b'=') => emit!(2, SyntaxKind::CaretEq),
                _ => emit!(1, SyntaxKind::Caret),
            },
            b'!' => match b1 {
                Some(b'=') => emit!(2, SyntaxKind::Neq),
                _ => emit!(1, SyntaxKind::Bang),
            },
            b'&' => match b1 {
                Some(b'&') => emit!(2, SyntaxKind::AmpAmp),
                Some(b'=') => emit!(2, SyntaxKind::AmpEq),
                _ => emit!(1, SyntaxKind::Amp),
            },
            b'|' => match b1 {
                Some(b'|') => emit!(2, SyntaxKind::PipePipe),
                Some(b'=') => emit!(2, SyntaxKind::PipeEq),
                _ => emit!(1, SyntaxKind::Pipe),
            },
            b'=' => match b1 {
                Some(b'=') => emit!(2, SyntaxKind::EqEq),
                Some(b'>') => emit!(2, SyntaxKind::FatArrow),
                _ => emit!(1, SyntaxKind::Eq),
            },
            b'<' => match (b1, b2) {
                (Some(b'='), Some(b'>')) => emit!(3, SyntaxKind::BidiArrow),
                (Some(b'<'), Some(b'=')) => emit!(3, SyntaxKind::ShlEq),
                (Some(b'<'), _) => emit!(2, SyntaxKind::Shl),
                (Some(b'='), _) => emit!(2, SyntaxKind::Le),
                _ => emit!(1, SyntaxKind::Lt),
            },
            b'>' => match (b1, b2) {
                (Some(b'>'), Some(b'=')) => emit!(3, SyntaxKind::ShrEq),
                (Some(b'>'), _) => emit!(2, SyntaxKind::Shr),
                (Some(b'='), _) => emit!(2, SyntaxKind::Ge),
                _ => emit!(1, SyntaxKind::Gt),
            },
            b'?' => match b1 {
                Some(b'?') => emit!(2, SyntaxKind::QuestionQuestion),
                Some(b'.') => emit!(2, SyntaxKind::QuestionDot),
                _ => emit!(1, SyntaxKind::Question),
            },
            b'.' => match (b1, b2) {
                (Some(b'.'), Some(b'=')) => emit!(3, SyntaxKind::DotDotEq),
                (Some(b'.'), _) => emit!(2, SyntaxKind::DotDot),
                _ => emit!(1, SyntaxKind::Dot),
            },
            _ => {
                // An unclassifiable char: consume exactly one whole char.
                self.bump();
                self.token_err(SyntaxKind::Error, start, LexError::UnexpectedCharacter)
            }
        }
    }
}

// --- Free helpers ----------------------------------------------------------

/// The radix for a `0x`/`0o`/`0b` prefix byte, or `None`.
#[inline]
fn radix_of(b: u8) -> Option<u32> {
    match b {
        b'x' | b'X' => Some(16),
        b'o' | b'O' => Some(8),
        b'b' | b'B' => Some(2),
        _ => None,
    }
}

/// The numeric value of an ASCII hex digit byte (assumes `b.is_ascii_hexdigit()`).
#[inline]
fn hex_value(b: u8) -> u32 {
    match b {
        b'0'..=b'9' => (b - b'0') as u32,
        b'a'..=b'f' => (b - b'a' + 10) as u32,
        b'A'..=b'F' => (b - b'A' + 10) as u32,
        _ => 0,
    }
}

/// Whether an ASCII byte can start an identifier without needing a full `char`
/// decode. Non-ASCII starts are handled through [`is_ident_start`].
#[inline]
fn is_ident_start_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic() || b >= 0x80
}

/// Whether `ch` can start an identifier (spec section 11 — XID_start, plus `_`).
///
/// Uses `char::is_alphabetic` as the Unicode-aware approximation of XID_start;
/// full Unicode 16.0 XID tables and NFC/confusable normalization land at
/// symbol-table entry in the next slice (this keeps the crate dependency-free).
#[inline]
fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic() || (!ch.is_ascii() && ch.is_alphanumeric())
}

/// Whether `ch` can continue an identifier (spec section 11 — XID_continue).
#[inline]
fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

/// A cheap confusable-identifier heuristic: flag an identifier that mixes ASCII
/// letters with non-ASCII letters, a common homoglyph attack shape. This is a
/// warning-level placeholder; the real Unicode confusable analysis runs at
/// symbol-table entry in the next slice.
fn is_confusable(text: &str) -> bool {
    let mut has_ascii_alpha = false;
    let mut has_non_ascii_alpha = false;
    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            has_ascii_alpha = true;
        } else if !ch.is_ascii() && ch.is_alphabetic() {
            has_non_ascii_alpha = true;
        }
    }
    has_ascii_alpha && has_non_ascii_alpha
}
