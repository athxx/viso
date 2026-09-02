//! The counter, driven headlessly by an input tape: the same pointer → state →
//! binding → dirty → derive pipeline the live window runs, but deterministic and
//! display-free (the headless backend is a first-class test surface).
//!
//! Like the other facade tests this drives the ui stores directly (reachable
//! through `viso::ui`) instead of standing up a scheduler and a window. It builds
//! the counter scene through a reactive `BuildCx` — exactly what `01-counter`'s
//! `Application::build` does — keeping the node handles so it can assert *which*
//! nodes a click dirties. Then it replays a synthetic click and folds the pending
//! writes through the state flush, mirroring the facade's frame order:
//! `route` → `take_pending` → `flush_state_transactions`.

use viso::prelude::*;
use viso::render::{Rect, Rgba};
use viso::ui::{
    Axis, BindingTable, BoxStyle, Inset, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, Size, StateStore,
};

const W: f32 = 200.0;
const H: f32 = 120.0;

/// The three stores the facade owns as fields and threads together every frame.
struct Scene {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    button: NodeId,
    bar: NodeId,
    count: StateId,
}

/// Build the counter scene — the same tree `01-counter` authors — and lay it out
/// on a `W`×`H` surface, returning the stores plus the node/state ids under test.
fn scene() -> Scene {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();

    let mut button = None;
    let mut bar = None;
    let (root, count) = {
        let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings);
        let count = cx.state(StateValue::Int(0));
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 12.0,
                padding: Inset::all(16.0),
                size: Size::fill(),
                style: BoxStyle::solid(Rgba {
                    r: 0.12,
                    g: 0.13,
                    b: 0.16,
                    a: 1.0,
                }),
                ..Default::default()
            },
            |cx| {
                let b = cx.leaf(LeafStyle {
                    size: Size::fixed(96.0, 48.0),
                    style: BoxStyle::solid(Rgba {
                        r: 0.20,
                        g: 0.45,
                        b: 0.95,
                        a: 1.0,
                    }),
                });
                let b = cx.on_pointer(b, move |ev| {
                    // Act on release so a down/up pair is one click — the router
                    // runs the handler on every phase (this is what the tape
                    // below exercises: the down sample must not also increment).
                    if ev.pointer().map(|p| p.phase) != Some(PointerPhase::Up) {
                        return;
                    }
                    let now = match ev.get(count) {
                        Some(StateValue::Int(n)) => n,
                        _ => 0,
                    };
                    ev.set(count, StateValue::Int(now + 1));
                });
                let b = cx.semantics(b, Semantics::role(Role::Label).with_label("Count"));
                cx.bind(count, b, DirtyClass::SEMANTICS);
                button = Some(b.id());

                let bar_leaf = cx.leaf(LeafStyle {
                    size: Size::fixed(160.0, 24.0),
                    style: BoxStyle::solid(Rgba {
                        r: 0.15,
                        g: 0.75,
                        b: 0.45,
                        a: 1.0,
                    }),
                });
                cx.bind(count, bar_leaf, DirtyClass::PAINT);
                bar = Some(bar_leaf.id());
            },
        );
        (cx.root().expect("the counter scene has a root"), count)
    };

    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: W,
        h: H,
    };
    let mut scratch = Vec::new();
    store.layout(root, surface, &mut scratch);
    // A fresh build marks everything dirty; clear so each test observes only the
    // dirt its own click produces (the facade clears at every frame's end).
    store.clear_dirty();

    Scene {
        store,
        states,
        bindings,
        root,
        button: button.expect("the build declared the button"),
        bar: bar.expect("the build declared the bar"),
        count,
    }
}

/// A primary-button click at `(x, y)` — a down/up pair, the smallest tap.
fn click(s: &mut Scene, x: f32, y: f32) {
    let mut chain = Vec::new();
    for phase in [PointerPhase::Down, PointerPhase::Up] {
        let ev = PointerEvent {
            x,
            y,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Default::default(),
        };
        PointerRouter::route(
            &mut s.store,
            &mut s.states,
            &s.bindings,
            s.root,
            ev,
            &mut chain,
        );
    }
    // Drain this transaction's writes and fan them through the binding edges —
    // the facade's FlushStateTransactions phase, minus computed/effect reactors
    // (the counter has neither).
    let mut changed = Vec::new();
    s.states.take_pending(&mut changed);
    s.store.flush_state_transactions(&changed, &s.bindings);
}

/// The button's center in surface coordinates: the 96×48 button sits at the
/// padding origin (16, 16), cross-centered in the 88px content band —
/// y = 16 + (88 - 48) / 2 = 36 — so its center is (16 + 48, 36 + 24) = (64, 60).
fn button_center() -> (f32, f32) {
    (64.0, 60.0)
}

#[test]
fn click_increments_and_dirties_exactly_the_bound_nodes() {
    let mut s = scene();
    assert_eq!(s.states.get(s.count), Some(StateValue::Int(0)));

    let (x, y) = button_center();
    click(&mut s, x, y);

    // The click ran the handler, which incremented the cell.
    assert_eq!(s.states.get(s.count), Some(StateValue::Int(1)));

    // The flush marked exactly the two bound edges: the button re-derives its
    // semantics, the bar repaints. Neither crosses into the other's class.
    let button_dirty = s.store.dirty(s.button);
    assert!(
        button_dirty.intersects(DirtyClass::SEMANTICS),
        "the button's SEMANTICS edge fired"
    );
    assert!(
        !button_dirty.intersects(DirtyClass::PAINT),
        "the button carries no PAINT edge"
    );

    let bar_dirty = s.store.dirty(s.bar);
    assert!(
        bar_dirty.intersects(DirtyClass::PAINT),
        "the bar's PAINT edge fired"
    );
    assert!(
        !bar_dirty.intersects(DirtyClass::SEMANTICS),
        "the bar carries no SEMANTICS edge"
    );
}

#[test]
fn repeated_clicks_accumulate() {
    let mut s = scene();
    let (x, y) = button_center();
    for expected in 1..=5 {
        click(&mut s, x, y);
        assert_eq!(
            s.states.get(s.count),
            Some(StateValue::Int(expected)),
            "each click adds one"
        );
    }
}

#[test]
fn a_click_that_misses_the_button_changes_nothing() {
    let mut s = scene();
    // The far bottom-right corner is inside the container's padding but past both
    // leaves — a miss on the button, so the count holds.
    click(&mut s, W - 2.0, H - 2.0);
    assert_eq!(s.states.get(s.count), Some(StateValue::Int(0)));
}

#[test]
fn derive_semantics_reflects_the_button_and_reacts_to_the_click() {
    let mut s = scene();

    // The authored semantics surface in the derived tree: the button is a Label
    // named "Count"; the bar is a plain Group (no authored semantics, no handler).
    let tree = s.store.derive_semantics(s.root);
    let button = tree.get(s.button).expect("the button is in the tree");
    assert_eq!(button.role, Role::Label);
    assert_eq!(button.label.as_deref(), Some("Count"));
    let bar = tree.get(s.bar).expect("the bar is in the tree");
    assert_eq!(bar.role, Role::Group);
    assert_eq!(bar.label, None);

    // A clean frame re-derives nothing; the click's SEMANTICS dirt makes the next
    // derive fire — the incremental accessibility contract.
    assert!(
        s.store.derive_semantics_dirty(s.root).is_none(),
        "a clean frame derives nothing"
    );
    let (x, y) = button_center();
    click(&mut s, x, y);
    assert!(
        s.store.derive_semantics_dirty(s.root).is_some(),
        "the click dirtied SEMANTICS, so the tree re-derives"
    );
}
