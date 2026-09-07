//! The [`Modal`] control — a dialog layered over the whole scene that takes focus
//! and, while open, confines keyboard navigation to its own content.
//!
//! A `Modal` is the dialog refinement of [`Popup`](crate::Popup): it composes a
//! floating *content* panel over the surface, with a dimming *scrim* under it. It
//! differs from a popup in three ways that make it modal:
//!
//! 1. **No anchor.** A popup hangs off a visible trigger (a button, a field); a
//!    modal has none — it covers the whole surface. The app drives it through a
//!    captured [`ModalHandle`], opening it from wherever the trigger lives.
//! 2. **Scrim on by default.** A modal dims the scene behind it so the background
//!    reads as inert; a plain popup has no scrim. The scrim is still optional (an
//!    app may pass a transparent color or its own), but the default is a
//!    half-transparent dark wash.
//! 3. **Focus trap + restore.** Opening a modal snapshots what currently holds
//!    focus, moves focus into the dialog content, and installs a *focus scope* so
//!    Tab cycles only inside the content while it is open (the background is
//!    unreachable from the keyboard). Closing releases the scope and sends focus
//!    back to where it was — the WAI-ARIA dialog focus contract.
//!
//! Like [`Popup`], the content builds once and is shown/hidden through the retained
//! [`hidden`](viso_ui::NodeStore::hidden) flag (a deferred request the router
//! applies after a handler returns — an [`EventCx`] holds no node store). An `open`
//! reactive `Bool` cell bound to the modal's `PAINT` makes an open/close one
//! targeted invalidation, not a rebuild. The content is a [`Role::Dialog`] in the
//! semantics tree, focusable, and honors Escape to close (an interactive control
//! must have a keyboard equivalent, AGENTS section 15), firing an optional
//! `on_dismiss`.
//!
//! The focus snapshot/restore and the scope install/clear ride the deferred-request
//! seam: the open/close handler records `request_focus` / `set_focus_scope` /
//! `clear_focus_scope` on the [`EventCx`], and the router applies them to the node
//! store after the handler returns. The pre-open focus target is read from
//! [`EventCx::focused`] (a `Copy` snapshot the router lends in) and kept in the
//! handle's `restore_focus` cell until the close.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use viso_widgets::{ModalHandleSlot, modal};
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Size};
//!
//! let handle: ModalHandleSlot = Rc::new(RefCell::new(None));
//! let control = modal()
//!     .content(|cx| { cx.leaf(LeafStyle { size: Size::fixed(240.0, 160.0), style: BoxStyle::NONE }); })
//!     .on_dismiss(|_ev| { /* the modal was dismissed */ })
//!     .handle(&handle);
//!
//! // Modal authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! control.build(&mut cx);
//! // `handle` is now filled; the app can `handle.borrow().clone().unwrap().open(ev)` from its trigger.
//! ```

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset, Key, Length,
    NodeId, Rgba, Role, Semantics, Size, StateId, StateValue,
};

/// A shared, mutable dismiss callback, fired when the modal closes (Escape, or a
/// programmatic [`ModalHandle::close`]/[`toggle`] that closes). It is cloned into
/// the content's key handler and the [`ModalHandle`] at build time so a keyboard
/// dismiss and a programmatic close drive the same `on_dismiss`. Dismissal is never
/// concurrent (keyboard and programmatic closes are serial within one input
/// transaction), so the runtime never re-enters the `RefCell` borrow.
type SharedDismiss = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>)>>>>;

/// A build-time content builder. It authors the dialog content subtree into the
/// modal; boxed so a `Modal` can hold the closure.
type ContentBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// The node focus should return to when the modal closes, snapshotted from
/// [`EventCx::focused`] on open. Shared between the [`ModalHandle`] and the
/// content's Escape key handler so a programmatic close and a keyboard close
/// restore the same target. Written on open, taken on close.
type RestoreFocus = Rc<Cell<Option<NodeId>>>;

/// A shared slot an application creates, passes to [`Modal::handle`], and reads
/// after `build` to obtain the control's [`ModalHandle`]. The `open` cell and the
/// content node id are minted inside `build`, so the handle cannot be returned by
/// the builder chain; the app supplies this slot up front and `build` fills it once
/// the ids exist — the same deferred-fill idiom as [`Popup`](crate::Popup). Written
/// once during build, read once after.
pub type ModalHandleSlot = Rc<RefCell<Option<ModalHandle>>>;

