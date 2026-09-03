//! Child/children/token accessors over a [`SyntaxNode`], shared by every typed AST
//! wrapper.
//!
//! A typed accessor never re-walks the whole tree: it filters the node's *direct*
//! children (nodes or tokens) by kind or by [`AstNode`] cast. This keeps the AST a
//! zero-allocation *view* — the green tree stays the single source of truth, and a
//! wrapper is just a `SyntaxNode` plus typed lenses onto its children.

use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use super::nodes::AstNode;

/// The first direct child node that casts to `N`.
pub(super) fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    parent.children().into_iter().find_map(N::cast)
}

/// Every direct child node that casts to `N`, in source order.
pub(super) fn children<N: AstNode>(parent: &SyntaxNode) -> impl Iterator<Item = N> {
    parent.children().into_iter().filter_map(N::cast)
}

/// The `n`-th (0-based) direct child node that casts to `N`, in source order.
/// Used where a node holds several children of the same kind and position — not
/// kind — distinguishes them (a `for`'s iterable vs key, an `if`'s then vs else).
pub(super) fn nth_child<N: AstNode>(parent: &SyntaxNode, n: usize) -> Option<N> {
    parent.children().into_iter().filter_map(N::cast).nth(n)
}

/// The first direct child token whose kind satisfies `pred`. Operators and
/// keyword markers ride as bare tokens between node children, so an operator
/// accessor reaches for the token directly rather than a wrapped node.
pub(super) fn token(parent: &SyntaxNode, pred: impl Fn(SyntaxKind) -> bool) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| pred(t.kind()))
}

/// The first direct child token that is one of the two identifier kinds (plain or
/// raw). Names in this grammar are bare `Ident`/`RawIdent` tokens, not wrapped
/// nodes, so a name accessor reaches for the token directly.
pub(super) fn name_token(parent: &SyntaxNode) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
}
