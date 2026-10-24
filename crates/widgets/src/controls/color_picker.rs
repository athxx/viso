//! The [`ColorPicker`] control — a colour picked by hue, saturation and
//! brightness, its opacity, or typed as hex.
//!
//! A picker stacks a square of the tones at its hue — or, as a
//! [`wheel`](ColorPicker::wheel), that square inside a ring of hues — a bar
//! of hues, a bar of opacity unless [`alpha`](ColorPicker::alpha) is off, and
//! a row holding the colour as hex and a swatch of it. Its parts, how they
//! answer the pointer and the keys and what they invalidate are
//! [`viso_ui::color_picker`]'s, shared with the view's `ColorPicker` node.
//! Each change fires `on_change` with the colour, straight linear RGBA.
//!
//! Semantics: a group named by its label holding sliders for the square, the
//! hue and the opacity, and the hex text field, their names
//! [`labels`](ColorPicker::labels).
//!
//! ```
//! use viso_widgets::color_picker;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, Rgba, StateStore, TextEdits, VirtualLists};
//!
//! let picker = color_picker("Fill")
//!     .value(Rgba::new(0.05, 0.2, 0.7, 1.0))
//!     .on_change(|_ev, color| {
//!         let _ = color;
//!     });
//! let ring = color_picker("Stroke").wheel(true).alpha(false);
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! cx.flex(Default::default(), |cx| {
//!     picker.build(cx);
//!     ring.build(cx);
//! });
//! ```

use std::cell::RefCell;
use std::rc::Rc;

pub use viso_ui::color_picker::{ColorPickerLabels, ColorPickerStyle};
use viso_ui::color_picker::{ColorPickerParts, DEFAULT_COLOR, PickerSpec};
use viso_ui::{BuildCx, Component, EventCx, Rgba, Role, Semantics, StateId};

/// What runs with the picker's new colour.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, Rgba)>>>>;

/// A colour picked by hue, saturation and brightness, its opacity, or typed
/// as hex. Construct one with [`color_picker`].
pub struct ColorPicker {
    label: String,
    value: Rgba,
    alpha: bool,
    wheel: bool,
    cell: Option<StateId>,
    labels: ColorPickerLabels,
    style: ColorPickerStyle,
    on_change: SharedChange,
}

/// A picker named `label` with a square, a hue bar and an opacity bar, at
/// an opaque blue.
pub fn color_picker(label: impl Into<String>) -> ColorPicker {
    ColorPicker {
        label: label.into(),
        value: DEFAULT_COLOR.to_rgba(1.0),
        alpha: true,
        wheel: false,
        cell: None,
        labels: ColorPickerLabels::default(),
        style: ColorPickerStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl ColorPicker {
    /// Run `handler` with each new colour, straight linear RGBA.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, Rgba) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Start at `color`, straight linear RGBA.
    pub fn value(mut self, color: Rgba) -> Self {
        self.value = color;
        self
    }

    /// Pick an opacity too (the default); without, the colour is opaque.
    pub fn alpha(mut self, alpha: bool) -> Self {
        self.alpha = alpha;
        self
    }

    /// Pick the hue round a ring about the square rather than along a bar.
    pub fn wheel(mut self, wheel: bool) -> Self {
        self.wheel = wheel;
        self
    }

    /// Keep the colour in colour cell `cell`, which it shows and writes,
    /// rather than a cell of its own.
    pub fn bind(mut self, cell: StateId) -> Self {
        self.cell = Some(cell);
        self
    }

    /// Name its parts for assistive technology.
    pub fn labels(mut self, labels: ColorPickerLabels) -> Self {
        self.labels = labels;
        self
    }

    /// Replace the whole [`ColorPickerStyle`].
    pub fn style(mut self, style: ColorPickerStyle) -> Self {
        self.style = style;
        self
    }

    /// Set its width (default 232).
    pub fn width(mut self, width: f32) -> Self {
        self.style.width = width;
        self
    }
}

impl Component for ColorPicker {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let change = Rc::clone(&self.on_change);
        let sink = Box::new(move |ev: &mut EventCx<'_>, color: Rgba| {
            if let Some(f) = change.borrow_mut().as_mut() {
                f(ev, color);
            }
        });
        let spec = PickerSpec {
            value: self.value,
            alpha: self.alpha,
            wheel: self.wheel,
            cell: self.cell,
            fill: false,
            labels: self.labels.clone(),
            style: self.style,
        };
        let parts = ColorPickerParts::build(cx, spec, Some(sink));
        cx.semantics(
            parts.column(),
            Semantics::role(Role::Group).with_label(self.label.clone()),
        );
    }
}
