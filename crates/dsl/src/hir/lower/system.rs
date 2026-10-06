//! `system` declarations: their member set, the hooks of the native traits they
//! implement, and the package's system run order.

use std::collections::{BTreeSet, HashSet};

use viso_behavior::native::NativeId;

use crate::ast::{AstNode, Member, SystemDecl, SystemOrder};
use crate::diag::{Diagnostic, Related};
use crate::hir::component::MemberEnv;
use crate::hir::infer::{InferCx, TypeEnv};
use crate::resolve::SymbolId;
use crate::syntax::{SyntaxKind, TextRange};

use super::{ModuleEnv, Ty, name_of};

/// One system of the package and the ordering its attributes declare.
pub(super) struct SystemNode {
    symbol: SymbolId,
    name: String,
    module: usize,
    /// Each system it must run after, with the span naming it.
    after: Vec<(SymbolId, TextRange)>,
    /// Each system it must run before, with the span naming it.
    before: Vec<(SymbolId, TextRange)>,
}

/// Reports each `view`, `event`, `slot` and `effect` member of a system, and
/// each `start` in one (`E9109`): a system is driven by its hooks, not
/// mounted.
pub(super) fn check_members(decl: &SystemDecl, diagnostics: &mut Vec<Diagnostic>) {
    for member in decl.members() {
        for start in member
            .syntax()
            .descendants()
            .into_iter()
            .filter(|node| node.kind() == SyntaxKind::StartStmt)
        {
            diagnostics.push(Diagnostic::error(
                "E9109",
                start.text_range(),
                "a system starts no task: its hooks run inside the tick, which hands no task \
                 result back",
            ));
        }
        let what = match member {
            Member::View(_) => "a `view`",
            Member::Event(_) => "an `event`",
            Member::Slot(_) => "a `slot`",
            Member::Effect(_) => "an `effect`",
            Member::Resource(_) => "a `resource`",
            _ => continue,
        };
        diagnostics.push(Diagnostic::error(
            "E9109",
            member.syntax().text_range(),
            format!("a system declares no {what}: it runs through its hooks and is never mounted"),
        ));
    }
}

