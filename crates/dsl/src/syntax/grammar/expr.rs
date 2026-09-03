//! The expression grammar: a Pratt (precedence-climbing) parser over the DSL's
//! operator table, producing the typed expression node kinds.
//!
//! ## Precedence
//!
//! The table has 15 binding levels, tightest first: postfix/primary, prefix
//! unary, `as` cast, `* / %`, `+ -`, `<< >>`, comparison (`< <= > >=`),
//! equality (`== !=`), `&`, `^`, `|`, `&&`, `||`, `??`, and range (`.. ..=`).
//! Two families are deliberately **non-associative** — comparison and equality
//! never chain (`a < b < c` is a diagnostic, not `(a < b) < c`), and neither do
//! the range operators — so the parser rejects a second operator at the same
//! level instead of silently choosing an associativity. `??` is right
//! associative; every other binary level is left associative.
//!
//! ## Left recursion without reopening nodes
//!
//! Precedence climbing parses a left operand, then — on seeing an operator that
//! binds tightly enough — wraps that already-parsed operand in a `BinaryExpr`
//! via [`Parser::start_at`], the forward-parent mechanism the event buffer
//! exists for. Postfix suffixes (`()` `[]` `.` `?.` `?`) work the same way: each
//! wraps the current expression as its first child.
//!
//! ## Record expressions in control-flow heads
//!
//! `Name { field: expr }` is ambiguous with the block that follows an `if` /
//! `match` / `for` / `while` head, so a record expression is only allowed there
//! inside parentheses. The [`Restrictions::no_record`] flag threads that context
//! down; hitting a `{` after a path in that position is diagnosed as E2801
//! rather than parsed as a record.

use super::super::kind::SyntaxKind;
use super::{CompletedMarker, ParseErrorKind, Parser};

/// Context that changes how an expression is parsed. Threaded down the climb so
/// a `{` is read as a record body in normal position but not in a control-flow
/// head (where it opens the block).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Restrictions {
    /// When set, a `Path { ... }` record expression is forbidden (the `{` starts
    /// the surrounding block instead) and diagnosed as E2801 if written bare.
    no_record: bool,
}

/// Whether the token at the cursor can begin an expression. Used by the
/// statement/entry grammar to decide between an expression and a recovery.
pub(super) fn at_expr_start(p: &Parser) -> bool {
    at_expr_start_kind(p.current())
}

fn at_expr_start_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        // Literals.
        IntLiteral
            | FloatLiteral
            | UnitLiteral
            | StringLiteral
            | RawStringLiteral
            | CharLiteral
            | ColorLiteral
            | TrueKw
            | FalseKw
            | NoneKw
            // Names and paths.
            | Ident
            | RawIdent
            | SelfValueKw
            | SelfTypeKw
            // Prefix operators.
            | Minus
            | Bang
            | Tilde
            | Amp
            | Star
            // Grouping / collections.
            | LParen
            | LBracket
            // Prefix keyword expressions.
            | IfKw
            | MatchKw
            | MoveKw
            // A `||`/`|` closure, or a leading range `..`.
            | PipePipe
            | Pipe
            | DotDot
            | DotDotEq
    )
}

/// Parses an expression in normal position (records allowed).
pub(super) fn expr(p: &mut Parser) {
    expr_bp(p, 0, Restrictions::default());
}

/// Parses an expression in a control-flow head: a bare record expression is
/// forbidden here (E2801) because its `{` would collide with the block.
pub(super) fn head_expr(p: &mut Parser) {
    expr_bp(p, 0, Restrictions { no_record: true });
}

