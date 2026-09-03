//! Expression type inference.
//!
//! This walks a resolved expression tree and assigns each expression a [`Ty`],
//! emitting the numeric diagnostics (`E2101`/`E2102`/`E2103`) as it goes. It is the
//! piece that discharges the doc's "no untyped numeric literal after HIR" contract:
//! an integer/float literal is not a runtime type until inference pins it, either
//! from an expected type flowing down or, failing that, from the host default
//! (`1` -> `I64`, `1.0` -> `F64`).
//!
//! The rules implemented here:
//!
//! - **Literal typing.** With an expected numeric type, a literal *instantiates* at
//!   that type directly (a float literal at `F32` is a normal `F32` instantiation, not
//!   an `F64 -> F32` narrowing), after a range/precision check. Without an expected
//!   type, a numeric literal takes the host default.
//! - **Numeric widening.** Where a value of one numeric type meets an expected type,
//!   the only implicit moves allowed are the safe widenings ([`Ty::check_implicit_widen`]);
//!   anything else is `E2102`. A non-numeric mismatch is `E2103`.
//! - **Unification.** `if`/`match` result types unify their branch types (equal, or a
//!   single common widening target); a non-unifiable set is `E2103` on the expression.
//!
//! Inference is deliberately decoupled from the HIR node types (built in a later
//! section) through the [`TypeEnv`] trait: everything inference needs to know about
//! *what a resolved name's type is* and *what a callee's signature is* is answered by
//! the environment, so this module is testable against a stub environment without the
//! component-lowering machinery existing yet.

use std::collections::HashMap;

use crate::ast::{AstNode, CallExpr, CastExpr, Expr, FieldExpr, PathExpr, TypePath};
use crate::diag::Diagnostic;
use crate::hir::ty::{Ty, TypeError, WidenError};
use crate::resolve::{Resolution, ResolvedRef};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

/// What inference needs to know about the surrounding program: the type of a resolved
/// name and the signature of a callable it resolves to. Supplying this as a trait keeps
/// inference independent of the HIR node types (which a later section builds) — the
/// component-lowering layer implements it over the real symbol/HIR tables, and tests
/// implement it over a stub.
pub trait TypeEnv {
    /// The type of a resolved name use (a `let`/param local, or a symbol — a `state`,
    /// `input`, `computed`, `const`, or record/enum type). `None` when the environment
    /// has no type for it (inference records `Ty::Unknown` and moves on; the missing
    /// binding is a resolver-level fact, not typed here).
    fn resolution_ty(&self, to: &Resolution) -> Option<Ty>;

    /// The signature `(params, ret)` of a callable a name resolves to, when the name is
    /// used in callee position. `None` if the resolution is not a callable (a plain
    /// value call is then an `E2103`).
    fn callee_signature(&self, to: &Resolution) -> Option<(Vec<Ty>, Ty)>;
}

/// The inference context for one expression walk: the resolved-reference index (so a
/// path use can be looked up by the span of its head segment), the environment, and the
/// diagnostics sink.
pub struct InferCx<'a> {
    /// Every resolved name use keyed by its token span, so a `PathExpr` head resolves in
    /// O(1).
    refs: HashMap<TextRange, Resolution>,
    /// The surrounding-program type oracle.
    env: &'a dyn TypeEnv,
    /// Diagnostics accumulated during the walk.
    diagnostics: Vec<Diagnostic>,
}

impl<'a> InferCx<'a> {
    /// Builds a context from a module's resolved references and a type environment.
    pub fn new(refs: &[ResolvedRef], env: &'a dyn TypeEnv) -> Self {
        let mut index = HashMap::with_capacity(refs.len());
        for r in refs {
            index.insert(r.range, r.to);
        }
        InferCx {
            refs: index,
            env,
            diagnostics: Vec::new(),
        }
    }

    /// Consumes the context, returning the diagnostics it gathered.
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    /// Borrows the diagnostics gathered so far (for a caller that keeps inferring).
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Infers the type of `expr`, using `expected` to type numeric literals and to check
    /// implicit conversions where the surrounding context demands a specific type. When
    /// `expected` is `None`, numeric literals take the host default and no conversion is
    /// checked here (the caller checks against its own expectation, if any).
    pub fn infer_expr(&mut self, expr: &Expr, expected: Option<&Ty>) -> Ty {
        let node = expr.syntax();
        match node.kind() {
            SyntaxKind::LiteralExpr => self.infer_literal(node, expected),
            SyntaxKind::PathExpr => self.check_against(self.infer_path(node), expected, node),
            SyntaxKind::FieldExpr => self.infer_field(node, expected),
            SyntaxKind::CallExpr => self.infer_call(node),
            SyntaxKind::BinaryExpr => self.infer_binary(node, expected),
            SyntaxKind::UnaryExpr => self.infer_unary(node, expected),
            SyntaxKind::CastExpr => self.infer_cast(node),
            SyntaxKind::ParenExpr => match first_child_expr(node) {
                Some(inner) => self.infer_expr(&inner, expected),
                None => Ty::Unknown,
            },
            SyntaxKind::TupleExpr => self.infer_tuple(node, expected),
            SyntaxKind::ListExpr => self.infer_list(node, expected),
            SyntaxKind::IfExpr => self.infer_if(node, expected),
            SyntaxKind::MatchExpr => self.infer_match(node, expected),
            // Advanced / not-yet-inferred forms lower to a placeholder this slice
            // (index/range/try/optional-field/record/closure); they resolve when their
            // consumer lands. They do not participate in numeric typing, so no diagnostic.
            _ => Ty::Unknown,
        }
    }

