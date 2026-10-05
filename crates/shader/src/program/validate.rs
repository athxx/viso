//! Rechecks a [`Program`] whatever built it: every expression's type follows
//! from its parts, every place is writable where it is written, every loop
//! is bounded, records and calls are acyclic, and each stage uses only what
//! it may.

use super::{
    Binding, Block, Builtin, Expr, ExprKind, Function, Intrinsic, Place, Program, Root, Scalar,
    Span, Stage, StageUse, Stmt, Ty,
};

/// A rule a program breaks, with the span of what breaks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramError {
    /// The stable diagnostic code.
    pub code: &'static str,
    pub message: String,
    pub span: Span,
}

impl ProgramError {
    pub fn new(code: &'static str, span: Span, message: impl Into<String>) -> ProgramError {
        ProgramError {
            code,
            message: message.into(),
            span,
        }
    }
}

/// An expression whose type does not follow from its parts.
const TYPE: &str = "E2103";
/// A construct outside the shader subset.
const SUBSET: &str = "E8105";
/// A stage or entry rule.
const STAGE: &str = "E8106";
/// A type with no layout where the interface needs one.
const ABI: &str = "E8104";

impl Program {
    /// Every rule the program breaks; `Ok` when it breaks none.
    pub fn validate(&self) -> Result<(), Vec<ProgramError>> {
        let mut v = Validator {
            program: self,
            errors: Vec::new(),
        };
        v.interface();
        for (index, function) in self.functions.iter().enumerate() {
            v.function(function, Some(index as u32));
        }
        for entry in self.entries() {
            v.function(entry, None);
        }
        if v.errors.is_empty() {
            Ok(())
        } else {
            Err(v.errors)
        }
    }
}

struct Validator<'p> {
    program: &'p Program,
    errors: Vec<ProgramError>,
}

/// Where in a function the walk is.
struct Cx<'f> {
    function: &'f Function,
    /// The function's own index, which it and the functions after it may not
    /// be called from it; `None` for an entry.
    index: Option<u32>,
    loops: u32,
    span: Span,
}

