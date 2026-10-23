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
//! highlight is a `PAINT` of one row's look.
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

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use viso_ui::{
    Align, AnchorAlign, Anchoring, Axis, BoxRules, BoxStyle, BuildCx, Component, DispatchPhase,
    EditIntent, EventCx, FieldLook, FlexStyle, FocusChange, FontSize, FontWeight, Handle, Inset,
    InteractionStyle, Justify, Key, KeyEvent, LeafStyle, Length, NodeId, PointerButtons,
    PointerPhase, Rgba, Role, SemanticState, Semantics, Shadow, Side, Size, Span, SpanStyle,
    StateId, StateValue, TextRequest, TextSpan, TextStyle, field_input,
};

use super::glyphs::{GLYPH, clear, magnifier};
use super::select::unhit_below;

/// What runs with the query or a submitted search.
type Report = Rc<RefCell<dyn FnMut(&mut EventCx<'_>, &str)>>;

/// The high bits of a debounced query's wake token; its low bits count the
/// edits, so a wake an edit since has overtaken is let go.
const DEBOUNCE: u32 = 0x5346_0000;

/// How a search field looks.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchFieldStyle {
    /// The field's box at rest.
    pub field: BoxStyle,
    /// The field's box under the pointer and while its list shows.
    pub field_hover: BoxStyle,
    /// The room inside the field.
    pub field_padding: Inset,
    /// The least width of the field.
    pub min_width: f32,
    pub font_size: f32,
    pub ink: Rgba,
    /// The ink of the magnifier and of the clear button's disc.
    pub quiet: Rgba,
    /// The panel the suggestions sit in.
    pub panel: BoxStyle,
    pub shadow: Option<Shadow>,
    /// The room between the panel's edge and its suggestions.
    pub padding: Inset,
    /// The room inside a suggestion.
    pub item_padding: Inset,
    /// The highlighted suggestion's box.
    pub highlight: BoxStyle,
    /// How the letters of a suggestion matching the query draw.
    pub matched: SpanStyle,
}

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

impl Default for SearchFieldStyle {
    fn default() -> Self {
        SearchFieldStyle {
            field: BoxStyle::solid(rgba(0.46, 0.46, 0.5, 0.12)).with_radius(8.0),
            field_hover: BoxStyle::solid(rgba(0.46, 0.46, 0.5, 0.18)).with_radius(8.0),
            field_padding: Inset {
                left: 8.0,
                right: 8.0,
                top: 6.0,
                bottom: 6.0,
            },
            min_width: 200.0,
            font_size: 14.0,
            ink: rgba(0.09, 0.09, 0.1, 1.0),
            quiet: rgba(0.42, 0.42, 0.45, 1.0),
            panel: BoxStyle {
                border: viso_ui::Border {
                    width: 1.0,
                    color: rgba(0.0, 0.0, 0.0, 0.12),
                },
                ..BoxStyle::solid(rgba(0.985, 0.985, 0.985, 1.0)).with_radius(8.0)
            },
            shadow: Some(Shadow::FLOATING),
            padding: Inset::all(5.0),
            item_padding: Inset {
                left: 8.0,
                right: 12.0,
                top: 5.0,
                bottom: 5.0,
            },
            highlight: BoxStyle::solid(rgba(0.23, 0.42, 0.94, 0.14)).with_radius(5.0),
            matched: SpanStyle::default().weight(FontWeight(600)),
        }
    }
}

/// A shared, mutable slot an application passes to a search field's
/// `handle` and reads after `build` for its [`SearchFieldHandle`].
pub type SearchFieldHandleSlot = Rc<RefCell<Option<SearchFieldHandle>>>;

/// A text field that searches as the user types, suggesting below it.
/// Construct one with [`search_field`].
pub struct SearchField {
    text: String,
    placeholder: String,
    label: Option<String>,
    clear_label: String,
    size: Size,
    style: SearchFieldStyle,
    debounce: Duration,
    max_suggestions: usize,
    on_change: Option<Report>,
    on_submit: Option<Report>,
    slot: Option<SearchFieldHandleSlot>,
}

/// An empty search field saying "Search", reporting each edit at once.
pub fn search_field() -> SearchField {
    SearchField {
        text: String::new(),
        placeholder: "Search".to_owned(),
        label: None,
        clear_label: "Clear".to_owned(),
        size: FIT,
        style: SearchFieldStyle::default(),
        debounce: Duration::ZERO,
        max_suggestions: 8,
        on_change: None,
        on_submit: None,
        slot: None,
    }
}

