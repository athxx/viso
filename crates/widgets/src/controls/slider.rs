//! The [`Slider`] control — a draggable thumb on a horizontal track that picks a
//! continuous (or stepped) value from a `[min, max]` range.
//!
//! A `Slider` is a focusable track carrying a thumb that the user drags with the
//! primary pointer or nudges with the arrow keys. Dragging and arrow-stepping are
//! the *same* action semantically — an interactive control must have a keyboard
//! equivalent (AGENTS section 15) — so both drive one shared `on_change`,
//! reporting the *external* value (`min` + the quantized fraction of the range).
//!
//! The internal state is a *relative* fraction in `0.0..=1.0` (matching how the
//! reference switch/toggle controls keep their reactive scalar cell small and
//! `Copy`), converted to the external `[min, max]` value only at the callback
//! boundary. Dragging is *delta*-based: a press records the down-x and the
//! relative value at that instant, and each move adds `(x - down_x) / track_width`
//! to the recorded relative — so the thumb tracks the finger without the handler
//! ever needing the track's world geometry (an `EventCx` handler has no node
//! bounds). The press captures the pointer (`capture_pointer`) so a drag keeps
//! receiving samples even when the cursor leaves the track box; the release frees
//! it.
//!
//! It maps to one interactive `viso-ui` node: a focusable flex row carrying a
//! reactive `value` cell (the relative fraction) bound to `PAINT`, composed of a
//! rounded *track* holding a circular *thumb* leaf, and an optional
//! [`Label`](crate::Label) child for the caption. Dragging writes the `value`
//! cell, repainting the row alone (a targeted invalidation — no rebuild,
//! architecture section 47). Its accessible role is [`Role::CheckBox`] (a
//! dedicated `Slider` role is a later slice) with the caption as its name; the
//! live value is proven through the reactive cell and input tapes. A thumb that
//! *slides* in paint with the cell (rather than sitting at its build-time
//! position) is a later slice, matching the [`Toggle`](crate::Toggle) precedent.
//!
//! ```
//! use viso_widgets::slider;
//! use viso_ui::{BuildCx, BindingTable, Component, NodeStore, StateStore, VirtualLists};
//!
//! let volume = slider("Volume").range(0.0, 100.0).value(30.0).on_change(|_ev, v| {
//!     // handle the new external value — e.g. write app state through the cx
//!     let _ = v;
//! });
//!
//! // A slider authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
//! volume.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset, Key,
    LeafStyle, Length, PointerButtons, PointerPhase, Rgba, Role, Semantics, Size, StateId,
    StateValue,
};

use crate::label;

/// A shared, mutable change callback carrying the new external value. It is cloned
/// into both the pointer handler and the key handler at build time so a drag and
/// an arrow-key step drive the same `on_change`. Pointer and keyboard input are
/// never concurrent, so the runtime never re-enters the `RefCell` borrow.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, f32)>>>>;

/// The track's fill — a neutral recessed channel behind the thumb.
const TRACK: Rgba = Rgba {
    r: 0.24,
    g: 0.25,
    b: 0.28,
    a: 1.0,
};

/// The thumb's fill — a bright accent so the handle reads at a glance.
const THUMB: Rgba = Rgba {
    r: 0.22,
    g: 0.45,
    b: 0.85,
    a: 1.0,
};

/// The caption color: near-white for contrast against a dark surface.
const CAPTION: Rgba = Rgba {
    r: 0.98,
    g: 0.98,
    b: 1.0,
    a: 1.0,
};

/// The default track width.
const TRACK_W: f32 = 120.0;

/// The default track height (its corner radius is half this — a full pill).
const TRACK_H: f32 = 6.0;

/// The default thumb diameter.
const THUMB_D: f32 = 18.0;

/// The gap between the track and the caption.
const GAP: f32 = 8.0;

