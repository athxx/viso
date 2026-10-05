//! Lowering typed bodies to the Behavior IR.
//!
//! A [`ProgramBuilder`] collects a whole package's functions and component
//! layouts while HIR lowering walks the modules; [`lower_body`] and
//! [`lower_value`] lower one callable body or one value expression against the
//! [`InferCx`] that typed it. Evaluation is left to right (receiver, then
//! arguments in source order, then the call), and every instruction records the
//! span of the innermost expression or statement it lowers.

mod expr;
mod pattern;
mod stmt;

use std::collections::{HashMap, HashSet};

use viso_behavior::game::InputSchema;
use viso_behavior::native::{NativeEntry, NativeId};
use viso_behavior::retype::ValueSchema;
use viso_behavior::{Migrator, PersistSlot, TaskPolicy};

use viso_ui::adaptive::EnvField;

use super::ir::{
    Body, ComponentLayout, Const, EffectEntry, EnvSlot, FuncId, Function, FunctionKind, Inst,
    NativeImport, Num, Program, Reg, Site, SystemLayout, Unsupported,
};
use super::probe::{Probe, ProbeShape};
use crate::ast::{AssignablePath, AstNode, Block, Expr};
use crate::hir::infer::InferCx;
use crate::hir::{CallableKind, ComponentSchema, Ty, TypeEnv};
use crate::resolve::{LocalSlot, Resolution, SymbolId};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

/// The result of lowering one construct.
type Lower<T> = Result<T, Unsupported>;

/// Where a component's `state` or `input` lives.
#[derive(Debug, Clone, Copy)]
enum Place {
    State(u32),
    Input(u32),
}

/// Collects a package's lowered behavior.
#[derive(Debug, Default)]
pub(crate) struct ProgramBuilder {
    program: Program,
    /// The function of each `fn`/`action`/`computed`/`const` declaration.
    by_symbol: HashMap<SymbolId, FuncId>,
    /// Each `state`/`input` declaration's component (by layout index) and slot.
    places: HashMap<SymbolId, (usize, Place)>,
    /// Each event declaration's index in its component.
    events: HashMap<SymbolId, u32>,
    /// The default of each record field, by record and field index.
    field_defaults: HashMap<(SymbolId, u32), FuncId>,
    /// The import index of each native function a body calls.
    natives: HashMap<NativeId, u32>,
    /// Whether a debug draw call lowers to nothing, as in a release build.
    strip_debug_draw: bool,
    /// The task slot names `start .. as` uses, by slot index.
    task_slots: Vec<String>,
    /// The task that awaits a native task, by its import and argument count.
    native_tasks: HashMap<(u32, usize), FuncId>,
}

/// What a lowered function is.
pub(crate) struct Def {
    /// Its name (`Component.member` for a component member).
    pub name: String,
    /// What it lowers.
    pub kind: FunctionKind,
    /// The declaration it lowers.
    pub symbol: Option<SymbolId>,
    /// The module it is declared in.
    pub module: usize,
    /// The placeholder it fills, when one was reserved for it (a record field
    /// default a literal referenced before the record was lowered).
    pub into: Option<FuncId>,
}

/// The name of the state slot holding the `env` field `field`; no source
/// name contains the `.`.
pub(crate) fn env_state(field: EnvField) -> String {
    format!("env.{}", field.name())
}

/// The reason a function that was referenced but never defined has.
const NO_BODY: &str = "has no body to run";

impl ProgramBuilder {
    /// An empty builder.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The function of the declaration `symbol`, reserving a placeholder named
    /// `name` until the declaration is lowered.
    fn func_of(&mut self, symbol: SymbolId, name: &str) -> FuncId {
        if let Some(id) = self.by_symbol.get(&symbol) {
            return *id;
        }
        let id = self.push(Function {
            name: name.to_string(),
            kind: FunctionKind::Fn,
            symbol: Some(symbol),
            module: 0,
            params: 0,
            captures: Vec::new(),
            body: Err(Unsupported {
                reason: NO_BODY.to_string(),
                at: TextRange::empty(0.into()),
            }),
        });
        self.by_symbol.insert(symbol, id);
        id
    }

    fn push(&mut self, function: Function) -> FuncId {
        let id = FuncId(self.program.functions.len() as u32);
        self.program.functions.push(function);
        id
    }

    /// The index of the task slot `name`.
    fn task_slot(&mut self, name: &str) -> u32 {
        let at = match self.task_slots.iter().position(|s| s == name) {
            Some(at) => at,
            None => {
                self.task_slots.push(name.to_owned());
                self.task_slots.len() - 1
            }
        };
        at as u32
    }

    /// The task that calls the native task `import` with its `argc`
    /// arguments and returns what it awaited, so `start` can run it.
    fn native_task(&mut self, import: u32, argc: usize, name: &str, module: usize) -> FuncId {
        if let Some(&id) = self.native_tasks.get(&(import, argc)) {
            return id;
        }
        let args: Vec<Reg> = (0..argc as u32).map(Reg).collect();
        let dst = Reg(argc as u32);
        let at = TextRange::empty(0.into());
        let id = self.push(Function {
            name: format!("{name}/task"),
            kind: FunctionKind::Task,
            symbol: None,
            module,
            params: argc as u32,
            captures: Vec::new(),
            body: Ok(Body {
                regs: argc as u32 + 1,
                insts: vec![
                    Inst::Native { dst, import, args },
                    Inst::Return { src: dst },
                ],
                spans: vec![at, at],
            }),
        });
        self.native_tasks.insert((import, argc), id);
        id
    }

