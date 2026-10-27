//! A program's entries as Rust, generated when the app builds and compiled
//! with it, run by the headless raster in place of interpreting them.
//!
//! [`emit`] writes the module: each function of the compiled program a Rust
//! function over the same registers, its expressions calls of [`rt`] with
//! constant registers, its statements the control flow the interpreter runs
//! the lanes' masks through. The rasterizer, the bindings and the interface
//! stay the interpreter's, so a generated program draws the same bits.
//!
//! The module's [`NativeKernel`] names the program it was generated from by
//! [`program_hash`]; [`CpuShader::with_native`](super::CpuShader::with_native)
//! takes it only for that program.

use std::fmt::Write as _;

use viso_ende::Encode;

use super::code::{self, Bin, CFunction, CLoop, CStmt, Code, Op, Run, Target};
use super::rt::{Cell, Env, Flow, Mask};
use crate::program::Program;

/// One entry of a generated program: it runs on the registers, the lanes of
/// the mask set, and returns how they ended.
pub type Entry = fn(&mut [Cell], &Env<'_>, Mask) -> Flow;

/// A program's entries compiled to Rust.
#[derive(Debug)]
pub struct NativeKernel {
    /// The [`program_hash`] of the program it was generated from.
    pub program: u64,
    /// How many registers it runs on.
    pub regs: u32,
    pub vertex: Entry,
    pub fragment: Entry,
}

/// The identity of `program`: the hash of its encoding, which carries no
/// source span.
pub fn program_hash(program: &Program) -> u64 {
    viso_ende::content_hash(&program.encode_to_vec())
}

/// The Rust of `program`, a validated program, as a module's items: its
/// functions and a `pub static KERNEL: NativeKernel`. `krate` is the path
/// the code names this module's parent by where it is compiled, such as
/// `::viso_shader::program::interp`.
pub fn emit(program: &Program, krate: &str) -> String {
    let code = code::compile(program);
    let mut e = Emitter {
        code: &code,
        out: String::new(),
        next: 0,
        indent: 1,
    };
    let out = &mut e.out;
    let _ = writeln!(
        out,
        "#![allow(unused_mut, unused_variables, unused_labels, clippy::all, clippy::pedantic)]"
    );
    let _ = writeln!(out, "use {krate}::rt::{{self, Cell, Env, Flow, Mask}};");
    let _ = writeln!(out, "const N: usize = {};", code.regs);
    let _ = writeln!(out, "type Regs = [Cell; N];");
    let _ = writeln!(
        out,
        "pub static KERNEL: {krate}::NativeKernel = {krate}::NativeKernel {{ program: {:#x}, regs: {}, vertex, fragment }};",
        program_hash(program),
        code.regs
    );
    for (name, entry) in [("vertex", &code.vertex), ("fragment", &code.fragment)] {
        let _ = writeln!(
            e.out,
            "fn {name}(r: &mut [Cell], env: &Env<'_>, mask: Mask) -> Flow {{"
        );
        match entry {
            Some(_) => {
                let _ = writeln!(
                    e.out,
                    "    let r: &mut Regs = r.try_into().expect(\"the registers the program was generated for\");"
                );
                let _ = writeln!(e.out, "    {name}_body(r, env, mask)");
            }
            None => {
                let _ = writeln!(e.out, "    Flow::new(0)");
            }
        }
        let _ = writeln!(e.out, "}}");
        if let Some(f) = entry {
            e.function(&format!("{name}_body"), f);
        }
    }
    for (k, f) in code.functions.iter().enumerate() {
        e.function(&format!("f{k}"), f);
    }
    e.out
}

struct Emitter<'c> {
    code: &'c Code,
    out: String,
    /// The next number naming a label or mask.
    next: u32,
    indent: usize,
}

