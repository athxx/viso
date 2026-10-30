//! Strings with holes: each hole is an expression whose value shows as text,
//! which takes a type with `Display`; an empty hole or a value without one is
//! `E2108`.

use super::InferCx;
use super::TypeEnv;
use crate::ast::{AstNode, Expr, InterpolatedString, StringPart};
use crate::diag::{Applicability, Diagnostic, Fix, TextEdit};
use crate::hir::ty::Ty;
use crate::syntax::{SyntaxKind, SyntaxNode};

impl InferCx<'_> {
    /// Types a string with holes, a `String`: each hole's value must have
    /// `Display` (`E2108`).
    pub(super) fn infer_interpolated(&mut self, node: &SyntaxNode) -> Ty {
        let Some(string) = InterpolatedString::cast(node.clone()) else {
            return Ty::String;
        };
        for part in string.parts() {
            match part {
                StringPart::Text(_) => {}
                StringPart::Hole(Some(value), _) => {
                    let ty = self.infer_expr(&value, None);
                    if !displayable(self.env, &ty) {
                        let message = format!(
                            "`{}` cannot be shown in a string: it does not implement `Display`",
                            self.describe(&ty)
                        );
                        self.diagnostics.push(Diagnostic::error(
                            "E2108",
                            value.syntax().text_range(),
                            message,
                        ));
                    }
                }
                StringPart::Hole(None, at) => self.diagnostics.push(Diagnostic::error(
                    "E2108",
                    at,
                    "a hole needs the value to show; a literal brace is `{{`",
                )),
            }
        }
        Ty::String
    }

    /// Types the value of a `String` (or `Option<String>`) property slot. A value
    /// of another displayable type is `E2103` with a machine-applicable fix that
    /// puts it in a string's hole, `"{..}"`: a text slot never converts implicitly.
    pub(crate) fn infer_text_value(&mut self, value: &Expr, target: &Ty) -> Ty {
        let mark = self.diagnostics.len();
        let ty = self.infer_promoted(value, target);
        let range = value.syntax().text_range();
        let Some(index) = self.diagnostics[mark..]
            .iter()
            .position(|d| d.code == "E2103" && d.primary == range)
            .map(|i| mark + i)
        else {
            return ty;
        };
        let end = self.diagnostics.len();
        let found = self.infer_expr(value, None);
        self.diagnostics.truncate(end);
        let text_like = matches!(found, Ty::String | Ty::Option(_) | Ty::Unknown | Ty::Never);
        let source = value.syntax().text();
        if !text_like && displayable(self.env, &found) && !source.contains('\n') {
            let note = format!(
                "a text property takes a `String`; show the `{}` in a string",
                self.describe(&found)
            );
            let diagnostic = &mut self.diagnostics[index];
            diagnostic.notes.push(note);
            diagnostic.fixes.push(Fix {
                title: "show the value in a string, `\"{..}\"`".to_string(),
                applicability: Applicability::MachineApplicable,
                edits: vec![TextEdit::new(range, format!("\"{{{}}}\"", source.trim()))],
            });
        }
        ty
    }
}

/// The text of a string literal between its quotes, escapes undecoded, and
/// whether it is raw.
pub(crate) fn string_literal(expr: &Expr) -> Option<(String, bool)> {
    let node = expr.syntax();
    if node.kind() != SyntaxKind::LiteralExpr {
        return None;
    }
    let token = node
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| !t.kind().is_trivia())?;
    let text = token.text().to_string();
    match token.kind() {
        SyntaxKind::StringLiteral => {
            let inner = text.strip_prefix('"')?;
            Some((inner.strip_suffix('"').unwrap_or(inner).to_string(), false))
        }
        SyntaxKind::RawStringLiteral => {
            let body = text.strip_prefix('r')?;
            let hashes = body.len() - body.trim_start_matches('#').len();
            let body = &body[hashes..];
            let body = body.strip_prefix('"')?;
            let body = body.strip_suffix(&"#".repeat(hashes)).unwrap_or(body);
            Some((body.strip_suffix('"').unwrap_or(body).to_string(), true))
        }
        _ => None,
    }
}

/// Whether a value of type `ty` implements `Display`: the scalars, text,
/// the single-unit length family and the other dimensional scalars do; a record
/// or enum does not. A nominal type this module does not declare as a record or
/// enum, and an undetermined type, are not reported.
pub(crate) fn displayable(env: &dyn TypeEnv, ty: &Ty) -> bool {
    match ty {
        Ty::Bool
        | Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::I64
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::U64
        | Ty::F32
        | Ty::F64
        | Ty::Char
        | Ty::String
        | Ty::Dp
        | Ty::Px
        | Ty::Sp
        | Ty::Em
        | Ty::Percent
        | Ty::Duration
        | Ty::Angle
        | Ty::Frequency
        | Ty::InferInt
        | Ty::InferFloat
        | Ty::Never
        | Ty::Unknown => true,
        Ty::Named(id, ..) => env.record_fields(*id).is_none() && env.enum_variants(*id).is_none(),
        Ty::Param(_)
        | Ty::Assoc(..)
        | Ty::Dyn(..)
        | Ty::Const(_)
        | Ty::Bytes
        | Ty::Unit
        | Ty::Color
        | Ty::MixedLength
        | Ty::Tuple(_)
        | Ty::Fn(..)
        | Ty::List(_)
        | Ty::Option(_)
        | Ty::Result(..)
        | Ty::Range(_)
        | Ty::RangeInclusive(_)
        | Ty::Resource(..)
        | Ty::ResourceState(..)
        | Ty::NodeRef(_)
        | Ty::Animation
        | Ty::Native(..) => false,
    }
}
