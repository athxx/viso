//! `Select` — a field showing one option of a list, the list opening from it
//! to choose another.
//!
//! The field shows the chosen option's label, or a placeholder before one is
//! chosen, and is as wide as the widest option. A press on it opens the list;
//! a release on an option chooses it, so a press, drag and release chooses in
//! one gesture, and a release where the list opened does not. The list opens
//! with the chosen option highlighted and scrolled into view; on Apple
//! systems ([`SelectKind::PopUp`]) it opens over the field with the chosen
//! option checked and level with it, elsewhere ([`SelectKind::DropDown`])
//! below the field, and a list taller than
//! [`SelectStyle::max_height`] scrolls. A press outside closes it unchanged.
//!
//! The keyboard, the list closed: Space, Alt+Down, Alt+Up and F4 open it (on
//! Apple systems Space, Up and Down); off Apple systems Up, Down, Home and
//! End choose the option before, after, first and last at once. The list
//! open: Up and Down move the highlight over the enabled options, Home and
//! End to the first and last, Page Up and Page Down by a list's height;
//! Enter and Space choose the highlighted option, Alt+Up, Alt+Down and F4 do
//! too, and Escape and Tab close the list unchanged. Typing, open or closed,
//! goes to the next option whose label starts with what was typed in the
//! last second ([`TYPE_AHEAD`]).
//!
//! Semantics: the field is a `combobox` announcing whether its list is
//! expanded, the list a `listbox`, each option an `option` announcing whether
//! it is selected and its place in the list.
//!
//! Invalidation: the list builds once with the field. Opening and closing
//! show and hide it; a highlight is a `PAINT` of one option's look, and a
//! choice shapes the field's label again.

use std::cell::{Cell, OnceCell, RefCell};
use std::num::NonZeroU32;
use std::rc::Rc;

use viso_ui::{
    Align, AnchorAlign, Anchoring, Axis, BoxRules, BoxStyle, BuildCx, Component, DispatchPhase,
    EventCx, FlexStyle, FontSize, Handle, ImeEvent, Inset, InteractionStyle, Justify, Key,
    KeyEvent, LeafStyle, Length, NodeId, PointerButtons, PointerPhase, Rgba, Role, ScrollStyle,
    SemanticState, Semantics, Shadow, Side, Size, Span, StateId, StateValue, TextOverflow,
    TextRequest, TextStyle, Vec2, primary_is_logo,
};

use super::glyphs::{GLYPH, check, chevron_down, chevrons_up_down, glyph_slot};
use super::menu::TYPE_AHEAD;
use crate::label;

/// How far a release lands from the press that opened the list before it
/// chooses: closer, it is the click that opened it.
const SWEEP_SLOP: f32 = 6.0;

/// The room between an option's check and its label, and the field's label
/// and its chevron.
const GAP: f32 = 6.0;

/// What runs when the user chooses another option.
type Change = Rc<RefCell<dyn FnMut(&mut EventCx<'_>, usize)>>;

/// One option of a [`Select`]. Construct one with [`select_option`], or from
/// its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectOption {
    label: String,
    enabled: bool,
}

/// An option labelled `label`.
pub fn select_option(label: impl Into<String>) -> SelectOption {
    SelectOption {
        label: label.into(),
        enabled: true,
    }
}

impl SelectOption {
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enable or disable it (default enabled): a disabled option shows dimmed
    /// and cannot be chosen.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

impl From<&str> for SelectOption {
    fn from(label: &str) -> Self {
        select_option(label)
    }
}

impl From<String> for SelectOption {
    fn from(label: String) -> Self {
        select_option(label)
    }
}

/// How a select looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectStyle {
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
    /// The ink of the placeholder and the chevron.
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
    /// Which platform's way the select works.
    pub kind: SelectKind,
}

/// Which platform's way a [`Select`] works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    /// The Apple systems' pop-up button: the list opens over the field, the
    /// chosen option checked and level with the field's label, and Up and
    /// Down open it.
    PopUp,
    /// The drop-down list elsewhere: the list opens below the field, the
    /// chosen option highlighted, and Up and Down choose with it closed.
    DropDown,
}