impl Validator<'_> {
    fn error(&mut self, code: &'static str, span: Span, message: impl Into<String>) {
        self.errors.push(ProgramError::new(code, span, message));
    }

    fn interface(&mut self) {
        let p = self.program;
        for (index, record) in p.records.iter().enumerate() {
            for field in &record.fields {
                let ok = match field.ty {
                    Ty::Record(i) => (i as usize) < index,
                    ty => ty.is_value() && ty != Ty::VertexOutput,
                };
                if !ok {
                    self.error(
                        TYPE,
                        field.span,
                        format!(
                            "field `{}` of `{}` cannot be a `{}`",
                            field.name,
                            record.name,
                            p.spell(field.ty)
                        ),
                    );
                }
            }
        }
        for binding in p.uniforms.iter().chain(&p.instance) {
            self.laid_out(binding);
        }
        for varying in &p.varyings {
            if !matches!(varying.ty.lanes(), Some((s, _)) if s.is_numeric()) {
                self.error(
                    STAGE,
                    varying.span,
                    format!(
                        "varying `{}` is a `{}`; a varying is a numeric scalar or vector",
                        varying.name,
                        p.spell(varying.ty)
                    ),
                );
            }
        }
        for texture in &p.textures {
            if !matches!(texture.ty, Ty::Texture(_)) {
                self.error(
                    TYPE,
                    texture.span,
                    format!("texture `{}` is not a texture", texture.name),
                );
            }
        }
        for sampler in &p.samplers {
            if sampler.ty != Ty::Sampler {
                self.error(
                    TYPE,
                    sampler.span,
                    format!("sampler `{}` is not a sampler", sampler.name),
                );
            }
        }
        if let Some(vertex) = &p.vertex
            && vertex.ret != Ty::VertexOutput
        {
            self.error(
                STAGE,
                vertex.span,
                "the vertex entry returns a `VertexOutput`",
            );
        }
        if let Some(fragment) = &p.fragment
            && !matches!(fragment.ret, Ty::VEC4 | Ty::Color)
        {
            self.error(
                STAGE,
                fragment.span,
                "the fragment entry returns a `Vec4F32` or a `ColorLinear`",
            );
        }
    }

    /// A uniform or instance member: data of a type with a layout.
    fn laid_out(&mut self, binding: &Binding) {
        fn bool_free(p: &Program, ty: Ty) -> bool {
            match ty {
                Ty::Scalar(Scalar::Bool) | Ty::Vector(Scalar::Bool, _) => false,
                Ty::Record(i) => p.records[i as usize]
                    .fields
                    .iter()
                    .all(|f| bool_free(p, f.ty)),
                _ => true,
            }
        }
        let ty = binding.ty;
        if !ty.is_value()
            || ty == Ty::VertexOutput
            || matches!(ty, Ty::Record(i) if i as usize >= self.program.records.len())
        {
            self.error(
                TYPE,
                binding.span,
                format!(
                    "`{}` cannot be a `{}`",
                    binding.name,
                    self.program.spell(ty)
                ),
            );
        } else if !bool_free(self.program, ty) {
            self.error(
                ABI,
                binding.span,
                format!(
                    "`{}` holds a `Bool`, which has no buffer layout; use `U32`",
                    binding.name
                ),
            );
        }
    }

    fn function(&mut self, function: &Function, index: Option<u32>) {
        let mut cx = Cx {
            function,
            index,
            loops: 0,
            span: function.span,
        };
        if function.params as usize > function.locals.len() {
            self.error(
                TYPE,
                function.span,
                format!("`{}` has more parameters than locals", function.name),
            );
            return;
        }
        if let Some(stage) = function.stage {
            if function.builtins.len() != function.params as usize {
                self.error(
                    STAGE,
                    function.span,
                    "every entry parameter receives a builtin",
                );
            }
            for (local, builtin) in function.locals.iter().zip(&function.builtins) {
                if Builtin::of(stage, &local.name) != Some(*builtin) || local.ty != builtin.ty() {
                    self.error(
                        STAGE,
                        function.span,
                        format!(
                            "entry parameter `{}` is no `{stage:?}` builtin of its type",
                            local.name
                        ),
                    );
                }
            }
        } else if function.ret != Ty::Unit && !function.ret.is_value() {
            self.error(
                TYPE,
                function.span,
                format!(
                    "`{}` cannot return a `{}`",
                    function.name,
                    self.program.spell(function.ret)
                ),
            );
        }
        for local in &function.locals {
            if !local.ty.is_value() {
                self.error(
                    TYPE,
                    function.span,
                    format!(
                        "local `{}` cannot be a `{}`",
                        local.name,
                        self.program.spell(local.ty)
                    ),
                );
            }
        }
        self.block(&function.body, &mut cx);
        if function.ret != Ty::Unit && !returns(&function.body) {
            self.error(
                TYPE,
                function.span,
                format!("`{}` can end without returning a value", function.name),
            );
        }
    }

    fn block(&mut self, block: &Block, cx: &mut Cx<'_>) {
        for stmt in &block.0 {
            self.stmt(stmt, cx);
        }
    }

    fn local(&mut self, local: u32, cx: &Cx<'_>) -> Option<Ty> {
        let ty = cx.function.locals.get(local as usize).map(|l| l.ty);
        if ty.is_none() {
            self.error(TYPE, cx.span, format!("local #{local} does not exist"));
        }
        ty
    }

    fn stmt(&mut self, stmt: &Stmt, cx: &mut Cx<'_>) {
        match stmt {
            Stmt::Let(local, value) => {
                let ty = self.expr(value, cx);
                if let (Some(want), Some(got)) = (self.local(*local, cx), ty)
                    && want != got
                {
                    self.mismatch(want, got, cx.span);
                }
            }
            Stmt::Declare(local) => {
                let _ = self.local(*local, cx);
            }
            Stmt::Assign(place, value) => {
                let want = self.place(place, cx);
                let got = self.expr(value, cx);
                if let (Some(want), Some(got)) = (want, got)
                    && want != got
                {
                    self.mismatch(want, got, cx.span);
                }
            }
            Stmt::If(cond, then, otherwise) => {
                self.condition(cond, cx);
                self.block(then, cx);
                self.block(otherwise, cx);
            }
            Stmt::For(l) => {
                let var = self.local(l.var, cx);
                if !matches!(var, Some(Ty::Scalar(Scalar::I32 | Scalar::U32))) {
                    self.error(TYPE, cx.span, "a loop counter is an `I32` or a `U32`");
                }
                for bound in [&l.start, &l.end] {
                    if let (Some(var), Some(got)) = (var, self.expr(bound, cx))
                        && var != got
                    {
                        self.mismatch(var, got, cx.span);
                    }
                }
                if !l.guarded && !literal_range(l) {
                    self.error("E8103", cx.span, "an unguarded loop needs literal bounds");
                }
                cx.loops += 1;
                self.block(&l.body, cx);
                cx.loops -= 1;
            }
            Stmt::Break | Stmt::Continue => {
                if cx.loops == 0 {
                    self.error("E2803", cx.span, "`break`/`continue` outside a loop");
                }
            }
            Stmt::Return(value) => {
                let got = match value {
                    Some(value) => self.expr(value, cx),
                    None => Some(Ty::Unit),
                };
                if let Some(got) = got
                    && got != cx.function.ret
                {
                    self.mismatch(cx.function.ret, got, cx.span);
                }
            }
            Stmt::Discard => {
                if cx.function.stage != Some(Stage::Fragment) {
                    self.error(STAGE, cx.span, "only the fragment entry discards");
                }
            }
        }
    }

    fn condition(&mut self, cond: &Expr, cx: &Cx<'_>) {
        if let Some(got) = self.expr(cond, cx)
            && got != Ty::BOOL
        {
            self.mismatch(Ty::BOOL, got, cx.span);
        }
    }

    fn mismatch(&mut self, want: Ty, got: Ty, span: Span) {
        let p = self.program;
        self.error(
            TYPE,
            span,
            format!("expected `{}`, found `{}`", p.spell(want), p.spell(got)),
        );
    }

    /// The type a place holds, when it is writable here.
    fn place(&mut self, place: &Place, cx: &Cx<'_>) -> Option<Ty> {
        let mut ty = match place.root {
            Root::Local(local) => {
                let l = cx.function.locals.get(local as usize)?;
                if !l.mutable {
                    self.error("E2110", cx.span, format!("`{}` is not mutable", l.name));
                }
                l.ty
            }
            Root::Varying(index) => {
                if cx.function.stage != Some(Stage::Vertex) {
                    self.error(STAGE, cx.span, "only the vertex entry writes varyings");
                }
                self.program.varyings.get(index as usize)?.ty
            }
        };
        for &step in &place.path {
            ty = self.step(ty, step, cx.span)?;
        }
        Some(ty)
    }

    /// The type of field, lane or column `step` of a `ty`.
    fn step(&mut self, ty: Ty, step: u32, span: Span) -> Option<Ty> {
        let next = match ty {
            Ty::Vector(s, n) => (step < u32::from(n)).then_some(Ty::Scalar(s)),
            Ty::Color => (step < 4).then_some(Ty::F32),
            Ty::Matrix(n) => (step < u32::from(n)).then_some(Ty::Vector(Scalar::F32, n)),
            _ => self
                .program
                .fields(ty)
                .and_then(|f| f.get(step as usize).map(|(_, t)| *t)),
        };
        if next.is_none() {
            self.error(
                TYPE,
                span,
                format!("`{}` has no member #{step}", self.program.spell(ty)),
            );
        }
        next
    }

    /// The expression's checked type: its declared one when that follows from
    /// its parts.
    fn expr(&mut self, e: &Expr, cx: &Cx<'_>) -> Option<Ty> {
        let p = self.program;
        let span = cx.span;
        let derived = match &e.kind {
            ExprKind::Bool(_) => Some(Ty::BOOL),
            ExprKind::I32(_) => Some(Ty::I32),
            ExprKind::U32(_) => Some(Ty::U32),
            ExprKind::F32(_) => Some(Ty::F32),
            ExprKind::Local(local) => self.local(*local, cx),
            ExprKind::Uniform(i) => p.uniforms.get(*i as usize).map(|b| b.ty),
            ExprKind::Instance(i) => p.instance.get(*i as usize).map(|b| b.ty),
            ExprKind::Varying(i) => p.varyings.get(*i as usize).map(|b| b.ty),
            ExprKind::Texture(i) => p.textures.get(*i as usize).map(|b| b.ty),
            ExprKind::Sampler(i) => p.samplers.get(*i as usize).map(|b| b.ty),
            ExprKind::Unary(op, operand) => {
                let operand = self.expr(operand, cx)?;
                op.result(operand)
            }
            ExprKind::Binary(op, lhs, rhs) => {
                let (lhs, rhs) = (self.expr(lhs, cx)?, self.expr(rhs, cx)?);
                op.result(lhs, rhs)
            }
            ExprKind::Construct(parts) => {
                let parts: Option<Vec<Ty>> = parts.iter().map(|part| self.expr(part, cx)).collect();
                constructs(p, e.ty, &parts?).then_some(e.ty)
            }
            ExprKind::Swizzle(base, lanes) => {
                let base = self.expr(base, cx)?;
                let (scalar, n) = match base {
                    Ty::Color => (Scalar::F32, 4),
                    other => other.lanes()?,
                };
                let count = e.ty.lanes().map_or(0, |(_, c)| c);
                let ok = e.ty.scalar() == Some(scalar)
                    && n > 1
                    && lanes[..count as usize].iter().all(|&l| l < n);
                ok.then_some(e.ty)
            }
            ExprKind::Member(base, field) => {
                let base = self.expr(base, cx)?;
                p.fields(base)
                    .and_then(|f| f.get(*field as usize).map(|(_, t)| *t))
            }
            ExprKind::Index(base, index) => {
                let base = self.expr(base, cx)?;
                let index = self.expr(index, cx)?;
                let integer = matches!(index, Ty::Scalar(Scalar::I32 | Scalar::U32));
                match base {
                    Ty::Vector(s, _) if integer => Some(Ty::Scalar(s)),
                    Ty::Matrix(n) if integer => Some(Ty::Vector(Scalar::F32, n)),
                    _ => None,
                }
            }
            ExprKind::Call(callee, args) => {
                let args: Option<Vec<Ty>> = args.iter().map(|a| self.expr(a, cx)).collect();
                let args = args?;
                if cx.index.is_some_and(|own| *callee >= own) {
                    self.error(SUBSET, span, "a shader function calls only functions declared before it in call order; recursion is not allowed");
                }
                let f = p.functions.get(*callee as usize)?;
                let params: Vec<Ty> = f.locals[..f.params as usize].iter().map(|l| l.ty).collect();
                (params == args).then_some(f.ret)
            }
            ExprKind::Intrinsic(intrinsic, args) => {
                let args: Option<Vec<Ty>> = args.iter().map(|a| self.expr(a, cx)).collect();
                let args = args?;
                self.stage_use(*intrinsic, cx);
                intrinsic.result(&args)
            }
            ExprKind::Convert(operand) => {
                let from = self.expr(operand, cx)?;
                match (from.lanes(), e.ty.lanes()) {
                    (Some((fs, fl)), Some((ts, tl))) => {
                        (fl == tl && fs.is_numeric() && ts.is_numeric()).then_some(e.ty)
                    }
                    _ => None,
                }
            }
        };
        match derived {
            Some(ty) if ty == e.ty => Some(ty),
            Some(ty) => {
                self.mismatch(e.ty, ty, span);
                None
            }
            None => {
                self.error(
                    TYPE,
                    span,
                    format!("an expression of `{}` is ill-formed", p.spell(e.ty)),
                );
                None
            }
        }
    }

    fn stage_use(&mut self, intrinsic: Intrinsic, cx: &Cx<'_>) {
        let stage = cx.function.stage;
        let bad = match intrinsic.stages() {
            StageUse::Any => false,
            StageUse::FragmentOnly => stage == Some(Stage::Vertex),
            StageUse::VertexOnly => stage == Some(Stage::Fragment),
        };
        if bad {
            self.error(
                STAGE,
                cx.span,
                format!("`{}` is not available in this stage", intrinsic.name()),
            );
        }
    }
}

