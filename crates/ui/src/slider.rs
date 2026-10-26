//! Sliders: a value picked along a track, or round a knob.
//!
//! A slider is a node's content ([`Content::Slider`]). It holds where its
//! thumbs stand as fractions of the way along, not the numbers they stand
//! for: whoever drives it — the `Slider`, `RangeSlider` and `Knob` widgets,
//! a view's `Slider` and `Knob` — keeps the number and its
//! [`SliderRange`], and tells the slider where to draw
//! ([`NodeStore::show_slider`]). It eases there, unless a drag is carrying
//! the thumb, which follows the pointer at once, or motion is reduced.
//!
//! A bar runs along its axis, its value growing to the right (to the left
//! read right to left) or upward; it fills from its start, or from its
//! origin when it has one, and draws a mark at each step when asked to and
//! there is room. A range bar has two thumbs, filled between. A knob is a
//! ring with a quarter open at the bottom, filled round clockwise, and a
//! face with a line pointing at the value.
//!
//! A bar can paint its track with colour rather than fill it
//! ([`TrackPaint`]): the hues round from red, or a colour running from clear
//! to opaque over a checkerboard; its thumb is then a ring the track shows
//! through.
//!
//! [`slider_input`] reads what a sample asks of a slider, so every driver
//! moves the same way. A press on a bar's track jumps the nearer thumb
//! there, one on a thumb picks it up where it is, and a drag carries it; a
//! knob is turned by dragging up or right, a range's width for the whole
//! turn. Shift held while dragging moves a tenth as far. The thumbs of a
//! range do not pass each other; pressed where they meet, the drag's
//! direction picks which moves. A double click puts the value back to its
//! default. The arrows step, Page Up and Page Down by ten steps, Home and
//! End go to the bounds; the wheel steps a focused slider under it.

use std::f32::consts::PI;
use std::time::Duration;

use viso_render::{
    AnalyticEllipse, AnalyticShadow, Border, Corners, LineCap, Path, Point, Primitive, Quad, Rect,
    Rgba, ShadowShape, Stroke,
};

use crate::color_plane::{CHECK, Hsv, Shade, checkerboard, rounded_mesh};
use crate::component::{BuildCx, Handle, LeafStyle, NodeStore};
use crate::content::Content;
use crate::context::EventCx;
use crate::dirty::DirtyClass;
use crate::input::{Key, PointerButtons, PointerPhase};
use crate::layout::{Axis, Sizing, Vec2};
use crate::node::NodeId;
use crate::spinner::arc;
use crate::style::BoxStyle;

/// A bar's size unless its node sizes it, lying along its axis.
pub const SLIDER_SIZE: Vec2 = Vec2 { x: 160.0, y: 24.0 };
/// A knob's size unless its node sizes it.
pub const KNOB_SIZE: Vec2 = Vec2 { x: 40.0, y: 40.0 };

/// The thickness of a bar's track.
const TRACK: f32 = 4.0;
/// The diameter of a bar's thumb, at most the bar's thickness.
const THUMB: f32 = 20.0;
/// The least room between two step marks for the marks to show.
const MARK_ROOM: f32 = 6.0;
/// The diameter of a step mark.
const MARK: f32 = 3.0;
/// How far a drag moves a knob through its whole turn.
const TURN: f32 = 200.0;
/// How much less a drag moves with Shift held.
const FINE: f32 = 0.1;
/// How much of the ring a knob leaves open at the bottom.
const GAP: f32 = PI * 0.5;
/// How quickly a thumb eases to where it is told: the time constant
/// (seconds).
const EASE: f32 = 0.06;
/// How far the wheel scrolls for one step.
const WHEEL_STEP: f32 = 16.0;
/// The high bits of a wheel's wake token; its low bits the steps, signed.
const WHEEL: u32 = 0x534c_0000;
/// How many cells a coloured track is cut into along its length.
const COLOR_CELLS: u32 = 48;
/// How many steps Page Up and Page Down take.
pub const PAGE: f64 = 10.0;

/// How a slider draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliderShape {
    /// A track along an axis, a thumb on it.
    Bar(Axis),
    /// A ring round a face, a line on the face pointing at the value.
    Knob,
}

/// What a bar's track shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackPaint {
    /// Its track colour, filled up to the value.
    Plain,
    /// The hues, from red round to red.
    Hue,
    /// This colour, clear at the start and opaque at the end, over a
    /// checkerboard.
    Alpha(Rgba),
}

/// One of a slider's thumbs: a range's lower and upper, a plain slider's
/// only one the upper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thumb {
    Low,
    High,
}

impl Thumb {
    fn at(self) -> usize {
        match self {
            Thumb::Low => 0,
            Thumb::High => 1,
        }
    }
}

/// A drag under way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drag {
    /// The thumb it carries; `None` while a range's thumbs it was pressed
    /// on meet, until its direction picks one.
    pub thumb: Option<Thumb>,
    /// Where it has carried the thumb, unclamped, as a fraction.
    pub raw: f32,
    /// Where the pointer last was, in window space.
    pub at: Vec2,
}

