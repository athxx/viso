//! One printer, three dialects: a checked body tree → MSL, WGSL, or HLSL text.
//!
//! MSL printing is the identity (the subset is spelled as MSL), which is what
//! the round-trip test pins. WGSL and HLSL printing lower the few constructs
//! whose spelling or semantics differ:
//!
//! | construct            | WGSL                                   | HLSL                          |
//! |----------------------|----------------------------------------|-------------------------------|
//! | declaration          | `let` / `var` from mutation analysis   | uninitialized → `(T)0`        |
//! | `c ? a : b`          | `select(b, a, c)`                      | unchanged                     |
//! | `v.rgb = e`          | `v = vec4<f32>(e, v.a)`                | unchanged                     |
//! | scalar broadcast     | explicit `vecN<T>(s)` where required   | explicit `((floatN)(s))`      |
//! | mutated parameter    | shadowed by a `var` copy               | unchanged                     |
//! | `tex.sample(s, uv)`  | `textureSampleLevel(tex, s, uv, 0.0)`  | `tex.SampleLevel(s, uv, 0.0)` |
//! | `tex.get_width()`    | `textureDimensions(tex).x`             | `viso_width(tex)`             |
//! | attribute fetch      | the entry's `viso_attrs` parameter     | the entry's `viso_attrs`      |
//!
//! Textures carry a single mip level, so an implicit-LOD sample is level 0;
//! spelling it explicitly keeps sampling legal under non-uniform control flow
//! (the blur's variable-length tap loop) on every target.
//!
//! Identifiers that collide with a target keyword or an emitted builtin are
//! renamed into the `_`-suffixed space the checker keeps free.

use std::borrow::Cow;
use std::fmt::Write as _;

use crate::ir::body::ast::{
    AssignOp, BinOp, Else, Expr, ExprKind, Function, Global, Item, Local, Res, Scalar, Stmt,
    StmtKind, StructName, Ty, TypeName, UnOp,
};
use crate::ir::body::check::{Builtin, swizzle_lanes};

/// A target shading language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    /// Metal Shading Language.
    Msl,
    /// WebGPU Shading Language.
    Wgsl,
    /// HLSL shader model 5.1.
    Hlsl,
}

/// The name of the vertex entry's attribute-struct parameter in WGSL and HLSL.
pub const ATTRS_PARAM: &str = "viso_attrs";

/// `M_PI_F` for targets without the constant.
const PI: &str = "3.14159265358979";

const WGSL_COLLISIONS: &[&str] = &[
    // keywords
    "alias",
    "break",
    "case",
    "const",
    "const_assert",
    "continue",
    "continuing",
    "default",
    "diagnostic",
    "discard",
    "else",
    "enable",
    "false",
    "fn",
    "for",
    "if",
    "let",
    "loop",
    "override",
    "requires",
    "return",
    "struct",
    "switch",
    "true",
    "var",
    "while",
    // reserved words
    "NULL",
    "Self",
    "abstract",
    "active",
    "alignas",
    "alignof",
    "as",
    "asm",
    "asm_fragment",
    "async",
    "attribute",
    "auto",
    "await",
    "become",
    "binding_array",
    "cast",
    "catch",
    "class",
    "co_await",
    "co_return",
    "co_yield",
    "coherent",
    "column_major",
    "common",
    "compile",
    "compile_fragment",
    "concept",
    "const_cast",
    "consteval",
    "constexpr",
    "constinit",
    "crate",
    "debugger",
    "decltype",
    "delete",
    "demote",
    "demote_to_helper",
    "do",
    "dynamic_cast",
    "enum",
    "explicit",
    "export",
    "extends",
    "extern",
    "external",
    "fallthrough",
    "filter",
    "final",
    "finally",
    "friend",
    "from",
    "fxgroup",
    "get",
    "goto",
    "groupshared",
    "highp",
    "impl",
    "implements",
    "import",
    "inline",
    "instanceof",
    "interface",
    "layout",
    "lowp",
    "macro",
    "macro_rules",
    "match",
    "mediump",
    "meta",
    "mod",
    "module",
    "move",
    "mut",
    "mutable",
    "namespace",
    "new",
    "nil",
    "noexcept",
    "noinline",
    "nointerpolation",
    "non_coherent",
    "noncoherent",
    "noperspective",
    "null",
    "nullptr",
    "of",
    "operator",
    "package",
    "packoffset",
    "partition",
    "pass",
    "patch",
    "pixelfragment",
    "precise",
    "precision",
    "premerge",
    "priv",
    "protected",
    "pub",
    "public",
    "readonly",
    "ref",
    "regardless",
    "register",
    "reinterpret_cast",
    "require",
    "resource",
    "restrict",
    "self",
    "set",
    "shared",
    "sizeof",
    "smooth",
    "snorm",
    "static",
    "static_assert",
    "static_cast",
    "std",
    "subroutine",
    "super",
    "target",
    "template",
    "this",
    "thread_local",
    "throw",
    "trait",
    "try",
    "type",
    "typedef",
    "typeid",
    "typename",
    "typeof",
    "union",
    "unless",
    "unorm",
    "unsafe",
    "unsized",
    "use",
    "using",
    "varying",
    "virtual",
    "volatile",
    "wgsl",
    "where",
    "with",
    "writeonly",
    "yield",
    // predeclared types and the builtins the printer emits
    "f32",
    "f16",
    "i32",
    "u32",
    "vec2",
    "vec3",
    "vec4",
    "array",
    "atomic",
    "ptr",
    "sampler",
    "texture_2d",
    "select",
    "textureSampleLevel",
    "textureDimensions",
    "dpdx",
    "dpdy",
    "inverseSqrt",
    // module-scope names the WGSL codegen declares
    "vid",
    "iid",
    "vin",
    "tex",
    "dst_tex",
    "samp",
    "instances",
    "verts",
];

