//! The game state layers: `@local` state, the Simulation domain a system's
//! Simulation hooks and every callable they reach form, and the checks that
//! keep gameplay independent of presentation and reproducible.
//!
//! Simulation code may not touch `@local` state or use a Presentation value
//! (`E9103`), nor reach a non-deterministic source (`E9104`): a native below
//! the package's determinism tier, a `task` or `await`, or the adaptive
//! environment. Its state must snapshot (`E9105`). A Presentation command it
//! calls is deferred by the scheduler, so it is allowed. A `@persist` state
//! of a system or a component names a key unique in the package, snapshots,
//! and needs the `storage.persist` capability (`E9106`).
//!
//! An `AudioProcess` hook runs on the audio thread: it and every callable it
//! reaches may not allocate, start a task, emit an event, load a resource,
//! call a closure value or a native that is not realtime-safe, recurse, or
//! loop without a static bound (`E9108`), checked over their lowered
//! instructions; an `AudioProcess` system implements no trait whose hooks
//! run off the audio thread.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};

use viso_behavior::game::AudioBlock;
use viso_behavior::native::{
    Determinism, HookDomain, NativeId, NativeKind, NativeObject, SchemaTy,
};

use crate::ast::{AstNode, Expr, Item, Member, decl_attributes};
use crate::behavior::probe::{ProbePayload, ProbeShape, ProbeVariant};
use crate::diag::{Diagnostic, Related};
use crate::hir::CapabilitySet;
use crate::hir::component::MemberEnv;
use crate::hir::infer::{FieldInfo, TypeEnv, VariantPayload};
use crate::resolve::{Resolution, ResolvedRef, SymbolId, SymbolKind};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

use super::{Declarations, HirComponent, InputDevices, Migrator, ModuleEnv, Ty, name_of};
use crate::behavior::ir::{BinaryOp, FuncId, Inst};
use crate::behavior::lower::ProgramBuilder;

/// What the package's targets are and how it is built: what game input and
/// determinism checks hold it to, the fixed step its games run at, whether
/// debug draw is compiled out, and the capabilities it is granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetProfile {
    /// The input devices the targets have.
    pub devices: InputDevices,
    /// The determinism tier the Simulation domain needs (`[game] determinism`).
    pub determinism: Determinism,
    /// The ticks a second of the fixed step (`[game] tick_rate`), at least 1:
    /// tick timers convert their durations with it.
    pub tick_rate: u32,
    /// The order a tick delivers contacts to its `CollisionListener`s
    /// (`[game] collision_delivery`).
    pub collision_delivery: viso_behavior::CollisionDelivery,
    /// A release build, which removes debug draw.
    pub release: bool,
    /// The capabilities the package is granted (`[package] capabilities`).
    pub capabilities: CapabilitySet,
    /// Whether accessibility findings (`E3704`, `E3708`) are errors
    /// (`--a11y strict`) rather than warnings.
    pub a11y_strict: bool,
    /// Whether localization findings (`E3705`) are errors and cover literal
    /// text too (`--i18n strict`).
    pub i18n_strict: bool,
    /// The package's message catalogs (`i18n/`), compiled: what `tr` and a
    /// `MessageKey` literal check against and the module carries.
    pub messages: Option<std::rc::Rc<crate::i18n::Messages>>,
}

impl Default for TargetProfile {
    fn default() -> TargetProfile {
        TargetProfile {
            devices: InputDevices::default(),
            determinism: Determinism::SameBinary,
            tick_rate: viso_behavior::DEFAULT_TICK_RATE,
            collision_delivery: viso_behavior::CollisionDelivery::EventMajor,
            release: false,
            capabilities: CapabilitySet::new(),
            a11y_strict: false,
            i18n_strict: false,
            messages: None,
        }
    }
}

/// A system's hook: the action implementing it, the trait hook's name and
/// its domain.
pub(super) type BoundHook = (SymbolId, String, HookDomain);

/// One callable body: what it mentions, calls natively and reads of the
/// environment, by span.
struct Body {
    module: usize,
    name: String,
    mentions: Vec<(SymbolId, TextRange)>,
    natives: Vec<(NativeId, TextRange)>,
    env: Vec<TextRange>,
    awaits: Vec<TextRange>,
    loops: Vec<Loop>,
}

/// A loop of a body, by its keyword: `None` bounds for a `while`, a `loop`
/// or a `for` over anything but a range written in its head, otherwise what
/// its bounds name.
struct Loop {
    at: TextRange,
    bounds: Option<Vec<Bound>>,
}

/// A name a range bound reads: a static bound reads only `const`s and the
/// size of the audio block.
enum Bound {
    Symbol(SymbolId),
    Native(NativeId),
}

/// The package's callables and Simulation roots, gathered module by module.
#[derive(Default)]
pub(super) struct Domains {
    bodies: HashMap<SymbolId, Body>,
    /// Each Simulation hook action and the `System.hook` it implements.
    roots: Vec<(SymbolId, String)>,
    /// Each `AudioProcess` hook action and the `System.hook` it implements.
    realtime: Vec<(SymbolId, String)>,
    /// Every `@local` state.
    locals: HashSet<SymbolId>,
    /// Every `@persist` state that checked, in package order.
    persisted: Vec<Persisted>,
}