impl SearchField {
    /// Start with `text` in the field.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    /// Show `text` in the field while it is empty (default "Search"); it
    /// names the field for assistive technology unless
    /// [`label`](Self::label) does.
    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Name the field for assistive technology.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Name the clear button for assistive technology (default "Clear").
    pub fn clear_label(mut self, label: impl Into<String>) -> Self {
        self.clear_label = label.into();
        self
    }

    /// Report the query once the typing has rested for `delay` rather than
    /// at every edit; clearing, Enter and a chosen suggestion report at once.
    pub fn debounce(mut self, delay: Duration) -> Self {
        self.debounce = delay;
        self
    }

    /// Show at most `n` suggestions (default 8).
    pub fn max_suggestions(mut self, n: usize) -> Self {
        self.max_suggestions = n;
        self
    }

    /// Set the field's size within its parent (default: its content's).
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// Replace the whole [`SearchFieldStyle`].
    pub fn style(mut self, style: SearchFieldStyle) -> Self {
        self.style = style;
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
        let handle = SearchFieldHandle(build(cx, self));
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(handle);
        }
    }
}

const FIT: Size = Size {
    width: Length::Fit,
    height: Length::Fit,
};

type Shared = Rc<OnceCell<Rc<SearchState>>>;

fn with(shared: &Shared, f: impl FnOnce(&SearchState)) {
    if let Some(state) = shared.get() {
        f(state);
    }
}

fn line(text: &str, ink: Rgba, style: &SearchFieldStyle) -> TextRequest {
    TextRequest {
        text: text.to_owned(),
        font_size: FontSize::Fixed(style.font_size),
        color: ink,
        soft_wrap: false,
        locale: None,
        style: TextStyle::default(),
        spans: Vec::new(),
    }
}

fn build(cx: &mut BuildCx<'_>, search: &SearchField) -> Rc<SearchState> {
    let style = &search.style;
    let shared: Shared = Rc::new(OnceCell::new());
    let open_cell = cx.state(StateValue::Bool(false));
    let hover_cell = cx.state(StateValue::Bool(false));
    let mut parts = None;
    cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: Align::Stretch,
            size: search.size,
            ..FlexStyle::default()
        },
        |cx| {
            let mut input = None;
            let mut cross = None;
            let field = cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    gap: 6.0,
                    padding: style.field_padding,
                    align: Align::Center,
                    justify: Justify::Start,
                    size: Size {
                        width: Length::Fill { weight: 1.0 },
                        height: Length::Fit,
                    },
                    style: style.field,
                },
                |cx| {
                    let lens = magnifier(cx, style.quiet);
                    unhit_below(cx, lens.id());
                    let room = style.min_width
                        - style.field_padding.left
                        - style.field_padding.right
                        - 12.0
                        - GLYPH * 2.0;
                    let leaf = cx.text_input(
                        LeafStyle {
                            size: Size {
                                width: Length::Fill { weight: 1.0 },
                                height: Length::Fit,
                            },
                            style: BoxStyle::NONE,
                        },
                        line(&search.text, style.ink, style),
                        FieldLook {
                            placeholder: search.placeholder.clone(),
                            ..FieldLook::default()
                        },
                    );
                    cx.set_box_rules(
                        leaf,
                        BoxRules {
                            min_width: Span::dp(room.max(0.0)),
                            ..BoxRules::NONE
                        },
                    );
                    input = Some(leaf);
                    let disc = Rgba {
                        a: style.quiet.a * 0.55,
                        ..style.quiet
                    };
                    let button = clear(cx, disc, rgba(1.0, 1.0, 1.0, 1.0));
                    unhit_below(cx, button.id());
                    cross = Some(button);
                },
            );
            let input = input.expect("built");
            let cross = cross.expect("built");
            let (panel, rows) = panel(cx, search, field, &shared);
            parts = Some((field, input, cross, panel, rows));
        },
    );
    let (field, input, cross, panel, rows) = parts.expect("built");
    cx.focusable(input, true);
    let mut semantics = Semantics::role(Role::SearchField);
    semantics.label = search
        .label
        .clone()
        .or_else(|| (!search.placeholder.is_empty()).then(|| search.placeholder.clone()));
    cx.semantics(input, semantics);
    cx.bind_semantic_state(input, move |cx| SemanticState {
        expanded: Some(matches!(cx.get(open_cell), Some(StateValue::Bool(true)))),
        ..SemanticState::default()
    });
    cx.semantics(
        cross,
        Semantics::role(Role::Button).with_label(search.clear_label.clone()),
    );
    cx.set_hidden(cross, search.text.is_empty());
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
    cx.on_pointer(field, move |ev| {
        if let Some(p) = ev.pointer()
            && ev.phase() != DispatchPhase::Capture
            && matches!(p.phase, PointerPhase::Enter | PointerPhase::Leave)
        {
            ev.set(hover_cell, StateValue::Bool(p.phase == PointerPhase::Enter));
        }
    });
    let s = Rc::clone(&shared);
    cx.on_pointer(cross, move |ev| with(&s, |state| state.cross_pointer(ev)));
    cx.on_pointer(input, |ev| {
        field_input(ev, false);
    });
    let s = Rc::clone(&shared);
    cx.on_key(input, move |ev| with(&s, |state| state.input_key(ev)));
    let state = Rc::new(SearchState {
        input: input.id(),
        cross: cross.id(),
        panel: panel.id(),
        rows,
        open_cell,
        shown: RefCell::new(search.text.clone()),
        echo: RefCell::new(None),
        suggestions: RefCell::new(Vec::new()),
        typed: Cell::new(false),
        lit: Cell::new(None),
        open: Cell::new(false),
        pressed: Cell::new(None),
        edits: Cell::new(0),
        debounce: search.debounce,
        on_change: search.on_change.clone(),
        on_submit: search.on_submit.clone(),
    });
    let _ = shared.set(Rc::clone(&state));
    state
}

