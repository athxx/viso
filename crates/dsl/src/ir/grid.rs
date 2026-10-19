//! Constant folding for a grid's template and a grid child's cell (§U3.5).
//!
//! A track list folds to the runtime's flat [`TrackSizing`] template, a
//! `tracks(n, t)` expanded in place; `Track::fixed` of a plain `dp` length is
//! a fixed track and of a plain `%` one a percentage of the grid's content
//! box. The gaps, the cell alignment and the adaptive columns fold alike.
//! `areas` folds to the cells each named area covers, which the grid's
//! children name by `grid.area` and lowering resolves to explicit cells, so
//! no name reaches the runtime.
//!
//! Each fold yields `None` for a value that is not a constant, and `Err` with
//! the reason for a constant layout does not take yet.

use crate::ast::{AstNode, CallExpr, Expr, ParenExpr, PathExpr};
use crate::hir::infer::format::template_literal;
use crate::hir::infer::pattern::unescape;
use crate::ir::length::{self, Consts, Lowered};
use crate::ir::ui_ir::{
    AdaptiveColumns, AlignItems, AutoRepeat, GridPlacement, LengthIr, TermsIr, TrackMax,
    TrackSizing,
};
use crate::syntax::{SyntaxKind, SyntaxNode};

/// `value` past its parentheses and the constants it names.
fn resolved(value: &Expr, consts: &Consts) -> Expr {
    let value = consts.value_of(value).unwrap_or_else(|| value.clone());
    match ParenExpr::cast(value.syntax().clone()).and_then(|p| p.inner()) {
        Some(inner) => resolved(&inner, consts),
        None => value,
    }
}

/// The segments of a path.
fn path(node: &SyntaxNode) -> Option<Vec<String>> {
    let path = PathExpr::cast(node.clone())?;
    Some(path.segments().map(|t| t.text().to_string()).collect())
}

/// A call's callee path and its positional arguments; `None` for a call with
/// a named argument.
fn call(node: &SyntaxNode) -> Option<(Vec<String>, Vec<Expr>)> {
    let callee = CallExpr::cast(node.clone())?.callee()?;
    let callee = path(callee.syntax())?;
    let args = length::arguments(node)?
        .into_iter()
        .map(|(name, value)| name.is_none().then_some(value))
        .collect::<Option<Vec<_>>>()?;
    Some((callee, args))
}

/// The variant a constant `Owner::variant` path names.
fn variant(value: &Expr, owner: &str) -> Option<String> {
    match path(value.syntax())?.as_slice() {
        [head, variant] if head == owner => Some(variant.clone()),
        _ => None,
    }
}

/// A plain-`dp` length constant, clamped non-negative (§19.8).
fn dp(value: &Expr, consts: &Consts, what: &str) -> Option<Result<f32, String>> {
    let terms = length::fold_terms(value, consts)?;
    Some(if terms.needs_env() || terms.pct != 0.0 {
        Err(format!(
            "{what} that reads the environment or a percentage does not lay out yet; a `dp` one does"
        ))
    } else {
        Ok(terms.dp.max(0.0))
    })
}

/// A non-negative share constant.
fn share(value: &Expr, consts: &Consts) -> Option<Result<f32, String>> {
    let share = length::fold_number(value, consts)?;
    Some(if share >= 0.0 {
        Ok(share)
    } else {
        Err("an `fr` share is a non-negative number".to_string())
    })
}

