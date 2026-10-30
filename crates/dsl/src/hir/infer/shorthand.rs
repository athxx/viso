//! Bare enum variants: where a value's expected type is an enum (or its
//! `Option`), `center` stands for `Align::center` and `fill { weight: 2.0 }`
//! for `Sizing::fill { weight: 2.0 }`.

use super::{InferCx, VariantPayload};
use crate::diag::{Applicability, Diagnostic, Fix, TextEdit};
use crate::hir::ty::Ty;
use crate::resolve::SymbolId;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::syntax::{SyntaxNode, SyntaxToken};

impl InferCx<'_> {
    /// The enum `expected` names, directly or inside its `Option`.
    fn expected_enum<'t>(&self, expected: Option<&'t Ty>) -> Option<(&'t Ty, SymbolId)> {
        let ty = match expected? {
            Ty::Option(inner) => inner.as_ref(),
            ty => ty,
        };
        let Ty::Named(id, _) = ty else {
            return None;
        };
        self.env.enum_variants(*id)?;
        Some((ty, *id))
    }

    /// The expected enum with the index of its variant `name`.
    fn expected_variant<'t>(
        &self,
        expected: Option<&'t Ty>,
        name: &str,
    ) -> Option<(&'t Ty, SymbolId, usize)> {
        let (ty, id) = self.expected_enum(expected)?;
        let index = self
            .env
            .enum_variants(id)?
            .iter()
            .position(|v| v.name == name)?;
        Some((ty, id, index))
    }

    /// The variant a bare name or record head at `at` stands for, when typing
    /// read it as one.
    pub(crate) fn shorthand_at(&self, at: crate::syntax::TextRange) -> Option<(SymbolId, usize)> {
        self.shorthands.get(&at).copied()
    }

    /// Types the bare name `head` (`node`) that nothing in scope declares: a
    /// variant of the expected enum, which a unit variant or a record variant
    /// whose fields all have defaults is; else the name is unresolved.
    pub(super) fn infer_shorthand(
        &mut self,
        node: &SyntaxNode,
        head: &SyntaxToken,
        expected: Option<&Ty>,
    ) -> Ty {
        let name = head.text();
        let name = name.trim_start_matches("r#");
        let Some((ty, id, index)) = self.expected_variant(expected, name) else {
            self.unresolved_shorthand(head, expected);
            return Ty::Unknown;
        };
        let ty = ty.clone();
        let owner = self.describe(&Ty::named(id));
        let payload = self
            .env
            .enum_variants(id)
            .and_then(|vs| vs.get(index))
            .map(|v| v.payload.clone());
        let message = match payload {
            Some(VariantPayload::Record(fields)) if fields.iter().any(|f| !f.has_default) => {
                let missing: Vec<String> = fields
                    .iter()
                    .filter(|f| !f.has_default)
                    .map(|f| format!("{}: …", f.name))
                    .collect();
                Some(format!(
                    "`{owner}::{name}` has fields without defaults: `{name} {{ {} }}`",
                    missing.join(", ")
                ))
            }
            Some(VariantPayload::Tuple(_)) => Some(format!(
                "`{owner}::{name}` carries values: `{owner}::{name}(…)`"
            )),
            _ => None,
        };
        if let Some(message) = message {
            self.diagnostics
                .push(Diagnostic::error("E2103", node.text_range(), message));
        }
        self.shorthands.insert(head.text_range(), (id, index));
        self.check_against(ty, expected, node)
    }

    /// The variant a bare record head `head` stands for, when the expected
    /// enum has a record variant of that name; `None` (reported) otherwise.
    pub(super) fn record_shorthand(
        &mut self,
        head: &SyntaxToken,
        expected: Option<&Ty>,
    ) -> Option<(SymbolId, usize)> {
        let name = head.text();
        match self.expected_variant(expected, name.trim_start_matches("r#")) {
            Some((_, id, index)) => {
                self.shorthands.insert(head.text_range(), (id, index));
                Some((id, index))
            }
            None => {
                self.unresolved_shorthand(head, expected);
                None
            }
        }
    }

    /// Raises the resolver's `E2001` for `head`, offering the expected enum's
    /// nearest variants too.
    fn unresolved_shorthand(&mut self, head: &SyntaxToken, expected: Option<&Ty>) {
        let text = head.text();
        let at = head.text_range();
        let mut diagnostic = self.env.deferred_unresolved(at).unwrap_or_else(|| {
            Diagnostic::error("E2001", at, format!("no value named `{text}` is in scope"))
        });
        if let Some((_, id)) = self.expected_enum(expected) {
            let owner = self.describe(&Ty::named(id));
            diagnostic.message = format!("no value or `{owner}` variant named `{text}`");
            let env = self.env;
            let variants = env.enum_variants(id).unwrap_or_default();
            let suggestions = nearest(
                &text,
                variants.iter().map(|v| Candidate {
                    name: v.name.as_str(),
                    declared_at: env.declaration_site(id, v.declared_at),
                }),
            );
            attach(&mut diagnostic, at, &suggestions);
        }
        self.diagnostics.push(diagnostic);
    }

    /// Reports `E2002` where the bare name `head` (`node`) resolves to a value
    /// but also names a variant of the expected enum.
    pub(super) fn check_shorthand_ambiguity(
        &mut self,
        node: &SyntaxNode,
        head: &SyntaxToken,
        expected: Option<&Ty>,
    ) {
        if !crate::ast::is_shorthand_site(node) {
            return;
        }
        let name = head.text();
        let name = name.trim_start_matches("r#");
        let Some((_, id, _)) = self.expected_variant(expected, name) else {
            return;
        };
        let owner = self.describe(&Ty::named(id));
        let range = node.text_range();
        let mut diagnostic = Diagnostic::error(
            "E2002",
            head.text_range(),
            format!("`{name}` is both a value in scope and a variant of `{owner}`"),
        );
        diagnostic.fixes.push(Fix {
            title: format!("use the variant `{owner}::{name}`"),
            applicability: Applicability::MaybeIncorrect,
            edits: vec![TextEdit::new(range, format!("{owner}::{name}"))],
        });
        diagnostic.fixes.push(Fix {
            title: format!("use the value `({name})`"),
            applicability: Applicability::MaybeIncorrect,
            edits: vec![TextEdit::new(range, format!("({name})"))],
        });
        self.diagnostics.push(diagnostic);
    }
}
