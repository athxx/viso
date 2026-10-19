//! Constant folding for where a child sits in a `Stack` or an `Absolute`
//! (§U3.6): a stack's `content_align`, its children's `stack.align` and
//! `stack.layer`, and an absolute child's `absolute.*` insets.
//!
//! Each fold yields `None` for a value that is not a constant.

use crate::ast::{AstNode, Expr};
use crate::ir::grid::{call, path, resolved, variant};
use crate::ir::length::{self, Consts};
use crate::ir::ui_ir::{Alignment2D, TermsIr};

/// Folds an `Alignment2D` variant.
pub(super) fn fold_alignment(value: &Expr, consts: &Consts) -> Option<Alignment2D> {
    Some(
        match variant(&resolved(value, consts), "Alignment2D")?.as_str() {
            "top_start" => Alignment2D::TopStart,
            "top" => Alignment2D::Top,
            "top_end" => Alignment2D::TopEnd,
            "start" => Alignment2D::Start,
            "center" => Alignment2D::Center,
            "end" => Alignment2D::End,
            "bottom_start" => Alignment2D::BottomStart,
            "bottom" => Alignment2D::Bottom,
            "bottom_end" => Alignment2D::BottomEnd,
            _ => return None,
        },
    )
}

/// What a `stack.*` binding sets of a stack child's place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Stacked {
    /// Its own alignment, `None` to take the stack's.
    Align(Option<Alignment2D>),
    /// The layer it paints in.
    Layer(i32),
}

/// Folds the `stack.member` binding of `value`.
pub(super) fn fold_stacked(member: &str, value: &Expr, consts: &Consts) -> Option<Stacked> {
    let value = resolved(value, consts);
    match member {
        "align" => Some(Stacked::Align(match optional(&value)? {
            Some(inner) => Some(fold_alignment(&inner, consts)?),
            None => None,
        })),
        "layer" => {
            let n = length::fold_number(&value, consts)?;
            let whole = n.fract() == 0.0 && (i32::MIN as f32..=i32::MAX as f32).contains(&n);
            whole.then_some(Stacked::Layer(n as i32))
        }
        _ => None,
    }
}

/// Folds an `absolute.*` inset: its terms, `None` for `Option::None`.
pub(super) fn fold_inset(value: &Expr, consts: &Consts) -> Option<Option<TermsIr>> {
    match optional(&resolved(value, consts))? {
        Some(inner) => Some(Some(length::fold_terms(&resolved(&inner, consts), consts)?)),
        None => Some(None),
    }
}

/// An optional value's inner value: a bare value or `Option::Some(v)`, and
/// `None` for `Option::None`.
fn optional(value: &Expr) -> Option<Option<Expr>> {
    let node = value.syntax();
    if path(node).is_some_and(|p| p == ["Option", "None"]) {
        return Some(None);
    }
    match call(node) {
        Some((callee, args)) if callee == ["Option", "Some"] => match args.as_slice() {
            [inner] => Some(Some(inner.clone())),
            _ => None,
        },
        _ => Some(Some(value.clone())),
    }
}