/// One row of the list, which shows one suggestion at a time.
struct Row {
    node: NodeId,
    /// Its text's node.
    text: NodeId,
    /// What its text draws now, without a mark.
    request: RefCell<TextRequest>,
    /// The cell its highlighted look reads.
    lit: StateId,
    /// Its place among the suggestions shown, which it announces.
    place: StateId,
    /// The stretch of its text drawn as matching.
    marked: RefCell<Option<std::ops::Range<usize>>>,
    matched: SpanStyle,
}

/// Build the list's panel against `field`: returns it and its rows.
fn panel(
    cx: &mut BuildCx<'_>,
    search: &SearchField,
    field: Handle,
    shared: &Shared,
) -> (Handle, Vec<Row>) {
    let style = &search.style;
    let mut rows = Vec::with_capacity(search.max_suggestions);
    let panel = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: Align::Stretch,
            padding: style.padding,
            size: FIT,
            style: style.panel,
            ..FlexStyle::default()
        },
        |cx| {
            for i in 0..search.max_suggestions {
                let lit = cx.state(StateValue::Bool(false));
                let place = cx.state(StateValue::Int(0));
                let request = line("", style.ink, style);
                let mut text = None;
                let node = cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        padding: style.item_padding,
                        align: Align::Center,
                        justify: Justify::Start,
                        size: Size {
                            width: Length::Fill { weight: 1.0 },
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                        ..FlexStyle::default()
                    },
                    |cx| {
                        let leaf = cx.leaf(LeafStyle {
                            size: FIT,
                            style: BoxStyle::NONE,
                        });
                        cx.text_request(leaf, request.clone());
                        text = Some(leaf.id());
                    },
                );
                unhit_below(cx, node.id());
                cx.interaction_style(
                    node,
                    InteractionStyle {
                        resting: BoxStyle::NONE,
                        hover: style.highlight,
                        pressed: style.highlight,
                        pressed_cell: None,
                        hover_cell: Some(lit),
                    },
                );
                cx.semantics(node, Semantics::role(Role::ListOption));
                cx.bind_semantic_state(node, move |cx| SemanticState {
                    position: match cx.get(place) {
                        Some(StateValue::Int(at)) if at > 0 => {
                            Some((at as u32 >> 16, at as u32 & 0xffff))
                        }
                        _ => None,
                    },
                    ..SemanticState::default()
                });
                let s = Rc::clone(shared);
                cx.on_pointer(node, move |ev| with(&s, |state| state.row_pointer(ev, i)));
                cx.set_hidden(node, true);
                rows.push(Row {
                    node: node.id(),
                    text: text.expect("built"),
                    request: RefCell::new(request),
                    lit,
                    place,
                    marked: RefCell::new(None),
                    matched: style.matched.clone(),
                });
            }
        },
    );
    cx.anchored(
        panel,
        field,
        Anchoring {
            side: Side::Bottom,
            align: AnchorAlign::Start,
            match_anchor: true,
            ..Anchoring::default()
        },
    );
    if let Some(shadow) = style.shadow {
        cx.shadow(panel, shadow);
    }
    cx.light_dismiss(panel);
    let s = Rc::clone(shared);
    cx.on_key(panel, move |ev| {
        if ev.dismissed() {
            with(&s, |state| state.close(ev));
        }
    });
    cx.set_hidden(panel, true);
    let mut semantics = Semantics::role(Role::ListBox);
    semantics.label = search.label.clone();
    cx.semantics(panel, semantics);
    (panel, rows)
}

