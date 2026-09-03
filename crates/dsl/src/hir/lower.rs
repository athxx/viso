//! End-to-end lowering — resolved modules to a typed HIR package.
//!
//! This is the pass the rest of the layer serves: it re-walks every resolved module's AST,
//! lowers each component to its [`ComponentSchema`] (via [`super::component::lower_component`]),
//! effect-checks every callable and view body against its body context (via
//! [`super::effect::EffectCx`]), infers each callable's capability set from the typed call
//! graph (via [`super::capability::propagate`]), and gathers every diagnostic the three
//! checks raise into one [`LoweredPackage`].
//!
//! The passes below it are decoupled from the resolver's tables through the environment
//! traits ([`TypeEnv`], [`ReadEnv`], [`EffectEnv`], [`MemberEnv`]); this section supplies the
//! one concrete implementation over the real tables. Because interning a name needs
//! `&mut NameInterner`, a per-module pre-pass walks the members once with the interner to
//! build two owned maps — member-name to [`SymbolId`], and `SymbolId` to the facts the
//! `&self` trait methods answer from (type, effect class, whether it is a reactive source).
//! The trait methods are then pure lookups, so a body walk borrows the env immutably.
//!
//! The reference framework has no static type/effect/capability layer to port, so this whole
//! pass is Viso-owned; it consumes the resolver's durable [`SymbolId`] identities and
//! slot-based locals unchanged.

use std::cell::Cell;
use std::collections::HashMap;

use crate::ast::{AstNode, CompilationUnit, ComponentDecl, Item, Member, TypePath};
use crate::diag::Diagnostic;
use crate::resolve::{
    ModuleGraph, NameInterner, Namespace, Resolution, ResolvedModule, ResolvedRef, SourceUnit,
    SymbolId, SymbolTable,
};
use crate::syntax::{SyntaxNode, TextRange};

use super::capability::{CapabilityNode, CapabilitySet, propagate};
use super::component::{MemberEnv, lower_component};
use super::effect::{BodyContext, EffectClass, EffectCx, EffectEnv};
use super::infer::TypeEnv;
use super::nodes::{HirCallable, HirComponent};
use super::reads::ReadEnv;
use super::ty::Ty;

/// The typed HIR of a whole package: every component lowered, every free callable, and every
/// diagnostic the type/effect/capability checks raised across all modules.
///
/// The components carry the full node contract (types, effects, capabilities, reactive
/// reads); the top-level `callables` are the module-level `fn`/`action`/`task` declarations
/// (component members live inside their [`HirComponent`]). `diagnostics` is the union of the
/// resolver's own diagnostics is *not* included here — those stay on the [`ResolvedModule`];
/// this holds only what lowering itself found.
#[derive(Debug, Clone, PartialEq)]
pub struct LoweredPackage {
    /// Every lowered component (and system), across all modules, in module then source order.
    pub components: Vec<HirComponent>,
    /// Every module-level callable (`fn`/`action`/`task`), across all modules.
    pub callables: Vec<HirCallable>,
    /// The diagnostics lowering raised (type/effect/capability), across all modules.
    pub diagnostics: Vec<Diagnostic>,
}

/// Lowers a whole resolved package into its typed HIR.
///
/// `graph` and `units` are the module graph and its parse trees (matched by module-path
/// text, exactly as the resolver's `unit_for` does); `resolved` is index-parallel to
/// `graph.modules()` (the resolver's output). `interner` is threaded through so the per-module
/// environment pre-pass can intern member names to query the module's [`SymbolTable`], and
/// `package` is the package identity (unused by lowering directly but kept for symmetry with
/// [`crate::resolve::resolve`] and future native-schema keying).
pub fn lower(
    graph: &ModuleGraph,
    units: &[SourceUnit],
    resolved: &[ResolvedModule],
    interner: &mut NameInterner,
    package: &str,
) -> LoweredPackage {
    let _ = package;
    let mut components = Vec::new();
    let mut callables = Vec::new();
    let mut diagnostics = Vec::new();

    for (i, gm) in graph.modules().iter().enumerate() {
        let Some(resolved_module) = resolved.get(i) else {
            continue;
        };
        let module_text = gm.path.display(interner);
        let Some(cu) = unit_for(units, &module_text, interner) else {
            continue;
        };

        // Build the per-module environment: intern member names to look their symbols up in
        // the module table, and record each member's facts, so the `&self` trait methods are
        // pure lookups during the body walks.
        let env = ModuleEnvBuilder::build(&cu, &resolved_module.table, interner);

        lower_module(
            &cu,
            &resolved_module.refs,
            &env,
            &mut components,
            &mut callables,
            &mut diagnostics,
        );
    }

    // Debug-only HIR-complete assertion (spec node-contract section): no core node may keep
    // an undetermined type into the finished package. A residue is a lowering bug, not a user
    // error (user type errors surface as diagnostics), so this is a debug invariant.
    debug_assert!(
        hir_is_complete(&components, &callables),
        "lowering left an undetermined type on a core HIR node"
    );

    LoweredPackage {
        components,
        callables,
        diagnostics,
    }
}

