//! The [`Popup`] control — a persistent anchor with a floating content layer
//! that opens over the rest of the scene.
//!
//! A `Popup` composes two parts in one overlapping stack: an *anchor* (always
//! visible — the button or field the popup hangs off) and *content* (the floating
//! panel — a menu, a tooltip, a color picker). The content is flagged an
//! [`overlay`](viso_ui::NodeStore::is_overlay), so it paints in the deferred top
//! layer over the whole scene regardless of its tree position, and is initially
//! [`hidden`](viso_ui::NodeStore::hidden), so a closed popup lays out and paints
//! nothing for it. Opening shows it; closing hides it. An optional *scrim* — a
//! half-transparent fill over the anchor's box — is a second overlay quad that
//! paints under the content but over the anchor, dimming what is behind the popup.
//!
//! Both the anchor and the content build once, at build time. Opening and closing
//! do not rebuild them — like [`NavigationStack`](crate::NavigationStack), the
//! content is shown and hidden through the retained `hidden` flag, a deferred
//! request the router applies after a handler returns (an [`EventCx`] holds no node
//! store, so a handler cannot flip the flag directly). An `open` reactive `Bool`
//! cell is bound to the popup's `PAINT` so an open/close is one targeted
//! invalidation, not a rebuild.
//!
//! The popup is dismissible from the keyboard: the content is focusable and honors
//! Escape to close (an interactive control must have a keyboard equivalent,
//! AGENTS section 15), firing an optional `on_dismiss`. An app captures a
//! [`PopupHandle`] (see [`Popup::handle`]) and calls
//! [`open`](PopupHandle::open)/[`close`](PopupHandle::close)/[`toggle`](PopupHandle::toggle)
//! from the anchor's own controls. Anchored *positioning* (edge flip, collision
//! avoidance) is a later slice; this slice anchors the content to the popup's box
//! and simply draws it on top.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use viso_widgets::{PopupHandleSlot, popup};
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Size};
//!
//! let handle: PopupHandleSlot = Rc::new(RefCell::new(None));
//! let control = popup()
//!     .anchor(|cx| { cx.leaf(LeafStyle { size: Size::fixed(80.0, 24.0), style: BoxStyle::NONE }); })
//!     .content(|cx| { cx.leaf(LeafStyle { size: Size::fixed(120.0, 60.0), style: BoxStyle::NONE }); })
//!     .on_dismiss(|_ev| { /* the popup was dismissed */ })
//!     .handle(&handle);
//!
//! // Popup authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! control.build(&mut cx);
//! // `handle` is now filled; the app can `handle.borrow().clone().unwrap().open(ev)` from the anchor.
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset, Justify, Key,
    Length, NodeId, Rgba, Role, Semantics, Size, StateId, StateValue,
};

/// A shared, mutable dismiss callback, fired when the popup closes (Escape, or a
/// programmatic [`PopupHandle::close`]/[`toggle`] that closes). It is cloned into
/// the content's key handler and the [`PopupHandle`] at build time so a keyboard
/// dismiss and a programmatic close drive the same `on_dismiss`. Dismissal is never
/// concurrent (keyboard and programmatic closes are serial within one input
/// transaction), so the runtime never re-enters the `RefCell` borrow.
type SharedDismiss = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>)>>>>;

/// A build-time content builder. It authors a subtree (the anchor, or the floating
/// content) into the popup; boxed so a `Popup` can hold the two closures.
type ContentBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// A shared slot an application creates, passes to [`Popup::handle`], and reads
/// after `build` to obtain the control's [`PopupHandle`]. The `open` cell and the
/// content node id are minted inside `build`, so the handle cannot be returned by
/// the builder chain; the app supplies this slot up front and `build` fills it once
/// the ids exist — the same deferred-fill idiom as [`NavigationStack`]. Written once
/// during build, read once after.
///
/// [`NavigationStack`]: crate::NavigationStack
pub type PopupHandleSlot = Rc<RefCell<Option<PopupHandle>>>;

