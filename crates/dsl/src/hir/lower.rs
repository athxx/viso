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
use std::collections::{HashMap, HashSet};

use viso_behavior::native::{NativeId, NativeKind, Natives, ThreadDomain};

use crate::ast::{
    AstNode, Block, CompilationUnit, ComponentDecl, ConstDecl, EnumVariant, InputDecl, Item,
    Member, Param, RecordDecl, RecordField, ReturnType, TypePath,
};
use crate::behavior::Program;
use crate::behavior::ir::FunctionKind;
use crate::behavior::lower::{Def, ProgramBuilder, lower_body, lower_value, unsupported};
use crate::diag::{Diagnostic, Related, Severity};
use crate::resolve::prelude::{Prelude, environment};
use crate::resolve::{
    ModuleGraph, NameInterner, Namespace, Resolution, ResolvedModule, ResolvedRef, SourceUnit,
    SymbolId, SymbolKind, SymbolTable,
};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

use super::capability::{CapabilityNode, CapabilitySet, propagate};
use super::component::{MemberEnv, lower_component};
use super::effect::{BodyContext, EffectClass, EffectCx, EffectEnv};
use super::infer::{
    EventInfo, FieldInfo, InferCx, TypeEnv, TypeSchemas, VariantInfo, VariantPayload,
};
use super::nodes::{ComponentSchema, HirCallable, HirComponent, HirSlot};
use super::ownership::check_stored;
use super::percent::PercentSources;
use super::reads::ReadEnv;
use super::ty::Ty;
use super::view::{
    HandlerSink, InputFlows, InputProp, PercentFlow, ViewEnv, check_input_bases,
    check_percent_flow, check_view,
};

mod input;
mod system;

pub use input::InputDevices;

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
    /// Every body lowered to the Behavior IR, with the reason each one that cannot
    /// run cannot.
    pub behavior: Program,
    /// The package's record and enum declarations, the prelude's included.
    pub types: TypeSchemas,
    /// Every `@migrate` function, across all modules, in module then source order.
    pub migrators: Vec<Migrator>,
}

/// A `fn` marked `@migrate(from: "T")`: hot reload calls it to carry a state
/// whose type changed from `T` to the function's return type, when the value
/// does not convert by itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Migrator {
    /// The old state type, as source spells it.
    pub from: String,
    /// The type of its one parameter, which the old value converts into.
    pub param: Ty,
    /// The new state type it returns.
    pub ret: Ty,
    /// The function.
    pub symbol: SymbolId,
}

/// Lowers a whole resolved package into its typed HIR.
///
/// `graph` and `units` are the module graph and its parse trees (matched by module-path
/// text, exactly as the resolver's `unit_for` does); `resolved` is index-parallel to
/// `graph.modules()` (the resolver's output). `interner` is threaded through so the per-module
/// environment pre-pass can intern member names to query the module's [`SymbolTable`], and
/// `package` is the package identity (unused by lowering directly but kept for symmetry with
/// [`crate::resolve::resolve`] and future native-schema keying). `devices` are the input
/// devices the package's targets have, which every input action needs a binding for.
pub fn lower(
    graph: &ModuleGraph,
    units: &[SourceUnit],
    resolved: &[ResolvedModule],
    interner: &mut NameInterner,
    package: &str,
    devices: InputDevices,
) -> LoweredPackage {
    let _ = package;
    let mut components = Vec::new();
    let mut callables = Vec::new();

    // First pass: every module's declarations into one package table (a module types what
    // it imports exactly as what it declares), and each module's own scope.
    let mut decls = Declarations::default();
    let prelude = Prelude::load(interner);
    ModuleScope::build(
        &prelude.unit,
        &prelude.module.table,
        &prelude.module.refs,
        None,
        interner,
        &mut decls,
    );
    decls.standard = prelude
        .types(interner)
        .map(|(name, id)| (name.to_owned(), id))
        .collect();
    decls.module_paths = graph
        .modules()
        .iter()
        .map(|gm| gm.path.display(interner))
        .collect();
    let modules: Vec<Option<(CompilationUnit, ModuleScope)>> = graph
        .modules()
        .iter()
        .enumerate()
        .map(|(i, gm)| {
            let resolved_module = resolved.get(i)?;
            let module_text = gm.path.display(interner);
            let cu = unit_for(units, &module_text, interner)?;
            let scope = ModuleScope::build(
                &cu,
                &resolved_module.table,
                &resolved_module.refs,
                Some(i),
                interner,
                &mut decls,
            );
            Some((cu, scope))
        })
        .collect();
    decls.input_action = Some(input::action_type(&decls));
    decls.devices = devices;

    // Second pass: lower each module against the package table. The capability call graph
    // spans the package, so a call into an imported callable is an edge like any other.
    let mut per_module: Vec<Vec<Diagnostic>> = Vec::with_capacity(modules.len());
    let mut input_flows: Vec<InputFlows> = Vec::with_capacity(modules.len());
    let mut cap = CapabilityGraphBuilder::default();
    let behavior = RefCell::new(ProgramBuilder::new());
    let mut migrators = Vec::new();
    let mut systems = Vec::new();
    for (i, module) in modules.iter().enumerate() {
        let mut module_diagnostics = Vec::new();
        let mut flows = InputFlows::default();
        if let (Some((cu, scope)), Some(resolved_module)) = (module, resolved.get(i)) {
            let env = ModuleEnv::new(&decls, scope, i, &behavior, graph.natives());
            cap.module = i;
            flows = lower_module(
                cu,
                &resolved_module.refs,
                &env,
                &mut components,
                &mut callables,
                &mut module_diagnostics,
                &mut cap,
                &mut systems,
            );
            collect_migrators(cu, &env, &mut module_diagnostics, &mut migrators);
        }
        per_module.push(module_diagnostics);
        input_flows.push(flows);
    }
    cap.finish(&mut components, &mut per_module);
    check_input_bases(&input_flows, &mut per_module);
    let order = system::order(&systems, &mut per_module);
    behavior.borrow_mut().order_systems(&order);

    let mut diagnostics = Vec::new();
    let mut module_diagnostics = Vec::with_capacity(per_module.len());
    for module in per_module {
        let start = diagnostics.len();
        diagnostics.extend(module);
        module_diagnostics.push(start..diagnostics.len());
    }

    // Debug-only HIR-complete assertion (spec node-contract section): no core node of a
    // package that compiled cleanly may keep an undetermined type. A user error recovers
    // with an undetermined type and surfaces as a diagnostic, so only a residue without one
    // is a lowering bug.
    debug_assert!(
        diagnostics.iter().any(|d| d.severity == Severity::Error)
            || hir_is_complete(&components, &callables),
        "lowering left an undetermined type on a core HIR node"
    );

    LoweredPackage {
        components,
        callables,
        diagnostics,
        module_diagnostics,
        behavior: behavior.into_inner().finish(),
        types: std::mem::take(&mut decls.types),
        migrators: migrators.into_iter().map(|(_, m)| m).collect(),
    }
}

/// Checks the `@migrate` functions of one compilation unit, at module level
/// and in each component and system, into `migrators`.
fn collect_migrators(
    cu: &CompilationUnit,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    migrators: &mut Vec<(TextRange, Migrator)>,
) {
    check_migrators(cu.syntax(), env, diagnostics, migrators, &|f| {
        env.scope.declared.get(&f.text_range()).copied()
    });
    for item in cu.items() {
        let decl = match item {
            Item::Export(e) => e.declaration(),
            other => Some(other),
        };
        let component = match decl {
            Some(Item::Component(c)) => Some(c),
            Some(Item::System(s)) => Some(s.as_component()),
            _ => None,
        };
        if let Some(c) = component {
            env.focus_component(&c);
            check_migrators(c.syntax(), env, diagnostics, migrators, &|f| {
                env.member_symbol(&support_name(f)?)
            });
        }
    }
}

/// Lowers one compilation unit's components/systems and module-level callables, running the
/// effect checks over their bodies and registering every callable in the capability graph;
/// returns what its views say about the percent bases of component inputs.
#[allow(clippy::too_many_arguments)]
fn lower_module(
    cu: &CompilationUnit,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    components: &mut Vec<HirComponent>,
    callables: &mut Vec<HirCallable>,
    diagnostics: &mut Vec<Diagnostic>,
    cap: &mut CapabilityGraphBuilder,
    systems: &mut Vec<system::SystemNode>,
) -> InputFlows {
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
        // Module-level `fn`/`action`/`task` bodies, `const` values and record field defaults
        // are type-checked here; their HIR nodes land with their consumer slice.
        match decl {
            Item::Component(c) => {
                env.focus_component(&c);
                let component =
                    lower_component_item(&c, refs, env, diagnostics, cap, &mut flows, &mut percent);
                components.push(component);
            }
            Item::System(s) => {
                let c = s.as_component();
                env.focus_component(&c);
                system::check_members(&s, diagnostics);
                let component =
                    lower_component_item(&c, refs, env, diagnostics, cap, &mut flows, &mut percent);
                let hooks = system::hooks(&s, env, diagnostics);
                let symbol = component.schema.symbol;
                env.behavior.borrow_mut().system(symbol, &hooks);
                systems.push(system::node(&s, symbol, env, diagnostics));
                components.push(component);
            }
            Item::Const(c) => {
                check_const(&c, refs, env, diagnostics, &mut percent);
                input::lower_map(&c, refs, env, diagnostics);
            }
            Item::Enum(e) => input::check_derives(&e, env, diagnostics),
            Item::Record(r) => check_field_defaults(&r, refs, env, diagnostics, &mut percent),
            Item::Fn(f) => {
                let callable = Callable {
                    name: name_of(f.name()),
                    kind: FunctionKind::Fn,
                    context: BodyContext::Fn,
                    symbol: env.scope.declared.get(&f.syntax().text_range()).copied(),
                    params: f.params(),
                    ret: f.return_type(),
                    body: f.body(),
                    clause: f.capability_clause(),
                };
                check_callable(&callable, refs, env, diagnostics, cap, &mut percent);
            }
            Item::Action(a) => {
                let callable = Callable {
                    name: name_of(a.name()),
                    kind: FunctionKind::Action,
                    context: BodyContext::Action,
                    symbol: env.scope.declared.get(&a.syntax().text_range()).copied(),
                    params: a.params(),
                    ret: a.return_type(),
                    body: a.body(),
                    clause: a.capability_clause(),
                };
                check_callable(&callable, refs, env, diagnostics, cap, &mut percent);
            }
            Item::Task(t) => {
                let callable = Callable {
                    name: name_of(t.name()),
                    kind: FunctionKind::Fn,
                    context: BodyContext::Task,
                    symbol: env.scope.declared.get(&t.syntax().text_range()).copied(),
                    params: t.params(),
                    ret: t.return_type(),
                    body: t.body(),
                    clause: t.capability_clause(),
                };
                check_callable(&callable, refs, env, diagnostics, cap, &mut percent);
            }
            _ => {}
        }
        let _ = &mut *callables;
    }

    let facts = percent.solve();
    check_percent_flow(&flows, &facts, env, diagnostics)
}

