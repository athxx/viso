//! The [`Sheet`] control — a panel that slides in from an edge of the surface,
//! dims the scene behind it, and traps focus while open.
//!
//! A `Sheet` is the *edge drawer* refinement of [`Modal`](crate::Modal): it
//! shares the modal scaffolding — a floating content panel over a dimming scrim,
//! a focus trap with restore, Escape-to-close, and an optional `on_dismiss` — but
//! differs in two ways that make it a drawer rather than a centered dialog:
//!
//! 1. **Edge anchor.** A modal covers the surface with a centered dialog; a sheet
//!    pins its content to one edge ([`SheetEdge`], default [`Bottom`](SheetEdge::Bottom))
//!    with a fixed extent along the slide axis and a full-bleed cross axis — a
//!    bottom drawer spans the width and is a fixed height, a side panel spans the
//!    height and is a fixed width.
//! 2. **Slide animation.** Opening a modal simply un-hides its content; opening a
//!    sheet slides the content in from off-screen and slides it back out on close.
//!    The slide is a transform-only animation ([`TranslateAnim`]): it writes the
//!    content's world-space translate each frame (`TRANSFORM | HIT_TEST | PAINT`)
//!    and never relayouts (AGENTS section 8.7). The content shows immediately on
//!    open (visible as it slides in) and hides only when the slide-*out* finishes,
//!    so the drawer is never abruptly clipped.
//!
//! The open/close/animate effects all ride the deferred-request seam an
//! [`EventCx`] exposes: the handler records the `hidden` flip, the focus move, the
//! focus-scope change, and the slide ([`EventCx::request_animation`]); the router
//! applies them after the handler returns (the slide is queued on the store and
//! the driver drains it into its animation registry the next frame). The slide-in
//! finalize is nothing (the content is already shown); the slide-*out* carries an
//! `on_done` that hides the content, releases the focus scope, and restores focus
//! exactly when the motion settles — a store-only callback (an
//! [`AnimationRegistry`](viso_ui::AnimationRegistry) tick holds no `EventCx`), so
//! `on_dismiss` still fires at close-*request* time like a modal.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use viso_widgets::{SheetEdge, SheetHandleSlot, sheet};
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Size};
//!
//! let handle: SheetHandleSlot = Rc::new(RefCell::new(None));
//! let control = sheet()
//!     .edge(SheetEdge::Bottom)
//!     .content(|cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); })
//!     .on_dismiss(|_ev| { /* the sheet was dismissed */ })
//!     .handle(&handle);
//!
//! // Sheet authors reactive state, so it builds through a reactive cx.
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
use std::time::Duration;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, DirtyClass, Easing, EventCx, FlexStyle, Inset, Key,
    Length, NodeId, Rgba, Role, Semantics, Size, StateId, StateValue, TranslateAnim, Vec2,
};

/// A shared, mutable dismiss callback, fired when the sheet closes (Escape, or a
/// programmatic [`SheetHandle::close`]/[`toggle`] that closes). Cloned into the
/// content's key handler and the [`SheetHandle`] at build time so a keyboard
/// dismiss and a programmatic close drive the same `on_dismiss`. Dismissal is
/// never concurrent (serial within one input transaction), so the runtime never
/// re-enters the borrow.
type SharedDismiss = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>)>>>>;

/// A build-time content builder. It authors the drawer content subtree into the
/// sheet; boxed so a `Sheet` can hold the closure.
type ContentBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// The node focus should return to when the sheet closes, snapshotted from
/// [`EventCx::focused`] on open. Shared between the [`SheetHandle`] and the
/// content's Escape key handler so a programmatic close and a keyboard close
/// restore the same target. Written on open, taken on close (in the slide-out's
/// completion callback, so focus returns only once the drawer has slid away).
type RestoreFocus = Rc<Cell<Option<NodeId>>>;

/// A shared slot an application creates, passes to [`Sheet::handle`], and reads
/// after `build` to obtain the control's [`SheetHandle`]. The `open` cell and the
/// content node id are minted inside `build`, so the handle cannot be returned by
/// the builder chain; the app supplies this slot up front and `build` fills it
/// once the ids exist — the same deferred-fill idiom as [`Modal`](crate::Modal).
pub type SheetHandleSlot = Rc<RefCell<Option<SheetHandle>>>;

