//! What a compiled program's operations and control flow do to its
//! registers, on four lanes at once: one implementation the interpreter
//! dispatches to and the Rust a program generates calls in line, so the two
//! agree to the bit.
//!
//! Every function takes the whole register file and register indices; with
//! the indices constant, as generated code passes them, each call reduces to
//! its arithmetic on fixed registers.

use std::cmp::Ordering;

use viso_gpu_abi::{CpuTexture, SamplerDesc};

pub use super::code::{Bin, Cmp, Derivative, Un};
use super::sample;
pub use crate::program::Scalar;

/// A register: four components by four lanes, each the bits of its scalar.
pub type Cell = [[u32; 4]; 4];

pub const ZERO: Cell = [[0; 4]; 4];

/// Which lanes run.
pub type Mask = [bool; 4];

/// The textures and samplers a draw binds.
#[derive(Debug, Clone, Copy)]
pub struct Env<'a> {
    pub textures: &'a [CpuTexture<'a>],
    pub samplers: &'a [SamplerDesc],
}

/// How the lanes of one function's run ended.
#[derive(Debug, Clone, Copy)]
pub struct Flow {
    /// The function's value's first register.
    pub ret: u32,
    pub returned: Mask,
    pub discarded: Mask,
    pub broke: Mask,
    pub continued: Mask,
}

impl Flow {
    /// A run of a function whose value is at `ret` that has not ended.
    #[inline(always)]
    pub fn new(ret: u32) -> Flow {
        Flow {
            ret,
            returned: [false; 4],
            discarded: [false; 4],
            broke: [false; 4],
            continued: [false; 4],
        }
    }

