//! The [`ColorPicker`] control — a colour picked by hue, saturation and
//! brightness, its opacity, or typed as hex.
//!
//! A picker stacks, top to bottom: a square of the tones at its hue —
//! saturation across, brightness up — or, as a [`wheel`](ColorPicker::wheel),
//! that square inside a ring of hues; a bar of hues under the square; a bar
//! of opacity, the colour running from clear to opaque over a checkerboard,
//! unless [`alpha`](ColorPicker::alpha) is off; and a row holding the colour
//! as hex and a swatch of it. The square, the ring and the bars draw through
//! [`viso_ui::color_plane`] and [`viso_ui::slider`], so what shows is the
//! exact colour each point stands for.
//!
//! A press on the square, the ring or a bar picks there and a drag carries
//! on; the arrows step the focused part by a hundredth (a tenth with Shift)
//! — the square's saturation across and brightness up and down, the ring's
//! hue round; a bar steps as a slider does. Hex takes `#rgb`, `#rgba`,
//! `#rrggbb` or `#rrggbbaa`, the `#` optional: the colour follows each edit
//! to a full form, and a short one once the field loses the focus or Enter
//! submits it, which shows the colour in full. The hue is kept while the colour is grey or black, and
//! the saturation while it is black, so dragging to black and back keeps
//! them. Each change fires `on_change` with the colour, straight linear RGBA.
//!
//! Semantics: a group named by its label holding sliders for the square
//! (announcing saturation and brightness), the hue and the opacity, and the
//! hex text field — every one a Tab stop, its names
//! [`labels`](ColorPicker::labels).
//!
//! Invalidation: the colour lives in one reactive colour cell, which a
//! structure hook shows on the planes, the bars, the swatch and the hex: a
//! `PAINT` of each, a `SEMANTICS` of the parts, and the hex re-seeded.
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

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_ui::color_plane::{
    ColorPlane, Hsv, PlaneChange, PlaneInput, PlanePart, PlaneShape, SWATCH_SIZE, format_hex,
    parse_hex, plane_input,
};
use viso_ui::slider::{
    Slider as SliderMark, SliderInput, SliderRange, SliderShape, Thumb, TrackPaint, slider_input,
};
use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, DispatchPhase, EventCx, FieldEvent, FieldLook,
    FieldSeed, FlexStyle, FocusChange, FontSize, Handle, InputFilter, Inset, LeafStyle, Length,
    NodeId, Rgba, Role, SemanticState, Semantics, Size, StackStyle, StateId, StateValue,
    StructureCx, TextRequest, TextStyle, field_input,
};

/// What runs with the picker's new colour.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, Rgba)>>>>;

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

/// The height of a hue or opacity bar.
const BAR: f32 = 24.0;

/// How a colour picker looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorPickerStyle {
    /// Its width; the square is three quarters as tall, a wheel as tall.
    pub width: f32,
    /// The ring round a focused puck.
    pub accent: Rgba,
    /// The box the hex sits in.
    pub field: BoxStyle,
    pub field_padding: Inset,
    /// The hex's ink and size.
    pub ink: Rgba,
    pub font_size: f32,
}

impl Default for ColorPickerStyle {
    fn default() -> Self {
        ColorPickerStyle {
            width: 232.0,
            accent: rgba(0.22, 0.45, 0.85, 1.0),
            field: BoxStyle::solid(rgba(0.46, 0.46, 0.5, 0.12)).with_radius(6.0),
            field_padding: Inset {
                left: 8.0,
                right: 8.0,
                top: 5.0,
                bottom: 5.0,
            },
            ink: rgba(0.09, 0.09, 0.1, 1.0),
            font_size: 14.0,
        }
    }
}

/// The names assistive technology gives a picker's parts.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorPickerLabels {
    /// The square, its value announced as its saturation and brightness.
    pub tone: String,
    pub saturation: String,
    pub brightness: String,
    pub hue: String,
    pub opacity: String,
    pub hex: String,
}

impl Default for ColorPickerLabels {
    fn default() -> Self {
        ColorPickerLabels {
            tone: "Saturation and brightness".to_owned(),
            saturation: "Saturation".to_owned(),
            brightness: "Brightness".to_owned(),
            hue: "Hue".to_owned(),
            opacity: "Opacity".to_owned(),
            hex: "Hex".to_owned(),
        }
    }
}

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
        value: Hsv::new(0.6, 0.7, 0.85).to_rgba(1.0),
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

