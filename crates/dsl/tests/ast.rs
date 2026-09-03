//! Commit-3 tests: the typed AST layer projected over the green/red tree.
//!
//! These cast the red roots produced by the commit-1/2 grammar fixtures into typed
//! wrappers and read them back through typed accessors — asserting that a cast
//! succeeds only for the matching kind, that accessors return the expected child
//! views, and that all three Rust-side entry productions (`ui!` fragment,
//! `component!` entry, `.vs`/`view!` compilation unit) project to AST without a
//! separate owned tree. The green tree stays the single source of truth: every
//! wrapper's `syntax()` round-trips to the node it was cast from.

use viso_dsl::ast::{
    AstNode, BinaryExpr, CompilationUnit, ComponentEntry, ElseBranch, Expr, Item, LiteralExpr,
    Member, PropertyBinding, UnaryExpr, ViewFragment, ViewIf, ViewItem, ViewMatch,
};
use viso_dsl::syntax::grammar::{Entry, parse, parse_entry};
use viso_dsl::syntax::{SyntaxKind, SyntaxNode, tokenize};

/// Parses `src` as a `.vs` compilation unit and returns the red root.
fn unit(src: &str) -> SyntaxNode {
    SyntaxNode::new_root(parse(&tokenize(src), src).root)
}

/// Parses `src` as a `component!` entry and returns the red root.
fn component_entry(src: &str) -> SyntaxNode {
    SyntaxNode::new_root(parse_entry(&tokenize(src), src, Entry::ComponentEntry).root)
}

/// Parses `src` as a `ui!` view fragment and returns the red root.
fn fragment(src: &str) -> SyntaxNode {
    SyntaxNode::new_root(parse_entry(&tokenize(src), src, Entry::ViewFragment).root)
}

/// The text of a name token, or `""` when the accessor found none.
fn name_of(t: Option<viso_dsl::syntax::SyntaxToken>) -> String {
    t.map(|t| t.text().to_string()).unwrap_or_default()
}

#[test]
fn cast_matches_only_its_own_kind() {
    let root = unit("component C { }");
    // The root casts to CompilationUnit and to nothing else.
    assert!(CompilationUnit::cast(root.clone()).is_some());
    assert!(ViewFragment::cast(root.clone()).is_none());
    assert!(ComponentEntry::cast(root).is_none());
}

#[test]
fn compilation_unit_projects_imports_and_items() {
    let cu = CompilationUnit::cast(unit(
        "import a::b as c;\nimport x::{y, z};\nexport component C { }\nrecord R { f: I32; }\n",
    ))
    .expect("a CompilationUnit");

    let imports: Vec<_> = cu.imports().collect();
    assert_eq!(imports.len(), 2, "two import declarations");
    assert_eq!(name_of(imports[0].rename().and_then(|r| r.name())), "c");
    assert_eq!(imports[1].items().count(), 2, "two selective import items");

    let items: Vec<_> = cu.items().collect();
    // export + record — the two top-level declarations (imports are their own list).
    assert!(matches!(items[0], Item::Export(_)));
    assert!(matches!(items[1], Item::Record(_)));
}

#[test]
fn export_unwraps_to_the_inner_declaration() {
    let cu = CompilationUnit::cast(unit("export component C { }")).expect("a CompilationUnit");
    let export = cu
        .items()
        .find_map(|it| match it {
            Item::Export(e) => Some(e),
            _ => None,
        })
        .expect("an ExportDecl");
    let inner = export.declaration().expect("an inner declaration");
    assert!(
        matches!(inner, Item::Component(_)),
        "export wraps a component"
    );
}

#[test]
fn component_members_read_through_typed_views() {
    let cu = CompilationUnit::cast(unit(
        "component Counter { input label: Text = \"hi\"; state n = 0; computed doubled: I32 = n; \
         action bump() { } view { Text { } } }",
    ))
    .expect("a CompilationUnit");
    let comp = cu
        .items()
        .find_map(|it| match it {
            Item::Component(c) => Some(c),
            _ => None,
        })
        .expect("a ComponentDecl");
    assert_eq!(name_of(comp.name()), "Counter");

    let members: Vec<_> = comp.members().collect();
    assert!(matches!(members[0], Member::Input(_)));
    assert!(matches!(members[1], Member::State(_)));
    assert!(matches!(members[2], Member::Computed(_)));
    assert!(matches!(members[3], Member::Action(_)));
    assert!(matches!(members[4], Member::View(_)));

    // The input's `:` type and `=` default read as a type view and an expr view.
    let input = members
        .iter()
        .find_map(|m| match m {
            Member::Input(i) => Some(i),
            _ => None,
        })
        .expect("an InputDecl");
    assert_eq!(name_of(input.name()), "label");
    assert!(
        input.ty().is_some(),
        "the `:` annotation is a TypePath view"
    );
    assert!(input.default().is_some(), "the `=` default is an Expr view");

    // Inferred state has no type view but does have an initializer expr.
    let state = members
        .iter()
        .find_map(|m| match m {
            Member::State(s) => Some(s),
            _ => None,
        })
        .expect("a StateDecl");
    assert!(state.ty().is_none(), "an inferred state has no type view");
    assert!(state.initializer().is_some());

    // The component reaches its view directly.
    assert!(comp.view().is_some(), "the component exposes its view decl");
}

