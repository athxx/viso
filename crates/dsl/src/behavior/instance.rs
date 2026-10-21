//! Inlining the user-component instances a mounted view places into the
//! mounted component's program.
//!
//! A user-component node does not mount a component of its own at run time:
//! the component's view mounts in its place, as part of the mounted view, and
//! the component's functions run against the mounted component. Each instance
//! therefore gets its own copy of every function of the component that touches
//! per-instance data — a state, an input or an event — rewritten for the
//! instance:
//!
//! - its states are hidden states of the mounted component, appended to its
//!   layout and named by the instance's identity ([`hidden_state`]); an
//!   instance a control-flow region mounts starts them from entries the
//!   region content runs when it mounts the instance ([`RegionalStates`]);
//! - an input read calls the caller's argument entry for that input (else the
//!   component's default, else `None`); one the caller's styles give calls the
//!   instance's function choosing among the styles' values by the component's
//!   `@selector` members;
//! - an `emit` calls each handler the caller wires to the event, with the
//!   event's arguments as its payload;
//! - every copy takes the scope values of the regions enclosing the instance's
//!   node ahead of its own, and passes them on to the copies it calls.
//!
//! A function touching none of these is shared by every instance. The copies of
//! the component's view functions are registered in the mounted layout at the
//! instance's [`Site`]s, where the mounted view's nodes and regions look them
//! up.

use std::collections::HashMap;

use super::ir::{
    BinaryOp, Body, ComponentLayout, Const, EffectEntry, EnvSlot, FuncId, Function, FunctionKind,
    Inst, Num, PathStep, Program, Reg, RegionalStates, Site, UnaryOp, Unsupported,
};
use super::lower::{add_game_slot, block_unsupported};
use crate::ir::{UiInstance, UiMemberWhen, UiStyledInput, UiTree};
use crate::resolve::SymbolId;
use crate::syntax::{TextRange, TextSize};

/// The name of the hidden state of the mounted component that keeps the state
/// `state` of the inlined instance `identity`.
pub(crate) fn hidden_state(identity: &str, state: &str) -> String {
    format!("{identity}.{state}")
}

/// Inlines every instance of `tree` into the layout of the component `root`
/// in `program`, in pre-order so a caller's copies exist before its callees
/// look them up. Instances of a component `program` has no layout for are
/// skipped.
pub(crate) fn inline_instances(program: &mut Program, root: SymbolId, tree: &UiTree) {
    if tree.instances.is_empty() {
        return;
    }
    let Some(root) = program.components.iter().position(|c| c.symbol == root) else {
        return;
    };
    let dependent = dependent_functions(program);
    for (index, instance) in tree.instances.iter().enumerate() {
        let Some(child) = program
            .components
            .iter()
            .find(|c| c.symbol == instance.component)
            .cloned()
        else {
            continue;
        };
        Inliner {
            program: &mut *program,
            root,
            dependent: &dependent,
            child: &child,
            instance,
            id: index as u32 + 1,
            clones: HashMap::new(),
            styled: HashMap::new(),
            base: 0,
            game: Vec::new(),
        }
        .run();
    }
    block_unsupported(program);
}

/// Which functions of `program` touch per-instance data: a state, an input or
/// an event, directly or through a function they call or close over.
fn dependent_functions(program: &Program) -> Vec<bool> {
    let touches = |inst: &Inst| {
        matches!(
            inst,
            Inst::LoadState { .. }
                | Inst::StoreState { .. }
                | Inst::LoadInput { .. }
                | Inst::Emit { .. }
                | Inst::Start { .. }
                | Inst::NodeRef { .. }
        )
    };
    let mut dependent: Vec<bool> = program
        .functions
        .iter()
        .map(|f| f.body.as_ref().is_ok_and(|b| b.insts.iter().any(touches)))
        .collect();
    loop {
        let mut changed = false;
        for (i, function) in program.functions.iter().enumerate() {
            if dependent[i] {
                continue;
            }
            let Ok(body) = &function.body else { continue };
            if body
                .insts
                .iter()
                .any(|inst| callee(inst).is_some_and(|f| dependent[f.0 as usize]))
            {
                dependent[i] = true;
                changed = true;
            }
        }
        if !changed {
            return dependent;
        }
    }
}

