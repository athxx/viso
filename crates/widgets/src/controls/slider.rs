//! The [`Slider`], [`Knob`](knob) and [`RangeSlider`] controls — a value
//! picked from `min..=max` along a track, round a knob, or a stretch of it
//! between two thumbs.
//!
//! Each draws through one [`viso_ui::slider`] node: a bar along its axis —
//! growing to the right, to the left read right to left, or upward when
//! [`vertical`](Slider::vertical) — filled from its start or from its
//! [`origin`](Slider::origin), marked at each step when
//! [`marks`](Slider::marks) asks and there is room; or a knob, a ring open at
//! the bottom round a face pointing at the value. The thumb follows a drag at
//! once and eases to a value set any other way.
//!
//! A press on a bar's track jumps the thumb there, one on the thumb picks it
//! up, and a drag carries it; a knob turns with a drag up or right. Shift
//! held while dragging moves a tenth as far, and a double click puts the
//! value back to its [`default_value`](Slider::default_value). The arrows
//! step (by `step`, or a hundredth of the range), Page Up/Down by ten steps,
//! Home and End to the bounds, and the wheel steps a focused slider under
//! it. Each change fires `on_change` once with the new value, on its step.
//!
//! Semantics: a slider or knob is one focusable [`Role::Slider`] named by its
//! label, announcing its value in its range. A range slider is a group named
//! by its label holding two sliders, its lower and upper thumbs, each a Tab
//! stop of its own; the focused thumb is ringed on the track.
//!
//! Invalidation: a value writes the slider's reactive cell, which tells the
//! node where its thumb stands: a `PAINT` of that node alone each frame it
//! moves, and its semantics.
//!
//! ```
//! use viso_widgets::{knob, range_slider, slider};
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
//!
//! let volume = slider("Volume").range(0.0, 100.0).value(30.0).on_change(|_ev, v| {
//!     let _ = v;
//! });
//! let pan = knob("Pan").range(-1.0, 1.0).origin(0.0);
//! let price = range_slider("Price").range(0.0, 500.0).step(10.0).values(100.0, 300.0);
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! cx.flex(Default::default(), |cx| {
//!     volume.build(cx);
//!     pan.build(cx);
//!     price.build(cx);
//! });
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::slider::{
    SLIDER_SIZE, Slide, Slider as SliderMark, SliderInput, SliderRange, SliderShape, Thumb,
    slider_input,
};
use viso_ui::{
    Axis, BoxStyle, BuildCx, Component, Content, DispatchPhase, EventCx, FocusChange, LeafStyle,
    Length, Rgba, Role, SemanticState, Semantics, Size, StackStyle, StateId, StateValue,
};

/// What runs with a slider's new value.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, f32)>>>>;

/// What runs with a range slider's new bounds.
type SharedRange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, f32, f32)>>>>;

/// How a slider looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliderStyle {
    /// Its size within its parent. `Fit` takes the slider's own: 160 by 24
    /// along its axis, 40 square for a knob.
    pub size: Size,
    /// The track, or a knob's ring.
    pub track: Rgba,
    /// The stretch filled, and a knob's pointer.
    pub fill: Rgba,
    /// The thumb, or a knob's face.
    pub thumb: Rgba,
}

impl Default for SliderStyle {
    fn default() -> Self {
        SliderStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            track: Rgba::new(0.47, 0.47, 0.5, 0.32),
            fill: Rgba::new(0.22, 0.45, 0.85, 1.0),
            thumb: Rgba::new(1.0, 1.0, 1.0, 1.0),
        }
    }
}

/// A value picked from `min..=max` along a track or round a knob.
/// Construct one with [`slider`] or [`knob`].
pub struct Slider {
    label: String,
    shape: SliderShape,
    min: f32,
    max: f32,
    step: f32,
    value: f32,
    default: Option<f32>,
    origin: Option<f32>,
    marks: bool,
    cell: Option<StateId>,
    style: SliderStyle,
    on_change: SharedChange,
}

