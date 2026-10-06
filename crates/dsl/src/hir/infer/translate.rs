//! `tr(key, name: value..)` and `MessageKey` literals, checked against the
//! package's message catalog (`E3706`): the key names a message of the
//! source locale, each argument the message takes is given once by name in
//! a kind it accepts, and a key held as a `MessageKey` value names a message
//! without arguments, since no call site of its value can pass them.

use viso_behavior::i18n::{MESSAGE_KEY, TR};
use viso_behavior::native::NativeId;

use super::format::{displayable, template_literal};
use super::pattern::unescape;
use super::{InferCx, VariantPayload, first_child_expr, is_numeric_ty};
use crate::ast::{AstNode, Expr};
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::i18n::{ArgKind, CatalogIssue, MessageSig};
use crate::resolve::SymbolId;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::syntax::span::TextSize;
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

/// How a `tr` argument reaches its message.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ArgPass {
    /// As is: a `String`, or a number the message formats for the reader.
    Raw,
    /// Its text, as `format` shows a value of the type.
    Shown(Ty),
    /// A value of the unit-only enum, by its variant's name.
    Variant(SymbolId),
}

/// A checked `tr` call: the message's id when the key is written in the
/// call, and how each argument passes, in the message's argument order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrCall {
    pub(crate) key: Option<u32>,
    pub(crate) args: Vec<(String, ArgPass)>,
}

/// The type of a message key.
pub(crate) fn message_key_ty() -> Ty {
    Ty::Native(NativeId::of(MESSAGE_KEY))
}

/// Whether the native `id` is `tr`.
pub(crate) fn is_tr(id: NativeId) -> bool {
    id == NativeId::of(TR)
}

/// The key a string literal spells, unescaped.
pub(crate) fn literal_key(expr: &Expr) -> Option<String> {
    let (text, raw) = template_literal(expr)?;
    if raw { Some(text) } else { unescape(&text) }
}