/// A colour the picker holds: its hue, saturation and brightness, and its
/// opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Tone {
    hsv: Hsv,
    alpha: f32,
}

impl Tone {
    fn rgba(self) -> Rgba {
        self.hsv.to_rgba(self.alpha)
    }
}

/// The colour a cell holds, `or` for any other value.
fn color(value: Option<StateValue>, or: Rgba) -> Rgba {
    match value {
        Some(StateValue::Color(r, g, b, a)) => rgba(r, g, b, a),
        _ => or,
    }
}

fn near(a: Rgba, b: Rgba) -> bool {
    [a.r - b.r, a.g - b.g, a.b - b.b, a.a - b.a]
        .iter()
        .all(|d| d.abs() < 1e-5)
}

/// What the picker shares between its handlers and its hook.
struct Picker {
    cell: StateId,
    /// Bumped to show the colour in full as hex again.
    round: StateId,
    alpha: bool,
    /// The colour last picked, keeping what a grey or black loses.
    tone: Cell<Tone>,
    /// The colour the hex was last typed as, which the field keeps showing
    /// as typed.
    typed: Cell<Option<Rgba>>,
    /// What the hex field holds as last edited.
    draft: RefCell<String>,
    plane: NodeId,
    /// The square's focus stop, and the ring's on a wheel.
    tone_stop: NodeId,
    hue_stop: Option<NodeId>,
    /// The hue bar, unless a wheel holds the hue, and the opacity bar.
    hue_bar: Option<NodeId>,
    alpha_bar: Option<NodeId>,
    hex: NodeId,
    swatch: NodeId,
    look: FieldLook,
    labels: ColorPickerLabels,
    style: ColorPickerStyle,
    on_change: SharedChange,
}

impl Picker {
    /// Hold `next`, writing `exact` — the colour as given, when it was — to
    /// the cell and reporting it.
    fn pick(&self, ev: &mut EventCx<'_>, next: Tone, exact: Option<Rgba>) {
        let next = Tone {
            alpha: if self.alpha { next.alpha } else { 1.0 },
            ..next
        };
        let rgba = exact.unwrap_or_else(|| next.rgba());
        if next == self.tone.get() && near(rgba, color(ev.get(self.cell), rgba)) {
            return;
        }
        self.tone.set(next);
        self.typed.set(None);
        ev.set(self.cell, StateValue::Color(rgba.r, rgba.g, rgba.b, rgba.a));
        if let Some(f) = self.on_change.borrow_mut().as_mut() {
            f(ev, rgba);
        }
    }

    /// Show the cell's colour on every part.
    fn show(&self, s: &mut StructureCx<'_>) {
        let was = self.tone.get();
        let held = color(s.states.get(self.cell), was.rgba());
        let tone = if near(held, was.rgba()) {
            was
        } else {
            Tone {
                hsv: Hsv::of(held, was.hsv),
                alpha: held.a,
            }
        };
        self.tone.set(tone);
        let Tone { hsv, alpha } = tone;
        s.store.show_color(self.plane, hsv, alpha);
        s.store.show_color(self.swatch, hsv, alpha);
        if let Some(bar) = self.hue_bar {
            s.store.show_slider(bar, 0.0, hsv.h);
            s.store
                .set_semantic_state(bar, SemanticState::slider(hsv.h * 360.0, 0.0, 360.0));
        }
        if let Some(stop) = self.hue_stop {
            s.store
                .set_semantic_state(stop, SemanticState::slider(hsv.h * 360.0, 0.0, 360.0));
        }
        if let Some(bar) = self.alpha_bar {
            s.store.show_slider(bar, 0.0, alpha);
            s.store
                .paint_slider(bar, TrackPaint::Alpha(hsv.to_rgba(1.0)));
            s.store
                .set_semantic_state(bar, SemanticState::slider(alpha * 100.0, 0.0, 100.0));
        }
        let percent = |f: f32| (f * 100.0).round() as i32;
        let value = format!(
            "{} {}%, {} {}%",
            self.labels.saturation,
            percent(hsv.s),
            self.labels.brightness,
            percent(hsv.v)
        );
        if s.store
            .semantics(self.tone_stop)
            .and_then(|sem| sem.value.as_deref())
            != Some(value.as_str())
        {
            let mut semantics = Semantics::role(Role::Slider).with_label(self.labels.tone.clone());
            semantics.value = Some(value);
            s.store.set_semantics(self.tone_stop, semantics);
        }
        if self.typed.get().is_none_or(|typed| !near(typed, held)) {
            self.typed.set(None);
            s.store.seed_text(
                self.hex,
                FieldSeed {
                    request: hex_line(&format_hex(held, self.alpha && held.a < 1.0), &self.style),
                    valued: true,
                    look: self.look.clone(),
                },
            );
        }
    }

