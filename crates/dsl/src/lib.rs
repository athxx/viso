//! `viso-dsl` — the `.vs` UI/DSL toolchain.
//!
//! Pipeline: tokenizer → CST → AST → HIR → type check → module graph → UI IR,
//! plus hot-reload diff, state-migration metadata, and source maps. `.vs` is
//! the sole canonical DSL extension. In release, the UI runtime
//! does not require parsing `.vs`: the DSL lowers to the same static
//! template + binding metadata the `#[component]` macro produces.
//!
//! It works against a widget schema/registry, never concrete widget types,
//! so it does not depend on `viso-widgets`.
//!
//! The frontend's lexical layer — a streaming tokenizer, a lossless green-tree
//! CST, a coarse parser skeleton, and single-edit incremental re-lex — lives in
//! [`syntax`], with the token stream and CST primitives the rest of the pipeline
//! consumes.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod aot;
pub mod assets;
pub mod ast;
pub mod behavior;
pub mod diag;
pub mod dump;
pub mod edit;
pub mod export;
pub mod format;
pub mod frontend;
pub mod hir;
pub mod hotreload;
pub mod i18n;
pub mod inspect;
pub mod ir;
pub mod package;
pub mod resolve;
pub mod scenario;
pub mod schema;
pub mod syntax;
pub mod view_behavior;
pub mod view_regions;

pub use diag::{Diagnostic, Severity};
pub use hir::{Ty, TypeError, WidenError};
pub use syntax::{
    Edit, GreenBuilder, GreenNode, LexError, Lexer, LineIndex, Parse, SyntaxKind, TextRange,
    TextSize, Token, parse, reparse_tokens, tokenize,
};

/// The canonical DSL source-file extension.
pub const SOURCE_EXTENSION: &str = "vs";
