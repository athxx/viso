//! Name resolution, typing, and the subset's stage and control-flow rules.
//!
//! After [`check_helpers`] and [`check_entry`] succeed, every [`Expr::ty`] is
//! filled, every [`ExprKind::Var`] carries its [`Res`], and every
//! [`Local::mutated`] is exact — which is everything a printer needs to choose
//! `let` vs `var`, insert splats, and rewrite swizzle stores.

use crate::ir::body::BodyError;
use crate::ir::body::ast::{
    AssignOp, BinOp, Case, Else, Expr, ExprKind, Function, Global, Item, Local, Res, Scalar, Stmt,
    StmtKind, StructName, Ty, TypeName, UnOp,
};
use crate::ir::body::lex::Span;
use crate::ir::module::{ShaderIr, VertexSource};
use crate::ir::types::{IrType, ScalarType};

/// The entry point a body fragment is the body of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// `vertex_main`, returning `VOut`.
    Vertex,
    /// `fragment_main`, returning `float4`.
    Fragment,
}

impl Stage {
    /// The entry point's return type.
    pub fn ret(self) -> TypeName {
        match self {
            Stage::Vertex => TypeName::Struct(StructName::VOut),
            Stage::Fragment => TypeName::Vector(Scalar::F32, 4),
        }
    }

    /// The entry point's name.
    pub fn entry_name(self) -> &'static str {
        match self {
            Stage::Vertex => "vertex_main",
            Stage::Fragment => "fragment_main",
        }
    }
}

/// A helper's callable signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// Source name.
    pub name: String,
    /// Return type.
    pub ret: TypeName,
    /// Parameter types, in order.
    pub params: Vec<TypeName>,
}

/// The helper signatures of a checked helper fragment, in source order.
pub fn signatures(items: &[Item]) -> Vec<Signature> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Function { func, .. } => Some(Signature {
                name: func.name.clone(),
                ret: func.ret,
                params: func.locals[..func.params].iter().map(|l| l.ty).collect(),
            }),
            Item::Comment { .. } => None,
        })
        .collect()
}

/// A builtin function of the subset (spelled as in MSL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Abs,
    Fabs,
    Floor,
    Ceil,
    Fract,
    Sign,
    Exp,
    Sqrt,
    Rsqrt,
    Sin,
    Cos,
    Dfdx,
    Dfdy,
    Saturate,
    Length,
    Dot,
    Normalize,
    Min,
    Max,
    Clamp,
    Mix,
    Step,
    Smoothstep,
    Pow,
    Atan2,
}

impl Builtin {
    /// The builtin a call name spells.
    pub fn from_name(name: &str) -> Option<Builtin> {
        Some(match name {
            "abs" => Builtin::Abs,
            "fabs" => Builtin::Fabs,
            "floor" => Builtin::Floor,
            "ceil" => Builtin::Ceil,
            "fract" => Builtin::Fract,
            "sign" => Builtin::Sign,
            "exp" => Builtin::Exp,
            "sqrt" => Builtin::Sqrt,
            "rsqrt" => Builtin::Rsqrt,
            "sin" => Builtin::Sin,
            "cos" => Builtin::Cos,
            "dfdx" => Builtin::Dfdx,
            "dfdy" => Builtin::Dfdy,
            "saturate" => Builtin::Saturate,
            "length" => Builtin::Length,
            "dot" => Builtin::Dot,
            "normalize" => Builtin::Normalize,
            "min" => Builtin::Min,
            "max" => Builtin::Max,
            "clamp" => Builtin::Clamp,
            "mix" => Builtin::Mix,
            "step" => Builtin::Step,
            "smoothstep" => Builtin::Smoothstep,
            "pow" => Builtin::Pow,
            "atan2" => Builtin::Atan2,
            _ => return None,
        })
    }

    /// Builtins whose scalar arguments broadcast against the vector result
    /// (`clamp(v, 0.0, 1.0)`), and the index of the argument that must carry
    /// the result's lane count, if any one must.
    pub fn broadcasts(self) -> Option<Option<usize>> {
        match self {
            Builtin::Min | Builtin::Max => Some(None),
            Builtin::Clamp => Some(Some(0)),
            Builtin::Step => Some(Some(1)),
            Builtin::Smoothstep => Some(Some(2)),
            _ => None,
        }
    }
}

/// Names no body may declare: the generated entry-point plumbing of every
/// target, the `__` space WGSL and HLSL reserve, and the `_`-suffixed space the
/// printers rename target-keyword collisions into.
fn is_reserved(name: &str) -> bool {
    matches!(
        name,
        "vin" | "vout" | "uniforms" | "vertex_main" | "fragment_main"
    ) || name.starts_with("viso_")
        || name.starts_with("__")
        || name.ends_with('_')
}

fn ir_ty(t: IrType) -> Ty {
    let s = |s: ScalarType| match s {
        ScalarType::F32 => Scalar::F32,
        ScalarType::U32 => Scalar::U32,
    };
    match t {
        IrType::Scalar(sc) => Ty::Scalar(s(sc)),
        IrType::Vector { scalar, lanes } => Ty::Vector(s(scalar), lanes),
    }
}

