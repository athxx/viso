//! The view grammar. A `view` declaration wraps a `ViewBlock` —
//! `"{" ViewStructureItem* "}"` — and every node body is another brace block of
//! members. Unlike an imperative [`block`](super::stmt::block), a view block holds
//! no statements or tail expression: only nodes, control flow, property bindings,
//! event handlers, and two-way bindings.
//!
//! ## The `:` here is a binding, not an ascription
//!
//! In a node body `PropertyPath ":" Expression ";"` binds a property; the same
//! colon in `node id: Type` separates a node's name from its component type. This is the declarative half of the `:` vs `=` split — the
//! imperative half (`let`/assignment) lives in [`super::stmt`]. The two never
//! collide because a view block is never entered from statement position.
//!
//! ## There is no `child` slot
//!
//! There is no implicit `child` slot; nodes carry explicit identity, so a
//! bare `child` node/name is diagnosed as E3001: an anonymous node is written as
//! its `Type { ... }` directly, and a named one as `node name: Type { ... }`.

use super::super::kind::SyntaxKind;
use super::super::span::TextRange;
use super::{ParseErrorKind, Parser, attributes, label, name};
use crate::diag::{Applicability, Fix, TextEdit};

/// `"view" ViewBlock` — the `view { ... }` member of a component.
pub(super) fn view_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::ViewKw);
    view_block(p);
    m.complete(p, SyntaxKind::ViewDecl);
}

/// A bare `ViewStructureItem*` body used by the `ui!` fragment entry, without the
/// surrounding braces a `view` declaration would add.
pub(super) fn view_fragment_items(p: &mut Parser) {
    while !p.at_end() {
        let before = p.cursor();
        view_structure_item(p);
        p.item_end();
        p.ensure_progress(before);
    }
}

/// `"{" ViewStructureItem* "}"` — a view block, a slot's default content or
/// what a slot fill puts in the slot.
fn view_block(p: &mut Parser) {
    if p.enter().is_some() {
        return;
    }
    let m = p.start();
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let before = p.cursor();
        view_structure_item(p);
        p.item_end();
        p.ensure_progress(before);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::ViewBlock);
    p.leave();
}

/// One structure item at the top of a view block: a node, a slot or a
/// control-flow form. Leading attributes are absorbed into the item they
/// decorate.
fn view_structure_item(p: &mut Parser) {
    attributes(p);
    match view_keyword(p) {
        Some(SyntaxKind::NodeKw) => return named_node(p),
        Some(SyntaxKind::SlotKw) => return slot_item(p),
        _ => {}
    }
    match p.current() {
        SyntaxKind::IfKw => view_if(p),
        SyntaxKind::ForKw => view_for(p),
        SyntaxKind::MatchKw => view_match(p),
        SyntaxKind::ChildKw => p.reserved_child(),
        _ if super::types::at_type_start(p) => anonymous_node(p),
        _ => p.err_and_bump(ParseErrorKind::UnexpectedTokens),
    }
}

/// `"node" IDENT ":" NodeHead`.
fn named_node(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::NodeKw);
    name(p);
    p.expect(SyntaxKind::Colon);
    node_head(p);
    m.complete(p, SyntaxKind::NamedNode);
}

/// `NodeHead` — an unnamed node, identified only by its type.
fn anonymous_node(p: &mut Parser) {
    let m = p.start();
    node_head(p);
    m.complete(p, SyntaxKind::AnonymousNode);
}

/// `ComponentType PrimaryArgument? NodeBody?`. The argument and the body
/// start on the type's line; a node with neither ends at its type.
fn node_head(p: &mut Parser) {
    component_type(p);
    if p.at(SyntaxKind::LParen) && !p.at_line_break() {
        primary_argument(p);
    }
    if p.at(SyntaxKind::LBrace) && !p.at_line_break() {
        node_body(p);
    }
}

/// A node's component type: a plain [`TypePath`](super::types).
fn component_type(p: &mut Parser) {
    super::types::type_(p);
}

/// `"(" Expr ")"` — the value of the node's primary property.
fn primary_argument(p: &mut Parser) {
    let m = p.start();
    p.bump_any();
    super::expr::expr(p);
    p.expect(SyntaxKind::RParen);
    m.complete(p, SyntaxKind::PrimaryArgument);
}

/// `"{" NodeMember* "}"` — the body of any node.
fn node_body(p: &mut Parser) {
    if p.enter().is_some() {
        return;
    }
    let m = p.start();
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let before = p.cursor();
        node_member(p);
        p.item_end();
        p.ensure_progress(before);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::NodeBody);
    p.leave();
}