/// What a slider draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slider {
    pub shape: SliderShape,
    /// Whether it has two thumbs, filled between.
    pub range: bool,
    /// Where its thumbs are told to stand, `0` to `1`: a range's lower and
    /// upper; a plain slider's value is the upper.
    pub value: [f32; 2],
    /// Where its thumbs stand drawn, easing toward `value`.
    pub shown: [f32; 2],
    /// Where a plain slider fills from, `None` for its start.
    pub origin: Option<f32>,
    /// How many steps its range holds, a mark drawn at each when there is
    /// room; `0` for none.
    pub marks: u32,
    pub track: Rgba,
    pub fill: Rgba,
    pub thumb: Rgba,
    /// What a bar's track shows.
    pub paint: TrackPaint,
    /// The drag carrying a thumb, if one is.
    pub drag: Option<Drag>,
    /// The thumb of a range that has the focus, ringed; a plain slider's
    /// node wears the focus ring itself.
    pub focus: Option<Thumb>,
    /// The wheel's scroll toward its next step.
    wheel: f32,
    /// Its size unless its node sizes it.
    pub natural: Vec2,
}

impl Slider {
    /// A slider of `shape` filled in `fill` on a `track`, its thumbs `thumb`,
    /// one standing at `value`.
    pub fn new(shape: SliderShape, track: Rgba, fill: Rgba, thumb: Rgba, value: f32) -> Slider {
        let value = clamp01(value);
        let natural = match shape {
            SliderShape::Bar(Axis::Row) => SLIDER_SIZE,
            SliderShape::Bar(Axis::Column) => Vec2 {
                x: SLIDER_SIZE.y,
                y: SLIDER_SIZE.x,
            },
            SliderShape::Knob => KNOB_SIZE,
        };
        Slider {
            shape,
            range: false,
            value: [0.0, value],
            shown: [0.0, value],
            origin: None,
            marks: 0,
            track,
            fill,
            thumb,
            paint: TrackPaint::Plain,
            drag: None,
            focus: None,
            wheel: 0.0,
            natural,
        }
    }

    /// The same with two thumbs, at `low` and `high`.
    pub fn with_range(mut self, low: f32, high: f32) -> Slider {
        let (low, high) = (clamp01(low), clamp01(high));
        let (low, high) = (low.min(high), high.max(low));
        self.range = true;
        self.value = [low, high];
        self.shown = self.value;
        self
    }

    /// The same filling from `origin`.
    pub fn with_origin(mut self, origin: Option<f32>) -> Slider {
        self.origin = origin.map(clamp01);
        self
    }

    /// The same with its track showing `paint`.
    pub fn with_paint(mut self, paint: TrackPaint) -> Slider {
        self.paint = paint;
        self
    }

    /// The same marking each of `steps` steps.
    pub fn with_marks(mut self, steps: u32) -> Slider {
        self.marks = steps;
        self
    }

    /// Whether its thumbs move toward where they are told.
    fn moving(&self) -> bool {
        self.shown != self.value
    }

    /// The thumb a drag carries.
    pub fn held(&self) -> Option<Thumb> {
        self.drag.and_then(|d| d.thumb)
    }

    /// How a bar of `rect` lies: where its value runs from and the way it
    /// runs, in window space, its thumb's diameter, and the span its centre
    /// travels.
    fn lie(&self, rect: Rect, rtl: bool) -> Lie {
        let SliderShape::Bar(axis) = self.shape else {
            return Lie::default();
        };
        let (len, thick) = match axis {
            Axis::Row => (rect.w, rect.h),
            Axis::Column => (rect.h, rect.w),
        };
        let thumb = THUMB.min(thick).min(len);
        let span = (len - thumb).max(0.0);
        let half = thumb * 0.5;
        let (start, way) = match axis {
            Axis::Row if rtl => (Vec2::new(rect.x + rect.w - half, 0.0), Vec2::new(-1.0, 0.0)),
            Axis::Row => (Vec2::new(rect.x + half, 0.0), Vec2::new(1.0, 0.0)),
            Axis::Column => (Vec2::new(0.0, rect.y + rect.h - half), Vec2::new(0.0, -1.0)),
        };
        Lie {
            start,
            way,
            thumb,
            span,
        }
    }

    /// The fraction a bar of `rect` puts at window point `at`, unclamped.
    fn fraction_at(&self, rect: Rect, rtl: bool, at: Vec2) -> f32 {
        let lie = self.lie(rect, rtl);
        if lie.span <= 0.0 {
            return 0.0;
        }
        let along = (at.x - lie.start.x) * lie.way.x + (at.y - lie.start.y) * lie.way.y;
        along / lie.span
    }

    /// How far, as a fraction, a pointer moving by `d` carries the thumb of
    /// a slider of `rect`.
    fn carried(&self, rect: Rect, rtl: bool, d: Vec2) -> f32 {
        match self.shape {
            SliderShape::Knob => (d.x - d.y) / TURN,
            SliderShape::Bar(_) => {
                let lie = self.lie(rect, rtl);
                if lie.span <= 0.0 {
                    return 0.0;
                }
                (d.x * lie.way.x + d.y * lie.way.y) / lie.span
            }
        }
    }

    /// The thumb a press at fraction `f` of a range picks: the nearer, or
    /// `None` where they meet.
    fn nearer(&self, f: f32) -> Option<Thumb> {
        if !self.range {
            return Some(Thumb::High);
        }
        let [low, high] = self.shown;
        if (high - low).abs() < 1e-4 {
            return if f < low - 1e-4 {
                Some(Thumb::Low)
            } else if f > high + 1e-4 {
                Some(Thumb::High)
            } else {
                None
            };
        }
        Some(if (f - low).abs() <= (f - high).abs() {
            Thumb::Low
        } else {
            Thumb::High
        })
    }

    /// `f` kept on its side of the other thumb of a range.
    pub fn bounded(&self, thumb: Thumb, f: f32) -> f32 {
        let f = clamp01(f);
        if !self.range {
            return f;
        }
        match thumb {
            Thumb::Low => f.min(self.value[1]),
            Thumb::High => f.max(self.value[0]),
        }
    }

