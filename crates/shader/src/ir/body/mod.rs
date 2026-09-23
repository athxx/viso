//! The built-in shader bodies as a typed program rather than MSL text.
//!
//! Every built-in's vertex body, helper functions and fragment body are written
//! once, in a small C-like shading subset whose spelling is MSL's. This module
//! turns those fragments into a checked tree — `source → parsed syntax → typed
//! IR → validation` — that each backend's printer lowers to its own language:
//!
//! - [`lex`] — tokens with spans, comments, and blank-line markers;
//! - [`parse`] — tokens → [`ast`] (syntax only, every node spanned);
//! - [`check`] — name resolution, types, stage rules, mutation analysis;
//! - [`print`] — one printer, three dialects (MSL, WGSL, HLSL).
//!
//! The subset is deliberately closed: braces are mandatory, comments stand on
//! their own lines between statements, every `switch` arm ends in `break` or
//! `return`, there is no implicit conversion between concrete types, and an
//! integer literal takes the type its context asks for. Anything outside it is a
//! spanned diagnostic, not a guess.

pub mod ast;
pub mod check;
pub mod lex;
pub mod parse;
pub mod print;

use crate::CompileStage;
use crate::diag::Diagnostic;
use crate::ir::body::ast::{Function, Item};
use crate::ir::body::lex::Span;
use crate::ir::module::ShaderIr;

/// Which fragment of a [`ShaderIr`] a body error is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyPart {
    /// `vertex_body`.
    Vertex,
    /// `helpers`.
    Helpers,
    /// `fragment_body`.
    Fragment,
}

impl BodyPart {
    /// The field name the span is relative to.
    pub fn name(self) -> &'static str {
        match self {
            BodyPart::Vertex => "vertex_body",
            BodyPart::Helpers => "helpers",
            BodyPart::Fragment => "fragment_body",
        }
    }
}

/// A lex, parse or check error in a body fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyError {
    /// The fragment the span points into; filled by [`parse_ir`].
    pub part: Option<BodyPart>,
    /// Stable diagnostic code: `S01xx` lexical, `S02xx` syntax, `S03xx` type
    /// and stage rules.
    pub code: &'static str,
    /// Where, relative to the fragment's first byte.
    pub span: Span,
    /// What is wrong.
    pub message: String,
}

impl BodyError {
    /// A new error with no fragment attached yet.
    pub fn new(code: &'static str, span: Span, message: impl Into<String>) -> BodyError {
        BodyError {
            part: None,
            code,
            span,
            message: message.into(),
        }
    }

    /// The compile stage this error belongs to.
    pub fn stage(&self) -> CompileStage {
        if self.code.starts_with("S03") {
            CompileStage::TypedIr
        } else {
            CompileStage::ParsedSyntax
        }
    }

    /// As a tooling diagnostic.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let mut d =
            Diagnostic::error(self.code, self.stage(), self.message.clone()).with_span(self.span);
        if let Some(part) = self.part {
            d = d.with_note(format!(
                "in `{}` at {}:{}",
                part.name(),
                self.span.line,
                self.span.col
            ));
        }
        d
    }
}

impl std::fmt::Display for BodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.part {
            Some(part) => write!(
                f,
                "{} {}:{}:{}: {}",
                self.code,
                part.name(),
                self.span.line,
                self.span.col,
                self.message
            ),
            None => write!(
                f,
                "{} {}:{}: {}",
                self.code, self.span.line, self.span.col, self.message
            ),
        }
    }
}

impl std::error::Error for BodyError {}

/// A built-in's three body fragments, parsed and checked.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedIr {
    /// The helper functions and their comments, in source order.
    pub helpers: Vec<Item>,
    /// The vertex body, as a function returning `VOut`.
    pub vertex: Function,
    /// The fragment body, as a function returning `float4`.
    pub fragment: Function,
}

impl ParsedIr {
    /// The helper functions, without the comments between them.
    pub fn helper_functions(&self) -> impl Iterator<Item = &Function> {
        self.helpers.iter().filter_map(|i| match i {
            Item::Function { func, .. } => Some(func),
            Item::Comment { .. } => None,
        })
    }
}

/// Parse and check all three body fragments of `ir`.
pub fn parse_ir(ir: &ShaderIr) -> Result<ParsedIr, BodyError> {
    let at = |part: BodyPart| {
        move |mut e: BodyError| {
            e.part = Some(part);
            e
        }
    };
    let mut helpers = parse::parse_items(ir.helpers).map_err(at(BodyPart::Helpers))?;
    let mut vertex =
        parse::parse_entry(ir.vertex_body, check::Stage::Vertex).map_err(at(BodyPart::Vertex))?;
    let mut fragment = parse::parse_entry(ir.fragment_body, check::Stage::Fragment)
        .map_err(at(BodyPart::Fragment))?;
    check::check_helpers(ir, &mut helpers).map_err(at(BodyPart::Helpers))?;
    let signatures = check::signatures(&helpers);
    check::check_entry(ir, &signatures, check::Stage::Vertex, &mut vertex)
        .map_err(at(BodyPart::Vertex))?;
    check::check_entry(ir, &signatures, check::Stage::Fragment, &mut fragment)
        .map_err(at(BodyPart::Fragment))?;
    Ok(ParsedIr {
        helpers,
        vertex,
        fragment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::body::lex::{TokenKind, lex};
    use crate::ir::body::print::{Lang, Printer};
    use crate::ir::module::builtin_irs;

    fn token_shape(src: &str) -> Vec<(TokenKind, bool)> {
        lex(src)
            .unwrap()
            .into_iter()
            .map(|t| (t.kind, t.blank_before))
            .collect()
    }

    #[test]
    fn every_builtin_parses_and_checks() {
        for ir in builtin_irs() {
            if let Err(e) = parse_ir(&ir) {
                panic!("{:?}: {e}", ir.kind);
            }
        }
    }

    #[test]
    fn msl_printer_round_trips_every_builtin() {
        for ir in builtin_irs() {
            let parsed = parse_ir(&ir).unwrap();
            let mut p = Printer::new(Lang::Msl);
            p.entry_body(&parsed.vertex);
            let vertex = p.finish();
            let mut p = Printer::new(Lang::Msl);
            p.items(&parsed.helpers);
            let helpers = p.finish();
            let mut p = Printer::new(Lang::Msl);
            p.entry_body(&parsed.fragment);
            let fragment = p.finish();
            assert_eq!(
                token_shape(&vertex),
                token_shape(ir.vertex_body),
                "{:?} vertex:\n{vertex}",
                ir.kind
            );
            assert_eq!(
                token_shape(&helpers),
                token_shape(ir.helpers),
                "{:?} helpers:\n{helpers}",
                ir.kind
            );
            assert_eq!(
                token_shape(&fragment),
                token_shape(ir.fragment_body),
                "{:?} fragment:\n{fragment}",
                ir.kind
            );
        }
    }

    #[test]
    fn errors_name_their_fragment_and_convert_to_diagnostics() {
        let mut ir = builtin_irs()[0].clone();
        ir.fragment_body = "float x = nope;\nreturn float4(x);";
        let e = parse_ir(&ir).unwrap_err();
        assert_eq!(e.part, Some(BodyPart::Fragment));
        assert_eq!(e.code, "S0301");
        assert_eq!((e.span.line, e.span.col), (1, 11));
        let d = e.to_diagnostic();
        assert_eq!(d.code, "S0301");
        assert_eq!(d.stage, CompileStage::TypedIr);
        assert_eq!(d.span, Some(e.span));
        assert!(e.to_string().starts_with("S0301 fragment_body:1:11:"));
    }
}