/// The fraction of the range one arrow-key press moves when the slider is
/// continuous (`step == 0`). A discrete slider steps by one `step` instead.
const KEY_STEP_FRACTION: f32 = 0.05;

/// The visual and layout parameters of a [`Slider`]: the track and thumb
/// dimensions and fills, plus the slider's own size request within its parent.
///
/// `size` defaults to [`Length::Fit`] on both axes so the slider shrinks to the
/// track plus the caption; override it with a [`Length::Fixed`] or
/// [`Length::Fill`] axis for a fixed or flexing box. All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliderStyle {
    /// The slider's own size request within its parent. Defaults to `Fit` on both
    /// axes (shrink to the track plus the caption).
    pub size: Size,
    /// The track's width — also the pixel span a full `0.0..=1.0` drag covers.
    pub track_width: f32,
    /// The track's height (its corner radius is half this — a full pill).
    pub track_height: f32,
    /// The thumb's diameter (a circle: radius = half this).
    pub thumb_diameter: f32,
    /// The track's fill.
    pub track: BoxStyle,
    /// The thumb's fill.
    pub thumb: BoxStyle,
}

impl Default for SliderStyle {
    fn default() -> Self {
        SliderStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            track_width: TRACK_W,
            track_height: TRACK_H,
            thumb_diameter: THUMB_D,
            // A full pill: radius = half the height.
            track: BoxStyle::solid(TRACK).with_radius(TRACK_H / 2.0),
            // A circle: radius = half the diameter.
            thumb: BoxStyle::solid(THUMB).with_radius(THUMB_D / 2.0),
        }
    }
}

/// A draggable thumb on a track that picks a value from a `[min, max]` range.
///
/// Construct one with [`slider`] and attach behavior with the chainable setters.
/// Dragging with the primary pointer or nudging with the arrow keys moves the
/// value and fires `on_change` with the new *external* value.
///
/// See the [module docs](self) for a build example. Invalidation: moving the
/// value writes a reactive `value` cell bound to `PAINT`, repainting the slider's
/// own row without a rebuild; the caption and structure are static.
pub struct Slider {
    /// The visible caption, also used as the accessible name.
    label: String,
    /// The inclusive lower bound of the external range.
    min: f32,
    /// The inclusive upper bound of the external range.
    max: f32,
    /// The quantization step in external units; `0.0` means continuous.
    step: f32,
    /// The initial external value (defaults to `min`).
    value: f32,
    style: SliderStyle,
    /// The shared change callback (see [`SharedChange`]). `None` until
    /// [`Slider::on_change`] is called; a slider with no handler still moves its
    /// value cell, just without notifying anyone.
    on_change: SharedChange,
}

