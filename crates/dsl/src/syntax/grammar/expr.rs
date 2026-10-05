//! The expression grammar: a Pratt (precedence-climbing) parser over the DSL's
//! operator table, producing the typed expression node kinds.
//!
//! ## Precedence
//!
//! The levels follow A.12, tightest first: postfix/primary, prefix unary
//! (`! ~ + - await`), `as` cast, `* / %`, `+ -`, `<< >>`, `&`, `^`, `|`, the
//! single comparison level (`== != < <= > >=`), `&&`, `||`, `??`, and range
//! (`.. ..=`). Comparison and range are **non-associative**: a second operator
//! at the same level is E2802 rather than a silently chosen associativity, and
//! the chain still folds left so the tree keeps every operand. `??` is right
//! associative; every other binary level is left associative. A range needs
//! both ends; a missing one is an expected-expression error.
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
    /// When set with `no_record`, a `{` after a path always opens the block:
    /// a style's `when` body binds properties, which a record body would
    /// otherwise look like.
    block_follows: bool,
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
            | Plus
            | Bang
            | Tilde
            | AwaitKw
            // Grouping / collections / block.
            | LParen
            | LBracket
            | LBrace
            // Prefix keyword expressions.
            | IfKw
            | MatchKw
            // A `||`/`|` closure (`move` lexes as an identifier).
            | PipePipe
            | Pipe
    )
}

/// Parses an expression in normal position (records allowed).
pub(super) fn expr(p: &mut Parser) {
    expr_bp(p, 0, Restrictions::default());
}

/// Parses an expression in a control-flow head: a bare record expression is
/// forbidden here (E2801) because its `{` would collide with the block.
pub(super) fn head_expr(p: &mut Parser) {
    expr_bp(
        p,
        0,
        Restrictions {
            no_record: true,
            block_follows: false,
        },
    );
}

/// Parses the selector of a style's `when`: an expression whose `{` always
/// opens the bindings after it.
pub(super) fn selector_expr(p: &mut Parser) {
    expr_bp(
        p,
        0,
        Restrictions {
            no_record: true,
            block_follows: true,
        },
    );
}

/// Parses a call or another postfix expression, with no binary operator
/// after it and no record body: the operand of `start`.
pub(super) fn call_expr(p: &mut Parser) {
    postfix_expr(
        p,
        Restrictions {
            no_record: true,
            block_follows: false,
        },
    );
}

