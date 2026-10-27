//! Forms — [`Field`]s whose values are checked as they change, gathered in
//! [`fieldset`]s and submitted through a [`Form`].
//!
//! A field is a control with its caption above it (its access key focusing
//! it), an optional help line, and a line saying what is wrong with the value
//! it holds. A field checks its control's value as it changes: `required`
//! refuses an empty one (blank text, an unchecked box, nothing chosen), each
//! [`rule`](Field::rule) refuses what it says. What is wrong is not shown
//! while the user is still filling the field in: it shows once the focus
//! leaves the field, or the form is submitted, and from then on follows each
//! change — gone as soon as the value is fixed.
//!
//! A [`Form`] gathers the fields given to it. [`Form::submit`] shows what is
//! wrong with every field, moves the focus to the first one refused, and
//! runs the form's submit handler only when every field passes; Enter in a
//! text field of the form submits it. A [`fieldset`] groups fields under a
//! legend.
//!
//! A control in a field is announced as required when it is, and as invalid
//! while what is wrong shows, its reason read with its description and named
//! as its error message. Any control implementing [`FormControl`] can stand in
//! a field: text inputs, number fields, check boxes, toggles, selects, radio
//! groups and date pickers do.
//!
//! ```
//! use viso_widgets::{Form, button, field, fieldset, text_input, checkbox};
//! use viso_ui::{BuildCx, BindingTable, Component, NodeStore, SemanticProjector, StateStore, TextEdits, VirtualLists};
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! let form = Form::new();
//! form.on_submit(|_ev| { /* send it */ });
//! let fields = form.clone();
//! fieldset("Account", move |cx| {
//!     field("&Email", text_input("Email"))
//!         .required("Enter your email address")
//!         .rule(|value| match value.text() {
//!             Some(text) if !text.contains('@') => Err("An email address has an @".into()),
//!             _ => Ok(()),
//!         })
//!         .form(&fields)
//!         .build(cx);
//!     field("Terms", checkbox("I accept the terms"))
//!         .required("Accept the terms to go on")
//!         .form(&fields)
//!         .build(cx);
//! })
//! .build(&mut cx);
//! let submit = form.clone();
//! button("Sign up").on_click(move |ev| { submit.submit(ev); }).build(&mut cx);
//! ```

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use viso_ui::ColorToken;

use viso_ui::{
    Align, Axis, Border, BoxStyle, BuildCx, Component, EventCx, FlexStyle, FocusChange, FontWeight,
    Inset, Justify, Length, NodeId, NodeStore, Rgba, Role, Semantics, Sizing, StateId, StateValue,
    Validity,
};

use crate::controls::labelled::{Labelled, labelled};
use crate::text::{Label, label};
use viso_ui::calendar::Date;

/// What a form control holds, as a field checks it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldValue<'a> {
    /// A text field's text.
    Text(&'a str),
    /// Whether a check box or a toggle is on.
    Flag(bool),
    /// A number field's number.
    Number(f64),
    /// The option chosen, if any.
    Choice(Option<usize>),
    /// The date picked, if any.
    Date(Option<Date>),
}