impl SelectKind {
    /// The way of the platform the app runs on.
    pub fn native() -> Self {
        if primary_is_logo() {
            SelectKind::PopUp
        } else {
            SelectKind::DropDown
        }
    }
}

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

impl Default for SelectStyle {
    fn default() -> Self {
        let border = viso_ui::Border {
            width: 1.0,
            color: rgba(0.0, 0.0, 0.0, 0.16),
        };
        SelectStyle {
            field: BoxStyle {
                border,
                ..BoxStyle::solid(rgba(1.0, 1.0, 1.0, 1.0)).with_radius(6.0)
            },
            field_hover: BoxStyle {
                border,
                ..BoxStyle::solid(rgba(0.96, 0.96, 0.965, 1.0)).with_radius(6.0)
            },
            field_padding: Inset {
                left: 10.0,
                right: 8.0,
                top: 5.0,
                bottom: 5.0,
            },
            min_width: 120.0,
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
            disabled: 0.4,
            max_height: 320.0,
            kind: SelectKind::native(),
        }
    }
}

/// A shared, mutable slot an application passes to a select's `handle` and
/// reads after `build` for its [`SelectHandle`].
pub type SelectHandleSlot = Rc<RefCell<Option<SelectHandle>>>;

/// A field choosing one of a list of options. Construct one with [`select`].
pub struct Select {
    options: Vec<SelectOption>,
    selected: Option<usize>,
    placeholder: Option<String>,
    label: Option<String>,
    size: Size,
    style: SelectStyle,
    on_change: Option<Change>,
    slot: Option<SelectHandleSlot>,
}

/// A select of `options`, none chosen yet.
pub fn select<I>(options: I) -> Select
where
    I: IntoIterator,
    I::Item: Into<SelectOption>,
{
    Select {
        options: options.into_iter().map(Into::into).collect(),
        selected: None,
        placeholder: None,
        label: None,
        size: FIT,
        style: SelectStyle::default(),
        on_change: None,
        slot: None,
    }
}

impl Select {
    /// Choose option `index` to start with.
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = Some(index);
        self
    }

    /// Show `text` in the field until an option is chosen.
    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = Some(text.into());
        self
    }

    /// Name the field for assistive technology.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the field's size within its parent (default: its content's).
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// Replace the whole [`SelectStyle`].
    pub fn style(mut self, style: SelectStyle) -> Self {
        self.style = style;
        self
    }

    /// Run `change` with the option's index when the user chooses another.
    pub fn on_change(mut self, change: impl FnMut(&mut EventCx<'_>, usize) + 'static) -> Self {
        self.on_change = Some(Rc::new(RefCell::new(change)));
        self
    }

    /// Fill `slot` with the select's [`SelectHandle`] when it builds.
    pub fn handle(mut self, slot: &SelectHandleSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }
}

impl Component for Select {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let handle = SelectHandle(build(cx, self));
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(handle);
        }
    }
}

const FIT: Size = Size {
    width: Length::Fit,
    height: Length::Fit,
};

/// The select state the handlers built before it reach.
type Shared = Rc<OnceCell<Rc<SelectState>>>;

fn with(shared: &Shared, f: impl FnOnce(&SelectState)) {
    if let Some(state) = shared.get() {
        f(state);
    }
}

/// The text request of `text` in `ink` at the style's size, on one line.
fn line(text: &str, ink: Rgba, style: &SelectStyle) -> TextRequest {
    TextRequest {
        text: text.to_owned(),
        font_size: FontSize::Fixed(style.font_size),
        color: ink,
        soft_wrap: false,
        locale: None,
        style: TextStyle {
            max_lines: NonZeroU32::new(1),
            overflow: TextOverflow::Ellipsis,
            ..TextStyle::default()
        },
        spans: Vec::new(),
    }
}

