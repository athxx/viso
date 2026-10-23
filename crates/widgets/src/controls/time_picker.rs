//! The [`TimePicker`] control — a time of day set a part at a time.
//!
//! The field holds the time as segments: the hour, the minute, the second
//! when [`seconds`](TimePicker::seconds) asks, and AM or PM on a clock that
//! runs to twelve — the locale's ([`viso_ui::calendar::hour_cycle`]) unless
//! [`hour_cycle`](TimePicker::hour_cycle) says otherwise. Each segment is a
//! Tab stop of its own and shows on the accent while it has the focus;
//! before a time is set each shows a placeholder.
//!
//! Up and Down step the focused segment round its range — the hour round
//! its clock keeping AM or PM, the minute by
//! [`minute_step`](TimePicker::minute_step) — and Home and End go to its
//! ends; Left and Right move between segments (mirrored right to left).
//! Typed digits set the segment, moving on to the next once no other digit
//! could follow: `7` is a whole hour, `1` waits for a second digit. On AM or
//! PM, `a` and `p` (the first letters of its names) set it, as Up and Down
//! do. A time outside [`min`](TimePicker::min) and
//! [`max`](TimePicker::max) is held to them. Each change fires `on_change`
//! with the time.
//!
//! Semantics: a group named by its label holding a spin button for each
//! segment, announcing its value and range; AM or PM announces its name.
//!
//! Invalidation: the time lives in one reactive cell, each segment's focus
//! in another; a structure hook shapes the segments' text again, and the
//! focus picks the segment's look.
//!
//! ```
//! use viso_widgets::time_picker;
//! use viso_ui::calendar::Time;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
//!
//! let alarm = time_picker("Alarm")
//!     .value(Time::new(7, 30, 0).unwrap())
//!     .minute_step(5)
//!     .on_change(|_ev, time| {
//!         let _ = time;
//!     });
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! alarm.build(&mut cx);
//! ```

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_ui::adaptive::EnvField;
use viso_ui::calendar::{HourCycle, NO_TIME, Time, hour_cycle};
use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, DispatchPhase, EventCx, FlexStyle, FocusChange,
    FontSize, FontWeight, Handle, ImeEvent, Inset, InteractionStyle, Justify, Key, LeafStyle,
    Length, NodeId, PointerButtons, PointerPhase, Rgba, Role, SemanticState, Semantics, Size,
    StateId, StateValue, StructureCx, TextAlign, TextRequest, TextStyle,
};

/// What runs with a new time.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, Time)>>>>;

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

/// The names a time picker gives its parts.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeNames {
    pub am: String,
    pub pm: String,
    pub hours: String,
    pub minutes: String,
    pub seconds: String,
    /// The AM or PM segment.
    pub period: String,
    /// What a segment shows before a time is set.
    pub placeholder: String,
}

impl Default for TimeNames {
    fn default() -> Self {
        TimeNames {
            am: "AM".to_owned(),
            pm: "PM".to_owned(),
            hours: "Hours".to_owned(),
            minutes: "Minutes".to_owned(),
            seconds: "Seconds".to_owned(),
            period: "AM/PM".to_owned(),
            placeholder: "--".to_owned(),
        }
    }
}

/// How a time picker looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimePickerStyle {
    pub field: BoxStyle,
    pub field_padding: Inset,
    pub font_size: f32,
    /// The time, and the placeholders and separators.
    pub ink: Rgba,
    pub quiet: Rgba,
    /// The focused segment's fill and ink.
    pub accent: Rgba,
    pub selected_ink: Rgba,
}

impl Default for TimePickerStyle {
    fn default() -> Self {
        TimePickerStyle {
            field: BoxStyle {
                border: viso_ui::Border {
                    width: 1.0,
                    color: rgba(0.0, 0.0, 0.0, 0.16),
                },
                ..BoxStyle::solid(rgba(1.0, 1.0, 1.0, 1.0)).with_radius(6.0)
            },
            field_padding: Inset {
                left: 6.0,
                right: 6.0,
                top: 4.0,
                bottom: 4.0,
            },
            font_size: 14.0,
            ink: rgba(0.09, 0.09, 0.1, 1.0),
            quiet: rgba(0.42, 0.42, 0.45, 1.0),
            accent: rgba(0.22, 0.45, 0.85, 1.0),
            selected_ink: rgba(1.0, 1.0, 1.0, 1.0),
        }
    }
}