impl FieldValue<'_> {
    /// Whether it holds nothing a required field asks for: blank text, an
    /// unchecked box, nothing chosen or picked. A number is never empty.
    pub fn is_empty(&self) -> bool {
        match *self {
            FieldValue::Text(text) => text.trim().is_empty(),
            FieldValue::Flag(on) => !on,
            FieldValue::Number(_) => false,
            FieldValue::Choice(choice) => choice.is_none(),
            FieldValue::Date(date) => date.is_none(),
        }
    }

    /// The text it holds, if it is a text field's.
    pub fn text(&self) -> Option<&str> {
        match *self {
            FieldValue::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The number it holds, if it is a number field's.
    pub fn number(&self) -> Option<f64> {
        match *self {
            FieldValue::Number(n) => Some(n),
            _ => None,
        }
    }
}

/// A handler told each new value a control holds.
pub type OnValue = Box<dyn FnMut(&mut EventCx<'_>, FieldValue<'_>)>;
/// A handler a control runs when the user asks to submit (Enter in a text
/// field).
pub type OnSubmit = Box<dyn FnMut(&mut EventCx<'_>)>;

/// A control that can stand in a [`Field`]: it says what it holds, and tells
/// a handler each new value after its own change handler.
pub trait FormControl: Component + 'static {
    /// What it holds as built.
    fn value(&self) -> FieldValue<'_>;

    /// This telling `report` each new value it holds, after its own change
    /// handler.
    fn reporting(self, report: OnValue) -> Self
    where
        Self: Sized;

    /// This running `submit` when the user asks to submit, if it has no
    /// submit handler of its own. Most controls have no such gesture.
    fn submitting(self, _submit: OnSubmit) -> Self
    where
        Self: Sized,
    {
        self
    }
}

/// A rule a field's value must pass: `Err` says what is wrong.
type Rule = Box<dyn Fn(&FieldValue<'_>) -> Result<(), String>>;

/// How a field and a fieldset look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FormStyle {
    /// The help line's and the error line's text size, and their colors.
    pub note_size: f32,
    pub help: Rgba,
    pub error: Rgba,
    /// The space between a field's caption, control and lines.
    pub gap: f32,
    /// A fieldset's legend size and color, its frame, its padding and the
    /// space between its fields.
    pub legend_size: f32,
    pub legend: Rgba,
    pub frame: BoxStyle,
    pub padding: f32,
    pub spacing: f32,
}

impl Default for FormStyle {
    fn default() -> Self {
        let line = ColorToken::Outline.rgba();
        FormStyle {
            note_size: 12.0,
            help: ColorToken::Muted.rgba(),
            error: ColorToken::Error.rgba(),
            gap: 4.0,
            legend_size: 15.0,
            legend: ColorToken::Foreground.rgba(),
            frame: BoxStyle {
                border: Border {
                    width: 1.0,
                    color: line,
                },
                ..BoxStyle::NONE.with_radius(8.0)
            },
            padding: 12.0,
            spacing: 12.0,
        }
    }
}

/// A field's checking, shared by its control's handlers, its build and its
/// form.
struct Check {
    required: RefCell<Option<String>>,
    rules: RefCell<Vec<Rule>>,
    /// What checking the value it holds last said is wrong.
    error: RefCell<Option<String>>,
    /// Whether what is wrong shows: the focus left the field, or the form was
    /// submitted.
    shown: Cell<bool>,
    /// The focus left a node of the field: the next structure pass looks
    /// whether it left the field.
    leaving: Cell<bool>,
    built: Cell<Option<Built>>,
    /// The form the field is gathered in.
    form: RefCell<Weak<FormState>>,
}

/// A built field's nodes and the cell its changes go through.
#[derive(Debug, Clone, Copy)]
struct Built {
    root: NodeId,
    control: NodeId,
    error: NodeId,
    /// Written to show the field's checking anew.
    revision: StateId,
}

impl Check {
    fn check(&self, value: &FieldValue<'_>) {
        let required = self.required.borrow();
        let error = match &*required {
            Some(message) if value.is_empty() => Some(message.clone()),
            _ => self
                .rules
                .borrow()
                .iter()
                .find_map(|rule| rule(value).err()),
        };
        *self.error.borrow_mut() = error;
    }

    /// Ask for the field's checking to be shown anew.
    fn refresh(&self, ev: &mut EventCx<'_>) {
        let Some(built) = self.built.get() else {
            return;
        };
        let n = match ev.get(built.revision) {
            Some(StateValue::Int(n)) => n,
            _ => 0,
        };
        ev.set(built.revision, StateValue::Int(n.wrapping_add(1)));
    }

    fn refused(&self) -> bool {
        self.error.borrow().is_some()
    }
}

/// A control with its caption, help and what is wrong with its value.
/// Construct one with [`field`].
///
/// Invalidation: a change re-checks the value in the control's handler; once
/// what is wrong shows, a change of it re-texts and shows or hides the error
/// line (`MEASURE | LAYOUT | PAINT`) and updates the control's validity
/// (`SEMANTICS`) in a structure hook. Nothing is rebuilt.
pub struct Field<C> {
    control: Labelled<C>,
    help: Option<String>,
    style: FormStyle,
    check: Rc<Check>,
}

/// A field captioned `caption` (an `&` marks its access key) holding
/// `control`.
pub fn field<C: FormControl>(caption: impl Into<String>, control: C) -> Field<C> {
    let check = Rc::new(Check {
        required: RefCell::new(None),
        rules: RefCell::new(Vec::new()),
        error: RefCell::new(None),
        shown: Cell::new(false),
        leaving: Cell::new(false),
        built: Cell::new(None),
        form: RefCell::new(Weak::new()),
    });
    let reporting = Rc::clone(&check);
    let submitting = Rc::clone(&check);
    let control = control
        .reporting(Box::new(move |ev, value| {
            reporting.check(&value);
            if reporting.shown.get() {
                reporting.refresh(ev);
            }
        }))
        .submitting(Box::new(move |ev| {
            let form = submitting.form.borrow().upgrade();
            if let Some(form) = form {
                Form(form).submit(ev);
            }
        }));
    Field {
        control: labelled(caption, control).above(),
        help: None,
        style: FormStyle::default(),
        check,
    }
}

impl<C: FormControl> Field<C> {
    /// Refuse an empty value, saying `message`.
    pub fn required(self, message: impl Into<String>) -> Self {
        *self.check.required.borrow_mut() = Some(message.into());
        self
    }

    /// Refuse a value `rule` says is wrong; rules are checked in order, after
    /// `required`, the first to refuse saying what is wrong.
    pub fn rule(self, rule: impl Fn(&FieldValue<'_>) -> Result<(), String> + 'static) -> Self {
        self.check.rules.borrow_mut().push(Box::new(rule));
        self
    }

    /// Show `help` under the control.
    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Gather the field in `form`.
    pub fn form(self, form: &Form) -> Self {
        *self.check.form.borrow_mut() = Rc::downgrade(&form.0);
        self
    }

    /// Replace the whole [`FormStyle`].
    pub fn style(mut self, style: FormStyle) -> Self {
        self.style = style;
        self
    }

    fn note(&self, text: &str, color: Rgba) -> Label {
        label(text)
            .font_size(self.style.note_size)
            .color(color)
            .wrap()
            .size(Sizing {
                width: Length::fill(),
                height: Length::Fit,
            })
    }
}

impl<C: FormControl> Component for Field<C> {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let style = self.style;
        let revision = cx.state(StateValue::Int(0));
        let error_line = self.note("", style.error);
        let mut error = None;
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: style.gap,
                align: Align::Start,
                justify: Justify::Start,
                size: Sizing {
                    width: Length::fill(),
                    height: Length::Fit,
                },
                ..FlexStyle::default()
            },
            |cx| {
                self.control.build(cx);
                if let Some(help) = &self.help {
                    self.note(help, style.help).build(cx);
                }
                error = Some(error_line.build_node(cx));
            },
        );
        let error = error.expect("the error line is built");
        cx.set_hidden(error, true);
        let Some(control) = cx.focusable_within(root) else {
            return;
        };
        let check = Rc::clone(&self.check);
        let built = Built {
            root: root.id(),
            control: control.id(),
            error: error.id(),
            revision,
        };
        check.built.set(Some(built));
        check.check(&self.control.field().value());
        if let Some(form) = check.form.borrow().upgrade() {
            form.fields.borrow_mut().push(Rc::clone(&check));
        }
        let leaving = Rc::clone(&check);
        cx.on_key(root, move |ev| {
            if ev.focus_change() == Some(FocusChange::Lost) && !leaving.shown.get() {
                leaving.leaving.set(true);
                leaving.refresh(ev);
            }
        });
        let template = error_line;
        cx.structure(|s| {
            show(s.store, &check, &template);
            s.store.add_structure_hook([revision], move |s, _| {
                if s.store.arena().is_live(built.root) {
                    show(s.store, &check, &template);
                }
            });
        });
    }
}

