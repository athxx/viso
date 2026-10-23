//! The [`RadioGroup`] control — a set of mutually exclusive options with one
//! selection.
//!
//! A radio group is "one selected value out of N". Unlike a [`CheckBox`] or a
//! [`Toggle`] — each of which owns its *own* boolean cell — every option in a
//! group shares *one* reactive `selected` cell holding the chosen index as an
//! [`StateValue::Int`]. Each option projects that cell into its semantic state
//! as `checked = (selected == index)`, which carries it to the accessibility
//! tree and moves the option's mark to it, so a selection repaints the newly-
//! and the previously-selected option alone, their marks easing over —
//! mutual exclusion is structural, not hand-coded. One `i32` drives the whole
//! group.
//!
//! Each option is one focusable node, as a [`CheckBox`] is: its caption is
//! its text and its [`CheckMark`] a circle at its start edge, filled with a
//! dot while selected; the group is a column of them. A primary-pointer click,
//! Enter or Space selects the option and fires `on_change` once with its
//! index. The group is one Tab stop (a roving group, landing on the selected
//! option); the arrows move the focus and the selection to the next or
//! previous option, wrapping, and Home and End to the first and last. An `&`
//! in a caption marks the option's access key: Alt with it takes the focus to
//! the option and selects it.
//!
//! Its accessible role is [`Role::Group`] for the container with a
//! [`Role::Radio`] per option named by its caption.
//!
//! ```
//! use viso_widgets::radio_group;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
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
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! group.build(&mut cx);
//! ```
//!
//! [`CheckBox`]: crate::CheckBox
//! [`Toggle`]: crate::Toggle

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::adaptive::LayoutDirection;
use viso_ui::{
    Access, Align, Axis, BoxStyle, BuildCx, CheckMark, CheckShape, Component, EventCx, FlexStyle,
    Inset, Justify, Key, LeafStyle, Length, NodeId, PointerButtons, PointerPhase, Rgba, Role,
    SemanticState, Semantics, Size, StateId, StateValue,
};

use super::check::INK;
use crate::controls::mnemonic::Mnemonic;

/// A shared, mutable change callback carrying the newly-selected index. It is
/// cloned into every option's pointer and key handler at build time so a pointer
/// click and a keyboard activation on any option drive the same `on_change`.
/// Pointer and keyboard input are never concurrent, so the runtime never
/// re-enters the `RefCell` borrow.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, usize)>>>>;

/// The vertical gap between options in the group's column.
const ROW_GAP: f32 = 8.0;

/// The look and size of a [`RadioGroup`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadioStyle {
    /// The group's own size request within its parent. Defaults to `Fit` on
    /// both axes (shrink to its options).
    pub size: Size,
    /// The captions' color, which inks the circles and their dots too.
    pub color: Rgba,
}

impl Default for RadioStyle {
    fn default() -> Self {
        RadioStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            color: INK,
        }
    }
}

/// A set of mutually exclusive options with a single selection.
///
/// Construct one with [`radio_group`] and attach behavior with the chainable
/// setters. Selecting an option — a primary-pointer press then release on it,
/// Enter/Space while it is focused, an arrow moving to it, or its access key —
/// writes the shared `selected` cell to that option's index and fires
/// `on_change` once with the new index.
///
/// See the [module docs](self) for a build example. Invalidation: selecting
/// writes the reactive `selected` cell, repainting the newly- and
/// previously-selected options without a rebuild; the captions and structure
/// are static.
pub struct RadioGroup {
    /// The option captions, also used as each option's accessible name; an
    /// `&` marks an option's access key.
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

