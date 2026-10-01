//! What may be written: an assignment target is a `state` or a mutable local, or a
//! field or element of one (`E2110` otherwise), and a `bind` source is a State Lens —
//! a `state` of the component or a field or element of one (`E3107` otherwise).

use super::{INTEGER_TYPES, InferCx, first_child_expr, is_integer_ty};
use crate::ast::{AssignablePath, AstNode, Expr, PathExpr};
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::resolve::{Resolution, SymbolKind};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

impl InferCx<'_> {
    /// Marks the local bound at `at` (a `mut` parameter's name token) mutable.
    pub fn mark_mutable(&mut self, at: TextRange) {
        if let Some(Resolution::Local(slot)) = self.refs.get(&at).copied() {
            self.mutable.insert(slot);
        }
    }

    /// Marks every local `pattern` binds mutable (a `let mut` or `mut` closure
    /// parameter pattern).
    pub(super) fn mark_mutable_in(&mut self, pattern: &SyntaxNode) {
        for token in pattern
            .descendants_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
        {
            self.mark_mutable(token.text_range());
        }
    }

    /// Reports an assignment `target` that is not writable (`E2110`): its root, under
    /// any field access and indexing, must name a `state` or a mutable local.
    pub(super) fn check_writable(&mut self, target: &SyntaxNode) {
        let mut root = target.clone();
        while matches!(root.kind(), SyntaxKind::FieldExpr | SyntaxKind::IndexExpr) {
            let Some(inner) = first_child_expr(&root) else {
                return;
            };
            root = inner.syntax().clone();
        }
        let segments = path_segments(&root);
        let [name] = segments.as_slice() else {
            self.diagnostics.push(Diagnostic::error(
                "E2110",
                target.text_range(),
                "only a `state`, a mutable local, or a field or element of one can be assigned",
            ));
            return;
        };
        let reason = match self.refs.get(&name.text_range()).copied() {
            Some(Resolution::Local(slot)) if !self.mutable.contains(&slot) => {
                "it is an immutable local; declare it `let mut` to assign it"
            }
            Some(Resolution::Symbol(id)) => match self.env.symbol_kind(id) {
                Some(SymbolKind::Input) => {
                    "an input is read-only in its component; `emit` an event to request a change"
                }
                Some(SymbolKind::Computed) => "a `computed` is derived from its sources",
                Some(SymbolKind::Const) => "it is a `const`",
                Some(SymbolKind::State) | None => return,
                Some(_) => "it is not a variable",
            },
            Some(Resolution::Env) => "the adaptive environment is read-only; a view only reads it",
            _ => return,
        };
        let message = format!("cannot assign to `{}`: {reason}", name.text());
        self.diagnostics
            .push(Diagnostic::error("E2110", target.text_range(), message));
    }

    /// Types the `bind` source `path`, reporting one that is no State Lens (`E3107`):
    /// its head must name a `state`, and each `.label` then selects a record field or
    /// tuple element and each `[index]` a list element.
    pub(crate) fn infer_lens(&mut self, path: &AssignablePath) -> Ty {
        let node = path.syntax();
        let mut parts = node.children_with_tokens().into_iter();
        let Some(head) = parts
            .by_ref()
            .filter_map(|e| e.as_token().cloned())
            .find(is_name)
        else {
            return Ty::Unknown;
        };
        let reason = match self.refs.get(&head.text_range()).copied() {
            Some(Resolution::Local(_)) => Some("a local binding"),
            Some(Resolution::Symbol(id)) => match self.env.symbol_kind(id) {
                Some(SymbolKind::State) | None => None,
                Some(SymbolKind::Input) => Some("an input, which its component cannot write"),
                Some(SymbolKind::Computed) => Some("a `computed`"),
                Some(SymbolKind::Const) => Some("a `const`"),
                Some(_) => Some("not a value"),
            },
            Some(Resolution::Native(_)) => Some("a native"),
            Some(Resolution::Env) => Some("the adaptive environment, which a view only reads"),
            None => None,
        };
        if let Some(reason) = reason {
            let message = format!(
                "`bind` writes back to its source, but `{}` is {reason}; bind a `state`",
                head.text()
            );
            self.diagnostics
                .push(Diagnostic::error("E3107", node.text_range(), message));
        }
        let mut ty = match self.refs.get(&head.text_range()).copied() {
            Some(to) if reason.is_none() => self.resolution_ty(&to),
            _ => Ty::Unknown,
        };
        let mut after_dot = false;
        for part in parts {
            if let Some(token) = part.as_token() {
                match token.kind() {
                    SyntaxKind::Dot => after_dot = true,
                    _ if after_dot && is_name(token) => {
                        after_dot = false;
                        ty = match ty {
                            Ty::Unknown => Ty::Unknown,
                            recv => self.member_ty(&recv, token, None).unwrap_or(Ty::Unknown),
                        };
                    }
                    _ => {}
                }
            } else if let Some(index) = part.as_node().and_then(|n| Expr::cast(n.clone())) {
                let index_ty = self.infer_expr(&index, None);
                if !matches!(index_ty, Ty::Unknown | Ty::Never) && !is_integer_ty(&index_ty) {
                    let actual = self.describe(&index_ty);
                    let message = format!("a list index is an integer, found `{actual}`");
                    self.diagnostics.push(
                        Diagnostic::error("E2103", index.syntax().text_range(), message)
                            .expecting(INTEGER_TYPES, actual),
                    );
                }
                ty = match ty {
                    Ty::List(elem) => *elem,
                    _ => Ty::Unknown,
                };
            }
        }
        ty
    }
}

/// The name segments of a single path expression; empty for any other expression.
fn path_segments(node: &SyntaxNode) -> Vec<SyntaxToken> {
    PathExpr::cast(node.clone())
        .map(|p| p.segments().collect())
        .unwrap_or_default()
}

fn is_name(token: &SyntaxToken) -> bool {
    matches!(token.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent)
}

/// Whether `node` carries a `mut` of its own (not one of a nested node).
pub(super) fn has_mut(node: &SyntaxNode) -> bool {
    node.children_with_tokens()
        .into_iter()
        .any(|e| e.as_token().is_some_and(|t| t.kind() == SyntaxKind::MutKw))
}