/// The hooks of the traits `decl` implements, each bound to the member action
/// of its name. Each bound must name a native scheduler trait, at most once,
/// and each hook needs an `action` taking exactly its parameters and
/// returning nothing (`E2201`); two bounds declaring one hook are ambiguous
/// (`E2202`).
pub(super) fn hooks(
    decl: &SystemDecl,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<(NativeId, SymbolId)> {
    let system = name_of(decl.name());
    let mut hooks: Vec<(NativeId, SymbolId)> = Vec::new();
    let mut traits: HashSet<NativeId> = HashSet::new();
    let mut bound_names: Vec<(&'static str, TextRange)> = Vec::new();
    for bound in decl.implements() {
        let Some(head) = bound.segments().next() else {
            continue;
        };
        let at = bound.syntax().text_range();
        let text = bound.syntax().text().to_string().trim().to_string();
        let entry = match env.scope.nominal.get(&head.text_range()) {
            Some(Ty::Native(id)) => env.natives.native_trait_by_id(*id),
            Some(_) => None,
            // The resolver reported the unresolved name.
            None => continue,
        };
        let Some(entry) = entry else {
            diagnostics.push(Diagnostic::error(
                "E2201",
                at,
                format!("`{text}` is not a trait a system can implement"),
            ));
            continue;
        };
        if !traits.insert(entry.id) {
            diagnostics.push(Diagnostic::error(
                "E2201",
                at,
                format!("`{system}` implements `{text}` twice"),
            ));
            continue;
        }
        for hook in entry.native_trait.hooks {
            if let Some((_, first)) = bound_names.iter().find(|(n, _)| *n == hook.name) {
                let mut diagnostic = Diagnostic::error(
                    "E2202",
                    at,
                    format!("two traits of `{system}` declare the hook `{}`", hook.name),
                );
                diagnostic
                    .related
                    .push(Related::new(*first, "the other trait is implemented here"));
                diagnostics.push(diagnostic);
                continue;
            }
            bound_names.push((hook.name, at));
            let package = env.package_types();
            let expected: Vec<Ty> = hook
                .params
                .iter()
                .map(|p| Ty::from_schema(&p.ty, &package))
                .collect();
            let cx = InferCx::new(&[], env);
            let signature = format!(
                "action {}({})",
                hook.name,
                hook.params
                    .iter()
                    .zip(&expected)
                    .map(|(p, ty)| format!("{}: {}", p.name, cx.describe(ty)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let action = decl.members().find_map(|m| match m {
                Member::Action(a) if name_of(a.name()) == hook.name => Some(a),
                _ => None,
            });
            let Some(action) = action else {
                let mut diagnostic = Diagnostic::error(
                    "E2201",
                    at,
                    format!("`{system}` implements `{text}` but declares no `{signature}`"),
                );
                diagnostic
                    .notes
                    .push(format!("add `{signature} {{ ... }}`"));
                diagnostics.push(diagnostic);
                continue;
            };
            let params: Vec<Ty> = action
                .params()
                .iter()
                .map(|p| env.annotation_of(p.syntax()))
                .collect();
            let returns = action
                .return_type()
                .and_then(|r| r.ty())
                .map(|ty| env.scope.annotation(ty.syntax()));
            let unit = returns.as_ref().is_none_or(|ty| *ty == Ty::Unit);
            if params != expected || !unit {
                let at = action
                    .name()
                    .map_or(action.syntax().text_range(), |n| n.text_range());
                let mut diagnostic = Diagnostic::error(
                    "E2201",
                    at,
                    format!("the hook `{text}.{}` is `{signature}`", hook.name),
                );
                diagnostic.related.push(Related::new(
                    bound.syntax().text_range(),
                    "required by this bound",
                ));
                diagnostics.push(diagnostic);
                continue;
            }
            if let Some(symbol) = env.member_symbol(hook.name) {
                hooks.push((entry.hook_id(hook.name), symbol));
            }
        }
    }
    hooks
}

/// The ordering `decl`'s `@after`/`@before` attributes declare; an argument
/// that names no system is `E2001`.
pub(super) fn node(
    decl: &SystemDecl,
    symbol: SymbolId,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> SystemNode {
    let mut node = SystemNode {
        symbol,
        name: name_of(decl.name()),
        module: env.module,
        after: Vec::new(),
        before: Vec::new(),
    };
    for (order, attr, names) in decl.ordering() {
        let attribute = match order {
            SystemOrder::After => "@after",
            SystemOrder::Before => "@before",
        };
        if names.is_empty() {
            diagnostics.push(Diagnostic::error(
                "E2001",
                attr.text_range(),
                format!("`{attribute}` names the systems to order against"),
            ));
        }
        for name in names {
            let Some(name) = name else {
                diagnostics.push(Diagnostic::error(
                    "E2001",
                    attr.text_range(),
                    format!("each argument of `{attribute}` is the name of a system"),
                ));
                continue;
            };
            let at = name.text_range();
            let target = match env.scope.nominal.get(&at) {
                Some(Ty::Named(id, ..)) => Some(*id),
                // The resolver reported the unresolved name.
                None => continue,
                Some(_) => None,
            };
            let Some(target) = target.filter(|t| env.decls.systems.contains(t)) else {
                diagnostics.push(Diagnostic::error(
                    "E2001",
                    at,
                    format!("`{}` is not a system", name.text()),
                ));
                continue;
            };
            match order {
                SystemOrder::After => node.after.push((target, at)),
                SystemOrder::Before => node.before.push((target, at)),
            }
        }
    }
    node
}

/// The package's systems in run order: every `@after`/`@before` respected,
/// otherwise declaration order. A cycle is `E9101`, reported in the module of
/// the system its first edge belongs to; its systems follow the ordered ones,
/// in declaration order.
pub(super) fn order(nodes: &[SystemNode], per_module: &mut [Vec<Diagnostic>]) -> Vec<SymbolId> {
    let index = |symbol: SymbolId| nodes.iter().position(|n| n.symbol == symbol);
    // `edges[i]` are the systems that must run after system `i`, each with the
    // span that asks for it and the system that span belongs to.
    let mut edges: Vec<Vec<(usize, TextRange, usize)>> = vec![Vec::new(); nodes.len()];
    let mut preds = vec![0usize; nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        for &(target, at) in &node.after {
            if let Some(t) = index(target) {
                edges[t].push((i, at, i));
                preds[i] += 1;
            }
        }
        for &(target, at) in &node.before {
            if let Some(t) = index(target) {
                edges[i].push((t, at, i));
                preds[t] += 1;
            }
        }
    }
    let mut ready: BTreeSet<usize> = (0..nodes.len()).filter(|&i| preds[i] == 0).collect();
    let mut order = Vec::with_capacity(nodes.len());
    let mut placed = vec![false; nodes.len()];
    while let Some(i) = ready.pop_first() {
        placed[i] = true;
        order.push(nodes[i].symbol);
        for &(next, _, _) in &edges[i] {
            preds[next] -= 1;
            if preds[next] == 0 {
                ready.insert(next);
            }
        }
    }
    if order.len() < nodes.len() {
        report_cycle(nodes, &edges, &placed, per_module);
        order.extend(
            nodes
                .iter()
                .zip(&placed)
                .filter(|(_, placed)| !**placed)
                .map(|(n, _)| n.symbol),
        );
    }
    order
}

/// Reports one cycle among the systems left unplaced (`E9101`): every one of
/// them has an unplaced predecessor, so walking predecessors from the first
/// reaches a system twice.
fn report_cycle(
    nodes: &[SystemNode],
    edges: &[Vec<(usize, TextRange, usize)>],
    placed: &[bool],
    per_module: &mut [Vec<Diagnostic>],
) {
    let Some(start) = placed.iter().position(|p| !p) else {
        return;
    };
    let predecessor = |of: usize| {
        edges.iter().enumerate().find_map(|(from, out)| {
            if placed[from] {
                return None;
            }
            out.iter()
                .find(|e| e.0 == of)
                .map(|&(_, at, owner)| (from, at, owner))
        })
    };
    // `path[j]` is the edge into `seen[j]` from `seen[j + 1]`.
    let mut seen = vec![start];
    let mut path: Vec<(usize, TextRange, usize)> = Vec::new();
    let first = loop {
        let Some(step) = predecessor(seen[seen.len() - 1]) else {
            return;
        };
        path.push(step);
        if let Some(k) = seen.iter().position(|&n| n == step.0) {
            break k;
        }
        seen.push(step.0);
    };
    // The walk ran against the edges; the cycle's run order is the reverse.
    let mut cycle: Vec<_> = path[first..].to_vec();
    cycle.reverse();
    let mut names: Vec<&str> = cycle
        .iter()
        .map(|&(from, _, _)| &*nodes[from].name)
        .collect();
    names.push(names[0]);
    let (_, primary, owner) = cycle[0];
    let mut diagnostic = Diagnostic::error(
        "E9101",
        primary,
        format!(
            "the systems `{}` are ordered in a cycle: none can run first",
            names.join("` → `")
        ),
    );
    for &(_, at, edge_owner) in &cycle[1..] {
        if nodes[edge_owner].module == nodes[owner].module {
            diagnostic
                .related
                .push(Related::new(at, "the cycle continues here"));
        }
    }
    if let Some(module) = per_module.get_mut(nodes[owner].module) {
        module.push(diagnostic);
    }
}
