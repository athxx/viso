//! A program as backend source: Metal Shading Language, WGSL and HLSL from
//! one walk of the typed body, each spelling its interface from the layout.
//!
//! Every target reads the same buffers:
//!
//! - the uniform block at the layout's offsets: a `constant` struct at
//!   `buffer(0)` with explicit padding (MSL), a `var<uniform>` at
//!   `@group(0) @binding(0)` with `@size` where the layout pads (WGSL), a
//!   `cbuffer` at `b0` with a `packoffset` per leaf (HLSL);
//! - the tightly packed instance array: a `device` buffer of packed vectors at
//!   `buffer(1)` (MSL), a read-only storage buffer at `@group(0) @binding(1)`
//!   (WGSL) or a `StructuredBuffer` at `t0, space1` (HLSL) of 4-byte scalar
//!   leaves, which no layout rule can pad;
//! - textures and samplers by index (`@group(1)`, textures first, on WGSL).
//!
//! Instance members the fragment entry reads cross as flat varyings after the
//! declared ones. Every name is generated (`l3`, `fn1`, `u0`), so no source
//! name can collide with a target word. HLSL holds a matrix transposed (its
//! rows are the program's columns), so `m * v` is `mul(v, m)` and a column is
//! a row index.

mod hlsl;
mod msl;
mod wgsl;

use std::fmt::Write;

use super::layout::{ShaderInterface, VERTEX_ENTRY};
use super::{
    BinaryOp, Block, Builtin, Expr, ExprKind, Function, Intrinsic, Lane, Loop, Place, Program,
    Root, Scalar, Stage, Stmt, Texel, Ty, UnaryOp,
};

/// The source language to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// Metal Shading Language 2.
    Msl,
    /// WGSL, for WebGPU and, through SPIR-V, Vulkan.
    Wgsl,
    /// HLSL shader model 5.1, for Direct3D 12.
    Hlsl,
}

/// The program as `target` source, laid out by `interface`.
pub fn emit(program: &Program, interface: &ShaderInterface, target: Target) -> String {
    let mut g = Gen {
        p: program,
        i: interface,
        t: target,
        out: String::with_capacity(8192),
        depth: 0,
        stage: None,
        forwarded: forwarded_instance(program),
        guards: 0,
    };
    match target {
        Target::Msl => msl::module(&mut g),
        Target::Wgsl => wgsl::module(&mut g),
        Target::Hlsl => hlsl::module(&mut g),
    }
    g.out
}

/// The instance members the fragment entry reads.
fn forwarded_instance(p: &Program) -> Vec<u32> {
    let mut out = Vec::new();
    if let Some(f) = &p.fragment {
        f.body.visit(&mut |e| {
            if let ExprKind::Instance(i) = e.kind
                && !out.contains(&i)
            {
                out.push(i);
            }
        });
    }
    out.sort_unstable();
    out
}

/// The lanes a leaf holds: a matrix's by column.
fn leaf_lanes(ty: Ty) -> u8 {
    match ty {
        Ty::Matrix(n) => n * n,
        Ty::Color => 4,
        other => other.lanes().map_or(1, |(_, n)| n),
    }
}

const LANES: [&str; 4] = ["x", "y", "z", "w"];

struct Gen<'a> {
    p: &'a Program,
    i: &'a ShaderInterface,
    t: Target,
    out: String,
    depth: usize,
    stage: Option<Stage>,
    forwarded: Vec<u32>,
    guards: u32,
}

