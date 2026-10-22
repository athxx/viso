//! The [`NumberField`] control — a text field holding a number that a drag
//! scrubs, the arrow keys and the wheel step, and typing edits.
//!
//! A `NumberField` is a [`TextInput`](crate::TextInput) whose buffer reads its
//! text as a number ([`viso_ui::number_format`]): until it has the focus a
//! drag along it scrubs the number and a click focuses it with its text
//! selected; with the focus it edits like any text field, Up and Down step
//! it (Page Up and Page Down ten steps, Shift ten times, Alt a tenth), the
//! wheel steps it, Enter or the focus leaving settles its text within the
//! range at the precision, and Escape puts back the number it last settled
//! on. It takes only a number's characters. Its handler is the shared
//! [`number_input`](viso_ui::number_input).
//!
//! [`on_change`](NumberField::on_change) fires with each number its text
//! comes to read as within the range: as it is typed, stepped, scrubbed or
//! settled. Its accessible role is [`Role::SpinButton`], announcing the
//! number and its bounds and taking increment and decrement.
//!
//! ```
//! use viso_widgets::number_field;
//!
//! let volume = number_field("Volume")
//!     .range(0.0, 100.0)
//!     .step(0.5)
//!     .value(42.0)
//!     .on_change(|_ev, value| {
//!         let _ = value;
//!     });
//! ```

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_ui::{
    BuildCx, Component, EventCx, FieldLook, FontSize, LeafStyle, NumberFormat, Role, SemanticState,
    Semantics, Size, StateValue, TextRequest, number_input, number_kind,
};

use crate::TextInputStyle;

/// A shared, mutable change callback carrying the field's new number.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, f64)>>>>;

/// An editable number with scrubbing, stepping and typed entry.
///
/// Construct one with [`number_field`] and configure it with the chainable
/// setters; see the [module docs](self).
pub struct NumberField {
    /// The accessible name.
    label: String,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    /// The decimal places it shows; as many as its step has when `None`.
    precision: Option<u8>,
    style: TextInputStyle,
    on_change: SharedChange,
}

/// Construct a [`NumberField`] named `label`, holding `0` with no bounds,
/// stepping by `1`.
pub fn number_field(label: impl Into<String>) -> NumberField {
    NumberField {
        label: label.into(),
        value: 0.0,
        min: f64::NEG_INFINITY,
        max: f64::INFINITY,
        step: 1.0,
        precision: None,
        style: TextInputStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl NumberField {
    /// Set the change callback, fired with each new number. Replaces any
    /// previously set handler.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, f64) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the number it starts with (defaults to `0`), held within its
    /// range.
    pub fn value(mut self, value: f64) -> Self {
        self.value = value;
        self
    }

    /// Hold it within `min..=max`; either bound infinite for none.
    pub fn range(mut self, min: f64, max: f64) -> Self {
        (self.min, self.max) = (min, max);
        self
    }

    /// Step it by `step` (defaults to `1`), showing as many decimal places
    /// as `step` has.
    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }

    /// Show `precision` decimal places, whatever its step.
    pub fn precision(mut self, precision: u8) -> Self {
        self.precision = Some(precision);
        self
    }

    /// Set the field's size request within its parent.
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Replace the whole [`TextInputStyle`].
    pub fn style(mut self, style: TextInputStyle) -> Self {
        self.style = style;
        self
    }
}

impl Component for NumberField {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let format = NumberFormat::new(self.min, self.max, self.step);
        let format = self
            .precision
            .map_or(format, |precision| format.with_precision(precision));
        let value = format.settle(self.value);
        let root = cx.text_input(
            LeafStyle {
                size: self.style.size,
                style: viso_ui::BoxStyle::NONE,
            },
            TextRequest {
                text: format.show(value),
                font_size: FontSize::Fixed(self.style.font_size),
                color: self.style.color,
                soft_wrap: false,
                locale: None,
                style: viso_ui::TextStyle::default(),
            },
            FieldLook {
                kind: number_kind(&format),
                number: Some(format),
                ..FieldLook::default()
            },
        );
        cx.focusable(root, true);

        // The number the field last reported, which the semantics announce.
        let shown = cx.state(StateValue::Float(value as f32));
        cx.bind_semantic_state(root, move |cx| {
            let value = match cx.get(shown) {
                Some(StateValue::Float(v)) => f64::from(v),
                _ => 0.0,
            };
            SemanticState::number(value, format.min, format.max)
        });

        cx.on_pointer(root, move |ev| {
            number_input(ev);
        });
        let on_change = Rc::clone(&self.on_change);
        let last = Cell::new(value);
        cx.on_key(root, move |ev| {
            let Some(text) = ev.text_change() else {
                number_input(ev);
                return;
            };
            let Some(number) = format.read(text).filter(|&v| format.holds(v)) else {
                return;
            };
            if number == last.replace(number) {
                return;
            }
            ev.set(shown, StateValue::Float(number as f32));
            if let Some(handler) = on_change.borrow_mut().as_mut() {
                handler(ev, number);
            }
        });
        cx.semantics(
            root,
            Semantics::role(Role::SpinButton).with_label(self.label.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{
        BindingTable, DispatchPhase, EditIntent, Key, KeyEvent, Modifiers, NodeId, NodeStore,
        SemanticProjector, StateStore, TextEdits, VirtualLists,
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

        fn build(&mut self, field: NumberField) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            field.build(&mut cx);
            cx.root().expect("a number field declares a root node")
        }

        fn key(&mut self, root: NodeId, key: Key) -> Vec<EditIntent> {
            let ev = KeyEvent {
                key,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            };
            let mut handler = self.store.take_key_handler(root).expect("key handler");
            let edits = {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                cx.__set_place(&self.store, root, DispatchPhase::Target, None);
                handler(&mut cx);
                cx.__take_edits()
            };
            self.store.restore_key_handler(root, handler);
            edits
        }

        fn text_change(&mut self, root: NodeId, text: &str) {
            let mut handler = self.store.take_key_handler(root).expect("key handler");
            {
                let mut cx = EventCx::__new_text_change(&mut self.states, &self.bindings, text);
                cx.__set_place(&self.store, root, DispatchPhase::Target, None);
                handler(&mut cx);
            }
            self.store.restore_key_handler(root, handler);
        }
    }

    #[test]
    fn a_number_field_is_a_spin_button_that_steps_and_reports_numbers_in_range() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        let mut rx = Reactive::new();
        let root = rx.build(
            number_field("Volume")
                .range(0.0, 10.0)
                .step(0.5)
                .value(2.0)
                .on_change(move |_, v| sink.borrow_mut().push(v)),
        );
        assert_eq!(
            rx.store.semantics(root).map(|s| s.role),
            Some(Role::SpinButton)
        );
        assert_eq!(rx.key(root, Key::Up), [EditIntent::Step(1.0)]);
        assert_eq!(rx.key(root, Key::Enter), [EditIntent::Settle]);
        for text in ["2.5", "2.5", "12", "", "3"] {
            rx.text_change(root, text);
        }
        assert_eq!(*seen.borrow(), [2.5, 3.0], "only new numbers in range");
    }
}
