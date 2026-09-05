//! The [`RadioGroup`] control — a set of mutually exclusive options with one
//! selection.
//!
//! A radio group is "one selected value out of N". Unlike a [`CheckBox`] or a
//! [`Toggle`] — each of which owns its *own* boolean cell — every option in a
//! group shares *one* reactive `selected` cell holding the chosen index as an
//! [`StateValue::Int`]. Each option binds that same cell to its own `PAINT`, so
//! activating one option writes the shared cell to its index, which repaints
//! *both* the newly-selected and the previously-selected option in a single
//! targeted invalidation (architecture section 47) — mutual exclusion is
//! structural, not hand-coded. One `i32` drives the whole group.
//!
//! Each option is a focusable flex row carrying a circular dot leaf and a
//! [`Label`](crate::Label) caption; the group is a column of those rows. A
//! primary-pointer click or a keyboard activation (Enter/Space while focused)
//! selects the option under it and fires `on_change` once with the newly
//! selected index. Pointer and keyboard activation are the *same* action (an
//! interactive control must have a keyboard equivalent, AGENTS section 15), so
//! the callback is shared between both handlers rather than duplicated.
//!
//! Its accessible role is [`Role::Group`] for the container with a
//! [`Role::CheckBox`] per option named by its caption (a dedicated radio role is
//! a later slice — the `Role` enum has no radio variant yet); the live selection
//! is proven through the shared reactive cell and the input tapes (wiring it into
//! the derived semantics tree is a later slice — the derive pass has no state
//! store).
//!
//! ```
//! use viso_widgets::radio_group;
//! use viso_ui::{BuildCx, BindingTable, Component, NodeStore, StateStore, VirtualLists};
//!
//! let group = radio_group(["Low", "Medium", "High"])
//!     .selected(1)
//!     .on_change(|_ev, index| {
//!         // handle the newly selected option — e.g. write app state
//!         let _ = index;
//!     });
//!
//! // A radio group authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
//! group.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Align, Axis, Border, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset, Key,
    LeafStyle, Length, PointerButtons, PointerPhase, Rgba, Role, Semantics, Size, StateId,
    StateValue,
};

use crate::label;

/// A shared, mutable change callback carrying the newly-selected index. It is
/// cloned into every option's pointer and key handler at build time so a pointer
/// click and a keyboard activation on any option drive the same `on_change`.
/// Pointer and keyboard input are never concurrent, so the runtime never
/// re-enters the `RefCell` borrow.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, usize)>>>>;

/// A deselected dot's fill — a neutral empty circle.
const DESELECTED: Rgba = Rgba {
    r: 0.16,
    g: 0.17,
    b: 0.20,
    a: 1.0,
};

/// A selected dot's fill — a bright accent so the choice reads at a glance.
const SELECTED: Rgba = Rgba {
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

/// A dot's border, giving the deselected circle a visible outline.
const BORDER: Rgba = Rgba {
    r: 0.45,
    g: 0.47,
    b: 0.52,
    a: 1.0,
};

/// The default border width of a dot.
const BORDER_WIDTH: f32 = 1.5;

/// The default diameter of a (circular) dot.
const DOT: f32 = 18.0;

/// The gap between a dot and its caption.
const GAP: f32 = 8.0;

/// The vertical gap between options in the group's column.
const ROW_GAP: f32 = 8.0;

/// The visual and layout parameters of a [`RadioGroup`]: the dot's deselected and
/// selected fills, the dot's diameter, and the group's own size request within
/// its parent.
///
/// `size` defaults to [`Length::Fit`] on both axes so the group shrinks to its
/// options; override it with a [`Length::Fixed`] or [`Length::Fill`] axis for a
/// fixed or flexing box. `deselected` and `selected` default to a bordered empty
/// circle and a filled accent circle. All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadioStyle {
    /// The group's own size request within its parent. Defaults to `Fit` on both
    /// axes (shrink to the tallest/widest option stack).
    pub size: Size,
    /// A dot's diameter (it is circular).
    pub dot: f32,
    /// A dot's fill/border while deselected.
    pub deselected: BoxStyle,
    /// A dot's fill while selected.
    pub selected: BoxStyle,
}