    /// What the plane's pointer or a focus stop's keys ask, `part` the stop's.
    fn plane(&self, ev: &mut EventCx<'_>, part: PlanePart) {
        let Some(input) = plane_input(ev, self.plane, part) else {
            return;
        };
        let was = self.tone.get();
        let hsv = was.hsv;
        let next = match input {
            PlaneInput::Hue(h) => Hsv { h, ..hsv },
            PlaneInput::Tone(s, v) => Hsv { s, v, ..hsv },
            PlaneInput::StepHue(d) => Hsv {
                h: (hsv.h + d).rem_euclid(1.0),
                ..hsv
            },
            PlaneInput::StepTone(ds, dv) => Hsv {
                s: (hsv.s + ds).clamp(0.0, 1.0),
                v: (hsv.v + dv).clamp(0.0, 1.0),
                ..hsv
            },
            PlaneInput::Release => return,
        };
        // A press focuses the part it is on.
        let pressed = match input {
            PlaneInput::Hue(_) => self.hue_stop,
            PlaneInput::Tone(..) => Some(self.tone_stop),
            _ => None,
        };
        if let Some(stop) = pressed
            && ev.pointer().is_some()
            && ev.focused() != Some(stop)
        {
            ev.request_focus(stop);
        }
        self.pick(ev, Tone { hsv: next, ..was }, None);
    }

    /// What a bar's pointer or keys ask: the hue's, or the opacity's.
    fn bar(&self, ev: &mut EventCx<'_>, bar: NodeId, hue: bool) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(input) = slider_input(ev, bar, Thumb::High) else {
            return;
        };
        let was = self.tone.get();
        let current = f64::from(if hue { was.hsv.h } else { was.alpha });
        let range = SliderRange::new(0.0, 1.0, None);
        let next = match input {
            SliderInput::Reset | SliderInput::Release => None,
            input => input.apply(current, &range, Thumb::High),
        };
        let Some(next) = next.map(|n| n as f32) else {
            return;
        };
        let next = if hue {
            Tone {
                hsv: Hsv { h: next, ..was.hsv },
                ..was
            }
        } else {
            Tone { alpha: next, ..was }
        };
        self.pick(ev, next, None);
    }

    /// What the hex field's keys and edits ask.
    fn hex(&self, ev: &mut EventCx<'_>) {
        if let Some(text) = ev.text_change() {
            self.draft.replace(text.to_owned());
            // A short form is taken once it settles, so deleting digits from
            // a full one does not flash the colours on the way.
            let digits = text.trim().trim_start_matches('#').len();
            if matches!(digits, 6 | 8) {
                self.take_hex(ev, text);
            }
            return;
        }
        let settle = match ev.focus_change() {
            Some(change) => change == FocusChange::Lost,
            None => field_input(ev, false) == Some(FieldEvent::Submitted),
        };
        if settle {
            let draft = self.draft.take();
            self.take_hex(ev, &draft);
            self.typed.set(None);
            let round = match ev.get(self.round) {
                Some(StateValue::Int(n)) => n,
                _ => 0,
            };
            ev.set(self.round, StateValue::Int(round.wrapping_add(1)));
        }
    }

    /// Pick the colour `text` reads as, if it reads as one.
    fn take_hex(&self, ev: &mut EventCx<'_>, text: &str) {
        let Some(typed) = parse_hex(text) else {
            return;
        };
        let typed = Rgba {
            a: if self.alpha { typed.a } else { 1.0 },
            ..typed
        };
        let was = self.tone.get();
        let next = Tone {
            hsv: Hsv::of(typed, was.hsv),
            alpha: typed.a,
        };
        self.pick(ev, next, Some(typed));
        self.typed.set(Some(typed));
    }
}

