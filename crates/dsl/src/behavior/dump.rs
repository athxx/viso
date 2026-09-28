//! The textual form of a [`Program`], for golden tests and `viso` tooling.
//!
//! The first line is `behavior-ir 1`. Each function prints as a header naming
//! it, its kind, parameter count and captured registers, then one instruction
//! per line prefixed by its index; a function that cannot run prints its reason
//! instead.

use std::fmt::{self, Write};

use super::ir::{
    BinaryOp, Body, Const, DisplayKind, Function, Inst, PathStep, Program, Reg, UnaryOp,
};

impl Program {
    /// The program's textual dump.
    pub fn dump(&self) -> String {
        let mut out = String::from("behavior-ir 1\n");
        for component in &self.components {
            let _ = writeln!(
                out,
                "component {} states=[{}] inputs=[{}] events=[{}]",
                component.name,
                component.states.join(", "),
                component.inputs.join(", "),
                component.events.join(", "),
            );
        }
        for (i, function) in self.functions.iter().enumerate() {
            let _ = write_function(&mut out, i, function);
        }
        out
    }
}

fn write_function(out: &mut String, index: usize, function: &Function) -> fmt::Result {
    write!(
        out,
        "\nfn#{index} {} {} params={}",
        function.name,
        function.kind.name(),
        function.params
    )?;
    if !function.captures.is_empty() {
        write!(out, " captures=[{}]", regs(&function.captures))?;
    }
    match &function.body {
        Ok(body) => {
            writeln!(out, " regs={}", body.regs)?;
            write_body(out, body)
        }
        Err(unsupported) => writeln!(out, "\n  unsupported: {}", unsupported.reason),
    }
}

fn write_body(out: &mut String, body: &Body) -> fmt::Result {
    for (i, inst) in body.insts.iter().enumerate() {
        write!(out, "  {i:>3}: ")?;
        write_inst(out, inst)?;
        out.push('\n');
    }
    Ok(())
}

fn write_inst(out: &mut String, inst: &Inst) -> fmt::Result {
    match inst {
        Inst::Const { dst, value } => write!(out, "{} = const {}", reg(*dst), constant(value)),
        Inst::Move { dst, src } => write!(out, "{} = {}", reg(*dst), reg(*src)),
        Inst::LoadState { dst, slot } => write!(out, "{} = state[{slot}]", reg(*dst)),
        Inst::StoreState { slot, src } => write!(out, "state[{slot}] = {}", reg(*src)),
        Inst::LoadInput { dst, slot } => write!(out, "{} = input[{slot}]", reg(*dst)),
        Inst::Unary { dst, op, src } => {
            write!(out, "{} = {} {}", reg(*dst), unary(*op), reg(*src))
        }
        Inst::Binary { dst, op, lhs, rhs } => write!(
            out,
            "{} = {} {}, {}",
            reg(*dst),
            binary(*op),
            reg(*lhs),
            reg(*rhs)
        ),
        Inst::Cast { dst, src, from, to } => write!(
            out,
            "{} = cast.{}.{} {}",
            reg(*dst),
            from.name(),
            to.name(),
            reg(*src)
        ),
        Inst::Call { dst, func, args } => {
            write!(out, "{} = call fn#{} ({})", reg(*dst), func.0, regs(args))
        }
        Inst::CallValue { dst, callee, args } => write!(
            out,
            "{} = call {} ({})",
            reg(*dst),
            reg(*callee),
            regs(args)
        ),
        Inst::Closure {
            dst,
            func,
            captures,
        } => write!(
            out,
            "{} = closure fn#{} [{}]",
            reg(*dst),
            func.0,
            regs(captures)
        ),
        Inst::Make { dst, tag, fields } => {
            write!(out, "{} = make #{tag} ({})", reg(*dst), regs(fields))
        }
        Inst::List { dst, items } => write!(out, "{} = list [{}]", reg(*dst), regs(items)),
        Inst::Field { dst, src, index } => {
            write!(out, "{} = {}.{index}", reg(*dst), reg(*src))
        }
        Inst::Index { dst, list, index } => {
            write!(out, "{} = {}[{}]", reg(*dst), reg(*list), reg(*index))
        }
        Inst::SetPath { root, path, src } => {
            write!(out, "{}", reg(*root))?;
            for step in path {
                match step {
                    PathStep::Field(index) => write!(out, ".{index}")?,
                    PathStep::Index(index) => write!(out, "[{}]", reg(*index))?,
                }
            }
            write!(out, " = {}", reg(*src))
        }
        Inst::Len { dst, src } => write!(out, "{} = len {}", reg(*dst), reg(*src)),
        Inst::Tag { dst, src } => write!(out, "{} = tag {}", reg(*dst), reg(*src)),
        Inst::IsNil { dst, src } => write!(out, "{} = is_nil {}", reg(*dst), reg(*src)),
        Inst::Jump { target } => write!(out, "jump @{target}"),
        Inst::JumpIf { cond, when, target } => {
            let op = if *when { "if" } else { "unless" };
            write!(out, "jump @{target} {op} {}", reg(*cond))
        }
        Inst::Switch {
            src,
            base,
            targets,
            default,
        } => {
            let targets: Vec<String> = targets.iter().map(|t| format!("@{t}")).collect();
            write!(
                out,
                "switch {} from {base} [{}] else @{default}",
                reg(*src),
                targets.join(", ")
            )
        }
        Inst::Return { src } => write!(out, "return {}", reg(*src)),
        Inst::Emit { event, args } => write!(out, "emit event#{event} ({})", regs(args)),
        Inst::Display { dst, src, kind } => {
            write!(
                out,
                "{} = display.{} {}",
                reg(*dst),
                display(*kind),
                reg(*src)
            )
        }
        Inst::Concat { dst, parts } => write!(out, "{} = concat ({})", reg(*dst), regs(parts)),
        Inst::Unreachable => write!(out, "unreachable"),
    }
}