    /// Types a numeric/scalar literal. A numeric literal instantiates at `expected` when
    /// one is given (after range/precision), else takes the host default; non-numeric
    /// literals (bool/char/string/color) have a fixed type checked against `expected`.
    fn infer_literal(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let Some(tok) = node
            .children_with_tokens()
            .into_iter()
            .find_map(|e| e.as_token().cloned())
        else {
            return Ty::Unknown;
        };
        let range = node.text_range();
        match tok.kind() {
            SyntaxKind::IntLiteral => self.type_int_literal(&tok.text(), expected, range),
            SyntaxKind::FloatLiteral => self.type_float_literal(&tok.text(), expected, range),
            SyntaxKind::TrueKw | SyntaxKind::FalseKw => {
                self.check_against(Ty::Bool, expected, node)
            }
            SyntaxKind::CharLiteral => self.check_against(Ty::Char, expected, node),
            SyntaxKind::StringLiteral | SyntaxKind::RawStringLiteral => {
                self.check_against(Ty::String, expected, node)
            }
            SyntaxKind::ColorLiteral => self.check_against(Ty::Color, expected, node),
            // A unit literal (`10dp`, `50%`) carries its dimension in the suffix; the
            // dimensional-suffix table is a later concern. Type it against the expected
            // dimension when one is given, else leave it undetermined.
            SyntaxKind::UnitLiteral => expected.cloned().unwrap_or(Ty::Unknown),
            _ => Ty::Unknown,
        }
    }

    /// Types an integer literal. With an integer `expected` type it instantiates there
    /// after a range check; with a float `expected` it is a mismatch (an int literal is
    /// not a float source implicitly); with no context it is the host default `I64`.
    fn type_int_literal(&mut self, text: &str, expected: Option<&Ty>, range: TextRange) -> Ty {
        let value = parse_int_literal(text);
        match expected {
            Some(target) if is_integer_ty(target) => {
                if let Some(v) = value
                    && !int_fits(v, target)
                {
                    self.diagnostics.push(Diagnostic::error(
                        "E2103",
                        range,
                        format!("integer literal out of range for `{}`", ty_name(target)),
                    ));
                }
                target.clone()
            }
            Some(target) if is_float_ty(target) => {
                // An integer literal in a float slot is a legal literal instantiation
                // (`1` where `F32` is expected is the float one), per the doc's
                // "instantiate the literal, do not convert" rule.
                target.clone()
            }
            Some(target) => {
                self.emit_mismatch(&Ty::I64, target, range);
                target.clone()
            }
            None => Ty::I64,
        }
    }

    /// Types a float literal. With a float `expected` it instantiates there (checking the
    /// value is representable); an integer or other `expected` is a mismatch; no context
    /// gives the host default `F64`.
    fn type_float_literal(&mut self, text: &str, expected: Option<&Ty>, range: TextRange) -> Ty {
        match expected {
            Some(target) if is_float_ty(target) => {
                // Instantiation, not conversion: reject only a value that has no
                // finite `f32` representation at all.
                if target == &Ty::F32
                    && let Some(v) = parse_float_literal(text)
                    && v.is_finite()
                    && (v as f32).is_infinite()
                {
                    self.diagnostics.push(Diagnostic::error(
                        "E2103",
                        range,
                        "float literal out of range for `F32`",
                    ));
                }
                target.clone()
            }
            Some(target) if is_integer_ty(target) => {
                self.emit_mismatch(&Ty::F64, target, range);
                target.clone()
            }
            Some(target) => {
                self.emit_mismatch(&Ty::F64, target, range);
                target.clone()
            }
            None => Ty::F64,
        }
    }

    /// Resolves a `PathExpr` to a type via the refs index and the environment.
    fn infer_path(&self, node: &SyntaxNode) -> Ty {
        let Some(path) = PathExpr::cast(node.clone()) else {
            return Ty::Unknown;
        };
        let Some(head) = path.segments().next() else {
            return Ty::Unknown;
        };
        match self.refs.get(&head.text_range()) {
            Some(to) => self.env.resolution_ty(to).unwrap_or(Ty::Unknown),
            None => Ty::Unknown,
        }
    }