/// Binding-power rungs, tightest-binding last so a larger number binds tighter.
/// Only the binary levels need a number; unary/postfix are handled structurally.
mod bp {
    pub(super) const RANGE: u8 = 1;
    pub(super) const NULLISH: u8 = 2;
    pub(super) const LOGIC_OR: u8 = 3;
    pub(super) const LOGIC_AND: u8 = 4;
    pub(super) const BIT_OR: u8 = 5;
    pub(super) const BIT_XOR: u8 = 6;
    pub(super) const BIT_AND: u8 = 7;
    pub(super) const EQUALITY: u8 = 8;
    pub(super) const COMPARISON: u8 = 9;
    pub(super) const SHIFT: u8 = 10;
    pub(super) const ADD: u8 = 11;
    pub(super) const MUL: u8 = 12;
    pub(super) const CAST: u8 = 13;
}

/// How a binary operator at a given level associates.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Assoc {
    Left,
    Right,
    /// Chaining is a diagnostic; the operator does not fold a second time.
    None,
}

/// The level and associativity of a binary operator, or `None` if `kind` is not
/// one. The `??` level is right-associative; comparison/equality/range are
/// non-associative; everything else is left-associative.
fn binary_op(kind: SyntaxKind) -> Option<(u8, Assoc)> {
    use SyntaxKind::*;
    let (level, assoc) = match kind {
        DotDot | DotDotEq => (bp::RANGE, Assoc::None),
        QuestionQuestion => (bp::NULLISH, Assoc::Right),
        PipePipe => (bp::LOGIC_OR, Assoc::Left),
        AmpAmp => (bp::LOGIC_AND, Assoc::Left),
        Pipe => (bp::BIT_OR, Assoc::Left),
        Caret => (bp::BIT_XOR, Assoc::Left),
        Amp => (bp::BIT_AND, Assoc::Left),
        EqEq | Neq => (bp::EQUALITY, Assoc::None),
        Lt | Le | Gt | Ge => (bp::COMPARISON, Assoc::None),
        Shl | Shr => (bp::SHIFT, Assoc::Left),
        Plus | Minus => (bp::ADD, Assoc::Left),
        Star | Slash | Percent => (bp::MUL, Assoc::Left),
        _ => return None,
    };
    Some((level, assoc))
}

/// Parses an expression binding at least as tightly as `min_bp`, folding binary
/// operators to the left (or right, per associativity) and diagnosing chained
/// non-associative operators.
fn expr_bp(p: &mut Parser, min_bp: u8, r: Restrictions) -> Option<CompletedMarker> {
    let mut lhs = unary_expr(p, r)?;

    loop {
        // The `as` cast binds tighter than any binary operator but looser than a
        // postfix suffix, so it is folded here at the top of the climb.
        if p.at(SyntaxKind::AsKw) && bp::CAST >= min_bp {
            let m = p.start_at(lhs);
            p.bump_any(); // `as`
            super::types::type_(p);
            lhs = m.complete(p, SyntaxKind::CastExpr);
            continue;
        }

        let Some((level, assoc)) = binary_op(p.current()) else {
            break;
        };
        if level < min_bp {
            break;
        }

        let is_range = matches!(p.current(), SyntaxKind::DotDot | SyntaxKind::DotDotEq);
        let m = p.start_at(lhs);
        p.bump_any(); // the operator

        // Non-associative operators fold exactly once: parse a right operand that
        // binds strictly tighter, so a second operator at the same level is left
        // for the caller and reported as an illegal chain.
        let next_min = match assoc {
            Assoc::Left | Assoc::None => level + 1,
            Assoc::Right => level,
        };

        // A range operator may have no right operand (`a..`), so only parse one
        // when an expression can start there.
        if is_range && !at_expr_start(p) {
            lhs = m.complete(p, SyntaxKind::RangeExpr);
        } else {
            expr_bp(p, next_min, r);
            let kind = if is_range {
                SyntaxKind::RangeExpr
            } else {
                SyntaxKind::BinaryExpr
            };
            lhs = m.complete(p, kind);
        }

        if assoc == Assoc::None {
            // A second operator at the same non-associative level is a chain.
            if let Some((next_level, _)) = binary_op(p.current())
                && next_level == level
            {
                let err = if is_range {
                    ParseErrorKind::NonAssocRange
                } else {
                    ParseErrorKind::NonAssocChain
                };
                p.error(err);
            }
            break;
        }
    }
    Some(lhs)
}

