//! Keyboard / focus / IME routing through the public facade: focus advances in
//! tree order and wraps, a key event reaches the *focused* node's handler and
//! bubbles through its ancestry, an IME preedit and commit both land on the
//! focused node, and a focus request a handler makes is applied by the router —
//! the full Slice E chain (focus target → dispatch → handler write / focus
//! move → invalidation), end to end.
//!
//! Like `pointer_routing`, this drives the ui stores directly (all reachable
//! through `viso::ui`) rather than standing up the live scheduler and a real
//! window: the facade's `on_input` calls the exact same `KeyRouter`/`focus_next`
//! this test drives, so exercising them over a facade-built tree proves the same
//! path without a platform surface. Key handlers are installed with
//! `set_key_handler` after building (there is no build-time `on_key` yet — the
//! focus/key surface is framework-level this slice, not authored per node).

use std::cell::RefCell;
use std::rc::Rc;

use viso::render::{Rect, Rgba};
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, DirtyClass, FlexStyle, ImeEvent, Key, KeyEvent,
    KeyRouter, LeafStyle, Modifiers, NodeId, NodeStore, Size, StateId, StateStore, StateValue,
    focus_next,
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

fn key(k: Key) -> KeyEvent {
    KeyEvent {
        key: k,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::default(),
    }
}

/// Build `flex { leaf, leaf }` with both leaves focusable, returning the store,
/// the root, and the two leaf ids in child order.
fn two_focusable_leaves() -> (NodeStore, NodeId, [NodeId; 2]) {
    let mut store = NodeStore::new();
    let leaves: Rc<RefCell<Vec<NodeId>>> = Rc::new(RefCell::new(Vec::new()));
    let root = {
        let sink = leaves.clone();
        let mut cx = BuildCx::new(&mut store);
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                size: Size::fixed(100.0, 100.0),
                style: BoxStyle::solid(SOLID),
                ..Default::default()
            },
            |cx| {
                for _ in 0..2 {
                    let l = cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        style: BoxStyle::solid(SOLID),
                    });
                    sink.borrow_mut().push(l.id());
                }
            },
        );
        cx.root().unwrap()
    };
    let ids = leaves.borrow();
    let out = [ids[0], ids[1]];
    for &id in &out {
        store.set_focusable(id, true);
    }
    drop(ids);
    (store, root, out)
}

#[test]
fn tab_moves_focus_in_order_and_wraps() {
    let (mut store, root, [a, b]) = two_focusable_leaves();

    // No focus yet: first Tab lands on the first focusable node.
    assert_eq!(focus_next(&mut store, root, true), Some(a));
    assert_eq!(store.focused(), Some(a));
    assert!(store.dirty(a).contains(DirtyClass::PAINT));

    store.clear_dirty();
    // Forward again advances a -> b, repainting exactly the two involved nodes.
    assert_eq!(focus_next(&mut store, root, true), Some(b));
    assert_eq!(store.focused(), Some(b));
    assert!(
        store.dirty(a).contains(DirtyClass::PAINT),
        "old focus repaints"
    );
    assert!(
        store.dirty(b).contains(DirtyClass::PAINT),
        "new focus repaints"
    );
    assert!(
        !store.dirty(root).intersects(DirtyClass::PAINT),
        "the container is not on the focus ring — PAINT does not bubble"
    );
    assert!(
        store.dirty(root).contains(DirtyClass::SEMANTICS),
        "a focus move is a semantic change, and SEMANTICS bubbles to the container"
    );

    // Forward from the last focusable wraps back to the first.
    assert_eq!(focus_next(&mut store, root, true), Some(a));
    assert_eq!(store.focused(), Some(a));
}