/// Lowers one `component` declaration: schema + view/callable effect checks, registering each
/// callable in the capability graph and collecting what its view reveals about input percent
/// bases into `flows`.
fn lower_component_item(
    decl: &ComponentDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    cap: &mut CapabilityGraphBuilder,
    flows: &mut Vec<PercentFlow>,
    percent: &mut PercentSources,
) -> HirComponent {
    env.focus_component(decl);
    check_bindable(decl, env, diagnostics);
    let schema = lower_component(decl, refs, env, diagnostics, percent);
    env.record_inferred(&schema);
    check_schema_ownership(&schema, env, diagnostics);
    env.behavior.borrow_mut().component(&schema);
    lower_member_values(decl, refs, env, &schema, diagnostics);
    let source_origin = decl.syntax().text_range();

    // Effect-check the view body (a reactive context) and every callable body in its context.
    if let Some(view) = decl.view()
        && let Some(block) = view.block()
    {
        let sink = HandlerSink {
            builder: env.behavior,
            module: env.module,
            component: &schema.name,
        };
        flows.push(check_view(
            refs,
            env,
            Some(env.component_symbol()),
            &block,
            diagnostics,
            percent,
            Some(sink),
        ));
        check_body(refs, BodyContext::View, env, block.syntax(), diagnostics);
    }
    for member in decl.members() {
        let (context, body) = match &member {
            Member::Computed(d) => (BodyContext::Computed, d.body()),
            Member::State(d) => (BodyContext::Initializer, d.initializer()),
            Member::Input(d) => (BodyContext::Initializer, d.default()),
            _ => continue,
        };
        if let Some(body) = body {
            check_body(refs, context, env, body.syntax(), diagnostics);
        }
    }
    check_component_callables(decl, refs, env, diagnostics, cap, percent);

    HirComponent {
        schema,
        source_origin,
    }
}

/// Reports each `state`, `input`, `computed` and event payload of a component
/// that holds a borrowed native handle (`E6102`).
fn check_schema_ownership(
    schema: &ComponentSchema,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let places = schema
        .states
        .iter()
        .map(|s| ("a `state`", &s.meta))
        .chain(schema.inputs.iter().map(|i| ("an `input`", &i.meta)))
        .chain(schema.computeds.iter().map(|c| ("a `computed`", &c.meta)))
        .chain(schema.events.iter().map(|e| ("an event payload", &e.meta)));
    for (place, meta) in places {
        check_stored(
            &meta.inferred_type,
            env.natives,
            place,
            meta.source_origin,
            diagnostics,
        );
    }
}

/// Checks each `fn`/`action`/`task` of a component (see [`check_callable`]).
fn check_component_callables(
    decl: &ComponentDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    cap: &mut CapabilityGraphBuilder,
    percent: &mut PercentSources,
) {
    let component = name_of(decl.name());
    for member in decl.members() {
        let callable = match &member {
            Member::Fn(f) => Callable {
                name: format!("{component}.{}", name_of(f.name())),
                kind: FunctionKind::Fn,
                context: BodyContext::Fn,
                symbol: env.member_symbol(&name_of(f.name())),
                params: f.params(),
                ret: f.return_type(),
                body: f.body(),
                clause: f.capability_clause(),
            },
            Member::Action(a) => Callable {
                name: format!("{component}.{}", name_of(a.name())),
                kind: FunctionKind::Action,
                context: BodyContext::Action,
                symbol: env.member_symbol(&name_of(a.name())),
                params: a.params(),
                ret: a.return_type(),
                body: a.body(),
                clause: a.capability_clause(),
            },
            Member::Task(t) => Callable {
                name: format!("{component}.{}", name_of(t.name())),
                kind: FunctionKind::Fn,
                context: BodyContext::Task,
                symbol: env.member_symbol(&name_of(t.name())),
                params: t.params(),
                ret: t.return_type(),
                body: t.body(),
                clause: t.capability_clause(),
            },
            _ => continue,
        };
        check_callable(&callable, refs, env, diagnostics, cap, percent);
    }
}

/// A `fn`/`action`/`task` declaration, a component's or the module's.
struct Callable {
    /// Its name in the Behavior IR (`Component.member` for a component member).
    name: String,
    /// What it lowers to.
    kind: FunctionKind,
    context: BodyContext,
    symbol: Option<SymbolId>,
    params: Vec<Param>,
    ret: Option<ReturnType>,
    body: Option<Block>,
    clause: Option<crate::ast::CapabilityClause>,
}

/// Effect-checks a callable's body in its body context, types it against its signature,
/// and registers it in the capability graph (its declared `requires {}` bound and the
/// callables its body calls).
fn check_callable(
    callable: &Callable,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    cap: &mut CapabilityGraphBuilder,
    percent: &mut PercentSources,
) {
    let body = &callable.body;
    let def = Def {
        name: callable.name.clone(),
        kind: callable.kind,
        symbol: callable.symbol,
        module: env.module,
        into: None,
    };
    let def = if callable.context == BodyContext::Task {
        let at = body
            .as_ref()
            .map_or_else(|| TextRange::empty(0.into()), |b| b.syntax().text_range());
        unsupported(
            &mut env.behavior.borrow_mut(),
            def,
            "is a `task`, which runs on the task runtime",
            at,
        );
        None
    } else {
        Some(def)
    };
    check_signature(refs, env, callable, diagnostics, percent, def);
    if let Some(block) = body {
        check_body(refs, callable.context, env, block.syntax(), diagnostics);
    }
    let declared = callable
        .clause
        .as_ref()
        .map(|c| (capability_set_of(c), c.syntax().text_range()));
    let calls = body
        .as_ref()
        .map(|b| callee_symbols(refs, b.syntax()))
        .unwrap_or_default();
    let direct = body
        .as_ref()
        .map(|b| env.native_capabilities(b.syntax().text_range()))
        .unwrap_or_default();
    cap.add(callable.symbol, direct, declared, calls);
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
    env: &ModuleEnv<'_>,
    callable: &Callable,
    diagnostics: &mut Vec<Diagnostic>,
    percent: &mut PercentSources,
    def: Option<Def>,
) {
    let Some(body) = &callable.body else {
        return;
    };
    let returns_value = callable.ret.is_some();
    let ret = callable.ret.as_ref().map(|r| env.annotation_of(r.syntax()));
    if let (Some(ty), Some(at)) = (&ret, &callable.ret) {
        check_stored(
            ty,
            env.natives,
            "a returned value",
            at.syntax().text_range(),
            diagnostics,
        );
    }
    let mut cx = InferCx::new(refs, env);
    for param in callable.params.iter().filter(|p| {
        p.syntax()
            .children_with_tokens()
            .into_iter()
            .any(|e| e.as_token().is_some_and(|t| t.kind() == SyntaxKind::MutKw))
    }) {
        if let Some(name) = param.name() {
            cx.mark_mutable(name.text_range());
        }
    }
    let params: Vec<(TextRange, Ty)> = callable
        .params
        .iter()
        .filter_map(|p| Some((p.name()?.text_range(), env.annotation_of(p.syntax()))))
        .collect();
    cx.check_callable(&params, ret.as_ref(), body);
    if let Some(def) = def {
        let mut b = env.behavior.borrow_mut();
        if has_errors(cx.diagnostics()) {
            unsupported(&mut b, def, TYPE_ERRORS, body.syntax().text_range());
        } else {
            let spans: Vec<TextRange> = params.iter().map(|(at, _)| *at).collect();
            lower_body(&mut b, &cx, def, &spans, body, returns_value);
        }
    }
    percent.extend(cx.take_percent_defs());
    diagnostics.extend(cx.into_diagnostics());
}

/// Types a `const` value against its annotation.
fn check_const(
    decl: &ConstDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    percent: &mut PercentSources,
) {
    let Some(value) = decl.value() else {
        return;
    };
    let want = env.annotation_of(decl.syntax());
    let mut cx = InferCx::new(refs, env);
    let ty = if want.has_unknown() {
        cx.infer_expr(&value, None)
    } else {
        cx.infer_promoted(&value, &want)
    };
    check_stored(
        &ty,
        env.natives,
        "a `const`",
        decl.syntax().text_range(),
        diagnostics,
    );
    check_body(
        refs,
        BodyContext::Initializer,
        env,
        value.syntax(),
        diagnostics,
    );
    let symbol = env.scope.declared.get(&decl.syntax().text_range()).copied();
    if let Some(symbol) = symbol {
        percent.define(Resolution::Symbol(symbol), cx.carry(value.syntax()));
    }
    let def = Def {
        name: name_of(decl.name()),
        kind: FunctionKind::Const,
        symbol,
        module: env.module,
        into: None,
    };
    lower_checked(env, &cx, def, &value, 0);
    percent.extend(cx.take_percent_defs());
    diagnostics.extend(cx.into_diagnostics());
}

/// Types each record field default against its field type; a record literal that omits
/// a defaulted field carries what the defaults do.
fn check_field_defaults(
    decl: &RecordDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    percent: &mut PercentSources,
) {
    let record = env.scope.declared.get(&decl.syntax().text_range()).copied();
    let record_name = name_of(decl.name());
    let mut cx = InferCx::new(refs, env);
    for (index, field) in decl.fields().enumerate() {
        let want = env.annotation_of(field.syntax());
        check_stored(
            &want,
            env.natives,
            "a record field",
            field.syntax().text_range(),
            diagnostics,
        );
        let Some(value) = field.default() else {
            continue;
        };
        let errors = cx.diagnostics().len();
        let _ = if want.has_unknown() {
            cx.infer_expr(&value, None)
        } else {
            cx.infer_promoted(&value, &want)
        };
        check_body(
            refs,
            BodyContext::Initializer,
            env,
            value.syntax(),
            diagnostics,
        );
        if let Some(record) = record {
            percent.define(Resolution::Symbol(record), cx.carry(value.syntax()));
            let name = format!("{record_name}.{}", name_of(field.name()));
            let slot = env
                .behavior
                .borrow_mut()
                .field_default_slot(record, index as u32, &name);
            let def = Def {
                name,
                kind: FunctionKind::FieldDefault,
                symbol: Some(record),
                module: env.module,
                into: Some(slot),
            };
            lower_checked(env, &cx, def, &value, errors);
        }
    }
    percent.extend(cx.take_percent_defs());
    diagnostics.extend(cx.into_diagnostics());
}