/// Whether `node` is `root` or inside it.
fn within(store: &NodeStore, mut node: NodeId, root: NodeId) -> bool {
    loop {
        if node == root {
            return true;
        }
        match store.arena().parent(node) {
            Some(parent) => node = parent,
            None => return false,
        }
    }
}

/// Show what checking says of the field: its error line, and its control's
/// validity.
fn show(store: &mut NodeStore, check: &Check, template: &Label) {
    let Some(built) = check.built.get() else {
        return;
    };
    if check.leaving.replace(false)
        && !store
            .focused()
            .is_some_and(|f| within(store, f, built.root))
    {
        check.shown.set(true);
    }
    let error = check.error.borrow();
    let refused = check.shown.get() && error.is_some();
    if let Some(text) = error.as_deref().filter(|_| refused) {
        let mut request = template.request();
        request.text = text.to_owned();
        store.set_text(built.error, request);
    }
    store.set_hidden(built.error, !refused);
    store.set_validity(
        built.control,
        Validity {
            required: check.required.borrow().is_some(),
            invalid: refused,
            error: refused.then_some(built.error),
        },
    );
}

/// The fields of a form, and what submitting it does. Cheap to clone: every
/// clone is the same form.
#[derive(Clone, Default)]
pub struct Form(Rc<FormState>);