/// A handle an application captures to drive a built [`Modal`] programmatically.
/// Cheap to clone (it holds only ids and shared cells). Call [`ModalHandle::open`]
/// to show the dialog, [`ModalHandle::close`] to hide it, and [`ModalHandle::toggle`]
/// to flip — all from within an [`EventCx`] (an event handler), since opening/closing
/// defers a `hidden` flip, a focus move, and a focus-scope change the router applies.
/// An open when already open, or a close when already closed, is a no-op.
#[derive(Clone)]
pub struct ModalHandle {
    /// The reactive cell holding the current open state.
    open: StateId,
    /// The dialog content node, shown/hidden to open/close and the focus scope's root.
    content: NodeId,
    /// Where focus returns on close: written from `EventCx::focused` on open.
    restore_focus: RestoreFocus,
    /// The shared `on_dismiss` callback, fired on a close.
    on_dismiss: SharedDismiss,
}

impl ModalHandle {
    /// Open the modal: show the content, trap focus inside it, and move focus into
    /// it. An open when already open is a no-op. Call from within an event handler.
    pub fn open(&self, ev: &mut EventCx<'_>) {
        set_open(self, ev, true);
    }

    /// Close the modal: hide the content, release the focus trap, send focus back to
    /// where it was, and fire `on_dismiss`. A close when already closed is a no-op.
    /// Call from within an event handler.
    pub fn close(&self, ev: &mut EventCx<'_>) {
        set_open(self, ev, false);
    }

    /// Toggle the modal: close it if open, open it if closed. Call from within an
    /// event handler.
    pub fn toggle(&self, ev: &mut EventCx<'_>) {
        let next = !matches!(ev.get(self.open), Some(StateValue::Bool(true)));
        set_open(self, ev, next);
    }
}

/// A translucent black wash — the default scrim dimming the scene behind an open
/// modal so the background reads as inert.
const DEFAULT_SCRIM: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.32,
};

/// The visual and layout parameters of a [`Modal`].
///
/// `size` is the modal's own size request within its parent, defaulting to
/// [`Length::Fill`] on both axes so the dialog (and its scrim) cover the surface.
/// `scrim` is the dimming color painted over the surface while open; unlike a
/// [`Popup`](crate::Popup) it defaults to a translucent dark wash ([`Some`]), the
/// modal backdrop. All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModalStyle {
    /// The modal's own size request within its parent. Defaults to `Fill` on both
    /// axes (the dialog and its scrim cover the surface).
    pub size: Size,
    /// The dimming color painted over the surface while open, under the content.
    /// Defaults to a translucent dark wash; `None` paints no scrim.
    pub scrim: Option<Rgba>,
}

impl Default for ModalStyle {
    fn default() -> Self {
        ModalStyle {
            size: Size::fill(),
            scrim: Some(DEFAULT_SCRIM),
        }
    }
}

/// A dialog layered over the whole scene that takes focus while open.
///
/// Construct one with [`modal`], give it [`content`](Modal::content), optionally a
/// [`scrim`](Modal::scrim) and an [`on_dismiss`](Modal::on_dismiss), and capture a
/// [`ModalHandle`] with [`handle`](Modal::handle) to open/close/toggle from
/// application code. Escape closes an open modal. There is no anchor: a modal covers
/// the surface and is driven entirely through its handle.
///
/// See the [module docs](self) for a build example. Invalidation: opening/closing
/// writes a reactive `open` cell bound to the modal's `PAINT`, defers a `hidden` flip
/// on the content the router applies (marking it `LAYOUT | PAINT`), and defers the
/// focus move and focus-scope change. No rebuild: the content builds once.
pub struct Modal {
    /// The dialog content builder (`None` authors no content — an empty dialog).
    content: Option<ContentBuilder>,
    style: ModalStyle,
    /// The shared dismiss callback (see [`SharedDismiss`]). `None` until
    /// [`Modal::on_dismiss`] is called; a modal with no callback still closes.
    on_dismiss: SharedDismiss,
    /// The app-supplied slot `build` fills with the control's [`ModalHandle`], or an
    /// unshared throwaway slot when the app did not ask for one.
    handle_slot: ModalHandleSlot,
}