#[test]
fn view_items_and_property_binding_project() {
    let cu = CompilationUnit::cast(unit(
        "component C { view { node label: Text { text: title; color: palette.fg; on click { } } } }",
    ))
    .expect("a CompilationUnit");
    let comp = cu
        .items()
        .find_map(|it| match it {
            Item::Component(c) => Some(c),
            _ => None,
        })
        .unwrap();
    let view = comp.view().unwrap();
    let block = view.block().expect("a view block");
    let items: Vec<_> = block.items().collect();

    let named = items
        .iter()
        .find_map(|it| match it {
            ViewItem::Named(n) => Some(n),
            _ => None,
        })
        .expect("a NamedNode");
    assert_eq!(name_of(named.name()), "label");
    assert!(named.ty().is_some(), "the node's `: Text` type view");

    let body = named.body().expect("a node body");
    let members: Vec<_> = body.members().collect();
    // Two property bindings and one event handler inside the node body.
    let bindings: Vec<&PropertyBinding> = members
        .iter()
        .filter_map(|m| match m {
            ViewItem::Property(p) => Some(p),
            _ => None,
        })
        .collect();
    assert_eq!(bindings.len(), 2, "two property bindings");
    // `text: title;` — the path is `text`, the value an Expr.
    let text_binding = &bindings[0];
    let path = text_binding.path().expect("a property path");
    assert_eq!(path.segments().count(), 1, "single-segment path `text`");
    assert!(
        text_binding.value().is_some(),
        "the `:` value is an Expr view"
    );

    assert!(
        members.iter().any(|m| matches!(m, ViewItem::Handler(_))),
        "the `on click {{ }}` handler projects to a ViewItem::Handler"
    );
}

#[test]
fn expr_casts_every_expression_kind_and_syntax_round_trips() {
    // The bare-expression entry roots an ExprStmt; its sole expression child casts
    // to the Expr enum, and `syntax()` returns that very node.
    let root = SyntaxNode::new_root(
        viso_dsl::syntax::grammar::parse_expr(&tokenize("a.b(c) + 1"), "a.b(c) + 1").root,
    );
    let expr_node = root
        .children()
        .into_iter()
        .find(|n| Expr::can_cast(n.kind()))
        .expect("a top-level expression node");
    let expr = Expr::cast(expr_node.clone()).expect("casts to Expr");
    assert!(
        expr.syntax().ptr_eq(&expr_node),
        "the wrapper's syntax() is the node it was cast from"
    );
    // The outer expression is the `+` binary.
    assert_eq!(expr.syntax().kind(), SyntaxKind::BinaryExpr);
}

#[test]
fn all_three_entry_productions_cast() {
    // `.vs` / `view!` compilation unit.
    assert!(CompilationUnit::cast(unit("component C { }")).is_some());

    // `component!` entry, reaching its single component.
    let ce = ComponentEntry::cast(component_entry("component C { state n = 0; }"))
        .expect("a ComponentEntry");
    let comp = ce.component().expect("the entry's component");
    assert_eq!(name_of(comp.name()), "C");

    // `ui!` bare view fragment, reaching its view items. The fragment's direct
    // item is the anonymous `Button` node; its property binding and handler live in
    // the node's body.
    let vf = ViewFragment::cast(fragment("Button { text: \"Save\"; on click { } }"))
        .expect("a ViewFragment");
    let button = vf
        .items()
        .find_map(|it| match it {
            ViewItem::Anonymous(n) => Some(n),
            _ => None,
        })
        .expect("the anonymous Button node");
    let body: Vec<_> = button.body().expect("a node body").members().collect();
    assert!(
        body.iter().any(|it| matches!(it, ViewItem::Property(_))),
        "the Button's property binding projects"
    );
    assert!(
        body.iter().any(|it| matches!(it, ViewItem::Handler(_))),
        "the Button's handler projects"
    );
}

