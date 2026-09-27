//! What a typed expression may carry a `Percent` component from (see
//! [`crate::hir::percent`]), and the definitions a body walk gives its names.

use super::{InferCx, binary_op_kind, child_exprs, first_child_expr};
use crate::ast::AstNode;
use crate::hir::percent::Carry;
use crate::hir::ty::Ty;
use crate::resolve::Resolution;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

impl InferCx<'_> {
    /// What the value of the already typed expression `node` may carry a `Percent`
    /// component from.
    pub(crate) fn carry(&self, node: &SyntaxNode) -> Carry {
        let mut carry = Carry::default();
        self.carry_into(node, &mut carry);
        carry
    }

    /// Takes the definitions the walk has given names so far: each local's
    /// initializer, destructured value and assignments, and each assignment to a
    /// symbol.
    pub(crate) fn take_percent_defs(&mut self) -> Vec<(Resolution, Carry)> {
        std::mem::take(&mut self.percent_defs)
    }

    /// Notes an expression whose own type is `Percent`.
    pub(super) fn note_percent(&mut self, ty: &Ty, node: &SyntaxNode) {
        if *ty == Ty::Percent {
            self.percent_typed.insert(node.text_range());
        }
    }

    /// Gives every local `pattern` binds the value carrying `carry`.
    pub(crate) fn define_pattern(&mut self, pattern: &SyntaxNode, carry: &Carry) {
        for token in tokens(pattern) {
            if let Some(local @ Resolution::Local(_)) = self.refs.get(&token.text_range()).copied()
            {
                self.percent_defs.push((local, carry.clone()));
            }
        }
    }

    /// Gives the name an assignment `target` writes (through any field or index) the
    /// value `value`.
    pub(super) fn define_target(&mut self, target: &SyntaxNode, value: &SyntaxNode) {
        let mut root = target.clone();
        while matches!(
            root.kind(),
            SyntaxKind::FieldExpr | SyntaxKind::IndexExpr | SyntaxKind::ParenExpr
        ) {
            let Some(inner) = first_child_expr(&root) else {
                return;
            };
            root = inner.syntax().clone();
        }
        if root.kind() != SyntaxKind::PathExpr {
            return;
        }
        let Some(name) = head(&root).and_then(|t| self.refs.get(&t.text_range()).copied()) else {
            return;
        };
        let carry = self.carry(value);
        self.percent_defs.push((name, carry));
    }

    fn carry_into(&self, node: &SyntaxNode, carry: &mut Carry) {
        if self.percent_typed.contains(&node.text_range()) {
            carry.spelled.get_or_insert(node.text_range());
            return;
        }
        match node.kind() {
            SyntaxKind::PathExpr => {
                if let Some(name) = head(node).and_then(|t| self.refs.get(&t.text_range())) {
                    carry.join(Carry {
                        spelled: None,
                        names: vec![*name],
                    });
                }
            }
            SyntaxKind::FieldExpr
            | SyntaxKind::OptionalFieldExpr
            | SyntaxKind::IndexExpr
            | SyntaxKind::TryExpr
            | SyntaxKind::ParenExpr
            | SyntaxKind::UnaryExpr => {
                if let Some(inner) = first_child_expr(node) {
                    self.carry_into(inner.syntax(), carry);
                }
            }
            SyntaxKind::BinaryExpr if is_test(binary_op_kind(node)) => {}
            SyntaxKind::BinaryExpr | SyntaxKind::TupleExpr | SyntaxKind::ListExpr => {
                for inner in child_exprs(node) {
                    self.carry_into(inner.syntax(), carry);
                }
            }
            SyntaxKind::RecordExpr => self.record_into(node, carry),
            SyntaxKind::IfExpr | SyntaxKind::IfStmt => self.if_into(node, carry),
            SyntaxKind::MatchExpr | SyntaxKind::MatchStmt => self.match_into(node, carry),
            SyntaxKind::BlockExpr => {
                if let Some(block) = child_of(node, SyntaxKind::Block) {
                    self.block_into(&block, carry);
                }
            }
            _ => {}
        }
    }

    /// A record literal: its field values, its spread base, and — when it omits a
    /// field its record defaults — the record's defaults.
    fn record_into(&self, node: &SyntaxNode, carry: &mut Carry) {
        let mut given = Vec::new();
        for field in node
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::RecordExprField)
        {
            let label = tokens(&field).into_iter().find(is_ident);
            match first_child_expr(&field) {
                Some(value) => self.carry_into(value.syntax(), carry),
                None => {
                    if let Some(name) = label.as_ref().and_then(|t| self.refs.get(&t.text_range()))
                    {
                        carry.join(Carry {
                            spelled: None,
                            names: vec![*name],
                        });
                    }
                }
            }
            given.extend(label.map(|t| t.text().to_string()));
        }
        let base = first_child_expr(node);
        if let Some(base) = &base {
            self.carry_into(base.syntax(), carry);
        }
        let record = node
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .take_while(|t| t.kind() != SyntaxKind::LBrace)
            .filter(is_ident)
            .last()
            .and_then(|t| self.symbol_at(t.text_range()));
        if base.is_none()
            && let Some(record) = record
            && let Some(fields) = self.env.record_fields(record)
            && fields
                .iter()
                .any(|f| f.has_default && !given.contains(&f.name))
        {
            carry.join(Carry {
                spelled: None,
                names: vec![Resolution::Symbol(record)],
            });
        }
    }

    /// An `if`: its branches, not its condition.
    fn if_into(&self, node: &SyntaxNode, carry: &mut Carry) {
        let mut condition = true;
        for child in node.children() {
            match child.kind() {
                SyntaxKind::Block => self.block_into(&child, carry),
                SyntaxKind::IfExpr | SyntaxKind::IfStmt if !condition => {
                    self.if_into(&child, carry);
                }
                _ => {}
            }
            condition = false;
        }
    }

    /// A `match`: its arm values, not its scrutinee or guards.
    fn match_into(&self, node: &SyntaxNode, carry: &mut Carry) {
        for arm in node
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::MatchArm)
        {
            if let Some(block) = child_of(&arm, SyntaxKind::Block) {
                self.block_into(&block, carry);
            } else if let Some(value) = child_exprs(&arm).last() {
                self.carry_into(value.syntax(), carry);
            }
        }
    }

    /// A block: its tail value.
    fn block_into(&self, block: &SyntaxNode, carry: &mut Carry) {
        let Some(tail) = block.children().into_iter().last() else {
            return;
        };
        match tail.kind() {
            SyntaxKind::ExprStmt if !tokens(&tail).iter().any(|t| t.kind() == SyntaxKind::Semi) => {
                if let Some(value) = first_child_expr(&tail) {
                    self.carry_into(value.syntax(), carry);
                }
            }
            SyntaxKind::IfStmt | SyntaxKind::MatchStmt => self.carry_into(&tail, carry),
            _ => {}
        }
    }
}

/// The head segment of a path expression.
fn head(path: &SyntaxNode) -> Option<SyntaxToken> {
    path.descendants_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(is_ident)
}

/// Whether a binary operator yields a truth value rather than combining its operands.
fn is_test(op: Option<SyntaxKind>) -> bool {
    matches!(
        op,
        Some(
            SyntaxKind::EqEq
                | SyntaxKind::Neq
                | SyntaxKind::Lt
                | SyntaxKind::Le
                | SyntaxKind::Gt
                | SyntaxKind::Ge
                | SyntaxKind::AmpAmp
                | SyntaxKind::PipePipe
        )
    )
}

fn is_ident(token: &SyntaxToken) -> bool {
    matches!(token.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent)
}

/// Every token under `node`.
fn tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
    node.descendants_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .collect()
}

fn child_of(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().into_iter().find(|c| c.kind() == kind)
}