    /// Adds `function`, filling the placeholder `into` or the one of its
    /// declaration if one was reserved.
    fn define(&mut self, function: Function, into: Option<FuncId>) -> FuncId {
        if let Some(id) = into {
            self.program.functions[id.0 as usize] = function;
            return id;
        }
        let named = matches!(
            function.kind,
            FunctionKind::Fn
                | FunctionKind::Action
                | FunctionKind::Task
                | FunctionKind::Computed
                | FunctionKind::Const
        );
        match function.symbol.filter(|_| named) {
            Some(symbol) => {
                let id = self.func_of(symbol, &function.name);
                self.program.functions[id.0 as usize] = function;
                id
            }
            None => self.push(function),
        }
    }

    /// Registers a component's layout: its state and input slots, its events and
    /// its `fn`/`action`/`computed` members. Must precede lowering its bodies.
    pub(crate) fn component(&mut self, schema: &ComponentSchema) {
        let layout = self.program.components.len();
        for (i, state) in schema.states.iter().enumerate() {
            if let Some(symbol) = state.meta.resolved_symbol {
                self.places.insert(symbol, (layout, Place::State(i as u32)));
            }
        }
        for (i, input) in schema.inputs.iter().enumerate() {
            if let Some(symbol) = input.meta.resolved_symbol {
                self.places.insert(symbol, (layout, Place::Input(i as u32)));
            }
        }
        for (i, event) in schema.events.iter().enumerate() {
            if let Some(symbol) = event.meta.resolved_symbol {
                self.events.insert(symbol, i as u32);
            }
        }
        let mut members = Vec::new();
        for callable in &schema.callables {
            if !matches!(callable.kind, CallableKind::Fn | CallableKind::Action) {
                continue;
            }
            if let Some(symbol) = callable.meta.resolved_symbol {
                let name = format!("{}.{}", schema.name, callable.name);
                members.push((callable.name.clone(), self.func_of(symbol, &name)));
            }
        }
        for computed in &schema.computeds {
            if let Some(symbol) = computed.meta.resolved_symbol {
                let name = format!("{}.{}", schema.name, computed.name);
                members.push((computed.name.clone(), self.func_of(symbol, &name)));
            }
        }
        self.program.components.push(ComponentLayout {
            symbol: schema.symbol,
            name: schema.name.clone(),
            states: schema.states.iter().map(|s| s.name.clone()).collect(),
            inputs: schema.inputs.iter().map(|s| s.name.clone()).collect(),
            events: schema.events.iter().map(|s| s.name.clone()).collect(),
            state_inits: vec![None; schema.states.len()],
            input_defaults: vec![None; schema.inputs.len()],
            members,
            handlers: Vec::new(),
            regional: Vec::new(),
            env: Vec::new(),
            effects: Vec::new(),
            starters: Vec::new(),
        });
    }

    /// Registers the component registered last as the system `symbol`,
    /// each hook implemented by the member action `action`.
    pub(crate) fn system(&mut self, symbol: SymbolId, hooks: &[(NativeId, SymbolId)]) {
        let Some(component) = self.program.components.len().checked_sub(1) else {
            return;
        };
        let hooks = hooks
            .iter()
            .filter_map(|&(hook, action)| Some((hook, *self.by_symbol.get(&action)?)))
            .collect();
        self.program.systems.push(SystemLayout {
            symbol,
            component: component as u32,
            hooks,
            snapshot: Vec::new(),
            locals: Vec::new(),
            probes: Vec::new(),
            persist: Vec::new(),
        });
    }

    /// Adds the state `state` of system `system` to what a game snapshot
    /// captures, under its type's schema hash `schema`.
    pub(crate) fn snapshot_state(&mut self, system: SymbolId, state: SymbolId, schema: u64) {
        let Some(&(_, Place::State(slot))) = self.places.get(&state) else {
            return;
        };
        if let Some(layout) = self.program.systems.iter_mut().find(|s| s.symbol == system) {
            layout.snapshot.push((state, slot, schema));
        }
    }

    /// Adds the `@local` state `state` of system `system`, of the type with
    /// schema hash `schema`.
    pub(crate) fn local_state(&mut self, system: SymbolId, state: SymbolId, schema: u64) {
        let Some(&(_, Place::State(slot))) = self.places.get(&state) else {
            return;
        };
        if let Some(layout) = self.program.systems.iter_mut().find(|s| s.symbol == system) {
            layout.locals.push((state, slot, schema));
        }
    }

    /// Traces the state `state`, named `name`, of system `system` in a game
    /// test, its values written as `shape`.
    pub(crate) fn probe_state(
        &mut self,
        system: SymbolId,
        state: SymbolId,
        name: &str,
        shape: ProbeShape,
    ) {
        let Some(&(_, Place::State(slot))) = self.places.get(&state) else {
            return;
        };
        if let Some(layout) = self.program.systems.iter_mut().find(|s| s.symbol == system) {
            layout.probes.push(Probe {
                name: name.to_owned(),
                slot,
                shape,
            });
        }
    }

    /// Persists the state `state` of system `system` under `key`, its type
    /// `schema`, spelled `spelling`.
    pub(crate) fn persist_state(
        &mut self,
        system: SymbolId,
        state: SymbolId,
        key: &str,
        schema: ValueSchema,
        spelling: String,
    ) {
        let Some(&(_, Place::State(slot))) = self.places.get(&state) else {
            return;
        };
        if let Some(layout) = self.program.systems.iter_mut().find(|s| s.symbol == system) {
            layout.persist.push(PersistSlot {
                key: key.into(),
                slot,
                schema,
                spelling: spelling.into(),
            });
        }
    }