    #[inline(always)]
    pub fn finished(&self, lane: usize) -> bool {
        self.returned[lane] || self.discarded[lane]
    }
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

#[inline(always)]
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

#[inline(always)]
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

#[inline(always)]
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

#[inline(always)]
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
#[inline(always)]
pub(super) fn derivative(kind: Derivative, x: &Cell, n: u8) -> Cell {
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

/// The scalar functions a program's intrinsics take, by the name generated
/// code calls them by.
pub mod math {
    #[inline(always)]
    pub fn floor(x: f32) -> f32 {
        x.floor()
    }
    #[inline(always)]
    pub fn ceil(x: f32) -> f32 {
        x.ceil()
    }
    #[inline(always)]
    pub fn round(x: f32) -> f32 {
        x.round_ties_even()
    }
    #[inline(always)]
    pub fn trunc(x: f32) -> f32 {
        x.trunc()
    }
    #[inline(always)]
    pub fn fract(x: f32) -> f32 {
        x - x.floor()
    }
    #[inline(always)]
    pub fn sqrt(x: f32) -> f32 {
        x.sqrt()
    }
    #[inline(always)]
    pub fn inverse_sqrt(x: f32) -> f32 {
        1.0 / x.sqrt()
    }
    #[inline(always)]
    pub fn exp(x: f32) -> f32 {
        x.exp()
    }
    #[inline(always)]
    pub fn exp2(x: f32) -> f32 {
        x.exp2()
    }
    #[inline(always)]
    pub fn log(x: f32) -> f32 {
        x.ln()
    }
    #[inline(always)]
    pub fn log2(x: f32) -> f32 {
        x.log2()
    }
    #[inline(always)]
    pub fn sin(x: f32) -> f32 {
        x.sin()
    }
    #[inline(always)]
    pub fn cos(x: f32) -> f32 {
        x.cos()
    }
    #[inline(always)]
    pub fn tan(x: f32) -> f32 {
        x.tan()
    }
    #[inline(always)]
    pub fn asin(x: f32) -> f32 {
        x.asin()
    }
    #[inline(always)]
    pub fn acos(x: f32) -> f32 {
        x.acos()
    }
    #[inline(always)]
    pub fn atan(x: f32) -> f32 {
        x.atan()
    }
    #[inline(always)]
    pub fn sinh(x: f32) -> f32 {
        x.sinh()
    }
    #[inline(always)]
    pub fn cosh(x: f32) -> f32 {
        x.cosh()
    }
    #[inline(always)]
    pub fn tanh(x: f32) -> f32 {
        x.tanh()
    }
    #[inline(always)]
    pub fn radians(x: f32) -> f32 {
        x * 0.017_453_292
    }
    #[inline(always)]
    pub fn degrees(x: f32) -> f32 {
        x * 57.295_78
    }
    #[inline(always)]
    pub fn atan2(y: f32, x: f32) -> f32 {
        y.atan2(x)
    }
    #[inline(always)]
    pub fn pow(x: f32, y: f32) -> f32 {
        x.powf(y)
    }
}

// The operations, each on registers of `r`.

#[inline(always)]
pub fn constant(r: &mut [Cell], dst: u32, bits: [u32; 4]) {
    r[dst as usize] = bits.map(|c| [c; 4]);
}

/// `count` registers from `src` to `dst`.
#[inline(always)]
pub fn copy(r: &mut [Cell], dst: u32, src: u32, count: u32) {
    let src = src as usize;
    r.copy_within(src..src + count as usize, dst as usize);
}

/// Components `at..at + n` of `src` into `dst` from component `to`, the
/// rest of `dst` kept.
#[inline(always)]
pub fn place(r: &mut [Cell], dst: u32, to: u8, src: u32, at: u8, n: u8) {
    let from = r[src as usize];
    let (to, at, n) = (usize::from(to), usize::from(at), usize::from(n));
    r[dst as usize][to..to + n].copy_from_slice(&from[at..at + n]);
}

#[inline(always)]
pub fn clear(r: &mut [Cell], dst: u32) {
    r[dst as usize] = ZERO;
}

/// Component 0 of `src` in each of the first `n`.
#[inline(always)]
pub fn splat(r: &mut [Cell], dst: u32, src: u32, n: u8) {
    let from = r[src as usize];
    let mut out = ZERO;
    out[..usize::from(n)].fill(from[0]);
    r[dst as usize] = out;
}

#[inline(always)]
pub fn swizzle(r: &mut [Cell], dst: u32, src: u32, lanes: [u8; 4], n: u8) {
    let from = r[src as usize];
    let mut out = ZERO;
    for (o, &k) in out.iter_mut().zip(&lanes).take(usize::from(n)) {
        *o = from[usize::from(k)];
    }
    r[dst as usize] = out;
}

#[inline(always)]
pub fn unary_op(r: &mut [Cell], kind: Un, dst: u32, src: u32, n: u8) {
    r[dst as usize] = unary(kind, &r[src as usize], n);
}

/// `kind` component by component; a one-component operand (`a1`, `c1`) is
/// spread over the other's.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub fn binary_op(r: &mut [Cell], kind: Bin, dst: u32, a: u32, c: u32, n: u8, a1: bool, c1: bool) {
    r[dst as usize] = binary(kind, &r[a as usize], &r[c as usize], n, a1, c1);
}

/// `a * m`, two `n`×`n` matrices.
#[inline(always)]
pub fn mat_mul(r: &mut [Cell], n: u8, dst: u32, a: u32, m: u32) {
    let n = usize::from(n);
    for c in 0..n {
        let column = r[m as usize + c];
        let mut out = ZERO;
        for (row, o) in out.iter_mut().enumerate().take(n) {
            *o = std::array::from_fn(|l| {
                b((0..n)
                    .map(|j| f(r[a as usize + j][row][l]) * f(column[j][l]))
                    .sum())
            });
        }
        r[dst as usize + c] = out;
    }
}

/// `m * v`.
#[inline(always)]
pub fn mat_vec(r: &mut [Cell], n: u8, dst: u32, m: u32, v: u32) {
    let (v, n) = (r[v as usize], usize::from(n));
    let mut out = ZERO;
    for (row, o) in out.iter_mut().enumerate().take(n) {
        *o = std::array::from_fn(|l| {
            b((0..n)
                .map(|c| f(r[m as usize + c][row][l]) * f(v[c][l]))
                .sum())
        });
    }
    r[dst as usize] = out;
}

/// `v * m`.
#[inline(always)]
pub fn vec_mat(r: &mut [Cell], n: u8, dst: u32, v: u32, m: u32) {
    let (v, n) = (r[v as usize], usize::from(n));
    let mut out = ZERO;
    for (c, o) in out.iter_mut().enumerate().take(n) {
        let column = r[m as usize + c];
        *o = std::array::from_fn(|l| b((0..n).map(|row| f(v[row][l]) * f(column[row][l])).sum()));
    }
    r[dst as usize] = out;
}

/// `a + m`, or `a - m`.
#[inline(always)]
pub fn mat_add(r: &mut [Cell], n: u8, dst: u32, a: u32, m: u32, sub: bool) {
    let kind = if sub { Bin::FSub } else { Bin::FAdd };
    for c in 0..u32::from(n) {
        let out = binary(
            kind,
            &r[(a + c) as usize],
            &r[(m + c) as usize],
            n,
            false,
            false,
        );
        r[(dst + c) as usize] = out;
    }
}

/// `m * s`, a scalar.
#[inline(always)]
pub fn mat_scale(r: &mut [Cell], n: u8, dst: u32, m: u32, s: u32) {
    let s = r[s as usize];
    for c in 0..u32::from(n) {
        let out = binary(Bin::FMul, &r[(m + c) as usize], &s, n, false, true);
        r[(dst + c) as usize] = out;
    }
}

/// `-m`.
#[inline(always)]
pub fn mat_neg(r: &mut [Cell], n: u8, dst: u32, m: u32) {
    for c in 0..u32::from(n) {
        let out = unary(Un::NegF, &r[(m + c) as usize], n);
        r[(dst + c) as usize] = out;
    }
}

#[inline(always)]
pub fn transpose(r: &mut [Cell], n: u8, dst: u32, m: u32) {
    let n = usize::from(n);
    for c in 0..n {
        let mut out = ZERO;
        for (row, o) in out.iter_mut().enumerate().take(n) {
            *o = r[m as usize + row][c];
        }
        r[dst as usize + c] = out;
    }
}

#[inline(always)]
pub fn convert_op(r: &mut [Cell], dst: u32, src: u32, from: Scalar, to: Scalar, n: u8) {
    r[dst as usize] = convert(&r[src as usize], from, to, n);
}

/// Component `index` of `src`, clamped to its `len`.
#[inline(always)]
pub fn index_lane(r: &mut [Cell], dst: u32, src: u32, index: u32, len: u8) {
    let (x, at) = (r[src as usize], r[index as usize]);
    let last = usize::from(len) - 1;
    let mut out = ZERO;
    out[0] = std::array::from_fn(|l| x[(at[0][l] as usize).min(last)][l]);
    r[dst as usize] = out;
}

/// Column `index` of the matrix at `base`, clamped to its `n`.
#[inline(always)]
pub fn index_column(r: &mut [Cell], dst: u32, base: u32, index: u32, n: u8) {
    let at = r[index as usize];
    let last = usize::from(n) - 1;
    let mut out = ZERO;
    for l in 0..4 {
        let column = &r[base as usize + (at[0][l] as usize).min(last)];
        for (o, c) in out.iter_mut().zip(column).take(usize::from(n)) {
            o[l] = c[l];
        }
    }
    r[dst as usize] = out;
}

#[inline(always)]
pub fn math1(r: &mut [Cell], g: fn(f32) -> f32, dst: u32, src: u32, n: u8) {
    r[dst as usize] = map(&r[src as usize], n, |x| b(g(f(x))));
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub fn math2(
    r: &mut [Cell],
    g: fn(f32, f32) -> f32,
    dst: u32,
    a: u32,
    c: u32,
    n: u8,
    a1: bool,
    c1: bool,
) {
    r[dst as usize] = zip(&r[a as usize], &r[c as usize], n, a1, c1, |x, y| {
        b(g(f(x), f(y)))
    });
}

/// `clamp(x, lo, hi)` of `scalar` lanes.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub fn clamp(
    r: &mut [Cell],
    scalar: Scalar,
    dst: u32,
    x: u32,
    lo: u32,
    hi: u32,
    n: u8,
    lo1: bool,
    hi1: bool,
) {
    let (up, down) = match scalar {
        Scalar::I32 => (Bin::IMax, Bin::IMin),
        Scalar::U32 => (Bin::UMax, Bin::UMin),
        _ => (Bin::FMax, Bin::FMin),
    };
    let raised = binary(up, &r[x as usize], &r[lo as usize], n, false, lo1);
    r[dst as usize] = binary(down, &raised, &r[hi as usize], n, false, hi1);
}

#[inline(always)]
pub fn mix(r: &mut [Cell], dst: u32, a: u32, c: u32, t: u32, n: u8, t1: bool) {
    let (x, y, t) = (r[a as usize], r[c as usize], r[t as usize]);
    let mut out = ZERO;
    for (k, o) in out.iter_mut().enumerate().take(usize::from(n)) {
        let tk = &t[if t1 { 0 } else { k }];
        *o = std::array::from_fn(|l| {
            let (x, y) = (f(x[k][l]), f(y[k][l]));
            b(x + (y - x) * f(tk[l]))
        });
    }
    r[dst as usize] = out;
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub fn smoothstep(r: &mut [Cell], dst: u32, e0: u32, e1: u32, x: u32, n: u8, e01: bool, e11: bool) {
    let (e0, e1, x) = (r[e0 as usize], r[e1 as usize], r[x as usize]);
    let mut out = ZERO;
    for (k, o) in out.iter_mut().enumerate().take(usize::from(n)) {
        let (lo, hi) = (&e0[if e01 { 0 } else { k }], &e1[if e11 { 0 } else { k }]);
        *o = std::array::from_fn(|l| {
            let (lo, hi) = (f(lo[l]), f(hi[l]));
            let t = ((f(x[k][l]) - lo) / (hi - lo)).clamp(0.0, 1.0);
            b(t * t * (3.0 - 2.0 * t))
        });
    }
    r[dst as usize] = out;
}

#[inline(always)]
pub fn dot_op(r: &mut [Cell], dst: u32, a: u32, c: u32, n: u8) {
    let (x, y) = (r[a as usize], r[c as usize]);
    let mut out = ZERO;
    out[0] = std::array::from_fn(|l| b(dot(&x, &y, n, l)));
    r[dst as usize] = out;
}

#[inline(always)]
pub fn length(r: &mut [Cell], dst: u32, src: u32, n: u8) {
    let x = r[src as usize];
    let mut out = ZERO;
    out[0] = std::array::from_fn(|l| b(dot(&x, &x, n, l).sqrt()));
    r[dst as usize] = out;
}

#[inline(always)]
pub fn distance(r: &mut [Cell], dst: u32, a: u32, c: u32, n: u8) {
    let d = binary(Bin::FSub, &r[a as usize], &r[c as usize], n, false, false);
    let mut out = ZERO;
    out[0] = std::array::from_fn(|l| b(dot(&d, &d, n, l).sqrt()));
    r[dst as usize] = out;
}

#[inline(always)]
pub fn normalize(r: &mut [Cell], dst: u32, src: u32, n: u8) {
    let x = r[src as usize];
    let len: [f32; 4] = std::array::from_fn(|l| dot(&x, &x, n, l).sqrt());
    let mut out = ZERO;
    for (o, c) in out.iter_mut().zip(&x).take(usize::from(n)) {
        *o = std::array::from_fn(|l| b(f(c[l]) / len[l]));
    }
    r[dst as usize] = out;
}

#[inline(always)]
pub fn cross(r: &mut [Cell], dst: u32, a: u32, c: u32) {
    let (x, y) = (r[a as usize], r[c as usize]);
    let mut out = ZERO;
    for (k, o) in out.iter_mut().enumerate().take(3) {
        let (p, q) = ((k + 1) % 3, (k + 2) % 3);
        *o = std::array::from_fn(|l| b(f(x[p][l]) * f(y[q][l]) - f(x[q][l]) * f(y[p][l])));
    }
    r[dst as usize] = out;
}

/// A fine derivative across the quad.
#[inline(always)]
pub fn derivative_op(r: &mut [Cell], kind: Derivative, dst: u32, src: u32, n: u8) {
    r[dst as usize] = derivative(kind, &r[src as usize], n);
}

/// Texture `texture` through sampler `sampler` at each lane's `uv`; a
/// binding the draw left out samples as zero.
#[inline(always)]
pub fn sample_op(
    r: &mut [Cell],
    env: &Env<'_>,
    dst: u32,
    texture: u32,
    sampler: u32,
    uv: u32,
    scalar: bool,
) {
    let uv = r[uv as usize];
    let mut out = ZERO;
    if let (Some(texture), Some(sampler)) = (
        env.textures.get(texture as usize),
        env.samplers.get(sampler as usize),
    ) {
        let take = if scalar { 1 } else { 4 };
        for l in 0..4 {
            let texel = sample(texture, sampler, f(uv[0][l]), f(uv[1][l]));
            for (o, t) in out.iter_mut().zip(texel).take(take) {
                o[l] = b(t);
            }
        }
    }
    r[dst as usize] = out;
}

#[inline(always)]
pub fn quad_vertex(r: &mut [Cell], dst: u32, src: u32) {
    // Two triangles, (0,0) (1,0) (0,1) and (1,0) (1,1) (0,1).
    const X: [f32; 6] = [0.0, 1.0, 0.0, 1.0, 1.0, 0.0];
    const Y: [f32; 6] = [0.0, 0.0, 1.0, 0.0, 1.0, 1.0];
    let vid = r[src as usize][0];
    let mut out = ZERO;
    out[0] = vid.map(|v| b(X[(v % 6) as usize]));
    out[1] = vid.map(|v| b(Y[(v % 6) as usize]));
    r[dst as usize] = out;
}

#[inline(always)]
pub fn to_clip(r: &mut [Cell], dst: u32, pos: u32, viewport: u32) {
    let (p, v) = (r[pos as usize], r[viewport as usize]);
    let mut out = ZERO;
    out[0] = std::array::from_fn(|l| b(f(p[0][l]) / f(v[0][l]) * 2.0 - 1.0));
    out[1] = std::array::from_fn(|l| b(1.0 - f(p[1][l]) / f(v[1][l]) * 2.0));
    out[3] = [b(1.0); 4];
    r[dst as usize] = out;
}

#[inline(always)]
pub fn rounded_rect_sdf(r: &mut [Cell], dst: u32, p: u32, size: u32, radius: u32) {
    let (p, size, radius) = (r[p as usize], r[size as usize], r[radius as usize]);
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
    r[dst as usize] = out;
}

// The control flow, on the lanes of a mask.

/// Whether any lane of `mask` runs.
#[inline(always)]
pub fn any(mask: &Mask) -> bool {
    mask.iter().any(|m| *m)
}

/// `count` registers from `src` to `dst` in the lanes of `mask`.
#[inline(always)]
pub fn masked(r: &mut [Cell], dst: u32, src: u32, count: u32, mask: Mask) {
    if mask == [true; 4] {
        copy(r, dst, src, count);
        return;
    }
    for k in 0..count as usize {
        let from = r[src as usize + k];
        for (t, s) in r[dst as usize + k].iter_mut().zip(&from) {
            for l in 0..4 {
                if mask[l] {
                    t[l] = s[l];
                }
            }
        }
    }
}

/// `count` registers from `dst` zeroed in the lanes of `mask`.
#[inline(always)]
pub fn declare(r: &mut [Cell], dst: u32, count: u32, mask: Mask) {
    for k in 0..count as usize {
        for c in &mut r[dst as usize + k] {
            for l in 0..4 {
                if mask[l] {
                    c[l] = 0;
                }
            }
        }
    }
}

/// The first `n` components of `src` into those of `reg` from `at`, in the
/// lanes of `mask`.
#[inline(always)]
pub fn assign_components(r: &mut [Cell], reg: u32, at: u8, n: u8, src: u32, mask: Mask) {
    let from = r[src as usize];
    let to = &mut r[reg as usize][usize::from(at)..];
    for (t, s) in to.iter_mut().zip(&from).take(usize::from(n)) {
        for l in 0..4 {
            if mask[l] {
                t[l] = s[l];
            }
        }
    }
}

/// Takes the lanes of `mask` out of it, flagging them in `flags`.
#[inline(always)]
pub fn leave(mask: &mut Mask, flags: &mut Mask) {
    for (m, flag) in mask.iter_mut().zip(flags) {
        *flag |= *m;
        *m = false;
    }
}

/// The lanes of `mask` whose condition at `cond` holds, and those whose
/// does not.
#[inline(always)]
pub fn split(r: &[Cell], cond: u32, mask: &Mask) -> (Mask, Mask) {
    let truth = r[cond as usize][0];
    (
        std::array::from_fn(|l| mask[l] && truth[l] != 0),
        std::array::from_fn(|l| mask[l] && truth[l] == 0),
    )
}

/// The lanes of either branch of an `if` that still run.
#[inline(always)]
pub fn join(then: &Mask, otherwise: &Mask) -> Mask {
    std::array::from_fn(|l| then[l] || otherwise[l])
}

/// A loop's running lanes kept to those whose variable at `var` is still
/// before the end at `end`.
#[inline(always)]
pub fn loop_guard(
    r: &[Cell],
    var: u32,
    end: u32,
    signed: bool,
    inclusive: bool,
    running: &mut Mask,
) {
    let (var, end) = (r[var as usize][0], r[end as usize][0]);
    for l in 0..4 {
        if running[l] {
            let o = if signed {
                i(var[l]).cmp(&i(end[l]))
            } else {
                var[l].cmp(&end[l])
            };
            running[l] = o.is_lt() || (inclusive && o.is_eq());
        }
    }
}

/// After a loop body ran on `body`: the lanes that go round again, each
/// stepping its variable at `var`.
#[inline(always)]
pub fn loop_next(r: &mut [Cell], var: u32, running: &mut Mask, body: &Mask, flow: &Flow) {
    let var = &mut r[var as usize][0];
    for l in 0..4 {
        running[l] = (body[l] || flow.continued[l]) && !flow.broke[l] && !flow.finished(l);
        if running[l] {
            var[l] = var[l].wrapping_add(1);
        }
    }
}

/// The lanes that entered a loop and did not return or discard in it.
#[inline(always)]
pub fn loop_exit(mask: &mut Mask, entered: &Mask, flow: &Flow) {
    for l in 0..4 {
        mask[l] = entered[l] && !flow.finished(l);
    }
}
