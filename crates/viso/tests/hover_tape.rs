//! Hover synthesis through the public facade router, driven by a move tape: a
//! sequence of `Move` samples across a nested tree, asserting the per-node
//! `Enter`/`Leave` pairs are synthesized in the platform-conventional order
//! (leave the old node before entering the new one), the store's hover slot
//! tracks the innermost hit, and a move that stays within one node synthesizes
//! nothing. The unit tests in `viso_ui::input` pin each rule in isolation; this
//! drives the whole sequence end to end over a facade-built tree, the same
//! `PointerRouter` the live `on_input` calls.
//!
//! Like `pointer_routing`, it drives the ui stores directly (all reachable
//! through `viso::ui`) rather than standing up a window: exercising the router
//! over a facade-built tree proves the same path without a platform surface.

use std::cell::RefCell;
use std::rc::Rc;

use viso::render::{Rect, Rgba};
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, FlexStyle, LeafStyle, Modifiers, NodeId, NodeStore,
    PointerButtons, PointerEvent, PointerPhase, PointerRouter, Size, StateStore,
};

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 200.0,
    h: 100.0,
};

const SOLID: Rgba = Rgba {
    r: 0.4,
    g: 0.4,
    b: 0.4,
    a: 1.0,
};

/// One observed hover event: which labeled node saw it, and its phase.
type Log = Rc<RefCell<Vec<(u32, PointerPhase)>>>;

fn mv(x: f32, y: f32) -> PointerEvent {
    PointerEvent {
        x,
        y,
        phase: PointerPhase::Move,
        buttons: PointerButtons::NONE,
        modifiers: Modifiers::default(),
    }
}

fn window_leave(x: f32, y: f32) -> PointerEvent {
    PointerEvent {
        x,
        y,
        phase: PointerPhase::Leave,
        buttons: PointerButtons::NONE,
        modifiers: Modifiers::default(),
    }
}

/// A row of two side-by-side leaves (labels 0 and 1) under a flex root, each
/// logging every hover phase it receives. Left leaf spans x 0..80, right leaf
/// x 100..180 with a gap (80..100) between them that is over no leaf.
struct Scene {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    left: NodeId,
    right: NodeId,
    log: Log,
    chain: Vec<NodeId>,
}

impl Scene {
    fn new() -> Self {
        let mut store = NodeStore::new();
        let states = StateStore::new();
        let bindings = BindingTable::new();
        let log: Log = Rc::new(RefCell::new(Vec::new()));

        let (root, left, right) = {
            let mut cx = BuildCx::new(&mut store);
            let mut left = None;
            let mut right = None;
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    size: Size::fixed(200.0, 100.0),
                    style: BoxStyle::solid(SOLID),
                    gap: 20.0,
                    ..Default::default()
                },
                |cx| {
                    for label in 0u32..2 {
                        let leaf = cx.leaf(LeafStyle {
                            size: Size::fixed(80.0, 100.0),
                            style: BoxStyle::solid(SOLID),
                        });
                        let log = log.clone();
                        cx.on_pointer(leaf, move |ev| {
                            if let Some(p) = ev.pointer() {
                                log.borrow_mut().push((label, p.phase));
                            }
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

        Scene {
            store,
            states,
            bindings,
            root,
            left,
            right,
            log,
            chain: Vec::new(),
        }
    }

    fn route(&mut self, ev: PointerEvent) {
        PointerRouter::route(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.root,
            ev,
            &mut self.chain,
        );
    }

    /// Drain the log since the last read.
    fn drain(&self) -> Vec<(u32, PointerPhase)> {
        self.log.borrow_mut().drain(..).collect()
    }
}

#[test]
fn move_tape_synthesizes_enter_leave_in_order_and_tracks_hover() {
    let mut s = Scene::new();
    // The gap between leaves is x 80..100 (left leaf 0..80, right 100..180).

    // 1. Enter the left leaf: Enter(0) then the Move itself lands on leaf 0.
    s.route(mv(10.0, 50.0));
    assert_eq!(
        s.drain(),
        vec![(0, PointerPhase::Enter), (0, PointerPhase::Move)],
        "entering the left leaf synthesizes Enter(0) before the Move"
    );
    assert_eq!(
        s.store.hovered(),
        Some(s.left),
        "the hover slot tracks the left leaf"
    );

    // 2. Move within the left leaf: no hover change, only the Move dispatches.
    s.route(mv(30.0, 50.0));
    assert_eq!(
        s.drain(),
        vec![(0, PointerPhase::Move)],
        "a move within the same node synthesizes no Enter/Leave"
    );
    assert_eq!(s.store.hovered(), Some(s.left), "hover slot unchanged");

    // 3. Move into the gap between the leaves. The filled flex container still
    //    lies under the pointer, so the innermost hit becomes the container (not
    //    "no node"): the left leaf receives Leave and hover moves to the root.
    //    The container has no handler, so it logs nothing.
    s.route(mv(90.0, 50.0));
    assert_eq!(
        s.drain(),
        vec![(0, PointerPhase::Leave)],
        "moving off the left leaf into the gap leaves it"
    );
    assert_eq!(
        s.store.hovered(),
        Some(s.root),
        "hover moves to the enclosing container over the gap"
    );

    // 4. Move onto the right leaf: Enter(1) then its Move.
    s.route(mv(140.0, 50.0));
    assert_eq!(
        s.drain(),
        vec![(1, PointerPhase::Enter), (1, PointerPhase::Move)],
        "entering the right leaf synthesizes Enter(1) before the Move"
    );
    assert_eq!(s.store.hovered(), Some(s.right), "hover slot tracks right");

    // 5. Move straight from the right leaf back onto the left leaf (skipping a
    //    gap sample): Leave(1) then Enter(0), in that order, then the Move.
    s.route(mv(10.0, 50.0));
    assert_eq!(
        s.drain(),
        vec![
            (1, PointerPhase::Leave),
            (0, PointerPhase::Enter),
            (0, PointerPhase::Move)
        ],
        "a cross-node move leaves the old node before entering the new one"
    );
    assert_eq!(s.store.hovered(), Some(s.left), "hover slot tracks left");

    // 6. Window leave: the hovered leaf receives Leave and the slot clears, with
    //    no coordinate-driven Move.
    s.route(window_leave(10.0, 50.0));
    assert_eq!(
        s.drain(),
        vec![(0, PointerPhase::Leave)],
        "a window leave synthesizes a per-node Leave to whatever was hovered"
    );
    assert_eq!(
        s.store.hovered(),
        None,
        "window leave clears the hover slot"
    );
}