    /// Draws it into `rect` (world space), mirrored when `rtl`.
    pub(crate) fn paint(&self, rect: Rect, rtl: bool, out: &mut Vec<Primitive>) {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        match self.shape {
            SliderShape::Bar(axis) => self.paint_bar(rect, axis, rtl, out),
            SliderShape::Knob => self.paint_knob(rect, out),
        }
    }

    fn paint_bar(&self, rect: Rect, axis: Axis, rtl: bool, out: &mut Vec<Primitive>) {
        let lie = self.lie(rect, rtl);
        let centre = Vec2::new(rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
        let thick = TRACK.min(lie.thumb.max(1.0));
        // The point along the bar at fraction `f`, on its centre line.
        let at = |f: f32| {
            let along = lie.span * f;
            match axis {
                Axis::Row => Vec2::new(lie.start.x + lie.way.x * along, centre.y),
                Axis::Column => Vec2::new(centre.x, lie.start.y + lie.way.y * along),
            }
        };
        // A capsule along the bar from `a` to `b`, `thick` across.
        let stretch = |a: Vec2, b: Vec2, thick: f32| match axis {
            Axis::Row => {
                let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
                Rect::new(x0 - thick * 0.5, a.y - thick * 0.5, x1 - x0 + thick, thick)
            }
            Axis::Column => {
                let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
                Rect::new(a.x - thick * 0.5, y0 - thick * 0.5, thick, y1 - y0 + thick)
            }
        };
        let ends = (at(0.0), at(1.0));
        if self.paint != TrackPaint::Plain {
            let track = stretch(ends.0, ends.1, (lie.thumb * 0.6).max(TRACK));
            self.paint_colors(track, axis, &lie, out);
            let p = at(self.shown[1]);
            self.paint_ring(p, lie.thumb * 0.5, out);
            return;
        }
        out.push(pill(stretch(ends.0, ends.1, thick), self.track));
        let [low, high] = self.shown;
        let (from, to) = match (self.range, self.origin) {
            (true, _) => (low, high),
            (false, Some(origin)) => (origin, high),
            (false, None) => (0.0, high),
        };
        let filled = |f: f32| f >= from.min(to) - 1e-4 && f <= from.max(to) + 1e-4;
        if (to - from).abs() > 1e-4 || (!self.range && self.origin.is_none()) {
            out.push(pill(stretch(at(from), at(to), thick), self.fill));
        }
        if self.marks > 0 && self.marks <= 1000 && lie.span / self.marks as f32 >= MARK_ROOM {
            let on = Rgba::new(1.0, 1.0, 1.0, 0.7 * self.fill.a);
            let off = Rgba {
                a: (self.fill.a * 0.45).min(1.0),
                ..self.fill
            };
            for i in 0..=self.marks {
                let f = i as f32 / self.marks as f32;
                let p = at(f);
                let color = if filled(f) { on } else { off };
                out.push(dot(p, MARK * 0.5, color));
            }
        }
        let thumbs: &[usize] = if self.range { &[0, 1] } else { &[1] };
        for &t in thumbs {
            let p = at(self.shown[t]);
            let r = lie.thumb * 0.5;
            if self.focus.map(Thumb::at) == Some(t) {
                let ring = r + 3.0;
                out.push(Primitive::AnalyticEllipse(AnalyticEllipse {
                    rect: Rect::new(p.x - ring, p.y - ring, ring * 2.0, ring * 2.0),
                    color: Rgba::TRANSPARENT,
                    border: Border {
                        width: 2.0,
                        color: self.fill,
                    },
                }));
            }
            if self.held().map(Thumb::at) == Some(t) {
                let halo = Rgba {
                    a: self.fill.a * 0.18,
                    ..self.fill
                };
                out.push(dot(p, r * 1.7, halo));
            }
            out.push(Primitive::AnalyticShadow(AnalyticShadow {
                rect: Rect::new(p.x - r, p.y - r, r * 2.0, r * 2.0),
                color: Rgba::new(0.0, 0.0, 0.0, 0.25 * self.thumb.a),
                radius: Corners::uniform(r),
                offset: [0.0, 1.0],
                sigma: 2.0,
                spread: 0.0,
                shape: ShadowShape::RoundedBox,
                inner: false,
            }));
            out.push(Primitive::Quad(Quad {
                rect: Rect::new(p.x - r, p.y - r, r * 2.0, r * 2.0),
                color: self.thumb,
                radius: r,
                border: Border {
                    width: 0.5,
                    color: Rgba::new(0.0, 0.0, 0.0, 0.12 * self.thumb.a),
                },
            }));
        }
    }

    /// A coloured track filling `track`, the colour at each point the one at
    /// the fraction it stands for along a bar lying as `lie`.
    fn paint_colors(&self, track: Rect, axis: Axis, lie: &Lie, out: &mut Vec<Primitive>) {
        let radius = (track.w.min(track.h) * 0.5).min(CHECK);
        let cells = match axis {
            Axis::Row => [COLOR_CELLS, 1],
            Axis::Column => [1, COLOR_CELLS],
        };
        let fraction = |x: f32, y: f32| {
            if lie.span <= 0.0 {
                return 0.0;
            }
            let along =
                (track.x + x - lie.start.x) * lie.way.x + (track.y + y - lie.start.y) * lie.way.y;
            clamp01(along / lie.span)
        };
        match self.paint {
            TrackPaint::Plain => {}
            TrackPaint::Hue => {
                let color = |x: f32, y: f32| Hsv::new(fraction(x, y), 1.0, 1.0).to_rgba(1.0);
                out.push(Primitive::Mesh(rounded_mesh(
                    track,
                    radius,
                    cells,
                    Shade::Smooth(&color),
                )));
            }
            TrackPaint::Alpha(tint) => {
                out.push(Primitive::Mesh(checkerboard(track, radius)));
                let color = |x: f32, y: f32| Rgba {
                    a: fraction(x, y),
                    ..tint
                };
                out.push(Primitive::Mesh(rounded_mesh(
                    track,
                    radius,
                    cells,
                    Shade::Smooth(&color),
                )));
            }
        }
    }

    /// A ring of radius `r` about `p` the track shows through, ringed again
    /// when it has the focus.
    fn paint_ring(&self, p: Vec2, r: f32, out: &mut Vec<Primitive>) {
        let ring = |r: f32, width: f32, color: Rgba| {
            Primitive::AnalyticEllipse(AnalyticEllipse {
                rect: Rect::new(p.x - r, p.y - r, r * 2.0, r * 2.0),
                color: Rgba::TRANSPARENT,
                border: Border { width, color },
            })
        };
        let r = if self.held().is_some() { r + 1.0 } else { r };
        out.push(ring(r + 0.75, 1.0, Rgba::new(0.0, 0.0, 0.0, 0.35)));
        out.push(ring(r, 3.0, self.thumb));
        out.push(ring(r - 3.0, 0.75, Rgba::new(0.0, 0.0, 0.0, 0.25)));
    }

    fn paint_knob(&self, rect: Rect, out: &mut Vec<Primitive>) {
        let side = rect.w.min(rect.h);
        let width = (side * 0.09).max(2.0);
        let radius = side * 0.5 - width * 0.5;
        let centre = Point {
            x: rect.x + rect.w * 0.5,
            y: rect.y + rect.h * 0.5,
        };
        // Clockwise from the top, the ring opening at the bottom.
        let start = PI + GAP * 0.5;
        let sweep = 2.0 * PI - GAP;
        let angle = |f: f32| start + sweep * f;
        let stroke = |cmds, color| {
            let mut stroke = Stroke::new(width, color);
            stroke.cap = LineCap::Round;
            Primitive::Path(Box::new(Path {
                cmds,
                fill: None,
                stroke: Some(stroke),
                shadow: None,
            }))
        };
        let mut ring = Vec::with_capacity(14);
        arc(&mut ring, centre, radius, angle(0.0), angle(1.0));
        out.push(stroke(ring, self.track));
        let [low, high] = self.shown;
        let (from, to) = match (self.range, self.origin) {
            (true, _) => (low, high),
            (false, Some(origin)) => (origin, high),
            (false, None) => (0.0, high),
        };
        let (from, to) = (from.min(to), from.max(to));
        if to - from > 1e-4 {
            let mut fill = Vec::with_capacity(14);
            arc(&mut fill, centre, radius, angle(from), angle(to));
            out.push(stroke(fill, self.fill));
        }
        let face = radius - width * 1.4;
        if face <= 1.0 {
            return;
        }
        let r = face;
        out.push(Primitive::AnalyticShadow(AnalyticShadow {
            rect: Rect::new(centre.x - r, centre.y - r, r * 2.0, r * 2.0),
            color: Rgba::new(0.0, 0.0, 0.0, 0.25 * self.thumb.a),
            radius: Corners::uniform(r),
            offset: [0.0, 1.0],
            sigma: 2.0,
            spread: 0.0,
            shape: ShadowShape::RoundedBox,
            inner: false,
        }));
        out.push(Primitive::AnalyticEllipse(AnalyticEllipse {
            rect: Rect::new(centre.x - r, centre.y - r, r * 2.0, r * 2.0),
            color: self.thumb,
            border: Border {
                width: 0.5,
                color: Rgba::new(0.0, 0.0, 0.0, 0.12 * self.thumb.a),
            },
        }));
        let thumbs: &[usize] = if self.range { &[0, 1] } else { &[1] };
        for &t in thumbs {
            let (sin, cos) = (angle(self.shown[t]) - PI * 0.5).sin_cos();
            let point = |k: f32| Point {
                x: centre.x + cos * face * k,
                y: centre.y + sin * face * k,
            };
            let cmds = vec![
                viso_render::PathCmd::MoveTo(point(0.3)),
                viso_render::PathCmd::LineTo(point(0.78)),
            ];
            let mut line = Stroke::new((width * 0.7).max(1.5), self.fill);
            line.cap = LineCap::Round;
            out.push(Primitive::Path(Box::new(Path {
                cmds,
                fill: None,
                stroke: Some(line),
                shadow: None,
            })));
        }
    }
}

/// How a bar lies in window space.
#[derive(Debug, Clone, Copy, Default)]
struct Lie {
    /// Where its value's thumb centre stands at `0`.
    start: Vec2,
    /// The way its value grows, a unit along its axis.
    way: Vec2,
    /// Its thumb's diameter.
    thumb: f32,
    /// How far its thumb's centre travels from `0` to `1`.
    span: f32,
}

fn clamp01(f: f32) -> f32 {
    if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) }
}

