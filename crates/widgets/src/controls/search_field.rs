//! `SearchField` — a text field that searches as the user types, with a list
//! of suggestions the application supplies below it.
//!
//! The field draws a magnifier before its text and, while it holds any, a
//! clear button after it. Each edit reports the query to `on_change` — at
//! once, or after the typing rests for [`SearchField::debounce`] — and the
//! application answers with suggestions through the field's
//! [`SearchFieldHandle::set_suggestions`], now or once a search comes back.
//! The list shows them below the field, the letters matching the query in
//! bold, while the field has the focus and holds what was typed. Its rows
//! are built once, as many as [`SearchField::max_suggestions`] allows, and
//! only their text changes, so a list that changes with every key builds
//! nothing.
//!
//! The keyboard: Down and Up move the highlight through the suggestions,
//! wrapping, opening the list if it is closed; Enter submits the highlighted
//! suggestion, or else the text, to `on_submit`; Escape closes the list, and
//! with it closed clears the text. The pointer: a click on a suggestion
//! submits it; the clear button empties the field, the focus staying in it.
//! A suggestion taken into the field reports the new query too.
//!
//! Semantics: the field is a [`Role::SearchField`] announcing whether its
//! list is expanded and naming the highlighted suggestion as its active
//! descendant; the list is a `listbox` of `option`s announcing their place;
//! the clear button is a `button` named "Clear", out of the Tab order, since
//! Escape clears from the keyboard.
//!
//! Invalidation: typing re-marks the shown rows' matched letters; a new list
//! of suggestions re-shapes the rows whose text changed and hides the rest; a
//! highlight is a `PAINT` of one row's look. The parts are
//! [`viso_ui::search_field`]'s, shared with the view's `SearchField` node.
//!
//! ```
//! use viso_widgets::{SearchFieldHandleSlot, search_field};
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
//!
//! let slot = SearchFieldHandleSlot::default();
//! let handle = slot.clone();
//! let search = search_field()
//!     .placeholder("Search cities")
//!     .on_change(move |ev, query| {
//!         let found = ["Auckland", "Wellington"].into_iter().filter(|c| c.to_lowercase().contains(&query.to_lowercase()));
//!         if let Some(h) = handle.borrow().as_ref() {
//!             h.set_suggestions(ev, found);
//!         }
//!     })
//!     .on_submit(|_ev, query| {
//!         let _ = query;
//!     })
//!     .handle(&slot);
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! search.build(&mut cx);
//! assert!(slot.borrow().is_some());
//! ```

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use viso_ui::part_writes::PartWrites;
pub use viso_ui::search_field::SearchFieldStyle;
use viso_ui::search_field::{SearchParts, SearchSpec};
use viso_ui::{BuildCx, Component, EventCx, Size};

/// What runs with the query or a submitted search.
type Report = Rc<RefCell<dyn FnMut(&mut EventCx<'_>, &str)>>;

/// The parts' report running `report`.
fn relay(report: &Option<Report>) -> Option<viso_ui::search_field::Report> {
    let report = report.clone()?;
    Some(Box::new(move |ev: &mut EventCx<'_>, text: &str| {
        (report.borrow_mut())(ev, text)
    }))
}

/// A shared, mutable slot an application passes to a search field's
/// `handle` and reads after `build` for its [`SearchFieldHandle`].
pub type SearchFieldHandleSlot = Rc<RefCell<Option<SearchFieldHandle>>>;

/// A text field that searches as the user types, suggesting below it.
/// Construct one with [`search_field`].
pub struct SearchField {
    spec: SearchSpec,
    on_change: Option<Report>,
    on_submit: Option<Report>,
    slot: Option<SearchFieldHandleSlot>,
}

/// An empty search field saying "Search", reporting each edit at once.
pub fn search_field() -> SearchField {
    SearchField {
        spec: SearchSpec::default(),
        on_change: None,
        on_submit: None,
        slot: None,
    }
}

impl SearchField {
    /// Start with `text` in the field.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.spec.text = text.into();
        self
    }

    /// Show `text` in the field while it is empty (default "Search"); it
    /// names the field for assistive technology unless
    /// [`label`](Self::label) does.
    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.spec.placeholder = text.into();
        self
    }

    /// Name the field for assistive technology.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.spec.label = Some(label.into());
        self
    }

    /// Name the clear button for assistive technology (default "Clear").
    pub fn clear_label(mut self, label: impl Into<String>) -> Self {
        self.spec.clear_label = label.into();
        self
    }

    /// Report the query once the typing has rested for `delay` rather than
    /// at every edit; clearing, Enter and a chosen suggestion report at once.
    pub fn debounce(mut self, delay: Duration) -> Self {
        self.spec.debounce = delay;
        self
    }

    /// Show at most `n` suggestions (default 8).
    pub fn max_suggestions(mut self, n: usize) -> Self {
        self.spec.max_suggestions = n;
        self
    }

    /// Set the field's size within its parent (default: its content's).
    pub fn size(mut self, size: Size) -> Self {
        self.spec.size = size;
        self
    }

    /// Replace the whole [`SearchFieldStyle`].
    pub fn style(mut self, style: SearchFieldStyle) -> Self {
        self.spec.style = style;
        self
    }

    /// Run `change` with the query as it changes.
    pub fn on_change(mut self, change: impl FnMut(&mut EventCx<'_>, &str) + 'static) -> Self {
        self.on_change = Some(Rc::new(RefCell::new(change)));
        self
    }

    /// Run `submit` with what is searched for: on Enter, or a suggestion
    /// chosen.
    pub fn on_submit(mut self, submit: impl FnMut(&mut EventCx<'_>, &str) + 'static) -> Self {
        self.on_submit = Some(Rc::new(RefCell::new(submit)));
        self
    }

    /// Fill `slot` with the field's [`SearchFieldHandle`] when it builds.
    pub fn handle(mut self, slot: &SearchFieldHandleSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }
}

impl Component for SearchField {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let parts = SearchParts::build(
            cx,
            &self.spec,
            relay(&self.on_change),
            relay(&self.on_submit),
        );
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(SearchFieldHandle(parts));
        }
    }
}

/// A handle an application captures to read a built search field's text,
/// set it and give it suggestions from its own handlers. Cheap to clone.
#[derive(Clone)]
pub struct SearchFieldHandle(SearchParts);

impl SearchFieldHandle {
    /// What the field holds.
    pub fn text(&self) -> String {
        self.0.text()
    }

    /// Put `text` in the field, without telling `on_change`.
    pub fn set_text(&self, ev: &mut EventCx<'_>, text: &str) {
        self.0.set_text(ev as &mut dyn PartWrites, text);
    }

    /// Suggest `suggestions` for the query, in order, as many as the field
    /// shows; none closes the list.
    pub fn set_suggestions<I>(&self, ev: &mut EventCx<'_>, suggestions: I)
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let suggestions = suggestions.into_iter().map(Into::into).collect();
        self.0.set_suggestions(ev, suggestions);
    }

    /// The suggestions the list shows.
    pub fn suggestions(&self) -> Vec<String> {
        self.0.suggestions()
    }

    /// Whether the list of suggestions shows.
    pub fn is_open(&self) -> bool {
        self.0.is_open()
    }

    /// Empty the field, telling `on_change`.
    pub fn clear(&self, ev: &mut EventCx<'_>) {
        self.0.clear(ev);
    }
}
