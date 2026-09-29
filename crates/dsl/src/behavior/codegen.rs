//! Bytecode generation: a [`Program`] becomes a verified
//! [`viso_behavior::Module`].
//!
//! Each IR instruction maps to one [`Op`] and keeps its span. `I64` and `F64`
//! arithmetic and ordering use the dedicated fast-path ops; `>`/`>=` are `<`/`<=`
//! with swapped operands; integer and scalar constants that fit an `i32` are
//! immediates; everything else goes to the chunk's constant pool (strings are
//! pooled once per chunk). A function whose frame, field index or unit-suffix
//! pool index does not fit the instruction encoding becomes a chunk without a
//! body, like any other unsupported function.

use std::collections::HashMap;
use std::rc::Rc;

use viso_behavior::{
    Arith, ArithOp, Chunk, ChunkKind, Code, Component, DisplayKind, Module, Num, Op, Span, Value,
    VerifyError,
};

use super::ir::{self, BinaryOp, Const, FunctionKind, Inst, PathStep, Program, Reg, UnaryOp};

impl Program {
    /// The program as a verified bytecode module: chunk `i` is function `i`,
    /// and component layouts keep their order.
    ///
    /// # Errors
    ///
    /// A [`VerifyError`] means the lowering produced malformed code, a compiler
    /// bug.
    pub fn bytecode(&self) -> Result<Module, VerifyError> {
        let chunks = self.functions.iter().map(chunk).collect();
        let components = self
            .components
            .iter()
            .map(|c| Component {
                name: c.name.as_str().into(),
                states: names(&c.states),
                inputs: names(&c.inputs),
                events: names(&c.events),
                state_inits: c.state_inits.iter().map(|f| f.map(|f| f.0)).collect(),
                input_defaults: c.input_defaults.iter().map(|f| f.map(|f| f.0)).collect(),
                members: c
                    .members
                    .iter()
                    .map(|(n, f)| (n.as_str().into(), f.0))
                    .collect(),
                handlers: c.handlers.iter().map(|(_, f)| f.0).collect(),
            })
            .collect();
        Module::new(chunks, components, self.natives.clone())
    }
}

fn names(names: &[String]) -> Box<[Box<str>]> {
    names.iter().map(|n| n.as_str().into()).collect()
}

fn chunk(function: &ir::Function) -> Chunk {
    let kind = match function.kind {
        FunctionKind::Fn => ChunkKind::Fn,
        FunctionKind::Action => ChunkKind::Action,
        FunctionKind::Closure => ChunkKind::Closure,
        FunctionKind::Computed => ChunkKind::Computed,
        FunctionKind::StateInit => ChunkKind::StateInit,
        FunctionKind::InputDefault => ChunkKind::InputDefault,
        FunctionKind::Const => ChunkKind::Const,
        FunctionKind::FieldDefault => ChunkKind::FieldDefault,
        FunctionKind::Handler => ChunkKind::Handler,
    };
    let params = u16::try_from(function.params).unwrap_or(u16::MAX);
    let frame = match &function.body {
        Ok(body) => u16::try_from(body.regs).ok(),
        Err(_) => Some(params.max(1)),
    };
    let captures: Option<Box<[u16]>> = function
        .captures
        .iter()
        .map(|r| u16::try_from(r.0).ok())
        .collect();
    let (Some(regs), Some(captures)) = (frame, captures) else {
        // Callers still see the declared arity and capture count; register 0
        // stands in for every capture.
        return Chunk {
            name: function.name.as_str().into(),
            kind,
            module: function.module as u32,
            params,
            regs: params.max(1),
            captures: vec![0; function.captures.len()].into(),
            body: Err("the frame has more than 65535 registers".into()),
        };
    };
    let body = match &function.body {
        Ok(body) => Emitter::default().body(body),
        Err(unsupported) => Err(unsupported.reason.as_str().into()),
    };
    Chunk {
        name: function.name.as_str().into(),
        kind,
        module: function.module as u32,
        params,
        regs: regs.max(params),
        captures,
        body,
    }
}

