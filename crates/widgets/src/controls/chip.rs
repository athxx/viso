//! The [`Chip`] control — a compact caption on a rounded body: a filter
//! that selects, an input that removes, or an action that clicks.
//!
//! A chip is one focusable node: its text is the caption and its check's
//! mark, a [`CheckShape::Chip`], the body over the whole node — outlined
//! while unselected, tinted with a tick growing in before the caption while
//! selected, the chip widening to make room — and, on a removable one, a
//! cross at its end edge.
//!
//! A primary press then release on the body, Enter or Space while focused,
//! or its access key activates it: a selectable chip flips its `selected`
//! cell and fires `on_change` with the new value, and every chip fires
//! `on_click`. A release on the cross, or Delete or Backspace while focused,
//! fires `on_remove` instead; an assistive technology is offered the same as
//! a "Remove" action. A selectable chip's role is
//! [`Role::CheckBox`](viso_ui::Role) carrying its selection, any other's
//! [`Role::Button`](viso_ui::Role), named by its caption.
//!
//! ```
//! use viso_widgets::chip;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
//!
//! let filter = chip("Vegetarian").selected(false).on_change(|_ev, selected| {
//!     let _ = selected;
//! });
//! let tag = chip("rust").on_remove(|_ev| {});
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! filter.build(&mut cx);
//! tag.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Access, BoxStyle, BuildCx, CheckMark, CheckShape, Component, EventCx, Key, LeafStyle, Length,
    PointerButtons, PointerPhase, Rgba, Role, SemanticState, Semantics, Size, StateId, StateValue,
};

use super::check::{INK, SharedChange};
use crate::controls::mnemonic::Mnemonic;

/// A shared, mutable callback with no payload, cloned into the pointer and
/// the key handler so both drive the same one.
type SharedSignal = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>)>>>>;

/// The look and size of a [`Chip`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChipStyle {
    /// The chip's own size request within its parent. Defaults to `Fit` on
    /// both axes: its caption with the room round it.
    pub size: Size,
    /// The caption's color, which inks the body, its tick and its cross too.
    pub color: Rgba,
}

impl Default for ChipStyle {
    fn default() -> Self {
        ChipStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            color: INK,
        }
    }
}

/// A compact caption on a rounded body that selects, removes or clicks.
///
/// Construct one with [`chip`]. [`Chip::selected`] or [`Chip::on_change`]
/// makes it selectable, [`Chip::on_remove`] gives it a cross, and
/// [`Chip::on_click`] hears every activation of its body.
///
/// See the [module docs](self) for a build example. Invalidation: a flip
/// re-measures and repaints the chip's one node each frame its tick grows
/// or shrinks; the caption and structure are static.
pub struct Chip {
    /// The visible caption, also used as the accessible name.
    label: String,
    /// The initial selection; `None` for a chip that does not select.
    selected: Option<bool>,
    style: ChipStyle,
    on_change: SharedChange,
    on_click: SharedSignal,
    on_remove: SharedSignal,
    removable: bool,
}

/// Construct a [`Chip`] with the given caption: neither selectable nor
/// removable, and no handler yet.
pub fn chip(label: impl Into<String>) -> Chip {
    Chip {
        label: label.into(),
        selected: None,
        style: ChipStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
        on_click: Rc::new(RefCell::new(None)),
        on_remove: Rc::new(RefCell::new(None)),
        removable: false,
    }
}

impl Chip {
    /// Make the chip selectable, starting `selected`.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }

    /// Set the change callback, fired with the new selection on each
    /// activation; it makes the chip selectable, unselected unless
    /// [`Chip::selected`] says otherwise.
    pub fn on_change(mut self, handler: impl FnMut(&mut EventCx<'_>, bool) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self.selected.get_or_insert(false);
        self
    }

    /// Set the click callback, fired on each activation of the body.
    pub fn on_click(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_click.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Give the chip a cross and set the callback a release on it, or
    /// Delete or Backspace while focused, fires.
    pub fn on_remove(mut self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_remove.borrow_mut() = Some(Box::new(handler));
        self.removable = true;
        self
    }

    /// Replace the whole [`ChipStyle`].
    pub fn style(mut self, style: ChipStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the chip's own size request within its parent (defaults to `Fit`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Set the caption's color, which inks the body, its tick and its cross.
    pub fn color(mut self, color: Rgba) -> Self {
        self.style.color = color;
        self
    }
}

/// Fires a signal callback, if one is set.
fn signal(cb: &SharedSignal, ev: &mut EventCx<'_>) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev);
    }
}

/// What activating a chip's body runs: a selectable one flips its cell and
/// reports the new selection, then every one reports the click.
#[derive(Clone)]
struct Activate {
    cell: Option<StateId>,
    on_change: SharedChange,
    on_click: SharedSignal,
}

impl Activate {
    fn run(&self, ev: &mut EventCx<'_>) {
        if let Some(cell) = self.cell {
            let next = !matches!(ev.get(cell), Some(StateValue::Bool(true)));
            ev.set(cell, StateValue::Bool(next));
            if let Some(f) = self.on_change.borrow_mut().as_mut() {
                f(ev, next);
            }
        }
        signal(&self.on_click, ev);
    }
}

impl Component for Chip {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let cell = self.selected.map(|s| cx.state(StateValue::Bool(s)));
        let node = cx.leaf(LeafStyle {
            size: self.style.size,
            style: BoxStyle::NONE,
        });
        let caption = Mnemonic::of(&self.label);
        cx.text_request(node, caption.label().color(self.style.color).request());
        cx.check_mark(
            node,
            CheckMark {
                shape: CheckShape::Chip {
                    removable: self.removable,
                },
                checked: self.selected.unwrap_or(false),
            },
        );
        let role = if cell.is_some() {
            Role::CheckBox
        } else {
            Role::Button
        };
        cx.semantics(
            node,
            Semantics::role(role).with_label(caption.text.as_str()),
        );
        if let Some(cell) = cell {
            cx.bind_semantic_state(node, move |cx| {
                SemanticState::checked(matches!(cx.get(cell), Some(StateValue::Bool(true))))
            });
        }
        cx.focusable(node, true);
        if let Some(key) = caption.char() {
            cx.access_key(node, key);
        }

        let activate = Activate {
            cell,
            on_change: self.on_change.clone(),
            on_click: self.on_click.clone(),
        };
        let removable = self.removable;
        let (act, remove) = (activate.clone(), self.on_remove.clone());
        cx.on_pointer(node, move |ev| {
            let Some(&p) = ev.pointer() else { return };
            if p.phase != PointerPhase::Up || !p.buttons.contains(PointerButtons::PRIMARY) {
                return;
            }
            if ev.removes_at(p.x, p.y) {
                signal(&remove, ev);
            } else {
                act.run(ev);
            }
        });
        let remove = self.on_remove.clone();
        let id = node.id();
        cx.on_key(node, move |ev| {
            if ev.access() == Some(Access::Key) {
                ev.stop_propagation();
                ev.request_focus(id);
                activate.run(ev);
            } else if let Some(k) = ev.key()
                && k.pressed
                && !k.repeat
            {
                match k.key {
                    Key::Enter | Key::Space => {
                        ev.stop_propagation();
                        activate.run(ev);
                    }
                    Key::Delete | Key::Backspace if removable => {
                        ev.stop_propagation();
                        signal(&remove, ev);
                    }
                    _ => {}
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use viso_ui::{
        BindingTable, KeyEvent, Modifiers, NodeId, NodeStore, PointerEvent, SemanticProjector,
        StateStore, TextEdits, VirtualLists,
    };

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

        fn build(&mut self, chip: Chip) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            chip.build(&mut cx);
            cx.root().expect("a chip declares a root node")
        }

        fn release(&mut self, root: NodeId) {
            let ev = PointerEvent {
                x: 0.0,
                y: 0.0,
                phase: PointerPhase::Up,
                buttons: PointerButtons::PRIMARY,
                modifiers: Modifiers::default(),
            };
            let mut handler = self.store.take_handler(root).expect("pointer handler");
            {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_handler(root, handler);
        }

        fn key(&mut self, root: NodeId, key: Key) {
            let ev = KeyEvent {
                key,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            };
            let mut handler = self.store.take_key_handler(root).expect("key handler");
            {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_key_handler(root, handler);
        }
    }

    #[test]
    fn a_filter_chip_flips_its_selection_and_reports_it() {
        let last = Rc::new(Cell::new(None::<bool>));
        let clicks = Rc::new(Cell::new(0));
        let (l, c) = (last.clone(), clicks.clone());
        let mut rx = Reactive::new();
        let root = rx.build(
            chip("Vegetarian")
                .on_change(move |_, selected| l.set(Some(selected)))
                .on_click(move |_| c.set(c.get() + 1)),
        );
        assert_eq!(
            rx.store.check_mark(root),
            Some(CheckMark {
                shape: CheckShape::Chip { removable: false },
                checked: false,
            })
        );
        assert_eq!(rx.store.semantics(root).unwrap().role, Role::CheckBox);
        rx.release(root);
        assert_eq!(last.get(), Some(true));
        rx.key(root, Key::Space);
        assert_eq!(last.get(), Some(false));
        assert_eq!(clicks.get(), 2, "every activation clicks too");
        rx.key(root, Key::Delete);
        assert_eq!(
            last.get(),
            Some(false),
            "a chip with no cross ignores Delete"
        );
    }

    #[test]
    fn an_input_chip_is_a_button_that_delete_removes() {
        let removed = Rc::new(Cell::new(0));
        let clicks = Rc::new(Cell::new(0));
        let (r, c) = (removed.clone(), clicks.clone());
        let mut rx = Reactive::new();
        let root = rx.build(
            chip("rust")
                .on_remove(move |_| r.set(r.get() + 1))
                .on_click(move |_| c.set(c.get() + 1)),
        );
        assert_eq!(
            rx.store.check_mark(root).map(|m| m.shape),
            Some(CheckShape::Chip { removable: true })
        );
        assert_eq!(rx.store.semantics(root).unwrap().role, Role::Button);
        rx.key(root, Key::Backspace);
        rx.key(root, Key::Delete);
        assert_eq!(removed.get(), 2);
        // Off the cross (no layout here, so the release lands at the start).
        rx.release(root);
        rx.key(root, Key::Enter);
        assert_eq!((removed.get(), clicks.get()), (2, 2));
    }
}
