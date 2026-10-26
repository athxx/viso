//! A combo box's parts — a text field with a list of options it filters as
//! the user types, choosing from a closed set of them or taking free text —
//! shared by the `ComboBox` widget and the view's `ComboBox` node.
//!
//! The focus stays in the field while the list shows: the highlighted option
//! is the field's active descendant, which assistive technology reads.
//! Typing opens the list and keeps the options whose label holds what was
//! typed, in their order, the matched letters in bold. Over a closed set the
//! first option starting with it is highlighted, and only an option is ever
//! the value: Enter or Tab chooses the highlighted one, and Escape, a press
//! outside or the focus leaving puts the chosen option's label back; with no
//! match the list says so ([`ComboSpec::empty`]) and Enter does nothing.
//! With [`free`](ComboSpec::free) text the text is the value: nothing is
//! highlighted until an arrow key moves there, Enter takes the text unless an
//! option is highlighted, the focus leaving takes it too, and a list with no
//! match closes.
//!
//! The keyboard: Down opens the list on its first option, Up on its last,
//! Alt+Down opens it as it is; open, Up and Down move the highlight,
//! wrapping, Page Up and Page Down by a list's height, Alt+Up closes it.
//! Home, End and the rest edit the text. The pointer: the chevron opens and
//! closes the whole list; a press on the field while it lacks the focus
//! opens it too, selecting the text; a click on an option chooses it.
//!
//! Semantics: the field is an `editablecombobox` (WAI-ARIA `combobox`)
//! announcing whether its list is expanded and naming the highlighted option
//! as its active descendant; the list is a `listbox` of `option`s announcing
//! whether each is the chosen one and its place among those shown.
//!
//! Invalidation: the list builds once with the field. Filtering hides and
//! shows options and shapes again the labels whose matched letters moved;
//! a highlight is a `PAINT` of one option's look.

