//! The [`Stepper`] control — a minus and a plus that step a number.
//!
//! A stepper is one focusable node drawing a pill split in two
//! ([`viso_ui::stepper`]): a press on the minus steps its number down by its
//! step, on the plus up, and holding the press steps on after a pause, faster
//! the longer it is held. The arrows step it by one, Page Up and Page Down by
//! ten and Home and End take it to its bounds. A step stops at a bound, or
//! comes round to the other one when it wraps; a half that can step no
//! further dims. Each step writes its number cell and fires `on_change` with
//! the new number. Its role is [`Role::SpinButton`](viso_ui::Role), carrying
//! its number and bounds, which an assistive technology steps with its
//! increment and decrement actions.
//!
//! ```
//! use viso_widgets::stepper;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
//!
//! let guests = stepper(2.0).range(1.0, 12.0).on_change(|_ev, guests| {
//!     let _ = guests;
//! });
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! guests.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::stepper::{Stepper as Pill, can_step, stepper_input};
use viso_ui::{
    BoxStyle, BuildCx, Component, Content, EventCx, LeafStyle, Length, NodeId, NodeStore, Rgba,
    Role, SemanticState, Semantics, Size, StateId, StateValue,
};

use super::check::INK;

/// A shared, mutable change callback carrying the new number, cloned into
/// the pointer and the key handler so both drive the same one.
type SharedNumber = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, f32)>>>>;

/// The look and size of a [`Stepper`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepperStyle {
    /// Its own size request within its parent. Defaults to `Fit` on both
    /// axes: 96 by 32.
    pub size: Size,
    /// The minus, the plus and the line between them.
    pub color: Rgba,
    /// The pill.
    pub fill: Rgba,
}

impl Default for StepperStyle {
    fn default() -> Self {
        StepperStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            color: INK,
            fill: Rgba { a: 0.12, ..INK },
        }
    }
}

/// A minus and a plus that step a number within its bounds.
///
/// Construct one with [`stepper`]. See the [module docs](self) for a build
/// example. Invalidation: a step repaints its one node.
pub struct Stepper {
    value: f32,
    min: Option<f32>,
    max: Option<f32>,
    step: f32,
    wrap: bool,
    label: Option<String>,
    cell: Option<StateId>,
    style: StepperStyle,
    on_change: SharedNumber,
}

/// A stepper starting at `value`, unbounded, stepping by one.
pub fn stepper(value: f32) -> Stepper {
    Stepper {
        value,
        min: None,
        max: None,
        step: 1.0,
        wrap: false,
        label: None,
        cell: None,
        style: StepperStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

/// What a stepper's number is kept within and stepped by.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    min: Option<f64>,
    max: Option<f64>,
    step: f64,
    wrap: bool,
}

impl Bounds {
    /// Shows `value` on stepper `id`: which halves can step, and what it
    /// tells assistive technology.
    fn show(self, store: &mut NodeStore, id: NodeId, value: f64) {
        let (lower, upper) = can_step(value, self.min, self.max, self.wrap);
        store.show_stepper(id, lower, upper);
        let state = SemanticState::number(
            value,
            self.min.unwrap_or(f64::NEG_INFINITY),
            self.max.unwrap_or(f64::INFINITY),
        );
        store.set_semantic_state(id, state);
    }
}

/// A float cell's number, `0` for any other value.
fn number(value: Option<StateValue>) -> f64 {
    match value {
        Some(StateValue::Float(v)) => f64::from(v),
        _ => 0.0,
    }
}

impl Stepper {
    /// Keep its number within `min..=max`.
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    /// Keep its number at or above `min`.
    pub fn min(mut self, min: f32) -> Self {
        self.min = Some(min);
        self
    }

    /// Keep its number at or below `max`.
    pub fn max(mut self, max: f32) -> Self {
        self.max = Some(max);
        self
    }

    /// Step by `step` (defaults to one).
    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    /// Come round to the other bound on a step past one, when both are set.
    pub fn wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }

    /// Name it for assistive technology.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Keep its number in float cell `cell`, which it shows and steps,
    /// rather than a cell of its own.
    pub fn bind(mut self, cell: StateId) -> Self {
        self.cell = Some(cell);
        self
    }

    /// Set the change callback, fired with the new number on each step.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, f32) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`StepperStyle`].
    pub fn style(mut self, style: StepperStyle) -> Self {
        self.style = style;
        self
    }

    /// Set its own size request within its parent (defaults to `Fit`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Set the ink of its minus, plus and the line between them.
    pub fn color(mut self, color: Rgba) -> Self {
        self.style.color = color;
        self
    }
}