/// Parses a prefix unary expression (`- ! ~ & *`, and a leading range), or falls
/// through to a postfix expression.
fn unary_expr(p: &mut Parser, r: Restrictions) -> Option<CompletedMarker> {
    use SyntaxKind::*;
    match p.current() {
        Minus | Bang | Tilde | Amp | Star => {
            let m = p.start();
            p.bump_any();
            unary_expr(p, r);
            Some(m.complete(p, UnaryExpr))
        }
        // A prefix range (`..hi` / `..=hi` / bare `..`).
        DotDot | DotDotEq => {
            let m = p.start();
            p.bump_any();
            if at_expr_start(p) {
                unary_expr(p, r);
            }
            Some(m.complete(p, RangeExpr))
        }
        _ => postfix_expr(p, r),
    }
}

/// Parses a primary expression and then any run of postfix suffixes: calls,
/// indexing, field / optional-field access, and the try operator. Each suffix
/// wraps the current expression as its first child.
fn postfix_expr(p: &mut Parser, r: Restrictions) -> Option<CompletedMarker> {
    let mut lhs = primary_expr(p, r)?;
    loop {
        lhs = match p.current() {
            SyntaxKind::LParen => {
                let m = p.start_at(lhs);
                arg_list(p);
                m.complete(p, SyntaxKind::CallExpr)
            }
            SyntaxKind::LBracket => {
                let m = p.start_at(lhs);
                p.bump_any(); // `[`
                expr(p);
                p.expect(SyntaxKind::RBracket);
                m.complete(p, SyntaxKind::IndexExpr)
            }
            SyntaxKind::Dot => {
                let m = p.start_at(lhs);
                p.bump_any(); // `.`
                field_name(p);
                m.complete(p, SyntaxKind::FieldExpr)
            }
            SyntaxKind::QuestionDot => {
                let m = p.start_at(lhs);
                p.bump_any(); // `?.`
                field_name(p);
                m.complete(p, SyntaxKind::OptionalFieldExpr)
            }
            SyntaxKind::Question => {
                let m = p.start_at(lhs);
                p.bump_any(); // `?`
                m.complete(p, SyntaxKind::TryExpr)
            }
            // A turbofish on a call: `path::<T>(...)`.
            SyntaxKind::ColonColon if p.nth(1) == SyntaxKind::Lt => {
                let m = p.start_at(lhs);
                generic_call_args(p);
                // The turbofish must be followed by a call to be meaningful; if a
                // `(` follows, fold it in the same wrapper as a `CallExpr`.
                if p.at(SyntaxKind::LParen) {
                    arg_list(p);
                    m.complete(p, SyntaxKind::CallExpr)
                } else {
                    m.complete(p, SyntaxKind::PathExpr)
                }
            }
            _ => break,
        };
    }
    Some(lhs)
}

