//! Native calls: `text::upper(s)`, `Stopwatch::start()` and `watch.elapsed_ms()`,
//! typed against the registered native schema. Each call bound to a native is
//! recorded, so effect checking, capability inference and lowering see the
//! function it calls without resolving it again.
//!
//! A tick duration parameter ([`SchemaTy::Ticks`]) takes a compile-time
//! constant `Duration`, converted here to whole ticks of the game's fixed
//! step, rounding up.

use viso_behavior::native::{NativeEntry, NativeId, NativeVariant, SchemaTy};

use super::{
    InferCx, binary_op_kind, child_exprs, compatible, parse_float_literal, parse_int_literal,
    split_unit_literal, unit_scale,
};
use crate::ast::{AstNode, Expr, FieldExpr};
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

/// A call bound to a native function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeCall {
    /// The function or method.
    pub id: NativeId,
    /// Whether the callee is `receiver.method`, the receiver passing as the
    /// first argument.
    pub receiver: bool,
}

impl InferCx<'_> {
    /// A native function's parameter and return types.
    fn signature(&self, entry: &NativeEntry) -> (Vec<Ty>, Ty) {
        let package = self.env.package_types();
        let params = entry
            .function
            .params
            .iter()
            .map(|p| Ty::from_schema(&p.ty, &package))
            .collect();
        (params, Ty::from_schema(&entry.function.ret, &package))
    }

    /// The schema enum variant `id` names, if it names one.
    pub(super) fn native_variant(&self, id: NativeId) -> Option<NativeVariant> {
        self.env.natives()?.variant_by_id(id)
    }

    /// The signature of a call through a path naming the native `id` (a
    /// function, or a handle method called with its receiver first), recording
    /// the call. A handle type is not callable.
    pub(super) fn native_path_call(
        &mut self,
        id: NativeId,
        node: &SyntaxNode,
        callee: TextRange,
    ) -> Option<(Vec<Ty>, Ty)> {
        let natives = self.env.natives()?;
        let Some(entry) = natives.function_by_id(id) else {
            let name = natives.ty_by_id(id).map_or("", |t| &*t.path);
            self.diagnostics.push(Diagnostic::error(
                "E2103",
                callee,
                format!("the native type `{name}` is not callable; call one of its functions"),
            ));
            return None;
        };
        self.record_native(node.text_range(), id, false);
        Some(self.signature(entry))
    }

    /// Types `receiver.method(args)` on a native handle type `ty`: the method's
    /// parameters after its receiver, recording the call. A name the type does
    /// not declare as a method is `E2001`, with the nearest method names of
    /// the type, those returning the expected type when any is near.
    pub(super) fn native_method_call(
        &mut self,
        ty: NativeId,
        callee: &SyntaxNode,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let Some(natives) = self.env.natives() else {
            return Ty::Unknown;
        };
        let Some(name) = FieldExpr::cast(callee.clone()).and_then(|f| f.field()) else {
            return Ty::Unknown;
        };
        let text = name.text();
        match natives.method(ty, &text).filter(|m| m.is_method(natives)) {
            Some(entry) if entry.function.property => {
                let short = self.native_type_name(ty);
                self.diagnostics.push(Diagnostic::error(
                    "E2103",
                    name.text_range(),
                    format!("`{short}.{text}` is a property: read it without `()`"),
                ));
                for arg in args {
                    let _ = self.infer_expr(arg, None);
                }
                Ty::Unknown
            }
            Some(entry) => {
                let id = entry.id;
                let (mut params, ret) = self.signature(entry);
                params.remove(0);
                self.record_native(node.text_range(), id, true);
                let ty = self.apply_signature(args, (params, ret), expected, node);
                self.native_ticks(id, args, true);
                ty
            }
            None => {
                let owner = natives.ty_by_id(ty);
                let path = owner.map_or("", |t| &*t.path);
                let short = path.rsplit("::").next().unwrap_or(path);
                let mut diagnostic = Diagnostic::error(
                    "E2001",
                    name.text_range(),
                    format!("`{short}` has no method `{text}`"),
                );
                let methods: Vec<(&str, Ty)> = owner
                    .into_iter()
                    .flat_map(|t| t.ty.methods.iter())
                    .filter_map(|m| {
                        let entry = natives.method(ty, m.name)?;
                        entry
                            .is_method(natives)
                            .then(|| (m.name, self.signature(entry).1))
                    })
                    .collect();
                let fits = |ret: &Ty| {
                    expected.is_some_and(|want| {
                        *want != Ty::Unknown && (compatible(want, ret) || ret.widens_to(want))
                    })
                };
                let candidate = |&(name, _): &(&'static str, Ty)| Candidate {
                    name,
                    declared_at: None,
                };
                let mut suggestions =
                    nearest(&text, methods.iter().filter(|m| fits(&m.1)).map(candidate));
                if suggestions.is_empty() {
                    suggestions = nearest(&text, methods.iter().map(candidate));
                }
                attach(&mut diagnostic, name.text_range(), &suggestions);
                self.diagnostics.push(diagnostic);
                for arg in args {
                    let _ = self.infer_expr(arg, None);
                }
                Ty::Unknown
            }
        }
    }

    /// Types `receiver.name` on a native handle type `ty`: a property's
    /// value, recording the call. Any other name is `E2001`, with the nearest
    /// property names.
    pub(super) fn native_property(
        &mut self,
        ty: NativeId,
        name: &crate::syntax::SyntaxToken,
        node: &SyntaxNode,
    ) -> Ty {
        let Some(natives) = self.env.natives() else {
            return Ty::Unknown;
        };
        let text = name.text();
        let property = natives
            .method(ty, &text)
            .filter(|m| m.function.property && m.is_method(natives));
        if let Some(entry) = property {
            let id = entry.id;
            let (_, ret) = self.signature(entry);
            self.record_native(node.text_range(), id, true);
            return ret;
        }
        let short = self.native_type_name(ty);
        let mut diagnostic = Diagnostic::error(
            "E2001",
            name.text_range(),
            format!("`{short}` has no property `{text}`"),
        );
        let properties = natives
            .ty_by_id(ty)
            .into_iter()
            .flat_map(|t| t.ty.methods.iter())
            .filter(|m| m.property)
            .map(|m| Candidate {
                name: m.name,
                declared_at: None,
            });
        let suggestions = nearest(&text, properties);
        attach(&mut diagnostic, name.text_range(), &suggestions);
        self.diagnostics.push(diagnostic);
        Ty::Unknown
    }

    /// A path naming a native used as a value, not called: `E2103`.
    pub(super) fn native_value(&mut self, id: NativeId, at: TextRange) -> Ty {
        let path = self.env.natives().and_then(|n| {
            n.function_by_id(id)
                .map(|f| f.path.to_string())
                .or_else(|| n.ty_by_id(id).map(|t| t.path.to_string()))
        });
        let path = path.unwrap_or_default();
        self.diagnostics.push(Diagnostic::error(
            "E2103",
            at,
            format!("the native `{path}` is not a value; call it"),
        ));
        Ty::Unknown
    }

    fn record_native(&mut self, call: TextRange, id: NativeId, receiver: bool) {
        self.native_calls.insert(call, NativeCall { id, receiver });
        self.env.record_native(call, id);
    }

    /// Converts each tick duration argument of a call to native `id` to whole
    /// ticks: `E2501` unless it is a constant, `E2112` if it is negative or
    /// too long. `args` follow the receiver of a method call.
    pub(super) fn native_ticks(&mut self, id: NativeId, args: &[Expr], receiver: bool) {
        let Some(entry) = self.env.natives().and_then(|n| n.function_by_id(id)) else {
            return;
        };
        let function = entry.function;
        let name = entry.path.rsplit("::").next().unwrap_or(&entry.path);
        let rate = self.env.tick_rate();
        let skip = usize::from(receiver);
        for (param, arg) in function.params.iter().skip(skip).zip(args) {
            if param.ty != SchemaTy::Ticks {
                continue;
            }
            let at = arg.syntax().text_range();
            let Some(seconds) = const_seconds(arg) else {
                let mut diagnostic = Diagnostic::error(
                    "E2501",
                    at,
                    format!("`{name}` takes `{}` as a compile-time duration", param.name),
                );
                diagnostic.notes.push(format!(
                    "write a duration literal or arithmetic on them, such as `250ms`; it \
                     becomes whole ticks of the {rate} Hz fixed step"
                ));
                self.diagnostics.push(diagnostic);
                continue;
            };
            match to_ticks(seconds, rate) {
                Some(ticks) => {
                    self.ticks.insert(at, ticks);
                }
                None => self.diagnostics.push(Diagnostic::error(
                    "E2112",
                    at,
                    format!(
                        "`{name}` rejects its arguments: `{}` is not a duration of 0 or \
                         more ticks that fits `I64`",
                        param.name
                    ),
                )),
            }
        }
    }

    /// The whole ticks the tick duration argument at `range` converts to.
    pub(crate) fn ticks_at(&self, range: TextRange) -> Option<i64> {
        self.ticks.get(&range).copied()
    }

    /// The native function the call at `range` is bound to.
    pub(crate) fn native_call(&self, range: TextRange) -> Option<NativeCall> {
        self.native_calls.get(&range).copied()
    }

    /// The name a native handle type is spelled with, for diagnostics.
    pub(super) fn native_type_name(&self, id: NativeId) -> String {
        self.env.natives().and_then(|n| n.ty_by_id(id)).map_or_else(
            || "<native>".to_string(),
            |t| t.path.rsplit("::").next().unwrap_or(&t.path).to_string(),
        )
    }
}