/// The function `inst` calls, closes over or starts.
fn callee(inst: &Inst) -> Option<FuncId> {
    match inst {
        Inst::Call { func, .. } | Inst::Closure { func, .. } | Inst::Start { task: func, .. } => {
            Some(*func)
        }
        _ => None,
    }
}

/// Inlines one instance.
struct Inliner<'p> {
    program: &'p mut Program,
    /// The mounted component's layout index.
    root: usize,
    dependent: &'p [bool],
    /// The instance's component layout.
    child: &'p ComponentLayout,
    instance: &'p UiInstance,
    /// The instance's number.
    id: u32,
    /// The copy of each function the instance runs its own copy of.
    clones: HashMap<FuncId, FuncId>,
    /// The function giving each input the caller's styles give, by slot.
    styled: HashMap<u32, FuncId>,
    /// The mounted slot of the instance's first state.
    base: u32,
    /// Each game slot of the instance's component and the mounted
    /// component's slot for the same query.
    game: Vec<(u32, u32)>,
}

impl Inliner<'_> {
    /// The mounted slot of the instance's state slot `slot`: the view's own
    /// for a game query, else the instance's hidden state.
    fn state_slot(&self, slot: u32) -> u32 {
        match self.game.iter().find(|&&(own, _)| own == slot) {
            Some(&(_, mounted)) => mounted,
            None => self.base + slot,
        }
    }

    fn run(mut self) {
        let depth = self.instance.depth;
        // The members a style's selector reads run as the instance's too.
        let selected: Vec<FuncId> = self
            .instance
            .styled
            .iter()
            .flat_map(|s| &s.arms)
            .flat_map(|a| &a.when)
            .filter_map(|step| match step {
                UiMemberWhen::Member(name) => self.child.member(name),
                _ => None,
            })
            .collect();
        let roots: Vec<FuncId> = self
            .child
            .handlers
            .iter()
            .map(|(_, f)| *f)
            .chain(self.child.state_inits.iter().flatten().copied())
            .chain(self.child.input_defaults.iter().flatten().copied())
            .chain(selected)
            .collect();
        // Every function reachable from the instance's entry points that
        // touches per-instance data, and every entry point that takes the
        // enclosing scope values, runs as the instance's own copy.
        let mut order = Vec::new();
        let mut stack: Vec<FuncId> = roots
            .iter()
            .copied()
            .filter(|f| depth > 0 || self.dependent[f.0 as usize])
            .collect();
        let next = self.program.functions.len() as u32;
        while let Some(f) = stack.pop() {
            if self.clones.contains_key(&f) {
                continue;
            }
            self.clones.insert(f, FuncId(next + order.len() as u32));
            order.push(f);
            if let Ok(body) = &self.program.function(f).body {
                stack.extend(
                    body.insts
                        .iter()
                        .filter_map(callee)
                        .filter(|g| self.dependent[g.0 as usize]),
                );
            }
        }

        let layout = &mut self.program.components[self.root];
        // The game queries the instance reads are the view's: one slot of the
        // mounted component each, whichever instance reads it.
        self.game = self
            .child
            .game
            .iter()
            .map(|g| (g.slot, add_game_slot(layout, &g.query)))
            .collect();
        self.base = layout.states.len() as u32;
        layout.states.extend(
            self.child
                .states
                .iter()
                .map(|s| hidden_state(&self.instance.identity, s)),
        );

        let first = next + order.len() as u32;
        self.styled = self
            .instance
            .styled
            .iter()
            .enumerate()
            .map(|(k, s)| (s.slot, FuncId(first + k as u32)))
            .collect();
        let copies: Vec<Function> = order.iter().map(|&f| self.copy(f)).collect();
        let styled: Vec<Function> = self
            .instance
            .styled
            .iter()
            .map(|s| self.styled_input(s))
            .collect();
        let starts = copies.iter().any(|f| {
            f.body
                .as_ref()
                .is_ok_and(|b| b.insts.iter().any(|i| matches!(i, Inst::Start { .. })))
        });
        self.program.functions.extend(copies);
        self.program.functions.extend(styled);
        if starts {
            self.program.components[self.root].starters.push(self.id);
        }

        let own = |f: FuncId| self.clones.get(&f).copied().unwrap_or(f);
        let handlers: Vec<(Site, FuncId)> = self
            .child
            .handlers
            .iter()
            .map(|(site, f)| {
                (
                    Site {
                        instance: self.id,
                        ..*site
                    },
                    own(*f),
                )
            })
            .collect();
        let inits: Vec<Option<FuncId>> =
            self.child.state_inits.iter().map(|f| f.map(own)).collect();
        let layout = &mut self.program.components[self.root];
        let start = layout.handlers.len() as u32;
        layout.handlers.extend(handlers);
        layout
            .effects
            .extend(self.child.effects.iter().map(|e| EffectEntry {
                instance: self.id,
                deps: e.deps.map(|d| start + d),
                body: start + e.body,
                run: e.run,
                resource: e.resource.map(|load| viso_behavior::ResourceLoad {
                    state: start + load.state,
                    write: start + load.write,
                    ..load
                }),
            }));
        layout.env.extend(self.child.env.iter().map(|e| EnvSlot {
            slot: self.base + e.slot,
            field: e.field,
            instance: self.id,
        }));
        if !self.instance.regional {
            layout.state_inits.extend(inits);
            return;
        }
        // Region content runs the initializers with its scope values each
        // time it mounts the instance, so they join the handler table at
        // sites no view item has.
        layout.state_inits.extend(inits.iter().map(|_| None));
        let entries = inits
            .into_iter()
            .enumerate()
            .map(|(state, init)| {
                let func = init?;
                let at = TextRange::empty(TextSize::from(state as u32));
                layout.handlers.push((
                    Site {
                        instance: self.id,
                        at,
                        part: 0,
                    },
                    func,
                ));
                Some(layout.handlers.len() as u32 - 1)
            })
            .collect();
        layout.regional.push(RegionalStates {
            instance: self.id,
            base: self.base,
            inits: entries,
        });
    }

    /// The function the caller registered at `at` in the view of the
    /// instance's parent.
    fn caller(&self, at: TextRange) -> Option<FuncId> {
        self.caller_part(at, 0)
    }

    /// The function the caller registered at part `part` of `at` in the view
    /// of the instance's parent.
    fn caller_part(&self, at: TextRange, part: u32) -> Option<FuncId> {
        let site = Site {
            instance: self.instance.parent,
            at,
            part,
        };
        let layout = &self.program.components[self.root];
        layout
            .handlers
            .iter()
            .find(|(s, _)| *s == site)
            .map(|h| h.1)
    }

    /// The instance's copy of `f`.
    fn copy(&self, f: FuncId) -> Function {
        let original = self.program.function(f);
        let depth = self.instance.depth;
        let closure = original.kind == FunctionKind::Closure;
        // Where the scope values arrive: ahead of a handler's and an entry's
        // own scope values, after every other function's parameters, and in
        // new captures of a closure.
        let at = match original.kind {
            FunctionKind::Handler => 1,
            FunctionKind::RegionEntry => 0,
            _ => original.params,
        };
        let mut copy = Function {
            name: format!("{} [{}]", original.name, self.instance.identity),
            kind: original.kind,
            symbol: None,
            module: original.module,
            params: if closure {
                original.params
            } else {
                original.params + depth
            },
            captures: original.captures.clone(),
            body: Err(Unsupported {
                reason: String::new(),
                at: TextRange::empty(TextSize::ZERO),
            }),
        };
        let body = match &original.body {
            Ok(body) => body,
            Err(unsupported) => {
                copy.body = Err(unsupported.clone());
                return copy;
            }
        };
        let shift = |r: Reg| {
            if closure || r.0 < at {
                r
            } else {
                Reg(r.0 + depth)
            }
        };
        let scope: Vec<Reg> = if closure {
            (body.regs..body.regs + depth).map(Reg).collect()
        } else {
            (at..at + depth).map(Reg).collect()
        };
        if closure {
            copy.captures.extend(scope.iter().copied());
        }
        let mut out = Rewrite {
            body: Body {
                regs: body.regs + depth,
                insts: Vec::with_capacity(body.insts.len()),
                spans: Vec::with_capacity(body.spans.len()),
            },
            starts: Vec::with_capacity(body.insts.len() + 1),
        };
        for (inst, &span) in body.insts.iter().zip(&body.spans) {
            out.starts.push(out.body.insts.len() as u32);
            let mut inst = inst.clone();
            regs(&mut inst, &shift);
            if let Err(reason) = self.rewrite(inst, span, &scope, &mut out) {
                copy.body = Err(Unsupported { reason, at: span });
                return copy;
            }
        }
        out.starts.push(out.body.insts.len() as u32);
        let starts = out.starts;
        let target = |t: &mut u32| *t = starts[*t as usize];
        for inst in &mut out.body.insts {
            match inst {
                Inst::Jump { target: t } | Inst::JumpIf { target: t, .. } => target(t),
                Inst::Switch {
                    targets, default, ..
                } => {
                    targets.iter_mut().for_each(target);
                    target(default);
                }
                _ => {}
            }
        }
        copy.body = Ok(out.body);
        copy
    }

    /// Emits the instance's form of `inst`, whose registers are already the
    /// copy's.
    fn rewrite(
        &self,
        inst: Inst,
        span: TextRange,
        scope: &[Reg],
        out: &mut Rewrite,
    ) -> Result<(), String> {
        let with_scope = |args: &[Reg]| -> Vec<Reg> { args.iter().chain(scope).copied().collect() };
        match inst {
            Inst::LoadState { dst, slot } => out.push(
                Inst::LoadState {
                    dst,
                    slot: self.state_slot(slot),
                },
                span,
            ),
            Inst::NodeRef { dst, node } => {
                if self.id >= 1 << 15 || node >= 1 << 16 {
                    return Err("too many named nodes or instances to reach as a `NodeRef`".into());
                }
                out.push(
                    Inst::NodeRef {
                        dst,
                        node: self.id << 16 | node,
                    },
                    span,
                );
            }
            Inst::StoreState { slot, src } => out.push(
                Inst::StoreState {
                    slot: self.base + slot,
                    src,
                },
                span,
            ),
            Inst::LoadInput { dst, slot } => match self.styled.get(&slot) {
                Some(&func) => out.push(
                    Inst::Call {
                        dst,
                        func,
                        args: scope.to_vec(),
                    },
                    span,
                ),
                None => self.load_input(dst, slot, scope, span, out)?,
            },
            Inst::Emit { event, args } => {
                let name = &self.child.events[event as usize];
                let wired: Vec<FuncId> = self
                    .instance
                    .handlers
                    .iter()
                    .filter(|(e, _)| e == name)
                    .map(|&(_, at)| {
                        self.caller(at)
                            .ok_or_else(|| format!("the handler of event `{name}` has no entry"))
                    })
                    .collect::<Result<_, _>>()?;
                if wired.is_empty() {
                    return Ok(());
                }
                let payload = out.reg();
                out.push(
                    Inst::Make {
                        dst: payload,
                        tag: 0,
                        fields: args,
                    },
                    span,
                );
                for func in wired {
                    let dst = out.reg();
                    out.push(
                        Inst::Call {
                            dst,
                            func,
                            args: with_scope(&[payload]),
                        },
                        span,
                    );
                }
            }
            Inst::Call { dst, func, args } => match self.clones.get(&func) {
                Some(&copy) => out.push(
                    Inst::Call {
                        dst,
                        func: copy,
                        args: with_scope(&args),
                    },
                    span,
                ),
                None => out.push(Inst::Call { dst, func, args }, span),
            },
            Inst::Closure {
                dst,
                func,
                captures,
            } => match self.clones.get(&func) {
                Some(&copy) => out.push(
                    Inst::Closure {
                        dst,
                        func: copy,
                        captures: with_scope(&captures),
                    },
                    span,
                ),
                None => out.push(
                    Inst::Closure {
                        dst,
                        func,
                        captures,
                    },
                    span,
                ),
            },
            Inst::Start {
                task,
                args,
                done,
                cancelled,
                slot,
                policy,
                ..
            } => {
                let (task, args) = match self.clones.get(&task) {
                    Some(&copy) => (copy, with_scope(&args)),
                    None => (task, args),
                };
                out.push(
                    Inst::Start {
                        task,
                        args,
                        done,
                        cancelled,
                        instance: self.id,
                        slot,
                        policy,
                    },
                    span,
                );
            }
            inst => out.push(inst, span),
        }
        Ok(())
    }

    /// Emits, into `dst`, the value of input `slot` as the caller binds it:
    /// its argument entry, else the component's default, else `None`.
    fn load_input(
        &self,
        dst: Reg,
        slot: u32,
        scope: &[Reg],
        span: TextRange,
        out: &mut Rewrite,
    ) -> Result<(), String> {
        let arg = self.instance.args.iter().find(|(s, _)| *s == slot);
        if let Some(&(_, at)) = arg {
            let func = self.caller(at).ok_or_else(|| {
                format!(
                    "the argument of input `{}` has no entry",
                    self.input_name(slot)
                )
            })?;
            out.push(
                Inst::Call {
                    dst,
                    func,
                    args: scope.to_vec(),
                },
                span,
            );
        } else if let Some(default) = self.child.input_defaults[slot as usize] {
            let (func, args) = match self.clones.get(&default) {
                Some(&copy) => (copy, scope.to_vec()),
                None => (default, Vec::new()),
            };
            out.push(Inst::Call { dst, func, args }, span);
        } else {
            out.push(
                Inst::Const {
                    dst,
                    value: Const::Nil,
                },
                span,
            );
        }
        Ok(())
    }

    /// The instance's function giving input `styled.slot` the value of the
    /// first of its caller's style arms whose selector holds, else the base's,
    /// else the value the caller binds.
    fn styled_input(&self, styled: &UiStyledInput) -> Function {
        let depth = self.instance.depth;
        let scope: Vec<Reg> = (0..depth).map(Reg).collect();
        let mut out = Rewrite {
            body: Body {
                regs: depth,
                insts: Vec::new(),
                spans: Vec::new(),
            },
            starts: Vec::new(),
        };
        let mut function = Function {
            name: format!(
                "{}.{} [{}]",
                self.child.name,
                self.input_name(styled.slot),
                self.instance.identity
            ),
            kind: FunctionKind::RegionEntry,
            symbol: None,
            module: self.instance.unit as usize,
            params: depth,
            captures: Vec::new(),
            body: Err(Unsupported {
                reason: String::new(),
                at: styled.at,
            }),
        };
        match self.styled_body(styled, &scope, &mut out) {
            Ok(()) => function.body = Ok(out.body),
            Err(reason) => {
                function.body = Err(Unsupported {
                    reason,
                    at: styled.at,
                })
            }
        }
        function
    }

    fn styled_body(
        &self,
        styled: &UiStyledInput,
        scope: &[Reg],
        out: &mut Rewrite,
    ) -> Result<(), String> {
        let span = styled.at;
        let part = |part: u32| {
            self.caller_part(styled.at, part)
                .ok_or_else(|| "the style's value has no entry".to_string())
        };
        for arm in &styled.arms {
            let cond = self.selector(&arm.when, scope, span, out)?;
            let skip = out.body.insts.len();
            out.push(
                Inst::JumpIf {
                    cond,
                    when: false,
                    target: 0,
                },
                span,
            );
            let dst = out.reg();
            out.push(
                Inst::Call {
                    dst,
                    func: part(arm.part)?,
                    args: scope.to_vec(),
                },
                span,
            );
            out.push(Inst::Return { src: dst }, span);
            let end = out.body.insts.len() as u32;
            if let Inst::JumpIf { target, .. } = &mut out.body.insts[skip] {
                *target = end;
            }
        }
        let dst = out.reg();
        match styled.base {
            Some(base) => out.push(
                Inst::Call {
                    dst,
                    func: part(base)?,
                    args: scope.to_vec(),
                },
                span,
            ),
            None => self.load_input(dst, styled.slot, scope, span, out)?,
        }
        out.push(Inst::Return { src: dst }, span);
        Ok(())
    }

    /// Emits the selector `when` over the component's `@selector` members,
    /// returning the register holding whether it holds.
    fn selector(
        &self,
        when: &[UiMemberWhen],
        scope: &[Reg],
        span: TextRange,
        out: &mut Rewrite,
    ) -> Result<Reg, String> {
        let mut stack: Vec<Reg> = Vec::new();
        for step in when {
            let dst = out.reg();
            match step {
                UiMemberWhen::Member(name) => self.member(name, dst, scope, span, out)?,
                UiMemberWhen::Not => {
                    let src = stack.pop().ok_or("a selector lacks an operand")?;
                    out.push(
                        Inst::Unary {
                            dst,
                            op: UnaryOp::Not,
                            src,
                        },
                        span,
                    );
                }
                UiMemberWhen::And | UiMemberWhen::Or => {
                    let rhs = stack.pop().ok_or("a selector lacks an operand")?;
                    let lhs = stack.pop().ok_or("a selector lacks an operand")?;
                    let op = match step {
                        UiMemberWhen::And => BinaryOp::BitAnd(Num::U64),
                        _ => BinaryOp::BitOr(Num::U64),
                    };
                    out.push(Inst::Binary { dst, op, lhs, rhs }, span);
                }
            }
            stack.push(dst);
        }
        stack.pop().ok_or_else(|| "an empty selector".to_string())
    }

    /// Emits, into `dst`, the value of the instance's member `name`: a state,
    /// an input as the caller binds it, or a `computed`.
    fn member(
        &self,
        name: &str,
        dst: Reg,
        scope: &[Reg],
        span: TextRange,
        out: &mut Rewrite,
    ) -> Result<(), String> {
        if let Some(slot) = self.child.states.iter().position(|s| s == name) {
            out.push(
                Inst::LoadState {
                    dst,
                    slot: self.base + slot as u32,
                },
                span,
            );
            return Ok(());
        }
        if let Some(slot) = self.child.inputs.iter().position(|s| s == name) {
            return self.load_input(dst, slot as u32, scope, span, out);
        }
        let func = self
            .child
            .member(name)
            .ok_or_else(|| format!("the selector `{name}` is no member"))?;
        let (func, args) = match self.clones.get(&func) {
            Some(&copy) => (copy, scope.to_vec()),
            None => (func, Vec::new()),
        };
        out.push(Inst::Call { dst, func, args }, span);
        Ok(())
    }

    fn input_name(&self, slot: u32) -> &str {
        self.child
            .inputs
            .get(slot as usize)
            .map_or("?", String::as_str)
    }
}

