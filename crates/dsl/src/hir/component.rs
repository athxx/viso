//! Component lowering — member classification, state ordering, computed topology.
//!
//! A `component` (or `system`) body is a flat list of members in source order; lowering
//! turns it into a [`ComponentSchema`] with the members bucketed by kind and two static
//! invariants discharged (spec state and computed sections):
//!
//! - **State initialization order.** A `state`'s initializer may read only *source-earlier*
//!   state; reading a *later* state is `E2104` (there is no "dependencies are clear so a
//!   forward read is fine" exception — a forward derivation must be a `computed`). Omitting a
//!   private state's type is allowed only when its initializer types to a single concrete
//!   type; a type that cannot be uniquely inferred is a compile error.
//! - **Computed dependency graph.** A `computed` may reference source-earlier *or* later
//!   computeds, so lowering builds the whole computed-on-computed dependency graph,
//!   topologically sorts the computeds into evaluation order, and reports a cycle as `E2105`
//!   with the full cycle path carried in the diagnostic's related spans.
//!
//! Both passes need to know, for a member's initializer/body, which reactive sources it
//! reads and what type it has — and the mapping from a member's *name* to its durable
//! [`SymbolId`]. Rather than pull the resolver's symbol tables and name interner into this
//! section, lowering takes those as an injected [`MemberEnv`], exactly as [`super::infer`],
//! [`super::effect`], and [`super::reads`] take their environments as traits. That keeps the
//! classification and graph passes unit-testable against a stub before the end-to-end `lower`
//! (the capability/`lower` section) wires the real tables in.

use std::collections::{BTreeSet, HashMap};

use crate::ast::{AstNode, ComponentDecl, Member};
use crate::diag::Diagnostic;
use crate::resolve::{ResolvedRef, SymbolId};
use crate::syntax::TextRange;

use super::infer::{InferCx, TypeEnv};
use super::nodes::{
    CallableKind, ComponentSchema, HirCallable, HirComputed, HirEvent, HirInput, HirMeta, HirState,
    OwnershipMode,
};
use super::reads::{ReadEnv, collect_reads};
use super::ty::{Ty, TypeError};

/// What component lowering needs from the surrounding program, beyond the type and read
/// environments the expression passes use. Supplying this as a trait keeps the
/// classification and graph passes independent of the resolver's symbol tables and name
/// interner: the end-to-end `lower` implements it over the real tables, tests over a stub.
pub trait MemberEnv: TypeEnv + ReadEnv {
    /// The durable symbol a member of *this component* declares, given the member's name.
    /// `None` when the name did not resolve to a member symbol (a malformed or duplicate
    /// declaration the resolver already diagnosed); such a member is lowered with no symbol
    /// and contributes no dependency edge.
    fn member_symbol(&self, name: &str) -> Option<SymbolId>;

    /// The component's own declaration symbol.
    fn component_symbol(&self) -> SymbolId;
}

