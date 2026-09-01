//! `viso-dsl` — the `.vs` UI/DSL toolchain (Part XIV).
//!
//! Pipeline: tokenizer → CST → AST → HIR → type check → module graph → UI IR,
//! plus hot-reload diff, state-migration metadata, and source maps. `.vs` is
//! the sole canonical DSL extension (AGENTS §1). In release, the UI runtime
//! does not require parsing `.vs` (§2.3): the DSL lowers to the same static
//! template + binding metadata the `#[component]` macro produces.
//!
//! It works against a widget schema/registry, never concrete widget types
//! (§10.1), so it does not depend on `viso-widgets`.
//!
//! Phase 0 status: contract-only skeleton.

#![forbid(unsafe_op_in_unsafe_fn)]

/// The canonical DSL source-file extension.
pub const SOURCE_EXTENSION: &str = "vs";
