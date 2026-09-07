//! The section 36 hit-testing baseline for hover synthesis: what a `Move`
//! sample costs on the steady path versus when it crosses a node boundary.
//!
//! The router's `route_pointer` hit-tests every non-captured `Move`, reads the
//! stored hover target, and only when the target *changed* dispatches a
//! per-node `Leave`/`Enter` pair (`sync_hover`). The steady case — the pointer
//! moving within the node it already hovers — is a hit test plus one
//! `new == old` comparison and no dispatch; the cross-node case adds the
//! leave+enter dispatch through two handlers. Section 7.3 forbids asserting the
//! steady case is cheap without measuring it, so this bench times both against
//! the same tree.
//!
//! Shape: a row of two sibling leaves under a flex root. `move_within` re-sends
//! two samples that both land on the same leaf (hover never changes — no
//! dispatch); `move_cross` alternates samples on the two leaves (every sample
//! crosses, forcing a leave+enter each time). A startup guard pins that the
//! within case dispatches nothing and the cross case flips the hover slot, so a
//! regression that breaks the diff fails loud rather than timing a no-op.
//!
//! Run release (`cargo bench -p viso-ui`); criterion defaults to a release
//! profile. Debug timing is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    Axis, BindingTable, BoxStyle, BuildCx, FlexStyle, LeafStyle, Modifiers, NodeId, NodeStore,
    PointerButtons, PointerEvent, PointerPhase, PointerRouter, Rect, Size, StateStore,
};

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 200.0,
    h: 100.0,
};

/// Left leaf spans x 0..100; right leaf x 100..200 (no gap, so every sample
/// lands on a leaf).
const LEFT_X: f32 = 25.0;
const LEFT_X2: f32 = 75.0;
const RIGHT_X: f32 = 150.0;
const MID_Y: f32 = 50.0;

/// Everything a hover route touches: the stores, the bindings the router flushes
/// nothing through here (the leaves have empty handlers), and the root to route
/// from. The two leaf ids let the guard check which one is hovered.
struct Harness {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    left: NodeId,
    right: NodeId,
    chain: Vec<NodeId>,
}

fn mv(x: f32) -> PointerEvent {
    PointerEvent {
        x,
        y: MID_Y,
        phase: PointerPhase::Move,
        buttons: PointerButtons::NONE,
        modifiers: Modifiers::default(),
    }
}

/// Two side-by-side leaves under a flex root, each with an empty pointer handler
/// so the enter/leave dispatch takes the real take-handler → restore path with
/// no user work of its own.
fn setup() -> Harness {
    let mut store = NodeStore::new();
    let states = StateStore::new();
    let bindings = BindingTable::new();

    let (root, left, right) = {
        let mut cx = BuildCx::new(&mut store);
        let mut left = None;
        let mut right = None;
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                size: Size::fixed(200.0, 100.0),
                style: BoxStyle::default(),
                ..Default::default()
            },
            |cx| {
                for label in 0u32..2 {
                    let leaf = cx.leaf(LeafStyle {
                        size: Size::fixed(100.0, 100.0),
                        style: BoxStyle::default(),
                    });
                    cx.on_pointer(leaf, move |_ev| {
                        let _ = label;
                    });
                    if label == 0 {
                        left = Some(leaf);
                    } else {
                        right = Some(leaf);
                    }
                }
            },
        );
        (cx.root().unwrap(), left.unwrap().id(), right.unwrap().id())
    };

    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);

    Harness {
        store,
        states,
        bindings,
        root,
        left,
        right,
        chain: Vec::new(),
    }
}

fn route(h: &mut Harness, ev: PointerEvent) {
    PointerRouter::route(
        &mut h.store,
        &mut h.states,
        &h.bindings,
        h.root,
        ev,
        &mut h.chain,
    );
}

/// Two samples that both land on the left leaf: the first commits hover to the
/// left leaf, the second is `new == old` — no dispatch, hover untouched.
fn move_within(h: &mut Harness) {
    route(h, mv(LEFT_X));
    route(h, mv(LEFT_X2));
}

/// Two samples on opposite leaves: each crosses a node boundary, forcing a
/// leave+enter dispatch and a hover-slot flip.
fn move_cross(h: &mut Harness) {
    route(h, mv(LEFT_X));
    route(h, mv(RIGHT_X));
}

/// The startup guard: `move_within` leaves hover on the left leaf (the second
/// sample changed nothing), and `move_cross` ends with hover on the right leaf
/// (the second sample crossed). A regression that breaks the `new == old` short
/// circuit or the diff fails the bench binary rather than timing a no-op.
/// Mirrors `style_resolve`'s startup guard.
fn assert_hover_diff_behaves() {
    let mut h = setup();
    move_within(&mut h);
    assert_eq!(
        h.store.hovered(),
        Some(h.left),
        "a move within the left leaf leaves hover on it"
    );

    let mut h = setup();
    move_cross(&mut h);
    assert_eq!(
        h.store.hovered(),
        Some(h.right),
        "a cross-node move ends with hover on the right leaf"
    );
}

fn bench_hover_diff(c: &mut Criterion) {
    assert_hover_diff_behaves();

    // Steady state: the pointer moves within the node it already hovers. Each
    // sample is a hit test plus a `new == old` comparison, no dispatch.
    c.bench_function("hover_move_within_node", |b| {
        let mut h = setup();
        // Prime the hover slot to the left leaf so both benched samples are
        // within-node.
        route(&mut h, mv(LEFT_X));
        b.iter(|| {
            route(black_box(&mut h), black_box(mv(LEFT_X2)));
        });
    });

    // The cross-node case: every sample flips the hover target, paying the
    // leave+enter dispatch through two handlers.
    c.bench_function("hover_move_cross_node", |b| {
        let mut h = setup();
        let mut left = true;
        b.iter(|| {
            let x = if left { LEFT_X } else { RIGHT_X };
            left = !left;
            route(black_box(&mut h), black_box(mv(x)));
        });
    });
}

criterion_group!(benches, bench_hover_diff);
criterion_main!(benches);