/// A capsule filling `rect` in `color`.
fn pill(rect: Rect, color: Rgba) -> Primitive {
    Primitive::AnalyticCapsule(viso_render::AnalyticCapsule {
        rect,
        color,
        border: Border::NONE,
    })
}

/// A disc of radius `r` about `p`.
fn dot(p: Vec2, r: f32, color: Rgba) -> Primitive {
    Primitive::AnalyticEllipse(AnalyticEllipse {
        rect: Rect::new(p.x - r, p.y - r, r * 2.0, r * 2.0),
        color,
        border: Border::NONE,
    })
}

/// The numbers a slider's fractions stand for: `min` to `max`, on a grid of
/// `step` from `min` when it has one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliderRange {
    pub min: f64,
    pub max: f64,
    pub step: Option<f64>,
}

impl SliderRange {
    /// `min` to `max`, on a grid of `step` when it is above zero.
    pub fn new(min: f64, max: f64, step: Option<f64>) -> SliderRange {
        SliderRange {
            min,
            max,
            step: step.filter(|s| *s > 0.0 && s.is_finite()),
        }
    }

    /// `value` on the nearest step, within the range.
    pub fn snap(&self, value: f64) -> f64 {
        let (low, high) = (self.min.min(self.max), self.max.max(self.min));
        let stepped = match self.step {
            Some(step) => self.min + ((value - self.min) / step).round() * step,
            None => value,
        };
        let v = stepped.clamp(low, high);
        if v.is_nan() { low } else { v }
    }

