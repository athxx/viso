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
use crate::dirty::DirtyClass;
use crate::hit_test::HitTestTree;
use crate::layout::{Axis, Vec2};
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

/// One node's dispatch outcome: whether a handler ran, and whether it asked the
/// router to stop walking the rest of the chain. The two are independent — a
/// handler can run without stopping, and (in principle) stop without appearing
/// to "run" — so they are reported separately.
#[derive(Clone, Copy, Default)]
struct Dispatched {
    ran: bool,
    stop: bool,
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
    // A captured pointer routes straight to the capturing node's chain,
    // bypassing hit testing — a drag or slider grab keeps receiving samples even
    // when the pointer leaves the node's box. On pointer-up the capture is
    // released (below), so the next sample hit-tests normally again.
    let target = match store.capture() {
        Some(captured) => captured,
        None => {
            let Some(hit) = HitTestTree::hit(store, root, ev.x, ev.y) else {
                chain.clear();
                return false;
            };
            hit
        }
    };
    dispatch_chain(store, root, target, chain, |s, n| {
        pointer_dispatch(s, states, bindings, n, &ev)
    })
}

/// A normalized scroll (wheel/trackpad) sample in physical pixels: the point
/// under the pointer and a per-axis delta added to the target viewport's scroll
/// offset. UI-tier mirror of the runtime scroll sample, kept crate-local so no
/// transport vocabulary rides down into the tree. A positive `delta_y` increases
/// the viewport's vertical offset (reveals later content); a positive `delta_x`
/// increases the horizontal offset. The facade decides the wheel-to-offset sign
/// when it lowers the platform sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollEvent {
    pub x: f32,
    pub y: f32,
    pub delta_x: f32,
    pub delta_y: f32,
    pub modifiers: Modifiers,
}

/// Routes a scroll sample to the innermost scroll viewport under the pointer
/// that can absorb it. A stable home for future momentum/rubber-band state,
/// mirroring [`PointerRouter`].
pub struct ScrollRouter;

impl ScrollRouter {
    /// Hit-test `ev`'s point, then walk up the target's ancestry to the first
    /// scroll viewport whose axis carries a nonzero delta component *and* still
    /// has range left in the direction of travel, and apply that component via
    /// [`NodeStore::scroll_by`]. A viewport scrolls only along its own axis, so a
    /// vertical list absorbs the vertical delta and passes a horizontal one to an
    /// enclosing horizontal scroller (nested-scroll chaining). Returns whether a
    /// viewport consumed any of the delta.
    ///
    /// Allocation-free: hit test plus a parent-link walk, no scratch buffer. A
    /// miss, or a point over no scrollable ancestor, consumes nothing and returns
    /// `false`.
    pub fn route(store: &mut NodeStore, root: NodeId, ev: ScrollEvent) -> bool {
        route_scroll(store, root, ev)
    }
}

/// Free-function form of [`ScrollRouter::route`].
pub fn route_scroll(store: &mut NodeStore, root: NodeId, ev: ScrollEvent) -> bool {
    let Some(target) = HitTestTree::hit(store, root, ev.x, ev.y) else {
        return false;
    };

    // Remaining delta to place; each viewport consumes its own axis's component.
    let mut remaining = Vec2 {
        x: ev.delta_x,
        y: ev.delta_y,
    };
    let mut consumed = false;

    // Walk from the target up to the root, letting each scroll viewport absorb
    // the delta on its axis if it has range left in the direction of travel.
    let mut node = Some(target);
    while let Some(n) = node {
        if let Some(axis) = store.scroll_axis(n) {
            let component = remaining.on(axis);
            if component != 0.0 {
                let offset = store.scroll(n).on(axis);
                let range = store.scroll_range(n, axis);
                // Range left to travel in this delta's direction: toward `range`
                // for a positive delta, toward `0` for a negative one.
                let room = if component > 0.0 {
                    range - offset
                } else {
                    offset
                };
                if room > 0.0 {
                    let step = axis_vec(axis, component);
                    store.scroll_by(n, step);
                    consumed = true;
                    // This axis is now placed on this viewport; zero it so an
                    // enclosing same-axis viewport does not double-apply it.
                    match axis {
                        Axis::Row => remaining.x = 0.0,
                        Axis::Column => remaining.y = 0.0,
                    }
                    if remaining == Vec2::ZERO {
                        break;
                    }
                }
            }
        }
        node = store.arena().links(n).and_then(|l| l.parent);
    }

    consumed
}

