//! The [`DatePicker`] control — a field showing a date that opens a
//! [`Calendar`] to pick another.
//!
//! The field shows the picked date in brief ([`DateNames::short`]), or a
//! placeholder before one is picked, beside a calendar glyph. A press on it,
//! or Space, Enter, Alt+Down or F4 while it has the focus, opens the
//! calendar in a panel below it, at the picked date's month with that day
//! focused (today's when none is picked, the nearest that may be picked).
//! Picking a day closes the panel and puts the focus back on the field;
//! Escape or a press outside closes it unchanged. While it is open, Tab and
//! Shift+Tab go round the calendar's buttons and its days.
//!
//! Everything a [`Calendar`] takes — bounds, refused days, the weeks' first
//! day, week numbers, names — the picker takes too, and hands on.
//!
//! Semantics: the field is a `combobox` named by its label, its value the
//! date in full, announcing whether the calendar is expanded; the panel a
//! `dialog` named by the label.
//!
//! Invalidation: the date lives in one reactive cell the field and the
//! calendar share; a pick shapes the field's text again, and opening and
//! closing show and hide the panel.
//!
//! ```
//! use viso_widgets::date_picker;
//! use viso_ui::calendar::Date;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
//!
//! let start = date_picker("Start")
//!     .value(Date::new(2026, 10, 24).unwrap())
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
//! start.build(&mut cx);
//! ```

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use viso_ui::ColorToken;

use viso_ui::calendar::{Date, NO_DATE, Weekday};
use viso_ui::{
    Align, AnchorAlign, Anchoring, Axis, BoxStyle, BuildCx, Component, DispatchPhase, EventCx,
    FlexStyle, FontSize, Inset, InteractionStyle, Justify, Key, LeafStyle, Length, NodeId,
    PointerButtons, PointerPhase, Rgba, Role, SemanticState, Semantics, Shadow, Side, Sizing,
    StateId, StateValue, TextRequest, TextStyle,
};

use super::calendar::{Calendar, CalendarStyle, DateNames, Month, calendar};
use viso_ui::glyphs::calendar_glyph;

/// What runs with a picked date.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, Date)>>>>;

/// How a date picker looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DatePickerStyle {
    /// The field, at rest and under the pointer or open.
    pub field: BoxStyle,
    pub field_hover: BoxStyle,
    pub field_padding: Inset,
    pub font_size: f32,
    /// The date, and the placeholder and the glyph.
    pub ink: Rgba,
    pub quiet: Rgba,
    /// The panel the calendar opens in, and its shadow.
    pub panel: BoxStyle,
    pub shadow: Option<Shadow>,
    pub padding: Inset,
    pub calendar: CalendarStyle,
}

impl Default for DatePickerStyle {
    fn default() -> Self {
        let border = viso_ui::Border {
            width: 1.0,
            color: ColorToken::Outline.rgba(),
        };
        DatePickerStyle {
            field: BoxStyle {
                border,
                ..BoxStyle::solid(ColorToken::Background.rgba()).with_radius(6.0)
            },
            field_hover: BoxStyle {
                border,
                ..BoxStyle::solid(ColorToken::Surface.rgba()).with_radius(6.0)
            },
            field_padding: Inset {
                left: 10.0,
                right: 8.0,
                top: 5.0,
                bottom: 5.0,
            },
            font_size: 14.0,
            ink: ColorToken::Foreground.rgba(),
            quiet: ColorToken::Muted.rgba(),
            panel: BoxStyle {
                border: viso_ui::Border {
                    width: 1.0,
                    color: ColorToken::Outline.rgba(),
                },
                ..BoxStyle::solid(ColorToken::Surface.rgba()).with_radius(10.0)
            },
            shadow: Some(Shadow::FLOATING),
            padding: Inset::all(10.0),
            calendar: CalendarStyle::default(),
        }
    }
}

/// A field showing a date that opens a calendar to pick another. Construct
/// one with [`date_picker`].
pub struct DatePicker {
    calendar: Calendar,
    placeholder: String,
    style: DatePickerStyle,
    on_change: SharedChange,
}