/// A time of day set a part at a time. Construct one with [`time_picker`].
pub struct TimePicker {
    label: String,
    value: Option<Time>,
    cell: Option<StateId>,
    min: Option<Time>,
    max: Option<Time>,
    seconds: bool,
    minute_step: u32,
    hour_cycle: Option<HourCycle>,
    names: TimeNames,
    style: TimePickerStyle,
    on_change: SharedChange,
}

/// A time picker named `label`, no time set, to the minute.
pub fn time_picker(label: impl Into<String>) -> TimePicker {
    TimePicker {
        label: label.into(),
        value: None,
        cell: None,
        min: None,
        max: None,
        seconds: false,
        minute_step: 1,
        hour_cycle: None,
        names: TimeNames::default(),
        style: TimePickerStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl TimePicker {
    /// Run `handler` with each new time.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, Time) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Start at `time`.
    pub fn value(mut self, time: Time) -> Self {
        self.value = Some(time);
        self
    }

    /// Keep the time in `cell`, which it shows and writes: the time's
    /// [`Time::to_state`], [`NO_TIME`] for none.
    pub fn bind(mut self, cell: StateId) -> Self {
        self.cell = Some(cell);
        self
    }

    /// Hold the time at `time` or later.
    pub fn min(mut self, time: Time) -> Self {
        self.min = Some(time);
        self
    }

    /// Hold the time at `time` or earlier.
    pub fn max(mut self, time: Time) -> Self {
        self.max = Some(time);
        self
    }

    /// Set the second too.
    pub fn seconds(mut self, on: bool) -> Self {
        self.seconds = on;
        self
    }

    /// Step the minute by `step` (default 1).
    pub fn minute_step(mut self, step: u32) -> Self {
        self.minute_step = step.clamp(1, 30);
        self
    }

    /// Run the clock to twelve or to twenty-four rather than as the locale
    /// does.
    pub fn hour_cycle(mut self, cycle: HourCycle) -> Self {
        self.hour_cycle = Some(cycle);
        self
    }

    /// Name its parts as `names` do.
    pub fn names(mut self, names: TimeNames) -> Self {
        self.names = names;
        self
    }

    /// Replace the whole [`TimePickerStyle`].
    pub fn style(mut self, style: TimePickerStyle) -> Self {
        self.style = style;
        self
    }
}

/// A segment of the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seg {
    Hour,
    Minute,
    Second,
    Period,
}

/// One segment's node and the cell its focus is in.
#[derive(Debug, Clone, Copy)]
struct Part {
    seg: Seg,
    leaf: Handle,
    node: NodeId,
    focused: StateId,
}

/// The digits being typed into a segment.
#[derive(Debug, Clone, Copy, Default)]
struct Typed {
    seg: Option<Seg>,
    /// The number typed so far, and how many digits.
    digits: u32,
    count: u32,
    /// The time when the typing began.
    from: Option<Time>,
}

/// What a time picker shares between its handlers and its hook.
struct Picker {
    cell: StateId,
    parts: Vec<Part>,
    /// The clock the segments show.
    cycle: Cell<HourCycle>,
    fixed_cycle: Option<HourCycle>,
    typed: RefCell<Typed>,
    min: Option<Time>,
    max: Option<Time>,
    step: u32,
    names: TimeNames,
    style: TimePickerStyle,
    on_change: SharedChange,
}

impl Picker {
    /// The time the cell holds.
    fn time(&self, ev: &EventCx<'_>) -> Option<Time> {
        Time::of_state(ev.get(self.cell))
    }

    /// What stepping from no time starts at.
    fn base(&self) -> Time {
        self.min.unwrap_or(Time::MIDNIGHT)
    }

    fn twelve(&self) -> bool {
        self.cycle.get() == HourCycle::H12
    }

    /// The segments that show, in order.
    fn shown(&self) -> impl Iterator<Item = &Part> {
        self.parts
            .iter()
            .filter(move |p| p.seg != Seg::Period || self.twelve())
    }

    /// Set `time`, held to the bounds, reporting it.
    fn set(&self, ev: &mut EventCx<'_>, time: Time) {
        let mut time = time;
        if let Some(min) = self.min {
            time = time.max(min);
        }
        if let Some(max) = self.max {
            time = time.min(max);
        }
        if self.time(ev) == Some(time) {
            return;
        }
        ev.set(self.cell, time.to_state());
        if let Some(f) = self.on_change.borrow_mut().as_mut() {
            f(ev, time);
        }
    }

    /// `time` with segment `seg` at `value`: the hour as the clock shows it.
    fn with(&self, time: Time, seg: Seg, value: u32) -> Time {
        let (h, m, s) = (time.hour(), time.minute(), time.second());
        let t = match seg {
            Seg::Hour if self.twelve() => {
                Time::new(value % 12 + if time.is_pm() { 12 } else { 0 }, m, s)
            }
            Seg::Hour => Time::new(value, m, s),
            Seg::Minute => Time::new(h, value, s),
            Seg::Second => Time::new(h, m, value),
            Seg::Period => Time::new(h % 12 + if value == 1 { 12 } else { 0 }, m, s),
        };
        t.unwrap_or(time)
    }

    /// Segment `seg` stepped `n` times round its range.
    fn step(&self, ev: &mut EventCx<'_>, seg: Seg, n: i32) {
        let time = self.time(ev).unwrap_or_else(|| self.base());
        let next = match seg {
            Seg::Hour if self.twelve() => {
                let h12 = (time.hour12() as i32 - 1 + n).rem_euclid(12) + 1;
                self.with(time, seg, h12 as u32)
            }
            Seg::Hour => self.with(time, seg, (time.hour() as i32 + n).rem_euclid(24) as u32),
            Seg::Minute => {
                let step = self.step as i32;
                let on = time.minute() as i32 / step * step;
                let off = time.minute() as i32 != on;
                // Off the grid, the first step down lands on it.
                let by = if off && n < 0 {
                    (n + 1) * step
                } else {
                    n * step
                };
                self.with(time, seg, (on + by).rem_euclid(60) as u32)
            }
            Seg::Second => self.with(time, seg, (time.second() as i32 + n).rem_euclid(60) as u32),
            Seg::Period => self.with(time, seg, u32::from(!time.is_pm())),
        };
        self.set(ev, next);
    }

    /// Segment `seg` at the end of its range: its last when `last`.
    fn end(&self, ev: &mut EventCx<'_>, seg: Seg, last: bool) {
        let time = self.time(ev).unwrap_or_else(|| self.base());
        let value = match (seg, last) {
            (Seg::Hour, false) if self.twelve() => 12,
            (Seg::Hour, true) if self.twelve() => 11,
            (Seg::Hour, false) | (Seg::Minute | Seg::Second | Seg::Period, false) => 0,
            (Seg::Hour, true) => 23,
            (Seg::Minute, true) => 60 - self.step,
            (Seg::Second, true) => 59,
            (Seg::Period, true) => 1,
        };
        self.set(ev, self.with(time, seg, value));
    }

    /// The segment `by` along from `seg`, if one shows there.
    fn along(&self, seg: Seg, by: i32) -> Option<&Part> {
        let shown: Vec<&Part> = self.shown().collect();
        let at = shown.iter().position(|p| p.seg == seg)? as i32 + by;
        usize::try_from(at)
            .ok()
            .and_then(|at| shown.get(at).copied())
    }

    /// Focus the segment `by` along from `seg`.
    fn move_focus(&self, ev: &mut EventCx<'_>, seg: Seg, by: i32) -> bool {
        let Some(part) = self.along(seg, by) else {
            return false;
        };
        ev.request_focus(part.node);
        true
    }

    /// A digit typed on segment `seg`; the segment the next goes to.
    fn digit(&self, ev: &mut EventCx<'_>, seg: Seg, d: u32) -> Seg {
        let mut typed = self.typed.borrow_mut();
        if typed.seg != Some(seg) {
            *typed = Typed {
                seg: Some(seg),
                digits: 0,
                count: 0,
                from: self.time(ev),
            };
        }
        let (low, high) = match seg {
            Seg::Hour if self.twelve() => (1, 12),
            Seg::Hour => (0, 23),
            _ => (0, 59),
        };
        let mut n = typed.digits * 10 + d;
        typed.count += 1;
        if n > high {
            // Too big with the digit before: it starts again.
            n = d;
            typed.count = 1;
        }
        typed.digits = n;
        // Whether a second digit could follow this one.
        let more = typed.count < 2 && n * 10 <= high;
        // The other segments as they were before the typing, whatever the
        // bounds made of its first digit.
        let from = typed.from.unwrap_or_else(|| self.base());
        if !more {
            *typed = Typed::default();
        }
        drop(typed);
        if n >= low {
            self.set(ev, self.with(from, seg, n));
        }
        if more {
            return seg;
        }
        match self.along(seg, 1) {
            Some(next) => {
                ev.request_focus(next.node);
                next.seg
            }
            None => seg,
        }
    }

    /// What a key or typed text on segment `seg` asks.
    fn key(&self, ev: &mut EventCx<'_>, part: Part) {
        let mut seg = part.seg;
        if let Some(change) = ev.focus_change() {
            ev.set(
                part.focused,
                StateValue::Bool(change == FocusChange::Gained),
            );
            *self.typed.borrow_mut() = Typed::default();
            return;
        }
        if let Some(ImeEvent::Commit { text }) = ev.ime().cloned() {
            ev.stop_propagation();
            for c in text.chars() {
                let lower = c.to_lowercase().next().unwrap_or(c);
                let first = |name: &str| name.chars().next().map(|f| f.to_lowercase().next());
                if seg == Seg::Period {
                    let time = self.time(ev).unwrap_or_else(|| self.base());
                    if first(&self.names.am) == Some(Some(lower)) {
                        self.set(ev, self.with(time, seg, 0));
                    } else if first(&self.names.pm) == Some(Some(lower)) {
                        self.set(ev, self.with(time, seg, 1));
                    }
                } else if let Some(d) = c.to_digit(10) {
                    seg = self.digit(ev, seg, d);
                }
            }
            return;
        }
        let Some(k) = ev.key().copied().filter(|k| k.pressed) else {
            return;
        };
        let across = if ev.reads_rtl() { -1 } else { 1 };
        let taken = match k.key {
            Key::Up => {
                self.step(ev, seg, 1);
                true
            }
            Key::Down => {
                self.step(ev, seg, -1);
                true
            }
            Key::Home => {
                self.end(ev, seg, false);
                true
            }
            Key::End => {
                self.end(ev, seg, true);
                true
            }
            Key::Right => self.move_focus(ev, seg, across),
            Key::Left => self.move_focus(ev, seg, -across),
            _ => false,
        };
        if taken {
            *self.typed.borrow_mut() = Typed::default();
            ev.stop_propagation();
        }
    }

    /// What segment `seg` is called.
    fn name(&self, seg: Seg) -> String {
        match seg {
            Seg::Hour => &self.names.hours,
            Seg::Minute => &self.names.minutes,
            Seg::Second => &self.names.seconds,
            Seg::Period => &self.names.period,
        }
        .clone()
    }

    /// Show the time on the segments.
    fn show(&self, s: &mut StructureCx<'_>) {
        let cycle = self
            .fixed_cycle
            .unwrap_or_else(|| hour_cycle(&s.states.env().environment().locale));
        self.cycle.set(cycle);
        let twelve = cycle == HourCycle::H12;
        let time = Time::of_state(s.states.get(self.cell));
        let style = &self.style;
        for part in &self.parts {
            if part.seg == Seg::Period {
                s.store.set_hidden(part.node, !twelve);
            }
            let focused = matches!(s.states.get(part.focused), Some(StateValue::Bool(true)));
            let (text, value, range) = match (part.seg, time) {
                (_, None) => (self.names.placeholder.clone(), None, (0.0, 0.0)),
                (Seg::Hour, Some(t)) if twelve => {
                    (t.hour12().to_string(), Some(t.hour12()), (1.0, 12.0))
                }
                (Seg::Hour, Some(t)) => (format!("{:02}", t.hour()), Some(t.hour()), (0.0, 23.0)),
                (Seg::Minute, Some(t)) => {
                    (format!("{:02}", t.minute()), Some(t.minute()), (0.0, 59.0))
                }
                (Seg::Second, Some(t)) => {
                    (format!("{:02}", t.second()), Some(t.second()), (0.0, 59.0))
                }
                (Seg::Period, Some(t)) => {
                    let name = if t.is_pm() {
                        &self.names.pm
                    } else {
                        &self.names.am
                    };
                    (name.clone(), Some(u32::from(t.is_pm())), (0.0, 1.0))
                }
            };
            let ink = if focused {
                style.selected_ink
            } else if time.is_none() {
                style.quiet
            } else {
                style.ink
            };
            s.store
                .set_text(part.node, line(&text, style.font_size, ink));
            let state = value.map_or(SemanticState::default(), |v| {
                SemanticState::number(f64::from(v), range.0, range.1)
            });
            s.store.set_semantic_state(part.node, state);
            let mut semantics = Semantics::role(Role::SpinButton).with_label(self.name(part.seg));
            semantics.value = time.map(|_| text);
            s.store.set_semantics(part.node, semantics);
        }
    }
}

/// A line of `text`, centred.
fn line(text: &str, size: f32, ink: Rgba) -> TextRequest {
    TextRequest {
        spans: Vec::new(),
        text: text.to_owned(),
        font_size: FontSize::Fixed(size),
        color: ink,
        soft_wrap: false,
        locale: None,
        style: TextStyle {
            weight: FontWeight(400),
            align: TextAlign::Center,
            ..TextStyle::default()
        },
    }
}

impl Component for TimePicker {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let style = self.style;
        let cell = self.cell.unwrap_or_else(|| {
            cx.state(StateValue::Int(self.value.map_or(NO_TIME, Time::seconds)))
        });
        let mut segs = vec![Seg::Hour, Seg::Minute];
        if self.seconds {
            segs.push(Seg::Second);
        }
        segs.push(Seg::Period);
        let mut parts = Vec::with_capacity(segs.len());
        let field = cx.flex(
            FlexStyle {
                axis: Axis::Row,
                padding: style.field_padding,
                align: Align::Center,
                justify: Justify::Start,
                style: style.field,
                ..FlexStyle::default()
            },
            |cx| {
                for (i, &seg) in segs.iter().enumerate() {
                    if i > 0 {
                        let gap = if seg == Seg::Period { " " } else { ":" };
                        let sep = cx.leaf(LeafStyle {
                            size: Size {
                                width: Length::Fit,
                                height: Length::Fit,
                            },
                            style: BoxStyle::NONE,
                        });
                        cx.text_request(sep, line(gap, style.font_size, style.quiet));
                    }
                    let width = match seg {
                        Seg::Period => style.font_size * 2.4,
                        _ => style.font_size * 1.6,
                    };
                    let leaf = cx.leaf(LeafStyle {
                        size: Size {
                            width: Length::Fixed(width),
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                    });
                    cx.text_request(leaf, line("", style.font_size, style.ink));
                    let focused = cx.state(StateValue::Bool(false));
                    let resting = BoxStyle::NONE.with_radius(4.0);
                    cx.interaction_style(
                        leaf,
                        InteractionStyle {
                            resting,
                            hover: resting,
                            pressed: BoxStyle::solid(style.accent).with_radius(4.0),
                            pressed_cell: Some(focused),
                            hover_cell: None,
                        },
                    );
                    cx.focusable(leaf, true);
                    cx.focus_ring(leaf, false);
                    parts.push(Part {
                        seg,
                        leaf,
                        node: leaf.id(),
                        focused,
                    });
                }
            },
        );
        cx.semantics(
            field,
            Semantics::role(Role::Group).with_label(self.label.clone()),
        );
        let picker = Rc::new(Picker {
            cell,
            parts: parts.clone(),
            cycle: Cell::new(HourCycle::H24),
            fixed_cycle: self.hour_cycle,
            typed: RefCell::new(Typed::default()),
            min: self.min,
            max: self.max,
            step: self.minute_step,
            names: self.names.clone(),
            style,
            on_change: Rc::clone(&self.on_change),
        });
        cx.structure(|s| {
            picker.show(s);
            let mut deps: Vec<StateId> = vec![cell];
            deps.extend(parts.iter().map(|p| p.focused));
            deps.extend(s.states.env_cell(EnvField::Locale));
            let hook = Rc::clone(&picker);
            s.store.add_structure_hook(deps, move |s, _| hook.show(s));
        });
        for part in parts {
            cx.on_pointer(part.leaf, move |ev| {
                if ev.phase() == DispatchPhase::Capture {
                    return;
                }
                if let Some(p) = ev.pointer()
                    && p.phase == PointerPhase::Down
                    && p.buttons.contains(PointerButtons::PRIMARY)
                    && ev.focused() != Some(part.node)
                {
                    ev.request_focus(part.node);
                }
            });
            let p = Rc::clone(&picker);
            cx.on_key(part.leaf, move |ev| p.key(ev, part));
        }
    }
}
