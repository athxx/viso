//! The `format(template, args..)` built-in: the template's placeholders are
//! matched against the arguments at compile time (`E2108`).

use super::{InferCx, TypeEnv, first_child_expr};
use crate::ast::{AstNode, Expr};
use crate::diag::{Applicability, Diagnostic, Fix, TextEdit};
use crate::hir::ty::Ty;
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

/// One placeholder of a template.
#[derive(Debug, PartialEq)]
pub(crate) enum Hole {
    /// `{}`: the next positional argument.
    Positional,
    /// `{name}`: the named argument `name:`.
    Named(String),
}

impl InferCx<'_> {
    /// Types a `format(..)` call: the template is a string literal whose `{}`
    /// placeholders consume the positional arguments in order and whose `{name}`
    /// placeholders consume the named ones; every argument is used exactly once
    /// and is displayable. Any mismatch is `E2108`. The result is `String`.
    pub(super) fn check_format(&mut self, node: &SyntaxNode) -> Ty {
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
        let Some(template) = positional.next() else {
            self.format_error(node.text_range(), "`format` needs a template string");
            return Ty::String;
        };
        let holes = match template_literal(&template) {
            Some((text, raw)) => match parse_template(&text, raw) {
                Ok(holes) => holes,
                Err(message) => {
                    self.format_error(template.syntax().text_range(), &message);
                    self.infer_format_args(positional.chain(named.into_iter().map(|n| n.2)));
                    return Ty::String;
                }
            },
            None => {
                let _ = self.infer_expr(&template, None);
                self.format_error(
                    template.syntax().text_range(),
                    "the `format` template must be a string literal",
                );
                self.infer_format_args(positional.chain(named.into_iter().map(|n| n.2)));
                return Ty::String;
            }
        };
        let positional: Vec<Expr> = positional.collect();

        let wanted = holes.iter().filter(|h| **h == Hole::Positional).count();
        if wanted != positional.len() {
            let message = format!(
                "the template has {wanted} `{{}}` placeholder{}, but {} positional argument{} {} given",
                plural(wanted),
                positional.len(),
                plural(positional.len()),
                if positional.len() == 1 { "is" } else { "are" },
            );
            let at = positional
                .get(wanted)
                .map_or(template.syntax().text_range(), |e| e.syntax().text_range());
            self.format_error(at, &message);
        }
        for hole in &holes {
            if let Hole::Named(name) = hole
                && !named.iter().any(|(n, _, _)| n == name)
            {
                let message = format!("the placeholder `{{{name}}}` has no argument `{name}:`");
                self.format_error(template.syntax().text_range(), &message);
            }
        }
        let mut seen: Vec<&str> = Vec::new();
        for (name, at, _) in &named {
            if seen.contains(&name.as_str()) {
                let message = format!("the argument `{name}:` is given twice");
                self.format_error(*at, &message);
            } else if !holes.contains(&Hole::Named(name.clone())) {
                let message = format!("the template has no `{{{name}}}` placeholder");
                self.format_error(*at, &message);
            }
            seen.push(name);
        }

        self.infer_format_args(positional.into_iter().chain(named.into_iter().map(|n| n.2)));
        Ty::String
    }

    /// Types each argument and reports the ones whose type is not displayable.
    fn infer_format_args(&mut self, args: impl Iterator<Item = Expr>) {
        for arg in args {
            let ty = self.infer_expr(&arg, None);
            if !displayable(self.env, &ty) {
                let message = format!(
                    "`{}` cannot be formatted: it does not implement `Display`",
                    self.describe(&ty)
                );
                self.format_error(arg.syntax().text_range(), &message);
            }
        }
    }

    /// Types the value of a `String` (or `Option<String>`) property slot. A value
    /// of another displayable type is `E2103` with a machine-applicable fix that
    /// wraps it in `format("{}", ..)`: a text slot never converts implicitly.
    pub(crate) fn infer_text_value(&mut self, value: &Expr, target: &Ty) -> Ty {
        let mark = self.diagnostics.len();
        let ty = self.infer_promoted(value, target);
        let range = value.syntax().text_range();
        let Some(index) = self.diagnostics[mark..]
            .iter()
            .position(|d| d.code == "E2103" && d.primary == range)
            .map(|i| mark + i)
        else {
            return ty;
        };
        let end = self.diagnostics.len();
        let found = self.infer_expr(value, None);
        self.diagnostics.truncate(end);
        let text_like = matches!(found, Ty::String | Ty::Option(_) | Ty::Unknown | Ty::Never);
        if !text_like && displayable(self.env, &found) {
            let source = value.syntax().text();
            let note = format!(
                "a text property takes a `String`; format the `{}` explicitly",
                self.describe(&found)
            );
            let diagnostic = &mut self.diagnostics[index];
            diagnostic.notes.push(note);
            diagnostic.fixes.push(Fix {
                title: "wrap the value in `format(\"{}\", ..)`".to_string(),
                applicability: Applicability::MachineApplicable,
                edits: vec![TextEdit::new(
                    range,
                    format!("format(\"{{}}\", {})", source.trim()),
                )],
            });
        }
        ty
    }

    fn format_error(&mut self, at: TextRange, message: &str) {
        self.diagnostics
            .push(Diagnostic::error("E2108", at, message.to_string()));
    }
}

