//! The [`Toggle`] control — a switch: a two-state on/off toggle with a label.
//!
//! A `Toggle` is a focusable pill-shaped track carrying a circular thumb that
//! flips a boolean `on` state on either a primary-pointer click (press then
//! release) or a keyboard activation (Enter/Space while focused), firing an
//! `on_change` callback with the *new* on value. Pointer and keyboard activation
//! are the *same* action: an interactive control must have a keyboard equivalent
//! (AGENTS section 15), so the callback is shared between both handlers rather
//! than duplicated.
//!
//! A `Toggle` is the structural sibling of a [`CheckBox`](crate::CheckBox): both
//! *toggle* (each activation reads the current cell and writes its inverse, so
//! the state stays until the next activation), unlike a [`Button`](crate::Button)
//! whose press is momentary. The difference is purely presentational — a switch
//! reads as a physical on/off track-and-thumb rather than a checked box, the
//! affordance platforms use for an immediate binary setting.
//!
//! It maps to one interactive `viso-ui` node: a focusable flex row carrying a
//! reactive `on` cell bound to `PAINT`, composed of a pill *track* (a rounded box
//! whose fill reflects the on state) holding a circular *thumb* leaf, and a
//! [`Label`](crate::Label) child for the caption. Toggling flips the `on` cell,
//! repainting the row alone (a targeted invalidation — no rebuild, architecture
//! section 47). Its accessible role is [`Role::CheckBox`] (a switch is a
//! two-state boolean toggle; a dedicated `Switch` role is a later slice) with the
//! caption as its name; the live on value is proven through the reactive cell and
//! input tapes (wiring it into the derived semantics tree is a later slice — the
//! derive pass has no state store).
//!
//! ```
//! use viso_widgets::toggle;
//! use viso_ui::{BuildCx, BindingTable, Component, NodeStore, StateStore, VirtualLists};
//!
//! let sw = toggle("Wi-Fi").on(true).on_change(|_ev, on| {
//!     // handle the new state — e.g. write app state through the event context
//!     let _ = on;
//! });
//!
//! // A toggle authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
//! sw.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset, Key,
    LeafStyle, Length, PointerButtons, PointerPhase, Rgba, Role, Semantics, Size, StateValue,
};

use crate::label;

/// A shared, mutable change callback carrying the new on value. It is cloned into
/// both the pointer handler and the key handler at build time so a pointer click
/// and a keyboard activation drive the same `on_change`. Pointer and keyboard
/// input are never concurrent, so the runtime never re-enters the `RefCell`
/// borrow.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, bool)>>>>;

/// The track's fill while off — a neutral recessed channel.
const OFF: Rgba = Rgba {
    r: 0.24,
    g: 0.25,
    b: 0.28,
    a: 1.0,
};

/// The track's fill while on — a bright accent so the state reads at a glance.
const ON: Rgba = Rgba {
    r: 0.22,
    g: 0.45,
    b: 0.85,
    a: 1.0,
};

/// The thumb's fill — near-white on both states, sliding within the track.
const THUMB: Rgba = Rgba {
    r: 0.98,
    g: 0.98,
    b: 1.0,
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
const TRACK_W: f32 = 40.0;

/// The default track height (its corner radius is half this — a full pill).
const TRACK_H: f32 = 22.0;

/// The default inset between the track edge and the thumb, on all sides.
const THUMB_INSET: f32 = 3.0;

/// The gap between the track and the caption.
const GAP: f32 = 8.0;

/// The visual and layout parameters of a [`Toggle`]: the track's off and on
/// fills, the track and thumb dimensions, and the toggle's own size request
/// within its parent.
///
/// `size` defaults to [`Length::Fit`] on both axes so the toggle shrinks to the
/// track plus the caption; override it with a [`Length::Fixed`] or
/// [`Length::Fill`] axis for a fixed or flexing box. `off` and `on` default to a
/// neutral channel and an accent channel; the track is a full pill (radius =
/// half its height). All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToggleStyle {
    /// The toggle's own size request within its parent. Defaults to `Fit` on
    /// both axes (shrink to the track plus the caption).
    pub size: Size,
    /// The track's width.
    pub track_width: f32,
    /// The track's height (its corner radius is half this — a full pill).
    pub track_height: f32,
    /// The inset between the track edge and the thumb, on all sides. The thumb's
    /// diameter is `track_height - 2 * thumb_inset`.
    pub thumb_inset: f32,
    /// The track's fill while off.
    pub off: BoxStyle,
    /// The track's fill while on.
    pub on: BoxStyle,
    /// The thumb's fill (a circle: radius = half its diameter).
    pub thumb: BoxStyle,
}

