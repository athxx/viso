//! The [`Calendar`] control — a month of days to pick a date from.
//!
//! A calendar shows one month as six weeks: a header with the month and its
//! year between buttons to the month before and after, a row naming the
//! days of the week, and the weeks, each a row of seven days, the days of
//! the months either side quieter. Its weeks start on the day the locale's
//! do ([`viso_ui::calendar::first_weekday`]) unless
//! [`first_weekday`](Calendar::first_weekday) says otherwise, and may lead
//! with their ISO week numbers. The selected day sits on a disc, today in a
//! ring, and the day under the pointer on a faint disc.
//!
//! A click on a day picks it — one of a month either side shows that month.
//! The days are one Tab stop, landing on the selected day; the arrows move a
//! day (across, mirrored right to left) or a week (up and down), Home and
//! End to the start and end of the week, Page Up and Page Down a month, with
//! Shift a year, moving to the month they reach; Enter or Space picks. Days
//! before [`min`](Calendar::min), after [`max`](Calendar::max) or that
//! [`unavailable`](Calendar::unavailable) refuses show quieter, cannot be
//! picked, and the keys pass over them. Each pick fires `on_change` with the
//! date.
//!
//! Semantics: a group named by its label holding the two buttons and a
//! table named by the month — a row of column headers naming the days of
//! the week, then a row for each week whose cells are its days, each named
//! by its full date, announcing whether it is selected, with a hint for
//! today and for a day that cannot be picked.
//!
//! Invalidation: the date lives in one reactive cell, the month shown in
//! another; a structure hook relabels just the days whose date or look
//! changed, and marks each week's selected day and today.
//!
//! ```
//! use viso_widgets::calendar;
//! use viso_ui::calendar::Date;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
//!
//! let due = calendar("Due")
//!     .value(Date::new(2026, 10, 24).unwrap())
//!     .min(Date::new(2026, 1, 1).unwrap())
//!     .week_numbers(true)
//!     .on_change(|_ev, date| {
//!         let _ = date;
//!     });
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! due.build(&mut cx);
//! ```

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_ui::adaptive::EnvField;
use viso_ui::calendar::{Date, Days, NO_DATE, Weekday, first_weekday};
use viso_ui::{
    Access, Align, Axis, BoxStyle, BuildCx, Component, DispatchPhase, EventCx, FlexStyle, FontSize,
    FontWeight, Handle, InteractionStyle, Justify, Key, LeafStyle, LengthTerms, NodeId,
    PointerButtons, PointerPhase, Rgba, Role, SemanticState, Semantics, Size, StateId, StateValue,
    StructureCx, TextAlign, TextRequest, TextStyle,
};

use super::glyphs::{chevron_end, chevron_start};

/// What runs with a picked date.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, Date)>>>>;

/// Which days cannot be picked.
type Refusal = Rc<dyn Fn(Date) -> bool>;

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

/// The weeks a month shows, and the days in each.
const WEEKS: usize = 6;
const DAYS: usize = 7;

/// The names a calendar gives months, days and its parts, and how it
/// writes a date.
#[derive(Debug, Clone)]
pub struct DateNames {
    /// January first.
    pub months: [String; 12],
    pub short_months: [String; 12],
    /// Monday first.
    pub weekdays: [String; 7],
    /// What heads each day's column, Monday first.
    pub short_weekdays: [String; 7],
    /// The buttons to the month before and the month after.
    pub previous: String,
    pub next: String,
    /// The hints on today and on a day that cannot be picked.
    pub today: String,
    pub unavailable: String,
    /// What a week number is called.
    pub week: String,
    /// A month and its year: "October 2026".
    pub title: fn(&DateNames, Date) -> String,
    /// A date in full: "Saturday, October 24, 2026".
    pub long: fn(&DateNames, Date) -> String,
    /// A date in brief: "Oct 24, 2026".
    pub short: fn(&DateNames, Date) -> String,
}

fn strings<const N: usize>(names: [&str; N]) -> [String; N] {
    names.map(str::to_owned)
}

