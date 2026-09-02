//! Phase-specific contexts.
//!
//! Instead of one omnipotent context, each frame phase gets a purpose-specific
//! one. This is both an architectural constraint and a performance / reasoning
//! tool: a `LayoutCx` cannot launch a network request, create a window, or
//! submit arbitrary GPU work.
//!
//! Phase 0 defines these as marker types so signatures like
//! `fn layout(&mut self, cx: &mut LayoutCx<'_>, ..)` can be written against a
//! stable contract before the real capabilities are filled in.

use core::marker::PhantomData;

use crate::binding::BindingTable;
use crate::component::NodeStore;
use crate::input::{ImeEvent, KeyEvent, PointerEvent};
use crate::node::NodeId;
use crate::state::{StateId, StateStore, StateValue};

macro_rules! phase_cx {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        pub struct $name<'a> {
            _life: PhantomData<&'a mut ()>,
        }
        impl<'a> $name<'a> {
            #[doc(hidden)]
            pub fn __new() -> Self {
                Self { _life: PhantomData }
            }
        }
    };
}

phase_cx!(/// Application-scope context: create windows, register services.
    AppCx);
phase_cx!(/// Layout context. No network, no window creation, no GPU submit.
    LayoutCx);
phase_cx!(/// Paint-data production context.
    PaintCx);
phase_cx!(/// Render/submit context (internal).
    RenderCx);
phase_cx!(/// Async task context.
    TaskCx);

/// State-flush / reactive update context.
///
/// This is the write end handed to component update / action handlers. Unlike
/// the marker contexts, it holds live borrows of the reactive stores so a
/// handler can read and write state. Writes are deferred: [`UpdateCx::set`]
/// records the change in the state store's per-frame pending set, and the
/// frame's flush phase turns each changed state into targeted node dirtying via
/// the [`BindingTable`]. Nothing recomputes at write time, so many writes in
/// one action collapse into one flush.
///
/// The context deliberately does *not* expose `mark_dirty` or layout entry
/// points: a handler declares intent by writing state, and invalidation follows
/// from the compiled bindings. It holds `nodes` for reads and for the flush's
/// own use, not as a manual-invalidation escape hatch.
pub struct UpdateCx<'a> {
    states: &'a mut StateStore,
    bindings: &'a BindingTable,
    #[allow(dead_code)]
    nodes: &'a mut NodeStore,
}

impl<'a> UpdateCx<'a> {
    /// Assemble the update context from the frame's reactive stores.
    #[doc(hidden)]
    pub fn __new(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        nodes: &'a mut NodeStore,
    ) -> Self {
        Self {
            states,
            bindings,
            nodes,
        }
    }

    /// Read the current value of a state cell. `None` for a stale handle.
    #[inline]
    pub fn get(&self, id: StateId) -> Option<StateValue> {
        self.states.get(id)
    }

    /// Write a state value. The change is recorded in this frame's pending set
    /// and applied to bound nodes by the flush phase, not here. Returns whether
    /// the write landed (the handle was live). Setting the current value is a
    /// no-op that schedules no work.
    #[inline]
    pub fn set(&mut self, id: StateId, value: StateValue) -> bool {
        self.states.set(id, value)
    }

    /// The binding table backing this frame — the flush reads it to turn
    /// changed states into node dirtying.
    #[inline]
    pub fn bindings(&self) -> &BindingTable {
        self.bindings
    }
}

/// Input-dispatch context handed to a pointer/key/IME handler.
///
/// Like [`UpdateCx`] it holds the reactive stores so a handler can read and
/// write state; writes are deferred ([`EventCx::set`] records the change in the
/// per-frame pending set, and the frame's flush turns it into targeted node
/// dirtying via the [`BindingTable`]). It deliberately holds no node store and
/// exposes no `mark_dirty`: a handler declares intent by writing state, and
/// invalidation follows from the compiled bindings.
///
/// Whichever of the three input kinds is under dispatch, the cx carries a borrow
/// of that event so the handler can read *which* sample it is reacting to: the
/// pointer sample (its phase, position, buttons — so a handler acts on release,
/// not on every phase), the key transition, or the composition string a text
/// control needs. Exactly one of the three is `Some` per dispatch; the others
/// are `None`. A handler may also request a focus change: the request is
/// recorded here and applied by the router after the handler returns, since the
/// cx holds no node store to mutate the focus slot directly.
pub struct EventCx<'a> {
    states: &'a mut StateStore,
    #[allow(dead_code)]
    bindings: &'a BindingTable,
    /// The pointer event currently being dispatched, if this is a pointer
    /// dispatch.
    pointer: Option<&'a PointerEvent>,
    /// The key event currently being dispatched, if this is a key dispatch.
    key: Option<&'a KeyEvent>,
    /// The IME event currently being dispatched, if this is an IME dispatch.
    ime: Option<&'a ImeEvent>,
    /// A focus request a handler made this dispatch: `Some(Some(id))` focuses
    /// `id`, `Some(None)` clears focus, `None` = no request. Applied by the
    /// router after the handler returns.
    focus_request: Option<Option<NodeId>>,
    /// A pointer-capture request a handler made this dispatch: `Some(Some(id))`
    /// captures the pointer to `id`, `Some(None)` releases capture, `None` = no
    /// request. Applied by the router after the handler returns.
    capture_request: Option<Option<NodeId>>,
    /// Set by [`EventCx::stop_propagation`]: when true, the router stops walking
    /// the remaining dispatch chain after this handler returns. Shared by the
    /// pointer, key, and IME paths.
    stop: bool,
}