impl Emitter<'_> {
    fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn fresh(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    fn function(&mut self, name: &str, f: &CFunction) {
        let _ = writeln!(
            self.out,
            "fn {name}(r: &mut Regs, env: &Env<'_>, mut mask: Mask) -> Flow {{"
        );
        self.indent = 1;
        let (ret, width) = (f.ret, f.ret_width);
        self.line(&format!("r[{ret}..{}].fill(rt::ZERO);", ret + width));
        self.line(&format!("let mut flow = Flow::new({ret});"));
        self.block(&f.body, "mask", ret);
        self.line("flow");
        self.out.push_str("}\n");
    }

    /// `stmts` on the lanes of the mask `mask` names, stopping when none
    /// runs; `ret` the function's value.
    fn block(&mut self, stmts: &[CStmt], mask: &str, ret: u32) {
        if stmts.is_empty() {
            return;
        }
        let label = self.fresh();
        self.line(&format!("'b{label}: {{"));
        self.indent += 1;
        for stmt in stmts {
            self.line(&format!("if !rt::any(&{mask}) {{ break 'b{label}; }}"));
            self.stmt(stmt, mask, ret);
        }
        self.indent -= 1;
        self.line("}");
    }

    fn stmt(&mut self, stmt: &CStmt, mask: &str, ret: u32) {
        match stmt {
            CStmt::Let { run, dst, count } => {
                self.run(*run);
                self.line(&format!(
                    "rt::masked(r, {dst}, {}, {count}, {mask});",
                    run.value
                ));
            }
            CStmt::Declare { dst, count } => {
                self.line(&format!("rt::declare(r, {dst}, {count}, {mask});"));
            }
            CStmt::Assign { run, target } => {
                self.run(*run);
                match *target {
                    Target::Regs { reg, count } => self.line(&format!(
                        "rt::masked(r, {reg}, {}, {count}, {mask});",
                        run.value
                    )),
                    Target::Components { reg, at, n } => self.line(&format!(
                        "rt::assign_components(r, {reg}, {at}, {n}, {}, {mask});",
                        run.value
                    )),
                }
            }
            CStmt::If {
                cond,
                then,
                otherwise,
            } => {
                self.run(*cond);
                let k = self.fresh();
                self.line(&format!(
                    "let (mut t{k}, mut e{k}) = rt::split(r, {}, &{mask});",
                    cond.value
                ));
                self.block(then, &format!("t{k}"), ret);
                self.block(otherwise, &format!("e{k}"), ret);
                self.line(&format!("{mask} = rt::join(&t{k}, &e{k});"));
            }
            CStmt::For(lp) => self.for_loop(lp, mask, ret),
            CStmt::Break => self.line(&format!("rt::leave(&mut {mask}, &mut flow.broke);")),
            CStmt::Continue => {
                self.line(&format!("rt::leave(&mut {mask}, &mut flow.continued);"));
            }
            CStmt::Return { run, count } => {
                if let Some(run) = run {
                    self.run(*run);
                    self.line(&format!(
                        "rt::masked(r, {ret}, {}, {count}, {mask});",
                        run.value
                    ));
                }
                self.line(&format!("rt::leave(&mut {mask}, &mut flow.returned);"));
            }
            CStmt::Discard => self.line(&format!("rt::leave(&mut {mask}, &mut flow.discarded);")),
        }
    }

    fn for_loop(&mut self, lp: &CLoop, mask: &str, ret: u32) {
        let k = self.fresh();
        self.line("{");
        self.indent += 1;
        self.line(&format!("let entered{k} = {mask};"));
        self.line(&format!("let saved{k} = (flow.broke, flow.continued);"));
        self.line("flow.broke = [false; 4];");
        self.line(&format!("let mut running{k} = entered{k};"));
        self.run(lp.start);
        self.line(&format!(
            "rt::masked(r, {}, {}, 1, running{k});",
            lp.var, lp.start.value
        ));
        self.line(&format!("'l{k}: for _ in 0..{}u32 {{", lp.max));
        self.indent += 1;
        if lp.guarded {
            self.run(lp.end);
            self.line(&format!(
                "rt::loop_guard(r, {}, {}, {}, {}, &mut running{k});",
                lp.var, lp.end.value, lp.signed, lp.inclusive
            ));
        }
        self.line(&format!("if !rt::any(&running{k}) {{ break 'l{k}; }}"));
        self.line("flow.continued = [false; 4];");
        self.line(&format!("let mut body{k} = running{k};"));
        self.block(&lp.body, &format!("body{k}"), ret);
        self.line(&format!(
            "rt::loop_next(r, {}, &mut running{k}, &body{k}, &flow);",
            lp.var
        ));
        self.indent -= 1;
        self.line("}");
        self.line(&format!("rt::loop_exit(&mut {mask}, &entered{k}, &flow);"));
        self.line(&format!("(flow.broke, flow.continued) = saved{k};"));
        self.indent -= 1;
        self.line("}");
    }

    fn run(&mut self, run: Run) {
        let code = self.code;
        for op in &code.ops[run.ops.0 as usize..run.ops.1 as usize] {
            let line = self.op(op);
            self.line(&line);
        }
    }

    fn op(&mut self, op: &Op) -> String {
        match *op {
            Op::Const { dst, bits } => format!(
                "rt::constant(r, {dst}, [{:#x}, {:#x}, {:#x}, {:#x}]);",
                bits[0], bits[1], bits[2], bits[3]
            ),
            Op::Copy { dst, src, count } => format!("rt::copy(r, {dst}, {src}, {count});"),
            Op::Place {
                dst,
                to,
                src,
                at,
                n,
            } => format!("rt::place(r, {dst}, {to}, {src}, {at}, {n});"),
            Op::Clear { dst } => format!("rt::clear(r, {dst});"),
            Op::Splat { dst, src, n } => format!("rt::splat(r, {dst}, {src}, {n});"),
            Op::Swizzle { dst, src, lanes, n } => format!(
                "rt::swizzle(r, {dst}, {src}, [{}, {}, {}, {}], {n});",
                lanes[0], lanes[1], lanes[2], lanes[3]
            ),
            Op::Unary { kind, dst, src, n } => {
                format!("rt::unary_op(r, rt::Un::{kind:?}, {dst}, {src}, {n});")
            }
            Op::Binary {
                kind,
                dst,
                a,
                b,
                n,
                a1,
                b1,
            } => format!(
                "rt::binary_op(r, {}, {dst}, {a}, {b}, {n}, {a1}, {b1});",
                bin(kind)
            ),
            Op::MatMul { n, dst, a, b } => format!("rt::mat_mul(r, {n}, {dst}, {a}, {b});"),
            Op::MatVec { n, dst, m, v } => format!("rt::mat_vec(r, {n}, {dst}, {m}, {v});"),
            Op::VecMat { n, dst, v, m } => format!("rt::vec_mat(r, {n}, {dst}, {v}, {m});"),
            Op::MatAdd { n, dst, a, b, sub } => {
                format!("rt::mat_add(r, {n}, {dst}, {a}, {b}, {sub});")
            }
            Op::MatScale { n, dst, m, s } => format!("rt::mat_scale(r, {n}, {dst}, {m}, {s});"),
            Op::MatNeg { n, dst, m } => format!("rt::mat_neg(r, {n}, {dst}, {m});"),
            Op::Transpose { n, dst, m } => format!("rt::transpose(r, {n}, {dst}, {m});"),
            Op::Convert {
                dst,
                src,
                from,
                to,
                n,
            } => format!(
                "rt::convert_op(r, {dst}, {src}, rt::Scalar::{from:?}, rt::Scalar::{to:?}, {n});"
            ),
            Op::IndexLane {
                dst,
                src,
                index,
                len,
            } => format!("rt::index_lane(r, {dst}, {src}, {index}, {len});"),
            Op::IndexColumn {
                dst,
                base,
                index,
                n,
            } => format!("rt::index_column(r, {dst}, {base}, {index}, {n});"),
            Op::Math1 {
                name, dst, src, n, ..
            } => format!("rt::math1(r, rt::math::{name}, {dst}, {src}, {n});"),
            Op::Math2 {
                name,
                dst,
                a,
                b,
                n,
                a1,
                b1,
                ..
            } => format!("rt::math2(r, rt::math::{name}, {dst}, {a}, {b}, {n}, {a1}, {b1});"),
            Op::Clamp {
                scalar,
                dst,
                x,
                lo,
                hi,
                n,
                lo1,
                hi1,
            } => format!(
                "rt::clamp(r, rt::Scalar::{scalar:?}, {dst}, {x}, {lo}, {hi}, {n}, {lo1}, {hi1});"
            ),
            Op::Mix {
                dst,
                a,
                b,
                t,
                n,
                t1,
            } => format!("rt::mix(r, {dst}, {a}, {b}, {t}, {n}, {t1});"),
            Op::Smoothstep {
                dst,
                e0,
                e1,
                x,
                n,
                e01,
                e11,
            } => format!("rt::smoothstep(r, {dst}, {e0}, {e1}, {x}, {n}, {e01}, {e11});"),
            Op::Dot { dst, a, b, n } => format!("rt::dot_op(r, {dst}, {a}, {b}, {n});"),
            Op::Length { dst, src, n } => format!("rt::length(r, {dst}, {src}, {n});"),
            Op::Distance { dst, a, b, n } => format!("rt::distance(r, {dst}, {a}, {b}, {n});"),
            Op::Normalize { dst, src, n } => format!("rt::normalize(r, {dst}, {src}, {n});"),
            Op::Cross { dst, a, b } => format!("rt::cross(r, {dst}, {a}, {b});"),
            Op::Derivative { kind, dst, src, n } => {
                format!("rt::derivative_op(r, rt::Derivative::{kind:?}, {dst}, {src}, {n});")
            }
            Op::Sample {
                dst,
                texture,
                sampler,
                uv,
                scalar,
            } => format!("rt::sample_op(r, env, {dst}, {texture}, {sampler}, {uv}, {scalar});"),
            Op::QuadVertex { dst, src } => format!("rt::quad_vertex(r, {dst}, {src});"),
            Op::ToClip { dst, pos, viewport } => {
                format!("rt::to_clip(r, {dst}, {pos}, {viewport});")
            }
            Op::RoundedRectSdf {
                dst,
                p,
                size,
                radius,
            } => format!("rt::rounded_rect_sdf(r, {dst}, {p}, {size}, {radius});"),
            Op::Call {
                function,
                args,
                dst,
                width,
            } => {
                let code = self.code;
                let mut text = String::new();
                for &(src, param, w) in &code.args[args.0 as usize..args.1 as usize] {
                    let _ = write!(text, "rt::copy(r, {param}, {src}, {w}); ");
                }
                let ret = code.functions[function as usize].ret;
                let _ = write!(
                    text,
                    "f{function}(r, env, [true; 4]); rt::copy(r, {dst}, {ret}, {width});"
                );
                text
            }
        }
    }
}

/// `kind` as the code names it.
fn bin(kind: Bin) -> String {
    match kind {
        Bin::FCmp(c) => format!("rt::Bin::FCmp(rt::Cmp::{c:?})"),
        Bin::ICmp(c) => format!("rt::Bin::ICmp(rt::Cmp::{c:?})"),
        Bin::UCmp(c) => format!("rt::Bin::UCmp(rt::Cmp::{c:?})"),
        other => format!("rt::Bin::{other:?}"),
    }
}