/// A `@persist` state: its system, key and type, and where it is marked.
struct Persisted {
    module: usize,
    /// The system or component that declares the state.
    owner: SymbolId,
    state: SymbolId,
    key: String,
    at: TextRange,
    ty: Ty,
}

/// The capability a `@persist` state needs.
const PERSIST: &str = "storage.persist";

/// Whether `node` carries the attribute `name`.
fn has_attribute(node: &SyntaxNode, name: &str) -> Option<SyntaxNode> {
    decl_attributes(node)
        .into_iter()
        .find(|(n, _, _)| n == name)
        .map(|(_, attr, _)| attr)
}

impl Domains {
    /// Records the callables, `@local` states and Simulation hooks of the
    /// module `env` lowered, checking each `@local` (it marks a system
    /// `state`, `E9103`) and each Simulation state's type (`E9105`).
    pub(super) fn collect(
        &mut self,
        items: &[Item],
        refs: &[ResolvedRef],
        env: &ModuleEnv<'_>,
        components: &[HirComponent],
        hooks: &[(SymbolId, Vec<BoundHook>)],
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let natives = env.native_calls.borrow();
        let mut index: Vec<(TextRange, Resolution)> =
            refs.iter().map(|r| (r.range, r.to)).collect();
        index.sort_by_key(|(range, _)| range.start());
        let body = |name: String, node: &SyntaxNode| {
            let range = node.text_range();
            let start = index.partition_point(|(r, _)| r.start() < range.start());
            let inside = index[start..]
                .iter()
                .take_while(|(r, _)| r.end() <= range.end());
            let mut mentions = Vec::new();
            let mut uses_env = Vec::new();
            for &(at, to) in inside {
                match to {
                    Resolution::Symbol(s) => mentions.push((s, at)),
                    Resolution::Env | Resolution::Theme => uses_env.push(at),
                    Resolution::Local(_) | Resolution::Native(_) => {}
                }
            }
            let calls = natives
                .iter()
                .filter(|(at, _)| range.contains_range(**at))
                .map(|(at, id)| (*id, *at))
                .collect();
            let awaits = node
                .descendants_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .filter(|t| t.kind() == SyntaxKind::AwaitKw)
                .map(|t| t.text_range())
                .collect();
            let loops = node
                .descendants()
                .into_iter()
                .filter_map(|n| {
                    let keyword = n
                        .children_with_tokens()
                        .into_iter()
                        .filter_map(|e| e.as_token().cloned())
                        .find(|t| {
                            matches!(
                                t.kind(),
                                SyntaxKind::WhileKw | SyntaxKind::ForKw | SyntaxKind::LoopKw
                            )
                        })?
                        .text_range();
                    let bounds = match n.kind() {
                        SyntaxKind::WhileStmt | SyntaxKind::LoopStmt => None,
                        SyntaxKind::ForStmt => static_bounds(&n, &index, &natives),
                        _ => return None,
                    };
                    Some(Loop {
                        at: keyword,
                        bounds,
                    })
                })
                .collect();
            Body {
                module: env.module,
                name,
                mentions,
                natives: calls,
                env: uses_env,
                awaits,
                loops,
            }
        };
        for item in items {
            let decl = match item {
                Item::Export(e) => e.declaration(),
                other => Some(other.clone()),
            };
            match decl {
                Some(Item::System(system)) => {
                    let component = system.as_component();
                    env.focus_component(&component);
                    let system_name = name_of(system.name());
                    let system_symbol = env.component_symbol();
                    let states = components
                        .iter()
                        .find(|c| c.source_origin == system.syntax().text_range())
                        .map(|c| &c.schema.states[..])
                        .unwrap_or_default();
                    for member in system.members() {
                        let node = member.syntax().clone();
                        let local = has_attribute(&node, "local");
                        let probe = has_attribute(&node, "probe");
                        let persist = has_attribute(&node, "persist");
                        let (name, is_body) = match &member {
                            Member::State(s) => (name_of(s.name()), false),
                            Member::Fn(f) => (name_of(f.name()), true),
                            Member::Action(a) => (name_of(a.name()), true),
                            Member::Computed(c) => (name_of(c.name()), true),
                            Member::Task(t) => (name_of(t.name()), true),
                            _ => (String::new(), false),
                        };
                        let symbol = env.member_symbol(&name);
                        if let Member::State(_) = member {
                            let state = states
                                .iter()
                                .find(|s| symbol.is_some() && s.meta.resolved_symbol == symbol);
                            if let (Some(_), Some(attr)) = (&local, &probe) {
                                misplaced_probe(attr, diagnostics);
                            }
                            if let (Some(attr), Some(state), Some(symbol)) =
                                (&persist, state, symbol)
                            {
                                self.persist(
                                    attr,
                                    &name,
                                    &state.meta.inferred_type,
                                    (system_symbol, symbol),
                                    env,
                                    diagnostics,
                                );
                            }
                            if local.is_some() {
                                self.locals.extend(symbol);
                                if let (Some(state), Some(symbol)) = (state, symbol) {
                                    let schema = schema_hash(&state.meta.inferred_type, env);
                                    env.behavior.borrow_mut().local_state(
                                        system_symbol,
                                        symbol,
                                        schema,
                                    );
                                }
                            } else if let Some(state) = state {
                                let ty = &state.meta.inferred_type;
                                if check_snapshot(ty, &name, &node, env, diagnostics)
                                    && let Some(symbol) = symbol
                                {
                                    env.behavior.borrow_mut().snapshot_state(
                                        system_symbol,
                                        symbol,
                                        schema_hash(ty, env),
                                    );
                                    if probe.is_some() {
                                        let shape = probe_shape(ty, env, &mut Vec::new());
                                        env.behavior.borrow_mut().probe_state(
                                            system_symbol,
                                            symbol,
                                            &name,
                                            shape,
                                        );
                                    }
                                }
                            }
                        } else {
                            if let Some(attr) = local {
                                misplaced_local(&attr, diagnostics);
                            }
                            if let Some(attr) = probe {
                                misplaced_probe(&attr, diagnostics);
                            }
                            if let Some(attr) = persist {
                                misplaced_persist(&attr, diagnostics);
                            }
                        }
                        if is_body && let Some(symbol) = symbol {
                            let qualified = format!("{system_name}.{name}");
                            self.bodies.insert(symbol, body(qualified, &node));
                        }
                    }
                    for (owner, bound) in hooks {
                        if *owner != system_symbol {
                            continue;
                        }
                        for (action, hook, domain) in bound {
                            let root = (*action, format!("{system_name}.{hook}"));
                            match domain {
                                HookDomain::Simulation => self.roots.push(root),
                                HookDomain::Realtime => self.realtime.push(root),
                                HookDomain::Presentation => {}
                            }
                        }
                        let realtime = bound.iter().filter(|(_, _, d)| *d == HookDomain::Realtime);
                        if realtime.clone().next().is_some()
                            && bound.iter().any(|(_, _, d)| *d != HookDomain::Realtime)
                            && let Some(name) = system.name()
                        {
                            diagnostics.push(Diagnostic::error(
                                "E9108",
                                name.text_range(),
                                format!(
                                    "`{system_name}` implements `AudioProcess`, so its state \
                                     lives on the audio thread and it implements no trait run \
                                     elsewhere; pass data to it with `send_audio` and \
                                     `AudioCommands`, and back with `block.send` and \
                                     `AudioListener`"
                                ),
                            ));
                        }
                    }
                }
                Some(Item::Component(component)) => {
                    env.focus_component(&component);
                    let owner = env.component_symbol();
                    let states = components
                        .iter()
                        .find(|c| c.source_origin == component.syntax().text_range())
                        .map(|c| &c.schema.states[..])
                        .unwrap_or_default();
                    for member in component.members() {
                        if let Some(attr) = has_attribute(member.syntax(), "local") {
                            misplaced_local(&attr, diagnostics);
                        }
                        if let Some(attr) = has_attribute(member.syntax(), "probe") {
                            misplaced_probe(&attr, diagnostics);
                        }
                        let Some(attr) = has_attribute(member.syntax(), "persist") else {
                            continue;
                        };
                        let Member::State(s) = &member else {
                            misplaced_persist(&attr, diagnostics);
                            continue;
                        };
                        let name = name_of(s.name());
                        let symbol = env.member_symbol(&name);
                        let state = states
                            .iter()
                            .find(|s| symbol.is_some() && s.meta.resolved_symbol == symbol);
                        if let (Some(state), Some(symbol)) = (state, symbol) {
                            self.persist(
                                &attr,
                                &name,
                                &state.meta.inferred_type,
                                (owner, symbol),
                                env,
                                diagnostics,
                            );
                        }
                    }
                }
                Some(Item::Fn(f)) => {
                    if let Some(&s) = env.scope.declared.get(&f.syntax().text_range()) {
                        self.bodies.insert(s, body(name_of(f.name()), f.syntax()));
                    }
                }
                Some(Item::Action(a)) => {
                    if let Some(&s) = env.scope.declared.get(&a.syntax().text_range()) {
                        self.bodies.insert(s, body(name_of(a.name()), a.syntax()));
                    }
                }
                _ => {}
            }
        }
    }

