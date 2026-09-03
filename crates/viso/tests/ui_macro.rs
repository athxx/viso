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
    BindingTable, BuildCx, DirtyClass, NodeStore, StateStore, StateValue, VirtualLists,
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
        let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
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
