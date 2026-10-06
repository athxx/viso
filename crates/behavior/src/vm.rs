//! The interpreter: budgets, faults, component instances and action
//! transactions.
//!
//! Every outermost [`Vm::call`] is one transaction. A state write replaces the
//! slot's value at once, so later reads in the same transaction see it, and the
//! value it replaced goes to an undo log on the slot's first write. When the
//! call returns, the transaction commits: the instance's revision rises by one if
//! any state was written, the written slots are marked dirty, and the queued
//! events are handed to the caller. When it faults, every written slot is
//! restored from the undo log and the queued events are dropped, so a failed
//! action leaves no trace. A nested action call is part of the outer
//! transaction.
//!
//! Budgets are per outermost invocation: every executed instruction spends one
//! unit of the instruction budget, every heap allocation the program makes
//! (strings, lists, aggregates, closures, event payloads, copies made by a write
//! into a shared value) is charged against the memory budget, and the call
//! nesting is bounded.
//!
//! A native call spends its schema's cost in instructions and one unit of the
//! native call quota, and its result is charged against the memory budget. A
//! native error or panic is a [`FaultKind::NativeFailure`] fault; the native's
//! own side effects are outside the transaction and are not rolled back.
//!
//! A `task` runs on a [`Fiber`]: [`Vm::start_task`] runs it until it awaits a
//! task native, which hands over the work it waits for; the fiber keeps the
//! task's registers and frames, and [`Vm::resume_task`] continues it with the
//! work's value. Each run between two suspensions is one invocation, with its
//! own budget. A committed call's `start`s are handed to the caller, which
//! starts the tasks.

use std::fmt;
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;

use crate::arith;
use crate::i18n::Translator;
use crate::memo::{self, Memo, ReadGraph, Reads};
use crate::module::{Code, Module, Span, TaskPolicy};
use crate::native::{
    NativeCx, NativeError, NativeFunction, NativeFuture, NativeKind, Natives, SchemaConflict,
    Services, ThreadDomain,
};
use crate::op::{DisplayKind, Op};
use crate::value::{Aggregate, Closure, Value};

/// The limits of one outermost invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The instructions it may execute.
    pub instructions: u64,
    /// The heap bytes it may allocate.
    pub memory: u64,
    /// The deepest call nesting, counting the entry chunk.
    pub depth: u32,
    /// The native calls it may make.
    pub native_calls: u32,
}

impl Default for Budget {
    /// 16 Mi instructions, 64 MiB, 256 nested calls and 1024 native calls.
    fn default() -> Budget {
        Budget {
            instructions: 1 << 24,
            memory: 64 << 20,
            depth: 256,
            native_calls: 1024,
        }
    }
}

/// What went wrong at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultKind {
    /// The instruction budget ran out.
    InstructionBudget,
    /// The call nesting exceeded the budget.
    CallDepth,
    /// The memory budget ran out.
    MemoryBudget,
    /// The native call quota ran out.
    NativeCallBudget,
    /// An integer result was out of its type's range.
    Overflow,
    /// An integer division or remainder by zero.
    DivideByZero,
    /// A shift amount outside `0..bits`.
    ShiftRange,
    /// A list index outside the list.
    IndexOutOfBounds,
    /// A chunk without a body, an unlinked native or a native on a thread
    /// domain the interpreter does not provide was called.
    Unsupported,
    /// A native was called without a capability it requires.
    CapabilityDenied,
    /// A native function returned an error or panicked.
    NativeFailure,
    /// An instance was created without a required input.
    MissingInput,
    /// The code broke an invariant the compiler guarantees (a value of the
    /// wrong kind, an `Unreachable` reached, a mismatched call).
    Internal,
    /// A computed's evaluation reached the same computed again.
    ReactiveCycle,
    /// A task read a state or an input after it suspended.
    SuspendedRead,
}

impl FaultKind {
    /// The stable diagnostic code.
    pub fn code(self) -> &'static str {
        match self {
            FaultKind::InstructionBudget | FaultKind::CallDepth | FaultKind::NativeCallBudget => {
                "E7101"
            }
            FaultKind::MemoryBudget => "E7102",
            FaultKind::Overflow | FaultKind::DivideByZero | FaultKind::ShiftRange => "E7103",
            FaultKind::IndexOutOfBounds => "E7104",
            FaultKind::Unsupported | FaultKind::MissingInput | FaultKind::Internal => "E7105",
            FaultKind::NativeFailure => "E7106",
            FaultKind::CapabilityDenied => "E6103",
            FaultKind::ReactiveCycle => "E4202",
            FaultKind::SuspendedRead => "E4102",
        }
    }

    /// A description of the fault.
    pub fn describe(self) -> &'static str {
        match self {
            FaultKind::InstructionBudget => "the instruction budget is exhausted",
            FaultKind::CallDepth => "the call depth budget is exhausted",
            FaultKind::MemoryBudget => "the memory budget is exhausted",
            FaultKind::NativeCallBudget => "the native call quota is exhausted",
            FaultKind::Overflow => "integer overflow",
            FaultKind::DivideByZero => "integer division by zero",
            FaultKind::ShiftRange => "shift amount out of range",
            FaultKind::IndexOutOfBounds => "index out of bounds",
            FaultKind::Unsupported => "the called function cannot run",
            FaultKind::CapabilityDenied => "a required capability is not granted",
            FaultKind::NativeFailure => "a native function failed",
            FaultKind::MissingInput => "a required input has no value",
            FaultKind::Internal => "internal behavior fault",
            FaultKind::ReactiveCycle => "a computed depends on itself",
            FaultKind::SuspendedRead => "a task read component state after it suspended",
        }
    }
}

/// Where a fault happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    /// The chunk.
    pub chunk: u32,
    /// The instruction.
    pub pc: u32,
    /// Its source span in the chunk's file ([`Span::default`] when the chunk has
    /// no body).
    pub span: Span,
}

/// A run-time fault. The transaction it happened in was rolled back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// What went wrong.
    pub kind: FaultKind,
    /// Where, when it happened in code.
    pub at: Option<Location>,
    /// A description.
    pub message: String,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.code(), self.message)?;
        if let Some(at) = self.at {
            write!(f, " (chunk {} at {})", at.chunk, at.pc)?;
        }
        Ok(())
    }
}

impl std::error::Error for Fault {}

/// A component event queued by `emit`.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The event, by index into the component's events.
    pub index: u32,
    /// The arguments, in parameter order.
    pub args: Box<[Value]>,
}