/// Builds one chunk's code.
#[derive(Default)]
struct Emitter {
    ops: Vec<Op>,
    consts: Vec<Value>,
    ext: Vec<u32>,
    strings: HashMap<String, u32>,
}

fn reg(r: Reg) -> u16 {
    // The frame size was checked to fit `u16`, and every register is below it.
    r.0 as u16
}

fn num(n: ir::Num) -> Num {
    match n {
        ir::Num::I8 => Num::I8,
        ir::Num::I16 => Num::I16,
        ir::Num::I32 => Num::I32,
        ir::Num::I64 => Num::I64,
        ir::Num::U8 => Num::U8,
        ir::Num::U16 => Num::U16,
        ir::Num::U32 => Num::U32,
        ir::Num::U64 => Num::U64,
        ir::Num::F32 => Num::F32,
        ir::Num::F64 => Num::F64,
    }
}

impl Emitter {
    fn body(mut self, body: &ir::Body) -> Result<Code, Box<str>> {
        for inst in &body.insts {
            let op = self.inst(inst)?;
            self.ops.push(op);
        }
        let spans = body
            .spans
            .iter()
            .map(|s| Span {
                start: s.start().to_u32(),
                end: s.end().to_u32(),
            })
            .collect();
        Ok(Code {
            ops: self.ops.into(),
            consts: self.consts.into(),
            ext: self.ext.into(),
            spans,
        })
    }

    fn constant(&mut self, value: Value) -> u32 {
        if let Value::Str(s) = &value
            && let Some(&index) = self.strings.get(s.as_str())
        {
            return index;
        }
        let index = self.consts.len() as u32;
        if let Value::Str(s) = &value {
            self.strings.insert(s.as_str().to_owned(), index);
        }
        self.consts.push(value);
        index
    }

    /// Appends `head` then `regs` (as a count and the registers) to the operand
    /// table and returns its offset.
    fn operands(&mut self, head: &[u32], regs: &[Reg]) -> u32 {
        let at = self.ext.len() as u32;
        self.ext.extend_from_slice(head);
        self.ext.push(regs.len() as u32);
        self.ext.extend(regs.iter().map(|r| r.0));
        at
    }

    fn int(&mut self, dst: u16, value: i64) -> Op {
        match i32::try_from(value) {
            Ok(value) => Op::Int { dst, value },
            Err(_) => Op::Const {
                dst,
                index: self.constant(Value::Int(value)),
            },
        }
    }