    /// Types a field access. Field-type resolution needs the record/component schema the
    /// receiver's type names, which a later section supplies through the environment; for
    /// now the receiver is inferred (so its own diagnostics fire) and the field yields a
    /// placeholder checked against `expected` only when trivially known.
    fn infer_field(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        if let Some(field) = FieldExpr::cast(node.clone())
            && let Some(recv) = field.receiver()
        {
            let _ = self.infer_expr(&recv, None);
        }
        // Field-type lookup is a schema query deferred to component lowering; the
        // placeholder is intentionally undetermined here and does not force `expected`.
        let _ = expected;
        Ty::Unknown
    }

    /// Types a call: resolves the callee's signature, then checks each argument against
    /// its parameter type (widening allowed, `E2102`/`E2103` otherwise) and returns the
    /// declared return type.
    fn infer_call(&mut self, node: &SyntaxNode) -> Ty {
        let Some(call) = CallExpr::cast(node.clone()) else {
            return Ty::Unknown;
        };
        let sig = call.callee().and_then(|callee| {
            let callee_node = callee.syntax();
            if callee_node.kind() == SyntaxKind::PathExpr
                && let Some(head) =
                    PathExpr::cast(callee_node.clone()).and_then(|p| p.segments().next())
                && let Some(to) = self.refs.get(&head.text_range())
            {
                return self.env.callee_signature(to);
            }
            None
        });

        let args = call_args(node);
        match sig {
            Some((params, ret)) => {
                for (i, arg) in args.iter().enumerate() {
                    let expected = params.get(i);
                    let _ = self.infer_expr(arg, expected);
                }
                ret
            }
            None => {
                // Unknown callee signature: still infer arguments so their own
                // diagnostics fire, then leave the result undetermined.
                for arg in &args {
                    let _ = self.infer_expr(arg, None);
                }
                Ty::Unknown
            }
        }
    }

    /// Types a binary expression. Comparison/logical operators yield `Bool`; arithmetic
    /// and bitwise operators yield the operands' common numeric type (widened to fit),
    /// with `E2102` on an illegal implicit mix.
    fn infer_binary(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let mut operands = child_exprs(node);
        let rhs = operands.pop();
        let lhs = operands.pop();
        let op = binary_op_kind(node);

        let lty = lhs
            .map(|e| self.infer_expr(&e, None))
            .unwrap_or(Ty::Unknown);
        // Comparisons/logicals produce Bool regardless of numeric widening direction.
        if matches!(
            op,
            Some(
                SyntaxKind::EqEq
                    | SyntaxKind::Neq
                    | SyntaxKind::Lt
                    | SyntaxKind::Le
                    | SyntaxKind::Gt
                    | SyntaxKind::Ge
                    | SyntaxKind::AmpAmp
                    | SyntaxKind::PipePipe
            )
        ) {
            if let Some(r) = rhs {
                let _ = self.infer_expr(&r, None);
            }
            return self.check_against(Ty::Bool, expected, node);
        }

        // Arithmetic/bitwise: the right operand types against the left (so a literal on
        // one side instantiates at the other's type where possible).
        let rty = match rhs {
            Some(r) => {
                let want = if lty == Ty::Unknown { None } else { Some(&lty) };
                self.infer_expr(&r, want)
            }
            None => Ty::Unknown,
        };
        let result = unify_numeric(&lty, &rty).unwrap_or_else(|| {
            // A non-widenable numeric mix is an illegal implicit conversion.
            if is_numeric_ty(&lty) && is_numeric_ty(&rty) && lty != rty {
                self.diagnostics.push(Diagnostic::error(
                    "E2102",
                    node.text_range(),
                    "operands have incompatible numeric types; an explicit cast is required",
                ));
            }
            if lty == Ty::Unknown {
                rty.clone()
            } else {
                lty.clone()
            }
        });
        self.check_against(result, expected, node)
    }

    /// Types a unary expression: `!` on `Bool` -> `Bool`; `-`/`~` preserve the operand's
    /// numeric type.
    fn infer_unary(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let op = unary_op_kind(node);
        let operand = first_child_expr(node);
        let inner = match &operand {
            Some(e) => {
                let want = if op == Some(SyntaxKind::Bang) {
                    Some(&Ty::Bool)
                } else {
                    expected
                };
                self.infer_expr(e, want)
            }
            None => Ty::Unknown,
        };
        let result = match op {
            Some(SyntaxKind::Bang) => Ty::Bool,
            _ => inner,
        };
        self.check_against(result, expected, node)
    }

    /// Types an `expr as Type` cast. The target type is the result; an unknown target
    /// name is `E2103` and a `Float` target is `E2101`. A cast is the explicit escape
    /// hatch, so the operand's own type is inferred but not conversion-checked here.
    fn infer_cast(&mut self, node: &SyntaxNode) -> Ty {
        let Some(cast) = CastExpr::cast(node.clone()) else {
            return Ty::Unknown;
        };
        if let Some(op) = cast.operand() {
            let _ = self.infer_expr(&op, None);
        }
        match cast.ty() {
            Some(tp) => self.resolve_annotation(&tp, node.text_range()),
            None => Ty::Unknown,
        }
    }

