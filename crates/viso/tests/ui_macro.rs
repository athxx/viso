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

/// A constant padding pads the container's content: plain `dp` as the built
/// style, an `sp` edge or one whose `start` and `end` differ as bound terms the
/// store folds at layout against the text scale and the layout direction.
#[test]
fn padding_pads_the_content_and_follows_the_environment() {
    use viso::render::Rect;
    use viso::ui::LengthEnv;
    use viso::ui::adaptive::LayoutDirection;

    let build = viso::ui! {
        Column {
            padding: 8dp;
            Column {
                width: 100dp;
                padding: EdgeInsets { start: 6dp, top: 1sp };
                Text { width: 10dp; height: 10dp; }
            }
            Grid {
                padding: EdgeInsets::all(3dp);
                Text { width: 10dp; height: 10dp; }
            }
        }
    };
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };
    let inner = store.arena().links(root).unwrap().first_child.unwrap();
    let text = store.arena().links(inner).unwrap().first_child.unwrap();
    let grid = store.arena().links(inner).unwrap().next_sibling.unwrap();
    let cell = store.arena().links(grid).unwrap().first_child.unwrap();
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    store.layout(root, surface, &mut Vec::new());
    assert_eq!((store.bounds(inner).x, store.bounds(inner).y), (8.0, 8.0));
    assert_eq!((store.bounds(text).x, store.bounds(text).y), (14.0, 9.0));
    let (g, c) = (store.bounds(grid), store.bounds(cell));
    assert_eq!(
        (c.x - g.x, c.y - g.y),
        (3.0, 3.0),
        "the grid pads its cells"
    );

    store.set_length_env(LengthEnv {
        text_scale: 2.0,
        direction: LayoutDirection::Rtl,
        ..LengthEnv::default()
    });
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(
        (store.bounds(text).x, store.bounds(text).y),
        (8.0, 10.0),
        "`start` is the right edge right to left; `top` follows the text scale"
    );
}

/// Size bounds, a margin and a `%` padding read the parent's content box: the
/// constant ones as the node's box rules, a margin in `sp` as bound terms that
/// follow the text scale.
#[test]
fn box_rules_read_the_parent_content_box_and_follow_the_environment() {
    use viso::render::Rect;
    use viso::ui::LengthEnv;

    let build = viso::ui! {
        Column {
            Column {
                width: 200dp;
                height: 100dp;
                padding: 10%;
                Text { width: 10dp; height: 10dp; }
            }
            Text { width: 500dp; max_width: 50%; height: 10dp; }
            Text { width: 10dp; min_width: 30dp; height: 10dp; margin: EdgeInsets { start: 6dp, top: 4dp }; }
            Text { width: 10dp; height: 10dp; margin: EdgeInsets { start: 2sp }; }
        }
    };
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };
    let mut children = Vec::new();
    let mut at = store.arena().links(root).unwrap().first_child;
    while let Some(child) = at {
        children.push(child);
        at = store.arena().links(child).unwrap().next_sibling;
    }
    let padded = store
        .arena()
        .links(children[0])
        .unwrap()
        .first_child
        .unwrap();
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    store.layout(root, surface, &mut Vec::new());
    let b = |store: &NodeStore, node| {
        let b = store.bounds(node);
        (b.x, b.y, b.w)
    };
    assert_eq!(
        b(&store, padded),
        (40.0, 30.0, 10.0),
        "10% of the 400×300 content box"
    );
    assert_eq!(
        b(&store, children[1]),
        (0.0, 100.0, 200.0),
        "at most half of 400dp"
    );
    assert_eq!(
        b(&store, children[2]),
        (6.0, 114.0, 30.0),
        "at least 30dp, past its margin"
    );
    assert_eq!(b(&store, children[3]).0, 2.0);

    store.set_length_env(LengthEnv {
        text_scale: 2.0,
        ..LengthEnv::default()
    });
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(b(&store, children[3]).0, 4.0, "2sp at text scale 2");
}

/// A grid's tracks, gaps, cell alignment and areas, and its children's
/// cells, lay out from the macro as from the package; a row gap in `sp`
/// follows the text scale.
#[test]
fn a_grid_places_its_children_by_tracks_areas_and_cells() {
    use viso::render::Rect;
    use viso::ui::LengthEnv;

    let build = viso::ui! {
        Grid {
            width: 300dp;
            height: 200dp;
            columns: [Track::fixed(100dp), Track::fr(1.0), Track::fr(2.0)];
            rows: tracks(2, Track::fixed(40dp));
            column_gap: 10dp;
            row_gap: 14sp;
            align_items: GridAlign::center;
            areas: ["head head head", "side main main", "side main main"];
            Text { width: 10dp; height: 10dp; grid.area: "main"; }
            Text { width: 10dp; height: 10dp; grid.column: 0; grid.row: 2; }
            Text { width: 10dp; height: 10dp; grid.column: 2; }
        }
    };
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };
    let mut children = Vec::new();
    let mut at = store.arena().links(root).unwrap().first_child;
    while let Some(child) = at {
        children.push(child);
        at = store.arena().links(child).unwrap().next_sibling;
    }
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 300.0,
        h: 200.0,
    };
    store.layout(root, surface, &mut Vec::new());
    let at = |store: &NodeStore, node| {
        let b = store.bounds(node);
        (b.x, b.y)
    };
    assert_eq!(at(&store, children[0]), (110.0, 81.0));
    assert_eq!(at(&store, children[1]), (0.0, 108.0));
    assert_eq!(at(&store, children[2]), (180.0, 15.0));

    store.set_length_env(LengthEnv {
        text_scale: 2.0,
        ..LengthEnv::default()
    });
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(
        at(&store, children[1]),
        (0.0, 136.0),
        "14sp at text scale 2"
    );
}
