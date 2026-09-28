//! The view grammar (Appendix A.8). A `view` declaration wraps a `ViewBlock` —
//! `"{" ViewStructureItem* "}"` — and every node body is another brace block of
//! members. Unlike an imperative [`block`](super::stmt::block), a view block holds
//! no statements or tail expression: only nodes, control flow, property bindings,
//! event handlers, and two-way bindings.
//!
//! ## The `:` here is a binding, not an ascription
//!
//! In a node body `PropertyPath ":" Expression ";"` binds a property; the same
//! colon in `node id: Type` and `part id: Type` separates a node's name from its
//! component type. This is the declarative half of the `:` vs `=` split — the
//! imperative half (`let`/assignment) lives in [`super::stmt`]. The two never
//! collide because a view block is never entered from statement position.
//!
//! ## `child` is gone
//!
//! Makepad's implicit `child` slot is replaced by explicit node identity, so a
//! bare `child` node/name is diagnosed as E3001: an anonymous node is written as
//! its `Type { ... }` directly, and a named one as `node name: Type { ... }`.

use super::super::kind::SyntaxKind;
use super::{ParseErrorKind, Parser, attributes, label, name};

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
        p.ensure_progress(before);
    }
}

/// `"{" ViewStructureItem* "}"` — a view block or the body of a `fill` clause.
fn view_block(p: &mut Parser) {
    let m = p.start();
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let before = p.cursor();
        view_structure_item(p);
        p.ensure_progress(before);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::ViewBlock);
}

/// One structure item at the top of a view block: a node, a control-flow form, or
/// a `template` use (parsed as an advanced item). Leading attributes are absorbed
/// into the item they decorate.
fn view_structure_item(p: &mut Parser) {
    attributes(p);
    match view_keyword(p) {
        Some(SyntaxKind::NodeKw) => return named_node(p),
        Some(SyntaxKind::PartKw) => return part_node(p),
        Some(SyntaxKind::UseKw) => return template_use(p),
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

/// `"node" IDENT ":" ComponentType NodeBody`.
fn named_node(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::NodeKw);
    name(p);
    p.expect(SyntaxKind::Colon);
    component_type(p);
    node_body(p);
    m.complete(p, SyntaxKind::NamedNode);
}

/// `"part" IDENT ":" ComponentType NodeBody`.
fn part_node(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::PartKw);
    name(p);
    p.expect(SyntaxKind::Colon);
    component_type(p);
    node_body(p);
    m.complete(p, SyntaxKind::PartNode);
}

/// `ComponentType NodeBody` — an unnamed node, identified only by its type.
fn anonymous_node(p: &mut Parser) {
    let m = p.start();
    component_type(p);
    node_body(p);
    m.complete(p, SyntaxKind::AnonymousNode);
}

/// A node's component type: a plain [`TypePath`](super::types).
fn component_type(p: &mut Parser) {
    super::types::type_(p);
}

/// `"{" NodeMember* "}"` — the body of any node.
fn node_body(p: &mut Parser) {
    let m = p.start();
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let before = p.cursor();
        node_member(p);
        p.ensure_progress(before);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::NodeBody);
}