/// The reason a body whose typing raised an error cannot run.
const TYPE_ERRORS: &str = "has type errors";

/// Whether `diagnostics` holds an error.
fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

/// Lowers the value `value` that `cx` typed, unless its typing raised an error
/// (any of `cx`'s diagnostics from index `from` on).
fn lower_checked(
    env: &ModuleEnv<'_>,
    cx: &InferCx<'_>,
    def: Def,
    value: &crate::ast::Expr,
    from: usize,
) -> crate::behavior::ir::FuncId {
    let mut b = env.behavior.borrow_mut();
    if has_errors(&cx.diagnostics()[from..]) {
        unsupported(&mut b, def, TYPE_ERRORS, value.syntax().text_range())
    } else {
        lower_value(&mut b, cx, def, value)
    }
}

/// Lowers each `state` initializer, `computed` body and `input` default of a
/// component to the Behavior IR.
///
/// The values are typed again here against the members' settled types: the
/// schema pass types each one before the types of the members it reads are all
/// inferred. Only an `input` default's diagnostics are new; the others were
/// reported by the schema pass.
fn lower_member_values(
    decl: &ComponentDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    schema: &ComponentSchema,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let settled = |name: &str| {
        schema
            .states
            .iter()
            .map(|s| (&s.name, &s.meta))
            .chain(schema.computeds.iter().map(|c| (&c.name, &c.meta)))
            .chain(schema.inputs.iter().map(|i| (&i.name, &i.meta)))
            .find(|(n, _)| n.as_str() == name)
            .map(|(_, meta)| meta)
    };
    for member in decl.members() {
        let (kind, name, value) = match &member {
            Member::State(d) => (FunctionKind::StateInit, d.name(), d.initializer()),
            Member::Computed(d) => (FunctionKind::Computed, d.name(), d.body()),
            Member::Input(d) => (FunctionKind::InputDefault, d.name(), d.default()),
            _ => continue,
        };
        let name = name_of(name);
        let (Some(value), Some(meta)) = (value, settled(&name)) else {
            continue;
        };
        let want = &meta.inferred_type;
        let mut cx = InferCx::new(refs, env);
        let _ = if want.has_unknown() {
            cx.infer_expr(&value, None)
        } else {
            cx.infer_promoted(&value, want)
        };
        let def = Def {
            name: format!("{}.{name}", schema.name),
            kind,
            symbol: meta.resolved_symbol,
            module: env.module,
            into: None,
        };
        let func = lower_checked(env, &cx, def, &value, 0);
        if let Some(symbol) = meta.resolved_symbol {
            match kind {
                FunctionKind::StateInit => env.behavior.borrow_mut().state_init(symbol, func),
                FunctionKind::InputDefault => {
                    env.behavior.borrow_mut().input_default(symbol, func);
                }
                _ => {}
            }
        }
        if kind == FunctionKind::InputDefault {
            diagnostics.extend(cx.into_diagnostics());
        }
    }
}

/// Whether every core HIR node in the package carries a determined type (the HIR-complete
/// assertion). Nominal `Unknown` types on view/callable placeholders are exempt only where the
/// node contract allows them; a core member (`input`/`state`/`computed`) that omitted its type
/// must have inferred one.
fn hir_is_complete(components: &[HirComponent], callables: &[HirCallable]) -> bool {
    for component in components {
        for state in &component.schema.states {
            // A state whose type was annotated may be a nominal `Unknown` (resolved elsewhere
            // this slice); an omitted-type state must have been determined.
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
/// graph identity and the lowered node its inferred set is written back onto), its declared
/// `requires {}` bound, the symbols it calls, and the module it is declared in.
struct PendingCallable {
    symbol: Option<SymbolId>,
    /// The capabilities the native functions its body calls require.
    direct: CapabilitySet,
    declared: Option<(CapabilitySet, TextRange)>,
    calls: Vec<SymbolId>,
    module: usize,
}

/// Accumulates the package's callables into a symbol-keyed capability call graph, then
/// resolves it and writes inferred sets back onto the lowered callable nodes.
#[derive(Default)]
struct CapabilityGraphBuilder {
    pending: Vec<PendingCallable>,
    /// The module the callables being added are declared in.
    module: usize,
}

impl CapabilityGraphBuilder {
    /// Records one callable of the current module.
    fn add(
        &mut self,
        symbol: Option<SymbolId>,
        direct: CapabilitySet,
        declared: Option<(CapabilitySet, TextRange)>,
        calls: Vec<SymbolId>,
    ) {
        self.pending.push(PendingCallable {
            symbol,
            direct,
            declared,
            calls,
            module: self.module,
        });
    }

    /// Resolves the graph: maps symbols to node indices, turns each call into an edge (a
    /// call to anything but a callable of the package — a native, a closure — adds none),
    /// runs [`propagate`], writes each inferred set back onto the lowered callable with the
    /// same symbol, and appends each `E2601` to its module's diagnostics.
    fn finish(self, components: &mut [HirComponent], diagnostics: &mut [Vec<Diagnostic>]) {
        if self.pending.is_empty() {
            return;
        }

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
                direct: p.direct.clone(),
                declared: p.declared.clone(),
                calls: p
                    .calls
                    .iter()
                    .filter_map(|s| index_of.get(s).copied())
                    .collect(),
            })
            .collect();

        let (inferred, diags) = propagate(&nodes);
        for (node, diagnostic) in diags {
            if let Some(module) = diagnostics.get_mut(self.pending[node].module) {
                module.push(diagnostic);
            }
        }

        let mut inferred_of: HashMap<SymbolId, CapabilitySet> = HashMap::new();
        for (p, set) in self.pending.iter().zip(inferred) {
            if let Some(sym) = p.symbol
                && !set.is_empty()
            {
                inferred_of.insert(sym, set);
            }
        }
        if inferred_of.is_empty() {
            return;
        }
        for callable in components
            .iter_mut()
            .flat_map(|c| c.schema.callables.iter_mut())
        {
            if let Some(set) = callable
                .meta
                .resolved_symbol
                .and_then(|sym| inferred_of.get(&sym))
            {
                callable.meta.capability_set = set.clone();
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
    /// What kind of declaration the symbol is, for `TypeEnv::symbol_kind`.
    kind: SymbolKind,
}

/// Every declaration in the package, keyed by its durable symbol: what each one is, the
/// fields of each record and event payload, the variants of each enum, and each
/// component's inputs and events.
#[derive(Default)]
struct Declarations {
    /// The events of every component.
    events: HashMap<SymbolId, Vec<EventInfo>>,
    /// Symbol → facts, for the type/effect/read trait methods.
    facts: HashMap<SymbolId, MemberFacts>,
    /// The fields of every record and event payload, the variants of every enum, and
    /// the declared name of every record, enum, event and component.
    types: TypeSchemas,
    /// The `(params, ret)` signature of every `fn` and `action`.
    signatures: HashMap<SymbolId, (Vec<Ty>, Ty)>,
    /// The inputs of every component, as node properties.
    inputs: HashMap<SymbolId, Vec<InputProp>>,
    /// The slots of every component, which its callers fill.
    slots: HashMap<SymbolId, Vec<HirSlot>>,
    /// Every system, which `@after`/`@before` may name.
    systems: HashSet<SymbolId>,
    /// Every enum deriving `InputAction`.
    input_derives: HashSet<SymbolId>,
    /// Every `InputMap` constant, in module order; the first is the package's.
    input_maps: Vec<input::MapDecl>,
    /// The type of an input action, once every module is declared.
    input_action: Option<Ty>,
    /// The input devices the package's targets have.
    devices: InputDevices,
    /// The prelude's types by name.
    standard: HashMap<String, SymbolId>,
    /// The index of the module declaring each record, enum, event, component and
    /// system; the prelude's are absent, having no file to point into.
    homes: HashMap<SymbolId, usize>,
    /// The `::`-joined path of every module, by graph index.
    module_paths: Vec<String>,
}

/// What one module adds to the package [`Declarations`]: its owners' member names, its
/// declarations' spans, and the resolver's bindings for its type annotations.
#[derive(Default)]
struct ModuleScope {
    /// Owner → member name → symbol, both value and event namespaces, for
    /// [`MemberEnv::member_symbol`].
    members: HashMap<SymbolId, HashMap<String, SymbolId>>,
    /// Component declaration syntax range → its symbol, so the env can focus on the component
    /// currently being lowered without confusing several components in one module.
    components: HashMap<TextRange, SymbolId>,
    /// Name token span → the type the resolver bound it to, for nominal annotations.
    nominal: HashMap<TextRange, Ty>,
    /// `const`, record, enum and module-level callable declaration syntax range → its
    /// symbol.
    declared: HashMap<TextRange, SymbolId>,
    /// The module's graph index, or `None` for the prelude.
    home: Option<usize>,
}

/// The concrete [`MemberEnv`]/[`TypeEnv`]/[`ViewEnv`]/[`ReadEnv`]/[`EffectEnv`] for one
/// module: its scope over the package declarations.
struct ModuleEnv<'p> {
    decls: &'p Declarations,
    scope: &'p ModuleScope,
    /// The component currently being lowered, answered by [`MemberEnv::component_symbol`]. A
    /// module may declare several components, so the *current* one is set per component
    /// before its `lower_component` call. `Cell` keeps the trait methods `&self`.
    component: Cell<SymbolId>,
    /// The types lowering inferred for unannotated `state`/`computed` members, recorded
    /// as each component lowers so later body walks see them.
    inferred: RefCell<HashMap<SymbolId, Ty>>,
    /// The module's graph index.
    module: usize,
    /// The package's Behavior IR, which each body joins once it is typed.
    behavior: &'p RefCell<ProgramBuilder>,
    /// The native registry the module's native paths resolved against.
    natives: &'p Natives,
    /// Call expression range → the native function it calls, recorded as each body is
    /// typed so its effect walk sees the native's effect and thread domain.
    native_calls: RefCell<HashMap<TextRange, NativeId>>,
}

impl TypeEnv for ModuleEnv<'_> {
    fn resolution_ty(&self, to: &Resolution) -> Option<Ty> {
        match to {
            Resolution::Symbol(id) => self
                .decls
                .facts
                .get(id)
                .map(|f| f.ty.clone())
                .filter(|ty| *ty != Ty::Unknown)
                .or_else(|| self.inferred.borrow().get(id).cloned()),
            Resolution::Env => Some(Ty::Named(environment())),
            Resolution::Local(_) | Resolution::Native(_) => None,
        }
    }

    fn natives(&self) -> Option<&Natives> {
        Some(self.natives)
    }

    fn input_action(&self) -> Ty {
        self.decls.input_action.clone().unwrap_or(Ty::Unknown)
    }

    fn record_native(&self, call: TextRange, id: NativeId) {
        self.native_calls.borrow_mut().insert(call, id);
    }

    fn callee_signature(&self, to: &Resolution) -> Option<(Vec<Ty>, Ty)> {
        match to {
            Resolution::Symbol(id) => self.decls.signatures.get(id).cloned(),
            Resolution::Local(_) | Resolution::Native(_) | Resolution::Env => None,
        }
    }

    fn record_fields(&self, ty: SymbolId) -> Option<&[FieldInfo]> {
        self.decls.types.records.get(&ty).map(Vec::as_slice)
    }

    fn enum_variants(&self, ty: SymbolId) -> Option<&[VariantInfo]> {
        self.decls.types.enums.get(&ty).map(Vec::as_slice)
    }

    fn type_name(&self, ty: SymbolId) -> Option<&str> {
        self.decls.types.names.get(&ty).map(String::as_str)
    }

    fn symbol_kind(&self, id: SymbolId) -> Option<SymbolKind> {
        self.decls.facts.get(&id).map(|f| f.kind)
    }

    fn component_events(&self, component: SymbolId) -> Option<&[EventInfo]> {
        self.decls.events.get(&component).map(Vec::as_slice)
    }

    fn enclosing_component(&self) -> Option<SymbolId> {
        Some(self.component.get())
    }

    fn declaration_site(
        &self,
        owner: SymbolId,
        range: TextRange,
    ) -> Option<(Option<&str>, TextRange)> {
        let home = *self.decls.homes.get(&owner)?;
        let module = (home != self.module).then(|| self.decls.module_paths[home].as_str());
        Some((module, range))
    }
}

impl ViewEnv for ModuleEnv<'_> {
    fn component_inputs(&self, component: SymbolId) -> Option<&[InputProp]> {
        self.decls.inputs.get(&component).map(Vec::as_slice)
    }

    fn component_slots(&self, component: SymbolId) -> Option<&[HirSlot]> {
        self.decls.slots.get(&component).map(Vec::as_slice)
    }

    fn standard_type(&self, name: &str) -> Option<SymbolId> {
        self.decls.standard.get(name).copied()
    }

    fn widgets(&self) -> &Natives {
        self.natives
    }
}

impl ReadEnv for ModuleEnv<'_> {
    fn reactive_source(&self, to: &Resolution) -> Option<SymbolId> {
        match to {
            Resolution::Symbol(id) => self.decls.facts.get(id).and_then(|f| {
                if f.is_reactive_source {
                    Some(*id)
                } else {
                    None
                }
            }),
            Resolution::Local(_) | Resolution::Native(_) | Resolution::Env => None,
        }
    }
}

