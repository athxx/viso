//! Normalized pointer input: the UI-tier event and the router that dispatches
//! it along the retained ancestry chain.
//!
//! The transport tier reports pointer samples in logical points; this tier
//! works in physical pixels — the same space as `bounds` and hit testing — so
//! the facade converts once at the boundary. These value types are UI-tier
//! mirrors (the UI crate does not depend on the platform crate); they carry the
//! same meaning without inverting the dependency direction.

/// The lifecycle position of a pointer sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Down,
    Move,
    Up,
    /// The pointer left the window bounds.
    Leave,
}

/// Pointer buttons as a bitmask; a mask (not a single button) so a sample can
/// report chords. `PRIMARY` is the left button on a conventional mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PointerButtons(pub u8);

impl PointerButtons {
    pub const NONE: PointerButtons = PointerButtons(0);
    pub const PRIMARY: PointerButtons = PointerButtons(1 << 0);
    pub const SECONDARY: PointerButtons = PointerButtons(1 << 1);
    pub const MIDDLE: PointerButtons = PointerButtons(1 << 2);

    /// Whether every button in `other` is currently pressed.
    #[inline]
    pub fn contains(self, other: PointerButtons) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no button is pressed.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Keyboard modifier state accompanying a pointer sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    /// The Command/Windows/Super key.
    pub logo: bool,
}

/// A normalized pointer event in physical pixels (window-top-left origin) — the
/// same coordinate space as node `bounds`, so hit testing needs no conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    pub phase: PointerPhase,
    pub buttons: PointerButtons,
    pub modifiers: Modifiers,
}

/// A minimal platform-independent key identity (UI-tier mirror of the runtime
/// key, kept crate-local so no transport vocabulary rides down into the tree).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Escape,
    Enter,
    Space,
    Tab,
    Backspace,
    /// Any key not in the minimal set, carrying its raw platform scancode.
    Other(u32),
}

/// A normalized key event (UI tier). Coordinate-free: routed to the focused
/// node, not by hit testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    /// True on press, false on release.
    pub pressed: bool,
    /// True if this press is an OS auto-repeat.
    pub repeat: bool,
    pub modifiers: Modifiers,
}

/// A normalized IME event: an in-progress composition (preedit) or a committed
/// segment. Like [`KeyEvent`] it routes to the focused node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImeEvent {
    /// The composing string, shown inline and replaced on each update. An empty
    /// preedit signals cancel/clear.
    Preedit {
        text: String,
        /// Caret position within `text`, in bytes.
        caret: usize,
    },
    /// A committed segment: the composition finished and this text is final.
    Commit { text: String },
}

use crate::binding::BindingTable;
use crate::component::NodeStore;
use crate::context::EventCx;
use crate::hit_test::HitTestTree;
use crate::node::NodeId;
use crate::state::StateStore;

/// Routes one pointer event along the retained ancestry chain of the node it
/// hits: capture (root → just above target), target (once), bubble (just above
/// target → root). Framework-owned, so the tree is walked once per event along
/// a single path — not a per-widget event poll of the whole tree.
///
/// A single-node chain (the target is the root) runs only the target phase, so
/// its handler fires exactly once. Every ancestor's handler fires twice (once
/// capturing, once bubbling); the target's fires once — the standard
/// capture/target/bubble contract. Consume/`stop_propagation` is a later slice;
/// for now every phase runs.
pub struct PointerRouter;

impl PointerRouter {
    /// Hit-test `ev`, then dispatch capture → target → bubble along the target's
    /// ancestry, driving each node's handler (if any) with a fresh [`EventCx`]
    /// over the reactive stores. Writes a handler makes are deferred into the
    /// state store's pending set, exactly like a code-driven update.
    ///
    /// `chain` is a caller-owned scratch buffer reused across events so the
    /// steady path allocates nothing. Returns whether any handler ran (`false`
    /// on a miss or when no node on the chain had a handler).
    pub fn route(
        store: &mut NodeStore,
        states: &mut StateStore,
        bindings: &BindingTable,
        root: NodeId,
        ev: PointerEvent,
        chain: &mut Vec<NodeId>,
    ) -> bool {
        route_pointer(store, states, bindings, root, ev, chain)
    }
}

/// Free-function form of [`PointerRouter::route`] (the struct is a stable home
/// for future capture/gesture state).
pub fn route_pointer(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    root: NodeId,
    ev: PointerEvent,
    chain: &mut Vec<NodeId>,
) -> bool {
    chain.clear();
    let Some(target) = HitTestTree::hit(store, root, ev.x, ev.y) else {
        return false;
    };

    // Build the chain root-first by walking parent links up from the target and
    // reversing in place: chain[0] = root … chain[n-1] = target.
    chain.push(target);
    let mut cur = target;
    while let Some(parent) = store.arena().links(cur).and_then(|l| l.parent) {
        chain.push(parent);
        cur = parent;
    }
    chain.reverse();

    let n = chain.len();
    let mut ran = false;

    // Collecting the ancestor ids up front lets us iterate them without holding
    // a borrow of `chain` across the `&mut store` dispatch. The chain is a small
    // reused scratch; copying the (Copy) ids costs nothing on the steady path.
    let target = chain[n - 1];

    // Capture: root down to, but not including, the target.
    for &node in &chain[..n - 1] {
        ran |= dispatch(store, states, bindings, node);
    }
    // Target: exactly once.
    ran |= dispatch(store, states, bindings, target);
    // Bubble: the node just below the target, up to root.
    for &node in chain[..n - 1].iter().rev() {
        ran |= dispatch(store, states, bindings, node);
    }
    ran
}

