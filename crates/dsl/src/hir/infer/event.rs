//! The methods every event a handler receives has: `e.stop_propagation()`,
//! `e.stop_immediate_propagation()` and `e.prevent_default()`, the actions of
//! `viso::event` of their names. They take no arguments; a record of the
//! module's own declaring a method of one of these names keeps it.

use viso_behavior::event::EVENT_PATH;

use super::InferCx;
use crate::ast::Expr;
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::syntax::{SyntaxNode, SyntaxToken};

impl InferCx<'_> {
    /// Types `e.name()`, the call `node`, `name` one of the event methods.
    pub(super) fn event_method(
        &mut self,
        name: &SyntaxToken,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let text = name.text();
        if !args.is_empty() {
            self.diagnostics.push(Diagnostic::error(
                "E2103",
                node.text_range(),
                format!("`{text}` takes no arguments"),
            ));
            self.infer_args_alone(args);
        }
        let path = format!("{EVENT_PATH}::{text}");
        if let Some(entry) = self.env.natives().and_then(|n| n.function(&path)) {
            self.record_native(node.text_range(), entry.id, false);
        }
        self.check_against(Ty::Unit, expected, node)
    }
}