// --- Section 1: view control-flow + two-way + fill + handler accessors -------

/// Collects the top-level view items of a `ui!` fragment.
fn fragment_items(src: &str) -> Vec<ViewItem> {
    ViewFragment::cast(fragment(src))
        .expect("a ViewFragment")
        .items()
        .collect()
}

/// The first item of a fragment that is a [`ViewIf`].
fn only_if(src: &str) -> ViewIf {
    fragment_items(src)
        .into_iter()
        .find_map(|it| match it {
            ViewItem::If(n) => Some(n),
            _ => None,
        })
        .expect("a ViewIf")
}

#[test]
fn view_if_reads_condition_blocks_and_else_chain() {
    // A plain `if / else` — the else branch is a trailing block, distinguished from
    // the `then` block by position (both are `ViewBlock` children).
    let vi = only_if("if ready { Text {} } else { Spinner {} }");
    assert!(vi.condition().is_some(), "the head condition is an Expr");
    let then_block = vi.then_block().expect("a then block");
    assert_eq!(
        then_block.items().count(),
        1,
        "the then block holds one node"
    );
    match vi.else_branch().expect("an else branch") {
        ElseBranch::Block(b) => assert_eq!(b.items().count(), 1, "the else block holds one node"),
        ElseBranch::If(_) => panic!("a plain else is a block, not a chained if"),
    }

    // A chained `else if` nests a ViewIf rather than a block.
    let chained = only_if("if a { Text {} } else if b { Label {} }");
    match chained.else_branch().expect("an else branch") {
        ElseBranch::If(inner) => {
            assert!(
                inner.condition().is_some(),
                "the chained if has its own head"
            );
        }
        ElseBranch::Block(_) => panic!("`else if` chains a ViewIf"),
    }

    // No else at all.
    let bare = only_if("if a { Text {} }");
    assert!(bare.else_branch().is_none(), "no else branch present");

    // The `preserve` identity string is read as a literal token.
    let preserved = only_if("if a preserve \"row\" { Text {} }");
    assert_eq!(
        preserved
            .preserve()
            .map(|t| t.text().to_string())
            .unwrap_or_default(),
        "\"row\"",
        "the preserve string literal token"
    );
    assert!(
        preserved.then_block().is_some(),
        "the block still follows the preserve clause"
    );
}

#[test]
fn view_for_distinguishes_iterable_from_key_by_position() {
    let vf = fragment_items("for item in items key item.id { Row {} }")
        .into_iter()
        .find_map(|it| match it {
            ViewItem::For(n) => Some(n),
            _ => None,
        })
        .expect("a ViewFor");

    // The loop pattern binds `item`.
    let pat = vf.pattern().expect("a loop pattern");
    assert_eq!(
        name_of(pat.binding_name()),
        "item",
        "the loop introduces the binding `item`"
    );

    // Two head expressions share the `Expr` kind; only position tells them apart.
    let iterable = vf.iterable().expect("the iterable head");
    assert_eq!(
        iterable.syntax().text().to_string(),
        "items",
        "the first head expr is the iterable"
    );
    let key = vf.key().expect("the key head");
    assert_eq!(
        key.syntax().text().to_string(),
        "item.id",
        "the second head expr is the stable key"
    );
    assert_eq!(vf.body().expect("a loop body").items().count(), 1);
}

#[test]
fn view_match_reads_scrutinee_and_arms() {
    let vm: ViewMatch =
        fragment_items("match status { Active => { Text {} }, Idle if slow => { Spinner {} } }")
            .into_iter()
            .find_map(|it| match it {
                ViewItem::Match(n) => Some(n),
                _ => None,
            })
            .expect("a ViewMatch");

    assert_eq!(
        vm.scrutinee()
            .expect("a scrutinee")
            .syntax()
            .text()
            .to_string(),
        "status"
    );
    let arms: Vec<_> = vm.arms().collect();
    assert_eq!(arms.len(), 2, "two match arms");

    // First arm: pattern `Active`, no guard.
    assert!(arms[0].pattern().is_some(), "the first arm has a pattern");
    assert!(arms[0].guard().is_none(), "the first arm has no guard");
    assert!(arms[0].body().is_some(), "the first arm has a body block");

    // Second arm: pattern `Idle` with an `if slow` guard — guard is the arm's only
    // direct Expr child, distinct from the Pattern node.
    assert!(arms[1].pattern().is_some(), "the second arm has a pattern");
    assert_eq!(
        arms[1]
            .guard()
            .expect("a guard")
            .syntax()
            .text()
            .to_string(),
        "slow",
        "the guard expression is `slow`"
    );
}