    /// Types a tuple expression element-wise. An `expected` tuple type distributes over
    /// the elements; otherwise elements are inferred without context.
    fn infer_tuple(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let elems = child_exprs(node);
        let expected_elems = match expected {
            Some(Ty::Tuple(tys)) if tys.len() == elems.len() => Some(tys),
            _ => None,
        };
        let mut tys = Vec::with_capacity(elems.len());
        for (i, e) in elems.iter().enumerate() {
            let want = expected_elems.and_then(|es| es.get(i));
            tys.push(self.infer_expr(e, want));
        }
        Ty::Tuple(tys)
    }

    /// Types a list expression. An `expected` `List<T>` flows `T` into every element and
    /// is the result; otherwise the element type is unified across the elements.
    fn infer_list(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let elems = child_exprs(node);
        let expected_elem = match expected {
            Some(Ty::List(inner)) => Some(inner.as_ref()),
            _ => None,
        };
        let mut elem_ty: Option<Ty> = expected_elem.cloned();
        for e in &elems {
            let t = self.infer_expr(e, elem_ty.as_ref().or(expected_elem));
            elem_ty = match elem_ty {
                None => Some(t),
                Some(prev) => Some(unify_numeric(&prev, &t).unwrap_or(prev)),
            };
        }
        Ty::List(Box::new(elem_ty.unwrap_or(Ty::Unknown)))
    }

    /// Types an `if`/`else` expression: the condition types against `Bool`, and the
    /// branch result types unify (`E2103` if they cannot).
    fn infer_if(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        // An IfExpr's children are: condition Expr, then Block, optional else (Block or
        // IfExpr). We type the condition against Bool and unify the block tail types.
        let mut branch_tys = Vec::new();
        let mut saw_cond = false;
        for child in node.children() {
            match child.kind() {
                k if Expr::can_cast(k) && !saw_cond => {
                    saw_cond = true;
                    if let Some(cond) = Expr::cast(child) {
                        let _ = self.infer_expr(&cond, Some(&Ty::Bool));
                    }
                }
                SyntaxKind::Block => {
                    branch_tys.push(self.infer_block_tail(&child, expected));
                }
                k if Expr::can_cast(k) => {
                    // else-if chain.
                    if let Some(inner) = Expr::cast(child) {
                        branch_tys.push(self.infer_expr(&inner, expected));
                    }
                }
                _ => {}
            }
        }
        self.unify_branches(&branch_tys, expected, node)
    }

    /// Types a `match` expression: the arm result types unify (`E2103` otherwise). The
    /// scrutinee is inferred for its own diagnostics.
    fn infer_match(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        if let Some(scrut) = first_child_expr(node) {
            let _ = self.infer_expr(&scrut, None);
        }
        let mut arm_tys = Vec::new();
        for arm in node.children() {
            if arm.kind() == SyntaxKind::MatchArm {
                // An arm's value is its trailing Block or Expr (after the pattern and an
                // optional guard). Take the last child that is a block/expr.
                if let Some(block) = arm
                    .children()
                    .into_iter()
                    .rev()
                    .find(|c| c.kind() == SyntaxKind::Block)
                {
                    arm_tys.push(self.infer_block_tail(&block, expected));
                } else if let Some(value) = arm.children().into_iter().rev().find_map(Expr::cast) {
                    arm_tys.push(self.infer_expr(&value, expected));
                }
            }
        }
        self.unify_branches(&arm_tys, expected, node)
    }

    /// The type a block evaluates to: the type of its trailing tail expression, or `Unit`
    /// if it ends in a statement. Statements are not re-inferred here beyond the tail
    /// (statement-level inference is a component-lowering concern); this keeps `if`/`match`
    /// result unification working on the common expression-bodied-block shape.
    fn infer_block_tail(&mut self, block: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        // Every statement is wrapped: the parser always folds a trailing expression into
        // an `ExprStmt` (a missing `;` is a recovered diagnostic, not a distinct node). So
        // the block's value is the `Expr` inside its last `ExprStmt`.
        match block
            .children()
            .into_iter()
            .rev()
            .find(|c| c.kind() == SyntaxKind::ExprStmt)
        {
            Some(stmt) => match first_child_expr(&stmt) {
                Some(tail) => self.infer_expr(&tail, expected),
                None => Ty::Unit,
            },
            None => Ty::Unit,
        }
    }

    /// Unifies branch result types into one type, emitting `E2103` on the given node when
    /// they are incompatible. An empty set (no branches) is `Unit`.
    fn unify_branches(&mut self, tys: &[Ty], expected: Option<&Ty>, node: &SyntaxNode) -> Ty {
        let mut acc: Option<Ty> = expected.cloned();
        for t in tys {
            acc = match acc {
                None => Some(t.clone()),
                Some(prev) => match unify_numeric(&prev, t) {
                    Some(u) => Some(u),
                    None if &prev == t => Some(prev),
                    None => {
                        self.diagnostics.push(Diagnostic::error(
                            "E2103",
                            node.text_range(),
                            "incompatible types across branches",
                        ));
                        Some(prev)
                    }
                },
            };
        }
        acc.unwrap_or(Ty::Unit)
    }