/// Where `query`, lowercased, first occurs in `text`, as bytes of `text`:
/// `None` when it does not, or when lowercasing moved the bytes.
fn match_range(text: &str, query: &str) -> Option<std::ops::Range<usize>> {
    let query = query.to_lowercase();
    let key = text.to_lowercase();
    if query.is_empty() || key.len() != text.len() {
        return None;
    }
    let at = key.find(&query)?;
    let end = at + query.len();
    (text.is_char_boundary(at) && text.is_char_boundary(end)).then_some(at..end)
}

/// A search field, shared by its handlers.
pub(crate) struct SearchState {
    input: NodeId,
    cross: NodeId,
    panel: NodeId,
    rows: Vec<Row>,
    open_cell: StateId,
    /// What the field holds now.
    shown: RefCell<String>,
    /// The text the field was given, whose change it does not take as
    /// typing.
    echo: RefCell<Option<String>>,
    /// The suggestions the rows show, in order.
    suggestions: RefCell<Vec<String>>,
    /// Whether the field holds what was typed, which the list suggests for.
    typed: Cell<bool>,
    lit: Cell<Option<usize>>,
    open: Cell<bool>,
    /// The row a press landed on.
    pressed: Cell<Option<usize>>,
    /// Counts edits, so a debounced report an edit overtook is let go.
    edits: Cell<u32>,
    debounce: Duration,
    on_change: Option<Report>,
    on_submit: Option<Report>,
}

impl SearchState {
    /// Show the list, when it has suggestions for what was typed.
    fn open(&self, ev: &mut EventCx<'_>) {
        if self.suggestions.borrow().is_empty() || self.open.replace(true) {
            return;
        }
        ev.set_hidden(self.panel, false);
        ev.set(self.open_cell, StateValue::Bool(true));
    }

    /// Hide the list.
    fn close(&self, ev: &mut EventCx<'_>) {
        if !self.open.replace(false) {
            return;
        }
        self.light(ev, None);
        ev.set_hidden(self.panel, true);
        ev.set(self.open_cell, StateValue::Bool(false));
        self.pressed.set(None);
    }

    /// Highlight row `row`, or none: the field names it as its active
    /// descendant.
    fn light(&self, ev: &mut EventCx<'_>, row: Option<usize>) {
        let was = self.lit.replace(row);
        if was == row {
            return;
        }
        if let Some(r) = was {
            ev.set(self.rows[r].lit, StateValue::Bool(false));
        }
        if let Some(r) = row {
            ev.set(self.rows[r].lit, StateValue::Bool(true));
        }
        ev.set_active_descendant(self.input, row.map(|r| self.rows[r].node));
    }

    /// Mark each shown row's letters matching the query.
    fn mark(&self, ev: &mut EventCx<'_>) {
        let query = self.shown.borrow();
        for row in self.rows.iter().take(self.suggestions.borrow().len()) {
            let request = row.request.borrow();
            let mark = match_range(&request.text, &query);
            if *row.marked.borrow() != mark {
                let mut marked = request.clone();
                if let Some(range) = mark.clone() {
                    marked.spans = vec![TextSpan {
                        range,
                        style: row.matched.clone(),
                    }];
                }
                ev.set_text(row.text, marked);
                *row.marked.borrow_mut() = mark;
            }
        }
    }