fn show(t: Ty) -> String {
    let s = |s: Scalar| match s {
        Scalar::Bool => "bool",
        Scalar::F32 => "float",
        Scalar::I32 => "int",
        Scalar::U32 => "uint",
    };
    match t {
        Ty::Unknown => "?".into(),
        Ty::Scalar(sc) => s(sc).into(),
        Ty::Vector(sc, n) => format!("{}{n}", s(sc)),
        Ty::AbsInt => "integer literal".into(),
        Ty::Struct(st) => format!("{st:?}"),
        Ty::Texture => "texture2d".into(),
        Ty::Sampler => "sampler".into(),
        Ty::AttrBuffer => "attribute buffer".into(),
        Ty::Void => "void".into(),
    }
}

/// Whether a value of type `from` may initialize or be passed as `to`.
pub fn coerces(from: Ty, to: Ty) -> bool {
    from == to
        || (from == Ty::AbsInt && matches!(to, Ty::Scalar(Scalar::F32 | Scalar::I32 | Scalar::U32)))
}

/// The common element scalar of two numeric operands: `Some(None)` when both
/// are integer literals, `None` when they disagree.
fn unify_scalar(a: Ty, b: Ty) -> Option<Option<Scalar>> {
    let sa = if a == Ty::AbsInt {
        None
    } else {
        Some(a.scalar()?)
    };
    let sb = if b == Ty::AbsInt {
        None
    } else {
        Some(b.scalar()?)
    };
    match (sa, sb) {
        (Some(x), Some(y)) if x == y => Some(Some(x)),
        (Some(x), None) | (None, Some(x)) if x != Scalar::Bool => Some(Some(x)),
        (None, None) => Some(None),
        _ => None,
    }
}

fn merge_lanes(a: u8, b: u8) -> Option<u8> {
    match (a, b) {
        _ if a == b => Some(a),
        (1, n) | (n, 1) => Some(n),
        _ => None,
    }
}

fn make_ty(s: Option<Scalar>, lanes: u8) -> Ty {
    match s {
        None => Ty::AbsInt,
        Some(s) if lanes == 1 => Ty::Scalar(s),
        Some(s) => Ty::Vector(s, lanes),
    }
}

fn is_int(s: Scalar) -> bool {
    matches!(s, Scalar::I32 | Scalar::U32)
}

/// Swizzle lane indices for `name`, if it is one (`xyzw` or `rgba`, not mixed).
pub fn swizzle_lanes(name: &str) -> Option<Vec<u8>> {
    if name.is_empty() || name.len() > 4 {
        return None;
    }
    ["xyzw", "rgba"].iter().find_map(|set| {
        name.chars()
            .map(|c| set.find(c).map(|i| i as u8))
            .collect::<Option<Vec<u8>>>()
    })
}

/// A literal switch label's value.
fn label_value(e: &Expr) -> Option<u64> {
    let text = match &e.kind {
        ExprKind::Int(t) => t.as_str(),
        ExprKind::Uint(t) => t.strip_suffix('u')?,
        _ => return None,
    };
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// Whether the statements return on every path.
pub fn terminates(stmts: &[Stmt]) -> bool {
    let Some(last) = stmts
        .iter()
        .rev()
        .find(|s| !matches!(s.kind, StmtKind::Comment(_)))
    else {
        return false;
    };
    match &last.kind {
        StmtKind::Return(_) => true,
        StmtKind::If {
            then,
            els: Some(els),
            ..
        } => {
            terminates(then)
                && match els {
                    Else::Block(b) => terminates(b),
                    Else::If(s) => terminates(std::slice::from_ref(s)),
                }
        }
        StmtKind::Switch { cases, .. } => cases.iter().all(|c| terminates(&c.body)),
        _ => false,
    }
}

fn last_real(stmts: &[Stmt]) -> Option<usize> {
    stmts
        .iter()
        .rposition(|s| !matches!(s.kind, StmtKind::Comment(_)))
}

/// Check every helper in order; a helper may call only the helpers before it.
pub fn check_helpers(ir: &ShaderIr, items: &mut [Item]) -> Result<(), BodyError> {
    let mut earlier: Vec<Signature> = Vec::new();
    for item in items.iter_mut() {
        let Item::Function { func, .. } = item else {
            continue;
        };
        if is_reserved(&func.name) || Builtin::from_name(&func.name).is_some() {
            return Err(BodyError::new(
                "S0303",
                func.span,
                format!("`{}` is reserved and cannot name a helper", func.name),
            ));
        }
        if earlier.iter().any(|s| s.name == func.name) {
            return Err(BodyError::new(
                "S0302",
                func.span,
                format!("helper `{}` is defined twice", func.name),
            ));
        }
        check_function(ir, &earlier, None, func)?;
        earlier.push(Signature {
            name: func.name.clone(),
            ret: func.ret,
            params: func.locals[..func.params].iter().map(|l| l.ty).collect(),
        });
    }
    Ok(())
}

/// Check an entry-point body against its stage and the helper signatures.
pub fn check_entry(
    ir: &ShaderIr,
    helpers: &[Signature],
    stage: Stage,
    func: &mut Function,
) -> Result<(), BodyError> {
    check_function(ir, helpers, Some(stage), func)
}

fn check_function(
    ir: &ShaderIr,
    callable: &[Signature],
    stage: Option<Stage>,
    func: &mut Function,
) -> Result<(), BodyError> {
    let mut cx = Cx {
        ir,
        stage,
        callable,
        locals: std::mem::take(&mut func.locals),
        scopes: vec![Vec::new()],
        ret: func.ret.ty(),
        fetch: None,
    };
    let result = cx.function(func);
    func.locals = cx.locals;
    result
}

struct Cx<'a> {
    ir: &'a ShaderIr,
    stage: Option<Stage>,
    callable: &'a [Signature],
    locals: Vec<Local>,
    scopes: Vec<Vec<u32>>,
    ret: Ty,
    /// The attribute-fetch declaration's local, once seen.
    fetch: Option<u32>,
}