/// A handle an application captures to drive a built [`Popup`] programmatically.
/// Cheap to clone (it holds only an id, a state id, and shared cells). Call
/// [`PopupHandle::open`] to show the content, [`PopupHandle::close`] to hide it, and
/// [`PopupHandle::toggle`] to flip — all from within an [`EventCx`] (an event
/// handler), since opening/closing defers a `hidden` flip the router applies. An
/// open when already open, or a close when already closed, is a no-op.
#[derive(Clone)]
pub struct PopupHandle {
    /// The reactive cell holding the current open state.
    open: StateId,
    /// The floating content node, shown/hidden to open/close.
    content: NodeId,
    /// The shared `on_dismiss` callback, fired on a close.
    on_dismiss: SharedDismiss,
}

impl PopupHandle {
    /// Open the popup: show the content. An open when already open is a no-op.
    /// Call from within an event handler.
    pub fn open(&self, ev: &mut EventCx<'_>) {
        set_open(&self.on_dismiss, ev, self.open, self.content, true);
    }

    /// Close the popup: hide the content and fire `on_dismiss`. A close when
    /// already closed is a no-op. Call from within an event handler.
    pub fn close(&self, ev: &mut EventCx<'_>) {
        set_open(&self.on_dismiss, ev, self.open, self.content, false);
    }

    /// Toggle the popup: close it if open, open it if closed. Call from within an
    /// event handler.
    pub fn toggle(&self, ev: &mut EventCx<'_>) {
        let next = !matches!(ev.get(self.open), Some(StateValue::Bool(true)));
        set_open(&self.on_dismiss, ev, self.open, self.content, next);
    }
}

/// The visual and layout parameters of a [`Popup`].
///
/// `size` is the popup's own size request within its parent, defaulting to
/// [`Length::Fit`] on both axes so the popup sizes to its anchor. `scrim` is an
/// optional dimming color painted over the popup's box while open; it defaults to
/// `None` — a plain popup (menu, tooltip) has no scrim, keeping the open frame to
/// the content quad alone. A modal-style dimmed backdrop sets a translucent scrim.
/// All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PopupStyle {
    /// The popup's own size request within its parent. Defaults to `Fit` on both
    /// axes (the popup sizes to its anchor).
    pub size: Size,
    /// An optional dimming color painted over the popup's box while open, under the
    /// content and over the anchor. `None` (the default) paints no scrim.
    pub scrim: Option<Rgba>,
}

impl Default for PopupStyle {
    fn default() -> Self {
        PopupStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            scrim: None,
        }
    }
}

/// A persistent anchor with a floating content layer that opens over the scene.
///
/// Construct one with [`popup`], give it an [`anchor`](Popup::anchor) and
/// [`content`](Popup::content), optionally a [`scrim`](Popup::scrim) and an
/// [`on_dismiss`](Popup::on_dismiss), and capture a [`PopupHandle`] with
/// [`handle`](Popup::handle) to open/close/toggle from application code. Escape
/// closes an open popup.
///
/// See the [module docs](self) for a build example. Invalidation: opening/closing
/// writes a reactive `open` cell bound to the popup's `PAINT`, and defers a `hidden`
/// flip on the content the router applies (marking it `LAYOUT | PAINT`). No rebuild:
/// the anchor and content build once.
pub struct Popup {
    /// The always-visible anchor builder (`None` authors no anchor — a bare
    /// floating popup).
    anchor: Option<ContentBuilder>,
    /// The floating content builder (`None` authors no content — an empty popup).
    content: Option<ContentBuilder>,
    style: PopupStyle,
    /// The shared dismiss callback (see [`SharedDismiss`]). `None` until
    /// [`Popup::on_dismiss`] is called; a popup with no callback still closes.
    on_dismiss: SharedDismiss,
    /// The app-supplied slot `build` fills with the control's [`PopupHandle`], or an
    /// unshared throwaway slot when the app did not ask for one.
    handle_slot: PopupHandleSlot,
}

