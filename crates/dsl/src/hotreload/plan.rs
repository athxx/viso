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

use viso_ui::StateValue;

use crate::behavior::ir::{FuncId, FunctionKind};
use crate::diag::{Diagnostic, Related};
use crate::frontend::{Compiled, Origin, Source, SourceKind, compile_file, compile_fragment};
use crate::hir::{Ty, TypeSchemas};
use crate::ir::binding_ir::BindingIr;
use crate::ir::ui_ir::UiTree;
use crate::resolve::SymbolId;
use crate::syntax::TextRange;
use crate::view_behavior::{ViewBehavior, state_value, view_behavior};

/// A successfully compiled and validated reload candidate — pure data, no live
/// tree touched.
///
/// The three arrays are index-aligned only in the sense that [`sources`] and
/// [`source_names`] are 1:1 (name `source_names[i]` minted `sources[i]`); the
/// tree and bindings key into the source set by [`SymbolId`].
#[derive(Debug, Clone, Default)]
pub struct CandidatePlan {
    /// The recompiled static template.
    pub tree: UiTree,
    /// The recompiled reactive binding edges (`SymbolId → node/DirtyClass`).
    pub bindings: BindingIr,
    /// Each reactive source's compile-stable identity, in first-appearance order.
    pub sources: Vec<SymbolId>,
    /// The source name that minted each identity, aligned 1:1 with [`sources`].
    pub source_names: Vec<String>,
    /// Each source's initial cell value, aligned 1:1 with [`sources`]: a state's
    /// constant initializer, `None` for a source the reload does not initialize.
    pub initials: Vec<Option<StateValue>>,
    /// Each source's type, aligned 1:1 with [`sources`]: a state's inferred
    /// type, [`Ty::Unknown`] for any other source.
    pub types: Vec<Ty>,
    /// Where each source is declared, aligned 1:1 with [`sources`].
    pub declared: Vec<TextRange>,
    /// The record and enum declarations the types name.
    pub schemas: TypeSchemas,
    /// The behavior function computing each defaulted record field, by record
    /// and field index, ascending.
    pub field_defaults: Vec<((SymbolId, u32), FuncId)>,
    /// The `@migrate` functions a retyped state may be carried by.
    pub migrators: Vec<MigrateFn>,
    /// The view's behavior, `None` when no node declares a handler.
    pub view: Option<ViewBehavior>,
}