/// A horizontal slider named `label`, `0` to `1`, continuous, at `0`.
pub fn slider(label: impl Into<String>) -> Slider {
    Slider {
        label: label.into(),
        shape: SliderShape::Bar(Axis::Row),
        min: 0.0,
        max: 1.0,
        step: 0.0,
        value: 0.0,
        default: None,
        origin: None,
        marks: false,
        cell: None,
        style: SliderStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

/// A knob named `label`, `0` to `1`, continuous, at `0`.
pub fn knob(label: impl Into<String>) -> Slider {
    Slider {
        shape: SliderShape::Knob,
        ..slider(label)
    }
}

impl Slider {
    /// Run `handler` with each new value.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, f32) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Pick from `min` to `max`.
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    /// Keep the value on a grid of `step` from `min`; `0` (the default) is
    /// continuous.
    pub fn step(mut self, step: f32) -> Self {
        self.step = step.max(0.0);
        self
    }

    /// Start at `value` (default: `min`).
    pub fn value(mut self, value: f32) -> Self {
        self.value = value;
        self
    }

    /// Go back to `value` on a double click (default: the starting value).
    pub fn default_value(mut self, value: f32) -> Self {
        self.default = Some(value);
        self
    }

    /// Fill from `value` rather than from the start: a balance or a trim
    /// fills either way from its centre.
    pub fn origin(mut self, value: f32) -> Self {
        self.origin = Some(value);
        self
    }

    /// Mark each step on the track, where there is room.
    pub fn marks(mut self, marks: bool) -> Self {
        self.marks = marks;
        self
    }

    /// Run up the screen rather than across; a knob stays a knob.
    pub fn vertical(mut self) -> Self {
        if let SliderShape::Bar(_) = self.shape {
            self.shape = SliderShape::Bar(Axis::Column);
        }
        self
    }

    /// Keep the value in float cell `cell`, which it shows and writes,
    /// rather than a cell of its own.
    pub fn bind(mut self, cell: StateId) -> Self {
        self.cell = Some(cell);
        self
    }

    /// Replace the whole [`SliderStyle`].
    pub fn style(mut self, style: SliderStyle) -> Self {
        self.style = style;
        self
    }

    /// Set its size within its parent (default: its own).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    fn grid(&self) -> SliderRange {
        SliderRange::new(
            f64::from(self.min),
            f64::from(self.max),
            Some(f64::from(self.step)),
        )
    }
}

/// The float a cell holds, `or` for any other value.
fn float(value: Option<StateValue>, or: f64) -> f64 {
    match value {
        Some(StateValue::Float(v)) => f64::from(v),
        Some(StateValue::Int(v)) => f64::from(v),
        _ => or,
    }
}

/// The marks a slider over `grid` draws when asked to.
fn marks_of(asked: bool, grid: &SliderRange) -> u32 {
    if asked { grid.steps() } else { 0 }
}

/// The slider drawn for `shape` in `style`, at `value`.
fn mark(shape: SliderShape, style: &SliderStyle, value: f32) -> SliderMark {
    SliderMark::new(shape, style.track, style.fill, style.thumb, value)
}

/// `size`, its `Fit` axes taking a range slider's own size: a range's
/// thumbs size it no further.
fn sized(size: Size, axis: Axis) -> Size {
    let own = match axis {
        Axis::Row => SLIDER_SIZE,
        Axis::Column => viso_ui::Vec2::new(SLIDER_SIZE.y, SLIDER_SIZE.x),
    };
    let fit = |l: Length, v: f32| match l {
        Length::Fit => Length::Fixed(v),
        l => l,
    };
    Size {
        width: fit(size.width, own.x),
        height: fit(size.height, own.y),
    }
}

impl Component for Slider {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let grid = self.grid();
        let start = grid.snap(f64::from(self.value));
        let cell = self
            .cell
            .unwrap_or_else(|| cx.state(StateValue::Float(start as f32)));
        let default = self.default.map_or(start, |d| grid.snap(f64::from(d)));
        let slider = mark(self.shape, &self.style, grid.fraction(start))
            .with_origin(self.origin.map(|o| grid.fraction(f64::from(o))))
            .with_marks(marks_of(self.marks, &grid));
        let leaf = cx.slider_leaf(self.style.size, slider);
        let id = leaf.id();
        cx.focusable(leaf, true);
        cx.semantics(
            leaf,
            Semantics::role(Role::Slider).with_label(self.label.clone()),
        );
        let (min, max) = (self.min, self.max);
        cx.bind_semantic_state(leaf, move |cx| {
            SemanticState::slider(float(cx.get(cell), f64::from(min)) as f32, min, max)
        });
        cx.structure(|s| {
            let show = move |s: &mut viso_ui::StructureCx<'_>| {
                let f = grid.fraction(float(s.states.get(cell), grid.min));
                s.store.show_slider(id, 0.0, f);
            };
            // A bound cell may hold a value already; it shows at once.
            show(s);
            s.store.land_slider(id);
            s.store.add_structure_hook([cell], move |s, _| show(s));
        });
        let respond = {
            let cb = Rc::clone(&self.on_change);
            move |ev: &mut EventCx<'_>| {
                if ev.phase() == DispatchPhase::Capture {
                    return;
                }
                let Some(input) = slider_input(ev, id, Thumb::High) else {
                    return;
                };
                let current = float(ev.get(cell), grid.min);
                let next = match input {
                    SliderInput::Reset => Some(default).filter(|d| *d != current),
                    input => input.apply(current, &grid, Thumb::High),
                };
                if let Some(next) = next {
                    ev.set(cell, StateValue::Float(next as f32));
                    if let Some(f) = cb.borrow_mut().as_mut() {
                        f(ev, next as f32);
                    }
                }
            }
        };
        let respond = Rc::new(respond);
        let pointer = Rc::clone(&respond);
        cx.on_pointer(leaf, move |ev| pointer(ev));
        cx.on_key(leaf, move |ev| respond(ev));
    }
}

