//! Constant folding for length-family property values (DSL §19).
//!
//! A length constant folds term-wise: every `dp`/`px`/`sp`/`em`/`%` literal
//! contributes to its own coefficient, and `+`, `-`, unary `-`, scalar `*` and
//! scalar `/` combine coefficients without resolving any unit. `100% - 16dp`
//! therefore folds to `{ dp: -16, pct: 100 }`, never to a pixel number.
//!
//! A value of only `dp` and `%` terms lowers straight to a layout length: `dp`
//! becomes the fixed part and `%` the basis ratio. A value with a `px`, `sp` or
//! `em` term depends on `scale_factor`, text scale or the resolved font size, so
//! it lowers to its five terms, which the runtime folds at layout against the
//! node's environment. A bare number is a scalar, not a length, and never folds
//! into one.

use crate::ast::{
    AstNode, BinaryExpr, CallExpr, Expr, LiteralExpr, ParenExpr, PathExpr, UnaryExpr,
};
use std::cell::Cell;
use std::collections::HashMap;

use crate::ir::ui_ir::{EdgeTermsIr, InsetIr, LengthIr, TermsIr};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::span::TextRange;
use crate::syntax::{SyntaxKind, SyntaxNode};

/// The constants of the file a view lowers in, each reference to one by the
/// span of its name, standing for the value it declares.
#[derive(Debug, Default)]
pub struct Consts {
    values: HashMap<TextRange, Expr>,
    /// How deep the constants being folded nest, so a cycle (an error the
    /// checker reports) ends.
    depth: Cell<u32>,
}

/// How deep constants fold through one another.
const CONST_DEPTH: u32 = 32;

impl Consts {
    /// The constants each reference of `refs` names, from `values`, the
    /// value of each by its symbol.
    pub(crate) fn new(refs: &[ResolvedRef], values: &HashMap<SymbolId, Expr>) -> Self {
        let values = refs
            .iter()
            .filter_map(|r| match r.to {
                Resolution::Symbol(id) => Some((r.range, values.get(&id)?.clone())),
                _ => None,
            })
            .collect();
        Consts {
            values,
            depth: Cell::new(0),
        }
    }

    /// `value` with each reference to a constant replaced by its value, as
    /// far as one leads to another: `None` when `value` names no constant.
    pub(super) fn value_of(&self, value: &Expr) -> Option<Expr> {
        let mut at = self.named(value)?;
        for _ in 0..CONST_DEPTH {
            match self.named(&at) {
                Some(next) => at = next,
                None => return Some(at),
            }
        }
        None
    }

    /// What `f` folds of the value of the constant `path` names.
    fn with<T>(&self, path: &Expr, f: impl FnOnce(&Expr) -> Option<T>) -> Option<T> {
        let value = self.named(path)?;
        if self.depth.get() >= CONST_DEPTH {
            return None;
        }
        self.depth.set(self.depth.get() + 1);
        let folded = f(&value);
        self.depth.set(self.depth.get() - 1);
        folded
    }

    /// The value of the constant the one-segment path `value` names.
    fn named(&self, value: &Expr) -> Option<Expr> {
        let path = PathExpr::cast(value.syntax().clone())?;
        let mut segments = path.segments();
        let name = segments.next()?;
        if segments.next().is_some() {
            return None;
        }
        self.values.get(&name.text_range()).cloned()
    }
}

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

    /// The terms as the runtime folds them, with `%` as a fraction.
    fn lower(self) -> TermsIr {
        TermsIr {
            dp: self.dp,
            px: self.px,
            sp: self.sp,
            em: self.em,
            pct: self.pct / 100.0,
        }
    }
}

/// A length constant as it lowers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Lowered {
    /// A length layout resolves as is.
    Layout(LengthIr),
    /// A length that folds against the node's environment at layout.
    Env(TermsIr),
}

/// A folded constant: a scalar or a length-family value.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Folded {
    Scalar(f32),
    Length(Terms),
}

/// Folds a size property (`width`, `height`).
///
/// A pure-`dp` constant is `Fixed`, clamped to the non-negative size domain
/// (§19.8); one with a `%` term is `Relative`, whose clamp happens at layout once
/// the basis is known; one with an environment term keeps its terms. Anything
/// else — a non-constant, a scalar, a non-length unit, a non-finite result —
/// yields `None`.
pub(super) fn fold_size(value: &Expr, consts: &Consts) -> Option<Lowered> {
    let t = fold_length(value, consts)?;
    if t.needs_env() {
        return Some(Lowered::Env(t.lower()));
    }
    Some(Lowered::Layout(if t.pct == 0.0 {
        LengthIr::Fixed(t.dp.max(0.0))
    } else {
        LengthIr::Relative {
            fixed: t.dp,
            pct: t.pct / 100.0,
        }
    }))
}