    /// Show `suggestions` in the rows, as many as there are rows, hiding the
    /// rest; the list shows while the field holds what was typed.
    fn suggest(&self, ev: &mut EventCx<'_>, suggestions: Vec<String>) {
        self.light(ev, None);
        let n = suggestions.len().min(self.rows.len());
        for (i, row) in self.rows.iter().enumerate() {
            let shows = i < n;
            ev.set_hidden(row.node, !shows);
            if !shows {
                continue;
            }
            let text = &suggestions[i];
            if row.request.borrow().text != *text {
                row.request.borrow_mut().text = text.clone();
                *row.marked.borrow_mut() = Some(0..0);
            }
            let place = ((i as u32 + 1) << 16 | n as u32) as i32;
            ev.set(row.place, StateValue::Int(place));
            ev.set_semantics(
                row.node,
                Semantics::role(Role::ListOption).with_label(text.clone()),
            );
        }
        *self.suggestions.borrow_mut() = suggestions.into_iter().take(n).collect();
        self.mark(ev);
        if n == 0 {
            self.close(ev);
        } else if self.typed.get() && ev.focused() == Some(self.input) {
            self.open(ev);
        }
    }

    /// Write `text` into the field, unless it holds it.
    fn show(&self, ev: &mut EventCx<'_>, text: &str) {
        if *self.shown.borrow() == text {
            return;
        }
        *self.shown.borrow_mut() = text.to_owned();
        *self.echo.borrow_mut() = Some(text.to_owned());
        ev.edit(self.input, EditIntent::SelectAll);
        ev.edit(self.input, EditIntent::Paste(text.to_owned()));
        ev.set_hidden(self.cross, text.is_empty());
    }

    /// Report the query now, letting any debounced report go.
    fn report(&self, ev: &mut EventCx<'_>) {
        self.edits.set(self.edits.get().wrapping_add(1));
        let query = self.shown.borrow().clone();
        if let Some(change) = &self.on_change {
            (change.borrow_mut())(ev, &query);
        }
    }

    /// Submit `text`.
    fn submit(&self, ev: &mut EventCx<'_>, text: &str) {
        if let Some(submit) = &self.on_submit {
            (submit.borrow_mut())(ev, text);
        }
    }

    /// The user typed `text`: report it, at once or once the typing rests.
    fn typed(&self, ev: &mut EventCx<'_>, text: &str) {
        *self.shown.borrow_mut() = text.to_owned();
        self.typed.set(!text.is_empty());
        ev.set_hidden(self.cross, text.is_empty());
        if text.is_empty() {
            self.close(ev);
        }
        self.mark(ev);
        if self.debounce.is_zero() {
            self.report(ev);
        } else {
            let edits = self.edits.get().wrapping_add(1);
            self.edits.set(edits);
            ev.wake_after(self.input, self.debounce, DEBOUNCE | (edits & 0xffff));
        }
    }

    /// Empty the field, report the empty query and keep the focus in it.
    fn clear(&self, ev: &mut EventCx<'_>) {
        self.close(ev);
        self.typed.set(false);
        self.show(ev, "");
        self.mark(ev);
        self.report(ev);
    }

    /// Take suggestion `row` into the field and submit it.
    fn pick(&self, ev: &mut EventCx<'_>, row: usize) {
        let Some(text) = self.suggestions.borrow().get(row).cloned() else {
            return;
        };
        self.close(ev);
        self.typed.set(false);
        self.show(ev, &text);
        self.mark(ev);
        self.report(ev);
        self.submit(ev, &text);
    }

    /// The row `by` steps from the highlight, wrapping.
    fn step(&self, by: isize) -> Option<usize> {
        let n = self.suggestions.borrow().len() as isize;
        if n == 0 {
            return None;
        }
        Some(match self.lit.get() {
            None if by > 0 => 0,
            None => n as usize - 1,
            Some(at) => (at as isize + by).rem_euclid(n) as usize,
        })
    }

