//! The `ui!` proc-macro end to end through the facade (DSL source forms; declarative
//! syntax is not rebuild semantics).
//!
//! `ui!` runs the *shared* Viso DSL frontend at Rust compile time and expands to a
//! static `BuildCx` builder closure — no runtime parse, no per-frame rebuild. These
//! tests exercise the expansion the only way that proves it: by taking the emitted
//! `|cx: &mut BuildCx| -> Handle` closure and mounting it into a real `NodeStore`,
//! then asserting on the retained tree and (for the reactive case) on the exact
//! `BindingTable` edge the macro compiled from a `text: count;` property.
//!
//! The macro lives in the compile-time-only `viso-ui-macros` crate and emits
//! `::viso_ui::…` paths; it resolves here because the facade re-exports both `ui!`
//! and the `viso_ui` builder types. A normal app reaches all of this through
//! `use viso::prelude::*;`.

use viso::ui::{
    BindingTable, BuildCx, DirtyClass, NodeStore, SemanticProjector, StateStore, StateValue,
    TextEdits, VirtualLists,
};

/// A static-only fragment expands to a builder closure that mounts a real retained
/// tree: a Column flex with one Text leaf child. No bindings, no reactive stores.
#[test]
fn static_fragment_mounts_a_retained_tree() {
    let build = viso::ui! {
        Column {
            Text { }
        }
    };

    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        let root = build(&mut cx);
        // The closure returned the Column's handle, and it is the build root.
        assert_eq!(cx.root(), Some(root.id()), "the Column is the mounted root");
        root.id()
    };

    // Column (flex) with exactly one child (the Text leaf), and no siblings.
    let links = store.arena().links(root).expect("root is live");
    let child = links.first_child.expect("Column mounted its Text child");
    assert!(
        store.arena().links(child).unwrap().next_sibling.is_none(),
        "Column mounted exactly one child"
    );
}

/// A `text: count;` property whose value reads an in-scope reactive `StateId`
/// compiles to a static `cx.bind(count, <text-leaf>, MEASURE|LAYOUT|PAINT|SEMANTICS)`
/// edge — the dirty invalidation class for text content — recorded against the leaf.
/// A later write to `count` then dirties exactly that node and class.
#[test]
fn reactive_fragment_compiles_a_static_binding_edge() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    // The reactive source the fragment names. `count` is an ordinary in-scope Rust
    // `StateId`; the macro emits `cx.bind(count, …)` and Rust hygiene resolves it to
    // this binding at the call site.
    let count = states.alloc(StateValue::Int(0));

    let build = viso::ui! {
        Column {
            Text { text: count; }
        }
    };

    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        build(&mut cx).id()
    };
    // Pre-order: Column = root, its Text child carries the binding.
    let text_leaf = store
        .arena()
        .links(root)
        .unwrap()
        .first_child
        .expect("the Column mounted its Text leaf");

    // The compiled edge is a static one under `count`, targeting the Text leaf with
    // the text-content dirty class — never a dynamic fallback.
    let text_class =
        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::SEMANTICS;
    store.clear_dirty();
    let applied = {
        let changed = [count];
        store.flush_state_transactions(&changed, &bindings)
    };
    assert_eq!(
        applied, 1,
        "the write reached exactly the one compiled edge"
    );
    assert_eq!(
        store.dirty(text_leaf),
        text_class,
        "the text binding dirties precisely its dirty-class set"
    );
}

/// `px` and `sp` lengths compile to bound term sums the store folds at layout
/// against the window's scale factor and text scale, and re-fold when the
/// environment moves. (Rust's lexer reads `1.5em` as a malformed exponent, so an
/// `em` length is authored in `.vs`.)
#[test]
fn environment_lengths_fold_at_layout_and_follow_the_environment() {
    use viso::render::Rect;
    use viso::ui::LengthEnv;

    let build = viso::ui! {
        Row {
            Column {
                font_size: 20dp;
                width: 100dp;
                Text { width: 50% - 20dp; height: 2sp + 4px; }
            }
        }
    };
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };
    let column = store.arena().links(root).unwrap().first_child.unwrap();
    let text = store.arena().links(column).unwrap().first_child.unwrap();
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(
        store.bounds(text).w,
        30.0,
        "50% of the column's 100dp, less 20dp"
    );
    assert_eq!(store.bounds(text).h, 6.0, "2sp + 4px at scale 1");
    assert_eq!(store.resolved_font_size(text), Some(20.0));

    store.set_length_env(LengthEnv {
        scale_factor: 2.0,
        text_scale: 1.5,
        ..LengthEnv::default()
    });
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(
        store.bounds(text).w,
        30.0,
        "a dp-and-percent width reads no environment"
    );
    assert_eq!(
        store.bounds(text).h,
        5.0,
        "2sp + 4px at text scale 1.5, scale 2"
    );
}