/// Construct an empty [`Modal`] with no content yet, the default dark scrim, and no
/// callback. Chain [`Modal::content`] to give it a dialog body, [`Modal::scrim`] to
/// change or drop the backdrop, [`Modal::on_dismiss`] to react to a close,
/// [`Modal::handle`] to capture a [`ModalHandle`], and
/// [`Modal::style`]/[`Modal::size`] to adjust its size.
pub fn modal() -> Modal {
    Modal {
        content: None,
        style: ModalStyle::default(),
        on_dismiss: Rc::new(RefCell::new(None)),
        handle_slot: Rc::new(RefCell::new(None)),
    }
}

impl Modal {
    /// Set the content builder: the dialog body shown while the modal is open.
    /// Replaces any previously set content.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Set the dimming scrim color painted over the surface while open. Pass a fully
    /// transparent color to suppress the backdrop while keeping the modal semantics.
    pub fn scrim(mut self, scrim: Rgba) -> Self {
        self.style.scrim = Some(scrim);
        self
    }

    /// Set the dismiss callback, fired when the modal closes (Escape or a
    /// programmatic close/toggle-to-closed). Replaces any previously set handler.
    pub fn on_dismiss(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_dismiss.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`ModalStyle`].
    pub fn style(mut self, style: ModalStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the modal's own size request within its parent (defaults to `Fill`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Register an app-supplied [`ModalHandleSlot`] for programmatic open/close.
    /// During [`build`](Component::build) the control fills the slot with a
    /// [`ModalHandle`] bound to the just-minted `open` cell and content id; the app
    /// reads the slot after building and clones the handle into its trigger control.
    /// Storing the destination this way (rather than returning it) lets `build` fill
    /// it once the ids exist.
    pub fn handle(mut self, slot: &ModalHandleSlot) -> Self {
        self.handle_slot = slot.clone();
        self
    }
}

/// Drive the shared dismiss callback if one is set; a callback-less modal is a
/// no-op. Dismissal is serial within one input transaction, so the borrow is
/// uncontended.
fn fire_dismiss(cb: &SharedDismiss, ev: &mut EventCx<'_>) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev);
    }
}

/// Set the modal's open state to `next` — but only when it is a genuine change.
/// Opening when already open, or closing when already closed, is a no-op: no cell
/// write, no `hidden` flip, no focus move, no callback. This also coalesces the
/// capture/bubble double-dispatch a router performs when Escape lands on a leaf
/// inside the focused content (the content root is then an ancestor, so its handler
/// runs on both passes): the first pass writes `open` and flips everything; the
/// second reads the just-written value and short-circuits. [`EventCx::set`] writes
/// the cell eagerly (the flush is deferred, but the stored value updates now), so
/// the guard sees the first pass's write within the same route.
///
/// On **open**: snapshot the currently-focused node into `restore_focus` (read from
/// [`EventCx::focused`]), show the content, install the focus scope on the content
/// (trapping Tab inside the dialog), and move focus into the content.
///
/// On **close**: hide the content, clear the focus scope (releasing the trap), send
/// focus back to the snapshotted node (or clear focus if there was none), and fire
/// `on_dismiss`.
///
/// All of the store-touching effects (`set_hidden`, `request_focus`/`clear_focus`,
/// `set_focus_scope`/`clear_focus_scope`) are deferred — an `EventCx` holds no node
/// store; the router applies them after the handler returns.
fn set_open(h: &ModalHandle, ev: &mut EventCx<'_>, next: bool) {
    let cur = matches!(ev.get(h.open), Some(StateValue::Bool(true)));
    if cur == next {
        return;
    }
    ev.set(h.open, StateValue::Bool(next));
    ev.set_hidden(h.content, !next);
    if next {
        // Remember where focus was so close can send it back, then trap focus in
        // the dialog and move focus into it.
        h.restore_focus.set(ev.focused());
        ev.set_focus_scope(h.content);
        ev.request_focus(h.content);
    } else {
        // Release the trap and restore focus to the pre-open target (or clear it if
        // nothing was focused when the modal opened).
        ev.clear_focus_scope();
        match h.restore_focus.take() {
            Some(prev) => ev.request_focus(prev),
            None => ev.clear_focus(),
        }
        fire_dismiss(&h.on_dismiss, ev);
    }
}

impl Component for Modal {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One shared `open` cell holding the current open state. The modal binds it
        // to its PAINT so an open/close repaints in one targeted invalidation, not a
        // rebuild. The content switches through a deferred `hidden` flip, not this
        // cell. A modal starts closed.
        let open = cx.state(StateValue::Bool(false));

