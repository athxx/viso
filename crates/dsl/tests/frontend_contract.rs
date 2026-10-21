//! The three source forms share one pipeline: an equivalent `component!` body and
//! `.vs` file lower to the same Typed HIR, UI IR and Binding IR, and a `ui!`
//! fragment lowers to the UI IR of a stateless component declaring it as its view.
//!
//! Spans are offsets into the source each form was given, so the equivalent sources
//! are laid out at the same offsets: the inline forms blank out, with spaces, the
//! text they do not contain. Whitespace is trivia, so this changes nothing but the
//! alignment, and the IR can then be compared exactly. Reactive-source symbols are
//! minted per package, so binding edges are compared by source name.

use viso_dsl::frontend::{Compiled, Origin, compile_component, compile_file, compile_fragment};
use viso_dsl::ir::{BindingKind, DirtyClass, NodeKey};

const FILE: &str = "\
component Counter {
    state count = 0;
    state enabled = true;
    view {
        Column {
            width: 120dp;
            Text { text: format(\"{}\", count); }
            Text { visible: enabled; }
        }
    }
}
";

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["counter".to_owned()],
        language: None,
        ..Default::default()
    }
}

/// `source` with every byte outside `range` replaced by a space, newlines kept.
fn blank_outside(source: &str, range: std::ops::Range<usize>) -> String {
    source
        .char_indices()
        .map(|(at, c)| {
            if range.contains(&at) || c == '\n' {
                c
            } else {
                ' '
            }
        })
        .collect()
}

/// The `component!` body: the file's component without its `component` keyword.
fn component_body() -> String {
    let start = FILE.find("Counter").unwrap();
    blank_outside(FILE, start..FILE.len())
}

/// A stateless view, as a `ui!` fragment.
const FRAGMENT: &str = "Column { width: 120dp; Text { text: \"0\"; } Text { visible: true; } }";

/// A file declaring `FRAGMENT` as a stateless component's view, the fragment at
/// offset `at`.
fn stateless_file(at: usize) -> String {
    let head = "component Counter { view {";
    format!("{head}{}{FRAGMENT} }} }}\n", " ".repeat(at - head.len()))
}

type Edge = (String, NodeKey, String, DirtyClass, BindingKind);

fn edges(compiled: &Compiled) -> Vec<Edge> {
    compiled
        .bindings
        .edges
        .iter()
        .map(|edge| {
            let source = compiled
                .source(edge.source)
                .expect("an edge reads a source");
            (
                source.name.clone(),
                edge.node,
                edge.property.clone(),
                edge.class,
                edge.kind,
            )
        })
        .collect()
}

fn clean(compiled: Compiled) -> Compiled {
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    compiled
}

#[test]
fn component_and_file_lower_to_the_same_typed_hir() {
    let inline = clean(compile_component(&component_body(), &origin()));
    let file = clean(compile_file(FILE, &origin()));
    let mut inline_hir = inline.component.expect("the body declares a component");
    let file_hir = file.component.expect("the file declares a component");
    // The one difference: the inline declaration has no `component` keyword to span.
    assert_eq!(
        inline_hir.source_origin.start().to_u32(),
        file_hir.source_origin.start().to_u32() + "component ".len() as u32
    );
    assert_eq!(inline_hir.source_origin.end(), file_hir.source_origin.end());
    inline_hir.source_origin = file_hir.source_origin;
    assert_eq!(inline_hir, file_hir);
    assert_eq!(inline.sources, file.sources);
}

#[test]
fn component_and_file_lower_to_the_same_ui_and_binding_ir() {
    let inline = clean(compile_component(&component_body(), &origin()));
    let file = clean(compile_file(FILE, &origin()));

    assert!(!file.tree.items.is_empty());
    assert_eq!(inline.tree, file.tree);
    assert_eq!(file.bindings.static_edges().count(), 2);
    assert_eq!(edges(&inline), edges(&file));
    assert_eq!(inline.keys, file.keys);
}

#[test]
fn a_fragment_lowers_to_the_ui_ir_of_a_stateless_component() {
    let fragment = clean(compile_fragment(FRAGMENT));
    let file = clean(compile_file(
        &stateless_file(fragment.base.to_u32() as usize),
        &origin(),
    ));

    assert!(!file.tree.items.is_empty());
    assert_eq!(fragment.tree, file.tree);
    assert_eq!(fragment.bindings.edges, []);
    assert_eq!(fragment.bindings.dynamic_fallback_nodes, 0);
    assert_eq!(fragment.keys, file.keys);
}
