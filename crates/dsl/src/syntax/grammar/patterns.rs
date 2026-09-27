//! The pattern grammar (Appendix A.13, §70). Every pattern, including each
//! nested subpattern, is wrapped in a [`SyntaxKind::Pattern`] node whose single
//! child is the production that matched: an or-pattern, an `@` binding, a range,
//! or a primary pattern.
//!
//! A bare single identifier is always a binding (`IdentPattern`). A payload-free
//! variant must be qualified (`State::idle`, a `QualifiedVariantPattern`), and a
//! constructor is recognized by the `(` or `{` that follows its path. The parser
//! never guesses from capitalization.

use super::super::kind::SyntaxKind;
use super::{CompletedMarker, ParseErrorKind, Parser};

/// `Pattern ::= OrPattern`.
pub(super) fn pattern(p: &mut Parser) {
    let m = p.start();
    or_pattern(p);
    m.complete(p, SyntaxKind::Pattern);
}

/// A pattern without top-level `|` alternatives, for closure parameters, whose
/// list is itself closed by `|`.
pub(super) fn pattern_no_alt(p: &mut Parser) {
    let m = p.start();
    binding_pattern(p);
    m.complete(p, SyntaxKind::Pattern);
}

/// `BindingPattern ("|" BindingPattern)*`.
fn or_pattern(p: &mut Parser) {
    let Some(first) = binding_pattern(p) else {
        return;
    };
    if p.at(SyntaxKind::Pipe) {
        let m = p.start_at(first);
        while p.eat(SyntaxKind::Pipe) {
            binding_pattern(p);
        }
        m.complete(p, SyntaxKind::OrPattern);
    }
}

/// `"mut"? IDENT "@" RangePattern | RangePattern`.
fn binding_pattern(p: &mut Parser) -> Option<CompletedMarker> {
    let at = usize::from(p.at(SyntaxKind::MutKw));
    if p.nth_is_ident(at) && p.nth(at + 1) == SyntaxKind::At {
        let m = p.start();
        p.eat(SyntaxKind::MutKw);
        super::name(p);
        p.bump_any(); // `@`
        range_pattern(p);
        return Some(m.complete(p, SyntaxKind::BindingPattern));
    }
    range_pattern(p)
}

/// `PrimaryPattern ((".." | "..=") PrimaryPattern)?` — both ends are required.
fn range_pattern(p: &mut Parser) -> Option<CompletedMarker> {
    let lo = primary_pattern(p)?;
    if matches!(p.current(), SyntaxKind::DotDot | SyntaxKind::DotDotEq) {
        let m = p.start_at(lo);
        p.bump_any(); // `..` / `..=`
        primary_pattern(p);
        return Some(m.complete(p, SyntaxKind::RangePattern));
    }
    Some(lo)
}

/// One primary pattern, or `None` (with a diagnostic) if the cursor cannot
/// start one. A token that cannot start a pattern and is not a separator is
/// consumed into an error node so recovery makes progress.
fn primary_pattern(p: &mut Parser) -> Option<CompletedMarker> {
    use SyntaxKind::*;
    match p.current() {
        Ident if p.token_text(0) == "_" => {
            let m = p.start();
            p.bump_any();
            Some(m.complete(p, WildcardPattern))
        }
        IntLiteral | CharLiteral | StringLiteral | RawStringLiteral | TrueKw | FalseKw | NoneKw => {
            let m = p.start();
            p.bump_any();
            Some(m.complete(p, LiteralPattern))
        }
        Minus if p.nth(1) == IntLiteral => {
            let m = p.start();
            p.bump_any(); // `-`
            p.bump_any();
            Some(m.complete(p, LiteralPattern))
        }
        MutKw => {
            let m = p.start();
            p.bump_any(); // `mut`
            super::name(p);
            Some(m.complete(p, IdentPattern))
        }
        Ident | RawIdent | SelfTypeKw => Some(path_pattern(p)),
        LParen => Some(paren_or_tuple(p)),
        LBracket => Some(list_pattern(p)),
        k if k.is_strict_keyword() => {
            // A strict keyword in a binding position: E1301, kept as the name.
            let m = p.start();
            super::name(p);
            Some(m.complete(p, IdentPattern))
        }
        Comma | RParen | RBracket | RBrace | FatArrow | Eq | Colon | Pipe => {
            p.error(ParseErrorKind::MissingToken);
            None
        }
        _ if p.at_end() => {
            p.error(ParseErrorKind::MissingToken);
            None
        }
        _ => {
            p.err_and_bump(ParseErrorKind::UnexpectedTokens);
            None
        }
    }
}