        // The modal: the scrim and the floating content overlap in a stretched stack
        // (an aligned column whose children fill the region overlaps them), the same
        // shape Popup uses. Both are flagged overlay so they paint in the top layer,
        // and both start hidden (a closed modal shows nothing). The scrim is authored
        // before the content, so the top-layer pass paints it first (under) and the
        // content after (over).
        let scrim_color = self.style.scrim;
        let dismiss_cb = self.on_dismiss.clone();
        let restore_focus: RestoreFocus = Rc::new(Cell::new(None));
        let mut content_id = None;
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 0.0,
                padding: Inset::all(0.0),
                align: Align::Stretch,
                size: self.style.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                // The scrim: a dimming fill over the whole surface, under the content.
                // A top-layer overlay authored before the content, starting hidden
                // with it.
                if let Some(color) = scrim_color {
                    let scrim = cx.leaf(viso_ui::LeafStyle {
                        size: Size::fill(),
                        style: BoxStyle::solid(color),
                    });
                    cx.set_overlay(scrim, true);
                    cx.set_hidden(scrim, true);
                }

                // The dialog content: authored once, flagged overlay (top layer), and
                // hidden until opened. Its root is focusable (the focus scope's root
                // and the focus target on open), closes on Escape, and is a Dialog in
                // the semantics tree.
                let content = cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        gap: 0.0,
                        padding: Inset::all(0.0),
                        align: Align::Stretch,
                        size: Size {
                            width: Length::Fit,
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                    },
                    |cx| {
                        if let Some(content) = &self.content {
                            (content)(cx);
                        }
                    },
                );
                cx.set_overlay(content, true);
                cx.set_hidden(content, true);
                cx.focusable(content, true);
                cx.semantics(content, Semantics::role(Role::Dialog));

                // Escape closes the modal from the keyboard, driving the same
                // close path (scope release + focus restore + dismiss) as the handle.
                let key_cb = dismiss_cb.clone();
                let key_restore = restore_focus.clone();
                let content_node = content.id();
                cx.on_key(content, move |ev| {
                    let Some(k) = ev.key() else { return };
                    if !k.pressed || k.repeat {
                        return;
                    }
                    if matches!(k.key, Key::Escape) {
                        let h = ModalHandle {
                            open,
                            content: content_node,
                            restore_focus: key_restore.clone(),
                            on_dismiss: key_cb.clone(),
                        };
                        set_open(&h, ev, false);
                    }
                });

                content_id = Some(content.id());
            },
        );

        let content = content_id.expect("modal authors its content node");
        cx.bind(open, root, DirtyClass::PAINT);
        cx.semantics(root, Semantics::role(Role::Group));

        // Fill the app-supplied handle slot (if any) with a handle bound to the
        // just-minted cell, content id, and restore-focus cell, so the app can
        // open/close programmatically.
        *self.handle_slot.borrow_mut() = Some(ModalHandle {
            open,
            content,
            restore_focus,
            on_dismiss: self.on_dismiss.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use viso_ui::{
        BindingTable, KeyEvent, LeafStyle, Modifiers, NodeStore, PointerButtons, PointerEvent,
        PointerPhase, SemanticProjector, StateStore, TextEdits, VirtualLists,
    };

    /// The reactive stores a modal build writes into, kept together so a test can
    /// build the control and then drive its handlers (and any captured
    /// [`ModalHandle`]) against the same state.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        text_edits: TextEdits,
        projectors: SemanticProjector,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
                text_edits: TextEdits::new(),
                projectors: SemanticProjector::new(),
            }
        }

        /// Build a control through a reactive cx (it authors state, so a plain
        /// `BuildCx::new` would panic) and return its root (the modal container).
        fn build(&mut self, control: Modal) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            control.build(&mut cx);
            cx.root().expect("modal declares a root node")
        }

        /// Feed a key sample to the focused node's key handler, restoring it after,
        /// then apply every deferred request it recorded — the router discipline
        /// (take, drive, restore, apply) reproduced for the test: hidden flips, the
        /// focus move, and the focus-scope change. The current focus slot is lent in
        /// so the handler's `ev.focused()` reads it, exactly as the router does.
        fn key(&mut self, node: NodeId, ev: KeyEvent) {
            let mut handler = self.store.take_key_handler(node).expect("key handler");
            let (hidden, focus, scope) = {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                cx.__set_focused(self.store.focused());
                handler(&mut cx);
                (
                    cx.__take_hidden_requests(),
                    cx.__take_focus_request(),
                    cx.__take_focus_scope_request(),
                )
            };
            self.store.restore_key_handler(node, handler);
            self.apply(hidden, focus, scope);
        }

        /// Drive a `ModalHandle` action (open/close/toggle) as a router would: run it
        /// inside a throwaway pointer `EventCx` (lending the current focus slot in),
        /// take the deferred requests, and apply them to the store.
        fn drive(&mut self, act: impl FnOnce(&mut EventCx<'_>)) {
            let ev = read_pointer();
            let (hidden, focus, scope) = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                cx.__set_focused(self.store.focused());
                act(&mut cx);
                (
                    cx.__take_hidden_requests(),
                    cx.__take_focus_request(),
                    cx.__take_focus_scope_request(),
                )
            };
            self.apply(hidden, focus, scope);
        }

        /// Apply the deferred requests a dispatch recorded to the store, mirroring the
        /// router's drain order: hidden flips, then the focus move, then the scope.
        fn apply(
            &mut self,
            hidden: Vec<(NodeId, bool)>,
            focus: Option<Option<NodeId>>,
            scope: Option<Option<NodeId>>,
        ) {
            for (id, h) in hidden {
                self.store.set_hidden(id, h);
            }
            if let Some(target) = focus {
                self.store.set_focused(target);
            }
            if let Some(s) = scope {
                self.store.set_focus_scope(s);
            }
        }

        /// The current value of the shared open cell (via a throwaway read cx).
        fn is_open(&mut self, cell: StateId) -> Option<bool> {
            let ev = read_pointer();
            let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            match cx.get(cell) {
                Some(StateValue::Bool(b)) => Some(b),
                _ => None,
            }
        }
    }

    /// A neutral pointer sample for read-only / handle-driven cx construction.
    fn read_pointer() -> PointerEvent {
        PointerEvent {
            x: 0.0,
            y: 0.0,
            phase: PointerPhase::Move,
            buttons: PointerButtons::NONE,
            modifiers: Modifiers::default(),
        }
    }

    /// A key press/release sample.
    fn key_ev(key: Key, pressed: bool, repeat: bool) -> KeyEvent {
        KeyEvent {
            key,
            pressed,
            repeat,
            modifiers: Modifiers::default(),
        }
    }

    /// The shared open cell authored by the build. Modal authors exactly one state
    /// cell (the shared `open` Bool), so a fresh `StateStore` allocating one `Bool`
    /// cell yields the very handle the build produced (there is no public
    /// constructor for a bare `StateId`, and the build does not return it).
    fn open_cell() -> StateId {
        StateStore::new().alloc(StateValue::Bool(false))
    }

    /// Collect a node's direct children by walking the arena sibling chain.
    fn children(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
        let arena = store.arena();
        let mut out = Vec::new();
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        out
    }

    /// A single fixed leaf, so a builder composes a real subtree rather than an
    /// empty node.
    fn leaf(cx: &mut BuildCx<'_>) {
        cx.leaf(LeafStyle {
            size: Size::fixed(10.0, 10.0),
            style: BoxStyle::NONE,
        });
    }

    /// A basic content modal with an app-captured handle, the common shape (the
    /// default dark scrim is on).
    fn basic(slot: &ModalHandleSlot) -> Modal {
        modal().content(leaf).handle(slot)
    }

    /// A modal defaults to a scrim and a content node, both top-layer overlays that
    /// start hidden; the scrim paints under the content (authored before it). There
    /// is no anchor.
    #[test]
    fn scrim_and_content_are_hidden_overlays_and_scrim_is_under_content() {
        let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));

        let kids = children(&rx.store, root);
        assert_eq!(kids.len(), 2, "a scrim and a content node, no anchor");
        let (scrim, content) = (kids[0], kids[1]);
        assert!(
            rx.store.is_overlay(scrim),
            "the scrim is a top-layer overlay"
        );
        assert!(rx.store.hidden(scrim), "the scrim starts hidden (closed)");
        assert!(
            rx.store.is_overlay(content),
            "the content is a top-layer overlay"
        );
        assert!(
            rx.store.hidden(content),
            "the content starts hidden (closed)"
        );
        // Author order: scrim before content, so the top-layer pass paints the scrim
        // first (under) and the content after (over).
        let arena = rx.store.arena();
        let scrim_next = arena.links(scrim).and_then(|l| l.next_sibling);
        assert_eq!(
            scrim_next,
            Some(content),
            "the scrim paints under the content"
        );
    }

    /// The modal root is a `Group`; the content root is a focusable `Dialog` with a
    /// key handler for Escape-to-close.
    #[test]
    fn modal_root_is_group_and_content_is_a_focusable_dialog() {
        let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));

        assert_eq!(
            rx.store.semantics(root).expect("root has semantics").role,
            Role::Group,
            "the modal is a Group container"
        );
        let content = children(&rx.store, root)[1];
        assert_eq!(
            rx.store.semantics(content).expect("content semantics").role,
            Role::Dialog,
            "the content is a Dialog"
        );
        assert!(rx.store.focusable(content), "the content is focusable");
        assert!(
            rx.store.has_key_handler(content),
            "the content attaches a key handler for Escape"
        );
    }

    /// A `ModalHandle::open` shows the content, moves the open cell, installs the
    /// focus scope on the content, and moves focus into it; an open when already open
    /// is a no-op.
    #[test]
    fn handle_open_shows_content_traps_and_moves_focus_and_is_idempotent() {
        let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));
        let cell = open_cell();
        let content = children(&rx.store, root)[1];
        let h = slot.borrow().clone().expect("build fills the handle slot");

        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        assert_eq!(rx.is_open(cell), Some(true), "open moves the open cell");
        assert!(!rx.store.hidden(content), "the content is now shown");
        assert_eq!(
            rx.store.focus_scope(),
            Some(content),
            "open traps focus in the content subtree"
        );
        assert_eq!(
            rx.store.focused(),
            Some(content),
            "open moves focus into the content"
        );

        // A second open is a no-op.
        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        assert_eq!(rx.is_open(cell), Some(true), "a second open is a no-op");
        assert!(!rx.store.hidden(content), "the content stays shown");
    }

    /// A `ModalHandle::close` hides the content, releases the focus trap, restores
    /// focus to where it was before the open, fires `on_dismiss` once, and is
    /// idempotent.
    #[test]
    fn handle_close_hides_releases_restores_focus_and_fires_dismiss() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();

        let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot).on_dismiss(move |_ev| {
            c.set(c.get() + 1);
        }));
        let cell = open_cell();
        let content = children(&rx.store, root)[1];
        let h = slot.borrow().clone().expect("build fills the handle slot");

        // Something else holds focus before the modal opens — the restore target.
        let trigger = children(&rx.store, content)[0];
        rx.store.set_focusable(trigger, true);
        rx.store.set_focused(Some(trigger));

        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        assert_eq!(rx.store.focused(), Some(content), "open focuses the dialog");

        let n = h.clone();
        rx.drive(|ev| n.close(ev));
        assert_eq!(rx.is_open(cell), Some(false), "close moves the open cell");
        assert!(rx.store.hidden(content), "the content is now hidden");
        assert_eq!(
            rx.store.focus_scope(),
            None,
            "close releases the focus trap"
        );
        assert_eq!(
            rx.store.focused(),
            Some(trigger),
            "close restores focus to the pre-open node"
        );
        assert_eq!(count.get(), 1, "close fires on_dismiss once");

        // A second close is a no-op.
        let n = h.clone();
        rx.drive(|ev| n.close(ev));
        assert_eq!(rx.is_open(cell), Some(false), "a second close is a no-op");
        assert_eq!(count.get(), 1, "and does not fire again");
    }

    /// A `ModalHandle::toggle` flips open/closed, firing `on_dismiss` on the close and
    /// releasing the focus trap.
    #[test]
    fn handle_toggle_flips_open_and_closed() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();

        let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot).on_dismiss(move |_ev| {
            c.set(c.get() + 1);
        }));
        let cell = open_cell();
        let content = children(&rx.store, root)[1];
        let h = slot.borrow().clone().expect("build fills the handle slot");

        let n = h.clone();
        rx.drive(|ev| n.toggle(ev));
        assert_eq!(rx.is_open(cell), Some(true), "toggle opens a closed modal");
        assert!(!rx.store.hidden(content), "the content is shown");
        assert_eq!(
            rx.store.focus_scope(),
            Some(content),
            "toggle-open traps focus"
        );
        assert_eq!(count.get(), 0, "opening does not fire on_dismiss");

        let n = h.clone();
        rx.drive(|ev| n.toggle(ev));
        assert_eq!(rx.is_open(cell), Some(false), "toggle closes an open modal");
        assert!(rx.store.hidden(content), "the content is hidden");
        assert_eq!(
            rx.store.focus_scope(),
            None,
            "toggle-close releases the trap"
        );
        assert_eq!(count.get(), 1, "closing fires on_dismiss once");
    }

    /// Escape on the focused content closes the modal, releases the trap, restores
    /// focus, and fires `on_dismiss`; auto-repeat and key-up do not, and Escape on an
    /// already-closed modal is a no-op.
    #[test]
    fn escape_closes_the_open_modal() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();

        let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot).on_dismiss(move |_ev| {
            c.set(c.get() + 1);
        }));
        let cell = open_cell();
        let content = children(&rx.store, root)[1];
        let h = slot.borrow().clone().expect("build fills the handle slot");

        // A pre-open focus target, so Escape's close can restore it.
        let trigger = children(&rx.store, content)[0];
        rx.store.set_focusable(trigger, true);
        rx.store.set_focused(Some(trigger));

        // Open (focuses the dialog), then Escape on the content closes.
        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        rx.key(content, key_ev(Key::Escape, true, false));
        assert_eq!(rx.is_open(cell), Some(false), "Escape closes the modal");
        assert!(rx.store.hidden(content), "the content is hidden");
        assert_eq!(rx.store.focus_scope(), None, "Escape releases the trap");
        assert_eq!(
            rx.store.focused(),
            Some(trigger),
            "Escape restores focus to the pre-open node"
        );
        assert_eq!(count.get(), 1, "the Escape dismiss fires on_dismiss");

        // Reopen; auto-repeat and key-up do not close.
        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        let before = count.get();
        rx.key(content, key_ev(Key::Escape, true, true)); // repeat: ignored
        rx.key(content, key_ev(Key::Escape, false, false)); // key-up: ignored
        assert_eq!(rx.is_open(cell), Some(true), "repeat/key-up do not close");
        assert_eq!(count.get(), before, "and do not fire on_dismiss");

        // Close, then Escape on a closed modal is a no-op.
        let n = h.clone();
        rx.drive(|ev| n.close(ev));
        let before = count.get();
        rx.key(content, key_ev(Key::Escape, true, false));
        assert_eq!(
            rx.is_open(cell),
            Some(false),
            "Escape on a closed modal is a no-op"
        );
        assert_eq!(count.get(), before, "and does not fire again");
    }

    /// With no pre-open focus (nothing focused when the modal opens), close clears
    /// focus rather than restoring a stale node.
    #[test]
    fn close_with_no_prior_focus_clears_focus() {
        let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));
        let content = children(&rx.store, root)[1];
        let h = slot.borrow().clone().expect("build fills the handle slot");
        assert_eq!(rx.store.focused(), None, "nothing is focused before open");

        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        assert_eq!(rx.store.focused(), Some(content), "open focuses the dialog");

        let n = h.clone();
        rx.drive(|ev| n.close(ev));
        assert_eq!(
            rx.store.focused(),
            None,
            "close clears focus when there was no pre-open target"
        );
    }

    /// A dropped scrim (`scrim` set to a transparent color is still a scrim; passing
    /// through `ModalStyle` with `scrim: None` authors no scrim node) leaves only the
    /// content node.
    #[test]
    fn a_scrimless_modal_has_only_a_content_node() {
        let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(
            modal()
                .content(leaf)
                .style(ModalStyle {
                    size: Size::fill(),
                    scrim: None,
                })
                .handle(&slot),
        );

        let kids = children(&rx.store, root);
        assert_eq!(kids.len(), 1, "no scrim node, just the content");
        assert!(rx.store.is_overlay(kids[0]), "the content is an overlay");
    }

    /// A handle-less modal still opens/closes through the shared cell — `build` does
    /// not panic and a close with no callback is a no-op on the callback.
    #[test]
    fn handleless_modal_still_opens_and_closes() {
        let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));
        let cell = open_cell();
        let content = children(&rx.store, root)[1];
        let h = slot.borrow().clone().expect("build fills the handle slot");

        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        assert_eq!(
            rx.is_open(cell),
            Some(true),
            "a callback-less modal still moves the shared cell"
        );
        assert!(!rx.store.hidden(content), "and shows the content");

        let n = h.clone();
        rx.drive(|ev| n.close(ev));
        assert_eq!(
            rx.is_open(cell),
            Some(false),
            "and closes again without a callback"
        );
    }
}
