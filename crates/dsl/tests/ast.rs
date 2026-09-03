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
    AstNode, CompilationUnit, ComponentEntry, Expr, Item, Member, PropertyBinding, ViewFragment,
    ViewItem,
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