/// A [`Vec2`] with `value` on `axis` and zero on the other — the per-axis delta
/// handed to [`NodeStore::scroll_by`].
#[inline]
fn axis_vec(axis: Axis, value: f32) -> Vec2 {
    match axis {
        Axis::Row => Vec2 { x: value, y: 0.0 },
        Axis::Column => Vec2 { x: 0.0, y: value },
    }
}

/// Build `target`'s root-first ancestry chain and dispatch capture → target →
/// bubble, calling `dispatch` for each node on the path. Shared by pointer, key,
/// and IME routing — only the target source (hit-test vs focus) and the handler
/// column that `dispatch` reads differ; the walk itself is one and the same.
///
/// A single-node chain (`target == root`) runs only the target phase. Every
/// ancestor's `dispatch` fires twice (capture, then bubble); the target's fires
/// once. `chain` is caller-owned scratch reused across events, so the steady
/// path allocates nothing. Returns whether any dispatch reported it ran.
fn dispatch_chain(
    store: &mut NodeStore,
    _root: NodeId,
    target: NodeId,
    chain: &mut Vec<NodeId>,
    mut dispatch: impl FnMut(&mut NodeStore, NodeId) -> Dispatched,
) -> bool {
    chain.clear();
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
    let target = chain[n - 1];

    // The ancestor ids are Copy; iterating the scratch slice while calling
    // `dispatch(&mut store, ..)` is sound because `dispatch` borrows the state
    // stores, never `chain`. `stop_propagation` in any handler ends the walk
    // after that handler returns — the current handler still completes, but no
    // further node on the chain is visited (in this phase or the next).
    //
    // Capture: root down to, but not including, target.
    for &node in &chain[..n - 1] {
        let d = dispatch(store, node);
        ran |= d.ran;
        if d.stop {
            return ran;
        }
    }
    // Target: exactly once.
    let d = dispatch(store, target);
    ran |= d.ran;
    if d.stop {
        return ran;
    }
    // Bubble: the node just below the target, up to root.
    for &node in chain[..n - 1].iter().rev() {
        let d = dispatch(store, node);
        ran |= d.ran;
        if d.stop {
            return ran;
        }
    }
    ran
}

/// Call one node's *pointer* handler if it has one, moving it out of the store
/// for the call so the store's state can be lent to the [`EventCx`] without
/// aliasing, then putting it back. Returns whether a handler ran.
fn pointer_dispatch(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    node: NodeId,
    event: &PointerEvent,
) -> Dispatched {
    let Some(mut handler) = store.take_handler(node) else {
        return Dispatched::default();
    };
    let (capture, stop) = {
        let mut ev = EventCx::__new_pointer(states, bindings, event);
        handler(&mut ev);
        (ev.__take_capture_request(), ev.__stop_requested())
    };
    store.restore_handler(node, handler);
    // A capture request is applied against this node: `Some(id)` captures to the
    // requested node, `None` releases. Applied after the handler returns because
    // the cx holds no node store to touch the capture slot directly.
    if let Some(request) = capture {
        store.set_capture(request);
    }
    Dispatched { ran: true, stop }
}