    /// Whether a system persists a state.
    pub(crate) fn persists(&self) -> bool {
        self.program.systems.iter().any(|s| !s.persist.is_empty())
    }

    /// Sets the `@migrate` functions a persisted value converts by.
    pub(crate) fn migrators(&mut self, migrators: Vec<Migrator>) {
        self.program.migrators = migrators;
    }

    /// The function of the `fn` declared as `symbol`, once it has a body.
    pub(crate) fn fn_of(&self, symbol: SymbolId) -> Option<FuncId> {
        self.program
            .functions
            .iter()
            .position(|f| f.symbol == Some(symbol) && f.kind == FunctionKind::Fn && f.body.is_ok())
            .map(|i| FuncId(i as u32))
    }

    /// Sets the ticks a second of the systems' fixed step.
    pub(crate) fn tick_rate(&mut self, tick_rate: u32) {
        self.program.tick_rate = Some(tick_rate);
    }

    /// Sets the capabilities the package is granted.
    pub(crate) fn capabilities(&mut self, capabilities: Vec<String>) {
        self.program.capabilities = capabilities;
    }

    /// Makes debug draw calls lower to nothing.
    pub(crate) fn strip_debug_draw(&mut self, strip: bool) {
        self.strip_debug_draw = strip;
    }

    /// Whether debug draw calls lower to nothing.
    pub(crate) fn strips_debug_draw(&self) -> bool {
        self.strip_debug_draw
    }

    /// Sets the package's input schema.
    pub(crate) fn input(&mut self, schema: InputSchema) {
        self.program.input = Some(schema);
    }

    /// Puts the systems in run order: those in `order` by their place in it,
    /// then the rest as registered.
    pub(crate) fn order_systems(&mut self, order: &[SymbolId]) {
        let rank = |s: &SystemLayout| order.iter().position(|&o| o == s.symbol);
        self.program
            .systems
            .sort_by_key(|s| rank(s).unwrap_or(usize::MAX));
    }

    /// The state slot of the component registered last that holds the `env`
    /// field `field` its view reads, allocated on first read.
    pub(crate) fn env_slot(&mut self, field: EnvField) -> Option<u32> {
        let layout = self.program.components.last_mut()?;
        if let Some(slot) = layout.env_slot(0, field) {
            return Some(slot);
        }
        let slot = layout.states.len() as u32;
        layout.states.push(env_state(field));
        layout.state_inits.push(None);
        layout.env.push(EnvSlot {
            slot,
            field,
            instance: 0,
        });
        Some(slot)
    }

    /// Records `func` as the handler of the `on` item at `at` in the view of
    /// the component registered last.
    pub(crate) fn handler(&mut self, at: TextRange, func: FuncId) {
        if let Some(layout) = self.program.components.last_mut() {
            layout.handlers.push((Site::own(at), func));
        }
    }

    /// Records an effect of the component registered last: `deps`, lowered
    /// from the dependency list at `deps_at`, and `body`, from the body at
    /// `body_at`, join its handler table at those sites.
    pub(crate) fn effect(
        &mut self,
        deps: Option<(TextRange, FuncId)>,
        body: (TextRange, FuncId),
        run: viso_behavior::EffectRun,
    ) {
        let Some(layout) = self.program.components.last_mut() else {
            return;
        };
        let mut entry = |(at, func): (TextRange, FuncId)| {
            layout.handlers.push((Site::own(at), func));
            layout.handlers.len() as u32 - 1
        };
        let deps = deps.map(&mut entry);
        let body = entry(body);
        layout.effects.push(EffectEntry {
            instance: 0,
            deps,
            body,
            run,
            resource: None,
        });
    }

