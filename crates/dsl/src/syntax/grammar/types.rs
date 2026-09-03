//! The type grammar. Commit 1 needs only enough to parse the type operand of an
//! `as` cast, a turbofish argument, and a closure parameter annotation, so this
//! is a minimal type-path parser; the full type grammar (generic bounds,
//! function types, tuples, references) lands with the declaration grammar.

use super::super::kind::SyntaxKind;
use super::{ParseErrorKind, Parser};

/// Parses a type: a `::`-separated path of segments, each optionally carrying a
/// `<...>` generic-argument list.
pub(super) fn type_(p: &mut Parser) {
    let m = p.start();
    if !at_type_start(p) {
        p.error(ParseErrorKind::MissingToken);
        m.abandon(p);
        return;
    }
    type_path(p);
    m.complete(p, SyntaxKind::TypePath);
}

/// Whether the token at the cursor can begin a type.
pub(super) fn at_type_start(p: &Parser) -> bool {
    matches!(
        p.current(),
        SyntaxKind::Ident | SyntaxKind::RawIdent | SyntaxKind::SelfTypeKw
    )
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