/// Advance focus to the next (`forward`) or previous focusable node in tree
/// pre-order, wrapping. A `None` current focus starts at the first focusable
/// node (or the last, going backward). Returns the new focused id, or `None` if
/// no node is focusable. Marks PAINT + SEMANTICS dirty on both the old and new
/// focused nodes (the focus-ring repaint and the accessibility re-fold) and
/// updates the focus slot.
///
/// The focusable list is collected into a small local `Vec`: focus traversal is
/// a cold, once-per-Tab action, not a per-frame hot path, so the allocation is
/// acceptable. If a huge focusable set ever makes this hot, it moves to a cached
/// ordered index — a style/semantics concern, not this path's.
pub fn focus_next(store: &mut NodeStore, root: NodeId, forward: bool) -> Option<NodeId> {
    // Pre-order walk (first_child, then next_sibling), collecting focusables in
    // the tree's natural order.
    let mut order: Vec<NodeId> = Vec::new();
    let mut stack: Vec<NodeId> = vec![root];
    while let Some(node) = stack.pop() {
        if store.focusable(node) {
            order.push(node);
        }
        // Push children so the first child is visited next: gather them, then
        // push in reverse so the leftmost is popped first.
        let mut kids: Vec<NodeId> = Vec::new();
        let mut child = store.arena().links(node).and_then(|l| l.first_child);
        while let Some(c) = child {
            kids.push(c);
            child = store.arena().links(c).and_then(|l| l.next_sibling);
        }
        for &c in kids.iter().rev() {
            stack.push(c);
        }
    }
    if order.is_empty() {
        return None;
    }

    let old = store.focused();
    let new = match old.and_then(|f| order.iter().position(|&n| n == f)) {
        Some(idx) => {
            let len = order.len();
            let step = if forward { 1 } else { len - 1 };
            order[(idx + step) % len]
        }
        // No live focus in the ring: enter at the first (or last) focusable.
        None => {
            if forward {
                order[0]
            } else {
                order[order.len() - 1]
            }
        }
    };

    apply_focus(store, old, Some(new));
    Some(new)
}

/// Move the focus slot from `old` to `new`, repainting both (the focus-ring
/// leaves the old node and lands on the new) and marking both semantically
/// changed (a focus move is a semantic event, so the derived accessibility tree
/// re-folds). Either may be `None`. Shared by the focus ring and by a handler's
/// deferred focus request. SEMANTICS bubbles to the root; PAINT stays local.
fn apply_focus(store: &mut NodeStore, old: Option<NodeId>, new: Option<NodeId>) {
    if old == new {
        return;
    }
    if let Some(o) = old {
        store.mark_dirty(o, DirtyClass::PAINT | DirtyClass::SEMANTICS);
    }
    if let Some(n) = new {
        store.mark_dirty(n, DirtyClass::PAINT | DirtyClass::SEMANTICS);
    }
    store.set_focused(new);
}

/// Routes key and IME events to the currently focused node's ancestry, driving
/// each node's *key* handler (a column distinct from the pointer handlers). A
/// stable home for future keyboard-navigation state, mirroring [`PointerRouter`].
pub struct KeyRouter;

impl KeyRouter {
    /// Route a key event to the focused node's ancestry (capture → target →
    /// bubble), driving each node's key handler. No focused node ⇒ no dispatch
    /// (returns `false`). Any focus request a handler makes is applied after it
    /// returns. Tab/Shift-Tab focus traversal is the caller's policy, not baked
    /// in here. `chain` is caller-owned scratch reused across events.
    pub fn route_key(
        store: &mut NodeStore,
        states: &mut StateStore,
        bindings: &BindingTable,
        root: NodeId,
        ev: KeyEvent,
        chain: &mut Vec<NodeId>,
    ) -> bool {
        let Some(target) = store.focused() else {
            chain.clear();
            return false;
        };
        dispatch_chain(store, root, target, chain, |s, n| {
            key_dispatch(s, states, bindings, n, &ev)
        })
    }