impl Component for Stepper {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let cell = self
            .cell
            .unwrap_or_else(|| cx.state(StateValue::Float(self.value)));
        let node = cx.leaf(LeafStyle {
            size: self.style.size,
            style: BoxStyle::NONE,
        });
        let id = node.id();
        let pill = Pill::new(self.style.color, self.style.fill);
        let mut semantics = Semantics::role(Role::SpinButton);
        semantics.label = self.label.clone();
        cx.semantics(node, semantics);
        cx.focusable(node, true);
        let bounds = Bounds {
            min: self.min.map(f64::from),
            max: self.max.map(f64::from),
            step: f64::from(self.step),
            wrap: self.wrap,
        };
        cx.structure(|s| {
            s.store.set_content_payload(id, Content::Stepper(pill));
            bounds.show(s.store, id, number(s.states.get(cell)));
            s.store.add_structure_hook([cell], move |s, _| {
                bounds.show(s.store, id, number(s.states.get(cell)));
            });
        });

        let respond = {
            let on_change = self.on_change.clone();
            move |ev: &mut EventCx<'_>| {
                let Some(input) = stepper_input(ev, id) else {
                    return;
                };
                let value = number(ev.get(cell));
                let Some(next) =
                    input.apply(value, bounds.step, bounds.min, bounds.max, bounds.wrap)
                else {
                    return;
                };
                let next = next as f32;
                ev.set(cell, StateValue::Float(next));
                if let Some(f) = on_change.borrow_mut().as_mut() {
                    f(ev, next);
                }
            }
        };
        cx.on_pointer(node, respond.clone());
        cx.on_key(node, respond);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use viso_ui::stepper::StepHalf;
    use viso_ui::{
        BindingTable, Key, KeyEvent, Modifiers, SemanticProjector, StateStore, TextEdits,
        VirtualLists,
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

        fn build(&mut self, stepper: Stepper) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            stepper.build(&mut cx);
            cx.root().expect("a stepper declares a root node")
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
    fn a_stepper_is_one_spin_button_drawing_its_pill() {
        let mut rx = Reactive::new();
        let root = rx.build(stepper(3.0).range(0.0, 3.0).label("Guests"));
        assert!(rx.store.focusable(root));
        let semantics = rx.store.semantics(root).unwrap();
        assert_eq!(semantics.role, Role::SpinButton);
        assert_eq!(semantics.label.as_deref(), Some("Guests"));
        let pill = rx.store.stepper(root).expect("it draws a stepper");
        assert!(pill.lower && !pill.upper, "at its most, the plus dims");
        assert_eq!(
            rx.store.semantic_state(root),
            Some(SemanticState::number(3.0, 0.0, 3.0))
        );
        assert_eq!(
            StepHalf::of_token(StepHalf::Down.token()),
            Some(StepHalf::Down)
        );
    }

    #[test]
    fn keys_step_it_within_its_bounds_and_report_each_number() {
        let seen = Rc::new(Cell::new(f32::NAN));
        let s = seen.clone();
        let mut rx = Reactive::new();
        let root = rx.build(
            stepper(1.0)
                .range(0.0, 20.0)
                .step(0.5)
                .on_change(move |_, v| s.set(v)),
        );
        rx.key(root, Key::Up);
        assert_eq!(seen.get(), 1.5);
        rx.key(root, Key::PageDown);
        assert_eq!(seen.get(), 0.0, "ten steps down stop at the least");
        rx.key(root, Key::End);
        assert_eq!(seen.get(), 20.0);
    }
}
