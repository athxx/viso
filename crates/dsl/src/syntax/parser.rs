//! The minimal parser skeleton: a coarse recursive-descent pass that groups the
//! flat token stream into a lossless CST at declaration/brace granularity.
//!
//! This is deliberately *not* the typed grammar — no EBNF productions, no
//! `:` vs `=` context split, no precedence-correct expressions. Those land in
//! the next slice. What this pass does provide is the tree *shape* the CST
//! contract requires: every token lands under a node, balanced `{...}` become
//! [`SyntaxKind::Block`] nodes, top-level declarations (a declaration keyword up
//! to their terminating `;` or `{...}`) become [`SyntaxKind::Item`] nodes, and on
//! a structural error the parser emits an [`SyntaxKind::ErrorNode`] and
//! **synchronizes** forward to the next `;` / `,` / `}` / declaration keyword so
//! one mistake does not cascade. Trivia are attached in stream order, keeping the
//! result byte-for-byte lossless.
//!
//! The parser records multiple diagnostics per pass (it never stops at the first
//! error), and because it consumes the same [`Token`] stream a full or an
//! incremental re-lex produces, an incrementally reparsed tree is identical to a
//! full reparse of the edited source.

use std::rc::Rc;

use super::cst::{GreenBuilder, GreenNode};
use super::kind::SyntaxKind;
use super::span::{TextRange, TextSize};
use super::token::Token;

/// A structural (non-lexical) diagnostic the skeleton parser emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    /// The byte span the error covers.
    pub range: TextRange,
    /// What went wrong.
    pub kind: ParseErrorKind,
}

/// The kind of a structural parse error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseErrorKind {
    /// A `{`, `(`, or `[` was never closed before end of input.
    UnclosedDelimiter,
    /// A `}`, `)`, or `]` appeared with no matching opener.
    UnmatchedCloser,
    /// Tokens appeared at the top level that did not start a declaration and
    /// were grouped into an error node during recovery.
    UnexpectedTokens,
}

impl ParseErrorKind {
    /// The stable diagnostic code for this error (spec 30). The `Parse` prefix
    /// namespaces the parser's codes, distinct from the lexer's `Lex` codes.
    pub const fn code(self) -> &'static str {
        match self {
            ParseErrorKind::UnclosedDelimiter => "Parse0001",
            ParseErrorKind::UnmatchedCloser => "Parse0002",
            ParseErrorKind::UnexpectedTokens => "Parse0003",
        }
    }

    /// A short, human-readable description for the diagnostic message.
    pub const fn message(self) -> &'static str {
        match self {
            ParseErrorKind::UnclosedDelimiter => "unclosed delimiter",
            ParseErrorKind::UnmatchedCloser => "unmatched closing delimiter",
            ParseErrorKind::UnexpectedTokens => "unexpected tokens",
        }
    }
}

/// The result of a parse: the lossless CST root plus any structural diagnostics.
///
/// Lexical errors ride on the green tokens themselves (see
/// [`GreenNode::errors`](super::cst::GreenNode::errors)); this `errors` list is
/// only the *structural* diagnostics the parser added.
#[derive(Debug, Clone)]
pub struct Parse {
    /// The lossless CST root. `root.text()` equals the original source.
    pub root: Rc<GreenNode>,
    /// Structural diagnostics, in source order.
    pub errors: Vec<ParseError>,
}

/// Parses `tokens` (the full stream from the lexer, including trivia and the
/// final [`SyntaxKind::Eof`]) over `source` into a lossless CST.
pub fn parse(tokens: &[Token], source: &str) -> Parse {
    let mut parser = Parser::new(tokens, source);
    parser.parse_root();
    Parse {
        root: parser.builder.finish(),
        errors: parser.errors,
    }
}

/// The coarse recursive-descent parser state.
struct Parser<'t, 's> {
    tokens: &'t [Token],
    source: &'s str,
    /// Index of the next token to consume.
    pos: usize,
    builder: GreenBuilder,
    errors: Vec<ParseError>,
}