/// Folds a track list (`columns`, `rows`): a list of tracks, or
/// `tracks(count, track)`.
pub(super) fn fold_tracks(
    value: &Expr,
    consts: &Consts,
) -> Option<Result<Vec<TrackSizing>, String>> {
    let value = resolved(value, consts);
    let node = value.syntax();
    if let Some((callee, args)) = call(node)
        && callee == ["tracks"]
    {
        // The checker reports a call of another shape.
        let [count, track] = args.as_slice() else {
            return Some(Ok(Vec::new()));
        };
        let count = length::fold_number(&resolved(count, consts), consts)?;
        if count.fract() != 0.0 || !(0.0..=f32::from(u16::MAX)).contains(&count) {
            return None;
        }
        return Some(fold_track(track, consts)?.map(|track| vec![track; count as usize]));
    }
    if node.kind() != SyntaxKind::ListExpr {
        return None;
    }
    let mut tracks = Vec::new();
    for item in node.children().into_iter().filter_map(Expr::cast) {
        match fold_track(&item, consts)? {
            Ok(track) => tracks.push(track),
            Err(reason) => return Some(Err(reason)),
        }
    }
    Some(Ok(tracks))
}

/// Folds one `Track`.
pub(super) fn fold_track(value: &Expr, consts: &Consts) -> Option<Result<TrackSizing, String>> {
    let value = resolved(value, consts);
    let node = value.syntax();
    if variant(&value, "Track").as_deref() == Some("auto") {
        return Some(Ok(TrackSizing::Auto));
    }
    let (callee, args) = call(node)?;
    let [ty, ctor] = callee.as_slice() else {
        return None;
    };
    if ty != "Track" {
        return None;
    }
    Some(match (ctor.as_str(), args.as_slice()) {
        ("fixed", [len]) => {
            let terms = length::fold_terms(&resolved(len, consts), consts)?;
            fixed_track(terms)
        }
        ("fr", [fr]) => share(&resolved(fr, consts), consts)?.map(TrackSizing::Fr),
        ("fit_content", [limit]) => dp(&resolved(limit, consts), consts, "a `fit_content` limit")?
            .map(TrackSizing::FitContent),
        ("minmax", [min, max]) => {
            let min = match dp(&resolved(min, consts), consts, "a `minmax` floor")? {
                Ok(min) => min,
                Err(reason) => return Some(Err(reason)),
            };
            fold_track_max(max, consts)?.map(|max| match max {
                TrackMax::Px(max) => TrackSizing::Minmax(min, max),
                TrackMax::Fr(fr) => TrackSizing::FlexMin(min, fr),
            })
        }
        _ => return None,
    })
}

/// A `Track::fixed` of `terms`: plain `dp` is a fixed track, plain `%` a
/// percentage of the grid's content box.
fn fixed_track(terms: TermsIr) -> Result<TrackSizing, String> {
    if terms.needs_env() {
        Err("a track that reads the environment (`px`, `sp`, `em`) does not lay out yet; a `dp` or `%` one does".to_string())
    } else if terms.pct == 0.0 {
        Ok(TrackSizing::Fixed(terms.dp.max(0.0)))
    } else if terms.dp == 0.0 {
        Ok(TrackSizing::Percent(terms.pct.max(0.0)))
    } else {
        Err(
            "a track that adds `dp` to `%` does not lay out yet; a `dp` or a `%` one does"
                .to_string(),
        )
    }
}

/// Folds a `TrackMax`.
fn fold_track_max(value: &Expr, consts: &Consts) -> Option<Result<TrackMax, String>> {
    let value = resolved(value, consts);
    let (callee, args) = call(value.syntax())?;
    let ([ty, ctor], [arg]) = (callee.as_slice(), args.as_slice()) else {
        return None;
    };
    if ty != "TrackMax" {
        return None;
    }
    let arg = resolved(arg, consts);
    match ctor.as_str() {
        "length" => Some(dp(&arg, consts, "a track maximum")?.map(TrackMax::Px)),
        "fr" => Some(share(&arg, consts)?.map(TrackMax::Fr)),
        _ => None,
    }
}

/// Folds a grid gap (`column_gap`, `row_gap`): plain `dp` to its extent,
/// clamped non-negative, one reading the environment to its terms.
pub(super) fn fold_gap(value: &Expr, consts: &Consts) -> Option<Result<Lowered, String>> {
    let terms = length::fold_terms(value, consts)?;
    Some(if terms.pct != 0.0 {
        Err(
            "a grid gap with a `%` term does not lay out yet; a `dp`, `px`, `sp` or `em` one does"
                .to_string(),
        )
    } else if terms.needs_env() {
        Ok(Lowered::Env(terms))
    } else {
        Ok(Lowered::Layout(LengthIr::Fixed(terms.dp.max(0.0))))
    })
}