/// One member of a node body: a property binding, a two-way binding, an event
/// handler, a `fill` clause, a nested node, a control-flow form, or a part
/// override/replacement. Dispatch is on the leading keyword; anything else is
/// tried as a property binding (`PropertyPath : Expr ;`).
fn node_member(p: &mut Parser) {
    attributes(p);
    match view_keyword(p) {
        Some(SyntaxKind::OnKw) => return event_handler(p),
        Some(SyntaxKind::BindKw) => return two_way_binding(p),
        Some(SyntaxKind::FillKw) => return fill_clause(p),
        Some(SyntaxKind::NodeKw) => return named_node(p),
        Some(SyntaxKind::PartKw) => return part_node(p),
        Some(SyntaxKind::UseKw) => return template_use(p),
        Some(SyntaxKind::OverrideKw) => return part_override(p),
        Some(SyntaxKind::ReplaceKw) => return part_replace(p),
        _ => {}
    }
    match p.current() {
        SyntaxKind::IfKw => view_if(p),
        SyntaxKind::ForKw => view_for(p),
        SyntaxKind::MatchKw => view_match(p),
        SyntaxKind::ChildKw => p.reserved_child(),
        // A leading name is either a nested anonymous node (`Type { ... }`) or a
        // property binding (`path : expr ;`). A `{` after the type marks the node;
        // otherwise it is a property path.
        SyntaxKind::Ident | SyntaxKind::RawIdent => {
            if starts_anonymous_node(p) {
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
/// start (§12.4): `node IDENT :`, `part IDENT :`, `on IDENT`, `bind IDENT`,
/// `fill IDENT`, `use IDENT`, `override part`, `replace part`. Otherwise the
/// word is an ordinary identifier (`on: x;` binds a property named `on`).
fn view_keyword(p: &Parser) -> Option<SyntaxKind> {
    let kw = p.nth_contextual(0)?;
    let ok = match kw {
        SyntaxKind::NodeKw | SyntaxKind::PartKw => {
            p.nth_is_ident(1) && p.nth(2) == SyntaxKind::Colon
        }
        SyntaxKind::OnKw | SyntaxKind::BindKw | SyntaxKind::FillKw | SyntaxKind::UseKw => {
            p.nth_is_ident(1)
        }
        SyntaxKind::OverrideKw | SyntaxKind::ReplaceKw => {
            p.nth_at_contextual(1, SyntaxKind::PartKw)
        }
        _ => false,
    };
    ok.then_some(kw)
}

/// Whether the upcoming tokens are an anonymous node rather than a property
/// binding: a type path (`Ident (:: Ident)* (< ... >)?`) immediately followed by
/// a `{`. A property binding instead reaches a `:` (with no `::`) or `.` before
/// any brace.
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
            // A generic argument list or an immediate brace makes this a node.
            SyntaxKind::Lt => return true,
            SyntaxKind::LBrace => return true,
            _ => return false,
        }
    }
}

/// `PropertyPath ":" Expression ";"` — the declarative property binding.
fn property_binding(p: &mut Parser) {
    let m = p.start();
    property_path(p);
    p.expect(SyntaxKind::Colon);
    super::expr::expr(p);
    p.expect(SyntaxKind::Semi);
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
    p.expect(SyntaxKind::Semi);
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
        // `on click => expr` — the old arrow form is rejected in favor of a block.
        p.error(ParseErrorKind::HandlerNotArrow);
        p.bump_any(); // `=>`
        super::stmt::statement(p);
    } else {
        super::stmt::block(p);
    }
    m.complete(p, SyntaxKind::EventHandler);
}

/// `"fill" IDENT ViewBlock` — content projected into a named slot.
fn fill_clause(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::FillKw);
    name(p);
    view_block(p);
    m.complete(p, SyntaxKind::FillClause);
}

/// `"if" HeadExpression ("preserve" STRING_LITERAL)? ViewBlock ("else" (ViewIf |
/// ViewBlock))?`.
fn view_if(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `if`
    super::expr::head_expr(p);
    if p.at_contextual(SyntaxKind::PreserveKw)
        && matches!(
            p.nth(1),
            SyntaxKind::StringLiteral | SyntaxKind::RawStringLiteral
        )
    {
        p.bump_as(SyntaxKind::PreserveKw);
        if p.at(SyntaxKind::StringLiteral) || p.at(SyntaxKind::RawStringLiteral) {
            p.bump_any();
        } else {
            p.error(ParseErrorKind::MissingToken);
        }
    }
    view_block(p);
    if p.eat(SyntaxKind::ElseKw) {
        if p.at(SyntaxKind::IfKw) {
            view_if(p);
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
        if !p.eat(SyntaxKind::Comma) {
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

/// `"override" "part" IDENT "{" (PropertyBinding | TwoWayBinding | EventHandler)*
/// "}"`. Parsed as an advanced item until part metaprogramming resolution lands.
fn part_override(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::OverrideKw);
    p.bump_as(SyntaxKind::PartKw);
    name(p);
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        match p.current() {
            SyntaxKind::Ident if view_keyword(p) == Some(SyntaxKind::OnKw) => event_handler(p),
            SyntaxKind::Ident if view_keyword(p) == Some(SyntaxKind::BindKw) => two_way_binding(p),
            SyntaxKind::Ident | SyntaxKind::RawIdent => property_binding(p),
            _ => p.err_and_bump(ParseErrorKind::UnexpectedTokens),
        }
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::AdvancedItem);
}

/// `"replace" "part" IDENT ViewBlock`. Parsed as an advanced item for now.
fn part_replace(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::ReplaceKw);
    p.bump_as(SyntaxKind::PartKw);
    name(p);
    view_block(p);
    m.complete(p, SyntaxKind::AdvancedItem);
}

/// A `use` template instantiation. Parsed to an advanced item until template
/// resolution lands; the whole construct is consumed up to its terminator so
/// recovery is clean.
fn template_use(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::UseKw);
    super::types::type_(p);
    if p.at(SyntaxKind::LParen) {
        super::expr::arg_list(p);
    }
    if p.at(SyntaxKind::LBrace) {
        node_body(p);
    } else {
        p.eat(SyntaxKind::Semi);
    }
    m.complete(p, SyntaxKind::AdvancedItem);
}