/// The edge a [`Sheet`] anchors to and slides in from.
///
/// The edge fixes three things: which axis the drawer slides along, which side its
/// content pins to, and where its fixed extent applies. A [`Bottom`](Self::Bottom)
/// or [`Top`](Self::Top) sheet spans the width and is a fixed height (a bottom or
/// top drawer); a [`Leading`](Self::Leading) or [`Trailing`](Self::Trailing) sheet
/// spans the height and is a fixed width (a side panel). Defaults to
/// [`Bottom`](Self::Bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SheetEdge {
    /// Anchored to the bottom edge; slides up in, down out. The default drawer.
    #[default]
    Bottom,
    /// Anchored to the top edge; slides down in, up out.
    Top,
    /// Anchored to the leading (left) edge; slides right in, left out.
    Leading,
    /// Anchored to the trailing (right) edge; slides left in, right out.
    Trailing,
}

impl SheetEdge {
    /// The main axis of the sheet root: the axis the content pins along. Bottom/Top
    /// stack vertically (Column); Leading/Trailing stack horizontally (Row).
    fn axis(self) -> Axis {
        match self {
            SheetEdge::Bottom | SheetEdge::Top => Axis::Column,
            SheetEdge::Leading | SheetEdge::Trailing => Axis::Row,
        }
    }

    /// Whether the content pins to the *far* (main-end) edge and so needs a leading
    /// fill spacer to push it there. A flex places its child at main-start, so
    /// Top/Leading (main-start edges) need no spacer, while Bottom/Trailing
    /// (main-end edges) do. This is the edge-anchor mechanism: the layout engine's
    /// `Align` is cross-axis only, so a `Fill` spacer sibling authored before the
    /// content is what pins it to the far edge.
    fn needs_spacer(self) -> bool {
        matches!(self, SheetEdge::Bottom | SheetEdge::Trailing)
    }

    /// The content's size request for a given fixed `extent` along the slide axis:
    /// full-bleed on the cross axis, `extent` on the main axis.
    fn content_size(self, extent: f32) -> Size {
        match self.axis() {
            Axis::Column => Size {
                width: Length::Fill { weight: 1.0 },
                height: Length::Fixed(extent),
            },
            Axis::Row => Size {
                width: Length::Fixed(extent),
                height: Length::Fill { weight: 1.0 },
            },
        }
    }

    /// The off-screen translate that hides the content fully past its anchored
    /// edge, given its fixed `extent`. The slide runs `from` this offset `to`
    /// `Vec2::ZERO` on open (and the reverse on close). World is `bounds − translate`,
    /// so to push the content *past* its edge by one extent the translate points
    /// *into* the surface: a bottom drawer (resting at the bottom edge) hides by
    /// moving world *down* off-screen, which is a translate of `-extent` in y.
    fn offscreen(self, extent: f32) -> Vec2 {
        match self {
            // Rest at the bottom; hide below → world moves +y → translate −y.
            SheetEdge::Bottom => Vec2 { x: 0.0, y: -extent },
            // Rest at the top; hide above → world moves −y → translate +y.
            SheetEdge::Top => Vec2 { x: 0.0, y: extent },
            // Rest at the left; hide off the left → world moves −x → translate +x.
            SheetEdge::Leading => Vec2 { x: extent, y: 0.0 },
            // Rest at the right; hide off the right → world moves +x → translate −x.
            SheetEdge::Trailing => Vec2 { x: -extent, y: 0.0 },
        }
    }
}

/// A handle an application captures to drive a built [`Sheet`] programmatically.
/// Cheap to clone (it holds only ids, the fixed extent, the edge, and shared
/// cells). Call [`SheetHandle::open`] to slide the drawer in, [`SheetHandle::close`]
/// to slide it out, and [`SheetHandle::toggle`] to flip — all from within an
/// [`EventCx`]. An open when already open, or a close when already closed, is a
/// no-op.
#[derive(Clone)]
pub struct SheetHandle {
    /// The reactive cell holding the current open state.
    open: StateId,
    /// The drawer content node: shown/hidden and slid, the focus scope's root.
    content: NodeId,
    /// The edge the drawer anchors to (fixes the slide direction).
    edge: SheetEdge,
    /// The content's fixed extent along the slide axis (the off-screen distance).
    extent: f32,
    /// The slide duration (both directions).
    duration: Duration,
    /// Where focus returns on close: written from `EventCx::focused` on open,
    /// taken by the slide-out completion callback.
    restore_focus: RestoreFocus,
    /// The shared `on_dismiss` callback, fired on a close.
    on_dismiss: SharedDismiss,
}

impl SheetHandle {
    /// Open the sheet: show the content, slide it in from off-screen, trap focus
    /// inside it, and move focus into it. An open when already open is a no-op.
    /// Call from within an event handler.
    pub fn open(&self, ev: &mut EventCx<'_>) {
        set_open(self, ev, true);
    }

