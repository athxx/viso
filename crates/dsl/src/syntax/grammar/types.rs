//! The type grammar (Appendix A.3). A type is a function type, a tuple type, an
//! array or slice type, a trait-object type, `Self`, or a `::`-separated type
//! path whose segments each carry an optional `<...>` generic-argument list.
//!
//! Function types are recognized by their leading callable keyword lexing as an
//! ordinary identifier (`Fn`/`FnMut`/`ActionFn`/`TaskFn` are context words, not
//! keywords), so the parser peeks for a `(` after such an identifier. Tuple and
//! array/slice types share the `(`/`[` openers with expression forms, but in a
//! type position there is no ambiguity: only types appear here.

use super::super::kind::SyntaxKind;
use super::{ParseErrorKind, Parser};

/// Parses a type. Wraps the specific shape in the node kind that matches it so a
/// later AST cast can distinguish a tuple type from a path without re-lexing.
pub(super) fn type_(p: &mut Parser) {
    match p.current() {
        SyntaxKind::LParen => tuple_or_paren_type(p),
        SyntaxKind::LBracket => array_or_slice_type(p),
        SyntaxKind::DynKw => trait_object_type(p),
        _ if at_function_type(p) => function_type(p),
        _ if at_type_start(p) => {
            let m = p.start();
            type_path(p);
            m.complete(p, SyntaxKind::TypePath);
        }
        _ => {
            let m = p.start();
            p.error(ParseErrorKind::MissingToken);
            m.abandon(p);
        }
    }
}

/// Whether the token at the cursor can begin a type.
pub(super) fn at_type_start(p: &Parser) -> bool {
    matches!(
        p.current(),
        SyntaxKind::Ident
            | SyntaxKind::RawIdent
            | SyntaxKind::SelfTypeKw
            | SyntaxKind::LParen
            | SyntaxKind::LBracket
            | SyntaxKind::DynKw
    )
}

/// Whether the cursor is at a function type: one of the callable context words
/// (`Fn`/`FnMut`/`ActionFn`/`TaskFn`) immediately followed by a `(`.
fn at_function_type(p: &Parser) -> bool {
    if !p.at(SyntaxKind::Ident) {
        return false;
    }
    if p.nth(1) != SyntaxKind::LParen {
        return false;
    }
    matches!(p.token_text(0), "Fn" | "FnMut" | "ActionFn" | "TaskFn")
}

/// `("Fn" | "FnMut" | "ActionFn" | "TaskFn") "(" TypeList? ")" "->" Type`.
fn function_type(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // the callable context word
    p.expect(SyntaxKind::LParen);
    while !p.at(SyntaxKind::RParen) && !p.at_end() {
        type_(p);
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
    }
    p.expect(SyntaxKind::RParen);
    if p.eat(SyntaxKind::Arrow) {
        type_(p);
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
    m.complete(p, SyntaxKind::TypePath);
}

/// A parenthesized type `"(" Type ")"` or a tuple type `"(" Type "," ... ")"`.
/// The grammar's tuple requires at least one trailing comma to disambiguate a
/// one-element tuple from a grouping; we complete as `TupleType` whenever a comma
/// appears and otherwise leave the inner type's own node as the result.
fn tuple_or_paren_type(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `(`
    let mut saw_comma = false;
    while !p.at(SyntaxKind::RParen) && !p.at_end() {
        type_(p);
        if p.eat(SyntaxKind::Comma) {
            saw_comma = true;
        } else {
            break;
        }
    }
    p.expect(SyntaxKind::RParen);
    let _ = saw_comma;
    m.complete(p, SyntaxKind::TupleType);
}

/// An array type `"[" Type ";" ConstExpr "]"` or a slice type `"[" Type "]"`.
/// Both fold into the `ArrayType` node kind; the presence of the `;` length is
/// what a later pass reads to tell them apart.
fn array_or_slice_type(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `[`
    type_(p);
    if p.eat(SyntaxKind::Semi) {
        // The length is a const expression; parse it as a general expression.
        super::expr::expr(p);
    }
    p.expect(SyntaxKind::RBracket);
    m.complete(p, SyntaxKind::ArrayType);
}

/// A trait-object type `"dyn" TraitBounds`.
fn trait_object_type(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `dyn`
    trait_bounds(p);
    m.complete(p, SyntaxKind::TypePath);
}

/// A `TypePath ("+" TypePath)*` list of trait bounds (A.3).
pub(super) fn trait_bounds(p: &mut Parser) {
    type_path_as_bound(p);
    while p.at(SyntaxKind::Plus) {
        p.bump_any(); // `+`
        type_path_as_bound(p);
    }
}

/// One trait bound: a bare type path wrapped in its own `TypePath` node.
fn type_path_as_bound(p: &mut Parser) {
    let m = p.start();
    type_path(p);
    m.complete(p, SyntaxKind::TypePath);
}

/// A `Segment ("::" Segment)*` type path; each segment is a name plus an
/// optional generic-argument list.
fn type_path(p: &mut Parser) {
    type_path_segment(p);
    while p.at(SyntaxKind::ColonColon) {
        p.bump_any(); // `::`
        type_path_segment(p);
    }
}

/// One `IDENT GenericArgs?` type-path segment.
fn type_path_segment(p: &mut Parser) {
    let m = p.start();
    if p.at(SyntaxKind::Ident) || p.at(SyntaxKind::RawIdent) || p.at(SyntaxKind::SelfTypeKw) {
        p.bump_any();
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
    if p.at(SyntaxKind::Lt) {
        generic_args(p);
    }
    m.complete(p, SyntaxKind::TypePathSegment);
}

/// A `< Type, ... >` generic-argument list on a type-path segment.
fn generic_args(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `<`
    while !p.at(SyntaxKind::Gt) && !p.at_end() {
        type_(p);
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
    }
    p.expect(SyntaxKind::Gt);
    m.complete(p, SyntaxKind::GenericArgs);
}