/// Construct an empty [`Popup`] with no anchor, no content, no scrim, and no
/// callback yet. Chain [`Popup::anchor`]/[`Popup::content`] to give it parts,
/// [`Popup::scrim`] to add a backdrop, [`Popup::on_dismiss`] to react to a close,
/// [`Popup::handle`] to capture a [`PopupHandle`], and
/// [`Popup::style`]/[`Popup::size`] to adjust its size.
pub fn popup() -> Popup {
    Popup {
        anchor: None,
        content: None,
        style: PopupStyle::default(),
        on_dismiss: Rc::new(RefCell::new(None)),
        handle_slot: Rc::new(RefCell::new(None)),
    }
}

impl Popup {
    /// Set the anchor builder: the always-visible content the popup hangs off.
    /// Replaces any previously set anchor.
    pub fn anchor(mut self, anchor: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.anchor = Some(Box::new(anchor));
        self
    }

    /// Set the content builder: the floating panel shown while the popup is open.
    /// Replaces any previously set content.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Set the dimming scrim color painted over the popup's box while open.
    pub fn scrim(mut self, scrim: Rgba) -> Self {
        self.style.scrim = Some(scrim);
        self
    }

    /// Set the dismiss callback, fired when the popup closes (Escape or a
    /// programmatic close/toggle-to-closed). Replaces any previously set handler.
    pub fn on_dismiss(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_dismiss.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`PopupStyle`].
    pub fn style(mut self, style: PopupStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the popup's own size request within its parent (defaults to `Fit`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Register an app-supplied [`PopupHandleSlot`] for programmatic open/close.
    /// During [`build`](Component::build) the control fills the slot with a
    /// [`PopupHandle`] bound to the just-minted `open` cell and content id; the app
    /// reads the slot after building and clones the handle into the anchor's
    /// controls. Storing the destination this way (rather than returning it) lets
    /// `build` fill it once the ids exist.
    pub fn handle(mut self, slot: &PopupHandleSlot) -> Self {
        self.handle_slot = slot.clone();
        self
    }
}

/// Drive the shared dismiss callback if one is set; a callback-less popup is a
/// no-op. Dismissal is serial within one input transaction, so the borrow is
/// uncontended.
fn fire_dismiss(cb: &SharedDismiss, ev: &mut EventCx<'_>) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev);
    }
}

/// Set the popup's open state to `next` — but only when it is a genuine change.
/// Opening when already open, or closing when already closed, is a no-op: no cell
/// write, no `hidden` flip, no callback. This also coalesces the capture/bubble
/// double-dispatch a router performs when Escape lands on a leaf inside the focused
/// content (the content root is then an ancestor, so its handler runs on both
/// passes): the first pass writes `open`, flips `hidden`, and (on a close) fires;
/// the second reads the just-written value and short-circuits. [`EventCx::set`]
/// writes the cell eagerly (the flush is deferred, but the stored value updates
/// now), so the guard sees the first pass's write within the same route.
///
/// The `set_hidden` request is deferred (an `EventCx` holds no node store); the
/// router applies it after the handler returns, showing or hiding the content.
/// `on_dismiss` fires only on a close.
fn set_open(cb: &SharedDismiss, ev: &mut EventCx<'_>, open: StateId, content: NodeId, next: bool) {
    let cur = matches!(ev.get(open), Some(StateValue::Bool(true)));
    if cur == next {
        return;
    }
    ev.set(open, StateValue::Bool(next));
    // Open => show the content; close => hide it. Overlay order (set at build) puts
    // the shown content over the whole scene.
    ev.set_hidden(content, !next);
    if !next {
        fire_dismiss(cb, ev);
    }
}

impl Component for Popup {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One shared `open` cell holding the current open state. The popup binds it
        // to its PAINT so an open/close repaints in one targeted invalidation, not a
        // rebuild. The content switches through a deferred `hidden` flip, not this
        // cell. A popup starts closed.
        let open = cx.state(StateValue::Bool(false));