impl EffectEnv for ModuleEnv<'_> {
    fn callee_effect(&self, to: &Resolution) -> Option<EffectClass> {
        match to {
            Resolution::Symbol(id) => self.decls.facts.get(id).and_then(|f| f.effect),
            Resolution::Local(_) | Resolution::Native(_) | Resolution::Env => None,
        }
    }

    fn native_call(&self, call: TextRange) -> Option<(EffectClass, ThreadDomain)> {
        let id = *self.native_calls.borrow().get(&call)?;
        let function = self.natives.function_by_id(id)?.function;
        let class = match function.kind {
            NativeKind::Fn if function.deterministic => EffectClass::Pure,
            NativeKind::Fn => EffectClass::Read,
            NativeKind::Action => EffectClass::Action,
            NativeKind::Task => EffectClass::Task,
        };
        Some((class, function.thread))
    }

    fn is_state(&self, to: &Resolution) -> bool {
        matches!(to, Resolution::Symbol(id)
            if self.decls.facts.get(id).is_some_and(|f| f.kind == SymbolKind::State))
    }
}

impl MemberEnv for ModuleEnv<'_> {
    fn member_symbol(&self, name: &str) -> Option<SymbolId> {
        self.scope
            .members
            .get(&self.component.get())?
            .get(name)
            .copied()
    }

    fn component_symbol(&self) -> SymbolId {
        self.component.get()
    }
}

impl<'p> ModuleEnv<'p> {
    fn new(
        decls: &'p Declarations,
        scope: &'p ModuleScope,
        module: usize,
        behavior: &'p RefCell<ProgramBuilder>,
        natives: &'p Natives,
    ) -> Self {
        ModuleEnv {
            decls,
            scope,
            component: Cell::new(SymbolId::from_parts(0, 0)),
            inferred: RefCell::default(),
            module,
            behavior,
            natives,
            native_calls: RefCell::default(),
        }
    }

    /// The capabilities the native functions called within `body` require.
    fn native_capabilities(&self, body: TextRange) -> CapabilitySet {
        let mut set = CapabilitySet::new();
        for (call, id) in self.native_calls.borrow().iter() {
            if body.contains_range(*call)
                && let Some(entry) = self.natives.function_by_id(*id)
            {
                for capability in entry.function.capabilities {
                    set.insert(*capability);
                }
            }
        }
        set
    }

    /// The type annotated on `node`, through this module's bindings.
    fn annotation_of(&self, node: &SyntaxNode) -> Ty {
        self.scope.annotation_of(node)
    }

    /// Points the environment at the component about to be lowered (keyed by its declaration's
    /// syntax range, recorded in the pre-pass), so `component_symbol` answers with its symbol.
    fn focus_component(&self, decl: &ComponentDecl) {
        if let Some(sym) = self.scope.components.get(&decl.syntax().text_range()) {
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

impl ModuleScope {
    /// Records that `sym` is declared in this module.
    fn place(&self, decls: &mut Declarations, sym: SymbolId) {
        if let Some(home) = self.home {
            decls.homes.insert(sym, home);
        }
    }

    /// Walks every declaration once (with the interner, to intern names and query the
    /// table), recording the module's member names and declaration spans here and each
    /// declaration's facts, fields, variants, signature and inputs into `decls`.
    fn build(
        cu: &CompilationUnit,
        table: &SymbolTable,
        refs: &[ResolvedRef],
        home: Option<usize>,
        interner: &mut NameInterner,
        decls: &mut Declarations,
    ) -> ModuleScope {
        let mut scope = ModuleScope {
            home,
            nominal: refs
                .iter()
                .filter_map(|r| Some((r.range, r.to.nominal()?)))
                .collect(),
            ..ModuleScope::default()
        };

        for item in cu.items() {
            let decl = match item {
                Item::Export(e) => match e.declaration() {
                    Some(inner) => inner,
                    None => continue,
                },
                other => other,
            };
            let system = match &decl {
                Item::System(s) => Some(s.as_component()),
                _ => None,
            };
            match &decl {
                Item::Component(_) | Item::System(_) => {
                    // Record this component's symbol keyed by its declaration span, so the env
                    // can focus on whichever component it is currently lowering (a module may
                    // declare several), and its members from its own member table. A system
                    // is a component without a view.
                    let c = match (&decl, &system) {
                        (Item::Component(c), _) => c,
                        (_, Some(c)) => c,
                        _ => continue,
                    };
                    let Some(sym) = decl_symbol(table, interner, c.name(), Namespace::Type) else {
                        continue;
                    };
                    scope.components.insert(c.syntax().text_range(), sym);
                    if system.is_some() {
                        decls.systems.insert(sym);
                    }
                    scope.place(decls, sym);
                    decls.types.names.insert(sym, name_of(c.name()));
                    let Some(members) = table.members(sym) else {
                        continue;
                    };
                    let inputs = scope.inputs_of(c, members, interner);
                    decls.inputs.insert(sym, inputs);
                    // The component's own lowering reports what is wrong with its slots.
                    decls
                        .slots
                        .insert(sym, super::component::slots_of(c, &mut Vec::new()));
                    for member in c.members() {
                        scope.record_member(decls, sym, &member, members, interner);
                    }
                }
                Item::Record(r) => {
                    if let Some(sym) = decl_symbol(table, interner, r.name(), Namespace::Type) {
                        let fields = r.fields().filter_map(|f| scope.field_info(&f)).collect();
                        decls.types.records.insert(sym, fields);
                        scope.place(decls, sym);
                        scope.declared.insert(r.syntax().text_range(), sym);
                        decls.types.names.insert(sym, name_of(r.name()));
                    }
                }
                Item::Enum(e) => {
                    if let Some(sym) = decl_symbol(table, interner, e.name(), Namespace::Type) {
                        let variants = e
                            .variants()
                            .filter_map(|v| scope.variant_info(&v))
                            .collect();
                        decls.types.enums.insert(sym, variants);
                        scope.place(decls, sym);
                        scope.declared.insert(e.syntax().text_range(), sym);
                        decls.types.names.insert(sym, name_of(e.name()));
                        let derives = e.derives().into_iter().flat_map(|(_, names)| names);
                        if derives
                            .flatten()
                            .any(|n| n.text() == viso_behavior::game::INPUT_ACTION_DERIVE)
                        {
                            decls.input_derives.insert(sym);
                        }
                    }
                }
                Item::Const(c) => {
                    if let Some(sym) = decl_symbol(table, interner, c.name(), Namespace::Value) {
                        scope.declared.insert(c.syntax().text_range(), sym);
                        let ty = scope.annotation_of(c.syntax());
                        input::collect_map(c, sym, &ty, &scope, decls);
                        decls.facts.insert(
                            sym,
                            MemberFacts {
                                ty,
                                effect: None,
                                is_reactive_source: false,
                                kind: SymbolKind::Const,
                            },
                        );
                    }
                }
                Item::Fn(f) => {
                    let (params, ret) = (f.params(), f.return_type());
                    let sym = decl_symbol(table, interner, f.name(), Namespace::Value).map(|sym| {
                        scope.record_callable(decls, sym, &params, ret, EffectClass::Read)
                    });
                    scope
                        .declared
                        .extend(sym.map(|sym| (f.syntax().text_range(), sym)));
                }
                Item::Action(a) => {
                    let (params, ret) = (a.params(), a.return_type());
                    let sym = decl_symbol(table, interner, a.name(), Namespace::Value).map(|sym| {
                        scope.record_callable(decls, sym, &params, ret, EffectClass::Action)
                    });
                    scope
                        .declared
                        .extend(sym.map(|sym| (a.syntax().text_range(), sym)));
                }
                Item::Task(t) => {
                    let (params, ret) = (t.params(), t.return_type());
                    let sym = decl_symbol(table, interner, t.name(), Namespace::Value).map(|sym| {
                        scope.record_callable(decls, sym, &params, ret, EffectClass::Task)
                    });
                    scope
                        .declared
                        .extend(sym.map(|sym| (t.syntax().text_range(), sym)));
                }
                _ => {}
            }
        }
        scope
    }

    /// Records one member of `owner` (looked up in its member `table`): its
    /// name→symbol entry, its facts, and an event's payload record.
    fn record_member(
        &mut self,
        decls: &mut Declarations,
        owner: SymbolId,
        member: &Member,
        table: &SymbolTable,
        interner: &mut NameInterner,
    ) {
        let (name_tok, namespace, ty, kind) = match member {
            Member::Input(d) => (
                d.name(),
                Namespace::Value,
                self.annotation_of(d.syntax()),
                SymbolKind::Input,
            ),
            Member::State(d) => (
                d.name(),
                Namespace::Value,
                self.annotation_of(d.syntax()),
                SymbolKind::State,
            ),
            Member::Computed(d) => (
                d.name(),
                Namespace::Value,
                self.annotation_of(d.syntax()),
                SymbolKind::Computed,
            ),
            Member::Event(d) => (d.name(), Namespace::Event, Ty::Unknown, SymbolKind::Event),
            Member::Fn(d) => {
                let (params, ret) = (d.params(), d.return_type());
                let sym = decl_symbol(table, interner, d.name(), Namespace::Value)
                    .map(|sym| self.record_callable(decls, sym, &params, ret, EffectClass::Read));
                if let Some(sym) = sym {
                    self.own(owner, name_of(d.name()), sym);
                }
                return;
            }
            Member::Action(d) => {
                let (params, ret) = (d.params(), d.return_type());
                let sym = decl_symbol(table, interner, d.name(), Namespace::Value)
                    .map(|sym| self.record_callable(decls, sym, &params, ret, EffectClass::Action));
                if let Some(sym) = sym {
                    self.own(owner, name_of(d.name()), sym);
                }
                return;
            }
            Member::Task(d) => {
                let (params, ret) = (d.params(), d.return_type());
                let sym = decl_symbol(table, interner, d.name(), Namespace::Value)
                    .map(|sym| self.record_callable(decls, sym, &params, ret, EffectClass::Task));
                if let Some(sym) = sym {
                    self.own(owner, name_of(d.name()), sym);
                }
                return;
            }
            Member::Slot(_) | Member::View(_) => return,
        };

        let Some(tok) = name_tok else {
            return;
        };
        let text = tok.text().to_string();
        let name = interner.intern(&text);
        let Some(sym) = table.get(name, namespace).map(|s| s.id) else {
            return;
        };
        if let Member::Event(d) = member {
            let fields = self.event_fields(d.syntax());
            decls.types.records.insert(sym, fields);
            self.place(decls, sym);
            decls.types.names.insert(sym, text.clone());
            decls.events.entry(owner).or_default().push(EventInfo {
                name: text.clone(),
                symbol: sym,
                declared_at: tok.text_range(),
            });
        }
        self.own(owner, text, sym);
        decls.facts.insert(
            sym,
            MemberFacts {
                ty,
                effect: None,
                is_reactive_source: kind != SymbolKind::Event,
                kind,
            },
        );
    }

    /// Records `name` as the member `sym` of `owner`.
    fn own(&mut self, owner: SymbolId, name: String, sym: SymbolId) {
        self.members.entry(owner).or_default().insert(name, sym);
    }

    /// An event's parameters, as the fields of its payload record.
    fn event_fields(&self, decl: &SyntaxNode) -> Vec<FieldInfo> {
        decl.children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::EventParam)
            .filter_map(|param| {
                let name = param
                    .children_with_tokens()
                    .into_iter()
                    .filter_map(|e| e.as_token().cloned())
                    .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))?;
                Some(FieldInfo {
                    name: name.text().to_string(),
                    ty: self.annotation_of(&param),
                    has_default: false,
                    declared_at: name.text_range(),
                })
            })
            .collect()
    }

