//! The [`CheckBox`] control — a two-state toggle with a label.
//!
//! A `CheckBox` is a focusable caption with a box at its start, filled and
//! ticked while it is checked. A primary-pointer click (press then release)
//! or a keyboard activation (Enter/Space while focused) flips its `checked`
//! state and fires `on_change` with the new value: the two are the same
//! action, so the callback is shared.
//!
//! Unlike a [`Button`](crate::Button), whose press is momentary, a `CheckBox`
//! *toggles*: each activation writes the inverse of its `checked` cell, so the
//! box stays ticked until the next one.
//!
//! It is one node: its text is the caption and its check's mark the box,
//! sized from the caption's first line and inked with its color. A flip
//! repaints the node alone while the box fills and its tick draws in over a
//! short ease (at once while motion is reduced). Its accessible role is
//! [`Role::CheckBox`](viso_ui::Role) named by the caption, the live checked
//! value reaching the derived semantics tree through a semantic-state
//! projection.
//!
//! ```
//! use viso_widgets::checkbox;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
//!
//! let toggle = checkbox("Enable sound").on_change(|_ev, checked| {
//!     // handle the new state — e.g. write app state through the event context
//!     let _ = checked;
//! });
//!
//! // A checkbox authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! toggle.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{BuildCx, CheckShape, Component, EventCx, Length, Rgba, Size};

use super::check::{Check, INK, SharedChange};

/// The look and size of a [`CheckBox`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckBoxStyle {
    /// The checkbox's own size request within its parent. Defaults to `Fit` on
    /// both axes (shrink to the box plus the caption).
    pub size: Size,
    /// The caption's color, which inks the box and its tick too.
    pub color: Rgba,
}

impl Default for CheckBoxStyle {
    fn default() -> Self {
        CheckBoxStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            color: INK,
        }
    }
}

/// A two-state toggle: a focusable caption with a box it ticks.
///
/// Construct one with [`checkbox`] and attach behavior with the chainable
/// setters. Each activation — a primary-pointer press then release, or
/// Enter/Space while focused — flips the checked state and fires `on_change`
/// once with the new value.
///
/// See the [module docs](self) for a build example. Invalidation: a flip
/// repaints the checkbox's one node each frame its box eases; the caption and
/// structure are static.
pub struct CheckBox {
    /// The visible caption, also used as the accessible name.
    label: String,
    /// The initial checked state (defaults to `false`).
    checked: bool,
    style: CheckBoxStyle,
    /// The shared change callback (see [`SharedChange`]). `None` until
    /// [`CheckBox::on_change`] is called; a checkbox with no handler still
    /// toggles its visible state, just without notifying anyone.
    on_change: SharedChange,
}