    /// Checks the `@persist` attribute `attr` of the state `name` of type
    /// `ty`, `(owner, state)` by symbol (the owner a system or a component): it names a key, the type
    /// snapshots and the package is granted `storage.persist` (`E9106`).
    /// The defaults a stored value of an older type may need are kept.
    fn persist(
        &mut self,
        attr: &SyntaxNode,
        name: &str,
        ty: &Ty,
        (owner, state): (SymbolId, SymbolId),
        env: &ModuleEnv<'_>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let at = attr.text_range();
        let Some(key) = persist_key(attr) else {
            diagnostics.push(Diagnostic::error(
                "E9106",
                at,
                "`@persist` names the key its state is stored under: `@persist(\"best_score\")`",
            ));
            return;
        };
        if let Some(why) = not_snapshot(ty, env, &mut HashSet::new()) {
            diagnostics.push(Diagnostic::error(
                "E9106",
                at,
                format!("the state `{name}` cannot persist: {why}"),
            ));
            return;
        }
        if !env.decls.profile.capabilities.contains(PERSIST) {
            let mut diagnostic = Diagnostic::error(
                "E9106",
                at,
                format!("persisting the state `{name}` needs the `{PERSIST}` capability"),
            );
            diagnostic.notes.push(format!(
                "grant it in `Viso.toml`: `[package] capabilities = [\"{PERSIST}\"]`"
            ));
            diagnostics.push(diagnostic);
            return;
        }
        keep_defaults(ty, env, &mut HashSet::new());
        self.persisted.push(Persisted {
            module: env.module,
            owner,
            state,
            key,
            at,
            ty: ty.clone(),
        });
    }