/// Folds a `Sizing` property (`width`, `height`): a length, which stands for
/// `Sizing::fixed` of it, `Sizing::fixed(len)`, `Sizing::fill` (weight `1`),
/// `Sizing::fill { weight: w }` or `Sizing::fit`. `Some(Err)` is a constant
/// layout does not size by, and why; `None` is not a constant.
pub(super) fn fold_sizing(value: &Expr, consts: &Consts) -> Option<Result<Lowered, String>> {
    if let Some(length) = fold_size(value, consts) {
        return Some(Ok(length));
    }
    let node = value.syntax();
    match node.kind() {
        SyntaxKind::ParenExpr => fold_sizing(&ParenExpr::cast(node.clone())?.inner()?, consts),
        SyntaxKind::PathExpr => {
            let path = PathExpr::cast(node.clone())?;
            let segments: Vec<String> = path.segments().map(|t| t.text()).collect();
            let [ty, variant] = segments.as_slice() else {
                return None;
            };
            if ty != "Sizing" {
                return None;
            }
            Some(match variant.as_str() {
                "fill" => Ok(Lowered::Layout(LengthIr::Fill { weight: 1.0 })),
                "fit" => Ok(Lowered::Layout(LengthIr::Fit)),
                "min_content" | "max_content" => Err(format!(
                    "`Sizing::{variant}` does not lay out yet; `fixed`, `fill` and `fit` do"
                )),
                _ => return None,
            })
        }
        SyntaxKind::CallExpr => {
            let call = CallExpr::cast(node.clone())?;
            let callee = PathExpr::cast(call.callee()?.syntax().clone())?;
            let path: Vec<String> = callee.segments().map(|t| t.text().to_string()).collect();
            if path != ["Sizing", "fixed"] {
                return None;
            }
            match arguments(node)?.as_slice() {
                [(None, length)] => fold_size(length, consts).map(Ok),
                _ => None,
            }
        }
        SyntaxKind::RecordExpr => {
            if record_head(node) != ["Sizing", "fill"] {
                return None;
            }
            let mut weight = 1.0;
            for field in node.children() {
                if field.kind() != SyntaxKind::RecordExprField {
                    continue;
                }
                let value = field.children().into_iter().find_map(Expr::cast)?;
                weight = match fold(&value, consts)? {
                    Folded::Scalar(w) if w.is_finite() && w > 0.0 => w,
                    _ => {
                        return Some(Err(
                            "a `Sizing::fill` weight is a positive number".to_string()
                        ));
                    }
                };
            }
            Some(Ok(Lowered::Layout(LengthIr::Fill { weight })))
        }
        _ => None,
    }
}

/// The names a record literal's head spells (`EdgeInsets`, `Sizing::fill`).
pub(super) fn record_head(node: &SyntaxNode) -> Vec<String> {
    let mut head = Vec::new();
    for element in node.descendants_with_tokens() {
        let Some(token) = element.as_token() else {
            continue;
        };
        match token.kind() {
            SyntaxKind::LBrace => break,
            SyntaxKind::Ident | SyntaxKind::RawIdent => head.push(token.text().to_string()),
            _ => {}
        }
    }
    head
}

/// Folds a gap property. Only a constant without a `%` term folds: a pure-`dp`
/// one to its extent, clamped non-negative (§19.8), one with an environment
/// term to its terms.
pub(super) fn fold_gap(value: &Expr, consts: &Consts) -> Option<Lowered> {
    let t = fold_length(value, consts)?;
    if t.pct != 0.0 {
        return None;
    }
    Some(if t.needs_env() {
        Lowered::Env(t.lower())
    } else {
        Lowered::Layout(LengthIr::Fixed(t.dp.max(0.0)))
    })
}

/// Folds a `font_size` property to its terms: every unit, `em` and `%` reading
/// the parent's font size, resolves in the typography context at layout.
pub(super) fn fold_font_size(value: &Expr, consts: &Consts) -> Option<TermsIr> {
    fold_length(value, consts).map(Terms::lower)
}

/// A finite length-family constant's terms as the runtime folds them.
pub(super) fn fold_terms(value: &Expr, consts: &Consts) -> Option<TermsIr> {
    fold_length(value, consts).map(Terms::lower)
}

/// A finite scalar constant.
pub(super) fn fold_number(value: &Expr, consts: &Consts) -> Option<f32> {
    match fold(value, consts)? {
        Folded::Scalar(v) if v.is_finite() => Some(v),
        _ => None,
    }
}

