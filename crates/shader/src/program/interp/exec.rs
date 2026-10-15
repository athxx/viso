//! A compiled program run on four lanes at once: the four pixels of a quad,
//! or four vertices. Every operation of an expression runs on every lane, a
//! lane its control flow left included, so a derivative reads its
//! neighbours as the GPU's helpers give them; only writes to locals,
//! varyings and return values keep to the lanes still running.

use std::cmp::Ordering;

use viso_gpu::{CpuTexture, SamplerDesc};

use super::code::{
    Bin, CFunction, CLoop, CStmt, Cmp, Code, Derivative, Op, Reg, Run, Target, Un, Word,
};
use super::sample;
use crate::program::{Builtin, Scalar, Value};

/// A register: four components by four lanes, each the bits of its scalar.
pub(super) type Cell = [[u32; 4]; 4];

pub(super) const ZERO: Cell = [[0; 4]; 4];

/// Which lanes run.
type Mask = [bool; 4];

/// How the lanes of one function's run ended.
struct Flow {
    /// The function's value's first register.
    ret: Reg,
    returned: Mask,
    discarded: Mask,
    broke: Mask,
    continued: Mask,
}

impl Flow {
    fn finished(&self, lane: usize) -> bool {
        self.returned[lane] || self.discarded[lane]
    }
}

/// A program's registers and the textures a draw binds.
pub(super) struct Machine<'a> {
    code: &'a Code,
    textures: &'a [CpuTexture<'a>],
    samplers: &'a [SamplerDesc],
    regs: Vec<Cell>,
}

#[inline(always)]
fn f(bits: u32) -> f32 {
    f32::from_bits(bits)
}

#[inline(always)]
fn b(v: f32) -> u32 {
    v.to_bits()
}

#[inline(always)]
fn i(bits: u32) -> i32 {
    bits.cast_signed()
}

#[inline(always)]
fn u(v: i32) -> u32 {
    v.cast_unsigned()
}

/// `g` component by component of `x` and `y`, a one-component operand
/// spread, on the first `n` components; the rest zero.
#[inline(always)]
fn zip(x: &Cell, y: &Cell, n: u8, x1: bool, y1: bool, g: impl Fn(u32, u32) -> u32) -> Cell {
    let mut out = ZERO;
    for (c, o) in out.iter_mut().enumerate().take(usize::from(n)) {
        let (a, b) = (&x[if x1 { 0 } else { c }], &y[if y1 { 0 } else { c }]);
        *o = std::array::from_fn(|l| g(a[l], b[l]));
    }
    out
}

/// `g` on each of the first `n` components of `x`; the rest zero.
#[inline(always)]
fn map(x: &Cell, n: u8, g: impl Fn(u32) -> u32) -> Cell {
    let mut out = ZERO;
    for (o, a) in out.iter_mut().zip(x).take(usize::from(n)) {
        *o = a.map(&g);
    }
    out
}

/// The sum of the products of the first `n` components of `x` and `y` in
/// `lane`.
#[inline(always)]
fn dot(x: &Cell, y: &Cell, n: u8, lane: usize) -> f32 {
    (0..usize::from(n))
        .map(|c| f(x[c][lane]) * f(y[c][lane]))
        .sum()
}

fn order(c: Cmp, o: Option<Ordering>) -> u32 {
    // A comparison with NaN is false; `!=` with NaN is true.
    let Some(o) = o else {
        return u32::from(c == Cmp::Ne);
    };
    u32::from(match c {
        Cmp::Eq => o == Ordering::Equal,
        Cmp::Ne => o != Ordering::Equal,
        Cmp::Lt => o == Ordering::Less,
        Cmp::Le => o != Ordering::Greater,
        Cmp::Gt => o == Ordering::Greater,
        Cmp::Ge => o != Ordering::Less,
    })
}

