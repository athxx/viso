//! The methods of `List<T>`: `len`, `is_empty`, `get`, `first`, `last` and
//! `contains` read any list; `push`, `insert`, `remove`, `pop`, `clear` and
//! `retain` edit the list a writable place holds — a `state`, a mutable local,
//! or a field or element of one — as an assignment to it does.

use super::InferCx;
use crate::ast::{AstNode, Expr};
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::syntax::{SyntaxNode, SyntaxToken};

/// Every method a list has.
pub(crate) const LIST_METHODS: [&str; 12] = [
    "len", "is_empty", "get", "first", "last", "contains", "push", "insert", "remove", "pop",
    "clear", "retain",
];

/// Whether list method `name` edits its receiver in place.
pub(crate) fn edits_receiver(name: &str) -> bool {
    matches!(
        name,
        "push" | "insert" | "remove" | "pop" | "clear" | "retain"
    )
}

impl InferCx<'_> {
    /// Types `recv.name(args)` on a receiver of `List<elem>`.
    pub(super) fn list_method(
        &mut self,
        name: &SyntaxToken,
        recv: &Expr,
        elem: &Ty,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let elem = elem.clone();
        let option = || Ty::Option(Box::new(elem.clone()));
        let text = name.text();
        let (params, ret) = match text.as_str() {
            "len" => (vec![], Ty::I64),
            "is_empty" => (vec![], Ty::Bool),
            "get" => (vec![Ty::I64], option()),
            "first" | "last" | "pop" => (vec![], option()),
            "contains" => {
                self.check_comparable(&elem, name.text_range());
                (vec![elem.clone()], Ty::Bool)
            }
            "push" => (vec![elem.clone()], Ty::Unit),
            "insert" => (vec![Ty::I64, elem.clone()], Ty::Unit),
            "remove" => (vec![Ty::I64], elem.clone()),
            "clear" => (vec![], Ty::Unit),
            "retain" => (
                vec![Ty::Fn(vec![elem.clone()], Box::new(Ty::Bool))],
                Ty::Unit,
            ),
            _ => {
                let message = format!(
                    "no method `{text}` on `{}`",
                    self.describe(&Ty::List(Box::new(elem.clone())))
                );
                let mut diagnostic = Diagnostic::error("E2001", name.text_range(), message);
                let suggestions = nearest(
                    &text,
                    LIST_METHODS.iter().map(|name| Candidate {
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
        if edits_receiver(&text) {
            self.check_writable(recv.syntax());
            self.env.record_list_write(node.text_range());
        }
        self.apply_signature(args, (params, ret), expected, node)
    }
}
