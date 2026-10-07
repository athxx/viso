//! The statement grammar (Appendix A.11). A block is `"{" Statement* TailExpr?
//! "}"`; a statement is a `let`, an assignment, an expression statement, one of
//! the jump statements (`return`/`break`/`continue`/`emit`), a `transaction`, or
//! one of the imperative control-flow forms (`if`/`match`/`while`/`for`/`loop`).
//!
//! ## The `:` vs `=` split
//!
//! This is imperative code, so binding uses `=`: `let mut p: T = e;` and
//! `path op= e;` are the only places `=` (or an augmenting `+=` family operator)
//! appears. A `:` here is only ever a type ascription (`let x: T`), never a
//! property binding — property binding with `:` lives in the view grammar. The
//! two never collide because the grammar position decides which one is legal.
//!
//! ## Leading `if` / `match` are statements, not tail expressions
//!
//! At statement start a bare `if`/`match` is parsed as an `IfStatement` /
//! `MatchStatement` (Appendix A.11 note), so an `if`/`match` that is meant to be
//! the block's tail value must be parenthesized or `return`ed. Preferring the
//! statement form here removes the `Statement* TailExpression?` ambiguity.

use super::super::kind::SyntaxKind;
use super::{ParseErrorKind, Parser};

/// A `{ Statement* TailExpr? }` block used as a function/closure/control body.
pub(super) fn block(p: &mut Parser) {
    if p.enter().is_some() {
        return;
    }
    let m = p.start();
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let before = p.cursor();
        statement(p);
        p.ensure_progress(before);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::Block);
    p.leave();
}

/// Parses one statement (or the block's trailing tail expression). Dispatches on
/// the leading token; anything that cannot start a statement is wrapped in an
/// error node so recovery makes progress without dropping the token.
pub(super) fn statement(p: &mut Parser) {
    super::attributes(p);
    match p.current() {
        SyntaxKind::LetKw => let_stmt(p),
        SyntaxKind::ReturnKw => return_stmt(p),
        SyntaxKind::BreakKw => break_stmt(p),
        SyntaxKind::ContinueKw => continue_stmt(p),
        SyntaxKind::Ident if p.at_contextual(SyntaxKind::EmitKw) && p.nth_is_ident(1) => {
            emit_stmt(p)
        }
        SyntaxKind::Ident
            if p.at_contextual(SyntaxKind::TransactionKw) && p.nth(1) == SyntaxKind::LBrace =>
        {
            transaction_stmt(p)
        }
        SyntaxKind::Ident if p.at_contextual(SyntaxKind::StartKw) && p.nth_is_ident(1) => {
            start_stmt(p)
        }
        SyntaxKind::LBrace => block_stmt(p),
        SyntaxKind::WhileKw => while_stmt(p),
        SyntaxKind::LoopKw => loop_stmt(p),
        SyntaxKind::ForKw => for_stmt(p),
        // A leading `if`/`match` binds as the statement form, not a tail value.
        SyntaxKind::IfKw => if_stmt(p),
        SyntaxKind::MatchKw => match_stmt(p),
        _ if super::expr::at_expr_start(p) => expr_or_assign_stmt(p),
        _ => p.err_and_bump(ParseErrorKind::UnexpectedTokens),
    }
}

/// `"let" "mut"? Pattern (":" Type)? "=" Expression ";"`.
fn let_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `let`
    p.eat(SyntaxKind::MutKw);
    super::patterns::pattern(p);
    if p.eat(SyntaxKind::Colon) {
        super::types::type_(p);
    }
    if p.eat(SyntaxKind::Eq) {
        super::expr::expr(p);
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::LetStmt);
}

/// Either an assignment statement (`AssignablePath op= Expr ";"`) or a bare
/// expression statement (`Expr ";"`). The two share a leading expression, so we
/// parse the expression first and then decide based on a following assignment
/// operator — an `AssignStmt` re-labels the whole thing and captures the RHS.
fn expr_or_assign_stmt(p: &mut Parser) {
    let m = p.start();
    super::expr::expr(p);
    if is_assign_op(p.current()) {
        // The left side must have been an assignable path; a non-path LHS is a
        // semantic error caught later, but the shape is still an assignment.
        p.bump_any(); // the assignment operator
        super::expr::expr(p);
        p.expect(SyntaxKind::Semi);
        m.complete(p, SyntaxKind::AssignStmt);
    } else {
        // The block's last expression may omit its `;`: it is the tail value.
        if !p.at(SyntaxKind::RBrace) && !p.at_end() {
            p.expect(SyntaxKind::Semi);
        }
        m.complete(p, SyntaxKind::ExprStmt);
    }
}

/// A nested `{ ... }` block in statement position, with an optional `;`. Its
/// value is the tail when it ends the enclosing block.
fn block_stmt(p: &mut Parser) {
    let m = p.start();
    super::expr::block_expr(p);
    p.eat(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ExprStmt);
}