    /// Records a `fn`/`action`/`task`: its effect class and signature (a `fn` is also a
    /// value of its function type). A task call yields the task's declared result;
    /// `await` marks where it suspends. Returns its symbol.
    fn record_callable(
        &self,
        decls: &mut Declarations,
        sym: SymbolId,
        params: &[Param],
        ret: Option<ReturnType>,
        effect: EffectClass,
    ) -> SymbolId {
        let params: Vec<Ty> = params
            .iter()
            .map(|p| self.annotation_of(p.syntax()))
            .collect();
        let ret = ret.map_or(Ty::Unit, |r| self.annotation_of(r.syntax()));
        let ty = match effect {
            EffectClass::Read => Ty::Fn(params.clone(), Box::new(ret.clone())),
            _ => Ty::Unknown,
        };
        decls.signatures.insert(sym, (params, ret));
        decls.facts.insert(
            sym,
            MemberFacts {
                ty,
                effect: Some(effect),
                is_reactive_source: false,
                kind: match effect {
                    EffectClass::Action => SymbolKind::Action,
                    EffectClass::Task => SymbolKind::Task,
                    EffectClass::Pure | EffectClass::Read => SymbolKind::Function,
                },
            },
        );
        sym
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
        Ty::from_annotation(ty, &|at| self.nominal.get(&at).cloned()).unwrap_or(Ty::Unknown)
    }
}

/// Whether `node` is a type annotation node.
fn is_type_node(node: &SyntaxNode) -> bool {
    matches!(node.kind(), SyntaxKind::TypePath | SyntaxKind::TupleType)
}

/// Whether an attribute is `@bindable(..)`.
/// Checks each `@bindable` in a component body (`E3701` otherwise): it marks an
/// `input` and names, as its one argument, an event of the same component whose first
/// parameter has the input's type — the event a `bind` writes back through.
fn check_bindable(decl: &ComponentDecl, env: &ModuleEnv<'_>, diagnostics: &mut Vec<Diagnostic>) {
    let members = decl.syntax().children();
    let mut events: HashMap<String, &SyntaxNode> = HashMap::new();
    for member in &members {
        if member.kind() == SyntaxKind::EventDecl
            && let Some(name) = support_name(member)
        {
            events.insert(name, member);
        }
    }
    let mut pending: Vec<&SyntaxNode> = Vec::new();
    for member in &members {
        if member.kind() == SyntaxKind::Attribute {
            if is_bindable(member) {
                pending.push(member);
            }
            continue;
        }
        for attr in pending.drain(..) {
            if let Err(message) = bindable_pairing(attr, member, &events, env) {
                diagnostics.push(Diagnostic::error("E3701", attr.text_range(), message));
            }
        }
    }
    for attr in pending {
        diagnostics.push(Diagnostic::error(
            "E3701",
            attr.text_range(),
            "`@bindable` marks an `input`",
        ));
    }
}

/// Why `attr` (a `@bindable`) does not pair `member` with an event, if it does not.
fn bindable_pairing(
    attr: &SyntaxNode,
    member: &SyntaxNode,
    events: &HashMap<String, &SyntaxNode>,
    env: &ModuleEnv<'_>,
) -> Result<(), String> {
    if member.kind() != SyntaxKind::InputDecl {
        return Err("`@bindable` marks an `input`".to_string());
    }
    let input = support_name(member).unwrap_or_default();
    let usage = || {
        format!(
            "`@bindable` names the event that writes `{input}` back: `@bindable(changed)` with `event changed(value: T)`"
        )
    };
    let Some(event) = bindable_event(attr) else {
        return Err(usage());
    };
    let Some(decl) = events.get(&event) else {
        return Err(format!(
            "`@bindable` names `{event}`, but this component declares no event `{event}`"
        ));
    };
    let Some(param) = decl
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::EventParam)
    else {
        return Err(format!(
            "event `{event}` carries no value to write `{input}` back with; give it a first parameter"
        ));
    };
    let want = env.annotation_of(member);
    let have = env.annotation_of(&param);
    if !want.has_unknown() && !have.has_unknown() && want != have {
        let cx = InferCx::new(&[], env);
        return Err(format!(
            "event `{event}` writes `{input}` back with its first parameter, of type `{}`, but `{input}` is `{}`",
            cx.describe(&have),
            cx.describe(&want),
        ));
    }
    Ok(())
}

/// The name a declaration node declares, without a raw-identifier prefix.
fn support_name(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
        .map(|t| t.text().trim_start_matches("r#").to_string())
}

/// The event a `@bindable(event)` attribute names: its single unlabeled
/// one-segment argument.
fn bindable_event(attr: &SyntaxNode) -> Option<String> {
    let list = attr
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::ArgumentList)?;
    let args: Vec<SyntaxNode> = list
        .children()
        .into_iter()
        .filter(|c| c.kind() == SyntaxKind::Argument)
        .collect();
    let [arg] = args.as_slice() else {
        return None;
    };
    let labeled = arg
        .children_with_tokens()
        .into_iter()
        .any(|e| e.as_token().is_some_and(|t| t.kind() == SyntaxKind::Colon));
    match arg.children().as_slice() {
        [path] if !labeled && path.kind() == SyntaxKind::PathExpr => {
            crate::ast::PathExpr::cast(path.clone())
                .map(|p| p.segments().collect::<Vec<_>>())
                .and_then(|segments| match segments.as_slice() {
                    [name] => Some(name.text().trim_start_matches("r#").to_string()),
                    _ => None,
                })
        }
        _ => None,
    }
}

/// Each two-way input of `decl` and the event that writes it back: an input
/// preceded by a `@bindable(event)` attribute, by its index among the inputs.
pub(crate) fn write_backs(decl: &ComponentDecl) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut inputs = 0;
    let mut event = None;
    for child in decl.syntax().children() {
        match child.kind() {
            SyntaxKind::Attribute if is_bindable(&child) => event = bindable_event(&child),
            SyntaxKind::Attribute => {}
            SyntaxKind::InputDecl => {
                if let Some(event) = event.take() {
                    out.push((inputs, event));
                }
                inputs += 1;
            }
            _ => event = None,
        }
    }
    out
}

fn is_bindable(attr: &SyntaxNode) -> bool {
    attribute_is(attr, "bindable")
}

/// Whether an attribute's path is the one-segment `name`.
fn attribute_is(attr: &SyntaxNode, name: &str) -> bool {
    attr.children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::PathExpr)
        .is_some_and(|p| p.text().to_string().trim() == name)
}