impl ToggleStyle {
    /// The thumb's diameter, derived from the track height and inset.
    fn thumb_diameter(&self) -> f32 {
        (self.track_height - 2.0 * self.thumb_inset).max(0.0)
    }
}

impl Default for ToggleStyle {
    fn default() -> Self {
        ToggleStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            track_width: TRACK_W,
            track_height: TRACK_H,
            thumb_inset: THUMB_INSET,
            // A full pill: radius = half the height.
            off: BoxStyle::solid(OFF).with_radius(TRACK_H / 2.0),
            on: BoxStyle::solid(ON).with_radius(TRACK_H / 2.0),
            // A circle: radius = half the thumb diameter.
            thumb: BoxStyle::solid(THUMB).with_radius((TRACK_H - 2.0 * THUMB_INSET).max(0.0) / 2.0),
        }
    }
}

/// A switch: a focusable pill track with a sliding thumb and a caption.
///
/// Construct one with [`toggle`] and attach behavior with the chainable setters.
/// Each activation — a primary-pointer press then release, or Enter/Space while
/// focused — flips the on state and fires `on_change` once with the new value.
///
/// See the [module docs](self) for a build example. Invalidation: toggling flips
/// a reactive `on` cell bound to `PAINT`, repainting the toggle's own row without
/// a rebuild; the caption and structure are static.
pub struct Toggle {
    /// The visible caption, also used as the accessible name.
    label: String,
    /// The initial on state (defaults to `false`).
    on: bool,
    style: ToggleStyle,
    /// The shared change callback (see [`SharedChange`]). `None` until
    /// [`Toggle::on_change`] is called; a toggle with no handler still flips its
    /// visible state, just without notifying anyone.
    on_change: SharedChange,
}

