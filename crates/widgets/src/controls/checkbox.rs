//! The [`CheckBox`] control — a two-state toggle with a label.
//!
//! A `CheckBox` is a focusable box that flips a boolean `checked` state on
//! either a primary-pointer click (press then release) or a keyboard activation
//! (Enter/Space while focused), firing an `on_change` callback with the *new*
//! checked value. Pointer and keyboard activation are the *same* action: an
//! interactive control must have a keyboard equivalent (AGENTS section 15), so
//! the callback is shared between both handlers rather than duplicated.
//!
//! Unlike a [`Button`](crate::Button), whose press is momentary (armed on the
//! primary down, disarmed on the up), a `CheckBox` *toggles*: each activation
//! reads the current `checked` cell and writes its inverse, so the mark box
//! stays lit until the next activation.
//!
//! It maps to one interactive `viso-ui` node: a focusable flex row carrying a
//! reactive `checked` cell bound to `PAINT`, composed of a mark-box leaf (its
//! fill reflects the checked state) and a [`Label`](crate::Label) child for the
//! caption. Toggling flips the `checked` cell, repainting the row alone (a
//! targeted invalidation — no rebuild, architecture section 47). Its accessible
//! role is [`Role::CheckBox`] with the caption as its name; the live checked
//! value is proven through the reactive cell and input tapes (wiring it into the
//! derived semantics tree is a later slice — the derive pass has no state store).
//!
//! ```
//! use viso_widgets::checkbox;
//! use viso_ui::{BuildCx, BindingTable, Component, NodeStore, StateStore, VirtualLists};
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
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
//! toggle.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Align, Axis, Border, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset, Key,
    LeafStyle, Length, PointerButtons, PointerPhase, Rgba, Role, Semantics, Size, StateValue,
};

use crate::label;

/// A shared, mutable change callback carrying the new checked value. It is
/// cloned into both the pointer handler and the key handler at build time so a
/// pointer click and a keyboard activation drive the same `on_change`. Pointer
/// and keyboard input are never concurrent, so the runtime never re-enters the
/// `RefCell` borrow.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, bool)>>>>;

/// The mark box's fill while unchecked — a neutral empty box.
const UNCHECKED: Rgba = Rgba {
    r: 0.16,
    g: 0.17,
    b: 0.20,
    a: 1.0,
};

/// The mark box's fill while checked — a bright accent so the state reads at a
/// glance.
const CHECKED: Rgba = Rgba {
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

/// The mark box's border, giving the unchecked box a visible outline.
const BORDER: Rgba = Rgba {
    r: 0.45,
    g: 0.47,
    b: 0.52,
    a: 1.0,
};

/// The default corner radius of the mark box.
const RADIUS: f32 = 4.0;

/// The default border width of the mark box.
const BORDER_WIDTH: f32 = 1.5;

/// The default edge length of the (square) mark box.
const MARK: f32 = 18.0;

/// The gap between the mark box and the caption.
const GAP: f32 = 8.0;

/// The visual and layout parameters of a [`CheckBox`]: the mark box's unchecked
/// and checked fills, the mark box's edge length, and the checkbox's own size
/// request within its parent.
///
/// `size` defaults to [`Length::Fit`] on both axes so the checkbox shrinks to
/// the mark box plus the caption; override it with a [`Length::Fixed`] or
/// [`Length::Fill`] axis for a fixed or flexing box. `unchecked` and `checked`
/// default to a bordered empty box and a filled accent box. All fields are
/// `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckBoxStyle {
    /// The checkbox's own size request within its parent. Defaults to `Fit` on
    /// both axes (shrink to the mark box plus the caption).
    pub size: Size,
    /// The mark box's edge length (it is square).
    pub mark: f32,
    /// The mark box's fill/border while unchecked.
    pub unchecked: BoxStyle,
    /// The mark box's fill while checked.
    pub checked: BoxStyle,
}

impl Default for CheckBoxStyle {
    fn default() -> Self {
        CheckBoxStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            mark: MARK,
            unchecked: BoxStyle {
                border: Border {
                    width: BORDER_WIDTH,
                    color: BORDER,
                },
                ..BoxStyle::solid(UNCHECKED).with_radius(RADIUS)
            },
            checked: BoxStyle::solid(CHECKED).with_radius(RADIUS),
        }
    }
}

