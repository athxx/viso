//! The [`Toast`] control — a transient notification that appears at an edge of
//! the surface, announces itself politely, and auto-dismisses after a fixed
//! delay without ever taking focus.
//!
//! A `Toast` is the *non-modal* sibling of [`Modal`](crate::Modal) and
//! [`Sheet`](crate::Sheet): it composes a small floating *content* panel over the
//! surface, but strips away everything that makes a dialog modal —
//!
//! 1. **No scrim.** A toast does not dim the scene: the app stays fully usable
//!    while it shows. There is no backdrop at all.
//! 2. **No focus trap, no focus steal.** A modal snapshots focus, moves focus
//!    into the dialog, and traps Tab inside it; a toast does none of this. Its
//!    content is *not* focusable and installs no focus scope, so keyboard
//!    navigation is unaffected — the user keeps working while it shows. This is
//!    the defining non-modal property (WAI-ARIA `role=status`, a *polite* live
//!    region an assistive technology reads without moving focus).
//! 3. **Auto-dismiss.** Opening a modal leaves it open until something closes it;
//!    showing a toast arms a one-shot [timer](viso_ui::TimerRegistry) that hides
//!    the content once `duration` elapses. The timer costs no frame while it
//!    waits (the scheduler blocks on its deadline — section 7.1), so a shown
//!    toast is zero-CPU until it auto-dismisses.
//!
//! Like [`Modal`], the content builds once and is shown/hidden through the
//! retained [`hidden`](viso_ui::NodeStore::hidden) flag. It pins to an edge
//! ([`ToastEdge`], default [`Bottom`](ToastEdge::Bottom)) with a `Fit` size,
//! centered on the cross axis — a small notification against an edge, not a
//! full-bleed drawer. The edge-pin reuses the sheet's mechanism: a `Fill` spacer
//! sibling pushes the content to a far (main-end) edge, since the layout
//! engine's [`Align`](viso_ui::Align) is cross-axis only.
//!
//! ## Show, auto-dismiss, and re-show
//!
//! A [`ToastHandle`] drives the toast: [`show`](ToastHandle::show) un-hides the
//! content and arms the auto-dismiss timer through the deferred
//! [`request_timer`](viso_ui::EventCx::request_timer) seam (recorded on the
//! [`EventCx`], armed by the driver next frame against that frame's clock, so the
//! deadline is deterministic under a headless clock). Because a timer's fire
//! callback receives only `&mut NodeStore` (it cannot itself schedule — an
//! auto-dismiss cannot re-enter an [`EventCx`]), the fire *hides the content*;
//! the optional `on_dismiss` fires only on a **manual**
//! [`dismiss`](ToastHandle::dismiss) (which has an `EventCx`), the same
//! discipline the [`Sheet`](crate::Sheet)'s store-only slide-out completion
//! follows.
//!
//! Re-showing before the previous toast auto-dismisses, or dismissing early,
//! must not let a stale timer hide a freshly-shown toast. Each show snapshots a
//! monotonic *epoch* (a shared [`Cell`]); the timer's fire hides the content only
//! if the epoch still matches the show that armed it. A manual dismiss and a
//! re-show both bump the epoch, so a pending timer's fire becomes a no-op — no
//! per-timer cancellation bookkeeping, just a single integer compare in the fire.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use viso_widgets::{ToastEdge, ToastHandleSlot, toast};
//! use viso_ui::{BuildCx, BindingTable, Component, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Size};
//!
//! let handle: ToastHandleSlot = Rc::new(RefCell::new(None));
//! let control = toast()
//!     .edge(ToastEdge::Bottom)
//!     .content(|cx| { cx.leaf(LeafStyle { size: Size::fixed(240.0, 48.0), style: BoxStyle::NONE }); })
//!     .on_dismiss(|_ev| { /* the toast was manually dismissed */ })
//!     .handle(&handle);
//!
//! // Toast authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits);
//! control.build(&mut cx);
//! // `handle` is now filled; the app can `handle.borrow().clone().unwrap().show(ev)` from anywhere.
//! ```

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset, Length,
    NodeId, Role, Semantics, Size, StateId, StateValue,
};