    /// Close the sheet: slide the content back off-screen, then (when the slide
    /// finishes) hide it, release the focus trap, and send focus back where it was.
    /// Fires `on_dismiss` at close time. A close when already closed is a no-op.
    /// Call from within an event handler.
    pub fn close(&self, ev: &mut EventCx<'_>) {
        set_open(self, ev, false);
    }

    /// Toggle the sheet: close it if open, open it if closed. Call from within an
    /// event handler.
    pub fn toggle(&self, ev: &mut EventCx<'_>) {
        let next = !matches!(ev.get(self.open), Some(StateValue::Bool(true)));
        set_open(self, ev, next);
    }
}

/// A translucent black wash — the default scrim dimming the scene behind an open
/// sheet so the background reads as inert.
const DEFAULT_SCRIM: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.32,
};

/// The default fixed extent (height of a bottom/top drawer, width of a side panel)
/// along the slide axis when the app does not set one.
const DEFAULT_EXTENT: f32 = 320.0;

/// The default slide duration (both directions) — a brisk, natural drawer motion.
const DEFAULT_DURATION: Duration = Duration::from_millis(250);

/// The visual and layout parameters of a [`Sheet`].
///
/// `scrim` is the dimming color painted over the surface while open, under the
/// content; defaults to a translucent dark wash ([`Some`]) like a modal backdrop.
/// `edge` fixes the anchor and slide direction (default [`Bottom`](SheetEdge::Bottom)).
/// `extent` is the drawer's size along the slide axis (its height for a
/// bottom/top drawer, its width for a side panel). `duration` is the slide time.
/// All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SheetStyle {
    /// The dimming color painted over the surface while open, under the content.
    /// Defaults to a translucent dark wash; `None` paints no scrim.
    pub scrim: Option<Rgba>,
    /// The edge the drawer anchors to and slides in from. Defaults to `Bottom`.
    pub edge: SheetEdge,
    /// The drawer's fixed extent along the slide axis (height for bottom/top,
    /// width for a side panel). Defaults to [`DEFAULT_EXTENT`].
    pub extent: f32,
    /// The slide duration, both directions. Defaults to [`DEFAULT_DURATION`].
    pub duration: Duration,
}

impl Default for SheetStyle {
    fn default() -> Self {
        SheetStyle {
            scrim: Some(DEFAULT_SCRIM),
            edge: SheetEdge::default(),
            extent: DEFAULT_EXTENT,
            duration: DEFAULT_DURATION,
        }
    }
}

/// A panel that slides in from an edge of the surface and takes focus while open.
///
/// Construct one with [`sheet`], give it [`content`](Sheet::content), optionally an
/// [`edge`](Sheet::edge), [`extent`](Sheet::extent), [`duration`](Sheet::duration),
/// [`scrim`](Sheet::scrim), and an [`on_dismiss`](Sheet::on_dismiss), and capture a
/// [`SheetHandle`] with [`handle`](Sheet::handle) to open/close/toggle from
/// application code. Escape closes an open sheet. There is no anchor node: a sheet
/// covers the surface and is driven entirely through its handle.
///
/// See the [module docs](self) for a build example. Invalidation: opening/closing
/// writes a reactive `open` cell bound to the sheet's `PAINT`, defers a `hidden`
/// flip on the content, defers the focus move and focus-scope change, and requests
/// a transform-only slide (`TRANSFORM | HIT_TEST | PAINT`, never a relayout). No
/// rebuild: the content builds once.
pub struct Sheet {
    /// The drawer content builder (`None` authors no content — an empty drawer).
    content: Option<ContentBuilder>,
    style: SheetStyle,
    /// The shared dismiss callback (see [`SharedDismiss`]). `None` until
    /// [`Sheet::on_dismiss`] is called; a sheet with no callback still closes.
    on_dismiss: SharedDismiss,
    /// The app-supplied slot `build` fills with the control's [`SheetHandle`], or an
    /// unshared throwaway slot when the app did not ask for one.
    handle_slot: SheetHandleSlot,
}

/// Construct an empty [`Sheet`] with no content yet, a default dark scrim, a
/// bottom edge, and no callback. Chain [`Sheet::content`] to give it a drawer body,
/// [`Sheet::edge`] to choose the anchor, [`Sheet::extent`]/[`Sheet::duration`] to
/// size and time the slide, [`Sheet::scrim`] to change or drop the backdrop,
/// [`Sheet::on_dismiss`] to react to a close, and [`Sheet::handle`] to capture a
/// [`SheetHandle`].
pub fn sheet() -> Sheet {
    Sheet {
        content: None,
        style: SheetStyle::default(),
        on_dismiss: Rc::new(RefCell::new(None)),
        handle_slot: Rc::new(RefCell::new(None)),
    }
}