/// Lowers one component declaration into its typed [`ComponentSchema`], appending any
/// `E2103`/`E2104`/`E2105` (and the numeric `E2101`/`E2102`) diagnostics the passes raise to
/// `diagnostics`. The `refs` are the enclosing module's resolved references (so an
/// initializer/body expression's names resolve); `env` answers the type/read/symbol
/// questions.
pub fn lower_component(
    decl: &ComponentDecl,
    refs: &[ResolvedRef],
    env: &dyn MemberEnv,
    diagnostics: &mut Vec<Diagnostic>,
) -> ComponentSchema {
    let name = decl
        .name()
        .map(|t| t.text().to_string())
        .unwrap_or_default();

    let mut schema = ComponentSchema {
        name,
        symbol: env.component_symbol(),
        inputs: Vec::new(),
        states: Vec::new(),
        computeds: Vec::new(),
        events: Vec::new(),
        callables: Vec::new(),
        view: None,
    };

    // Classify members into buckets in source order, lowering each to its node. State and
    // computed carry the extra ordering/topology work below, so collect their raw shapes
    // (declaration + symbol + reactive reads) alongside the node as we go.
    let mut state_order: Vec<StateEntry> = Vec::new();
    let mut computed_order: Vec<ComputedEntry> = Vec::new();

    for member in decl.members() {
        match member {
            Member::Input(input) => {
                let member_name = token_text(input.name());
                let symbol = env.member_symbol(&member_name);
                let ty = input
                    .ty()
                    .map(|t| resolve_annotation(&t, input.syntax().text_range(), diagnostics))
                    .unwrap_or(Ty::Unknown);
                let meta = decl_meta(
                    symbol,
                    ty,
                    super::effect::EffectClass::Read,
                    OwnershipMode::Borrowed,
                    input.syntax().text_range(),
                );
                schema.inputs.push(HirInput {
                    name: member_name,
                    meta,
                });
            }
            Member::State(state) => {
                let member_name = token_text(state.name());
                let symbol = env.member_symbol(&member_name);
                let annotated = state.ty();
                let type_was_annotated = annotated.is_some();
                let span = state.syntax().text_range();

                // Infer the initializer's type under the annotation (when present), so an
                // omitted type is filled and an annotated one is checked.
                let (ty, reads) = if let Some(init) = state.initializer() {
                    let expected = annotated
                        .as_ref()
                        .map(|a| resolve_annotation(a, span, diagnostics));
                    let mut cx = InferCx::new(refs, env);
                    let inferred = cx.infer_expr(&init, expected.as_ref());
                    diagnostics.extend(cx.into_diagnostics());
                    let reads = collect_reads(refs, env, &init);
                    (expected.unwrap_or(inferred), reads)
                } else {
                    // No initializer: the type must be the annotation (or unknown).
                    let ty = annotated
                        .as_ref()
                        .map(|a| resolve_annotation(a, span, diagnostics))
                        .unwrap_or(Ty::Unknown);
                    (ty, BTreeSet::new())
                };

                // A private state that omitted its type must infer a single concrete type;
                // an undetermined result is a compile error (spec state section).
                if !type_was_annotated && is_undetermined(&ty) {
                    diagnostics.push(Diagnostic::error(
                        "E2103",
                        span,
                        "cannot uniquely infer the type of this `state`; add an explicit type \
                         annotation",
                    ));
                }

                let mut meta = decl_meta(
                    symbol,
                    ty,
                    super::effect::EffectClass::Read,
                    OwnershipMode::Owned,
                    span,
                );
                meta.reactive_reads = reads.clone();
                state_order.push(StateEntry {
                    symbol,
                    reads,
                    span,
                    node: HirState {
                        name: member_name,
                        type_was_annotated,
                        meta,
                    },
                });
            }
            Member::Computed(computed) => {
                let member_name = token_text(computed.name());
                let symbol = env.member_symbol(&member_name);
                let annotated = computed.ty();
                let span = computed.syntax().text_range();

                let (ty, reads) = if let Some(body) = computed.body() {
                    let expected = annotated
                        .as_ref()
                        .map(|a| resolve_annotation(a, span, diagnostics));
                    let mut cx = InferCx::new(refs, env);
                    let inferred = cx.infer_expr(&body, expected.as_ref());
                    diagnostics.extend(cx.into_diagnostics());
                    let reads = collect_reads(refs, env, &body);
                    (expected.unwrap_or(inferred), reads)
                } else {
                    let ty = annotated
                        .as_ref()
                        .map(|a| resolve_annotation(a, span, diagnostics))
                        .unwrap_or(Ty::Unknown);
                    (ty, BTreeSet::new())
                };

                if annotated.is_none() && is_undetermined(&ty) {
                    diagnostics.push(Diagnostic::error(
                        "E2103",
                        span,
                        "cannot uniquely infer the type of this `computed`; add an explicit type \
                         annotation",
                    ));
                }

                let mut meta = decl_meta(
                    symbol,
                    ty,
                    super::effect::EffectClass::Read,
                    OwnershipMode::Derived,
                    span,
                );
                meta.reactive_reads = reads.clone();
                computed_order.push(ComputedEntry {
                    symbol,
                    reads,
                    span,
                    node: HirComputed {
                        name: member_name,
                        meta,
                    },
                });
            }
            Member::Event(event) => {
                let member_name = token_text(event.name());
                let symbol = env.member_symbol(&member_name);
                let meta = decl_meta(
                    symbol,
                    Ty::Unit,
                    super::effect::EffectClass::Pure,
                    OwnershipMode::None,
                    event.syntax().text_range(),
                );
                schema.events.push(HirEvent {
                    name: member_name,
                    meta,
                });
            }
            Member::Fn(f) => schema.callables.push(callable_node(
                token_text(f.name()),
                CallableKind::Fn,
                super::effect::EffectClass::Read,
                env.member_symbol(&token_text(f.name())),
                f.syntax().text_range(),
            )),
            Member::Action(a) => schema.callables.push(callable_node(
                token_text(a.name()),
                CallableKind::Action,
                super::effect::EffectClass::Action,
                env.member_symbol(&token_text(a.name())),
                a.syntax().text_range(),
            )),
            Member::Task(t) => schema.callables.push(callable_node(
                token_text(t.name()),
                CallableKind::Task,
                super::effect::EffectClass::Task,
                env.member_symbol(&token_text(t.name())),
                t.syntax().text_range(),
            )),
            Member::View(view) => {
                schema.view = Some(view.syntax().text_range());
            }
        }
    }

    // State ordering: an initializer may read only source-earlier state (E2104).
    check_state_order(&state_order, diagnostics);
    schema.states = state_order.into_iter().map(|e| e.node).collect();

    // Computed topology: build the computed-on-computed graph, topologically sort, and on a
    // cycle emit E2105 with the full path. The schema's computeds are stored in evaluation
    // order (or, when a cycle blocks a total order, source order for the tangled members).
    schema.computeds = order_computeds(computed_order, diagnostics);

    schema
}

