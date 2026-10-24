//! A colour picker's parts — a colour picked by hue, saturation and
//! brightness, its opacity, or typed as hex — built into a column, shared by
//! the `ColorPicker` widget and the view's `ColorPicker` node.
//!
//! The column stacks, top to bottom: a square of the tones at its hue —
//! saturation across, brightness up — or, on a wheel, that square inside a
//! ring of hues; a bar of hues under the square; a bar of opacity, the
//! colour running from clear to opaque over a checkerboard, unless the
//! picker takes no opacity; and a row holding the colour as hex and a swatch
//! of it. The square, the ring and the bars draw through
//! [`color_plane`](crate::color_plane) and [`slider`](crate::slider), so
//! what shows is the exact colour each point stands for.
//!
//! A press on the square, the ring or a bar picks there and a drag carries
//! on; the arrows step the focused part by a hundredth (a tenth with Shift)
//! — the square's saturation across and brightness up and down, the ring's
//! hue round; a bar steps as a slider does. Hex takes `#rgb`, `#rgba`,
//! `#rrggbb` or `#rrggbbaa`, the `#` optional: the colour follows each edit
//! to a full form, and a short one once the field loses the focus or Enter
//! submits it, which shows the colour in full. The hue is kept while the
//! colour is grey or black, and the saturation while it is black, so
//! dragging to black and back keeps them. Each change runs the picker's
//! [`PickSink`] with the colour, straight linear RGBA.
//!
//! Whether it takes an opacity and whether it is a wheel can change once
//! built ([`ColorPickerParts::configure`]): the parts it does not use are
//! hidden, not rebuilt.
//!
//! Semantics: sliders for the square (announcing saturation and
//! brightness), the hue and the opacity, and the hex text field — every one
//! a Tab stop, named by its [`ColorPickerLabels`]. The column is the caller's
//! to name.
//!
//! Invalidation: the colour lives in one colour cell, which a structure hook
//! shows on the planes, the bars, the swatch and the hex: a `PAINT` of each,
//! a `SEMANTICS` of the parts, and the hex re-seeded.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::color_plane::{
    ColorPlane, Hsv, PlaneChange, PlaneInput, PlanePart, PlaneShape, SWATCH_SIZE, format_hex,
    parse_hex, plane_input,
};
use crate::slider::{
    Slider as SliderMark, SliderInput, SliderRange, SliderShape, Thumb, TrackPaint, slider_input,
};
use crate::{
    Align, Axis, BoxStyle, BuildCx, DirtyClass, DispatchPhase, EventCx, FieldEvent, FieldLook,
    FieldSeed, FlexStyle, FocusChange, FontSize, Handle, InputFilter, Inset, LeafStyle, Length,
    NodeId, Rgba, Role, SemanticState, Semantics, Size, StackStyle, StateId, StateValue,
    StructureCx, TextRequest, TextStyle, Vec2, field_input,
};

/// What runs with a picker's new colour, straight linear RGBA.
pub type PickSink = Box<dyn FnMut(&mut EventCx<'_>, Rgba)>;

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

/// The height of a hue or opacity bar.
const BAR: f32 = 24.0;

/// The colour a picker starts at when given none: an opaque blue.
pub const DEFAULT_COLOR: Hsv = Hsv {
    h: 0.6,
    s: 0.7,
    v: 0.85,
};

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

/// What a picker's parts are built from.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerSpec {
    /// The colour it starts at, straight linear RGBA.
    pub value: Rgba,
    /// Whether it picks an opacity; without, the colour is opaque.
    pub alpha: bool,
    /// Whether the hue goes round a ring about the square rather than along
    /// a bar.
    pub wheel: bool,
    /// The colour cell it shows and writes, or `None` for one of its own.
    pub cell: Option<StateId>,
    /// How wide its column is: [`ColorPickerStyle::width`] exactly, or as
    /// wide as its parent lets it, that width its natural one.
    pub fill: bool,
    pub labels: ColorPickerLabels,
    pub style: ColorPickerStyle,
}