/// Folds `align_items`: a `GridAlign` variant.
pub(super) fn fold_align_items(value: &Expr, consts: &Consts) -> Option<AlignItems> {
    Some(
        match variant(&resolved(value, consts), "GridAlign")?.as_str() {
            "stretch" => AlignItems::Stretch,
            "start" => AlignItems::Start,
            "center" => AlignItems::Center,
            "end" => AlignItems::End,
            _ => return None,
        },
    )
}

/// Folds `adaptive_columns`: `Option::None`, or an `AdaptiveColumns { .. }`,
/// bare or in `Option::Some`.
pub(super) fn fold_adaptive(
    value: &Expr,
    consts: &Consts,
) -> Option<Result<Option<AdaptiveColumns>, String>> {
    let value = resolved(value, consts);
    let node = value.syntax();
    if path(node).is_some_and(|p| p == ["Option", "None"]) {
        return Some(Ok(None));
    }
    if let Some((callee, args)) = call(node)
        && callee == ["Option", "Some"]
    {
        let [inner] = args.as_slice() else {
            return None;
        };
        return fold_adaptive(inner, consts);
    }
    if node.kind() != SyntaxKind::RecordExpr || length::record_head(node) != ["AdaptiveColumns"] {
        return None;
    }
    let mut columns = AdaptiveColumns {
        mode: AutoRepeat::Fill,
        min: 0.0,
        max: TrackMax::Fr(1.0),
    };
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
        match label.text().trim_start_matches("r#") {
            "mode" => {
                columns.mode = match variant(&resolved(&value, consts), "AutoRepeat")?.as_str() {
                    "fill" => AutoRepeat::Fill,
                    "fit" => AutoRepeat::Fit,
                    _ => return None,
                }
            }
            "min" => match dp(
                &resolved(&value, consts),
                consts,
                "an adaptive column floor",
            )? {
                Ok(min) => columns.min = min,
                Err(reason) => return Some(Err(reason)),
            },
            "max" => match fold_track_max(&value, consts)? {
                Ok(max) => columns.max = max,
                Err(reason) => return Some(Err(reason)),
            },
            _ => return None,
        }
    }
    Some(Ok(Some(columns)))
}

/// A constant string literal's text.
fn string(value: &Expr, consts: &Consts) -> Option<String> {
    let (body, raw) = template_literal(&resolved(value, consts))?;
    if raw { Some(body) } else { unescape(&body) }
}

/// Folds `areas`: each string a row of whitespace-separated area names, `.`
/// an empty cell. `Some(Err)` is a template whose rows differ in length or
/// whose area is not a rectangle, and why.
pub(super) fn fold_areas(
    value: &Expr,
    consts: &Consts,
) -> Option<Result<Vec<(String, GridPlacement)>, String>> {
    let value = resolved(value, consts);
    let node = value.syntax();
    if node.kind() != SyntaxKind::ListExpr {
        return None;
    }
    let mut rows = Vec::new();
    for item in node.children().into_iter().filter_map(Expr::cast) {
        let row = string(&item, consts)?;
        rows.push(
            row.split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
    }
    Some(areas_of(&rows))
}

/// The cells each area of the template `rows` covers.
fn areas_of(rows: &[Vec<String>]) -> Result<Vec<(String, GridPlacement)>, String> {
    let width = rows.first().map_or(0, Vec::len);
    if let Some(r) = rows.iter().position(|row| row.len() != width) {
        return Err(format!(
            "every row of `areas` names the same number of cells: row {r} names {}, row 0 names {width}",
            rows[r].len()
        ));
    }
    if rows.len() > usize::from(u16::MAX) || width > usize::from(u16::MAX) {
        return Err("`areas` names more rows or columns than a grid has".to_string());
    }
    let mut areas: Vec<(String, [usize; 4])> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, name) in row.iter().enumerate() {
            if name == "." {
                continue;
            }
            match areas.iter_mut().find(|(n, _)| n == name) {
                Some((_, [c0, r0, c1, r1])) => {
                    *c0 = (*c0).min(c);
                    *r0 = (*r0).min(r);
                    *c1 = (*c1).max(c);
                    *r1 = (*r1).max(r);
                }
                None => areas.push((name.clone(), [c, r, c, r])),
            }
        }
    }
    areas
        .into_iter()
        .map(|(name, [c0, r0, c1, r1])| {
            let filled = (r0..=r1).all(|r| (c0..=c1).all(|c| rows[r][c] == name));
            if !filled {
                return Err(format!("the area `{name}` of `areas` is not a rectangle"));
            }
            let place = GridPlacement {
                column: Some(c0 as u16),
                row: Some(r0 as u16),
                column_span: (c1 - c0 + 1) as u16,
                row_span: (r1 - r0 + 1) as u16,
            };
            Ok((name, place))
        })
        .collect()
}