/// A date picker named `label` with no date picked.
pub fn date_picker(label: impl Into<String>) -> DatePicker {
    DatePicker {
        calendar: calendar(label),
        placeholder: "Select a date".to_owned(),
        style: DatePickerStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl DatePicker {
    /// Run `handler` with each date picked.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, Date) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Start with `date` picked.
    pub fn value(mut self, date: Date) -> Self {
        self.calendar = self.calendar.value(date);
        self
    }

    /// Keep the date in `cell` ([`Calendar::bind`]).
    pub fn bind(mut self, cell: StateId) -> Self {
        self.calendar = self.calendar.bind(cell);
        self
    }

    /// Let no day before `date` be picked.
    pub fn min(mut self, date: Date) -> Self {
        self.calendar = self.calendar.min(date);
        self
    }

    /// Let no day after `date` be picked.
    pub fn max(mut self, date: Date) -> Self {
        self.calendar = self.calendar.max(date);
        self
    }

    /// Let no day be picked that `refuse` refuses.
    pub fn unavailable(mut self, refuse: impl Fn(Date) -> bool + 'static) -> Self {
        self.calendar = self.calendar.unavailable(refuse);
        self
    }

    /// Start the calendar's weeks on `day` rather than the locale's first.
    pub fn first_weekday(mut self, day: Weekday) -> Self {
        self.calendar = self.calendar.first_weekday(day);
        self
    }

    /// Lead the calendar's weeks with their ISO week numbers.
    pub fn week_numbers(mut self, on: bool) -> Self {
        self.calendar = self.calendar.week_numbers(on);
        self
    }

    /// Ring `date` as today rather than the system clock's.
    pub fn today(mut self, date: Date) -> Self {
        self.calendar = self.calendar.today(date);
        self
    }

    /// Name the months and days, and write dates, as `names` do.
    pub fn names(mut self, names: DateNames) -> Self {
        self.calendar = self.calendar.names(names);
        self
    }

    /// What the field shows before a date is picked (default "Select a
    /// date").
    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Replace the whole [`DatePickerStyle`].
    pub fn style(mut self, style: DatePickerStyle) -> Self {
        self.style = style;
        self
    }
}

/// What a date picker shares between its handlers and its hook.
struct Picker {
    field: NodeId,
    panel: NodeId,
    open_cell: StateId,
    open: Cell<bool>,
    /// The calendar, once built.
    month: RefCell<Option<Rc<Month>>>,
}

impl Picker {
    fn month(&self) -> Rc<Month> {
        Rc::clone(self.month.borrow().as_ref().expect("built"))
    }

    fn open(&self, ev: &mut EventCx<'_>) {
        if self.open.replace(true) {
            return;
        }
        ev.set_hidden(self.panel, false);
        ev.set(self.open_cell, StateValue::Bool(true));
        let month = self.month();
        match month.landing(ev) {
            Some(date) => month.go(ev, date),
            None => ev.request_focus(month.next),
        }
    }

    fn close(&self, ev: &mut EventCx<'_>) {
        if !self.open.replace(false) {
            return;
        }
        ev.set_hidden(self.panel, true);
        ev.set(self.open_cell, StateValue::Bool(false));
        ev.request_focus(self.field);
    }

    /// What a key asks of the picker, the field or the panel having the
    /// focus.
    fn key(&self, ev: &mut EventCx<'_>) {
        let Some(k) = ev.key().copied().filter(|k| k.pressed) else {
            return;
        };
        if !self.open.get() {
            let opens = matches!(k.key, Key::Space | Key::Enter | Key::F4)
                || (k.key == Key::Down && k.modifiers.alt);
            if opens && ev.focused() == Some(self.field) {
                ev.stop_propagation();
                if !k.repeat {
                    self.open(ev);
                }
            }
            return;
        }
        match k.key {
            Key::Escape => {
                ev.stop_propagation();
                self.close(ev);
            }
            Key::Up if k.modifiers.alt => {
                ev.stop_propagation();
                self.close(ev);
            }
            Key::Tab => {
                // Round the month's buttons and its days.
                ev.stop_propagation();
                let month = self.month();
                let stops = month.stops(ev);
                if stops.is_empty() {
                    return;
                }
                let n = stops.len();
                let focused = ev.focused();
                let at = focused.and_then(|f| {
                    stops
                        .iter()
                        .position(|&s| s == f)
                        .or_else(|| month.holds(f).then_some(n - 1))
                });
                let next = match (at, k.modifiers.shift) {
                    (Some(i), false) => (i + 1) % n,
                    (Some(i), true) => (i + n - 1) % n,
                    (None, _) => n - 1,
                };
                ev.request_focus(stops[next]);
            }
            _ => {}
        }
    }
}

impl Component for DatePicker {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let style = self.style;
        let value = self.calendar.cell.unwrap_or_else(|| {
            cx.state(StateValue::Int(
                self.calendar.value.map_or(NO_DATE, Date::days),
            ))
        });
        let open_cell = cx.state(StateValue::Bool(false));
        let hover_cell = cx.state(StateValue::Bool(false));
        let names = self.calendar.names.clone();
        let mut parts = None;
        let wrapper = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                align: Align::Start,
                ..FlexStyle::default()
            },
            |cx| {
                let mut text = None;
                let field = cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: 8.0,
                        padding: style.field_padding,
                        align: Align::Center,
                        justify: Justify::Start,
                        style: style.field,
                        ..FlexStyle::default()
                    },
                    |cx| {
                        let leaf = cx.leaf(LeafStyle {
                            size: Sizing {
                                width: Length::Fit,
                                height: Length::Fit,
                            },
                            style: BoxStyle::NONE,
                        });
                        cx.text_request(leaf, line("", style.font_size, style.ink));
                        text = Some(leaf.id());
                        calendar_glyph(cx, style.quiet);
                    },
                );
                cx.unhit_below(field);
                let mut month = None;
                let panel = cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        padding: style.padding,
                        style: style.panel,
                        ..FlexStyle::default()
                    },
                    |cx| {
                        let inner = Calendar {
                            label: self.calendar.label.clone(),
                            value: self.calendar.value,
                            cell: Some(value),
                            min: self.calendar.min,
                            max: self.calendar.max,
                            unavailable: self.calendar.unavailable.clone(),
                            first_weekday: self.calendar.first_weekday,
                            week_numbers: self.calendar.week_numbers,
                            today: self.calendar.today,
                            names: names.clone(),
                            style: style.calendar,
                            on_change: Rc::new(RefCell::new(None)),
                        };
                        month = Some((inner.build_month(cx), Rc::clone(&inner.on_change)));
                    },
                );
                parts = Some((field, text.expect("built"), panel, month.expect("built")));
            },
        );
        let (field, text, panel, (month, picked)) = parts.expect("built");
        cx.anchored(
            panel,
            field,
            Anchoring {
                side: Side::Bottom,
                align: AnchorAlign::Start,
                offset: 0.0,
                ..Anchoring::default()
            },
        );
        if let Some(shadow) = style.shadow {
            cx.shadow(panel, shadow);
        }
        cx.light_dismiss(panel);
        cx.set_hidden(panel, true);
        cx.semantics(
            panel,
            Semantics::role(Role::Dialog).with_label(self.calendar.label.clone()),
        );
        cx.focusable(field, true);
        cx.bind_semantic_state(field, move |cx| SemanticState {
            expanded: Some(matches!(cx.get(open_cell), Some(StateValue::Bool(true)))),
            ..SemanticState::default()
        });
        cx.interaction_style(
            field,
            InteractionStyle {
                resting: style.field,
                hover: style.field_hover,
                pressed: style.field_hover,
                pressed_cell: Some(open_cell),
                hover_cell: Some(hover_cell),
            },
        );
        let picker = Rc::new(Picker {
            field: field.id(),
            panel: panel.id(),
            open_cell,
            open: Cell::new(false),
            month: RefCell::new(Some(month)),
        });
        // A pick closes the panel, then reports.
        {
            let p = Rc::clone(&picker);
            let report = Rc::clone(&self.on_change);
            *picked.borrow_mut() = Some(Box::new(move |ev: &mut EventCx<'_>, date: Date| {
                p.close(ev);
                if let Some(f) = report.borrow_mut().as_mut() {
                    f(ev, date);
                }
            }));
        }
        let label = self.calendar.label.clone();
        let placeholder = self.placeholder.clone();
        cx.structure(|s| {
            let show = move |s: &mut viso_ui::StructureCx<'_>| {
                let date = Date::of_state(s.states.get(value));
                let (shown, ink) = match date {
                    Some(d) => ((names.short)(&names, d), style.ink),
                    None => (placeholder.clone(), style.quiet),
                };
                s.store.set_text(text, line(&shown, style.font_size, ink));
                let mut semantics = Semantics::role(Role::ComboBox).with_label(label.clone());
                semantics.value = date.map(|d| (names.long)(&names, d));
                s.store.set_semantics(field.id(), semantics);
            };
            show(s);
            s.store.add_structure_hook([value], move |s, _| show(s));
        });
        let p = Rc::clone(&picker);
        cx.on_pointer(field, move |ev| {
            if ev.phase() == DispatchPhase::Capture {
                return;
            }
            let Some(pointer) = ev.pointer().copied() else {
                return;
            };
            match pointer.phase {
                PointerPhase::Enter => {
                    ev.set(hover_cell, StateValue::Bool(true));
                }
                PointerPhase::Leave => {
                    ev.set(hover_cell, StateValue::Bool(false));
                }
                PointerPhase::Down if pointer.buttons.contains(PointerButtons::PRIMARY) => {
                    if p.open.get() {
                        p.close(ev);
                    } else {
                        p.open(ev);
                    }
                }
                _ => {}
            }
        });
        let p = Rc::clone(&picker);
        cx.on_key(panel, move |ev| {
            if ev.dismissed() {
                p.close(ev);
            }
        });
        let p = Rc::clone(&picker);
        cx.on_key(wrapper, move |ev| p.key(ev));
    }
}

/// A line of `text`.
fn line(text: &str, size: f32, ink: Rgba) -> TextRequest {
    TextRequest {
        spans: Vec::new(),
        text: text.to_owned(),
        font_size: FontSize::Fixed(size),
        color: ink,
        soft_wrap: false,
        locale: None,
        style: TextStyle::default(),
    }
}

impl crate::FormControl for DatePicker {
    fn value(&self) -> crate::FieldValue<'_> {
        crate::FieldValue::Date(self.calendar.value)
    }

    fn reporting(self, mut report: crate::OnValue) -> Self {
        let mut own = self.on_change.borrow_mut().take();
        self.on_change(move |ev, date| {
            if let Some(own) = own.as_mut() {
                own(ev, date);
            }
            report(ev, crate::FieldValue::Date(Some(date)));
        })
    }
}