impl Sheet {
    /// Set the content builder: the drawer body shown while the sheet is open.
    /// Replaces any previously set content.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Set the edge the drawer anchors to and slides in from (default `Bottom`).
    pub fn edge(mut self, edge: SheetEdge) -> Self {
        self.style.edge = edge;
        self
    }

    /// Set the drawer's fixed extent along the slide axis (its height for a
    /// bottom/top drawer, its width for a side panel).
    pub fn extent(mut self, extent: f32) -> Self {
        self.style.extent = extent;
        self
    }

    /// Set the slide duration (both directions).
    pub fn duration(mut self, duration: Duration) -> Self {
        self.style.duration = duration;
        self
    }

    /// Set the dimming scrim color painted over the surface while open. Pass a fully
    /// transparent color to suppress the backdrop while keeping the sheet semantics.
    pub fn scrim(mut self, scrim: Rgba) -> Self {
        self.style.scrim = Some(scrim);
        self
    }

    /// Set the dismiss callback, fired when the sheet closes (Escape or a
    /// programmatic close/toggle-to-closed). Replaces any previously set handler.
    pub fn on_dismiss(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_dismiss.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`SheetStyle`].
    pub fn style(mut self, style: SheetStyle) -> Self {
        self.style = style;
        self
    }

    /// Register an app-supplied [`SheetHandleSlot`] for programmatic open/close.
    /// During [`build`](Component::build) the control fills the slot with a
    /// [`SheetHandle`] bound to the just-minted `open` cell, content id, edge, and
    /// extent; the app reads the slot after building and clones the handle into its
    /// trigger control. Storing the destination this way (rather than returning it)
    /// lets `build` fill it once the ids exist.
    pub fn handle(mut self, slot: &SheetHandleSlot) -> Self {
        self.handle_slot = slot.clone();
        self
    }
}

/// Drive the shared dismiss callback if one is set; a callback-less sheet is a
/// no-op. Dismissal is serial within one input transaction, so the borrow is
/// uncontended.
fn fire_dismiss(cb: &SharedDismiss, ev: &mut EventCx<'_>) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev);
    }
}

/// Set the sheet's open state to `next` — but only when it is a genuine change.
/// Opening when already open, or closing when already closed, is a no-op: no cell
/// write, no slide, no focus move, no callback. This also coalesces the
/// capture/bubble double-dispatch a router performs when Escape lands on a leaf
/// inside the focused content: the first pass writes `open` and starts everything;
/// the second reads the just-written value and short-circuits.
///
/// On **open**: snapshot the currently-focused node into `restore_focus`, show the
/// content, start the slide-in (off-screen → rest, `EaseOut`), install the focus
/// scope on the content (trapping Tab inside the drawer), and move focus into the
/// content.
///
/// On **close**: start the slide-out (rest → off-screen, `EaseIn`) whose completion
/// callback hides the content, releases the focus scope, and restores focus (so the
/// drawer is visible and focus-trapped for the whole slide-out, and the release only
/// lands once it has slid away); fire `on_dismiss` now.
///
/// The `hidden` flip, focus move, and focus-scope change on open are deferred (the
/// router applies them after the handler returns); the slide is deferred too (queued
/// on the store, drained by the driver into its registry next frame). On close, the
/// content stays shown and focus-trapped — the slide-out `on_done` (a store-only
/// callback) performs the hide/release/restore when the motion settles.
fn set_open(h: &SheetHandle, ev: &mut EventCx<'_>, next: bool) {
    let cur = matches!(ev.get(h.open), Some(StateValue::Bool(true)));
    if cur == next {
        return;
    }
    ev.set(h.open, StateValue::Bool(next));

    let offscreen = h.edge.offscreen(h.extent);
    let content = h.content;
    if next {
        // Remember where focus was so close can send it back, show the content, and
        // slide it in from off-screen to rest. Then trap focus in the drawer and
        // move focus into it.
        h.restore_focus.set(ev.focused());
        ev.set_hidden(content, false);
        ev.request_animation(TranslateAnim::new(
            content,
            offscreen,
            Vec2::ZERO,
            h.duration,
            Easing::EaseOut,
        ));
        ev.set_focus_scope(content);
        ev.request_focus(content);
    } else {
        // Slide out from rest back off-screen; only when the slide finishes hide the
        // content, release the focus trap, and restore focus. If the drawer is
        // re-opened mid-slide-out, `AnimationRegistry::start` replaces this animation
        // (the slide-in takes over immediately) and this `on_done` never fires — so
        // the content is not spuriously hidden. `on_dismiss` fires now, at close time.
        let restore = h.restore_focus.clone();
        ev.request_animation(
            TranslateAnim::new(content, Vec2::ZERO, offscreen, h.duration, Easing::EaseIn).on_done(
                move |store| {
                    store.set_hidden(content, true);
                    store.set_focus_scope(None);
                    store.set_focused(restore.take());
                },
            ),
        );
        fire_dismiss(&h.on_dismiss, ev);
    }
}