fn build(cx: &mut BuildCx<'_>, select: &Select) -> Rc<SelectState> {
    let style = &select.style;
    let n = select.options.len();
    let chosen = select.selected.filter(|&i| i < n);
    let shared: Shared = Rc::new(OnceCell::new());
    let chosen_cell = cx.state(StateValue::Int(chosen.map_or(-1, |i| i as i32)));
    let open_cell = cx.state(StateValue::Bool(false));
    let hover_cell = cx.state(StateValue::Bool(false));
    let shown = |i: Option<usize>| match i {
        Some(i) => line(&select.options[i].label, style.ink, style),
        None => line(
            select.placeholder.as_deref().unwrap_or(" "),
            style.quiet,
            style,
        ),
    };
    let mut parts = None;
    let wrapper = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: Align::Stretch,
            size: select.size,
            ..FlexStyle::default()
        },
        |cx| {
            let mut text = None;
            let field = cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    gap: GAP,
                    padding: style.field_padding,
                    align: Align::Center,
                    justify: Justify::Start,
                    size: FIT,
                    style: style.field,
                },
                |cx| {
                    cx.flex(
                        FlexStyle {
                            axis: Axis::Column,
                            align: Align::Start,
                            size: Size {
                                width: Length::Fill { weight: 1.0 },
                                height: Length::Fit,
                            },
                            ..FlexStyle::default()
                        },
                        |cx| {
                            let room = style.min_width
                                - style.field_padding.left
                                - style.field_padding.right
                                - GAP
                                - GLYPH;
                            cx.leaf(LeafStyle {
                                size: Size::fixed(room.max(0.0), 0.0),
                                style: BoxStyle::NONE,
                            });
                            let leaf = cx.leaf(LeafStyle {
                                size: FIT,
                                style: BoxStyle::NONE,
                            });
                            let request = shown(chosen);
                            cx.semantics(
                                leaf,
                                Semantics::role(Role::Label).with_label(request.text.clone()),
                            );
                            cx.text_request(leaf, request);
                            text = Some(leaf.id());
                            // Unseen, flat copies of every label hold the
                            // field as wide as the widest.
                            let labels = select.options.iter().map(|o| o.label.as_str());
                            for text in labels.chain(select.placeholder.as_deref()) {
                                let sizer = cx.leaf(LeafStyle {
                                    size: Size {
                                        width: Length::Fit,
                                        height: Length::Fixed(0.0),
                                    },
                                    style: BoxStyle::NONE,
                                });
                                cx.text_request(sizer, line(text, style.ink, style));
                                cx.structure(|s| {
                                    s.store.set_opacity(sizer.id(), 0.0);
                                    s.store.set_hittable(sizer.id(), false);
                                });
                            }
                        },
                    );
                    if style.kind == SelectKind::PopUp {
                        chevrons_up_down(cx, style.quiet);
                    } else {
                        chevron_down(cx, style.quiet);
                    }
                },
            );
            unhit_below(cx, field.id());
            let (panel, scroller, rows) = panel(cx, select, field, chosen, chosen_cell, &shared);
            parts = Some((field, text.expect("built"), panel, scroller, rows));
        },
    );
    let (field, text, panel, scroller, options) = parts.expect("built");
    cx.focusable(field, true);
    let mut semantics = Semantics::role(Role::ComboBox);
    semantics.label = select.label.clone();
    cx.semantics(field, semantics);
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
    let s = Rc::clone(&shared);
    cx.on_pointer(field, move |ev| {
        with(&s, |state| state.field_pointer(ev, hover_cell))
    });
    let s = Rc::clone(&shared);
    cx.on_key(wrapper, move |ev| with(&s, |state| state.key(ev)));
    let state = Rc::new(SelectState {
        wrapper: wrapper.id(),
        field: field.id(),
        panel: panel.id(),
        scroller,
        text,
        options,
        shown: (0..n)
            .map(|i| shown(Some(i)))
            .chain(std::iter::once(shown(None)))
            .collect(),
        chosen_cell,
        open_cell,
        chosen: Cell::new(chosen),
        lit: Cell::new(None),
        open: Cell::new(false),
        sweep: Cell::new(None),
        pressed: Cell::new(None),
        typed: RefCell::new(String::new()),
        typed_tick: Cell::new(0),
        over: style.kind == SelectKind::PopUp,
        on_change: select.on_change.clone(),
    });
    let _ = shared.set(Rc::clone(&state));
    state
}