/// A field/method name after `.` or `?.`: an identifier or a tuple index.
fn field_name(p: &mut Parser) {
    if p.at(SyntaxKind::Ident) || p.at(SyntaxKind::RawIdent) || p.at(SyntaxKind::IntLiteral) {
        p.bump_any();
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
}

/// A `::<T, ...>` turbofish argument list on a call.
fn generic_call_args(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `::`
    p.bump_any(); // `<`
    while !p.at(SyntaxKind::Gt) && !p.at_end() {
        super::types::type_(p);
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
    }
    p.expect(SyntaxKind::Gt);
    m.complete(p, SyntaxKind::GenericCallArgs);
}

/// A `( arg, ... )` call argument list. Each argument is either positional
/// (`expr`) or named (`ident: expr`). Shared with the `emit` statement.
pub(super) fn arg_list(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `(`
    while !p.at(SyntaxKind::RParen) && !p.at_end() {
        argument(p);
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
    }
    p.expect(SyntaxKind::RParen);
    m.complete(p, SyntaxKind::ArgumentList);
}

/// One call argument: `ident: expr` (named) or `expr` (positional).
fn argument(p: &mut Parser) {
    let m = p.start();
    if (p.at(SyntaxKind::Ident) || p.at(SyntaxKind::RawIdent)) && p.nth(1) == SyntaxKind::Colon {
        p.bump_any(); // name
        p.bump_any(); // `:`
    }
    expr(p);
    m.complete(p, SyntaxKind::Argument);
}

/// Parses a primary expression: a literal, a path (optionally a record or a
/// call target), a parenthesized/tuple expression, a list, a closure, or an
/// `if`/`match` expression. Records forward parse errors as `None` so the caller
/// can recover.
fn primary_expr(p: &mut Parser, r: Restrictions) -> Option<CompletedMarker> {
    use SyntaxKind::*;
    let cm = match p.current() {
        IntLiteral | FloatLiteral | UnitLiteral | StringLiteral | RawStringLiteral
        | CharLiteral | ColorLiteral | TrueKw | FalseKw | NoneKw => {
            let m = p.start();
            p.bump_any();
            m.complete(p, LiteralExpr)
        }
        Ident | RawIdent | SelfValueKw | SelfTypeKw => path_or_record_expr(p, r),
        LParen => paren_or_tuple_expr(p),
        LBracket => list_expr(p),
        PipePipe | Pipe | MoveKw => closure_expr(p),
        IfKw => if_expr(p),
        MatchKw => match_expr(p),
        _ => {
            p.error(ParseErrorKind::ExpectedExpr);
            p.err_and_bump(ParseErrorKind::ExpectedExpr);
            return None;
        }
    };
    Some(cm)
}

/// A path expression, or — when a `{` follows and records are allowed here — a
/// record-construction expression. In a control-flow head a bare record is
/// diagnosed (E2801) instead of parsed.
fn path_or_record_expr(p: &mut Parser, r: Restrictions) -> CompletedMarker {
    let m = p.start();
    path(p);
    if p.at(SyntaxKind::LBrace) {
        if r.no_record {
            // The `{` opens the surrounding block, not a record body: flag the
            // ambiguity and leave the brace for the block grammar.
            p.error(ParseErrorKind::RecordExprInHead);
            return m.complete(p, SyntaxKind::PathExpr);
        }
        record_body(p);
        return m.complete(p, SyntaxKind::RecordExpr);
    }
    m.complete(p, SyntaxKind::PathExpr)
}

/// A bare `IDENT ("::" IDENT)*` path wrapped in a `PathExpr`, used by the
/// declaration grammar for attribute names (`@ Path`).
pub(super) fn path_only(p: &mut Parser) {
    let m = p.start();
    path(p);
    m.complete(p, SyntaxKind::PathExpr);
}

/// A `IDENT ("::" IDENT)*` path (segment turbofish is handled as a postfix).
fn path(p: &mut Parser) {
    p.bump_any(); // first segment
    while p.at(SyntaxKind::ColonColon) && p.nth(1) != SyntaxKind::Lt {
        p.bump_any(); // `::`
        if p.at(SyntaxKind::Ident) || p.at(SyntaxKind::RawIdent) {
            p.bump_any();
        } else {
            p.error(ParseErrorKind::MissingToken);
            break;
        }
    }
}

/// The `{ field: expr, .. }` body of a record expression.
fn record_body(p: &mut Parser) {
    p.bump_any(); // `{`
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let m = p.start();
        if p.eat(SyntaxKind::DotDot) {
            // A functional-update spread `.. base`.
            expr(p);
        } else if p.at(SyntaxKind::Ident) || p.at(SyntaxKind::RawIdent) {
            p.bump_any(); // field name
            if p.eat(SyntaxKind::Colon) {
                expr(p);
            }
        } else {
            p.error(ParseErrorKind::ExpectedExpr);
            m.abandon(p);
            p.err_and_bump(ParseErrorKind::UnexpectedTokens);
            continue;
        }
        m.complete(p, SyntaxKind::RecordExprField);
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
    }
    p.expect(SyntaxKind::RBrace);
}