/// A constant padding as it lowers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Padding {
    /// Plain `dp` edges whose `start` and `end` agree: the inset layout reads
    /// as is.
    Fixed(InsetIr),
    /// Edges that read the environment, or the layout direction to place
    /// `start` and `end`.
    Env(EdgeTermsIr),
}

/// Folds a `padding` property: a length (every edge), `EdgeInsets::zero()`,
/// `EdgeInsets::all(v)`, `EdgeInsets::axes(inline: a, block: b)` or an
/// `EdgeInsets { .. }` literal whose fields are constants, a missing field
/// being `0dp`. Each edge is clamped non-negative (§19.8), a `%` term's at
/// layout once the parent's content box is known. A value that is not a
/// constant yields `None`.
pub(super) fn fold_padding(value: &Expr, consts: &Consts) -> Option<Padding> {
    let edges = padding_edges(value, consts)?;
    let all = [edges.top, edges.end, edges.bottom, edges.start];
    let clamp = |t: Terms| {
        if t.needs_env() || t.pct != 0.0 {
            t.lower()
        } else {
            Terms {
                dp: t.dp.max(0.0),
                ..Terms::default()
            }
            .lower()
        }
    };
    if all.iter().any(|t| t.needs_env() || t.pct != 0.0) || edges.start != edges.end {
        return Some(Padding::Env(EdgeTermsIr {
            top: clamp(edges.top),
            end: clamp(edges.end),
            bottom: clamp(edges.bottom),
            start: clamp(edges.start),
        }));
    }
    Some(Padding::Fixed(InsetIr {
        left: edges.start.dp.max(0.0),
        top: edges.top.dp.max(0.0),
        right: edges.end.dp.max(0.0),
        bottom: edges.bottom.dp.max(0.0),
    }))
}

/// Folds a `margin` property, spelled as a padding is, to its edges' terms;
/// a margin may be negative.
pub(super) fn fold_margin(value: &Expr, consts: &Consts) -> Option<EdgeTermsIr> {
    let edges = padding_edges(value, consts)?;
    Some(EdgeTermsIr {
        top: edges.top.lower(),
        end: edges.end.lower(),
        bottom: edges.bottom.lower(),
        start: edges.start.lower(),
    })
}