/// A pattern that starts with a name: a binding (`x`), a qualified variant
/// (`State::idle`), or a constructor (`Some(x)`, `Point { x, y }`,
/// `Shape::Circle(r)`).
fn path_pattern(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let mut len = 1;
    while p.nth(len) == ColonColon && (p.nth_is_ident(len + 1) || p.nth(len + 1).is_keyword()) {
        len += 2;
    }
    let payload = matches!(p.nth(len), LParen | LBrace);
    let m = p.start();
    if payload {
        let path = p.start();
        super::types::type_path(p);
        path.complete(p, TypePath);
        if p.at(LParen) {
            p.bump_any(); // `(`
            while !p.at(RParen) && !p.at_end() {
                pattern(p);
                if !p.eat(Comma) {
                    break;
                }
            }
            p.expect(RParen);
        } else {
            p.bump_any(); // `{`
            while !p.at(RBrace) && !p.at_end() {
                record_pattern_field(p);
                if !p.eat(Comma) {
                    break;
                }
            }
            p.expect(RBrace);
        }
        m.complete(p, ConstructorPattern)
    } else if len > 1 {
        p.bump_any();
        while p.at(ColonColon) {
            p.bump_any(); // `::`
            super::label(p);
        }
        m.complete(p, QualifiedVariantPattern)
    } else if p.at(SelfTypeKw) {
        // `Self` alone names no binding; it is only a constructor path head.
        p.error(ParseErrorKind::MissingToken);
        p.bump_any();
        m.complete(p, QualifiedVariantPattern)
    } else {
        super::name(p);
        m.complete(p, IdentPattern)
    }
}

/// `Label ":" Pattern | IDENT | ".."` — one field of a record constructor
/// pattern. The shorthand `IDENT` also binds that name.
fn record_pattern_field(p: &mut Parser) {
    let m = p.start();
    if p.at(SyntaxKind::DotDot) {
        p.bump_any();
    } else if super::at_label(p) && p.nth(1) == SyntaxKind::Colon {
        super::label(p);
        p.bump_any(); // `:`
        pattern(p);
    } else if super::at_label(p) {
        super::name(p);
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
    m.complete(p, SyntaxKind::RecordPatternField);
}

/// `"(" Pattern ")"` (grouping) or `"(" Pattern "," ... ")"` (a tuple). The
/// trailing comma after a single element is what makes a one-tuple.
fn paren_or_tuple(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump_any(); // `(`
    let mut tuple = p.at(SyntaxKind::RParen);
    while !p.at(SyntaxKind::RParen) && !p.at_end() {
        pattern(p);
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
        tuple = true;
    }
    p.expect(SyntaxKind::RParen);
    let kind = if tuple {
        SyntaxKind::TuplePattern
    } else {
        SyntaxKind::ParenPattern
    };
    m.complete(p, kind)
}

/// `"[" (Pattern | ".." IDENT?),* "]"`.
fn list_pattern(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump_any(); // `[`
    while !p.at(SyntaxKind::RBracket) && !p.at_end() {
        if p.at(SyntaxKind::DotDot) {
            let r = p.start();
            p.bump_any(); // `..`
            if p.nth_is_ident(0) {
                super::name(p);
            }
            r.complete(p, SyntaxKind::RestPattern);
        } else {
            pattern(p);
        }
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
    }
    p.expect(SyntaxKind::RBracket);
    m.complete(p, SyntaxKind::ListPattern)
}