/// What a `grid.*` binding sets of a child's cell.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Cell {
    /// The start column, `None` to flow.
    Column(Option<u16>),
    /// The start row, `None` to flow.
    Row(Option<u16>),
    /// The columns spanned.
    ColumnSpan(u16),
    /// The rows spanned.
    RowSpan(u16),
    /// The area named, `None` for none.
    Area(Option<String>),
}

/// Folds the `grid.member` binding of `value`. `Some(Err)` is a span of
/// `0`.
pub(super) fn fold_cell(
    member: &str,
    value: &Expr,
    consts: &Consts,
) -> Option<Result<Cell, String>> {
    let value = resolved(value, consts);
    let index = |value: &Expr| {
        let n = length::fold_number(&resolved(value, consts), consts)?;
        (n.fract() == 0.0 && (0.0..=f32::from(u16::MAX)).contains(&n)).then_some(n as u16)
    };
    // `grid.column`, `grid.row` and `grid.area` are optional: a bare value,
    // `Option::Some(v)` or `Option::None`.
    let optional = |value: &Expr| -> Option<Option<Expr>> {
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
    };
    Some(Ok(match member {
        "column" | "row" => {
            let at = match optional(&value)? {
                Some(v) => Some(index(&v)?),
                None => None,
            };
            if member == "column" {
                Cell::Column(at)
            } else {
                Cell::Row(at)
            }
        }
        "column_span" | "row_span" => {
            let span = index(&value)?;
            if span == 0 {
                return Some(Err(format!("a `grid.{member}` spans at least one track")));
            }
            if member == "column_span" {
                Cell::ColumnSpan(span)
            } else {
                Cell::RowSpan(span)
            }
        }
        "area" => Cell::Area(match optional(&value)? {
            Some(v) => Some(string(&v, consts)?),
            None => None,
        }),
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(rows: &[&str]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.split_whitespace().map(str::to_string).collect())
            .collect()
    }

    #[test]
    fn each_area_covers_its_rectangle() {
        let areas = areas_of(&rows(&["head head", "side main", ". main"])).unwrap();
        let place = |column, row, column_span, row_span| GridPlacement {
            column: Some(column),
            row: Some(row),
            column_span,
            row_span,
        };
        assert_eq!(
            areas,
            [
                ("head".to_string(), place(0, 0, 2, 1)),
                ("side".to_string(), place(0, 1, 1, 1)),
                ("main".to_string(), place(1, 1, 1, 2)),
            ]
        );
    }

    #[test]
    fn a_ragged_or_broken_template_is_invalid() {
        assert!(areas_of(&rows(&["a a", "b"])).is_err());
        assert!(areas_of(&rows(&["a b", "b a"])).is_err());
        assert!(areas_of(&rows(&["a . a"])).is_err());
    }
}