impl Component for Sheet {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One shared `open` cell holding the current open state. The sheet binds it
        // to its PAINT so an open/close repaints in one targeted invalidation, not a
        // rebuild. The content shows/hides and slides through deferred requests, not
        // this cell. A sheet starts closed.
        let open = cx.state(StateValue::Bool(false));

        let scrim_color = self.style.scrim;
        let edge = self.style.edge;
        let extent = self.style.extent;
        let duration = self.style.duration;
        let content_size = edge.content_size(extent);
        let dismiss_cb = self.on_dismiss.clone();
        let restore_focus: RestoreFocus = Rc::new(Cell::new(None));
        let mut content_id = None;

        // The sheet root: a stretched flex along the slide axis. The scrim fills the
        // whole surface under everything; a `Fill` spacer (for far-edge anchors)
        // pushes the content to the anchored edge, since the layout engine's `Align`
        // is cross-axis only. Both scrim and content are top-layer overlays that
        // start hidden (a closed sheet shows nothing). The scrim is authored first,
        // so the top-layer pass paints it under the content.
        let root = cx.flex(
            FlexStyle {
                axis: edge.axis(),
                gap: 0.0,
                padding: Inset::all(0.0),
                align: Align::Stretch,
                size: Size::fill(),
                style: BoxStyle::NONE,
            },
            |cx| {
                // The scrim: a dimming fill over the whole surface, under the content.
                // A top-layer overlay authored before the content, starting hidden.
                if let Some(color) = scrim_color {
                    let scrim = cx.leaf(viso_ui::LeafStyle {
                        size: Size::fill(),
                        style: BoxStyle::solid(color),
                    });
                    cx.set_overlay(scrim, true);
                    cx.set_hidden(scrim, true);
                }

                // A fill spacer that pushes the content to the far (main-end) edge —
                // Bottom/Trailing anchors. Top/Leading pin to main-start and need
                // none. The spacer never paints (no style) and is not an overlay, so
                // it stays out of the top-layer pass; it exists only to consume the
                // leftover main-axis space so the content sits against its edge.
                if edge.needs_spacer() {
                    cx.leaf(viso_ui::LeafStyle {
                        size: match edge.axis() {
                            Axis::Column => Size {
                                width: Length::Fill { weight: 1.0 },
                                height: Length::Fill { weight: 1.0 },
                            },
                            Axis::Row => Size {
                                width: Length::Fill { weight: 1.0 },
                                height: Length::Fill { weight: 1.0 },
                            },
                        },
                        style: BoxStyle::NONE,
                    });
                }

                // The drawer content: authored once, flagged overlay (top layer), and
                // hidden until opened. Its root is focusable (the focus scope's root
                // and the focus target on open), closes on Escape, and is a Dialog in
                // the semantics tree (a sheet is a dialog presentation — WAI-ARIA
                // groups edge drawers under dialog, so no new role).
                let content = cx.flex(
                    FlexStyle {
                        axis: edge.axis(),
                        gap: 0.0,
                        padding: Inset::all(0.0),
                        align: Align::Stretch,
                        size: content_size,
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

                // Escape closes the sheet from the keyboard, driving the same close
                // path (slide-out + deferred release + dismiss) as the handle.
                let key_cb = dismiss_cb.clone();
                let key_restore = restore_focus.clone();
                let content_node = content.id();
                cx.on_key(content, move |ev| {
                    let Some(k) = ev.key() else { return };
                    if !k.pressed || k.repeat {
                        return;
                    }
                    if matches!(k.key, Key::Escape) {
                        let h = SheetHandle {
                            open,
                            content: content_node,
                            edge,
                            extent,
                            duration,
                            restore_focus: key_restore.clone(),
                            on_dismiss: key_cb.clone(),
                        };
                        set_open(&h, ev, false);
                    }
                });

                content_id = Some(content.id());
            },
        );

        let content = content_id.expect("sheet authors its content node");
        cx.bind(open, root, DirtyClass::PAINT);
        cx.semantics(root, Semantics::role(Role::Group));

        // Fill the app-supplied handle slot (if any) with a handle bound to the
        // just-minted cell, content id, edge, extent, and restore-focus cell, so the
        // app can open/close programmatically.
        *self.handle_slot.borrow_mut() = Some(SheetHandle {
            open,
            content,
            edge,
            extent,
            duration,
            restore_focus,
            on_dismiss: self.on_dismiss.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{
        BindingTable, KeyEvent, Modifiers, NodeStore, PointerButtons, PointerEvent, PointerPhase,
        SemanticProjector, StateStore, TextEdits, TranslateAnim, VirtualLists,
    };

    /// The reactive stores a sheet build writes into, kept together so a test can
    /// build the control and then drive its handlers (and any captured
    /// [`SheetHandle`]) against the same state — the router discipline reproduced.
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
        /// `BuildCx::new` would panic) and return its root (the sheet container).
        fn build(&mut self, control: Sheet) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            control.build(&mut cx);
            cx.root().expect("sheet declares a root node")
        }

        /// Drive a `SheetHandle` action (open/close/toggle) as a router would: run it
        /// inside a throwaway pointer `EventCx`, take the deferred requests, and apply
        /// them to the store — including queuing any slide, exactly as the router's
        /// drain does. Returns the animations the dispatch requested so a test can
        /// inspect the slide vectors.
        fn drive(&mut self, act: impl FnOnce(&mut EventCx<'_>)) -> Vec<TranslateAnim> {
            let ev = read_pointer();
            let (hidden, focus, scope, anims) = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                cx.__set_focused(self.store.focused());
                act(&mut cx);
                (
                    cx.__take_hidden_requests(),
                    cx.__take_focus_request(),
                    cx.__take_focus_scope_request(),
                    cx.__take_animation_requests(),
                )
            };
            self.apply(hidden, focus, scope);
            anims
        }

        /// Feed a key sample to the focused node's key handler, restoring it after,
        /// then apply every deferred request it recorded (the router discipline).
        /// Returns the animations the handler requested.
        fn key(&mut self, node: NodeId, ev: KeyEvent) -> Vec<TranslateAnim> {
            let mut handler = self.store.take_key_handler(node).expect("key handler");
            let (hidden, focus, scope, anims) = {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                cx.__set_focused(self.store.focused());
                handler(&mut cx);
                (
                    cx.__take_hidden_requests(),
                    cx.__take_focus_request(),
                    cx.__take_focus_scope_request(),
                    cx.__take_animation_requests(),
                )
            };
            self.store.restore_key_handler(node, handler);
            self.apply(hidden, focus, scope);
            anims
        }

        /// Apply the deferred requests a dispatch recorded to the store, mirroring the
        /// router's drain order: hidden flips, then the focus move, then the scope.
        /// (Animations are returned by the caller rather than queued, so a test can
        /// drive the registry itself.)
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

    /// The shared open cell authored by the build. Sheet authors exactly one state
    /// cell (the shared `open` Bool), so a fresh `StateStore` allocating one `Bool`
    /// cell yields the very handle the build produced.
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

    use viso_ui::LeafStyle;

    /// A basic content sheet with an app-captured handle (the common shape: default
    /// dark scrim, bottom edge).
    fn basic(slot: &SheetHandleSlot) -> Sheet {
        sheet().content(leaf).handle(slot)
    }

    /// The content node of a built sheet: the last child of the root (after the
    /// scrim and, for far-edge anchors, the fill spacer).
    fn content_of(store: &NodeStore, root: NodeId) -> NodeId {
        *children(store, root)
            .last()
            .expect("sheet has a content node")
    }

    /// A default bottom sheet: scrim under a fill spacer under the content, all
    /// three children of the root; scrim and content are hidden top-layer overlays.
    #[test]
    fn bottom_sheet_pins_content_to_the_edge_with_a_spacer_and_hides_overlays() {
        let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));

        let kids = children(&rx.store, root);
        assert_eq!(
            kids.len(),
            3,
            "a scrim, a fill spacer (bottom anchor), and the content"
        );
        let (scrim, spacer, content) = (kids[0], kids[1], kids[2]);

        assert!(
            rx.store.is_overlay(scrim),
            "the scrim is a top-layer overlay"
        );
        assert!(rx.store.hidden(scrim), "the scrim starts hidden (closed)");
        assert!(
            !rx.store.is_overlay(spacer),
            "the spacer is not an overlay — it only consumes main-axis space"
        );
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
        assert_eq!(scrim_next, Some(spacer), "the scrim is authored first");
        let spacer_next = arena.links(spacer).and_then(|l| l.next_sibling);
        assert_eq!(
            spacer_next,
            Some(content),
            "content is authored last (over)"
        );
    }

    /// A top sheet pins to the main-start edge, so it needs no spacer: scrim + content.
    #[test]
    fn top_sheet_has_no_spacer() {
        let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(sheet().content(leaf).edge(SheetEdge::Top).handle(&slot));

        let kids = children(&rx.store, root);
        assert_eq!(
            kids.len(),
            2,
            "a top anchor pins to main-start, so no fill spacer"
        );
    }

    /// The sheet root is a `Group`; the content root is a focusable `Dialog`.
    #[test]
    fn sheet_root_is_group_and_content_is_a_focusable_dialog() {
        let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));

