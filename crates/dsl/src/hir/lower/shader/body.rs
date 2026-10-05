//! Shader bodies: statements and expressions checked against the shader
//! subset and lowered into the program's structured statements.
//!
//! An `if` or `match` used as a value lowers into a declared local each branch
//! assigns; a `match` lowers into `if` chains comparing a scalar against its
//! arms' literals. An untyped literal takes its type from the other operand
//! or argument, else `I32` or `F32`.

use viso_shader::program::{
    BinaryOp, Block, Builtin, Expr, ExprKind, Function, Intrinsic, Local, Loop, Place, Root,
    Scalar, Stage, StageUse, Stmt, Ty, UnaryOp,
};

use super::{Global, Shader, span, stage_word};
use crate::ast::{AstNode, ShaderEntry, ShaderFn};
use crate::diag::Diagnostic;
use crate::hir::Ty as HostTy;
use crate::hir::infer::{parse_int_literal, split_unit_literal};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

/// How a block or branch ends.
enum End {
    /// With this value.
    Value(Expr),
    /// It never reaches its end.
    Diverges,
    /// Without a value.
    Falls,
}

/// One function or entry being checked.
struct Body<'s, 'e, 'p> {
    sh: &'s mut Shader<'e, 'p>,
    /// The entry's stage; `None` in a `fn`.
    stage: Option<Stage>,
    ret: Ty,
    locals: Vec<Local>,
    scopes: Vec<Vec<(String, u32)>>,
    loops: u32,
    /// Each `fn` this one calls, with the call's span.
    calls: Vec<(u32, TextRange)>,
    /// Locals whose initializer did not check: reading one reports nothing
    /// more.
    poisoned: Vec<u32>,
}

/// Checks the shader `fn` `f` (its signature already typed), giving its
/// function and the calls it makes.
pub(super) fn function(sh: &mut Shader<'_, '_>, f: &ShaderFn) -> (Function, Vec<(u32, TextRange)>) {
    let name = f.name().map(|n| n.text()).unwrap_or_default();
    let at = f.name().map_or(f.syntax().text_range(), |n| n.text_range());
    let (params, ret) = match sh.fns.get(&name) {
        Some((_, sig)) => (sig.params.clone(), sig.ret),
        None => (Vec::new(), Ty::Unit),
    };
    let mut body = Body::new(sh, None, ret);
    for (param, ty) in f.params().iter().zip(&params) {
        body.param(param.syntax(), *ty);
    }
    if !ty_ok_as_value(ret) && ret != Ty::Unit {
        let spelled = body.sh.program.spell(ret);
        body.error(
            "E8105",
            at,
            format!("a shader `fn` cannot return a `{spelled}`"),
        );
    }
    let block = body.callable_body(f.body().map(|b| b.syntax().clone()), at);
    let function = Function {
        name: name.as_str().into(),
        stage: None,
        params: params.len() as u32,
        builtins: Vec::new(),
        ret,
        locals: std::mem::take(&mut body.locals),
        body: block,
        span: span(at),
    };
    (function, body.calls)
}

/// Checks the entry `entry` of `stage`.
pub(super) fn entry(
    sh: &mut Shader<'_, '_>,
    entry: &ShaderEntry,
    stage: Stage,
) -> (Option<Function>, Vec<(u32, TextRange)>) {
    let at = entry
        .stage_token()
        .map_or(entry.syntax().text_range(), |t| t.text_range());
    let errors = sh.diagnostics.len();
    let ret = match entry.return_type() {
        Some(r) => sh.annotation(r.syntax()),
        None => None,
    };
    let ret = match (stage, ret) {
        (Stage::Vertex, Some(Ty::VertexOutput)) => Ty::VertexOutput,
        (Stage::Fragment, Some(t @ (Ty::VEC4 | Ty::Color))) => t,
        (_, found) => {
            if found.is_some() || sh.diagnostics.len() == errors {
                let want = match stage {
                    Stage::Vertex => "a `VertexOutput`",
                    Stage::Fragment => "a `Vec4F32` or a `ColorLinear`",
                };
                let range = entry.return_type().map_or(at, |r| r.syntax().text_range());
                sh.error(
                    "E8106",
                    range,
                    format!("a {} entry returns {want}", stage_word(stage)),
                );
            }
            match stage {
                Stage::Vertex => Ty::VertexOutput,
                Stage::Fragment => Ty::VEC4,
            }
        }
    };
    let mut body = Body::new(sh, Some(stage), ret);
    let mut builtins = Vec::new();
    for param in entry.params() {
        let Some(name) = param.name() else {
            continue;
        };
        let text = name.text();
        let ty = body.sh.annotation(param.syntax());
        let builtin = Builtin::of(stage, &text);
        match builtin {
            Some(b) if ty == Some(b.ty()) => {}
            Some(b) => {
                let want = body.sh.program.spell(b.ty());
                body.error(
                    "E8106",
                    name.text_range(),
                    format!("builtin `{text}` is a `{want}`"),
                );
            }
            None => {
                let offered = Builtin::names(stage).join("`, `");
                body.error(
                    "E8106",
                    name.text_range(),
                    format!(
                        "`{text}` is no {} builtin; it takes `{offered}`",
                        stage_word(stage)
                    ),
                );
            }
        }
        let builtin = builtin.unwrap_or(Builtin::VertexId);
        builtins.push(builtin);
        body.param(param.syntax(), builtin.ty());
    }
    let block = body.callable_body(entry.body().map(|b| b.syntax().clone()), at);
    let function = Function {
        name: stage_word(stage).into(),
        stage: Some(stage),
        params: builtins.len() as u32,
        builtins,
        ret,
        locals: std::mem::take(&mut body.locals),
        body: block,
        span: span(at),
    };
    (Some(function), Vec::new())
}

/// Renumbers every call in `block` by `position`, old index to new.
pub(super) fn renumber_calls(block: &mut Block, position: &[u32]) {
    fn expr(e: &mut Expr, position: &[u32]) {
        match &mut e.kind {
            ExprKind::Call(callee, args) => {
                *callee = position[*callee as usize];
                args.iter_mut().for_each(|a| expr(a, position));
            }
            ExprKind::Intrinsic(_, args) | ExprKind::Construct(args) => {
                args.iter_mut().for_each(|a| expr(a, position));
            }
            ExprKind::Unary(_, a)
            | ExprKind::Swizzle(a, _)
            | ExprKind::Member(a, _)
            | ExprKind::Convert(a) => expr(a, position),
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
                expr(a, position);
                expr(b, position);
            }
            _ => {}
        }
    }
    for stmt in &mut block.0 {
        match stmt {
            Stmt::Let(_, e) | Stmt::Assign(_, e) | Stmt::Return(Some(e)) => expr(e, position),
            Stmt::If(c, a, b) => {
                expr(c, position);
                renumber_calls(a, position);
                renumber_calls(b, position);
            }
            Stmt::For(l) => {
                expr(&mut l.start, position);
                expr(&mut l.end, position);
                renumber_calls(&mut l.body, position);
            }
            _ => {}
        }
    }
}

/// Whether a local, parameter or return may be a `ty`.
fn ty_ok_as_value(ty: Ty) -> bool {
    ty.is_value() && ty != Ty::VertexOutput
}

fn tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .filter(|t| !t.kind().is_trivia())
}

fn has_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    tokens(node).any(|t| t.kind() == kind)
}

fn exprs(node: &SyntaxNode) -> Vec<SyntaxNode> {
    node.children()
        .into_iter()
        .filter(|c| crate::ast::Expr::can_cast(c.kind()))
        .collect()
}

fn child(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().into_iter().find(|c| c.kind() == kind)
}

fn name_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    tokens(node).find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
}

/// Whether `node` is a literal without a suffix, possibly negated or
/// parenthesized: its type comes from its context.
fn untyped(node: &SyntaxNode) -> bool {
    match node.kind() {
        SyntaxKind::LiteralExpr => tokens(node)
            .any(|t| matches!(t.kind(), SyntaxKind::IntLiteral | SyntaxKind::FloatLiteral)),
        SyntaxKind::UnaryExpr => {
            has_token(node, SyntaxKind::Minus) && exprs(node).first().is_some_and(untyped)
        }
        SyntaxKind::ParenExpr => exprs(node).first().is_some_and(untyped),
        _ => false,
    }
}

/// The scalar a literal takes under `hint`.
fn hinted(hint: Option<Ty>) -> Option<Scalar> {
    match hint? {
        Ty::Matrix(_) | Ty::Color => Some(Scalar::F32),
        other => other.scalar(),
    }
}

/// The lanes `name` swizzles out of `lanes` lanes: one to four letters of
/// `xyzw` or of `rgba`.
fn swizzle(name: &str, lanes: u8) -> Option<Vec<u8>> {
    let set = |c: char| match c {
        'x' | 'y' | 'z' | 'w' => Some((0, "xyzw".find(c)? as u8)),
        'r' | 'g' | 'b' | 'a' => Some((1, "rgba".find(c)? as u8)),
        _ => None,
    };
    let picked: Option<Vec<(u8, u8)>> = name.chars().map(set).collect();
    let picked = picked?;
    let one_set = picked.windows(2).all(|w| w[0].0 == w[1].0);
    let ok = (1..=4).contains(&picked.len()) && one_set && picked.iter().all(|(_, l)| *l < lanes);
    ok.then(|| picked.into_iter().map(|(_, l)| l).collect())
}

