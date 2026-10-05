//! `task` members (§36): a task sees the component as it was when it started,
//! so it reads no state, input or computed — directly or through a `fn` — once
//! it may have suspended (`E4102`).

use std::collections::{BTreeSet, HashMap};

use crate::ast::{AstNode, CallExpr, ComponentDecl, Member, PathExpr};
use crate::diag::Diagnostic;
use crate::hir::effect::{EffectClass, EffectEnv};
use crate::hir::infer::unary_op_kind;
use crate::hir::nodes::ComponentSchema;
use crate::hir::reads::{ReadEnv, WithDerived, tracked_reads};
use crate::resolve::{Resolution, ResolvedRef};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

use super::ModuleEnv;

/// Checks the body of every `task` of the component `decl`.
pub(super) fn check_tasks(
    decl: &ComponentDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    schema: &ComponentSchema,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for member in decl.members() {
        if let Member::Task(task) = member
            && let Some(body) = task.body()
        {
            check_suspended_reads(refs, env, schema, body.syntax(), diagnostics);
        }
    }
}

/// Reports each reactive value `body` reads where the task may already have
/// suspended: after a suspension point (an `await`, or a call of a task), or
/// inside a loop that suspends, whose next round runs after it.
fn check_suspended_reads(
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    schema: &ComponentSchema,
    body: &SyntaxNode,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let index: HashMap<TextRange, Resolution> = refs.iter().map(|r| (r.range, r.to)).collect();
    let mut suspensions = Vec::new();
    collect_suspensions(&index, env, body, &mut suspensions);
    let Some(first) = suspensions.iter().map(|s| s.end()).min() else {
        return;
    };
    // The parts of a loop that run again after its body suspended.
    let again: Vec<TextRange> = body
        .descendants()
        .into_iter()
        .filter(|node| {
            matches!(
                node.kind(),
                SyntaxKind::WhileStmt | SyntaxKind::LoopStmt | SyntaxKind::ForStmt
            ) && suspensions
                .iter()
                .any(|s| node.text_range().contains_range(*s))
        })
        .filter_map(|node| {
            let start = match node.kind() {
                SyntaxKind::ForStmt => node
                    .children()
                    .into_iter()
                    .find(|c| c.kind() == SyntaxKind::Block)?
                    .text_range()
                    .start(),
                _ => node.text_range().start(),
            };
            Some(TextRange::new(start, node.text_range().end()))
        })
        .collect();
    let derived = |symbol| {
        schema
            .derived
            .of(symbol)
            .is_some_and(|reads| !reads.is_empty())
    };
    let reads = WithDerived {
        env,
        derived: &derived,
    };
    let mut reported = BTreeSet::new();
    for (symbol, token) in tracked_reads(refs, &reads, body) {
        let at = token.text_range();
        let late = at.start() >= first || again.iter().any(|r| r.contains_range(at));
        if !late || !reported.insert(symbol) {
            continue;
        }
        let base = env.reactive_source(&Resolution::Symbol(symbol)).is_some();
        let what = if !base {
            format!("calls `{}`, which reads component state,", token.text())
        } else {
            format!("reads `{}`", token.text())
        };
        diagnostics.push(Diagnostic::error(
            "E4102",
            at,
            format!(
                "the task {what} where it may have suspended; a task sees the component as \
                 it started, so read it before the first `await` or pass it in as an argument"
            ),
        ));
    }
}

/// The span of every suspension point under `node`: each `await` and each
/// call of a task.
fn collect_suspensions(
    index: &HashMap<TextRange, Resolution>,
    env: &ModuleEnv<'_>,
    node: &SyntaxNode,
    out: &mut Vec<TextRange>,
) {
    let suspends = match node.kind() {
        SyntaxKind::UnaryExpr => unary_op_kind(node) == Some(SyntaxKind::AwaitKw),
        SyntaxKind::CallExpr => calls_task(index, env, node),
        _ => false,
    };
    if suspends {
        out.push(node.text_range());
    }
    for child in node.children() {
        collect_suspensions(index, env, &child, out);
    }
}

pub(super) fn calls_task(
    index: &HashMap<TextRange, Resolution>,
    env: &ModuleEnv<'_>,
    call: &SyntaxNode,
) -> bool {
    if let Some((class, _)) = env.native_call(call.text_range()) {
        return class == EffectClass::Task;
    }
    CallExpr::cast(call.clone())
        .and_then(|c| c.callee())
        .filter(|c| c.syntax().kind() == SyntaxKind::PathExpr)
        .and_then(|c| PathExpr::cast(c.syntax().clone()))
        .and_then(|p| p.segments().next())
        .and_then(|head| index.get(&head.text_range()))
        .and_then(|to| env.callee_effect(to))
        == Some(EffectClass::Task)
}