/// Construct a [`Toggle`] with the given caption, off, and no handler yet. Chain
/// [`Toggle::on_change`] to give it behavior, [`Toggle::on`] to start it on, and
/// [`Toggle::style`]/[`Toggle::size`] to adjust its appearance.
pub fn toggle(label: impl Into<String>) -> Toggle {
    Toggle {
        label: label.into(),
        on: false,
        style: ToggleStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl Toggle {
    /// Set the change callback, fired with the new on value on a primary-pointer
    /// click or a keyboard activation. Replaces any previously set handler.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, bool) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the initial on state (defaults to `false`).
    pub fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    /// Replace the whole [`ToggleStyle`].
    pub fn style(mut self, style: ToggleStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the toggle's own size request within its parent (defaults to `Fit`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }
}

/// Drive the shared callback with the new on value if one is set; a handler-less
/// toggle is a no-op. Pointer and keyboard activation never overlap, so the
/// borrow is uncontended.
fn fire(cb: &SharedChange, ev: &mut EventCx<'_>, on: bool) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev, on);
    }
}

/// Read the current on value from the reactive cell, defaulting to `false` for a
/// stale handle or a non-bool value (neither happens in normal use — the cell is
/// authored as `Bool` and lives as long as the node).
fn read_on(ev: &EventCx<'_>, cell: viso_ui::StateId) -> bool {
    matches!(ev.get(cell), Some(StateValue::Bool(true)))
}

impl Component for Toggle {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // A reactive `on` cell bound to PAINT: toggling repaints the row alone
        // (targeted invalidation, not a rebuild). The track's fill is chosen at
        // build from this cell's initial value; the live value is proven through
        // the cell and input tapes (paint reflects the build-time style, so the
        // thumb sits at its initial-state edge — a sliding thumb driven by the
        // cell in paint is a later slice).
        let on = cx.state(StateValue::Bool(self.on));

        // The track box reflects the initial on state; the thumb pins to the
        // near edge when off and the far edge when on. `build` takes `&self`, so
        // the styles (`Copy`) are read by value and the caption is cloned (cold,
        // build-time — not a per-frame path).
        let track_box = if self.on {
            self.style.on
        } else {
            self.style.off
        };
        let track_w = self.style.track_width;
        let track_h = self.style.track_height;
        let thumb_inset = self.style.thumb_inset;
        let thumb_d = self.style.thumb_diameter();
        let thumb_box = self.style.thumb;
        // Pin the thumb to the far edge when on, the near edge when off, by
        // splitting the track's free main-axis room (width minus thumb diameter
        // minus both insets) into the leading pad.
        let free = (track_w - thumb_d - 2.0 * thumb_inset).max(0.0);
        let lead = if self.on { free } else { 0.0 };
        let caption = self.label.clone();

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
                // the thumb pushed to its initial-state edge by the leading pad.
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: 0.0,
                        padding: Inset {
                            left: thumb_inset + lead,
                            top: thumb_inset,
                            right: thumb_inset,
                            bottom: thumb_inset,
                        },
                        align: Align::Center,
                        size: Size::fixed(track_w, track_h),
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

        // Bind the on cell to the row's PAINT so a toggle repaints just this
        // subtree. The track's fill swap is expressed by rebinding the box in
        // paint via the cell; this slice paints the initial-state track and marks
        // PAINT dirty on toggle so the frame re-emits the node.
        cx.bind(on, root, DirtyClass::PAINT);
        cx.focusable(root, true);

        // Pointer activation: a primary press-then-release toggles the on cell
        // and fires `on_change` with the new value. One press/release is one
        // toggle — the release is the activation, matching Button's click gate.
        let pointer_cb = self.on_change.clone();
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase == PointerPhase::Up && p.buttons.contains(PointerButtons::PRIMARY) {
                let next = !read_on(ev, on);
                ev.set(on, StateValue::Bool(next));
                fire(&pointer_cb, ev, next);
            }
        });

        // Keyboard activation: Enter/Space pressed (not an auto-repeat) toggles
        // the same cell and fires the same `on_change` — the accessibility
        // equivalent of the pointer click.
        let key_cb = self.on_change.clone();
        cx.on_key(root, move |ev| {
            if let Some(k) = ev.key()
                && k.pressed
                && !k.repeat
                && matches!(k.key, Key::Enter | Key::Space)
            {
                let next = !read_on(ev, on);
                ev.set(on, StateValue::Bool(next));
                fire(&key_cb, ev, next);
            }
        });

        // A switch is a two-state boolean toggle, semantically a checkbox; a
        // dedicated `Switch` role is a later slice. The caption is its accessible
        // name, so the name matches the visible label (AGENTS section 15). The
        // live on value is proven by the reactive cell and input tapes; wiring it
        // into the derived tree is a later slice (the derive pass has no state
        // store).
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
        BindingTable, KeyEvent, Modifiers, NodeId, NodeStore, PointerEvent, StateId, StateStore,
        VirtualLists,
    };

    /// The reactive stores a toggle build writes into, kept together so a test can
    /// build a toggle and then drive its handlers against the same state.
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

        /// Build a toggle through a reactive cx (a toggle authors state, so a
        /// plain `BuildCx::new` would panic) and return its root node.
        fn build(&mut self, sw: Toggle) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
            );
            sw.build(&mut cx);
            cx.root().expect("toggle declares a root node")
        }

        /// Feed a pointer sample to the root's pointer handler, restoring it after
        /// (the router borrow discipline: take, drive, restore).
        fn pointer(&mut self, root: NodeId, ev: PointerEvent) {
            let mut handler = self.store.take_handler(root).expect("pointer handler");
            {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_handler(root, handler);
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

        /// The current value of a bool state cell (via a throwaway read cx).
        fn on(&mut self, cell: StateId) -> bool {
            let ev = PointerEvent {
                x: 0.0,
                y: 0.0,
                phase: PointerPhase::Move,
                buttons: PointerButtons::NONE,
                modifiers: Modifiers::default(),
            };
            let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            matches!(cx.get(cell), Some(StateValue::Bool(true)))
        }
    }

    /// A primary-button pointer sample in the given phase.
    fn primary(phase: PointerPhase) -> PointerEvent {
        PointerEvent {
            x: 0.0,
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

    /// The first state cell authored by the build — the toggle's `on` cell (it
    /// authors exactly one). A fresh `StateStore` mints its first cell at index 0,
    /// generation 0, so allocating one cell in a throwaway store yields the very
    /// handle the toggle's build produced (there is no public constructor for a
    /// bare `StateId`, and the build does not return it).
    fn first_cell() -> StateId {
        StateStore::new().alloc(StateValue::Bool(false))
    }

    /// A toggle builds an interactive node: a pointer handler, a key handler, a
    /// focusable flag, a `CheckBox` semantics node named by its caption, and two
    /// composed children (the track and the caption). The track itself holds the
    /// thumb as its only child.
    #[test]
    fn toggle_builds_a_focusable_switch_node_with_track_thumb_and_caption() {
        let mut rx = Reactive::new();
        let root = rx.build(toggle("Wi-Fi"));

        assert!(
            rx.store.has_handler(root),
            "toggle attaches a pointer handler"
        );
        assert!(
            rx.store.has_key_handler(root),
            "toggle attaches a key handler"
        );
        assert!(rx.store.focusable(root), "toggle is focusable");
        assert_eq!(
            child_count(&rx.store, root),
            2,
            "toggle composes a track and a caption"
        );

        // The track is the row's first child; the thumb is the track's only child.
        let track = rx
            .store
            .arena()
            .links(root)
            .and_then(|l| l.first_child)
            .expect("toggle has a track as its first child");
        assert_eq!(
            child_count(&rx.store, track),
            1,
            "the track holds the thumb as its only child"
        );

        let sem = rx.store.semantics(root).expect("toggle authors semantics");
        assert_eq!(sem.role, Role::CheckBox);
        assert_eq!(sem.label.as_deref(), Some("Wi-Fi"));
    }

    /// The chainable setters override size, the whole style, and the initial on
    /// state, and the default size is `Fit` on both axes and off.
    #[test]
    fn setters_override_style_default_size_is_fit_default_off() {
        let default = toggle("x");
        assert_eq!(default.style.size.width, Length::Fit);
        assert_eq!(default.style.size.height, Length::Fit);
        assert!(!default.on, "a toggle starts off");

        let widget = toggle("x").size(Size::fill()).on(true);
        assert_eq!(widget.style.size, Size::fill());
        assert!(widget.on, "on(true) starts it on");
    }

    /// A primary press-then-release toggles the on cell and fires `on_change`
    /// once with the new value; a second click toggles back.
    #[test]
    fn pointer_click_toggles_on_and_fires_change() {
        let last = Rc::new(Cell::new(None::<bool>));
        let count = Rc::new(Cell::new(0u32));
        let (l, c) = (last.clone(), count.clone());
        let mut rx = Reactive::new();
        let root = rx.build(toggle("Wi-Fi").on_change(move |_, on| {
            l.set(Some(on));
            c.set(c.get() + 1);
        }));
        let cell = first_cell();

        assert!(!rx.on(cell), "starts off");

        rx.pointer(root, primary(PointerPhase::Down));
        assert_eq!(count.get(), 0, "the press alone does not toggle");

        rx.pointer(root, primary(PointerPhase::Up));
        assert!(rx.on(cell), "the release toggles to on");
        assert_eq!(count.get(), 1, "one activation is one change");
        assert_eq!(last.get(), Some(true), "on_change carries the new value");

        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up));
        assert!(!rx.on(cell), "a second click toggles back off");
        assert_eq!(count.get(), 2);
        assert_eq!(last.get(), Some(false), "and reports the new value");
    }

    /// A non-primary pointer button does not toggle.
    #[test]
    fn non_primary_pointer_does_not_toggle() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();
        let mut rx = Reactive::new();
        let root = rx.build(toggle("Wi-Fi").on_change(move |_, _| c.set(c.get() + 1)));
        let cell = first_cell();

        let non_primary_up = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary(PointerPhase::Up)
        };
        rx.pointer(root, non_primary_up);
        assert!(!rx.on(cell), "a non-primary button does not toggle");
        assert_eq!(count.get(), 0);
    }

    /// Enter and Space (pressed, not auto-repeat) each toggle the cell and fire
    /// `on_change`; an auto-repeat and a key-up do not.
    #[test]
    fn keyboard_enter_and_space_toggle_repeat_does_not() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();
        let mut rx = Reactive::new();
        let root = rx.build(toggle("Wi-Fi").on_change(move |_, _| c.set(c.get() + 1)));
        let cell = first_cell();

        rx.key(root, key_ev(Key::Enter, true, false));
        assert!(rx.on(cell), "Enter toggles to on");
        rx.key(root, key_ev(Key::Space, true, false));
        assert!(!rx.on(cell), "Space toggles back");
        rx.key(root, key_ev(Key::Enter, true, true)); // auto-repeat: ignored
        rx.key(root, key_ev(Key::Enter, false, false)); // key-up: ignored

        assert_eq!(
            count.get(),
            2,
            "Enter and Space each fire once; repeat/up do not"
        );
        assert!(!rx.on(cell), "repeat/up left the state unchanged");
    }

    /// A handler-less toggle still flips its visible state without panicking — a
    /// toggle with no `on_change` is still a well-formed switch.
    #[test]
    fn toggle_without_handler_still_toggles() {
        let mut rx = Reactive::new();
        let root = rx.build(toggle("Wi-Fi"));
        let cell = first_cell();

        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up)); // must not panic with no callback
        assert!(rx.on(cell), "still toggles the visible state");
        assert!(rx.store.focusable(root), "still focusable");
    }
}