/// A shared, mutable dismiss callback, fired when the toast is **manually**
/// dismissed (a programmatic [`ToastHandle::dismiss`], which runs inside an
/// [`EventCx`]). An auto-dismiss timer fires with only `&mut NodeStore` and so
/// cannot drive it — the same store-only discipline the
/// [`Sheet`](crate::Sheet)'s slide-out completion follows. Dismissal is never
/// concurrent (serial within one input transaction), so the runtime never
/// re-enters the borrow.
type SharedDismiss = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>)>>>>;

/// A build-time content builder. It authors the notification content subtree
/// into the toast; boxed so a `Toast` can hold the closure.
type ContentBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// The show *epoch*: a monotonic counter bumped on every show and on every
/// manual dismiss. An auto-dismiss timer captures the epoch it was armed under
/// and hides the content only if the epoch still matches — so a re-show or an
/// early dismiss makes a still-pending timer's fire a no-op. Shared between the
/// [`ToastHandle`] (which bumps it) and each armed timer (which reads it),
/// `Copy`-cheap and allocation-free to check.
type Epoch = Rc<Cell<u64>>;

/// A shared slot an application creates, passes to [`Toast::handle`], and reads
/// after `build` to obtain the control's [`ToastHandle`]. The `open` cell and
/// the content node id are minted inside `build`, so the handle cannot be
/// returned by the builder chain; the app supplies this slot up front and
/// `build` fills it once the ids exist — the same deferred-fill idiom as
/// [`Modal`](crate::Modal).
pub type ToastHandleSlot = Rc<RefCell<Option<ToastHandle>>>;

/// The edge a [`Toast`] anchors to.
///
/// The edge fixes which axis the notification pins along and which side its
/// content sits against. A [`Bottom`](Self::Bottom) or [`Top`](Self::Top) toast
/// pins to that horizontal edge, centered across the width; a
/// [`Leading`](Self::Leading) or [`Trailing`](Self::Trailing) toast pins to that
/// vertical edge, centered down the height. Defaults to
/// [`Bottom`](Self::Bottom) — the common toast position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToastEdge {
    /// Pinned to the bottom edge, centered horizontally. The default toast spot.
    #[default]
    Bottom,
    /// Pinned to the top edge, centered horizontally.
    Top,
    /// Pinned to the leading (left) edge, centered vertically.
    Leading,
    /// Pinned to the trailing (right) edge, centered vertically.
    Trailing,
}

impl ToastEdge {
    /// The main axis of the toast root: the axis the content pins along.
    /// Bottom/Top stack vertically (Column); Leading/Trailing stack horizontally
    /// (Row). The cross axis is where the content centers.
    fn axis(self) -> Axis {
        match self {
            ToastEdge::Bottom | ToastEdge::Top => Axis::Column,
            ToastEdge::Leading | ToastEdge::Trailing => Axis::Row,
        }
    }

    /// Whether the content pins to the *far* (main-end) edge and so needs a
    /// leading fill spacer to push it there. A flex places its child at
    /// main-start, so Top/Leading (main-start edges) need no spacer, while
    /// Bottom/Trailing (main-end edges) do — the same edge-anchor mechanism the
    /// [`Sheet`](crate::Sheet) uses, since [`Align`](viso_ui::Align) is
    /// cross-axis only.
    fn needs_spacer(self) -> bool {
        matches!(self, ToastEdge::Bottom | ToastEdge::Trailing)
    }
}

/// A handle an application captures to drive a built [`Toast`] programmatically.
/// Cheap to clone (it holds only ids, the duration, and shared cells). Call
/// [`ToastHandle::show`] to reveal the toast and arm its auto-dismiss, and
/// [`ToastHandle::dismiss`] to hide it early — both from within an [`EventCx`],
/// since each defers a `hidden` flip (and `show` a timer arm) the router applies.
#[derive(Clone)]
pub struct ToastHandle {
    /// The reactive cell holding the current shown state.
    open: StateId,
    /// The notification content node, shown/hidden to reveal/dismiss.
    content: NodeId,
    /// How long a shown toast waits before it auto-dismisses.
    duration: Duration,
    /// The show epoch, bumped on every show and every manual dismiss so a stale
    /// auto-dismiss timer's fire is ignored.
    epoch: Epoch,
    /// The shared `on_dismiss` callback, fired on a manual dismiss.
    on_dismiss: SharedDismiss,
}