    /// Records each `@persist` state in its system's layout, the first of
    /// two under one key (`E9106` for the second), with the package's
    /// `@migrate` functions when any state persists.
    pub(super) fn persist_slots(
        &self,
        decls: &Declarations,
        behavior: &RefCell<ProgramBuilder>,
        migrators: &[(TextRange, Migrator)],
        per_module: &mut [Vec<Diagnostic>],
    ) {
        let schema = |ty: &Ty| {
            let builder = behavior.borrow();
            decls.types.value_schema(ty, &|record, index| {
                builder.field_default(record, index).map(|f| f.0)
            })
        };
        let mut keys: HashMap<&str, &Persisted> = HashMap::new();
        for persisted in &self.persisted {
            if let Some(first) = keys.get(persisted.key.as_str()) {
                let mut diagnostic = Diagnostic::error(
                    "E9106",
                    persisted.at,
                    format!("a second state persists under the key `{}`", persisted.key),
                );
                if first.module == persisted.module {
                    diagnostic
                        .related
                        .push(Related::new(first.at, "the first is marked here"));
                } else {
                    diagnostic.notes.push(format!(
                        "the first is in module `{}`",
                        decls.module_paths[first.module]
                    ));
                }
                per_module[persisted.module].push(diagnostic);
                continue;
            }
            keys.insert(&persisted.key, persisted);
            let value = schema(&persisted.ty);
            behavior.borrow_mut().persist_state(
                persisted.owner,
                persisted.state,
                &persisted.key,
                value,
                decls.types.describe(&persisted.ty),
            );
        }
        if !behavior.borrow().persists() {
            return;
        }
        let migrators = migrators
            .iter()
            .filter_map(|(_, m)| {
                let chunk = behavior.borrow().fn_of(m.symbol)?.0;
                Some(viso_behavior::Migrator {
                    from: m.from.as_str().into(),
                    param: schema(&m.param),
                    ret: schema(&m.ret),
                    chunk,
                })
            })
            .collect();
        behavior.borrow_mut().migrators(migrators);
    }

    /// Checks every callable the Simulation roots reach, reporting into the
    /// module that declares each.
    pub(super) fn check(
        &self,
        decls: &Declarations,
        natives: &viso_behavior::native::Natives,
        profile: TargetProfile,
        per_module: &mut [Vec<Diagnostic>],
    ) {
        // Breadth-first from the roots, each body reached once with the root
        // it was first reached from.
        let mut reached: HashMap<SymbolId, &str> = HashMap::new();
        let mut queue: VecDeque<SymbolId> = VecDeque::new();
        for (action, root) in &self.roots {
            if reached.insert(*action, root).is_none() {
                queue.push_back(*action);
            }
        }
        let mut order = Vec::new();
        while let Some(symbol) = queue.pop_front() {
            order.push(symbol);
            let Some(body) = self.bodies.get(&symbol) else {
                continue;
            };
            let root = reached[&symbol];
            for &(mention, _) in &body.mentions {
                if self.bodies.contains_key(&mention) && !reached.contains_key(&mention) {
                    reached.insert(mention, root);
                    queue.push_back(mention);
                }
            }
        }
        let mut reported: HashSet<TextRange> = HashSet::new();
        for symbol in order {
            let Some(body) = self.bodies.get(&symbol) else {
                continue;
            };
            let root = reached[&symbol];
            let mut found = Vec::new();
            for &(mention, at) in &body.mentions {
                if self.locals.contains(&mention) {
                    found.push((
                        "E9103",
                        at,
                        "the Simulation domain does not read or write `@local` state".to_owned(),
                    ));
                } else if decls.facts.get(&mention).map(|f| f.kind) == Some(SymbolKind::Task) {
                    found.push((
                        "E9104",
                        at,
                        "the Simulation domain does not start a `task`".to_owned(),
                    ));
                }
            }
            for &(id, at) in &body.natives {
                let Some(entry) = natives.function_by_id(id) else {
                    continue;
                };
                let f = entry.function;
                let name = entry.path.rsplit("::").next().unwrap_or(&entry.path);
                if f.presentation {
                    if f.ret != SchemaTy::Unit {
                        found.push((
                            "E9103",
                            at,
                            format!(
                                "`{name}` returns a Presentation value, which the Simulation \
                                 domain does not use"
                            ),
                        ));
                    }
                } else if f.kind == NativeKind::Task {
                    found.push((
                        "E9104",
                        at,
                        format!(
                            "`{name}` is a native task, which the Simulation domain does not await"
                        ),
                    ));
                } else if f.determinism < profile.determinism {
                    found.push((
                        "E9104",
                        at,
                        format!(
                            "`{name}` is reproducible at `{}`, below the `{}` the Simulation \
                             domain needs",
                            f.determinism.name(),
                            profile.determinism.name()
                        ),
                    ));
                }
            }
            for &at in &body.env {
                found.push((
                    "E9104",
                    at,
                    "the Simulation domain does not read the adaptive environment".to_owned(),
                ));
            }
            for &at in &body.awaits {
                found.push((
                    "E9104",
                    at,
                    "the Simulation domain does not `await`".to_owned(),
                ));
            }
            for (code, at, message) in found {
                if !reported.insert(at) {
                    continue;
                }
                let mut diagnostic = Diagnostic::error(code, at, message);
                diagnostic.notes.push(if body.name == root {
                    format!("`{root}` is a Simulation hook")
                } else {
                    format!(
                        "`{}` runs in the Simulation domain: `{root}` reaches it",
                        body.name
                    )
                });
                if let Some(module) = per_module.get_mut(body.module) {
                    module.push(diagnostic);
                }
            }
        }
    }
}