impl Default for DateNames {
    fn default() -> Self {
        DateNames {
            months: strings([
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ]),
            short_months: strings([
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ]),
            weekdays: strings([
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
                "Sunday",
            ]),
            short_weekdays: strings(["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"]),
            previous: "Previous month".to_owned(),
            next: "Next month".to_owned(),
            today: "Today".to_owned(),
            unavailable: "Unavailable".to_owned(),
            week: "Week".to_owned(),
            title: |names, d| format!("{} {}", names.month(d), d.year()),
            long: |names, d| {
                format!(
                    "{}, {} {}, {}",
                    names.weekday(d.weekday()),
                    names.month(d),
                    d.day(),
                    d.year()
                )
            },
            short: |names, d| {
                format!(
                    "{} {}, {}",
                    names.short_months[d.month() as usize - 1],
                    d.day(),
                    d.year()
                )
            },
        }
    }
}

impl DateNames {
    /// The name of `date`'s month.
    pub fn month(&self, date: Date) -> &str {
        &self.months[date.month() as usize - 1]
    }

    pub fn weekday(&self, day: Weekday) -> &str {
        &self.weekdays[day.index() as usize]
    }
}

/// How a calendar looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalendarStyle {
    /// The side of a day's square.
    pub cell: f32,
    pub font_size: f32,
    /// The month and year's size.
    pub title_size: f32,
    /// A day of the month shown.
    pub ink: Rgba,
    /// The days of other months, the column heads and the week numbers.
    pub quiet: Rgba,
    /// The day on the selected disc.
    pub selected_ink: Rgba,
    /// The selected disc and today's ring.
    pub accent: Rgba,
    /// The disc under the pointer, and under a button it is over.
    pub hover: Rgba,
}

impl Default for CalendarStyle {
    fn default() -> Self {
        CalendarStyle {
            cell: 36.0,
            font_size: 14.0,
            title_size: 15.0,
            ink: rgba(0.09, 0.09, 0.1, 1.0),
            quiet: rgba(0.42, 0.42, 0.45, 1.0),
            selected_ink: rgba(1.0, 1.0, 1.0, 1.0),
            accent: rgba(0.22, 0.45, 0.85, 1.0),
            hover: rgba(0.46, 0.46, 0.5, 0.14),
        }
    }
}

/// A month of days to pick a date from. Construct one with [`calendar`].
pub struct Calendar {
    pub(crate) label: String,
    pub(crate) value: Option<Date>,
    pub(crate) cell: Option<StateId>,
    pub(crate) min: Option<Date>,
    pub(crate) max: Option<Date>,
    pub(crate) unavailable: Option<Refusal>,
    pub(crate) first_weekday: Option<Weekday>,
    pub(crate) week_numbers: bool,
    pub(crate) today: Option<Date>,
    pub(crate) names: DateNames,
    pub(crate) style: CalendarStyle,
    pub(crate) on_change: SharedChange,
}