impl ToastHandle {
    /// Show the toast: reveal the content and arm the auto-dismiss timer. Showing
    /// again while already shown resets the timer (the fresh show bumps the epoch,
    /// so the earlier timer's fire is ignored, and arms a new one). Call from
    /// within an event handler.
    pub fn show(&self, ev: &mut EventCx<'_>) {
        // A new show generation: any timer armed by an earlier show now fires into
        // a stale epoch and is ignored, so re-showing resets the auto-dismiss.
        let epoch = self.epoch.get().wrapping_add(1);
        self.epoch.set(epoch);

        ev.set(self.open, StateValue::Bool(true));
        ev.set_hidden(self.content, false);

        // Arm the auto-dismiss. The fire callback gets only the store, so it hides
        // the content directly (it cannot re-enter an EventCx to run `on_dismiss` —
        // an auto-dismiss is silent, like the Sheet's store-only slide-out
        // completion). The epoch guard keeps a stale timer from hiding a re-shown
        // toast.
        let content = self.content;
        let guard = self.epoch.clone();
        ev.request_timer(content, self.duration, move |store| {
            if guard.get() == epoch {
                store.set_hidden(content, true);
            }
        });
    }

    /// Dismiss the toast early: hide the content now and fire `on_dismiss`. Bumps
    /// the epoch so a still-pending auto-dismiss timer's fire is ignored. A
    /// dismiss when already hidden still fires `on_dismiss` only if it was shown
    /// (a no-op dismiss of a hidden toast does nothing). Call from within an event
    /// handler.
    pub fn dismiss(&self, ev: &mut EventCx<'_>) {
        // Only a shown toast dismisses (and fires the callback); dismissing a
        // hidden toast is a no-op, matching the modal's open-state guard.
        let shown = matches!(ev.get(self.open), Some(StateValue::Bool(true)));
        if !shown {
            return;
        }
        // Bump the epoch so the pending auto-dismiss timer's fire is ignored — the
        // manual dismiss wins.
        self.epoch.set(self.epoch.get().wrapping_add(1));
        ev.set(self.open, StateValue::Bool(false));
        ev.set_hidden(self.content, true);
        fire_dismiss(&self.on_dismiss, ev);
    }
}

/// The default duration a shown toast waits before auto-dismissing — a common
/// notification dwell time, long enough to read a short message.
const DEFAULT_DURATION: Duration = Duration::from_millis(4000);

/// The visual and layout parameters of a [`Toast`].
///
/// `edge` fixes the anchor (default [`Bottom`](ToastEdge::Bottom)). `duration` is
/// how long a shown toast waits before auto-dismissing. Both fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToastStyle {
    /// The edge the notification pins to. Defaults to `Bottom`.
    pub edge: ToastEdge,
    /// How long a shown toast waits before auto-dismissing. Defaults to
    /// [`DEFAULT_DURATION`].
    pub duration: Duration,
}

impl Default for ToastStyle {
    fn default() -> Self {
        ToastStyle {
            edge: ToastEdge::default(),
            duration: DEFAULT_DURATION,
        }
    }
}

/// A transient notification that appears at an edge, announces itself politely,
/// and auto-dismisses after a fixed delay without taking focus.
///
/// Construct one with [`toast`], give it [`content`](Toast::content), optionally
/// an [`edge`](Toast::edge), a [`duration`](Toast::duration), and an
/// [`on_dismiss`](Toast::on_dismiss), and capture a [`ToastHandle`] with
/// [`handle`](Toast::handle) to show/dismiss from application code. There is no
/// anchor, no scrim, and no focus interaction: a toast pins to an edge and is
/// driven entirely through its handle.
///
/// See the [module docs](self) for a build example. Invalidation: showing writes
/// a reactive `open` cell bound to the toast's `PAINT`, defers a `hidden` flip on
/// the content the router applies (marking it `LAYOUT | PAINT`), and defers a
/// one-shot timer arm. No rebuild: the content builds once.
pub struct Toast {
    /// The notification content builder (`None` authors no content — an empty
    /// toast).
    content: Option<ContentBuilder>,
    style: ToastStyle,
    /// The shared dismiss callback (see [`SharedDismiss`]). `None` until
    /// [`Toast::on_dismiss`] is called; a toast with no callback still dismisses.
    on_dismiss: SharedDismiss,
    /// The app-supplied slot `build` fills with the control's [`ToastHandle`], or
    /// an unshared throwaway slot when the app did not ask for one.
    handle_slot: ToastHandleSlot,
}

