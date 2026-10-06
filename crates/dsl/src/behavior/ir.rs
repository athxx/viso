//! The Behavior IR: register-based instructions for `fn`/`action` bodies,
//! computed values, state initializers, input defaults, constants and record
//! field defaults.
//!
//! A function owns a flat register file. Its arguments arrive in `r0..` in
//! parameter order; a closure's captured values arrive in the registers its
//! [`Function::captures`] names. Control flow is by instruction index: every
//! jump target is the index of an instruction in the same body, patched once the
//! body is lowered.
//!
//! Operators are typed ([`Num`]): `I64 + I64` is `add.i64`, and the dimensional
//! scalars (lengths, `Duration`, `Angle`, `Frequency`) compute in `f64`. `&&`
//! and `||` lower to branches, never to an eager operator, and an assignment is
//! read-operate-write with no value.
//!
//! Values have one representation per type, fixed here so the IR and the VM
//! agree (see the language specification's behavior-runtime section):
//! integers are 64-bit patterns (signed ones sign-extended), floats are `f64`
//! (an `F32` result is rounded to `f32` after every operation), `Duration` is in
//! seconds, `Angle` in degrees and `Frequency` in hertz. `Option<T>` is `nil` for
//! `None` and the bare `T` for `Some`, so `Option<Option<T>>` has no
//! representation and is not lowered. A unit enum variant is its tag; a payload
//! variant, a record, a tuple and a range are aggregates.

pub use viso_behavior::NativeImport;

use viso_behavior::game::InputSchema;
use viso_behavior::native::NativeId;
use viso_behavior::{PersistSlot, TaskPolicy};
use viso_ui::adaptive::EnvField;

use super::probe::Probe;
use crate::resolve::SymbolId;
use crate::syntax::TextRange;

/// A register of a function's frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Reg(pub u32);

/// A function of a [`Program`], by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FuncId(pub u32);

/// The machine type a typed operator works at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Num {
    /// `I8`.
    I8,
    /// `I16`.
    I16,
    /// `I32`.
    I32,
    /// `I64`.
    I64,
    /// `U8`.
    U8,
    /// `U16`.
    U16,
    /// `U32` (also `Char` in comparisons).
    U32,
    /// `U64`.
    U64,
    /// `F32`.
    F32,
    /// `F64` and every dimensional scalar.
    F64,
}

impl Num {
    /// Whether this is a float type.
    pub fn is_float(self) -> bool {
        matches!(self, Num::F32 | Num::F64)
    }

    /// Whether this is a signed integer type.
    pub fn is_signed(self) -> bool {
        matches!(self, Num::I8 | Num::I16 | Num::I32 | Num::I64)
    }

    /// The type's name in dumps.
    pub fn name(self) -> &'static str {
        match self {
            Num::I8 => "i8",
            Num::I16 => "i16",
            Num::I32 => "i32",
            Num::I64 => "i64",
            Num::U8 => "u8",
            Num::U16 => "u16",
            Num::U32 => "u32",
            Num::U64 => "u64",
            Num::F32 => "f32",
            Num::F64 => "f64",
        }
    }
}

/// A constant operand.
#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    /// The unit value `()`.
    Unit,
    /// A `Bool`.
    Bool(bool),
    /// An integer, already in range for its type.
    Int(i128),
    /// A float or dimensional scalar, in its canonical unit.
    Float(f64),
    /// A `Char`.
    Char(char),
    /// A `String`.
    Str(String),
    /// A `Color`, as packed RGBA8 (`0xRRGGBBAA`).
    Color(u32),
    /// `None`.
    Nil,
    /// A unit enum variant, by index.
    Tag(u32),
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Arithmetic negation.
    Neg(Num),
    /// Logical `!` on a `Bool`.
    Not,
    /// Bitwise complement.
    BitNot(Num),
}

/// A binary operator. `&&` and `||` are not operators here: they lower to
/// branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `+`.
    Add(Num),
    /// `-`.
    Sub(Num),
    /// `*`.
    Mul(Num),
    /// `/`.
    Div(Num),
    /// `%`.
    Rem(Num),
    /// `&` (also `Bool` conjunction without short-circuit, at `u64`).
    BitAnd(Num),
    /// `|`.
    BitOr(Num),
    /// `^`.
    BitXor(Num),
    /// `<<`.
    Shl(Num),
    /// `>>` (arithmetic for signed types).
    Shr(Num),
    /// `<`.
    Lt(Num),
    /// `<=`.
    Le(Num),
    /// `>`.
    Gt(Num),
    /// `>=`.
    Ge(Num),
    /// Structural `==`; floats compare by IEEE rules.
    Eq,
    /// Structural `!=`.
    Ne,
    /// `String` concatenation.
    Concat,
}