    /// The fraction of the way along `value` stands.
    pub fn fraction(&self, value: f64) -> f32 {
        let span = self.max - self.min;
        if span == 0.0 || !span.is_finite() {
            return 0.0;
        }
        clamp01(((value - self.min) / span) as f32)
    }

    /// The value at fraction `f`, on its step.
    pub fn at(&self, f: f32) -> f64 {
        self.snap(self.min + f64::from(clamp01(f)) * (self.max - self.min))
    }

    /// How many steps the range holds, `0` without a grid or with too many
    /// to count.
    pub fn steps(&self) -> u32 {
        match self.step {
            Some(step) => {
                let n = ((self.max - self.min).abs() / step).round();
                if n.is_finite() && n <= f64::from(u32::MAX) {
                    n as u32
                } else {
                    0
                }
            }
            None => 0,
        }
    }

    /// The unit an arrow steps by: its step, or a hundredth of the range.
    pub fn unit(&self) -> f64 {
        self.step.unwrap_or((self.max - self.min) * 0.01)
    }
}

/// What a sample asks of a slider ([`slider_input`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SliderInput {
    /// A press or a drag puts `Thumb` at fraction `f32`.
    To(Thumb, f32),
    /// A key or the wheel steps `Thumb` by this many units.
    Step(Thumb, f64),
    /// Home: `Thumb` to the least value.
    ToMin(Thumb),
    /// End: `Thumb` to the greatest.
    ToMax(Thumb),
    /// A double click: back to the default value.
    Reset,
    /// The drag let go.
    Release,
}

impl SliderInput {
    /// What the input makes of `value`, the number `thumb` stands for in
    /// `range`, `None` when it leaves it as it is or is not about it.
    pub fn apply(self, value: f64, range: &SliderRange, thumb: Thumb) -> Option<f64> {
        let next = match self {
            SliderInput::To(t, f) if t == thumb => range.at(f),
            SliderInput::Step(t, n) if t == thumb => range.snap(value + range.unit() * n),
            SliderInput::ToMin(t) if t == thumb => range.min.min(range.max),
            SliderInput::ToMax(t) if t == thumb => range.max.max(range.min),
            _ => return None,
        };
        (next != value).then_some(next)
    }

    /// The thumb it moves, if it moves one.
    pub fn thumb(self) -> Option<Thumb> {
        match self {
            SliderInput::To(t, _)
            | SliderInput::Step(t, _)
            | SliderInput::ToMin(t)
            | SliderInput::ToMax(t) => Some(t),
            SliderInput::Reset | SliderInput::Release => None,
        }
    }
}

/// What a drag does to a slider's bookkeeping, recorded by a handler and
/// applied by the router ([`EventCx::slide`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Slide {
    /// A drag starts, or moves on.
    Drag(Drag),
    /// The drag lets go.
    Release,
    /// A thumb of a range takes the focus, or none has it.
    Focus(Option<Thumb>),
}

