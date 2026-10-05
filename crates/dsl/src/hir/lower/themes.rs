//! `theme` declarations (§60, U12.2): a constant `Theme` whose items name its
//! fields, each once (`E2001` for no such field, `E2103` for a second value or
//! one of another type); a field no item gives comes from the base theme, else
//! from its default, and one with neither is `E2103`. The values are pure
//! initializers (`E2502`); the base graph is acyclic (`E2003`). A host switches
//! its views to a theme by name, so a package names each theme once (`E2002`).

use std::collections::{HashMap, HashSet};

use crate::ast::{AstNode, Expr, Item, ThemeDecl};
use crate::behavior::ir::FunctionKind;
use crate::behavior::lower::{Def, lower_theme, unsupported};
use crate::diag::Diagnostic;
use crate::hir::effect::BodyContext;
use crate::hir::infer::{InferCx, TypeEnv};
use crate::hir::ty::Ty;
use crate::resolve::prelude::theme_record;
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::TextRange;

use super::{ModuleEnv, TYPE_ERRORS, check_body, has_errors, name_of};

/// The themes of `items` whose base or values lead back to themselves.
pub(super) fn cyclic(
    items: &[ThemeDecl],
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
) -> HashSet<SymbolId> {
    let symbols: HashMap<SymbolId, TextRange> = items
        .iter()
        .filter_map(|t| {
            let at = t.syntax().text_range();
            Some((*env.scope.declared.get(&at)?, at))
        })
        .collect();
    let reads = |at: TextRange| -> Vec<SymbolId> {
        refs.iter()
            .filter(|r| at.contains_range(r.range))
            .filter_map(|r| match r.to {
                Resolution::Symbol(id) if symbols.contains_key(&id) => Some(id),
                _ => None,
            })
            .collect()
    };
    let edges: HashMap<SymbolId, Vec<SymbolId>> =
        symbols.iter().map(|(&id, &at)| (id, reads(at))).collect();
    let mut cyclic = HashSet::new();
    for &start in symbols.keys() {
        let mut seen = HashSet::new();
        let mut stack = edges[&start].clone();
        while let Some(next) = stack.pop() {
            if next == start {
                cyclic.insert(start);
                break;
            }
            if seen.insert(next) {
                stack.extend(edges.get(&next).into_iter().flatten());
            }
        }
    }
    cyclic
}

/// The theme declarations of a module's items.
pub(super) fn themes_of(items: impl Iterator<Item = Item>) -> Vec<ThemeDecl> {
    items
        .filter_map(|item| match item {
            Item::Export(e) => e.declaration(),
            other => Some(other),
        })
        .filter_map(|item| match item {
            Item::Theme(t) => Some(t),
            _ => None,
        })
        .collect()
}

/// Checks and lowers the theme `decl`; `cyclic` holds the module's themes on
/// a base cycle.
pub(super) fn lower(
    decl: &ThemeDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    cyclic: &HashSet<SymbolId>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let name = name_of(decl.name());
    let at = decl
        .name()
        .map_or(decl.syntax().text_range(), |t| t.text_range());
    let symbol = env.scope.declared.get(&decl.syntax().text_range()).copied();
    let record = theme_record();
    let fields = env.record_fields(record).unwrap_or_default().to_vec();
    let mut cx = InferCx::new(refs, env);
    let mut failed = false;

    let base = decl.base().and_then(|base| {
        let segments: Vec<_> = base.segments().collect();
        let head = segments.first()?;
        let found = match (&segments[..], ref_at(refs, head.text_range())) {
            ([_], Some(Resolution::Symbol(id))) => Some(id),
            _ => None,
        };
        let theme = Ty::Named(record);
        match found {
            Some(id) if cx.resolution_ty(&Resolution::Symbol(id)) == theme => Some(id),
            Some(_) => {
                diagnostics.push(Diagnostic::error(
                    "E2103",
                    base.syntax().text_range(),
                    format!(
                        "a theme's base is a `Theme`, which `{}` is not",
                        base.syntax().text()
                    ),
                ));
                failed = true;
                None
            }
            None => {
                diagnostics.push(Diagnostic::error(
                    "E2001",
                    base.syntax().text_range(),
                    format!("no theme `{}` to base `{name}` on", base.syntax().text()),
                ));
                failed = true;
                None
            }
        }
    });
    if symbol.is_some_and(|s| cyclic.contains(&s)) {
        diagnostics.push(Diagnostic::error(
            "E2003",
            at,
            format!("the theme `{name}` is based on itself"),
        ));
        failed = true;
    }

    let mut items: Vec<(u32, Expr)> = Vec::new();
    for item in decl.items() {
        let (Some(label), Some(value)) = (item.name(), item.value()) else {
            failed = true;
            continue;
        };
        let text = label.text();
        let Some(index) = fields.iter().position(|f| f.name == text) else {
            let names: Vec<String> = fields.iter().map(|f| format!("`{}`", f.name)).collect();
            diagnostics.push(Diagnostic::error(
                "E2001",
                label.text_range(),
                format!(
                    "no field `{text}` on `Theme`; a theme gives {}",
                    names.join(", ")
                ),
            ));
            let _ = cx.infer_expr(&value, None);
            failed = true;
            continue;
        };
        if items.iter().any(|(i, _)| *i == index as u32) {
            diagnostics.push(Diagnostic::error(
                "E2103",
                label.text_range(),
                format!("the field `{text}` is given twice"),
            ));
            failed = true;
            continue;
        }
        let _ = cx.infer_promoted(&value, &fields[index].ty);
        check_body(
            refs,
            BodyContext::Initializer,
            env,
            value.syntax(),
            diagnostics,
        );
        items.push((index as u32, value));
    }
    if decl.base().is_none() {
        let missing: Vec<String> = fields
            .iter()
            .enumerate()
            .filter(|(i, f)| !f.has_default && !items.iter().any(|(at, _)| *at == *i as u32))
            .map(|(_, f)| format!("`{}`", f.name))
            .collect();
        if !missing.is_empty() {
            diagnostics.push(Diagnostic::error(
                "E2103",
                at,
                format!(
                    "the theme `{name}` gives no {}, which `Theme` has no default for",
                    missing.join(", ")
                ),
            ));
            failed = true;
        }
    }

    let Some(symbol) = symbol else {
        diagnostics.extend(cx.into_diagnostics());
        return;
    };
    let def = Def {
        name: name.clone(),
        kind: FunctionKind::Const,
        symbol: Some(symbol),
        module: env.module,
        into: None,
    };
    let mut b = env.behavior.borrow_mut();
    let func = if failed || has_errors(cx.diagnostics()) {
        unsupported(&mut b, def, TYPE_ERRORS, at)
    } else {
        lower_theme(&mut b, &cx, def, record, base, &items, at)
    };
    if b.theme(&name, func).is_err() {
        diagnostics.push(Diagnostic::error(
            "E2002",
            at,
            format!(
                "another theme is named `{name}`; a host switches to a theme by name, so a \
                 package names each one once"
            ),
        ));
    }
    drop(b);
    diagnostics.extend(cx.into_diagnostics());
}

fn ref_at(refs: &[ResolvedRef], at: TextRange) -> Option<Resolution> {
    refs.iter().find(|r| r.range == at).map(|r| r.to)
}