/// How [`Inst::Display`] renders a value as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayKind {
    /// `true`/`false`.
    Bool,
    /// A signed integer.
    Signed,
    /// An unsigned integer.
    Unsigned,
    /// An `F32`, printed at `f32` precision.
    F32,
    /// An `F64`.
    F64,
    /// A `Char`.
    Char,
    /// A `String`, as is.
    Str,
    /// A dimensional scalar followed by its canonical unit suffix.
    Dimension(&'static str),
}

/// One step of a write path into a value.
#[derive(Debug, Clone, PartialEq)]
pub enum PathStep {
    /// Aggregate field `n`.
    Field(u32),
    /// List element at the index in the register.
    Index(Reg),
}

/// One instruction.
#[derive(Debug, Clone, PartialEq)]
pub enum Inst {
    /// `dst = value`.
    Const { dst: Reg, value: Const },
    /// `dst = src`.
    Move { dst: Reg, src: Reg },
    /// `dst = state[slot]`, reading the action's own pending writes.
    LoadState { dst: Reg, slot: u32 },
    /// `state[slot] = src`, committed when the outermost action ends.
    StoreState { slot: u32, src: Reg },
    /// `dst = input[slot]`.
    LoadInput { dst: Reg, slot: u32 },
    /// `dst = op src`.
    Unary { dst: Reg, op: UnaryOp, src: Reg },
    /// `dst = lhs op rhs`.
    Binary {
        dst: Reg,
        op: BinaryOp,
        lhs: Reg,
        rhs: Reg,
    },
    /// `dst = src as to`, with Rust `as` semantics.
    Cast {
        dst: Reg,
        src: Reg,
        from: Num,
        to: Num,
    },
    /// `dst = func(args..)`.
    Call {
        dst: Reg,
        func: FuncId,
        args: Vec<Reg>,
    },
    /// `dst = callee(args..)` for a closure value.
    CallValue {
        dst: Reg,
        callee: Reg,
        args: Vec<Reg>,
    },
    /// `dst = native(args..)`, `import` indexing the program's native imports;
    /// a method's receiver is its first argument.
    Native {
        dst: Reg,
        import: u32,
        args: Vec<Reg>,
    },
    /// `dst` = a closure over `func` capturing `captures` by value.
    Closure {
        dst: Reg,
        func: FuncId,
        captures: Vec<Reg>,
    },
    /// `dst` = an aggregate of `fields` with variant tag `tag`.
    Make {
        dst: Reg,
        tag: u32,
        fields: Vec<Reg>,
    },
    /// `dst` = a list of `items`.
    List { dst: Reg, items: Vec<Reg> },
    /// `dst = src.index`.
    Field { dst: Reg, src: Reg, index: u32 },
    /// `dst = list[index]`, faulting out of bounds.
    Index { dst: Reg, list: Reg, index: Reg },
    /// `root.path = src`, in place (copy on write).
    SetPath {
        root: Reg,
        path: Vec<PathStep>,
        src: Reg,
    },
    /// `dst = len(src)` of a list, as `I64`.
    Len { dst: Reg, src: Reg },
    /// `dst` = the enum variant tag of `src`.
    Tag { dst: Reg, src: Reg },
    /// `dst = src == None`.
    IsNil { dst: Reg, src: Reg },
    /// Continue at `target`.
    Jump { target: u32 },
    /// Continue at `target` when `cond` is `when`.
    JumpIf { cond: Reg, when: bool, target: u32 },
    /// Continue at `targets[src - base]`, or at `default` outside the table.
    Switch {
        src: Reg,
        base: i64,
        targets: Vec<u32>,
        default: u32,
    },
    /// Return `src`.
    Return { src: Reg },
    /// Queue component event `event` with `args` in parameter order, delivered
    /// when the outermost action commits.
    Emit { event: u32, args: Vec<Reg> },
    /// Start task `task` with `args` when the outermost action commits, in
    /// the task slot `slot` of the instance `instance` under `policy`; `done`
    /// is the closure its value runs (the `success` and `error` handlers),
    /// `cancelled` the one a cancellation runs.
    Start {
        task: FuncId,
        args: Vec<Reg>,
        done: Option<Reg>,
        cancelled: Option<Reg>,
        instance: u32,
        slot: Option<u32>,
        policy: TaskPolicy,
    },
    /// `dst` = the text of `src`.
    Display {
        dst: Reg,
        src: Reg,
        kind: DisplayKind,
    },
    /// `dst` = the concatenation of the `String` parts.
    Concat { dst: Reg, parts: Vec<Reg> },
    /// `dst` = the text of the catalog message `message` (its id, or a
    /// `MessageKey`) for the reader of `locale` (`env.locale`), with `args` in
    /// the message's argument order.
    Translate {
        dst: Reg,
        message: Reg,
        locale: Reg,
        args: Vec<Reg>,
    },
    /// A point no execution reaches (after an exhaustive `match`); reaching it
    /// is an internal fault.
    Unreachable,
}