pub(super) fn binary(kind: Bin, x: &Cell, y: &Cell, n: u8, x1: bool, y1: bool) -> Cell {
    let z = |g: fn(u32, u32) -> u32| zip(x, y, n, x1, y1, g);
    match kind {
        Bin::FAdd => z(|a, c| b(f(a) + f(c))),
        Bin::FSub => z(|a, c| b(f(a) - f(c))),
        Bin::FMul => z(|a, c| b(f(a) * f(c))),
        Bin::FDiv => z(|a, c| b(f(a) / f(c))),
        Bin::FRem => z(|a, c| b(f(a) % f(c))),
        Bin::FMin => z(|a, c| b(f(a).min(f(c)))),
        Bin::FMax => z(|a, c| b(f(a).max(f(c)))),
        Bin::FStep => z(|edge, x| b(if f(x) < f(edge) { 0.0 } else { 1.0 })),
        Bin::IAdd | Bin::UAdd => z(u32::wrapping_add),
        Bin::ISub | Bin::USub => z(u32::wrapping_sub),
        Bin::IMul | Bin::UMul => z(u32::wrapping_mul),
        Bin::IDiv => z(|a, c| {
            if c == 0 {
                a
            } else {
                u(i(a).wrapping_div(i(c)))
            }
        }),
        Bin::IRem => z(|a, c| {
            if c == 0 {
                0
            } else {
                u(i(a).wrapping_rem(i(c)))
            }
        }),
        Bin::UDiv => z(|a, c| a.checked_div(c).unwrap_or(a)),
        Bin::URem => z(|a, c| a.checked_rem(c).unwrap_or(0)),
        Bin::IMin => z(|a, c| u(i(a).min(i(c)))),
        Bin::IMax => z(|a, c| u(i(a).max(i(c)))),
        Bin::UMin => z(u32::min),
        Bin::UMax => z(u32::max),
        Bin::And => z(|a, c| a & c),
        Bin::Or => z(|a, c| a | c),
        Bin::Xor => z(|a, c| a ^ c),
        Bin::Shl => z(|a, c| a.wrapping_shl(c & 31)),
        Bin::IShr => z(|a, c| u(i(a).wrapping_shr(c & 31))),
        Bin::UShr => z(|a, c| a.wrapping_shr(c & 31)),
        Bin::FCmp(c) => zip(x, y, n, x1, y1, |a, d| order(c, f(a).partial_cmp(&f(d)))),
        Bin::ICmp(c) => zip(x, y, n, x1, y1, |a, d| order(c, Some(i(a).cmp(&i(d))))),
        Bin::UCmp(c) => zip(x, y, n, x1, y1, |a, d| order(c, Some(a.cmp(&d)))),
    }
}

fn unary(kind: Un, x: &Cell, n: u8) -> Cell {
    match kind {
        Un::NegF => map(x, n, |a| b(-f(a))),
        Un::NegI => map(x, n, |a| u(i(a).wrapping_neg())),
        Un::Not => map(x, n, |a| a ^ 1),
        Un::BitNot => map(x, n, |a| !a),
        Un::AbsF => map(x, n, |a| b(f(a).abs())),
        Un::AbsI => map(x, n, |a| u(i(a).wrapping_abs())),
        Un::Sign => map(x, n, |a| {
            let a = f(a);
            b(if a > 0.0 {
                1.0
            } else if a < 0.0 {
                -1.0
            } else {
                0.0
            })
        }),
    }
}

pub(super) fn convert(x: &Cell, from: Scalar, to: Scalar, n: u8) -> Cell {
    match (from, to) {
        (Scalar::F32, Scalar::F32) => *x,
        (Scalar::F32, Scalar::I32) => map(x, n, |a| u(f(a) as i32)),
        (Scalar::F32, Scalar::U32) => map(x, n, |a| f(a) as u32),
        (Scalar::F32, Scalar::Bool) => map(x, n, |a| u32::from(f(a) != 0.0)),
        (Scalar::I32, Scalar::F32) => map(x, n, |a| b(i(a) as f32)),
        (Scalar::U32 | Scalar::Bool, Scalar::F32) => map(x, n, |a| b(a as f32)),
        (_, Scalar::Bool) => map(x, n, |a| u32::from(a != 0)),
        // Between `I32`, `U32` and `Bool` to integers the bits stay.
        _ => *x,
    }
}