/// Binding-power rungs, tightest-binding last so a larger number binds tighter.
/// Only the binary levels need a number; unary/postfix are handled structurally.
mod bp {
    pub(super) const RANGE: u8 = 1;
    pub(super) const NULLISH: u8 = 2;
    pub(super) const LOGIC_OR: u8 = 3;
    pub(super) const LOGIC_AND: u8 = 4;
    pub(super) const COMPARISON: u8 = 5;
    pub(super) const BIT_OR: u8 = 6;
    pub(super) const BIT_XOR: u8 = 7;
    pub(super) const BIT_AND: u8 = 8;
    pub(super) const SHIFT: u8 = 9;
    pub(super) const ADD: u8 = 10;
    pub(super) const MUL: u8 = 11;
    pub(super) const CAST: u8 = 12;
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
/// one. The `??` level is right-associative; comparison and range are
/// non-associative; everything else is left-associative.
fn binary_op(kind: SyntaxKind) -> Option<(u8, Assoc)> {
    use SyntaxKind::*;
    let (level, assoc) = match kind {
        DotDot | DotDotEq => (bp::RANGE, Assoc::None),
        QuestionQuestion => (bp::NULLISH, Assoc::Right),
        PipePipe => (bp::LOGIC_OR, Assoc::Left),
        AmpAmp => (bp::LOGIC_AND, Assoc::Left),
        EqEq | Neq | Lt | Le | Gt | Ge => (bp::COMPARISON, Assoc::None),
        Pipe => (bp::BIT_OR, Assoc::Left),
        Caret => (bp::BIT_XOR, Assoc::Left),
        Amp => (bp::BIT_AND, Assoc::Left),
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
    // The non-associative level the previous fold used, so a second operator at
    // that level is reported as a chain.
    let mut non_assoc: Option<u8> = None;

    loop {
        // The `as` cast binds tighter than any binary operator but looser than a
        // unary prefix, so it is folded here at the top of the climb.
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

        let is_range = level == bp::RANGE;
        if non_assoc == Some(level) {
            p.error(if is_range {
                ParseErrorKind::NonAssocRange
            } else {
                ParseErrorKind::NonAssocChain
            });
        }
        let m = p.start_at(lhs);
        p.bump_any(); // the operator

        // Non-associative operators take a right operand that binds strictly
        // tighter, so a second operator at the same level returns here.
        let next_min = match assoc {
            Assoc::Left | Assoc::None => level + 1,
            Assoc::Right => level,
        };
        // In a control-flow head a `{` opens the body, never the right operand.
        if at_expr_start(p) && !(r.no_record && p.at(SyntaxKind::LBrace)) {
            expr_bp(p, next_min, r);
        } else {
            p.error(ParseErrorKind::ExpectedExpr);
        }
        let kind = if is_range {
            SyntaxKind::RangeExpr
        } else {
            SyntaxKind::BinaryExpr
        };
        lhs = m.complete(p, kind);
        non_assoc = (assoc == Assoc::None).then_some(level);
    }
    Some(lhs)
}

/// Parses a prefix unary expression (`! ~ + - await`), or falls through to a
/// postfix expression. A range with no start (`..hi`) is diagnosed and parsed
/// as a range so the operand still lands in the tree.
fn unary_expr(p: &mut Parser, r: Restrictions) -> Option<CompletedMarker> {
    use SyntaxKind::*;
    match p.current() {
        Minus | Plus | Bang | Tilde | AwaitKw => {
            let m = p.start();
            p.bump_any();
            unary_expr(p, r);
            Some(m.complete(p, UnaryExpr))
        }
        DotDot | DotDotEq => {
            let m = p.start();
            p.error(ParseErrorKind::ExpectedExpr);
            p.bump_any();
            if at_expr_start(p) {
                expr_bp(p, bp::NULLISH, r);
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
        // `name<T>(..)` without the turbofish (E2004): recognized only when the
        // `<...>` run reads as type arguments and a call, path or record follows,
        // so `a < b` stays a comparison.
        if p.at(SyntaxKind::Lt) && p.kind_of(lhs) == SyntaxKind::PathExpr && at_bare_generics(p, r)
        {
            p.error(ParseErrorKind::GenericWithoutTurbofish);
            lhs = generic_suffix(p, lhs, r);
            continue;
        }
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
            // A turbofish on a call or record: `path::<T>(...)`.
            SyntaxKind::ColonColon if p.nth(1) == SyntaxKind::Lt => generic_suffix(p, lhs, r),
            _ => break,
        };
    }
    Some(lhs)
}

/// Folds a generic-argument list (with or without its turbofish `::`) onto
/// `lhs`, together with the call or record body that follows it.
fn generic_suffix(p: &mut Parser, lhs: CompletedMarker, r: Restrictions) -> CompletedMarker {
    let m = p.start_at(lhs);
    super::types::generic_args(p, SyntaxKind::GenericCallArgs);
    if p.at(SyntaxKind::LParen) {
        arg_list(p);
        m.complete(p, SyntaxKind::CallExpr)
    } else if p.at(SyntaxKind::LBrace) && !r.no_record {
        record_body(p);
        m.complete(p, SyntaxKind::RecordExpr)
    } else {
        m.complete(p, SyntaxKind::PathExpr)
    }
}

/// Whether the `<` at the cursor opens generic arguments written without a
/// turbofish. Every argument must start like a type (an uppercase name,
/// `Self`, `dyn`, `const`, `(` or `[`), the list must balance within the
/// type-token alphabet, and a `(`, `::` or record `{` must follow the close.
fn at_bare_generics(p: &Parser, r: Restrictions) -> bool {
    use SyntaxKind::*;
    const LIMIT: usize = 64;
    let mut angle = 0i32;
    let mut nest = 0i32;
    let mut arg_start = true;
    for n in 0..LIMIT {
        let kind = p.nth(n);
        if arg_start && angle == 1 && nest == 0 {
            let type_like = match kind {
                Ident => p.token_text(n).starts_with(|c: char| c.is_uppercase()),
                RawIdent | SelfTypeKw | DynKw | ConstKw | LParen | LBracket => true,
                _ => false,
            };
            if !type_like {
                return false;
            }
        }
        arg_start = false;
        match kind {
            Lt => angle += 1,
            Gt => angle -= 1,
            Shr => angle -= 2,
            LParen | LBracket => nest += 1,
            RParen | RBracket => nest -= 1,
            Comma => arg_start = true,
            Ident | RawIdent | ColonColon | SelfTypeKw | DynKw | ConstKw | Semi | IntLiteral
            | Plus | Minus | Arrow => {}
            _ => return false,
        }
        if angle < 0 || nest < 0 {
            return false;
        }
        if angle == 0 {
            return match p.nth(n + 1) {
                LParen | ColonColon => true,
                LBrace => !r.no_record,
                _ => false,
            };
        }
    }
    false
}

/// A field/method name after `.` or `?.`: a label or a tuple index.
fn field_name(p: &mut Parser) {
    if p.at(SyntaxKind::IntLiteral) {
        p.bump_any();
    } else {
        super::label(p);
    }
}

/// A const generic argument's expression (`const 4`, `const N * 2`): parsed
/// above the comparison and shift levels so a closing `>`/`>>` ends it.
pub(super) fn const_arg_expr(p: &mut Parser) {
    expr_bp(p, bp::ADD, Restrictions::default());
}

/// A `( arg, ... )` call argument list. Each argument is either positional
/// (`expr`) or named (`ident: expr`). Shared with the `emit` statement.
pub(super) fn arg_list(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `(`
    while !p.at(SyntaxKind::RParen) && !p.at_end() {
        argument(p);
        if !p.eat(SyntaxKind::Comma) {
            // `f(a b)`: report the missing comma and keep reading arguments.
            if at_expr_start(p) {
                p.error(ParseErrorKind::MissingToken);
                continue;
            }
            break;
        }
    }
    p.expect(SyntaxKind::RParen);
    m.complete(p, SyntaxKind::ArgumentList);
}

/// One call argument: `label: expr` (named) or `expr` (positional).
fn argument(p: &mut Parser) {
    let m = p.start();
    if super::at_label(p) && p.nth(1) == SyntaxKind::Colon {
        super::label(p);
        p.bump_any(); // `:`
    }
    expr(p);
    m.complete(p, SyntaxKind::Argument);
}

/// Parses a primary expression: a literal, a path (optionally a record or a
/// call target), a parenthesized/tuple expression, a list, a closure, or an
/// `if`/`match` expression, or a block. Records forward parse errors as `None`
/// so the caller can recover.
fn primary_expr(p: &mut Parser, r: Restrictions) -> Option<CompletedMarker> {
    use SyntaxKind::*;
    let cm = match p.current() {
        IntLiteral | FloatLiteral | UnitLiteral | StringLiteral | RawStringLiteral
        | CharLiteral | ColorLiteral | TrueKw | FalseKw | NoneKw => {
            let m = p.start();
            p.bump_any();
            m.complete(p, LiteralExpr)
        }
        Ident if p.at_contextual(MoveKw) && matches!(p.nth(1), Pipe | PipePipe) => closure_expr(p),
        Ident | RawIdent | SelfValueKw | SelfTypeKw => path_or_record_expr(p, r),
        LParen => paren_or_tuple_expr(p),
        LBracket => list_expr(p),
        LBrace => block_expr(p),
        PipePipe | Pipe => closure_expr(p),
        IfKw => if_expr(p),
        MatchKw => match_expr(p),
        // A separator or closer belongs to the enclosing production: report the
        // gap without consuming it.
        Semi | Comma | RParen | RBracket | RBrace | FatArrow | Eof => {
            p.error(ParseErrorKind::ExpectedExpr);
            return None;
        }
        _ => {
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
            if r.block_follows || !looks_like_record_body(p) {
                // The `{` opens the surrounding block.
                return m.complete(p, SyntaxKind::PathExpr);
            }
            // A record literal written bare in the head: flag the ambiguity and
            // still read it as a record so the real block after it parses.
            p.error(ParseErrorKind::RecordExprInHead);
        }
        record_body(p);
        return m.complete(p, SyntaxKind::RecordExpr);
    }
    m.complete(p, SyntaxKind::PathExpr)
}

/// Whether the `{` at the cursor opens something only a record body can be:
/// `{ label: ..`, `{ name, ..`, `{ ..base`, or an empty `{ }` directly followed
/// by another `{`. Anything else is the control-flow block.
fn looks_like_record_body(p: &Parser) -> bool {
    use SyntaxKind::*;
    let label_at = |n: usize| p.nth_is_ident(n) || p.nth(n).is_keyword();
    match p.nth(1) {
        DotDot => true,
        RBrace => p.nth(2) == LBrace,
        _ if label_at(1) => p.nth(2) == Colon || (p.nth(2) == Comma && label_at(3)),
        _ => false,
    }
}

/// A bare `IDENT ("::" IDENT)*` path wrapped in a `PathExpr`, used by the
/// declaration grammar for attribute names (`@ Path`).
pub(super) fn path_only(p: &mut Parser) {
    let m = p.start();
    path(p);
    m.complete(p, SyntaxKind::PathExpr);
}

/// A `IDENT ("::" Label)*` path (segment turbofish is handled as a postfix).
fn path(p: &mut Parser) {
    p.bump_any(); // first segment
    while p.at(SyntaxKind::ColonColon) && p.nth(1) != SyntaxKind::Lt {
        p.bump_any(); // `::`
        if !super::at_label(p) {
            p.error(ParseErrorKind::MissingToken);
            break;
        }
        super::label(p);
    }
}

/// A block in expression position.
pub(super) fn block_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    super::stmt::block(p);
    m.complete(p, SyntaxKind::BlockExpr)
}

/// The `{ field: expr, .. }` body of a record expression.
fn record_body(p: &mut Parser) {
    p.bump_any(); // `{`
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let m = p.start();
        if p.eat(SyntaxKind::DotDot) {
            // A functional-update spread `.. base`.
            expr(p);
        } else if super::at_label(p) && p.nth(1) == SyntaxKind::Colon {
            super::label(p);
            p.bump_any(); // `:`
            expr(p);
        } else if super::at_label(p) {
            // Shorthand `{ id }` binds a name, so a strict keyword is E1301.
            super::name(p);
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

/// A closure `move? (|params| | ||) (-> Type)? (expr | block)`.
fn closure_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.eat_contextual(SyntaxKind::MoveKw);
    if p.at(SyntaxKind::PipePipe) {
        // An empty parameter list spelled `||`.
        p.bump_any();
    } else {
        closure_params(p);
    }
    if p.eat(SyntaxKind::Arrow) {
        super::types::type_(p);
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
        // No top-level `|` alternatives: that `|` closes the parameter list.
        super::patterns::pattern_no_alt(p);
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

/// An `if head { .. } else { .. }` expression. The `else` arm is required in
/// expression position; a missing one is reported and the tree keeps its shape.
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
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
    m.complete(p, SyntaxKind::IfExpr)
}

/// A `match head { arm, ... }` expression.
fn match_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump_any(); // `match`
    head_expr(p);
    match_arms(p);
    m.complete(p, SyntaxKind::MatchExpr)
}

/// The `{ arm, ... }` arm list shared by the `match` expression and statement.
/// Arms are comma-separated with an optional trailing comma.
pub(super) fn match_arms(p: &mut Parser) {
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let before = p.cursor();
        match_arm(p);
        if !p.eat(SyntaxKind::Comma) && !p.at(SyntaxKind::RBrace) {
            p.error(ParseErrorKind::MissingToken);
        }
        p.ensure_progress(before);
    }
    p.expect(SyntaxKind::RBrace);
}

/// One `pattern (if guard)? => (expr | block)` match arm.
fn match_arm(p: &mut Parser) {
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
    m.complete(p, SyntaxKind::MatchArm);
}