impl<'a> EventCx<'a> {
    /// Assemble the event context for the pointer path with no borrowed sample —
    /// a handler here can read and write state but not inspect the pointer. Used
    /// by state-only tests; the live router uses [`EventCx::__new_pointer`].
    #[doc(hidden)]
    pub fn __new(states: &'a mut StateStore, bindings: &'a BindingTable) -> Self {
        Self {
            states,
            bindings,
            pointer: None,
            key: None,
            ime: None,
            focus_request: None,
            capture_request: None,
            stop: false,
        }
    }

    /// Assemble the event context for a pointer dispatch, injecting the borrowed
    /// pointer sample so the handler can read it via [`EventCx::pointer`] — its
    /// phase in particular, so a click acts on `Up` rather than every sample.
    #[doc(hidden)]
    pub fn __new_pointer(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        pointer: &'a PointerEvent,
    ) -> Self {
        Self {
            states,
            bindings,
            pointer: Some(pointer),
            key: None,
            ime: None,
            focus_request: None,
            capture_request: None,
            stop: false,
        }
    }

    /// Assemble the event context for a key dispatch, injecting the borrowed
    /// key event so the handler can read it via [`EventCx::key`].
    #[doc(hidden)]
    pub fn __new_key(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        key: &'a KeyEvent,
    ) -> Self {
        Self {
            states,
            bindings,
            pointer: None,
            key: Some(key),
            ime: None,
            focus_request: None,
            capture_request: None,
            stop: false,
        }
    }

    /// Assemble the event context for an IME dispatch, injecting the borrowed
    /// IME event so the handler can read it via [`EventCx::ime`].
    #[doc(hidden)]
    pub fn __new_ime(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        ime: &'a ImeEvent,
    ) -> Self {
        Self {
            states,
            bindings,
            pointer: None,
            key: None,
            ime: Some(ime),
            focus_request: None,
            capture_request: None,
            stop: false,
        }
    }

    /// Read the current value of a state cell. `None` for a stale handle.
    #[inline]
    pub fn get(&self, id: StateId) -> Option<StateValue> {
        self.states.get(id)
    }

    /// Write a state value. Deferred: recorded in this frame's pending set and
    /// applied to bound nodes by the flush phase, not here. Returns whether the
    /// write landed (the handle was live). Setting the current value is a no-op.
    #[inline]
    pub fn set(&mut self, id: StateId, value: StateValue) -> bool {
        self.states.set(id, value)
    }

    /// The pointer sample under dispatch, if this is a pointer dispatch; `None`
    /// on the key and IME paths. A pointer handler reads it to gate on phase —
    /// e.g. act on `PointerPhase::Up` so a down/up pair counts as one click —
    /// or to read the position, buttons, or modifiers of the sample.
    #[inline]
    pub fn pointer(&self) -> Option<&PointerEvent> {
        self.pointer
    }

    /// The key event under dispatch, if this is a key dispatch; `None` on the
    /// pointer and IME paths.
    #[inline]
    pub fn key(&self) -> Option<&KeyEvent> {
        self.key
    }

    /// The IME event under dispatch, if this is an IME dispatch; `None` on the
    /// pointer and key paths.
    #[inline]
    pub fn ime(&self) -> Option<&ImeEvent> {
        self.ime
    }

    /// Request that focus move to `id` after this handler returns. The router
    /// applies it (dirtying the old and new focused nodes' paint); the cx holds
    /// no node store, so nothing moves at call time.
    #[inline]
    pub fn request_focus(&mut self, id: NodeId) {
        self.focus_request = Some(Some(id));
    }

    /// Request that focus be cleared after this handler returns.
    #[inline]
    pub fn clear_focus(&mut self) {
        self.focus_request = Some(None);
    }

    /// Take the pending focus request out of the cx for the router to apply.
    /// `None` means the handler made no request this dispatch.
    #[doc(hidden)]
    pub fn __take_focus_request(&mut self) -> Option<Option<NodeId>> {
        self.focus_request.take()
    }