    /// Resolves a type annotation to a [`Ty`], emitting `E2101` for `Float` and `E2103`
    /// for an unknown builtin name. A nominal (non-builtin) name is left as `Unknown`
    /// here — nominal type resolution is a symbol-table query the environment owns.
    fn resolve_annotation(&mut self, path: &TypePath, range: TextRange) -> Ty {
        match Ty::from_type_path(path) {
            Ok(Some(ty)) => ty,
            Ok(None) => Ty::Unknown,
            Err(err) => {
                self.diagnostics
                    .push(Diagnostic::error(err.code(), range, err.message()));
                match err {
                    TypeError::FloatRemoved | TypeError::UnknownType => Ty::Unknown,
                }
            }
        }
    }

    /// Checks a produced type against an expected type: identical or a legal widening is
    /// accepted (returning the *expected* type so it flows outward), else the appropriate
    /// diagnostic (`E2102` for an illegal numeric widening, `E2103` for any other
    /// mismatch) is emitted and the expected type is returned to bound error cascades.
    fn check_against(&mut self, produced: Ty, expected: Option<&Ty>, node: &SyntaxNode) -> Ty {
        let Some(target) = expected else {
            return produced;
        };
        if &produced == target || produced == Ty::Unknown || target == &Ty::Unknown {
            return if produced == Ty::Unknown {
                target.clone()
            } else {
                produced
            };
        }
        if is_numeric_ty(&produced) && is_numeric_ty(target) {
            match produced.check_implicit_widen(target) {
                Ok(()) => target.clone(),
                Err(WidenError::IllegalImplicit) => {
                    self.diagnostics.push(Diagnostic::error(
                        "E2102",
                        node.text_range(),
                        format!(
                            "illegal implicit conversion from `{}` to `{}`; an explicit cast is required",
                            ty_name(&produced),
                            ty_name(target)
                        ),
                    ));
                    target.clone()
                }
            }
        } else {
            self.emit_mismatch(&produced, target, node.text_range());
            target.clone()
        }
    }

    /// Emits an `E2103` type mismatch.
    fn emit_mismatch(&mut self, produced: &Ty, target: &Ty, range: TextRange) {
        self.diagnostics.push(Diagnostic::error(
            "E2103",
            range,
            format!(
                "type mismatch: expected `{}`, found `{}`",
                ty_name(target),
                ty_name(produced)
            ),
        ));
    }
}

// --- free helpers ------------------------------------------------------------

/// Whether `ty` is one of the integer scalar types.
fn is_integer_ty(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64
    )
}

/// Whether `ty` is one of the float scalar types.
fn is_float_ty(ty: &Ty) -> bool {
    matches!(ty, Ty::F32 | Ty::F64)
}

/// Whether `ty` is any numeric scalar (integer or float).
fn is_numeric_ty(ty: &Ty) -> bool {
    is_integer_ty(ty) || is_float_ty(ty)
}

/// The common numeric type of two operand types: the one the other widens to, if either
/// direction is a legal safe widening; `None` when they are not numerically unifiable.
fn unify_numeric(a: &Ty, b: &Ty) -> Option<Ty> {
    if a == b {
        return Some(a.clone());
    }
    if a == &Ty::Unknown {
        return Some(b.clone());
    }
    if b == &Ty::Unknown {
        return Some(a.clone());
    }
    if a.widens_to(b) {
        Some(b.clone())
    } else if b.widens_to(a) {
        Some(a.clone())
    } else {
        None
    }
}

/// The inclusive integer range representable by an integer scalar type, as `i128` so both
/// signed and unsigned widths fit.
fn int_fits(value: i128, ty: &Ty) -> bool {
    let (lo, hi): (i128, i128) = match ty {
        Ty::I8 => (i8::MIN as i128, i8::MAX as i128),
        Ty::I16 => (i16::MIN as i128, i16::MAX as i128),
        Ty::I32 => (i32::MIN as i128, i32::MAX as i128),
        Ty::I64 => (i64::MIN as i128, i64::MAX as i128),
        Ty::U8 => (0, u8::MAX as i128),
        Ty::U16 => (0, u16::MAX as i128),
        Ty::U32 => (0, u32::MAX as i128),
        Ty::U64 => (0, u64::MAX as i128),
        _ => return true,
    };
    value >= lo && value <= hi
}

