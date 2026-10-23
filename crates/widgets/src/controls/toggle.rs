//! The [`Toggle`] control — a switch: a two-state on/off toggle with a label.
//!
//! A `Toggle` is a focusable caption with a pill track at its start whose
//! knob slides to the far end while it is on. A primary-pointer click (press
//! then release) or a keyboard activation (Enter/Space while focused) flips
//! its `on` state and fires `on_change` with the new value: the two are the
//! same action, so the callback is shared (AGENTS section 15).
//!
//! A `Toggle` is the structural sibling of a [`CheckBox`](crate::CheckBox):
//! the same one node, its mark a switch rather than a box — the affordance
//! platforms use for an immediate binary setting. The track is sized from the
//! caption's first line and inked with its color; a flip repaints the node
//! alone while the knob slides over on a spring (at once while motion is
//! reduced). Its accessible role is [`Role::CheckBox`](viso_ui::Role) named by
//! the caption, the live on value reaching the derived semantics tree through
//! a semantic-state projection.
//!
//! ```
//! use viso_widgets::toggle;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
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
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! sw.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{BuildCx, CheckShape, Component, EventCx, Length, Rgba, Size};

use super::check::{Check, INK, SharedChange};

/// The look and size of a [`Toggle`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToggleStyle {
    /// The toggle's own size request within its parent. Defaults to `Fit` on
    /// both axes (shrink to the track plus the caption).
    pub size: Size,
    /// The caption's color, which inks the track and its knob too.
    pub color: Rgba,
}

impl Default for ToggleStyle {
    fn default() -> Self {
        ToggleStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            color: INK,
        }
    }
}

/// A switch: a focusable caption with a sliding knob in a pill track.
///
/// Construct one with [`toggle`] and attach behavior with the chainable setters.
/// Each activation — a primary-pointer press then release, or Enter/Space while
/// focused — flips the on state and fires `on_change` once with the new value.
///
/// See the [module docs](self) for a build example. Invalidation: a flip
/// repaints the toggle's one node each frame its knob slides; the caption and
/// structure are static.
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

    /// Set the caption's color, which inks the track and its knob too.
    pub fn color(mut self, color: Rgba) -> Self {
        self.style.color = color;
        self
    }
}

impl Component for Toggle {
    fn build(&self, cx: &mut BuildCx<'_>) {
        Check {
            shape: CheckShape::Switch,
            caption: &self.label,
            checked: self.on,
            size: self.style.size,
            color: self.style.color,
            on_change: &self.on_change,
        }
        .build(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use viso_ui::{
        BindingTable, CheckMark, Key, KeyEvent, Modifiers, NodeId, NodeStore, PointerButtons,
        PointerEvent, PointerPhase, Role, SemanticProjector, SemanticState, StateId, StateStore,
        StateValue, TextEdits, VirtualLists,
    };

    /// The reactive stores a toggle build writes into, kept together so a test can
    /// build a toggle and then drive its handlers against the same state.
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

        /// Build a toggle through a reactive cx (a toggle authors state, so a
        /// plain `BuildCx::new` would panic) and return its root node.
        fn build(&mut self, sw: Toggle) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
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

        /// Run the flush phase (drain pending state writes, wake the projections
        /// that read them) and derive the semantics tree, returning the root
        /// node's live `SemanticState`. This is exactly the frame's flush + derive
        /// sequence, so a snapshot taken here is what an assistive technology sees.
        fn derive_state(&mut self, root: NodeId) -> Option<SemanticState> {
            let mut changed = Vec::new();
            self.states.take_pending(&mut changed);
            self.projectors
                .wake(&changed, &self.states, &mut self.store);
            self.store
                .derive_semantics(root)
                .get(root)
                .and_then(|n| n.state)
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
        arena.children(parent).count()
    }

    /// The first state cell authored by the build — the toggle's `on` cell (it
    /// authors exactly one). A fresh `StateStore` mints its first cell at index 0,
    /// generation 0, so allocating one cell in a throwaway store yields the very
    /// handle the toggle's build produced (there is no public constructor for a
    /// bare `StateId`, and the build does not return it).
    fn first_cell() -> StateId {
        StateStore::new().alloc(StateValue::Bool(false))
    }

    /// A toggle builds one interactive node: a pointer handler, a key handler, a
    /// focusable flag, a `CheckBox` semantics node named by its caption, its
    /// caption text and a switch mark.
    #[test]
    fn toggle_builds_one_focusable_caption_drawing_its_track() {
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
        assert_eq!(child_count(&rx.store, root), 0, "toggle is one node");
        assert!(
            rx.store.text_request(root).is_some(),
            "its text is the caption"
        );
        assert_eq!(
            rx.store.check_mark(root),
            Some(CheckMark {
                shape: CheckShape::Switch,
                checked: false,
            }),
            "it draws a switch track beside it"
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

    /// The live on value reaches the derived accessibility tree as a checked
    /// state: `checked=false` from the first frame (seeded at build), flips to
    /// `true` after a click, and back to `false` after a second — each snapshot
    /// taken after a flush + derive, exactly as the frame runs it.
    #[test]
    fn semantics_snapshot_tracks_the_live_on_state() {
        let mut rx = Reactive::new();
        let root = rx.build(toggle("Wi-Fi"));

        assert_eq!(
            rx.derive_state(root),
            Some(SemanticState::checked(false)),
            "the first frame already carries the seeded off state"
        );

        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up));
        assert_eq!(
            rx.derive_state(root),
            Some(SemanticState::checked(true)),
            "a click drives the derived tree to on"
        );

        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up));
        assert_eq!(
            rx.derive_state(root),
            Some(SemanticState::checked(false)),
            "a second click drives it back off"
        );
    }

    /// A toggle built `on(true)` seeds the derived tree on from the first frame —
    /// the projection reads the cell's initial value at build.
    #[test]
    fn semantics_snapshot_seeds_an_initially_on_switch() {
        let mut rx = Reactive::new();
        let root = rx.build(toggle("Wi-Fi").on(true));
        assert_eq!(
            rx.derive_state(root),
            Some(SemanticState::checked(true)),
            "on(true) seeds the derived tree on without any input"
        );
    }

    /// A flip moves the mark with the state it projects: the mark the node
    /// draws follows the cell once the flush runs.
    #[test]
    fn a_click_moves_the_mark_to_the_new_state() {
        let mut rx = Reactive::new();
        let root = rx.build(toggle("Wi-Fi"));
        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up));
        rx.derive_state(root);
        assert_eq!(
            rx.store.check_mark(root),
            Some(CheckMark {
                shape: viso_ui::CheckShape::Switch,
                checked: true,
            })
        );
        assert!(rx.store.is_transitioning(), "the mark eases over");
    }
}