    /// Route an IME event to the focused node's ancestry, same shape as
    /// [`KeyRouter::route_key`] but driving the key handler with the IME event
    /// (the handler reads it via `cx.ime()`). No focused node ⇒ no dispatch.
    pub fn route_ime(
        store: &mut NodeStore,
        states: &mut StateStore,
        bindings: &BindingTable,
        root: NodeId,
        ev: ImeEvent,
        chain: &mut Vec<NodeId>,
    ) -> bool {
        let Some(target) = store.focused() else {
            chain.clear();
            return false;
        };
        dispatch_chain(store, root, target, chain, |s, n| {
            ime_dispatch(s, states, bindings, n, &ev)
        })
    }
}

/// Call one node's *key* handler with the key event, moving it out of the store
/// for the call so state can be lent to the [`EventCx`] without aliasing, then
/// putting it back. Applies any focus request the handler made. Returns whether
/// a handler ran.
fn key_dispatch(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    node: NodeId,
    ev: &KeyEvent,
) -> Dispatched {
    let Some(mut handler) = store.take_key_handler(node) else {
        return Dispatched::default();
    };
    let (request, stop) = {
        let mut cx = EventCx::__new_key(states, bindings, ev);
        handler(&mut cx);
        (cx.__take_focus_request(), cx.__stop_requested())
    };
    store.restore_key_handler(node, handler);
    if let Some(target) = request {
        apply_focus(store, store.focused(), target);
    }
    Dispatched { ran: true, stop }
}

