//! Commit-2 grammar tests: the view surface — a component's `view` block and its
//! node bodies. These assert node identity (`node id: Type {}` / anonymous
//! `Type {}`), the declarative `:` property binding, event handlers, two-way
//! bindings, keyed `for`, and the diagnostics that enforce the language's breaks
//! from Makepad: `child` is reserved (E3001), a `for` without `key` is E3401, and
//! an arrow-bodied handler is E3201. Every input round-trips byte-for-byte.

use viso_dsl::syntax::grammar::{Entry, parse, parse_entry};
use viso_dsl::syntax::{ParseErrorKind, SyntaxKind, SyntaxNode, tokenize};

/// Parses `src` as a `.vs` compilation unit and returns the red root.
fn unit(src: &str) -> SyntaxNode {
    SyntaxNode::new_root(parse(&tokenize(src), src).root)
}

/// Parses `src` as a `ui!` view fragment and returns the red root.
fn fragment(src: &str) -> SyntaxNode {
    SyntaxNode::new_root(parse_entry(&tokenize(src), src, Entry::ViewFragment).root)
}

/// Whether `src`, parsed as a compilation unit, produces at least one error of
/// `kind`.
fn unit_has_error(src: &str, kind: ParseErrorKind) -> bool {
    parse(&tokenize(src), src)
        .errors
        .iter()
        .any(|e| e.kind == kind)
}

/// The first descendant node of `root` whose kind is `kind`.
fn first(root: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    root.descendants().into_iter().find(|n| n.kind() == kind)
}

/// The number of descendant nodes of `root` whose kind is `kind`.
fn count(root: &SyntaxNode, kind: SyntaxKind) -> usize {
    root.descendants()
        .into_iter()
        .filter(|n| n.kind() == kind)
        .count()
}

/// Wraps `body` (view-block members) in a minimal component so a fragment of view
/// syntax can be parsed through the compilation-unit entry.
fn view_of(body: &str) -> String {
    format!("component C {{ view {{ {body} }} }}")
}

#[test]
fn named_node_uses_colon_for_its_type() {
    // `node id: Type { }` — the `:` separates the node's name from its type; this is
    // the declarative half of the `:` vs `=` split, not an ascription.
    let root = unit(&view_of("node label: Text { }"));
    let node = first(&root, SyntaxKind::NamedNode).expect("a NamedNode");
    assert!(
        first(&node, SyntaxKind::TypePath).is_some(),
        "the node's component type follows the `:`"
    );
    assert!(first(&node, SyntaxKind::NodeBody).is_some());
}

#[test]
fn anonymous_node_is_a_bare_type_with_a_body() {
    // A node with no name is written as its `Type { }` directly.
    let root = unit(&view_of("Column { Text { } }"));
    assert!(
        count(&root, SyntaxKind::AnonymousNode) >= 2,
        "the outer Column and inner Text are both anonymous nodes"
    );
    assert!(
        first(&root, SyntaxKind::NamedNode).is_none(),
        "neither node is named"
    );
}

#[test]
fn property_binding_uses_colon_not_equals() {
    // `text: label;` binds a property with `:` and a terminating `;`.
    let root = unit(&view_of("Text { text: label; color: palette.fg; }"));
    assert_eq!(count(&root, SyntaxKind::PropertyBinding), 2);
    let binding = first(&root, SyntaxKind::PropertyBinding).expect("a PropertyBinding");
    assert!(
        first(&binding, SyntaxKind::PropertyPath).is_some(),
        "the binding's left side is a property path"
    );
}

#[test]
fn dotted_property_path_binds() {
    let root = unit(&view_of("Node { layout.padding: 4; }"));
    let path = first(&root, SyntaxKind::PropertyPath).expect("a PropertyPath");
    // A dotted path keeps every segment under one PropertyPath node.
    assert_eq!(path.text(), "layout.padding");
}

#[test]
fn event_handler_uses_a_block_not_an_arrow() {
    // `on click { }` — a handler body is a block.
    let ok = unit(&view_of("Button { on click { } }"));
    assert!(first(&ok, SyntaxKind::EventHandler).is_some());
    assert!(!unit_has_error(
        &view_of("Button { on click { } }"),
        ParseErrorKind::HandlerNotArrow
    ));

    // `on click => expr` — the old arrow form is rejected (E3201).
    assert!(unit_has_error(
        &view_of("Button { on click => doit(); }"),
        ParseErrorKind::HandlerNotArrow
    ));
}

#[test]
fn two_way_binding_parses() {
    let root = unit(&view_of("Field { bind value <=> state.text; }"));
    assert!(first(&root, SyntaxKind::TwoWayBinding).is_some());
    assert!(
        first(&root, SyntaxKind::AssignablePath).is_some(),
        "the right side is an assignable path"
    );
}

#[test]
fn for_requires_a_key_clause() {
    // A keyed `for` parses without a for-missing-key error.
    let keyed = view_of("for item in items key item.id { Row { } }");
    assert!(first(&unit(&keyed), SyntaxKind::ViewFor).is_some());
    assert!(!unit_has_error(&keyed, ParseErrorKind::ForMissingKey));

    // A `for` with no `key` clause is E3401.
    let unkeyed = view_of("for item in items { Row { } }");
    assert!(unit_has_error(&unkeyed, ParseErrorKind::ForMissingKey));
}

#[test]
fn view_if_and_match_parse() {
    let iff = unit(&view_of("if cond { Text { } } else { Row { } }"));
    assert!(first(&iff, SyntaxKind::ViewIf).is_some());

    let m = unit(&view_of(
        "match state { A => { Text { } }, _ => { Row { } } }",
    ));
    let vm = first(&m, SyntaxKind::ViewMatch).expect("a ViewMatch");
    assert_eq!(count(&vm, SyntaxKind::ViewMatchArm), 2, "two match arms");
}

#[test]
fn child_is_reserved() {
    // Makepad's implicit `child` slot is gone; a `child` node/member is E3001.
    assert!(unit_has_error(
        &view_of("child { }"),
        ParseErrorKind::ChildReserved
    ));
    assert!(unit_has_error(
        &view_of("Row { child { } }"),
        ParseErrorKind::ChildReserved
    ));
}

#[test]
fn view_fragment_entry_parses_bare_items() {
    // `ui!` parses view items with no surrounding component/view wrapper.
    let root = fragment("Button { text: \"Save\"; on click { } }");
    assert_eq!(root.kind(), SyntaxKind::ViewFragment);
    assert!(first(&root, SyntaxKind::AnonymousNode).is_some());
    assert!(first(&root, SyntaxKind::PropertyBinding).is_some());
    assert!(first(&root, SyntaxKind::EventHandler).is_some());
}

#[test]
fn view_losslessness_holds_over_a_corpus() {
    let corpus = [
        view_of("node label: Text { text: title; }"),
        view_of("Column { Text { } Row { } }"),
        view_of("Button { on capture click(e) { doit(e); } }"),
        view_of("Field { bind value <=> model.text using Codec; }"),
        view_of("for row in rows key row.id { for cell in row.cells key cell.id { Cell { } } }"),
        view_of("if a { Text { } } else if b { Row { } } else { Empty { } }"),
        view_of("@animated node fade: Box { opacity: 0.5; }"),
        view_of("fill header { Title { } }"),
    ];
    for src in corpus {
        let root = unit(&src);
        assert_eq!(root.text(), src, "round-trip {src:?}");
    }
}