const HLSL_COLLISIONS: &[&str] = &[
    // keywords and reserved words
    "AppendStructuredBuffer",
    "BlendState",
    "Buffer",
    "ByteAddressBuffer",
    "CompileShader",
    "ComputeShader",
    "ConstantBuffer",
    "DepthStencilState",
    "DepthStencilView",
    "DomainShader",
    "GeometryShader",
    "Hullshader",
    "InputPatch",
    "LineStream",
    "OutputPatch",
    "PixelShader",
    "PointStream",
    "RWBuffer",
    "RWByteAddressBuffer",
    "RWStructuredBuffer",
    "RWTexture1D",
    "RWTexture2D",
    "RWTexture3D",
    "RasterizerState",
    "RenderTargetView",
    "SamplerComparisonState",
    "SamplerState",
    "StructuredBuffer",
    "Texture1D",
    "Texture2D",
    "Texture2DMS",
    "Texture3D",
    "TextureCube",
    "TriangleStream",
    "VertexShader",
    "asm",
    "asm_fragment",
    "auto",
    "cbuffer",
    "centroid",
    "char",
    "class",
    "column_major",
    "compile",
    "compile_fragment",
    "const_cast",
    "continue",
    "delete",
    "discard",
    "do",
    "double",
    "dword",
    "dynamic_cast",
    "enum",
    "explicit",
    "export",
    "extern",
    "friend",
    "fxgroup",
    "globallycoherent",
    "goto",
    "groupshared",
    "half",
    "in",
    "inout",
    "interface",
    "line",
    "lineadj",
    "linear",
    "long",
    "matrix",
    "min16float",
    "min16int",
    "min16uint",
    "mutable",
    "namespace",
    "new",
    "nointerpolation",
    "noperspective",
    "operator",
    "out",
    "packoffset",
    "pass",
    "pixelfragment",
    "point",
    "precise",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "row_major",
    "sample",
    "sampler",
    "shared",
    "short",
    "signed",
    "sizeof",
    "snorm",
    "stateblock",
    "stateblock_state",
    "static_cast",
    "string",
    "tbuffer",
    "technique",
    "technique10",
    "technique11",
    "template",
    "texture",
    "this",
    "throw",
    "triangle",
    "triangleadj",
    "try",
    "typedef",
    "typename",
    "uniform",
    "union",
    "unorm",
    "unsigned",
    "using",
    "vector",
    "vertexfragment",
    "virtual",
    "void",
    "volatile",
    "while",
    // the intrinsics the printer emits under a different name than the source
    "lerp",
    "frac",
    "ddx",
    "ddy",
    // global names the HLSL codegen declares
    "vid",
    "iid",
    "vin",
    "tex",
    "dst_tex",
    "samp",
    "instances",
    "verts",
];