/// Parses an integer literal's decimal/hex/octal/binary digits into an `i128`, stripping a
/// trailing type suffix and `_` separators. `None` if it does not fit `i128` (a value that
/// large is out of range for every scalar anyway).
fn parse_int_literal(text: &str) -> Option<i128> {
    // Strip a type suffix: digits/`0x`.. body then optional `I32`/`u8`/... — split at the
    // first ASCII letter that is not part of a radix prefix.
    let body = strip_int_suffix(text);
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    let (radix, digits) = if let Some(rest) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        (16, rest)
    } else if let Some(rest) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        (8, rest)
    } else if let Some(rest) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        (2, rest)
    } else {
        (10, cleaned.as_str())
    };
    i128::from_str_radix(digits, radix).ok()
}

/// Splits an integer literal's numeric body from a trailing type suffix (`100i32`).
fn strip_int_suffix(text: &str) -> &str {
    // A suffix begins at the first letter after the digits that is not a radix marker in
    // position 1 (`x`/`o`/`b` right after a leading `0`).
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        let is_radix_marker = i == 1
            && bytes.first() == Some(&b'0')
            && matches!(c, 'x' | 'X' | 'o' | 'O' | 'b' | 'B');
        if c.is_ascii_alphabetic() && !is_radix_marker && c != '_' {
            // For hex, digits a–f are part of the number; only stop at a non-hex letter
            // when we are in hex. Simplify: a suffix always starts with `i`/`u`/`f`.
            if matches!(c, 'i' | 'u' | 'f' | 'I' | 'U' | 'F') && i >= 1 {
                return &text[..i];
            }
        }
        i += 1;
    }
    text
}

/// Parses a float literal's body into an `f64`, stripping a type suffix and `_`.
fn parse_float_literal(text: &str) -> Option<f64> {
    let body = match text.find(['f', 'F']) {
        Some(idx) if idx > 0 => &text[..idx],
        _ => text,
    };
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().ok()
}

/// A one-word name for a type, for diagnostic messages.
fn ty_name(ty: &Ty) -> &'static str {
    match ty {
        Ty::Bool => "Bool",
        Ty::I8 => "I8",
        Ty::I16 => "I16",
        Ty::I32 => "I32",
        Ty::I64 => "I64",
        Ty::U8 => "U8",
        Ty::U16 => "U16",
        Ty::U32 => "U32",
        Ty::U64 => "U64",
        Ty::F32 => "F32",
        Ty::F64 => "F64",
        Ty::Char => "Char",
        Ty::String => "String",
        Ty::Bytes => "Bytes",
        Ty::Unit => "Unit",
        Ty::Never => "Never",
        Ty::Color => "Color",
        Ty::Dp => "Dp",
        Ty::Px => "Px",
        Ty::Sp => "Sp",
        Ty::Percent => "Percent",
        Ty::Duration => "Duration",
        Ty::Angle => "Angle",
        Ty::Frequency => "Frequency",
        Ty::Named(_) => "<named>",
        Ty::Tuple(_) => "<tuple>",
        Ty::Fn(_, _) => "<fn>",
        Ty::List(_) => "<list>",
        Ty::Option(_) => "<option>",
        Ty::InferInt => "<int>",
        Ty::InferFloat => "<float>",
        Ty::Unknown => "<unknown>",
    }
}

/// The direct `Expr` children of a node, in order.
fn child_exprs(node: &SyntaxNode) -> Vec<Expr> {
    node.children().into_iter().filter_map(Expr::cast).collect()
}

/// The first direct `Expr` child of a node.
fn first_child_expr(node: &SyntaxNode) -> Option<Expr> {
    node.children().into_iter().find_map(Expr::cast)
}

/// The argument expressions of a call. Arguments live in the call's `ArgumentList`
/// child, each wrapped in an `Argument` node (which, for a named argument, holds a
/// leading `ident :` before the value expression); we project each `Argument`'s value
/// expression in order.
fn call_args(node: &SyntaxNode) -> Vec<Expr> {
    let mut args = Vec::new();
    for child in node.children() {
        if child.kind() == SyntaxKind::ArgumentList {
            for arg in child.children() {
                if arg.kind() == SyntaxKind::Argument
                    && let Some(e) = first_child_expr(&arg)
                {
                    args.push(e);
                }
            }
        }
    }
    args
}

/// The operator token kind of a binary expression (the operator token between operands).
fn binary_op_kind(node: &SyntaxNode) -> Option<SyntaxKind> {
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().map(|t| t.kind()))
        .find(|k| is_binary_op(*k))
}

/// The operator token kind of a unary expression.
fn unary_op_kind(node: &SyntaxNode) -> Option<SyntaxKind> {
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().map(|t| t.kind()))
        .find(|k| matches!(k, SyntaxKind::Minus | SyntaxKind::Bang | SyntaxKind::Tilde))
}