    /// Records the resource `resource`, named `name`, of the component
    /// registered last: its slot starts `ResourceState::idle`, and an effect
    /// gated on `key` (a region entry) runs `load` (an effect body starting
    /// the loader) under `load`'s policies, with an entry reading the slot
    /// and a handler writing it. The four entries join the handler table at
    /// `sites`: the key's, the load's, and two more no view item has.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resource(
        &mut self,
        resource: SymbolId,
        name: &str,
        module: usize,
        sites: [TextRange; 4],
        key: FuncId,
        load: FuncId,
        policies: viso_behavior::ResourceLoad,
    ) {
        let Some(&(layout, Place::State(slot))) = self.places.get(&resource) else {
            return;
        };
        let at = sites[2];
        let mut func = |kind, params, insts: Vec<Inst>| {
            let spans = vec![at; insts.len()];
            self.push(Function {
                name: name.to_owned(),
                kind,
                symbol: None,
                module,
                params,
                captures: Vec::new(),
                body: Ok(Body {
                    regs: 2,
                    insts,
                    spans,
                }),
            })
        };
        let (r0, r1) = (Reg(0), Reg(1));
        let init = func(
            FunctionKind::StateInit,
            0,
            vec![
                Inst::Const {
                    dst: r0,
                    value: Const::Tag(0),
                },
                Inst::Return { src: r0 },
            ],
        );
        let read = func(
            FunctionKind::RegionEntry,
            0,
            vec![Inst::LoadState { dst: r0, slot }, Inst::Return { src: r0 }],
        );
        let write = func(
            FunctionKind::Handler,
            1,
            vec![
                Inst::StoreState { slot, src: r0 },
                Inst::Const {
                    dst: r1,
                    value: Const::Unit,
                },
                Inst::Return { src: r1 },
            ],
        );
        let layout = &mut self.program.components[layout];
        layout.state_inits[slot as usize] = Some(init);
        let mut entry = |at: TextRange, func: FuncId| {
            layout.handlers.push((Site::own(at), func));
            layout.handlers.len() as u32 - 1
        };
        let deps = entry(sites[0], key);
        let body = entry(sites[1], load);
        let state = entry(sites[2], read);
        let write = entry(sites[3], write);
        layout.effects.push(EffectEntry {
            instance: 0,
            deps: Some(deps),
            body,
            run: viso_behavior::EffectRun::MountAndChange,
            resource: Some(viso_behavior::ResourceLoad {
                state,
                write,
                ..policies
            }),
        });
    }

    /// Records `func` as the initializer of the state `state`.
    pub(crate) fn state_init(&mut self, state: SymbolId, func: FuncId) {
        if let Some(&(layout, Place::State(slot))) = self.places.get(&state) {
            self.program.components[layout].state_inits[slot as usize] = Some(func);
        }
    }

    /// Records `func` as the default of the input `input`.
    pub(crate) fn input_default(&mut self, input: SymbolId, func: FuncId) {
        if let Some(&(layout, Place::Input(slot))) = self.places.get(&input) {
            self.program.components[layout].input_defaults[slot as usize] = Some(func);
        }
    }

    /// The function computing the default of field `index` of the record
    /// `record`, reserving a placeholder named `name` until it is lowered.
    pub(crate) fn field_default_slot(
        &mut self,
        record: SymbolId,
        index: u32,
        name: &str,
    ) -> FuncId {
        if let Some(id) = self.field_defaults.get(&(record, index)) {
            return *id;
        }
        let id = self.push(Function {
            name: name.to_string(),
            kind: FunctionKind::FieldDefault,
            symbol: Some(record),
            module: 0,
            params: 0,
            captures: Vec::new(),
            body: Err(Unsupported {
                reason: NO_BODY.to_string(),
                at: TextRange::empty(0.into()),
            }),
        });
        self.field_defaults.insert((record, index), id);
        id
    }

    /// The field defaults a record literal reserved that nothing has lowered.
    pub(crate) fn unlowered_defaults(&self) -> Vec<(SymbolId, u32)> {
        let unlowered = |id: &FuncId| {
            matches!(
                &self.program.functions[id.0 as usize].body,
                Err(unsupported) if unsupported.reason == NO_BODY
            )
        };
        let mut keys: Vec<_> = self
            .field_defaults
            .iter()
            .filter(|(_, id)| unlowered(id))
            .map(|(&key, _)| key)
            .collect();
        keys.sort_unstable();
        keys
    }

    /// The function computing the default of field `index` of `record`, if
    /// one is reserved.
    pub(crate) fn field_default(&self, record: SymbolId, index: u32) -> Option<FuncId> {
        self.field_defaults.get(&(record, index)).copied()
    }

    /// Clears the source spans of the body of `id`, lowered from source no
    /// module of the package holds.
    pub(crate) fn unspan(&mut self, id: FuncId) {
        if let Ok(body) = &mut self.program.functions[id.0 as usize].body {
            body.spans.fill(TextRange::empty(0.into()));
        }
    }

    /// The import index of the native function `entry`, adding the import on
    /// its first call.
    fn native(&mut self, entry: &NativeEntry) -> u32 {
        *self.natives.entry(entry.id).or_insert_with(|| {
            let imports = &mut self.program.natives;
            imports.push(NativeImport {
                path: entry.path.clone(),
                signature: entry.function.signature(),
                params: entry.function.params.len() as u16,
            });
            (imports.len() - 1) as u32
        })
    }

    /// The finished program. A function that calls, or closes over, one that
    /// cannot run cannot run either.
    pub(crate) fn finish(mut self) -> Program {
        block_unsupported(&mut self.program);
        let mut defaults: Vec<_> = self.field_defaults.into_iter().collect();
        defaults.sort_unstable_by_key(|&(key, _)| key);
        self.program.field_defaults = defaults;
        self.program
    }
}

