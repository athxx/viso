//! Style tokens through the public facade: a node's style binds to a theme
//! token, resolving folds the token's current value onto the node's style, a
//! theme swap (a state write on the token's backing cell) re-resolves *only*
//! the bound nodes through the ordinary flush, an untokenized node is never
//! touched, a token change dirties STYLE + PAINT but not LAYOUT, and a frame
//! with no token change runs no style cascade — Slice F end to end.
//!
//! Like `keyboard_routing`, this drives the ui stores directly (all reachable
//! through `viso::ui`) rather than standing up a live scheduler and a window:
//! a token is a `StateValue` cell in the ordinary `StateStore`, so a theme swap
//! rides the exact same pending / flush / `mark_dirty` path a counter uses. The
//! test builds that path by hand — `set()` the cell, drain pending, flush the
//! bindings, then run the incremental `resolve_styles` STYLE pass — which is the
//! same sequence a frame runs, with no style-specific invalidation machinery.

use std::cell::RefCell;
use std::rc::Rc;

use viso::render::Rgba;
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, DirtyClass, FlexStyle, LeafStyle, NodeId, NodeStore,
    Size, StateStore, StateValue, StyleId, Theme, TokenInterner, TokenNamespace,
};

const RED: Rgba = Rgba {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const DARK: Rgba = Rgba {
    r: 0.1,
    g: 0.1,
    b: 0.1,
    a: 1.0,
};
const LIGHT: Rgba = Rgba {
    r: 0.9,
    g: 0.9,
    b: 0.9,
    a: 1.0,
};

/// A theme with one `color.bg` token backed by a fresh state cell holding
/// `initial`. Returns the pieces a node binds against.
fn theme_with_bg(states: &mut StateStore, initial: Rgba) -> (Theme, StyleId, viso::ui::StateId) {
    let mut interner = TokenInterner::new();
    let mut theme = Theme::new();
    let bg = interner.intern(TokenNamespace::Color, "bg");
    let cell = states.alloc(StateValue::Color(
        initial.r, initial.g, initial.b, initial.a,
    ));
    theme.define(bg, cell);
    (theme, StyleId::fill(bg), cell)
}

/// Bind `style_id`'s tokens' backing cells to `node` in `bindings` (STYLE +
/// PAINT), attach the binding to the node, and run the initial resolve — the
/// once-per-node wiring the framework does when a node adopts a token style.
fn bind_style(
    store: &mut NodeStore,
    bindings: &mut BindingTable,
    theme: &Theme,
    states: &StateStore,
    node: NodeId,
    style_id: StyleId,
) {
    for token in style_id.tokens() {
        if let Some(cell) = theme.cell(token) {
            bindings.bind(cell, node, DirtyClass::STYLE | DirtyClass::PAINT);
        }
    }
    store.set_style_token(node, style_id);
    store.resolve_styles(theme, states);
}

/// Drain the state store's pending writes and flush them through the bindings,
/// then run the STYLE-resolve pass — the frame sequence a theme swap triggers.
/// Returns how many nodes re-resolved their style.
fn flush_and_resolve(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    theme: &Theme,
) -> u32 {
    let mut pending = Vec::new();
    states.take_pending(&mut pending);
    store.flush_state_transactions(&pending, bindings);
    store.resolve_styles(theme, states)
}

#[test]
fn token_resolves_to_theme_value() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let (theme, bg_style, _cell) = theme_with_bg(&mut states, DARK);
    let mut bindings = BindingTable::new();

    let leaf = {
        let mut cx = BuildCx::new(&mut store);
        cx.leaf(LeafStyle::default()).id()
    };
    bind_style(&mut store, &mut bindings, &theme, &states, leaf, bg_style);

    assert_eq!(
        store.style(leaf).fill,
        DARK,
        "the node's fill resolved to the theme token's value"
    );
}