/// Call one node's handler if it has one, moving it out of the store for the
/// call so the store's state can be lent to the [`EventCx`] without aliasing,
/// then putting it back. Returns whether a handler ran.
fn dispatch(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    node: NodeId,
) -> bool {
    let Some(mut handler) = store.take_handler(node) else {
        return false;
    };
    {
        let mut ev = EventCx::__new(states, bindings);
        handler(&mut ev);
    }
    store.restore_handler(node, handler);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_mask_contains_and_empty() {
        assert!(PointerButtons::NONE.is_empty());
        assert!(!PointerButtons::PRIMARY.is_empty());
        let chord = PointerButtons(PointerButtons::PRIMARY.0 | PointerButtons::SECONDARY.0);
        assert!(chord.contains(PointerButtons::PRIMARY));
        assert!(chord.contains(PointerButtons::SECONDARY));
        assert!(!chord.contains(PointerButtons::MIDDLE));
    }

    #[test]
    fn event_is_copy_and_holds_physical_coords() {
        let ev = PointerEvent {
            x: 12.0,
            y: 34.0,
            phase: PointerPhase::Down,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        };
        let copied = ev; // Copy
        assert_eq!(copied.x, 12.0);
        assert_eq!(ev.phase, PointerPhase::Down);
    }

    // ---- routing ----

    use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
    use crate::layout::{Axis, Size};
    use crate::state::{StateId, StateStore, StateValue};
    use std::cell::RefCell;
    use std::rc::Rc;
    use viso_render::Rect;

    fn down(x: f32, y: f32) -> PointerEvent {
        PointerEvent {
            x,
            y,
            phase: PointerPhase::Down,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        }
    }

    /// Record the id and phase-tag each dispatched handler observed, so a test
    /// can assert the capture/target/bubble order. The tag comes from the state
    /// cell the handler bumps; here we just log the node label via a shared Vec.
    fn log_handler(
        log: Rc<RefCell<Vec<u32>>>,
        label: u32,
    ) -> impl FnMut(&mut EventCx<'_>) + 'static {
        move |_ev| log.borrow_mut().push(label)
    }

    #[test]
    fn miss_dispatches_nothing() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let hit = Rc::new(RefCell::new(false));
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let h = cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                ..Default::default()
            });
            let flag = hit.clone();
            cx.on_pointer(h, move |_| *flag.borrow_mut() = true);
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
            &mut scratch,
        );

        let mut chain = Vec::new();
        // Point well outside the 10x10 root.
        let ran = route_pointer(
            &mut store,
            &mut states,
            &bindings,
            root,
            down(50.0, 50.0),
            &mut chain,
        );
        assert!(!ran);
        assert!(!*hit.borrow());
    }

    #[test]
    fn single_node_runs_target_phase_once() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let h = cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                ..Default::default()
            });
            cx.on_pointer(h, log_handler(log.clone(), 0));
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
            &mut scratch,
        );

        let mut chain = Vec::new();
        let ran = route_pointer(
            &mut store,
            &mut states,
            &bindings,
            root,
            down(5.0, 5.0),
            &mut chain,
        );
        assert!(ran);
        // Target-only: the root's handler fires exactly once, not twice.
        assert_eq!(*log.borrow(), vec![0]);
    }

    #[test]
    fn capture_target_bubble_order() {
        // root (label 0) > child (label 1); target is the child.
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let flex = cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    size: Size::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    let c = cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        ..Default::default()
                    });
                    cx.on_pointer(c, log_handler(log.clone(), 1));
                },
            );
            cx.on_pointer(flex, log_handler(log.clone(), 0));
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            &mut scratch,
        );

        let mut chain = Vec::new();
        // Point inside the child (placed at origin, 40x40).
        let ran = route_pointer(
            &mut store,
            &mut states,
            &bindings,
            root,
            down(10.0, 10.0),
            &mut chain,
        );
        assert!(ran);
        // capture: root(0). target: child(1). bubble: root(0).
        assert_eq!(*log.borrow(), vec![0, 1, 0]);
    }

    #[test]
    fn handler_write_lands_in_pending() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let count: StateId = states.alloc(StateValue::Int(0));
        let bindings = BindingTable::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let h = cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                ..Default::default()
            });
            cx.on_pointer(h, move |ev| {
                let now = match ev.get(count) {
                    Some(StateValue::Int(n)) => n,
                    _ => 0,
                };
                ev.set(count, StateValue::Int(now + 1));
            });
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
            &mut scratch,
        );

        let mut chain = Vec::new();
        route_pointer(
            &mut store,
            &mut states,
            &bindings,
            root,
            down(5.0, 5.0),
            &mut chain,
        );
        assert_eq!(states.get(count), Some(StateValue::Int(1)));
        assert!(states.has_pending());
    }
}