/// Construct a [`Slider`] with the given caption, a default `0.0..=1.0`
/// continuous range starting at `min`, and no handler yet. Chain
/// [`Slider::range`]/[`Slider::step`]/[`Slider::value`] to shape the range and
/// [`Slider::on_change`] to give it behavior.
pub fn slider(label: impl Into<String>) -> Slider {
    Slider {
        label: label.into(),
        min: 0.0,
        max: 1.0,
        step: 0.0,
        value: 0.0,
        style: SliderStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl Slider {
    /// Set the change callback, fired with the new *external* value on a drag or
    /// an arrow-key step. Replaces any previously set handler.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, f32) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the inclusive external range. A degenerate range (`max <= min`) pins
    /// the slider to `min`.
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    /// Set the quantization step in external units. `0.0` (the default) is a
    /// continuous slider; a positive step snaps the reported value to a grid.
    pub fn step(mut self, step: f32) -> Self {
        self.step = step.max(0.0);
        self
    }

    /// Set the initial external value (defaults to `min`). Clamped into range at
    /// build time.
    pub fn value(mut self, value: f32) -> Self {
        self.value = value;
        self
    }

    /// Replace the whole [`SliderStyle`].
    pub fn style(mut self, style: SliderStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the slider's own size request within its parent (defaults to `Fit`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// The span of the external range, guarding a degenerate `max <= min`.
    fn span(&self) -> f32 {
        self.max - self.min
    }

    /// The initial relative fraction in `0.0..=1.0` from the clamped initial
    /// external value. A degenerate range maps everything to `0.0`.
    fn initial_relative(&self) -> f32 {
        let span = self.span();
        if span <= 0.0 {
            0.0
        } else {
            ((self.value - self.min) / span).clamp(0.0, 1.0)
        }
    }
}

/// Convert a relative fraction to the external value, applying the step grid when
/// the slider is discrete. Mirrors the reference `to_external` mapping: a discrete
/// slider floors `relative * span / step` to a whole number of steps.
fn to_external(relative: f32, min: f32, span: f32, step: f32) -> f32 {
    if span <= 0.0 {
        return min;
    }
    if step > 0.0 {
        (relative * span / step).floor() * step + min
    } else {
        relative * span + min
    }
}

/// Drive the shared callback with the new external value if one is set; a
/// handler-less slider is a no-op. Pointer and keyboard activation never overlap,
/// so the borrow is uncontended.
fn fire(cb: &SharedChange, ev: &mut EventCx<'_>, value: f32) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev, value);
    }
}

/// Read a float state cell, defaulting to `0.0` for a stale handle or a non-float
/// value (neither happens in normal use — the cells are authored as `Float` and
/// live as long as the node).
fn read_f32(ev: &EventCx<'_>, cell: StateId) -> f32 {
    match ev.get(cell) {
        Some(StateValue::Float(v)) => v,
        _ => 0.0,
    }
}

impl Component for Slider {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // Three reactive `Float` cells keep the drag self-contained: `value` is
        // the relative fraction (bound to PAINT so a move repaints the row alone,
        // a targeted invalidation, not a rebuild); `down_x` and `start_rel` record
        // the press anchor so a move is a pure delta from it — the handler never
        // needs the track's world geometry (an `EventCx` has no node bounds).
        let rel0 = self.initial_relative();
        let value = cx.state(StateValue::Float(rel0));
        let down_x = cx.state(StateValue::Float(0.0));
        let start_rel = cx.state(StateValue::Float(rel0));

        // Read the (Copy) style and range by value; `build` takes `&self`, so the
        // caption is cloned (cold, build-time — not a per-frame path).
        let track_w = self.style.track_width;
        let track_h = self.style.track_height;
        let thumb_d = self.style.thumb_diameter;
        let track_box = self.style.track;
        let thumb_box = self.style.thumb;
        let caption = self.label.clone();
        let (min, span, step) = (self.min, self.span(), self.step);

        // Pin the thumb to its initial-relative position by splitting the track's
        // free main-axis room (width minus the thumb diameter) into the leading
        // pad. A thumb that slides in paint with the cell is a later slice
        // (matching the Toggle precedent).
        let free = (track_w - thumb_d).max(0.0);
        let lead = rel0 * free;

        let root = cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: GAP,
                padding: Inset::all(0.0),
                align: Align::Center,
                size: self.style.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                // The track: a fixed pill holding the thumb, cross-centered, with
                // the thumb pushed to its initial position by the leading pad. The
                // track is sized to the thumb's height so the (larger) thumb is the
                // row's cross extent; the thin track sits centered behind it.
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: 0.0,
                        padding: Inset {
                            left: lead,
                            top: 0.0,
                            right: 0.0,
                            bottom: 0.0,
                        },
                        align: Align::Center,
                        size: Size::fixed(track_w, thumb_d.max(track_h)),
                        style: track_box,
                    },
                    |cx| {
                        cx.leaf(LeafStyle {
                            size: Size::fixed(thumb_d, thumb_d),
                            style: thumb_box,
                        });
                    },
                );
                label(caption.clone()).color(CAPTION).build(cx);
            },
        );

        // Bind the relative-value cell to the row's PAINT so a move repaints just
        // this subtree.
        cx.bind(value, root, DirtyClass::PAINT);
        cx.focusable(root, true);

        // Pointer drag: a primary press records the anchor (down-x + the relative
        // value at that instant) and captures the pointer, so subsequent samples
        // route here even outside the track box. Each move adds the pixel delta
        // over the track width to the recorded relative and fires `on_change` with
        // the new external value. The release frees the capture. The delta model
        // needs no node geometry — only the build-time `track_width`.
        let pointer_cb = self.on_change.clone();
        let root_id = root.id();
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if !p.buttons.contains(PointerButtons::PRIMARY) && p.phase != PointerPhase::Up {
                return;
            }
            match p.phase {
                PointerPhase::Down => {
                    ev.set(down_x, StateValue::Float(p.x));
                    ev.set(start_rel, StateValue::Float(read_f32(ev, value)));
                    ev.capture_pointer(root_id);
                }
                PointerPhase::Move => {
                    let dx = p.x - read_f32(ev, down_x);
                    let delta = if track_w > 0.0 { dx / track_w } else { 0.0 };
                    let next = (read_f32(ev, start_rel) + delta).clamp(0.0, 1.0);
                    if ev.set(value, StateValue::Float(next)) {
                        fire(&pointer_cb, ev, to_external(next, min, span, step));
                    }
                }
                PointerPhase::Up => {
                    ev.release_pointer();
                }
                PointerPhase::Leave => {}
            }
        });

        // Keyboard step: Left/Down decrement and Right/Up increment the relative
        // value by one step (a discrete slider steps by `step / span`, a continuous
        // one by a small fraction), firing the same `on_change` — the accessibility
        // equivalent of the drag (AGENTS section 15). Auto-repeat is honored so a
        // held arrow ramps the value.
        let key_cb = self.on_change.clone();
        cx.on_key(root, move |ev| {
            let Some(k) = ev.key() else { return };
            if !k.pressed {
                return;
            }
            let dir = match k.key {
                Key::Right | Key::Up => 1.0,
                Key::Left | Key::Down => -1.0,
                _ => return,
            };
            let increment = if step > 0.0 && span > 0.0 {
                step / span
            } else {
                KEY_STEP_FRACTION
            };
            let next = (read_f32(ev, value) + dir * increment).clamp(0.0, 1.0);
            if ev.set(value, StateValue::Float(next)) {
                fire(&key_cb, ev, to_external(next, min, span, step));
            }
        });

        // A slider is a continuous/stepped value picker; a dedicated `Slider` role
        // is a later slice, so it derives `Role::CheckBox` for now. The caption is
        // its accessible name, so the name matches the visible label (AGENTS
        // section 15). The live value is proven by the reactive cell and input
        // tapes; wiring it into the derived tree is a later slice (the derive pass
        // has no state store).
        cx.semantics(
            root,
            Semantics::role(Role::CheckBox).with_label(self.label.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use viso_ui::{
        BindingTable, KeyEvent, Modifiers, NodeId, NodeStore, PointerEvent, StateStore,
        VirtualLists,
    };

    /// The reactive stores a slider build writes into, kept together so a test can
    /// build a slider and then drive its handlers against the same state.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
            }
        }

        /// Build a slider through a reactive cx (a slider authors state, so a plain
        /// `BuildCx::new` would panic) and return its root node.
        fn build(&mut self, s: Slider) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
            );
            s.build(&mut cx);
            cx.root().expect("slider declares a root node")
        }

        /// Feed a pointer sample to the root's pointer handler, restoring it after,
        /// and return any pending capture request the handler made.
        fn pointer(&mut self, root: NodeId, ev: PointerEvent) -> Option<Option<NodeId>> {
            let mut handler = self.store.take_handler(root).expect("pointer handler");
            let capture = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                cx.__take_capture_request()
            };
            self.store.restore_handler(root, handler);
            capture
        }

        /// Feed a key sample to the root's key handler, restoring it after.
        fn key(&mut self, root: NodeId, ev: KeyEvent) {
            let mut handler = self.store.take_key_handler(root).expect("key handler");
            {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_key_handler(root, handler);
        }

        /// The current relative value of the slider's `value` cell — the first
        /// cell the build authors (a fresh store mints index 0, generation 0).
        fn relative(&mut self) -> f32 {
            let ev = still();
            let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            match cx.get(value_cell()) {
                Some(StateValue::Float(v)) => v,
                _ => f32::NAN,
            }
        }
    }

    /// The slider's `value` cell handle. The build authors `value` first, so in a
    /// fresh store it is index 0, generation 0 — the handle a throwaway store mints
    /// for its first `Float` cell (there is no public bare-`StateId` constructor).
    fn value_cell() -> StateId {
        StateStore::new().alloc(StateValue::Float(0.0))
    }

    /// A still (no-button) pointer sample, for read-only state peeks.
    fn still() -> PointerEvent {
        PointerEvent {
            x: 0.0,
            y: 0.0,
            phase: PointerPhase::Move,
            buttons: PointerButtons::NONE,
            modifiers: Modifiers::default(),
        }
    }

    /// A primary-button pointer sample at `x` in the given phase.
    fn primary_at(x: f32, phase: PointerPhase) -> PointerEvent {
        PointerEvent {
            x,
            y: 0.0,
            phase,
            buttons: PointerButtons::PRIMARY,
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

    /// Count a node's direct children by walking the arena sibling chain.
    fn child_count(store: &NodeStore, parent: NodeId) -> usize {
        let arena = store.arena();
        let mut n = 0;
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            n += 1;
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        n
    }

    /// A slider builds an interactive node: a pointer handler, a key handler, a
    /// focusable flag, a `CheckBox` semantics node named by its caption, and two
    /// composed children (the track and the caption). The track holds the thumb as
    /// its only child.
    #[test]
    fn slider_builds_a_focusable_node_with_track_thumb_and_caption() {
        let mut rx = Reactive::new();
        let root = rx.build(slider("Volume"));

        assert!(
            rx.store.has_handler(root),
            "slider attaches a pointer handler"
        );
        assert!(
            rx.store.has_key_handler(root),
            "slider attaches a key handler"
        );
        assert!(rx.store.focusable(root), "slider is focusable");
        assert_eq!(
            child_count(&rx.store, root),
            2,
            "slider composes a track and a caption"
        );

        let track = rx
            .store
            .arena()
            .links(root)
            .and_then(|l| l.first_child)
            .expect("slider has a track as its first child");
        assert_eq!(
            child_count(&rx.store, track),
            1,
            "the track holds the thumb as its only child"
        );

        let sem = rx.store.semantics(root).expect("slider authors semantics");
        assert_eq!(sem.role, Role::CheckBox);
        assert_eq!(sem.label.as_deref(), Some("Volume"));
    }

    /// The chainable setters override the range, step, initial value, and size,
    /// and the default is a `0.0..=1.0` continuous slider at `min`, `Fit` on both
    /// axes.
    #[test]
    fn setters_override_defaults() {
        let default = slider("x");
        assert_eq!(default.min, 0.0);
        assert_eq!(default.max, 1.0);
        assert_eq!(default.step, 0.0);
        assert_eq!(default.value, 0.0);
        assert_eq!(default.style.size.width, Length::Fit);
        assert_eq!(default.style.size.height, Length::Fit);

        let widget = slider("x")
            .range(0.0, 100.0)
            .step(10.0)
            .value(30.0)
            .size(Size::fill());
        assert_eq!(widget.min, 0.0);
        assert_eq!(widget.max, 100.0);
        assert_eq!(widget.step, 10.0);
        assert_eq!(widget.value, 30.0);
        assert_eq!(widget.style.size, Size::fill());
        // 30 of 100 is 0.3 of the range.
        assert!((widget.initial_relative() - 0.3).abs() < 1e-6);
    }

    /// `to_external` maps a relative fraction back to the range, and a discrete
    /// step snaps to the grid.
    #[test]
    fn to_external_maps_and_snaps() {
        // Continuous: linear over the range.
        assert!((to_external(0.5, 0.0, 100.0, 0.0) - 50.0).abs() < 1e-6);
        assert!((to_external(0.0, 10.0, 40.0, 0.0) - 10.0).abs() < 1e-6);
        assert!((to_external(1.0, 10.0, 40.0, 0.0) - 50.0).abs() < 1e-6);
        // Discrete step 10 over [0,100]: 0.47*100 = 47 -> floors to 40.
        assert!((to_external(0.47, 0.0, 100.0, 10.0) - 40.0).abs() < 1e-6);
        // Degenerate range pins to min.
        assert!((to_external(0.9, 5.0, 0.0, 0.0) - 5.0).abs() < 1e-6);
    }

    /// A primary press records the anchor and captures the pointer to the root; a
    /// move drags the relative value by the pixel delta over the track width and
    /// fires `on_change` with the new external value; the release frees capture.
    #[test]
    fn pointer_drag_moves_value_captures_and_fires_change() {
        let last = Rc::new(Cell::new(None::<f32>));
        let count = Rc::new(Cell::new(0u32));
        let (l, c) = (last.clone(), count.clone());
        let mut rx = Reactive::new();
        // A [0,100] slider with the default 120px track, starting at 0.
        let root = rx.build(slider("Volume").range(0.0, 100.0).on_change(move |_, v| {
            l.set(Some(v));
            c.set(c.get() + 1);
        }));

        assert_eq!(rx.relative(), 0.0, "starts at the min");

        // Press at x=0: records the anchor and requests capture to the root.
        let cap = rx.pointer(root, primary_at(0.0, PointerPhase::Down));
        assert_eq!(cap, Some(Some(root)), "the press captures the pointer");
        assert_eq!(count.get(), 0, "the press alone does not fire a change");

        // Move right by 60px over a 120px track: +0.5 relative -> 0.5 -> 50.0.
        rx.pointer(root, primary_at(60.0, PointerPhase::Move));
        assert!((rx.relative() - 0.5).abs() < 1e-6, "dragged to the middle");
        assert_eq!(count.get(), 1, "the move fires one change");
        assert_eq!(
            last.get(),
            Some(50.0),
            "on_change carries the external value"
        );

        // Move past the far end: clamps to 1.0 -> 100.0.
        rx.pointer(root, primary_at(200.0, PointerPhase::Move));
        assert!((rx.relative() - 1.0).abs() < 1e-6, "clamps at the far end");
        assert_eq!(last.get(), Some(100.0));

        // Release frees capture.
        let cap = rx.pointer(root, primary_at(200.0, PointerPhase::Up));
        assert_eq!(cap, Some(None), "the release frees the capture");
    }

    /// The drag is a pure delta from the press anchor: a second press re-anchors,
    /// so a move after it is measured from the new down-x, not the old one.
    #[test]
    fn drag_reanchors_on_each_press() {
        let mut rx = Reactive::new();
        let root = rx.build(slider("v").range(0.0, 1.0)); // 120px track, span 1

        // First drag to 0.5.
        rx.pointer(root, primary_at(0.0, PointerPhase::Down));
        rx.pointer(root, primary_at(60.0, PointerPhase::Move));
        assert!((rx.relative() - 0.5).abs() < 1e-6);
        rx.pointer(root, primary_at(60.0, PointerPhase::Up));

        // Second press at x=100 re-anchors at relative 0.5; a move to x=112 is
        // +12px = +0.1, landing at 0.6 — not measured from the original down-x.
        rx.pointer(root, primary_at(100.0, PointerPhase::Down));
        rx.pointer(root, primary_at(112.0, PointerPhase::Move));
        assert!(
            (rx.relative() - 0.6).abs() < 1e-6,
            "the second drag deltas from the new anchor"
        );
    }

    /// A non-primary button neither anchors nor drags.
    #[test]
    fn non_primary_pointer_does_not_drag() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();
        let mut rx = Reactive::new();
        let root = rx.build(slider("v").on_change(move |_, _| c.set(c.get() + 1)));

        let non_primary_down = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary_at(0.0, PointerPhase::Down)
        };
        let cap = rx.pointer(root, non_primary_down);
        assert_eq!(cap, None, "a non-primary press does not capture");
        let non_primary_move = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary_at(60.0, PointerPhase::Move)
        };
        rx.pointer(root, non_primary_move);
        assert_eq!(count.get(), 0, "a non-primary drag fires nothing");
    }

    /// Arrow keys step the value and fire `on_change`; Right/Up increment,
    /// Left/Down decrement, and a continuous slider steps by the default fraction.
    #[test]
    fn arrow_keys_step_the_value() {
        let last = Rc::new(Cell::new(None::<f32>));
        let count = Rc::new(Cell::new(0u32));
        let (l, c) = (last.clone(), count.clone());
        let mut rx = Reactive::new();
        // Continuous [0,100]: the default key step is 5% of the range = 5 units.
        let root = rx.build(
            slider("v")
                .range(0.0, 100.0)
                .value(50.0)
                .on_change(move |_, v| {
                    l.set(Some(v));
                    c.set(c.get() + 1);
                }),
        );

        assert!((rx.relative() - 0.5).abs() < 1e-6, "starts mid-range");

        rx.key(root, key_ev(Key::Right, true, false));
        assert!((rx.relative() - 0.55).abs() < 1e-6, "Right steps up 5%");
        assert_eq!(last.get(), Some(55.0));

        rx.key(root, key_ev(Key::Up, true, false));
        assert!((rx.relative() - 0.6).abs() < 1e-6, "Up steps up too");

        rx.key(root, key_ev(Key::Left, true, false));
        assert!((rx.relative() - 0.55).abs() < 1e-6, "Left steps down");
        rx.key(root, key_ev(Key::Down, true, false));
        assert!((rx.relative() - 0.5).abs() < 1e-6, "Down steps down too");

        assert_eq!(count.get(), 4, "each arrow press fired one change");

        // A key-up and an unrelated key do nothing.
        rx.key(root, key_ev(Key::Right, false, false));
        rx.key(root, key_ev(Key::Enter, true, false));
        assert_eq!(count.get(), 4, "key-up and Enter do not step");
    }

    /// A discrete slider steps by exactly one grid unit per arrow press.
    #[test]
    fn discrete_arrow_step_is_one_grid_unit() {
        let last = Rc::new(Cell::new(None::<f32>));
        let l = last.clone();
        let mut rx = Reactive::new();
        // [0,100] step 10, starting at 0.
        let root = rx.build(
            slider("v")
                .range(0.0, 100.0)
                .step(10.0)
                .on_change(move |_, v| l.set(Some(v))),
        );

        rx.key(root, key_ev(Key::Right, true, false));
        assert_eq!(last.get(), Some(10.0), "one Right is one step of 10");
        rx.key(root, key_ev(Key::Right, true, false));
        assert_eq!(last.get(), Some(20.0), "another Right is another step");
    }

    /// A handler-less slider still moves its value without panicking.
    #[test]
    fn slider_without_handler_still_moves() {
        let mut rx = Reactive::new();
        let root = rx.build(slider("v").range(0.0, 1.0));

        rx.pointer(root, primary_at(0.0, PointerPhase::Down));
        rx.pointer(root, primary_at(60.0, PointerPhase::Move)); // no panic with no callback
        assert!((rx.relative() - 0.5).abs() < 1e-6, "still moves the value");
        assert!(rx.store.focusable(root), "still focusable");
    }
}