/// A task start queued by `start`.
#[derive(Debug, Clone, PartialEq)]
pub struct Start {
    /// The task's chunk.
    pub task: u32,
    /// Its arguments.
    pub args: Box<[Value]>,
    /// The closure run with the task's value once it returns: its `success`
    /// and `error` handlers.
    pub done: Option<Value>,
    /// The closure run when the task is cancelled while its starter lives.
    pub cancelled: Option<Value>,
    /// The inlined component instance the `start` belongs to, `0` for the
    /// mounted component's own.
    pub instance: u32,
    /// The instance's task slot it runs in, `None` for an unnamed start.
    pub slot: Option<u32>,
    /// What the slot does while an earlier task runs.
    pub policy: TaskPolicy,
}

/// A committed invocation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    /// What it returned.
    pub value: Value,
    /// The events it queued, in emit order.
    pub events: Vec<Event>,
    /// The task starts it queued, in order.
    pub starts: Vec<Start>,
}

/// A task suspended at a task native it awaits: its registers, frames and
/// next instruction.
pub struct Fiber {
    stack: Vec<Value>,
    frames: Vec<Frame>,
    cursor: Cursor,
    /// The register the awaited value lands in.
    dst: u16,
}

impl fmt::Debug for Fiber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Fiber(chunk {}, pc {})",
            self.cursor.chunk, self.cursor.pc
        )
    }
}

/// Where a task's run stopped.
pub enum TaskStep {
    /// It returned this value.
    Done(Value),
    /// It awaits the work; resume it with the work's result.
    Awaiting(Fiber, NativeFuture),
}

impl fmt::Debug for TaskStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskStep::Done(value) => f.debug_tuple("Done").field(value).finish(),
            TaskStep::Awaiting(fiber, _) => f.debug_tuple("Awaiting").field(fiber).finish(),
        }
    }
}

/// How a run of the interpreter loop ended.
enum Exit {
    Return(Value),
    /// Awaiting the work at a task native, its value due in `dst`.
    Await(u16, NativeFuture),
}

/// What the last invocation spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cost {
    /// Instructions executed.
    pub instructions: u64,
    /// Heap bytes charged.
    pub memory: u64,
    /// Native calls made.
    pub native_calls: u32,
}

/// A component instance: its state and input values, a revision that rises
/// once per committed transaction that wrote state, the set of state slots
/// written since the host last cleared it, and the cached value of each
/// computed evaluated since a slot it reads last changed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Instance {
    component: Option<u32>,
    states: Vec<Value>,
    inputs: Vec<Value>,
    revision: u64,
    dirty: Vec<u64>,
    graph: Option<Rc<ReadGraph>>,
    memo: Vec<Memo>,
}

impl Instance {
    /// An instance of no component, for calling functions that touch no state.
    pub fn detached() -> Instance {
        Instance::default()
    }

    /// The component, by index.
    pub fn component(&self) -> Option<u32> {
        self.component
    }

    /// Every state value, by slot.
    pub fn states(&self) -> &[Value] {
        &self.states
    }

    /// Every input value, by slot.
    pub fn inputs(&self) -> &[Value] {
        &self.inputs
    }

    /// Sets input `slot`, as a parent re-binding it does.
    ///
    /// # Panics
    ///
    /// If `slot` is out of range.
    pub fn set_input(&mut self, slot: usize, value: Value) {
        if self.inputs[slot] != value {
            self.inputs[slot] = value;
            self.forget(|graph| graph.input_readers(slot));
        }
    }

    /// Sets state `slot` from outside a transaction, as a host that keeps the
    /// authoritative copy of a state elsewhere does before a call. Neither marks
    /// the slot written nor raises the revision.
    ///
    /// # Panics
    ///
    /// If `slot` is out of range.
    pub fn set_state(&mut self, slot: usize, value: Value) {
        if self.states[slot] != value {
            self.states[slot] = value;
            self.forget_state(slot);
        }
    }

    /// Empties the cache entries of the computeds reading state `slot`.
    #[inline]
    fn forget_state(&mut self, slot: usize) {
        self.forget(|graph| graph.state_readers(slot));
    }

    /// Empties the cache entries `readers` names.
    fn forget(&mut self, readers: impl FnOnce(&ReadGraph) -> &[u32]) {
        let Some(graph) = &self.graph else { return };
        for &entry in readers(graph) {
            self.memo[entry as usize] = Memo::Empty;
        }
    }

    /// Whether computed `chunk`'s value is cached.
    pub fn is_cached(&self, chunk: u32) -> bool {
        self.graph
            .as_ref()
            .and_then(|graph| graph.entry(chunk))
            .is_some_and(|entry| matches!(self.memo[entry], Memo::Ready(_)))
    }

    /// The number of committed transactions that wrote state.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The state slots written since the last [`Instance::clear_dirty`], in slot
    /// order.
    pub fn dirty(&self) -> impl Iterator<Item = usize> + '_ {
        self.dirty.iter().enumerate().flat_map(|(word, &bits)| {
            (0..64)
                .filter(move |bit| bits & (1 << bit) != 0)
                .map(move |bit| word * 64 + bit)
        })
    }

    /// Forgets which slots were written.
    pub fn clear_dirty(&mut self) {
        self.dirty.fill(0);
    }
}

/// A suspended caller.
struct Frame {
    chunk: u32,
    pc: u32,
    base: u32,
    dst: u16,
}

/// The executing chunk, its next instruction and its frame base.
struct Cursor {
    chunk: u32,
    pc: usize,
    base: usize,
}

/// A linked native import.
#[derive(Clone, Copy)]
struct Linked {
    function: &'static NativeFunction,
    /// The first capability it requires that was not granted.
    denied: Option<&'static str>,
}

/// A Presentation command a call issued while the interpreter defers them
/// ([`Vm::defer_presentation`]): the native import and its arguments, run
/// later by [`Vm::deliver`].
#[derive(Debug, Clone)]
pub struct Deferred {
    /// The native import.
    pub import: u32,
    /// Its arguments.
    pub args: Box<[Value]>,
}