fn err(code: &'static str, span: Span, msg: impl Into<String>) -> BodyError {
    BodyError::new(code, span, msg)
}

impl Cx<'_> {
    fn function(&mut self, func: &mut Function) -> Result<(), BodyError> {
        for i in 0..func.params {
            self.declare(i as u32, func.span)?;
            if matches!(self.locals[i].ty, TypeName::Struct(_)) {
                return Err(err(
                    "S0341",
                    func.span,
                    "helper parameters must be scalars or vectors",
                ));
            }
        }
        if self.stage == Some(Stage::Vertex) {
            self.check_fetch_shape(&func.body, func.span)?;
        }
        self.block_in_scope(&mut func.body, false)?;
        if !terminates(&func.body) {
            return Err(err(
                "S0340",
                func.span,
                format!("`{}` does not return on every path", func.name),
            ));
        }
        if let Some(l) = self.fetch
            && self.locals[l as usize].mutated
        {
            return Err(err(
                "S0312",
                func.span,
                format!(
                    "the attribute fetch `{}` is read-only",
                    self.locals[l as usize].name
                ),
            ));
        }
        Ok(())
    }

    /// The vertex stage opens with exactly one attribute fetch:
    /// `InstanceIn x = instances[iid];` or `VertexIn x = verts[vid];`.
    fn check_fetch_shape(&self, body: &[Stmt], span: Span) -> Result<(), BodyError> {
        let (sname, buf, index) = match self.ir.vertex_source {
            VertexSource::PerInstance => (StructName::InstanceIn, "instances", "iid"),
            VertexSource::PerVertex => (StructName::VertexIn, "verts", "vid"),
        };
        let first = body
            .iter()
            .find(|s| !matches!(s.kind, StmtKind::Comment(_)));
        let ok = first.is_some_and(|s| match &s.kind {
            StmtKind::Decl {
                ty: TypeName::Struct(t),
                init: Some(init),
                ..
            } if *t == sname => match &init.kind {
                ExprKind::Index { base, index: i } => {
                    matches!(&base.kind, ExprKind::Var { name, .. } if name == buf)
                        && matches!(&i.kind, ExprKind::Var { name, .. } if name == index)
                }
                _ => false,
            },
            _ => false,
        });
        if ok {
            Ok(())
        } else {
            Err(err(
                "S0310",
                first.map_or(span, |s| s.span),
                format!("the vertex body must begin with `{sname:?} <name> = {buf}[{index}];`"),
            ))
        }
    }

    fn visible(&self, name: &str) -> Option<u32> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter().rev())
            .copied()
            .find(|&i| self.locals[i as usize].name == name)
    }

    fn global(&self, name: &str) -> Option<(Global, Ty)> {
        if name == "M_PI_F" {
            return Some((Global::Pi, Ty::Scalar(Scalar::F32)));
        }
        let tc = self.ir.texture_count;
        let per_instance = self.ir.vertex_source == VertexSource::PerInstance;
        match (self.stage?, name) {
            (Stage::Vertex, "vid") => Some((Global::Vid, Ty::Scalar(Scalar::U32))),
            (Stage::Vertex, "iid") if per_instance => Some((Global::Iid, Ty::Scalar(Scalar::U32))),
            (Stage::Vertex, "u") => Some((Global::Uniforms, Ty::Struct(StructName::Uniforms))),
            (Stage::Vertex, "instances") if per_instance => {
                Some((Global::Instances, Ty::AttrBuffer))
            }
            (Stage::Vertex, "verts") if !per_instance => Some((Global::Verts, Ty::AttrBuffer)),
            (Stage::Fragment, "in") => Some((Global::In, Ty::Struct(StructName::VOut))),
            (Stage::Fragment, "tex") if tc >= 1 => Some((Global::Tex, Ty::Texture)),
            (Stage::Fragment, "dst_tex") if tc >= 2 => Some((Global::DstTex, Ty::Texture)),
            (Stage::Fragment, "samp") if tc >= 1 => Some((Global::Samp, Ty::Sampler)),
            _ => None,
        }
    }

    fn declare(&mut self, idx: u32, span: Span) -> Result<(), BodyError> {
        let name = self.locals[idx as usize].name.clone();
        if is_reserved(&name) {
            return Err(err(
                "S0303",
                span,
                format!("`{name}` is reserved and cannot be declared"),
            ));
        }
        if self.visible(&name).is_some()
            || self.global(&name).is_some()
            || self.callable.iter().any(|s| s.name == name)
        {
            return Err(err(
                "S0302",
                span,
                format!("`{name}` is already defined in this scope"),
            ));
        }
        self.scopes
            .last_mut()
            .expect("a scope is always open")
            .push(idx);
        Ok(())
    }

    fn block_in_scope(&mut self, stmts: &mut [Stmt], in_case: bool) -> Result<(), BodyError> {
        self.scopes.push(Vec::new());
        let last = last_real(stmts);
        let mut result = Ok(());
        for (i, s) in stmts.iter_mut().enumerate() {
            if matches!(s.kind, StmtKind::Break) {
                if !(in_case && Some(i) == last) {
                    result = Err(err("S0334", s.span, "`break` may only end a `switch` case"));
                    break;
                }
                continue;
            }
            if let Err(e) = self.stmt(s) {
                result = Err(e);
                break;
            }
        }
        self.scopes.pop();
        result
    }

    fn expect(&self, got: Ty, want: Ty, span: Span, what: &str) -> Result<(), BodyError> {
        if coerces(got, want) {
            Ok(())
        } else {
            Err(err(
                "S0304",
                span,
                format!("{what}: expected `{}`, found `{}`", show(want), show(got)),
            ))
        }
    }

    fn cond(&mut self, e: &mut Expr) -> Result<(), BodyError> {
        let t = self.expr(e)?;
        if t == Ty::Scalar(Scalar::Bool) {
            Ok(())
        } else {
            Err(err(
                "S0330",
                e.span,
                format!("a condition must be `bool`, found `{}`", show(t)),
            ))
        }
    }

    fn stmt(&mut self, s: &mut Stmt) -> Result<(), BodyError> {
        let span = s.span;
        match &mut s.kind {
            StmtKind::Comment(_) | StmtKind::Break => Ok(()),
            StmtKind::Decl {
                ty, local, init, ..
            } => {
                let is_fetch = self.stage == Some(Stage::Vertex)
                    && self.fetch.is_none()
                    && matches!(
                        ty,
                        TypeName::Struct(StructName::InstanceIn | StructName::VertexIn)
                    );
                match ty {
                    TypeName::Struct(StructName::InstanceIn | StructName::VertexIn)
                        if !is_fetch =>
                    {
                        return Err(err(
                            "S0310",
                            span,
                            "attribute structs are only read by the opening fetch",
                        ));
                    }
                    TypeName::Struct(StructName::VOut)
                        if self.stage != Some(Stage::Vertex) || init.is_some() =>
                    {
                        return Err(err(
                            "S0341",
                            span,
                            "`VOut` is declared once, uninitialized, in the vertex body",
                        ));
                    }
                    TypeName::Struct(StructName::Uniforms) => {
                        return Err(err("S0341", span, "`Uniforms` cannot be declared"));
                    }
                    _ => {}
                }
                if let Some(init) = init {
                    let t = if is_fetch {
                        self.fetch_expr(init)?
                    } else {
                        self.expr(init)?
                    };
                    self.expect(t, ty.ty(), init.span, "initializer")?;
                }
                let local = *local;
                self.declare(local, span)?;
                if is_fetch {
                    self.fetch = Some(local);
                }
                Ok(())
            }
            StmtKind::Assign { target, op, value } => {
                let tt = self.lvalue(target)?;
                let vt = self.expr(value)?;
                match op {
                    AssignOp::Set => self.expect(vt, tt, value.span, "assignment"),
                    AssignOp::Compound(bop) => {
                        let rt = self.binary_ty(*bop, tt, vt, span)?;
                        self.expect(rt, tt, span, "compound assignment")
                    }
                }
            }
            StmtKind::Step { target, .. } => {
                let ok = matches!(target.kind, ExprKind::Var { .. });
                let t = self.lvalue(target)?;
                if ok && matches!(t, Ty::Scalar(Scalar::I32 | Scalar::U32)) {
                    Ok(())
                } else {
                    Err(err(
                        "S0322",
                        span,
                        "`++`/`--` apply only to an integer local",
                    ))
                }
            }
            StmtKind::If { cond, then, els } => {
                self.cond(cond)?;
                self.block_in_scope(then, false)?;
                match els {
                    None => Ok(()),
                    Some(Else::Block(b)) => self.block_in_scope(b, false),
                    Some(Else::If(s)) => self.stmt(s),
                }
            }
            StmtKind::Switch { selector, cases } => self.switch(selector, cases, span),
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                let init_ok = matches!(
                    init.kind,
                    StmtKind::Decl { init: Some(_), .. } | StmtKind::Assign { .. }
                );
                let step_ok = matches!(step.kind, StmtKind::Step { .. } | StmtKind::Assign { .. });
                if !init_ok || !step_ok {
                    return Err(err(
                        "S0335",
                        span,
                        "a `for` loop is `for (init; cond; step)` with an initialized declaration or assignment and an increment or assignment",
                    ));
                }
                self.scopes.push(Vec::new());
                let r = (|| {
                    self.stmt(init)?;
                    self.cond(cond)?;
                    self.stmt(step)?;
                    self.block_in_scope(body, false)
                })();
                self.scopes.pop();
                r
            }
            StmtKind::Return(value) => match value {
                Some(v) => {
                    let t = self.expr(v)?;
                    self.expect(t, self.ret, v.span, "return value")
                }
                None => Err(err("S0336", span, "`return` needs a value")),
            },
        }
    }

    fn switch(
        &mut self,
        selector: &mut Expr,
        cases: &mut [Case],
        span: Span,
    ) -> Result<(), BodyError> {
        let mut st = self.expr(selector)?;
        if st == Ty::AbsInt {
            st = Ty::Scalar(Scalar::I32);
            selector.ty = st;
        }
        let Ty::Scalar(sel @ (Scalar::I32 | Scalar::U32)) = st else {
            return Err(err(
                "S0331",
                selector.span,
                format!(
                    "a switch selector must be `int` or `uint`, found `{}`",
                    show(st)
                ),
            ));
        };
        let defaults = cases.iter().filter(|c| c.label.is_none()).count();
        if defaults != 1 {
            return Err(err(
                "S0332",
                span,
                "a switch needs exactly one `default` arm",
            ));
        }
        let mut seen: Vec<u64> = Vec::new();
        for case in cases.iter_mut() {
            if let Some(label) = &mut case.label {
                let v = label_value(label).ok_or_else(|| {
                    err(
                        "S0331",
                        label.span,
                        "a case label must be an integer literal",
                    )
                })?;
                if sel == Scalar::I32 && matches!(label.kind, ExprKind::Uint(_)) {
                    return Err(err(
                        "S0331",
                        label.span,
                        "a `uint` label on an `int` selector",
                    ));
                }
                if seen.contains(&v) {
                    return Err(err("S0331", label.span, "duplicate case label"));
                }
                seen.push(v);
                label.ty = Ty::Scalar(sel);
            }
            let ends = last_real(&case.body).is_some_and(|i| {
                matches!(case.body[i].kind, StmtKind::Break | StmtKind::Return(_))
            });
            if !ends {
                let at = case.label.as_ref().map_or(span, |l| l.span);
                return Err(err(
                    "S0333",
                    at,
                    "every case must end in `break` or `return` (no fallthrough)",
                ));
            }
            self.block_in_scope(&mut case.body, true)?;
        }
        Ok(())
    }

    /// The attribute fetch's `instances[iid]` / `verts[vid]`.
    fn fetch_expr(&mut self, e: &mut Expr) -> Result<Ty, BodyError> {
        let ExprKind::Index { base, index } = &mut e.kind else {
            return self.expr(e);
        };
        self.expr(base)?;
        self.expr(index)?;
        e.ty = Ty::Struct(match self.ir.vertex_source {
            VertexSource::PerInstance => StructName::InstanceIn,
            VertexSource::PerVertex => StructName::VertexIn,
        });
        Ok(e.ty)
    }

    fn lvalue(&mut self, e: &mut Expr) -> Result<Ty, BodyError> {
        let span = e.span;
        let t = match &mut e.kind {
            ExprKind::Var { name, res } => {
                let Some(l) = self.visible(name) else {
                    return Err(if self.global(name).is_some() {
                        err("S0320", span, format!("stage input `{name}` is read-only"))
                    } else {
                        err("S0301", span, format!("unknown name `{name}`"))
                    });
                };
                *res = Res::Local(l);
                self.locals[l as usize].mutated = true;
                self.locals[l as usize].ty.ty()
            }
            ExprKind::Field { base, name } => {
                let bt = self.lvalue(base)?;
                let t = self.member(bt, name, span)?;
                if bt.is_vector()
                    && let Some(lanes) = swizzle_lanes(name)
                    && lanes.len() > 1
                    && lanes.windows(2).any(|w| w[1] != w[0] + 1)
                {
                    return Err(err(
                        "S0321",
                        span,
                        "a multi-lane swizzle store must name consecutive lanes in order",
                    ));
                }
                t
            }
            _ => {
                return Err(err(
                    "S0320",
                    span,
                    "only a local, or a member or swizzle of one, can be assigned",
                ));
            }
        };
        e.ty = t;
        Ok(t)
    }

    fn member(&self, base: Ty, name: &str, span: Span) -> Result<Ty, BodyError> {
        let fields = |sn: StructName| -> Option<Ty> {
            match sn {
                StructName::VOut => self
                    .ir
                    .varyings
                    .iter()
                    .find(|v| v.name == name)
                    .map(|v| ir_ty(v.ty)),
                StructName::InstanceIn | StructName::VertexIn => self
                    .ir
                    .attributes
                    .iter()
                    .find(|f| f.name == name)
                    .map(|f| ir_ty(f.ty)),
                StructName::Uniforms => self
                    .ir
                    .uniforms
                    .iter()
                    .find(|f| f.name == name)
                    .map(|f| ir_ty(f.ty)),
            }
        };
        let found = match base {
            Ty::Struct(sn) => fields(sn),
            Ty::Vector(s, n) => swizzle_lanes(name)
                .filter(|l| l.iter().all(|&i| i < n))
                .map(|l| make_ty(Some(s), l.len() as u8)),
            _ => None,
        };
        found.ok_or_else(|| {
            err(
                "S0306",
                span,
                format!("`{}` has no member `{name}`", show(base)),
            )
        })
    }

    fn expr(&mut self, e: &mut Expr) -> Result<Ty, BodyError> {
        let span = e.span;
        let t = match &mut e.kind {
            ExprKind::Float(_) => Ty::Scalar(Scalar::F32),
            ExprKind::Int(_) => Ty::AbsInt,
            ExprKind::Uint(_) => Ty::Scalar(Scalar::U32),
            ExprKind::Var { name, res } => {
                if let Some(l) = self.visible(name) {
                    *res = Res::Local(l);
                    self.locals[l as usize].ty.ty()
                } else if let Some((g, t)) = self.global(name) {
                    *res = Res::Global(g);
                    t
                } else {
                    return Err(err("S0301", span, format!("unknown name `{name}`")));
                }
            }
            ExprKind::Paren(inner) => self.expr(inner)?,
            ExprKind::Unary(op, inner) => {
                let t = self.expr(inner)?;
                let ok = match op {
                    UnOp::Neg => {
                        t == Ty::AbsInt || matches!(t.scalar(), Some(Scalar::F32 | Scalar::I32))
                    }
                    UnOp::Not => t == Ty::Scalar(Scalar::Bool),
                    UnOp::BitNot => t == Ty::AbsInt || t.scalar().is_some_and(is_int),
                };
                if !ok {
                    return Err(err(
                        "S0309",
                        span,
                        format!("`{}` does not apply to `{}`", op.text(), show(t)),
                    ));
                }
                t
            }
            ExprKind::Binary(op, l, r) => {
                let lt = self.expr(l)?;
                let rt = self.expr(r)?;
                self.binary_ty(*op, lt, rt, span)?
            }
            ExprKind::Ternary(c, a, b) => {
                self.cond(c)?;
                let at = self.expr(a)?;
                let bt = self.expr(b)?;
                if coerces(at, bt) {
                    bt
                } else if coerces(bt, at) {
                    at
                } else {
                    return Err(err(
                        "S0309",
                        span,
                        format!("ternary branches differ: `{}` vs `{}`", show(at), show(bt)),
                    ));
                }
            }
            ExprKind::Field { base, name } => {
                let bt = self.expr(base)?;
                self.member(bt, name, span)?
            }
            ExprKind::Method { base, method, args } => {
                let bt = self.expr(base)?;
                let mut ats = Vec::with_capacity(args.len());
                for a in args.iter_mut() {
                    ats.push(self.expr(a)?);
                }
                match (bt, method.as_str(), ats.as_slice()) {
                    (Ty::Texture, "sample", [Ty::Sampler, Ty::Vector(Scalar::F32, 2)]) => {
                        Ty::Vector(Scalar::F32, 4)
                    }
                    (Ty::Texture, "get_width" | "get_height", []) => Ty::Scalar(Scalar::U32),
                    _ => {
                        return Err(err(
                            "S0307",
                            span,
                            format!(
                                "no method `{method}` on `{}` taking these arguments",
                                show(bt)
                            ),
                        ));
                    }
                }
            }
            ExprKind::Index { .. } => {
                return Err(err(
                    "S0311",
                    span,
                    "indexing is only the vertex body's opening attribute fetch",
                ));
            }
            ExprKind::Construct { ty, args } => {
                let ty = *ty;
                let mut ats = Vec::with_capacity(args.len());
                for a in args.iter_mut() {
                    ats.push(self.expr(a)?);
                }
                self.construct(ty, &ats, span)?
            }
            ExprKind::Call { name, args } => {
                if self.visible(name).is_some() {
                    return Err(err(
                        "S0305",
                        span,
                        format!("`{name}` is a local here, not a function"),
                    ));
                }
                let mut ats = Vec::with_capacity(args.len());
                for a in args.iter_mut() {
                    ats.push(self.expr(a)?);
                }
                if let Some(b) = Builtin::from_name(name) {
                    self.builtin(b, &ats).ok_or_else(|| {
                        err(
                            "S0305",
                            span,
                            format!(
                                "`{name}` does not accept ({})",
                                ats.iter().map(|&t| show(t)).collect::<Vec<_>>().join(", ")
                            ),
                        )
                    })?
                } else if let Some(sig) = self.callable.iter().find(|s| s.name == *name) {
                    if sig.params.len() != args.len() {
                        return Err(err(
                            "S0305",
                            span,
                            format!(
                                "`{name}` takes {} arguments, found {}",
                                sig.params.len(),
                                args.len()
                            ),
                        ));
                    }
                    for (a, p) in args.iter().zip(&sig.params) {
                        self.expect(a.ty, p.ty(), a.span, "argument")?;
                    }
                    sig.ret.ty()
                } else {
                    return Err(err(
                        "S0305",
                        span,
                        format!("unknown function `{name}` (helpers must be defined before use)"),
                    ));
                }
            }
        };
        e.ty = t;
        Ok(t)
    }

    fn binary_ty(&self, op: BinOp, l: Ty, r: Ty, span: Span) -> Result<Ty, BodyError> {
        let bad = || {
            err(
                "S0309",
                span,
                format!(
                    "`{}` does not apply to `{}` and `{}`",
                    op.text(),
                    show(l),
                    show(r)
                ),
            )
        };
        match op {
            BinOp::And | BinOp::Or => {
                let b = Ty::Scalar(Scalar::Bool);
                if l == b && r == b { Ok(b) } else { Err(bad()) }
            }
            _ if op.is_comparison() => {
                let s = unify_scalar(l, r).ok_or_else(bad)?;
                let scalars = l.lanes() == Some(1) && r.lanes() == Some(1);
                let bool_ok = s != Some(Scalar::Bool) || matches!(op, BinOp::Eq | BinOp::Ne);
                if scalars && bool_ok {
                    Ok(Ty::Scalar(Scalar::Bool))
                } else {
                    Err(bad())
                }
            }
            BinOp::Shl | BinOp::Shr => {
                let ls = if l == Ty::AbsInt { None } else { l.scalar() };
                let int_l = l == Ty::AbsInt || ls.is_some_and(is_int);
                let u32_r = r == Ty::AbsInt || r.scalar() == Some(Scalar::U32);
                if int_l
                    && u32_r
                    && merge_lanes(l.lanes().unwrap_or(0), r.lanes().unwrap_or(0)) == l.lanes()
                {
                    Ok(l)
                } else {
                    Err(bad())
                }
            }
            _ => {
                let s = unify_scalar(l, r).ok_or_else(bad)?;
                let lanes = merge_lanes(l.lanes().ok_or_else(bad)?, r.lanes().ok_or_else(bad)?)
                    .ok_or_else(bad)?;
                let ok = match s {
                    Some(Scalar::Bool) => false,
                    Some(s) if op.is_bitwise() || op == BinOp::Rem => is_int(s),
                    _ => true,
                };
                if ok {
                    Ok(make_ty(s, lanes))
                } else {
                    Err(bad())
                }
            }
        }
    }

    fn construct(&self, ty: TypeName, args: &[Ty], span: Span) -> Result<Ty, BodyError> {
        let numeric = |t: Ty| {
            t == Ty::AbsInt || matches!(t, Ty::Scalar(_) | Ty::Vector(..)) && t.scalar().is_some()
        };
        let ok = match ty {
            TypeName::Scalar(Scalar::Bool) | TypeName::Struct(_) => false,
            TypeName::Scalar(_) => {
                args.len() == 1 && (args[0] == Ty::AbsInt || matches!(args[0], Ty::Scalar(_)))
            }
            TypeName::Vector(s, n) => match args {
                [a] if a.lanes() == Some(1) => coerces(*a, Ty::Scalar(s)),
                [Ty::Vector(_, m)] => *m == n,
                _ => {
                    args.iter().all(|&a| {
                        numeric(a) && (a == Ty::AbsInt || a.scalar() == Some(s))
                            || (a == Ty::AbsInt && s != Scalar::Bool)
                    }) && args
                        .iter()
                        .map(|a| a.lanes().unwrap_or(0) as u32)
                        .sum::<u32>()
                        == n as u32
                        && args.iter().all(|&a| a != Ty::AbsInt || s != Scalar::Bool)
                }
            },
        };
        if ok {
            Ok(ty.ty())
        } else {
            Err(err(
                "S0308",
                span,
                format!(
                    "cannot construct `{}` from ({})",
                    show(ty.ty()),
                    args.iter().map(|&t| show(t)).collect::<Vec<_>>().join(", ")
                ),
            ))
        }
    }

    fn builtin(&self, b: Builtin, args: &[Ty]) -> Option<Ty> {
        let float = |t: Ty| -> Option<Ty> {
            match t {
                Ty::AbsInt => Some(Ty::Scalar(Scalar::F32)),
                _ if t.scalar() == Some(Scalar::F32) => Some(t),
                _ => None,
            }
        };
        match (b, args) {
            (
                Builtin::Fabs
                | Builtin::Floor
                | Builtin::Ceil
                | Builtin::Fract
                | Builtin::Exp
                | Builtin::Sqrt
                | Builtin::Rsqrt
                | Builtin::Sin
                | Builtin::Cos
                | Builtin::Dfdx
                | Builtin::Dfdy
                | Builtin::Saturate,
                [a],
            ) => float(*a),
            (Builtin::Abs | Builtin::Sign, [a]) => match a.scalar() {
                Some(Scalar::F32 | Scalar::I32) => Some(*a),
                _ if *a == Ty::AbsInt => Some(Ty::AbsInt),
                _ => None,
            },
            (Builtin::Length, [a]) => float(*a).map(|_| Ty::Scalar(Scalar::F32)),
            (Builtin::Normalize, [a @ Ty::Vector(Scalar::F32, _)]) => Some(*a),
            (Builtin::Dot, [a @ Ty::Vector(Scalar::F32, _), b]) if a == b => {
                Some(Ty::Scalar(Scalar::F32))
            }
            (Builtin::Pow | Builtin::Atan2, [a, b]) => {
                let a = float(*a)?;
                (float(*b)? == a).then_some(a)
            }
            (Builtin::Mix, [a, c, t]) => {
                let a = float(*a)?;
                let t = float(*t)?;
                (float(*c)? == a && (t == a || t == Ty::Scalar(Scalar::F32))).then_some(a)
            }
            _ => {
                let lead = b.broadcasts()?;
                let arity = match b {
                    Builtin::Min | Builtin::Max | Builtin::Step => 2,
                    _ => 3,
                };
                if args.len() != arity {
                    return None;
                }
                let mut s: Option<Scalar> = None;
                let mut lanes = 1u8;
                for &a in args {
                    let sa = if a == Ty::AbsInt {
                        None
                    } else {
                        Some(a.scalar()?)
                    };
                    s = match (s, sa) {
                        (Some(x), Some(y)) if x != y => return None,
                        (x, y) => x.or(y),
                    };
                    lanes = merge_lanes(lanes, a.lanes()?)?;
                }
                if s == Some(Scalar::Bool) {
                    return None;
                }
                if matches!(b, Builtin::Step | Builtin::Smoothstep) {
                    if !s.is_none_or(|s| s == Scalar::F32) {
                        return None;
                    }
                    s = Some(Scalar::F32);
                }
                if let Some(i) = lead
                    && args[i].lanes() != Some(lanes)
                {
                    return None;
                }
                Some(make_ty(s, lanes))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::body::parse::parse_entry;
    use crate::ir::module::{image_ir, quad_ir};

    fn frag(ir: &ShaderIr, src: &str) -> Result<Function, BodyError> {
        let mut f = parse_entry(src, Stage::Fragment)?;
        check_entry(ir, &[], Stage::Fragment, &mut f)?;
        Ok(f)
    }

    fn code(ir: &ShaderIr, src: &str) -> &'static str {
        frag(ir, src).unwrap_err().code
    }

    #[test]
    fn types_and_mutation_are_filled() {
        let f = frag(
            &quad_ir(),
            "float4 c = in.color;\nc.rgb = c.rgb * 0.5;\nfloat k = clamp(1, 0.0, 1.0);\nreturn clamp(c, 0.0, k);",
        )
        .unwrap();
        assert!(f.locals[0].mutated);
        assert!(!f.locals[1].mutated);
        let StmtKind::Return(Some(r)) = &f.body[3].kind else {
            panic!()
        };
        assert_eq!(r.ty, Ty::Vector(Scalar::F32, 4));
    }

    #[test]
    fn integer_literals_adapt_but_concrete_types_do_not_mix() {
        let ir = quad_ir();
        assert!(frag(&ir, "float x = 1 + in.radius;\nreturn float4(x);").is_ok());
        assert!(frag(&ir, "uint h = 3u;\nh ^= h >> 15;\nreturn float4(float(h));").is_ok());
        assert_eq!(
            code(
                &ir,
                "int i = 1;\nfloat x = in.radius * i;\nreturn float4(x);"
            ),
            "S0309"
        );
        assert_eq!(code(&ir, "int x = 1.0;\nreturn float4(0.0);"), "S0304");
    }

    #[test]
    fn stage_and_scope_rules() {
        let ir = quad_ir();
        assert_eq!(code(&ir, "return tex.sample(samp, in.local);"), "S0301");
        assert!(frag(&image_ir(), "return tex.sample(samp, in.uv);").is_ok());
        assert_eq!(
            code(&ir, "float x = 1.0;\nfloat x = 2.0;\nreturn float4(x);"),
            "S0302"
        );
        assert_eq!(
            code(&ir, "float vout = 1.0;\nreturn float4(vout);"),
            "S0303"
        );
        assert_eq!(code(&ir, "in.radius = 1.0;\nreturn float4(0.0);"), "S0320");
        assert_eq!(
            code(&ir, "float4 c = in.color;\nc.rb = float2(0.0);\nreturn c;"),
            "S0321"
        );
        assert_eq!(code(&ir, "float x = in.nope;\nreturn float4(x);"), "S0306");
        assert_eq!(
            code(
                &ir,
                "if (in.radius) {\n    return float4(0.0);\n}\nreturn float4(1.0);"
            ),
            "S0330"
        );
    }

    #[test]
    fn control_flow_rules() {
        let ir = quad_ir();
        assert_eq!(code(&ir, "float x = 0.0;"), "S0340");
        assert_eq!(
            code(
                &ir,
                "switch (1) {\n    case 0: break;\n}\nreturn float4(0.0);"
            ),
            "S0332"
        );
        assert_eq!(
            code(
                &ir,
                "float x;\nswitch (1) {\n    case 0: x = 1.0;\n    default: break;\n}\nreturn float4(x);"
            ),
            "S0333"
        );
        assert_eq!(code(&ir, "break;\nreturn float4(0.0);"), "S0334");
        assert!(
            frag(
                &ir,
                "switch (2) {\n    case 0: return float4(0.0);\n    default: return float4(1.0);\n}"
            )
            .is_ok()
        );
    }

    #[test]
    fn the_vertex_body_opens_with_the_attribute_fetch() {
        let ir = quad_ir();
        let check = |src: &str| {
            let mut f = parse_entry(src, Stage::Vertex).unwrap();
            check_entry(&ir, &[], Stage::Vertex, &mut f).map(|_| f)
        };
        assert_eq!(check("VOut out;\nreturn out;").unwrap_err().code, "S0310");
        assert_eq!(
            check("InstanceIn inst = instances[iid];\ninst.radius = 1.0;\nVOut out;\nreturn out;")
                .unwrap_err()
                .code,
            "S0312"
        );
        let f = check(
            "InstanceIn inst = instances[iid];\nVOut out;\nout.radius = inst.radius;\nreturn out;",
        )
        .unwrap();
        assert!(f.locals[1].mutated);
    }
}
