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

use crate::ast::{AstNode, PathExpr, ViewFragment};
use crate::diag::{Diagnostic, Severity};
use crate::ir::binding_ir::BindingIr;
use crate::ir::ui_ir::UiTree;
use crate::ir::{analyze_keys, lower_bindings, lower_fragment_items};
use crate::resolve::{NameInterner, SymbolId, resolve_fragment};
use crate::syntax::grammar::{Entry, parse_entry};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange, TextSize, tokenize};

use std::collections::BTreeSet;

/// The synthetic package a bare fragment resolves against.
///
/// This MUST be byte-identical to the `ui!` proc-macro's `FRAGMENT_PACKAGE`
/// (`crates/ui-macros/src/lib.rs`): the resolver fingerprints a source's
/// identity from `(package, name)`, so a `ui!`-built live tree and a
/// hot-reloaded candidate agree on a state's [`SymbolId`] only if both resolve
/// under the same package anchor. That agreement is what lets the migration
/// stage match a live state cell to its recompiled counterpart by identity.
const FRAGMENT_PACKAGE: &str = "<ui!>";

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
/// [`Severity::Error`] from parse, resolution, or key analysis is fatal — the
/// whole set is returned so the caller reports every problem at once and keeps
/// the last-good UI (the transaction never reaches commit). Warnings (e.g. a
/// keyless stateful `for`) are non-fatal and left in the IR for a lint pass,
/// matching the build-time frontend.
pub fn plan(source: &str) -> Result<CandidatePlan, Vec<Diagnostic>> {
    let parse = parse_entry(&tokenize(source), source, Entry::ViewFragment);
    let root = SyntaxNode::new_root(parse.root);

    let Some(fragment) = ViewFragment::cast(root.clone()) else {
        // A body that will not even cast to a fragment is a hard structural
        // error; surface it as a fatal diagnostic with the whole-source span.
        let whole = TextRange::new(TextSize::ZERO, TextSize::new(source.len() as u32));
        return Err(vec![Diagnostic::error(
            "E4200",
            whole,
            "hot reload source is not a valid view fragment",
        )]);
    };

    // Candidate reactive sources: the head segment of every value-position path,
    // first-appearance order, deduplicated — the same set the proc-macro derives,
    // so the resolver mints the same per-name identities.
    let source_names = candidate_sources(&root);
    let candidate_refs: Vec<&str> = source_names.iter().map(String::as_str).collect();

    let mut interner = NameInterner::new();
    let resolved = resolve_fragment(&fragment, &candidate_refs, &mut interner, FRAGMENT_PACKAGE);

    let tree = lower_fragment_items(fragment.items());
    let env = crate::hir::SourceSet::new(resolved.sources.iter().copied());
    let bindings = lower_bindings(&tree, &root, &resolved.refs, &env);
    let keys = analyze_keys(&tree, &root, &resolved.refs, &env);

    // Gather every fatal diagnostic across the frontend stages. One fatal → the
    // candidate is rejected; the caller keeps last-good.
    let mut fatal: Vec<Diagnostic> = Vec::new();
    collect_fatal(&parse.errors, &mut fatal);
    collect_fatal(&resolved.errors, &mut fatal);
    collect_fatal(&keys.diagnostics, &mut fatal);
    if !fatal.is_empty() {
        return Err(fatal);
    }

    Ok(CandidatePlan {
        tree,
        bindings,
        sources: resolved.sources,
        source_names,
    })
}

/// Move the `Severity::Error` diagnostics from `diags` into `out`, cloning them so
/// the returned set owns its spans/codes. Non-error severities are left behind.
fn collect_fatal(diags: &[Diagnostic], out: &mut Vec<Diagnostic>) {
    for d in diags {
        if d.severity == Severity::Error {
            out.push(d.clone());
        }
    }
}

/// Every candidate reactive-source name in the fragment: the head segment of each
/// `PathExpr`, first-appearance order, deduplicated. Mirrors the proc-macro's
/// `candidate_sources` so both paths derive the same identities from the same
/// source.
fn candidate_sources(root: &SyntaxNode) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut names = Vec::new();
    for node in root.descendants() {
        if node.kind() != SyntaxKind::PathExpr {
            continue;
        }
        let Some(path) = PathExpr::cast(node) else {
            continue;
        };
        if let Some(head) = path.segments().next() {
            let text = head.text();
            if seen.insert(text.clone()) {
                names.push(text);
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

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
