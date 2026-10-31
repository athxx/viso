//! The methods of `String`: `len` (its characters), `is_empty`, `contains`,
//! `starts_with` and `ends_with` read it; `trim`, `to_lowercase` and
//! `to_uppercase` give a new string.

use super::InferCx;
use crate::ast::Expr;
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::syntax::{SyntaxNode, SyntaxToken};

/// Every method a string has.
pub(crate) const STRING_METHODS: [&str; 8] = [
    "len",
    "is_empty",
    "contains",
    "starts_with",
    "ends_with",
    "trim",
    "to_lowercase",
    "to_uppercase",
];

impl InferCx<'_> {
    /// Types `recv.name(args)` on a receiver of `String`.
    pub(super) fn string_method(
        &mut self,
        name: &SyntaxToken,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let text = name.text();
        let signature = match text.as_str() {
            "len" => (vec![], Ty::I64),
            "is_empty" => (vec![], Ty::Bool),
            "contains" | "starts_with" | "ends_with" => (vec![Ty::String], Ty::Bool),
            "trim" | "to_lowercase" | "to_uppercase" => (vec![], Ty::String),
            _ => {
                let message = format!("no method `{text}` on `String`");
                let mut diagnostic = Diagnostic::error("E2001", name.text_range(), message);
                let suggestions = nearest(
                    &text,
                    STRING_METHODS.iter().map(|name| Candidate {
                        name,
                        declared_at: None,
                    }),
                );
                attach(&mut diagnostic, name.text_range(), &suggestions);
                self.diagnostics.push(diagnostic);
                self.infer_args_alone(args);
                return Ty::Unknown;
            }
        };
        if args.len() != signature.0.len() {
            let n = signature.0.len();
            let message = format!(
                "`{text}` takes {n} argument{}, but {} {} given",
                if n == 1 { "" } else { "s" },
                args.len(),
                if args.len() == 1 { "is" } else { "are" },
            );
            self.diagnostics
                .push(Diagnostic::error("E2103", node.text_range(), message));
            self.infer_args_alone(args);
            return signature.1;
        }
        self.apply_signature(args, signature, expected, node)
    }
}