fn reg(r: Reg) -> String {
    format!("r{}", r.0)
}

fn regs(rs: &[Reg]) -> String {
    rs.iter().map(|r| reg(*r)).collect::<Vec<_>>().join(", ")
}

fn constant(value: &Const) -> String {
    match value {
        Const::Unit => "()".into(),
        Const::Bool(b) => b.to_string(),
        Const::Int(v) => v.to_string(),
        Const::Float(v) => format!("{v:?}"),
        Const::Char(c) => format!("{c:?}"),
        Const::Str(s) => format!("{s:?}"),
        Const::Color(rgba) => format!("#{rgba:08x}"),
        Const::Nil => "nil".into(),
        Const::Tag(tag) => format!("#{tag}"),
    }
}

fn unary(op: UnaryOp) -> String {
    match op {
        UnaryOp::Neg(num) => format!("neg.{}", num.name()),
        UnaryOp::Not => "not".into(),
        UnaryOp::BitNot(num) => format!("bitnot.{}", num.name()),
    }
}

fn binary(op: BinaryOp) -> String {
    let (name, num) = match op {
        BinaryOp::Add(n) => ("add", Some(n)),
        BinaryOp::Sub(n) => ("sub", Some(n)),
        BinaryOp::Mul(n) => ("mul", Some(n)),
        BinaryOp::Div(n) => ("div", Some(n)),
        BinaryOp::Rem(n) => ("rem", Some(n)),
        BinaryOp::BitAnd(n) => ("and", Some(n)),
        BinaryOp::BitOr(n) => ("or", Some(n)),
        BinaryOp::BitXor(n) => ("xor", Some(n)),
        BinaryOp::Shl(n) => ("shl", Some(n)),
        BinaryOp::Shr(n) => ("shr", Some(n)),
        BinaryOp::Lt(n) => ("lt", Some(n)),
        BinaryOp::Le(n) => ("le", Some(n)),
        BinaryOp::Gt(n) => ("gt", Some(n)),
        BinaryOp::Ge(n) => ("ge", Some(n)),
        BinaryOp::Eq => ("eq", None),
        BinaryOp::Ne => ("ne", None),
        BinaryOp::Concat => ("concat", None),
    };
    match num {
        Some(num) => format!("{name}.{}", num.name()),
        None => name.into(),
    }
}

fn display(kind: DisplayKind) -> String {
    match kind {
        DisplayKind::Bool => "bool".into(),
        DisplayKind::Signed => "signed".into(),
        DisplayKind::Unsigned => "unsigned".into(),
        DisplayKind::F32 => "f32".into(),
        DisplayKind::F64 => "f64".into(),
        DisplayKind::Char => "char".into(),
        DisplayKind::Str => "str".into(),
        DisplayKind::Dimension(suffix) => format!("dim({suffix})"),
    }
}