        assert_eq!(
            rx.store.semantics(root).expect("root has semantics").role,
            Role::Group,
            "the sheet is a Group container"
        );
        let content = content_of(&rx.store, root);
        assert_eq!(
            rx.store.semantics(content).expect("content semantics").role,
            Role::Dialog,
            "the content is a Dialog"
        );
        assert!(
            rx.store.focusable(content),
            "the content is focusable (focus target and scope root)"
        );
    }

    /// The off-screen `from` vector for each edge pushes the content fully past its
    /// anchored edge by one extent, in the right direction (world = bounds − translate).
    #[test]
    fn each_edge_slides_from_off_its_anchored_edge() {
        let e = 200.0;
        assert_eq!(
            SheetEdge::Bottom.offscreen(e),
            Vec2 { x: 0.0, y: -e },
            "bottom hides below (world +y → translate −y)"
        );
        assert_eq!(
            SheetEdge::Top.offscreen(e),
            Vec2 { x: 0.0, y: e },
            "top hides above (world −y → translate +y)"
        );
        assert_eq!(
            SheetEdge::Leading.offscreen(e),
            Vec2 { x: e, y: 0.0 },
            "leading hides off the left (world −x → translate +x)"
        );
        assert_eq!(
            SheetEdge::Trailing.offscreen(e),
            Vec2 { x: -e, y: 0.0 },
            "trailing hides off the right (world +x → translate −x)"
        );
    }

    /// Opening requests a slide-in from off-screen to rest, shows the content, traps
    /// focus, moves focus into the content, and flips the open cell.
    #[test]
    fn open_requests_slide_in_and_traps_focus() {
        let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));
        let content = content_of(&rx.store, root);
        let handle = slot.borrow().clone().expect("handle filled by build");
        let cell = open_cell();

        let anims = rx.drive(|ev| handle.open(ev));

        assert_eq!(rx.is_open(cell), Some(true), "open flips the cell");
        assert!(!rx.store.hidden(content), "the content shows to slide in");
        assert_eq!(
            rx.store.focus_scope(),
            Some(content),
            "focus is trapped in the content"
        );
        assert_eq!(
            rx.store.focused(),
            Some(content),
            "focus moves into the content"
        );
        assert_eq!(anims.len(), 1, "one slide requested");
        let anim = &anims[0];
        assert_eq!(anim.node, content, "the slide targets the content");
        assert_eq!(
            anim.from,
            SheetEdge::Bottom.offscreen(DEFAULT_EXTENT),
            "slides in from off the bottom edge"
        );
        assert_eq!(anim.to, Vec2::ZERO, "slides to rest");
    }

    /// Closing requests a slide-out (reverse) and fires on_dismiss; the content stays
    /// shown and focus-trapped until the slide's completion callback runs (so the
    /// deferred requests carry no hide/release — those live in `on_done`).
    #[test]
    fn close_requests_slide_out_and_defers_the_hide_to_completion() {
        let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
        let dismissed = Rc::new(Cell::new(0u32));
        let d = dismissed.clone();
        let mut rx = Reactive::new();
        let root = rx.build(
            sheet()
                .content(leaf)
                .on_dismiss(move |_| d.set(d.get() + 1))
                .handle(&slot),
        );
        let content = content_of(&rx.store, root);
        let handle = slot.borrow().clone().expect("handle filled by build");

        rx.drive(|ev| handle.open(ev));
        // Simulate the slide-in landing (registry would have unhidden already; the
        // content is shown, focus trapped).
        assert!(!rx.store.hidden(content));

        let anims = rx.drive(|ev| handle.close(ev));

        assert_eq!(dismissed.get(), 1, "on_dismiss fires once at close time");
        assert!(
            !rx.store.hidden(content),
            "the content stays shown during the slide-out"
        );
        assert_eq!(
            rx.store.focus_scope(),
            Some(content),
            "focus stays trapped during the slide-out"
        );
        assert_eq!(anims.len(), 1, "one slide-out requested");
        assert_eq!(anims[0].from, Vec2::ZERO, "slides out from rest");
        assert_eq!(
            anims[0].to,
            SheetEdge::Bottom.offscreen(DEFAULT_EXTENT),
            "slides out to off-screen"
        );

        // Run the slide-out to completion through a registry, exactly as the driver
        // does: the on_done hides the content, releases the scope, and restores focus.
        let mut reg = viso_ui::AnimationRegistry::new();
        for anim in anims {
            reg.start(anim);
        }
        // A tick past the duration lands the slide and fires on_done.
        reg.tick(&mut rx.store, DEFAULT_DURATION + Duration::from_millis(1));
        assert!(reg.is_empty(), "the slide-out finished");
        assert!(
            rx.store.hidden(content),
            "the completion callback hides the content"
        );
        assert_eq!(
            rx.store.focus_scope(),
            None,
            "the completion callback releases the focus trap"
        );
    }

    /// Escape on the focused content closes the sheet (slide-out + dismiss).
    #[test]
    fn escape_closes_the_sheet() {
        let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
        let dismissed = Rc::new(Cell::new(0u32));
        let d = dismissed.clone();
        let mut rx = Reactive::new();
        let root = rx.build(
            sheet()
                .content(leaf)
                .on_dismiss(move |_| d.set(d.get() + 1))
                .handle(&slot),
        );
        let content = content_of(&rx.store, root);
        let handle = slot.borrow().clone().expect("handle filled by build");
        let cell = open_cell();

        rx.drive(|ev| handle.open(ev));
        assert_eq!(rx.is_open(cell), Some(true));

        let anims = rx.key(content, key_ev(Key::Escape, true, false));
        assert_eq!(rx.is_open(cell), Some(false), "Escape closes");
        assert_eq!(dismissed.get(), 1, "Escape fires on_dismiss");
        assert_eq!(anims.len(), 1, "Escape starts the slide-out");
        assert_eq!(anims[0].to, SheetEdge::Bottom.offscreen(DEFAULT_EXTENT));
    }

    /// Re-opening mid-slide-out replaces the slide-out with a slide-in on the same
    /// node, so the replaced slide-out's `on_done` never fires (the content is not
    /// spuriously hidden). This is the `AnimationRegistry::start` replace contract,
    /// exercised through the sheet's real open/close requests.
    #[test]
    fn reopening_mid_slide_out_cancels_the_hide() {
        let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(basic(&slot));
        let content = content_of(&rx.store, root);
        let handle = slot.borrow().clone().expect("handle filled by build");

        rx.drive(|ev| handle.open(ev));
        let out = rx.drive(|ev| handle.close(ev));

        let mut reg = viso_ui::AnimationRegistry::new();
        for anim in out {
            reg.start(anim);
        }
        // Advance the slide-out partway (not to completion).
        reg.tick(&mut rx.store, DEFAULT_DURATION / 2);
        assert!(!rx.store.hidden(content), "still sliding out, still shown");

        // Re-open mid-slide-out: the slide-in replaces the slide-out on the content.
        let back = rx.drive(|ev| handle.open(ev));
        for anim in back {
            reg.start(anim);
        }
        assert_eq!(
            reg.len(),
            1,
            "same node — the slide-in replaced the slide-out"
        );

        // Run to completion: the surviving slide-in has no hide callback, so the
        // content stays shown (the cancelled slide-out's on_done never ran).
        reg.tick(&mut rx.store, DEFAULT_DURATION + Duration::from_millis(1));
        assert!(reg.is_empty());
        assert!(
            !rx.store.hidden(content),
            "the cancelled slide-out never hid the content"
        );
    }

    /// A repeated open is a no-op: no second slide, no re-trap.
    #[test]
    fn opening_twice_is_a_no_op() {
        let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        rx.build(basic(&slot));
        let handle = slot.borrow().clone().expect("handle filled by build");

        let first = rx.drive(|ev| handle.open(ev));
        assert_eq!(first.len(), 1, "the first open slides in");
        let second = rx.drive(|ev| handle.open(ev));
        assert!(second.is_empty(), "a second open requests nothing");
    }

    /// Toggle flips open→closed→open, each flip requesting one slide.
    #[test]
    fn toggle_flips_and_slides_each_way() {
        let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        rx.build(basic(&slot));
        let handle = slot.borrow().clone().expect("handle filled by build");
        let cell = open_cell();

        let a = rx.drive(|ev| handle.toggle(ev));
        assert_eq!(rx.is_open(cell), Some(true), "toggle opens");
        assert_eq!(a[0].to, Vec2::ZERO, "toggle-open slides in to rest");

        let b = rx.drive(|ev| handle.toggle(ev));
        assert_eq!(rx.is_open(cell), Some(false), "toggle closes");
        assert_eq!(
            b[0].to,
            SheetEdge::Bottom.offscreen(DEFAULT_EXTENT),
            "toggle-close slides out"
        );
    }
}