/// Make everything below `node` let the pointer through to it.
pub(super) fn unhit_below(cx: &mut BuildCx<'_>, node: NodeId) {
    cx.structure(|s| {
        let mut stack: Vec<NodeId> = s.store.arena().children(node).collect();
        while let Some(id) = stack.pop() {
            s.store.set_hittable(id, false);
            stack.extend(s.store.arena().children(id));
        }
    });
}

/// Build the list's panel against `field`: returns it, its scroll viewport
/// and its options.
fn panel(
    cx: &mut BuildCx<'_>,
    select: &Select,
    field: Handle,
    chosen: Option<usize>,
    chosen_cell: StateId,
    shared: &Shared,
) -> (Handle, NodeId, Vec<Opt>) {
    let style = &select.style;
    let n = select.options.len();
    let mut options = Vec::with_capacity(n);
    let mut scroller = None;
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
            let viewport = cx.scroll(
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
                            for (i, option) in select.options.iter().enumerate() {
                                let (opt, node) = row(cx, option, style, chosen == Some(i));
                                let s = Rc::clone(shared);
                                cx.on_pointer(node, move |ev| {
                                    with(&s, |state| state.row_pointer(ev, i))
                                });
                                cx.bind_semantic_state(node, move |cx| SemanticState {
                                    selected: Some(matches!(
                                        cx.get(chosen_cell),
                                        Some(StateValue::Int(c)) if c == i as i32
                                    )),
                                    position: Some((i as u32 + 1, n as u32)),
                                    ..SemanticState::default()
                                });
                                options.push(opt);
                            }
                        },
                    );
                },
            );
            let room = style.max_height - style.padding.top - style.padding.bottom;
            cx.set_box_rules(
                viewport,
                BoxRules {
                    max_height: Span::dp(room.max(0.0)),
                    ..BoxRules::NONE
                },
            );
            scroller = Some(viewport.id());
        },
    );
    // Over the field, an option's label starts where the field's does.
    let offset = if style.kind == SelectKind::PopUp {
        style.field_padding.left - style.padding.left - style.item_padding.left - GLYPH - GAP
    } else {
        0.0
    };
    cx.anchored(
        panel,
        field,
        Anchoring {
            side: Side::Bottom,
            align: AnchorAlign::Start,
            match_anchor: true,
            offset,
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
    cx.focusable(panel, true);
    cx.semantics(panel, Semantics::role(Role::ListBox));
    (panel, scroller.expect("built"), options)
}

/// One option of the list.
struct Opt {
    node: NodeId,
    /// The cell its highlighted look reads.
    lit: StateId,
    /// Its check, over the field.
    mark: Option<NodeId>,
    enabled: bool,
    /// Its label, lowercased, which typing matches.
    key: String,
}

fn row(
    cx: &mut BuildCx<'_>,
    option: &SelectOption,
    style: &SelectStyle,
    chosen: bool,
) -> (Opt, Handle) {
    let lit = cx.state(StateValue::Bool(false));
    let mut mark = None;
    let node = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: GAP,
            padding: style.item_padding,
            align: Align::Center,
            justify: Justify::Start,
            size: Size {
                width: Length::Fill { weight: 1.0 },
                height: Length::Fit,
            },
            style: BoxStyle::NONE,
        },
        |cx| {
            if style.kind == SelectKind::PopUp {
                glyph_slot(cx, |cx| {
                    let glyph = check(cx, style.ink);
                    cx.set_hidden(glyph, !chosen);
                    mark = Some(glyph.id());
                });
            }
            label(option.label.clone())
                .font_size(style.font_size)
                .color(style.ink)
                .build_node(cx);
        },
    );
    unhit_below(cx, node.id());
    if !option.enabled {
        cx.structure(|s| {
            s.store.set_enabled(node.id(), false);
            s.store.set_hittable(node.id(), false);
            s.store.set_opacity(node.id(), style.disabled);
        });
    }
    cx.focusable(node, true);
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
        Semantics::role(Role::ListOption).with_label(option.label.clone()),
    );
    let opt = Opt {
        node: node.id(),
        lit,
        mark,
        enabled: option.enabled,
        key: option.label.to_lowercase(),
    };
    (opt, node)
}

/// Which way a move of the highlight or the choice goes.
#[derive(Debug, Clone, Copy)]
enum Step {
    By(isize),
    First,
    Last,
}

