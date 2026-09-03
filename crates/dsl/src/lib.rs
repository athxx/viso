//! `viso-dsl` — the `.vs` UI/DSL toolchain (Part XIV).
//!
//! Pipeline: tokenizer → CST → AST → HIR → type check → module graph → UI IR,
//! plus hot-reload diff, state-migration metadata, and source maps. `.vs` is
//! the sole canonical DSL extension (AGENTS section 1). In release, the UI runtime
//! does not require parsing `.vs` (section 2.3): the DSL lowers to the same static
//! template + binding metadata the `#[component]` macro produces.
//!
//! It works against a widget schema/registry, never concrete widget types
//! (section 10.1), so it does not depend on `viso-widgets`.
//!
//! Phase 6 status: the frontend's lexical layer has landed — a streaming
//! tokenizer, a lossless green-tree CST, a coarse parser skeleton, and
//! single-edit incremental re-lex. See [`syntax`] for the token stream and CST
//! primitives the rest of the pipeline consumes.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod ast;
pub mod resolve;
pub mod syntax;

pub use syntax::{
    Edit, GreenBuilder, GreenNode, LexError, Lexer, LineIndex, Parse, ParseError, SyntaxKind,
    TextRange, TextSize, Token, parse, reparse_tokens, tokenize,
};

/// The canonical DSL source-file extension.
pub const SOURCE_EXTENSION: &str = "vs";
