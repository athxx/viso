//! Where a bare enum variant may stand for its qualified path.
//!
//! A property's value (`align: center`), a primary argument and a record
//! field's value (`Sizing::fill { .. }`'s fields, `P { edge: top }`) know the
//! type they expect, so a one-segment name there may be a variant of that
//! type's enum: `center` for `Align::center`, `fill { weight: 2.0 }` for
//! `Sizing::fill { weight: 2.0 }`. The position carries through the tails of
//! `if`/`match` written there. Parentheses opt out: `(center)` always reads the
//! value `center`.

use crate::syntax::{SyntaxKind, SyntaxNode};

use super::{AstNode, PathExpr};

/// Whether `value` is a bare name (`center`) or a record literal with a bare
/// head (`fill { .. }`) written where an expected enum may supply its owner.
pub fn is_shorthand_site(value: &SyntaxNode) -> bool {
    let bare = match value.kind() {
        SyntaxKind::PathExpr => {
            PathExpr::cast(value.clone()).is_some_and(|p| {
                let mut segments = p.segments();
                segments
                    .next()
                    .is_some_and(|s| matches!(s.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
                    && segments.next().is_none()
            }) && !has_token(value, SyntaxKind::ColonColon)
        }
        SyntaxKind::RecordExpr => bare_record_head(value),
        _ => false,
    };
    bare && in_value_position(value)
}

/// Whether a record literal's head is one name, without generic arguments.
fn bare_record_head(record: &SyntaxNode) -> bool {
    let mut names = 0;
    for element in record.children_with_tokens() {
        if let Some(node) = element.as_node()
            && node.kind() == SyntaxKind::GenericCallArgs
        {
            return false;
        }
        let Some(token) = element.as_token() else {
            continue;
        };
        match token.kind() {
            SyntaxKind::LBrace => break,
            SyntaxKind::Ident | SyntaxKind::RawIdent => names += 1,
            SyntaxKind::ColonColon => return false,
            _ => {}
        }
    }
    names == 1
}

fn has_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    node.children_with_tokens()
        .into_iter()
        .any(|e| e.as_token().is_some_and(|t| t.kind() == kind))
}

/// Whether `node` is the value of a property binding, primary argument or
/// record field, directly or as a tail of an `if`/`match` that is.
fn in_value_position(node: &SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        SyntaxKind::PropertyBinding | SyntaxKind::PrimaryArgument => true,
        SyntaxKind::RecordExprField => !has_token(&parent, SyntaxKind::DotDot),
        SyntaxKind::ExprStmt | SyntaxKind::BlockExpr => in_value_position(&parent),
        SyntaxKind::Block => parent.children().last() == Some(node) && in_value_position(&parent),
        SyntaxKind::IfExpr | SyntaxKind::IfStmt => {
            let condition = parent
                .children()
                .into_iter()
                .find(|c| c.kind() != SyntaxKind::Block);
            condition.is_none_or(|c| c != *node) && in_value_position(&parent)
        }
        SyntaxKind::MatchArm => {
            let arrow = parent
                .children_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .find(|t| t.kind() == SyntaxKind::FatArrow);
            arrow.is_some_and(|a| node.text_range().start() >= a.text_range().end())
                && in_value_position(&parent)
        }
        SyntaxKind::MatchExpr | SyntaxKind::MatchStmt => {
            node.kind() == SyntaxKind::MatchArm && in_value_position(&parent)
        }
        _ => false,
    }
}