/// IME twin of [`key_dispatch`]: drives the key handler with the IME event so a
/// handler reads the preedit/commit string through `cx.ime()`.
fn ime_dispatch(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    node: NodeId,
    ev: &ImeEvent,
) -> Dispatched {
    let Some(mut handler) = store.take_key_handler(node) else {
        return Dispatched::default();
    };
    let (request, stop) = {
        let mut cx = EventCx::__new_ime(states, bindings, ev);
        handler(&mut cx);
        (cx.__take_focus_request(), cx.__stop_requested())
    };
    store.restore_key_handler(node, handler);
    if let Some(target) = request {
        apply_focus(store, store.focused(), target);
    }
    Dispatched { ran: true, stop }
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

    // ---- focus / key / IME ----

    use crate::node::NodeId;

    /// Build flex { leaf a, leaf b, leaf c } and mark a,b,c focusable, returning
    /// (root, [a, b, c]). Non-focusable structural nodes (the flex) are skipped
    /// by `focus_next`, which these tests rely on.
    fn three_focusable_leaves() -> (NodeStore, NodeId, [NodeId; 3]) {
        let mut store = NodeStore::new();
        let kids = Rc::new(RefCell::new(Vec::new()));
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let kids2 = kids.clone();
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    size: Size::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    for _ in 0..3 {
                        let leaf = cx.leaf(LeafStyle {
                            size: Size::fixed(10.0, 10.0),
                            ..Default::default()
                        });
                        kids2.borrow_mut().push(leaf.id());
                    }
                },
            );
            cx.root().unwrap()
        };
        let k = kids.borrow();
        let ids = [k[0], k[1], k[2]];
        for &id in &ids {
            store.set_focusable(id, true);
        }
        (store, root, ids)
    }

    #[test]
    fn focus_next_steps_in_order_and_wraps() {
        let (mut store, root, [a, b, c]) = three_focusable_leaves();
        // No focus yet: forward enters at the first focusable.
        assert_eq!(focus_next(&mut store, root, true), Some(a));
        assert_eq!(focus_next(&mut store, root, true), Some(b));
        assert_eq!(focus_next(&mut store, root, true), Some(c));
        // Wrap past the last back to the first.
        assert_eq!(focus_next(&mut store, root, true), Some(a));
        // Backward wraps the other way.
        assert_eq!(focus_next(&mut store, root, false), Some(c));
    }

    #[test]
    fn focus_next_over_no_focusable_returns_none() {
        // A lone non-focusable leaf: nothing to land on.
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                ..Default::default()
            });
            cx.root().unwrap()
        };
        assert_eq!(focus_next(&mut store, root, true), None);
        assert_eq!(store.focused(), None);
    }

    #[test]
    fn focus_change_marks_paint_on_old_and_new_only() {
        let (mut store, root, [a, b, _c]) = three_focusable_leaves();
        // Enter focus at `a`, then clear all dirty so the next move is isolated.
        focus_next(&mut store, root, true);
        assert_eq!(store.focused(), Some(a));
        store.clear_dirty();
        assert!(store.dirty(a).is_empty());
        assert!(store.dirty(b).is_empty());

        // Move a -> b: exactly a and b repaint and re-fold their semantics.
        focus_next(&mut store, root, true);
        assert_eq!(store.focused(), Some(b));
        assert!(
            store
                .dirty(a)
                .contains(DirtyClass::PAINT | DirtyClass::SEMANTICS)
        );
        assert!(
            store
                .dirty(b)
                .contains(DirtyClass::PAINT | DirtyClass::SEMANTICS)
        );
        // The parent never repaints; it only learns via the bubbling SEMANTICS
        // mark that its subtree's accessibility changed.
        assert!(
            !store.dirty(root).intersects(DirtyClass::PAINT),
            "the parent does not repaint"
        );
        assert!(
            store.dirty(root).contains(DirtyClass::SEMANTICS),
            "SEMANTICS bubbles to the parent"
        );
    }

    #[test]
    fn route_key_reaches_focused_node_and_bubbles() {
        // flex(key handler logs 0) { leaf(key handler logs 1, focused) }.
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let child = Rc::new(RefCell::new(None));
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let child2 = child.clone();
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
                    *child2.borrow_mut() = Some(c.id());
                },
            );
            (flex.id(), cx.root().unwrap())
        };
        let (flex_id, root) = root;
        let child_id = child.borrow().unwrap();
        store.set_key_handler(flex_id, Box::new(key_log(log.clone(), 0)));
        store.set_key_handler(child_id, Box::new(key_log(log.clone(), 1)));
        store.set_focusable(child_id, true);
        store.set_focused(Some(child_id));

        let mut chain = Vec::new();
        let ran = KeyRouter::route_key(
            &mut store,
            &mut states,
            &bindings,
            root,
            key(Key::Enter),
            &mut chain,
        );
        assert!(ran);
        // capture: flex(0). target: child(1). bubble: flex(0).
        assert_eq!(*log.borrow(), vec![0, 1, 0]);
    }

    #[test]
    fn route_key_with_no_focus_dispatches_nothing() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let ran = Rc::new(RefCell::new(false));
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                ..Default::default()
            });
            cx.root().unwrap()
        };
        let flag = ran.clone();
        store.set_key_handler(root, Box::new(move |_| *flag.borrow_mut() = true));
        // No focus set.
        let mut chain = Vec::new();
        let dispatched = KeyRouter::route_key(
            &mut store,
            &mut states,
            &bindings,
            root,
            key(Key::Enter),
            &mut chain,
        );
        assert!(!dispatched);
        assert!(!*ran.borrow());
    }

    #[test]
    fn pointer_and_key_columns_are_isolated() {
        // A node with a pointer handler and a key handler: a pointer route fires
        // only the pointer one, a key route only the key one.
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let pointer_ran = Rc::new(RefCell::new(false));
        let key_ran = Rc::new(RefCell::new(false));
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let h = cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                ..Default::default()
            });
            let pflag = pointer_ran.clone();
            cx.on_pointer(h, move |_| *pflag.borrow_mut() = true);
            cx.root().unwrap()
        };
        let kflag = key_ran.clone();
        store.set_key_handler(root, Box::new(move |_| *kflag.borrow_mut() = true));
        store.set_focusable(root, true);
        store.set_focused(Some(root));
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
        // Pointer route: only the pointer handler fires.
        route_pointer(
            &mut store,
            &mut states,
            &bindings,
            root,
            down(5.0, 5.0),
            &mut chain,
        );
        assert!(*pointer_ran.borrow(), "pointer route fires pointer handler");
        assert!(!*key_ran.borrow(), "pointer route leaves key handler alone");

        // Key route: only the key handler fires.
        *pointer_ran.borrow_mut() = false;
        KeyRouter::route_key(
            &mut store,
            &mut states,
            &bindings,
            root,
            key(Key::Enter),
            &mut chain,
        );
        assert!(*key_ran.borrow(), "key route fires key handler");
        assert!(
            !*pointer_ran.borrow(),
            "key route leaves pointer handler alone"
        );
    }

    #[test]
    fn route_ime_preedit_then_commit_reach_focused() {
        // A focused leaf whose key handler records each IME string it sees.
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                ..Default::default()
            });
            cx.root().unwrap()
        };
        let seen2 = seen.clone();
        store.set_key_handler(
            root,
            Box::new(move |cx: &mut EventCx<'_>| {
                if let Some(ime) = cx.ime() {
                    let s = match ime {
                        ImeEvent::Preedit { text, .. } => format!("pre:{text}"),
                        ImeEvent::Commit { text } => format!("commit:{text}"),
                    };
                    seen2.borrow_mut().push(s);
                }
            }),
        );
        store.set_focusable(root, true);
        store.set_focused(Some(root));

        let mut chain = Vec::new();
        KeyRouter::route_ime(
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
        KeyRouter::route_ime(
            &mut store,
            &mut states,
            &bindings,
            root,
            ImeEvent::Commit {
                text: "你".to_string(),
            },
            &mut chain,
        );
        assert_eq!(
            *seen.borrow(),
            vec!["pre:n".to_string(), "commit:你".to_string()]
        );
    }

    #[test]
    fn focus_request_from_a_handler_moves_focus() {
        // Node a is focused and its key handler requests focus move to b.
        let (mut store, root, [a, b, _c]) = three_focusable_leaves();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        store.set_key_handler(a, Box::new(move |cx: &mut EventCx<'_>| cx.request_focus(b)));
        store.set_focused(Some(a));
        store.clear_dirty();

        let mut chain = Vec::new();
        KeyRouter::route_key(
            &mut store,
            &mut states,
            &bindings,
            root,
            key(Key::Tab),
            &mut chain,
        );
        assert_eq!(store.focused(), Some(b));
        assert!(store.dirty(a).contains(DirtyClass::PAINT));
        assert!(store.dirty(b).contains(DirtyClass::PAINT));
    }

    fn key(k: Key) -> KeyEvent {
        KeyEvent {
            key: k,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }
    }

    /// Key-handler twin of `log_handler`: push a label each time it runs.
    fn key_log(log: Rc<RefCell<Vec<u32>>>, label: u32) -> impl FnMut(&mut EventCx<'_>) + 'static {
        move |_ev| log.borrow_mut().push(label)
    }

    // ---- scroll routing ----

    use crate::component::ScrollStyle;

    fn wheel(x: f32, y: f32, dx: f32, dy: f32) -> ScrollEvent {
        ScrollEvent {
            x,
            y,
            delta_x: dx,
            delta_y: dy,
            modifiers: Modifiers::default(),
        }
    }

    /// A 100×100 vertical viewport whose content child is 100×300 (200px of
    /// vertical range), laid out at the origin. Returns the store, the viewport,
    /// and its inner content node.
    fn scroll_scene() -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let mut content_id = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.scroll(
                ScrollStyle {
                    axis: Axis::Column,
                    size: Size::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    let inner = cx.leaf(LeafStyle {
                        size: Size::fixed(100.0, 300.0),
                        ..Default::default()
                    });
                    content_id = Some(inner.id());
                },
            );
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
        (store, root, content_id.unwrap())
    }

    #[test]
    fn scroll_absorbs_axis_delta_and_marks_transform() {
        let (mut store, viewport, _content) = scroll_scene();
        store.clear_dirty();
        let consumed = route_scroll(&mut store, viewport, wheel(50.0, 50.0, 0.0, 40.0));
        assert!(consumed);
        assert_eq!(store.scroll(viewport), Vec2 { x: 0.0, y: 40.0 });
        // A scroll dirties only transform/hit-test/paint, never layout/measure.
        let d = store.dirty(viewport);
        assert!(d.contains(DirtyClass::TRANSFORM));
        assert!(d.contains(DirtyClass::HIT_TEST));
        assert!(d.contains(DirtyClass::PAINT));
        assert!(!d.intersects(DirtyClass::LAYOUT | DirtyClass::MEASURE));
    }

    #[test]
    fn scroll_clamps_to_range() {
        let (mut store, viewport, _content) = scroll_scene();
        // Range is content(300) − viewport(100) = 200; a huge delta clamps there.
        route_scroll(&mut store, viewport, wheel(50.0, 50.0, 0.0, 10_000.0));
        assert_eq!(store.scroll(viewport), Vec2 { x: 0.0, y: 200.0 });
        // Scrolling further down past the clamp moves nothing.
        store.clear_dirty();
        let consumed = route_scroll(&mut store, viewport, wheel(50.0, 50.0, 0.0, 50.0));
        assert!(!consumed, "no room past the clamp");
        assert_eq!(store.scroll(viewport), Vec2 { x: 0.0, y: 200.0 });
    }

    #[test]
    fn scroll_ignores_cross_axis_and_offaxis_only() {
        let (mut store, viewport, _content) = scroll_scene();
        // A column viewport does not scroll horizontally; a pure-x delta is left
        // unconsumed (no horizontal range on this viewport).
        let consumed = route_scroll(&mut store, viewport, wheel(50.0, 50.0, 40.0, 0.0));
        assert!(!consumed);
        assert_eq!(store.scroll(viewport), Vec2::ZERO);
    }

    #[test]
    fn scroll_miss_consumes_nothing() {
        let (mut store, viewport, _content) = scroll_scene();
        let consumed = route_scroll(&mut store, viewport, wheel(500.0, 500.0, 0.0, 40.0));
        assert!(!consumed);
        assert_eq!(store.scroll(viewport), Vec2::ZERO);
    }

    /// Outer horizontal viewport containing an inner vertical viewport: a diagonal
    /// wheel places its vertical component on the inner and its horizontal on the
    /// outer (nested-scroll chaining, each axis to its own scroller).
    #[test]
    fn nested_scroll_splits_delta_by_axis() {
        let mut store = NodeStore::new();
        let mut inner_id = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            // Outer: horizontal 100×100 viewport, content 300 wide → 200px x-range.
            cx.scroll(
                ScrollStyle {
                    axis: Axis::Row,
                    size: Size::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    // The outer's single content child is the inner vertical
                    // viewport, sized to the outer content extent (300 wide).
                    let inner = cx.scroll(
                        ScrollStyle {
                            axis: Axis::Column,
                            size: Size::fixed(300.0, 100.0),
                            ..Default::default()
                        },
                        |cx| {
                            cx.leaf(LeafStyle {
                                size: Size::fixed(300.0, 300.0),
                                ..Default::default()
                            });
                        },
                    );
                    inner_id = Some(inner.id());
                },
            );
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
        let inner = inner_id.unwrap();

        // A point inside the inner viewport, diagonal wheel.
        let consumed = route_scroll(&mut store, root, wheel(50.0, 50.0, 30.0, 40.0));
        assert!(consumed);
        // Inner (vertical) took the y; outer (horizontal) took the x.
        assert_eq!(store.scroll(inner), Vec2 { x: 0.0, y: 40.0 });
        assert_eq!(store.scroll(root), Vec2 { x: 30.0, y: 0.0 });
    }
}