/// Folds a size bound (`min_width`, `max_height`, ..) to its terms, a `dp`
/// one clamped non-negative (§19.8). A greatest bound also takes
/// `Option::None` (unbounded, `Some(None)`) and `Option::Some(length)`.
pub(super) fn fold_bound(value: &Expr, consts: &Consts, greatest: bool) -> Option<Option<TermsIr>> {
    let length = |value: &Expr| {
        let t = fold_length(value, consts)?;
        Some(if t.needs_env() || t.pct != 0.0 {
            t.lower()
        } else {
            Terms {
                dp: t.dp.max(0.0),
                ..Terms::default()
            }
            .lower()
        })
    };
    if let Some(terms) = length(value) {
        return Some(Some(terms));
    }
    if !greatest {
        return None;
    }
    let node = value.syntax();
    match node.kind() {
        SyntaxKind::ParenExpr => fold_bound(&ParenExpr::cast(node.clone())?.inner()?, consts, true),
        SyntaxKind::PathExpr => {
            let path = PathExpr::cast(node.clone())?;
            let segments: Vec<String> = path.segments().map(|t| t.text()).collect();
            matches!(segments.as_slice(), [o, n] if o == "Option" && n == "None").then_some(None)
        }
        SyntaxKind::CallExpr => {
            let call = CallExpr::cast(node.clone())?;
            let callee = PathExpr::cast(call.callee()?.syntax().clone())?;
            let path: Vec<String> = callee.segments().map(|t| t.text().to_string()).collect();
            if path != ["Option", "Some"] {
                return None;
            }
            match arguments(node)?.as_slice() {
                [(None, v)] => length(v).map(Some),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The four edges of a padding value, in their own terms.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Edges {
    top: Terms,
    end: Terms,
    bottom: Terms,
    start: Terms,
}

impl Edges {
    fn all(terms: Terms) -> Self {
        Self {
            top: terms,
            end: terms,
            bottom: terms,
            start: terms,
        }
    }
}

fn padding_edges(value: &Expr, consts: &Consts) -> Option<Edges> {
    if let Some(all) = fold_length(value, consts) {
        return Some(Edges::all(all));
    }
    let node = value.syntax();
    match node.kind() {
        SyntaxKind::ParenExpr => padding_edges(&ParenExpr::cast(node.clone())?.inner()?, consts),
        SyntaxKind::CallExpr => {
            let call = CallExpr::cast(node.clone())?;
            let callee = PathExpr::cast(call.callee()?.syntax().clone())?;
            let path: Vec<String> = callee.segments().map(|t| t.text().to_string()).collect();
            let [ty, ctor] = path.as_slice() else {
                return None;
            };
            if ty != "EdgeInsets" {
                return None;
            }
            let args = arguments(node)?;
            match (ctor.as_str(), args.as_slice()) {
                ("zero", []) => Some(Edges::default()),
                ("all", [(None, v)]) => Some(Edges::all(fold_length(v, consts)?)),
                ("axes", [(inline, a), (block, b)]) => {
                    let (a, b) = match (inline.as_deref(), block.as_deref()) {
                        (None | Some("inline"), None | Some("block")) => (a, b),
                        (Some("block"), Some("inline")) => (b, a),
                        _ => return None,
                    };
                    let (inline, block) = (fold_length(a, consts)?, fold_length(b, consts)?);
                    Some(Edges {
                        top: block,
                        end: inline,
                        bottom: block,
                        start: inline,
                    })
                }
                _ => None,
            }
        }
        SyntaxKind::RecordExpr => {
            let head: Vec<String> = node
                .children_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .take_while(|t| t.kind() != SyntaxKind::LBrace)
                .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
                .map(|t| t.text().to_string())
                .collect();
            if head != ["EdgeInsets"] {
                return None;
            }
            let mut edges = Edges::default();
            for field in node.children() {
                if field.kind() != SyntaxKind::RecordExprField {
                    continue;
                }
                let label = field
                    .children_with_tokens()
                    .into_iter()
                    .filter_map(|e| e.as_token().cloned())
                    .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))?;
                // A `..base` or a shorthand reads a value, not a constant.
                let value = field.children().into_iter().find_map(Expr::cast)?;
                let terms = fold_length(&value, consts)?;
                match label.text().to_string().as_str() {
                    "top" => edges.top = terms,
                    "end" => edges.end = terms,
                    "bottom" => edges.bottom = terms,
                    "start" => edges.start = terms,
                    _ => return None,
                }
            }
            Some(edges)
        }
        _ => None,
    }
}

/// A call's arguments, each with its name when it is named.
pub(super) fn arguments(call: &SyntaxNode) -> Option<Vec<(Option<String>, Expr)>> {
    let list = call
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::ArgumentList)?;
    list.children()
        .into_iter()
        .filter(|arg| arg.kind() == SyntaxKind::Argument)
        .map(|arg| {
            let value = arg.children().into_iter().find_map(Expr::cast)?;
            let named = arg
                .children_with_tokens()
                .into_iter()
                .any(|e| e.as_token().is_some_and(|t| t.kind() == SyntaxKind::Colon));
            let name = named
                .then(|| {
                    arg.children_with_tokens()
                        .into_iter()
                        .filter_map(|e| e.as_token().cloned())
                        .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
                        .map(|t| t.text().to_string())
                })
                .flatten();
            Some((name, value))
        })
        .collect()
}

/// A finite length-family constant's terms.
fn fold_length(value: &Expr, consts: &Consts) -> Option<Terms> {
    match fold(value, consts)? {
        Folded::Length(t) if t.is_finite() => Some(t),
        _ => None,
    }
}

fn fold(expr: &Expr, consts: &Consts) -> Option<Folded> {
    let node = expr.syntax();
    match node.kind() {
        SyntaxKind::LiteralExpr => literal(&LiteralExpr::cast(node.clone())?),
        SyntaxKind::ParenExpr => fold(&ParenExpr::cast(node.clone())?.inner()?, consts),
        SyntaxKind::UnaryExpr => {
            let unary = UnaryExpr::cast(node.clone())?;
            if unary.op()?.kind() != SyntaxKind::Minus {
                return None;
            }
            Some(match fold(&unary.operand()?, consts)? {
                Folded::Scalar(v) => Folded::Scalar(-v),
                Folded::Length(t) => Folded::Length(t.map(|v| -v)),
            })
        }
        SyntaxKind::BinaryExpr => binary(&BinaryExpr::cast(node.clone())?, consts),
        SyntaxKind::PathExpr => consts.with(expr, |value| fold(value, consts)),
        _ => None,
    }
}

/// The §19.3 arithmetic over folded constants. Length ± scalar, length × length
/// and length / length are not lengths and do not fold; neither does a division
/// by a constant zero (`E2109` belongs to the type checker).
fn binary(bin: &BinaryExpr, consts: &Consts) -> Option<Folded> {
    use Folded::{Length, Scalar};
    let lhs = fold(&bin.lhs()?, consts)?;
    let rhs = fold(&bin.rhs()?, consts)?;
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
