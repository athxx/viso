//! `start` statements (§39): the policy a named task slot runs under and the
//! handler items, each at most once (`E4302`).

use viso_behavior::TaskPolicy;

use crate::ast::{AstNode, Expr, PathExpr, StartStmt};
use crate::diag::Diagnostic;
use crate::hir::infer::{call_args, child_exprs, parse_int_literal};
use crate::syntax::{SyntaxKind, SyntaxNode};

/// The slot policy of `start`: its `policy = [..]`, `keep_latest` for a named
/// slot without one, `None` for an unnamed start (every one runs). Each
/// problem is reported into `diagnostics` (`E4302`); a malformed policy falls
/// back to the default.
pub(crate) fn slot_policy(
    start: &StartStmt,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<TaskPolicy> {
    let named = start.slot().is_some();
    let policies: Vec<_> = start
        .handlers()
        .map(|h| h.policies().collect())
        .unwrap_or_default();
    let mut policy = None;
    for (i, item) in policies.iter().enumerate() {
        let at = item.syntax().text_range();
        if i > 0 {
            diagnostics.push(Diagnostic::error(
                "E4302",
                at,
                "a `start` takes one `policy = [..];`",
            ));
            continue;
        }
        if !named {
            diagnostics.push(Diagnostic::error(
                "E4302",
                at,
                "a policy governs a named task slot; name it with `as`",
            ));
            continue;
        }
        policy = item.value().and_then(|list| parse(&list, diagnostics));
    }
    named.then(|| policy.unwrap_or(TaskPolicy::KeepLatest))
}

/// Reports a `success`, `error` or `cancelled` handler given twice (`E4302`).
pub(crate) fn check_handlers(start: &StartStmt, diagnostics: &mut Vec<Diagnostic>) {
    let Some(handlers) = start.handlers() else {
        return;
    };
    let mut seen = [false; 3];
    for item in handlers.syntax().children() {
        let (index, name) = match item.kind() {
            SyntaxKind::StartSuccess => (0, "success"),
            SyntaxKind::StartError => (1, "error"),
            SyntaxKind::StartCancelled => (2, "cancelled"),
            _ => continue,
        };
        if std::mem::replace(&mut seen[index], true) {
            diagnostics.push(Diagnostic::error(
                "E4302",
                item.text_range(),
                format!("a `start` takes one `{name}` handler"),
            ));
        }
    }
}

/// The one policy of the list `list`.
fn parse(list: &Expr, diagnostics: &mut Vec<Diagnostic>) -> Option<TaskPolicy> {
    let at = list.syntax().text_range();
    let mut report = |message: &str| {
        diagnostics.push(Diagnostic::error("E4302", at, message.to_owned()));
        None
    };
    if list.syntax().kind() != SyntaxKind::ListExpr {
        return report("a task policy is a list: `policy = [TaskPolicy::keep_latest];`");
    }
    let items = child_exprs(list.syntax());
    let [item] = &items[..] else {
        return report(
            "a task slot runs under one policy: `keep_latest`, `drop_new`, `queue` or \
             `parallel(n)`",
        );
    };
    match item.syntax().kind() {
        SyntaxKind::PathExpr => {
            let path = segments(item.syntax());
            let [head, name] = &path[..] else {
                return report(UNKNOWN);
            };
            match (head.as_str(), name.as_str()) {
                ("TaskPolicy", "keep_latest") => Some(TaskPolicy::KeepLatest),
                ("TaskPolicy", "drop_new") => Some(TaskPolicy::DropNew),
                ("TaskPolicy", "queue") => Some(TaskPolicy::Queue),
                _ => report(UNKNOWN),
            }
        }
        SyntaxKind::CallExpr => {
            let callee = item
                .syntax()
                .children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::PathExpr);
            let is_parallel = callee.is_some_and(|c| segments(&c) == ["TaskPolicy", "parallel"]);
            if !is_parallel {
                return report(UNKNOWN);
            }
            let args = call_args(item.syntax());
            let count = match &args[..] {
                [arg] if arg.syntax().kind() == SyntaxKind::LiteralExpr => {
                    parse_int_literal(arg.syntax().text().to_string().trim())
                }
                _ => None,
            };
            match count
                .and_then(|n| u32::try_from(n).ok())
                .filter(|&n| n >= 1)
            {
                Some(n) => Some(TaskPolicy::Parallel(n)),
                None => report("`TaskPolicy::parallel(n)` runs a literal `n` of at least 1"),
            }
        }
        _ => report(UNKNOWN),
    }
}

const UNKNOWN: &str = "a task policy is `TaskPolicy::keep_latest`, `TaskPolicy::drop_new`, \
                       `TaskPolicy::queue` or `TaskPolicy::parallel(n)`";

fn segments(path: &SyntaxNode) -> Vec<String> {
    PathExpr::cast(path.clone())
        .map(|p| p.segments().map(|s| s.text().to_string()).collect())
        .unwrap_or_default()
}