        // The popup: the anchor and the floating content overlap in a stretched
        // stack (an aligned column whose children fill the region overlaps them),
        // the same shape NavigationStack uses. The anchor lays out and paints in
        // place; the content is flagged overlay so it paints in the top layer, and
        // starts hidden.
        let scrim_color = self.style.scrim;
        let dismiss_cb = self.on_dismiss.clone();
        let mut content_id = None;
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 0.0,
                padding: Inset::all(0.0),
                align: Align::Stretch,
                justify: Justify::Start,
                size: self.style.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                // The anchor: always visible, painted in place.
                if let Some(anchor) = &self.anchor {
                    (anchor)(cx);
                }

                // The scrim: a dimming fill over the popup's box, under the content
                // and over the anchor. It is a second overlay quad authored before
                // the content, so the top-layer pass paints it first (scrim), then
                // the content. It starts hidden with the content.
                if let Some(color) = scrim_color {
                    let scrim = cx.leaf(viso_ui::LeafStyle {
                        size: Size::fill(),
                        style: BoxStyle::solid(color),
                    });
                    cx.set_overlay(scrim, true);
                    cx.set_hidden(scrim, true);
                }

                // The floating content: authored once, flagged overlay (top layer),
                // and hidden until opened. Its root is focusable and closes on
                // Escape, and is a Group in the semantics tree (a dialog role is a
                // Modal refinement, a later slice).
                let content = cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        gap: 0.0,
                        padding: Inset::all(0.0),
                        align: Align::Stretch,
                        justify: Justify::Start,
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
                cx.semantics(content, Semantics::role(Role::Group));

                let key_cb = dismiss_cb.clone();
                let content_node = content.id();
                cx.on_key(content, move |ev| {
                    let Some(k) = ev.key() else { return };
                    if !k.pressed || k.repeat {
                        return;
                    }
                    if matches!(k.key, Key::Escape) {
                        set_open(&key_cb, ev, open, content_node, false);
                    }
                });

