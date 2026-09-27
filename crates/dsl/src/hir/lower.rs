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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::ast::{
    AstNode, Block, CompilationUnit, ComponentDecl, ConstDecl, EnumVariant, InputDecl, Item,
    Member, Param, RecordDecl, RecordField, ReturnType, TypePath,
};
use crate::diag::Diagnostic;
use crate::resolve::{
    ModuleGraph, NameInterner, Namespace, Resolution, ResolvedModule, ResolvedRef, SourceUnit,
    SymbolId, SymbolTable,
};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

use super::capability::{CapabilityNode, CapabilitySet, propagate};
use super::component::{MemberEnv, lower_component};
use super::effect::{BodyContext, EffectClass, EffectCx, EffectEnv};
use super::infer::{FieldInfo, InferCx, TypeEnv, VariantInfo, VariantPayload};
use super::nodes::{ComponentSchema, HirCallable, HirComponent};
use super::percent::PercentSources;
use super::reads::ReadEnv;
use super::ty::Ty;
use super::view::{InputProp, PercentFlow, ViewEnv, check_percent_flow, check_view};

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
    /// The slice of `diagnostics` each module raised, index-parallel to the graph's
    /// modules, so a multi-file caller can attribute each one to its source.
    pub module_diagnostics: Vec<std::ops::Range<usize>>,
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
    let mut module_diagnostics = Vec::with_capacity(graph.modules().len());

    for (i, gm) in graph.modules().iter().enumerate() {
        let start = diagnostics.len();
        module_diagnostics.push(start..start);
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
        let env = ModuleEnv::build(&cu, &resolved_module.table, &resolved_module.refs, interner);

        lower_module(
            &cu,
            &resolved_module.refs,
            &env,
            &mut components,
            &mut callables,
            &mut diagnostics,
        );
        module_diagnostics[i] = start..diagnostics.len();
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
        module_diagnostics,
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
    let mut flows = Vec::new();
    let mut percent = PercentSources::default();

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
        // Module-level `fn`/`action`/`task` bodies, `const` values and record field defaults
        // are type-checked here; their HIR nodes land with their consumer slice.
        match decl {
            Item::Component(c) => {
                let component = lower_component_item(
                    &c,
                    refs,
                    env,
                    diagnostics,
                    &mut cap,
                    &mut flows,
                    &mut percent,
                );
                components.push(component);
            }
            Item::Const(c) => check_const(&c, refs, env, diagnostics, &mut percent),
            Item::Record(r) => check_field_defaults(&r, refs, env, diagnostics, &mut percent),
            Item::Fn(f) => {
                check_signature(
                    refs,
                    env,
                    &f.params(),
                    f.return_type(),
                    f.body(),
                    diagnostics,
                    &mut percent,
                );
            }
            Item::Action(a) => {
                check_signature(
                    refs,
                    env,
                    &a.params(),
                    a.return_type(),
                    a.body(),
                    diagnostics,
                    &mut percent,
                );
            }
            Item::Task(t) => {
                check_signature(
                    refs,
                    env,
                    &t.params(),
                    t.return_type(),
                    t.body(),
                    diagnostics,
                    &mut percent,
                );
            }
            _ => {}
        }
        let _ = &mut *callables;
    }

    let facts = percent.solve();
    check_percent_flow(&flows, &facts, env, diagnostics);

    // Resolve the capability call graph to a fixed point and write each inferred set back onto
    // its callable node, then append any `requires {}` violations.
    cap.finish(components, diagnostics);
}