/// Checks each `@migrate` among the children of `parent` (a module or a component
/// body; `E3712` otherwise) and records each valid one in `migrators`, by the range
/// of its attribute: it marks a `fn`, names the old type as its one argument
/// `from: "T"`, and the `fn` takes one parameter of type `T` and returns the new
/// type. At most one `fn` migrates a pair of types. `symbol` is the symbol of a
/// `fn` declaration node.
fn check_migrators(
    parent: &SyntaxNode,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    migrators: &mut Vec<(TextRange, Migrator)>,
    symbol: &dyn Fn(&SyntaxNode) -> Option<SymbolId>,
) {
    let mut pending: Vec<SyntaxNode> = Vec::new();
    for child in parent.children() {
        if child.kind() == SyntaxKind::Attribute {
            if attribute_is(&child, "migrate") {
                pending.push(child);
            }
            continue;
        }
        let target = match child.kind() {
            SyntaxKind::ExportDecl => child
                .children()
                .into_iter()
                .find(|c| !matches!(c.kind(), SyntaxKind::Attribute)),
            _ => Some(child.clone()),
        };
        for attr in pending.drain(..) {
            let at = attr.text_range();
            let migrator = target
                .as_ref()
                .ok_or_else(|| "`@migrate` marks a `fn`".to_string())
                .and_then(|target| migrator(&attr, target, env, symbol));
            let migrator = match migrator {
                Ok(migrator) => migrator,
                Err(message) => {
                    diagnostics.push(Diagnostic::error("E3712", at, message));
                    continue;
                }
            };
            if let Some((first, _)) = migrators
                .iter()
                .find(|(_, m)| m.from == migrator.from && m.ret == migrator.ret)
            {
                let cx = InferCx::new(&[], env);
                let mut diagnostic = Diagnostic::error(
                    "E3712",
                    at,
                    format!(
                        "a second `@migrate` function from `{}` to `{}`",
                        migrator.from,
                        cx.describe(&migrator.ret)
                    ),
                );
                if parent.text_range().contains_range(*first) {
                    diagnostic
                        .related
                        .push(Related::new(*first, "the first is marked here"));
                }
                diagnostics.push(diagnostic);
                continue;
            }
            migrators.push((at, migrator));
        }
    }
    for attr in pending {
        diagnostics.push(Diagnostic::error(
            "E3712",
            attr.text_range(),
            "`@migrate` marks a `fn`",
        ));
    }
}

/// The migration `attr` (a `@migrate`) makes of `target`, or why it makes none.
fn migrator(
    attr: &SyntaxNode,
    target: &SyntaxNode,
    env: &ModuleEnv<'_>,
    symbol: &dyn Fn(&SyntaxNode) -> Option<SymbolId>,
) -> Result<Migrator, String> {
    if target.kind() != SyntaxKind::FnDecl {
        return Err("`@migrate` marks a `fn`".to_string());
    }
    let Some(from) = migrate_from(attr) else {
        return Err(
            "`@migrate` names the state type it migrates from: `@migrate(from: \"I64\")`"
                .to_string(),
        );
    };
    let Some(symbol) = symbol(target) else {
        return Err("internal: the `@migrate` function has no symbol".to_string());
    };
    let Some((params, ret)) = env.decls.signatures.get(&symbol) else {
        return Err("internal: the `@migrate` function has no signature".to_string());
    };
    let name = support_name(target).unwrap_or_default();
    let [param] = params.as_slice() else {
        return Err(format!(
            "`{name}` migrates a `{from}`, so it takes that value as its one parameter"
        ));
    };
    let has_ret = target
        .children()
        .into_iter()
        .any(|c| c.kind() == SyntaxKind::ReturnType);
    if !has_ret {
        return Err(format!("`{name}` returns the state's new type; declare it"));
    }
    let cx = InferCx::new(&[], env);
    let spelled = cx.describe(param);
    if !param.has_unknown() && spelled != from {
        return Err(format!(
            "`{name}` migrates a `{from}`, but its parameter is `{spelled}`"
        ));
    }
    Ok(Migrator {
        from,
        param: param.clone(),
        ret: ret.clone(),
        symbol,
    })
}