/// A copy's body being emitted.
struct Rewrite {
    body: Body,
    /// The index in the copy of each original instruction's first emitted
    /// one, then of the end.
    starts: Vec<u32>,
}

impl Rewrite {
    fn push(&mut self, inst: Inst, span: TextRange) {
        self.body.insts.push(inst);
        self.body.spans.push(span);
    }

    /// A fresh register.
    fn reg(&mut self) -> Reg {
        let reg = Reg(self.body.regs);
        self.body.regs += 1;
        reg
    }
}

/// Maps every register `inst` names through `f`.
fn regs(inst: &mut Inst, f: &impl Fn(Reg) -> Reg) {
    let all = |rs: &mut Vec<Reg>| rs.iter_mut().for_each(|r| *r = f(*r));
    match inst {
        Inst::Const { dst, .. } | Inst::LoadState { dst, .. } | Inst::LoadInput { dst, .. } => {
            *dst = f(*dst)
        }
        Inst::StoreState { src, .. } | Inst::Return { src } => *src = f(*src),
        Inst::Move { dst, src }
        | Inst::Unary { dst, src, .. }
        | Inst::Cast { dst, src, .. }
        | Inst::Field { dst, src, .. }
        | Inst::Len { dst, src }
        | Inst::Tag { dst, src }
        | Inst::IsNil { dst, src }
        | Inst::Display { dst, src, .. } => {
            *dst = f(*dst);
            *src = f(*src);
        }
        Inst::Binary { dst, lhs, rhs, .. } => {
            *dst = f(*dst);
            *lhs = f(*lhs);
            *rhs = f(*rhs);
        }
        Inst::Call { dst, args, .. } | Inst::Native { dst, args, .. } => {
            *dst = f(*dst);
            all(args);
        }
        Inst::CallValue { dst, callee, args } => {
            *dst = f(*dst);
            *callee = f(*callee);
            all(args);
        }
        Inst::Closure { dst, captures, .. } => {
            *dst = f(*dst);
            all(captures);
        }
        Inst::Make { dst, fields, .. } => {
            *dst = f(*dst);
            all(fields);
        }
        Inst::List { dst, items } => {
            *dst = f(*dst);
            all(items);
        }
        Inst::Concat { dst, parts } => {
            *dst = f(*dst);
            all(parts);
        }
        Inst::Translate {
            dst,
            message,
            locale,
            args,
        } => {
            *dst = f(*dst);
            *message = f(*message);
            *locale = f(*locale);
            all(args);
        }
        Inst::Index { dst, list, index } | Inst::Remove { dst, list, index } => {
            *dst = f(*dst);
            *list = f(*list);
            *index = f(*index);
        }
        Inst::Push { list, item } => {
            *list = f(*list);
            *item = f(*item);
        }
        Inst::Insert { list, index, item } => {
            *list = f(*list);
            *index = f(*index);
            *item = f(*item);
        }
        Inst::Truncate { list, len } => {
            *list = f(*list);
            *len = f(*len);
        }
        Inst::SetPath { root, path, src } => {
            *root = f(*root);
            *src = f(*src);
            for step in path {
                if let PathStep::Index(r) = step {
                    *r = f(*r);
                }
            }
        }
        Inst::JumpIf { cond, .. } => *cond = f(*cond),
        Inst::Switch { src, .. } => *src = f(*src),
        Inst::Emit { args, .. } => all(args),
        Inst::NodeRef { dst, .. } => *dst = f(*dst),
        Inst::Node {
            dst, node, args, ..
        } => {
            *dst = f(*dst);
            *node = f(*node);
            all(args);
        }
        Inst::Start {
            args,
            done,
            cancelled,
            ..
        } => {
            all(args);
            for r in [done, cancelled].into_iter().flatten() {
                *r = f(*r);
            }
        }
        Inst::Jump { .. } | Inst::Unreachable => {}
    }
}