/// Whether `parts` construct a `ty`.
pub(crate) fn constructs(p: &Program, ty: Ty, parts: &[Ty]) -> bool {
    let lanes = |parts: &[Ty], scalar: Scalar| -> Option<u32> {
        parts.iter().try_fold(0u32, |sum, part| match part.lanes() {
            Some((s, n)) if s == scalar => Some(sum + u32::from(n)),
            _ => None,
        })
    };
    match ty {
        Ty::Vector(scalar, n) => {
            matches!(parts, [Ty::Scalar(s)] if *s == scalar)
                || lanes(parts, scalar) == Some(u32::from(n))
        }
        Ty::Color => matches!(parts, [Ty::VEC4]) || lanes(parts, Scalar::F32) == Some(4),
        Ty::Matrix(n) => {
            parts.len() == n as usize && parts.iter().all(|t| *t == Ty::Vector(Scalar::F32, n))
                || parts.len() == (n * n) as usize && parts.iter().all(|t| *t == Ty::F32)
        }
        Ty::Record(_) | Ty::VertexOutput => p.fields(ty).is_some_and(|f| {
            f.len() == parts.len() && f.iter().zip(parts).all(|((_, t), p)| t == p)
        }),
        _ => false,
    }
}

/// Whether every path through `block` returns or discards.
pub(crate) fn returns(block: &Block) -> bool {
    block.0.iter().any(|stmt| match stmt {
        Stmt::Return(_) | Stmt::Discard => true,
        Stmt::If(_, then, otherwise) => returns(then) && returns(otherwise),
        _ => false,
    })
}

fn literal_range(l: &super::Loop) -> bool {
    let literal = |e: &Expr| matches!(e.kind, ExprKind::I32(_) | ExprKind::U32(_));
    literal(&l.start) && literal(&l.end)
}