/// The seconds a constant `Duration` expression stands for: duration
/// literals, parentheses, negation, sums and differences of durations, and
/// durations scaled by or divided by a plain number.
fn const_seconds(expr: &Expr) -> Option<f64> {
    match const_number(expr)? {
        (seconds, true) => Some(seconds),
        (_, false) => None,
    }
}

/// A constant number and whether it is a duration (in seconds).
fn const_number(expr: &Expr) -> Option<(f64, bool)> {
    let node = expr.syntax();
    match node.kind() {
        SyntaxKind::ParenExpr => const_number(&child_exprs(node).into_iter().next()?),
        SyntaxKind::LiteralExpr => {
            let token = node
                .children_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .find(|t| !t.kind().is_trivia())?;
            let text = token.text();
            let number = |body: &str| {
                parse_int_literal(body)
                    .map(|v| v as f64)
                    .or_else(|| parse_float_literal(body))
            };
            match split_unit_literal(&text) {
                Some((body, Ty::Duration)) => {
                    let suffix = &text[body.len()..];
                    Some((number(body)? * unit_scale(suffix), true))
                }
                Some(_) => None,
                None => Some((number(&text)?, false)),
            }
        }
        SyntaxKind::UnaryExpr => {
            let negated = node
                .children_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .any(|t| t.kind() == SyntaxKind::Minus);
            let (value, duration) = const_number(&child_exprs(node).into_iter().next()?)?;
            negated.then_some((-value, duration))
        }
        SyntaxKind::BinaryExpr => {
            let mut operands = child_exprs(node).into_iter();
            let (a, da) = const_number(&operands.next()?)?;
            let (b, db) = const_number(&operands.next()?)?;
            match (binary_op_kind(node)?, da, db) {
                (SyntaxKind::Plus, true, true) => Some((a + b, true)),
                (SyntaxKind::Minus, true, true) => Some((a - b, true)),
                (SyntaxKind::Star, true, false) => Some((a * b, true)),
                (SyntaxKind::Star, false, true) => Some((a * b, true)),
                (SyntaxKind::Slash, true, false) if b != 0.0 => Some((a / b, true)),
                (SyntaxKind::Plus, false, false) => Some((a + b, false)),
                (SyntaxKind::Minus, false, false) => Some((a - b, false)),
                (SyntaxKind::Star, false, false) => Some((a * b, false)),
                (SyntaxKind::Slash, false, false) if b != 0.0 => Some((a / b, false)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `seconds` as whole ticks at `rate` per second, rounding up; exact for any
/// duration a whole number of nanoseconds long. `None` for a negative or
/// non-finite duration or one past the tick range.
pub(crate) fn to_ticks(seconds: f64, rate: u32) -> Option<i64> {
    let nanos = (seconds * 1e9).round();
    if !nanos.is_finite() || nanos < 0.0 || nanos > 1e27 {
        return None;
    }
    let scaled = nanos as i128 * i128::from(rate);
    let ticks = (scaled + 999_999_999) / 1_000_000_000;
    i64::try_from(ticks).ok()
}