#[test]
fn key_reaches_focused_node_and_bubbles() {
    let (mut store, root, [a, _b]) = two_focusable_leaves();
    let mut states = StateStore::new();
    let bindings = BindingTable::new();

    // Log the visit order across the container (root) and the focused leaf `a`.
    let log: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let sink = log.clone();
        store.set_key_handler(root, Box::new(move |_cx| sink.borrow_mut().push(0)));
    }
    {
        let sink = log.clone();
        store.set_key_handler(a, Box::new(move |_cx| sink.borrow_mut().push(1)));
    }
    store.set_focused(Some(a));

    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);

    let mut chain = Vec::new();
    let ran = KeyRouter::route_key(
        &mut store,
        &mut states,
        &bindings,
        root,
        key(Key::Enter),
        &mut chain,
    );

    assert!(ran, "a key event reaches the focused node's handler");
    // capture (root) -> target (leaf) -> bubble (root): the same ancestry walk
    // pointer routing uses, with the target chosen by focus rather than hit-test.
    assert_eq!(*log.borrow(), vec![0, 1, 0]);
}

#[test]
fn key_with_no_focus_dispatches_nothing() {
    let (mut store, root, [a, _b]) = two_focusable_leaves();
    let mut states = StateStore::new();
    let bindings = BindingTable::new();

    let log: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let sink = log.clone();
        store.set_key_handler(a, Box::new(move |_cx| sink.borrow_mut().push(1)));
    }
    // Deliberately leave `focused` as None.

    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);

    let mut chain = Vec::new();
    let ran = KeyRouter::route_key(
        &mut store,
        &mut states,
        &bindings,
        root,
        key(Key::Enter),
        &mut chain,
    );

    assert!(!ran, "no focused node ⇒ no dispatch");
    assert!(log.borrow().is_empty(), "no handler ran");
}

#[test]
fn ime_preedit_then_commit_route_to_focused() {
    let (mut store, root, [a, _b]) = two_focusable_leaves();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();

    // The focused leaf bumps a bound cell on each IME event it sees.
    let count: StateId = states.alloc(StateValue::Int(0));
    {
        store.set_key_handler(
            a,
            Box::new(move |cx| {
                let now = match cx.get(count) {
                    Some(StateValue::Int(n)) => n,
                    _ => 0,
                };
                cx.set(count, StateValue::Int(now + 1));
            }),
        );
    }
    bindings.bind(count, a, DirtyClass::PAINT);
    store.set_focused(Some(a));

    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);

    let mut chain = Vec::new();
    let ran_preedit = KeyRouter::route_ime(
        &mut store,
        &mut states,
        &bindings,
        root,
        ImeEvent::Preedit {
            text: "n".to_string(),
            caret: 1,
        },
        &mut chain,
    );
    let ran_commit = KeyRouter::route_ime(
        &mut store,
        &mut states,
        &bindings,
        root,
        ImeEvent::Commit {
            text: "你".to_string(),
        },
        &mut chain,
    );

    assert!(ran_preedit && ran_commit, "both IME events reach the focus");
    // The handler ran twice (preedit + commit); the write is deferred until flush.
    assert_eq!(states.get(count), Some(StateValue::Int(2)));
    assert!(
        store.dirty(a).is_empty(),
        "no node is dirtied at write time"
    );

    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    let applied = store.flush_state_transactions(&changed, &bindings);

    assert_eq!(applied, 1, "only the single bound edge applies");
    assert!(
        store.dirty(a).contains(DirtyClass::PAINT),
        "the bound focused leaf is paint-dirty after the flush"
    );
}

#[test]
fn focus_request_from_a_handler_moves_focus() {
    let (mut store, root, [a, b]) = two_focusable_leaves();
    let mut states = StateStore::new();
    let bindings = BindingTable::new();

    // Focus `a`; its key handler requests focus move to `b`.
    store.set_focused(Some(a));
    {
        store.set_key_handler(a, Box::new(move |cx| cx.request_focus(b)));
    }

    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);

    let mut chain = Vec::new();
    let ran = KeyRouter::route_key(
        &mut store,
        &mut states,
        &bindings,
        root,
        key(Key::Tab),
        &mut chain,
    );

    assert!(ran, "the handler on the focused node ran");
    assert_eq!(
        store.focused(),
        Some(b),
        "the router applied the focus request"
    );
    assert!(
        store.dirty(a).contains(DirtyClass::PAINT),
        "old focus repaints"
    );
    assert!(
        store.dirty(b).contains(DirtyClass::PAINT),
        "new focus repaints"
    );
}
