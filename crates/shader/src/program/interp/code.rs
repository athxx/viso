//! A program compiled for the CPU: every value in registers at places fixed
//! when it compiles, every expression a flat list of operations on all four
//! lanes of a register at once, statements kept as the structured control
//! flow the lanes' masks run through.
//!
//! A register holds four 32-bit components of four lanes, each component the
//! bits of its scalar: a scalar or vector takes one, a matrix one a column,
//! a record its fields' in order. Shaders do not recurse, so each function's
//! parameters, locals, return value and temporaries take registers of their
//! own for the whole program, and a call needs no frame.

use super::rt;
use crate::program::layout::BlockLayout;
use crate::program::{
    BinaryOp, Binding, Block, Builtin, Expr, ExprKind, Function, Intrinsic, Loop, Place, Program,
    Root, Scalar, Stmt, Texel, Ty, UnaryOp,
};

/// A register index.
pub(super) type Reg = u32;

/// One operation on every lane of its registers; components past `n` are
/// written zero.
#[derive(Debug, Clone, Copy)]
pub(super) enum Op {
    /// The same components in every lane.
    Const {
        dst: Reg,
        bits: [u32; 4],
    },
    /// `count` registers from `src` to `dst`.
    Copy {
        dst: Reg,
        src: Reg,
        count: u32,
    },
    /// Components `at..at + n` of `src` into `dst` from component `to`, the
    /// rest of `dst` kept.
    Place {
        dst: Reg,
        to: u8,
        src: Reg,
        at: u8,
        n: u8,
    },
    /// `dst` cleared, for a construction to place into.
    Clear {
        dst: Reg,
    },
    /// Component 0 of `src` in each of the first `n`.
    Splat {
        dst: Reg,
        src: Reg,
        n: u8,
    },
    Swizzle {
        dst: Reg,
        src: Reg,
        lanes: [u8; 4],
        n: u8,
    },
    Unary {
        kind: Un,
        dst: Reg,
        src: Reg,
        n: u8,
    },
    /// `kind` component by component; a one-component operand (`a1`, `b1`)
    /// is spread over the other's.
    Binary {
        kind: Bin,
        dst: Reg,
        a: Reg,
        b: Reg,
        n: u8,
        a1: bool,
        b1: bool,
    },
    /// `a * b`, two `n`×`n` matrices.
    MatMul {
        n: u8,
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// `m * v`.
    MatVec {
        n: u8,
        dst: Reg,
        m: Reg,
        v: Reg,
    },
    /// `v * m`.
    VecMat {
        n: u8,
        dst: Reg,
        v: Reg,
        m: Reg,
    },
    /// `a + b`, or `a - b`.
    MatAdd {
        n: u8,
        dst: Reg,
        a: Reg,
        b: Reg,
        sub: bool,
    },
    /// `m * s`, a scalar.
    MatScale {
        n: u8,
        dst: Reg,
        m: Reg,
        s: Reg,
    },
    /// `-m`.
    MatNeg {
        n: u8,
        dst: Reg,
        m: Reg,
    },
    Transpose {
        n: u8,
        dst: Reg,
        m: Reg,
    },
    Convert {
        dst: Reg,
        src: Reg,
        from: Scalar,
        to: Scalar,
        n: u8,
    },
    /// Component `index` of `src`, clamped to its `len`.
    IndexLane {
        dst: Reg,
        src: Reg,
        index: Reg,
        len: u8,
    },
    /// Column `index` of the matrix at `base`, clamped to its `n`.
    IndexColumn {
        dst: Reg,
        base: Reg,
        index: Reg,
        n: u8,
    },
    Math1 {
        f: fn(f32) -> f32,
        /// `f`'s name in [`rt::math`](super::rt::math).
        name: &'static str,
        dst: Reg,
        src: Reg,
        n: u8,
    },
    Math2 {
        f: fn(f32, f32) -> f32,
        /// `f`'s name in [`rt::math`](super::rt::math).
        name: &'static str,
        dst: Reg,
        a: Reg,
        b: Reg,
        n: u8,
        a1: bool,
        b1: bool,
    },
    /// `clamp(x, lo, hi)` of `scalar` lanes.
    Clamp {
        scalar: Scalar,
        dst: Reg,
        x: Reg,
        lo: Reg,
        hi: Reg,
        n: u8,
        lo1: bool,
        hi1: bool,
    },
    Mix {
        dst: Reg,
        a: Reg,
        b: Reg,
        t: Reg,
        n: u8,
        t1: bool,
    },
    Smoothstep {
        dst: Reg,
        e0: Reg,
        e1: Reg,
        x: Reg,
        n: u8,
        e01: bool,
        e11: bool,
    },
    Dot {
        dst: Reg,
        a: Reg,
        b: Reg,
        n: u8,
    },
    Length {
        dst: Reg,
        src: Reg,
        n: u8,
    },
    Distance {
        dst: Reg,
        a: Reg,
        b: Reg,
        n: u8,
    },
    Normalize {
        dst: Reg,
        src: Reg,
        n: u8,
    },
    Cross {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// A fine derivative across the quad.
    Derivative {
        kind: Derivative,
        dst: Reg,
        src: Reg,
        n: u8,
    },
    Sample {
        dst: Reg,
        texture: u32,
        sampler: u32,
        uv: Reg,
        /// Whether the texture is `Texture2D<F32>`.
        scalar: bool,
    },
    QuadVertex {
        dst: Reg,
        src: Reg,
    },
    ToClip {
        dst: Reg,
        pos: Reg,
        viewport: Reg,
    },
    RoundedRectSdf {
        dst: Reg,
        p: Reg,
        size: Reg,
        radius: Reg,
    },
    /// Calls function `function`: `args` of [`Code::args`] into its
    /// parameters, then its value, `width` registers, into `dst`.
    Call {
        function: u32,
        args: (u32, u32),
        dst: Reg,
        width: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Un {
    NegF,
    NegI,
    Not,
    BitNot,
    AbsF,
    AbsI,
    Sign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bin {
    FAdd,
    FSub,
    FMul,
    FDiv,
    FRem,
    FMin,
    FMax,
    FStep,
    IAdd,
    ISub,
    IMul,
    IDiv,
    IRem,
    IMin,
    IMax,
    UAdd,
    USub,
    UMul,
    UDiv,
    URem,
    UMin,
    UMax,
    And,
    Or,
    Xor,
    Shl,
    /// `>>` of `I32` lanes: arithmetic.
    IShr,
    UShr,
    /// A comparison of `F32` lanes.
    FCmp(Cmp),
    ICmp(Cmp),
    UCmp(Cmp),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derivative {
    X,
    Y,
    Width,
}

/// An expression compiled: its operations in [`Code::ops`] and the register
/// its value is in.
#[derive(Debug, Clone, Copy)]
pub(super) struct Run {
    pub ops: (u32, u32),
    pub value: Reg,
}

/// Where an assignment writes.
#[derive(Debug, Clone, Copy)]
pub(super) enum Target {
    /// Whole registers.
    Regs { reg: Reg, count: u32 },
    /// `n` components from `at` of one register.
    Components { reg: Reg, at: u8, n: u8 },
}

#[derive(Debug, Clone)]
pub(super) enum CStmt {
    Let {
        run: Run,
        dst: Reg,
        count: u32,
    },
    /// Binds registers to zero.
    Declare {
        dst: Reg,
        count: u32,
    },
    Assign {
        run: Run,
        target: Target,
    },
    If {
        cond: Run,
        then: Vec<CStmt>,
        otherwise: Vec<CStmt>,
    },
    For(Box<CLoop>),
    Break,
    Continue,
    Return {
        run: Option<Run>,
        count: u32,
    },
    Discard,
}

#[derive(Debug, Clone)]
pub(super) struct CLoop {
    pub var: Reg,
    pub signed: bool,
    pub start: Run,
    pub end: Run,
    pub inclusive: bool,
    pub max: u32,
    pub guarded: bool,
    pub body: Vec<CStmt>,
}

/// A function compiled.
#[derive(Debug, Clone)]
pub(super) struct CFunction {
    /// Each parameter's first register and its width.
    pub params: Vec<(Reg, u32)>,
    /// What each parameter of an entry receives.
    pub builtins: Vec<Builtin>,
    /// Its value's first register and width.
    pub ret: Reg,
    pub ret_width: u32,
    pub body: Vec<CStmt>,
}

/// A program compiled.
#[derive(Debug, Clone, Default)]
pub(super) struct Code {
    /// How many registers it takes.
    pub regs: u32,
    pub ops: Vec<Op>,
    /// The arguments of calls: each the register and width of a value, and
    /// the parameter register it goes to.
    pub args: Vec<(Reg, Reg, u32)>,
    pub functions: Vec<CFunction>,
    pub vertex: Option<CFunction>,
    pub fragment: Option<CFunction>,
    /// Each uniform member's first register.
    pub uniforms: Vec<Reg>,
    /// Each instance member's first register.
    pub instance: Vec<Reg>,
    /// Each varying's register, and whether it interpolates (an `F32` one).
    pub varyings: Vec<(Reg, bool)>,
    /// Each varying's component count.
    pub varying_lanes: Vec<u8>,
}

/// A word of a uniform block or instance record and the register component
/// it binds.
#[derive(Debug, Clone, Copy)]
pub(super) struct Word {
    pub reg: Reg,
    pub component: u8,
    /// Its byte offset in the block.
    pub offset: u32,
    /// Whether it is a `Bool`, any word but zero true.
    pub boolean: bool,
}

/// The words of `layout`, a block of `members` placed at `regs`.
pub(super) fn words(
    p: &Program,
    members: &[Binding],
    regs: &[Reg],
    layout: &BlockLayout,
) -> Vec<Word> {
    let mut words = Vec::new();
    for field in &layout.fields {
        let mut reg = regs[field.member as usize];
        let mut ty = members[field.member as usize].ty;
        for &step in &field.path {
            let fields = p.fields(ty).unwrap_or_default();
            reg += fields[..step as usize]
                .iter()
                .map(|(_, t)| width(p, *t))
                .sum::<u32>();
            ty = fields[step as usize].1;
        }
        match field.ty {
            Ty::Matrix(n) => {
                for c in 0..u32::from(n) {
                    for r in 0..n {
                        words.push(Word {
                            reg: reg + c,
                            component: r,
                            offset: field.offset + c * field.matrix_stride + 4 * u32::from(r),
                            boolean: false,
                        });
                    }
                }
            }
            ty => {
                let (scalar, n) = shape(ty);
                for k in 0..n {
                    words.push(Word {
                        reg,
                        component: k,
                        offset: field.offset + 4 * u32::from(k),
                        boolean: scalar == Scalar::Bool,
                    });
                }
            }
        }
    }
    words
}

/// How many registers a value of `ty` takes.
pub(super) fn width(p: &Program, ty: Ty) -> u32 {
    match ty {
        Ty::Scalar(_) | Ty::Vector(..) | Ty::Color | Ty::VertexOutput => 1,
        Ty::Matrix(n) => u32::from(n),
        Ty::Record(_) => p
            .fields(ty)
            .unwrap_or_default()
            .into_iter()
            .map(|(_, t)| width(p, t))
            .sum(),
        Ty::Unit | Ty::Texture(_) | Ty::Sampler => 0,
    }
}

/// The scalar and component count of a register holding `ty`.
fn shape(ty: Ty) -> (Scalar, u8) {
    match ty {
        Ty::Color => (Scalar::F32, 4),
        Ty::Matrix(n) => (Scalar::F32, n),
        Ty::VertexOutput => (Scalar::F32, 4),
        other => other.lanes().unwrap_or((Scalar::F32, 1)),
    }
}

/// What an expression compiles to.
#[derive(Debug, Clone, Copy)]
enum Operand {
    Reg(Reg),
    Texture(u32),
    Sampler(u32),
}

impl Operand {
    fn reg(self) -> Reg {
        match self {
            Operand::Reg(r) => r,
            _ => 0,
        }
    }
}

/// Compiles `p`, a validated program.
pub(super) fn compile(p: &Program) -> Code {
    let mut c = Compiler {
        p,
        next: 0,
        code: Code {
            regs: 0,
            ops: Vec::new(),
            args: Vec::new(),
            functions: Vec::new(),
            vertex: None,
            fragment: None,
            uniforms: Vec::new(),
            instance: Vec::new(),
            varyings: Vec::new(),
            varying_lanes: Vec::new(),
        },
        locals: Vec::new(),
        current: None,
        params: Vec::new(),
    };
    for binding in &p.uniforms {
        let reg = c.alloc(width(p, binding.ty));
        c.code.uniforms.push(reg);
    }
    for binding in &p.instance {
        let reg = c.alloc(width(p, binding.ty));
        c.code.instance.push(reg);
    }
    for binding in &p.varyings {
        let reg = c.alloc(1);
        c.code.varyings.push((reg, binding.ty.is_float()));
        c.code.varying_lanes.push(shape(binding.ty).1);
    }
    // Every function's parameters are placed before any body compiles, so
    // a call knows where its arguments go.
    c.params = p
        .functions
        .iter()
        .map(|f| {
            f.locals[..f.params as usize]
                .iter()
                .map(|l| {
                    let w = width(p, l.ty);
                    (c.alloc(w), w)
                })
                .collect()
        })
        .collect();
    for (index, f) in p.functions.iter().enumerate() {
        let params = c.params[index].clone();
        let compiled = c.function(f, params);
        c.code.functions.push(compiled);
    }
    if let Some(f) = &p.vertex {
        let params = c.entry_params(f);
        c.code.vertex = Some(c.function(f, params));
    }
    if let Some(f) = &p.fragment {
        let params = c.entry_params(f);
        c.code.fragment = Some(c.function(f, params));
    }
    c.code.regs = c.next;
    c.code
}

struct Compiler<'p> {
    p: &'p Program,
    next: Reg,
    code: Code,
    /// The current function's locals: first register and width.
    locals: Vec<(Reg, u32)>,
    /// The function being compiled.
    current: Option<&'p Function>,
    /// Each function's parameters.
    params: Vec<Vec<(Reg, u32)>>,
}

impl<'p> Compiler<'p> {
    fn alloc(&mut self, count: u32) -> Reg {
        let reg = self.next;
        self.next += count.max(1);
        reg
    }

    fn entry_params(&mut self, f: &Function) -> Vec<(Reg, u32)> {
        f.locals[..f.params as usize]
            .iter()
            .map(|l| {
                let w = width(self.p, l.ty);
                (self.alloc(w), w)
            })
            .collect()
    }

    fn function(&mut self, f: &'p Function, params: Vec<(Reg, u32)>) -> CFunction {
        self.current = Some(f);
        self.locals = params.clone();
        for local in &f.locals[f.params as usize..] {
            let w = width(self.p, local.ty);
            let reg = self.alloc(w);
            self.locals.push((reg, w));
        }
        let ret_width = width(self.p, f.ret);
        let ret = self.alloc(ret_width);
        let body = self.block(&f.body);
        CFunction {
            params,
            builtins: f.builtins.clone(),
            ret,
            ret_width,
            body,
        }
    }

    fn block(&mut self, block: &Block) -> Vec<CStmt> {
        block.0.iter().map(|s| self.stmt(s)).collect()
    }

    fn stmt(&mut self, stmt: &Stmt) -> CStmt {
        match stmt {
            Stmt::Let(local, value) => {
                let (dst, count) = self.locals[*local as usize];
                CStmt::Let {
                    run: self.run(value),
                    dst,
                    count,
                }
            }
            Stmt::Declare(local) => {
                let (dst, count) = self.locals[*local as usize];
                CStmt::Declare { dst, count }
            }
            Stmt::Assign(place, value) => CStmt::Assign {
                run: self.run(value),
                target: self.target(place),
            },
            Stmt::If(cond, then, otherwise) => CStmt::If {
                cond: self.run(cond),
                then: self.block(then),
                otherwise: self.block(otherwise),
            },
            Stmt::For(l) => CStmt::For(Box::new(self.for_loop(l))),
            Stmt::Break => CStmt::Break,
            Stmt::Continue => CStmt::Continue,
            Stmt::Return(value) => CStmt::Return {
                run: value.as_ref().map(|e| self.run(e)),
                count: value.as_ref().map_or(0, |e| width(self.p, e.ty)),
            },
            Stmt::Discard => CStmt::Discard,
        }
    }

    fn for_loop(&mut self, l: &Loop) -> CLoop {
        let (var, _) = self.locals[l.var as usize];
        let signed = matches!(l.start.ty.scalar(), Some(Scalar::I32));
        CLoop {
            var,
            signed,
            start: self.run(&l.start),
            end: self.run(&l.end),
            inclusive: l.inclusive,
            max: l.max,
            guarded: l.guarded,
            body: self.block(&l.body),
        }
    }

    /// Where `place` writes: its root's registers, narrowed by its path.
    fn target(&mut self, place: &Place) -> Target {
        let (mut reg, mut ty) = match place.root {
            Root::Local(l) => {
                let (reg, _) = self.locals[l as usize];
                (reg, self.local_ty(l))
            }
            Root::Varying(v) => (
                self.code.varyings[v as usize].0,
                self.p.varyings[v as usize].ty,
            ),
        };
        for &step in &place.path {
            match ty {
                Ty::Vector(..) | Ty::Color => {
                    return Target::Components {
                        reg,
                        at: step as u8,
                        n: 1,
                    };
                }
                Ty::Matrix(n) => {
                    reg += step;
                    ty = Ty::Vector(Scalar::F32, n);
                }
                _ => {
                    let fields = self.p.fields(ty).unwrap_or_default();
                    let offset: u32 = fields[..step as usize]
                        .iter()
                        .map(|(_, t)| width(self.p, *t))
                        .sum();
                    reg += offset;
                    ty = fields[step as usize].1;
                }
            }
        }
        Target::Regs {
            reg,
            count: width(self.p, ty),
        }
    }

    fn local_ty(&self, local: u32) -> Ty {
        self.current
            .and_then(|f| f.locals.get(local as usize))
            .map_or(Ty::F32, |l| l.ty)
    }

    /// `e` compiled as one run of operations.
    fn run(&mut self, e: &Expr) -> Run {
        let start = self.code.ops.len() as u32;
        let value = self.expr(e).reg();
        Run {
            ops: (start, self.code.ops.len() as u32),
            value,
        }
    }

    fn push(&mut self, op: Op) {
        self.code.ops.push(op);
    }

    /// A fresh register for a value of `ty`, the operations to fill it
    /// pushed by the caller.
    fn temp(&mut self, ty: Ty) -> Reg {
        let w = width(self.p, ty);
        self.alloc(w)
    }

    fn reg(&mut self, e: &Expr) -> Reg {
        self.expr(e).reg()
    }

    fn expr(&mut self, e: &Expr) -> Operand {
        let p = self.p;
        match &e.kind {
            ExprKind::Bool(b) => self.constant([u32::from(*b), 0, 0, 0]),
            ExprKind::I32(v) => self.constant([v.cast_unsigned(), 0, 0, 0]),
            ExprKind::U32(v) => self.constant([*v, 0, 0, 0]),
            ExprKind::F32(v) => self.constant([v.to_bits(), 0, 0, 0]),
            ExprKind::Local(l) => Operand::Reg(self.locals[*l as usize].0),
            ExprKind::Uniform(i) => Operand::Reg(self.code.uniforms[*i as usize]),
            ExprKind::Instance(i) => Operand::Reg(self.code.instance[*i as usize]),
            ExprKind::Varying(v) => Operand::Reg(self.code.varyings[*v as usize].0),
            ExprKind::Texture(t) => Operand::Texture(*t),
            ExprKind::Sampler(s) => Operand::Sampler(*s),
            ExprKind::Unary(op, a) => {
                let src = self.reg(a);
                let dst = self.temp(e.ty);
                match (op, a.ty) {
                    (UnaryOp::Neg, Ty::Matrix(n)) => self.push(Op::MatNeg { n, dst, m: src }),
                    (op, ty) => {
                        let (scalar, n) = shape(ty);
                        let kind = match (op, scalar) {
                            (UnaryOp::Neg, Scalar::F32) => Un::NegF,
                            (UnaryOp::Neg, _) => Un::NegI,
                            (UnaryOp::Not, _) => Un::Not,
                            (UnaryOp::BitNot, _) => Un::BitNot,
                        };
                        self.push(Op::Unary { kind, dst, src, n });
                    }
                }
                Operand::Reg(dst)
            }
            ExprKind::Binary(op, a, b) => Operand::Reg(self.binary(*op, a, b, e.ty)),
            ExprKind::Construct(parts) => Operand::Reg(self.construct(e.ty, parts)),
            ExprKind::Swizzle(base, lanes) => {
                let src = self.reg(base);
                let dst = self.temp(e.ty);
                let n = shape(e.ty).1;
                self.push(Op::Swizzle {
                    dst,
                    src,
                    lanes: *lanes,
                    n,
                });
                Operand::Reg(dst)
            }
            ExprKind::Member(base, field) => {
                let base_reg = self.reg(base);
                match base.ty {
                    Ty::VertexOutput => Operand::Reg(base_reg),
                    ty => {
                        let fields = p.fields(ty).unwrap_or_default();
                        let offset: u32 = fields[..*field as usize]
                            .iter()
                            .map(|(_, t)| width(p, *t))
                            .sum();
                        Operand::Reg(base_reg + offset)
                    }
                }
            }
            ExprKind::Index(base, index) => {
                let index = self.reg(index);
                let src = self.reg(base);
                let dst = self.temp(e.ty);
                match base.ty {
                    Ty::Matrix(n) => self.push(Op::IndexColumn {
                        dst,
                        base: src,
                        index,
                        n,
                    }),
                    ty => self.push(Op::IndexLane {
                        dst,
                        src,
                        index,
                        len: shape(ty).1,
                    }),
                }
                Operand::Reg(dst)
            }
            ExprKind::Call(callee, args) => {
                let values: Vec<(Reg, u32)> =
                    args.iter().map(|a| (self.reg(a), width(p, a.ty))).collect();
                let start = self.code.args.len() as u32;
                let params = self.params[*callee as usize].clone();
                for ((src, w), (param, _)) in values.into_iter().zip(params) {
                    self.code.args.push((src, param, w));
                }
                let end = self.code.args.len() as u32;
                let width = width(p, e.ty);
                let dst = self.alloc(width);
                self.push(Op::Call {
                    function: *callee,
                    args: (start, end),
                    dst,
                    width,
                });
                Operand::Reg(dst)
            }
            ExprKind::Intrinsic(i, args) => self.intrinsic(*i, args, e.ty),
            ExprKind::Convert(a) => {
                let src = self.reg(a);
                let dst = self.temp(e.ty);
                let (from, n) = shape(a.ty);
                let (to, _) = shape(e.ty);
                self.push(Op::Convert {
                    dst,
                    src,
                    from,
                    to,
                    n,
                });
                Operand::Reg(dst)
            }
        }
    }

    fn constant(&mut self, bits: [u32; 4]) -> Operand {
        let dst = self.alloc(1);
        self.push(Op::Const { dst, bits });
        Operand::Reg(dst)
    }

    fn binary(&mut self, op: BinaryOp, a: &Expr, b: &Expr, ty: Ty) -> Reg {
        use BinaryOp as B;
        let (ra, rb) = (self.reg(a), self.reg(b));
        let dst = self.temp(ty);
        match (op, a.ty, b.ty) {
            (B::Mul, Ty::Matrix(n), Ty::Matrix(_)) => {
                self.push(Op::MatMul {
                    n,
                    dst,
                    a: ra,
                    b: rb,
                });
                return dst;
            }
            (B::Mul, Ty::Matrix(n), Ty::Vector(..)) => {
                self.push(Op::MatVec {
                    n,
                    dst,
                    m: ra,
                    v: rb,
                });
                return dst;
            }
            (B::Mul, Ty::Vector(..), Ty::Matrix(n)) => {
                self.push(Op::VecMat {
                    n,
                    dst,
                    v: ra,
                    m: rb,
                });
                return dst;
            }
            (B::Add | B::Sub, Ty::Matrix(n), Ty::Matrix(_)) => {
                self.push(Op::MatAdd {
                    n,
                    dst,
                    a: ra,
                    b: rb,
                    sub: op == B::Sub,
                });
                return dst;
            }
            (B::Mul, Ty::Matrix(n), _) => {
                self.push(Op::MatScale {
                    n,
                    dst,
                    m: ra,
                    s: rb,
                });
                return dst;
            }
            (B::Mul, _, Ty::Matrix(n)) => {
                self.push(Op::MatScale {
                    n,
                    dst,
                    m: rb,
                    s: ra,
                });
                return dst;
            }
            _ => {}
        }
        let (scalar, na) = shape(a.ty);
        let nb = shape(b.ty).1;
        let n = na.max(nb);
        let cmp = |c| match scalar {
            Scalar::F32 => Bin::FCmp(c),
            Scalar::I32 => Bin::ICmp(c),
            Scalar::U32 | Scalar::Bool => Bin::UCmp(c),
        };
        let kind = match (op, scalar) {
            (B::Add, Scalar::F32) => Bin::FAdd,
            (B::Sub, Scalar::F32) => Bin::FSub,
            (B::Mul, Scalar::F32) => Bin::FMul,
            (B::Div, Scalar::F32) => Bin::FDiv,
            (B::Rem, Scalar::F32) => Bin::FRem,
            (B::Add, Scalar::I32) => Bin::IAdd,
            (B::Sub, Scalar::I32) => Bin::ISub,
            (B::Mul, Scalar::I32) => Bin::IMul,
            (B::Div, Scalar::I32) => Bin::IDiv,
            (B::Rem, Scalar::I32) => Bin::IRem,
            (B::Add, _) => Bin::UAdd,
            (B::Sub, _) => Bin::USub,
            (B::Mul, _) => Bin::UMul,
            (B::Div, _) => Bin::UDiv,
            (B::Rem, _) => Bin::URem,
            (B::BitAnd | B::And, _) => Bin::And,
            (B::BitOr | B::Or, _) => Bin::Or,
            (B::BitXor, _) => Bin::Xor,
            (B::Shl, _) => Bin::Shl,
            (B::Shr, Scalar::I32) => Bin::IShr,
            (B::Shr, _) => Bin::UShr,
            (B::Eq, _) => cmp(Cmp::Eq),
            (B::Ne, _) => cmp(Cmp::Ne),
            (B::Lt, _) => cmp(Cmp::Lt),
            (B::Le, _) => cmp(Cmp::Le),
            (B::Gt, _) => cmp(Cmp::Gt),
            (B::Ge, _) => cmp(Cmp::Ge),
        };
        self.push(Op::Binary {
            kind,
            dst,
            a: ra,
            b: rb,
            n,
            a1: na == 1 && n > 1,
            b1: nb == 1 && n > 1,
        });
        dst
    }

    fn construct(&mut self, ty: Ty, parts: &[Expr]) -> Reg {
        let p = self.p;
        let regs: Vec<(Reg, Ty)> = parts.iter().map(|e| (self.reg(e), e.ty)).collect();
        let dst = self.temp(ty);
        match ty {
            Ty::Record(_) | Ty::VertexOutput => {
                let mut at = dst;
                for (src, part) in regs {
                    let count = width(p, part);
                    self.push(Op::Copy {
                        dst: at,
                        src,
                        count,
                    });
                    at += count;
                }
            }
            Ty::Matrix(n) => {
                for column in 0..u32::from(n) {
                    self.push(Op::Clear { dst: dst + column });
                }
                let mut at = 0u32;
                let n32 = u32::from(n);
                for (src, part) in regs {
                    let len = shape(part).1;
                    if len > 1 && u32::from(len) == n32 && at.is_multiple_of(n32) {
                        if at < n32 * n32 {
                            self.push(Op::Copy {
                                dst: dst + at / n32,
                                src,
                                count: 1,
                            });
                        }
                        at += n32;
                        continue;
                    }
                    for k in 0..len {
                        if at < n32 * n32 {
                            self.push(Op::Place {
                                dst: dst + at / n32,
                                to: (at % n32) as u8,
                                src,
                                at: k,
                                n: 1,
                            });
                        }
                        at += 1;
                    }
                }
            }
            _ => {
                let n = shape(ty).1;
                if let [(src, part)] = regs[..]
                    && shape(part).1 == 1
                {
                    // One scalar fills every lane.
                    self.push(Op::Splat { dst, src, n });
                    return dst;
                }
                self.push(Op::Clear { dst });
                let mut at = 0u8;
                for (src, part) in regs {
                    let len = shape(part).1;
                    let take = len.min(n.saturating_sub(at));
                    if take > 0 {
                        self.push(Op::Place {
                            dst,
                            to: at,
                            src,
                            at: 0,
                            n: take,
                        });
                    }
                    at = at.saturating_add(len);
                }
            }
        }
        dst
    }

    fn intrinsic(&mut self, i: Intrinsic, args: &[Expr], ty: Ty) -> Operand {
        use Intrinsic as I;
        let ops: Vec<Operand> = args.iter().map(|a| self.expr(a)).collect();
        let reg = |k: usize| ops[k].reg();
        let n = shape(ty).1;
        let one = |k: usize| shape(args[k].ty).1 == 1 && n > 1;
        let dst = self.temp(ty);
        let math1 = |name: &'static str, f: fn(f32) -> f32| Op::Math1 {
            f,
            name,
            dst,
            src: reg(0),
            n,
        };
        let math2 = |name: &'static str, f: fn(f32, f32) -> f32| Op::Math2 {
            f,
            name,
            dst,
            a: reg(0),
            b: reg(1),
            n,
            a1: one(0),
            b1: one(1),
        };
        let scalar = shape(ty).0;
        let op = match i {
            I::Abs => Op::Unary {
                kind: if scalar == Scalar::F32 {
                    Un::AbsF
                } else if scalar == Scalar::I32 {
                    Un::AbsI
                } else {
                    return Operand::Reg(reg(0));
                },
                dst,
                src: reg(0),
                n,
            },
            I::Sign => Op::Unary {
                kind: Un::Sign,
                dst,
                src: reg(0),
                n,
            },
            I::Floor => math1("floor", rt::math::floor),
            I::Ceil => math1("ceil", rt::math::ceil),
            I::Round => math1("round", rt::math::round),
            I::Trunc => math1("trunc", rt::math::trunc),
            I::Fract => math1("fract", rt::math::fract),
            I::Sqrt => math1("sqrt", rt::math::sqrt),
            I::InverseSqrt => math1("inverse_sqrt", rt::math::inverse_sqrt),
            I::Exp => math1("exp", rt::math::exp),
            I::Exp2 => math1("exp2", rt::math::exp2),
            I::Log => math1("log", rt::math::log),
            I::Log2 => math1("log2", rt::math::log2),
            I::Sin => math1("sin", rt::math::sin),
            I::Cos => math1("cos", rt::math::cos),
            I::Tan => math1("tan", rt::math::tan),
            I::Asin => math1("asin", rt::math::asin),
            I::Acos => math1("acos", rt::math::acos),
            I::Atan => math1("atan", rt::math::atan),
            I::Sinh => math1("sinh", rt::math::sinh),
            I::Cosh => math1("cosh", rt::math::cosh),
            I::Tanh => math1("tanh", rt::math::tanh),
            I::Radians => math1("radians", rt::math::radians),
            I::Degrees => math1("degrees", rt::math::degrees),
            I::Atan2 => math2("atan2", rt::math::atan2),
            I::Pow => math2("pow", rt::math::pow),
            I::Step => Op::Binary {
                kind: Bin::FStep,
                dst,
                a: reg(0),
                b: reg(1),
                n,
                a1: one(0),
                b1: false,
            },
            I::Min | I::Max => {
                let min = i == I::Min;
                let kind = match (scalar, min) {
                    (Scalar::I32, true) => Bin::IMin,
                    (Scalar::I32, false) => Bin::IMax,
                    (Scalar::U32, true) => Bin::UMin,
                    (Scalar::U32, false) => Bin::UMax,
                    (_, true) => Bin::FMin,
                    (_, false) => Bin::FMax,
                };
                Op::Binary {
                    kind,
                    dst,
                    a: reg(0),
                    b: reg(1),
                    n,
                    a1: one(0),
                    b1: one(1),
                }
            }
            I::Clamp => Op::Clamp {
                scalar,
                dst,
                x: reg(0),
                lo: reg(1),
                hi: reg(2),
                n,
                lo1: one(1),
                hi1: one(2),
            },
            I::Mix => Op::Mix {
                dst,
                a: reg(0),
                b: reg(1),
                t: reg(2),
                n,
                t1: one(2),
            },
            I::Smoothstep => Op::Smoothstep {
                dst,
                e0: reg(0),
                e1: reg(1),
                x: reg(2),
                n,
                e01: one(0),
                e11: one(1),
            },
            I::Length => Op::Length {
                dst,
                src: reg(0),
                n: shape(args[0].ty).1,
            },
            I::Distance => Op::Distance {
                dst,
                a: reg(0),
                b: reg(1),
                n: shape(args[0].ty).1,
            },
            I::Dot => Op::Dot {
                dst,
                a: reg(0),
                b: reg(1),
                n: shape(args[0].ty).1,
            },
            I::Normalize => Op::Normalize {
                dst,
                src: reg(0),
                n,
            },
            I::Cross => Op::Cross {
                dst,
                a: reg(0),
                b: reg(1),
            },
            I::Transpose => Op::Transpose {
                n: shape(ty).1,
                dst,
                m: reg(0),
            },
            I::Dpdx | I::Dpdy | I::Fwidth => Op::Derivative {
                kind: match i {
                    I::Dpdx => Derivative::X,
                    I::Dpdy => Derivative::Y,
                    _ => Derivative::Width,
                },
                dst,
                src: reg(0),
                n,
            },
            I::Sample | I::SampleLevel => {
                let (Operand::Texture(texture), Operand::Sampler(sampler)) = (ops[0], ops[1])
                else {
                    self.push(Op::Clear { dst });
                    return Operand::Reg(dst);
                };
                Op::Sample {
                    dst,
                    texture,
                    sampler,
                    uv: reg(2),
                    scalar: matches!(args[0].ty, Ty::Texture(Texel::F32)),
                }
            }
            I::ToVec4 => return Operand::Reg(reg(0)),
            I::QuadVertex => Op::QuadVertex { dst, src: reg(0) },
            I::ToClip => Op::ToClip {
                dst,
                pos: reg(0),
                viewport: reg(1),
            },
            I::RoundedRectSdf => Op::RoundedRectSdf {
                dst,
                p: reg(0),
                size: reg(1),
                radius: reg(2),
            },
        };
        self.push(op);
        Operand::Reg(dst)
    }
}