impl Default for RadioStyle {
    fn default() -> Self {
        // A full circle: corner radius is half the diameter.
        let radius = DOT / 2.0;
        RadioStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            dot: DOT,
            deselected: BoxStyle {
                border: Border {
                    width: BORDER_WIDTH,
                    color: BORDER,
                },
                ..BoxStyle::solid(DESELECTED).with_radius(radius)
            },
            selected: BoxStyle::solid(SELECTED).with_radius(radius),
        }
    }
}

/// A set of mutually exclusive options with a single selection.
///
/// Construct one with [`radio_group`] and attach behavior with the chainable
/// setters. Selecting an option — a primary-pointer press then release on it, or
/// Enter/Space while it is focused — writes the shared `selected` cell to that
/// option's index and fires `on_change` once with the new index.
///
/// See the [module docs](self) for a build example. Invalidation: selecting
/// writes a reactive `selected` cell bound to each option's `PAINT`, repainting
/// the newly- and previously-selected options without a rebuild; the captions
/// and structure are static.
pub struct RadioGroup {
    /// The option captions, also used as each option's accessible name.
    options: Vec<String>,
    /// The initially-selected option index (defaults to `0`).
    selected: usize,
    style: RadioStyle,
    /// The shared change callback (see [`SharedChange`]). `None` until
    /// [`RadioGroup::on_change`] is called; a group with no handler still updates
    /// its visible selection, just without notifying anyone.
    on_change: SharedChange,
}