    fn inst(&mut self, inst: &Inst) -> Result<Op, Box<str>> {
        Ok(match inst {
            Inst::Const { dst, value } => {
                let dst = reg(*dst);
                match value {
                    Const::Unit => Op::Int { dst, value: 0 },
                    Const::Bool(b) => Op::Int {
                        dst,
                        value: i32::from(*b),
                    },
                    // A `U64` above `i64::MAX` keeps its 64-bit pattern.
                    Const::Int(v) => self.int(dst, *v as i64),
                    Const::Float(v) => Op::Const {
                        dst,
                        index: self.constant(Value::Float(*v)),
                    },
                    Const::Char(c) => self.int(dst, i64::from(u32::from(*c))),
                    Const::Str(s) => Op::Const {
                        dst,
                        index: self.constant(Value::Str(Rc::new(s.clone()))),
                    },
                    Const::Color(rgba) => self.int(dst, i64::from(*rgba)),
                    Const::Nil => Op::Nil { dst },
                    Const::Tag(t) => self.int(dst, i64::from(*t)),
                }
            }
            Inst::Move { dst, src } => Op::Move {
                dst: reg(*dst),
                src: reg(*src),
            },
            Inst::LoadState { dst, slot } => Op::LoadState {
                dst: reg(*dst),
                slot: *slot,
            },
            Inst::StoreState { slot, src } => Op::StoreState {
                src: reg(*src),
                slot: *slot,
            },
            Inst::LoadInput { dst, slot } => Op::LoadInput {
                dst: reg(*dst),
                slot: *slot,
            },
            Inst::Unary { dst, op, src } => {
                let (dst, src) = (reg(*dst), reg(*src));
                match op {
                    UnaryOp::Neg(n) => Op::Neg {
                        num: num(*n),
                        dst,
                        src,
                    },
                    UnaryOp::Not => Op::Not { dst, src },
                    UnaryOp::BitNot(n) => Op::BitNot {
                        num: num(*n),
                        dst,
                        src,
                    },
                }
            }
            Inst::Binary { dst, op, lhs, rhs } => {
                binary(*op, reg(*dst), reg(*lhs), reg(*rhs), |parts| {
                    self.operands(&[], parts)
                })
            }
            Inst::Cast { dst, src, from, to } => Op::Cast {
                from: num(*from),
                to: num(*to),
                dst: reg(*dst),
                src: reg(*src),
            },
            Inst::Call { dst, func, args } => Op::Call {
                dst: reg(*dst),
                ext: self.operands(&[func.0], args),
            },
            Inst::Native { dst, import, args } => Op::Native {
                dst: reg(*dst),
                ext: self.operands(&[*import], args),
            },
            Inst::CallValue { dst, callee, args } => Op::CallValue {
                dst: reg(*dst),
                ext: self.operands(&[callee.0], args),
            },
            Inst::Closure {
                dst,
                func,
                captures,
            } => Op::Closure {
                dst: reg(*dst),
                ext: self.operands(&[func.0], captures),
            },
            Inst::Make { dst, tag, fields } => Op::Make {
                dst: reg(*dst),
                ext: self.operands(&[*tag], fields),
            },
            Inst::List { dst, items } => Op::List {
                dst: reg(*dst),
                ext: self.operands(&[], items),
            },
            Inst::Field { dst, src, index } => Op::Field {
                dst: reg(*dst),
                src: reg(*src),
                index: u16::try_from(*index).map_err(|_| "a field index above 65535")?,
            },
            Inst::Index { dst, list, index } => Op::Index {
                dst: reg(*dst),
                list: reg(*list),
                index: reg(*index),
            },
            Inst::SetPath { root, path, src } => {
                let at = self.ext.len() as u32;
                self.ext.push(src.0);
                self.ext.push(path.len() as u32);
                for step in path {
                    match step {
                        PathStep::Field(i) => self.ext.extend([0, *i]),
                        PathStep::Index(r) => self.ext.extend([1, r.0]),
                    }
                }
                Op::SetPath {
                    root: reg(*root),
                    ext: at,
                }
            }
            Inst::Len { dst, src } => Op::Len {
                dst: reg(*dst),
                src: reg(*src),
            },
            Inst::Tag { dst, src } => Op::Tag {
                dst: reg(*dst),
                src: reg(*src),
            },
            Inst::IsNil { dst, src } => Op::IsNil {
                dst: reg(*dst),
                src: reg(*src),
            },
            Inst::Jump { target } => Op::Jump { target: *target },
            Inst::JumpIf { cond, when, target } => {
                let (cond, target) = (reg(*cond), *target);
                if *when {
                    Op::JumpIf { cond, target }
                } else {
                    Op::JumpUnless { cond, target }
                }
            }
            Inst::Switch {
                src,
                base,
                targets,
                default,
            } => {
                let at = self.ext.len() as u32;
                let pattern = *base as u64;
                self.ext.extend([
                    pattern as u32,
                    (pattern >> 32) as u32,
                    *default,
                    targets.len() as u32,
                ]);
                self.ext.extend_from_slice(targets);
                Op::Switch {
                    src: reg(*src),
                    ext: at,
                }
            }
            Inst::Return { src } => Op::Return { src: reg(*src) },
            Inst::Emit { event, args } => Op::Emit {
                ext: self.operands(&[*event], args),
            },
            Inst::Display { dst, src, kind } => {
                let (dst, src) = (reg(*dst), reg(*src));
                let kind = match kind {
                    ir::DisplayKind::Bool => DisplayKind::Bool,
                    ir::DisplayKind::Signed => DisplayKind::Signed,
                    ir::DisplayKind::Unsigned => DisplayKind::Unsigned,
                    ir::DisplayKind::F32 => DisplayKind::F32,
                    ir::DisplayKind::F64 => DisplayKind::F64,
                    ir::DisplayKind::Char => DisplayKind::Char,
                    ir::DisplayKind::Str => DisplayKind::Str,
                    ir::DisplayKind::Dimension(suffix) => {
                        let index = self.constant(Value::str(*suffix));
                        return Ok(Op::DisplayDim {
                            dst,
                            src,
                            suffix: u16::try_from(index)
                                .map_err(|_| "more than 65535 constants")?,
                        });
                    }
                };
                Op::Display { kind, dst, src }
            }
            Inst::Concat { dst, parts } => Op::Concat {
                dst: reg(*dst),
                ext: self.operands(&[], parts),
            },
            Inst::Unreachable => Op::Unreachable,
        })
    }
}