/// What a sample on slider `node` asks of it — the keys acting on `focus`,
/// its focused thumb — recording the drag it starts or carries on; `None`
/// for one that asks nothing. The arrows, Page Up/Down, Home and End stop
/// propagating.
pub fn slider_input(ev: &mut EventCx<'_>, node: NodeId, focus: Thumb) -> Option<SliderInput> {
    let slider = ev.slider(node)?;
    if let Some(p) = ev.pointer().copied() {
        let rect = ev.rect_of(node)?;
        let rtl = ev.reads_rtl();
        let at = Vec2::new(p.x, p.y);
        let primary = p.buttons.contains(PointerButtons::PRIMARY);
        match p.phase {
            PointerPhase::Down if primary => {
                ev.capture_pointer(node);
                if ev.clicks() >= 2 {
                    return Some(SliderInput::Reset);
                }
                let (thumb, raw, jumped) = match slider.shape {
                    SliderShape::Knob => (Some(focus), slider.value[focus.at()], false),
                    SliderShape::Bar(_) => {
                        let f = slider.fraction_at(rect, rtl, at);
                        let thumb = slider.nearer(f);
                        let on = thumb.map_or(slider.shown[1], |t| slider.shown[t.at()]);
                        let lie = slider.lie(rect, rtl);
                        // On the thumb, it is picked up where it is.
                        let off = lie.span * (f - on);
                        if off.abs() <= lie.thumb * 0.5 {
                            (thumb, on, false)
                        } else {
                            (thumb, f, true)
                        }
                    }
                };
                ev.slide(node, Slide::Drag(Drag { thumb, raw, at }));
                return match thumb {
                    Some(t) if jumped => Some(SliderInput::To(t, slider.bounded(t, raw))),
                    _ => None,
                };
            }
            PointerPhase::Move if primary => {
                let drag = slider.drag?;
                let mut d = slider.carried(rect, rtl, at - drag.at);
                if p.modifiers.shift {
                    d *= FINE;
                }
                let thumb = drag.thumb.or_else(|| {
                    (d != 0.0).then_some(if d > 0.0 { Thumb::High } else { Thumb::Low })
                });
                let raw = drag.raw + d;
                ev.slide(node, Slide::Drag(Drag { thumb, raw, at }));
                let t = thumb?;
                return Some(SliderInput::To(t, slider.bounded(t, raw)));
            }
            PointerPhase::Up if slider.drag.is_some() => {
                ev.slide(node, Slide::Release);
                ev.release_pointer();
                return Some(SliderInput::Release);
            }
            _ => return None,
        }
    }
    if let Some(token) = ev.woken().filter(|t| t & 0xffff_0000 == WHEEL) {
        let steps = f64::from(token as u16 as i16);
        return Some(SliderInput::Step(focus, steps));
    }
    let k = ev.key().copied().filter(|k| k.pressed)?;
    let rtl = ev.reads_rtl();
    let input = match k.key {
        Key::Up => SliderInput::Step(focus, 1.0),
        Key::Down => SliderInput::Step(focus, -1.0),
        Key::Right => SliderInput::Step(focus, if rtl { -1.0 } else { 1.0 }),
        Key::Left => SliderInput::Step(focus, if rtl { 1.0 } else { -1.0 }),
        Key::PageUp => SliderInput::Step(focus, PAGE),
        Key::PageDown => SliderInput::Step(focus, -PAGE),
        Key::Home => SliderInput::ToMin(focus),
        Key::End => SliderInput::ToMax(focus),
        _ => return None,
    };
    ev.stop_propagation();
    Some(input)
}

/// The sliders of a store whose thumbs move.
#[derive(Debug, Default)]
pub(crate) struct Sliders {
    moving: Vec<NodeId>,
}

impl Sliders {
    pub(crate) fn clear(&mut self) {
        self.moving.clear();
    }

    /// Forgets the slider of the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        self.moving.retain(|id| id.index() != index);
    }
}

impl BuildCx<'_> {
    /// A slider leaf asking `size` of its parent; see
    /// [`NodeStore::set_slider`].
    pub fn slider_leaf(&mut self, size: Sizing, slider: Slider) -> Handle {
        let handle = self.leaf(LeafStyle {
            size,
            style: BoxStyle::NONE,
        });
        self.store.set_slider(handle.id(), slider);
        handle
    }
}

impl NodeStore {
    /// Node `id`'s slider, if it draws one.
    pub fn slider(&self, id: NodeId) -> Option<&Slider> {
        match self.content_payload(id) {
            Some(Content::Slider(slider)) => Some(slider),
            _ => None,
        }
    }

    fn slider_mut(&mut self, id: NodeId) -> Option<&mut Slider> {
        if !self.arena().is_live(id) {
            return None;
        }
        match self.content_payload_mut(id) {
            Some(Content::Slider(slider)) => Some(slider),
            _ => None,
        }
    }

    /// Make node `id` draw `slider`. One that draws a slider of its shape
    /// already keeps where its thumbs stand and its drag, and eases from
    /// there to `slider`'s values.
    pub fn set_slider(&mut self, id: NodeId, slider: Slider) {
        if !self.arena().is_live(id) {
            return;
        }
        let next = match self.slider(id) {
            Some(was) if was.shape == slider.shape && was.range == slider.range => {
                let next = Slider {
                    shown: was.shown,
                    drag: was.drag,
                    focus: was.focus,
                    wheel: was.wheel,
                    ..slider
                };
                if next == *was {
                    return;
                }
                if next.natural != was.natural {
                    self.mark_dirty(id, DirtyClass::MEASURE);
                }
                next
            }
            _ => {
                self.mark_dirty(id, DirtyClass::MEASURE);
                slider
            }
        };
        self.set_content_payload(id, Content::Slider(next));
        self.settle_slider(id);
    }

    /// Tell slider `id` where its thumbs stand, `low` and `high` (a plain
    /// slider's value): it eases there, or lands at once while a drag
    /// carries it or motion is reduced.
    pub fn show_slider(&mut self, id: NodeId, low: f32, high: f32) {
        let Some(slider) = self.slider_mut(id) else {
            return;
        };
        let value = if slider.range {
            let (low, high) = (clamp01(low), clamp01(high));
            [low.min(high), high.max(low)]
        } else {
            [0.0, clamp01(high)]
        };
        if slider.value == value {
            return;
        }
        slider.value = value;
        self.settle_slider(id);
    }

