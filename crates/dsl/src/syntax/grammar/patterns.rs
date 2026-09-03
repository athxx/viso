//! The pattern grammar. Commit 1 needs only enough of a pattern to parse a
//! `match` arm's left-hand side; the full pattern grammar (record/tuple/binding
//! subpatterns, `|` alternatives, ranges) lands with the declaration grammar.

use super::super::kind::SyntaxKind;
use super::{ParseErrorKind, Parser};

/// Parses a pattern. Minimal for commit 1: a wildcard `_`, a literal, or a path
/// (an enum variant / binding name), so `match` arms parse end to end.
pub(super) fn pattern(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    match p.current() {
        // A wildcard, a binding name, or an enum-variant path.
        Ident | RawIdent | SelfTypeKw => {
            p.bump_any();
            while p.at(ColonColon) {
                p.bump_any(); // `::`
                if p.at(Ident) || p.at(RawIdent) || p.at(SelfTypeKw) {
                    p.bump_any();
                } else {
                    p.error(ParseErrorKind::MissingToken);
                    break;
                }
            }
        }
        // A literal pattern.
        IntLiteral | FloatLiteral | UnitLiteral | StringLiteral | RawStringLiteral
        | CharLiteral | ColorLiteral | TrueKw | FalseKw | NoneKw => {
            p.bump_any();
        }
        _ => {
            p.error(ParseErrorKind::MissingToken);
        }
    }
    m.complete(p, SyntaxKind::Pattern);
}
