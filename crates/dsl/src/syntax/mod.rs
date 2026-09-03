//! The `.vs` frontend's lexical layer: a streaming tokenizer, a lossless
//! green-tree CST, a coarse parser skeleton, and single-edit incremental re-lex.
//!
//! This is the primitive layer every downstream stage (AST, name resolution,
//! typed HIR, the UI/Binding/Shader IR, plus the formatter, language server, and
//! hot reloader) is built on. Its two contracts are **losslessness** — the CST
//! reconstructs the source byte-for-byte, trivia and all — and **recovery** — no
//! input, however malformed or truncated, makes the lexer panic or the parser
//! stop at the first error.
//!
//! Pipeline position (AGENTS 21.2):
//! `source → [tokenizer] → [lossless CST] → AST → …`. The typed grammar, AST,
//! and name resolution are the next slice; this slice delivers the token stream,
//! the green tree, and a coarse declaration/brace-granularity parse.
//!
//! Coordinate model: tokens store a byte-offset [`TextRange`]; a per-source
//! [`LineIndex`] derives line/column, Unicode-scalar, and UTF-16 coordinates on
//! demand, so the hot token vector stays lean while editors still get the three
//! coordinate systems they need.

pub mod cst;
pub mod grammar;
pub mod kind;
pub mod lexer;
pub mod parser;
pub mod red;
pub mod reparse;
pub mod span;
pub mod token;

pub use cst::{GreenBuilder, GreenChild, GreenNode, GreenToken, flat_tree};
pub use grammar::{Entry, parse_entry};
pub use kind::SyntaxKind;
pub use lexer::{LexState, Lexer, tokenize};
pub use parser::{Parse, ParseErrorKind, parse};
pub use red::{Ancestors, SyntaxElement, SyntaxNode, SyntaxToken};
pub use reparse::{Edit, reparse_tokens};
pub use span::{LineCol, LineIndex, TextRange, TextSize};
pub use token::{LexError, Token};