/// One member of a node body: a property binding, a two-way binding, an event
/// handler, a slot fill, a nested node, a slot or a control-flow form.
/// Dispatch is on the leading keyword; a lowercase name before `{` fills a
/// slot; anything else is tried as a property binding (`PropertyPath : Expr`).
pub(super) fn node_member(p: &mut Parser) {
    attributes(p);
    match view_keyword(p) {
        Some(SyntaxKind::OnKw) => return event_handler(p),
        Some(SyntaxKind::BindKw) => return two_way_binding(p),
        Some(SyntaxKind::NodeKw) => return named_node(p),
        Some(SyntaxKind::SlotKw) => return slot_item(p),
        _ => {}
    }
    match p.current() {
        SyntaxKind::IfKw => view_if(p),
        SyntaxKind::ForKw => view_for(p),
        SyntaxKind::MatchKw => view_match(p),
        SyntaxKind::ChildKw => p.reserved_child(),
        // A leading name is a slot fill (`name { ... }`), a nested anonymous
        // node (`Type { ... }`) or a property binding (`path : expr`).
        SyntaxKind::Ident | SyntaxKind::RawIdent => {
            if starts_slot_fill(p) {
                slot_fill(p);
            } else if starts_anonymous_node(p) {
                anonymous_node(p);
            } else {
                property_binding(p);
            }
        }
        // A property path is a label, so `type: x;` binds a property named `type`.
        k if k.is_keyword() && matches!(p.nth(1), SyntaxKind::Colon | SyntaxKind::Dot) => {
            property_binding(p)
        }
        _ => p.err_and_bump(ParseErrorKind::UnexpectedTokens),
    }
}

/// The contextual keyword the cursor acts as at a node-body / view-block item
/// start: `node IDENT :`, `on IDENT`, `bind IDENT`, and `slot` unless a `:`
/// or `.` makes it a property path. Otherwise the word is an ordinary
/// identifier (`on: x;` binds a property named `on`).
fn view_keyword(p: &Parser) -> Option<SyntaxKind> {
    let kw = p.nth_contextual(0)?;
    let ok = match kw {
        SyntaxKind::NodeKw => p.nth_is_ident(1) && p.nth(2) == SyntaxKind::Colon,
        SyntaxKind::OnKw | SyntaxKind::BindKw => p.nth_is_ident(1),
        SyntaxKind::SlotKw => !matches!(p.nth(1), SyntaxKind::Colon | SyntaxKind::Dot),
        _ => false,
    };
    ok.then_some(kw)
}

/// Whether the upcoming tokens fill a slot: a lowercase name and a `{` on
/// its line. Slot names are `snake_case` and node types `UpperCamelCase`.
fn starts_slot_fill(p: &Parser) -> bool {
    let first = p.token_text(0).trim_start_matches("r#").chars().next();
    first.is_some_and(|c| c.is_lowercase() || c == '_')
        && p.nth(1) == SyntaxKind::LBrace
        && !p.line_start.get(p.pos + 1).copied().unwrap_or(false)
}

/// Whether the upcoming tokens are an anonymous node rather than a property
/// binding: a type path (`Ident (:: Ident)* (< ... >)?`) followed by its
/// argument, its body or the item's end. A property binding instead reaches
/// a `:` (with no `::`) or `.`.
fn starts_anonymous_node(p: &Parser) -> bool {
    let mut i = 0;
    // Walk a `::`-separated path of identifiers; generic args make it a type.
    loop {
        if !matches!(p.nth(i), SyntaxKind::Ident | SyntaxKind::RawIdent) {
            return false;
        }
        i += 1;
        match p.nth(i) {
            SyntaxKind::ColonColon => {
                i += 1;
                continue;
            }
            SyntaxKind::Lt | SyntaxKind::LParen | SyntaxKind::LBrace => return true,
            SyntaxKind::Colon | SyntaxKind::Dot => return false,
            _ => return p.nth_at_item_end(i),
        }
    }
}

/// `PropertyPath ":" Expression ";"` — the declarative property binding.
pub(super) fn property_binding(p: &mut Parser) {
    let m = p.start();
    property_path(p);
    p.expect(SyntaxKind::Colon);
    super::expr::expr(p);
    p.eat(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::PropertyBinding);
}

/// `Label ("." Label)*` — a dotted property path.
fn property_path(p: &mut Parser) {
    let m = p.start();
    label(p);
    while p.at(SyntaxKind::Dot) {
        p.bump_any(); // `.`
        label(p);
    }
    m.complete(p, SyntaxKind::PropertyPath);
}

/// `"bind" PropertyPath "<=>" AssignablePath ("using" TypePath)? ";"`.
fn two_way_binding(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::BindKw);
    property_path(p);
    p.expect(SyntaxKind::BidiArrow);
    assignable_path(p);
    if p.eat_contextual(SyntaxKind::UsingKw) {
        super::types::type_(p);
    }
    p.eat(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::TwoWayBinding);
}

/// `IDENT AssignableSuffix*` where a suffix is `"." IDENT` or `"[" Expr "]"` — the
/// mutable target of a two-way binding.
fn assignable_path(p: &mut Parser) {
    let m = p.start();
    name(p);
    loop {
        match p.current() {
            SyntaxKind::Dot => {
                p.bump_any(); // `.`
                label(p);
            }
            SyntaxKind::LBracket => {
                p.bump_any(); // `[`
                super::expr::expr(p);
                p.expect(SyntaxKind::RBracket);
            }
            _ => break,
        }
    }
    m.complete(p, SyntaxKind::AssignablePath);
}