/// Whether a token kind is a binary operator.
fn is_binary_op(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Star
            | SyntaxKind::Slash
            | SyntaxKind::Percent
            | SyntaxKind::Amp
            | SyntaxKind::Pipe
            | SyntaxKind::Caret
            | SyntaxKind::Shl
            | SyntaxKind::Shr
            | SyntaxKind::EqEq
            | SyntaxKind::Neq
            | SyntaxKind::Lt
            | SyntaxKind::Le
            | SyntaxKind::Gt
            | SyntaxKind::Ge
            | SyntaxKind::AmpAmp
            | SyntaxKind::PipePipe
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::SymbolId;
    use crate::syntax::{SyntaxNode, tokenize};
    use std::collections::HashMap;

    /// A stub environment: it answers `resolution_ty` / `callee_signature` from two
    /// maps keyed by `SymbolId`, so inference can be exercised without the real
    /// symbol/HIR tables. Path tests wire a name's head-token span to a `SymbolId`
    /// through the `ResolvedRef` index, and register that id's type/signature here.
    #[derive(Default)]
    struct StubEnv {
        tys: HashMap<SymbolId, Ty>,
        sigs: HashMap<SymbolId, (Vec<Ty>, Ty)>,
    }

    impl TypeEnv for StubEnv {
        fn resolution_ty(&self, to: &Resolution) -> Option<Ty> {
            match to {
                Resolution::Symbol(id) => self.tys.get(id).cloned(),
                Resolution::Local(_) => None,
            }
        }

        fn callee_signature(&self, to: &Resolution) -> Option<(Vec<Ty>, Ty)> {
            match to {
                Resolution::Symbol(id) => self.sigs.get(id).cloned(),
                Resolution::Local(_) => None,
            }
        }
    }

    /// Parses a bare expression fragment and returns its typed `Expr` view plus the
    /// syntax root (kept alive for the borrow). The fragment entry roots at an
    /// `ExprStmt` whose sole child expression is the one under test.
    fn parse_fragment(src: &str) -> (SyntaxNode, Expr) {
        let parse = crate::syntax::grammar::parse_expr(&tokenize(src), src);
        let root = SyntaxNode::new_root(parse.root);
        // The `Expr` entry roots at `ExprStmt`; the value is its first `Expr` child.
        let expr = root
            .descendants()
            .into_iter()
            .find_map(Expr::cast)
            .expect("fragment parses to an expression");
        (root, expr)
    }

    /// The span of the first identifier token whose text equals `name`, used to wire a
    /// path use to a stub resolution.
    fn ident_range(root: &SyntaxNode, name: &str) -> TextRange {
        root.descendants_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .find(|t| t.kind() == SyntaxKind::Ident && t.text() == name)
            .map(|t| t.text_range())
            .unwrap_or_else(|| panic!("no identifier token `{name}`"))
    }

    /// Infers a fragment with no environment bindings and no expected type.
    fn infer_bare(src: &str) -> (Ty, Vec<Diagnostic>) {
        let (_root, expr) = parse_fragment(src);
        let env = StubEnv::default();
        let mut cx = InferCx::new(&[], &env);
        let ty = cx.infer_expr(&expr, None);
        (ty, cx.into_diagnostics())
    }

    /// Infers a fragment against an expected type, with no environment bindings.
    fn infer_expecting(src: &str, expected: &Ty) -> (Ty, Vec<Diagnostic>) {
        let (_root, expr) = parse_fragment(src);
        let env = StubEnv::default();
        let mut cx = InferCx::new(&[], &env);
        let ty = cx.infer_expr(&expr, Some(expected));
        (ty, cx.into_diagnostics())
    }

    fn codes(diags: &[Diagnostic]) -> Vec<&str> {
        diags.iter().map(|d| d.code).collect()
    }

    // --- literal typing (host default vs expected) --------------------------

    #[test]
    fn integer_literal_defaults_to_i64_without_context() {
        let (ty, diags) = infer_bare("1");
        assert_eq!(ty, Ty::I64);
        assert!(diags.is_empty());
    }

    #[test]
    fn float_literal_defaults_to_f64_without_context() {
        let (ty, diags) = infer_bare("1.0");
        assert_eq!(ty, Ty::F64);
        assert!(diags.is_empty());
    }

    #[test]
    fn integer_literal_instantiates_at_expected_int() {
        let (ty, diags) = infer_expecting("1", &Ty::U8);
        assert_eq!(ty, Ty::U8);
        assert!(diags.is_empty());
    }

    #[test]
    fn integer_literal_instantiates_at_expected_float() {
        // `1` where `F32` is expected is a legal literal instantiation, not a
        // conversion — no diagnostic.
        let (ty, diags) = infer_expecting("1", &Ty::F32);
        assert_eq!(ty, Ty::F32);
        assert!(diags.is_empty());
    }

    #[test]
    fn integer_literal_out_of_range_is_e2103() {
        let (ty, diags) = infer_expecting("300", &Ty::U8);
        assert_eq!(ty, Ty::U8);
        assert_eq!(codes(&diags), ["E2103"]);
    }

    #[test]
    fn float_literal_instantiates_at_expected_f32() {
        let (ty, diags) = infer_expecting("1.5", &Ty::F32);
        assert_eq!(ty, Ty::F32);
        assert!(diags.is_empty());
    }

    #[test]
    fn float_literal_in_integer_slot_is_e2103() {
        let (ty, diags) = infer_expecting("1.5", &Ty::I32);
        assert_eq!(ty, Ty::I32);
        assert_eq!(codes(&diags), ["E2103"]);
    }

    #[test]
    fn bool_and_string_literals_type_directly() {
        assert_eq!(infer_bare("true").0, Ty::Bool);
        assert_eq!(infer_bare("\"x\"").0, Ty::String);
    }

    // --- widening / conversion ---------------------------------------------

    #[test]
    fn safe_widening_is_accepted() {
        // An `I8`-typed name widens to an `I64` slot without a diagnostic.
        let (root, expr) = parse_fragment("small");
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(1, 0);
        env.tys.insert(id, Ty::I8);
        let refs = [ResolvedRef {
            range: ident_range(&root, "small"),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, Some(&Ty::I64));
        assert_eq!(ty, Ty::I64);
        assert!(cx.into_diagnostics().is_empty());
    }

    #[test]
    fn illegal_implicit_conversion_is_e2102() {
        // A `U32`-typed name in an `I64` slot crosses the signed/unsigned boundary.
        let (root, expr) = parse_fragment("n");
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(2, 0);
        env.tys.insert(id, Ty::U32);
        let refs = [ResolvedRef {
            range: ident_range(&root, "n"),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, Some(&Ty::I64));
        assert_eq!(ty, Ty::I64);
        assert_eq!(codes(cx.diagnostics()), ["E2102"]);
    }

    #[test]
    fn non_numeric_mismatch_is_e2103() {
        let (root, expr) = parse_fragment("flag");
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(3, 0);
        env.tys.insert(id, Ty::Bool);
        let refs = [ResolvedRef {
            range: ident_range(&root, "flag"),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, Some(&Ty::String));
        assert_eq!(ty, Ty::String);
        assert_eq!(codes(cx.diagnostics()), ["E2103"]);
    }

    // --- annotations --------------------------------------------------------

    #[test]
    fn float_annotation_on_cast_is_e2101() {
        let (ty, diags) = infer_bare("1 as Float");
        assert_eq!(ty, Ty::Unknown);
        assert_eq!(codes(&diags), ["E2101"]);
    }

    #[test]
    fn cast_target_is_the_result_type() {
        let (ty, diags) = infer_bare("x as I32");
        assert_eq!(ty, Ty::I32);
        assert!(diags.is_empty());
    }

    // --- operators ----------------------------------------------------------

    #[test]
    fn comparison_yields_bool() {
        assert_eq!(infer_bare("1 < 2").0, Ty::Bool);
        assert_eq!(infer_bare("true && false").0, Ty::Bool);
    }

    #[test]
    fn arithmetic_unifies_operand_widths() {
        // Two default-int literals stay `I64`.
        assert_eq!(infer_bare("1 + 2").0, Ty::I64);
    }

    #[test]
    fn logical_not_yields_bool() {
        assert_eq!(infer_bare("!true").0, Ty::Bool);
    }

    // --- if / match unification --------------------------------------------

    #[test]
    fn if_branches_unify_to_common_type() {
        let (ty, diags) = infer_bare("if true { 1 } else { 2 }");
        assert_eq!(ty, Ty::I64);
        assert!(diags.is_empty());
    }

    #[test]
    fn if_branches_of_incompatible_types_are_e2103() {
        let (_ty, diags) = infer_bare("if true { 1 } else { \"x\" }");
        assert!(codes(&diags).contains(&"E2103"));
    }

    // --- calls --------------------------------------------------------------

    #[test]
    fn call_returns_declared_return_and_checks_args() {
        // `f(1)` with signature `(I32) -> Bool`: the literal instantiates at `I32`,
        // the call yields `Bool`, and no diagnostic fires.
        let (root, expr) = parse_fragment("f(1)");
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(4, 0);
        env.sigs.insert(id, (vec![Ty::I32], Ty::Bool));
        let refs = [ResolvedRef {
            range: ident_range(&root, "f"),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, None);
        assert_eq!(ty, Ty::Bool);
        assert!(cx.into_diagnostics().is_empty());
    }

    #[test]
    fn call_argument_type_mismatch_is_reported() {
        // `g(true)` with signature `(I32) -> Unit`: a `Bool` in an `I32` slot.
        let (root, expr) = parse_fragment("g(true)");
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(5, 0);
        env.sigs.insert(id, (vec![Ty::I32], Ty::Unit));
        let refs = [ResolvedRef {
            range: ident_range(&root, "g"),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, None);
        assert_eq!(ty, Ty::Unit);
        assert_eq!(codes(cx.diagnostics()), ["E2103"]);
    }

    // --- malformed input does not panic ------------------------------------

    #[test]
    fn malformed_input_does_not_panic() {
        // A truncated expression parses with recovery; inference must not panic.
        for src in ["1 +", "if", "f(", "(", "1 as"] {
            let _ = infer_bare(src);
        }
    }
}