/// The type a `@migrate(from: "T")` attribute names: its single argument,
/// labeled `from`, a plain string literal.
fn migrate_from(attr: &SyntaxNode) -> Option<String> {
    let list = attr
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::ArgumentList)?;
    let args: Vec<SyntaxNode> = list
        .children()
        .into_iter()
        .filter(|c| c.kind() == SyntaxKind::Argument)
        .collect();
    let [arg] = args.as_slice() else {
        return None;
    };
    let label = arg
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| !t.kind().is_trivia())?;
    if label.text() != "from" {
        return None;
    }
    let values = arg.children();
    let [value] = values.as_slice() else {
        return None;
    };
    if value.kind() != SyntaxKind::LiteralExpr {
        return None;
    }
    let token = value
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| !t.kind().is_trivia())?;
    if token.kind() != SyntaxKind::StringLiteral {
        return None;
    }
    let text = token.text();
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    let from = super::infer::pattern::unescape(body)?;
    (!from.is_empty() && !from.contains('{')).then_some(from)
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
    use crate::diag::Applicability;
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
        lower(
            &graph,
            &units,
            &resolved,
            &mut interner,
            "app",
            InputDevices::default(),
        )
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

    /// Lowers `app` as a package with a `lib` module holding `lib`, returning the
    /// diagnostic codes each module raised.
    /// The diagnostics of `lib` and `app` lowered as one package, by module.
    fn lower_pair(lib: &str, app: &str) -> (Vec<Diagnostic>, Vec<Diagnostic>) {
        let mut interner = NameInterner::new();
        let mut unit = |path: &str, src: &str| {
            let tokens = crate::syntax::tokenize(src);
            let parse = crate::syntax::grammar::parse(&tokens, src);
            SourceUnit::new(
                crate::resolve::ModulePath::intern(&mut interner, &[path]),
                parse,
            )
        };
        let units = vec![unit("lib", lib), unit("app", app)];
        let graph = ModuleGraph::build(&units, &interner);
        let resolved = resolve(&graph, &units, &mut interner, "app");
        for module in &resolved {
            assert!(module.errors.is_empty(), "{:?}", module.errors);
        }
        let pkg = lower(
            &graph,
            &units,
            &resolved,
            &mut interner,
            "app",
            InputDevices::default(),
        );
        let mut lib_diagnostics = Vec::new();
        let mut app_diagnostics = Vec::new();
        for (module, range) in graph.modules().iter().zip(&pkg.module_diagnostics) {
            let diagnostics = pkg.diagnostics[range.clone()].to_vec();
            match module.path.display(&interner).as_str() {
                "lib" => lib_diagnostics = diagnostics,
                _ => app_diagnostics = diagnostics,
            }
        }
        (lib_diagnostics, app_diagnostics)
    }

    fn lower_with_lib(lib: &str, app: &str) -> (Vec<&'static str>, Vec<&'static str>) {
        let (lib, app) = lower_pair(lib, app);
        let codes = |ds: Vec<Diagnostic>| ds.iter().map(|d| d.code).collect();
        (codes(lib), codes(app))
    }

    #[test]
    fn imported_records_and_enums_type_as_declared() {
        let lib = "export record P { x: I64; y: I64 = 0; }\n\
                   export enum Shape { Dot; Circle(I64); }";
        let app = |body: &str| lower_with_lib(lib, &format!("import lib::{{ P, Shape }};\n{body}"));
        let clean = (vec![], vec![]);
        assert_eq!(app("fn f(p: P) -> I64 { p.x + p.y }"), clean);
        assert_eq!(app("fn g() -> P { P { x: 1 } }"), clean);
        assert_eq!(
            app("fn h(s: Shape) -> I64 { match s { Shape::Dot => 0, Shape::Circle(r) => r } }"),
            clean
        );
        assert_eq!(app("fn f(p: P) -> String { p.x }").1, ["E2103"]);
        assert_eq!(app("fn f(p: P) -> I64 { p.z }").1, ["E2001"]);
        assert_eq!(app("fn g() -> P { P { y: 1 } }").1, ["E2103"]);
        assert_eq!(
            app("fn h(s: Shape) -> I64 { match s { Shape::Dot => 0 } }").1,
            ["E2301"]
        );
    }

    #[test]
    fn imported_callables_check_their_calls() {
        let lib = "export fn twice(x: I64) -> I64 { x * 2 }\n\
                   export action save() { }";
        let app =
            |body: &str| lower_with_lib(lib, &format!("import lib::{{ twice, save }};\n{body}"));
        assert_eq!(app("const C: I64 = twice(1);"), (vec![], vec![]));
        assert_eq!(app("const C: String = twice(1);").1, ["E2103"]);
        assert_eq!(app("fn f() { save(); }").1, ["E2501"]);
    }

    #[test]
    fn imported_components_check_their_properties_and_events() {
        let lib = "export component Card {\n\
                   \x20 input title: String;\n\
                   \x20 event picked(index: I64);\n\
                   \x20 view { }\n\
                   }";
        let app = |node: &str| {
            lower_with_lib(
                lib,
                &format!(
                    "import lib::{{ Card }};\ncomponent App {{\n  state n = 0;\n  view {{ Card {{ {node} }} }}\n}}"
                ),
            )
        };
        assert_eq!(
            app("title: \"a\"; on picked(ev) { n = ev.index; }"),
            (vec![], vec![])
        );
        assert_eq!(app("title: 1;").1, ["E2103"]);
        assert_eq!(
            app("on picked(ev) { let s: String = ev.index; }").1,
            ["E2103"]
        );
        assert_eq!(app("on pick { }").1, ["E3202"]);
    }

    #[test]
    fn a_suggestion_declared_in_another_module_names_it() {
        let lib = "export component Card {\n\
                   \x20 input title: String;\n\
                   \x20 event picked(index: I64);\n\
                   \x20 view { }\n\
                   }";
        let app = |node: &str| {
            let src = format!(
                "import lib::{{ Card }};\ncomponent App {{\n  view {{ Card {{ {node} }} }}\n}}"
            );
            let (_, app) = lower_pair(lib, &src);
            assert_eq!(app.len(), 1, "{app:?}");
            let related = app[0].related[0].clone();
            let at = related.range.start().to_u32() as usize;
            (
                related.module,
                &lib[at..related.range.end().to_u32() as usize],
            )
        };
        assert_eq!(app("on pickd { }"), (Some("lib".to_string()), "picked"));
        assert_eq!(app("titl: \"a\";"), (Some("lib".to_string()), "title"));
    }

    #[test]
    fn an_imported_input_keeps_its_percent_basis() {
        let lib = "export component Card {\n\
                   \x20 input size: MixedLength = 0dp\n\
                   \x20 input shift: MixedLength = 0dp\n\
                   \x20 view { Column { width: size + 4dp; translate: Offset { x: shift, y: 0dp }; } }\n\
                   }\n\
                   export component Frame {\n\
                   \x20 input inset: MixedLength = 0dp\n\
                   \x20 view { Card { shift: inset; } }\n\
                   }";
        let app = |node: &str| {
            lower_with_lib(
                lib,
                &format!(
                    "import lib::{{ Card, Frame }};\ncomponent App {{\n  view {{ {node} }}\n}}"
                ),
            )
        };
        assert_eq!(app("Card { size: 50%; shift: 4dp; }"), (vec![], vec![]));
        assert_eq!(app("Card { shift: 50%; }"), (vec![], vec!["E3104"]));
        assert_eq!(app("Frame { inset: 10%; }"), (vec![], vec!["E3104"]));
    }

    /// The diagnostic codes lowering `src` reports, in order.
    #[test]
    fn optional_chaining_reads_through_an_option() {
        let decls = "record P { x: I64; tag: Option<String>; }\n";
        assert_clean(&format!(
            "{decls}fn f(p: Option<P>) -> Option<I64> {{ p?.x }}\n\
             fn g(p: Option<P>) -> Option<String> {{ p?.tag }}"
        ));
        assert_eq!(
            codes(&format!("{decls}fn f(p: Option<P>) -> I64 {{ p?.x }}")),
            ["E2103"]
        );
        let pkg = lower_src(&format!("{decls}fn f(p: P) -> I64 {{ p?.x }}"));
        let [diagnostic] = pkg.diagnostics.as_slice() else {
            panic!("expected one diagnostic, got {:?}", pkg.diagnostics);
        };
        assert_eq!(diagnostic.code, "E2103");
        let [fix] = diagnostic.fixes.as_slice() else {
            panic!("expected one fix");
        };
        assert_eq!(fix.applicability, Applicability::MachineApplicable);
        assert_eq!(fix.edits[0].replacement, ".");
        assert_eq!(fix.edits[0].range, diagnostic.primary);
        assert_eq!(
            codes(&format!("{decls}fn f(p: P) -> Option<I64> {{ p?.x }}")),
            ["E2103", "E2103"]
        );
        assert_clean("fn f(s: Option<String>) { s?.len(); }");
        assert_eq!(codes("fn f(s: String) { s?.len(); }"), ["E2103"]);
    }

    #[test]
    fn only_actions_and_handlers_mutate() {
        let c = |members: &str| {
            codes(&format!(
                "component C {{ state n: I64 = 0; event e(); action a() {{}} {members} view {{ Text {{}} }} }}"
            ))
        };
        assert!(c("action b() { n = 1; n += 1; emit e(); a(); }").is_empty());
        assert!(c("fn f() { let mut m = n; m = 2; }").is_empty());
        assert_eq!(c("fn f() { n = 1; }"), ["E2501"]);
        assert_eq!(c("fn f() { n += 1; }"), ["E2501"]);
        assert_eq!(c("fn f() { emit e(); }"), ["E2501"]);
        assert_eq!(c("task t() -> I64 { n = 1; return 1; }"), ["E2501"]);
        assert_eq!(c("task t() -> I64 { emit e(); return 1; }"), ["E2501"]);
        assert_eq!(c("computed m: I64 = { n = 2; n };"), ["E2502"]);
        assert_eq!(c("computed m: I64 = { a(); n };"), ["E2502"]);
        assert_eq!(c("state s: I64 = { a(); 1 };"), ["E2501"]);
        assert_eq!(
            codes("action a() {}\nconst K: I64 = { a(); 1 };"),
            ["E2501"]
        );
        assert_eq!(
            codes("action a() {}\nrecord P { x: I64 = { a(); 1 }; }"),
            ["E2501"]
        );
    }

    #[test]
    fn a_view_handler_is_an_event_body() {
        assert_clean(
            "component C { state n: I64 = 0; event e(); action a() {}\n\
             view { Button { on click { a(); n += 1; emit e(); } } } }",
        );
        assert_eq!(
            codes("component C { action a() {} view { Text { text: a(); } } }"),
            ["E2103", "E2502"]
        );
    }

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
        assert_eq!(codes("action save() { }\nfn f() { save(); }"), ["E2501"]);
        assert_clean(
            "task fetch(id: I64) -> String { return \"\"; }\n\
             task show(id: I64) -> String { return await fetch(id); }",
        );
        assert_eq!(
            codes(
                "task fetch(id: I64) -> String { return \"\"; }\n\
                 task count() -> I64 { return await fetch(1); }"
            ),
            ["E2103"]
        );
        assert_eq!(
            codes(
                "task fetch(id: I64) -> String { return \"\"; }\n\
                 task show() -> String { return await fetch(\"a\"); }"
            ),
            ["E2103"]
        );
        assert_eq!(codes("task t() -> I64 { return \"a\"; }"), ["E2103"]);
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
                || d.related.iter().any(|r| r.label.contains("opacity")),
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
                "component Card {\n  @default slot body: SlotList<Node>;\n  view { }\n}\n\
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
                || d.related.iter().any(|r| r.label.contains("row")),
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
        let reason = &pkg.diagnostics[0].related[0].label;
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
        let reason = &pkg.diagnostics[0].related[0].label;
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
        let reason = &pkg.diagnostics[0].related[0].label;
        assert!(reason.contains("translate"), "{reason}");
    }

    #[test]
    fn assignments_write_only_state_and_mutable_locals() {
        assert_clean(
            "record P { x: I64 }\n\
             component C {\n\
             \x20 state n = 0\n\
             \x20 state p = P { x: 1 }\n\
             \x20 state xs: List<I64> = []\n\
             \x20 action a(mut k: I64) {\n\
             \x20   let mut m = 1;\n\
             \x20   m += 1;\n\
             \x20   k = m;\n\
             \x20   n = k;\n\
             \x20   p.x = 2;\n\
             \x20   xs[0] = 3;\n\
             \x20   let f = |mut q: I64| { q = 1; q };\n\
             \x20   let (mut r, s) = (1, 2);\n\
             \x20   r = s + f(0);\n\
             \x20 }\n\
             \x20 view { }\n\
             }",
        );
        let body = |decls: &str, stmt: &str| {
            codes(&format!(
                "const K: I64 = 1;\ncomponent C {{\n  input i: I64\n  state n = 0\n  computed c: I64 = n + 1\n{decls}  action a(k: I64) {{ {stmt} }}\n  view {{ }}\n}}"
            ))
        };
        assert_eq!(body("", "i = 1;"), ["E2110"]);
        assert_eq!(body("", "c = 1;"), ["E2110"]);
        assert_eq!(body("", "K = 1;"), ["E2110"]);
        assert_eq!(body("", "k = 1;"), ["E2110"]);
        assert_eq!(body("", "let m = 1; m += 1;"), ["E2110"]);
        assert_eq!(body("", "let (m, j) = (1, 2); m = j;"), ["E2110"]);
        assert_eq!(body("", "(n) = 1;"), ["E2110"]);
        assert_eq!(body("", "n = 1;"), Vec::<&str>::new());
    }

    #[test]
    fn bind_sources_are_state_lenses_of_the_property_type() {
        let stepper = "record P { x: I64, label: String }\n\
                       component Stepper {\n\
                       \x20 @bindable(changed)\n\
                       \x20 input value: I64\n\
                       \x20 event changed(value: I64)\n\
                       \x20 view { }\n\
                       }\n";
        let app = |members: &str, bind: &str| {
            codes(&format!(
                "{stepper}component App {{\n  input i: I64\n  state n = 0\n  state p = P {{ x: 1, label: \"a\" }}\n  state xs: List<I64> = []\n  computed c: I64 = n + 1\n{members}  view {{ {bind} }}\n}}"
            ))
        };
        let none = Vec::<&str>::new();
        assert_eq!(app("", "Stepper { bind value <=> n; }"), none);
        assert_eq!(app("", "Stepper { bind value <=> p.x; }"), none);
        assert_eq!(app("", "Stepper { bind value <=> xs[0]; }"), none);
        assert_eq!(app("", "Stepper { bind value <=> i; }"), ["E3107"]);
        assert_eq!(app("", "Stepper { bind value <=> c; }"), ["E3107"]);
        assert_eq!(
            app("", "for k in xs { Stepper { bind value <=> k; } }"),
            ["E3107"]
        );
        assert_eq!(app("", "Stepper { bind value <=> p.label; }"), ["E2103"]);
        assert_eq!(
            app("", "Stepper { bind value <=> p.label using Parse; }"),
            none
        );
        assert_eq!(app("", "Stepper { bind value <=> xs[\"a\"]; }"), ["E2103"]);
    }

    #[test]
    fn bindable_names_an_event_writing_the_input_back() {
        let stepper = |attr: &str, member: &str, event: &str| {
            codes(&format!(
                "component Stepper {{\n  {attr}\n  {member}\n  {event}\n  view {{ }}\n}}"
            ))
        };
        let none = Vec::<&str>::new();
        let input = "input value: I64";
        let changed = "event changed(value: I64)";
        assert_eq!(stepper("@bindable(changed)", input, changed), none);
        assert_eq!(stepper("@bindable(moved)", input, changed), ["E3701"]);
        assert_eq!(stepper("@bindable", input, changed), ["E3701"]);
        assert_eq!(
            stepper("@bindable(changed, changed)", input, changed),
            ["E3701"]
        );
        assert_eq!(
            stepper("@bindable(changed)", input, "event changed(value: String)"),
            ["E3701"]
        );
        assert_eq!(
            stepper("@bindable(changed)", input, "event changed()"),
            ["E3701"]
        );
        assert_eq!(
            stepper("@bindable(changed)", "state value: I64 = 0", changed),
            ["E3701"]
        );
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

    #[test]
    fn members_of_different_components_do_not_collide() {
        assert_clean(
            "const n = \"module\";\n\
             component A {\n  state n = 0\n  event changed(v: I64);\n  computed m: I64 = n\n  view { }\n}\n\
             component B {\n  state n = \"b\"\n  event changed(v: String);\n  computed m: String = n\n  view { }\n}",
        );
    }

    #[test]
    fn handlers_type_their_payload_against_the_event() {
        let stepper =
            "component Stepper {\n  event changed(value: I64, source: String);\n  view { }\n}\n";
        let app = |state: &str, handler: &str| {
            format!(
                "{stepper}component App {{\n  {state}\n  view {{ Stepper {{ {handler} }} }}\n}}"
            )
        };
        assert_clean(&app("state n = 0", "on changed(ev) { n = ev.value; }"));
        assert_clean(&app("state n = 0", "on click { n = 1; }"));
        assert_clean(&app("state n = 0", "on changed { n = 1; }"));
        assert_eq!(
            codes(&app("state s = \"\"", "on changed(ev) { s = ev.value; }")),
            ["E2103"]
        );
        let pkg = lower_src(&app("state n = 0", "on chnged(ev) { n = 1; }"));
        let codes: Vec<_> = pkg.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["E3202"]);
        assert_eq!(pkg.diagnostics[0].fixes[0].edits[0].replacement, "changed");
        // A built-in widget takes its own events and the standard ones.
        let pkg = lower_src("component App {\n  view { Button { on clik { } } }\n}");
        let codes: Vec<_> = pkg.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["E3202"]);
        assert_eq!(pkg.diagnostics[0].fixes[0].edits[0].replacement, "click");
    }

    #[test]
    fn standard_and_widget_events_type_their_payload() {
        let app = |state: &str, node: &str| {
            format!("component App {{\n  {state}\n  view {{ {node} }}\n}}")
        };
        assert_clean(&app(
            "state n: Dp = 0dp",
            "Row { on pointer_down(e) { n = e.position.x; } }",
        ));
        assert_eq!(
            codes(&app(
                "state s = \"\"",
                "Row { on pointer_down(e) { s = e.position.x; } }"
            )),
            ["E2103"]
        );
        assert_clean(&app(
            "state p: Option<Point> = Option::None",
            "Button { on click(e) { p = e.position; } }",
        ));
        assert_eq!(
            codes(&app(
                "state p = Point { x: 0dp, y: 0dp }",
                "Button { on click(e) { p = e.position; } }"
            )),
            ["E2103"]
        );
        assert_clean(&app(
            "state b = PointerButton::primary\n  state k: Option<Key> = Option::None",
            "Column { on pointer_up(e) { b = e.button; } on key_down(e) { k = Option::Some(e.key); } }",
        ));
        assert_clean(&app(
            "state f: F32 = 0.0",
            "Slider { on changed(ev) { f = ev.value; } }",
        ));
        assert_eq!(
            codes(&app(
                "state s = \"\"",
                "Slider { on changed(ev) { s = ev.value; } }"
            )),
            ["E2103"]
        );
        assert_clean(&app(
            "state t = \"\"\n  state done = false",
            "TextInput { on changed(ev) { t = ev.value; } on submitted { done = true; } }",
        ));
        assert_clean(&app(
            "state ok = false",
            "Stack { on animation_end(e) { ok = e.finished; } }",
        ));
        let pkg = lower_src(&app("", "Slider { on chaned(e) { } }"));
        let found: Vec<_> = pkg.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(found, ["E3202"]);
        assert_eq!(pkg.diagnostics[0].fixes[0].edits[0].replacement, "changed");
        assert_eq!(codes(&app("", "Row { on changed(e) { } }")), ["E3202"]);
        assert_eq!(codes(&app("", "FocusScope { on click { } }")), ["E3202"]);
        assert_clean(&app(
            "state n = 0",
            "KeyShortcut { on triggered { n += 1; } }",
        ));
    }

    #[test]
    fn prelude_types_are_in_scope_and_shadowed_by_declarations() {
        assert_clean(
            "component C {\n  state o: Offset = Offset { x: 4dp }\n  state k = KeyChord { key: Key::char('s'), primary: true }\n  view { Column { translate: Offset::zero(); } }\n}",
        );
        assert_clean(
            "record Point { a: I64 }\ncomponent C {\n  state p = Point { a: 1 }\n  view { }\n}",
        );
    }

    #[test]
    fn emit_arguments_are_checked_against_the_event() {
        let src = |body: &str| {
            format!(
                "component C {{\n  event changed(value: I64, source: String);\n  action go() {{ {body} }}\n  view {{ Button {{ on click {{ {body} }} }} }}\n}}"
            )
        };
        assert_clean(&src("emit changed(1, \"a\");"));
        assert_clean(&src("emit changed(source: \"a\", value: 1);"));
        assert_clean(&src("emit changed(1, source: \"a\");"));
        let one = |body: &str| {
            let codes = codes(&src(body));
            assert_eq!(codes.len(), 2, "{body}: {codes:?}");
            assert_eq!(codes[0], codes[1]);
            codes[0]
        };
        assert_eq!(one("emit chnged(1, \"a\");"), "E3202");
        assert_eq!(one("emit changed(1);"), "E3202");
        assert_eq!(one("emit changed(1, \"a\", 2);"), "E3202");
        assert_eq!(one("emit changed(1, value: 2, source: \"a\");"), "E3202");
        assert_eq!(one("emit changed(1, sourc: \"a\");"), "E3202");
        assert_eq!(one("emit changed(\"a\", \"b\");"), "E2103");
    }

    const PANEL: &str = "component Panel {\n\
         \x20 slot header: Slot<Node>;\n\
         \x20 slot footer: OptionalSlot<Node>;\n\
         \x20 @default slot body: SlotList<Node>;\n\
         \x20 view { Column { SlotOutlet { slot: header; } SlotOutlet { slot: body; } SlotOutlet { slot: footer; } } }\n\
         }\n";

    fn with_panel(view: &str) -> Vec<&'static str> {
        codes(&format!(
            "{PANEL}component C {{\n  state on = true;\n  state items = [1, 2];\n  view {{ {view} }}\n}}"
        ))
    }

    #[test]
    fn slots_are_filled_by_cardinality() {
        assert_clean(&format!(
            "{PANEL}component C {{\n  state on = true;\n  view {{ Panel {{ \
             fill header {{ if on {{ Text {{}} }} else {{ Row {{}} }} }} \
             Text {{}} Text {{}} }} }}\n}}"
        ));
        assert_clean(&format!(
            "{PANEL}component C {{\n  view {{ Panel {{ fill header {{ Fragment {{ Text {{}} }} }} \
             fill footer {{ Text {{}} }} fill body {{ Text {{}} }} }} }}\n}}"
        ));
        assert_clean("component C {\n  view { Scroll { fill content { Column {} } } }\n}");
        assert_eq!(
            with_panel("Panel { Text {} }"),
            ["E3502"],
            "header unfilled"
        );
        assert_eq!(
            with_panel("Panel { fill header { if on { Text {} } } }"),
            ["E3502"],
            "an `if` without `else` may fill nothing"
        );
        assert_eq!(
            with_panel("Panel { fill header { for i in items key i { Text {} } } }"),
            ["E3502"]
        );
        assert_eq!(
            with_panel("Panel { fill header { Text {} } fill footer { Text {} Text {} } }"),
            ["E3502"]
        );
        assert_eq!(
            with_panel("Panel { fill header { Text {} } fill header { Text {} } }"),
            ["E3502"],
            "a single slot is filled once"
        );
        assert_eq!(
            with_panel("Panel { fill header { Text {} } Text {} fill body { Text {} } }"),
            ["E3502"],
            "bare items and `fill` of the default slot"
        );
        assert_eq!(
            codes("component C {\n  view { Scroll { Column {} Column {} } }\n}"),
            ["E3502"]
        );
    }

    #[test]
    fn a_node_without_a_default_slot_takes_no_bare_items() {
        let pkg = lower_src("component C {\n  view { Column { Text { Row {} } } }\n}");
        assert_eq!(
            pkg.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
            ["E3003"]
        );
        assert_eq!(
            codes(
                "component Card {\n  slot title: Slot<Node>;\n  view { }\n}\n\
                 component C {\n  view { Card { Text {} } }\n}"
            ),
            ["E3003", "E3502"],
            "no default slot, and `title` is left unfilled"
        );
        let pkg = lower_src(&format!(
            "{PANEL}component C {{\n  view {{ Panel {{ fill heder {{ Text {{}} }} fill header {{ Text {{}} }} }} }}\n}}"
        ));
        let d = pkg
            .diagnostics
            .iter()
            .find(|d| d.code == "E3501")
            .expect("an unknown slot is E3501");
        assert!(
            d.notes.iter().any(|n| n.contains("header"))
                || d.related.iter().any(|r| r.label.contains("header")),
            "the misspelling suggests `header`, got {d:?}"
        );
        assert_eq!(pkg.diagnostics.len(), 1, "{:?}", pkg.diagnostics);
    }

    #[test]
    fn slot_declarations_are_checked() {
        assert_eq!(
            codes(
                "component C {\n  @default slot a: SlotList<Node>;\n  \
                 @default slot b: SlotList<Node>;\n  view { Column {} }\n}"
            ),
            ["E3004"]
        );
        assert_eq!(
            codes("component C {\n  slot a: Slot<String>;\n  view { Column {} }\n}"),
            ["E2103"]
        );
        assert_eq!(
            codes("component C {\n  slot a: Slot<Node> = empty;\n  view { Column {} }\n}"),
            ["E3502"]
        );
        assert_clean(
            "component C {\n  slot a: OptionalSlot<Node> = None;\n  \
             slot b: SlotList<Node> = empty;\n  \
             view { Column { SlotOutlet { slot: a; } SlotOutlet { slot: b; } } }\n}",
        );
    }

    #[test]
    fn an_outlet_places_one_slot_of_its_own_component_once() {
        assert_eq!(
            codes(
                "component C {\n  slot a: SlotList<Node>;\n  \
                 view { Column { SlotOutlet { slot: b; } } }\n}"
            ),
            ["E3501"]
        );
        assert_eq!(
            codes("component C {\n  view { Column { SlotOutlet {} } }\n}"),
            ["E3501"]
        );
        assert_eq!(
            codes(
                "component C {\n  slot a: SlotList<Node>;\n  \
                 view { Column { SlotOutlet { slot: a; } SlotOutlet { slot: a; } } }\n}"
            ),
            ["E3502"]
        );
        assert_eq!(
            codes(
                "component C {\n  slot a: SlotList<Node>;\n  state items = [1];\n  \
                 view { Column { for i in items key i { SlotOutlet { slot: a; } } } }\n}"
            ),
            ["E3502"]
        );
        assert_eq!(
            codes(
                "component Wrap {\n  slot a: Slot<Node>;\n  \
                 view { Column { SlotOutlet { slot: a; } } }\n}\n\
                 component C {\n  slot a: Slot<Node>;\n  \
                 view { Wrap { fill a { SlotOutlet { slot: a; } } } }\n}"
            ),
            Vec::<&str>::new(),
            "an outlet forwards the caller's slot, counted by its cardinality"
        );
    }
}