fn lanes_array(lanes: &[u8]) -> [u8; 4] {
    let mut out = [0; 4];
    out[..lanes.len()].copy_from_slice(lanes);
    out
}

/// Whether every path through `stmts` leaves the enclosing block.
fn diverges(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Stmt::Return(_) | Stmt::Discard | Stmt::Break | Stmt::Continue => true,
        Stmt::If(_, a, b) => diverges(&a.0) && diverges(&b.0),
        _ => false,
    })
}

impl<'s, 'e, 'p> Body<'s, 'e, 'p> {
    fn new(sh: &'s mut Shader<'e, 'p>, stage: Option<Stage>, ret: Ty) -> Self {
        Body {
            sh,
            stage,
            ret,
            locals: Vec::new(),
            scopes: vec![Vec::new()],
            loops: 0,
            calls: Vec::new(),
            poisoned: Vec::new(),
        }
    }

    fn error(&mut self, code: &'static str, range: TextRange, message: impl Into<String>) {
        self.sh.error(code, range, message);
    }

    fn spell(&self, ty: Ty) -> String {
        self.sh.program.spell(ty)
    }

    fn param(&mut self, param: &SyntaxNode, ty: Ty) {
        let Some(name) = name_token(param) else {
            return;
        };
        if !ty_ok_as_value(ty) {
            let spelled = self.spell(ty);
            self.error(
                "E8105",
                name.text_range(),
                format!("a parameter cannot be a `{spelled}`"),
            );
        }
        self.bind(&name.text(), ty, has_token(param, SyntaxKind::MutKw));
    }