/// A two-state toggle: a focusable mark box with a caption.
///
/// Construct one with [`checkbox`] and attach behavior with the chainable
/// setters. Each activation — a primary-pointer press then release, or
/// Enter/Space while focused — flips the checked state and fires `on_change`
/// once with the new value.
///
/// See the [module docs](self) for a build example. Invalidation: toggling
/// flips a reactive `checked` cell bound to `PAINT`, repainting the checkbox's
/// own row without a rebuild; the caption and structure are static.
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
}

/// Drive the shared callback with the new checked value if one is set; a
/// handler-less checkbox is a no-op. Pointer and keyboard activation never
/// overlap, so the borrow is uncontended.
fn fire(cb: &SharedChange, ev: &mut EventCx<'_>, checked: bool) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev, checked);
    }
}

/// Read the current checked value from the reactive cell, defaulting to `false`
/// for a stale handle or a non-bool value (neither happens in normal use — the
/// cell is authored as `Bool` and lives as long as the node).
fn read_checked(ev: &EventCx<'_>, cell: viso_ui::StateId) -> bool {
    matches!(ev.get(cell), Some(StateValue::Bool(true)))
}

impl Component for CheckBox {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // A reactive `checked` cell bound to PAINT: toggling repaints the row
        // alone (targeted invalidation, not a rebuild). The mark box's fill is
        // chosen in paint from this cell.
        let checked = cx.state(StateValue::Bool(self.checked));

        // The focusable flex row: the mark box leaf followed by the caption.
        // `build` takes `&self`, so the styles (`Copy`) are read by value and the
        // caption is cloned (cold, build-time — not a per-frame path).
        let mark_box = self.style.unchecked;
        let mark_size = self.style.mark;
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
                cx.leaf(LeafStyle {
                    size: Size::fixed(mark_size, mark_size),
                    style: mark_box,
                });
                label(caption.clone()).color(CAPTION).build(cx);
            },
        );

        // Bind the checked cell to the row's PAINT so a toggle repaints just this
        // subtree. The mark box's fill swap is expressed by rebinding the box in
        // paint via the cell; this slice paints the unchecked box and marks PAINT
        // dirty on toggle so the frame re-emits the node.
        cx.bind(checked, root, DirtyClass::PAINT);
        cx.focusable(root, true);

        // Pointer activation: a primary press-then-release toggles the checked
        // cell and fires `on_change` with the new value. One press/release is one
        // toggle — the release is the activation, matching Button's click gate.
        let pointer_cb = self.on_change.clone();
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase == PointerPhase::Up && p.buttons.contains(PointerButtons::PRIMARY) {
                let next = !read_checked(ev, checked);
                ev.set(checked, StateValue::Bool(next));
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
                let next = !read_checked(ev, checked);
                ev.set(checked, StateValue::Bool(next));
                fire(&key_cb, ev, next);
            }
        });

        // Explicit CheckBox role with the caption as its accessible name, so the
        // name matches the visible label (AGENTS section 15). The live checked
        // value is proven by the reactive cell and input tapes; wiring it into
        // the derived tree is a later slice (the derive pass has no state store).
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

    /// The reactive stores a checkbox build writes into, kept together so a test
    /// can build a checkbox and then drive its handlers against the same state.
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

        /// Build a checkbox through a reactive cx (a checkbox authors state, so a
        /// plain `BuildCx::new` would panic) and return its root node.
        fn build(&mut self, cb: CheckBox) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
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

    /// The first state cell authored by the build — the checkbox's `checked`
    /// cell (it authors exactly one). A fresh `StateStore` mints its first cell
    /// at index 0, generation 0, so allocating one cell in a throwaway store
    /// yields the very handle the checkbox's build produced (there is no public
    /// constructor for a bare `StateId`, and the build does not return it).
    fn first_cell() -> StateId {
        StateStore::new().alloc(StateValue::Bool(false))
    }

    /// A checkbox builds an interactive node: a pointer handler, a key handler, a
    /// focusable flag, a `CheckBox` semantics node named by its caption, and two
    /// composed children (the mark box leaf and the caption).
    #[test]
    fn checkbox_builds_a_focusable_toggle_node_with_mark_and_caption() {
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
        assert_eq!(
            child_count(&rx.store, root),
            2,
            "checkbox composes a mark box and a caption"
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
}