/// A calendar named `label` with no date picked, showing this month.
pub fn calendar(label: impl Into<String>) -> Calendar {
    Calendar {
        label: label.into(),
        value: None,
        cell: None,
        min: None,
        max: None,
        unavailable: None,
        first_weekday: None,
        week_numbers: false,
        today: None,
        names: DateNames::default(),
        style: CalendarStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl Calendar {
    /// Run `handler` with each date picked.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, Date) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Start with `date` picked, showing its month.
    pub fn value(mut self, date: Date) -> Self {
        self.value = Some(date);
        self
    }

    /// Keep the date in `cell`, which it shows and writes: the date's
    /// [`Date::to_state`], [`NO_DATE`] for none.
    pub fn bind(mut self, cell: StateId) -> Self {
        self.cell = Some(cell);
        self
    }

    /// Let no day before `date` be picked.
    pub fn min(mut self, date: Date) -> Self {
        self.min = Some(date);
        self
    }

    /// Let no day after `date` be picked.
    pub fn max(mut self, date: Date) -> Self {
        self.max = Some(date);
        self
    }

    /// Let no day be picked that `refuse` refuses.
    pub fn unavailable(mut self, refuse: impl Fn(Date) -> bool + 'static) -> Self {
        self.unavailable = Some(Rc::new(refuse));
        self
    }

    /// Start the weeks on `day` rather than the locale's first.
    pub fn first_weekday(mut self, day: Weekday) -> Self {
        self.first_weekday = Some(day);
        self
    }

    /// Lead each week with its ISO week number.
    pub fn week_numbers(mut self, on: bool) -> Self {
        self.week_numbers = on;
        self
    }

    /// Ring `date` as today rather than the system clock's.
    pub fn today(mut self, date: Date) -> Self {
        self.today = Some(date);
        self
    }

    /// Name the months, the days and its parts, and write dates, as `names`
    /// do.
    pub fn names(mut self, names: DateNames) -> Self {
        self.names = names;
        self
    }

    /// Replace the whole [`CalendarStyle`].
    pub fn style(mut self, style: CalendarStyle) -> Self {
        self.style = style;
        self
    }
}

/// How a day shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Look {
    /// A day of the month shown.
    Plain,
    /// A day of a month either side.
    Other,
    Selected,
    /// One that cannot be picked.
    Off,
}

/// What a calendar shares between its handlers and its hook.
pub(crate) struct Month {
    /// The picked date and the month shown, a [`Date::month_index`].
    pub(crate) value: StateId,
    month: StateId,
    /// The day the grid starts on, and the weeks' first day.
    pub(crate) start: Cell<Date>,
    first: Cell<Weekday>,
    fixed_first: Option<Weekday>,
    min: Option<Date>,
    max: Option<Date>,
    unavailable: Option<Refusal>,
    today: Option<Date>,
    title: NodeId,
    table: NodeId,
    pub(crate) previous: NodeId,
    pub(crate) next: NodeId,
    heads: [NodeId; DAYS],
    weeks: [NodeId; WEEKS],
    numbers: Option<[NodeId; WEEKS]>,
    days: [[NodeId; DAYS]; WEEKS],
    /// What each day last showed.
    shown: RefCell<[Option<(Date, Look)>; WEEKS * DAYS]>,
    names: DateNames,
    style: CalendarStyle,
    on_change: SharedChange,
}

impl Month {
    /// Whether `date` may be picked.
    fn available(&self, date: Date) -> bool {
        self.min.is_none_or(|m| date >= m)
            && self.max.is_none_or(|m| date <= m)
            && self.unavailable.as_ref().is_none_or(|refuse| !refuse(date))
    }

    fn day(&self, k: usize) -> NodeId {
        self.days[k / DAYS][k % DAYS]
    }

    /// The month shown, as its first.
    fn shown_month(&self, value: Option<StateValue>) -> Date {
        match value {
            Some(StateValue::Int(m)) => Date::of_month_index(m),
            _ => Date::today().first_of_month(),
        }
    }

    /// Show the month and the date the cells hold.
    fn show(&self, s: &mut StructureCx<'_>) {
        let first = self
            .fixed_first
            .unwrap_or_else(|| first_weekday(&s.states.env().environment().locale));
        self.first.set(first);
        let month = self.shown_month(s.states.get(self.month));
        let start = month.grid_start(first);
        self.start.set(start);
        let picked = Date::of_state(s.states.get(self.value));
        let today = self.today.unwrap_or_else(Date::today);
        let style = &self.style;
        let title = (self.names.title)(&self.names, month);
        if s.store
            .semantics(self.table)
            .and_then(|t| t.label.as_deref())
            != Some(&title)
        {
            s.store.set_text(
                self.title,
                line(
                    &title,
                    style.title_size,
                    style.ink,
                    FontWeight(600),
                    TextAlign::Center,
                    None,
                ),
            );
            s.store
                .set_semantics(self.table, Semantics::role(Role::Table).with_label(title));
        }
        for (i, &head) in self.heads.iter().enumerate() {
            let day = first.after(i as u32);
            let name = &self.names.short_weekdays[day.index() as usize];
            s.store.set_text(
                head,
                line(
                    name,
                    style.font_size * 0.85,
                    style.quiet,
                    FontWeight(500),
                    TextAlign::Center,
                    Some(style.cell),
                ),
            );
            s.store.set_semantics(
                head,
                Semantics::role(Role::ColumnHeader).with_label(self.names.weekday(day)),
            );
        }
        let mut shown = self.shown.borrow_mut();
        for (w, &row) in self.weeks.iter().enumerate() {
            let mut marks = Days::new(u8::from(self.numbers.is_some()), style.accent, style.hover);
            for d in 0..DAYS {
                let k = w * DAYS + d;
                let date = start.add_days(k as i32);
                let look = if !self.available(date) {
                    Look::Off
                } else if picked == Some(date) {
                    Look::Selected
                } else if date.month() != month.month() {
                    Look::Other
                } else {
                    Look::Plain
                };
                if picked == Some(date) && month.month() == date.month() {
                    marks.selected = Some(d as u8);
                }
                if date == today && month.month() == date.month() {
                    marks.today = Some(d as u8);
                }
                if shown[k] == Some((date, look)) {
                    continue;
                }
                let cell = self.day(k);
                let (ink, weight) = match look {
                    Look::Plain => (style.ink, FontWeight(400)),
                    Look::Selected if date.month() == month.month() => {
                        (style.selected_ink, FontWeight(600))
                    }
                    Look::Selected | Look::Other => (style.quiet, FontWeight(400)),
                    Look::Off => (
                        Rgba {
                            a: style.quiet.a * 0.45,
                            ..style.quiet
                        },
                        FontWeight(400),
                    ),
                };
                s.store.set_text(
                    cell,
                    line(
                        &date.day().to_string(),
                        style.font_size,
                        ink,
                        weight,
                        TextAlign::Center,
                        Some(style.cell),
                    ),
                );
                let mut semantics =
                    Semantics::role(Role::Cell).with_label((self.names.long)(&self.names, date));
                if look == Look::Off {
                    semantics.hint = Some(self.names.unavailable.clone());
                } else if date == today {
                    semantics.hint = Some(self.names.today.clone());
                }
                s.store.set_semantics(cell, semantics);
                s.store.set_semantic_state(
                    cell,
                    SemanticState {
                        selected: Some(look == Look::Selected),
                        ..SemanticState::default()
                    },
                );
                shown[k] = Some((date, look));
            }
            s.store.set_days(row, marks);
            if let Some(numbers) = self.numbers {
                // The week's number is its Thursday's, whichever day it
                // starts on.
                let thursday = (0..DAYS)
                    .map(|d| start.add_days((w * DAYS + d) as i32))
                    .find(|d| d.weekday() == Weekday::Thursday)
                    .unwrap_or(start);
                let n = thursday.iso_week();
                s.store.set_text(
                    numbers[w],
                    line(
                        &n.to_string(),
                        style.font_size * 0.8,
                        style.quiet,
                        FontWeight(400),
                        TextAlign::Center,
                        Some(style.cell),
                    ),
                );
                s.store.set_semantics(
                    numbers[w],
                    Semantics::role(Role::Label).with_label(format!("{} {n}", self.names.week)),
                );
            }
        }
        let before = month.add_days(-1);
        let after = month.add_months(1);
        s.store
            .set_enabled(self.previous, self.min.is_none_or(|m| before >= m));
        s.store
            .set_enabled(self.next, self.max.is_none_or(|m| after <= m));
    }

    /// Pick `date`, showing its month and focusing it.
    fn pick(&self, ev: &mut EventCx<'_>, date: Date) {
        if !self.available(date) {
            return;
        }
        self.go(ev, date);
        if Date::of_state(ev.get(self.value)) == Some(date) {
            return;
        }
        ev.set(self.value, date.to_state());
        if let Some(f) = self.on_change.borrow_mut().as_mut() {
            f(ev, date);
        }
    }

    /// Focus `date`, showing its month.
    pub(crate) fn go(&self, ev: &mut EventCx<'_>, date: Date) {
        let index = date.month_index();
        if ev.get(self.month) != Some(StateValue::Int(index)) {
            ev.set(self.month, StateValue::Int(index));
        }
        let start = Date::of_month_index(index).grid_start(self.first.get());
        let k = (date.days() - start.days()).clamp(0, (WEEKS * DAYS) as i32 - 1) as usize;
        let cell = self.day(k);
        if ev.focused() != Some(cell) {
            ev.request_focus(cell);
        }
    }

    /// The day to focus on arriving: the picked one, else today, the
    /// nearest that may be picked.
    pub(crate) fn landing(&self, ev: &EventCx<'_>) -> Option<Date> {
        let from = Date::of_state(ev.get(self.value))
            .unwrap_or_else(|| self.today.unwrap_or_else(Date::today));
        self.settle(from, 1).or_else(|| self.settle(from, -1))
    }

    /// Its Tab stops in order, as a panel going round them has them: the
    /// buttons that can turn the month, then the day to land on in the month
    /// shown — the picked one, else the first that may be picked.
    pub(crate) fn stops(&self, ev: &EventCx<'_>) -> Vec<NodeId> {
        let month = self.shown_month(ev.get(self.month));
        let before = month.add_days(-1);
        let after = month.add_months(1);
        let mut stops = Vec::with_capacity(3);
        if self.min.is_none_or(|m| before >= m) {
            stops.push(self.previous);
        }
        if self.max.is_none_or(|m| after <= m) {
            stops.push(self.next);
        }
        let picked =
            Date::of_state(ev.get(self.value)).filter(|d| d.month_index() == month.month_index());
        let day = picked.or_else(|| {
            self.settle(month, 1)
                .filter(|d| d.month_index() == month.month_index())
        });
        if let Some(day) = day {
            let start = month.grid_start(self.first.get());
            let k = (day.days() - start.days()).clamp(0, (WEEKS * DAYS) as i32 - 1) as usize;
            stops.push(self.day(k));
        }
        stops
    }

    /// Whether `node` is one of its days.
    pub(crate) fn holds(&self, node: NodeId) -> bool {
        self.days.iter().flatten().any(|&d| d == node)
    }

    /// Show the month `by` months on.
    fn turn(&self, ev: &mut EventCx<'_>, by: i32) {
        let month = self.shown_month(ev.get(self.month)).add_months(by);
        ev.set(self.month, StateValue::Int(month.month_index()));
    }

    /// The nearest day from `date` that may be picked, held within the
    /// bounds and passing over refused days the way `way` steps.
    fn settle(&self, date: Date, way: i32) -> Option<Date> {
        let mut date = date;
        if let Some(min) = self.min {
            date = date.max(min);
        }
        if let Some(max) = self.max {
            date = date.min(max);
        }
        let way = if way == 0 { 1 } else { way.signum() };
        for _ in 0..=366 {
            if self.available(date) {
                return Some(date);
            }
            let next = date.add_days(way);
            if self.min.is_some_and(|m| next < m) || self.max.is_some_and(|m| next > m) {
                return None;
            }
            date = next;
        }
        None
    }

    /// What a key on day `k` asks.
    fn key(&self, ev: &mut EventCx<'_>, k: usize) {
        if ev.focus_change().is_some() {
            return;
        }
        let date = self.start.get().add_days(k as i32);
        if ev.access() == Some(Access::Key) {
            ev.stop_propagation();
            self.pick(ev, date);
            return;
        }
        let Some(key) = ev.key().copied().filter(|k| k.pressed) else {
            return;
        };
        let across = if ev.reads_rtl() { -1 } else { 1 };
        let into_week = (date.weekday().index() + 7 - self.first.get().index()) % 7;
        let shift = key.modifiers.shift;
        let (target, way) = match key.key {
            Key::Enter | Key::Space if !key.repeat => {
                ev.stop_propagation();
                self.pick(ev, date);
                return;
            }
            Key::Right => (date.add_days(across), across),
            Key::Left => (date.add_days(-across), -across),
            Key::Down => (date.add_days(7), 1),
            Key::Up => (date.add_days(-7), -1),
            // The week's ends, passing back into the week over a refused
            // one.
            Key::Home => (date.add_days(-(into_week as i32)), 1),
            Key::End => (date.add_days(6 - into_week as i32), -1),
            Key::PageDown if shift => (date.add_years(1), 1),
            Key::PageUp if shift => (date.add_years(-1), -1),
            Key::PageDown => (date.add_months(1), 1),
            Key::PageUp => (date.add_months(-1), -1),
            _ => return,
        };
        ev.stop_propagation();
        if let Some(target) = self.settle(target, way) {
            self.go(ev, target);
        }
    }
}

/// A line of `text` in a box `side` tall, centred in it.
fn line(
    text: &str,
    size: f32,
    ink: Rgba,
    weight: FontWeight,
    align: TextAlign,
    side: Option<f32>,
) -> TextRequest {
    TextRequest {
        spans: Vec::new(),
        text: text.to_owned(),
        font_size: FontSize::Fixed(size),
        color: ink,
        soft_wrap: false,
        locale: None,
        style: TextStyle {
            weight,
            align,
            line_height: side.map(LengthTerms::dp),
            ..TextStyle::default()
        },
    }
}

/// A round button of a glyph, named `label`, that runs `press`.
fn nav_button(
    cx: &mut BuildCx<'_>,
    label: &str,
    style: &CalendarStyle,
    glyph: fn(&mut BuildCx<'_>, Rgba) -> Handle,
    press: impl Fn(&mut EventCx<'_>) + 'static,
) -> Handle {
    let hovered = cx.state(StateValue::Bool(false));
    let resting = BoxStyle::NONE.with_radius(14.0);
    let button = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            align: Align::Center,
            justify: Justify::Center,
            size: Size::fixed(28.0, 28.0),
            style: resting,
            ..FlexStyle::default()
        },
        |cx| {
            glyph(cx, style.ink);
        },
    );
    cx.interaction_style(
        button,
        InteractionStyle {
            resting,
            hover: BoxStyle::solid(style.hover).with_radius(14.0),
            pressed: BoxStyle::solid(style.hover).with_radius(14.0),
            pressed_cell: None,
            hover_cell: Some(hovered),
        },
    );
    cx.focusable(button, true);
    cx.semantics(button, Semantics::role(Role::Button).with_label(label));
    let press = Rc::new(press);
    let p = Rc::clone(&press);
    cx.on_pointer(button, move |ev| {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(pointer) = ev.pointer().copied() else {
            return;
        };
        match pointer.phase {
            PointerPhase::Enter => {
                ev.set(hovered, StateValue::Bool(true));
            }
            PointerPhase::Leave => {
                ev.set(hovered, StateValue::Bool(false));
            }
            PointerPhase::Down if pointer.buttons.contains(PointerButtons::PRIMARY) => p(ev),
            _ => {}
        }
    });
    cx.on_key(button, move |ev| {
        let pressed = ev.access() == Some(Access::Key)
            || ev.key().is_some_and(|k| {
                k.pressed && !k.repeat && matches!(k.key, Key::Enter | Key::Space)
            });
        if pressed {
            ev.stop_propagation();
            press(ev);
        }
    });
    button
}

/// A text leaf `width` by `height`, empty until its hook fills it.
fn text_leaf(cx: &mut BuildCx<'_>, size: Size) -> Handle {
    let leaf = cx.leaf(LeafStyle {
        size,
        style: BoxStyle::NONE,
    });
    cx.text_request(
        leaf,
        line(
            "",
            14.0,
            Rgba::TRANSPARENT,
            FontWeight(400),
            TextAlign::Center,
            None,
        ),
    )
}

impl Calendar {
    /// Builds the calendar, returning what its handlers share.
    pub(crate) fn build_month(&self, cx: &mut BuildCx<'_>) -> Rc<Month> {
        let value = self
            .cell
            .unwrap_or_else(|| cx.state(StateValue::Int(self.value.map_or(NO_DATE, Date::days))));
        let shown = self
            .value
            .unwrap_or_else(|| self.today.unwrap_or_else(Date::today));
        let month = cx.state(StateValue::Int(shown.month_index()));
        let style = self.style;
        let cell = style.cell;
        let side = Size::fixed(cell, cell);
        // The buttons turn the month through the shared calendar, built
        // after them.
        let shared: Rc<RefCell<Option<Rc<Month>>>> = Rc::new(RefCell::new(None));
        let mut parts = None;
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 4.0,
                align: Align::Stretch,
                ..FlexStyle::default()
            },
            |cx| {
                let mut header = (None, None, None);
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        align: Align::Center,
                        justify: Justify::Start,
                        ..FlexStyle::default()
                    },
                    |cx| {
                        let turn = |by: i32| {
                            let shared = Rc::clone(&shared);
                            move |ev: &mut EventCx<'_>| {
                                if let Some(m) = shared.borrow().as_ref() {
                                    m.turn(ev, by);
                                }
                            }
                        };
                        header.0 = Some(nav_button(
                            cx,
                            &self.names.previous,
                            &style,
                            chevron_start,
                            turn(-1),
                        ));
                        header.1 = Some(text_leaf(
                            cx,
                            Size {
                                width: viso_ui::Length::Fill { weight: 1.0 },
                                height: viso_ui::Length::Fit,
                            },
                        ));
                        header.2 = Some(nav_button(
                            cx,
                            &self.names.next,
                            &style,
                            chevron_end,
                            turn(1),
                        ));
                    },
                );
                let mut heads = Vec::with_capacity(DAYS);
                let mut weeks = Vec::with_capacity(WEEKS);
                let mut numbers = Vec::new();
                let mut days = Vec::with_capacity(WEEKS);
                let row = FlexStyle {
                    axis: Axis::Row,
                    align: Align::Center,
                    ..FlexStyle::default()
                };
                let table = cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        align: Align::Start,
                        ..FlexStyle::default()
                    },
                    |cx| {
                        let head_row = cx.flex(row, |cx| {
                            if self.week_numbers {
                                cx.leaf(LeafStyle {
                                    size: Size::fixed(cell, cell * 0.8),
                                    style: BoxStyle::NONE,
                                });
                            }
                            for _ in 0..DAYS {
                                heads.push(text_leaf(cx, Size::fixed(cell, cell * 0.8)));
                            }
                        });
                        cx.semantics(head_row, Semantics::role(Role::Row));
                        for _ in 0..WEEKS {
                            let mut week = Vec::with_capacity(DAYS);
                            let r = cx.flex(row, |cx| {
                                if self.week_numbers {
                                    numbers.push(text_leaf(cx, side).id());
                                }
                                for _ in 0..DAYS {
                                    let day = text_leaf(cx, side);
                                    cx.focusable(day, true);
                                    week.push(day);
                                }
                            });
                            cx.semantics(r, Semantics::role(Role::Row));
                            weeks.push(r);
                            days.push(week);
                        }
                    },
                );
                cx.roving(table);
                parts = Some((header, table, heads, weeks, numbers, days));
            },
        );
        let ((previous, title, next), table, heads, weeks, numbers, days) = parts.expect("built");
        cx.semantics(
            root,
            Semantics::role(Role::Group).with_label(self.label.clone()),
        );
        let ids = |v: &[Handle]| v.iter().map(|h| h.id()).collect::<Vec<_>>();
        let day_ids: Vec<[NodeId; DAYS]> = days
            .iter()
            .map(|w| ids(w).try_into().expect("seven days"))
            .collect();
        let m = Rc::new(Month {
            value,
            month,
            start: Cell::new(shown.grid_start(Weekday::Monday)),
            first: Cell::new(Weekday::Monday),
            fixed_first: self.first_weekday,
            min: self.min,
            max: self.max,
            unavailable: self.unavailable.clone(),
            today: self.today,
            title: title.expect("built").id(),
            table: table.id(),
            previous: previous.expect("built").id(),
            next: next.expect("built").id(),
            heads: ids(&heads).try_into().expect("seven heads"),
            weeks: ids(&weeks).try_into().expect("six weeks"),
            numbers: self
                .week_numbers
                .then(|| numbers.clone().try_into().expect("six numbers")),
            days: day_ids.try_into().expect("six weeks"),
            shown: RefCell::new([None; WEEKS * DAYS]),
            names: self.names.clone(),
            style,
            on_change: Rc::clone(&self.on_change),
        });
        *shared.borrow_mut() = Some(Rc::clone(&m));
        cx.structure(|s| {
            for &row in &m.weeks {
                s.store.set_days(
                    row,
                    Days::new(u8::from(m.numbers.is_some()), style.accent, style.hover),
                );
            }
            m.show(s);
            let mut deps = vec![value, month];
            deps.extend(s.states.env_cell(EnvField::Locale));
            let hook = Rc::clone(&m);
            s.store.add_structure_hook(deps, move |s, _| hook.show(s));
        });
        for (w, week) in days.iter().enumerate() {
            let row = m.weeks[w];
            for (d, &day) in week.iter().enumerate() {
                let k = w * DAYS + d;
                let p = Rc::clone(&m);
                cx.on_pointer(day, move |ev| {
                    if ev.phase() == DispatchPhase::Capture {
                        return;
                    }
                    let Some(pointer) = ev.pointer().copied() else {
                        return;
                    };
                    match pointer.phase {
                        PointerPhase::Enter => ev.hover_day(row, Some(d as u8)),
                        PointerPhase::Leave => ev.hover_day(row, None),
                        PointerPhase::Down if pointer.buttons.contains(PointerButtons::PRIMARY) => {
                            let date = p.start.get().add_days(k as i32);
                            p.pick(ev, date);
                        }
                        _ => {}
                    }
                });
                let p = Rc::clone(&m);
                cx.on_key(day, move |ev| p.key(ev, k));
            }
        }
        m
    }
}

impl Component for Calendar {
    fn build(&self, cx: &mut BuildCx<'_>) {
        self.build_month(cx);
    }
}