impl Domains {
    /// Checks every function the `AudioProcess` roots reach, over the
    /// instructions `behavior` lowered for it and its source, reporting into
    /// the module that declares each (`E9108`).
    pub(super) fn check_realtime(
        &self,
        decls: &Declarations,
        natives: &viso_behavior::native::Natives,
        behavior: &ProgramBuilder,
        per_module: &mut [Vec<Diagnostic>],
    ) {
        if self.realtime.is_empty() {
            return;
        }
        let program = behavior.program();
        let body_of = |f: FuncId| {
            program
                .functions
                .get(f.0 as usize)
                .and_then(|f| f.body.as_ref().ok())
        };
        // Breadth-first over calls and closures from the roots, each
        // function reached once with the root it was first reached from.
        let mut reached: HashMap<FuncId, &str> = HashMap::new();
        let mut queue: VecDeque<FuncId> = VecDeque::new();
        let mut roots = Vec::new();
        for (action, root) in &self.realtime {
            if let Some(f) = behavior.function_of(*action)
                && reached.insert(f, root).is_none()
            {
                roots.push(f);
                queue.push_back(f);
            }
        }
        let mut order = Vec::new();
        while let Some(f) = queue.pop_front() {
            order.push(f);
            let root = reached[&f];
            for inst in body_of(f).map_or(&[][..], |b| &b.insts[..]) {
                if let Inst::Call { func, .. } | Inst::Closure { func, .. } = inst
                    && !reached.contains_key(func)
                {
                    reached.insert(*func, root);
                    queue.push_back(*func);
                }
            }
        }
        let recursive = recursive_calls(&roots, &body_of);
        let mut reported: HashSet<TextRange> = HashSet::new();
        for f in order {
            let (Some(function), Some(body)) = (program.functions.get(f.0 as usize), body_of(f))
            else {
                continue;
            };
            let root = reached[&f];
            let mut found: Vec<(TextRange, String)> = Vec::new();
            for (pc, inst) in body.insts.iter().enumerate() {
                let at = body.spans[pc];
                let message = match inst {
                    Inst::Make { .. } => "builds a record, tuple or enum payload, which allocates",
                    Inst::List { .. } => "builds a list, which allocates",
                    Inst::Closure { .. } => "creates a closure, which allocates",
                    Inst::Concat { .. }
                    | Inst::Display { .. }
                    | Inst::Translate { .. }
                    | Inst::Binary {
                        op: BinaryOp::Concat,
                        ..
                    } => "builds a `String`, which allocates",
                    Inst::SetPath { .. } | Inst::Remove { .. } | Inst::Truncate { .. } => {
                        "writes into a record or list in place, which copies it while it is shared"
                    }
                    Inst::Push { .. } | Inst::Insert { .. } => "grows a list, which allocates",
                    Inst::Start { .. } => "starts a task",
                    Inst::Emit { .. } => {
                        "emits an event; the audio thread passes data only through a bounded \
                         lock-free queue"
                    }
                    Inst::CallValue { .. } => {
                        "calls a closure value, which the check cannot follow"
                    }
                    Inst::Call { .. } if recursive.contains(&(f, pc)) => {
                        "recurses, which has no static bound"
                    }
                    Inst::Native { import, .. } => {
                        let Some(import) = program.natives.get(*import as usize) else {
                            continue;
                        };
                        let Some(entry) = natives.function_by_id(NativeId::of(&import.path)) else {
                            continue;
                        };
                        if entry.function.realtime_safe {
                            continue;
                        }
                        let name = import.path.rsplit("::").next().unwrap_or(&import.path);
                        found.push((at, format!("`{name}` is not realtime-safe")));
                        continue;
                    }
                    _ => continue,
                };
                found.push((at, format!("the audio thread {message}")));
            }
            if let Some(source) = function.symbol.and_then(|s| self.bodies.get(&s)) {
                for &(mention, at) in &source.mentions {
                    if decls.facts.get(&mention).map(|f| f.kind) == Some(SymbolKind::Resource) {
                        found.push((at, "the audio thread does not load a resource".to_owned()));
                    }
                }
                let fixed = |bound: &Bound| match bound {
                    Bound::Symbol(s) => {
                        decls.facts.get(s).map(|f| f.kind) == Some(SymbolKind::Const)
                    }
                    Bound::Native(id) => natives.function_by_id(*id).is_some_and(|entry| {
                        entry
                            .path
                            .strip_prefix(AudioBlock::PATH)
                            .is_some_and(|method| method.starts_with("::"))
                    }),
                };
                for l in &source.loops {
                    if !l.bounds.as_ref().is_some_and(|b| b.iter().all(fixed)) {
                        found.push((
                            l.at,
                            "the audio thread loops only over a range bounded by constants and \
                             the block's size"
                                .to_owned(),
                        ));
                    }
                }
            }
            for (at, message) in found {
                if !reported.insert(at) {
                    continue;
                }
                let mut diagnostic = Diagnostic::error("E9108", at, message);
                diagnostic.notes.push(if function.name == root {
                    format!("`{root}` is an `AudioProcess` hook")
                } else {
                    format!(
                        "`{}` runs on the audio thread: `{root}` reaches it",
                        function.name
                    )
                });
                if let Some(module) = per_module.get_mut(function.module) {
                    module.push(diagnostic);
                }
            }
        }
    }
}