/// The text of a template given as a string literal, and whether it is raw.
pub(crate) fn template_literal(expr: &Expr) -> Option<(String, bool)> {
    let node = expr.syntax();
    if node.kind() != SyntaxKind::LiteralExpr {
        return None;
    }
    let token = node
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| !t.kind().is_trivia())?;
    let text = token.text().to_string();
    match token.kind() {
        SyntaxKind::StringLiteral => {
            let inner = text.strip_prefix('"')?;
            Some((inner.strip_suffix('"').unwrap_or(inner).to_string(), false))
        }
        SyntaxKind::RawStringLiteral => {
            let body = text.strip_prefix('r')?;
            let hashes = body.len() - body.trim_start_matches('#').len();
            let body = &body[hashes..];
            let body = body.strip_prefix('"')?;
            let body = body.strip_suffix(&"#".repeat(hashes)).unwrap_or(body);
            Some((body.strip_suffix('"').unwrap_or(body).to_string(), true))
        }
        _ => None,
    }
}

/// The placeholders of a template body (between the quotes), or why it is
/// malformed. In a non-raw template, `\` escapes are skipped (so `\u{..}` is not
/// a placeholder).
pub(crate) fn parse_template(text: &str, raw: bool) -> Result<Vec<Hole>, String> {
    let mut holes = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if !raw => {
                if chars.next() == Some('u') && chars.peek() == Some(&'{') {
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                    }
                }
            }
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
            }
            '{' => {
                let mut inner = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    inner.push(c);
                }
                if !closed {
                    return Err(
                        "unclosed `{` in the template; write `{{` for a literal brace".into(),
                    );
                }
                let inner = inner.trim();
                if inner.is_empty() {
                    holes.push(Hole::Positional);
                } else if is_ident(inner) {
                    holes.push(Hole::Named(inner.to_string()));
                } else {
                    return Err(format!(
                        "`{{{inner}}}` is not a placeholder: use `{{}}` or `{{name}}` (format specifiers are not supported)"
                    ));
                }
            }
            '}' => {
                return Err("unmatched `}` in the template; write `}}` for a literal brace".into());
            }
            _ => {}
        }
    }
    Ok(holes)
}

fn is_ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// Whether a value of type `ty` implements `Display` (§17): the scalars, text,
/// the single-unit length family and the other dimensional scalars do; a record
/// or enum does not. A nominal type this module does not declare as a record or
/// enum, and an undetermined type, are not reported.
pub(crate) fn displayable(env: &dyn TypeEnv, ty: &Ty) -> bool {
    match ty {
        Ty::Bool
        | Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::I64
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::U64
        | Ty::F32
        | Ty::F64
        | Ty::Char
        | Ty::String
        | Ty::Dp
        | Ty::Px
        | Ty::Sp
        | Ty::Em
        | Ty::Percent
        | Ty::Duration
        | Ty::Angle
        | Ty::Frequency
        | Ty::InferInt
        | Ty::InferFloat
        | Ty::Never
        | Ty::Unknown => true,
        Ty::Named(id) => env.record_fields(*id).is_none() && env.enum_variants(*id).is_none(),
        Ty::Bytes
        | Ty::Unit
        | Ty::Color
        | Ty::MixedLength
        | Ty::Tuple(_)
        | Ty::Fn(..)
        | Ty::List(_)
        | Ty::Option(_)
        | Ty::Result(..)
        | Ty::Range(_)
        | Ty::RangeInclusive(_)
        | Ty::Resource(..)
        | Ty::ResourceState(..)
        | Ty::Native(_) => false,
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::{Hole, parse_template};

    #[test]
    fn template_placeholders() {
        assert_eq!(
            parse_template("a {} b {name} {{x}} \\u{41}", false),
            Ok(vec![Hole::Positional, Hole::Named("name".into())])
        );
        assert!(parse_template("{:>3}", false).is_err());
        assert!(parse_template("open {", false).is_err());
        assert!(parse_template("close }", false).is_err());
        // A raw template has no escapes.
        assert!(parse_template("\\u{41}", true).is_err());
    }
}

/// One piece of a parsed template: literal text or a placeholder.
#[derive(Debug, PartialEq)]
pub(crate) enum Piece {
    /// Literal text, with `{{`/`}}` and (in a non-raw template) escapes resolved.
    Text(String),
    /// A placeholder.
    Hole(Hole),
}

/// The pieces of a template's body text in order, `None` when the template is
/// malformed (already an `E2108`) or an escape is.
pub(crate) fn template_pieces(text: &str, raw: bool) -> Option<Vec<Piece>> {
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let flush = |literal: &mut String, pieces: &mut Vec<Piece>| -> Option<()> {
        if !literal.is_empty() {
            let text = if raw {
                std::mem::take(literal)
            } else {
                let text = super::pattern::unescape(literal)?;
                literal.clear();
                text
            };
            pieces.push(Piece::Text(text));
        }
        Some(())
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if !raw => {
                literal.push(c);
                let escaped = chars.next()?;
                literal.push(escaped);
                if escaped == 'u' && chars.peek() == Some(&'{') {
                    for c in chars.by_ref() {
                        literal.push(c);
                        if c == '}' {
                            break;
                        }
                    }
                }
            }
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                    inner.push(c);
                }
                flush(&mut literal, &mut pieces)?;
                let inner = inner.trim();
                pieces.push(Piece::Hole(if inner.is_empty() {
                    Hole::Positional
                } else {
                    Hole::Named(inner.to_string())
                }));
            }
            '}' => return None,
            _ => literal.push(c),
        }
    }
    flush(&mut literal, &mut pieces)?;
    Some(pieces)
}
