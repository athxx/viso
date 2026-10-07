//! The instruction set.
//!
//! Every [`Op`] is 8 bytes. Registers are `u16` frame offsets. An instruction
//! with a variable number of operands (calls, aggregates, lists, emits, jump
//! tables, write paths, concatenation) keeps them in its chunk's operand table
//! ([`Code::ext`](crate::Code::ext)) and holds the table offset; each such
//! variant documents its operand layout.

/// The machine type a typed operator works at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
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
    /// `U32`.
    U32,
    /// `U64`.
    U64,
    /// `F32`.
    F32,
    /// `F64` and every dimensional scalar.
    F64,
}

impl Num {
    const ALL: [Num; 10] = [
        Num::I8,
        Num::I16,
        Num::I32,
        Num::I64,
        Num::U8,
        Num::U16,
        Num::U32,
        Num::U64,
        Num::F32,
        Num::F64,
    ];

    /// Whether this is a float type.
    #[inline]
    pub fn is_float(self) -> bool {
        matches!(self, Num::F32 | Num::F64)
    }

    /// Whether this is a signed integer type.
    #[inline]
    pub fn is_signed(self) -> bool {
        matches!(self, Num::I8 | Num::I16 | Num::I32 | Num::I64)
    }

    /// The bit width of an integer type (`64` for the floats).
    #[inline]
    pub fn bits(self) -> u32 {
        match self {
            Num::I8 | Num::U8 => 8,
            Num::I16 | Num::U16 => 16,
            Num::I32 | Num::U32 | Num::F32 => 32,
            Num::I64 | Num::U64 | Num::F64 => 64,
        }
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

/// A typed binary arithmetic, bitwise or ordering operator. `>` and `>=` are
/// `Lt`/`Le` with the operands swapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Arith {
    /// `+`.
    Add,
    /// `-`.
    Sub,
    /// `*`.
    Mul,
    /// `/`.
    Div,
    /// `%`.
    Rem,
    /// `&`.
    And,
    /// `|`.
    Or,
    /// `^`.
    Xor,
    /// `<<`.
    Shl,
    /// `>>`, arithmetic for signed types.
    Shr,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
}

impl Arith {
    const ALL: [Arith; 12] = [
        Arith::Add,
        Arith::Sub,
        Arith::Mul,
        Arith::Div,
        Arith::Rem,
        Arith::And,
        Arith::Or,
        Arith::Xor,
        Arith::Shl,
        Arith::Shr,
        Arith::Lt,
        Arith::Le,
    ];

    /// The operator's name in dumps.
    pub fn name(self) -> &'static str {
        match self {
            Arith::Add => "add",
            Arith::Sub => "sub",
            Arith::Mul => "mul",
            Arith::Div => "div",
            Arith::Rem => "rem",
            Arith::And => "and",
            Arith::Or => "or",
            Arith::Xor => "xor",
            Arith::Shl => "shl",
            Arith::Shr => "shr",
            Arith::Lt => "lt",
            Arith::Le => "le",
        }
    }
}

/// An [`Arith`] at a [`Num`], packed into one byte so [`Op::Arith`] stays 8
/// bytes: the operator in the high nibble, the type in the low.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArithOp(u8);

impl ArithOp {
    /// `op` at `num`.
    #[inline]
    pub fn new(op: Arith, num: Num) -> ArithOp {
        ArithOp(((op as u8) << 4) | num as u8)
    }

    /// The operator.
    #[inline]
    pub fn op(self) -> Arith {
        Arith::ALL[usize::from(self.0 >> 4)]
    }

    /// The type.
    #[inline]
    pub fn num(self) -> Num {
        Num::ALL[usize::from(self.0 & 0xf)]
    }
}

impl std::fmt::Debug for ArithOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.op().name(), self.num().name())
    }
}

/// How [`Op::Display`] renders a value as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
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
}