/// Construct a [`CheckBox`] with the given caption, unchecked, and no handler
/// yet. Chain [`CheckBox::on_change`] to give it behavior,
/// [`CheckBox::checked`] to start it checked, and [`CheckBox::style`]/
/// [`CheckBox::size`] to adjust its appearance.
pub fn checkbox(label: impl Into<String>) -> CheckBox {
    CheckBox {
        label: label.into(),
        checked: false,
        style: CheckBoxStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl CheckBox {
    /// Set the change callback, fired with the new checked value on a
    /// primary-pointer click or a keyboard activation. Replaces any previously
    /// set handler.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, bool) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the initial checked state (defaults to `false`).
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Replace the whole [`CheckBoxStyle`].
    pub fn style(mut self, style: CheckBoxStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the checkbox's own size request within its parent (defaults to `Fit`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Set the caption's color, which inks the box and its tick too.
    pub fn color(mut self, color: Rgba) -> Self {
        self.style.color = color;
        self
    }
}

impl Component for CheckBox {
    fn build(&self, cx: &mut BuildCx<'_>) {
        Check {
            shape: CheckShape::Box,
            caption: &self.label,
            checked: self.checked,
            size: self.style.size,
            color: self.style.color,
            on_change: &self.on_change,
        }
        .build(cx);
    }
}

impl crate::FormControl for CheckBox {
    fn value(&self) -> crate::FieldValue<'_> {
        crate::FieldValue::Flag(self.checked)
    }

    fn reporting(self, mut report: crate::OnValue) -> Self {
        let mut own = self.on_change.borrow_mut().take();
        self.on_change(move |ev, on| {
            if let Some(own) = own.as_mut() {
                own(ev, on);
            }
            report(ev, crate::FieldValue::Flag(on));
        })
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

    /// The reactive stores a checkbox build writes into, kept together so a test
    /// can build a checkbox and then drive its handlers against the same state.
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

        /// Build a checkbox through a reactive cx (a checkbox authors state, so a
        /// plain `BuildCx::new` would panic) and return its root node.
        fn build(&mut self, cb: CheckBox) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            cb.build(&mut cx);
            cx.root().expect("checkbox declares a root node")
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
        fn checked(&mut self, cell: StateId) -> bool {
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
        /// node's live `SemanticState`. This is what the frame does between an
        /// input transaction and an accessibility snapshot: the projection writes
        /// the node column, then the single-layer derive reads it.
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

    /// The first state cell authored by the build — the checkbox's `checked`
    /// cell (it authors exactly one). A fresh `StateStore` mints its first cell
    /// at index 0, generation 0, so allocating one cell in a throwaway store
    /// yields the very handle the checkbox's build produced (there is no public
    /// constructor for a bare `StateId`, and the build does not return it).
    fn first_cell() -> StateId {
        StateStore::new().alloc(StateValue::Bool(false))
    }

    /// A checkbox builds one interactive node: a pointer handler, a key handler,
    /// a focusable flag, a `CheckBox` semantics node named by its caption, its
    /// caption text and a box mark.
    #[test]
    fn checkbox_builds_one_focusable_caption_drawing_its_box() {
        let mut rx = Reactive::new();
        let root = rx.build(checkbox("Sound"));

        assert!(
            rx.store.has_handler(root),
            "checkbox attaches a pointer handler"
        );
        assert!(
            rx.store.has_key_handler(root),
            "checkbox attaches a key handler"
        );
        assert!(rx.store.focusable(root), "checkbox is focusable");
        assert_eq!(child_count(&rx.store, root), 0, "checkbox is one node");
        assert!(
            rx.store.text_request(root).is_some(),
            "its text is the caption"
        );
        assert_eq!(
            rx.store.check_mark(root),
            Some(CheckMark {
                shape: CheckShape::Box,
                checked: false,
            }),
            "it draws a box beside it"
        );

        let sem = rx
            .store
            .semantics(root)
            .expect("checkbox authors semantics");
        assert_eq!(sem.role, Role::CheckBox);
        assert_eq!(sem.label.as_deref(), Some("Sound"));
    }

    /// The chainable setters override size, the whole style, and the initial
    /// checked state, and the default size is `Fit` on both axes and unchecked.
    #[test]
    fn setters_override_style_default_size_is_fit_default_unchecked() {
        let default = checkbox("x");
        assert_eq!(default.style.size.width, Length::Fit);
        assert_eq!(default.style.size.height, Length::Fit);
        assert!(!default.checked, "a checkbox starts unchecked");

        let widget = checkbox("x").size(Size::fill()).checked(true);
        assert_eq!(widget.style.size, Size::fill());
        assert!(widget.checked, "checked(true) starts it checked");
    }

    /// A primary press-then-release toggles the checked cell and fires
    /// `on_change` once with the new value; a second click toggles back.
    #[test]
    fn pointer_click_toggles_checked_and_fires_change() {
        let last = Rc::new(Cell::new(None::<bool>));
        let count = Rc::new(Cell::new(0u32));
        let (l, c) = (last.clone(), count.clone());
        let mut rx = Reactive::new();
        let root = rx.build(checkbox("Sound").on_change(move |_, checked| {
            l.set(Some(checked));
            c.set(c.get() + 1);
        }));
        let cell = first_cell();

        assert!(!rx.checked(cell), "starts unchecked");

        rx.pointer(root, primary(PointerPhase::Down));
        assert_eq!(count.get(), 0, "the press alone does not toggle");

        rx.pointer(root, primary(PointerPhase::Up));
        assert!(rx.checked(cell), "the release toggles to checked");
        assert_eq!(count.get(), 1, "one activation is one change");
        assert_eq!(last.get(), Some(true), "on_change carries the new value");

        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up));
        assert!(
            !rx.checked(cell),
            "a second click toggles back to unchecked"
        );
        assert_eq!(count.get(), 2);
        assert_eq!(last.get(), Some(false), "and reports the new value");
    }

    /// A non-primary pointer button does not toggle.
    #[test]
    fn non_primary_pointer_does_not_toggle() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();
        let mut rx = Reactive::new();
        let root = rx.build(checkbox("Sound").on_change(move |_, _| c.set(c.get() + 1)));
        let cell = first_cell();

        let non_primary_up = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary(PointerPhase::Up)
        };
        rx.pointer(root, non_primary_up);
        assert!(!rx.checked(cell), "a non-primary button does not toggle");
        assert_eq!(count.get(), 0);
    }

    /// Enter and Space (pressed, not auto-repeat) each toggle the cell and fire
    /// `on_change`; an auto-repeat and a key-up do not.
    #[test]
    fn keyboard_enter_and_space_toggle_repeat_does_not() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();
        let mut rx = Reactive::new();
        let root = rx.build(checkbox("Sound").on_change(move |_, _| c.set(c.get() + 1)));
        let cell = first_cell();

        rx.key(root, key_ev(Key::Enter, true, false));
        assert!(rx.checked(cell), "Enter toggles to checked");
        rx.key(root, key_ev(Key::Space, true, false));
        assert!(!rx.checked(cell), "Space toggles back");
        rx.key(root, key_ev(Key::Enter, true, true)); // auto-repeat: ignored
        rx.key(root, key_ev(Key::Enter, false, false)); // key-up: ignored

        assert_eq!(
            count.get(),
            2,
            "Enter and Space each fire once; repeat/up do not"
        );
        assert!(!rx.checked(cell), "repeat/up left the state unchanged");
    }

    /// A handler-less checkbox still toggles its visible state without panicking
    /// — a checkbox with no `on_change` is still a well-formed toggle.
    #[test]
    fn checkbox_without_handler_still_toggles() {
        let mut rx = Reactive::new();
        let root = rx.build(checkbox("Sound"));
        let cell = first_cell();

        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up)); // must not panic with no callback
        assert!(rx.checked(cell), "still toggles the visible state");
        assert!(rx.store.focusable(root), "still focusable");
    }

    /// The live checked value reaches the derived accessibility tree: it is
    /// `checked=false` from the first frame (seeded at build, before any input),
    /// flips to `true` after a click, and back to `false` after a second click —
    /// each snapshot taken after a flush + derive, exactly as the frame runs it.
    #[test]
    fn semantics_snapshot_tracks_the_live_checked_state() {
        let mut rx = Reactive::new();
        let root = rx.build(checkbox("Sound"));

        assert_eq!(
            rx.derive_state(root),
            Some(SemanticState::checked(false)),
            "the first frame already carries the seeded unchecked state"
        );

        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up));
        assert_eq!(
            rx.derive_state(root),
            Some(SemanticState::checked(true)),
            "a click drives the derived tree to checked"
        );

        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up));
        assert_eq!(
            rx.derive_state(root),
            Some(SemanticState::checked(false)),
            "a second click drives it back to unchecked"
        );
    }

    /// A checkbox built `checked(true)` seeds the derived tree checked from the
    /// first frame — the projection reads the cell's initial value at build.
    #[test]
    fn semantics_snapshot_seeds_an_initially_checked_box() {
        let mut rx = Reactive::new();
        let root = rx.build(checkbox("Sound").checked(true));
        assert_eq!(
            rx.derive_state(root),
            Some(SemanticState::checked(true)),
            "checked(true) seeds the derived tree checked without any input"
        );
    }

    /// A flip moves the mark with the state it projects: the mark the node
    /// draws follows the cell once the flush runs.
    #[test]
    fn a_click_moves_the_mark_to_the_new_state() {
        let mut rx = Reactive::new();
        let root = rx.build(checkbox("Sound"));
        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up));
        rx.derive_state(root);
        assert_eq!(
            rx.store.check_mark(root),
            Some(CheckMark {
                shape: viso_ui::CheckShape::Box,
                checked: true,
            })
        );
        assert!(rx.store.is_transitioning(), "the mark eases over");
    }
}