impl InferCx<'_> {
    /// The checked `tr` call at `range`.
    pub(crate) fn tr_call(&self, range: TextRange) -> Option<&TrCall> {
        self.tr_calls.get(&range)
    }

    fn catalog_error(&mut self, at: TextRange, message: impl Into<String>) -> &mut Diagnostic {
        self.diagnostics
            .push(Diagnostic::error(CatalogIssue::CODE, at, message));
        self.diagnostics.last_mut().expect("just pushed")
    }

    /// The message `key` names, reporting a key the catalog does not have,
    /// with the nearest keys it has.
    fn message_named(&mut self, key: &str, at: TextRange) -> Option<(u32, MessageSig)> {
        let Some(messages) = self.env.messages() else {
            self.catalog_error(
                at,
                format!(
                    "`{key}` is looked up in the package's message catalog, but it has none: add \
                     `{}/{}.toml`",
                    crate::i18n::CATALOG_DIR,
                    crate::i18n::DEFAULT_SOURCE
                ),
            );
            return None;
        };
        if let Some((id, sig)) = messages.message(key) {
            return Some((id, sig.clone()));
        }
        let source = messages.source().to_owned();
        let keys: Vec<String> = messages.sigs().iter().map(|s| s.key.clone()).collect();
        let suggestions = nearest(
            key,
            keys.iter().map(|k| Candidate {
                name: k,
                declared_at: None,
            }),
        );
        let names: Vec<String> = suggestions.iter().map(|s| s.name.to_owned()).collect();
        let diagnostic = self.catalog_error(
            at,
            format!("`{key}` is not a message of the `{source}` catalog"),
        );
        let suggestions: Vec<Candidate<'_>> = names
            .iter()
            .map(|name| Candidate {
                name,
                declared_at: None,
            })
            .collect();
        // The fix replaces the key inside its quotes.
        let one = TextSize::from(1);
        let inner = TextRange::new(at.start() + one, at.end() - one);
        attach(diagnostic, inner, &suggestions);
        None
    }

    /// Types a string literal where a `MessageKey` is expected: it names a
    /// message of the catalog that takes no arguments.
    pub(super) fn infer_message_key(&mut self, node: &SyntaxNode) -> Ty {
        let Some(expr) = Expr::cast(node.clone()) else {
            return message_key_ty();
        };
        let at = node.text_range();
        let Some(key) = literal_key(&expr) else {
            self.catalog_error(at, "a malformed message key");
            return message_key_ty();
        };
        if let Some((_, sig)) = self.message_named(&key, at)
            && !sig.args.is_empty()
        {
            let names: Vec<String> = sig.args.iter().map(|(n, _)| format!("`{n}:`")).collect();
            let diagnostic = self.catalog_error(
                at,
                format!(
                    "`{key}` takes {}, which a `MessageKey` value cannot carry",
                    names.join(", ")
                ),
            );
            diagnostic.notes.push(format!(
                "pass the key to `tr` directly: `tr(\"{key}\", ..)`"
            ));
        }
        message_key_ty()
    }

    /// Types a `tr(key, name: value..)` call at `node`; its result is the
    /// message's text.
    pub(super) fn check_tr(&mut self, node: &SyntaxNode, callee: TextRange) -> Ty {
        self.tr_context(node, callee);
        let mut positional: Vec<Expr> = Vec::new();
        let mut named: Vec<(String, TextRange, Expr)> = Vec::new();
        for arg in node
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::ArgumentList)
            .flat_map(|list| list.children())
            .filter(|a| a.kind() == SyntaxKind::Argument)
        {
            let Some(value) = first_child_expr(&arg) else {
                continue;
            };
            let label = arg
                .children_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent));
            match label {
                Some(label) => named.push((
                    label.text().trim_start_matches("r#").to_string(),
                    label.text_range(),
                    value,
                )),
                None => positional.push(value),
            }
        }
        let mut positional = positional.into_iter();
        let Some(key) = positional.next() else {
            self.catalog_error(callee, "`tr` takes a message key, as `tr(\"inbox.title\")`");
            self.infer_untyped(named.into_iter().map(|n| n.2));
            return Ty::String;
        };
        for extra in positional {
            let at = extra.syntax().text_range();
            self.catalog_error(
                at,
                "a `tr` argument is named after the message's placeholder, as `count: n`",
            );
            let _ = self.infer_expr(&extra, None);
        }

        let key_at = key.syntax().text_range();
        let sig = if key.syntax().kind() == SyntaxKind::LiteralExpr && literal_key(&key).is_some() {
            self.types
                .insert((key_at, key.syntax().kind()), message_key_ty());
            let text = literal_key(&key).unwrap_or_default();
            self.message_named(&text, key_at)
        } else {
            let _ = self.infer_expr(&key, Some(&message_key_ty()));
            if let Some((_, at, _)) = named.first() {
                let diagnostic = self.catalog_error(
                    *at,
                    "a `MessageKey` value names a message without arguments, so `tr` passes \
                     none with it",
                );
                diagnostic
                    .notes
                    .push("write the key in the call to pass arguments".to_owned());
            }
            self.infer_untyped(named.into_iter().map(|n| n.2));
            self.tr_calls.insert(
                node.text_range(),
                TrCall {
                    key: None,
                    args: Vec::new(),
                },
            );
            return Ty::String;
        };
        let Some((id, sig)) = sig else {
            self.infer_untyped(named.into_iter().map(|n| n.2));
            return Ty::String;
        };

        let mut passes: Vec<Option<ArgPass>> = vec![None; sig.args.len()];
        for (name, at, value) in &named {
            let Some(index) = sig.args.iter().position(|(n, _)| n == name) else {
                let names: Vec<String> = sig.args.iter().map(|(n, _)| n.clone()).collect();
                let message = if names.is_empty() {
                    format!("`{}` takes no arguments", sig.key)
                } else {
                    format!("`{}` has no placeholder `{name}`", sig.key)
                };
                let diagnostic = self.catalog_error(*at, message);
                let near: Vec<Candidate<'_>> = nearest(
                    name,
                    names.iter().map(|n| Candidate {
                        name: n,
                        declared_at: None,
                    }),
                );
                attach(diagnostic, *at, &near);
                let _ = self.infer_expr(value, None);
                continue;
            };
            if passes[index].is_some() {
                self.catalog_error(*at, format!("the argument `{name}:` is given twice"));
                let _ = self.infer_expr(value, None);
                continue;
            }
            let kind = sig.args[index].1;
            passes[index] = Some(
                self.tr_arg(value, kind, &sig.key, name)
                    .unwrap_or(ArgPass::Raw),
            );
        }
        let missing: Vec<String> = sig
            .args
            .iter()
            .zip(&passes)
            .filter(|(_, p)| p.is_none())
            .map(|((n, _), _)| format!("`{n}:`"))
            .collect();
        if !missing.is_empty() {
            self.catalog_error(
                node.text_range(),
                format!("`{}` needs {}", sig.key, missing.join(", ")),
            );
        }
        let args = sig
            .args
            .iter()
            .zip(passes)
            .map(|((n, _), p)| (n.clone(), p.unwrap_or(ArgPass::Raw)))
            .collect();
        self.tr_calls.insert(
            node.text_range(),
            TrCall {
                key: Some(id),
                args,
            },
        );
        Ty::String
    }

    /// Types the argument `name:` of message `key`, which uses it as `kind`.
    fn tr_arg(&mut self, value: &Expr, kind: ArgKind, key: &str, name: &str) -> Option<ArgPass> {
        let ty = self.infer_expr(value, None);
        let at = value.syntax().text_range();
        let number = is_numeric_ty(&ty) || matches!(ty, Ty::InferInt | Ty::InferFloat);
        let pass = match (&ty, kind) {
            (Ty::Unknown | Ty::Never, _) => Some(ArgPass::Raw),
            (Ty::String, ArgKind::Text | ArgKind::Select) => Some(ArgPass::Raw),
            (_, ArgKind::Text | ArgKind::Number) if number => Some(ArgPass::Raw),
            (Ty::Named(id), ArgKind::Select) if self.unit_enum(*id) => Some(ArgPass::Variant(*id)),
            (_, ArgKind::Text) if displayable(self.env, &ty) => Some(ArgPass::Shown(ty.clone())),
            _ => None,
        };
        if pass.is_none() {
            let wanted = match kind {
                ArgKind::Text => "text or a value with `Display`",
                ArgKind::Number => "a number",
                ArgKind::Select => "a `String` or an enum without payloads",
            };
            let found = self.describe(&ty);
            self.diagnostics.push(
                Diagnostic::error(
                    CatalogIssue::CODE,
                    at,
                    format!("`{key}` uses `{name}` as {wanted}, found `{found}`"),
                )
                .expecting([wanted], found.clone()),
            );
        }
        pass
    }

    /// Whether `id` is an enum whose variants all carry nothing.
    fn unit_enum(&self, id: SymbolId) -> bool {
        self.env
            .enum_variants(id)
            .is_some_and(|vs| vs.iter().all(|v| v.payload == VariantPayload::Unit))
    }

    fn infer_untyped(&mut self, values: impl Iterator<Item = Expr>) {
        for value in values {
            let _ = self.infer_expr(&value, None);
        }
    }

    /// Reports a `tr` outside the code of a component instance, whose
    /// `env.locale` it reads: in a `fn`, a task, an initializer, a system or
    /// a module-level declaration.
    fn tr_context(&mut self, node: &SyntaxNode, callee: TextRange) {
        for ancestor in node.ancestors() {
            match ancestor.kind() {
                SyntaxKind::ViewDecl
                | SyntaxKind::StyleDecl
                | SyntaxKind::ViewFragment
                | SyntaxKind::ComponentDecl => return,
                SyntaxKind::SystemDecl => {
                    self.diagnostics.push(Diagnostic::error(
                        "E9109",
                        callee,
                        "a system is never mounted, so it has no reader to translate for; \
                         keep the `MessageKey` and let a view call `tr`",
                    ));
                    return;
                }
                SyntaxKind::FnDecl
                | SyntaxKind::TaskDecl
                | SyntaxKind::StateDecl
                | SyntaxKind::InputDecl
                | SyntaxKind::ConstDecl
                | SyntaxKind::ThemeDecl
                | SyntaxKind::ResourceDecl => break,
                _ => {}
            }
        }
        let mut diagnostic = Diagnostic::error(
            "E2111",
            callee,
            "`tr` reads the reader's `env.locale`, so it is called in a component's view, \
             computeds, actions and effects",
        );
        diagnostic.notes.push(
            "return the `MessageKey` from here and call `tr` with it where the text is shown"
                .to_owned(),
        );
        self.diagnostics.push(diagnostic);
    }
}