/// What a function lowers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionKind {
    /// A `fn`.
    Fn,
    /// An `action`: its state writes and emits form one transaction.
    Action,
    /// A closure expression.
    Closure,
    /// A `computed` value.
    Computed,
    /// A `state` initializer.
    StateInit,
    /// An `input` default.
    InputDefault,
    /// A `const` value.
    Const,
    /// A record field default.
    FieldDefault,
    /// A view event handler.
    Handler,
    /// A view region's entry: an arm choice, an iterable or a key.
    RegionEntry,
    /// An `effect` body, returning its cleanup closure or `Nil`.
    Effect,
    /// A `task`, run as a fiber that suspends at each `await`.
    Task,
}

impl FunctionKind {
    /// The kind's name in dumps.
    pub fn name(self) -> &'static str {
        match self {
            FunctionKind::Fn => "fn",
            FunctionKind::Action => "action",
            FunctionKind::Closure => "closure",
            FunctionKind::Computed => "computed",
            FunctionKind::StateInit => "state-init",
            FunctionKind::InputDefault => "input-default",
            FunctionKind::Const => "const",
            FunctionKind::FieldDefault => "field-default",
            FunctionKind::Handler => "handler",
            FunctionKind::RegionEntry => "region-entry",
            FunctionKind::Effect => "effect",
            FunctionKind::Task => "task",
        }
    }
}

/// Why a function has no body to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// What the lowering cannot represent yet.
    pub reason: String,
    /// Where.
    pub at: TextRange,
}

/// A lowered body. `spans` is index-parallel to `insts`: the primary source span
/// of each instruction.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Body {
    /// The frame size.
    pub regs: u32,
    /// The instructions.
    pub insts: Vec<Inst>,
    /// The source span of each instruction.
    pub spans: Vec<TextRange>,
}

/// One function.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    /// The declared name (`Component.member` for a component member, `<closure>`
    /// for a closure).
    pub name: String,
    /// What it lowers.
    pub kind: FunctionKind,
    /// The declaration it lowers, if any (a closure has none).
    pub symbol: Option<SymbolId>,
    /// The index of the module it is declared in (its spans' file).
    pub module: usize,
    /// The number of arguments, arriving in `r0..`.
    pub params: u32,
    /// The registers a closure's captured values arrive in, in capture order.
    pub captures: Vec<Reg>,
    /// The body, or why there is none.
    pub body: Result<Body, Unsupported>,
}

/// A component's runtime layout: its state and input slots, events and members.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentLayout {
    /// The component.
    pub symbol: SymbolId,
    /// Its name.
    pub name: String,
    /// The state names, by slot.
    pub states: Vec<String>,
    /// The input names, by slot.
    pub inputs: Vec<String>,
    /// The event names, by index.
    pub events: Vec<String>,
    /// Each state slot's initializer, if it has one.
    pub state_inits: Vec<Option<FuncId>>,
    /// Each input slot's default, if it has one.
    pub input_defaults: Vec<Option<FuncId>>,
    /// The `fn`/`action`/`computed` members, by name.
    pub members: Vec<(String, FuncId)>,
    /// The view's event handlers and region entries, by the site they are
    /// registered at; a handler's index here is the one its view node names.
    pub handlers: Vec<(Site, FuncId)>,
    /// Each inlined instance a control-flow region mounts, in instance order:
    /// its states start from these entries each time region content mounts it,
    /// not from [`state_inits`](Self::state_inits).
    pub regional: Vec<RegionalStates>,
    /// Each `env` field the view reads, held in a state slot the runtime
    /// fills rather than an initializer.
    pub env: Vec<EnvSlot>,
    /// The component's effects, then those of each inlined instance, in
    /// source order.
    pub effects: Vec<EffectEntry>,
    /// The inlined instances that start tasks, ascending.
    pub starters: Vec<u32>,
    /// Each `@persist` state, in declaration order.
    pub persist: Vec<PersistSlot>,
}

/// An `effect` registered in a component layout: its entries in the handler
/// table and its run policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectEntry {
    /// The component instance it belongs to (`0` for the view's own
    /// component).
    pub instance: u32,
    /// The handler-table index of the region entry computing its dependency
    /// values; `None` without dependencies.
    pub deps: Option<u32>,
    /// The handler-table index of its body.
    pub body: u32,
    /// When it runs.
    pub run: viso_behavior::EffectRun,
    /// What it loads, for a `resource`.
    pub resource: Option<viso_behavior::ResourceLoad>,
}

/// An `env` field a view reads and the state slot that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvSlot {
    /// The state slot.
    pub slot: u32,
    /// The field.
    pub field: EnvField,
    /// The component instance that reads it (`0` for the view's own
    /// component).
    pub instance: u32,
}