/// Construct an empty [`Toast`] with no content yet, a bottom edge, the default
/// duration, and no callback. Chain [`Toast::content`] to give it a body,
/// [`Toast::edge`] to choose the anchor, [`Toast::duration`] to set the dwell
/// time, [`Toast::on_dismiss`] to react to a manual dismiss, and
/// [`Toast::handle`] to capture a [`ToastHandle`].
pub fn toast() -> Toast {
    Toast {
        content: None,
        style: ToastStyle::default(),
        on_dismiss: Rc::new(RefCell::new(None)),
        handle_slot: Rc::new(RefCell::new(None)),
    }
}

impl Toast {
    /// Set the content builder: the notification body shown while the toast is up.
    /// Replaces any previously set content.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Set the edge the toast pins to (defaults to `Bottom`).
    pub fn edge(mut self, edge: ToastEdge) -> Self {
        self.style.edge = edge;
        self
    }

    /// Set how long a shown toast waits before auto-dismissing (defaults to
    /// [`DEFAULT_DURATION`]).
    pub fn duration(mut self, duration: Duration) -> Self {
        self.style.duration = duration;
        self
    }

    /// Set the dismiss callback, fired on a manual [`ToastHandle::dismiss`] (an
    /// auto-dismiss is silent). Replaces any previously set handler.
    pub fn on_dismiss(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_dismiss.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`ToastStyle`].
    pub fn style(mut self, style: ToastStyle) -> Self {
        self.style = style;
        self
    }

    /// Register an app-supplied [`ToastHandleSlot`] for programmatic
    /// show/dismiss. During [`build`](Component::build) the control fills the slot
    /// with a [`ToastHandle`] bound to the just-minted `open` cell, content id,
    /// and epoch; the app reads the slot after building and clones the handle into
    /// wherever it triggers notifications.
    pub fn handle(mut self, slot: &ToastHandleSlot) -> Self {
        self.handle_slot = slot.clone();
        self
    }
}

/// Drive the shared dismiss callback if one is set; a callback-less toast is a
/// no-op. Dismissal is serial within one input transaction, so the borrow is
/// uncontended.
fn fire_dismiss(cb: &SharedDismiss, ev: &mut EventCx<'_>) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev);
    }
}

impl Component for Toast {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One shared `open` cell holding the current shown state, bound to the
        // toast's PAINT so a show/dismiss repaints in one targeted invalidation,
        // not a rebuild. The content switches through a deferred `hidden` flip, not
        // this cell. A toast starts hidden.
        let open = cx.state(StateValue::Bool(false));

        let edge = self.style.edge;
        let epoch: Epoch = Rc::new(Cell::new(0));
        let mut content_id = None;

