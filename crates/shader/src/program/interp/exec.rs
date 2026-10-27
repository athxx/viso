//! A compiled program run on four lanes at once: the four pixels of a quad,
//! or four vertices. Every operation of an expression runs on every lane, a
//! lane its control flow left included, so a derivative reads its
//! neighbours as the GPU's helpers give them; only writes to locals,
//! varyings and return values keep to the lanes still running.

use viso_gpu_abi::{CpuTexture, SamplerDesc};

use super::code::{CFunction, CLoop, CStmt, Code, Op, Reg, Run, Target, Word};
use super::native::NativeKernel;
use super::rt::{self, Env, Flow, Mask};
use crate::program::{Builtin, Value};

pub(super) use super::rt::{Cell, ZERO};

/// A program's registers and the textures a draw binds.
pub(super) struct Machine<'a> {
    code: &'a Code,
    env: Env<'a>,
    regs: Vec<Cell>,
    /// The entries compiled to Rust, run in place of interpreting them.
    native: Option<&'static NativeKernel>,
}

impl<'a> Machine<'a> {
    pub(super) fn new(
        code: &'a Code,
        textures: &'a [CpuTexture<'a>],
        samplers: &'a [SamplerDesc],
        native: Option<&'static NativeKernel>,
    ) -> Machine<'a> {
        Machine {
            code,
            env: Env { textures, samplers },
            regs: vec![ZERO; code.regs as usize],
            native,
        }
    }

    /// Binds `values`, one per member placed at `regs`.
    pub(super) fn bind_values(&mut self, regs: &[Reg], values: &[Value]) {
        for (value, reg) in values.iter().zip(regs) {
            self.load(*reg, value);
        }
    }

    /// Binds the words of `bytes` that `words` place.
    pub(super) fn bind_words(&mut self, words: &[Word], bytes: &[u8]) {
        for w in words {
            let at = w.offset as usize;
            let bits = bytes
                .get(at..at + 4)
                .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            let bits = if w.boolean {
                u32::from(bits != 0)
            } else {
                bits
            };
            self.regs[w.reg as usize][usize::from(w.component)] = [bits; 4];
        }
    }

    /// Writes `value` into every lane from `reg`; how many registers it took.
    fn load(&mut self, reg: Reg, value: &Value) -> u32 {
        match value {
            Value::Lanes(..) => {
                let mut cell = ZERO;
                for (c, lane) in cell.iter_mut().zip(value.lanes()) {
                    *c = [lane.bits(); 4];
                }
                self.regs[reg as usize] = cell;
                1
            }
            Value::Matrix(n, columns) => {
                let n = usize::from(*n);
                for (k, column) in columns.iter().take(n).enumerate() {
                    let mut cell = ZERO;
                    for (c, v) in cell.iter_mut().zip(&column[..n]) {
                        *c = [v.to_bits(); 4];
                    }
                    self.regs[reg as usize + k] = cell;
                }
                n as u32
            }
            Value::Record(fields) => {
                let mut at = reg;
                for field in fields {
                    at += self.load(at, field);
                }
                at - reg
            }
            Value::Texture(_) | Value::Sampler(_) => 0,
        }
    }

    /// The vertex entry for the vertices `vids[..count]` of instance `iid`,
    /// one a lane: each lane's clip position, its varyings left for
    /// [`varying`](Self::varying).
    pub(super) fn vertex(
        &mut self,
        f: &'a CFunction,
        vids: [u32; 4],
        count: usize,
        iid: u32,
    ) -> [[f32; 4]; 4] {
        for (&(reg, _), builtin) in f.params.iter().zip(&f.builtins) {
            let mut cell = ZERO;
            match builtin {
                Builtin::VertexId => cell[0] = vids,
                Builtin::InstanceId => cell[0] = [iid; 4],
                Builtin::FragCoord => {}
            }
            self.regs[reg as usize] = cell;
        }
        for &(reg, _) in &self.code.varyings {
            self.regs[reg as usize] = ZERO;
        }
        let mask = std::array::from_fn(|l| l < count);
        let ret = match self.native {
            Some(k) => (k.vertex)(&mut self.regs, &self.env, mask).ret,
            None => self.enter(f, mask).0,
        };
        let clip = &self.regs[ret as usize];
        std::array::from_fn(|l| std::array::from_fn(|c| f32::from_bits(clip[c][l])))
    }

    /// Varying `v` as the vertex entry left it.
    pub(super) fn varying(&self, v: usize) -> &Cell {
        &self.regs[self.code.varyings[v].0 as usize]
    }

    /// Sets varying `v` for the fragment entry.
    pub(super) fn set_varying(&mut self, v: usize, cell: Cell) {
        self.regs[self.code.varyings[v].0 as usize] = cell;
    }

    /// The fragment entry for a quad whose varyings are set: each lane's
    /// color, `None` when it discarded or returned nothing.
    pub(super) fn fragment(
        &mut self,
        f: &'a CFunction,
        frag_coord: [[f32; 4]; 4],
    ) -> [Option<[f32; 4]>; 4] {
        for (&(reg, _), builtin) in f.params.iter().zip(&f.builtins) {
            let mut cell = ZERO;
            if *builtin == Builtin::FragCoord {
                for (c, column) in cell.iter_mut().enumerate() {
                    *column = std::array::from_fn(|l| frag_coord[l][c].to_bits());
                }
            }
            self.regs[reg as usize] = cell;
        }
        let (ret, flow) = match self.native {
            Some(k) => {
                let flow = (k.fragment)(&mut self.regs, &self.env, [true; 4]);
                (flow.ret, flow)
            }
            None => self.enter(f, [true; 4]),
        };
        let color = &self.regs[ret as usize];
        std::array::from_fn(|l| {
            (flow.returned[l] && !flow.discarded[l])
                .then(|| std::array::from_fn(|c| f32::from_bits(color[c][l])))
        })
    }

    /// Runs `f`'s body on the lanes of `mask`, its value cleared first.
    fn enter(&mut self, f: &'a CFunction, mut mask: Mask) -> (Reg, Flow) {
        let ret = f.ret as usize;
        self.regs[ret..ret + f.ret_width as usize].fill(ZERO);
        let mut flow = Flow::new(f.ret);
        self.block(&f.body, &mut mask, &mut flow);
        (f.ret, flow)
    }

    /// Runs `stmts` on the lanes of `mask`; a lane that returns, discards,
    /// breaks or continues leaves it.
    fn block(&mut self, stmts: &'a [CStmt], mask: &mut Mask, flow: &mut Flow) {
        for stmt in stmts {
            if !rt::any(mask) {
                return;
            }
            self.stmt(stmt, mask, flow);
        }
    }

    fn stmt(&mut self, stmt: &'a CStmt, mask: &mut Mask, flow: &mut Flow) {
        let r = &mut self.regs;
        match stmt {
            CStmt::Let { run, dst, count } => {
                self.run(*run);
                rt::masked(&mut self.regs, *dst, run.value, *count, *mask);
            }
            CStmt::Declare { dst, count } => rt::declare(r, *dst, *count, *mask),
            CStmt::Assign { run, target } => {
                self.run(*run);
                let r = &mut self.regs;
                match *target {
                    Target::Regs { reg, count } => rt::masked(r, reg, run.value, count, *mask),
                    Target::Components { reg, at, n } => {
                        rt::assign_components(r, reg, at, n, run.value, *mask);
                    }
                }
            }
            CStmt::If {
                cond,
                then,
                otherwise,
            } => {
                self.run(*cond);
                let (mut then_mask, mut else_mask) = rt::split(&self.regs, cond.value, mask);
                self.block(then, &mut then_mask, flow);
                self.block(otherwise, &mut else_mask, flow);
                *mask = rt::join(&then_mask, &else_mask);
            }
            CStmt::For(l) => self.for_loop(l, mask, flow),
            CStmt::Break => rt::leave(mask, &mut flow.broke),
            CStmt::Continue => rt::leave(mask, &mut flow.continued),
            CStmt::Return { run, count } => {
                if let Some(run) = run {
                    self.run(*run);
                    rt::masked(&mut self.regs, flow.ret, run.value, *count, *mask);
                }
                rt::leave(mask, &mut flow.returned);
            }
            CStmt::Discard => rt::leave(mask, &mut flow.discarded),
        }
    }

    fn for_loop(&mut self, lp: &'a CLoop, mask: &mut Mask, flow: &mut Flow) {
        let entered = *mask;
        let saved = (flow.broke, flow.continued);
        flow.broke = [false; 4];
        let mut running = entered;
        self.run(lp.start);
        rt::masked(&mut self.regs, lp.var, lp.start.value, 1, running);
        for _ in 0..lp.max {
            if lp.guarded {
                self.run(lp.end);
                rt::loop_guard(
                    &self.regs,
                    lp.var,
                    lp.end.value,
                    lp.signed,
                    lp.inclusive,
                    &mut running,
                );
            }
            if !rt::any(&running) {
                break;
            }
            flow.continued = [false; 4];
            let mut body = running;
            self.block(&lp.body, &mut body, flow);
            rt::loop_next(&mut self.regs, lp.var, &mut running, &body, flow);
        }
        rt::loop_exit(mask, &entered, flow);
        (flow.broke, flow.continued) = saved;
    }

    /// Runs an expression's operations.
    fn run(&mut self, run: Run) {
        let code = self.code;
        for op in &code.ops[run.ops.0 as usize..run.ops.1 as usize] {
            self.op(op);
        }
    }

    fn op(&mut self, op: &'a Op) {
        let r = &mut self.regs[..];
        match *op {
            Op::Const { dst, bits } => rt::constant(r, dst, bits),
            Op::Copy { dst, src, count } => rt::copy(r, dst, src, count),
            Op::Place {
                dst,
                to,
                src,
                at,
                n,
            } => rt::place(r, dst, to, src, at, n),
            Op::Clear { dst } => rt::clear(r, dst),
            Op::Splat { dst, src, n } => rt::splat(r, dst, src, n),
            Op::Swizzle { dst, src, lanes, n } => rt::swizzle(r, dst, src, lanes, n),
            Op::Unary { kind, dst, src, n } => rt::unary_op(r, kind, dst, src, n),
            Op::Binary {
                kind,
                dst,
                a,
                b,
                n,
                a1,
                b1,
            } => rt::binary_op(r, kind, dst, a, b, n, a1, b1),
            Op::MatMul { n, dst, a, b } => rt::mat_mul(r, n, dst, a, b),
            Op::MatVec { n, dst, m, v } => rt::mat_vec(r, n, dst, m, v),
            Op::VecMat { n, dst, v, m } => rt::vec_mat(r, n, dst, v, m),
            Op::MatAdd { n, dst, a, b, sub } => rt::mat_add(r, n, dst, a, b, sub),
            Op::MatScale { n, dst, m, s } => rt::mat_scale(r, n, dst, m, s),
            Op::MatNeg { n, dst, m } => rt::mat_neg(r, n, dst, m),
            Op::Transpose { n, dst, m } => rt::transpose(r, n, dst, m),
            Op::Convert {
                dst,
                src,
                from,
                to,
                n,
            } => rt::convert_op(r, dst, src, from, to, n),
            Op::IndexLane {
                dst,
                src,
                index,
                len,
            } => rt::index_lane(r, dst, src, index, len),
            Op::IndexColumn {
                dst,
                base,
                index,
                n,
            } => rt::index_column(r, dst, base, index, n),
            Op::Math1 { f, dst, src, n, .. } => rt::math1(r, f, dst, src, n),
            Op::Math2 {
                f,
                dst,
                a,
                b,
                n,
                a1,
                b1,
                ..
            } => rt::math2(r, f, dst, a, b, n, a1, b1),
            Op::Clamp {
                scalar,
                dst,
                x,
                lo,
                hi,
                n,
                lo1,
                hi1,
            } => rt::clamp(r, scalar, dst, x, lo, hi, n, lo1, hi1),
            Op::Mix {
                dst,
                a,
                b,
                t,
                n,
                t1,
            } => rt::mix(r, dst, a, b, t, n, t1),
            Op::Smoothstep {
                dst,
                e0,
                e1,
                x,
                n,
                e01,
                e11,
            } => rt::smoothstep(r, dst, e0, e1, x, n, e01, e11),
            Op::Dot { dst, a, b, n } => rt::dot_op(r, dst, a, b, n),
            Op::Length { dst, src, n } => rt::length(r, dst, src, n),
            Op::Distance { dst, a, b, n } => rt::distance(r, dst, a, b, n),
            Op::Normalize { dst, src, n } => rt::normalize(r, dst, src, n),
            Op::Cross { dst, a, b } => rt::cross(r, dst, a, b),
            Op::Derivative { kind, dst, src, n } => rt::derivative_op(r, kind, dst, src, n),
            Op::Sample {
                dst,
                texture,
                sampler,
                uv,
                scalar,
            } => rt::sample_op(r, &self.env, dst, texture, sampler, uv, scalar),
            Op::QuadVertex { dst, src } => rt::quad_vertex(r, dst, src),
            Op::ToClip { dst, pos, viewport } => rt::to_clip(r, dst, pos, viewport),
            Op::RoundedRectSdf {
                dst,
                p,
                size,
                radius,
            } => rt::rounded_rect_sdf(r, dst, p, size, radius),
            Op::Call {
                function,
                args,
                dst,
                width,
            } => {
                let code = self.code;
                for &(src, param, w) in &code.args[args.0 as usize..args.1 as usize] {
                    rt::copy(&mut self.regs, param, src, w);
                }
                let (ret, _) = self.enter(&code.functions[function as usize], [true; 4]);
                rt::copy(&mut self.regs, dst, ret, width);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::Scalar;
    use crate::program::interp::code::{Bin, Cmp, Derivative};
    use crate::program::interp::rt::{binary, convert, derivative};

    /// `bits` in component 0 of every lane.
    fn scalar(bits: u32) -> Cell {
        let mut cell = ZERO;
        cell[0] = [bits; 4];
        cell
    }

    fn floats(v: &[f32]) -> Cell {
        let mut cell = ZERO;
        for (c, v) in cell.iter_mut().zip(v) {
            *c = [v.to_bits(); 4];
        }
        cell
    }

    #[test]
    fn matrices_are_by_column() {
        // Columns (1, 2) and (3, 4): the matrix [[1, 3], [2, 4]]; the vector
        // (1, 10) at 2, the scalar 2 at 3.
        let ops = vec![
            Op::MatVec {
                n: 2,
                dst: 4,
                m: 0,
                v: 2,
            },
            Op::VecMat {
                n: 2,
                dst: 5,
                v: 2,
                m: 0,
            },
            Op::MatMul {
                n: 2,
                dst: 6,
                a: 0,
                b: 0,
            },
            Op::MatScale {
                n: 2,
                dst: 8,
                m: 0,
                s: 3,
            },
            Op::Transpose {
                n: 2,
                dst: 10,
                m: 0,
            },
        ];
        let code = Code {
            regs: 12,
            ops,
            ..Code::default()
        };
        let mut m = Machine::new(&code, &[], &[], None);
        m.regs[0] = floats(&[1.0, 2.0]);
        m.regs[1] = floats(&[3.0, 4.0]);
        m.regs[2] = floats(&[1.0, 10.0]);
        m.regs[3] = floats(&[2.0]);
        m.run(Run {
            ops: (0, code.ops.len() as u32),
            value: 0,
        });
        assert_eq!(m.regs[4], floats(&[31.0, 42.0]));
        assert_eq!(m.regs[5], floats(&[21.0, 43.0]));
        assert_eq!(m.regs[6..8], [floats(&[7.0, 10.0]), floats(&[15.0, 22.0])]);
        assert_eq!(m.regs[8..10], [floats(&[2.0, 4.0]), floats(&[6.0, 8.0])]);
        assert_eq!(m.regs[10..12], [floats(&[1.0, 3.0]), floats(&[2.0, 4.0])]);
    }

    #[test]
    fn integer_edges_are_defined() {
        let op = |kind, a: u32, c: u32| binary(kind, &scalar(a), &scalar(c), 1, false, false)[0][0];
        let i = |v: i32| v.cast_unsigned();
        assert_eq!(op(Bin::IDiv, i(7), 0), i(7));
        assert_eq!(op(Bin::URem, 7, 0), 0);
        assert_eq!(op(Bin::IDiv, i(i32::MIN), i(-1)), i(i32::MIN));
        assert_eq!(op(Bin::Shl, 1, 33), 2);
        assert_eq!(op(Bin::IShr, i(-8), 1), i(-4));
        assert_eq!(op(Bin::IAdd, i(i32::MAX), 1), i(i32::MIN));
        assert_eq!(
            op(Bin::FRem, (-7.5f32).to_bits(), 2.0f32.to_bits()),
            (-1.5f32).to_bits()
        );
        assert_eq!(
            binary(
                Bin::FAdd,
                &floats(&[1.0, 2.0]),
                &floats(&[0.5]),
                2,
                false,
                true
            ),
            floats(&[1.5, 2.5])
        );
        let nan = f32::NAN.to_bits();
        assert_eq!(op(Bin::FCmp(Cmp::Lt), nan, 0), 0);
        assert_eq!(op(Bin::FCmp(Cmp::Ne), nan, nan), 1);
        let to = |x: Cell, from, to| convert(&x, from, to, 1)[0][0];
        assert_eq!(to(floats(&[-2.7]), Scalar::F32, Scalar::I32), i(-2));
        assert_eq!(to(scalar(i(-1)), Scalar::I32, Scalar::U32), u32::MAX);
        assert_eq!(to(scalar(5), Scalar::U32, Scalar::Bool), 1);
    }

    #[test]
    fn derivatives_are_fine_across_the_quad() {
        let mut x = ZERO;
        x[0] = [1.0f32, 3.0, 6.0, 10.0].map(f32::to_bits);
        let dx = derivative(Derivative::X, &x, 1)[0].map(f32::from_bits);
        let dy = derivative(Derivative::Y, &x, 1)[0].map(f32::from_bits);
        let width = derivative(Derivative::Width, &x, 1)[0].map(f32::from_bits);
        assert_eq!(dx, [2.0, 2.0, 4.0, 4.0]);
        assert_eq!(dy, [5.0, 7.0, 5.0, 7.0]);
        assert_eq!(width, [7.0, 9.0, 9.0, 11.0]);
    }
}
