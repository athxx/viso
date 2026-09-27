//! Constant folding for length-family property values (DSL §19).
//!
//! A length constant folds term-wise: every `dp`/`px`/`sp`/`em`/`%` literal
//! contributes to its own coefficient, and `+`, `-`, unary `-`, scalar `*` and
//! scalar `/` combine coefficients without resolving any unit. `100% - 16dp`
//! therefore folds to `{ dp: -16, pct: 100 }`, never to a pixel number.
//!
//! Only the terms layout can resolve without an environment scalar lower here:
//! `dp` becomes the fixed part and `%` the basis ratio. A value with a `px`, `sp`
//! or `em` term depends on `scale_factor`, text scale or the resolved font size,
//! so it is left pending rather than folded to a wrong constant. A bare number is
//! a scalar, not a length, and never folds into one.

use crate::ast::{AstNode, BinaryExpr, Expr, LiteralExpr, ParenExpr, UnaryExpr};
use crate::ir::ui_ir::LengthIr;
use crate::syntax::SyntaxKind;

/// The five coefficients of a length-family constant.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Terms {
    dp: f32,
    px: f32,
    sp: f32,
    em: f32,
    /// In percent units: `50%` is `50.0`.
    pct: f32,
}

impl Terms {
    fn map(self, f: impl Fn(f32) -> f32) -> Terms {
        Terms {
            dp: f(self.dp),
            px: f(self.px),
            sp: f(self.sp),
            em: f(self.em),
            pct: f(self.pct),
        }
    }

    fn zip(self, other: Terms, f: impl Fn(f32, f32) -> f32) -> Terms {
        Terms {
            dp: f(self.dp, other.dp),
            px: f(self.px, other.px),
            sp: f(self.sp, other.sp),
            em: f(self.em, other.em),
            pct: f(self.pct, other.pct),
        }
    }

    fn is_finite(self) -> bool {
        [self.dp, self.px, self.sp, self.em, self.pct]
            .iter()
            .all(|v| v.is_finite())
    }

    /// Whether a term needs an environment scalar to resolve.
    fn needs_env(self) -> bool {
        self.px != 0.0 || self.sp != 0.0 || self.em != 0.0
    }
}

/// A folded constant: a scalar or a length-family value.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Folded {
    Scalar(f32),
    Length(Terms),
}

/// Folds a size property (`width`, `height`) into a [`LengthIr`].
///
/// A pure-`dp` constant is `Fixed`, clamped to the non-negative size domain
/// (§19.8); one with a `%` term is `Relative`, whose clamp happens at layout once
/// the basis is known. Anything else — a non-constant, a scalar, a non-length
/// unit, an environment term, a non-finite result — yields `None`.
pub(super) fn fold_size(value: &Expr) -> Option<LengthIr> {
    let Folded::Length(t) = fold(value)? else {
        return None;
    };
    if !t.is_finite() || t.needs_env() {
        return None;
    }
    if t.pct == 0.0 {
        Some(LengthIr::Fixed(t.dp.max(0.0)))
    } else {
        Some(LengthIr::Relative {
            fixed: t.dp,
            pct: t.pct / 100.0,
        })
    }
}

/// Folds a gap property into a dp extent. A gap has no percent basis, so only a
/// pure-`dp` constant folds; the result is clamped non-negative (§19.8).
pub(super) fn fold_gap(value: &Expr) -> Option<f32> {
    let Folded::Length(t) = fold(value)? else {
        return None;
    };
    let dp_only = t.is_finite() && !t.needs_env() && t.pct == 0.0;
    dp_only.then_some(t.dp.max(0.0))
}

fn fold(expr: &Expr) -> Option<Folded> {
    let node = expr.syntax();
    match node.kind() {
        SyntaxKind::LiteralExpr => literal(&LiteralExpr::cast(node.clone())?),
        SyntaxKind::ParenExpr => fold(&ParenExpr::cast(node.clone())?.inner()?),
        SyntaxKind::UnaryExpr => {
            let unary = UnaryExpr::cast(node.clone())?;
            if unary.op()?.kind() != SyntaxKind::Minus {
                return None;
            }
            Some(match fold(&unary.operand()?)? {
                Folded::Scalar(v) => Folded::Scalar(-v),
                Folded::Length(t) => Folded::Length(t.map(|v| -v)),
            })
        }
        SyntaxKind::BinaryExpr => binary(&BinaryExpr::cast(node.clone())?),
        _ => None,
    }
}