impl<'t, 's> Parser<'t, 's> {
    fn new(tokens: &'t [Token], source: &'s str) -> Parser<'t, 's> {
        Parser {
            tokens,
            source,
            pos: 0,
            builder: GreenBuilder::new(),
            errors: Vec::new(),
        }
    }

    // --- Token cursor (trivia-aware) --------------------------------------

    /// The kind of the token at `pos`, or [`SyntaxKind::Eof`] past the end.
    fn current(&self) -> SyntaxKind {
        self.tokens
            .get(self.pos)
            .map_or(SyntaxKind::Eof, |t| t.kind)
    }

    /// Whether the cursor is at the final [`SyntaxKind::Eof`] token (or past it).
    fn at_eof(&self) -> bool {
        self.current() == SyntaxKind::Eof
    }

    /// Pushes the current token into the tree and advances. Never called at EOF
    /// (the `Eof` token is not part of the tree).
    fn bump(&mut self) {
        let token = self.tokens[self.pos];
        debug_assert_ne!(token.kind, SyntaxKind::Eof);
        self.builder.token_from(token, self.source);
        self.pos += 1;
    }

    /// Pushes any run of leading trivia into the current node without treating it
    /// as significant. Keeps whitespace/comments attached in stream order.
    fn bump_trivia(&mut self) {
        while self.current().is_trivia() {
            self.bump();
        }
    }

    /// The byte offset of the token at `pos` (end of input for the `Eof` token).
    fn offset(&self) -> TextSize {
        match self.tokens.get(self.pos) {
            Some(t) => t.range.start(),
            None => self.tokens.last().map_or(TextSize::ZERO, |t| t.range.end()),
        }
    }

    // --- Grammar (coarse) --------------------------------------------------

    /// Parses the whole document: a `Root` node of items, blocks, and recovered
    /// error nodes, terminating at EOF.
    fn parse_root(&mut self) {
        self.builder.start_node(SyntaxKind::Root);
        while !self.at_eof() {
            self.bump_trivia();
            if self.at_eof() {
                break;
            }
            match self.current() {
                // A stray closer at the top level: recover by wrapping it.
                SyntaxKind::RBrace | SyntaxKind::RParen | SyntaxKind::RBracket => {
                    let start = self.offset();
                    self.builder.start_node(SyntaxKind::ErrorNode);
                    self.bump();
                    self.builder.finish_node();
                    self.push_error(start, ParseErrorKind::UnmatchedCloser);
                }
                // A brace-delimited block standing on its own.
                SyntaxKind::LBrace => self.parse_block(),
                // A declaration keyword opens an item.
                k if k.is_item_start() => self.parse_item(),
                // Anything else at the top level is unexpected; group and sync.
                _ => self.parse_error_run(),
            }
        }
        self.builder.finish_node();
    }

    /// Parses a coarse item: a declaration keyword and everything up to and
    /// including its terminating `;` or balanced `{...}`.
    fn parse_item(&mut self) {
        self.builder.start_node(SyntaxKind::Item);
        // Consume the leading declaration keyword up front so the `is_item_start`
        // break below only fires on the *next* declaration, not this one (which
        // would loop forever, since `parse_root` re-dispatches on the same token).
        self.bump_trivia();
        debug_assert!(self.current().is_item_start());
        self.bump();
        // The remaining header tokens, until we hit the item terminator (`;` or a
        // `{`), a top-level closer, the next declaration, or EOF.
        loop {
            self.bump_trivia();
            match self.current() {
                SyntaxKind::Eof => break,
                SyntaxKind::Semi => {
                    self.bump(); // include the terminating `;`
                    break;
                }
                SyntaxKind::LBrace => {
                    self.parse_block(); // the item body; item ends after it
                    break;
                }
                // A top-level closer or a new declaration ends this item without
                // consuming the boundary token (the root loop handles it).
                SyntaxKind::RBrace | SyntaxKind::RParen | SyntaxKind::RBracket => break,
                k if k.is_item_start() => break,
                _ => self.bump(),
            }
        }
        self.builder.finish_node();
    }