/// Lowers one `component` declaration: schema + view/callable effect checks, registering each
/// callable in the capability graph and collecting what its view reveals about input percent
/// bases into `flows`.
fn lower_component_item(
    decl: &ComponentDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv,
    diagnostics: &mut Vec<Diagnostic>,
    cap: &mut CapabilityGraphBuilder,
    flows: &mut Vec<PercentFlow>,
    percent: &mut PercentSources,
) -> HirComponent {
    env.focus_component(decl);
    let schema = lower_component(decl, refs, env, diagnostics, percent);
    env.record_inferred(&schema);
    let source_origin = decl.syntax().text_range();

    // Effect-check the view body (a reactive context) and every callable body in its context.
    if let Some(view) = decl.view()
        && let Some(block) = view.block()
    {
        check_body(refs, BodyContext::View, env, block.syntax(), diagnostics);
        flows.push(check_view(
            refs,
            env,
            Some(env.component_symbol()),
            &block,
            diagnostics,
            percent,
        ));
    }
    check_component_callables(
        decl,
        refs,
        env,
        diagnostics,
        cap,
        &schema.callables,
        percent,
    );

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
    percent: &mut PercentSources,
) {
    for member in decl.members() {
        let (context, body, clause, name, params, ret) = match &member {
            Member::Fn(f) => (
                BodyContext::Fn,
                f.body(),
                f.capability_clause(),
                name_of(f.name()),
                f.params(),
                f.return_type(),
            ),
            Member::Action(a) => (
                BodyContext::Action,
                a.body(),
                a.capability_clause(),
                name_of(a.name()),
                a.params(),
                a.return_type(),
            ),
            Member::Task(t) => (
                BodyContext::Task,
                t.body(),
                t.capability_clause(),
                name_of(t.name()),
                t.params(),
                t.return_type(),
            ),
            _ => continue,
        };

        if let Some(block) = &body {
            check_body(refs, context, env, block.syntax(), diagnostics);
        }
        check_signature(refs, env, &params, ret, body.clone(), diagnostics, percent);

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

/// Type-checks a callable body against its signature: each parameter binds its annotated
/// type, and the body's value and every `return` meet the declared return type (`Unit`
/// when none is declared).
fn check_signature(
    refs: &[ResolvedRef],
    env: &ModuleEnv,
    params: &[Param],
    ret: Option<ReturnType>,
    body: Option<Block>,
    diagnostics: &mut Vec<Diagnostic>,
    percent: &mut PercentSources,
) {
    let Some(body) = body else {
        return;
    };
    let params: Vec<(TextRange, Ty)> = params
        .iter()
        .filter_map(|p| Some((p.name()?.text_range(), env.annotation_of(p.syntax()))))
        .collect();
    let ret = ret.map(|r| env.annotation_of(r.syntax()));
    let mut cx = InferCx::new(refs, env);
    cx.check_callable(&params, ret.as_ref(), &body);
    percent.extend(cx.take_percent_defs());
    diagnostics.extend(cx.into_diagnostics());
}

/// Types a `const` value against its annotation.
fn check_const(
    decl: &ConstDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv,
    diagnostics: &mut Vec<Diagnostic>,
    percent: &mut PercentSources,
) {
    let Some(value) = decl.value() else {
        return;
    };
    let want = env.annotation_of(decl.syntax());
    let mut cx = InferCx::new(refs, env);
    let _ = if want.has_unknown() {
        cx.infer_expr(&value, None)
    } else {
        cx.infer_promoted(&value, &want)
    };
    if let Some(symbol) = env.declared.get(&decl.syntax().text_range()) {
        percent.define(Resolution::Symbol(*symbol), cx.carry(value.syntax()));
    }
    percent.extend(cx.take_percent_defs());
    diagnostics.extend(cx.into_diagnostics());
}

/// Types each record field default against its field type; a record literal that omits
/// a defaulted field carries what the defaults do.
fn check_field_defaults(
    decl: &RecordDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv,
    diagnostics: &mut Vec<Diagnostic>,
    percent: &mut PercentSources,
) {
    let record = env.declared.get(&decl.syntax().text_range()).copied();
    let mut cx = InferCx::new(refs, env);
    for field in decl.fields() {
        let Some(value) = field.default() else {
            continue;
        };
        let want = env.annotation_of(field.syntax());
        let _ = if want.has_unknown() {
            cx.infer_expr(&value, None)
        } else {
            cx.infer_promoted(&value, &want)
        };
        if let Some(record) = record {
            percent.define(Resolution::Symbol(record), cx.carry(value.syntax()));
        }
    }
    percent.extend(cx.take_percent_defs());
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

/// The facts the environment answers about one symbol, cached from the pre-pass so the
/// `&self` trait methods are pure lookups.
struct MemberFacts {
    /// The symbol's declared type (a member's or `const`'s annotation, a `fn`'s function
    /// type), for `TypeEnv::resolution_ty`. `Unknown` when the declaration does not fix it.
    ty: Ty,
    /// The symbol's effect class when it is a callable, for `EffectEnv::callee_effect`.
    effect: Option<EffectClass>,
    /// Whether the member is a reactive source (`state`/`input`/`computed`), for
    /// `ReadEnv::reactive_source`.
    is_reactive_source: bool,
}

/// The concrete [`MemberEnv`]/[`TypeEnv`]/[`ViewEnv`]/[`ReadEnv`]/[`EffectEnv`] for one
/// module, built over the resolver's [`SymbolTable`] and references with the facts
/// precomputed.
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
    /// Name token span → the symbol the resolver bound it to, for nominal annotations.
    nominal: HashMap<TextRange, SymbolId>,
    /// The fields of every record the module declares.
    records: HashMap<SymbolId, Vec<FieldInfo>>,
    /// `const` and record declaration syntax range → its symbol.
    declared: HashMap<TextRange, SymbolId>,
    /// The variants of every enum the module declares.
    enums: HashMap<SymbolId, Vec<VariantInfo>>,
    /// The declared name of every record, enum and component, for diagnostics.
    type_names: HashMap<SymbolId, String>,
    /// The `(params, ret)` signature of every `fn` and `action`.
    signatures: HashMap<SymbolId, (Vec<Ty>, Ty)>,
    /// The inputs of every component the module declares, as node properties.
    inputs: HashMap<SymbolId, Vec<InputProp>>,
    /// The types lowering inferred for unannotated `state`/`computed` members, recorded
    /// as each component lowers so later body walks see them.
    inferred: RefCell<HashMap<SymbolId, Ty>>,
}

impl TypeEnv for ModuleEnv {
    fn resolution_ty(&self, to: &Resolution) -> Option<Ty> {
        match to {
            Resolution::Symbol(id) => self
                .facts
                .get(id)
                .map(|f| f.ty.clone())
                .filter(|ty| *ty != Ty::Unknown)
                .or_else(|| self.inferred.borrow().get(id).cloned()),
            Resolution::Local(_) => None,
        }
    }

    fn callee_signature(&self, to: &Resolution) -> Option<(Vec<Ty>, Ty)> {
        match to {
            Resolution::Symbol(id) => self.signatures.get(id).cloned(),
            Resolution::Local(_) => None,
        }
    }

    fn record_fields(&self, ty: SymbolId) -> Option<&[FieldInfo]> {
        self.records.get(&ty).map(Vec::as_slice)
    }

    fn enum_variants(&self, ty: SymbolId) -> Option<&[VariantInfo]> {
        self.enums.get(&ty).map(Vec::as_slice)
    }

    fn type_name(&self, ty: SymbolId) -> Option<&str> {
        self.type_names.get(&ty).map(String::as_str)
    }
}

impl ViewEnv for ModuleEnv {
    fn component_inputs(&self, component: SymbolId) -> Option<&[InputProp]> {
        self.inputs.get(&component).map(Vec::as_slice)
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
    /// Walks every declaration once (with the interner, to intern names and query the
    /// table) and records the name→symbol map, the per-symbol facts, and the record, enum,
    /// signature and component-input tables.
    fn build(
        cu: &CompilationUnit,
        table: &SymbolTable,
        refs: &[ResolvedRef],
        interner: &mut NameInterner,
    ) -> ModuleEnv {
        let mut env = ModuleEnv {
            members: HashMap::new(),
            facts: HashMap::new(),
            components: HashMap::new(),
            component: Cell::new(SymbolId::from_parts(0, 0)),
            nominal: refs
                .iter()
                .filter_map(|r| match r.to {
                    Resolution::Symbol(id) => Some((r.range, id)),
                    Resolution::Local(_) => None,
                })
                .collect(),
            records: HashMap::new(),
            declared: HashMap::new(),
            enums: HashMap::new(),
            type_names: HashMap::new(),
            signatures: HashMap::new(),
            inputs: HashMap::new(),
            inferred: RefCell::default(),
        };

        for item in cu.items() {
            let decl = match item {
                Item::Export(e) => match e.declaration() {
                    Some(inner) => inner,
                    None => continue,
                },
                other => other,
            };
            match &decl {
                Item::Component(c) => {
                    // Record this component's symbol keyed by its declaration span, so the env
                    // can focus on whichever component it is currently lowering (a module may
                    // declare several). The facts/members maps below stay module-wide.
                    if let Some(sym) = decl_symbol(table, interner, c.name(), Namespace::Type) {
                        env.components.insert(c.syntax().text_range(), sym);
                        env.component.set(sym);
                        env.type_names.insert(sym, name_of(c.name()));
                        let inputs = env.inputs_of(c, table, interner);
                        env.inputs.insert(sym, inputs);
                    }
                    for member in c.members() {
                        env.record_member(&member, table, interner);
                    }
                }
                Item::System(s) => {
                    if let Some(sym) = decl_symbol(table, interner, s.name(), Namespace::Type) {
                        env.component.set(sym);
                    }
                    for member in s.members() {
                        env.record_member(&member, table, interner);
                    }
                }
                Item::Record(r) => {
                    if let Some(sym) = decl_symbol(table, interner, r.name(), Namespace::Type) {
                        let fields = r.fields().filter_map(|f| env.field_info(&f)).collect();
                        env.records.insert(sym, fields);
                        env.declared.insert(r.syntax().text_range(), sym);
                        env.type_names.insert(sym, name_of(r.name()));
                    }
                }
                Item::Enum(e) => {
                    if let Some(sym) = decl_symbol(table, interner, e.name(), Namespace::Type) {
                        let variants = e.variants().filter_map(|v| env.variant_info(&v)).collect();
                        env.enums.insert(sym, variants);
                        env.type_names.insert(sym, name_of(e.name()));
                    }
                }
                Item::Const(c) => {
                    if let Some(sym) = decl_symbol(table, interner, c.name(), Namespace::Value) {
                        env.declared.insert(c.syntax().text_range(), sym);
                        let ty = env.annotation_of(c.syntax());
                        env.facts.insert(
                            sym,
                            MemberFacts {
                                ty,
                                effect: None,
                                is_reactive_source: false,
                            },
                        );
                    }
                }
                Item::Fn(f) => {
                    let (params, ret) = (f.params(), f.return_type());
                    env.record_callable(table, interner, f.name(), &params, ret, EffectClass::Read);
                }
                Item::Action(a) => {
                    let (params, ret) = (a.params(), a.return_type());
                    env.record_callable(
                        table,
                        interner,
                        a.name(),
                        &params,
                        ret,
                        EffectClass::Action,
                    );
                }
                Item::Task(t) => {
                    let (params, ret) = (t.params(), t.return_type());
                    env.record_callable(table, interner, t.name(), &params, ret, EffectClass::Task);
                }
                _ => {}
            }
        }
        env
    }

    /// Records one member's name→symbol entry and its facts.
    fn record_member(&mut self, member: &Member, table: &SymbolTable, interner: &mut NameInterner) {
        let (name_tok, namespace, ty, is_source) = match member {
            Member::Input(d) => (
                d.name(),
                Namespace::Value,
                self.annotation_of(d.syntax()),
                true,
            ),
            Member::State(d) => (
                d.name(),
                Namespace::Value,
                self.annotation_of(d.syntax()),
                true,
            ),
            Member::Computed(d) => (
                d.name(),
                Namespace::Value,
                self.annotation_of(d.syntax()),
                true,
            ),
            Member::Event(d) => (d.name(), Namespace::Event, Ty::Unknown, false),
            Member::Fn(d) => {
                let (params, ret) = (d.params(), d.return_type());
                let sym = self.record_callable(
                    table,
                    interner,
                    d.name(),
                    &params,
                    ret,
                    EffectClass::Read,
                );
                if let Some(sym) = sym {
                    self.members.insert(name_of(d.name()), sym);
                }
                return;
            }
            Member::Action(d) => {
                let (params, ret) = (d.params(), d.return_type());
                let sym = self.record_callable(
                    table,
                    interner,
                    d.name(),
                    &params,
                    ret,
                    EffectClass::Action,
                );
                if let Some(sym) = sym {
                    self.members.insert(name_of(d.name()), sym);
                }
                return;
            }
            Member::Task(d) => {
                let (params, ret) = (d.params(), d.return_type());
                let sym = self.record_callable(
                    table,
                    interner,
                    d.name(),
                    &params,
                    ret,
                    EffectClass::Task,
                );
                if let Some(sym) = sym {
                    self.members.insert(name_of(d.name()), sym);
                }
                return;
            }
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
        self.members.insert(text, sym);
        self.facts.insert(
            sym,
            MemberFacts {
                ty,
                effect: None,
                is_reactive_source: is_source,
            },
        );
    }

    /// Records a `fn`/`action`/`task`: its effect class, and for a `fn` or `action` its
    /// signature (a `fn` is also a value of its function type). Returns its symbol.
    fn record_callable(
        &mut self,
        table: &SymbolTable,
        interner: &mut NameInterner,
        name: Option<crate::syntax::SyntaxToken>,
        params: &[Param],
        ret: Option<ReturnType>,
        effect: EffectClass,
    ) -> Option<SymbolId> {
        let sym = decl_symbol(table, interner, name, Namespace::Value)?;
        let params: Vec<Ty> = params
            .iter()
            .map(|p| self.annotation_of(p.syntax()))
            .collect();
        let ret = ret.map_or(Ty::Unit, |r| self.annotation_of(r.syntax()));
        let ty = match effect {
            EffectClass::Read => Ty::Fn(params.clone(), Box::new(ret.clone())),
            _ => Ty::Unknown,
        };
        if !matches!(effect, EffectClass::Task) {
            self.signatures.insert(sym, (params, ret));
        }
        self.facts.insert(
            sym,
            MemberFacts {
                ty,
                effect: Some(effect),
                is_reactive_source: false,
            },
        );
        Some(sym)
    }

    /// The inputs of a component as node properties. An input preceded by a
    /// `@bindable(event)` attribute is two-way.
    fn inputs_of(
        &self,
        decl: &ComponentDecl,
        table: &SymbolTable,
        interner: &mut NameInterner,
    ) -> Vec<InputProp> {
        let mut inputs = Vec::new();
        let mut bindable = false;
        for child in decl.syntax().children() {
            match child.kind() {
                SyntaxKind::Attribute => bindable |= is_bindable(&child),
                SyntaxKind::InputDecl => {
                    if let Some(name) = InputDecl::cast(child.clone()).and_then(|d| d.name()) {
                        inputs.push(InputProp {
                            name: name.text().trim_start_matches("r#").to_string(),
                            ty: self.annotation_of(&child),
                            two_way: bindable,
                            declared_at: name.text_range(),
                            symbol: decl_symbol(
                                table,
                                interner,
                                Some(name.clone()),
                                Namespace::Value,
                            ),
                        });
                    }
                    bindable = false;
                }
                _ => bindable = false,
            }
        }
        inputs
    }

    /// One record field (of a record or a record-payload variant).
    fn field_info(&self, field: &RecordField) -> Option<FieldInfo> {
        let name = field.name()?;
        Some(FieldInfo {
            name: name.text().to_string(),
            ty: self.annotation_of(field.syntax()),
            has_default: field.default().is_some(),
            declared_at: name.text_range(),
        })
    }

    /// One enum variant and its payload: none, `( T,* )`, or `{ field,* }`.
    fn variant_info(&self, variant: &EnumVariant) -> Option<VariantInfo> {
        let name = variant.name()?;
        let payload = match variant
            .syntax()
            .children()
            .into_iter()
            .find(|c| c.kind() == SyntaxKind::VariantPayload)
        {
            None => VariantPayload::Unit,
            Some(payload) => {
                let is_tuple = payload
                    .children_with_tokens()
                    .into_iter()
                    .filter_map(|e| e.as_token().cloned())
                    .any(|t| t.kind() == SyntaxKind::LParen);
                if is_tuple {
                    VariantPayload::Tuple(
                        payload
                            .children()
                            .into_iter()
                            .filter(is_type_node)
                            .map(|c| self.annotation(&c))
                            .collect(),
                    )
                } else {
                    VariantPayload::Record(
                        payload
                            .children()
                            .into_iter()
                            .filter_map(RecordField::cast)
                            .filter_map(|f| self.field_info(&f))
                            .collect(),
                    )
                }
            }
        };
        Some(VariantInfo {
            name: name.text().to_string(),
            payload,
            declared_at: name.text_range(),
        })
    }

    /// The type annotated on `node` (its first type child), `Unknown` when absent.
    fn annotation_of(&self, node: &SyntaxNode) -> Ty {
        node.children()
            .into_iter()
            .find(is_type_node)
            .map_or(Ty::Unknown, |ty| self.annotation(&ty))
    }

    /// A type node lowered to a [`Ty`], nominal names through the resolver's bindings. An
    /// annotation that does not lower is `Unknown` (its diagnostic is raised where the
    /// declaration itself is lowered).
    fn annotation(&self, ty: &SyntaxNode) -> Ty {
        Ty::from_annotation(ty, &|at| self.nominal.get(&at).copied()).unwrap_or(Ty::Unknown)
    }

    /// Points the environment at the component about to be lowered (keyed by its declaration's
    /// syntax range, recorded in the pre-pass), so `component_symbol` answers with its symbol.
    fn focus_component(&self, decl: &ComponentDecl) {
        if let Some(sym) = self.components.get(&decl.syntax().text_range()) {
            self.component.set(*sym);
        }
    }

    /// Records the types lowering inferred for a component's unannotated `state` and
    /// `computed` members.
    fn record_inferred(&self, schema: &ComponentSchema) {
        let mut inferred = self.inferred.borrow_mut();
        let metas = schema
            .states
            .iter()
            .map(|s| &s.meta)
            .chain(schema.computeds.iter().map(|c| &c.meta));
        for meta in metas {
            if let Some(id) = meta.resolved_symbol
                && !meta.inferred_type.has_unknown()
            {
                inferred.insert(id, meta.inferred_type.clone());
            }
        }
    }
}

/// Whether `node` is a type annotation node.
fn is_type_node(node: &SyntaxNode) -> bool {
    matches!(node.kind(), SyntaxKind::TypePath | SyntaxKind::TupleType)
}

/// Whether an attribute is `@bindable(..)`.
fn is_bindable(attr: &SyntaxNode) -> bool {
    attr.children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::PathExpr)
        .is_some_and(|p| p.text().to_string().trim() == "bindable")
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

    /// The diagnostic codes lowering `src` reports, in order.
    fn codes(src: &str) -> Vec<&'static str> {
        lower_src(src).diagnostics.iter().map(|d| d.code).collect()
    }

    fn assert_clean(src: &str) {
        let pkg = lower_src(src);
        assert!(
            pkg.diagnostics.is_empty(),
            "expected no diagnostics, got {:?}",
            pkg.diagnostics
        );
    }

    #[test]
    fn fn_signatures_type_bodies_and_calls() {
        assert_clean(
            "fn square(x: I64) -> I64 { x * x }\n\
             fn twice(x: I64) -> I64 { return square(square(x)); }\n\
             const LIMIT: I64 = 4;",
        );
        assert_eq!(codes("fn f(x: I64) -> String { x }"), ["E2103"]);
        assert_eq!(
            codes("fn f(x: I64) -> I64 { x }\nconst C: I64 = f(\"a\");"),
            ["E2103"]
        );
        assert_eq!(codes("fn f() -> I64 { let y = 1; }"), ["E2103"]);
        assert_eq!(codes("const C: String = 1;"), ["E2103"]);
    }

    #[test]
    fn statements_and_closures() {
        assert_clean(
            "fn f(n: I64) -> I64 {\n\
             \x20 let mut total = 0;\n\
             \x20 for i in 0..n { total += i; }\n\
             \x20 loop { if total > 10 { break; } total += 1; }\n\
             \x20 let found = total;\n\
             \x20 let add = |a: I64, b: I64| a + b;\n\
             \x20 add(found, 1)\n\
             }",
        );
        assert_eq!(codes("fn f() { let c = |a| a; }"), ["E2401"]);
        assert_eq!(codes("fn f() { break; }"), ["E2803"]);
        assert_eq!(codes("fn f() { while true { break 1; } }"), ["E2803"]);
        assert_eq!(
            codes("fn f() { loop { let c = || { continue; }; } }"),
            ["E2803"]
        );
    }

    #[test]
    fn records_and_enums_type_nominally() {
        let decls = "record P { x: I64; y: I64 = 0; }\n\
                     enum Shape { Dot; Circle(I64); Rect { w: I64; }; }\n";
        assert_clean(&format!(
            "{decls}fn f(p: P) -> I64 {{ p.x + p.y }}\n\
             fn g() -> P {{ P {{ x: 1 }} }}\n\
             fn h(s: Shape) -> I64 {{ match s {{ Shape::Dot => 0, Shape::Circle(r) => r, Shape::Rect {{ w }} => w }} }}"
        ));
        let pkg = lower_src(&format!("{decls}fn f(p: P) -> I64 {{ p.z }}"));
        let codes: Vec<_> = pkg.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["E2001"]);
        assert_eq!(
            self::codes(&format!("{decls}fn g() -> P {{ P {{ y: 1 }} }}")),
            ["E2103"],
            "a field without a default must be given"
        );
        assert_eq!(
            self::codes(&format!("{decls}fn g() -> P {{ P {{ x: \"a\" }} }}")),
            ["E2103"]
        );
        assert_eq!(
            self::codes(&format!("{decls}record Q {{ n: I64 = \"a\"; }}")),
            ["E2103"],
            "a field default is typed against the field"
        );
    }

    #[test]
    fn match_exhaustiveness_and_reachability() {
        let decls = "enum Shape { Dot; Circle(I64); }\n";
        assert_eq!(
            codes(&format!(
                "{decls}fn h(s: Shape) -> I64 {{ match s {{ Shape::Dot => 0 }} }}"
            )),
            ["E2301"]
        );
        assert_eq!(
            codes(&format!(
                "{decls}fn h(s: Shape) -> I64 {{ match s {{ _ => 0, Shape::Dot => 1 }} }}"
            )),
            ["E2302"]
        );
    }

    /// The single diagnostic `src` produces, as `(code, message)`.
    fn only(src: &str) -> (&'static str, String) {
        let pkg = lower_src(src);
        match pkg.diagnostics.as_slice() {
            [d] => (d.code, d.message.clone()),
            other => panic!("expected one diagnostic, got {other:?}"),
        }
    }

    #[test]
    fn integer_char_and_string_matches() {
        let (code, message) = only("fn f(n: U8) -> I64 { match n { 0 => 0, 1..=9 => 1 } }");
        assert_eq!(code, "E2301");
        assert!(message.contains("`10..=255`"), "{message}");
        assert_clean("fn f(n: U8) -> I64 { match n { 0 => 0, 1..=9 => 1, 10..=255 => 2 } }");
        assert_clean("fn f(n: I8) -> I64 { match n { -128..0 => 0, 0 => 1, 1..=127 => 2 } }");
        assert_eq!(
            codes("fn f(n: U8) -> I64 { match n { 0..=200 => 0, 5 => 1, _ => 2 } }"),
            ["E2302"]
        );
        let (code, message) = only("fn f(n: I64) -> I64 { match n { 0 => 0, 2 => 1 } }");
        assert_eq!(code, "E2301");
        assert!(message.contains("`1`"), "{message}");
        let (_, message) =
            only("fn f(n: U8) -> I64 { match n { 0 => 0, 2 => 1, 4 => 2, 6 => 3, 8 => 4 } }");
        assert!(message.ends_with("and 1 more not covered"), "{message}");
        assert_clean(
            "fn f(c: Char) -> I64 { match c { '\\0'..='\\u{D7FF}' => 0, '\\u{E000}'..='\\u{10FFFF}' => 1 } }",
        );
        assert_eq!(
            codes("fn f(c: Char) -> I64 { match c { 'a' => 0 } }"),
            ["E2301"]
        );
        assert_eq!(
            codes("fn f(s: String) -> I64 { match s { \"a\" => 0, \"a\" => 1, _ => 2 } }"),
            ["E2302"]
        );
        assert_eq!(
            codes("fn f(s: String) -> I64 { match s { \"a\" => 0 } }"),
            ["E2301"]
        );
        assert_clean(
            "fn f(o: Option<U8>) -> I64 { match o { Some(0..=127) => 0, Some(128..=255) => 1, None => 2 } }",
        );
    }

    #[test]
    fn list_matches() {
        assert_clean("fn f(l: List<I64>) -> I64 { match l { [] => 0, [x, ..] => x } }");
        assert_clean(
            "fn f(l: List<Bool>) -> I64 { match l { [] => 0, [true, ..] => 1, [.., false] => 2, [false, .., true] => 3 } }",
        );
        let (code, message) = only("fn f(l: List<I64>) -> I64 { match l { [] => 0, [_] => 1 } }");
        assert_eq!(code, "E2301");
        assert!(message.contains("`[_, _, ..]`"), "{message}");
        assert_eq!(
            codes("fn f(l: List<I64>) -> I64 { match l { [..] => 0, [_, _] => 1 } }"),
            ["E2302"]
        );
        assert_eq!(
            codes("fn f(l: List<I64>) -> I64 { match l { [.., _, ..] => 0, _ => 1 } }"),
            ["E2103"]
        );
    }

    #[test]
    fn pattern_shapes_are_typed() {
        assert_eq!(
            codes("fn f(n: U8) -> I64 { match n { 256 => 0, _ => 1 } }"),
            ["E2103"]
        );
        assert_eq!(
            codes("fn f(x: F64) -> I64 { match x { 1 => 0, _ => 1 } }"),
            ["E2103"]
        );
        assert_eq!(
            codes("fn f(n: I64) -> I64 { match n { \"a\" => 0, _ => 1 } }"),
            ["E2103"]
        );
        assert_eq!(
            codes("fn f(n: I64) -> I64 { match n { Some(x) => x, _ => 1 } }"),
            ["E2103"]
        );
        assert_eq!(
            codes("fn f(n: I64) -> I64 { match n { 'a'..=9 => 0, _ => 1 } }"),
            ["E2103"]
        );
        assert_eq!(
            codes("fn f(n: I64) -> I64 { match n { 5..1 => 0, _ => 1 } }"),
            ["E2103"]
        );
        assert_eq!(
            codes("fn f(n: I64) -> I64 { match n { [x] => x, _ => 1 } }"),
            ["E2103"]
        );
        assert_eq!(
            codes("fn f(t: (I64, I64)) -> I64 { match t { (a, b, c) => a } }"),
            ["E2103"]
        );
    }

    #[test]
    fn irrefutable_positions() {
        let decls = "record P { x: I64; y: I64; }\nenum E { A(I64); B; }\n";
        assert_clean(&format!(
            "{decls}fn f(p: P, t: (I64, P)) -> I64 {{\n\
             \x20 let P {{ x, .. }} = p;\n\
             \x20 let (a, P {{ y: b, .. }}) = t;\n\
             \x20 for (i, q) in [(1, p)] {{ }}\n\
             \x20 let g = |(m, n): (I64, I64)| m + n;\n\
             \x20 x + a + b\n\
             }}"
        ));
        let (code, message) = only(&format!(
            "{decls}fn f(o: Option<I64>) {{ let Option::Some(x) = o; }}"
        ));
        assert_eq!(code, "E2303");
        assert!(message.contains("`let`"), "{message}");
        assert_eq!(
            codes(&format!("{decls}fn f(e: E) {{ let E::A(x) = e; }}")),
            ["E2303"]
        );
        assert_eq!(codes("fn f(t: (I64, I64)) { let (0, y) = t; }"), ["E2303"]);
        assert_eq!(
            codes("fn f(l: List<I64>) { for [x] in [l] { } }"),
            ["E2303"]
        );
        assert_eq!(codes("fn f() { let g = |1: I64| 0; }"), ["E2303"]);
    }

    #[test]
    fn view_matches_are_checked() {
        let decls = "enum Tab { Home; Settings; }\n";
        assert_clean(&format!(
            "{decls}component C {{\n  state t = Tab::Home\n  view {{ match t {{ Tab::Home => {{ Text {{ }} }}, Tab::Settings => {{ Text {{ }} }} }} }}\n}}"
        ));
        assert_eq!(
            codes(&format!(
                "{decls}component C {{\n  state t = Tab::Home\n  view {{ match t {{ Tab::Home => {{ Text {{ }} }} }} }}\n}}"
            )),
            ["E2301"]
        );
        assert_eq!(
            codes("component C {\n  view { for Some(x) in [Some(1)] { Text { } } }\n}"),
            ["E2303"]
        );
    }

    #[test]
    fn format_templates_are_checked() {
        assert_clean("fn f(n: I64) -> String { format(\"{} of {total}\", n, total: 3) }");
        assert_eq!(
            codes("fn f(n: I64) -> String { format(\"{} {}\", n) }"),
            ["E2108"]
        );
        assert_eq!(
            codes("fn f(n: I64) -> String { format(\"{x}\", n) }"),
            ["E2108", "E2108"]
        );
    }

    #[test]
    fn a_text_property_takes_only_a_string() {
        assert_clean(
            "component C {\n  state n = 0\n  view { Text { text: format(\"{}\", n); } }\n}",
        );
        let src = "component C {\n  state n = 0\n  view { Text { text: n; } }\n}";
        let pkg = lower_src(src);
        let codes: Vec<_> = pkg.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["E2103"]);
        let fix = pkg.diagnostics[0]
            .fixes
            .first()
            .expect("a wrap-in-format fix");
        assert_eq!(
            fix.applicability,
            crate::diag::Applicability::MachineApplicable
        );
        assert_eq!(fix.edits[0].replacement, "format(\"{}\", n)");
        assert_eq!(&src[fix.edits[0].range.as_usize()], "n");

        let decls = "record P { x: I64; }\n";
        let pkg = lower_src(&format!(
            "{decls}component C {{\n  input p: P\n  view {{ Text {{ text: p; }} }}\n}}"
        ));
        assert!(
            pkg.diagnostics.iter().all(|d| d.fixes.is_empty()),
            "a record has no `Display`, so no format fix is offered"
        );
        assert_eq!(
            self::codes(&format!(
                "{decls}fn f(p: P) -> String {{ format(\"{{}}\", p) }}"
            )),
            ["E2108"]
        );
    }

    #[test]
    fn view_properties_are_checked_against_the_schema() {
        assert_clean(
            "component C {\n\
             \x20 state on = true\n\
             \x20 view { Column { width: 50%; Toggle { bind checked <=> on; } Text { text: \"a\"; opacity: 0.5; } } }\n\
             }",
        );
        let pkg = lower_src("component C {\n  view { Text { opacty: 0.5; } }\n}");
        let d = pkg
            .diagnostics
            .iter()
            .find(|d| d.code == "E3101")
            .expect("an unknown property is E3101");
        assert!(
            d.notes.iter().any(|n| n.contains("opacity"))
                || d.related.iter().any(|(_, m)| m.contains("opacity")),
            "the misspelling suggests `opacity`, got {d:?}"
        );
        assert_eq!(
            codes("component C {\n  view { Text { text: \"a\"; text: \"b\"; } }\n}"),
            ["E3102"]
        );
        assert_eq!(
            codes("component C {\n  state s = \"a\"\n  view { Text { bind text <=> s; } }\n}"),
            ["E3103"]
        );
        assert_eq!(
            codes("component C {\n  view { Text { translate: 50%; } }\n}"),
            ["E3104"]
        );
        assert_eq!(
            codes("component C {\n  view { Toggle { checked: 1; } }\n}"),
            ["E2103"]
        );
    }

    #[test]
    fn parent_provided_properties_are_checked_against_the_direct_parent() {
        assert_clean(
            "component C {\n\
             \x20 state on = true\n\
             \x20 view { Grid { Text { grid.row: 1; grid.column_span: 2; } \
             Fragment { Text { grid.column: 0; } } \
             if on { Text { grid.area: \"a\"; } } } \
             Stack { Text { stack.layer: 1; } } Absolute { Text { absolute.top: 50%; } } }\n\
             }",
        );
        assert_eq!(
            codes("component C {\n  view { Row { Text { grid.row: 1; } } }\n}"),
            ["E3702"]
        );
        assert_eq!(
            codes("component C {\n  view { Text { stack.layer: 1; } }\n}"),
            ["E3702"]
        );
        assert_eq!(
            codes(
                "component Card {\n  view { }\n}\n\
                 component C {\n  view { Grid { Card { Text { grid.row: 0; } } } }\n}"
            ),
            ["E3702"]
        );
        let pkg = lower_src("component C {\n  view { Grid { Text { grid.rows: 1; } } }\n}");
        let d = pkg
            .diagnostics
            .iter()
            .find(|d| d.code == "E3101")
            .expect("an unknown child property is E3101");
        assert!(
            d.notes.iter().any(|n| n.contains("row"))
                || d.related.iter().any(|(_, m)| m.contains("row")),
            "the misspelling suggests `row`, got {d:?}"
        );
        assert_eq!(
            codes("component C {\n  view { Grid { Text { grid.row: \"a\"; } } }\n}"),
            ["E2103"]
        );
        assert_eq!(
            codes("component C {\n  view { Grid { Text { grid.row: 50%; } } }\n}"),
            ["E2103"]
        );
    }

    #[test]
    fn input_percent_basis_follows_its_bindings() {
        let card = "component Card {\n\
                    \x20 input size: MixedLength = 0dp\n\
                    \x20 input shift: MixedLength = 0dp\n\
                    \x20 view { Column { width: size + 4dp; translate: Offset { x: shift, y: 0dp }; } }\n\
                    }\n";
        assert_clean(&format!(
            "{card}component App {{\n  view {{ Card {{ size: 50%; shift: 4dp; }} }}\n}}"
        ));
        let pkg = lower_src(&format!(
            "{card}component App {{\n  view {{ Card {{ shift: 50%; }} }}\n}}"
        ));
        let found: Vec<_> = pkg.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(found, ["E3104"]);
        let (_, reason) = &pkg.diagnostics[0].related[0];
        assert!(reason.contains("translate"), "{reason}");

        // Forwarded through another component's input, declared later in the module.
        assert_eq!(
            codes(&format!(
                "component App {{\n  view {{ Frame {{ inset: 10%; }} }}\n}}\n\
                 component Frame {{\n  input inset: MixedLength = 0dp\n  view {{ Card {{ shift: inset; }} }}\n}}\n{card}"
            )),
            ["E3104"]
        );
    }

    #[test]
    fn a_percent_component_flows_through_values() {
        let decls = "record At { x: MixedLength; y: MixedLength; }\n";
        let flagged = |body: &str| codes(&format!("{decls}component C {{\n{body}\n}}"));
        // Through a state initializer, a computed, a field access and a local.
        assert_eq!(
            flagged("  state o = At { x: 50%, y: 0dp }\n  view { Text { translate: o; } }"),
            ["E3104"]
        );
        assert_eq!(
            flagged(
                "  state w = 50%\n  computed c = w + 4dp\n\
                 \x20 view { Text { translate: Offset { x: c, y: 0dp }; } }"
            ),
            ["E3104"]
        );
        assert_eq!(
            flagged(
                "  state o = At { x: 50%, y: 0dp }\n\
                 \x20 view { Text { translate: Offset { x: o.x, y: 0dp }; } }"
            ),
            ["E3104"]
        );
        assert_eq!(
            flagged(
                "  state x: MixedLength = 0dp\n  action go() { let p = 25%; x = p + 1dp; }\n\
                 \x20 view { Text { translate: Offset { x: x, y: 0dp }; } }"
            ),
            ["E3104"]
        );
        // The diagnostic points back at the spelled `Percent`.
        let pkg = lower_src(&format!(
            "{decls}component C {{\n  state o = At {{ x: 50%, y: 0dp }}\n  view {{ Text {{ translate: o; }} }}\n}}"
        ));
        let (_, reason) = &pkg.diagnostics[0].related[0];
        assert!(reason.contains("comes from"), "{reason}");

        // A call's result, a comparison and a percent-typed ratio carry no component.
        assert_clean(
            "fn f(p: Percent) -> MixedLength { 0dp }\n\
             component C {\n  state w = 50%\n\
             \x20 view { Text { translate: Offset { x: f(w), y: 0dp }; visible: w > 10%; } }\n}",
        );
        assert_clean(&format!(
            "{decls}component C {{\n  state o = At {{ x: 4dp, y: 0dp }}\n  view {{ Text {{ translate: o; }} }}\n}}"
        ));
    }

    #[test]
    fn record_defaults_carry_into_literals_that_omit_them() {
        let decls = "record Pad { x: MixedLength = 50%; y: MixedLength = 0dp; }\n";
        assert_eq!(
            codes(&format!(
                "{decls}component C {{\n  state p = Pad {{ y: 1dp }}\n  view {{ Text {{ translate: p; }} }}\n}}"
            )),
            ["E3104"]
        );
        assert_clean(&format!(
            "{decls}component C {{\n  state p = Pad {{ x: 1dp, y: 1dp }}\n  view {{ Text {{ translate: p; }} }}\n}}"
        ));
    }

    #[test]
    fn an_input_reached_through_a_computed_has_no_basis() {
        let card = "component Card {\n\
                    \x20 input shift: MixedLength = 0dp\n\
                    \x20 computed moved = shift + 4dp\n\
                    \x20 view { Column { translate: Offset { x: moved, y: 0dp }; } }\n\
                    }\n";
        assert_clean(&format!(
            "{card}component App {{\n  view {{ Card {{ shift: 4dp; }} }}\n}}"
        ));
        let pkg = lower_src(&format!(
            "{card}component App {{\n  state s = 50%\n  view {{ Card {{ shift: s; }} }}\n}}"
        ));
        let found: Vec<_> = pkg.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(found, ["E3104"]);
        let (_, reason) = &pkg.diagnostics[0].related[0];
        assert!(reason.contains("translate"), "{reason}");
    }

    #[test]
    fn component_inputs_are_node_properties() {
        let stepper = "component Stepper {\n\
                       \x20 @bindable(changed)\n\
                       \x20 input value: I64\n\
                       \x20 input step: I64 = 1\n\
                       \x20 event changed(value: I64)\n\
                       \x20 view { }\n\
                       }\n";
        assert_clean(&format!(
            "{stepper}component App {{\n  state n = 0\n  view {{ Stepper {{ bind value <=> n; step: 2; }} }}\n}}"
        ));
        assert_eq!(
            codes(&format!(
                "{stepper}component App {{\n  state n = 0\n  view {{ Stepper {{ bind step <=> n; }} }}\n}}"
            )),
            ["E3103"]
        );
        assert_eq!(
            codes(&format!(
                "{stepper}component App {{\n  view {{ Stepper {{ stpe: 2; }} }}\n}}"
            )),
            ["E3101"]
        );
        assert_eq!(
            codes(&format!(
                "{stepper}component App {{\n  view {{ Stepper {{ step: \"a\"; }} }}\n}}"
            )),
            ["E2103"]
        );
    }
}
