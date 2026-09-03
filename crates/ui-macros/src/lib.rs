//! The `ui!` procedural macro (AGENTS section 21.5): the Rust-side ViewFragment
//! entry point of the Viso DSL.
//!
//! A proc-macro crate is a compile-time dylib, so — unlike the leaf `viso-macros`
//! derive crate — it MAY carry an ordinary (non-proc-macro) library dependency. This
//! crate uses that to drive the *shared* Viso DSL frontend at Rust compile time: it
//! depends on `viso-dsl` to tokenize / parse / resolve / lower a `ui! { … }` body
//! through the exact same pipeline `component!` / `view!` use, then emits a static
//! `viso_ui` builder closure. No runtime parse, no VDOM rebuild (section 59), no
//! macro-only DSL semantics (section 21.5).
//!
//! The emitted tokens name `::viso_ui::…` paths; this crate does not depend on
//! `viso-ui`. The facade `viso` re-exports `ui!` and already depends on `viso-ui`, so
//! the emitted paths resolve at the call site (the `GpuInstance` re-export precedent).
//!
//! Pipeline (identical checks/IR to the component frontend):
//! `ui! { … }` tokens
//!   -> re-stringified ViewFragment source
//!   -> `viso_dsl::tokenize` + `parse_entry(Entry::ViewFragment)`
//!   -> `ViewFragment::cast`
//!   -> gather candidate reactive-source names (value-position path heads)
//!   -> `resolve_fragment` (mints one `SymbolId` per candidate) + `lower_fragment_items`
//!   -> `lower_bindings` + `analyze_keys` over a `SourceSet` `ReadEnv`
//!   -> diagnostics surfaced as `compile_error!`
//!   -> `emit::emit_fragment` -> a `::viso_ui::BuildCx` builder closure.
//!
//! A compiler-known typed binding never silently falls back to dynamic tracking
//! (section 10.3): each reactive read the caller named becomes a static `cx.bind`
//! edge; `dynamic` is a separate, explicit escape hatch.

mod emit;

use std::collections::{BTreeSet, HashMap};

use proc_macro::TokenStream;
use quote::quote;
use syn::Ident;

use viso_dsl::ast::{AstNode, PathExpr, ViewFragment};
use viso_dsl::diag::{Diagnostic, Severity};
use viso_dsl::hir::SourceSet;
use viso_dsl::ir::{analyze_keys, lower_bindings, lower_fragment_items};
use viso_dsl::resolve::{NameInterner, resolve_fragment};
use viso_dsl::syntax::grammar::{Entry, parse_entry};
use viso_dsl::syntax::{SyntaxKind, SyntaxNode, tokenize};

/// The synthetic package a bare `ui!` fragment resolves against. It has no
/// surrounding component / compilation unit, so its reactive-source symbols are
/// minted under this fixed package (matching the fragment-frontend tests).
const FRAGMENT_PACKAGE: &str = "<ui!>";

/// Lowers a `ui! { … }` ViewFragment body to a static `::viso_ui` builder closure.
///
/// See the crate docs for the full pipeline. On any frontend error (parse, resolve,
/// or a deferred control-flow region) the macro expands to a `compile_error!` rather
/// than mounting a wrong tree.
#[proc_macro]
pub fn ui(input: TokenStream) -> TokenStream {
    // The `ui!` body is a ViewFragment: the token text IS the DSL source. Rendering
    // the TokenStream back to a string round-trips through the shared lexer, so the
    // fragment frontend sees exactly what a `.vs` fragment would.
    let source = input.to_string();

    let parse = parse_entry(&tokenize(&source), &source, Entry::ViewFragment);
    let root = SyntaxNode::new_root(parse.root);
    let Some(fragment) = ViewFragment::cast(root.clone()) else {
        return compile_error("`ui!` body is not a valid view fragment");
    };

    // Candidate reactive sources: every value-position path head in the fragment.
    // Node types parse as their own token, never as a `PathExpr`, so every `PathExpr`
    // head is a genuine value read. `resolve_fragment` mints one `SymbolId` per
    // candidate 1:1 in order; a candidate that turns out to be a loop-local or node
    // name resolves to a `Local` in the walker and simply produces no edge.
    let candidates = candidate_sources(&root);
    let candidate_refs: Vec<&str> = candidates.iter().map(String::as_str).collect();

    let mut interner = NameInterner::new();
    let resolved = resolve_fragment(&fragment, &candidate_refs, &mut interner, FRAGMENT_PACKAGE);

    let tree = lower_fragment_items(fragment.items());
    let env = SourceSet::new(resolved.sources.iter().copied());
    let bindings = lower_bindings(&tree, &root, &resolved.refs, &env);
    let keys = analyze_keys(&tree, &root, &resolved.refs, &env);

    // Surface every fatal diagnostic from the shared frontend. Warnings (e.g. the
    // keyless-stateful-`for` strict finding, E3402) do not abort the build — they are
    // recorded in the IR and left for a lint/strict pass, matching the component
    // frontend's non-fatal handling.
    let mut errors: Vec<String> = Vec::new();
    collect_errors(&parse.errors, &mut errors);
    collect_errors(&resolved.errors, &mut errors);
    collect_errors(&keys.diagnostics, &mut errors);
    if !errors.is_empty() {
        return compile_error(&errors.join("\n"));
    }

    // Map each minted source symbol back to the user's in-scope Rust `StateId`
    // identifier, so the emitter's `cx.bind(<ident>, …)` resolves by hygiene at the
    // call site. `resolved.sources[i]` corresponds to `candidates[i]`.
    let mut sources: HashMap<_, Ident> = HashMap::new();
    for (name, id) in candidates.iter().zip(resolved.sources.iter()) {
        // A candidate name is a bare identifier from the source; it is a valid Rust
        // ident by construction. Keep the first mapping if a name repeats.
        if let Ok(ident) = syn::parse_str::<Ident>(name) {
            sources.entry(*id).or_insert(ident);
        }
    }

    match emit::emit_fragment(&tree, &bindings, &sources) {
        Ok(tokens) => tokens.into(),
        Err(message) => compile_error(&message),
    }
}

/// Collects the fatal (`Severity::Error`) diagnostics from `diags` into `out` as
/// rendered messages. Non-error severities are left for later lint passes.
fn collect_errors(diags: &[Diagnostic], out: &mut Vec<String>) {
    for d in diags {
        if d.severity == Severity::Error {
            out.push(format!("{}: {}", d.code, d.message));
        }
    }
}

/// Every candidate reactive-source name in the fragment: the head segment of each
/// `PathExpr`, in first-appearance order, deduplicated. This is the set the caller's
/// Rust scope must supply as `StateId`s; the frontend decides which are actually
/// reactive (a node/loop-local resolves to a `Local` and yields no edge).
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

/// Renders a `compile_error!("…")` invocation carrying `message`, so a frontend
/// failure surfaces at the `ui!` call site as an ordinary Rust compile error.
fn compile_error(message: &str) -> TokenStream {
    // No trailing `;`: `ui!` is used in expression position (`let b = ui! { … };`), so
    // the expansion must itself be an expression. A bare `compile_error!(…)` call is a
    // valid expression and equally valid as a statement.
    quote! { ::core::compile_error!(#message) }.into()
}