/// A select, shared by its handlers.
pub(crate) struct SelectState {
    wrapper: NodeId,
    field: NodeId,
    panel: NodeId,
    scroller: NodeId,
    /// The field's label.
    text: NodeId,
    options: Vec<Opt>,
    /// What the field's label shows for each option, then for none.
    shown: Vec<TextRequest>,
    chosen_cell: StateId,
    open_cell: StateId,
    chosen: Cell<Option<usize>>,
    lit: Cell<Option<usize>>,
    open: Cell<bool>,
    /// Where the press that opened the list landed, until it lifts.
    sweep: Cell<Option<Vec2>>,
    /// The option a press landed on.
    pressed: Cell<Option<usize>>,
    typed: RefCell<String>,
    typed_tick: Cell<u32>,
    /// Whether it is a [`SelectKind::PopUp`].
    over: bool,
    on_change: Option<Change>,
}

impl SelectState {
    fn open(&self, ev: &mut EventCx<'_>, sweep: Option<Vec2>) {
        if self.open.replace(true) {
            return;
        }
        ev.set_hidden(self.panel, false);
        ev.set(self.open_cell, StateValue::Bool(true));
        let over = self
            .chosen
            .get()
            .filter(|_| self.over)
            .map(|i| self.options[i].node);
        ev.anchor_over(self.panel, over);
        ev.request_focus(self.panel);
        self.sweep.set(sweep);
        let start = self
            .chosen
            .get()
            .filter(|&i| self.options[i].enabled)
            .or_else(|| self.end(Step::First));
        self.light(ev, start);
    }

    fn close(&self, ev: &mut EventCx<'_>) {
        if !self.open.replace(false) {
            return;
        }
        self.light(ev, None);
        ev.set_hidden(self.panel, true);
        ev.set(self.open_cell, StateValue::Bool(false));
        self.sweep.set(None);
        self.pressed.set(None);
        self.typed.borrow_mut().clear();
        ev.request_focus(self.field);
    }

    /// Highlight option `row`, or none; the focus follows it, so assistive
    /// technology reads the option.
    fn light(&self, ev: &mut EventCx<'_>, row: Option<usize>) {
        let was = self.lit.replace(row);
        if was == row {
            return;
        }
        if let Some(r) = was {
            ev.set(self.options[r].lit, StateValue::Bool(false));
        }
        if let Some(r) = row {
            let node = self.options[r].node;
            ev.set(self.options[r].lit, StateValue::Bool(true));
            ev.request_focus(node);
            ev.scroll_into_view(node);
        }
    }

    /// Make option `row`, or none, the chosen one; the user's choice of
    /// another tells `on_change`.
    fn choose(&self, ev: &mut EventCx<'_>, row: Option<usize>, by_user: bool) {
        let was = self.chosen.replace(row);
        if was == row {
            return;
        }
        for (r, on) in [(was, false), (row, true)] {
            if let Some(mark) = r.and_then(|r| self.options[r].mark) {
                ev.set_hidden(mark, !on);
            }
        }
        ev.set(
            self.chosen_cell,
            StateValue::Int(row.map_or(-1, |r| r as i32)),
        );
        let shown = row.unwrap_or(self.options.len());
        ev.set_text(self.text, self.shown[shown].clone());
        if by_user && let (Some(row), Some(change)) = (row, &self.on_change) {
            (change.borrow_mut())(ev, row);
        }
    }

    /// Choose option `row` and close the list.
    fn pick(&self, ev: &mut EventCx<'_>, row: usize) {
        if !self.options[row].enabled {
            return;
        }
        self.close(ev);
        self.choose(ev, Some(row), true);
    }

    /// The first or last enabled option.
    fn end(&self, step: Step) -> Option<usize> {
        let enabled = |&r: &usize| self.options[r].enabled;
        match step {
            Step::Last => (0..self.options.len()).rev().find(enabled),
            _ => (0..self.options.len()).find(enabled),
        }
    }