        // The toast root: an edge-pinning flex over the whole surface. The content
        // sits at the anchored edge (a `Fill` spacer pushes it to a far edge) and
        // centers on the cross axis — a small notification against an edge, not a
        // full-bleed panel. There is no scrim: the app stays usable while it shows.
        let root = cx.flex(
            FlexStyle {
                axis: edge.axis(),
                gap: 0.0,
                padding: Inset::all(0.0),
                // Cross-axis center: the notification is centered along the edge it
                // pins to (a bottom toast is centered horizontally).
                align: Align::Center,
                size: Size::fill(),
                style: BoxStyle::NONE,
            },
            |cx| {
                // A fill spacer that pushes the content to the far (main-end) edge —
                // Bottom/Trailing anchors. Top/Leading pin to main-start and need
                // none. The spacer never paints (no style) and is not an overlay, so
                // it stays out of the top-layer pass; it exists only to consume the
                // leftover main-axis space so the content sits against its edge.
                if edge.needs_spacer() {
                    cx.leaf(viso_ui::LeafStyle {
                        size: Size {
                            width: Length::Fill { weight: 1.0 },
                            height: Length::Fill { weight: 1.0 },
                        },
                        style: BoxStyle::NONE,
                    });
                }

                // The notification content: authored once, flagged overlay (top
                // layer), and hidden until shown. Sized to fit its body (a small
                // notification, not a full-bleed drawer). Unlike a modal or sheet it
                // is NOT focusable and installs no focus scope — a toast never steals
                // the keyboard — and is a `Status` (a polite live region) in the
                // semantics tree.
                let content = cx.flex(
                    FlexStyle {
                        axis: edge.axis(),
                        gap: 0.0,
                        padding: Inset::all(0.0),
                        align: Align::Center,
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
                cx.semantics(content, Semantics::role(Role::Status));

                content_id = Some(content.id());
            },
        );

        let content = content_id.expect("toast authors its content node");
        cx.bind(open, root, DirtyClass::PAINT);
        cx.semantics(root, Semantics::role(Role::Group));

        // Fill the app-supplied handle slot (if any) with a handle bound to the
        // just-minted cell, content id, duration, and epoch, so the app can
        // show/dismiss programmatically.
        *self.handle_slot.borrow_mut() = Some(ToastHandle {
            open,
            content,
            duration: self.style.duration,
            epoch,
            on_dismiss: self.on_dismiss.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use viso_ui::{
        BindingTable, LeafStyle, Modifiers, NodeStore, PointerButtons, PointerEvent, PointerPhase,
        Rect, StateStore, TextEdits, TimerRegistry, TimerRequest, VirtualLists,
    };

    /// The reactive stores a toast build writes into, kept together so a test can
    /// build the control and then drive its handlers (and the captured
    /// [`ToastHandle`]) against the same state — plus a live [`TimerRegistry`] so
    /// the deferred timer arms a show records are exercised end to end (arm on a
    /// frame, then `fire_due` past the deadline), the way the driver does.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        text_edits: TextEdits,
        timers: TimerRegistry,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
                text_edits: TextEdits::new(),
                timers: TimerRegistry::new(),
            }
        }

        /// Build a control through a reactive cx (it authors state, so a plain
        /// `BuildCx::new` would panic) and return its root (the toast container).
        fn build(&mut self, control: Toast) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
            );
            control.build(&mut cx);
            let root = cx.root().expect("toast declares a root node");
            // Lay it out so the content node is live (a timer scopes to a live node)
            // and any world boxes exist.
            let mut scratch = Vec::new();
            self.store.layout(
                root,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 800.0,
                    h: 600.0,
                },
                &mut scratch,
            );
            root
        }

        /// Drive a `ToastHandle` action (show/dismiss) as a router would: run it
        /// inside a throwaway pointer `EventCx`, take the deferred requests (hidden
        /// flips and any timer arms), apply the hidden flips to the store, and arm
        /// the timer requests on the live registry against `now` — the driver's
        /// FlushStateTransactions discipline reproduced for the test.
        fn drive(&mut self, now: std::time::Instant, act: impl FnOnce(&mut EventCx<'_>)) {
            let ev = read_pointer();
            let (hidden, timer_reqs) = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                cx.__set_focused(self.store.focused());
                act(&mut cx);
                (cx.__take_hidden_requests(), cx.__take_timer_requests())
            };
            for (id, h) in hidden {
                self.store.set_hidden(id, h);
            }
            for req in timer_reqs {
                self.timers.arm_request(req, now);
            }
        }

        /// Fire every timer due at `now`, exactly as the driver does at a frame
        /// head — an auto-dismiss's `on_fire` runs here against the live store.
        fn fire_due(&mut self, now: std::time::Instant) {
            self.timers.fire_due(&mut self.store, now);
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

    /// A neutral pointer sample for handle-driven cx construction.
    fn read_pointer() -> PointerEvent {
        PointerEvent {
            x: 0.0,
            y: 0.0,
            buttons: PointerButtons::NONE,
            modifiers: Modifiers::default(),
            phase: PointerPhase::Move,
        }
    }

    /// A toast with a fixed 100x40 content leaf and a captured handle, built into
    /// `r`. Returns the handle and the content node id.
    fn built_toast(r: &mut Reactive, duration: Duration) -> (ToastHandle, NodeId) {
        let slot: ToastHandleSlot = Rc::new(RefCell::new(None));
        let control = toast()
            .duration(duration)
            .content(|cx| {
                cx.leaf(LeafStyle {
                    size: Size::fixed(100.0, 40.0),
                    style: BoxStyle::NONE,
                });
            })
            .handle(&slot);
        r.build(control);
        let handle = slot.borrow().clone().expect("build filled the handle slot");
        let content = handle.content;
        (handle, content)
    }

    #[test]
    fn a_toast_builds_hidden_with_a_status_content() {
        let mut r = Reactive::new();
        let (handle, content) = built_toast(&mut r, Duration::from_millis(4000));

        // The content is authored once and starts hidden (a toast shows nothing
        // until `show`), and it starts closed.
        assert!(r.store.hidden(content), "the content starts hidden");
        assert_eq!(r.is_open(handle.open), Some(false), "a toast starts closed");

        // The content is a Status (a polite live region), not a Dialog: a toast
        // announces without taking focus.
        assert_eq!(
            r.store
                .semantics(content)
                .expect("content has authored semantics")
                .role,
            Role::Status,
            "the toast content is a Status role"
        );
    }

    #[test]
    fn the_content_is_not_focusable() {
        // The defining non-modal property: a toast never steals the keyboard, so
        // its content is not focusable (a modal/sheet content is). This is what
        // lets the user keep working while a toast shows.
        let mut r = Reactive::new();
        let (_handle, content) = built_toast(&mut r, Duration::from_millis(4000));
        assert!(
            !r.store.focusable(content),
            "toast content is not focusable (it does not steal focus)"
        );
    }

    #[test]
    fn show_reveals_the_content_and_arms_the_auto_dismiss() {
        let mut r = Reactive::new();
        let dur = Duration::from_millis(4000);
        let (handle, content) = built_toast(&mut r, dur);

        let t0 = std::time::Instant::now();
        let h = handle.clone();
        r.drive(t0, move |ev| h.show(ev));

        // Showing reveals the content, marks it open, and armed exactly one timer.
        assert!(!r.store.hidden(content), "show reveals the content");
        assert_eq!(r.is_open(handle.open), Some(true), "show marks it open");
        assert_eq!(r.timers.len(), 1, "show armed one auto-dismiss timer");
        assert_eq!(
            r.timers.earliest(),
            Some(t0 + dur),
            "the auto-dismiss deadline is the show frame's now + duration"
        );
    }

    #[test]
    fn the_auto_dismiss_hides_the_content_when_the_timer_fires() {
        let mut r = Reactive::new();
        let dur = Duration::from_millis(4000);
        let (handle, content) = built_toast(&mut r, dur);

        let t0 = std::time::Instant::now();
        let h = handle.clone();
        r.drive(t0, move |ev| h.show(ev));
        assert!(!r.store.hidden(content), "shown before the deadline");

        // Before the deadline: still shown, timer still pending.
        r.fire_due(t0 + Duration::from_millis(2000));
        assert!(!r.store.hidden(content), "still shown before the deadline");
        assert_eq!(r.timers.len(), 1, "timer still pending before the deadline");

        // Cross the deadline: the auto-dismiss fires once and hides the content,
        // and the registry empties (the loop is free to idle).
        r.fire_due(t0 + dur);
        assert!(r.store.hidden(content), "the auto-dismiss hid the content");
        assert!(
            r.timers.is_empty(),
            "the one-shot timer fired and was removed"
        );
    }

    #[test]
    fn a_manual_dismiss_hides_early_and_fires_on_dismiss_once() {
        let mut r = Reactive::new();
        let dur = Duration::from_millis(4000);
        let dismissed = Rc::new(Cell::new(0u32));

        let slot: ToastHandleSlot = Rc::new(RefCell::new(None));
        let tally = Rc::clone(&dismissed);
        let control = toast()
            .duration(dur)
            .content(|cx| {
                cx.leaf(LeafStyle {
                    size: Size::fixed(100.0, 40.0),
                    style: BoxStyle::NONE,
                });
            })
            .on_dismiss(move |_ev| tally.set(tally.get() + 1))
            .handle(&slot);
        r.build(control);
        let handle = slot.borrow().clone().expect("build filled the handle slot");
        let content = handle.content;

        let t0 = std::time::Instant::now();
        let h = handle.clone();
        r.drive(t0, move |ev| h.show(ev));
        assert!(!r.store.hidden(content), "shown");

        // Dismiss early — well before the 4s deadline.
        let h = handle.clone();
        r.drive(t0 + Duration::from_millis(500), move |ev| h.dismiss(ev));
        assert!(r.store.hidden(content), "manual dismiss hides the content");
        assert_eq!(
            r.is_open(handle.open),
            Some(false),
            "dismiss marks it closed"
        );
        assert_eq!(
            dismissed.get(),
            1,
            "on_dismiss fired once on manual dismiss"
        );

        // The stale auto-dismiss timer still fires at its deadline, but the epoch
        // guard makes it a no-op — the content stays hidden, on_dismiss stays at 1.
        r.fire_due(t0 + dur);
        assert!(r.store.hidden(content), "content stays hidden");
        assert_eq!(
            dismissed.get(),
            1,
            "a stale auto-dismiss timer fires no callback"
        );
    }

    #[test]
    fn re_showing_resets_the_timer_and_a_stale_fire_is_ignored() {
        let mut r = Reactive::new();
        let dur = Duration::from_millis(4000);
        let (handle, content) = built_toast(&mut r, dur);

        let t0 = std::time::Instant::now();
        let h = handle.clone();
        r.drive(t0, move |ev| h.show(ev));

        // Re-show at t0 + 3s (still before the first deadline at t0 + 4s). The
        // second show bumps the epoch and arms a fresh timer at (t0 + 3s) + 4s.
        let t1 = t0 + Duration::from_millis(3000);
        let h = handle.clone();
        r.drive(t1, move |ev| h.show(ev));
        assert_eq!(r.timers.len(), 2, "two timers are now armed (old + new)");

        // The first (stale) timer fires at its deadline t0 + 4s: the epoch no longer
        // matches, so it does NOT hide the freshly-shown toast.
        r.fire_due(t0 + dur);
        assert!(
            !r.store.hidden(content),
            "a stale timer does not hide the re-shown toast"
        );
        assert_eq!(r.timers.len(), 1, "the stale timer fired and was removed");

        // The fresh timer fires at t1 + 4s: its epoch matches, so it hides.
        r.fire_due(t1 + dur);
        assert!(r.store.hidden(content), "the fresh timer hides the toast");
        assert!(r.timers.is_empty());
    }

    #[test]
    fn dismissing_a_hidden_toast_is_a_noop() {
        let mut r = Reactive::new();
        let dismissed = Rc::new(Cell::new(0u32));

        let slot: ToastHandleSlot = Rc::new(RefCell::new(None));
        let tally = Rc::clone(&dismissed);
        let control = toast()
            .content(|cx| {
                cx.leaf(LeafStyle {
                    size: Size::fixed(100.0, 40.0),
                    style: BoxStyle::NONE,
                });
            })
            .on_dismiss(move |_ev| tally.set(tally.get() + 1))
            .handle(&slot);
        r.build(control);
        let handle = slot.borrow().clone().expect("build filled the handle slot");

        // Dismissing a never-shown toast does nothing and fires no callback.
        let t0 = std::time::Instant::now();
        let h = handle.clone();
        r.drive(t0, move |ev| h.dismiss(ev));
        assert_eq!(r.is_open(handle.open), Some(false), "still closed");
        assert_eq!(dismissed.get(), 0, "no callback for a no-op dismiss");
    }

    /// A guard so the `TimerRequest` import is exercised even though tests reach it
    /// only through the registry — keeps the intent explicit for a reader.
    #[allow(dead_code)]
    fn _timer_request_type(_r: TimerRequest) {}
}