    /// Request that the pointer be captured to `id` after this handler returns.
    /// While captured, subsequent pointer samples route straight to `id` (a
    /// drag, a slider grab) instead of hit-testing. The router applies it; the
    /// cx holds no node store, so nothing moves at call time.
    #[inline]
    pub fn capture_pointer(&mut self, id: NodeId) {
        self.capture_request = Some(Some(id));
    }

    /// Request that pointer capture be released after this handler returns.
    #[inline]
    pub fn release_pointer(&mut self) {
        self.capture_request = Some(None);
    }

    /// Take the pending capture request out of the cx for the router to apply.
    /// `None` means the handler made no request this dispatch.
    #[doc(hidden)]
    pub fn __take_capture_request(&mut self) -> Option<Option<NodeId>> {
        self.capture_request.take()
    }

    /// Stop the event from propagating to the rest of the dispatch chain. The
    /// current handler still finishes; the router walks no further ancestors (or
    /// descendants) for this event. Shared by the pointer, key, and IME paths.
    #[inline]
    pub fn stop_propagation(&mut self) {
        self.stop = true;
    }

    /// Whether a handler called [`stop_propagation`](Self::stop_propagation) this
    /// dispatch. The router checks it after each handler to decide whether to
    /// continue the chain.
    #[doc(hidden)]
    pub fn __stop_requested(&self) -> bool {
        self.stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::BindingTable;
    use crate::component::{BuildCx, LeafStyle, NodeStore};
    use crate::input::{ImeEvent, Key, KeyEvent, PointerButtons, PointerEvent, PointerPhase};
    use crate::layout::Size;
    use crate::state::{StateStore, StateValue};

    /// Mint a live `NodeId` (its fields are private, so a test can't build one
    /// directly — allocate a real leaf and read its handle back).
    fn a_live_node() -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            let h = cx.leaf(LeafStyle {
                size: Size::fixed(1.0, 1.0),
                ..Default::default()
            });
            cx.root();
            h.id()
        };
        (store, id)
    }

    #[test]
    fn event_cx_reads_and_defers_writes() {
        let mut states = StateStore::new();
        let id = states.alloc(StateValue::Int(1));
        let bindings = BindingTable::new();
        {
            let mut cx = EventCx::__new(&mut states, &bindings);
            assert_eq!(cx.get(id), Some(StateValue::Int(1)));
            assert!(cx.set(id, StateValue::Int(2)));
        }
        assert_eq!(states.get(id), Some(StateValue::Int(2)));
        assert!(states.has_pending());
    }

    #[test]
    fn no_event_cx_carries_no_sample() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let cx = EventCx::__new(&mut states, &bindings);
        assert!(cx.pointer().is_none());
        assert!(cx.key().is_none());
        assert!(cx.ime().is_none());
    }

    #[test]
    fn pointer_cx_exposes_the_pointer_event_only() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let ev = PointerEvent {
            x: 3.0,
            y: 4.0,
            phase: PointerPhase::Up,
            buttons: PointerButtons::PRIMARY,
            modifiers: Default::default(),
        };
        let cx = EventCx::__new_pointer(&mut states, &bindings, &ev);
        assert_eq!(cx.pointer(), Some(&ev));
        assert_eq!(cx.pointer().map(|p| p.phase), Some(PointerPhase::Up));
        assert!(cx.key().is_none());
        assert!(cx.ime().is_none());
    }

    #[test]
    fn key_cx_exposes_the_key_event_only() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let ev = KeyEvent {
            key: Key::Enter,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        let cx = EventCx::__new_key(&mut states, &bindings, &ev);
        assert_eq!(cx.key(), Some(&ev));
        assert!(cx.pointer().is_none());
        assert!(cx.ime().is_none());
    }

    #[test]
    fn ime_cx_exposes_the_ime_event_only() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let ev = ImeEvent::Commit {
            text: "a".to_string(),
        };
        let cx = EventCx::__new_ime(&mut states, &bindings, &ev);
        assert_eq!(cx.ime(), Some(&ev));
        assert!(cx.pointer().is_none());
        assert!(cx.key().is_none());
    }

    #[test]
    fn request_focus_records_a_deferred_request() {
        let (_store, id) = a_live_node();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut cx = EventCx::__new(&mut states, &bindings);
        assert_eq!(cx.__take_focus_request(), None, "no request by default");
        cx.request_focus(id);
        assert_eq!(cx.__take_focus_request(), Some(Some(id)));
        assert_eq!(cx.__take_focus_request(), None, "taking clears it");
    }

    #[test]
    fn clear_focus_records_a_clear_request() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut cx = EventCx::__new(&mut states, &bindings);
        cx.clear_focus();
        assert_eq!(cx.__take_focus_request(), Some(None));
    }
}