/// `"on" EventPhase? IDENT ("(" Pattern ")")? Block`. An arrow body (`on x => ..`)
/// is diagnosed as E3201: handlers always use a block.
fn event_handler(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::OnKw);
    if p.nth_is_ident(1) {
        let _ = p.eat_contextual(SyntaxKind::CaptureKw) || p.eat_contextual(SyntaxKind::BubbleKw);
    }
    name(p);
    if p.at(SyntaxKind::LParen) {
        p.bump_any(); // `(`
        super::patterns::pattern(p);
        p.expect(SyntaxKind::RParen);
    }
    if p.at(SyntaxKind::FatArrow) {
        // `on click => expr` — the old arrow form is rejected in favor of a
        // block, into which the fix wraps the statement.
        let arrow = p.offset();
        let index = p.errors.len();
        p.error(ParseErrorKind::HandlerNotArrow);
        p.bump_any(); // `=>`
        let body = p.offset();
        super::stmt::statement(p);
        let end = p.consumed_end();
        if end > body
            && let Some(diagnostic) = p.errors.get_mut(index)
        {
            diagnostic.fixes.push(Fix {
                title: "wrap the handler in a block".to_string(),
                applicability: Applicability::MachineApplicable,
                edits: vec![
                    TextEdit::new(TextRange::new(arrow, body), "{ "),
                    TextEdit::new(TextRange::empty(end), " }"),
                ],
            });
        }
    } else {
        super::stmt::block(p);
    }
    m.complete(p, SyntaxKind::EventHandler);
}

/// `IDENT ViewBlock` — what a component node puts in its named slot.
fn slot_fill(p: &mut Parser) {
    let m = p.start();
    name(p);
    view_block(p);
    m.complete(p, SyntaxKind::SlotFill);
}

/// `"slot" IDENT? ViewBlock?` — where a component's view places the slot
/// `IDENT` (the default slot without one), with default content. The name
/// and the block start on the keyword's line.
fn slot_item(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::SlotKw);
    if p.nth_is_ident(0) && !p.at_line_break() {
        name(p);
    }
    if p.at(SyntaxKind::LBrace) && !p.at_line_break() {
        view_block(p);
    }
    m.complete(p, SyntaxKind::SlotItem);
}

/// `"if" HeadExpression ("preserve" STRING_LITERAL)? ViewBlock ("else" (ViewIf |
/// ViewBlock))?`.
fn view_if(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `if`
    super::expr::head_expr(p);
    if p.at_contextual(SyntaxKind::PreserveKw) {
        p.bump_as(SyntaxKind::PreserveKw);
        if p.at(SyntaxKind::StringLiteral) || p.at(SyntaxKind::RawStringLiteral) {
            p.bump_any();
        } else {
            // A dynamic or missing identity: consume what stands in its place so
            // the block still parses.
            p.error(ParseErrorKind::PreserveNotLiteral);
            if !p.at(SyntaxKind::LBrace) {
                super::expr::head_expr(p);
            }
        }
    }
    view_block(p);
    if p.eat(SyntaxKind::ElseKw) {
        if p.at(SyntaxKind::IfKw) {
            if p.enter().is_none() {
                view_if(p);
                p.leave();
            }
        } else {
            view_block(p);
        }
    }
    m.complete(p, SyntaxKind::ViewIf);
}

/// `"for" Pattern "in" HeadExpression "key" HeadExpression ViewBlock`. The `key`
/// clause is mandatory for repeated view content (stable identity); its absence is
/// diagnosed as E3401.
fn view_for(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `for`
    super::patterns::pattern(p);
    if !p.eat(SyntaxKind::InKw) {
        p.error(ParseErrorKind::MissingToken);
    }
    super::expr::head_expr(p);
    if p.eat_contextual(SyntaxKind::KeyKw) {
        super::expr::head_expr(p);
    } else {
        p.error(ParseErrorKind::ForMissingKey);
    }
    view_block(p);
    m.complete(p, SyntaxKind::ViewFor);
}

/// `"match" HeadExpression "{" ViewMatchArm ("," ViewMatchArm)* ","? "}"`.
fn view_match(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `match`
    super::expr::head_expr(p);
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        view_match_arm(p);
        if !p.eat(SyntaxKind::Comma) && !p.at_line_break() {
            break;
        }
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::ViewMatch);
}

/// `Pattern ("if" Expression)? "=>" ViewBlock` — one arm of a view match.
fn view_match_arm(p: &mut Parser) {
    let m = p.start();
    super::patterns::pattern(p);
    if p.eat(SyntaxKind::IfKw) {
        super::expr::expr(p);
    }
    p.expect(SyntaxKind::FatArrow);
    view_block(p);
    m.complete(p, SyntaxKind::ViewMatchArm);
}