/// A behavior interpreter over one module.
///
/// It owns reusable scratch space (the register stack, call frames, undo log
/// and event queue), so a warmed-up invocation that allocates no values
/// performs no heap allocation.
///
/// Before a module's natives can run, [`Vm::link`] resolves them against a
/// registry; natives reach the host through the [`Services`] installed with
/// [`Vm::services_mut`].
pub struct Vm {
    module: Rc<Module>,
    budget: Budget,
    linked: Box<[Option<Linked>]>,
    /// The capabilities [`link`](Self::link) granted.
    granted: Box<[Box<str>]>,
    services: Services,
    native_args: Vec<Value>,
    native_calls: u32,
    stack: Vec<Value>,
    frames: Vec<Frame>,
    fuel: u64,
    allocated: u64,
    undo: Vec<(u32, Value)>,
    marks: Vec<u64>,
    events: Vec<Event>,
    detail: String,
    graph: Rc<ReadGraph>,
    /// Whether the running invocation's instance caches against `graph`.
    memo: bool,
    /// Whether a Presentation native defers instead of running.
    deferring: bool,
    deferred: Vec<Deferred>,
    starts: Vec<Start>,
    /// Whether the running invocation is a task's, which may suspend.
    fiber: bool,
    /// Whether the running task suspended before: it sees the component as
    /// it started, so it reads no state or input any more.
    resumed: bool,
    /// The work the last task native handed over.
    awaiting: Option<NativeFuture>,
    /// The catalog tables and readers translating has decoded, made on the
    /// first `Translate`.
    translator: Option<Box<Translator>>,
}

type Step<T> = Result<T, FaultKind>;

impl Vm {
    /// An interpreter for `module` with `budget` per invocation.
    pub fn new(module: Rc<Module>, budget: Budget) -> Vm {
        Vm {
            linked: vec![None; module.natives().len()].into(),
            graph: Rc::new(ReadGraph::new(&module)),
            memo: false,
            module,
            budget,
            granted: Box::new([]),
            services: Services::default(),
            native_args: Vec::new(),
            native_calls: 0,
            stack: Vec::new(),
            frames: Vec::new(),
            fuel: 0,
            allocated: 0,
            undo: Vec::new(),
            marks: Vec::new(),
            events: Vec::new(),
            detail: String::new(),
            deferring: false,
            deferred: Vec::new(),
            starts: Vec::new(),
            fiber: false,
            awaiting: None,
            resumed: false,
            translator: None,
        }
    }

    /// The module it runs.
    pub fn module(&self) -> &Rc<Module> {
        &self.module
    }

    /// What chunk `chunk` reads, following every call and closure it makes:
    /// the slots whose change can change its result.
    ///
    /// # Panics
    ///
    /// If `chunk` is out of range.
    pub fn reads(&self, chunk: u32) -> Reads {
        memo::reads(&self.module, chunk)
    }

    /// The per-invocation budget.
    pub fn budget(&self) -> Budget {
        self.budget
    }

    /// Replaces the per-invocation budget.
    pub fn set_budget(&mut self, budget: Budget) {
        self.budget = budget;
    }

    /// What the last invocation spent.
    pub fn cost(&self) -> Cost {
        Cost {
            instructions: self.budget.instructions.saturating_sub(self.fuel),
            memory: self.allocated,
            native_calls: self.native_calls,
        }
    }

    /// Resolves every native import of the module against `natives`, granting
    /// `capabilities`. A native requiring a capability outside the grant still
    /// links, and calling it faults with [`FaultKind::CapabilityDenied`].
    ///
    /// # Errors
    ///
    /// A [`SchemaConflict`] (`E6101`) if an import is not registered or was
    /// compiled against a different signature; no import is then linked.
    pub fn link(&mut self, natives: &Natives, capabilities: &[&str]) -> Result<(), SchemaConflict> {
        let linked = self
            .module
            .natives()
            .iter()
            .map(|import| {
                let conflict = |message: String| SchemaConflict {
                    path: import.path.to_string(),
                    message,
                };
                let Some(entry) = natives.function(&import.path) else {
                    return Err(conflict(format!("`{}` is not registered", import.path)));
                };
                let function = entry.function;
                if function.signature() != import.signature
                    || function.params.len() != usize::from(import.params)
                {
                    return Err(conflict(format!(
                        "`{}` was compiled against another schema than the registered `{function:?}`",
                        import.path
                    )));
                }
                let denied = function
                    .capabilities
                    .iter()
                    .copied()
                    .find(|c| !capabilities.contains(c));
                Ok(Some(Linked { function, denied }))
            })
            .collect::<Result<_, _>>()?;
        self.linked = linked;
        self.granted = capabilities.iter().map(|&c| c.into()).collect();
        Ok(())
    }

    /// Whether [`link`](Self::link) granted `capability`.
    pub fn granted(&self, capability: &str) -> bool {
        self.granted.iter().any(|c| **c == *capability)
    }

    /// The capabilities [`link`](Self::link) granted.
    pub fn grant(&self) -> &[Box<str>] {
        &self.granted
    }

    /// Makes a Presentation native called from now on defer as a
    /// [`Deferred`] command instead of running, or run again. A faulting call
    /// discards the commands it issued.
    pub fn defer_presentation(&mut self, on: bool) {
        self.deferring = on;
    }

    /// The commands deferred so far, in issue order.
    pub fn deferred_mut(&mut self) -> &mut Vec<Deferred> {
        &mut self.deferred
    }