/// A `( ... )` group: an empty unit `()`, a parenthesized expression, or a
/// tuple (a trailing or interior comma promotes it to a `TupleExpr`).
fn paren_or_tuple_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump_any(); // `(`
    let mut count = 0usize;
    let mut saw_comma = false;
    while !p.at(SyntaxKind::RParen) && !p.at_end() {
        expr(p);
        count += 1;
        if p.eat(SyntaxKind::Comma) {
            saw_comma = true;
        } else {
            break;
        }
    }
    p.expect(SyntaxKind::RParen);
    // `(e)` is a parenthesized expression; `()`, `(e,)`, `(a, b)` are tuples.
    let kind = if saw_comma || count != 1 {
        SyntaxKind::TupleExpr
    } else {
        SyntaxKind::ParenExpr
    };
    m.complete(p, kind)
}

/// A `[ e, ... ]` list expression.
fn list_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump_any(); // `[`
    while !p.at(SyntaxKind::RBracket) && !p.at_end() {
        expr(p);
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
    }
    p.expect(SyntaxKind::RBracket);
    m.complete(p, SyntaxKind::ListExpr)
}

/// A closure `move? (|params| | ||) (expr | block)`.
fn closure_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.eat(SyntaxKind::MoveKw);
    if p.at(SyntaxKind::PipePipe) {
        // An empty parameter list spelled `||`.
        p.bump_any();
    } else {
        closure_params(p);
    }
    // The body is a block or a bare expression.
    if p.at(SyntaxKind::LBrace) {
        super::stmt::block(p);
    } else {
        expr(p);
    }
    m.complete(p, SyntaxKind::ClosureExpr)
}

/// A `| param, ... |` closure parameter list.
fn closure_params(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `|`
    while !p.at(SyntaxKind::Pipe) && !p.at_end() {
        let param = p.start();
        p.eat(SyntaxKind::MutKw);
        if p.at(SyntaxKind::Ident) || p.at(SyntaxKind::RawIdent) {
            p.bump_any();
        } else {
            p.error(ParseErrorKind::MissingToken);
        }
        if p.eat(SyntaxKind::Colon) {
            super::types::type_(p);
        }
        param.complete(p, SyntaxKind::ClosureParam);
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
    }
    p.expect(SyntaxKind::Pipe);
    m.complete(p, SyntaxKind::ClosureParams);
}

/// An `if head { .. } else { .. }` expression. Both arms are required in
/// expression position, but recovery tolerates a missing `else`.
fn if_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump_any(); // `if`
    head_expr(p);
    super::stmt::block(p);
    if p.eat(SyntaxKind::ElseKw) {
        if p.at(SyntaxKind::IfKw) {
            if_expr(p);
        } else {
            super::stmt::block(p);
        }
    }
    m.complete(p, SyntaxKind::IfExpr)
}

/// A `match head { arm, ... }` expression.
fn match_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump_any(); // `match`
    head_expr(p);
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        match_arm(p);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::MatchExpr)
}

/// One `pattern (if guard)? => (expr | block)` match arm. Shared with the
/// statement grammar's `match` statement, which parses the same arm shape.
pub(super) fn match_arm(p: &mut Parser) {
    let m = p.start();
    super::patterns::pattern(p);
    if p.eat(SyntaxKind::IfKw) {
        expr(p);
    }
    p.expect(SyntaxKind::FatArrow);
    if p.at(SyntaxKind::LBrace) {
        super::stmt::block(p);
    } else {
        expr(p);
    }
    p.eat(SyntaxKind::Comma);
    m.complete(p, SyntaxKind::MatchArm);
}
