//! The methods of `NodeRef<T>`: `focus`, `blur`, `scroll_into_view`,
//! `animate` and `play` on any named node, `scroll_to` and `scroll_by` on a
//! `Scroll`, `scroll_to_index` and `scroll_to_key` on a `VirtualList`; each
//! scroll takes a last `smooth: Bool`, gliding there when true. Each
//! queues a request the view applies once the transaction
//! commits and its structure settles, so a node the same action mounts is
//! reached; the value is `()`, and `animate`'s and `play`'s the `Animation`
//! it starts,
//! whose `cancel` and `finish` queue requests of their own and whose
//! `is_running` answers at once.

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
                NodeAction::ALL
                    .iter()
                    .filter(|a| !a.on_animation())
                    .map(|action| Candidate {
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
            NodeAction::ScrollTo | NodeAction::ScrollBy => Some("Scroll"),
            NodeAction::ScrollToIndex | NodeAction::ScrollToKey => Some("VirtualList"),
            _ => None,
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
        let standard =
            |cx: &Self, name: &str| cx.env.standard_type(name).map_or(Ty::Unknown, Ty::named);
        let mut params = match action {
            NodeAction::ScrollTo | NodeAction::ScrollBy => vec![standard(self, "Offset")],
            NodeAction::Animate => vec![standard(self, "Animate"), standard(self, "Transition")],
            NodeAction::Play => vec![standard(self, "Keyframes")],
            NodeAction::ScrollToIndex => vec![Ty::I64],
            // A key is whatever the list's `for` keys its items by; one of
            // another kind matches no row.
            NodeAction::ScrollToKey => vec![Ty::Unknown],
            _ => Vec::new(),
        };
        let ret = if action.starts_animation() {
            Ty::Animation
        } else {
            Ty::Unit
        };
        // A scroll may glide.
        let scroll = matches!(
            action,
            NodeAction::ScrollIntoView
                | NodeAction::ScrollTo
                | NodeAction::ScrollBy
                | NodeAction::ScrollToIndex
                | NodeAction::ScrollToKey
        );
        if scroll && args.len() == params.len() + 1 {
            params.push(Ty::Bool);
        }
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
            return ret;
        }
        self.apply_signature(args, (params, ret), expected, node)
    }

    /// Types `recv.name(args)` on a receiver of `Animation`.
    pub(super) fn animation_method(
        &mut self,
        name: &SyntaxToken,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let text = name.text();
        let Some(action) = NodeAction::animation_method(&text) else {
            let message = format!("no method `{text}` on `Animation`");
            let mut diagnostic = Diagnostic::error("E2001", name.text_range(), message);
            let suggestions = nearest(
                &text,
                NodeAction::ALL
                    .iter()
                    .filter(|a| a.on_animation())
                    .map(|action| Candidate {
                        name: action.method(),
                        declared_at: None,
                    }),
            );
            attach(&mut diagnostic, name.text_range(), &suggestions);
            self.diagnostics.push(diagnostic);
            self.infer_args_alone(args);
            return Ty::Unknown;
        };
        let ret = match action {
            NodeAction::IsRunning => Ty::Bool,
            _ => Ty::Unit,
        };
        if !args.is_empty() {
            let message = format!("`{text}` takes no arguments, but {} given", args.len());
            self.diagnostics
                .push(Diagnostic::error("E2103", node.text_range(), message));
            self.infer_args_alone(args);
            return ret;
        }
        self.apply_signature(args, (Vec::new(), ret), expected, node)
    }
}