/// A stretch of `min..=max` picked between two thumbs. Construct one with
/// [`range_slider`].
pub struct RangeSlider {
    label: String,
    labels: [String; 2],
    axis: Axis,
    min: f32,
    max: f32,
    step: f32,
    values: (f32, f32),
    marks: bool,
    cells: Option<(StateId, StateId)>,
    style: SliderStyle,
    on_change: SharedRange,
}

/// A horizontal range slider named `label`, `0` to `1`, continuous, over the
/// whole range.
pub fn range_slider(label: impl Into<String>) -> RangeSlider {
    RangeSlider {
        label: label.into(),
        labels: ["Minimum".to_owned(), "Maximum".to_owned()],
        axis: Axis::Row,
        min: 0.0,
        max: 1.0,
        step: 0.0,
        values: (0.0, 1.0),
        marks: false,
        cells: None,
        style: SliderStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl RangeSlider {
    /// Run `handler` with the new lower and upper bounds at each change.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, f32, f32) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Pick from `min` to `max`.
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    /// Keep both bounds on a grid of `step` from `min`.
    pub fn step(mut self, step: f32) -> Self {
        self.step = step.max(0.0);
        self
    }

    /// Start with the stretch from `low` to `high` (default: the whole
    /// range).
    pub fn values(mut self, low: f32, high: f32) -> Self {
        self.values = (low.min(high), high.max(low));
        self
    }

    /// Name the lower and upper thumbs for assistive technology (default
    /// "Minimum" and "Maximum").
    pub fn thumb_labels(mut self, low: impl Into<String>, high: impl Into<String>) -> Self {
        self.labels = [low.into(), high.into()];
        self
    }

    /// Mark each step on the track, where there is room.
    pub fn marks(mut self, marks: bool) -> Self {
        self.marks = marks;
        self
    }

    /// Run up the screen rather than across.
    pub fn vertical(mut self) -> Self {
        self.axis = Axis::Column;
        self
    }

    /// Keep the bounds in float cells `low` and `high`, which it shows and
    /// writes, rather than cells of its own.
    pub fn bind(mut self, low: StateId, high: StateId) -> Self {
        self.cells = Some((low, high));
        self
    }

    /// Replace the whole [`SliderStyle`].
    pub fn style(mut self, style: SliderStyle) -> Self {
        self.style = style;
        self
    }

    /// Set its size within its parent (default: 160 by 24 along its axis).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }
}

impl Component for RangeSlider {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let grid = SliderRange::new(
            f64::from(self.min),
            f64::from(self.max),
            Some(f64::from(self.step)),
        );
        let (low0, high0) = (
            grid.snap(f64::from(self.values.0)),
            grid.snap(f64::from(self.values.1)),
        );
        let (low, high) = self.cells.unwrap_or_else(|| {
            (
                cx.state(StateValue::Float(low0 as f32)),
                cx.state(StateValue::Float(high0 as f32)),
            )
        });
        let cells = [low, high];
        let style = self.style;
        let (axis, steps) = (self.axis, marks_of(self.marks, &grid));
        let mut thumbs = [None, None];
        let track = cx.stack(
            StackStyle {
                size: sized(self.style.size, self.axis),
                ..StackStyle::default()
            },
            |cx| {
                for (i, slot) in thumbs.iter_mut().enumerate() {
                    let leaf = cx.leaf(LeafStyle {
                        size: Size::fill(),
                        style: BoxStyle::NONE,
                    });
                    *slot = Some(leaf);
                    cx.focusable(leaf, true);
                    cx.focus_ring(leaf, false);
                    cx.semantics(
                        leaf,
                        Semantics::role(Role::Slider).with_label(self.labels[i].clone()),
                    );
                    let (min, max, cell) = (self.min, self.max, cells[i]);
                    cx.bind_semantic_state(leaf, move |cx| {
                        SemanticState::slider(float(cx.get(cell), f64::from(min)) as f32, min, max)
                    });
                }
            },
        );
        let id = track.id();
        let thumbs = thumbs.map(|t| t.expect("built"));
        let ids = thumbs.map(|t| t.id());
        cx.structure(|s| {
            let at = |c: StateId, or: f64| grid.fraction(float(s.states.get(c), or));
            let slider = mark(SliderShape::Bar(axis), &style, 0.0)
                .with_range(at(low, low0), at(high, high0))
                .with_marks(steps);
            s.store.set_content_payload(id, Content::Slider(slider));
            s.store.add_structure_hook(cells, move |s, _| {
                let f = |c: StateId| grid.fraction(float(s.states.get(c), grid.min));
                s.store.show_slider(id, f(low), f(high));
            });
        });
        cx.semantics(
            track,
            Semantics::role(Role::Group).with_label(self.label.clone()),
        );

        // Move `thumb` as `input` asks, reporting the new bounds.
        let respond = {
            let cb = Rc::clone(&self.on_change);
            move |ev: &mut EventCx<'_>, input: SliderInput, thumb: Thumb| {
                let i = match thumb {
                    Thumb::Low => 0,
                    Thumb::High => 1,
                };
                let current = float(ev.get(cells[i]), grid.min);
                let other = float(ev.get(cells[1 - i]), grid.min);
                let Some(next) = input.apply(current, &grid, thumb) else {
                    return;
                };
                // The thumbs do not pass each other.
                let next = match thumb {
                    Thumb::Low => next.min(other),
                    Thumb::High => next.max(other),
                };
                if next == current {
                    return;
                }
                ev.set(cells[i], StateValue::Float(next as f32));
                let (lo, hi) = if i == 0 { (next, other) } else { (other, next) };
                if let Some(f) = cb.borrow_mut().as_mut() {
                    f(ev, lo as f32, hi as f32);
                }
            }
        };
        let respond = Rc::new(respond);
        let pointer = Rc::clone(&respond);
        cx.on_pointer(track, move |ev| {
            if ev.phase() == DispatchPhase::Capture {
                return;
            }
            let Some(input) = slider_input(ev, id, Thumb::High) else {
                return;
            };
            if let Some(thumb) = input.thumb() {
                let i = if thumb == Thumb::Low { 0 } else { 1 };
                if ev.focused() != Some(ids[i]) {
                    ev.request_focus(ids[i]);
                }
                pointer(ev, input, thumb);
            }
        });
        for (i, thumb) in [Thumb::Low, Thumb::High].into_iter().enumerate() {
            let respond = Rc::clone(&respond);
            cx.on_key(thumbs[i], move |ev| {
                if let Some(change) = ev.focus_change() {
                    let focus = (change == FocusChange::Gained).then_some(thumb);
                    if focus.is_some() || ev.slider(id).is_some_and(|s| s.focus == Some(thumb)) {
                        ev.slide(id, Slide::Focus(focus));
                    }
                    return;
                }
                // The keys and the wheel reach the focused thumb.
                if let Some(input) = slider_input(ev, id, thumb) {
                    respond(ev, input, thumb);
                }
            });
        }
    }
}