/// The §19.3 arithmetic over folded constants. Length ± scalar, length × length
/// and length / length are not lengths and do not fold; neither does a division
/// by a constant zero (`E2109` belongs to the type checker).
fn binary(bin: &BinaryExpr) -> Option<Folded> {
    use Folded::{Length, Scalar};
    let lhs = fold(&bin.lhs()?)?;
    let rhs = fold(&bin.rhs()?)?;
    match (bin.op()?.kind(), lhs, rhs) {
        (SyntaxKind::Plus, Scalar(a), Scalar(b)) => Some(Scalar(a + b)),
        (SyntaxKind::Minus, Scalar(a), Scalar(b)) => Some(Scalar(a - b)),
        (SyntaxKind::Star, Scalar(a), Scalar(b)) => Some(Scalar(a * b)),
        (SyntaxKind::Slash, Scalar(a), Scalar(b)) if b != 0.0 => Some(Scalar(a / b)),
        (SyntaxKind::Plus, Length(a), Length(b)) => Some(Length(a.zip(b, |x, y| x + y))),
        (SyntaxKind::Minus, Length(a), Length(b)) => Some(Length(a.zip(b, |x, y| x - y))),
        (SyntaxKind::Star, Length(t), Scalar(s)) | (SyntaxKind::Star, Scalar(s), Length(t)) => {
            Some(Length(t.map(|v| v * s)))
        }
        (SyntaxKind::Slash, Length(t), Scalar(s)) if s != 0.0 => Some(Length(t.map(|v| v / s))),
        _ => None,
    }
}

fn literal(lit: &LiteralExpr) -> Option<Folded> {
    let token = lit.token()?;
    let text = token.text();
    match token.kind() {
        SyntaxKind::IntLiteral => int_value(&text).map(Folded::Scalar),
        SyntaxKind::FloatLiteral => decimal(&text).map(Folded::Scalar),
        SyntaxKind::UnitLiteral => unit_literal(&text),
        _ => None,
    }
}

/// A suffixed literal: a length-family unit folds to its term, a numeric type
/// suffix (`2u32`, `1.5f32`) is a scalar, and every other suffix does not fold.
fn unit_literal(text: &str) -> Option<Folded> {
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '_'))
        .unwrap_or(text.len());
    let (body, suffix) = text.split_at(split);
    let n = decimal(body)?;
    let one = |t: Terms| Some(Folded::Length(t));
    match suffix {
        "dp" => one(Terms {
            dp: n,
            ..Terms::default()
        }),
        "px" => one(Terms {
            px: n,
            ..Terms::default()
        }),
        "sp" => one(Terms {
            sp: n,
            ..Terms::default()
        }),
        "em" => one(Terms {
            em: n,
            ..Terms::default()
        }),
        "%" => one(Terms {
            pct: n,
            ..Terms::default()
        }),
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "f32" | "f64" => {
            Some(Folded::Scalar(n))
        }
        _ => None,
    }
}

/// A decimal or radix-prefixed integer spelling, separators removed.
fn int_value(text: &str) -> Option<f32> {
    let digits: String = text.chars().filter(|&c| c != '_').collect();
    let radix = match digits.get(..2) {
        Some("0x") => 16,
        Some("0o") => 8,
        Some("0b") => 2,
        _ => return decimal(&digits),
    };
    u64::from_str_radix(&digits[2..], radix)
        .ok()
        .map(|v| v as f32)
}

/// A decimal number spelling, separators removed.
fn decimal(text: &str) -> Option<f32> {
    let digits: String = text.chars().filter(|&c| c != '_').collect();
    digits.parse::<f32>().ok()
}