    fn cross_pointer(&self, ev: &mut EventCx<'_>) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(p) = ev.pointer().copied() else {
            return;
        };
        if p.phase == PointerPhase::Down && p.buttons.contains(PointerButtons::PRIMARY) {
            ev.stop_propagation();
            ev.request_focus(self.input);
            self.clear(ev);
        }
    }

    fn row_pointer(&self, ev: &mut EventCx<'_>, row: usize) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(p) = ev.pointer().copied() else {
            return;
        };
        match p.phase {
            PointerPhase::Move | PointerPhase::Enter if self.lit.get() != Some(row) => {
                self.light(ev, Some(row));
            }
            PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
                self.pressed.set(Some(row));
            }
            PointerPhase::Up
                if p.buttons.contains(PointerButtons::PRIMARY)
                    && self.pressed.take() == Some(row) =>
            {
                self.pick(ev, row);
            }
            _ => {}
        }
    }

    fn input_key(&self, ev: &mut EventCx<'_>) {
        if let Some(token) = ev.woken() {
            if token == DEBOUNCE | (self.edits.get() & 0xffff) {
                let query = self.shown.borrow().clone();
                if let Some(change) = &self.on_change {
                    (change.borrow_mut())(ev, &query);
                }
            }
            return;
        }
        if let Some(text) = ev.text_change() {
            let echoed = self.echo.borrow().as_deref() == Some(text);
            if echoed {
                *self.echo.borrow_mut() = None;
            } else {
                let text = text.to_owned();
                self.typed(ev, &text);
            }
            return;
        }
        if let Some(change) = ev.focus_change() {
            if change == FocusChange::Lost && ev.focused() != Some(self.input) {
                self.close(ev);
            }
            return;
        }
        if let Some(k) = ev.key().copied()
            && self.key(ev, k)
        {
            ev.stop_propagation();
            return;
        }
        field_input(ev, false);
    }

    /// A key the field takes before it edits with it; whether it took it.
    fn key(&self, ev: &mut EventCx<'_>, k: KeyEvent) -> bool {
        let open = self.open.get();
        let taken = match k.key {
            Key::Down | Key::Up => !self.suggestions.borrow().is_empty(),
            Key::Enter => true,
            Key::Escape => open || !self.shown.borrow().is_empty(),
            _ => false,
        };
        if !taken || !k.pressed {
            return taken;
        }
        match k.key {
            Key::Down | Key::Up => {
                if !open {
                    self.typed.set(true);
                    self.open(ev);
                }
                let to = self.step(if k.key == Key::Down { 1 } else { -1 });
                self.light(ev, to);
            }
            Key::Enter if !k.repeat => match self.lit.get() {
                Some(row) if open => self.pick(ev, row),
                _ => {
                    self.close(ev);
                    self.typed.set(false);
                    let text = self.shown.borrow().clone();
                    if self.edits.get() & 0xffff != 0 && !self.debounce.is_zero() {
                        self.report(ev);
                    }
                    self.submit(ev, &text);
                }
            },
            Key::Escape if open => self.close(ev),
            Key::Escape => self.clear(ev),
            _ => {}
        }
        true
    }
}

/// A handle an application captures to read a built search field's text,
/// set it and give it suggestions from its own handlers. Cheap to clone.
#[derive(Clone)]
pub struct SearchFieldHandle(Rc<SearchState>);

impl SearchFieldHandle {
    /// What the field holds.
    pub fn text(&self) -> String {
        self.0.shown.borrow().clone()
    }

    /// Put `text` in the field, without telling `on_change`.
    pub fn set_text(&self, ev: &mut EventCx<'_>, text: &str) {
        let state = &self.0;
        state.close(ev);
        state.typed.set(false);
        state.show(ev, text);
        state.mark(ev);
    }

    /// Suggest `suggestions` for the query, in order, as many as the field
    /// shows; none closes the list.
    pub fn set_suggestions<I>(&self, ev: &mut EventCx<'_>, suggestions: I)
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let suggestions = suggestions.into_iter().map(Into::into).collect();
        self.0.suggest(ev, suggestions);
    }

    /// The suggestions the list shows.
    pub fn suggestions(&self) -> Vec<String> {
        self.0.suggestions.borrow().clone()
    }

    /// Whether the list of suggestions shows.
    pub fn is_open(&self) -> bool {
        self.0.open.get()
    }

    /// Empty the field, telling `on_change`.
    pub fn clear(&self, ev: &mut EventCx<'_>) {
        self.0.clear(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::match_range;

    #[test]
    fn the_query_is_matched_whatever_its_case() {
        assert_eq!(match_range("Lisbon", "BO"), Some(3..5));
        assert_eq!(match_range("Oslo", "o"), Some(0..1));
        assert_eq!(match_range("Oslo", ""), None);
        assert_eq!(match_range("Oslo", "x"), None);
        // Lowercasing İ moves the bytes: nothing is marked.
        assert_eq!(match_range("İzmir", "zm"), None);
    }
}