#[test]
fn theme_swap_reresolves_only_bound_nodes() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let (theme, bg_style, cell) = theme_with_bg(&mut states, DARK);
    let mut bindings = BindingTable::new();

    // Two leaves bind to the same token; a third keeps a literal style.
    let sink: Rc<RefCell<Vec<NodeId>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let capture = Rc::clone(&sink);
        let mut cx = BuildCx::new(&mut store);
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                size: Size::fixed(100.0, 100.0),
                ..Default::default()
            },
            |cx| {
                capture
                    .borrow_mut()
                    .push(cx.leaf(LeafStyle::default()).id());
                capture
                    .borrow_mut()
                    .push(cx.leaf(LeafStyle::default()).id());
                capture.borrow_mut().push(
                    cx.leaf(LeafStyle {
                        size: Size::fixed(1.0, 1.0),
                        style: BoxStyle::solid(RED),
                    })
                    .id(),
                );
            },
        );
    }
    let (a, b, literal) = {
        let ids = sink.borrow();
        (ids[0], ids[1], ids[2])
    };
    bind_style(&mut store, &mut bindings, &theme, &states, a, bg_style);
    bind_style(&mut store, &mut bindings, &theme, &states, b, bg_style);
    store.clear_dirty();

    // A theme swap: write the token's backing cell.
    states.set(cell, StateValue::Color(LIGHT.r, LIGHT.g, LIGHT.b, LIGHT.a));
    let resolved = flush_and_resolve(&mut store, &mut states, &bindings, &theme);

    assert_eq!(resolved, 2, "only the two bound nodes re-resolved");
    assert_eq!(
        store.style(a).fill,
        LIGHT,
        "bound node a took the new value"
    );
    assert_eq!(
        store.style(b).fill,
        LIGHT,
        "bound node b took the new value"
    );
    assert_eq!(
        store.style(literal).fill,
        RED,
        "the literal-styled node was untouched by the swap"
    );
}

#[test]
fn unrelated_untokenized_node_untouched() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let (theme, bg_style, cell) = theme_with_bg(&mut states, DARK);
    let mut bindings = BindingTable::new();

    let (bound, literal) = {
        let mut cx = BuildCx::new(&mut store);
        let a = cx.leaf(LeafStyle::default()).id();
        let b = cx
            .leaf(LeafStyle {
                size: Size::fixed(1.0, 1.0),
                style: BoxStyle::solid(RED),
            })
            .id();
        (a, b)
    };
    bind_style(&mut store, &mut bindings, &theme, &states, bound, bg_style);
    store.clear_dirty();

    states.set(cell, StateValue::Color(LIGHT.r, LIGHT.g, LIGHT.b, LIGHT.a));
    flush_and_resolve(&mut store, &mut states, &bindings, &theme);

    assert!(
        store.dirty(literal).is_empty(),
        "the swap never dirtied the untokenized node"
    );
    assert_eq!(store.style(literal).fill, RED, "its literal fill stands");
}

#[test]
fn token_change_dirties_style_paint_not_layout() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let (theme, bg_style, cell) = theme_with_bg(&mut states, DARK);
    let mut bindings = BindingTable::new();

    let leaf = {
        let mut cx = BuildCx::new(&mut store);
        cx.leaf(LeafStyle::default()).id()
    };
    bind_style(&mut store, &mut bindings, &theme, &states, leaf, bg_style);
    store.clear_dirty();

    states.set(cell, StateValue::Color(LIGHT.r, LIGHT.g, LIGHT.b, LIGHT.a));
    let mut pending = Vec::new();
    states.take_pending(&mut pending);
    store.flush_state_transactions(&pending, &bindings);

    let dirty = store.dirty(leaf);
    assert!(
        dirty.intersects(DirtyClass::STYLE),
        "a color-token swap dirties STYLE"
    );
    assert!(
        dirty.intersects(DirtyClass::PAINT),
        "a color-token swap dirties PAINT"
    );
    assert!(
        !dirty.intersects(DirtyClass::MEASURE | DirtyClass::LAYOUT),
        "a color-token swap never touches MEASURE/LAYOUT"
    );
}

#[test]
fn clean_frame_runs_no_style_cascade() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let (theme, bg_style, _cell) = theme_with_bg(&mut states, DARK);
    let mut bindings = BindingTable::new();

    let leaf = {
        let mut cx = BuildCx::new(&mut store);
        cx.leaf(LeafStyle::default()).id()
    };
    bind_style(&mut store, &mut bindings, &theme, &states, leaf, bg_style);
    store.clear_dirty();

    // No token change this frame: nothing pending, nothing to re-resolve.
    let resolved = flush_and_resolve(&mut store, &mut states, &bindings, &theme);
    assert_eq!(
        resolved, 0,
        "a frame with no token change resolves no style"
    );
    assert!(!store.any_dirty(), "and marks nothing dirty");
}