    fn bind(&mut self, name: &str, ty: Ty, mutable: bool) -> u32 {
        let index = self.locals.len() as u32;
        self.locals.push(Local {
            name: name.into(),
            ty,
            mutable,
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.push((name.to_owned(), index));
        }
        index
    }

    /// A fresh mutable local no name reaches.
    fn temporary(&mut self, ty: Ty) -> u32 {
        let index = self.locals.len() as u32;
        self.locals.push(Local {
            name: format!("t{index}").into(),
            ty,
            mutable: true,
        });
        index
    }

    fn lookup(&self, name: &str) -> Option<u32> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter().rev())
            .find(|(n, _)| n == name)
            .map(|(_, i)| *i)
    }

    /// The body of a `fn` or entry: its tail is its return value.
    fn callable_body(&mut self, block: Option<SyntaxNode>, at: TextRange) -> Block {
        let Some(block) = block else {
            return Block::default();
        };
        let mut out = Vec::new();
        let want = (self.ret != Ty::Unit).then_some(Some(self.ret));
        match self.block(&block, &mut out, want) {
            Some(End::Value(value)) => {
                if let Some(value) = self.coerce(value, self.ret, block.text_range()) {
                    out.push(Stmt::Return(Some(value)));
                }
            }
            Some(End::Falls) if self.ret != Ty::Unit => {
                let spelled = self.spell(self.ret);
                let end = block.last_child().map_or(at, |_| block.text_range());
                let mut d = Diagnostic::error(
                    "E2103",
                    end,
                    format!("this body can end without returning a `{spelled}`"),
                );
                d = d.expecting([spelled], "Unit");
                self.sh.diagnostics.push(d);
            }
            _ => {}
        }
        Block(out)
    }

    /// Checks a block's statements into `out` in their own scope; with `want`
    /// its tail expression is its value, typed under the hint.
    fn block(
        &mut self,
        block: &SyntaxNode,
        out: &mut Vec<Stmt>,
        want: Option<Option<Ty>>,
    ) -> Option<End> {
        self.scopes.push(Vec::new());
        let children = block.children();
        let mut attributes = Vec::new();
        let mut end = Some(End::Falls);
        for (i, node) in children.iter().enumerate() {
            if node.kind() == SyntaxKind::Attribute {
                attributes.push(node.clone());
                continue;
            }
            if let (true, Some(hint)) = (i + 1 == children.len(), want)
                && let Some(value) = self.tail(node, hint, out)
            {
                self.stray_attributes(&mut attributes);
                end = value;
                break;
            }
            self.stmt(node, std::mem::take(&mut attributes), out);
        }
        self.stray_attributes(&mut attributes);
        self.scopes.pop();
        match end {
            Some(End::Falls) if diverges(out) => Some(End::Diverges),
            other => other,
        }
    }

    fn stray_attributes(&mut self, attributes: &mut Vec<SyntaxNode>) {
        for attr in attributes.drain(..) {
            self.error(
                "E8105",
                attr.text_range(),
                "this attribute marks nothing here: `@max_iterations` marks a `for` loop",
            );
        }
    }

    /// A block's last statement as its value, when it is one: an expression
    /// without `;`, or an `if`/`match` statement. `None` when it is a plain
    /// statement; `Some(None)` when its value did not check.
    fn tail(
        &mut self,
        node: &SyntaxNode,
        hint: Option<Ty>,
        out: &mut Vec<Stmt>,
    ) -> Option<Option<End>> {
        match node.kind() {
            SyntaxKind::ExprStmt if !has_token(node, SyntaxKind::Semi) => {
                let value = exprs(node).into_iter().next()?;
                Some(self.value(&value, hint, out))
            }
            SyntaxKind::IfStmt | SyntaxKind::MatchStmt => Some(self.value(node, hint, out)),
            _ => None,
        }
    }

    /// An expression in value position, which may be a branching one.
    fn value(&mut self, node: &SyntaxNode, hint: Option<Ty>, out: &mut Vec<Stmt>) -> Option<End> {
        match node.kind() {
            SyntaxKind::IfExpr | SyntaxKind::IfStmt => self.if_value(node, hint, out),
            SyntaxKind::MatchExpr | SyntaxKind::MatchStmt => {
                self.match_lowered(node, Some(hint), out)
            }
            SyntaxKind::BlockExpr => {
                let block = child(node, SyntaxKind::Block)?;
                self.block(&block, out, Some(hint))
            }
            _ => self.expr(node, hint, out).map(End::Value),
        }
    }

    /// The value of a branching expression, `None` when it does not check or
    /// never produces one.
    fn branch_value(
        &mut self,
        node: &SyntaxNode,
        hint: Option<Ty>,
        out: &mut Vec<Stmt>,
    ) -> Option<Expr> {
        match self.value(node, hint, out)? {
            End::Value(e) => Some(e),
            End::Diverges | End::Falls => {
                self.error("E2103", node.text_range(), "this has no value");
                None
            }
        }
    }

    // --- statements ---------------------------------------------------------

    fn stmt(&mut self, node: &SyntaxNode, attributes: Vec<SyntaxNode>, out: &mut Vec<Stmt>) {
        let mut attributes = attributes;
        if node.kind() == SyntaxKind::ForStmt {
            self.for_stmt(node, attributes, out);
            return;
        }
        self.stray_attributes(&mut attributes);
        let at = node.text_range();
        match node.kind() {
            SyntaxKind::LetStmt => self.let_stmt(node, out),
            SyntaxKind::AssignStmt => self.assign(node, out),
            SyntaxKind::ExprStmt => self.expr_stmt(node, out),
            SyntaxKind::ReturnStmt => {
                let value = exprs(node).into_iter().next();
                match value {
                    Some(value) => {
                        let ret = self.ret;
                        // A value that did not check still ends the path.
                        let e = self.expect(&value, ret, out);
                        out.push(Stmt::Return(e));
                    }
                    None if self.ret == Ty::Unit => out.push(Stmt::Return(None)),
                    None => {
                        let spelled = self.spell(self.ret);
                        self.error(
                            "E2103",
                            at,
                            format!("this returns no value; expected `{spelled}`"),
                        );
                    }
                }
            }
            SyntaxKind::BreakStmt | SyntaxKind::ContinueStmt => {
                if self.loops == 0 {
                    self.error("E2803", at, "`break`/`continue` outside a loop");
                } else if !exprs(node).is_empty() {
                    self.error("E8105", at, "a shader `break` takes no value");
                } else if node.kind() == SyntaxKind::BreakStmt {
                    out.push(Stmt::Break);
                } else {
                    out.push(Stmt::Continue);
                }
            }
            SyntaxKind::IfStmt => self.if_stmt(node, out),
            SyntaxKind::MatchStmt => {
                let _ = self.match_lowered(node, None, out);
            }
            SyntaxKind::Block => {
                let _ = self.block(node, out, None);
            }
            SyntaxKind::WhileStmt | SyntaxKind::LoopStmt => {
                let head = tokens(node).next().map_or(at, |t| t.text_range());
                self.error("E8103", head, "a shader loop needs a static bound: use `for i in 0..n` with literal bounds or `@max_iterations(N)`");
            }
            SyntaxKind::EmitStmt | SyntaxKind::TransactionStmt => {
                self.error("E8105", at, "a shader has no events or transactions");
            }
            _ => {}
        }
    }

    fn let_stmt(&mut self, node: &SyntaxNode, out: &mut Vec<Stmt>) {
        let Some(pattern) = child(node, SyntaxKind::Pattern) else {
            return;
        };
        let ident = pattern
            .children()
            .into_iter()
            .find(|c| c.kind() == SyntaxKind::IdentPattern)
            .and_then(|p| name_token(&p));
        let annotation =
            child(node, SyntaxKind::TypePath).or_else(|| child(node, SyntaxKind::TupleType));
        let declared = match &annotation {
            Some(ty) => match self.sh.ty(ty) {
                Some(ty) => Some(ty),
                None => return,
            },
            None => None,
        };
        let Some(init) = exprs(node).into_iter().next() else {
            return;
        };
        let value = match declared {
            Some(ty) => self.expect_value(&init, ty, out),
            None => self.branch_value(&init, None, out),
        };
        let Some(ident) = ident else {
            self.error(
                "E8105",
                pattern.text_range(),
                "a shader `let` binds one name",
            );
            return;
        };
        let Some(value) = value else {
            // Bind the name anyway so its uses do not cascade.
            let local = self.bind(&ident.text(), declared.unwrap_or(Ty::Unit), true);
            self.poisoned.push(local);
            return;
        };
        if !ty_ok_as_value(value.ty) {
            let spelled = self.spell(value.ty);
            self.error(
                "E8105",
                init.text_range(),
                format!("a `{spelled}` is a binding, not a value a local holds"),
            );
            return;
        }
        let mutable = has_token(node, SyntaxKind::MutKw)
            || pattern
                .descendants()
                .iter()
                .any(|n| has_token(n, SyntaxKind::MutKw));
        let local = self.bind(&ident.text(), value.ty, mutable);
        out.push(Stmt::Let(local, value));
    }

    fn assign(&mut self, node: &SyntaxNode, out: &mut Vec<Stmt>) {
        let parts = exprs(node);
        let [target, value] = parts.as_slice() else {
            return;
        };
        let op = tokens(node).find_map(|t| {
            Some(match t.kind() {
                SyntaxKind::Eq => None,
                SyntaxKind::PlusEq => Some(BinaryOp::Add),
                SyntaxKind::MinusEq => Some(BinaryOp::Sub),
                SyntaxKind::StarEq => Some(BinaryOp::Mul),
                SyntaxKind::SlashEq => Some(BinaryOp::Div),
                SyntaxKind::PercentEq => Some(BinaryOp::Rem),
                SyntaxKind::AmpEq => Some(BinaryOp::BitAnd),
                SyntaxKind::PipeEq => Some(BinaryOp::BitOr),
                SyntaxKind::CaretEq => Some(BinaryOp::BitXor),
                SyntaxKind::ShlEq => Some(BinaryOp::Shl),
                SyntaxKind::ShrEq => Some(BinaryOp::Shr),
                _ => return None,
            })
        });
        let Some(op) = op else {
            return;
        };
        let Some((place, ty)) = self.place(target) else {
            return;
        };
        let value = match op {
            None => self.expect_value(value, ty, out),
            Some(op) => {
                let current = self.read(&place, ty);
                let Some(rhs) = self.expr(value, Some(operand_hint(op, ty)), out) else {
                    return;
                };
                match op.result(ty, rhs.ty) {
                    Some(result) if result == ty => Some(Expr::new(
                        ty,
                        ExprKind::Binary(op, Box::new(current), Box::new(rhs)),
                    )),
                    _ => {
                        let (l, r) = (self.spell(ty), self.spell(rhs.ty));
                        self.error(
                            "E2103",
                            node.text_range(),
                            format!("`{}=` does not apply to `{l}` and `{r}`", op.symbol()),
                        );
                        None
                    }
                }
            }
        };
        if let Some(value) = value {
            out.push(Stmt::Assign(place, value));
        }
    }

    /// The writable place `node` names and the type it holds.
    fn place(&mut self, node: &SyntaxNode) -> Option<(Place, Ty)> {
        let at = node.text_range();
        match node.kind() {
            SyntaxKind::ParenExpr => self.place(exprs(node).first()?),
            SyntaxKind::PathExpr => {
                let name = name_token(node)?.text();
                if let Some(local) = self.lookup(&name) {
                    let l = &self.locals[local as usize];
                    let ty = l.ty;
                    if !l.mutable {
                        self.error(
                            "E2110",
                            at,
                            format!("`{name}` is not mutable; declare it `let mut`"),
                        );
                    }
                    return Some((
                        Place {
                            root: Root::Local(local),
                            path: Vec::new(),
                        },
                        ty,
                    ));
                }
                match self.sh.globals.get(&name).map(|(g, _)| *g) {
                    Some(Global::Varying(i)) => {
                        if self.stage != Some(Stage::Vertex) {
                            self.error("E8106", at, "only the vertex entry writes varyings");
                        }
                        let ty = self.sh.program.varyings[i as usize].ty;
                        Some((
                            Place {
                                root: Root::Varying(i),
                                path: Vec::new(),
                            },
                            ty,
                        ))
                    }
                    Some(_) => {
                        self.error("E2110", at, format!("`{name}` is read-only"));
                        None
                    }
                    None => {
                        self.error("E2001", at, format!("cannot find `{name}` in this shader"));
                        None
                    }
                }
            }
            SyntaxKind::FieldExpr => {
                let receiver = exprs(node).into_iter().next()?;
                let field = name_token(node)?;
                let (mut place, ty) = self.place(&receiver)?;
                let name = field.text();
                let step = match ty {
                    Ty::Vector(..) | Ty::Color => {
                        let lanes = ty.lanes().map_or(4, |(_, n)| n);
                        match swizzle(&name, lanes) {
                            Some(l) if l.len() == 1 => Some((
                                u32::from(l[0]),
                                Ty::of_lanes(ty.scalar().unwrap_or(Scalar::F32), 1),
                            )),
                            Some(_) => {
                                self.error(
                                    "E8105",
                                    field.text_range(),
                                    "assign one lane at a time",
                                );
                                return None;
                            }
                            None => None,
                        }
                    }
                    _ => self.sh.program.fields(ty).and_then(|f| {
                        f.iter()
                            .position(|(n, _)| *n == name)
                            .map(|i| (i as u32, f[i].1))
                    }),
                };
                let Some((step, next)) = step else {
                    let spelled = self.spell(ty);
                    self.error(
                        "E2103",
                        field.text_range(),
                        format!("`{spelled}` has no field `{name}`"),
                    );
                    return None;
                };
                place.path.push(step);
                Some((place, next))
            }
            SyntaxKind::IndexExpr => {
                let parts = exprs(node);
                let [base, index] = parts.as_slice() else {
                    return None;
                };
                let (mut place, ty) = self.place(base)?;
                let Some(i) = int_literal(index) else {
                    self.error(
                        "E8105",
                        index.text_range(),
                        "an assignment indexes by an integer literal",
                    );
                    return None;
                };
                let next = match ty {
                    Ty::Vector(s, n) if i < u32::from(n) => Ty::Scalar(s),
                    Ty::Matrix(n) if i < u32::from(n) => Ty::Vector(Scalar::F32, n),
                    _ => {
                        let spelled = self.spell(ty);
                        self.error(
                            "E2103",
                            index.text_range(),
                            format!("`{spelled}` has no element {i}"),
                        );
                        return None;
                    }
                };
                place.path.push(i);
                Some((place, next))
            }
            _ => {
                self.error("E2110", at, "this cannot be assigned");
                None
            }
        }
    }

    /// The value a place holds now.
    fn read(&self, place: &Place, ty: Ty) -> Expr {
        let (mut e, mut current) = match place.root {
            Root::Local(l) => {
                let t = self.locals[l as usize].ty;
                (Expr::new(t, ExprKind::Local(l)), t)
            }
            Root::Varying(v) => {
                let t = self.sh.program.varyings[v as usize].ty;
                (Expr::new(t, ExprKind::Varying(v)), t)
            }
        };
        for &step in &place.path {
            let (next, kind) = match current {
                Ty::Vector(s, _) => (Ty::Scalar(s), None),
                Ty::Color => (Ty::F32, None),
                Ty::Matrix(n) => (Ty::Vector(Scalar::F32, n), Some(true)),
                other => (
                    self.sh
                        .program
                        .fields(other)
                        .map_or(ty, |f| f[step as usize].1),
                    Some(false),
                ),
            };
            e = match kind {
                None => Expr::new(
                    next,
                    ExprKind::Swizzle(Box::new(e), lanes_array(&[step as u8])),
                ),
                Some(true) => Expr::new(
                    next,
                    ExprKind::Index(
                        Box::new(e),
                        Box::new(Expr::new(Ty::U32, ExprKind::U32(step))),
                    ),
                ),
                Some(false) => Expr::new(next, ExprKind::Member(Box::new(e), step)),
            };
            current = next;
        }
        e
    }

    fn expr_stmt(&mut self, node: &SyntaxNode, out: &mut Vec<Stmt>) {
        let Some(e) = exprs(node).into_iter().next() else {
            return;
        };
        if e.kind() == SyntaxKind::CallExpr
            && let Some(callee) = exprs(&e).into_iter().next()
            && callee.kind() == SyntaxKind::PathExpr
            && name_token(&callee).is_some_and(|t| t.text() == "discard")
        {
            if self.stage != Some(Stage::Fragment) {
                self.error("E8106", e.text_range(), "only the fragment entry discards");
            }
            out.push(Stmt::Discard);
            return;
        }
        match e.kind() {
            SyntaxKind::IfExpr => self.if_stmt(&e, out),
            SyntaxKind::MatchExpr => {
                let _ = self.match_lowered(&e, None, out);
            }
            SyntaxKind::BlockExpr => {
                if let Some(block) = child(&e, SyntaxKind::Block) {
                    let _ = self.block(&block, out, None);
                }
            }
            _ => {
                // A shader expression has no effect; its value is dropped.
                let _ = self.expr(&e, None, out);
            }
        }
    }

    fn if_stmt(&mut self, node: &SyntaxNode, out: &mut Vec<Stmt>) {
        let Some(cond) = exprs(node).into_iter().next() else {
            return;
        };
        let cond = self.expect(&cond, Ty::BOOL, out);
        let mut then = Vec::new();
        if let Some(block) = child(node, SyntaxKind::Block) {
            let _ = self.block(&block, &mut then, None);
        }
        let mut otherwise = Vec::new();
        if let Some(nested) =
            child(node, SyntaxKind::IfStmt).or_else(|| child(node, SyntaxKind::IfExpr))
        {
            self.if_stmt(&nested, &mut otherwise);
        } else if let Some(block) = node
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::Block)
            .nth(1)
        {
            let _ = self.block(&block, &mut otherwise, None);
        }
        if let Some(cond) = cond {
            out.push(Stmt::If(cond, Block(then), Block(otherwise)));
        }
    }

    /// An `if` with an `else` used as a value: a local each branch assigns.
    fn if_value(
        &mut self,
        node: &SyntaxNode,
        hint: Option<Ty>,
        out: &mut Vec<Stmt>,
    ) -> Option<End> {
        let cond = exprs(node).into_iter().next()?;
        let cond = self.expect(&cond, Ty::BOOL, out);
        let blocks: Vec<SyntaxNode> = node
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::Block)
            .collect();
        let nested = child(node, SyntaxKind::IfStmt).or_else(|| child(node, SyntaxKind::IfExpr));
        if nested.is_none() && blocks.len() < 2 {
            self.error(
                "E2103",
                node.text_range(),
                "an `if` used as a value needs an `else`",
            );
            return None;
        }
        let mut then = Vec::new();
        let first = self.block(blocks.first()?, &mut then, Some(hint))?;
        let hint = match &first {
            End::Value(e) => Some(e.ty),
            _ => hint,
        };
        let mut otherwise = Vec::new();
        let second = match &nested {
            Some(nested) => self.value(nested, hint, &mut otherwise)?,
            None => self.block(&blocks[1], &mut otherwise, Some(hint))?,
        };
        let cond = cond?;
        self.join(
            cond,
            (first, then),
            (second, otherwise),
            node.text_range(),
            out,
        )
    }

    /// Joins two branches of `cond` into one value: a local both assign.
    fn join(
        &mut self,
        cond: Expr,
        (a, mut then): (End, Vec<Stmt>),
        (b, mut otherwise): (End, Vec<Stmt>),
        at: TextRange,
        out: &mut Vec<Stmt>,
    ) -> Option<End> {
        let ty = match (&a, &b) {
            (End::Value(x), End::Value(y)) if x.ty != y.ty => {
                let (l, r) = (self.spell(x.ty), self.spell(y.ty));
                self.error("E2103", at, format!("the branches give `{l}` and `{r}`"));
                return None;
            }
            (End::Falls, _) | (_, End::Falls) => {
                self.error("E2103", at, "a branch has no value");
                return None;
            }
            (End::Value(x), _) | (_, End::Value(x)) => x.ty,
            (End::Diverges, End::Diverges) => {
                out.push(Stmt::If(cond, Block(then), Block(otherwise)));
                return Some(End::Diverges);
            }
        };
        let local = self.temporary(ty);
        for (end, stmts) in [(a, &mut then), (b, &mut otherwise)] {
            if let End::Value(v) = end {
                stmts.push(Stmt::Assign(
                    Place {
                        root: Root::Local(local),
                        path: Vec::new(),
                    },
                    v,
                ));
            }
        }
        out.push(Stmt::Declare(local));
        out.push(Stmt::If(cond, Block(then), Block(otherwise)));
        Some(End::Value(Expr::new(ty, ExprKind::Local(local))))
    }

    /// A `match` on an `I32`, `U32` or `Bool` lowered into an `if` chain; with
    /// `value` its arms give its value.
    fn match_lowered(
        &mut self,
        node: &SyntaxNode,
        value: Option<Option<Ty>>,
        out: &mut Vec<Stmt>,
    ) -> Option<End> {
        let scrutinee = exprs(node).into_iter().next()?;
        let s = self.expr(&scrutinee, None, out)?;
        if !matches!(s.ty, Ty::Scalar(Scalar::I32 | Scalar::U32 | Scalar::Bool)) {
            let spelled = self.spell(s.ty);
            self.error(
                "E8105",
                scrutinee.text_range(),
                format!("a shader `match` is on an `I32`, `U32` or `Bool`, not a `{spelled}`"),
            );
            return None;
        }
        let s_ty = s.ty;
        let s_local = self.temporary(s_ty);
        out.push(Stmt::Let(s_local, s));
        let arms: Vec<SyntaxNode> = node
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::MatchArm)
            .collect();
        // Each arm: its literals (empty for a catch-all) and its lowered body.
        let mut lowered: Vec<(Vec<Expr>, End, Vec<Stmt>, TextRange)> = Vec::new();
        let mut hint = value.flatten();
        let mut caught = false;
        let mut covered = [false; 2];
        for arm in &arms {
            let at = arm.text_range();
            if caught {
                self.error(
                    "E2302",
                    at,
                    "this arm is unreachable: an earlier arm matches everything",
                );
                continue;
            }
            if has_token(arm, SyntaxKind::IfKw) {
                self.error("E8105", at, "a shader `match` arm has no guard");
                continue;
            }
            let Some(pattern) = child(arm, SyntaxKind::Pattern) else {
                continue;
            };
            self.scopes.push(Vec::new());
            let literals = self.arm_pattern(&pattern, s_local, s_ty, &mut covered);
            let Some(literals) = literals else {
                self.scopes.pop();
                continue;
            };
            caught |= literals.is_empty();
            let body = arm
                .children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::Block || crate::ast::Expr::can_cast(c.kind()));
            let mut stmts = Vec::new();
            let end = match (&body, value) {
                (Some(b), Some(_)) if b.kind() == SyntaxKind::Block => {
                    self.block(b, &mut stmts, Some(hint))
                }
                (Some(b), Some(_)) => self.value(b, hint, &mut stmts),
                (Some(b), None) if b.kind() == SyntaxKind::Block => self.block(b, &mut stmts, None),
                (Some(b), None) => {
                    let _ = self.expr(b, None, &mut stmts);
                    Some(End::Falls)
                }
                (None, _) => Some(End::Falls),
            };
            self.scopes.pop();
            let Some(end) = end else {
                continue;
            };
            if let End::Value(e) = &end
                && hint.is_none()
            {
                hint = Some(e.ty);
            }
            lowered.push((literals, end, stmts, at));
        }
        let exhaustive = caught || (s_ty == Ty::BOOL && covered == [true, true]);
        if !exhaustive {
            self.error(
                "E2301",
                node.text_range(),
                "this `match` does not cover every value: add a `_` arm",
            );
            return None;
        }
        if value.is_none() {
            let mut chain: Vec<Stmt> = Vec::new();
            for (literals, _, stmts, _) in lowered.into_iter().rev() {
                chain = match self.any_of(&literals, s_local, s_ty) {
                    None => stmts,
                    Some(cond) => vec![Stmt::If(cond, Block(stmts), Block(chain))],
                };
            }
            out.extend(chain);
            return Some(End::Falls);
        }
        // As a value: every arm that falls through gives one of one type.
        let ty = lowered.iter().find_map(|(_, e, _, _)| match e {
            End::Value(v) => Some(v.ty),
            _ => None,
        });
        let Some(ty) = ty else {
            let all_diverge = lowered
                .iter()
                .all(|(_, e, _, _)| matches!(e, End::Diverges));
            if all_diverge {
                let mut chain: Vec<Stmt> = Vec::new();
                for (literals, _, stmts, _) in lowered.into_iter().rev() {
                    chain = match self.any_of(&literals, s_local, s_ty) {
                        None => stmts,
                        Some(cond) => vec![Stmt::If(cond, Block(stmts), Block(chain))],
                    };
                }
                out.extend(chain);
                return Some(End::Diverges);
            }
            self.error("E2103", node.text_range(), "this `match` has no value");
            return None;
        };
        let local = self.temporary(ty);
        let mut chain: Vec<Stmt> = Vec::new();
        for (literals, end, mut stmts, at) in lowered.into_iter().rev() {
            match end {
                End::Value(v) if v.ty == ty => stmts.push(Stmt::Assign(
                    Place {
                        root: Root::Local(local),
                        path: Vec::new(),
                    },
                    v,
                )),
                End::Value(v) => {
                    let (want, got) = (self.spell(ty), self.spell(v.ty));
                    self.error(
                        "E2103",
                        at,
                        format!("this arm gives `{got}`; the others give `{want}`"),
                    );
                    return None;
                }
                End::Diverges => {}
                End::Falls => {
                    self.error("E2103", at, "this arm has no value");
                    return None;
                }
            }
            chain = match self.any_of(&literals, s_local, s_ty) {
                None => stmts,
                Some(cond) => vec![Stmt::If(cond, Block(stmts), Block(chain))],
            };
        }
        out.push(Stmt::Declare(local));
        out.extend(chain);
        Some(End::Value(Expr::new(ty, ExprKind::Local(local))))
    }

    /// `s == a || s == b || ...`; `None` for a catch-all.
    fn any_of(&self, literals: &[Expr], s: u32, ty: Ty) -> Option<Expr> {
        literals.iter().fold(None, |acc, lit| {
            let eq = Expr::new(
                Ty::BOOL,
                ExprKind::Binary(
                    BinaryOp::Eq,
                    Box::new(Expr::new(ty, ExprKind::Local(s))),
                    Box::new(lit.clone()),
                ),
            );
            Some(match acc {
                None => eq,
                Some(acc) => Expr::new(
                    Ty::BOOL,
                    ExprKind::Binary(BinaryOp::Or, Box::new(acc), Box::new(eq)),
                ),
            })
        })
    }

    /// The literals an arm pattern matches, empty for a catch-all (a binding
    /// names the scrutinee in the arm).
    fn arm_pattern(
        &mut self,
        pattern: &SyntaxNode,
        s: u32,
        ty: Ty,
        covered: &mut [bool; 2],
    ) -> Option<Vec<Expr>> {
        let inner = pattern.children().into_iter().next()?;
        match inner.kind() {
            SyntaxKind::WildcardPattern => Some(Vec::new()),
            SyntaxKind::IdentPattern => {
                let name = name_token(&inner)?.text();
                if let Some(scope) = self.scopes.last_mut() {
                    scope.push((name, s));
                }
                Some(Vec::new())
            }
            SyntaxKind::LiteralPattern => Some(vec![self.pattern_literal(&inner, ty, covered)?]),
            SyntaxKind::OrPattern => {
                let mut all = Vec::new();
                for alt in inner.children() {
                    if alt.kind() != SyntaxKind::LiteralPattern {
                        self.error(
                            "E8105",
                            alt.text_range(),
                            "a shader `match` arm matches literals, `_` or a name",
                        );
                        return None;
                    }
                    all.push(self.pattern_literal(&alt, ty, covered)?);
                }
                Some(all)
            }
            SyntaxKind::Pattern => self.arm_pattern(&inner, s, ty, covered),
            _ => {
                self.error(
                    "E8105",
                    inner.text_range(),
                    "a shader `match` arm matches literals, `_` or a name",
                );
                None
            }
        }
    }

    fn pattern_literal(
        &mut self,
        node: &SyntaxNode,
        ty: Ty,
        covered: &mut [bool; 2],
    ) -> Option<Expr> {
        let negative = has_token(node, SyntaxKind::Minus);
        let tok = tokens(node).find(|t| t.kind() != SyntaxKind::Minus)?;
        let at = node.text_range();
        match (tok.kind(), ty) {
            (SyntaxKind::TrueKw | SyntaxKind::FalseKw, Ty::Scalar(Scalar::Bool)) => {
                let b = tok.kind() == SyntaxKind::TrueKw;
                covered[usize::from(b)] = true;
                Some(Expr::new(Ty::BOOL, ExprKind::Bool(b)))
            }
            (SyntaxKind::IntLiteral, Ty::Scalar(s @ (Scalar::I32 | Scalar::U32))) => {
                let value = parse_int_literal(&tok.text()).map(|v| if negative { -v } else { v });
                self.int_of(value, s, at)
            }
            _ => {
                let spelled = self.spell(ty);
                self.error(
                    "E2103",
                    at,
                    format!("this pattern does not match a `{spelled}`"),
                );
                None
            }
        }
    }

    fn for_stmt(&mut self, node: &SyntaxNode, attributes: Vec<SyntaxNode>, out: &mut Vec<Stmt>) {
        let head = tokens(node)
            .next()
            .map_or(node.text_range(), |t| t.text_range());
        let mut max_iterations = None;
        for attr in &attributes {
            let path = child(attr, SyntaxKind::PathExpr).map(|p| p.text().trim().to_owned());
            if path.as_deref() != Some("max_iterations") {
                let name = path.unwrap_or_default();
                self.error(
                    "E8105",
                    attr.text_range(),
                    format!("`@{name}` does not mark a shader loop"),
                );
                continue;
            }
            let args: Vec<SyntaxNode> = child(attr, SyntaxKind::ArgumentList)
                .map(|l| {
                    l.children()
                        .into_iter()
                        .filter(|c| c.kind() == SyntaxKind::Argument)
                        .collect()
                })
                .unwrap_or_default();
            let value = match args.as_slice() {
                [arg] => exprs(arg).first().and_then(int_literal).filter(|n| *n > 0),
                _ => None,
            };
            match value {
                Some(n) => max_iterations = Some(n),
                None => self.error(
                    "E8105",
                    attr.text_range(),
                    "`@max_iterations(N)` takes one positive integer literal",
                ),
            }
        }
        let Some(range) = exprs(node).into_iter().next() else {
            return;
        };
        if range.kind() != SyntaxKind::RangeExpr {
            self.error(
                "E8105",
                range.text_range(),
                "a shader `for` runs over an integer range `a..b`",
            );
            return;
        }
        let inclusive = has_token(&range, SyntaxKind::DotDotEq);
        let bounds = exprs(&range);
        let [lo, hi] = bounds.as_slice() else {
            self.error(
                "E8103",
                range.text_range(),
                "a shader loop range has both bounds",
            );
            return;
        };
        let (start, end) = if untyped(lo) && !untyped(hi) {
            let end = self.expr(hi, None, out);
            let start = self.expr(lo, end.as_ref().map(|e| e.ty), out);
            (start, end)
        } else {
            let start = self.expr(lo, None, out);
            let end = self.expr(hi, start.as_ref().map(|e| e.ty), out);
            (start, end)
        };
        let (Some(start), Some(end)) = (start, end) else {
            return;
        };
        if start.ty != end.ty || !matches!(start.ty, Ty::Scalar(Scalar::I32 | Scalar::U32)) {
            let (l, r) = (self.spell(start.ty), self.spell(end.ty));
            self.error(
                "E2103",
                range.text_range(),
                format!("a loop range is of one `I32` or `U32`, not `{l}` and `{r}`"),
            );
            return;
        }
        let literal = |e: &Expr| match e.kind {
            ExprKind::I32(v) => Some(i64::from(v)),
            ExprKind::U32(v) => Some(i64::from(v)),
            _ => None,
        };
        let count = match (literal(&start), literal(&end)) {
            (Some(a), Some(b)) => {
                Some((b - a + i64::from(inclusive)).clamp(0, i64::from(u32::MAX)) as u32)
            }
            _ => None,
        };
        let (max, guarded) = match (count, max_iterations) {
            (Some(count), _) => (count, false),
            (None, Some(max)) => (max, true),
            (None, None) => {
                self.error("E8103", head, "this loop's bound is not static: give literal bounds or mark it `@max_iterations(N)`");
                return;
            }
        };
        let ty = start.ty;
        self.scopes.push(Vec::new());
        let var = match child(node, SyntaxKind::Pattern).and_then(|p| {
            p.children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::IdentPattern)
                .and_then(|p| name_token(&p))
        }) {
            Some(name) => self.bind(&name.text(), ty, false),
            None => self.temporary(ty),
        };
        let mut body = Vec::new();
        self.loops += 1;
        if let Some(block) = child(node, SyntaxKind::Block) {
            let _ = self.block(&block, &mut body, None);
        }
        self.loops -= 1;
        self.scopes.pop();
        out.push(Stmt::For(Box::new(Loop {
            var,
            start,
            end,
            inclusive,
            max,
            guarded,
            body: Block(body),
        })));
    }

    // --- expressions --------------------------------------------------------

    /// `node` as a value of `want`.
    fn expect(&mut self, node: &SyntaxNode, want: Ty, out: &mut Vec<Stmt>) -> Option<Expr> {
        let e = self.expr(node, Some(want), out)?;
        self.coerce(e, want, node.text_range())
    }

    /// Like [`Self::expect`], for a value position that may branch.
    fn expect_value(&mut self, node: &SyntaxNode, want: Ty, out: &mut Vec<Stmt>) -> Option<Expr> {
        let e = self.branch_value(node, Some(want), out)?;
        self.coerce(e, want, node.text_range())
    }

    /// `e` when it is a `want`, else a mismatch: there is no implicit
    /// conversion.
    fn coerce(&mut self, e: Expr, want: Ty, at: TextRange) -> Option<Expr> {
        if e.ty == want {
            return Some(e);
        }
        let (w, g) = (self.spell(want), self.spell(e.ty));
        let numeric = |t: Ty| t.lanes().is_some_and(|(s, _)| s.is_numeric());
        let same_shape = numeric(want)
            && numeric(e.ty)
            && want.lanes().map(|l| l.1) == e.ty.lanes().map(|l| l.1);
        let mut d = if same_shape {
            let mut d = Diagnostic::error(
                "E2102",
                at,
                format!("a `{g}` does not convert to `{w}` implicitly"),
            );
            d.notes.push(format!("convert it explicitly: `... as {w}`"));
            d
        } else {
            Diagnostic::error("E2103", at, format!("expected `{w}`, found `{g}`"))
        };
        d = d.expecting([w], g);
        self.sh.diagnostics.push(d);
        None
    }

    /// Types `node` (its literals under `hint`) and lowers it, any branching
    /// part into statements on `out`.
    fn expr(&mut self, node: &SyntaxNode, hint: Option<Ty>, out: &mut Vec<Stmt>) -> Option<Expr> {
        let at = node.text_range();
        match node.kind() {
            SyntaxKind::LiteralExpr => self.literal(node, hint, false),
            SyntaxKind::ParenExpr => self.expr(exprs(node).first()?, hint, out),
            SyntaxKind::PathExpr => self.path(node),
            SyntaxKind::UnaryExpr => self.unary(node, hint, out),
            SyntaxKind::BinaryExpr => self.binary(node, hint, out),
            SyntaxKind::CastExpr => self.cast(node, out),
            SyntaxKind::FieldExpr => self.field(node, out),
            SyntaxKind::IndexExpr => self.index(node, out),
            SyntaxKind::CallExpr => self.call(node, hint, out),
            SyntaxKind::RecordExpr => self.record(node, out),
            SyntaxKind::IfExpr | SyntaxKind::MatchExpr | SyntaxKind::BlockExpr => {
                self.branch_value(node, hint, out)
            }
            SyntaxKind::ClosureExpr => {
                self.error("E8105", at, "a shader has no closures");
                None
            }
            SyntaxKind::TryExpr | SyntaxKind::OptionalFieldExpr => {
                self.error("E8105", at, "a shader has no `?`: nothing in it fails");
                None
            }
            SyntaxKind::ListExpr => {
                self.error("E8101", at, "`List` is a host type; a shader has no lists");
                None
            }
            SyntaxKind::TupleExpr => {
                self.error(
                    "E8101",
                    at,
                    "a tuple is a host type; use a vector or a `@shader_value` record",
                );
                None
            }
            SyntaxKind::RangeExpr => {
                self.error("E8105", at, "a range is only a shader `for` loop's head");
                None
            }
            _ => {
                self.error("E8105", at, "this is not part of the shader subset");
                None
            }
        }
    }

    /// A literal, negated when `negative`.
    fn literal(&mut self, node: &SyntaxNode, hint: Option<Ty>, negative: bool) -> Option<Expr> {
        let tok = tokens(node).next()?;
        let at = node.text_range();
        let text = tok.text();
        match tok.kind() {
            SyntaxKind::TrueKw | SyntaxKind::FalseKw if !negative => Some(Expr::new(
                Ty::BOOL,
                ExprKind::Bool(tok.kind() == SyntaxKind::TrueKw),
            )),
            SyntaxKind::IntLiteral => {
                let value = parse_int_literal(&text).map(|v| if negative { -v } else { v });
                match hinted(hint) {
                    Some(Scalar::F32) => Some(Expr::new(Ty::F32, ExprKind::F32(value? as f32))),
                    Some(s @ Scalar::U32) => self.int_of(value, s, at),
                    _ => self.int_of(value, Scalar::I32, at),
                }
            }
            SyntaxKind::FloatLiteral => {
                if matches!(hinted(hint), Some(Scalar::I32 | Scalar::U32)) {
                    let want = self.spell(Ty::Scalar(hinted(hint)?));
                    self.error("E2103", at, format!("a float literal is not an `{want}`"));
                    return None;
                }
                self.float_of(&text, negative, at)
            }
            SyntaxKind::UnitLiteral => {
                let Some((body, unit)) = split_unit_literal(&text) else {
                    self.error("E8101", at, "a unit literal is a host value");
                    return None;
                };
                match unit {
                    HostTy::F32 => self.float_of(body, negative, at),
                    HostTy::I32 => {
                        let value = parse_int_literal(body).map(|v| if negative { -v } else { v });
                        self.int_of(value, Scalar::I32, at)
                    }
                    HostTy::U32 => {
                        let value = parse_int_literal(body).map(|v| if negative { -v } else { v });
                        self.int_of(value, Scalar::U32, at)
                    }
                    HostTy::F64 => {
                        self.error("E8102", at, "a shader has no `F64`; use `f32`");
                        None
                    }
                    other => {
                        let name = crate::hir::infer::ty_name(&other);
                        self.error(
                            "E8101",
                            at,
                            format!(
                                "`{name}` is a host type; a shader uses `I32`, `U32` and `F32`"
                            ),
                        );
                        None
                    }
                }
            }
            SyntaxKind::StringLiteral | SyntaxKind::RawStringLiteral => {
                self.error(
                    "E8101",
                    at,
                    "`String` is a host type; a shader has no strings",
                );
                None
            }
            SyntaxKind::CharLiteral => {
                self.error("E8101", at, "`Char` is a host type");
                None
            }
            SyntaxKind::ColorLiteral => {
                let mut d = Diagnostic::error("E8101", at, "`Color` is a host type");
                d.notes
                    .push("write the linear color: `ColorLinear(r, g, b, a)`".into());
                self.sh.diagnostics.push(d);
                None
            }
            SyntaxKind::NoneKw => {
                self.error("E8101", at, "`Option` is a host type");
                None
            }
            _ => {
                self.error("E8105", at, "this literal is not part of the shader subset");
                None
            }
        }
    }

    fn int_of(&mut self, value: Option<i128>, scalar: Scalar, at: TextRange) -> Option<Expr> {
        let fits = match scalar {
            Scalar::I32 => value
                .and_then(|v| i32::try_from(v).ok())
                .map(|v| Expr::new(Ty::I32, ExprKind::I32(v))),
            _ => value
                .and_then(|v| u32::try_from(v).ok())
                .map(|v| Expr::new(Ty::U32, ExprKind::U32(v))),
        };
        if fits.is_none() {
            let name = scalar.name();
            self.error("E1203", at, format!("this literal does not fit `{name}`"));
        }
        fits
    }

    fn float_of(&mut self, text: &str, negative: bool, at: TextRange) -> Option<Expr> {
        let cleaned: String = text.chars().filter(|c| *c != '_').collect();
        match cleaned.parse::<f32>() {
            Ok(v) if v.is_finite() => Some(Expr::new(
                Ty::F32,
                ExprKind::F32(if negative { -v } else { v }),
            )),
            _ => {
                self.error("E1203", at, "this literal does not fit `F32`");
                None
            }
        }
    }

    fn path(&mut self, node: &SyntaxNode) -> Option<Expr> {
        let at = node.text_range();
        let segments: Vec<SyntaxToken> = tokens(node)
            .filter(|t| {
                matches!(
                    t.kind(),
                    SyntaxKind::Ident
                        | SyntaxKind::RawIdent
                        | SyntaxKind::SelfValueKw
                        | SyntaxKind::SelfTypeKw
                )
            })
            .collect();
        let [name] = segments.as_slice() else {
            self.error(
                "E2001",
                at,
                "a shader names its locals, its members and its `fn`s by one name",
            );
            return None;
        };
        let text = name.text();
        if let Some(local) = self.lookup(&text) {
            if self.poisoned.contains(&local) {
                return None;
            }
            let ty = self.locals[local as usize].ty;
            return Some(Expr::new(ty, ExprKind::Local(local)));
        }
        let Some((global, _)) = self.sh.globals.get(&text).copied() else {
            let mut d =
                Diagnostic::error("E2001", at, format!("cannot find `{text}` in this shader"));
            d.notes
                .push("a shader reads its parameters, its locals and its own members".into());
            self.sh.diagnostics.push(d);
            return None;
        };
        if self.stage.is_none() {
            self.error(
                "E8106",
                at,
                format!("a shader `fn` reads only its parameters; pass `{text}` in"),
            );
            return None;
        }
        let p = &self.sh.program;
        Some(match global {
            Global::Uniform(i) => Expr::new(p.uniforms[i as usize].ty, ExprKind::Uniform(i)),
            Global::Instance(i) => Expr::new(p.instance[i as usize].ty, ExprKind::Instance(i)),
            Global::Varying(i) => Expr::new(p.varyings[i as usize].ty, ExprKind::Varying(i)),
            Global::Texture(i) => Expr::new(p.textures[i as usize].ty, ExprKind::Texture(i)),
            Global::Sampler(i) => Expr::new(p.samplers[i as usize].ty, ExprKind::Sampler(i)),
        })
    }

    fn unary(&mut self, node: &SyntaxNode, hint: Option<Ty>, out: &mut Vec<Stmt>) -> Option<Expr> {
        let op = tokens(node).next()?;
        let operand = exprs(node).into_iter().next()?;
        let at = node.text_range();
        let op = match op.kind() {
            SyntaxKind::Minus => {
                if operand.kind() == SyntaxKind::LiteralExpr && untyped(&operand) {
                    return self.literal(&operand, hint, true);
                }
                UnaryOp::Neg
            }
            SyntaxKind::Plus => return self.expr(&operand, hint, out),
            SyntaxKind::Bang => UnaryOp::Not,
            SyntaxKind::Tilde => UnaryOp::BitNot,
            _ => {
                self.error("E8105", at, "a shader has no `await`");
                return None;
            }
        };
        let e = self.expr(&operand, hint, out)?;
        match op.result(e.ty) {
            Some(ty) => Some(Expr::new(ty, ExprKind::Unary(op, Box::new(e)))),
            None => {
                let spelled = self.spell(e.ty);
                self.error(
                    "E2103",
                    at,
                    format!("`{}` does not apply to `{spelled}`", op.symbol()),
                );
                None
            }
        }
    }

    fn binary(&mut self, node: &SyntaxNode, hint: Option<Ty>, out: &mut Vec<Stmt>) -> Option<Expr> {
        let parts = exprs(node);
        let [lhs, rhs] = parts.as_slice() else {
            return None;
        };
        let symbol = tokens(node).find(|t| {
            BinaryOp::from_symbol(&t.text()).is_some() || t.kind() == SyntaxKind::QuestionQuestion
        })?;
        let at = node.text_range();
        let Some(op) = BinaryOp::from_symbol(&symbol.text()) else {
            self.error(
                "E8105",
                symbol.text_range(),
                "a shader has no `??`: nothing in it is optional",
            );
            return None;
        };
        let operand = match op {
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor
            | BinaryOp::Shl
            | BinaryOp::Shr => hint,
            _ => None,
        };
        let (l, r) = if untyped(lhs) && !untyped(rhs) {
            let r = self.expr(rhs, operand, out);
            let l = self.expr(
                lhs,
                r.as_ref().map(|r| operand_hint(op, r.ty)).or(operand),
                out,
            );
            (l?, r?)
        } else {
            let l = self.expr(lhs, operand, out);
            let r = self.expr(
                rhs,
                l.as_ref().map(|l| operand_hint(op, l.ty)).or(operand),
                out,
            );
            (l?, r?)
        };
        match op.result(l.ty, r.ty) {
            Some(ty) => Some(Expr::new(
                ty,
                ExprKind::Binary(op, Box::new(l), Box::new(r)),
            )),
            None => {
                let (a, b) = (self.spell(l.ty), self.spell(r.ty));
                let both_numeric = l.ty.scalar().is_some_and(Scalar::is_numeric)
                    && r.ty.scalar().is_some_and(Scalar::is_numeric);
                let code = if both_numeric && l.ty.scalar() != r.ty.scalar() {
                    "E2102"
                } else {
                    "E2103"
                };
                let mut d = Diagnostic::error(
                    code,
                    at,
                    format!("`{}` does not apply to `{a}` and `{b}`", op.symbol()),
                );
                if code == "E2102" {
                    d.notes.push("convert one side explicitly with `as`".into());
                }
                self.sh.diagnostics.push(d);
                None
            }
        }
    }

    fn cast(&mut self, node: &SyntaxNode, out: &mut Vec<Stmt>) -> Option<Expr> {
        let operand = exprs(node).into_iter().next()?;
        let target = child(node, SyntaxKind::TypePath)?;
        let to = self.sh.ty(&target)?;
        let e = self.expr(&operand, Some(to), out)?;
        if e.ty == to {
            return Some(e);
        }
        match (e.ty.lanes(), to.lanes()) {
            (Some((fs, fl)), Some((ts, tl))) if fl == tl && fs.is_numeric() && ts.is_numeric() => {
                Some(Expr::new(to, ExprKind::Convert(Box::new(e))))
            }
            _ => {
                let (f, t) = (self.spell(e.ty), self.spell(to));
                self.error(
                    "E2103",
                    node.text_range(),
                    format!("a `{f}` does not convert to `{t}`"),
                );
                None
            }
        }
    }

    fn field(&mut self, node: &SyntaxNode, out: &mut Vec<Stmt>) -> Option<Expr> {
        let receiver = exprs(node).into_iter().next()?;
        let field = name_token(node)?;
        let base = self.expr(&receiver, None, out)?;
        let name = field.text();
        if let Some((scalar, lanes)) = match base.ty {
            Ty::Color => Some((Scalar::F32, 4)),
            Ty::Vector(s, n) => Some((s, n)),
            _ => None,
        } && let Some(picked) = swizzle(&name, lanes)
        {
            let ty = Ty::of_lanes(scalar, picked.len() as u8);
            return Some(Expr::new(
                ty,
                ExprKind::Swizzle(Box::new(base), lanes_array(&picked)),
            ));
        }
        let member = self.sh.program.fields(base.ty).and_then(|f| {
            f.iter()
                .position(|(n, _)| *n == name)
                .map(|i| (i as u32, f[i].1))
        });
        match member {
            Some((index, ty)) => Some(Expr::new(ty, ExprKind::Member(Box::new(base), index))),
            None => {
                let spelled = self.spell(base.ty);
                self.error(
                    "E2103",
                    field.text_range(),
                    format!("`{spelled}` has no field `{name}`"),
                );
                None
            }
        }
    }

    fn index(&mut self, node: &SyntaxNode, out: &mut Vec<Stmt>) -> Option<Expr> {
        let parts = exprs(node);
        let [base, index] = parts.as_slice() else {
            return None;
        };
        let b = self.expr(base, None, out)?;
        let element = match b.ty {
            Ty::Vector(s, n) => (Ty::Scalar(s), n),
            Ty::Matrix(n) => (Ty::Vector(Scalar::F32, n), n),
            _ => {
                let spelled = self.spell(b.ty);
                self.error(
                    "E2103",
                    base.text_range(),
                    format!("a `{spelled}` is not indexed"),
                );
                return None;
            }
        };
        if let Some(i) = int_literal(index) {
            if i >= u32::from(element.1) {
                let spelled = self.spell(b.ty);
                self.error(
                    "E2103",
                    index.text_range(),
                    format!("`{spelled}` has no element {i}"),
                );
                return None;
            }
            if let Ty::Vector(..) = b.ty {
                return Some(Expr::new(
                    element.0,
                    ExprKind::Swizzle(Box::new(b), lanes_array(&[i as u8])),
                ));
            }
        }
        let i = self.expr(index, Some(Ty::U32), out)?;
        if !matches!(i.ty, Ty::Scalar(Scalar::I32 | Scalar::U32)) {
            let spelled = self.spell(i.ty);
            self.error(
                "E2103",
                index.text_range(),
                format!("an index is an `I32` or a `U32`, not a `{spelled}`"),
            );
            return None;
        }
        Some(Expr::new(
            element.0,
            ExprKind::Index(Box::new(b), Box::new(i)),
        ))
    }

    fn call(&mut self, node: &SyntaxNode, hint: Option<Ty>, out: &mut Vec<Stmt>) -> Option<Expr> {
        let at = node.text_range();
        let callee = exprs(node).into_iter().next()?;
        let args: Vec<SyntaxNode> = child(node, SyntaxKind::ArgumentList)
            .map(|l| {
                l.children()
                    .into_iter()
                    .filter(|c| c.kind() == SyntaxKind::Argument)
                    .collect()
            })
            .unwrap_or_default();
        let mut arg_nodes = Vec::with_capacity(args.len() + 1);
        for arg in &args {
            if has_token(arg, SyntaxKind::Colon) {
                self.error(
                    "E8105",
                    arg.text_range(),
                    "a shader call passes its arguments in order, unnamed",
                );
                return None;
            }
            arg_nodes.push(exprs(arg).into_iter().next()?);
        }
        if has_token(node, SyntaxKind::ColonColon)
            || child(node, SyntaxKind::GenericCallArgs).is_some()
        {
            self.error("E8105", at, "a shader call takes no type arguments");
            return None;
        }
        // `a.f(b)` calls `f(a, b)`.
        let (name, receiver) = match callee.kind() {
            SyntaxKind::PathExpr => {
                let segments: Vec<_> = tokens(&callee)
                    .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
                    .collect();
                let [name] = segments.as_slice() else {
                    self.error(
                        "E2001",
                        callee.text_range(),
                        "a shader calls a function by one name",
                    );
                    return None;
                };
                (name.clone(), None)
            }
            SyntaxKind::FieldExpr => {
                let receiver = exprs(&callee).into_iter().next()?;
                (name_token(&callee)?, Some(receiver))
            }
            _ => {
                self.error(
                    "E8105",
                    callee.text_range(),
                    "a shader calls a function by name",
                );
                return None;
            }
        };
        let text = name.text();
        if text == "discard" {
            self.error("E8105", at, "`discard()` is a statement");
            return None;
        }
        if let Some(receiver) = receiver {
            arg_nodes.insert(0, receiver);
        }
        if let Some(ty) = Ty::from_name(&text).filter(|_| callee.kind() == SyntaxKind::PathExpr) {
            return self.construct(ty, &arg_nodes, at, out);
        }
        if let Some((index, sig)) = self
            .sh
            .fns
            .get(&text)
            .map(|(i, s)| (*i, (s.params.clone(), s.ret)))
        {
            let (params, ret) = sig;
            if params.len() != arg_nodes.len() {
                self.error(
                    "E2103",
                    at,
                    format!(
                        "`{text}` takes {} arguments, not {}",
                        params.len(),
                        arg_nodes.len()
                    ),
                );
                return None;
            }
            let mut values = Vec::with_capacity(params.len());
            for (node, param) in arg_nodes.iter().zip(&params) {
                values.push(self.expect(node, *param, out));
            }
            let values: Option<Vec<Expr>> = values.into_iter().collect();
            self.calls.push((index, at));
            return Some(Expr::new(ret, ExprKind::Call(index, values?)));
        }
        if let Some(intrinsic) = Intrinsic::from_name(&text) {
            return self.intrinsic(intrinsic, &arg_nodes, hint, at, out);
        }
        let mut d = Diagnostic::error(
            "E2001",
            name.text_range(),
            format!("cannot find function `{text}` in this shader"),
        );
        d.notes
            .push("a shader calls its own `fn`s and the shader intrinsics".into());
        self.sh.diagnostics.push(d);
        None
    }

    /// Types `args` with their literals under the first other argument's type.
    fn args(
        &mut self,
        args: &[SyntaxNode],
        default: Option<Ty>,
        out: &mut Vec<Stmt>,
    ) -> Option<Vec<Expr>> {
        let mut typed: Vec<Option<Expr>> = vec![None; args.len()];
        let mut ok = true;
        for (slot, node) in typed.iter_mut().zip(args) {
            if !untyped(node) {
                *slot = self.expr(node, None, out);
                ok &= slot.is_some();
            }
        }
        let pivot = typed
            .iter()
            .flatten()
            .map(|e| e.ty)
            .find(|t| t.lanes().is_some() || matches!(t, Ty::Matrix(_) | Ty::Color))
            .or(default);
        for (slot, node) in typed.iter_mut().zip(args) {
            if untyped(node) {
                *slot = self.expr(node, pivot, out);
                ok &= slot.is_some();
            }
        }
        if !ok {
            return None;
        }
        typed.into_iter().collect()
    }

    fn intrinsic(
        &mut self,
        intrinsic: Intrinsic,
        args: &[SyntaxNode],
        hint: Option<Ty>,
        at: TextRange,
        out: &mut Vec<Stmt>,
    ) -> Option<Expr> {
        if args.len() != intrinsic.arity() {
            self.error(
                "E2103",
                at,
                format!(
                    "`{}` takes {} arguments, not {}",
                    intrinsic.name(),
                    intrinsic.arity(),
                    args.len()
                ),
            );
            return None;
        }
        let default = match intrinsic {
            Intrinsic::QuadVertex => Some(Ty::U32),
            Intrinsic::Abs
            | Intrinsic::Sign
            | Intrinsic::Min
            | Intrinsic::Max
            | Intrinsic::Clamp => hint
                .or_else(|| {
                    args.iter()
                        .all(|a| int_literal(a).is_some())
                        .then_some(Ty::I32)
                })
                .or(Some(Ty::F32)),
            _ => Some(Ty::F32),
        };
        let values = self.args(args, default, out)?;
        let stage_ok = match intrinsic.stages() {
            StageUse::Any => true,
            StageUse::FragmentOnly => self.stage == Some(Stage::Fragment),
            StageUse::VertexOnly => self.stage == Some(Stage::Vertex),
        };
        if !stage_ok {
            let where_ = match intrinsic.stages() {
                StageUse::FragmentOnly => "the fragment entry",
                _ => "the vertex entry",
            };
            self.error(
                "E8106",
                at,
                format!("`{}` is available only in {where_}", intrinsic.name()),
            );
            return None;
        }
        let tys: Vec<Ty> = values.iter().map(|v| v.ty).collect();
        match intrinsic.result(&tys) {
            Some(ty) => Some(Expr::new(ty, ExprKind::Intrinsic(intrinsic, values))),
            None => {
                let spelled: Vec<String> = tys
                    .iter()
                    .map(|t| format!("`{}`", self.spell(*t)))
                    .collect();
                self.error(
                    "E2103",
                    at,
                    format!(
                        "`{}` does not take {}",
                        intrinsic.name(),
                        spelled.join(", ")
                    ),
                );
                None
            }
        }
    }

    fn construct(
        &mut self,
        ty: Ty,
        args: &[SyntaxNode],
        at: TextRange,
        out: &mut Vec<Stmt>,
    ) -> Option<Expr> {
        let scalar = match ty {
            Ty::Vector(s, _) => Ty::Scalar(s),
            Ty::Matrix(_) | Ty::Color => Ty::F32,
            _ => {
                let spelled = self.spell(ty);
                self.error(
                    "E8105",
                    at,
                    format!("a `{spelled}` is not constructed by a call"),
                );
                return None;
            }
        };
        let values = self.args(args, Some(scalar), out)?;
        let tys: Vec<Ty> = values.iter().map(|v| v.ty).collect();
        if self.sh.program.constructs(ty, &tys) {
            return Some(Expr::new(ty, ExprKind::Construct(values)));
        }
        let spelled = self.spell(ty);
        let parts: Vec<String> = tys
            .iter()
            .map(|t| format!("`{}`", self.spell(*t)))
            .collect();
        self.error(
            "E2103",
            at,
            format!("a `{spelled}` is not made of {}", parts.join(", ")),
        );
        None
    }

    fn record(&mut self, node: &SyntaxNode, out: &mut Vec<Stmt>) -> Option<Expr> {
        let at = node.text_range();
        let heads: Vec<SyntaxToken> = tokens(node)
            .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
            .collect();
        let head = heads.last()?;
        let ty = if head.text() == "VertexOutput" && heads.len() == 1 {
            Ty::VertexOutput
        } else {
            let symbol = match self.sh.env.scope.nominal.get(&head.text_range()) {
                Some(HostTy::Named(symbol)) => Some(*symbol),
                _ => None,
            };
            match symbol {
                Some(symbol) if self.sh.env.decls.shader_values.contains_key(&symbol) => {
                    self.sh.record(symbol)?
                }
                Some(_) => {
                    self.error(
                        "E8101",
                        head.text_range(),
                        format!("`{}` is not a `@shader_value` record", head.text()),
                    );
                    return None;
                }
                None => {
                    self.error(
                        "E2001",
                        head.text_range(),
                        format!("cannot find type `{}` in this shader", head.text()),
                    );
                    return None;
                }
            }
        };
        let fields: Vec<(String, Ty)> = self
            .sh
            .program
            .fields(ty)?
            .into_iter()
            .map(|(n, t)| (n.to_owned(), t))
            .collect();
        let mut given: Vec<Option<Expr>> = vec![None; fields.len()];
        let mut ok = true;
        for init in node
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::RecordExprField)
        {
            if has_token(&init, SyntaxKind::DotDot) {
                self.error(
                    "E8105",
                    init.text_range(),
                    "a shader record gives every field; there is no `..base`",
                );
                ok = false;
                continue;
            }
            let Some(name) = name_token(&init) else {
                continue;
            };
            let text = name.text();
            let Some(index) = fields.iter().position(|(n, _)| *n == text) else {
                let spelled = self.spell(ty);
                self.error(
                    "E2103",
                    name.text_range(),
                    format!("`{spelled}` has no field `{text}`"),
                );
                ok = false;
                continue;
            };
            if given[index].is_some() {
                self.error(
                    "E2103",
                    name.text_range(),
                    format!("field `{text}` is given twice"),
                );
                ok = false;
                continue;
            }
            let want = fields[index].1;
            let value = match exprs(&init).into_iter().next() {
                Some(value) => self.expect_value(&value, want, out),
                // `{ x }` reads `x`.
                None => {
                    let read = self.path_named(&text, name.text_range());
                    read.and_then(|e| self.coerce(e, want, name.text_range()))
                }
            };
            ok &= value.is_some();
            given[index] = value;
        }
        let missing: Vec<&str> = fields
            .iter()
            .zip(&given)
            .filter(|(_, g)| g.is_none())
            .map(|((n, _), _)| n.as_str())
            .collect();
        if ok && !missing.is_empty() {
            self.error(
                "E2103",
                at,
                format!("missing field `{}`", missing.join("`, `")),
            );
            return None;
        }
        let parts: Option<Vec<Expr>> = given.into_iter().collect();
        Some(Expr::new(ty, ExprKind::Construct(parts?)))
    }

    /// The value one name reads, for a record field shorthand.
    fn path_named(&mut self, name: &str, at: TextRange) -> Option<Expr> {
        if let Some(local) = self.lookup(name) {
            let ty = self.locals[local as usize].ty;
            return Some(Expr::new(ty, ExprKind::Local(local)));
        }
        let global = self.sh.globals.get(name).map(|(g, _)| *g);
        let p = &self.sh.program;
        match global {
            Some(_) if self.stage.is_none() => {
                self.error(
                    "E8106",
                    at,
                    format!("a shader `fn` reads only its parameters; pass `{name}` in"),
                );
                None
            }
            Some(Global::Uniform(i)) => {
                Some(Expr::new(p.uniforms[i as usize].ty, ExprKind::Uniform(i)))
            }
            Some(Global::Instance(i)) => {
                Some(Expr::new(p.instance[i as usize].ty, ExprKind::Instance(i)))
            }
            Some(Global::Varying(i)) => {
                Some(Expr::new(p.varyings[i as usize].ty, ExprKind::Varying(i)))
            }
            _ => {
                self.error("E2001", at, format!("cannot find `{name}` in this shader"));
                None
            }
        }
    }
}

/// The type an operand of `op` beside a `ty` takes its literals from.
fn operand_hint(op: BinaryOp, ty: Ty) -> Ty {
    match (op, ty) {
        (BinaryOp::Mul | BinaryOp::Div, Ty::Matrix(_)) => Ty::F32,
        (_, Ty::Color) => Ty::F32,
        _ => ty,
    }
}

/// The value of a non-negative integer literal expression.
fn int_literal(node: &SyntaxNode) -> Option<u32> {
    if node.kind() != SyntaxKind::LiteralExpr {
        return None;
    }
    let tok = tokens(node).next()?;
    let text = tok.text();
    let body = match tok.kind() {
        SyntaxKind::IntLiteral => text.as_str(),
        SyntaxKind::UnitLiteral => match split_unit_literal(&text) {
            Some((body, HostTy::I32 | HostTy::U32)) => body,
            _ => return None,
        },
        _ => return None,
    };
    parse_int_literal(body).and_then(|v| u32::try_from(v).ok())
}