/// A `state` member captured for the ordering pass: its symbol, the reactive sources its
/// initializer reads, its span, and its lowered node.
struct StateEntry {
    symbol: Option<SymbolId>,
    reads: BTreeSet<SymbolId>,
    span: TextRange,
    node: HirState,
}

/// A `computed` member captured for the topology pass.
struct ComputedEntry {
    symbol: Option<SymbolId>,
    reads: BTreeSet<SymbolId>,
    span: TextRange,
    node: HirComputed,
}

/// Checks that every `state` initializer reads only *source-earlier* state. A read of a
/// state declared later in source is `E2104` (a forward reference); the check ignores reads
/// of non-state sources (inputs/computeds), which have no ordering constraint here.
fn check_state_order(states: &[StateEntry], diagnostics: &mut Vec<Diagnostic>) {
    // The set of state symbols already declared as we scan forward.
    let mut seen: BTreeSet<SymbolId> = BTreeSet::new();
    // Every state symbol, to tell "reads a later state" apart from "reads a non-state".
    let all: BTreeSet<SymbolId> = states.iter().filter_map(|s| s.symbol).collect();

    for state in states {
        for &read in &state.reads {
            if all.contains(&read) && !seen.contains(&read) {
                // The read resolves to a state that has not been declared yet.
                diagnostics.push(Diagnostic::error(
                    "E2104",
                    state.span,
                    "a `state` initializer may only read earlier state; this reads state \
                     declared later — use a `computed` for a forward derivation",
                ));
            }
        }
        if let Some(sym) = state.symbol {
            seen.insert(sym);
        }
    }
}

