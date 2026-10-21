//! The methods of `NodeRef<T>` (U7.6): `focus`, `blur` and
//! `scroll_into_view` on any named node, `scroll_to` and `scroll_by` on a
//! `Scroll`, `scroll_to_index` and `scroll_to_key` on a `VirtualList`. Each
//! queues a request the view applies once the transaction
//! commits and its structure settles, so a node the same action mounts is
//! reached; the value is `()`.

use viso_behavior::NodeAction;

use super::InferCx;
use crate::ast::Expr;
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::syntax::{SyntaxNode, SyntaxToken};

impl InferCx<'_> {
    /// Types `recv.name(args)` on a receiver of `NodeRef<target>`.
    pub(super) fn node_method(
        &mut self,
        name: &SyntaxToken,
        target: &str,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let text = name.text();
        let Some(action) = NodeAction::named(&text) else {
            let message = format!("no method `{text}` on `NodeRef<{target}>`");
            let mut diagnostic = Diagnostic::error("E2001", name.text_range(), message);
            let suggestions = nearest(
                &text,
                NodeAction::ALL.iter().map(|action| Candidate {
                    name: action.method(),
                    declared_at: None,
                }),
            );
            attach(&mut diagnostic, name.text_range(), &suggestions);
            self.diagnostics.push(diagnostic);
            self.infer_args_alone(args);
            return Ty::Unknown;
        };
        let scrolls = match action {
            NodeAction::Focus | NodeAction::Blur | NodeAction::ScrollIntoView => None,
            NodeAction::ScrollTo | NodeAction::ScrollBy => Some("Scroll"),
            NodeAction::ScrollToIndex | NodeAction::ScrollToKey => Some("VirtualList"),
        };
        if let Some(scrolls) = scrolls
            && target != scrolls
        {
            let message = format!(
                "`{text}` scrolls a `{scrolls}`, not a `{target}`; \
                 `scroll_into_view` brings any node into view"
            );
            self.diagnostics
                .push(Diagnostic::error("E2001", name.text_range(), message));
            self.infer_args_alone(args);
            return Ty::Unit;
        }
        let params = match action {
            NodeAction::Focus | NodeAction::Blur | NodeAction::ScrollIntoView => Vec::new(),
            NodeAction::ScrollTo | NodeAction::ScrollBy => {
                let offset = self
                    .env
                    .standard_type("Offset")
                    .map_or(Ty::Unknown, Ty::named);
                vec![offset]
            }
            NodeAction::ScrollToIndex => vec![Ty::I64],
            // A key is whatever the list's `for` keys its items by; one of
            // another kind matches no row.
            NodeAction::ScrollToKey => vec![Ty::Unknown],
        };
        if args.len() != params.len() {
            let message = format!(
                "`{text}` takes {} argument{}, but {} {} given",
                params.len(),
                if params.len() == 1 { "" } else { "s" },
                args.len(),
                if args.len() == 1 { "is" } else { "are" },
            );
            self.diagnostics
                .push(Diagnostic::error("E2103", node.text_range(), message));
            self.infer_args_alone(args);
            return Ty::Unit;
        }
        self.apply_signature(args, (params, Ty::Unit), expected, node)
    }
}