/// The calls, by function and instruction, that close a cycle of calls
/// reachable from `roots`.
fn recursive_calls<'p>(
    roots: &[FuncId],
    body_of: &dyn Fn(FuncId) -> Option<&'p crate::behavior::ir::Body>,
) -> HashSet<(FuncId, usize)> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }
    let mut marks: HashMap<FuncId, Mark> = HashMap::new();
    let mut closing = HashSet::new();
    for &root in roots {
        if marks.contains_key(&root) {
            continue;
        }
        // Depth-first, each frame a function and the next instruction to
        // visit.
        let mut stack = vec![(root, 0usize)];
        marks.insert(root, Mark::Open);
        while let Some(top) = stack.last_mut() {
            let (f, pc) = *top;
            let insts = body_of(f).map_or(&[][..], |b| &b.insts[..]);
            let Some(inst) = insts.get(pc) else {
                marks.insert(f, Mark::Done);
                stack.pop();
                continue;
            };
            top.1 += 1;
            if let Inst::Call { func, .. } = inst {
                match marks.get(func) {
                    Some(Mark::Open) => {
                        closing.insert((f, pc));
                    }
                    Some(Mark::Done) => {}
                    None => {
                        marks.insert(*func, Mark::Open);
                        stack.push((*func, 0));
                    }
                }
            }
        }
    }
    closing
}

/// What the bounds of the `for` loop `node` name, when it iterates a range
/// written in its head whose bounds are built of literals, names and native
/// calls.
fn static_bounds(
    node: &SyntaxNode,
    index: &[(TextRange, Resolution)],
    natives: &HashMap<TextRange, NativeId>,
) -> Option<Vec<Bound>> {
    let exprs = |n: &SyntaxNode| -> Vec<SyntaxNode> {
        n.children()
            .into_iter()
            .filter(|c| Expr::cast(c.clone()).is_some())
            .collect()
    };
    let mut iterable = exprs(node).into_iter().next()?;
    while iterable.kind() == SyntaxKind::ParenExpr {
        iterable = exprs(&iterable).into_iter().next()?;
    }
    if iterable.kind() != SyntaxKind::RangeExpr {
        return None;
    }
    let bounds = exprs(&iterable);
    if bounds.len() != 2 {
        return None;
    }
    let mut out = Vec::new();
    let mut open = bounds;
    while let Some(n) = open.pop() {
        match n.kind() {
            SyntaxKind::LiteralExpr => {}
            SyntaxKind::UnaryExpr | SyntaxKind::BinaryExpr | SyntaxKind::ParenExpr => {
                open.extend(exprs(&n));
            }
            SyntaxKind::PathExpr => {
                let at = n.text_range();
                let start = index.partition_point(|(r, _)| r.start() < at.start());
                let (_, to) = index[start..]
                    .first()
                    .filter(|(r, _)| at.contains_range(*r))?;
                let Resolution::Symbol(s) = to else {
                    return None;
                };
                out.push(Bound::Symbol(*s));
            }
            SyntaxKind::CallExpr | SyntaxKind::FieldExpr => {
                out.push(Bound::Native(*natives.get(&n.text_range())?));
            }
            _ => return None,
        }
    }
    Some(out)
}

fn misplaced_local(attr: &SyntaxNode, diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.push(Diagnostic::error(
        "E9103",
        attr.text_range(),
        "`@local` marks a `state` of a `system`: Presentation state the Simulation never reads",
    ));
}

fn misplaced_probe(attr: &SyntaxNode, diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.push(Diagnostic::error(
        "E9110",
        attr.text_range(),
        "`@probe` marks a Simulation `state` of a `system`, which a game test traces every tick",
    ));
}

fn misplaced_persist(attr: &SyntaxNode, diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.push(Diagnostic::error(
        "E9106",
        attr.text_range(),
        "`@persist` marks a `state` of a `system` or a `component`",
    ));
}