/// The op for `dst = a op b`; `concat` emits a two-part concatenation's operands.
fn binary(op: BinaryOp, dst: u16, a: u16, b: u16, concat: impl FnOnce(&[Reg]) -> u32) -> Op {
    let generic = |arith: Arith, n: ir::Num, a: u16, b: u16| Op::Arith {
        op: ArithOp::new(arith, num(n)),
        dst,
        a,
        b,
    };
    match op {
        BinaryOp::Add(ir::Num::I64) => Op::AddI64 { dst, a, b },
        BinaryOp::Sub(ir::Num::I64) => Op::SubI64 { dst, a, b },
        BinaryOp::Mul(ir::Num::I64) => Op::MulI64 { dst, a, b },
        BinaryOp::Lt(ir::Num::I64) => Op::LtI64 { dst, a, b },
        BinaryOp::Le(ir::Num::I64) => Op::LeI64 { dst, a, b },
        BinaryOp::Gt(ir::Num::I64) => Op::LtI64 { dst, a: b, b: a },
        BinaryOp::Ge(ir::Num::I64) => Op::LeI64 { dst, a: b, b: a },
        BinaryOp::Add(ir::Num::F64) => Op::AddF64 { dst, a, b },
        BinaryOp::Sub(ir::Num::F64) => Op::SubF64 { dst, a, b },
        BinaryOp::Mul(ir::Num::F64) => Op::MulF64 { dst, a, b },
        BinaryOp::Div(ir::Num::F64) => Op::DivF64 { dst, a, b },
        BinaryOp::Lt(ir::Num::F64) => Op::LtF64 { dst, a, b },
        BinaryOp::Le(ir::Num::F64) => Op::LeF64 { dst, a, b },
        BinaryOp::Gt(ir::Num::F64) => Op::LtF64 { dst, a: b, b: a },
        BinaryOp::Ge(ir::Num::F64) => Op::LeF64 { dst, a: b, b: a },
        BinaryOp::Add(n) => generic(Arith::Add, n, a, b),
        BinaryOp::Sub(n) => generic(Arith::Sub, n, a, b),
        BinaryOp::Mul(n) => generic(Arith::Mul, n, a, b),
        BinaryOp::Div(n) => generic(Arith::Div, n, a, b),
        BinaryOp::Rem(n) => generic(Arith::Rem, n, a, b),
        BinaryOp::BitAnd(n) => generic(Arith::And, n, a, b),
        BinaryOp::BitOr(n) => generic(Arith::Or, n, a, b),
        BinaryOp::BitXor(n) => generic(Arith::Xor, n, a, b),
        BinaryOp::Shl(n) => generic(Arith::Shl, n, a, b),
        BinaryOp::Shr(n) => generic(Arith::Shr, n, a, b),
        BinaryOp::Lt(n) => generic(Arith::Lt, n, a, b),
        BinaryOp::Le(n) => generic(Arith::Le, n, a, b),
        BinaryOp::Gt(n) => generic(Arith::Lt, n, b, a),
        BinaryOp::Ge(n) => generic(Arith::Le, n, b, a),
        BinaryOp::Eq => Op::Eq { dst, a, b },
        BinaryOp::Ne => Op::Ne { dst, a, b },
        BinaryOp::Concat => Op::Concat {
            dst,
            ext: concat(&[Reg(u32::from(a)), Reg(u32::from(b))]),
        },
    }
}