/// Makes every function of `program` that calls or closes over one with no
/// body to run have none either, naming the callee and its reason.
pub(crate) fn block_unsupported(program: &mut Program) {
    loop {
        let mut changed = false;
        for i in 0..program.functions.len() {
            let Ok(body) = &program.functions[i].body else {
                continue;
            };
            let blocked = body.insts.iter().zip(&body.spans).find_map(|(inst, at)| {
                let callee = match inst {
                    Inst::Call { func, .. } | Inst::Closure { func, .. } => *func,
                    _ => return None,
                };
                let callee = &program.functions[callee.0 as usize];
                let reason = callee.body.as_ref().err()?;
                Some(Unsupported {
                    reason: format!(
                        "calls `{}`, which cannot run: {}",
                        callee.name, reason.reason
                    ),
                    at: *at,
                })
            });
            if let Some(blocked) = blocked {
                program.functions[i].body = Err(blocked);
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// Lowers a callable body. `params` are the parameter name spans, in order;
/// `returns_value` is whether the callable declares a return type.
pub(crate) fn lower_body(
    b: &mut ProgramBuilder,
    cx: &InferCx<'_>,
    def: Def,
    params: &[TextRange],
    body: &Block,
    returns_value: bool,
) -> FuncId {
    let mut l = Lowerer::new(cx, b, &def, body.syntax().text_range());
    let result = (|| {
        for at in params {
            let reg = l.reg();
            if let Some(Resolution::Local(slot)) = cx.resolution_at(*at) {
                l.frame().locals.insert(slot, reg);
            }
        }
        let value = l.block(body.syntax(), returns_value)?;
        let src = match value {
            Some(value) => value,
            None => l.unit(),
        };
        l.emit(Inst::Return { src });
        Ok(())
    })();
    let body = result.map(|()| l.frames.pop().unwrap_or_default().body);
    b.define(
        Function {
            name: def.name,
            kind: def.kind,
            symbol: def.symbol,
            module: def.module,
            params: params.len() as u32,
            captures: Vec::new(),
            body,
        },
        def.into,
    )
}

/// Lowers a value expression (a computed value, a state initializer, an input
/// or field default, a constant) to a function of no arguments returning it.
pub(crate) fn lower_value(
    b: &mut ProgramBuilder,
    cx: &InferCx<'_>,
    def: Def,
    value: &Expr,
) -> FuncId {
    let mut l = Lowerer::new(cx, b, &def, value.syntax().text_range());
    let result = l.expr(value).map(|src| {
        l.emit(Inst::Return { src });
    });
    let body = result.map(|()| l.frames.pop().unwrap_or_default().body);
    b.define(
        Function {
            name: def.name,
            kind: def.kind,
            symbol: def.symbol,
            module: def.module,
            params: 0,
            captures: Vec::new(),
            body,
        },
        def.into,
    )
}

/// Lowers a view event handler body. The event payload arrives in `r0` and is
/// destructured by `payload` (its pattern and type); the values of the
/// enclosing view regions the handler sits in arrive in the registers after
/// it, each destructured by its `scope` pattern.
pub(crate) fn lower_handler(
    b: &mut ProgramBuilder,
    cx: &InferCx<'_>,
    def: Def,
    payload: Option<(&SyntaxNode, &Ty)>,
    scope: &[(SyntaxNode, Ty)],
    body: &Block,
) -> FuncId {
    let mut l = Lowerer::new(cx, b, &def, body.syntax().text_range());
    let result = (|| {
        let event = l.reg();
        let regions: Vec<Reg> = scope.iter().map(|_| l.reg()).collect();
        if let Some((pattern, ty)) = payload {
            l.destructure(pattern, event, ty)?;
        }
        for ((pattern, ty), src) in scope.iter().zip(regions) {
            l.bind_matched(pattern, src, ty)?;
        }
        l.block(body.syntax(), false)?;
        let src = l.unit();
        l.emit(Inst::Return { src });
        Ok(())
    })();
    let body = result.map(|()| l.frames.pop().unwrap_or_default().body);
    b.define(
        Function {
            name: def.name,
            kind: def.kind,
            symbol: def.symbol,
            module: def.module,
            params: 1 + scope.len() as u32,
            captures: Vec::new(),
            body,
        },
        def.into,
    )
}

/// Lowers an effect's dependency list to a function returning the list of the
/// dependency values.
pub(crate) fn lower_effect_deps(
    b: &mut ProgramBuilder,
    cx: &InferCx<'_>,
    def: Def,
    deps: &[Expr],
    at: TextRange,
) -> FuncId {
    let mut l = Lowerer::new(cx, b, &def, at);
    let result = (|| {
        let mut items = Vec::with_capacity(deps.len());
        for dep in deps {
            items.push(l.expr(dep)?);
        }
        let dst = l.reg();
        l.emit(Inst::List { dst, items });
        l.emit(Inst::Return { src: dst });
        Ok(())
    })();
    let body = result.map(|()| l.frames.pop().unwrap_or_default().body);
    b.define(
        Function {
            name: def.name,
            kind: def.kind,
            symbol: def.symbol,
            module: def.module,
            params: 0,
            captures: Vec::new(),
            body,
        },
        def.into,
    )
}

/// Lowers an effect body to a function returning its `cleanup` as a closure,
/// or unit without one.
pub(crate) fn lower_effect_body(
    b: &mut ProgramBuilder,
    cx: &InferCx<'_>,
    def: Def,
    body: &crate::ast::EffectBody,
) -> FuncId {
    let mut l = Lowerer::new(cx, b, &def, body.syntax().text_range());
    let result = (|| {
        l.block(body.syntax(), false)?;
        let src = match body.cleanup().and_then(|c| c.block()) {
            Some(block) => l.cleanup_closure(block.syntax())?,
            None => l.unit(),
        };
        l.emit(Inst::Return { src });
        Ok(())
    })();
    let body = result.map(|()| l.frames.pop().unwrap_or_default().body);
    b.define(
        Function {
            name: def.name,
            kind: def.kind,
            symbol: def.symbol,
            module: def.module,
            params: 0,
            captures: Vec::new(),
            body,
        },
        def.into,
    )
}

/// Lowers a resource's `load` task call to an effect body that starts it
/// without handlers: the view host takes the start and settles the
/// resource's state with its result.
pub(crate) fn lower_resource_load(
    b: &mut ProgramBuilder,
    cx: &InferCx<'_>,
    def: Def,
    load: &Expr,
) -> FuncId {
    let mut l = Lowerer::new(cx, b, &def, load.syntax().text_range());
    let result = (|| {
        let (task, args) = l.task_call(load)?;
        l.emit(Inst::Start {
            task,
            args,
            done: None,
            cancelled: None,
            instance: 0,
            slot: None,
            policy: TaskPolicy::KeepLatest,
        });
        let src = l.unit();
        l.emit(Inst::Return { src });
        Ok(())
    })();
    let body = result.map(|()| l.frames.pop().unwrap_or_default().body);
    b.define(
        Function {
            name: def.name,
            kind: def.kind,
            symbol: def.symbol,
            module: def.module,
            params: 0,
            captures: Vec::new(),
            body,
        },
        def.into,
    )
}

/// What a view region's entry function computes from the enclosing regions'
/// scope values.
pub(crate) enum RegionEntry<'e> {
    /// A conditional's arm choice: the index of the first arm whose condition
    /// holds (an `else` arm, `None`, always holds), or `-1`.
    Conditions(&'e [Option<Expr>]),
    /// A `match` region's scrutinee.
    Value(&'e Expr),
    /// A `for` region's iterable: a list, or an integer range tagged
    /// [`viso_view::HALF_OPEN`] or [`viso_view::CLOSED`].
    Items(&'e Expr),
    /// A `for` item's key: the item arrives after the scope and is bound by
    /// `pattern` (of type `element`) before `key` runs.
    Key {
        /// The `for` pattern.
        pattern: &'e SyntaxNode,
        /// The item type.
        element: &'e Ty,
        /// The key expression.
        key: &'e Expr,
    },
    /// A `match` region's arm choice: the scrutinee (of type `ty`) arrives after
    /// the scope; the index of the first arm whose pattern and guard pass, or `-1`.
    Arms {
        /// Each arm's pattern and guard.
        arms: &'e [(SyntaxNode, Option<Expr>)],
        /// The scrutinee type.
        ty: &'e Ty,
    },
    /// The current value of a `bind` source, the value its two-way input reads.
    Lens(&'e AssignablePath),
}

impl RegionEntry<'_> {
    /// Whether the entry takes a value after the scope (a `for` item, a
    /// scrutinee).
    pub(crate) fn takes_subject(&self) -> bool {
        matches!(self, RegionEntry::Key { .. } | RegionEntry::Arms { .. })
    }
}

/// Lowers a view region's entry: a pure function of the enclosing regions'
/// values `scope` (each bound by its pattern), plus the entry's subject when it
/// [takes one](RegionEntry::takes_subject).
pub(crate) fn lower_region_entry(
    b: &mut ProgramBuilder,
    cx: &InferCx<'_>,
    def: Def,
    scope: &[(SyntaxNode, Ty)],
    entry: &RegionEntry<'_>,
    at: TextRange,
) -> FuncId {
    let mut l = Lowerer::new(cx, b, &def, at);
    let result = (|| {
        let regions: Vec<Reg> = scope.iter().map(|_| l.reg()).collect();
        let subject = if entry.takes_subject() {
            Some(l.reg())
        } else {
            None
        };
        for ((pattern, ty), src) in scope.iter().zip(regions) {
            l.bind_matched(pattern, src, ty)?;
        }
        let chosen = |l: &mut Lowerer<'_, '_>, index: usize| {
            let src = l.constant(Const::Int(index as i128));
            l.emit(Inst::Return { src });
        };
        match entry {
            RegionEntry::Conditions(conditions) => {
                for (index, condition) in conditions.iter().enumerate() {
                    match condition {
                        Some(condition) => {
                            let range = condition.syntax().text_range();
                            l.at(range, |l| -> Lower<()> {
                                let holds = l.expr(condition)?;
                                let skip = l.jump_if(holds, false);
                                chosen(l, index);
                                l.patch_here(&[skip]);
                                Ok(())
                            })?;
                        }
                        None => chosen(&mut l, index),
                    }
                }
                let src = l.constant(Const::Int(-1));
                l.emit(Inst::Return { src });
            }
            RegionEntry::Value(value) => {
                let src = l.expr(value)?;
                l.emit(Inst::Return { src });
            }
            RegionEntry::Lens(source) => {
                let (slot, path) = l.lens(source)?;
                let src = l.read_state(slot, &path);
                l.emit(Inst::Return { src });
            }
            RegionEntry::Items(items) => {
                let mut src = l.expr(items)?;
                if let Ty::Range(element) | Ty::RangeInclusive(element) = l.ty(items)? {
                    if num_of(&element).is_none_or(|num| num.is_float()) {
                        return l.bail("only an integer range can be iterated");
                    }
                    if matches!(l.ty(items)?, Ty::RangeInclusive(_)) {
                        let bounds = [0, 1].map(|index| {
                            let dst = l.reg();
                            l.emit(Inst::Field { dst, src, index });
                            dst
                        });
                        src = l.reg();
                        l.emit(Inst::Make {
                            dst: src,
                            tag: viso_view::CLOSED,
                            fields: bounds.to_vec(),
                        });
                    }
                }
                l.emit(Inst::Return { src });
            }
            RegionEntry::Key {
                pattern,
                element,
                key,
            } => {
                let item = subject.expect("a key entry takes its item");
                l.bind_matched(pattern, item, element)?;
                let src = l.expr(key)?;
                l.emit(Inst::Return { src });
            }
            RegionEntry::Arms { arms, ty } => {
                let src = subject.expect("an arm choice takes its scrutinee");
                for (index, (pattern, guard)) in arms.iter().enumerate() {
                    l.at(pattern.text_range(), |l| -> Lower<()> {
                        let mut fails = Vec::new();
                        l.test(pattern, src, ty, &mut fails)?;
                        if let Some(guard) = guard {
                            let pass = l.expr(guard)?;
                            fails.push(l.jump_if(pass, false));
                        }
                        chosen(l, index);
                        l.patch_here(&fails);
                        Ok(())
                    })?;
                }
                let src = l.constant(Const::Int(-1));
                l.emit(Inst::Return { src });
            }
        }
        Ok(())
    })();
    let body = result.map(|()| l.frames.pop().unwrap_or_default().body);
    let params = scope.len() as u32 + u32::from(entry.takes_subject());
    b.define(
        Function {
            name: def.name,
            kind: def.kind,
            symbol: def.symbol,
            module: def.module,
            params,
            captures: Vec::new(),
            body,
        },
        def.into,
    )
}

/// Lowers the write-back of a `bind` to a two-way component input: a handler
/// of the event the input is paired with, writing the event's first parameter
/// to `source`. The payload arrives in `r0`, the enclosing regions' values
/// `scope` after it.
pub(crate) fn lower_write_back(
    b: &mut ProgramBuilder,
    cx: &InferCx<'_>,
    def: Def,
    scope: &[(SyntaxNode, Ty)],
    source: &AssignablePath,
    at: TextRange,
) -> FuncId {
    let mut l = Lowerer::new(cx, b, &def, at);
    let result = (|| {
        let event = l.reg();
        let regions: Vec<Reg> = scope.iter().map(|_| l.reg()).collect();
        for ((pattern, ty), src) in scope.iter().zip(regions) {
            l.bind_matched(pattern, src, ty)?;
        }
        let (slot, path) = l.lens(source)?;
        let value = l.reg();
        l.emit(Inst::Field {
            dst: value,
            src: event,
            index: 0,
        });
        l.write_state(slot, path, value);
        let src = l.unit();
        l.emit(Inst::Return { src });
        Ok(())
    })();
    let body = result.map(|()| l.frames.pop().unwrap_or_default().body);
    b.define(
        Function {
            name: def.name,
            kind: def.kind,
            symbol: def.symbol,
            module: def.module,
            params: 1 + scope.len() as u32,
            captures: Vec::new(),
            body,
        },
        def.into,
    )
}

/// Adds a function that cannot run, for `reason` at `at`.
pub(crate) fn unsupported(b: &mut ProgramBuilder, def: Def, reason: &str, at: TextRange) -> FuncId {
    unsupported_with(b, def, 0, reason, at)
}

/// Adds a function of `params` arguments that cannot run, for `reason` at `at`.
pub(crate) fn unsupported_with(
    b: &mut ProgramBuilder,
    def: Def,
    params: u32,
    reason: &str,
    at: TextRange,
) -> FuncId {
    b.define(
        Function {
            name: def.name,
            kind: def.kind,
            symbol: def.symbol,
            module: def.module,
            params,
            captures: Vec::new(),
            body: Err(Unsupported {
                reason: reason.to_string(),
                at,
            }),
        },
        def.into,
    )
}

/// One enclosing loop: the jumps its `break`s and `continue`s patch.
#[derive(Debug, Default)]
struct LoopCx {
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

/// The function being lowered: the outermost one, or a closure inside it.
#[derive(Debug, Default)]
struct Frame {
    body: Body,
    /// The register of each local binding visible here.
    locals: HashMap<LocalSlot, Reg>,
    /// The locals this closure captures, with the registers they arrive in.
    captures: Vec<(LocalSlot, Reg)>,
    /// The captured locals, which the closure may not assign.
    captured: HashSet<LocalSlot>,
    /// The enclosing loops, innermost last.
    loops: Vec<LoopCx>,
}

/// Lowers one function and the closures inside it.
struct Lowerer<'l, 'a> {
    cx: &'l InferCx<'a>,
    env: &'a dyn TypeEnv,
    b: &'l mut ProgramBuilder,
    /// The name of the function being lowered, for naming its closures.
    name: String,
    module: usize,
    /// The frames being lowered, the innermost closure last.
    frames: Vec<Frame>,
    /// The span instructions are recorded at.
    at: TextRange,
}

impl<'l, 'a> Lowerer<'l, 'a> {
    fn new(cx: &'l InferCx<'a>, b: &'l mut ProgramBuilder, def: &Def, at: TextRange) -> Self {
        Self {
            cx,
            env: cx.env(),
            b,
            name: def.name.clone(),
            module: def.module,
            frames: vec![Frame::default()],
            at,
        }
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a frame is being lowered")
    }

    /// A fresh register.
    fn reg(&mut self) -> Reg {
        let body = &mut self.frame().body;
        body.regs += 1;
        Reg(body.regs - 1)
    }

    /// Appends `inst` at the current span, returning its index.
    fn emit(&mut self, inst: Inst) -> usize {
        let at = self.at;
        let body = &mut self.frame().body;
        body.insts.push(inst);
        body.spans.push(at);
        body.insts.len() - 1
    }

    /// The index the next instruction takes.
    fn here(&mut self) -> u32 {
        self.frame().body.insts.len() as u32
    }

    /// Points the jump at `at` to `target`.
    fn patch(&mut self, at: usize, target: u32) {
        match &mut self.frame().body.insts[at] {
            Inst::Jump { target: t } | Inst::JumpIf { target: t, .. } => *t = target,
            _ => unreachable!("only jumps are patched"),
        }
    }

    /// Points every jump in `jumps` to the next instruction.
    fn patch_here(&mut self, jumps: &[usize]) {
        let here = self.here();
        for at in jumps {
            self.patch(*at, here);
        }
    }

    /// `dst = value` into a fresh register.
    fn constant(&mut self, value: Const) -> Reg {
        let dst = self.reg();
        self.emit(Inst::Const { dst, value });
        dst
    }

    fn unit(&mut self) -> Reg {
        self.constant(Const::Unit)
    }

    /// A copy of `src` in a fresh register.
    fn copy(&mut self, src: Reg) -> Reg {
        let dst = self.reg();
        self.emit(Inst::Move { dst, src });
        dst
    }

    /// A jump to be patched.
    fn jump(&mut self) -> usize {
        self.emit(Inst::Jump { target: 0 })
    }

    /// A conditional jump, taken when `cond` is `when`, to be patched.
    fn jump_if(&mut self, cond: Reg, when: bool) -> usize {
        self.emit(Inst::JumpIf {
            cond,
            when,
            target: 0,
        })
    }

    fn bail<T>(&self, reason: impl Into<String>) -> Lower<T> {
        Err(Unsupported {
            reason: reason.into(),
            at: self.at,
        })
    }

    /// Runs `f` with instructions recorded at `at`.
    fn at<T>(&mut self, at: TextRange, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = std::mem::replace(&mut self.at, at);
        let out = f(self);
        self.at = saved;
        out
    }

    /// Whether `reg` holds a local binding (so a later write to the local would
    /// change it).
    fn is_local(&mut self, reg: Reg) -> bool {
        self.frame().locals.values().any(|r| *r == reg)
    }

    /// The register of the local `slot`, capturing it from an enclosing
    /// function when this is a closure.
    fn local(&mut self, slot: LocalSlot) -> Lower<Reg> {
        if let Some(reg) = self.frame().locals.get(&slot) {
            return Ok(*reg);
        }
        if self.frames.len() == 1 {
            return self.bail("a local is used before it is bound");
        }
        let reg = self.reg();
        let frame = self.frame();
        frame.locals.insert(slot, reg);
        frame.captures.push((slot, reg));
        frame.captured.insert(slot);
        Ok(reg)
    }

    /// Binds the local `slot` to the value in `src`. An `owned` source is a
    /// temporary nothing else reads, which the binding takes over.
    fn bind(&mut self, slot: LocalSlot, src: Reg, owned: bool) {
        if let Some(reg) = self.frame().locals.get(&slot).copied() {
            self.emit(Inst::Move { dst: reg, src });
            return;
        }
        let reg = if owned && !self.is_local(src) {
            src
        } else {
            self.copy(src)
        };
        self.frame().locals.insert(slot, reg);
    }

    /// Binds the name token at `at`, when it declares a local.
    fn bind_at(&mut self, at: TextRange, src: Reg, owned: bool) -> Lower<()> {
        match self.cx.resolution_at(at) {
            Some(Resolution::Local(slot)) => {
                self.bind(slot, src, owned);
                Ok(())
            }
            _ => self.bail("this pattern name is not a binding"),
        }
    }

    /// The type inference gave `expr`.
    fn ty(&self, expr: &Expr) -> Lower<Ty> {
        match self.cx.type_of(expr) {
            Some(ty) => {
                self.check_repr(ty)?;
                Ok(ty.clone())
            }
            None => self.bail("this expression has no inferred type"),
        }
    }

    /// Fails for a type the IR has no representation for. An undetermined type
    /// fails only at the top: the element type of an empty list or an absent
    /// option never reaches an operation.
    fn check_repr(&self, ty: &Ty) -> Lower<()> {
        match ty {
            Ty::Unknown => self.bail("this expression has an undetermined type"),
            other => self.check_parts(other),
        }
    }

    fn check_parts(&self, ty: &Ty) -> Lower<()> {
        match ty {
            Ty::MixedLength => self.bail("a mixed-unit length resolves only at layout"),
            Ty::Bytes => self.bail("`Bytes` values are not supported yet"),
            Ty::Option(inner) if matches!(**inner, Ty::Option(_)) => {
                self.bail("a nested `Option` has no runtime representation")
            }
            Ty::Option(t) | Ty::List(t) | Ty::Range(t) | Ty::RangeInclusive(t) => {
                self.check_parts(t)
            }
            Ty::Result(a, b) | Ty::Resource(a, b) | Ty::ResourceState(a, b) => {
                self.check_parts(a)?;
                self.check_parts(b)
            }
            Ty::Tuple(ts) => ts.iter().try_for_each(|t| self.check_parts(t)),
            Ty::Fn(ps, r) => {
                ps.iter().try_for_each(|t| self.check_parts(t))?;
                self.check_parts(r)
            }
            _ => Ok(()),
        }
    }

    /// Lowers `exprs` left to right. A local's register is copied when a later
    /// operand assigns to a local, so each operand keeps the value it had when
    /// evaluated.
    fn operands(&mut self, exprs: &[Expr]) -> Lower<Vec<Reg>> {
        let mut regs = Vec::with_capacity(exprs.len());
        for (i, e) in exprs.iter().enumerate() {
            let mut reg = self.expr(e)?;
            if self.is_local(reg) && exprs[i + 1..].iter().any(|l| assigns(l.syntax())) {
                reg = self.copy(reg);
            }
            regs.push(reg);
        }
        Ok(regs)
    }
}

/// Whether evaluating `node` may assign a local.
pub(super) fn assigns(node: &SyntaxNode) -> bool {
    node.descendants()
        .iter()
        .any(|n| n.kind() == SyntaxKind::AssignStmt)
}

/// The machine type values of `ty` compute at, if it is numeric.
fn num_of(ty: &Ty) -> Option<Num> {
    Some(match ty {
        Ty::I8 => Num::I8,
        Ty::I16 => Num::I16,
        Ty::I32 => Num::I32,
        Ty::I64 | Ty::InferInt => Num::I64,
        Ty::U8 => Num::U8,
        Ty::U16 => Num::U16,
        Ty::U32 => Num::U32,
        Ty::U64 => Num::U64,
        Ty::F32 => Num::F32,
        Ty::F64 | Ty::InferFloat => Num::F64,
        t if t.is_dimensional() => Num::F64,
        _ => return None,
    })
}