/// The hex as the field draws it.
fn hex_line(text: &str, style: &ColorPickerStyle) -> TextRequest {
    TextRequest {
        spans: Vec::new(),
        text: text.to_owned(),
        font_size: FontSize::Fixed(style.font_size),
        color: style.ink,
        soft_wrap: false,
        locale: None,
        style: TextStyle::default(),
    }
}

/// A bar of `paint` at `value`.
fn color_bar(cx: &mut BuildCx<'_>, paint: TrackPaint, value: f32, label: &str) -> Handle {
    let mark = SliderMark::new(
        SliderShape::Bar(Axis::Row),
        Rgba::TRANSPARENT,
        Rgba::TRANSPARENT,
        rgba(1.0, 1.0, 1.0, 1.0),
        value,
    )
    .with_paint(paint);
    let leaf = cx.slider_leaf(
        Size {
            width: Length::Fill { weight: 1.0 },
            height: Length::Fixed(BAR),
        },
        mark,
    );
    cx.focusable(leaf, true);
    cx.semantics(leaf, Semantics::role(Role::Slider).with_label(label));
    leaf
}

impl Component for ColorPicker {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let start = Rgba {
            a: if self.alpha { self.value.a } else { 1.0 },
            ..self.value
        };
        let cell = self
            .cell
            .unwrap_or_else(|| cx.state(StateValue::Color(start.r, start.g, start.b, start.a)));
        let round = cx.state(StateValue::Int(0));
        // A bound cell's colour shows from the structure pass on.
        let held = start;
        let tone = Tone {
            hsv: Hsv::of(held, Hsv::default()),
            alpha: if self.alpha { held.a } else { 1.0 },
        };
        let style = self.style;
        let width = style.width;
        let shape = if self.wheel {
            PlaneShape::Wheel
        } else {
            PlaneShape::Square
        };
        let look = FieldLook {
            max_length: Some(9),
            filter: Some(InputFilter::new(|piece: &str| {
                piece
                    .chars()
                    .filter(|c| c.is_ascii_hexdigit() || *c == '#')
                    .collect()
            })),
            ..FieldLook::default()
        };
        let mut parts = None;
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 10.0,
                align: Align::Stretch,
                size: Size {
                    width: Length::Fixed(width),
                    height: Length::Fit,
                },
                ..FlexStyle::default()
            },
            |cx| {
                let height = if self.wheel { width } else { width * 0.75 };
                let mut stops = (None, None);
                let plane = cx.stack(
                    StackStyle {
                        size: Size {
                            width: Length::Fill { weight: 1.0 },
                            height: Length::Fixed(height),
                        },
                        ..StackStyle::default()
                    },
                    |cx| {
                        let stop = |cx: &mut BuildCx<'_>, label: &str| {
                            let leaf = cx.leaf(LeafStyle {
                                size: Size::fill(),
                                style: BoxStyle::NONE,
                            });
                            cx.focusable(leaf, true);
                            cx.focus_ring(leaf, false);
                            cx.semantics(leaf, Semantics::role(Role::Slider).with_label(label));
                            leaf
                        };
                        if self.wheel {
                            stops.1 = Some(stop(cx, &self.labels.hue));
                        }
                        stops.0 = Some(stop(cx, &self.labels.tone));
                    },
                );
                let hue_bar = (!self.wheel)
                    .then(|| color_bar(cx, TrackPaint::Hue, tone.hsv.h, &self.labels.hue));
                let alpha_bar = self.alpha.then(|| {
                    let paint = TrackPaint::Alpha(tone.hsv.to_rgba(1.0));
                    color_bar(cx, paint, tone.alpha, &self.labels.opacity)
                });
                let mut row = (None, None);
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: 8.0,
                        align: Align::Center,
                        ..FlexStyle::default()
                    },
                    |cx| {
                        cx.flex(
                            FlexStyle {
                                axis: Axis::Row,
                                padding: style.field_padding,
                                align: Align::Center,
                                size: Size {
                                    width: Length::Fill { weight: 1.0 },
                                    height: Length::Fit,
                                },
                                style: style.field,
                                ..FlexStyle::default()
                            },
                            |cx| {
                                row.0 = Some(cx.text_input(
                                    LeafStyle {
                                        size: Size {
                                            width: Length::Fill { weight: 1.0 },
                                            height: Length::Fit,
                                        },
                                        style: BoxStyle::NONE,
                                    },
                                    hex_line(&format_hex(held, self.alpha && held.a < 1.0), &style),
                                    look.clone(),
                                ));
                            },
                        );
                        row.1 = Some(cx.color_plane_leaf(
                            Size::fixed(SWATCH_SIZE.x, SWATCH_SIZE.y),
                            ColorPlane::new(PlaneShape::Swatch, tone.hsv, tone.alpha, style.accent),
                        ));
                    },
                );
                parts = Some((
                    plane,
                    stops.0.expect("built"),
                    stops.1,
                    hue_bar,
                    alpha_bar,
                    row.0.expect("built"),
                    row.1.expect("built"),
                ));
            },
        );
        let (plane, tone_stop, hue_stop, hue_bar, alpha_bar, hex, swatch) = parts.expect("built");
        cx.semantics(
            root,
            Semantics::role(Role::Group).with_label(self.label.clone()),
        );
        cx.focusable(hex, true);
        cx.semantics(
            hex,
            Semantics::role(Role::TextField).with_label(self.labels.hex.clone()),
        );

        let picker = Rc::new(Picker {
            cell,
            round,
            alpha: self.alpha,
            tone: Cell::new(tone),
            typed: Cell::new(None),
            draft: RefCell::new(String::new()),
            plane: plane.id(),
            tone_stop: tone_stop.id(),
            hue_stop: hue_stop.map(|h| h.id()),
            hue_bar: hue_bar.map(|h| h.id()),
            alpha_bar: alpha_bar.map(|h| h.id()),
            hex: hex.id(),
            swatch: swatch.id(),
            look,
            labels: self.labels.clone(),
            style,
            on_change: Rc::clone(&self.on_change),
        });
        cx.structure(|s| {
            let plane_mark = ColorPlane::new(shape, tone.hsv, tone.alpha, style.accent);
            s.store.set_color_plane(plane.id(), plane_mark);
            picker.show(s);
            for bar in [picker.hue_bar, picker.alpha_bar].into_iter().flatten() {
                s.store.land_slider(bar);
            }
            let hook = Rc::clone(&picker);
            s.store
                .add_structure_hook([cell, round], move |s, _| hook.show(s));
        });

        let p = Rc::clone(&picker);
        cx.on_pointer(plane, move |ev| {
            if ev.phase() != DispatchPhase::Capture {
                let focus = ev
                    .color_plane(p.plane)
                    .and_then(|plane| plane.focus)
                    .unwrap_or(PlanePart::Tone);
                p.plane(ev, focus);
            }
        });
        let stops = [
            Some((tone_stop, PlanePart::Tone)),
            hue_stop.map(|h| (h, PlanePart::Hue)),
        ];
        for (stop, part) in stops.into_iter().flatten() {
            let p = Rc::clone(&picker);
            cx.on_key(stop, move |ev| {
                if let Some(change) = ev.focus_change() {
                    let focus = (change == FocusChange::Gained).then_some(part);
                    let held = ev.color_plane(p.plane).and_then(|plane| plane.focus);
                    if focus.is_some() || held == Some(part) {
                        ev.change_plane(p.plane, PlaneChange::Focus(focus));
                    }
                    return;
                }
                p.plane(ev, part);
            });
        }
        for (bar, hue) in [(hue_bar, true), (alpha_bar, false)] {
            let Some(bar) = bar else {
                continue;
            };
            let id = bar.id();
            let p = Rc::clone(&picker);
            cx.on_pointer(bar, move |ev| p.bar(ev, id, hue));
            let p = Rc::clone(&picker);
            cx.on_key(bar, move |ev| p.bar(ev, id, hue));
        }
        cx.on_pointer(hex, |ev| {
            field_input(ev, false);
        });
        let p = Rc::clone(&picker);
        cx.on_key(hex, move |ev| p.hex(ev));
    }
}