impl Gen<'_> {
    fn line(&mut self, text: &str) {
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// The target's spelling of a value of `ty`.
    fn ty(&self, ty: Ty) -> String {
        match self.t {
            Target::Msl => msl::ty(ty),
            Target::Wgsl => wgsl::ty(ty),
            Target::Hlsl => hlsl::ty(ty),
        }
    }

    fn float(&self, v: f32) -> String {
        if !v.is_finite() {
            let bits = v.to_bits();
            return match self.t {
                Target::Msl => format!("as_type<float>({bits:#x}u)"),
                Target::Wgsl => format!("bitcast<f32>({bits:#x}u)"),
                Target::Hlsl => format!("asfloat({bits:#x}u)"),
            };
        }
        let text = format!("{v:?}f");
        if v.is_sign_negative() {
            format!("({text})")
        } else {
            text
        }
    }

    fn lane(&self, l: Lane) -> String {
        let int = if self.t == Target::Wgsl { "i" } else { "" };
        match l {
            Lane::Bool(b) => b.to_string(),
            Lane::I32(i32::MIN) => format!("(-2147483647{int} - 1{int})"),
            Lane::I32(v) if v < 0 => format!("({v}{int})"),
            Lane::I32(v) => format!("{v}{int}"),
            Lane::U32(v) => format!("{v}u"),
            Lane::F32(v) => self.float(v),
        }
    }

    /// `text`, a scalar, spread to vector type `ty`.
    fn splat_text(&self, ty: Ty, text: &str) -> String {
        match self.t {
            Target::Hlsl => format!("(({})({text}))", self.ty(ty)),
            _ => format!("{}({text})", self.ty(ty)),
        }
    }

    /// `text` converted to `ty`.
    fn convert(&self, ty: Ty, text: &str) -> String {
        match self.t {
            Target::Hlsl => format!("(({})({text}))", self.ty(ty)),
            _ => format!("{}({text})", self.ty(ty)),
        }
    }

    /// A record or `VertexOutput` value from its fields.
    fn record(&self, ty: Ty, parts: &[String]) -> String {
        let name = self.ty(ty);
        match self.t {
            Target::Msl => format!("{name}{{{}}}", parts.join(", ")),
            Target::Wgsl => format!("{name}({})", parts.join(", ")),
            Target::Hlsl => format!("make_{name}({})", parts.join(", ")),
        }
    }

    /// Declares local `local` of `f`, initialized to `value` or to zero.
    fn declare(&mut self, f: &Function, local: u32, value: Option<String>) {
        let l = &f.locals[local as usize];
        let ty = self.ty(l.ty);
        let text = match (self.t, value) {
            (Target::Wgsl, Some(v)) if l.mutable => format!("var l{local}: {ty} = {v};"),
            (Target::Wgsl, Some(v)) => format!("let l{local}: {ty} = {v};"),
            (Target::Wgsl, None) => format!("var l{local}: {ty};"),
            (_, Some(v)) => format!("{ty} l{local} = {v};"),
            (Target::Msl, None) => format!("{ty} l{local} = {{}};"),
            (Target::Hlsl, None) => format!("{ty} l{local} = ({ty})0;"),
        };
        self.line(&text);
    }

    /// The program's helper functions, in call order.
    fn functions(&mut self) {
        for (index, f) in self.p.functions.iter().enumerate() {
            let ret = self.ty(f.ret);
            let params = &f.locals[..f.params as usize];
            let head = match self.t {
                Target::Wgsl => {
                    let list: Vec<String> = params
                        .iter()
                        .enumerate()
                        .map(|(i, l)| {
                            let name = if l.mutable { 'p' } else { 'l' };
                            format!("{name}{i}: {}", self.ty(l.ty))
                        })
                        .collect();
                    format!("fn fn{index}({}) -> {ret} {{", list.join(", "))
                }
                _ => {
                    let list: Vec<String> = params
                        .iter()
                        .enumerate()
                        .map(|(i, l)| format!("{} l{i}", self.ty(l.ty)))
                        .collect();
                    let storage = if self.t == Target::Msl { "static " } else { "" };
                    format!("{storage}{ret} fn{index}({}) {{", list.join(", "))
                }
            };
            self.line(&head);
            self.depth = 1;
            if self.t == Target::Wgsl {
                for (i, l) in params.iter().enumerate() {
                    if l.mutable {
                        let ty = self.ty(l.ty);
                        self.line(&format!("var l{i}: {ty} = p{i};"));
                    }
                }
            }
            self.block(&f.body, f);
            self.depth = 0;
            self.out.push_str("}\n\n");
        }
    }

    /// An entry's body after its target-specific head.
    fn entry_body(&mut self, f: &Function) {
        let stage = f.stage.unwrap_or(Stage::Vertex);
        self.depth = 1;
        self.stage = Some(stage);
        if stage == Stage::Vertex {
            self.line(match self.t {
                Target::Msl => "VOut vout = {};",
                Target::Wgsl => "var vout: VOut;",
                Target::Hlsl => "VOut vout = (VOut)0;",
            });
            if !self.i.instance.fields.is_empty() {
                self.line(match self.t {
                    Target::Wgsl => "let inst = instances[iid];",
                    _ => "Instance inst = instances[iid];",
                });
            }
            for (k, leaf) in self.i.instance.fields.iter().enumerate() {
                if !self.forwarded.contains(&leaf.member) {
                    continue;
                }
                if let Ty::Matrix(n) = leaf.ty {
                    for c in 0..n {
                        let column = self.instance_column(k, n, c);
                        self.line(&format!("vout.fi{k}_{c} = {column};"));
                    }
                } else {
                    let value = self.leaf('i', k, leaf.ty);
                    self.line(&format!("vout.fi{k} = {value};"));
                }
            }
        }
        for (local, builtin) in f.builtins.iter().enumerate() {
            let source = match builtin {
                Builtin::VertexId => "vid",
                Builtin::InstanceId => "iid",
                Builtin::FragCoord => "vin.clip_position",
            };
            self.declare(f, local as u32, Some(source.to_owned()));
        }
        self.block(&f.body, f);
        self.depth = 0;
        self.out.push_str("}\n\n");
        self.stage = None;
    }

    fn block(&mut self, block: &Block, f: &Function) {
        for stmt in &block.0 {
            self.stmt(stmt, f);
        }
    }

    fn nested(&mut self, block: &Block, f: &Function) {
        self.depth += 1;
        self.block(block, f);
        self.depth -= 1;
    }

    fn stmt(&mut self, stmt: &Stmt, f: &Function) {
        match stmt {
            Stmt::Let(local, value) => {
                let value = self.expr(value);
                self.declare(f, *local, Some(value));
            }
            Stmt::Declare(local) => self.declare(f, *local, None),
            Stmt::Assign(place, value) => {
                let target = self.place(place, f);
                let value = self.expr(value);
                self.line(&format!("{target} = {value};"));
            }
            Stmt::If(cond, then, otherwise) => {
                let cond = self.expr(cond);
                self.line(&format!("if ({cond}) {{"));
                self.nested(then, f);
                if otherwise.0.is_empty() {
                    self.line("}");
                } else {
                    self.line("} else {");
                    self.nested(otherwise, f);
                    self.line("}");
                }
            }
            Stmt::For(l) => self.for_loop(l, f),
            Stmt::Break => self.line("break;"),
            Stmt::Continue => self.line("continue;"),
            Stmt::Return(value) => self.ret(value.as_ref(), f),
            Stmt::Discard => {
                let (discard, zero) = match self.t {
                    Target::Msl => ("discard_fragment();", "return float4(0.0f);"),
                    Target::Wgsl => ("discard;", "return vec4<f32>(0.0f);"),
                    Target::Hlsl => ("discard;", "return (float4)0;"),
                };
                self.line(discard);
                self.line(zero);
            }
        }
    }

    fn ret(&mut self, value: Option<&Expr>, f: &Function) {
        match (f.stage, value) {
            (Some(Stage::Vertex), Some(value)) => {
                let value = self.expr(value);
                self.line(&format!("vout.clip_position = ({value}).clip_position;"));
                self.line("return vout;");
            }
            (_, Some(value)) => {
                let value = self.expr(value);
                self.line(&format!("return {value};"));
            }
            (_, None) => self.line("return;"),
        }
    }

    fn for_loop(&mut self, l: &Loop, f: &Function) {
        let local = &f.locals[l.var as usize];
        let ty = self.ty(local.ty);
        let start = self.expr(&l.start);
        let (v, g, max) = (l.var, self.guards, l.max);
        self.guards += 1;
        let wgsl = self.t == Target::Wgsl;
        if !l.guarded {
            // A literal range: `max` iterations, the counter rebuilt from the
            // trip count so `continue` needs no update clause.
            self.line(&if wgsl {
                format!("for (var g{g}: u32 = 0u; g{g} < {max}u; g{g}++) {{")
            } else {
                format!("for (uint g{g} = 0u; g{g} < {max}u; ++g{g}) {{")
            });
            self.depth += 1;
            let counter = self.convert(local.ty, &format!("g{g}"));
            self.declare(f, v, Some(format!("{start} + {counter}")));
            self.depth -= 1;
            self.nested(&l.body, f);
            self.line("}");
            return;
        }
        let end = self.expr(&l.end);
        let cmp = if l.inclusive { "<=" } else { "<" };
        if wgsl {
            self.line("{");
            self.depth += 1;
            self.line(&format!("var l{v}: {ty} = {start};"));
            self.line(&format!("var g{g}: u32 = 0u;"));
            self.line("loop {");
            self.depth += 1;
            self.line(&format!(
                "if (!(l{v} {cmp} {end} && g{g} < {max}u)) {{ break; }}"
            ));
            self.block(&l.body, f);
            self.line("continuing {");
            self.line(&format!("    l{v}++;"));
            self.line(&format!("    g{g}++;"));
            self.line("}");
            self.depth -= 1;
            self.line("}");
            self.depth -= 1;
            self.line("}");
        } else {
            self.line(&format!(
                "for ({ty} l{v} = {start}, g{g} = 0; l{v} {cmp} {end} && uint(g{g}) < {max}u; ++l{v}, ++g{g}) {{"
            ));
            self.nested(&l.body, f);
            self.line("}");
        }
    }

    fn place(&self, place: &Place, f: &Function) -> String {
        let (mut text, mut ty) = match place.root {
            Root::Local(l) => (format!("l{l}"), f.locals[l as usize].ty),
            Root::Varying(v) => (format!("vout.v{v}"), self.p.varyings[v as usize].ty),
        };
        for &step in &place.path {
            match ty {
                Ty::Vector(s, _) => {
                    text = format!("{text}.{}", LANES[step as usize]);
                    ty = Ty::Scalar(s);
                }
                Ty::Color => {
                    text = format!("{text}.{}", LANES[step as usize]);
                    ty = Ty::F32;
                }
                Ty::Matrix(n) => {
                    text = format!("{text}[{step}]");
                    ty = Ty::Vector(Scalar::F32, n);
                }
                Ty::VertexOutput => {
                    text = format!("{text}.clip_position");
                    ty = Ty::VEC4;
                }
                Ty::Record(r) => {
                    text = format!("{text}.f{step}");
                    ty = self.p.records[r as usize].fields[step as usize].ty;
                }
                _ => {}
            }
        }
        text
    }

    /// The scalar leaves `first..first + count` of instance leaf `k` as one
    /// value of `ty`, on a target that stores instances as scalars.
    fn instance_lanes(&self, k: usize, first: u8, count: u8, ty: Ty) -> String {
        let lanes: Vec<String> = (first..first + count)
            .map(|j| format!("inst.i{k}_{j}"))
            .collect();
        if count == 1 {
            lanes.join("")
        } else {
            format!("{}({})", self.ty(ty), lanes.join(", "))
        }
    }

    /// Column `c` of instance matrix leaf `k`, read in the vertex entry.
    fn instance_column(&self, k: usize, n: u8, c: u8) -> String {
        let column = Ty::Vector(Scalar::F32, n);
        match self.t {
            Target::Msl => format!("float{n}(inst.i{k}_{c})"),
            _ => self.instance_lanes(k, c * n, n, column),
        }
    }

    /// The value of leaf `k` of the uniform (`'u'`) or instance block, of
    /// type `ty`, as the current stage reads it.
    fn leaf(&self, block: char, k: usize, ty: Ty) -> String {
        let matrix = |columns: Vec<String>| format!("{}({})", self.ty(ty), columns.join(", "));
        if block == 'u' {
            return match (self.t, ty) {
                (Target::Msl, Ty::Matrix(_)) | (Target::Wgsl, _) => format!("u.u{k}"),
                (Target::Msl, _) => format!("{}(u.u{k})", self.ty(ty)),
                (Target::Hlsl, Ty::Matrix(n)) => {
                    matrix((0..n).map(|c| format!("u{k}_{c}")).collect())
                }
                (Target::Hlsl, _) => format!("u{k}"),
            };
        }
        match (self.stage, ty) {
            (Some(Stage::Fragment), Ty::Matrix(n)) => {
                matrix((0..n).map(|c| format!("vin.fi{k}_{c}")).collect())
            }
            (Some(Stage::Fragment), _) => format!("vin.fi{k}"),
            (_, Ty::Matrix(n)) => matrix((0..n).map(|c| self.instance_column(k, n, c)).collect()),
            (_, ty) => match self.t {
                Target::Msl => format!("{}(inst.i{k})", self.ty(ty)),
                _ => self.instance_lanes(k, 0, leaf_lanes(ty), ty),
            },
        }
    }

    /// Interface member `member` of a block at record path `path`, rebuilt
    /// from its leaves.
    fn member(&self, block: char, member: u32, ty: Ty, path: &mut Vec<u32>) -> String {
        if let Ty::Record(r) = ty {
            let fields = &self.p.records[r as usize].fields;
            let mut parts = Vec::with_capacity(fields.len());
            for (f, field) in fields.iter().enumerate() {
                path.push(f as u32);
                parts.push(self.member(block, member, field.ty, path));
                path.pop();
            }
            return self.record(ty, &parts);
        }
        let fields = if block == 'u' {
            &self.i.uniforms.fields
        } else {
            &self.i.instance.fields
        };
        let k = fields
            .iter()
            .position(|f| f.member == member && *f.path == **path)
            .unwrap_or(0);
        self.leaf(block, k, ty)
    }

    /// A member chain into a uniform or instance member, read leaf-wise.
    fn global_member(&self, e: &Expr) -> Option<String> {
        let mut path = Vec::new();
        let mut at = e;
        while let ExprKind::Member(base, field) = &at.kind {
            if !matches!(base.ty, Ty::Record(_)) {
                return None;
            }
            path.push(*field);
            at = base;
        }
        path.reverse();
        let (block, member) = match at.kind {
            ExprKind::Uniform(i) => ('u', i),
            ExprKind::Instance(i) => ('i', i),
            _ => return None,
        };
        Some(self.member(block, member, e.ty, &mut path))
    }

    fn expr(&self, e: &Expr) -> String {
        if let Some(text) = self.global_member(e) {
            return text;
        }
        match &e.kind {
            ExprKind::Bool(b) => self.lane(Lane::Bool(*b)),
            ExprKind::I32(v) => self.lane(Lane::I32(*v)),
            ExprKind::U32(v) => self.lane(Lane::U32(*v)),
            ExprKind::F32(v) => self.float(*v),
            ExprKind::Local(l) => format!("l{l}"),
            ExprKind::Uniform(_) | ExprKind::Instance(_) => String::new(),
            ExprKind::Varying(v) => match self.stage {
                Some(Stage::Fragment) => format!("vin.v{v}"),
                _ => format!("vout.v{v}"),
            },
            ExprKind::Texture(t) => format!("t{t}"),
            ExprKind::Sampler(s) => format!("s{s}"),
            ExprKind::Unary(op, a) => {
                let symbol = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                    UnaryOp::BitNot => "~",
                };
                format!("({symbol}{})", self.expr(a))
            }
            ExprKind::Binary(op, a, b) => self.binary(*op, a, b, e.ty),
            ExprKind::Construct(parts) => {
                let texts: Vec<String> = parts.iter().map(|p| self.expr(p)).collect();
                match e.ty {
                    Ty::Record(_) | Ty::VertexOutput => self.record(e.ty, &texts),
                    Ty::Vector(..) if matches!(parts.as_slice(), [p] if matches!(p.ty, Ty::Scalar(_))) => {
                        self.splat_text(e.ty, &texts[0])
                    }
                    ty => format!("{}({})", self.ty(ty), texts.join(", ")),
                }
            }
            ExprKind::Swizzle(base, lanes) => {
                let n = e.ty.lanes().map_or(1, |(_, n)| n);
                let letters: String = lanes[..usize::from(n)]
                    .iter()
                    .map(|&l| LANES[usize::from(l)])
                    .collect();
                format!("{}.{letters}", self.expr(base))
            }
            ExprKind::Member(base, field) => match base.ty {
                Ty::VertexOutput => format!("{}.clip_position", self.expr(base)),
                _ => format!("{}.f{field}", self.expr(base)),
            },
            ExprKind::Index(base, index) => {
                let n = match base.ty {
                    Ty::Vector(_, n) | Ty::Matrix(n) => n,
                    _ => 4,
                };
                let at = self.expr(index);
                let at = if index.ty == Ty::U32 {
                    at
                } else {
                    self.convert(Ty::U32, &at)
                };
                format!("{}[min({at}, {}u)]", self.expr(base), n - 1)
            }
            ExprKind::Call(callee, args) => {
                let args: Vec<String> = args.iter().map(|a| self.expr(a)).collect();
                format!("fn{callee}({})", args.join(", "))
            }
            ExprKind::Intrinsic(i, args) => self.intrinsic(*i, args, e.ty),
            ExprKind::Convert(a) => self.convert(e.ty, &self.expr(a)),
        }
    }

    fn binary(&self, op: BinaryOp, a: &Expr, b: &Expr, ty: Ty) -> String {
        let matrix = matches!(a.ty, Ty::Matrix(_)) || matches!(b.ty, Ty::Matrix(_));
        let scalar = |e: &Expr| matches!(e.ty, Ty::Scalar(_));
        match op {
            // HLSL's matrices are transposed: `a * b` is `mul(b, a)`.
            BinaryOp::Mul if self.t == Target::Hlsl && matrix && !scalar(a) && !scalar(b) => {
                format!("mul({}, {})", self.expr(b), self.expr(a))
            }
            BinaryOp::Rem if ty.is_float() && self.t != Target::Wgsl => {
                format!("fmod({}, {})", self.splat(a, ty), self.splat(b, ty))
            }
            // The shift amount is taken modulo the lane width on every target.
            BinaryOp::Shl | BinaryOp::Shr => {
                let amount = match (self.t, b.ty.lanes()) {
                    (Target::Wgsl, Some((s, n))) => {
                        let unsigned = Ty::of_lanes(Scalar::U32, n);
                        let text = self.expr(b);
                        let text = if s == Scalar::U32 {
                            text
                        } else {
                            self.convert(unsigned, &text)
                        };
                        let mask = if n == 1 {
                            "31u".to_owned()
                        } else {
                            self.splat_text(unsigned, "31u")
                        };
                        format!("({text} & {mask})")
                    }
                    (_, Some((s, _))) => {
                        let mask = if s == Scalar::U32 { "31u" } else { "31" };
                        format!("({} & {mask})", self.expr(b))
                    }
                    (_, None) => self.expr(b),
                };
                format!("({} {} {amount})", self.expr(a), op.symbol())
            }
            _ => format!("({} {} {})", self.expr(a), op.symbol(), self.expr(b)),
        }
    }

    /// `e` spread to `ty`'s lanes when it is a scalar beside a vector.
    fn splat(&self, e: &Expr, ty: Ty) -> String {
        let text = self.expr(e);
        if e.ty != ty && matches!(e.ty, Ty::Scalar(_)) && matches!(ty, Ty::Vector(..)) {
            self.splat_text(ty, &text)
        } else {
            text
        }
    }

    fn intrinsic(&self, i: Intrinsic, args: &[Expr], ty: Ty) -> String {
        use Intrinsic as I;
        let a = |k: usize| self.expr(&args[k]);
        let s = |k: usize| self.splat(&args[k], ty);
        let call = |name: &str, parts: Vec<String>| format!("{name}({})", parts.join(", "));
        let by = |msl: &'static str, wgsl: &'static str, hlsl: &'static str| match self.t {
            Target::Msl => msl,
            Target::Wgsl => wgsl,
            Target::Hlsl => hlsl,
        };
        match i {
            I::Abs if ty.scalar() == Some(Scalar::U32) => a(0),
            I::Abs => call("abs", vec![a(0)]),
            // HLSL's `sign` gives an integer.
            I::Sign if self.t == Target::Hlsl => self.convert(ty, &call("sign", vec![a(0)])),
            I::Sign => call("sign", vec![a(0)]),
            I::Floor => call("floor", vec![a(0)]),
            I::Ceil => call("ceil", vec![a(0)]),
            I::Round => call(by("rint", "round", "round"), vec![a(0)]),
            I::Trunc => call("trunc", vec![a(0)]),
            I::Fract => call(by("fract", "fract", "frac"), vec![a(0)]),
            I::Sqrt => call("sqrt", vec![a(0)]),
            I::InverseSqrt => call(by("rsqrt", "inverseSqrt", "rsqrt"), vec![a(0)]),
            I::Exp => call("exp", vec![a(0)]),
            I::Exp2 => call("exp2", vec![a(0)]),
            I::Log => call("log", vec![a(0)]),
            I::Log2 => call("log2", vec![a(0)]),
            I::Sin => call("sin", vec![a(0)]),
            I::Cos => call("cos", vec![a(0)]),
            I::Tan => call("tan", vec![a(0)]),
            I::Asin => call("asin", vec![a(0)]),
            I::Acos => call("acos", vec![a(0)]),
            I::Atan => call("atan", vec![a(0)]),
            I::Sinh => call("sinh", vec![a(0)]),
            I::Cosh => call("cosh", vec![a(0)]),
            I::Tanh => call("tanh", vec![a(0)]),
            I::Radians => format!("({} * {})", a(0), self.float(0.017_453_292)),
            I::Degrees => format!("({} * {})", a(0), self.float(57.295_78)),
            I::Atan2 => call("atan2", vec![a(0), a(1)]),
            I::Pow => call("pow", vec![a(0), a(1)]),
            I::Step => call("step", vec![s(0), a(1)]),
            I::Min => call("min", vec![a(0), a(1)]),
            I::Max => call("max", vec![a(0), a(1)]),
            I::Clamp => call("clamp", vec![a(0), s(1), s(2)]),
            I::Mix => call(by("mix", "mix", "lerp"), vec![a(0), a(1), s(2)]),
            I::Smoothstep => call("smoothstep", vec![s(0), s(1), a(2)]),
            I::Length => call("length", vec![a(0)]),
            I::Distance => call("distance", vec![a(0), a(1)]),
            I::Dot => call("dot", vec![a(0), a(1)]),
            I::Normalize => call("normalize", vec![a(0)]),
            I::Cross => call("cross", vec![a(0), a(1)]),
            I::Transpose => call("transpose", vec![a(0)]),
            I::Dpdx => call(by("dfdx", "dpdx", "ddx"), vec![a(0)]),
            I::Dpdy => call(by("dfdy", "dpdy", "ddy"), vec![a(0)]),
            I::Fwidth => call("fwidth", vec![a(0)]),
            I::Sample | I::SampleLevel => {
                let level = i == I::SampleLevel;
                let sampled = match self.t {
                    Target::Msl if level => {
                        format!("{}.sample({}, {}, level({}))", a(0), a(1), a(2), a(3))
                    }
                    Target::Msl => format!("{}.sample({}, {})", a(0), a(1), a(2)),
                    Target::Wgsl if level => {
                        call("textureSampleLevel", vec![a(0), a(1), a(2), a(3)])
                    }
                    Target::Wgsl => call("textureSample", vec![a(0), a(1), a(2)]),
                    Target::Hlsl if level => {
                        format!("{}.SampleLevel({}, {}, {})", a(0), a(1), a(2), a(3))
                    }
                    Target::Hlsl => format!("{}.Sample({}, {})", a(0), a(1), a(2)),
                };
                match args[0].ty {
                    Ty::Texture(Texel::F32) => format!("{sampled}.x"),
                    _ => sampled,
                }
            }
            I::ToVec4 => a(0),
            I::QuadVertex => call("viso_quad_vertex", vec![a(0)]),
            I::ToClip => call("viso_to_clip", vec![a(0), a(1)]),
            I::RoundedRectSdf => call("viso_rounded_rect_sdf", vec![a(0), a(1), a(2)]),
        }
    }
}

/// The entry name of `stage`.
fn entry_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Vertex => VERTEX_ENTRY,
        Stage::Fragment => super::layout::FRAGMENT_ENTRY,
    }
}

/// Writes `text` to `out`.
fn put(out: &mut String, text: std::fmt::Arguments<'_>) {
    let _ = out.write_fmt(text);
}