/// Orders the computeds into dependency-topological (evaluation) order, emitting `E2105`
/// with the full cycle path for every cycle found. A computed depends on another computed it
/// reads; reads of non-computed sources (state/input) impose no ordering here. When a cycle
/// blocks a total order, the tangled members keep their source order in the result so the
/// schema stays well-formed for later passes.
fn order_computeds(
    computeds: Vec<ComputedEntry>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<HirComputed> {
    let n = computeds.len();
    // Map each computed's symbol to its index, to turn a read into a graph edge.
    let mut index_of: HashMap<SymbolId, usize> = HashMap::with_capacity(n);
    for (i, c) in computeds.iter().enumerate() {
        if let Some(sym) = c.symbol {
            index_of.insert(sym, i);
        }
    }

    // Edges: `deps[i]` = the computed indices `i` depends on (reads that are computeds).
    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, c) in computeds.iter().enumerate() {
        for &read in &c.reads {
            if let Some(&j) = index_of.get(&read)
                && j != i
            {
                deps[i].push(j);
            }
        }
    }

    // Topological sort via DFS with a cycle-path trace. `state[i]`: 0 = unvisited, 1 = on the
    // current DFS stack, 2 = finished. A back-edge to an on-stack node is a cycle; the path
    // from that node down the stack to the re-entry is the cycle.
    let mut visit_state = vec![0u8; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut stack: Vec<usize> = Vec::new();
    // Cycles already reported (as the sorted set of their members), to report each once.
    let mut reported: Vec<BTreeSet<usize>> = Vec::new();

    for start in 0..n {
        if visit_state[start] == 0 {
            dfs(
                start,
                &deps,
                &computeds,
                &mut visit_state,
                &mut order,
                &mut stack,
                &mut reported,
                diagnostics,
            );
        }
    }

    // Edges point dependent → dependency, so the post-order of finishes already lists each
    // dependency before the computed that reads it — exactly evaluation order, no reverse.
    // Map indices back to nodes, consuming the entries.
    let mut nodes: Vec<Option<HirComputed>> = computeds.into_iter().map(|c| Some(c.node)).collect();
    order.into_iter().filter_map(|i| nodes[i].take()).collect()
}

/// One DFS step of the computed topological sort, tracing and reporting any cycle it closes.
#[allow(clippy::too_many_arguments)]
fn dfs(
    node: usize,
    deps: &[Vec<usize>],
    computeds: &[ComputedEntry],
    visit_state: &mut [u8],
    order: &mut Vec<usize>,
    stack: &mut Vec<usize>,
    reported: &mut Vec<BTreeSet<usize>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    visit_state[node] = 1;
    stack.push(node);
    for &next in &deps[node] {
        match visit_state[next] {
            0 => dfs(
                next,
                deps,
                computeds,
                visit_state,
                order,
                stack,
                reported,
                diagnostics,
            ),
            1 => report_cycle(next, stack, computeds, reported, diagnostics),
            _ => {}
        }
    }
    stack.pop();
    visit_state[node] = 2;
    order.push(node);
}

/// Reports an `E2105` for the cycle closed by a back-edge to `entry`, which is on the current
/// DFS `stack`. The cycle is the stack slice from `entry` to the top; its full path is
/// carried in the diagnostic's related spans (each member's declaration), reported once per
/// distinct member set.
fn report_cycle(
    entry: usize,
    stack: &[usize],
    computeds: &[ComputedEntry],
    reported: &mut Vec<BTreeSet<usize>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let start = stack.iter().position(|&n| n == entry).unwrap_or(0);
    let cycle: Vec<usize> = stack[start..].to_vec();
    let member_set: BTreeSet<usize> = cycle.iter().copied().collect();
    if reported.contains(&member_set) {
        return;
    }
    reported.push(member_set);

    let mut diag = Diagnostic::error(
        "E2105",
        computeds[entry].span,
        format!(
            "`computed` members form a dependency cycle of length {}; a `computed` cannot \
             transitively depend on itself",
            cycle.len()
        ),
    );
    // The full cycle path, in traversal order, closing back to the entry.
    for &member in &cycle {
        diag.related.push((
            computeds[member].span,
            format!("`{}` is part of the cycle", computeds[member].node.name),
        ));
    }
    diag.related.push((
        computeds[entry].span,
        format!("… which depends back on `{}`", computeds[entry].node.name),
    ));
    diagnostics.push(diag);
}

/// The text of an optional name token, empty when absent.
fn token_text(tok: Option<crate::syntax::SyntaxToken>) -> String {
    tok.map(|t| t.text().to_string()).unwrap_or_default()
}

/// Whether a type is still undetermined — an inference placeholder or an outright failure —
/// so an omitted-type member that lands here is a compile error.
fn is_undetermined(ty: &Ty) -> bool {
    matches!(ty, Ty::InferInt | Ty::InferFloat | Ty::Unknown)
}

/// Resolves a type annotation to a [`Ty`], emitting `E2101` for the removed `Float` type and
/// leaving a nominal (non-builtin) name as `Unknown` this section — nominal binding lands
/// with the end-to-end `lower` that has the resolver's type namespace. Uses the annotation's
/// enclosing declaration span for the diagnostic.
fn resolve_annotation(
    path: &crate::ast::TypePath,
    span: TextRange,
    diagnostics: &mut Vec<Diagnostic>,
) -> Ty {
    match Ty::from_type_path(path) {
        Ok(Some(ty)) => ty,
        // A name that is not a builtin scalar/UI type: a nominal type resolved elsewhere.
        Ok(None) => Ty::Unknown,
        Err(err) => {
            let code: &'static str = match err {
                TypeError::FloatRemoved => "E2101",
                TypeError::UnknownType => "E2103",
            };
            diagnostics.push(Diagnostic::error(code, span, err.message()));
            Ty::Unknown
        }
    }
}