/// A fine derivative of `x` across the quad, its lanes (0,0) (1,0) (0,1)
/// (1,1).
fn derivative(kind: Derivative, x: &Cell, n: u8) -> Cell {
    let dx = |c: &[u32; 4], l: usize| f(c[l | 1]) - f(c[l & !1]);
    let dy = |c: &[u32; 4], l: usize| f(c[(l & 1) | 2]) - f(c[l & 1]);
    let mut out = ZERO;
    for (o, c) in out.iter_mut().zip(x).take(usize::from(n)) {
        *o = std::array::from_fn(|l| {
            b(match kind {
                Derivative::X => dx(c, l),
                Derivative::Y => dy(c, l),
                Derivative::Width => dx(c, l).abs() + dy(c, l).abs(),
            })
        });
    }
    out
}

impl<'a> Machine<'a> {
    pub(super) fn new(
        code: &'a Code,
        textures: &'a [CpuTexture<'a>],
        samplers: &'a [SamplerDesc],
    ) -> Machine<'a> {
        Machine {
            code,
            textures,
            samplers,
            regs: vec![ZERO; code.regs as usize],
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
        let (ret, _) = self.enter(f, std::array::from_fn(|l| l < count));
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
        let (ret, flow) = self.enter(f, [true; 4]);
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
        let mut flow = Flow {
            ret: f.ret,
            returned: [false; 4],
            discarded: [false; 4],
            broke: [false; 4],
            continued: [false; 4],
        };
        self.block(&f.body, &mut mask, &mut flow);
        (f.ret, flow)
    }

    /// Runs `stmts` on the lanes of `mask`; a lane that returns, discards,
    /// breaks or continues leaves it.
    fn block(&mut self, stmts: &'a [CStmt], mask: &mut Mask, flow: &mut Flow) {
        for stmt in stmts {
            if !mask.iter().any(|m| *m) {
                return;
            }
            self.stmt(stmt, mask, flow);
        }
    }

    /// `count` registers from `src` to `dst` in the lanes of `mask`.
    fn masked(&mut self, dst: Reg, src: Reg, count: u32, mask: Mask) {
        if mask == [true; 4] {
            let src = src as usize;
            self.regs
                .copy_within(src..src + count as usize, dst as usize);
            return;
        }
        for k in 0..count as usize {
            let from = self.regs[src as usize + k];
            for (t, s) in self.regs[dst as usize + k].iter_mut().zip(&from) {
                for l in 0..4 {
                    if mask[l] {
                        t[l] = s[l];
                    }
                }
            }
        }
    }

    /// Takes the lanes of `mask` out of it, flagging them in `flags`.
    fn leave(mask: &mut Mask, flags: &mut Mask) {
        for (m, flag) in mask.iter_mut().zip(flags) {
            *flag |= *m;
            *m = false;
        }
    }

    fn stmt(&mut self, stmt: &'a CStmt, mask: &mut Mask, flow: &mut Flow) {
        match stmt {
            CStmt::Let { run, dst, count } => {
                self.run(*run);
                self.masked(*dst, run.value, *count, *mask);
            }
            CStmt::Declare { dst, count } => {
                for k in 0..*count as usize {
                    for c in &mut self.regs[*dst as usize + k] {
                        for l in 0..4 {
                            if mask[l] {
                                c[l] = 0;
                            }
                        }
                    }
                }
            }
            CStmt::Assign { run, target } => {
                self.run(*run);
                match *target {
                    Target::Regs { reg, count } => self.masked(reg, run.value, count, *mask),
                    Target::Components { reg, at, n } => {
                        let from = self.regs[run.value as usize];
                        let to = &mut self.regs[reg as usize][usize::from(at)..];
                        for (t, s) in to.iter_mut().zip(&from).take(usize::from(n)) {
                            for l in 0..4 {
                                if mask[l] {
                                    t[l] = s[l];
                                }
                            }
                        }
                    }
                }
            }
            CStmt::If {
                cond,
                then,
                otherwise,
            } => {
                self.run(*cond);
                let truth = self.regs[cond.value as usize][0];
                let mut then_mask: Mask = std::array::from_fn(|l| mask[l] && truth[l] != 0);
                let mut else_mask: Mask = std::array::from_fn(|l| mask[l] && truth[l] == 0);
                self.block(then, &mut then_mask, flow);
                self.block(otherwise, &mut else_mask, flow);
                *mask = std::array::from_fn(|l| then_mask[l] || else_mask[l]);
            }
            CStmt::For(l) => self.for_loop(l, mask, flow),
            CStmt::Break => Self::leave(mask, &mut flow.broke),
            CStmt::Continue => Self::leave(mask, &mut flow.continued),
            CStmt::Return { run, count } => {
                if let Some(run) = run {
                    self.run(*run);
                    self.masked(flow.ret, run.value, *count, *mask);
                }
                Self::leave(mask, &mut flow.returned);
            }
            CStmt::Discard => Self::leave(mask, &mut flow.discarded),
        }
    }

    fn for_loop(&mut self, lp: &'a CLoop, mask: &mut Mask, flow: &mut Flow) {
        let entered = *mask;
        let saved = (flow.broke, flow.continued);
        flow.broke = [false; 4];
        let mut running = entered;
        self.run(lp.start);
        self.masked(lp.var, lp.start.value, 1, running);
        for _ in 0..lp.max {
            if lp.guarded {
                self.run(lp.end);
                let (var, end) = (
                    self.regs[lp.var as usize][0],
                    self.regs[lp.end.value as usize][0],
                );
                for l in 0..4 {
                    if running[l] {
                        let o = if lp.signed {
                            i(var[l]).cmp(&i(end[l]))
                        } else {
                            var[l].cmp(&end[l])
                        };
                        running[l] = o.is_lt() || (lp.inclusive && o.is_eq());
                    }
                }
            }
            if !running.iter().any(|r| *r) {
                break;
            }
            flow.continued = [false; 4];
            let mut body = running;
            self.block(&lp.body, &mut body, flow);
            let var = &mut self.regs[lp.var as usize][0];
            for l in 0..4 {
                running[l] = (body[l] || flow.continued[l]) && !flow.broke[l] && !flow.finished(l);
                if running[l] {
                    var[l] = var[l].wrapping_add(1);
                }
            }
        }
        for l in 0..4 {
            mask[l] = entered[l] && !flow.finished(l);
        }
        (flow.broke, flow.continued) = saved;
    }

    /// Runs an expression's operations.
    fn run(&mut self, run: Run) {
        let code = self.code;
        for op in &code.ops[run.ops.0 as usize..run.ops.1 as usize] {
            self.op(op);
        }
    }

    #[inline(always)]
    fn reg(&self, reg: Reg) -> Cell {
        self.regs[reg as usize]
    }

    fn op(&mut self, op: &'a Op) {
        let (dst, out) = match *op {
            Op::Const { dst, bits } => (dst, bits.map(|c| [c; 4])),
            Op::Copy { dst, src, count } => {
                let src = src as usize;
                self.regs
                    .copy_within(src..src + count as usize, dst as usize);
                return;
            }
            Op::Place {
                dst,
                to,
                src,
                at,
                n,
            } => {
                let from = self.reg(src);
                let (to, at, n) = (usize::from(to), usize::from(at), usize::from(n));
                self.regs[dst as usize][to..to + n].copy_from_slice(&from[at..at + n]);
                return;
            }
            Op::Clear { dst } => (dst, ZERO),
            Op::Splat { dst, src, n } => {
                let from = self.reg(src);
                let mut out = ZERO;
                out[..usize::from(n)].fill(from[0]);
                (dst, out)
            }
            Op::Swizzle { dst, src, lanes, n } => {
                let from = self.reg(src);
                let mut out = ZERO;
                for (o, &k) in out.iter_mut().zip(&lanes).take(usize::from(n)) {
                    *o = from[usize::from(k)];
                }
                (dst, out)
            }
            Op::Unary { kind, dst, src, n } => (dst, unary(kind, &self.reg(src), n)),
            Op::Binary {
                kind,
                dst,
                a,
                b,
                n,
                a1,
                b1,
            } => (dst, binary(kind, &self.reg(a), &self.reg(b), n, a1, b1)),
            Op::MatMul { n, dst, a, b: m } => {
                let n = usize::from(n);
                for c in 0..n {
                    let column = self.reg(m + c as u32);
                    let mut out = ZERO;
                    for (row, o) in out.iter_mut().enumerate().take(n) {
                        *o = std::array::from_fn(|l| {
                            b((0..n)
                                .map(|j| f(self.regs[a as usize + j][row][l]) * f(column[j][l]))
                                .sum())
                        });
                    }
                    self.regs[dst as usize + c] = out;
                }
                return;
            }
            Op::MatVec { n, dst, m, v } => {
                let (v, n) = (self.reg(v), usize::from(n));
                let mut out = ZERO;
                for (row, o) in out.iter_mut().enumerate().take(n) {
                    *o = std::array::from_fn(|l| {
                        b((0..n)
                            .map(|c| f(self.regs[m as usize + c][row][l]) * f(v[c][l]))
                            .sum())
                    });
                }
                (dst, out)
            }
            Op::VecMat { n, dst, v, m } => {
                let (v, n) = (self.reg(v), usize::from(n));
                let mut out = ZERO;
                for (c, o) in out.iter_mut().enumerate().take(n) {
                    let column = self.reg(m + c as u32);
                    *o = std::array::from_fn(|l| {
                        b((0..n).map(|row| f(v[row][l]) * f(column[row][l])).sum())
                    });
                }
                (dst, out)
            }
            Op::MatAdd {
                n,
                dst,
                a,
                b: m,
                sub,
            } => {
                let kind = if sub { Bin::FSub } else { Bin::FAdd };
                for c in 0..u32::from(n) {
                    let out = binary(kind, &self.reg(a + c), &self.reg(m + c), n, false, false);
                    self.regs[(dst + c) as usize] = out;
                }
                return;
            }
            Op::MatScale { n, dst, m, s } => {
                let s = self.reg(s);
                for c in 0..u32::from(n) {
                    let out = binary(Bin::FMul, &self.reg(m + c), &s, n, false, true);
                    self.regs[(dst + c) as usize] = out;
                }
                return;
            }
            Op::MatNeg { n, dst, m } => {
                for c in 0..u32::from(n) {
                    let out = unary(Un::NegF, &self.reg(m + c), n);
                    self.regs[(dst + c) as usize] = out;
                }
                return;
            }
            Op::Transpose { n, dst, m } => {
                let n = usize::from(n);
                for c in 0..n {
                    let mut out = ZERO;
                    for (row, o) in out.iter_mut().enumerate().take(n) {
                        *o = self.regs[m as usize + row][c];
                    }
                    self.regs[dst as usize + c] = out;
                }
                return;
            }
            Op::Convert {
                dst,
                src,
                from,
                to,
                n,
            } => (dst, convert(&self.reg(src), from, to, n)),
            Op::IndexLane {
                dst,
                src,
                index,
                len,
            } => {
                let (x, at) = (self.reg(src), self.reg(index));
                let last = usize::from(len) - 1;
                let mut out = ZERO;
                out[0] = std::array::from_fn(|l| x[(at[0][l] as usize).min(last)][l]);
                (dst, out)
            }
            Op::IndexColumn {
                dst,
                base,
                index,
                n,
            } => {
                let at = self.reg(index);
                let last = usize::from(n) - 1;
                let mut out = ZERO;
                for l in 0..4 {
                    let column = &self.regs[base as usize + (at[0][l] as usize).min(last)];
                    for (o, c) in out.iter_mut().zip(column).take(usize::from(n)) {
                        o[l] = c[l];
                    }
                }
                (dst, out)
            }
            Op::Math1 { f: g, dst, src, n } => (dst, map(&self.reg(src), n, |x| b(g(f(x))))),
            Op::Math2 {
                f: g,
                dst,
                a,
                b: c,
                n,
                a1,
                b1,
            } => (
                dst,
                zip(&self.reg(a), &self.reg(c), n, a1, b1, |x, y| {
                    b(g(f(x), f(y)))
                }),
            ),
            Op::Clamp {
                scalar,
                dst,
                x,
                lo,
                hi,
                n,
                lo1,
                hi1,
            } => {
                let (up, down) = match scalar {
                    Scalar::I32 => (Bin::IMax, Bin::IMin),
                    Scalar::U32 => (Bin::UMax, Bin::UMin),
                    _ => (Bin::FMax, Bin::FMin),
                };
                let raised = binary(up, &self.reg(x), &self.reg(lo), n, false, lo1);
                (dst, binary(down, &raised, &self.reg(hi), n, false, hi1))
            }
            Op::Mix {
                dst,
                a,
                b: c,
                t,
                n,
                t1,
            } => {
                let (x, y, t) = (self.reg(a), self.reg(c), self.reg(t));
                let mut out = ZERO;
                for (k, o) in out.iter_mut().enumerate().take(usize::from(n)) {
                    let tk = &t[if t1 { 0 } else { k }];
                    *o = std::array::from_fn(|l| {
                        let (x, y) = (f(x[k][l]), f(y[k][l]));
                        b(x + (y - x) * f(tk[l]))
                    });
                }
                (dst, out)
            }
            Op::Smoothstep {
                dst,
                e0,
                e1,
                x,
                n,
                e01,
                e11,
            } => {
                let (e0, e1, x) = (self.reg(e0), self.reg(e1), self.reg(x));
                let mut out = ZERO;
                for (k, o) in out.iter_mut().enumerate().take(usize::from(n)) {
                    let (lo, hi) = (&e0[if e01 { 0 } else { k }], &e1[if e11 { 0 } else { k }]);
                    *o = std::array::from_fn(|l| {
                        let (lo, hi) = (f(lo[l]), f(hi[l]));
                        let t = ((f(x[k][l]) - lo) / (hi - lo)).clamp(0.0, 1.0);
                        b(t * t * (3.0 - 2.0 * t))
                    });
                }
                (dst, out)
            }
            Op::Dot { dst, a, b: c, n } => {
                let (x, y) = (self.reg(a), self.reg(c));
                let mut out = ZERO;
                out[0] = std::array::from_fn(|l| b(dot(&x, &y, n, l)));
                (dst, out)
            }
            Op::Length { dst, src, n } => {
                let x = self.reg(src);
                let mut out = ZERO;
                out[0] = std::array::from_fn(|l| b(dot(&x, &x, n, l).sqrt()));
                (dst, out)
            }
            Op::Distance { dst, a, b: c, n } => {
                let d = binary(Bin::FSub, &self.reg(a), &self.reg(c), n, false, false);
                let mut out = ZERO;
                out[0] = std::array::from_fn(|l| b(dot(&d, &d, n, l).sqrt()));
                (dst, out)
            }
            Op::Normalize { dst, src, n } => {
                let x = self.reg(src);
                let len: [f32; 4] = std::array::from_fn(|l| dot(&x, &x, n, l).sqrt());
                let mut out = ZERO;
                for (o, c) in out.iter_mut().zip(&x).take(usize::from(n)) {
                    *o = std::array::from_fn(|l| b(f(c[l]) / len[l]));
                }
                (dst, out)
            }
            Op::Cross { dst, a, b: c } => {
                let (x, y) = (self.reg(a), self.reg(c));
                let mut out = ZERO;
                for (k, o) in out.iter_mut().enumerate().take(3) {
                    let (p, q) = ((k + 1) % 3, (k + 2) % 3);
                    *o = std::array::from_fn(|l| {
                        b(f(x[p][l]) * f(y[q][l]) - f(x[q][l]) * f(y[p][l]))
                    });
                }
                (dst, out)
            }
            Op::Derivative { kind, dst, src, n } => (dst, derivative(kind, &self.reg(src), n)),
            Op::Sample {
                dst,
                texture,
                sampler,
                uv,
                scalar,
            } => {
                let uv = self.reg(uv);
                let mut out = ZERO;
                // A binding the draw left out samples as zero.
                if let (Some(texture), Some(sampler)) = (
                    self.textures.get(texture as usize),
                    self.samplers.get(sampler as usize),
                ) {
                    let take = if scalar { 1 } else { 4 };
                    for l in 0..4 {
                        let texel = sample(texture, sampler, f(uv[0][l]), f(uv[1][l]));
                        for (o, t) in out.iter_mut().zip(texel).take(take) {
                            o[l] = b(t);
                        }
                    }
                }
                (dst, out)
            }
            Op::QuadVertex { dst, src } => {
                // Two triangles, (0,0) (1,0) (0,1) and (1,0) (1,1) (0,1).
                const X: [f32; 6] = [0.0, 1.0, 0.0, 1.0, 1.0, 0.0];
                const Y: [f32; 6] = [0.0, 0.0, 1.0, 0.0, 1.0, 1.0];
                let vid = self.reg(src)[0];
                let mut out = ZERO;
                out[0] = vid.map(|v| b(X[(v % 6) as usize]));
                out[1] = vid.map(|v| b(Y[(v % 6) as usize]));
                (dst, out)
            }
            Op::ToClip { dst, pos, viewport } => {
                let (p, v) = (self.reg(pos), self.reg(viewport));
                let mut out = ZERO;
                out[0] = std::array::from_fn(|l| b(f(p[0][l]) / f(v[0][l]) * 2.0 - 1.0));
                out[1] = std::array::from_fn(|l| b(1.0 - f(p[1][l]) / f(v[1][l]) * 2.0));
                out[3] = [b(1.0); 4];
                (dst, out)
            }
            Op::RoundedRectSdf {
                dst,
                p,
                size,
                radius,
            } => {
                let (p, size, radius) = (self.reg(p), self.reg(size), self.reg(radius));
                let mut out = ZERO;
                out[0] = std::array::from_fn(|l| {
                    let extent = [f(size[0][l]) * 0.5, f(size[1][l]) * 0.5];
                    let r = f(radius[0][l]).clamp(0.0, extent[0].min(extent[1]));
                    let q = [
                        (f(p[0][l]) - extent[0]).abs() - (extent[0] - r),
                        (f(p[1][l]) - extent[1]).abs() - (extent[1] - r),
                    ];
                    let outside = [q[0].max(0.0), q[1].max(0.0)];
                    let length = (outside[0] * outside[0] + outside[1] * outside[1]).sqrt();
                    b(length + q[0].max(q[1]).min(0.0) - r)
                });
                (dst, out)
            }
            Op::Call {
                function,
                args,
                dst,
                width,
            } => {
                let code = self.code;
                for &(src, param, w) in &code.args[args.0 as usize..args.1 as usize] {
                    let src = src as usize;
                    self.regs.copy_within(src..src + w as usize, param as usize);
                }
                let (ret, _) = self.enter(&code.functions[function as usize], [true; 4]);
                let ret = ret as usize;
                self.regs
                    .copy_within(ret..ret + width as usize, dst as usize);
                return;
            }
        };
        self.regs[dst as usize] = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut m = Machine::new(&code, &[], &[]);
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
