//! Compile candidate + validate — the first, purely functional stage of the hot
//! reload transaction (architecture section 42; AGENTS 21.7).
//!
//! A hot reload is not a rebuild. It is a transaction whose early stages are all
//! pure functions of the new source and the prior compiled state; nothing here
//! touches the live UI tree. `plan` drives exactly the shared fragment frontend
//! the `ui!` proc-macro drives (tokenize → parse → resolve → lower UI/Binding IR
//! → key analysis), which is the runtime form of the "three source forms share
//! one frontend" contract (section 21.5). It collects every fatal diagnostic and,
//! if any is present, returns `Err` — the caller short-circuits before commit, so
//! the live tree is left at its last-good state without any snapshot (the
//! keep-last-good invariant; see the module docs).
//!
//! The output [`CandidatePlan`] is plain data: the new template [`UiTree`], its
//! compiled [`BindingIr`], and the reactive-source identities the resolver minted
//! (each a name-derived, compile-stable [`SymbolId`]). Those identities are the
//! durable keys the diff and migration stages align old and new state against.

use crate::diag::Diagnostic;
use crate::frontend::compile_fragment;
use crate::ir::binding_ir::BindingIr;
use crate::ir::ui_ir::UiTree;
use crate::resolve::SymbolId;

/// A successfully compiled and validated reload candidate — pure data, no live
/// tree touched.
///
/// The three arrays are index-aligned only in the sense that [`sources`] and
/// [`source_names`] are 1:1 (name `source_names[i]` minted `sources[i]`); the
/// tree and bindings key into the source set by [`SymbolId`].
#[derive(Debug, Clone)]
pub struct CandidatePlan {
    /// The recompiled static template.
    pub tree: UiTree,
    /// The recompiled reactive binding edges (`SymbolId → node/DirtyClass`).
    pub bindings: BindingIr,
    /// Each reactive source's compile-stable identity, in first-appearance order.
    pub sources: Vec<SymbolId>,
    /// The source name that minted each identity, aligned 1:1 with [`sources`].
    pub source_names: Vec<String>,
}

impl CandidatePlan {
    /// The identity a source name resolves to in this candidate, if the name is a
    /// reactive source here. Cold path (reload only).
    pub fn symbol_for_name(&self, name: &str) -> Option<SymbolId> {
        self.source_names
            .iter()
            .position(|n| n == name)
            .map(|i| self.sources[i])
    }
}

/// Compile and validate a fragment source into a [`CandidatePlan`], or return the
/// fatal diagnostics that make it uncompilable.
///
/// Pure: it reads only `source` and allocates only its own IR. Any
/// [`Severity::Error`](crate::diag::Severity::Error) from parse, resolution, or
/// key analysis is fatal — the whole set is returned so the caller reports every
/// problem at once and keeps the last-good UI (the transaction never reaches
/// commit). Warnings (e.g. a
/// keyless stateful `for`) are non-fatal and left in the IR for a lint pass,
/// matching the build-time frontend.
pub fn plan(source: &str) -> Result<CandidatePlan, Vec<Diagnostic>> {
    let compiled = compile_fragment(source);
    if compiled.has_errors() {
        return Err(compiled.errors().cloned().collect());
    }
    let (sources, source_names) = compiled
        .sources
        .into_iter()
        .map(|source| (source.symbol, source.name))
        .unzip();
    Ok(CandidatePlan {
        tree: compiled.tree,
        bindings: compiled.bindings,
        sources,
        source_names,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Severity;

    #[test]
    fn valid_fragment_compiles_to_a_plan() {
        let plan = plan("Text { text: label; }").expect("valid fragment compiles");
        assert_eq!(plan.tree.items.len(), 1, "one root node");
        assert!(
            plan.source_names.iter().any(|n| n == "label"),
            "the bound source name is a candidate"
        );
        assert_eq!(
            plan.sources.len(),
            plan.source_names.len(),
            "identities are 1:1 with names"
        );
    }

    #[test]
    fn same_name_mints_the_same_identity_across_compiles() {
        // The migration key depends on this: recompiling a fragment that still
        // reads `count` must resolve `count` to the identical SymbolId.
        let a = plan("Text { text: count; }").expect("compiles");
        let b = plan("Text { text: count; color: count; }").expect("compiles");
        assert_eq!(
            a.symbol_for_name("count"),
            b.symbol_for_name("count"),
            "a source name is compile-stable identity"
        );
    }

    #[test]
    fn malformed_source_is_rejected_without_a_plan() {
        let err = plan("Text { text: ;;; }").expect_err("malformed fragment is fatal");
        assert!(!err.is_empty(), "carries at least one fatal diagnostic");
        assert!(
            err.iter().all(|d| d.severity == Severity::Error),
            "only fatal diagnostics are returned"
        );
    }
}