/// Builds a declaration node's metadata block.
fn decl_meta(
    symbol: Option<SymbolId>,
    inferred_type: Ty,
    effect_class: super::effect::EffectClass,
    ownership_mode: OwnershipMode,
    source_origin: TextRange,
) -> HirMeta {
    match symbol {
        Some(sym) => HirMeta::decl(
            sym,
            inferred_type,
            effect_class,
            ownership_mode,
            source_origin,
        ),
        // A member the resolver did not give a symbol (malformed/duplicate): keep the node
        // so downstream passes see the shape, with no resolved symbol.
        None => HirMeta {
            resolved_symbol: None,
            inferred_type,
            effect_class,
            capability_set: super::capability::CapabilitySet::new(),
            ownership_mode,
            reactive_reads: BTreeSet::new(),
            source_origin,
            constant_value: None,
        },
    }
}

/// Builds a callable node's shape (name, kind, effect). The body's effect/capability
/// refinement is the effect/capability sections' work; this section records the declared
/// kind so the schema is complete.
fn callable_node(
    name: String,
    kind: CallableKind,
    effect_class: super::effect::EffectClass,
    symbol: Option<SymbolId>,
    source_origin: TextRange,
) -> HirCallable {
    HirCallable {
        name,
        kind,
        meta: decl_meta(
            symbol,
            Ty::Unit,
            effect_class,
            OwnershipMode::None,
            source_origin,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::Resolution;
    use crate::syntax::{SyntaxNode, tokenize};

    /// A stub member environment: member names → symbols, reactive-source symbols, and a set
    /// of resolved-name types. Every symbol used in an initializer resolves through `refs`.
    struct StubEnv {
        members: HashMap<String, SymbolId>,
        sources: BTreeSet<SymbolId>,
        component: SymbolId,
    }

    impl TypeEnv for StubEnv {
        fn resolution_ty(&self, to: &Resolution) -> Option<Ty> {
            match to {
                // A read of a reactive source is typed I64 for these tests (so an omitted
                // type infers uniquely).
                Resolution::Symbol(id) if self.sources.contains(id) => Some(Ty::I64),
                _ => None,
            }
        }
        fn callee_signature(&self, _to: &Resolution) -> Option<(Vec<Ty>, Ty)> {
            None
        }
    }

    impl ReadEnv for StubEnv {
        fn reactive_source(&self, to: &Resolution) -> Option<SymbolId> {
            match to {
                Resolution::Symbol(id) if self.sources.contains(id) => Some(*id),
                _ => None,
            }
        }
    }

    impl MemberEnv for StubEnv {
        fn member_symbol(&self, name: &str) -> Option<SymbolId> {
            self.members.get(name).copied()
        }
        fn component_symbol(&self) -> SymbolId {
            self.component
        }
    }

    /// Parses a `component { ... }` source into its `ComponentDecl`, its module's resolved
    /// refs (every identifier use mapped to the member symbol of the same name), and a stub
    /// env seeded from `members`/`sources`.
    fn setup(
        src: &str,
        members: &[(&str, SymbolId)],
        sources: &[SymbolId],
    ) -> (SyntaxNode, ComponentDecl, Vec<ResolvedRef>, StubEnv) {
        let tokens = tokenize(src);
        let parse = crate::syntax::grammar::parse(&tokens, src);
        let root = SyntaxNode::new_root(parse.root);
        let decl = root
            .descendants()
            .into_iter()
            .find_map(ComponentDecl::cast)
            .expect("source contains a component");

        let members_map: HashMap<String, SymbolId> =
            members.iter().map(|(n, id)| (n.to_string(), *id)).collect();

        // Every identifier token whose text names a member resolves to that member's symbol.
        let mut refs = Vec::new();
        for tok in root
            .descendants_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| t.kind() == crate::syntax::SyntaxKind::Ident)
        {
            if let Some(id) = members_map.get(tok.text().as_str()) {
                refs.push(ResolvedRef {
                    range: tok.text_range(),
                    to: Resolution::Symbol(*id),
                });
            }
        }

        let env = StubEnv {
            members: members_map,
            sources: sources.iter().copied().collect(),
            component: SymbolId::from_parts(999, 0),
        };
        (root, decl, refs, env)
    }

    #[test]
    fn members_are_classified_into_buckets() {
        let src = "component C {\n  input title: String\n  state count = 0\n  \
                   computed doubled: I64 = 1\n  event tapped\n  view { }\n}";
        let (_root, decl, refs, env) = setup(
            src,
            &[
                ("title", SymbolId::from_parts(1, 0)),
                ("count", SymbolId::from_parts(2, 0)),
                ("doubled", SymbolId::from_parts(3, 0)),
                ("tapped", SymbolId::from_parts(4, 0)),
            ],
            &[],
        );
        let mut diags = Vec::new();
        let schema = lower_component(&decl, &refs, &env, &mut diags);
        assert_eq!(schema.name, "C");
        assert_eq!(schema.inputs.len(), 1);
        assert_eq!(schema.states.len(), 1);
        assert_eq!(schema.computeds.len(), 1);
        assert_eq!(schema.events.len(), 1);
        assert!(schema.view.is_some());
    }

    #[test]
    fn a_state_reading_earlier_state_is_legal() {
        // `total` reads `count`, declared before it.
        let src = "component C {\n  state count = 1\n  state total = count\n}";
        let count = SymbolId::from_parts(2, 0);
        let total = SymbolId::from_parts(3, 0);
        let (_root, decl, refs, env) =
            setup(src, &[("count", count), ("total", total)], &[count, total]);
        let mut diags = Vec::new();
        let _ = lower_component(&decl, &refs, &env, &mut diags);
        assert!(
            diags.iter().all(|d| d.code != "E2104"),
            "reading earlier state is legal, got {diags:?}"
        );
    }

    #[test]
    fn a_state_reading_later_state_is_e2104() {
        // `first` reads `second`, declared after it — a forward reference.
        let src = "component C {\n  state first = second\n  state second = 1\n}";
        let first = SymbolId::from_parts(2, 0);
        let second = SymbolId::from_parts(3, 0);
        let (_root, decl, refs, env) = setup(
            src,
            &[("first", first), ("second", second)],
            &[first, second],
        );
        let mut diags = Vec::new();
        let _ = lower_component(&decl, &refs, &env, &mut diags);
        assert_eq!(
            diags.iter().filter(|d| d.code == "E2104").count(),
            1,
            "a forward state read is E2104, got {diags:?}"
        );
    }

    #[test]
    fn a_private_state_type_is_inferred() {
        let src = "component C {\n  state count = 0\n}";
        let count = SymbolId::from_parts(2, 0);
        let (_root, decl, refs, env) = setup(src, &[("count", count)], &[]);
        let mut diags = Vec::new();
        let schema = lower_component(&decl, &refs, &env, &mut diags);
        assert_eq!(schema.states.len(), 1);
        // `0` with no annotation takes the host default I64 and is not undetermined.
        assert_eq!(schema.states[0].meta.inferred_type, Ty::I64);
        assert!(!schema.states[0].meta.type_is_undetermined());
        assert!(!schema.states[0].type_was_annotated);
    }

    #[test]
    fn computeds_are_ordered_dependencies_first() {
        // `b` reads `a`; evaluation order must place `a` before `b` regardless of source
        // order. Declare `b` first to prove the sort reorders.
        let src = "component C {\n  computed b: I64 = a\n  computed a: I64 = 1\n}";
        let a = SymbolId::from_parts(2, 0);
        let b = SymbolId::from_parts(3, 0);
        let (_root, decl, refs, env) = setup(src, &[("a", a), ("b", b)], &[a, b]);
        let mut diags = Vec::new();
        let schema = lower_component(&decl, &refs, &env, &mut diags);
        let order: Vec<&str> = schema.computeds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            order,
            vec!["a", "b"],
            "dependencies come first, got {order:?}"
        );
        assert!(diags.iter().all(|d| d.code != "E2105"));
    }

    #[test]
    fn a_computed_cycle_is_e2105_with_the_full_path() {
        // `a` reads `b`, `b` reads `a` — a two-member cycle.
        let src = "component C {\n  computed a: I64 = b\n  computed b: I64 = a\n}";
        let a = SymbolId::from_parts(2, 0);
        let b = SymbolId::from_parts(3, 0);
        let (_root, decl, refs, env) = setup(src, &[("a", a), ("b", b)], &[a, b]);
        let mut diags = Vec::new();
        let _ = lower_component(&decl, &refs, &env, &mut diags);
        let cycle: Vec<&Diagnostic> = diags.iter().filter(|d| d.code == "E2105").collect();
        assert_eq!(cycle.len(), 1, "one cycle diagnostic, got {diags:?}");
        // The full path is carried in related spans (both members plus the close-back).
        assert!(
            cycle[0].related.len() >= 2,
            "the cycle path is in related spans, got {:?}",
            cycle[0].related
        );
    }

    #[test]
    fn reactive_reads_are_recorded_on_a_state() {
        // `total` reads `count`; its node's reactive_reads must contain `count`.
        let src = "component C {\n  state count = 1\n  state total = count\n}";
        let count = SymbolId::from_parts(2, 0);
        let total = SymbolId::from_parts(3, 0);
        let (_root, decl, refs, env) =
            setup(src, &[("count", count), ("total", total)], &[count, total]);
        let mut diags = Vec::new();
        let schema = lower_component(&decl, &refs, &env, &mut diags);
        let total_node = schema
            .states
            .iter()
            .find(|s| s.name == "total")
            .expect("total is present");
        assert!(total_node.meta.reactive_reads.contains(&count));
    }

    #[test]
    fn a_malformed_component_does_not_panic() {
        // A truncated body must lower without panicking (Slice-L recovery guarantee).
        for src in ["component C {", "component {}", "component C { state }"] {
            let tokens = tokenize(src);
            let parse = crate::syntax::grammar::parse(&tokens, src);
            let root = SyntaxNode::new_root(parse.root);
            if let Some(decl) = root.descendants().into_iter().find_map(ComponentDecl::cast) {
                let env = StubEnv {
                    members: HashMap::new(),
                    sources: BTreeSet::new(),
                    component: SymbolId::from_parts(999, 0),
                };
                let mut diags = Vec::new();
                let _ = lower_component(&decl, &[], &env, &mut diags);
            }
        }
    }
}