    /// Paint slider `id`'s track with `paint`.
    pub fn paint_slider(&mut self, id: NodeId, paint: TrackPaint) {
        let Some(slider) = self.slider_mut(id) else {
            return;
        };
        if slider.paint != paint {
            slider.paint = paint;
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// Land slider `id`'s thumbs where they are told to stand, at once.
    pub fn land_slider(&mut self, id: NodeId) {
        let Some(slider) = self.slider_mut(id) else {
            return;
        };
        if slider.moving() {
            slider.shown = slider.value;
            self.mark_dirty(id, DirtyClass::PAINT);
        }
        self.sliders.moving.retain(|&m| m != id);
    }

    /// Land slider `id` at once where it should, or set it easing there.
    fn settle_slider(&mut self, id: NodeId) {
        let snap = self.reduced_motion();
        let Some(slider) = self.slider_mut(id) else {
            return;
        };
        if snap || slider.drag.is_some() {
            slider.shown = slider.value;
        }
        let moving = slider.moving();
        self.mark_dirty(id, DirtyClass::PAINT);
        let list = &mut self.sliders.moving;
        match (moving, list.contains(&id)) {
            (true, false) => list.push(id),
            (false, true) => list.retain(|&m| m != id),
            _ => {}
        }
    }

    /// Apply what a handler made of a drag on slider `id`.
    pub(crate) fn slide(&mut self, id: NodeId, slide: Slide) {
        let Some(slider) = self.slider_mut(id) else {
            return;
        };
        let was = (slider.held(), slider.focus);
        match slide {
            Slide::Drag(drag) => slider.drag = Some(drag),
            Slide::Release => slider.drag = None,
            Slide::Focus(thumb) => slider.focus = thumb,
        }
        if (slider.held(), slider.focus) != was {
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// Step the focused slider under `target` — the slider, or a thumb of it
    /// — by the wheel's scroll `delta_y`, waking its handler for each step.
    /// Whether a focused slider took the scroll.
    pub(crate) fn wheel_slider(&mut self, target: NodeId, delta_y: f32) -> bool {
        let Some(focused) = self.focused() else {
            return false;
        };
        let mut node = Some(target);
        while let Some(n) = node {
            if self.slider(n).is_some() {
                break;
            }
            node = self.parent(n);
        }
        let Some(slider) = node else {
            return false;
        };
        if focused != slider && self.parent(focused) != Some(slider) {
            return false;
        }
        let Some(s) = self.slider_mut(slider) else {
            return false;
        };
        s.wheel -= delta_y;
        let steps = (s.wheel / WHEEL_STEP).trunc();
        s.wheel -= steps * WHEEL_STEP;
        if steps != 0.0 {
            let steps = steps.clamp(-1000.0, 1000.0) as i16;
            self.queue_wake(focused, WHEEL | u32::from(steps as u16));
        }
        true
    }

    /// Whether a slider that shows eases: the frame loop beats while one
    /// does.
    #[inline]
    pub fn is_sliding(&self) -> bool {
        self.sliders.moving.iter().any(|&id| self.shows(id))
    }

    /// Ease every moving slider by a frame of `delta`, repainting it; one
    /// that lands stops.
    pub fn tick_sliders(&mut self, delta: Duration) {
        let dt = delta.as_secs_f32().min(0.1);
        let k = 1.0 - (-dt / EASE).exp();
        let mut i = self.sliders.moving.len();
        while i > 0 {
            i -= 1;
            let id = self.sliders.moving[i];
            if !self.shows(id) {
                if self.slider(id).is_none() {
                    self.sliders.moving.swap_remove(i);
                }
                continue;
            }
            let Some(slider) = self.slider_mut(id) else {
                self.sliders.moving.swap_remove(i);
                continue;
            };
            for t in 0..2 {
                let gap = slider.value[t] - slider.shown[t];
                slider.shown[t] = if gap.abs() < 5e-4 {
                    slider.value[t]
                } else {
                    slider.shown[t] + gap * k
                };
            }
            let settled = !slider.moving();
            self.mark_dirty(id, DirtyClass::PAINT);
            if settled {
                self.sliders.moving.swap_remove(i);
            }
        }
    }
}

/// A knob's angle at fraction `f`, clockwise from the top: for tests.
#[cfg(test)]
fn knob_angle(f: f32) -> f32 {
    PI + GAP * 0.5 + (2.0 * PI - GAP) * f
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_4;

    const INK: Rgba = Rgba::new(0.2, 0.4, 0.9, 1.0);
    const GREY: Rgba = Rgba::new(0.5, 0.5, 0.5, 0.3);
    const WHITE: Rgba = Rgba::new(1.0, 1.0, 1.0, 1.0);

    fn bar(value: f32) -> Slider {
        Slider::new(SliderShape::Bar(Axis::Row), GREY, INK, WHITE, value)
    }

    #[test]
    fn a_range_snaps_to_its_grid_and_maps_fractions() {
        let r = SliderRange::new(0.0, 10.0, Some(2.0));
        assert_eq!(r.snap(4.9), 4.0);
        assert_eq!(r.snap(5.1), 6.0);
        assert_eq!(r.snap(11.0), 10.0);
        assert_eq!(r.at(0.5), 6.0, "5 rounds to the step above");
        assert_eq!(r.fraction(2.5), 0.25);
        assert_eq!(r.steps(), 5);
        assert_eq!(r.unit(), 2.0);
        let free = SliderRange::new(-1.0, 1.0, Some(0.0));
        assert_eq!(free.step, None);
        assert_eq!(free.unit(), 0.02);
        assert_eq!(free.at(0.75), 0.5);
        assert_eq!(SliderRange::new(3.0, 3.0, None).fraction(3.0), 0.0);
        assert_eq!(
            SliderInput::Step(Thumb::High, PAGE).apply(4.0, &r, Thumb::High),
            Some(10.0)
        );
        assert_eq!(
            SliderInput::Step(Thumb::High, 1.0).apply(10.0, &r, Thumb::High),
            None,
            "at its bound it stays"
        );
        assert_eq!(
            SliderInput::ToMin(Thumb::Low).apply(4.0, &r, Thumb::High),
            None
        );
    }

    #[test]
    fn a_bar_puts_its_value_along_its_axis_mirrored_right_to_left() {
        let s = bar(0.0);
        let rect = Rect::new(10.0, 0.0, 120.0, 24.0);
        assert_eq!(s.fraction_at(rect, false, Vec2::new(20.0, 5.0)), 0.0);
        assert_eq!(s.fraction_at(rect, false, Vec2::new(120.0, 5.0)), 1.0);
        assert_eq!(s.fraction_at(rect, false, Vec2::new(70.0, 5.0)), 0.5);
        assert_eq!(s.fraction_at(rect, true, Vec2::new(120.0, 5.0)), 0.0);
        let up = Slider::new(SliderShape::Bar(Axis::Column), GREY, INK, WHITE, 0.0);
        let tall = Rect::new(0.0, 0.0, 24.0, 120.0);
        assert_eq!(up.fraction_at(tall, false, Vec2::new(5.0, 110.0)), 0.0);
        assert_eq!(up.fraction_at(tall, false, Vec2::new(5.0, 10.0)), 1.0);
        assert_eq!(up.carried(tall, false, Vec2::new(0.0, -50.0)), 0.5);
        let knob = Slider::new(SliderShape::Knob, GREY, INK, WHITE, 0.0);
        assert_eq!(knob.carried(tall, false, Vec2::new(20.0, -30.0)), 0.25);
    }

    #[test]
    fn a_range_keeps_its_thumbs_apart_and_a_press_picks_the_nearer() {
        let s = bar(0.0).with_range(0.6, 0.2);
        assert_eq!(s.value, [0.2, 0.6]);
        assert_eq!(s.nearer(0.1), Some(Thumb::Low));
        assert_eq!(s.nearer(0.5), Some(Thumb::High));
        assert_eq!(s.bounded(Thumb::Low, 0.9), 0.6);
        assert_eq!(s.bounded(Thumb::High, 0.1), 0.2);
        let met = bar(0.0).with_range(0.4, 0.4);
        assert_eq!(met.nearer(0.4), None, "the drag's direction picks");
        assert_eq!(met.nearer(0.1), Some(Thumb::Low));
    }

    #[test]
    fn a_thumb_eases_unless_a_drag_carries_it() {
        let mut store = NodeStore::new();
        let root = BuildCx::new(&mut store)
            .slider_leaf(Sizing::fixed(160.0, 24.0), bar(0.0))
            .id();
        store.show_slider(root, 0.0, 1.0);
        assert!(store.is_sliding());
        store.tick_sliders(Duration::from_millis(16));
        let shown = store.slider(root).unwrap().shown[1];
        assert!(shown > 0.0 && shown < 1.0, "{shown} is on the way");
        for _ in 0..60 {
            store.tick_sliders(Duration::from_millis(16));
        }
        assert_eq!(store.slider(root).unwrap().shown[1], 1.0);
        assert!(!store.is_sliding());
        store.slide(
            root,
            Slide::Drag(Drag {
                thumb: Some(Thumb::High),
                raw: 1.0,
                at: Vec2::ZERO,
            }),
        );
        store.show_slider(root, 0.0, 0.3);
        assert_eq!(store.slider(root).unwrap().shown[1], 0.3, "it follows");
        assert!(!store.is_sliding());
        assert_eq!(store.slider(root).unwrap().held(), Some(Thumb::High));
        store.slide(root, Slide::Release);
        assert_eq!(store.slider(root).unwrap().held(), None);
    }

    #[test]
    fn a_bar_draws_its_track_fill_marks_and_thumb() {
        let s = bar(0.5).with_marks(4);
        let mut out = Vec::new();
        s.paint(Rect::new(0.0, 0.0, 120.0, 24.0), false, &mut out);
        let [
            Primitive::AnalyticCapsule(track),
            Primitive::AnalyticCapsule(fill),
            marks @ ..,
        ] = &out[..]
        else {
            panic!("{out:?}")
        };
        assert_eq!(track.rect, Rect::new(8.0, 10.0, 104.0, 4.0));
        assert_eq!(fill.rect, Rect::new(8.0, 10.0, 54.0, 4.0));
        let dots = marks
            .iter()
            .filter(|p| matches!(p, Primitive::AnalyticEllipse(e) if e.rect.w == MARK))
            .count();
        assert_eq!(dots, 5);
        let Some(Primitive::Quad(thumb)) = out.last() else {
            panic!()
        };
        assert_eq!(thumb.rect, Rect::new(50.0, 2.0, 20.0, 20.0));
        // Crowded, the marks give way.
        let mut out = Vec::new();
        bar(0.5)
            .with_marks(100)
            .paint(Rect::new(0.0, 0.0, 120.0, 24.0), false, &mut out);
        assert!(
            !out.iter()
                .any(|p| matches!(p, Primitive::AnalyticEllipse(_)))
        );
    }

    #[test]
    fn a_knob_opens_at_the_bottom_and_points_at_its_value() {
        assert!((knob_angle(0.0) - 5.0 * FRAC_PI_4).abs() < 1e-5);
        assert!(
            (knob_angle(0.5) - 2.0 * PI).abs() < 1e-5,
            "half way is the top"
        );
        let s = Slider::new(SliderShape::Knob, GREY, INK, WHITE, 0.5);
        let mut out = Vec::new();
        s.paint(Rect::new(0.0, 0.0, 40.0, 40.0), false, &mut out);
        let Some(Primitive::Path(line)) = out.last() else {
            panic!("{out:?}")
        };
        let [
            viso_render::PathCmd::MoveTo(a),
            viso_render::PathCmd::LineTo(b),
        ] = line.cmds[..]
        else {
            panic!()
        };
        assert!((a.x - 20.0).abs() < 1e-3 && (b.x - 20.0).abs() < 1e-3);
        assert!(b.y < a.y, "it points up");
    }
}
