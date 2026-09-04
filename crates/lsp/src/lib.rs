//! `viso-lsp` — the formatter and language server for `.vs` (Slice R).
//!
//! This crate delivers the Phase 6 tooling exit criterion (doc section 71:
//! "formatter / LSP / goto / rename / reference usable"). It is built in two
//! layers, deliberately separated:
//!
//! 1. a **pure analysis engine** ([`engine`]) — goto-definition, find-references,
//!    rename, and formatting expressed as plain functions from source text and a
//!    position to spans/edits, with **zero protocol dependencies**. Every one is
//!    headless unit-testable without an editor or a JSON-RPC transport.
//! 2. a **thin stdio JSON-RPC frontend** (the `viso-lsp` bin) that frames LSP
//!    messages and dispatches them to the engine. It owns no analysis logic and
//!    **no async runtime** — a synchronous `Content-Length`-framed loop, because a
//!    frontend tool adapts a protocol, it does not host an executor (AGENTS 25).
//!
//! Everything reuses the `viso-dsl` frontend: the lossless CST (byte-exact
//! `root.text()` round-trip), the name resolver's use→def edges plus the
//! [`SymbolDecl`](viso_dsl::resolve::SymbolDecl) definition spans it now records,
//! and [`LineIndex`](viso_dsl::LineIndex) for byte↔line/column↔UTF-16 conversion.
//! Slice R is thin adaptation over that frontend, not a second compiler.
//!
//! All of this is cold-path tooling (AGENTS 7.2): `HashMap`, owned `String`, and
//! `Vec` are the right tools here; there is no steady-state frame cost to guard.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod engine;
pub mod format;
pub mod index;
pub mod position;
pub mod rpc;
pub mod server;
pub mod source_map;

pub use engine::{
    Location, RenameError, TextEdit, find_references, format, goto_definition, rename,
};
pub use index::ReferenceIndex;
pub use position::{LspPosition, LspRange};
pub use rpc::Json;
pub use server::Server;
pub use source_map::{FileId, OpenDoc, SourceMap};