/// The key of a `@persist("key")` attribute: its one unlabeled argument, a
/// non-empty string literal.
fn persist_key(attr: &SyntaxNode) -> Option<String> {
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
    let values = arg.children();
    let [value] = values.as_slice() else {
        return None;
    };
    let labeled = arg
        .children_with_tokens()
        .into_iter()
        .any(|e| e.as_token().is_some_and(|t| t.kind() == SyntaxKind::Colon));
    if labeled || value.kind() != SyntaxKind::LiteralExpr {
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
    let key = crate::hir::infer::pattern::unescape(body)?;
    (!key.is_empty() && !key.contains('{')).then_some(key)
}

/// Keeps the default of every defaulted field of each record `ty` reaches,
/// which a stored value of an older version of the record takes.
fn keep_defaults(ty: &Ty, env: &ModuleEnv<'_>, seen: &mut HashSet<SymbolId>) {
    let each = |items: &[Ty], seen: &mut HashSet<SymbolId>| {
        for item in items {
            keep_defaults(item, env, seen);
        }
    };
    match ty {
        Ty::Tuple(items) => each(items, seen),
        Ty::List(t) | Ty::Option(t) | Ty::Range(t) | Ty::RangeInclusive(t) => {
            keep_defaults(t, env, seen);
        }
        Ty::Result(a, b) => {
            keep_defaults(a, env, seen);
            keep_defaults(b, env, seen);
        }
        Ty::Named(id, ..) if seen.insert(*id) => {
            if let Some(fields) = env.record_fields(*id) {
                let record = env.type_name(*id).unwrap_or_default().to_owned();
                for (index, field) in fields.iter().enumerate() {
                    if field.has_default {
                        let name = format!("{record}.{}", field.name);
                        env.behavior
                            .borrow_mut()
                            .field_default_slot(*id, index as u32, &name);
                    }
                    keep_defaults(&field.ty, env, seen);
                }
            } else if let Some(variants) = env.enum_variants(*id) {
                for variant in variants {
                    match &variant.payload {
                        VariantPayload::Unit => {}
                        VariantPayload::Tuple(items) => each(items, seen),
                        VariantPayload::Record(fields) => {
                            for field in fields {
                                keep_defaults(&field.ty, env, seen);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// How a game test writes a value of `ty`; `open` holds the nominal types
/// being described, a recursive one written by structure.
fn probe_shape(ty: &Ty, env: &ModuleEnv<'_>, open: &mut Vec<SymbolId>) -> ProbeShape {
    let fields = |fields: &[FieldInfo], open: &mut Vec<SymbolId>| {
        fields
            .iter()
            .map(|f| (f.name.clone(), probe_shape(&f.ty, env, open)))
            .collect()
    };
    match ty {
        Ty::Bool => ProbeShape::Bool,
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 => ProbeShape::Signed,
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 => ProbeShape::Unsigned,
        Ty::F32 | Ty::F64 | Ty::Duration | Ty::Angle | Ty::Frequency => ProbeShape::Float,
        Ty::Char => ProbeShape::Char,
        Ty::String => ProbeShape::Str,
        Ty::Unit => ProbeShape::Unit,
        Ty::Tuple(items) => {
            ProbeShape::Tuple(items.iter().map(|t| probe_shape(t, env, open)).collect())
        }
        Ty::List(item) => ProbeShape::List(Box::new(probe_shape(item, env, open))),
        Ty::Option(item) => ProbeShape::Option(Box::new(probe_shape(item, env, open))),
        Ty::Named(id, ..) if !open.contains(id) => {
            open.push(*id);
            let shape = if let Some(record) = env.record_fields(*id) {
                ProbeShape::Record(fields(record, open))
            } else if let Some(variants) = env.enum_variants(*id) {
                let variants = variants
                    .iter()
                    .map(|v| ProbeVariant {
                        name: v.name.clone(),
                        payload: match &v.payload {
                            VariantPayload::Unit => ProbePayload::Unit,
                            VariantPayload::Tuple(items) => ProbePayload::Tuple(
                                items.iter().map(|t| probe_shape(t, env, open)).collect(),
                            ),
                            VariantPayload::Record(record) => {
                                ProbePayload::Record(fields(record, open))
                            }
                        },
                    })
                    .collect();
                ProbeShape::Enum(variants)
            } else {
                ProbeShape::Value
            };
            open.pop();
            shape
        }
        _ => ProbeShape::Value,
    }
}

/// Whether a value of `ty` can be captured in a snapshot; `E9105` if not.
fn check_snapshot(
    ty: &Ty,
    name: &str,
    node: &SyntaxNode,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut seen = HashSet::new();
    let Some(why) = not_snapshot(ty, env, &mut seen) else {
        return true;
    };
    let at = node
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| t.text() == name)
        .map_or(node.text_range(), |t| t.text_range());
    let mut diagnostic = Diagnostic::error(
        "E9105",
        at,
        format!("the Simulation state `{name}` does not implement `Snapshot`: {why}"),
    );
    diagnostic
        .notes
        .push("mark it `@local` if only Presentation uses it".to_owned());
    diagnostic.related.push(Related::new(
        at,
        "a Simulation state is snapshotted every tick",
    ));
    diagnostics.push(diagnostic);
    false
}

/// A hash of the schema of `ty`: its structure with every record's field
/// names and every enum's variants spelled out, so a snapshot value restores
/// only into a state whose type would read it the same way.
fn schema_hash(ty: &Ty, env: &ModuleEnv<'_>) -> u64 {
    let mut text = String::new();
    schema_text(ty, env, &mut Vec::new(), &mut text);
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn schema_text(ty: &Ty, env: &ModuleEnv<'_>, open: &mut Vec<SymbolId>, out: &mut String) {
    use std::fmt::Write;
    let each = |items: &[Ty], open: &mut Vec<SymbolId>, out: &mut String| {
        for item in items {
            schema_text(item, env, open, out);
            out.push(',');
        }
    };
    match ty {
        Ty::Named(id, ..) => {
            let _ = write!(out, "{:x}{:x}", id.hi, id.lo);
            if open.contains(id) {
                return;
            }
            open.push(*id);
            if let Some(fields) = env.record_fields(*id) {
                out.push('{');
                for field in fields {
                    out.push_str(&field.name);
                    out.push(':');
                    schema_text(&field.ty, env, open, out);
                    out.push(',');
                }
                out.push('}');
            } else if let Some(variants) = env.enum_variants(*id) {
                out.push('[');
                for variant in variants {
                    out.push_str(&variant.name);
                    match &variant.payload {
                        VariantPayload::Unit => {}
                        VariantPayload::Tuple(items) => {
                            out.push('(');
                            each(items, open, out);
                            out.push(')');
                        }
                        VariantPayload::Record(fields) => {
                            out.push('{');
                            for field in fields {
                                out.push_str(&field.name);
                                out.push(':');
                                schema_text(&field.ty, env, open, out);
                                out.push(',');
                            }
                            out.push('}');
                        }
                    }
                    out.push(',');
                }
                out.push(']');
            }
            open.pop();
        }
        Ty::Tuple(items) => {
            out.push('(');
            each(items, open, out);
            out.push(')');
        }
        Ty::List(t) | Ty::Option(t) | Ty::Range(t) | Ty::RangeInclusive(t) => {
            let head = match ty {
                Ty::List(_) => "List<",
                Ty::Option(_) => "Option<",
                Ty::Range(_) => "Range<",
                _ => "RangeInclusive<",
            };
            out.push_str(head);
            schema_text(t, env, open, out);
            out.push('>');
        }
        Ty::Result(a, b) => {
            out.push_str("Result<");
            schema_text(a, env, open, out);
            out.push(',');
            schema_text(b, env, open, out);
            out.push('>');
        }
        other => {
            let _ = write!(out, "{other:?}");
        }
    }
}

/// Why a value of `ty` cannot be snapshotted, or `None` when it can: value
/// types derive `Snapshot`, a closure cannot, a native handle only when its
/// type declares it.
fn not_snapshot(ty: &Ty, env: &ModuleEnv<'_>, seen: &mut HashSet<SymbolId>) -> Option<String> {
    match ty {
        Ty::Fn(..) => Some("a closure has no snapshot".to_owned()),
        Ty::Native(id) => {
            let entry = env.natives.ty_by_id(*id)?;
            (!entry.ty.snapshots()).then(|| {
                let name = entry.path.rsplit("::").next().unwrap_or(&entry.path);
                format!("the native handle `{name}` declares no snapshot")
            })
        }
        Ty::Tuple(items) => items.iter().find_map(|t| not_snapshot(t, env, seen)),
        Ty::List(t) | Ty::Option(t) | Ty::Range(t) | Ty::RangeInclusive(t) => {
            not_snapshot(t, env, seen)
        }
        Ty::Result(a, b) => not_snapshot(a, env, seen).or_else(|| not_snapshot(b, env, seen)),
        Ty::Named(id, ..) => {
            if !seen.insert(*id) {
                return None;
            }
            if let Some(fields) = env.record_fields(*id) {
                return fields.iter().find_map(|f| not_snapshot(&f.ty, env, seen));
            }
            let variants = env.enum_variants(*id)?;
            variants.iter().find_map(|v| match &v.payload {
                VariantPayload::Unit => None,
                VariantPayload::Tuple(items) => {
                    items.iter().find_map(|t| not_snapshot(t, env, seen))
                }
                VariantPayload::Record(fields) => {
                    fields.iter().find_map(|f| not_snapshot(&f.ty, env, seen))
                }
            })
        }
        _ => None,
    }
}

/// Each hook action of a system with its trait hook's name and domain.
pub(super) fn hook_domains(env: &ModuleEnv<'_>, hooks: &[(NativeId, SymbolId)]) -> Vec<BoundHook> {
    hooks
        .iter()
        .filter_map(|&(id, action)| {
            let (name, domain) = env.natives.traits().iter().find_map(|t| {
                t.native_trait
                    .hooks
                    .iter()
                    .find(|h| t.hook_id(h.name) == id)
                    .map(|h| (h.name.to_owned(), h.domain))
            })?;
            Some((action, name, domain))
        })
        .collect()
}
