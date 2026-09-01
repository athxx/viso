//! Pointer routing through the public facade: a synthetic click reaches the hit
//! node's handler, the write it makes lands in the state store's pending set,
//! and the frame flush turns that changed cell into targeted node dirtying via
//! the compiled binding — the full Slice D chain (hit-test → dispatch → handler
//! write → invalidation), end to end.
//!
//! Like `hit_test`, this drives the ui stores directly (all reachable through
//! `viso::ui`) rather than standing up the live scheduler and a real window: the
//! facade's `on_input` calls the exact same `PointerRouter` this test drives, so
//! exercising the router over a facade-built tree proves the same path without a
//! platform surface.

use viso::render::{Rect, Rgba};
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, DirtyClass, FlexStyle, LeafStyle, Modifiers, NodeStore,
    PointerButtons, PointerEvent, PointerPhase, PointerRouter, Size, StateId, StateStore,
    StateValue,
};

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 200.0,
    h: 200.0,
};

const SOLID: Rgba = Rgba {
    r: 0.4,
    g: 0.4,
    b: 0.4,
    a: 1.0,
};

fn click(x: f32, y: f32) -> PointerEvent {
    PointerEvent {
        x,
        y,
        phase: PointerPhase::Down,
        buttons: PointerButtons::PRIMARY,
        modifiers: Modifiers::default(),
    }
}

#[test]
fn click_bumps_bound_state_and_flush_dirties_only_the_bound_node() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();

    // A counter cell the leaf's handler bumps, bound to the leaf's paint.
    let count: StateId = states.alloc(StateValue::Int(0));

    let (root, leaf) = {
        let mut cx = BuildCx::new(&mut store);
        let mut leaf = None;
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                size: Size::fixed(100.0, 100.0),
                style: BoxStyle::solid(SOLID),
                ..Default::default()
            },
            |cx| {
                let l = cx.leaf(LeafStyle {
                    size: Size::fixed(40.0, 40.0),
                    style: BoxStyle::solid(SOLID),
                });
                cx.on_pointer(l, move |ev| {
                    let now = match ev.get(count) {
                        Some(StateValue::Int(n)) => n,
                        _ => 0,
                    };
                    ev.set(count, StateValue::Int(now + 1));
                });
                leaf = Some(l);
            },
        );
        (cx.root().unwrap(), leaf.unwrap())
    };
    // Changing `count` repaints the leaf — the compiled edge the flush reads.
    bindings.bind(count, leaf.id(), DirtyClass::PAINT);

    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);

    // Synthesize a click inside the leaf and route it through the facade router.
    let mut chain = Vec::new();
    let ran = PointerRouter::route(
        &mut store,
        &mut states,
        &bindings,
        root,
        click(10.0, 10.0),
        &mut chain,
    );

    // The handler ran and its write is deferred, not yet applied to any node.
    assert!(ran, "the click should reach the leaf's handler");
    assert_eq!(states.get(count), Some(StateValue::Int(1)));
    assert!(states.has_pending(), "the write is pending until the flush");
    assert!(
        store.dirty(leaf.id()).is_empty(),
        "no node is dirtied at write time"
    );

    // Drain the transaction and flush — the frame step the scheduler drives.
    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    let applied = store.flush_state_transactions(&changed, &bindings);

    // Exactly the one bound edge fired, dirtying only the leaf, only for paint.
    assert_eq!(applied, 1, "only the single bound edge should apply");
    assert!(
        store.dirty(leaf.id()).contains(DirtyClass::PAINT),
        "the bound leaf is paint-dirty after the flush"
    );
    assert!(
        store.dirty(root).is_empty(),
        "the unbound container stays clean — invalidation is targeted"
    );
}

#[test]
fn click_outside_the_tree_flips_nothing() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let count: StateId = states.alloc(StateValue::Int(0));

    let (root, leaf) = {
        let mut cx = BuildCx::new(&mut store);
        let mut leaf = None;
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                size: Size::fixed(100.0, 100.0),
                style: BoxStyle::solid(SOLID),
                ..Default::default()
            },
            |cx| {
                let l = cx.leaf(LeafStyle {
                    size: Size::fixed(40.0, 40.0),
                    style: BoxStyle::solid(SOLID),
                });
                cx.on_pointer(l, move |ev| {
                    ev.set(count, StateValue::Int(1));
                });
                leaf = Some(l);
            },
        );
        (cx.root().unwrap(), leaf.unwrap())
    };
    bindings.bind(count, leaf.id(), DirtyClass::PAINT);

    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);

    // A click well outside the 100x100 root misses: no handler runs, no write.
    let mut chain = Vec::new();
    let ran = PointerRouter::route(
        &mut store,
        &mut states,
        &bindings,
        root,
        click(180.0, 180.0),
        &mut chain,
    );
    assert!(!ran, "a miss dispatches no handler");
    assert_eq!(states.get(count), Some(StateValue::Int(0)));
    assert!(!states.has_pending());
}