impl Default for PickerSpec {
    fn default() -> Self {
        PickerSpec {
            value: DEFAULT_COLOR.to_rgba(1.0),
            alpha: true,
            wheel: false,
            cell: None,
            fill: false,
            labels: ColorPickerLabels::default(),
            style: ColorPickerStyle::default(),
        }
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

/// What a picker's handlers and its hook share.
struct Picker {
    cell: StateId,
    /// Bumped to show the colour in full as hex again.
    round: StateId,
    alpha: Cell<bool>,
    wheel: Cell<bool>,
    /// The colour last picked, keeping what a grey or black loses.
    tone: Cell<Tone>,
    /// The colour the hex was last typed as, which the field keeps showing
    /// as typed.
    typed: Cell<Option<Rgba>>,
    /// What the hex field holds as last edited.
    draft: RefCell<String>,
    column: NodeId,
    plane: NodeId,
    /// The square's focus stop, and the ring's, shown on a wheel.
    tone_stop: NodeId,
    hue_stop: NodeId,
    /// The hue bar, hidden on a wheel, and the opacity bar, hidden without
    /// an opacity.
    hue_bar: NodeId,
    alpha_bar: NodeId,
    hex: NodeId,
    swatch: NodeId,
    look: FieldLook,
    labels: ColorPickerLabels,
    style: ColorPickerStyle,
    on_change: RefCell<Option<PickSink>>,
}

impl Picker {
    /// Hold `next`, writing `exact` — the colour as given, when it was — to
    /// the cell and reporting it.
    fn pick(&self, ev: &mut EventCx<'_>, next: Tone, exact: Option<Rgba>) {
        let next = Tone {
            alpha: if self.alpha.get() { next.alpha } else { 1.0 },
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
        let hue = SemanticState::slider(hsv.h * 360.0, 0.0, 360.0);
        s.store.show_slider(self.hue_bar, 0.0, hsv.h);
        s.store.set_semantic_state(self.hue_bar, hue);
        s.store.set_semantic_state(self.hue_stop, hue);
        s.store.show_slider(self.alpha_bar, 0.0, alpha);
        s.store
            .paint_slider(self.alpha_bar, TrackPaint::Alpha(hsv.to_rgba(1.0)));
        s.store.set_semantic_state(
            self.alpha_bar,
            SemanticState::slider(alpha * 100.0, 0.0, 100.0),
        );
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
                    request: hex_line(
                        &format_hex(held, self.alpha.get() && held.a < 1.0),
                        &self.style,
                    ),
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
            PlaneInput::Hue(_) => Some(self.hue_stop),
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
    fn bar(&self, ev: &mut EventCx<'_>, hue: bool) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let bar = if hue { self.hue_bar } else { self.alpha_bar };
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
            a: if self.alpha.get() { typed.a } else { 1.0 },
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

    /// Show the parts a picker taking an opacity `alpha` on a wheel `wheel`
    /// uses, and lay the plane out as tall as its shape is.
    fn configure(&self, s: &mut StructureCx<'_>, alpha: bool, wheel: bool) {
        self.alpha.set(alpha);
        self.wheel.set(wheel);
        s.store.set_hidden(self.hue_stop, !wheel);
        s.store.set_hidden(self.hue_bar, wheel);
        s.store.set_hidden(self.alpha_bar, !alpha);
        let width = self.style.width;
        let height = plane_height(width, wheel);
        let size = s.store.layout_input_mut(self.plane).size_mut();
        if size.height != Length::Fixed(height) {
            size.height = Length::Fixed(height);
            s.store
                .mark_dirty(self.plane, DirtyClass::MEASURE | DirtyClass::LAYOUT);
        }
        let tone = self.tone.get();
        let mut mark = ColorPlane::new(
            if wheel {
                PlaneShape::Wheel
            } else {
                PlaneShape::Square
            },
            tone.hsv,
            tone.alpha,
            self.style.accent,
        );
        mark.natural = Vec2::new(width, height);
        if let Some(was) = s.store.color_plane(self.plane) {
            mark.focus = was.focus.filter(|part| wheel || *part == PlanePart::Tone);
        }
        s.store.set_color_plane(self.plane, mark);
        // Without an opacity the colour held is opaque.
        let held = color(s.states.get(self.cell), tone.rgba());
        if !alpha && held.a < 1.0 {
            s.states
                .set(self.cell, StateValue::Color(held.r, held.g, held.b, 1.0));
        }
    }
}

/// How tall a plane `width` wide is: a wheel as tall, a square three
/// quarters.
fn plane_height(width: f32, wheel: bool) -> f32 {
    if wheel { width } else { width * 0.75 }
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

/// A picker's built parts: the handle that shows it an outside colour and
/// changes what it picks.
#[derive(Clone)]
pub struct ColorPickerParts(Rc<Picker>);

impl ColorPickerParts {
    /// Builds a picker's parts, as `spec` asks, into a column under the
    /// current parent, `on_change` running with each colour picked.
    pub fn build(cx: &mut BuildCx<'_>, spec: PickerSpec, on_change: Option<PickSink>) -> Self {
        let PickerSpec {
            value,
            alpha,
            wheel,
            cell,
            fill,
            labels,
            style,
        } = spec;
        let start = Rgba {
            a: if alpha { value.a } else { 1.0 },
            ..value
        };
        let cell =
            cell.unwrap_or_else(|| cx.state(StateValue::Color(start.r, start.g, start.b, start.a)));
        let round = cx.state(StateValue::Int(0));
        // A bound cell's colour shows from the structure pass on.
        let tone = Tone {
            hsv: Hsv::of(start, Hsv::default()),
            alpha: start.a,
        };
        let width = style.width;
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
        let fill_width = Length::Fill { weight: 1.0 };
        let mut parts = None;
        let column = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 10.0,
                align: Align::Stretch,
                size: Size {
                    width: if fill {
                        fill_width
                    } else {
                        Length::Fixed(width)
                    },
                    height: Length::Fit,
                },
                ..FlexStyle::default()
            },
            |cx| {
                let mut stops = (None, None);
                let plane = cx.stack(
                    StackStyle {
                        size: Size {
                            width: fill_width,
                            height: Length::Fixed(plane_height(width, wheel)),
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
                        stops.1 = Some(stop(cx, &labels.hue));
                        stops.0 = Some(stop(cx, &labels.tone));
                    },
                );
                let hue_bar = color_bar(cx, TrackPaint::Hue, tone.hsv.h, &labels.hue);
                let paint = TrackPaint::Alpha(tone.hsv.to_rgba(1.0));
                let alpha_bar = color_bar(cx, paint, tone.alpha, &labels.opacity);
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
                                    width: fill_width,
                                    height: Length::Fit,
                                },
                                style: style.field,
                                ..FlexStyle::default()
                            },
                            |cx| {
                                row.0 = Some(cx.text_input(
                                    LeafStyle {
                                        size: Size {
                                            width: fill_width,
                                            height: Length::Fit,
                                        },
                                        style: BoxStyle::NONE,
                                    },
                                    hex_line(&format_hex(start, alpha && start.a < 1.0), &style),
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
                    stops.1.expect("built"),
                    hue_bar,
                    alpha_bar,
                    row.0.expect("built"),
                    row.1.expect("built"),
                ));
            },
        );
        let (plane, tone_stop, hue_stop, hue_bar, alpha_bar, hex, swatch) = parts.expect("built");
        cx.focusable(hex, true);
        cx.semantics(
            hex,
            Semantics::role(Role::TextField).with_label(labels.hex.clone()),
        );

        let picker = Rc::new(Picker {
            cell,
            round,
            alpha: Cell::new(alpha),
            wheel: Cell::new(wheel),
            tone: Cell::new(tone),
            typed: Cell::new(None),
            draft: RefCell::new(String::new()),
            column: column.id(),
            plane: plane.id(),
            tone_stop: tone_stop.id(),
            hue_stop: hue_stop.id(),
            hue_bar: hue_bar.id(),
            alpha_bar: alpha_bar.id(),
            hex: hex.id(),
            swatch: swatch.id(),
            look,
            labels,
            style,
            on_change: RefCell::new(on_change),
        });
        cx.structure(|s| {
            picker.configure(s, alpha, wheel);
            picker.show(s);
            for bar in [picker.hue_bar, picker.alpha_bar] {
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
        for (stop, part) in [(tone_stop, PlanePart::Tone), (hue_stop, PlanePart::Hue)] {
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
            let p = Rc::clone(&picker);
            cx.on_pointer(bar, move |ev| p.bar(ev, hue));
            let p = Rc::clone(&picker);
            cx.on_key(bar, move |ev| p.bar(ev, hue));
        }
        cx.on_pointer(hex, |ev| {
            field_input(ev, false);
        });
        let p = Rc::clone(&picker);
        cx.on_key(hex, move |ev| p.hex(ev));
        ColorPickerParts(picker)
    }

    /// The column the parts are built in.
    pub fn column(&self) -> Handle {
        Handle::of(self.0.column)
    }

    /// The colour cell the picker shows and writes.
    pub fn cell(&self) -> StateId {
        self.0.cell
    }

    /// The colour the picker holds, straight linear RGBA.
    pub fn color(&self, s: &StructureCx<'_>) -> Rgba {
        color(s.states.get(self.0.cell), self.0.tone.get().rgba())
    }

    /// Show `color`, from outside the picker: its cell takes it, and the
    /// parts show it now. Without an opacity it shows opaque.
    pub fn show_color(&self, s: &mut StructureCx<'_>, color: Rgba) {
        let a = if self.0.alpha.get() { color.a } else { 1.0 };
        s.states
            .set(self.0.cell, StateValue::Color(color.r, color.g, color.b, a));
        self.0.show(s);
    }

    /// Whether it picks an opacity and whether the hue goes round a wheel.
    pub fn configure(&self, s: &mut StructureCx<'_>, alpha: bool, wheel: bool) {
        if (alpha, wheel) != (self.0.alpha.get(), self.0.wheel.get()) {
            self.0.configure(s, alpha, wheel);
            self.0.show(s);
        }
    }

    /// Run `sink` with each colour picked from now on, in place of what ran.
    pub fn on_change(&self, sink: Option<PickSink>) {
        *self.0.on_change.borrow_mut() = sink;
    }
}