/// Prints checked body trees in one dialect.
pub struct Printer {
    lang: Lang,
    out: String,
    depth: usize,
}

impl Printer {
    /// A printer for `lang`, starting at indent depth 0.
    pub fn new(lang: Lang) -> Printer {
        Printer {
            lang,
            out: String::new(),
            depth: 0,
        }
    }

    /// Start at `depth` levels of four-space indentation.
    pub fn with_depth(mut self, depth: usize) -> Printer {
        self.depth = depth;
        self
    }

    /// The printed text.
    pub fn finish(self) -> String {
        self.out
    }

    /// Print an entry point's body statements.
    pub fn entry_body(&mut self, func: &Function) {
        self.block(&func.body, &func.locals);
    }

    /// Print a helper fragment's items.
    pub fn items(&mut self, items: &[Item]) {
        for (i, item) in items.iter().enumerate() {
            match item {
                Item::Comment { text, blank_before } => {
                    if *blank_before && i > 0 {
                        self.out.push('\n');
                    }
                    self.comment(text);
                }
                Item::Function { func, blank_before } => {
                    if *blank_before && i > 0 {
                        self.out.push('\n');
                    }
                    self.function(func);
                }
            }
        }
    }

    /// `name`, renamed if it collides with a target keyword or emitted builtin.
    pub fn ident<'n>(&self, name: &'n str) -> Cow<'n, str> {
        let collisions = match self.lang {
            Lang::Msl => return Cow::Borrowed(name),
            Lang::Wgsl => WGSL_COLLISIONS,
            Lang::Hlsl => HLSL_COLLISIONS,
        };
        if collisions.contains(&name) {
            Cow::Owned(format!("{name}_"))
        } else {
            Cow::Borrowed(name)
        }
    }

    /// A source type in this dialect.
    pub fn type_name(&self, t: TypeName) -> String {
        match t {
            TypeName::Scalar(s) => self.scalar(s).to_string(),
            TypeName::Vector(s, n) => match self.lang {
                Lang::Wgsl => format!("vec{n}<{}>", self.scalar(s)),
                Lang::Msl | Lang::Hlsl => format!("{}{n}", self.scalar(s)),
            },
            TypeName::Struct(s) => struct_name(s).to_string(),
        }
    }

    fn scalar(&self, s: Scalar) -> &'static str {
        match (self.lang, s) {
            (_, Scalar::Bool) => "bool",
            (Lang::Wgsl, Scalar::F32) => "f32",
            (Lang::Wgsl, Scalar::I32) => "i32",
            (Lang::Wgsl, Scalar::U32) => "u32",
            (_, Scalar::F32) => "float",
            (_, Scalar::I32) => "int",
            (_, Scalar::U32) => "uint",
        }
    }

    fn ty_text(&self, t: Ty) -> String {
        match t {
            Ty::Scalar(s) => self.type_name(TypeName::Scalar(s)),
            Ty::Vector(s, n) => self.type_name(TypeName::Vector(s, n)),
            Ty::Struct(s) => self.type_name(TypeName::Struct(s)),
            other => unreachable!("no source spelling for {other:?}"),
        }
    }

    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
    }

    fn line(&mut self, text: &str) {
        self.indent();
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn comment(&mut self, text: &str) {
        if text.is_empty() {
            self.line("//");
        } else {
            self.line(&format!("// {text}"));
        }
    }

    fn function(&mut self, func: &Function) {
        let l = &func.locals;
        let params = &l[..func.params];
        let name = self.ident(&func.name).into_owned();
        let header = match self.lang {
            Lang::Msl | Lang::Hlsl => {
                let ps: Vec<String> = params
                    .iter()
                    .map(|p| format!("{} {}", self.type_name(p.ty), self.ident(&p.name)))
                    .collect();
                let prefix = if self.lang == Lang::Msl {
                    "static inline "
                } else {
                    ""
                };
                format!(
                    "{prefix}{} {name}({}) {{",
                    self.type_name(func.ret),
                    ps.join(", ")
                )
            }
            Lang::Wgsl => {
                let ps: Vec<String> = params
                    .iter()
                    .map(|p| {
                        let n = self.ident(&p.name);
                        let n = if p.mutated {
                            format!("{n}_")
                        } else {
                            n.into_owned()
                        };
                        format!("{n}: {}", self.type_name(p.ty))
                    })
                    .collect();
                format!(
                    "fn {name}({}) -> {} {{",
                    ps.join(", "),
                    self.type_name(func.ret)
                )
            }
        };
        self.line(&header);
        self.depth += 1;
        if self.lang == Lang::Wgsl {
            for p in params.iter().filter(|p| p.mutated) {
                let n = self.ident(&p.name);
                self.line(&format!("var {n}: {} = {n}_;", self.type_name(p.ty)));
            }
        }
        self.block(&func.body, l);
        self.depth -= 1;
        self.line("}");
    }

    fn block(&mut self, stmts: &[Stmt], l: &[Local]) {
        for (i, s) in stmts.iter().enumerate() {
            if s.blank_before && i > 0 {
                self.out.push('\n');
            }
            self.stmt(s, l);
        }
    }

    fn nested(&mut self, stmts: &[Stmt], l: &[Local]) {
        self.depth += 1;
        self.block(stmts, l);
        self.depth -= 1;
    }

    fn stmt(&mut self, s: &Stmt, l: &[Local]) {
        match &s.kind {
            StmtKind::Comment(text) => self.comment(text),
            StmtKind::Decl { .. } | StmtKind::Assign { .. } | StmtKind::Step { .. } => {
                let text = self.simple(s, l);
                self.line(&format!("{text};"));
            }
            StmtKind::Return(v) => match v {
                Some(v) => {
                    let text = self.expr(v, l);
                    self.line(&format!("return {text};"));
                }
                None => self.line("return;"),
            },
            StmtKind::Break => self.line("break;"),
            StmtKind::If { .. } => {
                self.indent();
                self.if_chain(s, l);
            }
            StmtKind::Switch { selector, cases } => {
                let sel = self.expr(selector, l);
                self.line(&format!("switch ({sel}) {{"));
                self.depth += 1;
                for case in cases {
                    let label = match &case.label {
                        Some(e) => format!("case {}:", self.expr(e, l)),
                        None => "default:".to_string(),
                    };
                    if self.lang == Lang::Wgsl {
                        // WGSL arms never fall through; the arm's closing
                        // `break` is implied by its braces.
                        let body = match case.body.last() {
                            Some(last) if matches!(last.kind, StmtKind::Break) => {
                                &case.body[..case.body.len() - 1]
                            }
                            _ => &case.body[..],
                        };
                        self.line(&format!("{label} {{"));
                        self.nested(body, l);
                        self.line("}");
                    } else {
                        self.line(&label);
                        self.nested(&case.body, l);
                    }
                }
                self.depth -= 1;
                self.line("}");
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                let header = format!(
                    "for ({}; {}; {}) {{",
                    self.simple(init, l),
                    self.expr(cond, l),
                    self.simple(step, l)
                );
                self.line(&header);
                self.nested(body, l);
                self.line("}");
            }
        }
    }

    /// An `if` whose first line's indent has already been written.
    fn if_chain(&mut self, s: &Stmt, l: &[Local]) {
        let StmtKind::If { cond, then, els } = &s.kind else {
            unreachable!("if_chain is only called on `if`");
        };
        let c = self.expr(cond, l);
        let _ = writeln!(self.out, "if ({c}) {{");
        self.nested(then, l);
        match els {
            None => self.line("}"),
            Some(Else::Block(b)) => {
                self.line("} else {");
                self.nested(b, l);
                self.line("}");
            }
            Some(Else::If(next)) => {
                self.indent();
                self.out.push_str("} else ");
                self.if_chain(next, l);
            }
        }
    }

    /// A declaration, assignment or increment without its `;`.
    fn simple(&self, s: &Stmt, l: &[Local]) -> String {
        match &s.kind {
            StmtKind::Decl {
                ty, local, init, ..
            } => {
                let local = &l[*local as usize];
                let name = self.local_name(local);
                let init = init.as_ref().map(|e| self.expr(e, l));
                match (self.lang, init) {
                    (Lang::Msl, Some(i)) => format!("{} {name} = {i}", self.type_name(*ty)),
                    (Lang::Msl, None) => format!("{} {name}", self.type_name(*ty)),
                    (Lang::Hlsl, Some(i)) => format!("{} {name} = {i}", self.type_name(*ty)),
                    (Lang::Hlsl, None) => {
                        let t = self.type_name(*ty);
                        format!("{t} {name} = ({t})0")
                    }
                    (Lang::Wgsl, Some(i)) => {
                        let kw = if local.mutated { "var" } else { "let" };
                        format!("{kw} {name}: {} = {i}", self.type_name(*ty))
                    }
                    (Lang::Wgsl, None) => format!("var {name}: {}", self.type_name(*ty)),
                }
            }
            StmtKind::Assign { target, op, value } => {
                if self.lang == Lang::Wgsl {
                    return self.wgsl_assign(target, *op, value, l);
                }
                format!(
                    "{} {} {}",
                    self.expr(target, l),
                    op.text(),
                    self.expr(value, l)
                )
            }
            StmtKind::Step {
                target,
                decrement,
                prefix,
            } => {
                let t = self.expr(target, l);
                let op = if *decrement { "--" } else { "++" };
                if *prefix && self.lang != Lang::Wgsl {
                    format!("{op}{t}")
                } else {
                    format!("{t}{op}")
                }
            }
            other => unreachable!("not a simple statement: {other:?}"),
        }
    }

    /// WGSL cannot store through a multi-lane swizzle (or through any swizzle
    /// of a swizzle): rebuild the whole vector instead, lane by lane, until
    /// the target is a real reference.
    fn wgsl_assign(&self, target: &Expr, op: AssignOp, value: &Expr, l: &[Local]) -> String {
        let is_store_swizzle = |t: &Expr| match &t.kind {
            ExprKind::Field { base, name } if base.ty.is_vector() => {
                name.len() > 1
                    || matches!(&base.kind, ExprKind::Field { base: b, .. } if b.ty.is_vector())
            }
            _ => false,
        };
        if !is_store_swizzle(target) {
            let mut v = self.expr(value, l);
            if let AssignOp::Compound(bop) = op
                && (bop.is_bitwise())
            {
                let want = if matches!(bop, BinOp::Shl | BinOp::Shr) {
                    target.ty.lanes().map(|n| vector_of(Scalar::U32, n))
                } else {
                    Some(target.ty)
                };
                if let Some(want) = want {
                    v = self.splat(v, value.ty, want);
                }
            }
            return format!("{} {} {v}", self.expr(target, l), op.text());
        }
        let mut value = match op {
            AssignOp::Set => value.clone(),
            AssignOp::Compound(bop) => Expr {
                kind: ExprKind::Binary(bop, Box::new(target.clone()), Box::new(value.clone())),
                span: value.span,
                ty: target.ty,
            },
        };
        let mut target = target.clone();
        while is_store_swizzle(&target) {
            let ExprKind::Field { base, name } = target.kind else {
                unreachable!("is_store_swizzle matched a field");
            };
            let Ty::Vector(s, n) = base.ty else {
                unreachable!("is_store_swizzle matched a vector base");
            };
            let lanes = swizzle_lanes(&name).expect("the checker validated the swizzle");
            let (first, last) = (lanes[0], lanes[lanes.len() - 1]);
            let lane = |i: u8| Expr {
                kind: ExprKind::Field {
                    base: base.clone(),
                    name: ["x", "y", "z", "w"][i as usize].to_string(),
                },
                span: base.span,
                ty: Ty::Scalar(s),
            };
            let mut args: Vec<Expr> = (0..first).map(lane).collect();
            args.push(value);
            args.extend((last + 1..n).map(lane));
            value = Expr {
                kind: ExprKind::Construct {
                    ty: TypeName::Vector(s, n),
                    args,
                },
                span: base.span,
                ty: base.ty,
            };
            target = *base;
        }
        format!("{} = {}", self.expr(&target, l), self.expr(&value, l))
    }

    fn local_name(&self, local: &Local) -> String {
        match (self.lang, local.name.as_str()) {
            (Lang::Msl, n) => n.to_string(),
            (_, n) => self.ident(n).into_owned(),
        }
    }

    fn global(&self, g: Global) -> &'static str {
        match (self.lang, g) {
            (_, Global::Vid) => "vid",
            (_, Global::Iid) => "iid",
            (_, Global::Instances) => "instances",
            (_, Global::Verts) => "verts",
            (_, Global::Tex) => "tex",
            (_, Global::DstTex) => "dst_tex",
            (_, Global::Samp) => "samp",
            (Lang::Msl, Global::Uniforms) => "u",
            (Lang::Msl, Global::In) => "in",
            (Lang::Msl, Global::Pi) => "M_PI_F",
            (_, Global::Uniforms) => "uniforms",
            (_, Global::In) => "vin",
            (_, Global::Pi) => PI,
        }
    }

    /// `text` of type `from`, widened to the vector `to` when the target needs
    /// the broadcast spelled out.
    fn splat(&self, text: String, from: Ty, to: Ty) -> String {
        if self.lang == Lang::Msl || !to.is_vector() || from.lanes() == to.lanes() {
            return text;
        }
        match self.lang {
            Lang::Wgsl => format!("{}({text})", self.ty_text(to)),
            _ => format!("(({})({text}))", self.ty_text(to)),
        }
    }

    fn args(&self, args: &[Expr], l: &[Local]) -> String {
        args.iter()
            .map(|a| self.expr(a, l))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn expr(&self, e: &Expr, l: &[Local]) -> String {
        match &e.kind {
            ExprKind::Float(t) => t.clone(),
            ExprKind::Uint(t) => t.clone(),
            ExprKind::Int(t) => {
                // HLSL types an unsuffixed literal as `int`; one past `i32::MAX`
                // is only meaningful as `uint`.
                let big =
                    self.lang == Lang::Hlsl && parse_int(t).is_some_and(|v| v > i32::MAX as u64);
                if big { format!("{t}u") } else { t.clone() }
            }
            ExprKind::Var { name, res } => match res {
                Res::Local(i) => self.local_name(&l[*i as usize]),
                Res::Global(g) => self.global(*g).to_string(),
                Res::Unresolved => unreachable!("unchecked name `{name}`"),
            },
            ExprKind::Paren(inner) => format!("({})", self.expr(inner, l)),
            ExprKind::Unary(op, inner) => format!("{}{}", op_text(*op), self.expr(inner, l)),
            ExprKind::Binary(op, a, b) => self.binary(*op, a, b, e.ty, l),
            ExprKind::Ternary(c, a, b) => {
                let (c, a, b) = (self.expr(c, l), self.expr(a, l), self.expr(b, l));
                match self.lang {
                    Lang::Wgsl => format!("select({b}, {a}, {c})"),
                    _ => format!("{c} ? {a} : {b}"),
                }
            }
            ExprKind::Field { base, name } => {
                let b = self.expr(base, l);
                if matches!(base.ty, Ty::Struct(_)) {
                    format!("{b}.{}", self.ident(name))
                } else {
                    format!("{b}.{name}")
                }
            }
            ExprKind::Method { base, method, args } => {
                let b = self.expr(base, l);
                let a = self.args(args, l);
                match (self.lang, method.as_str()) {
                    (Lang::Msl, _) => format!("{b}.{method}({a})"),
                    (Lang::Wgsl, "sample") => format!("textureSampleLevel({b}, {a}, 0.0)"),
                    (Lang::Wgsl, "get_width") => format!("textureDimensions({b}).x"),
                    (Lang::Wgsl, "get_height") => format!("textureDimensions({b}).y"),
                    (Lang::Hlsl, "sample") => format!("{b}.SampleLevel({a}, 0.0)"),
                    (Lang::Hlsl, "get_width") => format!("viso_width({b})"),
                    (Lang::Hlsl, "get_height") => format!("viso_height({b})"),
                    (_, m) => unreachable!("the checker admits no method `{m}`"),
                }
            }
            ExprKind::Index { base, index } => match self.lang {
                Lang::Msl => format!("{}[{}]", self.expr(base, l), self.expr(index, l)),
                _ => ATTRS_PARAM.to_string(),
            },
            ExprKind::Construct { ty, args } => {
                let t = self.type_name(*ty);
                match (self.lang, args.as_slice()) {
                    (Lang::Hlsl, [one]) => format!("(({t})({}))", self.expr(one, l)),
                    _ => format!("{t}({})", self.args(args, l)),
                }
            }
            ExprKind::Call { name, args } => match Builtin::from_name(name) {
                Some(b) => self.builtin(b, name, args, e.ty, l),
                None => format!("{}({})", self.ident(name), self.args(args, l)),
            },
        }
    }

    fn binary(&self, op: BinOp, a: &Expr, b: &Expr, ty: Ty, l: &[Local]) -> String {
        let mut at = self.expr(a, l);
        let mut bt = self.expr(b, l);
        if self.lang == Lang::Wgsl {
            if let ExprKind::Binary(child, ..) = a.kind
                && wgsl_needs_parens(op, child, true)
            {
                at = format!("({at})");
            }
            if let ExprKind::Binary(child, ..) = b.kind
                && wgsl_needs_parens(op, child, false)
            {
                bt = format!("({bt})");
            }
            // Only arithmetic operators broadcast a scalar operand in WGSL.
            if op.is_bitwise() && ty.is_vector() {
                let n = ty.lanes().expect("a vector has lanes");
                at = self.splat(at, a.ty, ty);
                let rhs = if matches!(op, BinOp::Shl | BinOp::Shr) {
                    vector_of(Scalar::U32, n)
                } else {
                    ty
                };
                bt = self.splat(bt, b.ty, rhs);
            }
        }
        format!("{at} {} {bt}", op.text())
    }

    fn builtin(&self, b: Builtin, name: &str, args: &[Expr], ty: Ty, l: &[Local]) -> String {
        let spelled = match (self.lang, b) {
            (Lang::Msl, _) => name,
            (Lang::Wgsl, Builtin::Fabs) => "abs",
            (Lang::Wgsl, Builtin::Dfdx) => "dpdx",
            (Lang::Wgsl, Builtin::Dfdy) => "dpdy",
            (Lang::Wgsl, Builtin::Rsqrt) => "inverseSqrt",
            (Lang::Hlsl, Builtin::Fabs) => "abs",
            (Lang::Hlsl, Builtin::Dfdx) => "ddx",
            (Lang::Hlsl, Builtin::Dfdy) => "ddy",
            (Lang::Hlsl, Builtin::Fract) => "frac",
            (Lang::Hlsl, Builtin::Mix) => "lerp",
            _ => name,
        };
        let widen = self.lang != Lang::Msl
            && (b.broadcasts().is_some() || (self.lang == Lang::Hlsl && b == Builtin::Mix));
        let parts: Vec<String> = args
            .iter()
            .map(|a| {
                let t = self.expr(a, l);
                if widen { self.splat(t, a.ty, ty) } else { t }
            })
            .collect();
        let call = format!("{spelled}({})", parts.join(", "));
        // HLSL's `sign` returns `int`; the subset's returns its argument type.
        if self.lang == Lang::Hlsl && b == Builtin::Sign && ty.scalar() == Some(Scalar::F32) {
            format!("(({}){call})", self.ty_text(ty))
        } else {
            call
        }
    }
}