    /// Parses a balanced `{ ... }` block. Nested `{}`/`()`/`[]` recurse; an
    /// unclosed block at EOF is flagged but still produces a node (lossless).
    fn parse_block(&mut self) {
        let start = self.offset();
        self.builder.start_node(SyntaxKind::Block);
        self.bump(); // `{`
        loop {
            self.bump_trivia();
            match self.current() {
                SyntaxKind::Eof => {
                    self.push_error(start, ParseErrorKind::UnclosedDelimiter);
                    break;
                }
                SyntaxKind::RBrace => {
                    self.bump(); // matching `}`
                    break;
                }
                SyntaxKind::LBrace => self.parse_block(),
                SyntaxKind::LParen => self.parse_group(SyntaxKind::RParen),
                SyntaxKind::LBracket => self.parse_group(SyntaxKind::RBracket),
                // A stray closer inside a block: consume it so we make progress,
                // flagging the mismatch.
                SyntaxKind::RParen | SyntaxKind::RBracket => {
                    let at = self.offset();
                    self.bump();
                    self.push_error(at, ParseErrorKind::UnmatchedCloser);
                }
                _ => self.bump(),
            }
        }
        self.builder.finish_node();
    }

    /// Parses a balanced `(...)` or `[...]` group, closed by `closer`. Used only
    /// to keep delimiter nesting balanced inside blocks; the contents are left
    /// flat (the typed grammar refines them in the next slice).
    fn parse_group(&mut self, closer: SyntaxKind) {
        let start = self.offset();
        self.builder.start_node(SyntaxKind::Block);
        self.bump(); // opener
        loop {
            self.bump_trivia();
            match self.current() {
                SyntaxKind::Eof => {
                    self.push_error(start, ParseErrorKind::UnclosedDelimiter);
                    break;
                }
                k if k == closer => {
                    self.bump(); // matching closer
                    break;
                }
                SyntaxKind::LBrace => self.parse_block(),
                SyntaxKind::LParen => self.parse_group(SyntaxKind::RParen),
                SyntaxKind::LBracket => self.parse_group(SyntaxKind::RBracket),
                // A closer of the wrong kind ends the group without consuming it,
                // so the outer context can match it.
                SyntaxKind::RBrace | SyntaxKind::RParen | SyntaxKind::RBracket => {
                    self.push_error(start, ParseErrorKind::UnclosedDelimiter);
                    break;
                }
                _ => self.bump(),
            }
        }
        self.builder.finish_node();
    }

    /// Groups a run of unexpected top-level tokens into an [`SyntaxKind::ErrorNode`]
    /// and synchronizes to the next `;` / `,` / `}` / declaration keyword.
    fn parse_error_run(&mut self) {
        let start = self.offset();
        self.builder.start_node(SyntaxKind::ErrorNode);
        // Consume at least one token so we always make progress.
        loop {
            self.bump_trivia();
            match self.current() {
                SyntaxKind::Eof => break,
                // Sync points: stop *before* the boundary so the root loop or the
                // item parser handles it, except `;`/`,` which we absorb.
                SyntaxKind::Semi | SyntaxKind::Comma => {
                    self.bump();
                    break;
                }
                SyntaxKind::RBrace | SyntaxKind::RParen | SyntaxKind::RBracket => break,
                SyntaxKind::LBrace => break,
                k if k.is_item_start() => break,
                _ => self.bump(),
            }
        }
        self.builder.finish_node();
        // The error node always spans at least the token(s) we consumed.
        let end = self.offset();
        self.errors.push(ParseError {
            range: TextRange::new(start, end),
            kind: ParseErrorKind::UnexpectedTokens,
        });
    }

    /// Records a structural error spanning from `offset` to the current cursor
    /// (clamped so it is never inverted).
    fn push_error(&mut self, offset: TextSize, kind: ParseErrorKind) {
        let now = self.offset();
        let end = if now.to_u32() >= offset.to_u32() {
            now
        } else {
            offset
        };
        self.errors.push(ParseError {
            range: TextRange::new(offset, end),
            kind,
        });
    }
}