/// One instruction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    /// `dst = consts[index]`.
    Const { dst: u16, index: u32 },
    /// `dst = Int(value)`.
    Int { dst: u16, value: i32 },
    /// `dst = Nil`.
    Nil { dst: u16 },
    /// `dst = src`.
    Move { dst: u16, src: u16 },
    /// `dst = state[slot]`, seeing the transaction's pending writes.
    LoadState { dst: u16, slot: u32 },
    /// `state[slot] = src`, pending until the transaction commits.
    StoreState { src: u16, slot: u32 },
    /// `dst = input[slot]`.
    LoadInput { dst: u16, slot: u32 },
    /// `dst = a + b` at `I64`, faulting on overflow.
    AddI64 { dst: u16, a: u16, b: u16 },
    /// `dst = a - b` at `I64`, faulting on overflow.
    SubI64 { dst: u16, a: u16, b: u16 },
    /// `dst = a * b` at `I64`, faulting on overflow.
    MulI64 { dst: u16, a: u16, b: u16 },
    /// `dst = a < b` at `I64`.
    LtI64 { dst: u16, a: u16, b: u16 },
    /// `dst = a <= b` at `I64`.
    LeI64 { dst: u16, a: u16, b: u16 },
    /// `dst = a + b` at `F64`.
    AddF64 { dst: u16, a: u16, b: u16 },
    /// `dst = a - b` at `F64`.
    SubF64 { dst: u16, a: u16, b: u16 },
    /// `dst = a * b` at `F64`.
    MulF64 { dst: u16, a: u16, b: u16 },
    /// `dst = a / b` at `F64`.
    DivF64 { dst: u16, a: u16, b: u16 },
    /// `dst = a < b` at `F64`.
    LtF64 { dst: u16, a: u16, b: u16 },
    /// `dst = a <= b` at `F64`.
    LeF64 { dst: u16, a: u16, b: u16 },
    /// `dst = a op b` for any other typed operator.
    Arith {
        op: ArithOp,
        dst: u16,
        a: u16,
        b: u16,
    },
    /// Structural `dst = a == b`.
    Eq { dst: u16, a: u16, b: u16 },
    /// Structural `dst = a != b`.
    Ne { dst: u16, a: u16, b: u16 },
    /// `dst = -src`, faulting on integer overflow.
    Neg { num: Num, dst: u16, src: u16 },
    /// `dst = !src` on a `Bool`.
    Not { dst: u16, src: u16 },
    /// `dst = !src`, the bitwise complement.
    BitNot { num: Num, dst: u16, src: u16 },
    /// `dst = src as to`, with Rust `as` semantics (wrapping between
    /// integers, saturating from float to integer).
    Cast {
        from: Num,
        to: Num,
        dst: u16,
        src: u16,
    },
    /// Call a chunk. Operands: `func, argc, args..`.
    Call { dst: u16, ext: u32 },
    /// Call the closure in a register. Operands: `callee, argc, args..`.
    CallValue { dst: u16, ext: u32 },
    /// Call a native function. Operands: `import, argc, args..`, `import`
    /// indexing the module's [`NativeImport`](crate::NativeImport)s.
    Native { dst: u16, ext: u32 },
    /// Create a closure. Operands: `func, n, captures..`.
    Closure { dst: u16, ext: u32 },
    /// Create an aggregate. Operands: `tag, n, fields..`.
    Make { dst: u16, ext: u32 },
    /// Create a list. Operands: `n, items..`.
    List { dst: u16, ext: u32 },
    /// Concatenate strings. Operands: `n, parts..`.
    Concat { dst: u16, ext: u32 },
    /// Format a message of the module's catalog for a reader. Operands:
    /// `message, locale, n, args..`: the registers holding the message id and
    /// the reader's `env.locale`, then its arguments in its order.
    Translate { dst: u16, ext: u32 },
    /// `dst = src.index` of an aggregate.
    Field { dst: u16, src: u16, index: u16 },
    /// `dst = list[index]`, faulting out of bounds.
    Index { dst: u16, list: u16, index: u16 },
    /// Write into a value in place, copying shared parts. Operands: `src, n`,
    /// then `n` steps of `kind, operand`: kind `0` is field `operand`, kind `1`
    /// is the list element at the index in register `operand`.
    SetPath { root: u16, ext: u32 },
    /// `dst = len(src)` of a list.
    Len { dst: u16, src: u16 },
    /// Appends `item` to the list in `list`, in place (copy on write).
    Push { list: u16, item: u16 },
    /// Inserts `item` before position `index` of the list in `list`, in place;
    /// `index` may be the length, and faults beyond it.
    Insert { list: u16, index: u16, item: u16 },
    /// `dst` = the element at `index` of the list in `list`, removed in place;
    /// faults out of bounds.
    Remove { dst: u16, list: u16, index: u16 },
    /// Shortens the list in `list` to `len` elements in place; a list no
    /// longer than `len` stays as it is.
    Truncate { list: u16, len: u16 },
    /// `dst` = the enum variant tag of `src`.
    Tag { dst: u16, src: u16 },
    /// `dst = src is Nil`.
    IsNil { dst: u16, src: u16 },
    /// Continue at `target`.
    Jump { target: u32 },
    /// Continue at `target` when `cond` is true.
    JumpIf { cond: u16, target: u32 },
    /// Continue at `target` when `cond` is false.
    JumpUnless { cond: u16, target: u32 },
    /// Continue at `targets[src - base]`, else at `default`. Operands:
    /// `base (low 32 bits), base (high 32 bits), default, n, targets..`.
    Switch { src: u16, ext: u32 },
    /// Return `src` to the caller.
    Return { src: u16 },
    /// Queue a component event. Operands: `event, n, args..`.
    Emit { ext: u32 },
    /// Queue a task start, run once the transaction commits. Operands:
    /// `func, argc, args.., done, cancelled, instance, slot, policy`: each
    /// handler is the register of its closure and the slot the instance's
    /// task slot, `u32::MAX` when absent; the policy is a
    /// [`TaskPolicy::word`](crate::TaskPolicy::word).
    Start { ext: u32 },
    /// `dst` = the text of `src`.
    Display {
        kind: DisplayKind,
        dst: u16,
        src: u16,
    },
    /// `dst` = the text of the dimensional scalar `src` followed by the unit
    /// suffix in `consts[suffix]`.
    DisplayDim { dst: u16, src: u16, suffix: u16 },
    /// A point no execution reaches.
    Unreachable,
}

const _: () = assert!(std::mem::size_of::<Op>() == 8);