fn struct_name(s: StructName) -> &'static str {
    match s {
        StructName::VOut => "VOut",
        StructName::InstanceIn => "InstanceIn",
        StructName::VertexIn => "VertexIn",
        StructName::Uniforms => "Uniforms",
    }
}

fn op_text(op: UnOp) -> &'static str {
    op.text()
}

fn vector_of(s: Scalar, n: u8) -> Ty {
    if n == 1 {
        Ty::Scalar(s)
    } else {
        Ty::Vector(s, n)
    }
}

fn parse_int(text: &str) -> Option<u64> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// Whether a binary `child` operand of `parent` needs explicit parentheses in
/// WGSL, whose grammar forbids several C precedence chains: shift operands and
/// mixed bitwise operators must be unary expressions, `&&`/`||` do not mix,
/// and relational operands cannot themselves be relational or bitwise.
fn wgsl_needs_parens(parent: BinOp, child: BinOp, is_left: bool) -> bool {
    let bitwise = |o: BinOp| matches!(o, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor);
    let logical = |o: BinOp| matches!(o, BinOp::And | BinOp::Or);
    let shift = |o: BinOp| matches!(o, BinOp::Shl | BinOp::Shr);
    match parent {
        p if shift(p) => true,
        p if bitwise(p) => !(is_left && child == p),
        p if logical(p) => bitwise(child) || (logical(child) && !(is_left && child == p)),
        p if p.is_comparison() => child.is_comparison() || bitwise(child) || logical(child),
        _ => bitwise(child) || logical(child) || child.is_comparison() || shift(child),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::body::check::{Stage, check_entry};
    use crate::ir::body::parse::parse_entry;
    use crate::ir::module::{ShaderIr, image_ir, quad_ir};

    fn print(lang: Lang, ir: &ShaderIr, src: &str) -> String {
        let mut f = parse_entry(src, Stage::Fragment).unwrap();
        check_entry(ir, &[], Stage::Fragment, &mut f).unwrap();
        let mut p = Printer::new(lang);
        p.entry_body(&f);
        p.finish()
    }

    #[test]
    fn wgsl_lowers_declarations_ternaries_and_swizzle_stores() {
        let src =
            "float4 c = in.color;\nfloat k = in.radius > 0.0 ? 1.0 : 0.0;\nc.rgb *= k;\nreturn c;";
        assert_eq!(
            print(Lang::Wgsl, &quad_ir(), src),
            "var c: vec4<f32> = vin.color;\n\
             let k: f32 = select(0.0, 1.0, vin.radius > 0.0);\n\
             c = vec4<f32>(c.rgb * k, c.w);\n\
             return c;\n"
        );
        assert_eq!(
            print(Lang::Hlsl, &quad_ir(), src),
            "float4 c = vin.color;\n\
             float k = vin.radius > 0.0 ? 1.0 : 0.0;\n\
             c.rgb *= k;\n\
             return c;\n"
        );
    }

    #[test]
    fn broadcasts_and_builtin_spellings() {
        let src = "float4 c = in.color;\nfloat4 d = clamp(c, 0.0, 1.0);\nfloat3 m = mix(c.rgb, d.rgb, 0.5);\nreturn float4(fract(m) + sign(m), 1.0);";
        let wgsl = print(Lang::Wgsl, &quad_ir(), src);
        assert!(
            wgsl.contains("clamp(c, vec4<f32>(0.0), vec4<f32>(1.0))"),
            "{wgsl}"
        );
        assert!(wgsl.contains("mix(c.rgb, d.rgb, 0.5)"), "{wgsl}");
        let hlsl = print(Lang::Hlsl, &quad_ir(), src);
        assert!(
            hlsl.contains("lerp(c.rgb, d.rgb, ((float3)(0.5)))"),
            "{hlsl}"
        );
        assert!(hlsl.contains("frac(m) + ((float3)sign(m))"), "{hlsl}");
    }

    #[test]
    fn wgsl_parenthesizes_shift_and_bitwise_chains() {
        let src = "uint h = 3u;\nh = h ^ h >> 15;\nh = h & 255u | 1u;\nreturn float4(float(h));";
        let wgsl = print(Lang::Wgsl, &quad_ir(), src);
        assert!(wgsl.contains("h = h ^ (h >> 15);"), "{wgsl}");
        assert!(wgsl.contains("h = (h & 255u) | 1u;"), "{wgsl}");
    }

    #[test]
    fn textures_and_collisions() {
        let src = "float4 sample = tex.sample(samp, in.uv);\nfloat w = float(tex.get_width());\nreturn sample * w;";
        let wgsl = print(Lang::Wgsl, &image_ir(), src);
        assert!(
            wgsl.contains("let sample: vec4<f32> = textureSampleLevel(tex, samp, vin.uv, 0.0);"),
            "{wgsl}"
        );
        assert!(wgsl.contains("f32(textureDimensions(tex).x)"), "{wgsl}");
        let hlsl = print(Lang::Hlsl, &image_ir(), src);
        assert!(
            hlsl.contains("float4 sample_ = tex.SampleLevel(samp, vin.uv, 0.0);"),
            "{hlsl}"
        );
        assert!(hlsl.contains("((float)(viso_width(tex)))"), "{hlsl}");
    }

    #[test]
    fn wgsl_switch_arms_are_braced() {
        let src = "float x = 0.0;\nswitch (int(in.radius)) {\n    case 0: x = 1.0; break;\n    default: break;\n}\nreturn float4(x);";
        let wgsl = print(Lang::Wgsl, &quad_ir(), src);
        assert!(
            wgsl.contains("switch (i32(vin.radius)) {\n    case 0: {\n        x = 1.0;\n    }\n    default: {\n    }\n}"),
            "{wgsl}"
        );
    }
}
