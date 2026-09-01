//! Reactive flush: a state write, drained once per frame, must dirty exactly the
//! nodes its bindings name — with the classes those bindings carry — and drive
//! the same incremental recompute a direct `mark_dirty` would.
//!
//! These drive the reactive stores directly (`StateStore` + `BindingTable` +
//! `NodeStore`, all public through `viso::ui`) rather than through the live
//! scheduler: the write→flush→dirty→recompute contract is what matters here, and
//! the scheduler's redraw-beat wiring is exercised by the runtime's own tests.
//! Correctness is asserted with the dirty sets and the `FrameRecompute` counters,
//! not pixels — pixel correctness is covered by `headless_scene`.

use viso::render::{Rect, Rgba};
use viso::ui::{
    Align, Axis, BindingTable, BoxStyle, BuildCx, DirtyClass, FlexStyle, Inset, LeafStyle, NodeId,
    NodeStore, Size, StateStore, StateValue,
};

const W: f32 = 200.0;
const H: f32 = 120.0;

const FILL: Rgba = Rgba {
    r: 0.5,
    g: 0.5,
    b: 0.5,
    a: 1.0,
};

fn surface() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W,
        h: H,
    }
}

/// A fill-sized Row container — not fixed-on-both-axes, so a rising MEASURE
/// passes through it rather than stopping.
fn flex_style() -> FlexStyle {
    FlexStyle {
        axis: Axis::Row,
        gap: 8.0,
        padding: Inset::all(12.0),
        align: Align::Center,
        size: Size::fill(),
        style: BoxStyle::solid(FILL),
    }
}

fn leaf(w: f32, h: f32) -> LeafStyle {
    LeafStyle {
        size: Size::fixed(w, h),
        style: BoxStyle::solid(FILL),
    }
}

/// The same two-branch tree the incremental-dirty test uses:
///
/// ```text
/// root (Row, fill)
/// ├── left (Row, fill)
/// │   ├── a (leaf)
/// │   └── b (leaf)
/// └── right (Row, fill)
///     └── c (leaf)
/// ```
struct Tree {
    root: NodeId,
    left: NodeId,
    a: NodeId,
    b: NodeId,
    right: NodeId,
    c: NodeId,
}

fn build_tree(store: &mut NodeStore) -> Tree {
    let mut left = None;
    let mut right = None;
    let mut a = None;
    let mut b = None;
    let mut c = None;

    let mut cx = BuildCx::new(store);
    let root = cx.flex(flex_style(), |cx| {
        left = Some(cx.flex(flex_style(), |cx| {
            a = Some(cx.leaf(leaf(40.0, 30.0)).id());
            b = Some(cx.leaf(leaf(40.0, 30.0)).id());
        }));
        right = Some(cx.flex(flex_style(), |cx| {
            c = Some(cx.leaf(leaf(40.0, 30.0)).id());
        }));
    });

    Tree {
        root: root.id(),
        left: left.unwrap().id(),
        a: a.unwrap(),
        b: b.unwrap(),
        right: right.unwrap().id(),
        c: c.unwrap(),
    }
}

/// Lay the whole tree out once and clear dirt, so a later flush measures only
/// that flush's incremental work.
fn settle(store: &mut NodeStore, root: NodeId) {
    let mut scratch = Vec::new();
    store.layout(root, surface(), &mut scratch);
    store.clear_dirty();
}

/// Run the flush phase exactly as the facade does: drain the store's pending
/// write-set into a reused buffer, then turn each changed id into targeted node
/// dirtying through the bindings. Returns how many edges were applied.
fn flush(states: &mut StateStore, bindings: &BindingTable, store: &mut NodeStore) -> u32 {
    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    store.flush_state_transactions(&changed, bindings)
}