#[test]
fn two_way_binding_reads_target_source_and_using() {
    let tw = fragment_items("Field { bind value <=> model.text using Text; }")
        .into_iter()
        .find_map(|it| match it {
            ViewItem::Anonymous(n) => n.body(),
            _ => None,
        })
        .expect("the Field body")
        .members()
        .find_map(|m| match m {
            ViewItem::TwoWayBinding(t) => Some(t),
            _ => None,
        })
        .expect("a TwoWayBinding");

    let target = tw.target().expect("a target property path");
    assert_eq!(target.segments().count(), 1, "the target path is `value`");
    let source = tw.source().expect("an assignable source path");
    // `model.text` — two name segments, the index-free field path.
    assert_eq!(source.segments().count(), 2, "source path `model.text`");
    assert!(tw.using_ty().is_some(), "the `using Text` coercion type");
}

#[test]
fn event_handler_reads_phase_and_payload() {
    let handler = fragment_items("Button { on capture click(evt) { } }")
        .into_iter()
        .find_map(|it| match it {
            ViewItem::Anonymous(n) => n.body(),
            _ => None,
        })
        .expect("the Button body")
        .members()
        .find_map(|m| match m {
            ViewItem::Handler(h) => Some(h),
            _ => None,
        })
        .expect("an EventHandler");

    assert_eq!(
        name_of(handler.phase()),
        "capture",
        "the capture phase marker"
    );
    assert_eq!(name_of(handler.event()), "click", "the event name");
    let payload = handler.payload().expect("a payload pattern");
    assert_eq!(
        name_of(payload.binding_name()),
        "evt",
        "the payload binds `evt`"
    );
    assert!(handler.body().is_some(), "the handler has a block body");
}

#[test]
fn fill_clause_reads_slot_name_and_body() {
    let fill = fragment_items("Card { fill header { Text {} } }")
        .into_iter()
        .find_map(|it| match it {
            ViewItem::Anonymous(n) => n.body(),
            _ => None,
        })
        .expect("the Card body")
        .members()
        .find_map(|m| match m {
            ViewItem::Fill(f) => Some(f),
            _ => None,
        })
        .expect("a FillClause");
    assert_eq!(name_of(fill.name()), "header", "the filled slot name");
    assert_eq!(
        fill.body().expect("a fill body").items().count(),
        1,
        "the projected content"
    );
}

// --- Section 1: expression-fold accessors ------------------------------------

/// Parses `src` as a bare expression and returns its sole top-level expression
/// node — the concrete kind (`LiteralExpr`/`BinaryExpr`/…) is then cast from it.
fn expr_of(src: &str) -> SyntaxNode {
    let root =
        SyntaxNode::new_root(viso_dsl::syntax::grammar::parse_expr(&tokenize(src), src).root);
    root.children()
        .into_iter()
        .find(|n| Expr::can_cast(n.kind()))
        .expect("a top-level expression")
}

#[test]
fn literal_expr_reads_its_token() {
    let lit = LiteralExpr::cast(expr_of("42")).expect("a literal expression");
    assert_eq!(
        lit.token().map(|t| t.text().to_string()),
        Some("42".to_string()),
        "the literal token text"
    );
}

#[test]
fn binary_expr_reads_operands_and_operator_by_position() {
    // `a + b` — lhs and rhs are both Expr children distinguished only by position.
    let bin = BinaryExpr::cast(expr_of("a + b")).expect("a binary expression");
    assert_eq!(
        bin.lhs().expect("lhs").syntax().text().to_string(),
        "a",
        "the first operand child"
    );
    assert_eq!(
        bin.rhs().expect("rhs").syntax().text().to_string(),
        "b",
        "the second operand child"
    );
    assert_eq!(
        bin.op().map(|t| t.text().to_string()),
        Some("+".to_string()),
        "the operator token between the operands"
    );
}

#[test]
fn unary_expr_reads_operator_and_operand() {
    let un = UnaryExpr::cast(expr_of("-x")).expect("a unary expression");
    assert_eq!(
        un.op().map(|t| t.text().to_string()),
        Some("-".to_string()),
        "the prefix operator token"
    );
    assert_eq!(
        un.operand()
            .expect("an operand")
            .syntax()
            .text()
            .to_string(),
        "x",
        "the operand expression"
    );
}

#[test]
fn typed_wrappers_round_trip_to_their_syntax_node() {
    // Losslessness spot-check: a wrapper's syntax() text equals the source slice,
    // so the green tree stays the single source of truth after projection.
    let vi = only_if("if ready { Text {} }");
    assert_eq!(vi.syntax().text().to_string(), "if ready { Text {} }");
}
