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

use viso_behavior::native::{NativeEntry, NativeId};

use super::ir::{
    Body, ComponentLayout, Const, FuncId, Function, FunctionKind, Inst, NativeImport, Num, Program,
    Reg, Site, Unsupported,
};
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

    /// Adds `function`, filling the placeholder `into` or the one of its
    /// declaration if one was reserved.
    fn define(&mut self, function: Function, into: Option<FuncId>) -> FuncId {
        if let Some(id) = into {
            self.program.functions[id.0 as usize] = function;
            return id;
        }
        let named = matches!(
            function.kind,
            FunctionKind::Fn | FunctionKind::Action | FunctionKind::Computed | FunctionKind::Const
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
        });
    }

    /// Records `func` as the handler of the `on` item at `at` in the view of
    /// the component registered last.
    pub(crate) fn handler(&mut self, at: TextRange, func: FuncId) {
        if let Some(layout) = self.program.components.last_mut() {
            layout.handlers.push((Site::own(at), func));
        }
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
            Ty::Result(a, b) => {
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
fn assigns(node: &SyntaxNode) -> bool {
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
