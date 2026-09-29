//! The three source forms share one pipeline: an equivalent `component!` body and
//! `.vs` file lower to the same Typed HIR, and the view each declares lowers to the
//! same UI IR and Binding IR as the equivalent `ui!` fragment.
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

/// The `ui!` fragment: the file's view block contents.
fn fragment() -> String {
    let open = FILE.find("view {").unwrap() + "view {".len();
    let close = FILE[..FILE.rfind('}').unwrap()].rfind('}').unwrap();
    blank_outside(FILE, open..close)
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
fn all_three_forms_lower_to_the_same_ui_and_binding_ir() {
    let fragment = clean(compile_fragment(&fragment()));
    let inline = clean(compile_component(&component_body(), &origin()));
    let file = clean(compile_file(FILE, &origin()));

    assert!(!file.tree.items.is_empty());
    assert_eq!(fragment.tree, file.tree);
    assert_eq!(inline.tree, file.tree);

    assert_eq!(file.bindings.static_edges().count(), 2);
    assert_eq!(edges(&fragment), edges(&file));
    assert_eq!(edges(&inline), edges(&file));
    assert_eq!(
        fragment.bindings.dynamic_fallback_nodes,
        file.bindings.dynamic_fallback_nodes
    );
    assert_eq!(fragment.keys, file.keys);
}