#[derive(Default)]
struct FormState {
    fields: RefCell<Vec<Rc<Check>>>,
    on_submit: RefCell<Option<OnSubmit>>,
}

impl Form {
    /// A form with no fields yet.
    pub fn new() -> Self {
        Form::default()
    }

    /// Run `handler` when the form is submitted with every field passing.
    pub fn on_submit(&self, handler: impl FnMut(&mut EventCx<'_>) + 'static) {
        *self.0.on_submit.borrow_mut() = Some(Box::new(handler));
    }

    /// Whether every field of the form shown passes.
    pub fn is_valid(&self) -> bool {
        self.0.fields.borrow().iter().all(|f| !f.refused())
    }

    /// Show what is wrong with every field shown, move the focus to the
    /// first refused, and run the submit handler if none is. Returns whether
    /// every field passed.
    pub fn submit(&self, ev: &mut EventCx<'_>) -> bool {
        let mut first = None;
        // A field no longer in the tree is not checked.
        self.0
            .fields
            .borrow_mut()
            .retain(|f| f.built.get().is_some_and(|b| ev.rect_of(b.root).is_some()));
        for check in self.0.fields.borrow().iter() {
            check.shown.set(true);
            check.refresh(ev);
            if first.is_none() && check.refused() {
                first = check.built.get().map(|b| b.control);
            }
        }
        if let Some(control) = first {
            ev.request_focus(control);
            return false;
        }
        let handler = self.0.on_submit.borrow_mut().take();
        if let Some(mut handler) = handler {
            handler(ev);
            let mut slot = self.0.on_submit.borrow_mut();
            if slot.is_none() {
                *slot = Some(handler);
            }
        }
        true
    }

    /// Hide what is wrong with every field until the focus leaves it or the
    /// form is submitted again — after its values were reset.
    pub fn reset(&self, ev: &mut EventCx<'_>) {
        for check in self.0.fields.borrow().iter() {
            check.shown.set(false);
            check.refresh(ev);
        }
    }
}

/// Fields grouped under a legend. Construct one with [`fieldset`].
pub struct Fieldset {
    legend: String,
    content: Box<dyn Fn(&mut BuildCx<'_>)>,
    style: FormStyle,
}

/// Fields `content` builds, grouped under `legend`.
pub fn fieldset(
    legend: impl Into<String>,
    content: impl Fn(&mut BuildCx<'_>) + 'static,
) -> Fieldset {
    Fieldset {
        legend: legend.into(),
        content: Box::new(content),
        style: FormStyle::default(),
    }
}

impl Fieldset {
    /// Replace the whole [`FormStyle`].
    pub fn style(mut self, style: FormStyle) -> Self {
        self.style = style;
        self
    }
}

impl Component for Fieldset {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let style = self.style;
        let group = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: style.spacing,
                padding: Inset::all(style.padding),
                align: Align::Stretch,
                justify: Justify::Start,
                size: Sizing {
                    width: Length::fill(),
                    height: Length::Fit,
                },
                style: style.frame,
            },
            |cx| {
                label(&self.legend)
                    .font_size(style.legend_size)
                    .color(style.legend)
                    .weight(FontWeight(600))
                    .build(cx);
                (self.content)(cx);
            },
        );
        cx.semantics(
            group,
            Semantics::role(Role::Group).with_label(self.legend.clone()),
        );
    }
}