#[test]
fn write_flush_dirties_only_bound_nodes_with_bound_classes() {
    let mut store = NodeStore::new();
    let t = build_tree(&mut store);
    settle(&mut store, t.root);

    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();

    // `color` drives leaf `a`'s paint only; `size` drives leaf `c`'s measure and
    // layout. Nothing binds the `left`/`right`/`b` nodes.
    let color = states.alloc(StateValue::Color(1.0, 0.0, 0.0, 1.0));
    let size = states.alloc(StateValue::Float(48.0));
    bindings.bind(color, t.a, DirtyClass::PAINT);
    bindings.bind(size, t.c, DirtyClass::MEASURE | DirtyClass::LAYOUT);

    // A transaction writes only `color`.
    assert!(states.set(color, StateValue::Color(0.0, 1.0, 0.0, 1.0)));
    assert!(states.has_pending());

    let applied = flush(&mut states, &bindings, &mut store);
    assert_eq!(applied, 1, "one edge applied: color -> a PAINT");
    assert!(!states.has_pending(), "flush drained the transaction");

    // `a` carries exactly PAINT; its bound sibling stays clean.
    assert!(store.dirty(t.a).contains(DirtyClass::PAINT));
    assert!(
        !store
            .dirty(t.a)
            .intersects(DirtyClass::MEASURE | DirtyClass::LAYOUT),
        "a color write must not imply measure/layout"
    );
    // The `size`-bound node was not written, so it stays clean.
    assert!(
        store.dirty(t.c).is_empty(),
        "unwritten state dirties nothing"
    );
    // A PAINT-only change is strictly local — no ancestor may carry layout dirt.
    for ancestor in [t.left, t.root] {
        assert!(
            !store
                .dirty(ancestor)
                .intersects(DirtyClass::PAINT | DirtyClass::LAYOUT | DirtyClass::MEASURE),
            "paint-only write must not bubble to ancestor {ancestor:?}"
        );
    }

    // The frame repaints but re-places nothing.
    let mut scratch = Vec::new();
    let mut redo = Vec::new();
    let (measured, laid_out) = store.relayout_dirty(t.root, surface(), &mut scratch, &mut redo);
    assert_eq!(measured, 0, "paint-only flush measures nothing");
    assert_eq!(laid_out, 0, "paint-only flush lays out nothing");
    let mut prims = Vec::new();
    assert!(
        store.repaint_dirty(t.root, &mut prims) > 0,
        "paint-only flush still rebuilds primitives"
    );
}

#[test]
fn many_writes_collapse_into_one_flush() {
    let mut store = NodeStore::new();
    let t = build_tree(&mut store);
    settle(&mut store, t.root);

    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();

    // One state bound to two nodes; another bound to a third.
    let s0 = states.alloc(StateValue::Int(0));
    let s1 = states.alloc(StateValue::Int(0));
    bindings.bind(s0, t.a, DirtyClass::PAINT);
    bindings.bind(s0, t.b, DirtyClass::PAINT);
    bindings.bind(s1, t.c, DirtyClass::PAINT);

    // A transaction writes `s0` twice and `s1` once — the pending set dedupes to
    // two ids, and a re-set of the same value schedules nothing.
    assert!(states.set(s0, StateValue::Int(1)));
    assert!(states.set(s0, StateValue::Int(2)));
    assert!(states.set(s1, StateValue::Int(5)));
    assert!(
        states.set(s1, StateValue::Int(5)),
        "no-op write still lands"
    );

    // One flush applies every edge for every changed id: s0->{a,b}, s1->{c}.
    let applied = flush(&mut states, &bindings, &mut store);
    assert_eq!(applied, 3, "two edges for s0 + one for s1, in one flush");

    for n in [t.a, t.b, t.c] {
        assert!(store.dirty(n).contains(DirtyClass::PAINT), "{n:?} painted");
    }
}

#[test]
fn idle_transaction_recomputes_nothing() {
    let mut store = NodeStore::new();
    let t = build_tree(&mut store);
    settle(&mut store, t.root);

    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let s = states.alloc(StateValue::Int(7));
    bindings.bind(s, t.a, DirtyClass::PAINT);

    // No write this frame: nothing pending, the flush applies nothing.
    assert!(!states.has_pending());
    let applied = flush(&mut states, &bindings, &mut store);
    assert_eq!(applied, 0, "an empty transaction applies no edges");

    // And the recompute is fully idle.
    let mut scratch = Vec::new();
    let mut redo = Vec::new();
    let (measured, laid_out) = store.relayout_dirty(t.root, surface(), &mut scratch, &mut redo);
    let mut prims = Vec::new();
    let painted = store.repaint_dirty(t.root, &mut prims);
    assert_eq!(
        (measured, laid_out, painted),
        (0, 0, 0),
        "idle frame is inert"
    );
}

#[test]
fn measure_binding_bubbles_through_flexible_ancestor() {
    let mut store = NodeStore::new();
    let t = build_tree(&mut store);
    settle(&mut store, t.root);

    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();

    // A width state bound to leaf `a` as MEASURE|LAYOUT: writing it must rise as
    // MEASURE through the flexible `left` up to the root, exactly as a direct
    // MEASURE mark on `a` would — the binding is just the delivery mechanism.
    let width = states.alloc(StateValue::Float(40.0));
    bindings.bind(width, t.a, DirtyClass::MEASURE | DirtyClass::LAYOUT);

    assert!(states.set(width, StateValue::Float(64.0)));
    flush(&mut states, &bindings, &mut store);

    assert!(store.dirty(t.a).contains(DirtyClass::MEASURE));
    assert!(
        store.dirty(t.left).contains(DirtyClass::MEASURE),
        "MEASURE rises through the flexible left container"
    );
    assert!(
        store.dirty(t.root).contains(DirtyClass::MEASURE),
        "MEASURE rises to the root"
    );
    assert!(
        store.dirty(t.right).is_empty(),
        "MEASURE does not cross into the sibling branch"
    );
}