use std::cell::{Cell, OnceCell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use crate::glyphs::{GLYPH, chevron_down, glyph_slot};
use crate::part_writes::PartWrites;
use crate::{
    Align, AnchorAlign, Anchoring, Axis, BoxRules, BoxStyle, BuildCx, DispatchPhase, EditIntent,
    EventCx, FieldLook, FieldSeed, FlexStyle, FocusChange, FontSize, FontWeight, Handle, Inset,
    InteractionStyle, Justify, Key, KeyEvent, LeafStyle, Length, NodeId, PointerButtons,
    PointerPhase, Rgba, Role, ScrollStyle, SemanticState, Semantics, Shadow, Side, Sizing, Span,
    SpanStyle, StateId, StateValue, TextRequest, TextSpan, TextStyle, field_input,
};

/// What runs when the user chooses an option, with its index.
pub type Pick = Box<dyn FnMut(&mut EventCx<'_>, usize)>;

/// What runs when a free-text combo box takes its text.
pub type Commit = Box<dyn FnMut(&mut EventCx<'_>, &str)>;

/// One option of a combo box: its label, and whether it can be chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboOption {
    pub label: String,
    pub enabled: bool,
}

impl ComboOption {
    /// An option labelled `label` that can be chosen.
    pub fn new(label: impl Into<String>) -> ComboOption {
        ComboOption {
            label: label.into(),
            enabled: true,
        }
    }
}

/// What a combo box's parts are built from.
#[derive(Debug, Clone, PartialEq)]
pub struct ComboSpec {
    pub options: Vec<ComboOption>,
    /// The option chosen to start with; the field shows its label.
    pub selected: Option<usize>,
    /// What a free-text box starts holding.
    pub text: String,
    /// Whether it takes any text, the options only suggesting it.
    pub free: bool,
    /// What the field shows while empty.
    pub placeholder: String,
    /// What the list says when no option matches what was typed.
    pub empty: String,
    /// The field's name for assistive technology.
    pub label: Option<String>,
    /// Its size within its parent.
    pub size: Sizing,
    pub style: ComboBoxStyle,
}

impl Default for ComboSpec {
    fn default() -> Self {
        ComboSpec {
            options: Vec::new(),
            selected: None,
            text: String::new(),
            free: false,
            placeholder: String::new(),
            empty: "No matches".to_owned(),
            label: None,
            size: FIT,
            style: ComboBoxStyle::default(),
        }
    }
}

/// How a combo box looks.
#[derive(Debug, Clone, PartialEq)]
pub struct ComboBoxStyle {
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
    /// The ink of the chevron and of the line saying nothing matches.
    pub quiet: Rgba,
    /// The panel the options sit in.
    pub panel: BoxStyle,
    pub shadow: Option<Shadow>,
    /// The room between the panel's edge and its options.
    pub padding: Inset,
    /// The room inside an option.
    pub item_padding: Inset,
    /// The highlighted option's box.
    pub highlight: BoxStyle,
    /// The opacity of a disabled option.
    pub disabled: f32,
    /// The greatest height of the panel; a longer list scrolls.
    pub max_height: f32,
    /// How the letters of an option matching what was typed draw.
    pub matched: SpanStyle,
}

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

impl Default for ComboBoxStyle {
    fn default() -> Self {
        let border = crate::Border {
            width: 1.0,
            color: rgba(0.0, 0.0, 0.0, 0.16),
        };
        ComboBoxStyle {
            field: BoxStyle {
                border,
                ..BoxStyle::solid(rgba(1.0, 1.0, 1.0, 1.0)).with_radius(6.0)
            },
            field_hover: BoxStyle {
                border,
                ..BoxStyle::solid(rgba(0.98, 0.98, 0.985, 1.0)).with_radius(6.0)
            },
            field_padding: Inset {
                left: 10.0,
                right: 6.0,
                top: 5.0,
                bottom: 5.0,
            },
            min_width: 160.0,
            font_size: 14.0,
            ink: rgba(0.09, 0.09, 0.1, 1.0),
            quiet: rgba(0.42, 0.42, 0.45, 1.0),
            panel: BoxStyle {
                border: crate::Border {
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
            disabled: 0.4,
            max_height: 320.0,
            matched: SpanStyle::default().weight(FontWeight(600)),
        }
    }
}

const FIT: Sizing = Sizing {
    width: Length::Fit,
    height: Length::Fit,
};

type Shared = Rc<OnceCell<Rc<ComboState>>>;

fn with(shared: &Shared, f: impl FnOnce(&ComboState)) {
    if let Some(state) = shared.get() {
        f(state);
    }
}

fn line(text: &str, ink: Rgba, style: &ComboBoxStyle) -> TextRequest {
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

fn build(
    cx: &mut BuildCx<'_>,
    combo: &ComboSpec,
    on_pick: Option<Pick>,
    on_text: Option<Commit>,
) -> Rc<ComboState> {
    let style = &combo.style;
    let n = combo.options.len();
    let chosen = combo.selected.filter(|&i| i < n);
    let text = match chosen {
        Some(i) if !combo.free || combo.text.is_empty() => combo.options[i].label.clone(),
        _ => combo.text.clone(),
    };
    let shared: Shared = Rc::new(OnceCell::new());
    let chosen_cell = cx.state(StateValue::Int(chosen.map_or(-1, |i| i as i32)));
    let open_cell = cx.state(StateValue::Bool(false));
    let hover_cell = cx.state(StateValue::Bool(false));
    let mut parts = None;
    let root = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: Align::Stretch,
            size: combo.size,
            ..FlexStyle::default()
        },
        |cx| {
            let mut input = None;
            let mut arrow = None;
            let field = cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    gap: 4.0,
                    padding: style.field_padding,
                    align: Align::Center,
                    justify: Justify::Start,
                    size: FIT,
                    style: style.field,
                },
                |cx| {
                    let room = style.min_width
                        - style.field_padding.left
                        - style.field_padding.right
                        - 4.0
                        - GLYPH;
                    let leaf = cx.text_input(
                        LeafStyle {
                            size: Sizing {
                                width: Length::Fill { weight: 1.0 },
                                height: Length::Fit,
                            },
                            style: BoxStyle::NONE,
                        },
                        line(&text, style.ink, style),
                        FieldLook {
                            placeholder: combo.placeholder.clone(),
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
                    arrow = Some(glyph_slot(cx, |cx| {
                        chevron_down(cx, style.quiet);
                    }));
                },
            );
            let arrow = arrow.expect("built");
            cx.unhit_below(arrow);
            let input = input.expect("built");
            let (panel, viewport, rows, empty) = panel(cx, combo, field, chosen_cell, &shared);
            parts = Some((field, input, arrow, panel, viewport, rows, empty));
        },
    );
    let (field, input, arrow, panel, viewport, rows, empty) = parts.expect("built");
    cx.focusable(input, true);
    let mut semantics = Semantics::role(Role::EditableComboBox);
    semantics.label = combo.label.clone();
    cx.semantics(input, semantics);
    cx.bind_semantic_state(input, move |cx| SemanticState {
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
    cx.on_pointer(field, move |ev| {
        if let Some(p) = ev.pointer()
            && ev.phase() != DispatchPhase::Capture
            && matches!(p.phase, PointerPhase::Enter | PointerPhase::Leave)
        {
            let over = p.phase == PointerPhase::Enter;
            ev.set(hover_cell, StateValue::Bool(over));
        }
    });
    let s = Rc::clone(&shared);
    cx.on_pointer(arrow, move |ev| with(&s, |state| state.arrow_pointer(ev)));
    let s = Rc::clone(&shared);
    cx.on_pointer(input, move |ev| with(&s, |state| state.input_pointer(ev)));
    let s = Rc::clone(&shared);
    cx.on_key(input, move |ev| with(&s, |state| state.input_key(ev)));
    let committed = match chosen {
        Some(i) => combo.options[i].label.clone(),
        None => text.clone(),
    };
    let state = Rc::new(ComboState {
        root: root.id(),
        input: input.id(),
        panel: panel.id(),
        viewport,
        empty,
        rows,
        free: combo.free,
        chosen_cell,
        open_cell,
        chosen: Cell::new(chosen),
        value: RefCell::new(if combo.free { text.clone() } else { committed }),
        shown: RefCell::new(text),
        echo: RefCell::new(None),
        typed: Cell::new(false),
        visible: RefCell::new((0..n).collect()),
        lit: Cell::new(None),
        open: Cell::new(false),
        pressed: Cell::new(None),
        look: FieldLook {
            placeholder: combo.placeholder.clone(),
            ..FieldLook::default()
        },
        style: combo.style.clone(),
        on_pick: on_pick.map(RefCell::new),
        on_text: on_text.map(RefCell::new),
    });
    let _ = shared.set(Rc::clone(&state));
    state
}

/// One option of the list.
struct Row {
    node: NodeId,
    /// Its label's node, and the text it draws without a match.
    text: NodeId,
    request: TextRequest,
    /// The cell its highlighted look reads.
    lit: StateId,
    /// Its place among the options shown, which it announces.
    place: StateId,
    enabled: bool,
    /// Its label, lowercased, which typing matches.
    key: String,
    /// The stretch of its label drawn as matching.
    marked: RefCell<Option<Range<usize>>>,
    matched: SpanStyle,
}

/// Build the list's panel against `field`: returns it, its scroll viewport,
/// its options and the line saying nothing matches.
fn panel(
    cx: &mut BuildCx<'_>,
    combo: &ComboSpec,
    field: Handle,
    chosen_cell: StateId,
    shared: &Shared,
) -> (Handle, NodeId, Vec<Row>, NodeId) {
    let style = &combo.style;
    let n = combo.options.len();
    let mut rows = Vec::with_capacity(n);
    let mut viewport = None;
    let mut empty = None;
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
            let view = cx.scroll(
                ScrollStyle {
                    axis: Axis::Column,
                    size: FIT,
                    style: BoxStyle::NONE,
                    ..Default::default()
                },
                |cx| {
                    cx.flex(
                        FlexStyle {
                            axis: Axis::Column,
                            align: Align::Stretch,
                            size: FIT,
                            ..FlexStyle::default()
                        },
                        |cx| {
                            for (i, option) in combo.options.iter().enumerate() {
                                let (row, node) = row(cx, option, style);
                                let s = Rc::clone(shared);
                                cx.on_pointer(node, move |ev| {
                                    with(&s, |state| state.row_pointer(ev, i))
                                });
                                let place = row.place;
                                cx.bind_semantic_state(node, move |cx| SemanticState {
                                    selected: Some(matches!(
                                        cx.get(chosen_cell),
                                        Some(StateValue::Int(c)) if c == i as i32
                                    )),
                                    position: match cx.get(place) {
                                        Some(StateValue::Int(at)) if at > 0 => {
                                            Some((at as u32 >> 16, at as u32 & 0xffff))
                                        }
                                        _ => None,
                                    },
                                    ..SemanticState::default()
                                });
                                rows.push(row);
                            }
                            let none = cx.flex(
                                FlexStyle {
                                    axis: Axis::Row,
                                    padding: style.item_padding,
                                    size: FIT,
                                    ..FlexStyle::default()
                                },
                                |cx| {
                                    let leaf = cx.leaf(LeafStyle {
                                        size: FIT,
                                        style: BoxStyle::NONE,
                                    });
                                    cx.text_request(leaf, line(&combo.empty, style.quiet, style));
                                    cx.semantics(
                                        leaf,
                                        Semantics::role(Role::Label)
                                            .with_label(combo.empty.clone()),
                                    );
                                },
                            );
                            cx.set_hidden(none, true);
                            empty = Some(none.id());
                        },
                    );
                },
            );
            let room = style.max_height - style.padding.top - style.padding.bottom;
            cx.set_box_rules(
                view,
                BoxRules {
                    max_height: Span::dp(room.max(0.0)),
                    ..BoxRules::NONE
                },
            );
            viewport = Some(view.id());
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
            with(&s, |state| state.dismiss(ev));
        }
    });
    cx.set_hidden(panel, true);
    let mut semantics = Semantics::role(Role::ListBox);
    semantics.label = combo.label.clone();
    cx.semantics(panel, semantics);
    (panel, viewport.expect("built"), rows, empty.expect("built"))
}

fn row(cx: &mut BuildCx<'_>, option: &ComboOption, style: &ComboBoxStyle) -> (Row, Handle) {
    let lit = cx.state(StateValue::Bool(false));
    let place = cx.state(StateValue::Int(0));
    let request = line(option.label.as_str(), style.ink, style);
    let mut text = None;
    let node = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            padding: style.item_padding,
            align: Align::Center,
            justify: Justify::Start,
            size: Sizing {
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
    cx.unhit_below(node);
    if !option.enabled {
        cx.structure(|s| {
            s.store.set_enabled(node.id(), false);
            s.store.set_hittable(node.id(), false);
            s.store.set_opacity(node.id(), style.disabled);
        });
    }
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
    cx.semantics(
        node,
        Semantics::role(Role::ListOption).with_label(option.label.as_str().to_owned()),
    );
    let row = Row {
        node: node.id(),
        text: text.expect("built"),
        request,
        lit,
        place,
        enabled: option.enabled,
        key: option.label.as_str().to_lowercase(),
        marked: RefCell::new(None),
        matched: style.matched.clone(),
    };
    (row, node)
}

/// Where `filter`, lowercased, first occurs in `label`, as bytes of `label`:
/// `None` when it does not, or when lowercasing moved the bytes.
fn match_range(label: &str, key: &str, filter: &str) -> Option<Range<usize>> {
    if filter.is_empty() || key.len() != label.len() {
        return None;
    }
    let at = key.find(filter)?;
    let end = at + filter.len();
    (label.is_char_boundary(at) && label.is_char_boundary(end)).then_some(at..end)
}

/// A combo box, shared by its handlers.
pub(crate) struct ComboState {
    /// The column the parts are built in.
    root: NodeId,
    input: NodeId,
    panel: NodeId,
    viewport: NodeId,
    /// The line saying nothing matches.
    empty: NodeId,
    rows: Vec<Row>,
    free: bool,
    chosen_cell: StateId,
    open_cell: StateId,
    chosen: Cell<Option<usize>>,
    /// The value: the chosen option's label, or a free-text box's text.
    value: RefCell<String>,
    /// What the field holds now.
    shown: RefCell<String>,
    /// The text the box wrote into the field, whose change it does not
    /// take as typing.
    echo: RefCell<Option<String>>,
    /// Whether the field holds what was typed rather than the value.
    typed: Cell<bool>,
    /// The options the list shows, in order.
    visible: RefCell<Vec<usize>>,
    lit: Cell<Option<usize>>,
    open: Cell<bool>,
    /// The option a press landed on.
    pressed: Cell<Option<usize>>,
    /// How the field draws its text, for a seed of it.
    look: FieldLook,
    style: ComboBoxStyle,
    on_pick: Option<RefCell<Pick>>,
    on_text: Option<RefCell<Commit>>,
}

impl ComboState {
    /// Open the list, showing the options `filter` keeps.
    fn open(&self, ev: &mut dyn PartWrites) {
        if self.open.replace(true) {
            return;
        }
        ev.set_hidden(self.panel, false);
        ev.set(self.open_cell, StateValue::Bool(true));
    }

    /// Close the list.
    fn close(&self, ev: &mut dyn PartWrites) {
        if !self.open.replace(false) {
            return;
        }
        self.light(ev, None);
        ev.set_hidden(self.panel, true);
        ev.set(self.open_cell, StateValue::Bool(false));
        self.pressed.set(None);
    }

    /// Show the options matching `filter`, all of them when it is empty, the
    /// matched letters marked.
    fn filter(&self, ev: &mut dyn PartWrites, filter: &str) {
        let filter = filter.to_lowercase();
        let mut visible = self.visible.borrow_mut();
        visible.clear();
        for (i, row) in self.rows.iter().enumerate() {
            let shows = filter.is_empty() || row.key.contains(&filter);
            if shows {
                visible.push(i);
            }
            ev.set_hidden(row.node, !shows);
            let mark = shows
                .then(|| match_range(&row.request.text, &row.key, &filter))
                .flatten();
            if *row.marked.borrow() != mark {
                let mut request = row.request.clone();
                if let Some(range) = mark.clone() {
                    request.spans = vec![TextSpan {
                        range,
                        style: row.matched.clone(),
                    }];
                }
                ev.set_text(row.text, request);
                *row.marked.borrow_mut() = mark;
            }
        }
        let n = visible.len() as u32;
        for (at, &i) in visible.iter().enumerate() {
            let place = ((at as u32 + 1) << 16 | n) as i32;
            ev.set(self.rows[i].place, StateValue::Int(place));
        }
        ev.set_hidden(self.empty, !visible.is_empty() || self.free);
    }

    /// Highlight option `row`, or none: the field names it as its active
    /// descendant.
    fn light(&self, ev: &mut dyn PartWrites, row: Option<usize>) {
        let was = self.lit.replace(row);
        if was == row {
            return;
        }
        if let Some(r) = was {
            ev.set(self.rows[r].lit, StateValue::Bool(false));
        }
        if let Some(r) = row {
            let node = self.rows[r].node;
            ev.set(self.rows[r].lit, StateValue::Bool(true));
            ev.scroll_into_view(node);
        }
        ev.set_active_descendant(self.input, row.map(|r| self.rows[r].node));
    }

    /// Write `text` into the field, unless it holds it.
    fn show(&self, w: &mut dyn PartWrites, text: &str) {
        if *self.shown.borrow() == text {
            return;
        }
        *self.shown.borrow_mut() = text.to_owned();
        *self.echo.borrow_mut() = Some(text.to_owned());
        let seed = FieldSeed {
            request: line(text, self.style.ink, &self.style),
            valued: true,
            look: self.look.clone(),
        };
        w.replace_text(self.input, seed);
    }

    /// Choose option `row`: it becomes the value and the field shows it.
    fn pick(&self, ev: &mut EventCx<'_>, row: usize) {
        if !self.rows[row].enabled {
            return;
        }
        self.close(ev);
        let label = self.rows[row].request.text.clone();
        self.chosen.set(Some(row));
        ev.set(self.chosen_cell, StateValue::Int(row as i32));
        self.show(ev, &label);
        self.typed.set(false);
        self.filter(ev, "");
        *self.value.borrow_mut() = label.clone();
        if let Some(pick) = &self.on_pick {
            (pick.borrow_mut())(ev, row);
        }
        if self.free
            && let Some(commit) = &self.on_text
        {
            (commit.borrow_mut())(ev, &label);
        }
    }

    /// Take a free-text box's text as its value.
    fn commit_text(&self, ev: &mut EventCx<'_>) {
        self.close(ev);
        self.typed.set(false);
        let text = self.shown.borrow().clone();
        if *self.value.borrow() == text {
            return;
        }
        *self.value.borrow_mut() = text.clone();
        // An option the text spells is chosen; other text chooses none.
        let chosen = self.rows.iter().position(|r| r.request.text == text);
        self.chosen.set(chosen);
        ev.set(
            self.chosen_cell,
            StateValue::Int(chosen.map_or(-1, |i| i as i32)),
        );
        if let Some(commit) = &self.on_text {
            (commit.borrow_mut())(ev, &text);
        }
    }

    /// Put the value back in the field and close the list.
    fn revert(&self, ev: &mut dyn PartWrites) {
        self.close(ev);
        if self.typed.replace(false) {
            let value = self.value.borrow().clone();
            self.show(ev, &value);
            self.filter(ev, "");
        }
    }

    /// The list closes for a press outside it.
    fn dismiss(&self, ev: &mut dyn PartWrites) {
        if self.free {
            self.close(ev);
        } else {
            self.revert(ev);
        }
    }

    /// Open the whole list, the value highlighted, the focus in the field.
    fn open_all(&self, ev: &mut dyn PartWrites) {
        self.filter(ev, "");
        self.open(ev);
        let chosen = self.chosen.get().filter(|&i| self.rows[i].enabled);
        self.light(ev, chosen);
    }

    /// The text the user typed: filter the list by it.
    fn typed(&self, ev: &mut EventCx<'_>, text: &str) {
        *self.shown.borrow_mut() = text.to_owned();
        self.typed.set(true);
        self.filter(ev, text);
        let visible = self.visible.borrow().clone();
        if self.free && (visible.is_empty() || text.is_empty()) {
            self.close(ev);
            return;
        }
        self.open(ev);
        let lit = if self.free {
            self.lit.get().filter(|r| visible.contains(r))
        } else if text.is_empty() {
            self.chosen.get().filter(|r| visible.contains(r))
        } else {
            // The first option starting with it, else the first holding it.
            let key = text.to_lowercase();
            let enabled = |&&r: &&usize| self.rows[r].enabled;
            visible
                .iter()
                .filter(enabled)
                .find(|&&r| self.rows[r].key.starts_with(&key))
                .or_else(|| visible.iter().find(enabled))
                .copied()
        };
        self.light(ev, lit);
    }

    /// The enabled option `by` steps from the highlight among those shown,
    /// wrapping, or stopping at the ends when `clamp`.
    fn step(&self, by: isize, clamp: bool) -> Option<usize> {
        let visible = self.visible.borrow();
        let n = visible.len() as isize;
        if n == 0 {
            return None;
        }
        let enabled = |at: isize| self.rows[visible[at as usize]].enabled;
        let from = self
            .lit
            .get()
            .and_then(|r| visible.iter().position(|&v| v == r))
            .map(|at| at as isize);
        let found = match (from, clamp) {
            (None, _) if by > 0 => (0..n).find(|&at| enabled(at)),
            (None, _) => (0..n).rev().find(|&at| enabled(at)),
            (Some(from), false) => (1..=n)
                .map(|k| (from + by.signum() * k).rem_euclid(n))
                .find(|&at| enabled(at)),
            (Some(from), true) => {
                let to = (from + by).clamp(0, n - 1);
                let ahead = if by > 0 {
                    (to..n).find(|&at| enabled(at))
                } else {
                    (0..=to).rev().find(|&at| enabled(at))
                };
                ahead.or(Some(from))
            }
        };
        found.map(|at| visible[at as usize])
    }

    /// How many options a page of the list holds.
    fn page(&self, ev: &EventCx<'_>) -> isize {
        let view = ev.rect_of(self.viewport).map_or(0.0, |r| r.h);
        let row = self
            .lit
            .get()
            .or_else(|| self.visible.borrow().first().copied())
            .and_then(|r| ev.rect_of(self.rows[r].node))
            .map_or(0.0, |r| r.h);
        if row <= 0.0 {
            return 1;
        }
        ((view / row).floor() as isize - 1).max(1)
    }

    fn arrow_pointer(&self, ev: &mut EventCx<'_>) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(p) = ev.pointer().copied() else {
            return;
        };
        if p.phase == PointerPhase::Down && p.buttons.contains(PointerButtons::PRIMARY) {
            ev.stop_propagation();
            if self.open.get() {
                self.close(ev);
            } else {
                ev.request_focus(self.input);
                ev.edit(self.input, EditIntent::SelectAll);
                self.open_all(ev);
            }
        }
    }

    fn input_pointer(&self, ev: &mut EventCx<'_>) {
        let first = ev.pointer().is_some_and(|p| {
            p.phase == PointerPhase::Down && p.buttons.contains(PointerButtons::PRIMARY)
        }) && ev.phase() == DispatchPhase::Target
            && ev.focused() != Some(self.input);
        field_input(ev, false);
        // A first press into the box opens the whole list, its text
        // selected, as a plain drop-down would.
        if first && !ev.direct() {
            ev.edit(self.input, EditIntent::SelectAll);
            if !self.open.get() {
                self.open_all(ev);
            }
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
        if let Some(text) = ev.text_change() {
            let echoed = self.echo.borrow().as_deref() == Some(text);
            if echoed {
                *self.echo.borrow_mut() = None;
            } else {
                self.typed(ev, text);
            }
            return;
        }
        if let Some(change) = ev.focus_change() {
            if change == FocusChange::Lost && ev.focused() != Some(self.input) {
                if self.free {
                    if self.typed.get() {
                        self.commit_text(ev);
                    } else {
                        self.close(ev);
                    }
                } else {
                    self.revert(ev);
                }
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

    /// A key the box takes before the field edits with it; whether it took
    /// it.
    fn key(&self, ev: &mut EventCx<'_>, k: KeyEvent) -> bool {
        let open = self.open.get();
        let alt = k.modifiers.alt;
        let taken = match k.key {
            Key::Down | Key::Up => true,
            Key::PageUp | Key::PageDown | Key::Escape => open,
            Key::Enter => open || self.free,
            Key::Tab => false,
            _ => false,
        };
        if k.key == Key::Tab && k.pressed {
            // The highlight is chosen, and the focus moves on.
            match self.lit.get() {
                Some(row) if open => self.pick(ev, row),
                _ if self.free && self.typed.get() => self.commit_text(ev),
                _ if !self.free => self.revert(ev),
                _ => self.close(ev),
            }
            return false;
        }
        if !taken || !k.pressed {
            return taken;
        }
        match k.key {
            Key::Down if alt && !open => self.open_all(ev),
            Key::Up if alt => self.dismiss(ev),
            Key::Down | Key::Up if !open => {
                let filter = if self.typed.get() {
                    self.shown.borrow().clone()
                } else {
                    String::new()
                };
                self.filter(ev, &filter);
                if self.visible.borrow().is_empty() {
                    return true;
                }
                self.open(ev);
                self.light(ev, None);
                let to = self.step(if k.key == Key::Down { 1 } else { -1 }, false);
                self.light(ev, to);
            }
            Key::Down | Key::Up => {
                let to = self.step(if k.key == Key::Down { 1 } else { -1 }, false);
                self.light(ev, to);
            }
            Key::PageDown | Key::PageUp => {
                let page = self.page(ev);
                let by = if k.key == Key::PageDown { page } else { -page };
                let to = self.step(by, true);
                self.light(ev, to);
            }
            Key::Enter if !k.repeat => match self.lit.get() {
                Some(row) if open => self.pick(ev, row),
                // With no match, Enter does nothing; the text stays to edit.
                _ if self.free => self.commit_text(ev),
                _ => {}
            },
            Key::Escape => {
                if self.free {
                    self.close(ev);
                } else {
                    self.revert(ev);
                }
            }
            _ => {}
        }
        true
    }
}

/// A combo box's built parts: the handle that reads and sets its value and
/// opens its list. Cheap to clone.
#[derive(Clone)]
pub struct ComboParts(Rc<ComboState>);

impl ComboParts {
    /// Builds a combo box's parts, as `spec` asks, under the current parent:
    /// `on_pick` runs with the index of an option the user chooses,
    /// `on_text` with the text a free-text box takes — on Enter, as the
    /// focus leaves, or when an option is chosen.
    pub fn build(
        cx: &mut BuildCx<'_>,
        spec: &ComboSpec,
        on_pick: Option<Pick>,
        on_text: Option<Commit>,
    ) -> ComboParts {
        ComboParts(build(cx, spec, on_pick, on_text))
    }

    /// The column the parts are built in.
    pub fn root(&self) -> Handle {
        Handle::of(self.0.root)
    }

    /// The chosen option's index: for free text, the option the text spells.
    pub fn selected(&self) -> Option<usize> {
        self.0.chosen.get()
    }

    /// The value: the chosen option's label, or a free-text box's text.
    pub fn text(&self) -> String {
        self.0.value.borrow().clone()
    }

    /// Choose option `index`, or none, without telling `on_pick`.
    pub fn select(&self, w: &mut dyn PartWrites, index: Option<usize>) {
        let state = &self.0;
        let index = index.filter(|&i| i < state.rows.len());
        state.close(w);
        state.chosen.set(index);
        w.set(
            state.chosen_cell,
            StateValue::Int(index.map_or(-1, |i| i as i32)),
        );
        let label = index.map_or(String::new(), |i| state.rows[i].request.text.clone());
        *state.value.borrow_mut() = label.clone();
        state.typed.set(false);
        state.show(w, &label);
        state.filter(w, "");
    }

    /// Set a free-text box's text, without telling `on_text`; the option
    /// it spells is chosen.
    pub fn set_text(&self, w: &mut dyn PartWrites, text: &str) {
        let state = &self.0;
        state.close(w);
        *state.value.borrow_mut() = text.to_owned();
        let chosen = state.rows.iter().position(|r| r.request.text == text);
        state.chosen.set(chosen);
        w.set(
            state.chosen_cell,
            StateValue::Int(chosen.map_or(-1, |i| i as i32)),
        );
        state.typed.set(false);
        state.show(w, text);
        state.filter(w, "");
    }

    /// Open the whole list.
    pub fn open(&self, w: &mut dyn PartWrites) {
        self.0.open_all(w);
    }

    /// Close the list, putting the value back in the field.
    pub fn close(&self, w: &mut dyn PartWrites) {
        self.0.dismiss(w);
    }

    /// Whether the list shows.
    pub fn is_open(&self) -> bool {
        self.0.open.get()
    }
}
