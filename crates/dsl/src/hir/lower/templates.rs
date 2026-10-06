//! `template` declarations (§58) and the `part`s of a view (§57): a template
//! is a component without state, its parameters its inputs, placed by `use`
//! and inlined into its caller's view like a component; its member set, the
//! parts a view exposes, and the expansion of `use` proven finite (`E3601`).

use std::collections::{HashMap, HashSet};

use crate::ast::{AstNode, CompilationUnit, ComponentDecl, Item, Member, TemplateDecl, TypePath};
use crate::diag::{Diagnostic, Related};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxKind, TextRange};

/// One `part` of a component's or a template's view.
#[derive(Debug, Clone)]
pub(crate) struct PartInfo {
    pub(crate) name: String,
    /// Its node type.
    pub(crate) ty: TypePath,
    /// Its name's span.
    pub(crate) at: TextRange,
}

/// The parts of the view of `decl`, in source order.
pub(crate) fn parts_of(decl: &ComponentDecl) -> Vec<PartInfo> {
    let Some(view) = decl.view() else {
        return Vec::new();
    };
    view.syntax()
        .descendants()
        .into_iter()
        .filter_map(crate::ast::PartNode::cast)
        .filter_map(|part| {
            let name = part.name()?;
            Some(PartInfo {
                name: name.text().trim_start_matches("r#").to_string(),
                ty: part.ty()?,
                at: name.text_range(),
            })
        })
        .collect()
}

/// Reports each member of a template other than `slot`, `const`, `fn` and
/// `view`, and a template without a view (`E3601`): a template only
/// generates view structure. A part named twice in one view is `E2002`.
pub(super) fn check_members(decl: &TemplateDecl, diagnostics: &mut Vec<Diagnostic>) {
    let component = decl.as_component();
    for member in component.members() {
        let what = match member {
            Member::Slot(_) | Member::Const(_) | Member::Fn(_) | Member::View(_) => continue,
            Member::Input(_) => "an `input`: its parameters are its inputs",
            Member::State(_) => "a `state`",
            Member::Computed(_) => "a `computed`",
            Member::Event(_) => "an `event`",
            Member::Action(_) => "an `action`",
            Member::Task(_) => "a `task`",
            Member::Effect(_) => "an `effect`",
            Member::Resource(_) => "a `resource`",
            Member::Native(_) => "a `native` declaration",
        };
        diagnostics.push(Diagnostic::error(
            "E3601",
            member.syntax().text_range(),
            format!(
                "a template only generates view structure, so it has no {what}; it holds \
                 `slot`, `const`, `fn` and `view` members"
            ),
        ));
    }
    if component.view().is_none() {
        let at = decl
            .name()
            .map_or_else(|| decl.syntax().text_range(), |n| n.text_range());
        diagnostics.push(Diagnostic::error(
            "E3601",
            at,
            "a template has a `view`, the structure each `use` places",
        ));
    }
}

/// Reports a part name a view declares twice (`E2002`): callers name parts.
pub(super) fn check_parts(decl: &ComponentDecl, diagnostics: &mut Vec<Diagnostic>) {
    let mut seen: HashMap<String, TextRange> = HashMap::new();
    for part in parts_of(decl) {
        if let Some(first) = seen.get(&part.name) {
            let mut diagnostic = Diagnostic::error(
                "E2002",
                part.at,
                format!("the part `{}` is already declared in this view", part.name),
            );
            diagnostic
                .related
                .push(Related::new(*first, "first declared here"));
            diagnostics.push(diagnostic);
            continue;
        }
        seen.insert(part.name, part.at);
    }
}

/// Reports each `use` that would expand forever (`E3601`): one whose template
/// places, directly or through other templates of the unit, the template the
/// `use` is in. A template's arguments are run-time values, so no recursion
/// through `use` ends at compile time.
pub(super) fn check_recursion(
    cu: &CompilationUnit,
    refs: &[ResolvedRef],
    templates: &HashSet<SymbolId>,
    declared: &dyn Fn(&ComponentDecl) -> Option<SymbolId>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let heads: HashMap<TextRange, SymbolId> = refs
        .iter()
        .filter_map(|r| match r.to {
            Resolution::Symbol(id) if templates.contains(&id) => Some((r.range, id)),
            _ => None,
        })
        .collect();
    // Each template of the unit and the templates its view uses, with where.
    let mut uses: Vec<(SymbolId, Vec<(SymbolId, TextRange)>)> = Vec::new();
    for item in cu.items() {
        let decl = match item {
            Item::Export(e) => e.declaration(),
            other => Some(other),
        };
        let Some(Item::Template(t)) = decl else {
            continue;
        };
        let c = t.as_component();
        let Some(symbol) = declared(&c) else {
            continue;
        };
        let used = c
            .view()
            .into_iter()
            .flat_map(|v| v.syntax().descendants())
            .filter(|n| n.kind() == SyntaxKind::TemplateUse)
            .filter_map(crate::ast::TemplateUse::cast)
            .filter_map(|u| {
                let head = u.ty()?.segments().next()?;
                Some((*heads.get(&head.text_range())?, u.syntax().text_range()))
            })
            .collect();
        uses.push((symbol, used));
    }
    let edges: HashMap<SymbolId, &Vec<(SymbolId, TextRange)>> =
        uses.iter().map(|(s, u)| (*s, u)).collect();
    for (symbol, used) in &uses {
        for (callee, at) in used {
            if reaches(*callee, *symbol, &edges) {
                diagnostics.push(Diagnostic::error(
                    "E3601",
                    *at,
                    "this `use` places the template it is in again, so its expansion never \
                     ends; a template's arguments are run-time values and cannot stop it",
                ));
            }
        }
    }
}

/// Whether the template `from` places `to`, directly or through others.
fn reaches(
    from: SymbolId,
    to: SymbolId,
    edges: &HashMap<SymbolId, &Vec<(SymbolId, TextRange)>>,
) -> bool {
    let mut seen = HashSet::new();
    let mut stack = vec![from];
    while let Some(at) = stack.pop() {
        if at == to {
            return true;
        }
        if !seen.insert(at) {
            continue;
        }
        if let Some(next) = edges.get(&at) {
            stack.extend(next.iter().map(|(s, _)| *s));
        }
    }
    false
}