/// The states of an inlined instance a control-flow region mounts: each mount
/// of the region content keeps its own values for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionalStates {
    /// The instance.
    pub instance: u32,
    /// The mounted slot of its first state; the others follow in order.
    pub base: u32,
    /// The handler-table index of each state's initializer, by state.
    pub inits: Vec<Option<u32>>,
}

/// Where a view function is registered: the component instance of the
/// mounted view it belongs to (`0` for the view's own component, one per
/// user-component node its view inlines) and the source range of the view
/// item it lowers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Site {
    /// The component instance.
    pub instance: u32,
    /// The view item.
    pub at: TextRange,
    /// Which of the item's entries: `0` for the item's own, else one of the
    /// values its styles give it, numbered from `1`.
    pub part: u32,
}

impl Site {
    /// The site of the item at `at` in the view's own component.
    pub fn own(at: TextRange) -> Site {
        Site {
            instance: 0,
            at,
            part: 0,
        }
    }

    /// The site of style value `part` of the node whose `styles` value is at
    /// `at`, in the view's own component.
    pub fn styled(at: TextRange, part: u32) -> Site {
        Site {
            instance: 0,
            at,
            part,
        }
    }
}

impl ComponentLayout {
    /// The member named `name`.
    pub fn member(&self, name: &str) -> Option<FuncId> {
        self.members.iter().find(|(n, _)| n == name).map(|m| m.1)
    }

    /// The state slot holding the field `field` the instance `instance`
    /// reads.
    pub fn env_slot(&self, instance: u32, field: EnvField) -> Option<u32> {
        self.env
            .iter()
            .find(|e| e.instance == instance && e.field == field)
            .map(|e| e.slot)
    }

    /// The index of the function registered at `site`.
    pub fn handler(&self, site: Site) -> Option<u32> {
        self.handlers
            .iter()
            .position(|(s, _)| *s == site)
            .map(|i| i as u32)
    }
}

/// A `system`: the component layout a scheduler instantiates and the member
/// action implementing each hook of the traits it implements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemLayout {
    /// The system.
    pub symbol: SymbolId,
    /// Its layout, an index into [`Program::components`].
    pub component: u32,
    /// Each hook, by its identity, and the action implementing it, in the
    /// order the traits declare them.
    pub hooks: Vec<(NativeId, FuncId)>,
    /// Each Simulation state a game snapshot captures: its declaration, slot
    /// and type schema hash.
    pub snapshot: Vec<(SymbolId, u32, u64)>,
    /// Each `@local` state, likewise.
    pub locals: Vec<(SymbolId, u32, u64)>,
    /// Each `@probe` state, in declaration order.
    pub probes: Vec<Probe>,
    /// Each `@persist` state, in declaration order.
    pub persist: Vec<PersistSlot>,
}

/// A package's lowered behavior.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Program {
    /// Every function, by [`FuncId`].
    pub functions: Vec<Function>,
    /// Every component's layout, in lowering order; a system's too.
    pub components: Vec<ComponentLayout>,
    /// Every system, in the order a scheduler runs them: `@after`/`@before`
    /// first, then declaration order.
    pub systems: Vec<SystemLayout>,
    /// Every native function a body calls, by import index.
    pub natives: Vec<NativeImport>,
    /// The function computing each defaulted record field, by record and field
    /// index, ascending.
    pub field_defaults: Vec<((SymbolId, u32), FuncId)>,
    /// The package's input schema, from its `InputMap` constant; `None` when
    /// its systems read the default `InputAction` set.
    pub input: Option<InputSchema>,
    /// The ticks a second of its systems' fixed step; `None` for the
    /// default.
    pub tick_rate: Option<u32>,
    /// The `@migrate` functions a persisted value converts by; none unless a
    /// system persists a state.
    pub migrators: Vec<viso_behavior::Migrator>,
    /// The capabilities the package is granted (`[package] capabilities`),
    /// sorted.
    pub capabilities: Vec<String>,
    /// Every `theme` declaration, by name: the constant computing its value.
    pub themes: Vec<(String, FuncId)>,
    /// The package's compiled message catalog, which the module carries when
    /// a body translates.
    pub catalog: Option<viso_behavior::i18n::Catalog>,
}

impl Program {
    /// The function computing the default of field `index` of record `record`.
    pub fn field_default(&self, record: SymbolId, index: u32) -> Option<FuncId> {
        self.field_defaults
            .binary_search_by_key(&(record, index), |&(key, _)| key)
            .ok()
            .map(|at| self.field_defaults[at].1)
    }

    /// The function `id`.
    pub fn function(&self, id: FuncId) -> &Function {
        &self.functions[id.0 as usize]
    }

    /// The component named `name`.
    pub fn component(&self, name: &str) -> Option<&ComponentLayout> {
        self.components.iter().find(|c| c.name == name)
    }
}