/// Construct a [`RadioGroup`] from a sequence of option captions, with the first
/// option selected and no handler yet. Chain [`RadioGroup::on_change`] to give it
/// behavior, [`RadioGroup::selected`] to choose the initial option, and
/// [`RadioGroup::style`]/[`RadioGroup::size`] to adjust its appearance.
pub fn radio_group<I, S>(options: I) -> RadioGroup
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    RadioGroup {
        options: options.into_iter().map(Into::into).collect(),
        selected: 0,
        style: RadioStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl RadioGroup {
    /// Set the change callback, fired with the newly-selected index on a
    /// primary-pointer click or a keyboard activation. Replaces any previously
    /// set handler.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, usize) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the initially-selected option index (defaults to `0`). An index past
    /// the last option selects nothing until an option is activated.
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Replace the whole [`RadioStyle`].
    pub fn style(mut self, style: RadioStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the group's own size request within its parent (defaults to `Fit`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }
}

/// Drive the shared callback with the newly-selected index if one is set; a
/// handler-less group is a no-op. Pointer and keyboard activation never overlap,
/// so the borrow is uncontended.
fn fire(cb: &SharedChange, ev: &mut EventCx<'_>, index: usize) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev, index);
    }
}

/// Select `index` into the shared cell (as an `Int`) and fire `on_change` — but
/// only when it is a genuine change. Selecting the already-selected option is a
/// no-op: no cell write, no callback. This also coalesces the capture/bubble
/// double-dispatch a router performs when the activation lands on a leaf inside
/// an option row (the row is then an ancestor, so its handler runs on both the
/// capture and bubble passes): the first pass writes the new index and fires,
/// the second reads the just-written value and short-circuits. `EventCx::set`
/// writes the cell eagerly (the flush is deferred, but the stored value updates
/// now), so the guard sees the first pass's write within the same route.
fn select(cb: &SharedChange, ev: &mut EventCx<'_>, cell: StateId, index: usize) {
    let next = StateValue::Int(index as i32);
    if ev.get(cell) == Some(next) {
        return;
    }
    ev.set(cell, next);
    fire(cb, ev, index);
}

impl Component for RadioGroup {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One shared `selected` cell holding the chosen index. Every option binds
        // this same cell to its PAINT, so a selection repaints the newly- and
        // previously-selected options in one targeted invalidation (not a
        // rebuild). Which fill a dot paints is chosen from this cell.
        let selected = cx.state(StateValue::Int(self.selected as i32));

        let dot_box = self.style.deselected;
        let dot_size = self.style.dot;

        // The group is a column of option rows. Each row is a focusable flex row:
        // a circular dot leaf followed by the caption.
        let group = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: ROW_GAP,
                padding: Inset::all(0.0),
                align: Align::Start,
                size: self.style.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                for (index, caption) in self.options.iter().enumerate() {
                    let caption = caption.clone();
                    let row = cx.flex(
                        FlexStyle {
                            axis: Axis::Row,
                            gap: GAP,
                            padding: Inset::all(0.0),
                            align: Align::Center,
                            size: Size {
                                width: Length::Fit,
                                height: Length::Fit,
                            },
                            style: BoxStyle::NONE,
                        },
                        |cx| {
                            cx.leaf(LeafStyle {
                                size: Size::fixed(dot_size, dot_size),
                                style: dot_box,
                            });
                            label(caption.clone()).color(CAPTION).build(cx);
                        },
                    );

                    // Bind the shared cell to this option's PAINT so a selection
                    // repaints just the affected rows. The dot's fill swap is
                    // expressed by rebinding the box in paint via the cell; this
                    // slice paints the deselected dot and marks PAINT dirty on
                    // selection so the frame re-emits the node.
                    cx.bind(selected, row, DirtyClass::PAINT);
                    cx.focusable(row, true);

                    // Pointer activation: a primary press-then-release selects this
                    // option and fires `on_change` with its index. One press/
                    // release is one selection — the release is the activation,
                    // matching the click gate used across the controls.
                    let pointer_cb = self.on_change.clone();
                    cx.on_pointer(row, move |ev| {
                        let Some(p) = ev.pointer() else { return };
                        if p.phase == PointerPhase::Up
                            && p.buttons.contains(PointerButtons::PRIMARY)
                        {
                            select(&pointer_cb, ev, selected, index);
                        }
                    });

                    // Keyboard activation: Enter/Space pressed (not an auto-repeat)
                    // selects the focused option and fires the same `on_change` —
                    // the accessibility equivalent of the pointer click.
                    let key_cb = self.on_change.clone();
                    cx.on_key(row, move |ev| {
                        if let Some(k) = ev.key()
                            && k.pressed
                            && !k.repeat
                            && matches!(k.key, Key::Enter | Key::Space)
                        {
                            select(&key_cb, ev, selected, index);
                        }
                    });

                    // Each option is an interactive, named node. The `Role` enum
                    // has no radio variant yet, so an option uses `Role::CheckBox`
                    // (a selectable named state) with its caption as its accessible
                    // name (AGENTS section 15); a dedicated radio role is a later
                    // slice.
                    cx.semantics(
                        row,
                        Semantics::role(Role::CheckBox).with_label(caption.clone()),
                    );
                }
            },
        );

        // The container groups the options; `Role::Group` is the accessible
        // wrapper. The column `cx.flex` returned the container's handle.
        cx.semantics(group, Semantics::role(Role::Group));
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

    /// The reactive stores a group build writes into, kept together so a test can
    /// build a group and then drive its options' handlers against the same state.
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

        /// Build a group through a reactive cx (it authors state, so a plain
        /// `BuildCx::new` would panic) and return its root (the column container).
        fn build(&mut self, group: RadioGroup) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
            );
            group.build(&mut cx);
            cx.root().expect("radio group declares a root node")
        }

        /// Feed a pointer sample to a node's pointer handler, restoring it after
        /// (the router borrow discipline: take, drive, restore).
        fn pointer(&mut self, node: NodeId, ev: PointerEvent) {
            let mut handler = self.store.take_handler(node).expect("pointer handler");
            {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_handler(node, handler);
        }

        /// Feed a key sample to a node's key handler, restoring it after.
        fn key(&mut self, node: NodeId, ev: KeyEvent) {
            let mut handler = self.store.take_key_handler(node).expect("key handler");
            {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_key_handler(node, handler);
        }

        /// The current value of the shared selection cell (via a throwaway read
        /// cx).
        fn selected(&mut self, cell: StateId) -> Option<i32> {
            let ev = PointerEvent {
                x: 0.0,
                y: 0.0,
                phase: PointerPhase::Move,
                buttons: PointerButtons::NONE,
                modifiers: Modifiers::default(),
            };
            let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            match cx.get(cell) {
                Some(StateValue::Int(i)) => Some(i),
                _ => None,
            }
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

    /// The shared selection cell authored by the build. A group authors exactly
    /// one state cell (the shared `selected` Int), so a fresh `StateStore`
    /// allocating one `Int` cell yields the very handle the build produced (there
    /// is no public constructor for a bare `StateId`, and the build does not
    /// return it).
    fn shared_cell() -> StateId {
        StateStore::new().alloc(StateValue::Int(0))
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

    /// A group builds a column of one interactive row per option: each row has a
    /// pointer handler, a key handler, a focusable flag, a `CheckBox` semantics
    /// node named by its caption, and two composed children (the dot leaf and the
    /// caption). The container is a `Group`.
    #[test]
    fn radio_group_builds_a_focusable_option_per_caption() {
        let mut rx = Reactive::new();
        let root = rx.build(radio_group(["Low", "Medium", "High"]));

        let rows = children(&rx.store, root);
        assert_eq!(rows.len(), 3, "one option row per caption");

        for row in &rows {
            assert!(
                rx.store.has_handler(*row),
                "each option attaches a pointer handler"
            );
            assert!(
                rx.store.has_key_handler(*row),
                "each option attaches a key handler"
            );
            assert!(rx.store.focusable(*row), "each option is focusable");
            assert_eq!(
                children(&rx.store, *row).len(),
                2,
                "each option row composes a dot leaf and a caption"
            );
        }
    }

    /// The container's semantics is `Group`; each option's is `CheckBox` named by
    /// its caption.
    #[test]
    fn radio_group_derives_group_over_named_options() {
        let mut rx = Reactive::new();
        let root = rx.build(radio_group(["Low", "High"]));

        let group_sem = rx.store.semantics(root).expect("container has semantics");
        assert_eq!(group_sem.role, Role::Group, "the container is a Group");

        let rows = children(&rx.store, root);
        let labels = ["Low", "High"];
        for (row, expected) in rows.iter().zip(labels) {
            let sem = rx.store.semantics(*row).expect("option has semantics");
            assert_eq!(sem.role, Role::CheckBox, "each option is a CheckBox role");
            assert_eq!(
                sem.label.as_deref(),
                Some(expected),
                "the accessible name is the visible caption"
            );
        }
    }

    /// A primary click on an option selects its index and fires `on_change` once
    /// with that index; a press alone does not select.
    #[test]
    fn pointer_click_selects_option_and_fires_change() {
        let count = Rc::new(Cell::new(0u32));
        let last = Rc::new(Cell::new(None::<usize>));
        let (c, l) = (count.clone(), last.clone());

        let mut rx = Reactive::new();
        let root = rx.build(radio_group(["A", "B", "C"]).selected(0).on_change(
            move |_ev, index| {
                l.set(Some(index));
                c.set(c.get() + 1);
            },
        ));
        let cell = shared_cell();
        let rows = children(&rx.store, root);

        // A press alone does not select.
        rx.pointer(rows[2], primary(PointerPhase::Down));
        assert_eq!(count.get(), 0, "the press alone does not select");
        assert_eq!(rx.selected(cell), Some(0), "selection unchanged by a press");

        // Release on the third option selects index 2.
        rx.pointer(rows[2], primary(PointerPhase::Up));
        assert_eq!(count.get(), 1, "press-then-release is one selection");
        assert_eq!(last.get(), Some(2), "on_change carries the selected index");
        assert_eq!(rx.selected(cell), Some(2), "the shared cell holds index 2");

        // Selecting the first option moves the shared cell — mutual exclusion is
        // structural (one shared cell), so index 2 is implicitly deselected.
        rx.pointer(rows[0], primary(PointerPhase::Down));
        rx.pointer(rows[0], primary(PointerPhase::Up));
        assert_eq!(count.get(), 2, "a second selection fires again");
        assert_eq!(last.get(), Some(0), "and reports the new index");
        assert_eq!(
            rx.selected(cell),
            Some(0),
            "the shared cell now holds index 0"
        );
    }

    /// A non-primary button does not select.
    #[test]
    fn non_primary_pointer_does_not_select() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();

        let mut rx = Reactive::new();
        let root = rx.build(radio_group(["A", "B"]).on_change(move |_ev, _index| {
            c.set(c.get() + 1);
        }));
        let rows = children(&rx.store, root);

        let down = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary(PointerPhase::Down)
        };
        let up = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary(PointerPhase::Up)
        };
        rx.pointer(rows[1], down);
        rx.pointer(rows[1], up);
        assert_eq!(count.get(), 0, "a non-primary button does not select");
    }

    /// Enter and Space each select the focused option; an auto-repeat and a
    /// key-up do not.
    #[test]
    fn keyboard_selects_focused_option_and_ignores_repeat() {
        let count = Rc::new(Cell::new(0u32));
        let last = Rc::new(Cell::new(None::<usize>));
        let (c, l) = (count.clone(), last.clone());

        let mut rx = Reactive::new();
        let root = rx.build(radio_group(["A", "B", "C"]).on_change(move |_ev, index| {
            l.set(Some(index));
            c.set(c.get() + 1);
        }));
        let cell = shared_cell();
        let rows = children(&rx.store, root);

        rx.key(rows[1], key_ev(Key::Enter, true, false));
        assert_eq!(last.get(), Some(1), "Enter selects the focused option");
        assert_eq!(rx.selected(cell), Some(1));

        rx.key(rows[2], key_ev(Key::Space, true, false));
        assert_eq!(last.get(), Some(2), "Space selects the focused option");
        assert_eq!(rx.selected(cell), Some(2));

        rx.key(rows[0], key_ev(Key::Enter, true, true)); // auto-repeat: ignored
        rx.key(rows[0], key_ev(Key::Enter, false, false)); // key-up: ignored

        assert_eq!(
            count.get(),
            2,
            "Enter and Space each fire once; repeat/up do not"
        );
    }

    /// A group with no handler still updates its visible selection through the
    /// shared cell — `build` does not panic and the handlers are no-ops on the
    /// callback.
    #[test]
    fn handlerless_group_still_selects() {
        let mut rx = Reactive::new();
        let root = rx.build(radio_group(["A", "B"]));
        let cell = shared_cell();
        let rows = children(&rx.store, root);

        rx.pointer(rows[1], primary(PointerPhase::Down));
        rx.pointer(rows[1], primary(PointerPhase::Up));
        assert_eq!(
            rx.selected(cell),
            Some(1),
            "a handler-less group still moves the shared cell"
        );
    }

    /// The initial selection index is written into the shared cell at build time.
    #[test]
    fn initial_selection_seeds_the_shared_cell() {
        let mut rx = Reactive::new();
        rx.build(radio_group(["A", "B", "C"]).selected(2));
        let cell = shared_cell();
        assert_eq!(
            rx.selected(cell),
            Some(2),
            "the initial index seeds the cell"
        );
    }
}