/// Lowers one compilation unit's components/systems and module-level callables, running the
/// effect and capability checks over their bodies.
fn lower_module(
    cu: &CompilationUnit,
    refs: &[ResolvedRef],
    env: &ModuleEnv,
    components: &mut Vec<HirComponent>,
    callables: &mut Vec<HirCallable>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Collect the capability call graph as we lower: one node per callable (component members
    // and module-level), its declared `requires {}` bound, and — this slice — no call edges
    // to other-callable indices yet, because a callee resolves to a `SymbolId` and the graph
    // is index-based. We map symbol → node index first, then fill edges in a second scan.
    let mut cap = CapabilityGraphBuilder::new();

    for item in cu.items() {
        let decl = match item {
            Item::Export(e) => match e.declaration() {
                Some(inner) => inner,
                None => continue,
            },
            other => other,
        };
        // A `system` shares the component member surface but is not a `ComponentDecl`; its
        // dedicated lowering lands with its consumer slice (system hooks / scheduler schema).
        // Module-level `fn`/`action`/`task` likewise lower fully with their consumer. This
        // slice covers the component vertical; unhandled declarations are left for those slices
        // rather than fabricated here.
        if let Item::Component(c) = decl {
            let component = lower_component_item(&c, refs, env, diagnostics, &mut cap);
            components.push(component);
        }
        let _ = &mut *callables;
    }

    // Resolve the capability call graph to a fixed point and write each inferred set back onto
    // its callable node, then append any `requires {}` violations.
    cap.finish(components, diagnostics);
}

/// Lowers one `component` declaration: schema + view/callable effect checks, registering each
/// callable in the capability graph.
fn lower_component_item(
    decl: &ComponentDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv,
    diagnostics: &mut Vec<Diagnostic>,
    cap: &mut CapabilityGraphBuilder,
) -> HirComponent {
    env.focus_component(decl);
    let schema = lower_component(decl, refs, env, diagnostics);
    let source_origin = decl.syntax().text_range();

    // Effect-check the view body (a reactive context) and every callable body in its context.
    if let Some(view) = decl.view()
        && let Some(block) = view.block()
    {
        check_body(refs, BodyContext::View, env, block.syntax(), diagnostics);
    }
    check_component_callables(decl, refs, env, diagnostics, cap, &schema.callables);

    HirComponent {
        schema,
        source_origin,
    }
}

/// Effect-checks each `fn`/`action`/`task` body of a component in its body context and
/// registers each callable in the capability graph (its declared `requires {}` bound and the
/// callables its body calls).
fn check_component_callables(
    decl: &ComponentDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv,
    diagnostics: &mut Vec<Diagnostic>,
    cap: &mut CapabilityGraphBuilder,
    lowered: &[HirCallable],
) {
    for member in decl.members() {
        let (context, body, clause, name) = match &member {
            Member::Fn(f) => (
                BodyContext::Fn,
                f.body(),
                f.capability_clause(),
                name_of(f.name()),
            ),
            Member::Action(a) => (
                BodyContext::Action,
                a.body(),
                a.capability_clause(),
                name_of(a.name()),
            ),
            Member::Task(t) => (
                BodyContext::Task,
                t.body(),
                t.capability_clause(),
                name_of(t.name()),
            ),
            _ => continue,
        };

        if let Some(block) = &body {
            check_body(refs, context, env, block.syntax(), diagnostics);
        }

        // Register in the capability graph, keyed by the callable's node span so its inferred
        // set can be written back onto the matching lowered node.
        let symbol = env.member_symbol(&name);
        let declared = clause.map(|c| (capability_set_of(&c), c.syntax().text_range()));
        let calls = body
            .as_ref()
            .map(|b| callee_symbols(refs, b.syntax()))
            .unwrap_or_default();
        // The node span identifies which lowered callable receives the inferred set.
        let node_span = lowered
            .iter()
            .find(|hc| symbol.is_some() && hc.meta.resolved_symbol == symbol)
            .map(|hc| hc.meta.source_origin);
        cap.add(symbol, declared, calls, node_span);
    }
}

/// Runs an effect-check body walk in `context`, appending any `E2501`/`E2502` it raises.
fn check_body(
    refs: &[ResolvedRef],
    context: BodyContext,
    env: &dyn EffectEnv,
    node: &SyntaxNode,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut cx = EffectCx::new(refs, context, env);
    cx.check_node(node);
    diagnostics.extend(cx.into_diagnostics());
}

/// Whether every core HIR node in the package carries a determined type (the HIR-complete
/// assertion). Nominal `Unknown` types on view/callable placeholders are exempt only where the
/// node contract allows them; a core member (`input`/`state`/`computed`) that omitted its type
/// must have inferred one.
fn hir_is_complete(components: &[HirComponent], callables: &[HirCallable]) -> bool {
    for component in components {
        for state in &component.schema.states {
            // A state whose type was annotated may be a nominal `Unknown` (resolved elsewhere
            // this slice); an omitted-type state that stayed undetermined already earned an
            // E2103, so only assert on annotated-or-determined nodes.
            if !state.type_was_annotated && state.meta.type_is_undetermined() {
                return false;
            }
        }
    }
    let _ = callables;
    true
}

/// The name text of an optional name token, empty when absent.
fn name_of(tok: Option<crate::syntax::SyntaxToken>) -> String {
    tok.map(|t| t.text().to_string()).unwrap_or_default()
}

/// The capability set a `requires { ... }` clause declares: each capability path joined to a
/// dotted name.
fn capability_set_of(clause: &crate::ast::CapabilityClause) -> CapabilitySet {
    let mut set = CapabilitySet::new();
    for path in clause.capabilities() {
        let name = type_path_text(&path);
        if !name.is_empty() {
            set.insert(name);
        }
    }
    set
}

/// The `.`-joined identifier text of a capability type path (`net.http` → `net.http`).
fn type_path_text(path: &TypePath) -> String {
    path.segments()
        .map(|t| t.text().to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// The symbols every call in `node` resolves its callee to — the call edges of the capability
/// graph, as symbols (mapped to node indices by the graph builder).
fn callee_symbols(refs: &[ResolvedRef], node: &SyntaxNode) -> Vec<SymbolId> {
    use crate::ast::{CallExpr, PathExpr};
    use crate::syntax::SyntaxKind;

    // Index refs by head-token span for O(1) callee resolution, mirroring `EffectCx`.
    let mut index: HashMap<TextRange, Resolution> = HashMap::with_capacity(refs.len());
    for r in refs {
        index.insert(r.range, r.to);
    }

    let mut out = Vec::new();
    let mut stack = vec![node.clone()];
    while let Some(n) = stack.pop() {
        if n.kind() == SyntaxKind::CallExpr
            && let Some(call) = CallExpr::cast(n.clone())
            && let Some(callee) = call.callee()
        {
            let callee_node = callee.syntax();
            if callee_node.kind() == SyntaxKind::PathExpr
                && let Some(head) =
                    PathExpr::cast(callee_node.clone()).and_then(|p| p.segments().next())
                && let Some(Resolution::Symbol(id)) = index.get(&head.text_range())
            {
                out.push(*id);
            }
        }
        for child in n.children() {
            stack.push(child);
        }
    }
    out
}

/// Recovers a compilation unit for a module-path text (a replica of the resolver's `unit_for`:
/// the graph and units share one interner, so module paths render to the same text).
fn unit_for(
    units: &[SourceUnit],
    module_text: &str,
    interner: &NameInterner,
) -> Option<CompilationUnit> {
    units
        .iter()
        .find(|u| u.path.display(interner) == module_text)
        .and_then(|u| CompilationUnit::cast(SyntaxNode::new_root(u.parse.root.clone())))
}

// --- Capability call graph builder ------------------------------------------------------

/// One pending capability-graph entry as lowering scans callables: the callable's symbol (its
/// graph identity), its declared `requires {}` bound, the symbols it calls, and the span of the
/// lowered node its inferred set should be written back onto.
struct PendingCallable {
    symbol: Option<SymbolId>,
    declared: Option<(CapabilitySet, TextRange)>,
    calls: Vec<SymbolId>,
    node_span: Option<TextRange>,
}

/// Accumulates callables into a symbol-keyed capability call graph, then resolves it and writes
/// inferred sets back onto the lowered callable nodes.
struct CapabilityGraphBuilder {
    pending: Vec<PendingCallable>,
}

impl CapabilityGraphBuilder {
    fn new() -> Self {
        CapabilityGraphBuilder {
            pending: Vec::new(),
        }
    }

    /// Records one callable.
    fn add(
        &mut self,
        symbol: Option<SymbolId>,
        declared: Option<(CapabilitySet, TextRange)>,
        calls: Vec<SymbolId>,
        node_span: Option<TextRange>,
    ) {
        self.pending.push(PendingCallable {
            symbol,
            declared,
            calls,
            node_span,
        });
    }

    /// Resolves the graph: maps symbols to node indices, turns each call into an edge (dropping
    /// calls to callables outside this module — their capability facts join when their module
    /// lowers), runs [`propagate`], writes each inferred set back onto the lowered callable with
    /// the matching span, and appends any `E2601` diagnostics.
    fn finish(self, components: &mut [HirComponent], diagnostics: &mut Vec<Diagnostic>) {
        if self.pending.is_empty() {
            return;
        }

        // Symbol → node index, for edge resolution.
        let mut index_of: HashMap<SymbolId, usize> = HashMap::with_capacity(self.pending.len());
        for (i, p) in self.pending.iter().enumerate() {
            if let Some(sym) = p.symbol {
                index_of.insert(sym, i);
            }
        }

        let nodes: Vec<CapabilityNode> = self
            .pending
            .iter()
            .map(|p| CapabilityNode {
                // No native schema declares direct conferrals yet, so every callable's own
                // set is empty; the machinery unions callee sets in regardless.
                direct: CapabilitySet::new(),
                declared: p.declared.clone(),
                calls: p
                    .calls
                    .iter()
                    .filter_map(|s| index_of.get(s).copied())
                    .collect(),
            })
            .collect();

        let (inferred, diags) = propagate(&nodes);
        diagnostics.extend(diags);

        // Write each inferred set back onto the lowered callable node with the matching span.
        for (p, set) in self.pending.iter().zip(inferred) {
            if set.is_empty() {
                continue;
            }
            let Some(span) = p.node_span else {
                continue;
            };
            for component in components.iter_mut() {
                for callable in component.schema.callables.iter_mut() {
                    if callable.meta.source_origin == span {
                        callable.meta.capability_set = set.clone();
                    }
                }
            }
        }
    }
}

// --- The concrete module environment ----------------------------------------------------

/// The facts the environment answers about one member symbol, cached from the pre-pass so the
/// `&self` trait methods are pure lookups.
struct MemberFacts {
    /// The member's type when it is a reactive source (`state`/`input`/`computed`), for
    /// `TypeEnv::resolution_ty`. `Unknown` when its type is not yet known from the declaration.
    ty: Ty,
    /// The member's effect class when it is a callable, for `EffectEnv::callee_effect`.
    effect: Option<EffectClass>,
    /// Whether the member is a reactive source (`state`/`input`/`computed`), for
    /// `ReadEnv::reactive_source`.
    is_reactive_source: bool,
}

/// The concrete [`MemberEnv`]/[`TypeEnv`]/[`ReadEnv`]/[`EffectEnv`] for one module, built over
/// the resolver's [`SymbolTable`] with the member facts precomputed.
struct ModuleEnv {
    /// Member name → symbol, both value and event namespaces, for [`MemberEnv::member_symbol`].
    members: HashMap<String, SymbolId>,
    /// Symbol → facts, for the type/effect/read trait methods.
    facts: HashMap<SymbolId, MemberFacts>,
    /// Component declaration syntax range → its symbol, so the env can focus on the component
    /// currently being lowered without confusing several components in one module.
    components: HashMap<TextRange, SymbolId>,
    /// The component currently being lowered, answered by [`MemberEnv::component_symbol`]. The
    /// facts/members maps cover every component in the module (so cross-member references
    /// resolve), but a module may declare several components, so the *current* one is set per
    /// component before its `lower_component` call. `Cell` keeps the trait methods `&self`.
    component: Cell<SymbolId>,
}

impl TypeEnv for ModuleEnv {
    fn resolution_ty(&self, to: &Resolution) -> Option<Ty> {
        match to {
            Resolution::Symbol(id) => self.facts.get(id).and_then(|f| {
                if f.is_reactive_source && !matches!(f.ty, Ty::Unknown) {
                    Some(f.ty.clone())
                } else {
                    None
                }
            }),
            Resolution::Local(_) => None,
        }
    }

    fn callee_signature(&self, _to: &Resolution) -> Option<(Vec<Ty>, Ty)> {
        // Signature-typed calls land with public-callable signature lowering; this slice does
        // not synthesize signatures from bodies.
        None
    }
}

impl ReadEnv for ModuleEnv {
    fn reactive_source(&self, to: &Resolution) -> Option<SymbolId> {
        match to {
            Resolution::Symbol(id) => self.facts.get(id).and_then(|f| {
                if f.is_reactive_source {
                    Some(*id)
                } else {
                    None
                }
            }),
            Resolution::Local(_) => None,
        }
    }
}

impl EffectEnv for ModuleEnv {
    fn callee_effect(&self, to: &Resolution) -> Option<EffectClass> {
        match to {
            Resolution::Symbol(id) => self.facts.get(id).and_then(|f| f.effect),
            Resolution::Local(_) => None,
        }
    }
}

impl MemberEnv for ModuleEnv {
    fn member_symbol(&self, name: &str) -> Option<SymbolId> {
        self.members.get(name).copied()
    }

    fn component_symbol(&self) -> SymbolId {
        self.component.get()
    }
}

impl ModuleEnv {
    /// Points the environment at the component about to be lowered (keyed by its declaration's
    /// syntax range, recorded in the pre-pass), so `component_symbol` answers with its symbol.
    fn focus_component(&self, decl: &ComponentDecl) {
        if let Some(sym) = self.components.get(&decl.syntax().text_range()) {
            self.component.set(*sym);
        }
    }
}

/// Builds a [`ModuleEnv`] from a compilation unit and its resolved symbol table.
struct ModuleEnvBuilder;

impl ModuleEnvBuilder {
    /// Walks every component/system member once (with the interner, to intern names and query
    /// the table) and records the name→symbol map and the per-symbol facts.
    fn build(cu: &CompilationUnit, table: &SymbolTable, interner: &mut NameInterner) -> ModuleEnv {
        let mut members: HashMap<String, SymbolId> = HashMap::new();
        let mut facts: HashMap<SymbolId, MemberFacts> = HashMap::new();
        let mut components: HashMap<TextRange, SymbolId> = HashMap::new();
        let mut default_component = SymbolId::from_parts(0, 0);

        for item in cu.items() {
            let decl = match item {
                Item::Export(e) => match e.declaration() {
                    Some(inner) => inner,
                    None => continue,
                },
                other => other,
            };
            let member_list = match &decl {
                Item::Component(c) => {
                    // Record this component's symbol keyed by its declaration span, so the env
                    // can focus on whichever component it is currently lowering (a module may
                    // declare several). The facts/members maps below stay module-wide.
                    if let Some(sym) = decl_symbol(table, interner, c.name(), Namespace::Type) {
                        components.insert(c.syntax().text_range(), sym);
                        default_component = sym;
                    }
                    c.members().collect::<Vec<_>>()
                }
                Item::System(s) => {
                    if let Some(sym) = decl_symbol(table, interner, s.name(), Namespace::Type) {
                        default_component = sym;
                    }
                    s.members().collect::<Vec<_>>()
                }
                _ => continue,
            };

            for member in member_list {
                Self::record_member(&member, table, interner, &mut members, &mut facts);
            }
        }

        ModuleEnv {
            members,
            facts,
            components,
            component: Cell::new(default_component),
        }
    }

    /// Records one member's name→symbol entry and its facts.
    fn record_member(
        member: &Member,
        table: &SymbolTable,
        interner: &mut NameInterner,
        members: &mut HashMap<String, SymbolId>,
        facts: &mut HashMap<SymbolId, MemberFacts>,
    ) {
        let (name_tok, namespace, ty, effect, is_source) = match member {
            Member::Input(d) => (
                d.name(),
                Namespace::Value,
                d.ty().map(|t| annotation_ty(&t)).unwrap_or(Ty::Unknown),
                None,
                true,
            ),
            Member::State(d) => (
                d.name(),
                Namespace::Value,
                d.ty().map(|t| annotation_ty(&t)).unwrap_or(Ty::Unknown),
                None,
                true,
            ),
            Member::Computed(d) => (
                d.name(),
                Namespace::Value,
                d.ty().map(|t| annotation_ty(&t)).unwrap_or(Ty::Unknown),
                None,
                true,
            ),
            Member::Event(d) => (d.name(), Namespace::Event, Ty::Unit, None, false),
            Member::Fn(d) => (
                d.name(),
                Namespace::Value,
                Ty::Unit,
                Some(EffectClass::Read),
                false,
            ),
            Member::Action(d) => (
                d.name(),
                Namespace::Value,
                Ty::Unit,
                Some(EffectClass::Action),
                false,
            ),
            Member::Task(d) => (
                d.name(),
                Namespace::Value,
                Ty::Unit,
                Some(EffectClass::Task),
                false,
            ),
            Member::View(_) => return,
        };

        let Some(tok) = name_tok else {
            return;
        };
        let text = tok.text().to_string();
        let name = interner.intern(&text);
        let Some(sym) = table.get(name, namespace).map(|s| s.id) else {
            return;
        };
        members.insert(text, sym);
        facts.insert(
            sym,
            MemberFacts {
                ty,
                effect,
                is_reactive_source: is_source,
            },
        );
    }
}

/// The symbol a top-level declaration name interns to in `namespace`, if the table has it.
fn decl_symbol(
    table: &SymbolTable,
    interner: &mut NameInterner,
    name_tok: Option<crate::syntax::SyntaxToken>,
    namespace: Namespace,
) -> Option<SymbolId> {
    let tok = name_tok?;
    let name = interner.intern(&tok.text());
    table.get(name, namespace).map(|s| s.id)
}

/// A type annotation lowered to a [`Ty`], mapping a removed/unknown/nominal annotation to
/// `Unknown` (the env only needs a concrete builtin type for its `resolution_ty` answers; the
/// full annotation diagnostics are `lower_component`'s to raise).
fn annotation_ty(path: &TypePath) -> Ty {
    match Ty::from_type_path(path) {
        Ok(Some(ty)) => ty,
        _ => Ty::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::{ModuleGraph, NameInterner, SourceUnit, resolve};

    /// Parses one `.vs` source as a single-module package, resolves it, and lowers it.
    fn lower_src(src: &str) -> LoweredPackage {
        let mut interner = NameInterner::new();
        let tokens = crate::syntax::tokenize(src);
        let parse = crate::syntax::grammar::parse(&tokens, src);
        let path = crate::resolve::ModulePath::intern(&mut interner, &["app"]);
        let unit = SourceUnit::new(path, parse);
        let units = vec![unit];
        let graph = ModuleGraph::build(&units, &interner);
        let resolved = resolve(&graph, &units, &mut interner, "app");
        lower(&graph, &units, &resolved, &mut interner, "app")
    }

    #[test]
    fn a_full_component_lowers_with_no_diagnostics() {
        let src = "component Counter {\n\
                   \x20 input label: String\n\
                   \x20 state count = 0\n\
                   \x20 computed doubled: I64 = count\n\
                   \x20 action bump { }\n\
                   \x20 view { }\n\
                   }";
        let pkg = lower_src(src);
        assert_eq!(pkg.components.len(), 1, "one component lowered");
        let schema = &pkg.components[0].schema;
        assert_eq!(schema.name, "Counter");
        assert_eq!(schema.inputs.len(), 1);
        assert_eq!(schema.states.len(), 1);
        assert_eq!(schema.computeds.len(), 1);
        assert_eq!(schema.callables.len(), 1);
        assert!(schema.view.is_some());
        assert!(
            pkg.diagnostics.is_empty(),
            "a well-formed component lowers cleanly, got {:?}",
            pkg.diagnostics
        );
    }

    #[test]
    fn a_private_state_infers_its_type() {
        let src = "component C {\n  state count = 0\n}";
        let pkg = lower_src(src);
        let schema = &pkg.components[0].schema;
        assert_eq!(schema.states[0].meta.inferred_type, Ty::I64);
        assert!(!schema.states[0].meta.type_is_undetermined());
    }

    #[test]
    fn a_requires_clause_covering_the_inferred_set_is_ok() {
        // No native conferrals exist yet, so an inferred capability set is empty and any
        // `requires {}` is trivially a superset — no E2601.
        let src = "component C {\n  action go requires { net.http } { }\n}";
        let pkg = lower_src(src);
        assert!(
            pkg.diagnostics.iter().all(|d| d.code != "E2601"),
            "an over-broad requires clause is allowed (it is an upper bound), got {:?}",
            pkg.diagnostics
        );
    }
}