/// A `@migrate(from: "T")` function of a candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct MigrateFn {
    /// The old state type it migrates, as source spells it.
    pub from: String,
    /// The type of its parameter.
    pub param: Ty,
    /// The new state type it returns.
    pub ret: Ty,
    /// The behavior function.
    pub func: FuncId,
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

    /// The initial cell value of source `symbol`, if the reload initializes it.
    pub fn initial(&self, symbol: SymbolId) -> Option<StateValue> {
        let index = self.sources.iter().position(|s| *s == symbol)?;
        self.initials.get(index).copied().flatten()
    }

    /// The type and declaration of source `symbol`.
    pub fn declaration(&self, symbol: SymbolId) -> Option<(&Ty, TextRange)> {
        let index = self.sources.iter().position(|s| *s == symbol)?;
        Some((self.types.get(index)?, *self.declared.get(index)?))
    }

    /// The behavior function computing field `index` of record `record`'s
    /// default.
    pub fn field_default(&self, record: SymbolId, index: u32) -> Option<FuncId> {
        self.field_defaults
            .binary_search_by_key(&(record, index), |&(at, _)| at)
            .ok()
            .map(|at| self.field_defaults[at].1)
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
///
/// A fragment's Rust-scope components mount only through `ui!`, so each is
/// `E2001` here: the fragment has no Rust scope to name them in.
pub fn plan(source: &str) -> Result<CandidatePlan, Vec<Diagnostic>> {
    let mut compiled = compile_fragment(source);
    let unknown = std::mem::take(&mut compiled.rust_components);
    compiled.diagnostics.extend(
        unknown.into_iter().map(|(name, at)| {
            Diagnostic::error("E2001", at, format!("no widget is named `{name}`"))
        }),
    );
    candidate(compiled)
}

/// Compile and validate a `.vs` file into a [`CandidatePlan`] for its component's
/// view, handlers included, or return the fatal diagnostics. Pure, like [`plan`].
pub fn plan_view(source: &str, origin: &Origin) -> Result<CandidatePlan, Vec<Diagnostic>> {
    candidate(compile_file(source, origin))
}

/// The candidate for a compiled source: its fatal diagnostics, including every
/// handler that does not mount and every identity two sources share, or its
/// plan.
fn candidate(compiled: Compiled) -> Result<CandidatePlan, Vec<Diagnostic>> {
    if compiled.has_errors() {
        return Err(compiled.errors().cloned().collect());
    }
    let collisions = collisions(&compiled);
    if !collisions.is_empty() {
        return Err(collisions);
    }
    let fallback = compiled
        .component
        .as_ref()
        .map_or(TextRange::empty(0.into()), |component| {
            component.source_origin
        });
    let view = view_behavior(&compiled).map_err(|errors| {
        errors
            .iter()
            .map(|error| error.diagnostic(fallback))
            .collect::<Vec<_>>()
    })?;
    let migrators = migrators(&compiled);
    let mut sources = Vec::with_capacity(compiled.sources.len());
    let mut source_names = Vec::with_capacity(compiled.sources.len());
    let mut initials = Vec::with_capacity(compiled.sources.len());
    let mut types = Vec::with_capacity(compiled.sources.len());
    let mut declared = Vec::with_capacity(compiled.sources.len());
    for source in compiled.sources {
        let (initial, ty, at) = match source.kind {
            SourceKind::State {
                initial,
                ty,
                declared,
            } => (initial.as_ref().and_then(state_value), ty, declared),
            _ => (None, Ty::Unknown, fallback),
        };
        sources.push(source.symbol);
        source_names.push(source.name);
        initials.push(initial);
        types.push(ty);
        declared.push(at);
    }
    Ok(CandidatePlan {
        tree: compiled.tree,
        bindings: compiled.bindings,
        sources,
        source_names,
        initials,
        types,
        declared,
        schemas: compiled.types,
        migrators,
        field_defaults: compiled.behavior.field_defaults,
        view,
    })
}

/// The `@migrate` functions of `compiled` that lowered to a runnable body.
fn migrators(compiled: &Compiled) -> Vec<MigrateFn> {
    compiled
        .migrators
        .iter()
        .filter_map(|m| {
            let at = compiled.behavior.functions.iter().position(|f| {
                f.symbol == Some(m.symbol) && f.kind == FunctionKind::Fn && f.body.is_ok()
            })?;
            Some(MigrateFn {
                from: m.from.clone(),
                param: m.param.clone(),
                ret: m.ret.clone(),
                func: FuncId(at as u32),
            })
        })
        .collect()
}

/// An `E5102` for each source whose identity an earlier source of the view
/// already holds: the reload would migrate two states into one cell. The
/// states of region content are not cells: each mount holds its own.
fn collisions(compiled: &Compiled) -> Vec<Diagnostic> {
    let all = &compiled.sources;
    let mut out = Vec::new();
    for (index, source) in all.iter().enumerate() {
        let Some(first) = all[..index].iter().find(|s| s.symbol == source.symbol) else {
            continue;
        };
        let at = |s: &Source| match s.kind {
            SourceKind::State { declared, .. } => declared,
            _ => TextRange::empty(0.into()),
        };
        let mut diagnostic = Diagnostic::error(
            "E5102",
            at(source),
            format!(
                "the state `{}` has the same stable identity as `{}`",
                source.name, first.name
            ),
        );
        diagnostic
            .related
            .push(Related::new(at(first), "the identity is first held here"));
        out.push(diagnostic);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Severity;

    #[test]
    fn two_states_of_one_identity_are_rejected() {
        let origin = Origin {
            package: "app".into(),
            module: vec!["view".into()],
            language: None,
        };
        let mut compiled = compile_file(
            "component C { state a = 0; state b = 0; view { Text {} } }",
            &origin,
        );
        assert!(collisions(&compiled).is_empty());
        let first = compiled.sources[0].symbol;
        compiled.sources[1].symbol = first;
        let [error] = <[Diagnostic; 1]>::try_from(collisions(&compiled)).expect("one collision");
        assert_eq!((error.code, error.severity), ("E5102", Severity::Error));
        assert!(error.message.contains("`b`") && error.message.contains("`a`"));
        assert_eq!(error.related.len(), 1);
        assert!(candidate(compiled).is_err(), "the candidate is rejected");
    }

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