    /// The enabled option `step` goes to from `from`, stopping at the ends:
    /// the nearest enabled one at or past the place it lands, else before it.
    fn step(&self, from: Option<usize>, step: Step) -> Option<usize> {
        let n = self.options.len() as isize;
        let by = match step {
            Step::By(by) => by,
            _ => return self.end(step),
        };
        let Some(from) = from else {
            return self.end(if by > 0 { Step::First } else { Step::Last });
        };
        let to = (from as isize + by).clamp(0, n - 1);
        let enabled = |r: isize| self.options[r as usize].enabled;
        let ahead = if by > 0 {
            (to..n).find(|&r| enabled(r))
        } else {
            (0..=to).rev().find(|&r| enabled(r))
        };
        let back = || {
            if by > 0 {
                (from as isize..to).rev().find(|&r| enabled(r))
            } else {
                (to + 1..=from as isize).find(|&r| enabled(r))
            }
        };
        ahead.or_else(back).map(|r| r as usize)
    }

    /// How many options a page of the list holds.
    fn page(&self, ev: &EventCx<'_>) -> isize {
        let view = ev.rect_of(self.scroller).map_or(0.0, |r| r.h);
        let row = self
            .options
            .first()
            .and_then(|o| ev.rect_of(o.node))
            .map_or(0.0, |r| r.h);
        if row <= 0.0 {
            return 1;
        }
        ((view / row).floor() as isize - 1).max(1)
    }

    /// Whether the focus is on the open list: its panel or an option.
    fn in_list(&self, ev: &EventCx<'_>) -> bool {
        ev.focused()
            .is_some_and(|f| f == self.panel || self.options.iter().any(|o| o.node == f))
    }