                content_id = Some(content.id());
            },
        );

        let content = content_id.expect("popup authors its content node");
        cx.bind(open, root, DirtyClass::PAINT);
        cx.semantics(root, Semantics::role(Role::Group));

        // Fill the app-supplied handle slot (if any) with a handle bound to the
        // just-minted cell and content id, so the app can open/close programmatically.
        *self.handle_slot.borrow_mut() = Some(PopupHandle {
            open,
            content,
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

    /// The reactive stores a popup build writes into, kept together so a test can
    /// build the control and then drive its handlers (and any captured
    /// [`PopupHandle`]) against the same state.
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
        /// `BuildCx::new` would panic) and return its root (the popup container).
        fn build(&mut self, control: Popup) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            control.build(&mut cx);
            cx.root().expect("popup declares a root node")
        }

        /// Feed a key sample to a node's key handler, restoring it after, then apply
        /// any deferred `hidden` flips it recorded — the router discipline (take,
        /// drive, restore, apply) reproduced for the test.
        fn key(&mut self, node: NodeId, ev: KeyEvent) {
            let mut handler = self.store.take_key_handler(node).expect("key handler");
            let hidden = {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                cx.__take_hidden_requests()
            };
            self.store.restore_key_handler(node, handler);
            for (id, h) in hidden {
                self.store.set_hidden(id, h);
            }
        }

        /// Drive a `PopupHandle` action (open/close/toggle) as a router would: run it
        /// inside a throwaway `EventCx`, take the deferred `hidden` flips, and apply.
        fn drive(&mut self, act: impl FnOnce(&mut EventCx<'_>)) {
            let ev = read_pointer();
            let hidden = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                act(&mut cx);
                cx.__take_hidden_requests()
            };
            for (id, h) in hidden {
                self.store.set_hidden(id, h);
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

    /// The shared open cell authored by the build. Popup authors exactly one state
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

    /// A basic anchor+content popup with an app-captured handle, the common shape.
    fn basic(slot: &PopupHandleSlot) -> Popup {
        popup().anchor(leaf).content(leaf).handle(slot)
    }

    /// The anchor lays out in place; the content is flagged overlay and starts
    /// hidden (a closed popup shows nothing for it).
    #[test]
    fn anchor_is_in_place_and_content_is_a_hidden_overlay() {
        let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));

        let kids = children(&rx.store, root);
        assert_eq!(kids.len(), 2, "an anchor and a content node");
        let (anchor, content) = (kids[0], kids[1]);
        assert!(!rx.store.is_overlay(anchor), "the anchor paints in place");
        assert!(!rx.store.hidden(anchor), "the anchor is visible");
        assert!(
            rx.store.is_overlay(content),
            "the content is a top-layer overlay"
        );
        assert!(
            rx.store.hidden(content),
            "the content starts hidden (closed)"
        );
    }

    /// The popup root is a `Group`; the content root is a focusable `Group` with a
    /// key handler for Escape-to-close.
    #[test]
    fn popup_root_and_content_are_groups_and_content_is_focusable() {
        let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));

        assert_eq!(
            rx.store.semantics(root).expect("root has semantics").role,
            Role::Group,
            "the popup is a Group container"
        );
        let content = children(&rx.store, root)[1];
        assert_eq!(
            rx.store.semantics(content).expect("content semantics").role,
            Role::Group,
            "the content is a Group"
        );
        assert!(rx.store.focusable(content), "the content is focusable");
        assert!(
            rx.store.has_key_handler(content),
            "the content attaches a key handler for Escape"
        );
    }

    /// A `PopupHandle::open` shows the content and moves the open cell; an open when
    /// already open is a no-op.
    #[test]
    fn handle_open_shows_content_and_is_idempotent() {
        let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));
        let cell = open_cell();
        let content = children(&rx.store, root)[1];
        let h = slot.borrow().clone().expect("build fills the handle slot");

        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        assert_eq!(rx.is_open(cell), Some(true), "open moves the open cell");
        assert!(!rx.store.hidden(content), "the content is now shown");

        // A second open is a no-op — the cell and the flag stay.
        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        assert_eq!(rx.is_open(cell), Some(true), "a second open is a no-op");
        assert!(!rx.store.hidden(content), "the content stays shown");
    }

    /// A `PopupHandle::close` hides the content, moves the open cell, and fires
    /// `on_dismiss` once; a close when already closed is a no-op.
    #[test]
    fn handle_close_hides_content_fires_dismiss_and_is_idempotent() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();

        let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot).on_dismiss(move |_ev| {
            c.set(c.get() + 1);
        }));
        let cell = open_cell();
        let content = children(&rx.store, root)[1];
        let h = slot.borrow().clone().expect("build fills the handle slot");

        // Open, then close.
        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        let n = h.clone();
        rx.drive(|ev| n.close(ev));
        assert_eq!(rx.is_open(cell), Some(false), "close moves the open cell");
        assert!(rx.store.hidden(content), "the content is now hidden");
        assert_eq!(count.get(), 1, "close fires on_dismiss once");

        // A second close is a no-op — no cell write, no extra dismiss.
        let n = h.clone();
        rx.drive(|ev| n.close(ev));
        assert_eq!(rx.is_open(cell), Some(false), "a second close is a no-op");
        assert_eq!(count.get(), 1, "and does not fire again");
    }

    /// A `PopupHandle::toggle` flips open/closed, firing `on_dismiss` on the close.
    #[test]
    fn handle_toggle_flips_open_and_closed() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();

        let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot).on_dismiss(move |_ev| {
            c.set(c.get() + 1);
        }));
        let cell = open_cell();
        let content = children(&rx.store, root)[1];
        let h = slot.borrow().clone().expect("build fills the handle slot");

        // Toggle open.
        let n = h.clone();
        rx.drive(|ev| n.toggle(ev));
        assert_eq!(rx.is_open(cell), Some(true), "toggle opens a closed popup");
        assert!(!rx.store.hidden(content), "the content is shown");
        assert_eq!(count.get(), 0, "opening does not fire on_dismiss");

        // Toggle closed.
        let n = h.clone();
        rx.drive(|ev| n.toggle(ev));
        assert_eq!(rx.is_open(cell), Some(false), "toggle closes an open popup");
        assert!(rx.store.hidden(content), "the content is hidden");
        assert_eq!(count.get(), 1, "closing fires on_dismiss once");
    }

    /// Escape on the focused content closes the popup and fires `on_dismiss`;
    /// auto-repeat and key-up do not, and Escape on an already-closed popup is a
    /// no-op.
    #[test]
    fn escape_closes_the_open_popup() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();

        let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot).on_dismiss(move |_ev| {
            c.set(c.get() + 1);
        }));
        let cell = open_cell();
        let content = children(&rx.store, root)[1];
        let h = slot.borrow().clone().expect("build fills the handle slot");

        // Open, then Escape closes.
        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        rx.key(content, key_ev(Key::Escape, true, false));
        assert_eq!(rx.is_open(cell), Some(false), "Escape closes the popup");
        assert!(rx.store.hidden(content), "the content is hidden");
        assert_eq!(count.get(), 1, "the Escape dismiss fires on_dismiss");

        // Reopen; auto-repeat and key-up do not close.
        let n = h.clone();
        rx.drive(|ev| n.open(ev));
        let before = count.get();
        rx.key(content, key_ev(Key::Escape, true, true)); // repeat: ignored
        rx.key(content, key_ev(Key::Escape, false, false)); // key-up: ignored
        assert_eq!(rx.is_open(cell), Some(true), "repeat/key-up do not close");
        assert_eq!(count.get(), before, "and do not fire on_dismiss");

        // Close, then Escape on a closed popup is a no-op.
        let n = h.clone();
        rx.drive(|ev| n.close(ev));
        let before = count.get();
        rx.key(content, key_ev(Key::Escape, true, false));
        assert_eq!(
            rx.is_open(cell),
            Some(false),
            "Escape on a closed popup is a no-op"
        );
        assert_eq!(count.get(), before, "and does not fire again");
    }

    /// With a scrim, a second overlay quad is authored between the anchor and the
    /// content, in that top-layer order, and starts hidden with the content.
    #[test]
    fn a_scrim_is_a_hidden_overlay_between_anchor_and_content() {
        let scrim = Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.5,
        };
        let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(
            popup()
                .anchor(leaf)
                .content(leaf)
                .scrim(scrim)
                .handle(&slot),
        );

        let kids = children(&rx.store, root);
        assert_eq!(kids.len(), 3, "an anchor, a scrim, and a content node");
        let (anchor, scrim_node, content) = (kids[0], kids[1], kids[2]);
        assert!(!rx.store.is_overlay(anchor), "the anchor paints in place");
        assert!(
            rx.store.is_overlay(scrim_node),
            "the scrim is a top-layer overlay"
        );
        assert!(
            rx.store.hidden(scrim_node),
            "the scrim starts hidden (closed)"
        );
        assert!(
            rx.store.is_overlay(content),
            "the content is a top-layer overlay"
        );
        // Author order: scrim before content, so the top-layer pass paints the
        // scrim first (under) and the content after (over).
        let arena = rx.store.arena();
        let scrim_next = arena.links(scrim_node).and_then(|l| l.next_sibling);
        assert_eq!(
            scrim_next,
            Some(content),
            "the scrim paints under the content"
        );
    }

    /// A handle-less popup still opens/closes through the shared cell — `build` does
    /// not panic and a close with no callback is a no-op on the callback.
    #[test]
    fn handleless_popup_still_opens_and_closes() {
        let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
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
            "a callback-less popup still moves the shared cell"
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