/// Whether `kind` is one of the assignment operators (`=` and the augmenting
/// family) that turns a leading expression statement into an assignment.
fn is_assign_op(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Eq
            | SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
            | SyntaxKind::PercentEq
            | SyntaxKind::AmpEq
            | SyntaxKind::PipeEq
            | SyntaxKind::CaretEq
            | SyntaxKind::ShlEq
            | SyntaxKind::ShrEq
    )
}

/// `"return" Expression? ";"`.
fn return_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `return`
    if super::expr::at_expr_start(p) {
        super::expr::expr(p);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ReturnStmt);
}

/// `"break" Expression? ";"`.
fn break_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `break`
    if super::expr::at_expr_start(p) {
        super::expr::expr(p);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::BreakStmt);
}

/// `"continue" ";"`.
fn continue_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `continue`
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ContinueStmt);
}

/// `"emit" IDENT "(" ArgumentList ")" ";"`.
fn emit_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::EmitKw);
    super::name(p);
    if p.at(SyntaxKind::LParen) {
        super::expr::arg_list(p);
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::EmitStmt);
}

/// `"transaction" Block`.
fn transaction_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::TransactionKw);
    block(p);
    m.complete(p, SyntaxKind::TransactionStmt);
}

/// `"start" CallExpression ("as" IDENT)? StartHandlers? ";"` (§39). The call
/// is a postfix expression, so `as` names the slot instead of casting.
fn start_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::StartKw);
    super::expr::call_expr(p);
    if p.at(SyntaxKind::AsKw) {
        let slot = p.start();
        p.bump_any(); // `as`
        super::name(p);
        slot.complete(p, SyntaxKind::StartSlot);
    }
    if p.at(SyntaxKind::LBrace) {
        start_handlers(p);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::StartStmt);
}

/// `"{" (policy "=" Expr ";" | success "(" Pattern ")" Block | error "("
/// Pattern ")" Block | cancelled Block)* "}"`.
fn start_handlers(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `{`
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let before = p.cursor();
        let item = p.start();
        if p.at_contextual(SyntaxKind::PolicyKw) && p.nth(1) == SyntaxKind::Eq {
            p.bump_as(SyntaxKind::PolicyKw);
            p.bump_any(); // `=`
            super::expr::expr(p);
            p.expect(SyntaxKind::Semi);
            item.complete(p, SyntaxKind::StartPolicy);
        } else if p.at_contextual(SyntaxKind::SuccessKw) && p.nth(1) == SyntaxKind::LParen {
            p.bump_as(SyntaxKind::SuccessKw);
            payload_handler(p);
            item.complete(p, SyntaxKind::StartSuccess);
        } else if p.at_contextual(SyntaxKind::ErrorKw) && p.nth(1) == SyntaxKind::LParen {
            p.bump_as(SyntaxKind::ErrorKw);
            payload_handler(p);
            item.complete(p, SyntaxKind::StartError);
        } else if p.at_contextual(SyntaxKind::CancelledKw) && p.nth(1) == SyntaxKind::LBrace {
            p.bump_as(SyntaxKind::CancelledKw);
            block(p);
            item.complete(p, SyntaxKind::StartCancelled);
        } else {
            item.abandon(p);
            p.err_and_bump(ParseErrorKind::UnexpectedTokens);
        }
        p.ensure_progress(before);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::StartHandlers);
}

/// `"(" Pattern ")" Block` — a `success` or `error` handler after its keyword.
fn payload_handler(p: &mut Parser) {
    p.bump_any(); // `(`
    super::patterns::pattern(p);
    p.expect(SyntaxKind::RParen);
    block(p);
}

/// `"while" HeadExpression Block`.
fn while_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `while`
    super::expr::head_expr(p);
    block(p);
    m.complete(p, SyntaxKind::WhileStmt);
}

/// `"loop" Block`.
fn loop_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `loop`
    block(p);
    m.complete(p, SyntaxKind::LoopStmt);
}

/// `"for" Pattern "in" HeadExpression Block` — the imperative loop, which (unlike
/// the view `for`) takes no `key` clause.
fn for_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `for`
    super::patterns::pattern(p);
    if !p.eat(SyntaxKind::InKw) {
        p.error(ParseErrorKind::MissingToken);
    }
    super::expr::head_expr(p);
    block(p);
    m.complete(p, SyntaxKind::ForStmt);
}

/// `"if" HeadExpression Block ("else" (IfStatement | Block))?` — statement form.
fn if_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `if`
    super::expr::head_expr(p);
    block(p);
    if p.eat(SyntaxKind::ElseKw) {
        if p.at(SyntaxKind::IfKw) {
            if p.enter().is_none() {
                if_stmt(p);
                p.leave();
            }
        } else {
            block(p);
        }
    }
    m.complete(p, SyntaxKind::IfStmt);
}

/// `MatchExpression ";"?` — a `match` used in statement position.
fn match_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `match`
    super::expr::head_expr(p);
    super::expr::match_arms(p);
    p.eat(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::MatchStmt);
}