    /// Runs the deferred `command` now.
    ///
    /// # Errors
    ///
    /// A [`FaultKind::NativeFailure`] fault if the native fails or panics, or
    /// [`FaultKind::Unsupported`] if its import is not linked.
    pub fn deliver(&mut self, command: &Deferred) -> Result<(), Fault> {
        let import = command.import as usize;
        let module = Rc::clone(&self.module);
        let fault = |kind: FaultKind, message: String| Fault {
            kind,
            at: None,
            message,
        };
        let path = module.natives().get(import).map_or("?", |n| &*n.path);
        let Some(Some(Linked { function, .. })) = self.linked.get(import).copied() else {
            return Err(fault(
                FaultKind::Unsupported,
                format!("native `{path}` is not linked"),
            ));
        };
        let services = &mut self.services;
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            (function.call)(&mut NativeCx::new(services), &command.args)
        }));
        match result {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(fault(
                FaultKind::NativeFailure,
                format!("native `{path}` failed: {error}"),
            )),
            Err(_) => Err(fault(
                FaultKind::NativeFailure,
                format!("native `{path}` panicked"),
            )),
        }
    }

    /// The host services natives use.
    pub fn services_mut(&mut self) -> &mut Services {
        &mut self.services
    }

    /// Creates an instance of component `component` with the given input
    /// values: every other input takes its default, then every state runs its
    /// initializer in slot order (a state without one starts `Nil`).
    ///
    /// # Panics
    ///
    /// If `component` or an input slot is out of range.
    pub fn instantiate(
        &mut self,
        component: u32,
        inputs: impl IntoIterator<Item = (usize, Value)>,
    ) -> Result<Instance, Fault> {
        let module = Rc::clone(&self.module);
        let layout = module.layout(component);
        let mut instance = Instance {
            component: Some(component),
            states: vec![Value::Nil; layout.states.len()],
            inputs: vec![Value::Nil; layout.inputs.len()],
            revision: 0,
            dirty: vec![0; layout.states.len().div_ceil(64)],
            memo: vec![Memo::Empty; self.graph.entries()],
            graph: Some(Rc::clone(&self.graph)),
        };
        let mut given = vec![false; layout.inputs.len()];
        for (slot, value) in inputs {
            instance.inputs[slot] = value;
            given[slot] = true;
        }
        for (slot, default) in layout.input_defaults.iter().enumerate() {
            if given[slot] {
                continue;
            }
            let Some(chunk) = *default else {
                return Err(Fault {
                    kind: FaultKind::MissingInput,
                    at: None,
                    message: format!("`{}` requires input `{}`", layout.name, layout.inputs[slot]),
                });
            };
            let value = self.call(&mut instance, chunk, &[])?.value;
            instance.set_input(slot, value);
        }
        for (slot, init) in layout.state_inits.iter().enumerate() {
            if let Some(chunk) = *init {
                let value = self.call(&mut instance, chunk, &[])?.value;
                instance.set_state(slot, value);
            }
        }
        instance.clear_dirty();
        Ok(instance)
    }

    /// Runs chunk `chunk` with `args` against `instance` as one transaction.
    pub fn call(
        &mut self,
        instance: &mut Instance,
        chunk: u32,
        args: &[Value],
    ) -> Result<Outcome, Fault> {
        self.invoke(instance, chunk, args, &[])
    }

    /// Runs the closure `callee` with `args` against `instance` as one
    /// transaction.
    pub fn call_value(
        &mut self,
        instance: &mut Instance,
        callee: &Value,
        args: &[Value],
    ) -> Result<Outcome, Fault> {
        let Value::Closure(closure) = callee else {
            return Err(Fault {
                kind: FaultKind::Internal,
                at: None,
                message: "the callee is not a closure".into(),
            });
        };
        let closure = Rc::clone(closure);
        self.invoke(instance, closure.func, args, &closure.captures)
    }

    fn invoke(
        &mut self,
        instance: &mut Instance,
        chunk: u32,
        args: &[Value],
        captures: &[Value],
    ) -> Result<Outcome, Fault> {
        let issued = self.begin(instance);
        if self.memo
            && let Some(entry) = self.graph.entry(chunk)
        {
            if let Memo::Ready(value) = &instance.memo[entry] {
                return Ok(Outcome {
                    value: value.clone(),
                    ..Outcome::default()
                });
            }
            instance.memo[entry] = Memo::Evaluating;
        }
        let module = Rc::clone(&self.module);
        let mut cursor = Cursor {
            chunk,
            pc: 0,
            base: 0,
        };
        let result = self
            .enter(&module, chunk, args, captures)
            .and_then(|code| self.exec(&module, instance, &mut cursor, code));
        match result {
            Ok(Exit::Return(value)) => Ok(self.commit(instance, value)),
            Ok(Exit::Await(..)) => {
                let fault = self.rollback(&module, instance, &cursor, FaultKind::Internal, issued);
                Err(fault)
            }
            Err(kind) => Err(self.rollback(&module, instance, &cursor, kind, issued)),
        }
    }

    /// Runs task chunk `chunk` with `args` against `instance` until it returns
    /// or awaits a task native. The run is one transaction, like a call.
    pub fn start_task(
        &mut self,
        instance: &mut Instance,
        chunk: u32,
        args: &[Value],
    ) -> Result<TaskStep, Fault> {
        let issued = self.begin(instance);
        let module = Rc::clone(&self.module);
        let mut cursor = Cursor {
            chunk,
            pc: 0,
            base: 0,
        };
        self.fiber = true;
        let result = self
            .enter(&module, chunk, args, &[])
            .and_then(|code| self.exec(&module, instance, &mut cursor, code));
        self.fiber = false;
        self.suspend_or_finish(&module, instance, cursor, result, issued)
    }

    /// Continues `fiber` against `instance` with the result of the work it
    /// awaited, until the task returns or awaits again; a failed work faults
    /// at the call that handed it over.
    pub fn resume_task(
        &mut self,
        instance: &mut Instance,
        fiber: Fiber,
        result: Result<Value, NativeError>,
    ) -> Result<TaskStep, Fault> {
        let issued = self.begin(instance);
        let module = Rc::clone(&self.module);
        let Fiber {
            stack,
            frames,
            mut cursor,
            dst,
        } = fiber;
        self.stack = stack;
        self.frames = frames;
        let result = match result {
            Ok(value) => {
                self.stack[cursor.base + usize::from(dst)] = value;
                self.fiber = true;
                self.resumed = true;
                let run = self
                    .body(&module, cursor.chunk)
                    .and_then(|code| self.exec(&module, instance, &mut cursor, code));
                self.fiber = false;
                self.resumed = false;
                run
            }
            Err(error) => self.trap(
                FaultKind::NativeFailure,
                format!("the work the task awaited failed: {error}"),
            ),
        };
        self.suspend_or_finish(&module, instance, cursor, result, issued)
    }

    /// Packs a task run that awaits into its fiber, or commits one that
    /// returned.
    fn suspend_or_finish(
        &mut self,
        module: &Module,
        instance: &mut Instance,
        cursor: Cursor,
        result: Step<Exit>,
        issued: usize,
    ) -> Result<TaskStep, Fault> {
        match result {
            Ok(Exit::Return(value)) => {
                let outcome = self.commit(instance, value);
                Ok(TaskStep::Done(outcome.value))
            }
            Ok(Exit::Await(dst, work)) => {
                self.commit(instance, Value::Nil);
                let fiber = Fiber {
                    stack: mem::take(&mut self.stack),
                    frames: mem::take(&mut self.frames),
                    cursor,
                    dst,
                };
                Ok(TaskStep::Awaiting(fiber, work))
            }
            Err(kind) => Err(self.rollback(module, instance, &cursor, kind, issued)),
        }
    }

    /// Resets the per-invocation budgets and logs for a run against
    /// `instance`; returns how many deferred commands were issued before it.
    fn begin(&mut self, instance: &Instance) -> usize {
        self.fuel = self.budget.instructions;
        self.allocated = 0;
        self.native_calls = 0;
        self.detail.clear();
        self.marks.clear();
        self.marks.resize(instance.states.len().div_ceil(64), 0);
        self.memo = instance
            .graph
            .as_ref()
            .is_some_and(|graph| Rc::ptr_eq(graph, &self.graph));
        self.deferred.len()
    }

    /// Commits the run: marks the written slots dirty and hands over its
    /// events and starts with `value`.
    fn commit(&mut self, instance: &mut Instance, value: Value) -> Outcome {
        if !self.undo.is_empty() {
            instance.revision += 1;
            for (slot, _) in self.undo.drain(..) {
                instance.dirty[slot as usize / 64] |= 1 << (slot % 64);
            }
        }
        Outcome {
            value,
            events: mem::take(&mut self.events),
            starts: mem::take(&mut self.starts),
        }
    }

    /// Rolls the run back and describes its fault `kind` at `cursor`.
    fn rollback(
        &mut self,
        module: &Module,
        instance: &mut Instance,
        cursor: &Cursor,
        kind: FaultKind,
        issued: usize,
    ) -> Fault {
        self.deferred.truncate(issued);
        while let Some((slot, old)) = self.undo.pop() {
            instance.states[slot as usize] = old;
            instance.forget_state(slot as usize);
        }
        for memo in &mut instance.memo {
            if *memo == Memo::Evaluating {
                *memo = Memo::Empty;
            }
        }
        self.events.clear();
        self.starts.clear();
        self.awaiting = None;
        self.stack.clear();
        self.frames.clear();
        let at = module.chunks().get(cursor.chunk as usize).map(|c| {
            let pc = cursor.pc.saturating_sub(1);
            Location {
                chunk: cursor.chunk,
                pc: pc as u32,
                span: c
                    .body
                    .as_ref()
                    .ok()
                    .and_then(|code| code.spans.get(pc).copied())
                    .unwrap_or_default(),
            }
        });
        let message = if self.detail.is_empty() {
            kind.describe().to_owned()
        } else {
            mem::take(&mut self.detail)
        };
        Fault { kind, at, message }
    }

    /// Sets up the entry frame of `chunk`.
    fn enter<'m>(
        &mut self,
        module: &'m Module,
        chunk: u32,
        args: &[Value],
        captures: &[Value],
    ) -> Step<&'m Code> {
        let Some(c) = module.chunks().get(chunk as usize) else {
            return self.trap(FaultKind::Internal, format!("no chunk {chunk}"));
        };
        let code = self.body(module, chunk)?;
        if args.len() != usize::from(c.params) || captures.len() != c.captures.len() {
            return self.trap(
                FaultKind::Internal,
                format!("`{}` called with the wrong number of values", c.name),
            );
        }
        self.stack.clear();
        self.stack.resize(usize::from(c.regs), Value::Nil);
        self.stack[..args.len()].clone_from_slice(args);
        for (&r, value) in c.captures.iter().zip(captures) {
            self.stack[usize::from(r)] = value.clone();
        }
        Ok(code)
    }

    /// The body of `chunk`, or an [`FaultKind::Unsupported`] fault naming why
    /// it has none.
    fn body<'m>(&mut self, module: &'m Module, chunk: u32) -> Step<&'m Code> {
        let c = module.chunk(chunk);
        match &c.body {
            Ok(code) => Ok(code),
            Err(reason) => self.trap(
                FaultKind::Unsupported,
                format!("`{}` cannot run: {reason}", c.name),
            ),
        }
    }

    /// Fails with `kind`, described by `detail`.
    fn trap<T>(&mut self, kind: FaultKind, detail: String) -> Step<T> {
        self.detail = detail;
        Err(kind)
    }

    /// Charges `bytes` against the memory budget.
    fn charge(&mut self, bytes: u64) -> Step<()> {
        self.allocated += bytes;
        if self.allocated > self.budget.memory {
            Err(FaultKind::MemoryBudget)
        } else {
            Ok(())
        }
    }

    #[inline(always)]
    fn int(&self, base: usize, r: u16) -> Step<i64> {
        self.stack[base + usize::from(r)]
            .as_int()
            .ok_or(FaultKind::Internal)
    }

    #[inline(always)]
    fn float(&self, base: usize, r: u16) -> Step<f64> {
        self.stack[base + usize::from(r)]
            .as_float()
            .ok_or(FaultKind::Internal)
    }

    /// The values of the registers named by `words`.
    fn gather(&self, base: usize, words: &[u32]) -> Box<[Value]> {
        words
            .iter()
            .map(|&r| self.stack[base + r as usize].clone())
            .collect()
    }

    /// Runs from `cursor` until the entry chunk returns.
    fn exec<'m>(
        &mut self,
        module: &'m Module,
        instance: &mut Instance,
        cur: &mut Cursor,
        mut code: &'m Code,
    ) -> Step<Exit> {
        let mut base = cur.base;
        loop {
            if self.fuel == 0 {
                return Err(FaultKind::InstructionBudget);
            }
            self.fuel -= 1;
            let op = code.ops[cur.pc];
            cur.pc += 1;
            let value = match op {
                Op::Const { dst, index } => (dst, code.consts[index as usize].clone()),
                Op::Int { dst, value } => (dst, Value::Int(i64::from(value))),
                Op::Nil { dst } => (dst, Value::Nil),
                Op::Move { dst, src } => (dst, self.stack[base + usize::from(src)].clone()),
                Op::LoadState { dst, slot } => {
                    if self.resumed {
                        return Err(FaultKind::SuspendedRead);
                    }
                    let Some(value) = instance.states.get(slot as usize) else {
                        return Err(FaultKind::Internal);
                    };
                    (dst, value.clone())
                }
                Op::StoreState { src, slot } => {
                    let value = self.stack[base + usize::from(src)].clone();
                    let Some(target) = instance.states.get_mut(slot as usize) else {
                        return Err(FaultKind::Internal);
                    };
                    let (word, bit) = (slot as usize / 64, 1u64 << (slot % 64));
                    if self.marks[word] & bit == 0 {
                        self.marks[word] |= bit;
                        self.undo.push((slot, mem::replace(target, value)));
                    } else {
                        *target = value;
                    }
                    if self.memo {
                        instance.forget_state(slot as usize);
                    }
                    continue;
                }
                Op::LoadInput { dst, slot } => {
                    if self.resumed {
                        return Err(FaultKind::SuspendedRead);
                    }
                    let Some(value) = instance.inputs.get(slot as usize) else {
                        return Err(FaultKind::Internal);
                    };
                    (dst, value.clone())
                }
                Op::AddI64 { dst, a, b } => {
                    let r = self.int(base, a)?.checked_add(self.int(base, b)?);
                    (dst, Value::Int(r.ok_or(FaultKind::Overflow)?))
                }
                Op::SubI64 { dst, a, b } => {
                    let r = self.int(base, a)?.checked_sub(self.int(base, b)?);
                    (dst, Value::Int(r.ok_or(FaultKind::Overflow)?))
                }
                Op::MulI64 { dst, a, b } => {
                    let r = self.int(base, a)?.checked_mul(self.int(base, b)?);
                    (dst, Value::Int(r.ok_or(FaultKind::Overflow)?))
                }
                Op::LtI64 { dst, a, b } => {
                    (dst, Value::bool(self.int(base, a)? < self.int(base, b)?))
                }
                Op::LeI64 { dst, a, b } => {
                    (dst, Value::bool(self.int(base, a)? <= self.int(base, b)?))
                }
                Op::AddF64 { dst, a, b } => (
                    dst,
                    Value::Float(self.float(base, a)? + self.float(base, b)?),
                ),
                Op::SubF64 { dst, a, b } => (
                    dst,
                    Value::Float(self.float(base, a)? - self.float(base, b)?),
                ),
                Op::MulF64 { dst, a, b } => (
                    dst,
                    Value::Float(self.float(base, a)? * self.float(base, b)?),
                ),
                Op::DivF64 { dst, a, b } => (
                    dst,
                    Value::Float(self.float(base, a)? / self.float(base, b)?),
                ),
                Op::LtF64 { dst, a, b } => (
                    dst,
                    Value::bool(self.float(base, a)? < self.float(base, b)?),
                ),
                Op::LeF64 { dst, a, b } => (
                    dst,
                    Value::bool(self.float(base, a)? <= self.float(base, b)?),
                ),
                Op::Eq { dst, a, b } => (
                    dst,
                    Value::bool(
                        self.stack[base + usize::from(a)] == self.stack[base + usize::from(b)],
                    ),
                ),
                Op::Ne { dst, a, b } => (
                    dst,
                    Value::bool(
                        self.stack[base + usize::from(a)] != self.stack[base + usize::from(b)],
                    ),
                ),
                Op::Not { dst, src } => (dst, Value::bool(self.int(base, src)? == 0)),
                Op::Call { dst, ext } => {
                    let at = ext as usize;
                    let func = code.ext[at];
                    let argc = code.ext[at + 1] as usize;
                    if self.memo
                        && let Some(entry) = self.graph.entry(func)
                    {
                        match &instance.memo[entry] {
                            Memo::Ready(value) => {
                                self.stack[base + usize::from(dst)] = value.clone();
                                continue;
                            }
                            Memo::Evaluating => {
                                let name = &module.chunk(func).name;
                                return self.trap(
                                    FaultKind::ReactiveCycle,
                                    format!("computed `{name}` depends on itself"),
                                );
                            }
                            Memo::Empty => instance.memo[entry] = Memo::Evaluating,
                        }
                    }
                    let body = self.body(module, func)?;
                    let callee = module.chunk(func);
                    let args = &code.ext[at + 2..at + 2 + argc];
                    self.push_frame(cur, base, dst, usize::from(callee.regs))?;
                    let callee_base = cur.base;
                    for (i, &r) in args.iter().enumerate() {
                        self.stack[callee_base + i] = self.stack[base + r as usize].clone();
                    }
                    cur.chunk = func;
                    code = body;
                    base = callee_base;
                    continue;
                }
                Op::CallValue { dst, ext } => {
                    let at = ext as usize;
                    let argc = code.ext[at + 1] as usize;
                    let Value::Closure(closure) = &self.stack[base + code.ext[at] as usize] else {
                        return Err(FaultKind::Internal);
                    };
                    let closure = Rc::clone(closure);
                    let Some(callee) = module.chunks().get(closure.func as usize) else {
                        return Err(FaultKind::Internal);
                    };
                    if usize::from(callee.params) != argc
                        || callee.captures.len() != closure.captures.len()
                    {
                        return Err(FaultKind::Internal);
                    }
                    let body = self.body(module, closure.func)?;
                    let args = &code.ext[at + 2..at + 2 + argc];
                    self.push_frame(cur, base, dst, usize::from(callee.regs))?;
                    let callee_base = cur.base;
                    for (i, &r) in args.iter().enumerate() {
                        self.stack[callee_base + i] = self.stack[base + r as usize].clone();
                    }
                    for (&r, value) in callee.captures.iter().zip(closure.captures.iter()) {
                        self.stack[callee_base + usize::from(r)] = value.clone();
                    }
                    cur.chunk = closure.func;
                    code = body;
                    base = callee_base;
                    continue;
                }
                Op::Field { dst, src, index } => {
                    let Value::Agg(agg) = &self.stack[base + usize::from(src)] else {
                        return Err(FaultKind::Internal);
                    };
                    let Some(field) = agg.fields.get(usize::from(index)) else {
                        return Err(FaultKind::Internal);
                    };
                    (dst, field.clone())
                }
                Op::SetPath { root, ext } => {
                    self.set_path(code, base, root, ext as usize)?;
                    continue;
                }
                Op::IsNil { dst, src } => (
                    dst,
                    Value::bool(self.stack[base + usize::from(src)].is_nil()),
                ),
                Op::Jump { target } => {
                    cur.pc = target as usize;
                    continue;
                }
                Op::JumpIf { cond, target } => {
                    if self.int(base, cond)? != 0 {
                        cur.pc = target as usize;
                    }
                    continue;
                }
                Op::JumpUnless { cond, target } => {
                    if self.int(base, cond)? == 0 {
                        cur.pc = target as usize;
                    }
                    continue;
                }
                Op::Switch { src, ext } => {
                    let at = ext as usize;
                    let low = u64::from(code.ext[at]);
                    let high = u64::from(code.ext[at + 1]);
                    let first = ((high << 32) | low) as i64;
                    let n = code.ext[at + 3] as usize;
                    let target = self
                        .int(base, src)?
                        .checked_sub(first)
                        .and_then(|i| usize::try_from(i).ok())
                        .filter(|&i| i < n)
                        .map_or(code.ext[at + 2], |i| code.ext[at + 4 + i]);
                    cur.pc = target as usize;
                    continue;
                }
                Op::Return { src } => {
                    let value = mem::take(&mut self.stack[base + usize::from(src)]);
                    self.stack.truncate(base);
                    if self.memo
                        && let Some(entry) = self.graph.entry(cur.chunk)
                    {
                        instance.memo[entry] = Memo::Ready(value.clone());
                    }
                    let Some(frame) = self.frames.pop() else {
                        return Ok(Exit::Return(value));
                    };
                    cur.chunk = frame.chunk;
                    cur.pc = frame.pc as usize;
                    cur.base = frame.base as usize;
                    base = cur.base;
                    code = self.body(module, frame.chunk)?;
                    (frame.dst, value)
                }
                Op::Emit { ext } => {
                    let at = ext as usize;
                    let n = code.ext[at + 1] as usize;
                    self.charge(16 + 16 * n as u64)?;
                    let args = self.gather(base, &code.ext[at + 2..at + 2 + n]);
                    self.events.push(Event {
                        index: code.ext[at],
                        args,
                    });
                    continue;
                }
                Op::Native { dst, ext } => {
                    let value = self.call_native(code, base, ext as usize)?;
                    if let Some(work) = self.awaiting.take() {
                        if !self.fiber {
                            return self.trap(
                                FaultKind::Internal,
                                "a task native was awaited outside a task".into(),
                            );
                        }
                        return Ok(Exit::Await(dst, work));
                    }
                    (dst, value)
                }
                Op::Start { ext } => {
                    let at = ext as usize;
                    let argc = code.ext[at + 1] as usize;
                    self.charge(64 + 16 * argc as u64)?;
                    let args = self.gather(base, &code.ext[at + 2..at + 2 + argc]);
                    let tail = &code.ext[at + 2 + argc..at + 7 + argc];
                    let handler =
                        |r: u32| (r != u32::MAX).then(|| self.stack[base + r as usize].clone());
                    let start = Start {
                        task: code.ext[at],
                        args,
                        done: handler(tail[0]),
                        cancelled: handler(tail[1]),
                        instance: tail[2],
                        slot: (tail[3] != u32::MAX).then_some(tail[3]),
                        policy: TaskPolicy::from_word(tail[4]),
                    };
                    self.starts.push(start);
                    continue;
                }
                Op::Unreachable => {
                    return self.trap(FaultKind::Internal, "reached unreachable code".into());
                }
                op @ (Op::Arith { dst, .. }
                | Op::Neg { dst, .. }
                | Op::BitNot { dst, .. }
                | Op::Cast { dst, .. }
                | Op::Closure { dst, .. }
                | Op::Make { dst, .. }
                | Op::List { dst, .. }
                | Op::Concat { dst, .. }
                | Op::Translate { dst, .. }
                | Op::Index { dst, .. }
                | Op::Len { dst, .. }
                | Op::Tag { dst, .. }
                | Op::Display { dst, .. }
                | Op::DisplayDim { dst, .. }) => (dst, self.compute(code, base, op)?),
            };
            let (dst, value) = value;
            self.stack[base + usize::from(dst)] = value;
        }
    }

    /// The value of an instruction off the hot dispatch path: generic
    /// arithmetic, casts, allocation, indexing and text rendering.
    #[inline(never)]
    fn compute(&mut self, code: &Code, base: usize, op: Op) -> Step<Value> {
        Ok(match op {
            Op::Arith { op, a, b, .. } => {
                let (x, y) = (
                    &self.stack[base + usize::from(a)],
                    &self.stack[base + usize::from(b)],
                );
                arith::binary(op, x, y)?
            }
            Op::Neg { num, src, .. } => arith::neg(num, &self.stack[base + usize::from(src)])?,
            Op::BitNot { num, src, .. } => {
                arith::bit_not(num, &self.stack[base + usize::from(src)])?
            }
            Op::Cast { from, to, src, .. } => {
                arith::cast(from, to, &self.stack[base + usize::from(src)])?
            }
            Op::Closure { ext, .. } => {
                let at = ext as usize;
                let n = code.ext[at + 1] as usize;
                self.charge(24 + 16 * n as u64)?;
                let captures = self.gather(base, &code.ext[at + 2..at + 2 + n]);
                let func = code.ext[at];
                Value::Closure(Rc::new(Closure { func, captures }))
            }
            Op::Make { ext, .. } => {
                let at = ext as usize;
                let n = code.ext[at + 1] as usize;
                self.charge(24 + 16 * n as u64)?;
                let fields = self.gather(base, &code.ext[at + 2..at + 2 + n]);
                let tag = code.ext[at];
                Value::Agg(Rc::new(Aggregate { tag, fields }))
            }
            Op::List { ext, .. } => {
                let at = ext as usize;
                let n = code.ext[at] as usize;
                self.charge(16 + 16 * n as u64)?;
                let items = self.gather(base, &code.ext[at + 1..at + 1 + n]);
                Value::List(Rc::new(items.into_vec()))
            }
            Op::Concat { ext, .. } => {
                let at = ext as usize;
                let parts = &code.ext[at + 1..at + 1 + code.ext[at] as usize];
                let mut len = 0;
                for &r in parts {
                    len += self.stack[base + r as usize]
                        .as_str()
                        .ok_or(FaultKind::Internal)?
                        .len();
                }
                self.charge(16 + len as u64)?;
                let mut text = String::with_capacity(len);
                for &r in parts {
                    text.push_str(self.stack[base + r as usize].as_str().unwrap_or_default());
                }
                Value::Str(Rc::new(text))
            }
            Op::Translate { ext, .. } => {
                let at = ext as usize;
                let n = code.ext[at + 2] as usize;
                let Some(catalog) = self.module.catalog.clone() else {
                    return self.trap(FaultKind::Internal, "no message catalog".into());
                };
                // The arguments gather into the native call scratch, so a
                // warm translation allocates only its text.
                let mut args = mem::take(&mut self.native_args);
                args.clear();
                args.extend(
                    code.ext[at + 3..at + 3 + n]
                        .iter()
                        .map(|&r| self.stack[base + r as usize].clone()),
                );
                let message = &self.stack[base + code.ext[at] as usize];
                let locale = &self.stack[base + code.ext[at + 1] as usize];
                let translator = self.translator.get_or_insert_default();
                let translated = translator.translate(&catalog, message, locale, &args);
                self.native_args = args;
                match translated {
                    Ok(value) => {
                        if let Value::Str(text) = &value {
                            self.charge(16 + text.len() as u64)?;
                        }
                        value
                    }
                    Err(detail) => return self.trap(FaultKind::Internal, detail),
                }
            }
            Op::Index { list, index, .. } => {
                let i = self.int(base, index)?;
                let Value::List(items) = &self.stack[base + usize::from(list)] else {
                    return Err(FaultKind::Internal);
                };
                match usize::try_from(i).ok().and_then(|i| items.get(i)) {
                    Some(item) => item.clone(),
                    None => {
                        let len = items.len();
                        return self.trap(
                            FaultKind::IndexOutOfBounds,
                            format!("index {i} is out of bounds of a list of {len}"),
                        );
                    }
                }
            }
            Op::Len { src, .. } => {
                let Value::List(items) = &self.stack[base + usize::from(src)] else {
                    return Err(FaultKind::Internal);
                };
                Value::Int(items.len() as i64)
            }
            Op::Tag { src, .. } => match &self.stack[base + usize::from(src)] {
                Value::Agg(agg) => Value::Int(i64::from(agg.tag)),
                Value::Int(tag) => Value::Int(*tag),
                _ => return Err(FaultKind::Internal),
            },
            Op::Display { kind, src, .. } => {
                let value = &self.stack[base + usize::from(src)];
                if kind == DisplayKind::Str {
                    if value.as_str().is_none() {
                        return Err(FaultKind::Internal);
                    }
                    value.clone()
                } else {
                    let text = arith::display(kind, value)?;
                    self.charge(16 + text.len() as u64)?;
                    Value::str(text)
                }
            }
            Op::DisplayDim { src, suffix, .. } => {
                let suffix = code.consts[usize::from(suffix)]
                    .as_str()
                    .unwrap_or_default();
                let text = arith::display_dim(&self.stack[base + usize::from(src)], suffix)?;
                self.charge(16 + text.len() as u64)?;
                Value::str(text)
            }
            _ => return Err(FaultKind::Internal),
        })
    }

    /// Runs the [`Op::Native`] whose operands start at `ext[at]`.
    #[inline(never)]
    fn call_native(&mut self, code: &Code, base: usize, at: usize) -> Step<Value> {
        let import = code.ext[at] as usize;
        let argc = code.ext[at + 1] as usize;
        let module = Rc::clone(&self.module);
        let path = &module.natives()[import].path;
        let Some(Linked { function, denied }) = self.linked[import] else {
            return self.trap(
                FaultKind::Unsupported,
                format!("native `{path}` is not linked"),
            );
        };
        if let Some(capability) = denied {
            return self.trap(
                FaultKind::CapabilityDenied,
                format!("native `{path}` requires capability `{capability}`, which is not granted"),
            );
        }
        if function.thread == ThreadDomain::Worker {
            return self.trap(
                FaultKind::Unsupported,
                format!("native `{path}` runs on a worker thread, which this interpreter does not provide"),
            );
        }
        if self.native_calls >= self.budget.native_calls {
            return Err(FaultKind::NativeCallBudget);
        }
        self.native_calls += 1;
        // The instruction itself already spent one unit.
        let extra = u64::from(function.cost.saturating_sub(1));
        if self.fuel < extra {
            self.fuel = 0;
            return Err(FaultKind::InstructionBudget);
        }
        self.fuel -= extra;
        if function.debug_draw && !cfg!(debug_assertions) {
            // Debug draw is removed from release builds.
            return Ok(Value::Int(0));
        }
        if function.presentation && self.deferring {
            let args = code.ext[at + 2..at + 2 + argc]
                .iter()
                .map(|&r| self.stack[base + r as usize].clone())
                .collect();
            self.deferred.push(Deferred {
                import: import as u32,
                args,
            });
            return Ok(Value::Int(0));
        }
        let mut args = mem::take(&mut self.native_args);
        args.clear();
        args.extend(
            code.ext[at + 2..at + 2 + argc]
                .iter()
                .map(|&r| self.stack[base + r as usize].clone()),
        );
        let mut cx = NativeCx::new(&mut self.services);
        let result = panic::catch_unwind(AssertUnwindSafe(|| (function.call)(&mut cx, &args)));
        let pending = cx.take_pending();
        args.clear();
        self.native_args = args;
        if pending.is_some() && function.kind != NativeKind::Task {
            return self.trap(
                FaultKind::Internal,
                format!("native `{path}` suspended, but it is no task"),
            );
        }
        if matches!(result, Ok(Ok(_))) {
            self.awaiting = pending;
        }
        match result {
            Ok(Ok(value)) => {
                self.charge(value.heap_bytes())?;
                Ok(value)
            }
            Ok(Err(error)) => self.trap(
                FaultKind::NativeFailure,
                format!("native `{path}` failed: {error}"),
            ),
            Err(payload) => {
                let reason = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("an unknown reason");
                let detail = format!("native `{path}` panicked: {reason}");
                self.trap(FaultKind::NativeFailure, detail)
            }
        }
    }

    /// Suspends the current frame (resuming at `cur.pc` into register `dst`)
    /// and opens a frame of `regs` registers on top of the stack, pointing
    /// `cur.base` at it.
    fn push_frame(&mut self, cur: &mut Cursor, base: usize, dst: u16, regs: usize) -> Step<()> {
        if self.frames.len() + 2 > self.budget.depth as usize {
            return Err(FaultKind::CallDepth);
        }
        self.frames.push(Frame {
            chunk: cur.chunk,
            pc: cur.pc as u32,
            base: base as u32,
            dst,
        });
        cur.base = self.stack.len();
        cur.pc = 0;
        self.stack.resize(cur.base + regs, Value::Nil);
        Ok(())
    }

    /// Runs the [`Op::SetPath`] whose operands start at `ext[at]`.
    fn set_path(&mut self, code: &Code, base: usize, root: u16, at: usize) -> Step<()> {
        let src = self.stack[base + code.ext[at] as usize].clone();
        let steps = &code.ext[at + 2..at + 2 + 2 * code.ext[at + 1] as usize];
        let mut value = mem::take(&mut self.stack[base + usize::from(root)]);
        let result = self.write_path(&mut value, steps, base, src);
        self.stack[base + usize::from(root)] = value;
        result
    }

    fn write_path(&mut self, root: &mut Value, steps: &[u32], base: usize, src: Value) -> Step<()> {
        let mut slot = root;
        for &[kind, arg] in steps.as_chunks::<2>().0 {
            let shared = match &*slot {
                Value::Agg(agg) => Rc::strong_count(agg) > 1,
                Value::List(items) => Rc::strong_count(items) > 1,
                _ => return Err(FaultKind::Internal),
            };
            if shared {
                self.charge(slot.heap_bytes())?;
            }
            slot = match (kind, slot) {
                (0, Value::Agg(agg)) => Rc::make_mut(agg)
                    .fields
                    .get_mut(arg as usize)
                    .ok_or(FaultKind::Internal)?,
                (1, Value::List(items)) => {
                    let i = self.stack[base + arg as usize]
                        .as_int()
                        .ok_or(FaultKind::Internal)?;
                    let len = items.len();
                    match usize::try_from(i).ok().filter(|&i| i < len) {
                        Some(i) => &mut Rc::make_mut(items)[i],
                        None => {
                            return self.trap(
                                FaultKind::IndexOutOfBounds,
                                format!("index {i} is out of bounds of a list of {len}"),
                            );
                        }
                    }
                }
                _ => return Err(FaultKind::Internal),
            };
        }
        *slot = src;
        Ok(())
    }
}