    fn field_pointer(&self, ev: &mut EventCx<'_>, hover: StateId) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(p) = ev.pointer().copied() else {
            return;
        };
        match p.phase {
            PointerPhase::Enter | PointerPhase::Leave => {
                ev.set(hover, StateValue::Bool(p.phase == PointerPhase::Enter));
            }
            PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
                if self.open.get() {
                    self.close(ev);
                } else {
                    self.open(ev, Some(Vec2::new(p.x, p.y)));
                }
            }
            PointerPhase::Up => self.sweep.set(None),
            _ => {}
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
            PointerPhase::Up if p.buttons.contains(PointerButtons::PRIMARY) => {
                let here = self.pressed.take() == Some(row);
                let swept = self
                    .sweep
                    .take()
                    .is_some_and(|o| (o.x - p.x).hypot(o.y - p.y) > SWEEP_SLOP);
                if here || swept {
                    self.pick(ev, row);
                }
            }
            _ => {}
        }
    }

    /// A key-handler dispatch on the select: a key or text from the field
    /// or the open list, or a wake.
    fn key(&self, ev: &mut EventCx<'_>) {
        if let Some(token) = ev.woken() {
            if token == self.typed_tick.get() {
                self.typed.borrow_mut().clear();
            }
            return;
        }
        let open = self.open.get();
        let ours = if open {
            self.in_list(ev)
        } else {
            ev.focused() == Some(self.field)
        };
        if !ours {
            return;
        }
        if let Some(ImeEvent::Commit { text }) = ev.ime() {
            let text = text.clone();
            if self.type_ahead(ev, &text) {
                ev.stop_propagation();
            }
            return;
        }
        let Some(k) = ev.key().copied() else {
            return;
        };
        let taken = if open {
            self.open_key(ev, k)
        } else {
            self.closed_key(ev, k)
        };
        if taken {
            ev.stop_propagation();
        }
    }

    fn open_key(&self, ev: &mut EventCx<'_>, k: KeyEvent) -> bool {
        let alt = k.modifiers.alt;
        let step = match k.key {
            Key::Up | Key::Down if alt => None,
            Key::Up => Some(Step::By(-1)),
            Key::Down => Some(Step::By(1)),
            Key::Home => Some(Step::First),
            Key::End => Some(Step::Last),
            Key::PageUp => Some(Step::By(-self.page(ev))),
            Key::PageDown => Some(Step::By(self.page(ev))),
            _ => None,
        };
        let taken = step.is_some()
            || matches!(
                k.key,
                Key::Up | Key::Down | Key::Enter | Key::Space | Key::Escape | Key::Tab | Key::F4
            );
        if !taken || !k.pressed {
            return taken;
        }
        if let Some(step) = step {
            let to = self.step(self.lit.get(), step);
            if to.is_some() {
                self.light(ev, to);
            }
            return true;
        }
        match k.key {
            // Mid-word, a space is typed.
            Key::Space if !self.typed.borrow().is_empty() => {}
            Key::Enter | Key::Space | Key::Up | Key::Down | Key::F4 if !k.repeat => {
                match self.lit.get() {
                    Some(row) => self.pick(ev, row),
                    None => self.close(ev),
                }
            }
            Key::Escape | Key::Tab => self.close(ev),
            _ => {}
        }
        true
    }

    fn closed_key(&self, ev: &mut EventCx<'_>, k: KeyEvent) -> bool {
        let alt = k.modifiers.alt;
        let opens = match k.key {
            Key::Space => self.typed.borrow().is_empty(),
            Key::Up | Key::Down => alt || self.over,
            Key::F4 => !self.over,
            _ => false,
        };
        if opens {
            if k.pressed && !k.repeat {
                self.open(ev, None);
            }
            return true;
        }
        if self.over {
            return k.key == Key::Space;
        }
        let step = match k.key {
            Key::Up => Step::By(-1),
            Key::Down => Step::By(1),
            Key::Home => Step::First,
            Key::End => Step::Last,
            Key::Space => return true,
            _ => return false,
        };
        if k.pressed {
            let to = self.step(self.chosen.get(), step);
            if to.is_some() {
                self.choose(ev, to, true);
            }
        }
        true
    }

    /// Go to the next option whose label starts with what was typed in the
    /// last second: highlight it in the open list, choose it in the closed
    /// one. Whether the text was taken.
    fn type_ahead(&self, ev: &mut EventCx<'_>, text: &str) -> bool {
        let typed = {
            let mut typed = self.typed.borrow_mut();
            // A space starts nothing.
            if typed.is_empty() && text.trim().is_empty() {
                return false;
            }
            typed.extend(text.chars().flat_map(char::to_lowercase));
            typed.clone()
        };
        let tick = self.typed_tick.get().wrapping_add(1);
        self.typed_tick.set(tick);
        ev.wake_after(self.wrapper, TYPE_AHEAD, tick);
        let n = self.options.len();
        // One letter again and again steps through the options it starts.
        let mut chars = typed.chars();
        let first = chars.next();
        let repeated = first.is_some_and(|c| chars.all(|d| d == c));
        let prefix = if repeated {
            &typed[..first.map_or(0, char::len_utf8)]
        } else {
            &typed[..]
        };
        let at = if self.open.get() {
            self.lit.get()
        } else {
            self.chosen.get()
        };
        // A new search, or one letter again, starts past where it is; a
        // longer prefix may stay on it.
        let fresh = typed.len() == text.len();
        let from = match at {
            Some(r) if repeated || fresh => r + 1,
            Some(r) => r,
            None => 0,
        };
        let hit = (0..n)
            .map(|k| (from + k) % n.max(1))
            .find(|&r| self.options[r].enabled && self.options[r].key.starts_with(prefix));
        if hit.is_some() {
            if self.open.get() {
                self.light(ev, hit);
            } else {
                self.choose(ev, hit, true);
            }
        }
        true
    }
}

/// A handle an application captures to read and set a built select's choice
/// and open its list from its own handlers. Cheap to clone.
#[derive(Clone)]
pub struct SelectHandle(Rc<SelectState>);

impl SelectHandle {
    /// The chosen option's index.
    pub fn selected(&self) -> Option<usize> {
        self.0.chosen.get()
    }

    /// Choose option `index`, or none, without telling `on_change`.
    pub fn select(&self, ev: &mut EventCx<'_>, index: Option<usize>) {
        let index = index.filter(|&i| i < self.0.options.len());
        self.0.choose(ev, index, false);
    }

    /// Open the list.
    pub fn open(&self, ev: &mut EventCx<'_>) {
        self.0.open(ev, None);
    }

    /// Close the list unchanged.
    pub fn close(&self, ev: &mut EventCx<'_>) {
        self.0.close(ev);
    }

    /// Whether the list shows.
    pub fn is_open(&self) -> bool {
        self.0.open.get()
    }
}