    /// Set the captions' color, which inks the circles and their dots too.
    pub fn color(mut self, color: Rgba) -> Self {
        self.style.color = color;
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
/// no-op: no cell write, no callback.
fn select(cb: &SharedChange, ev: &mut EventCx<'_>, cell: StateId, index: usize) {
    let next = StateValue::Int(index as i32);
    if ev.get(cell) == Some(next) {
        return;
    }
    ev.set(cell, next);
    fire(cb, ev, index);
}

/// The option an arrow, Home or End moves to from option `index` of `count`:
/// down and toward the end edge go on, up and toward the start go back,
/// wrapping.
pub(crate) fn step(key: Key, index: usize, count: usize, rtl: bool) -> Option<usize> {
    let (end, start) = if rtl {
        (Key::Left, Key::Right)
    } else {
        (Key::Right, Key::Left)
    };
    Some(match key {
        Key::Home => 0,
        Key::End => count - 1,
        k if k == Key::Down || k == end => (index + 1) % count,
        k if k == Key::Up || k == start => (index + count - 1) % count,
        _ => return None,
    })
}

impl Component for RadioGroup {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One shared `selected` cell holding the chosen index; each option
        // projects it into its own checked state, which moves its mark.
        let selected = cx.state(StateValue::Int(self.selected as i32));
        let color = self.style.color;
        let captions: Vec<Mnemonic> = self.options.iter().map(|o| Mnemonic::of(o)).collect();
        let mut options = Vec::with_capacity(captions.len());
        let group = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: ROW_GAP,
                padding: Inset::all(0.0),
                align: Align::Start,
                justify: Justify::Start,
                size: self.style.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                for (index, caption) in captions.iter().enumerate() {
                    let option = cx.leaf(LeafStyle {
                        size: Size {
                            width: Length::Fit,
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                    });
                    cx.text_request(option, caption.label().color(color).request());
                    cx.check_mark(
                        option,
                        CheckMark {
                            shape: CheckShape::Radio,
                            checked: index == self.selected,
                        },
                    );
                    cx.semantics(
                        option,
                        Semantics::role(Role::Radio).with_label(caption.text.as_str()),
                    );
                    cx.bind_semantic_state(option, move |cx| {
                        let chosen = matches!(
                            cx.get(selected),
                            Some(StateValue::Int(i)) if i as usize == index
                        );
                        SemanticState::checked(chosen)
                    });
                    cx.focusable(option, true);
                    if let Some(key) = caption.char() {
                        cx.access_key(option, key);
                    }
                    options.push(option);
                }
            },
        );
        cx.roving(group);
        cx.semantics(group, Semantics::role(Role::Group));

        let ids: Rc<[NodeId]> = options.iter().map(|o| o.id()).collect();
        for (index, &option) in options.iter().enumerate() {
            let cb = self.on_change.clone();
            cx.on_pointer(option, move |ev| {
                let Some(p) = ev.pointer() else { return };
                if p.phase == PointerPhase::Up && p.buttons.contains(PointerButtons::PRIMARY) {
                    select(&cb, ev, selected, index);
                }
            });
            let cb = self.on_change.clone();
            let ids = Rc::clone(&ids);
            cx.on_key(option, move |ev| {
                if ev.access() == Some(Access::Key) {
                    ev.stop_propagation();
                    ev.request_focus(ids[index]);
                    select(&cb, ev, selected, index);
                    return;
                }
                let Some(k) = ev.key().copied() else { return };
                if !k.pressed {
                    return;
                }
                if matches!(k.key, Key::Enter | Key::Space) {
                    ev.stop_propagation();
                    if !k.repeat {
                        select(&cb, ev, selected, index);
                    }
                    return;
                }
                let rtl = ev.env().environment().layout_direction == LayoutDirection::Rtl;
                if let Some(next) = step(k.key, index, ids.len(), rtl) {
                    ev.stop_propagation();
                    ev.request_focus(ids[next]);
                    select(&cb, ev, selected, next);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use viso_ui::{
        BindingTable, CheckMark, CheckShape, KeyEvent, Modifiers, NodeId, NodeStore, PointerEvent,
        SemanticProjector, SemanticState, StateId, StateStore, TextEdits, VirtualLists,
    };

    /// The reactive stores a group build writes into, kept together so a test can
    /// build a group and then drive its options' handlers against the same state.
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

        /// Build a group through a reactive cx (it authors state, so a plain
        /// `BuildCx::new` would panic) and return its root (the column container).
        fn build(&mut self, group: RadioGroup) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
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

        /// Flush the pending state changes through the semantic-state projectors
        /// (the flush phase, both stores live), then derive the semantics tree and
        /// return the given node's live `SemanticState` — exactly the frame's
        /// project-then-derive order.
        fn derive_state(&mut self, node: NodeId, root: NodeId) -> Option<SemanticState> {
            let mut changed = Vec::new();
            self.states.take_pending(&mut changed);
            self.projectors
                .wake(&changed, &self.states, &mut self.store);
            self.store
                .derive_semantics(root)
                .get(node)
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
        for c in arena.children(parent) {
            out.push(c);
        }
        out
    }

    /// A group builds a column of one focusable option per caption: each a
    /// caption drawing a radio mark, checked on the seeded selection, with a
    /// pointer and a key handler. The column is one roving Tab stop.
    #[test]
    fn radio_group_builds_a_focusable_option_per_caption() {
        let mut rx = Reactive::new();
        let root = rx.build(radio_group(["Low", "Medium", "High"]).selected(1));

        let rows = children(&rx.store, root);
        assert_eq!(rows.len(), 3, "one option per caption");
        assert!(rx.store.is_roving(root), "the group is one Tab stop");
        for (index, row) in rows.iter().enumerate() {
            assert!(rx.store.has_handler(*row));
            assert!(rx.store.has_key_handler(*row));
            assert!(rx.store.focusable(*row), "each option is focusable");
            assert!(children(&rx.store, *row).is_empty(), "one node");
            assert_eq!(
                rx.store.check_mark(*row),
                Some(CheckMark {
                    shape: CheckShape::Radio,
                    checked: index == 1,
                })
            );
        }
    }

    /// The arrows move the selection to the next or previous option, wrapping,
    /// and Home and End to the ends.
    #[test]
    fn the_arrows_move_the_selection_and_wrap() {
        let last = Rc::new(Cell::new(None::<usize>));
        let l = last.clone();
        let mut rx = Reactive::new();
        let root =
            rx.build(radio_group(["A", "B", "C"]).on_change(move |_ev, index| l.set(Some(index))));
        let cell = shared_cell();
        let rows = children(&rx.store, root);

        rx.key(rows[0], key_ev(Key::Down, true, false));
        assert_eq!(rx.selected(cell), Some(1));
        rx.key(rows[1], key_ev(Key::Right, true, true));
        assert_eq!(rx.selected(cell), Some(2), "a held arrow walks on");
        rx.key(rows[2], key_ev(Key::Down, true, false));
        assert_eq!(rx.selected(cell), Some(0), "wrapping");
        rx.key(rows[0], key_ev(Key::Up, true, false));
        assert_eq!(rx.selected(cell), Some(2));
        rx.key(rows[2], key_ev(Key::Home, true, false));
        assert_eq!(last.get(), Some(0));
        rx.key(rows[0], key_ev(Key::End, true, false));
        assert_eq!(last.get(), Some(2));
    }

    /// The container's semantics is `Group`; each option's is `Radio` named by
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
            assert_eq!(sem.role, Role::Radio, "each option is a Radio role");
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

    /// The live selection reaches the derived accessibility tree: exactly one
    /// option carries `checked = true`, the rest `false`, and selecting a
    /// different option moves that checked flag — mutual exclusion is visible in
    /// the semantics, not just the paint. Each snapshot is taken after a flush +
    /// derive, exactly as the frame runs it.
    #[test]
    fn semantics_snapshot_tracks_the_live_selection() {
        let mut rx = Reactive::new();
        let root = rx.build(radio_group(["Low", "Medium", "High"]).selected(1));
        let cell = shared_cell();
        let rows = children(&rx.store, root);

        // Seeded selection: option 1 is checked, the others are not.
        let checked: Vec<Option<bool>> = rows
            .iter()
            .map(|row| rx.derive_state(*row, root).and_then(|s| s.checked))
            .collect();
        assert_eq!(
            checked,
            vec![Some(false), Some(true), Some(false)],
            "the seeded selection marks exactly the chosen option checked"
        );

        // Select the third option; the checked flag moves off option 1 onto it.
        rx.pointer(rows[2], primary(PointerPhase::Down));
        rx.pointer(rows[2], primary(PointerPhase::Up));
        assert_eq!(rx.selected(cell), Some(2), "the shared cell moved to 2");

        let checked: Vec<Option<bool>> = rows
            .iter()
            .map(|row| rx.derive_state(*row, root).and_then(|s| s.checked))
            .collect();
        assert_eq!(
            checked,
            vec![Some(false), Some(false), Some(true)],
            "selecting a new option moves the checked flag — mutual exclusion is \
             visible in the derived semantics"
        );
    }
}
