//! The editor widget: a [`Session`] drawn a visible line at a time.
//!
//! The lines are the rows of a virtual list keyed by their stable ids, so
//! only the lines on screen have nodes, a line typed above the screen moves
//! the rows under it rather than rebuilding them, and scrolling a
//! hundred-thousand-line file costs what scrolling a short one does. A row
//! is an absolute box as tall as its line's rows, holding three layers:
//! backgrounds (the current line, selections, matches, decorations), the
//! text, and what goes over it (carets, underlines).
//!
//! Text is laid on a grid of monospace cells. A row's runs of characters the
//! face draws a cell each are one text node a run, colored by token spans;
//! a wide or unusual grapheme is a node alone at its cell, so nothing drifts
//! off the grid however a fallback face measures it. Nodes are kept from
//! one edit to the next: a row diffs what it drew against what it should
//! draw and touches only what changed.
//!
//! The cell's width is measured, not guessed: a hidden run of the face's
//! characters reports its width as the extent of a scroll box around it.
//! Every handler edits the session and bumps one state cell; the structure
//! hook on that cell draws the rows on screen again.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use viso_ui::virtual_list::ItemKey;
use viso_ui::{
    AbsoluteStyle, Axis, BoxStyle, BuildCx, Component, DispatchPhase, EventCx, FlexStyle,
    FontFamily, FontSize, FontSlant, FontWeight, ImeEvent, InputKind, LeafStyle, Length,
    LengthTerms, NodeId, Notice, NoticeKinds, PointerButtons, PointerPhase, Rect, Rgba, Role,
    ScrollStyle, Semantics, Size, SpanStyle, StateId, StateValue, StateWriter, StructureCx,
    TextEntryRequest, TextRequest, TextSpan, TextStyle, Vec2, VirtualListStyle,
};

use crate::highlight::{Language, Token, TokenKind};
use crate::keymap::{Command, command_for};
use crate::layout::{self, ItemKind};
use crate::selection::Selection;
use crate::session::{Config, InlaySource, Session};
use crate::theme::Theme;

/// How often a caret blinks, on and off.
const BLINK: Duration = Duration::from_millis(530);

/// The characters the cell is measured over.
const PROBE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A code editor over `text`.
pub fn code_editor(text: impl Into<String>) -> CodeEditor {
    CodeEditor {
        text: text.into(),
        language: None,
        config: Config::default(),
        theme: Theme::default(),
        font_size: 13.0,
        line_height: 1.5,
        family: FontFamily::Monospace,
        size: Size::fill(),
        label: "Code editor".into(),
        slot: None,
        on_change: None,
    }
}

/// What the editor tells its owner when its text changes.
pub type OnChange = Box<dyn FnMut(&mut EventCx<'_>, &Session)>;

/// Where a built editor puts its handle.
pub type EditorSlot = Rc<RefCell<Option<EditorHandle>>>;

/// A code editor, to build.
pub struct CodeEditor {
    text: String,
    language: Option<Rc<dyn Language>>,
    config: Config,
    theme: Theme,
    font_size: f32,
    line_height: f32,
    family: FontFamily,
    size: Size,
    label: String,
    slot: Option<EditorSlot>,
    on_change: Option<RefCell<Option<OnChange>>>,
}

impl CodeEditor {
    /// Highlight as `language`.
    pub fn language(mut self, language: Rc<dyn Language>) -> Self {
        self.language = Some(language);
        self
    }

    /// Edit as `config` says.
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Look as `theme` says.
    pub fn theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }

    /// Text `size` dp tall.
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size.max(4.0);
        self
    }

    /// Lines `factor` times the font size apart.
    pub fn line_height(mut self, factor: f32) -> Self {
        self.line_height = factor.max(1.0);
        self
    }

    /// Draw in `family`, a monospace face.
    pub fn family(mut self, family: FontFamily) -> Self {
        self.family = family;
        self
    }

    /// Its size within its parent.
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// Its accessible name.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Put its handle in `slot` when built.
    pub fn handle(mut self, slot: &EditorSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }

    /// Call `f` after every edit.
    pub fn on_change(mut self, f: impl FnMut(&mut EventCx<'_>, &Session) + 'static) -> Self {
        self.on_change = Some(RefCell::new(Some(Box::new(f))));
        self
    }
}

/// The measures the editor draws by, in dp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Metrics {
    pub font_size: f32,
    pub line_height: f32,
    /// The width of a cell.
    pub cell: f32,
    /// Whether `cell` was measured, not estimated.
    pub measured: bool,
    /// The gutter's width.
    pub gutter: f32,
    /// The room between the gutter and the text.
    pub pad: f32,
}

impl Metrics {
    /// Where the text starts in a row.
    #[inline]
    pub fn text_x(&self) -> f32 {
        self.gutter + self.pad
    }

    /// The x of cell `col` in a row.
    #[inline]
    pub fn x(&self, col: u32) -> f32 {
        self.text_x() + col as f32 * self.cell
    }
}

/// The nodes the editor keeps.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Nodes {
    pub root: NodeId,
    pub list: NodeId,
    pub revision: StateId,
}

/// One drawn text segment.
#[derive(Debug, Clone, PartialEq)]
struct Seg {
    x: f32,
    y: f32,
    text: String,
    color: Rgba,
    spans: Vec<TextSpan>,
}

/// One drawn rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mark {
    x: f32,
    y: f32,
    /// `None` fills the row across.
    w: Option<f32>,
    h: f32,
    color: Rgba,
    radius: f32,
}

/// What a row's text was drawn from: equal again, it is drawn as it is.
#[derive(Debug, Clone, PartialEq, Default)]
struct TextKey {
    text: String,
    tokens: Vec<Token>,
    inlays: Vec<(usize, u32)>,
    preedit: Option<(usize, String)>,
    folded: bool,
    rows: usize,
    cell: f32,
    font_size: f32,
}

/// A row the editor built.
#[derive(Debug)]
struct RowView {
    /// The box as tall as the line's rows.
    row: NodeId,
    marks: NodeId,
    text: NodeId,
    over: NodeId,
    /// The line it shows, by id, and where it was last.
    line_id: u32,
    line: usize,
    rows: usize,
    key: TextKey,
    segs: Vec<(NodeId, Seg)>,
    mark_nodes: Vec<(NodeId, Mark)>,
    over_nodes: Vec<(NodeId, Mark)>,
}

/// A drag in progress.
#[derive(Debug, Clone)]
enum Drag {
    /// Extending by character from the selection's anchor.
    Chars,
    /// Extending by word from the word first pressed.
    Words(std::ops::Range<usize>),
    /// Extending by line from the line first pressed.
    Lines(std::ops::Range<usize>),
    /// A box from a line and cell.
    Box(usize, f32),
}

/// The editor's state, shared by its handlers and its hook.
pub(crate) struct Shared {
    pub session: RefCell<Session>,
    pub theme: Theme,
    pub family: FontFamily,
    pub metrics: Cell<Metrics>,
    pub nodes: Cell<Option<Nodes>>,
    rows: RefCell<Vec<RowView>>,
    /// The list viewport's size and offset, as last laid out.
    pub viewport: Cell<Vec2>,
    pub scroll: Cell<Vec2>,
    pub focused: Cell<bool>,
    /// Whether the carets show, between blinks.
    blink_on: Cell<bool>,
    /// The token of the blink wake in flight; older ones are stale.
    blink_token: Cell<u32>,
    drag: RefCell<Option<Drag>>,
    on_change: RefCell<Option<OnChange>>,
    /// The document version the owner was last told of.
    told: Cell<u64>,
    /// The line count when the rows were last drawn.
    drawn_lines: Cell<usize>,
    /// The editor's accessible name.
    label: String,
    /// What the revision cell was last set to.
    revision: Cell<i32>,
}

impl Shared {
    /// Asks for the rows to be drawn again.
    pub fn bump(&self, cx: &mut (impl StateWriter + ?Sized)) {
        if let Some(nodes) = self.nodes.get() {
            let n = self.revision.get().wrapping_add(1);
            self.revision.set(n);
            cx.set(nodes.revision, StateValue::Int(n));
        }
    }
}

/// What a row on screen draws.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct DrawnRow {
    pub line: usize,
    pub row: NodeId,
    /// Each text node, where it is and what it says.
    pub text: Vec<(NodeId, f32, f32, String)>,
    /// Each text node's color, a byte at a time.
    pub colors: Vec<Vec<Rgba>>,
    /// Each background: where, how wide (`-1` across), its color.
    pub marks: Vec<(NodeId, f32, f32, f32, Rgba)>,
    /// Each caret or underline.
    pub over: Vec<(NodeId, f32, f32, f32, Rgba)>,
}

/// A built editor, to read and drive from outside it.
#[derive(Clone)]
pub struct EditorHandle(pub(crate) Rc<Shared>);

impl EditorHandle {
    /// Reads the session.
    pub fn with<R>(&self, f: impl FnOnce(&Session) -> R) -> R {
        f(&self.0.session.borrow())
    }

    /// Changes the session, then draws what changed: from a handler, an
    /// update or a task's result.
    pub fn edit<R>(
        &self,
        cx: &mut (impl StateWriter + ?Sized),
        f: impl FnOnce(&mut Session) -> R,
    ) -> R {
        let out = f(&mut self.0.session.borrow_mut());
        self.0.bump(cx);
        out
    }

    /// The whole text, lines ending as the file's did.
    pub fn text(&self) -> String {
        self.0.session.borrow().doc().save_text()
    }

    /// Runs `command` as its key would.
    pub fn run(&self, cx: &mut EventCx<'_>, command: Command) {
        run(&self.0, cx, command);
    }

    /// The list viewport and the editor's root node.
    pub fn nodes(&self) -> Option<(NodeId, NodeId)> {
        self.0.nodes.get().map(|n| (n.root, n.list))
    }

    /// What each row on screen draws, top to bottom: for tests and tools.
    #[doc(hidden)]
    pub fn drawn(&self) -> Vec<DrawnRow> {
        let mut out: Vec<DrawnRow> = self
            .0
            .rows
            .borrow()
            .iter()
            .map(|r| DrawnRow {
                line: r.line,
                row: r.row,
                text: r
                    .segs
                    .iter()
                    .map(|(n, s)| (*n, s.x, s.y, s.text.clone()))
                    .collect(),
                colors: r
                    .segs
                    .iter()
                    .map(|(_, s)| {
                        let mut colors = vec![s.color; s.text.len()];
                        for span in &s.spans {
                            if let Some(c) = span.style.color {
                                for b in &mut colors[span.range.clone()] {
                                    *b = c;
                                }
                            }
                        }
                        colors
                    })
                    .collect(),
                marks: r
                    .mark_nodes
                    .iter()
                    .map(|(n, m)| (*n, m.x, m.y, m.w.unwrap_or(-1.0), m.color))
                    .collect(),
                over: r
                    .over_nodes
                    .iter()
                    .map(|(n, m)| (*n, m.x, m.y, m.w.unwrap_or(-1.0), m.color))
                    .collect(),
            })
            .collect();
        out.sort_by_key(|r| r.line);
        out
    }

    /// Where a row's text starts, in dp from the row's left edge.
    pub fn text_x(&self) -> f32 {
        self.0.metrics.get().text_x()
    }

    /// The width of a cell and the height of a line, in dp.
    pub fn cell_size(&self) -> (f32, f32) {
        let m = self.0.metrics.get();
        (m.cell, m.line_height)
    }
}

impl Component for CodeEditor {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let mut session = Session::new(&self.text);
        session.set_config(self.config);
        if let Some(language) = &self.language {
            session.set_language(Rc::clone(language));
        }
        let lh = (self.font_size * self.line_height).round();
        let metrics = Metrics {
            font_size: self.font_size,
            line_height: lh,
            cell: (self.font_size * 0.6).max(1.0),
            measured: false,
            gutter: 0.0,
            pad: (self.font_size * 0.5).round(),
        };
        let shared = Rc::new(Shared {
            session: RefCell::new(session),
            theme: self.theme.clone(),
            family: self.family.clone(),
            metrics: Cell::new(metrics),
            nodes: Cell::new(None),
            rows: RefCell::new(Vec::new()),
            viewport: Cell::new(Vec2::ZERO),
            scroll: Cell::new(Vec2::ZERO),
            focused: Cell::new(false),
            blink_on: Cell::new(true),
            blink_token: Cell::new(0),
            drag: RefCell::new(None),
            on_change: RefCell::new(self.on_change.as_ref().and_then(|c| c.borrow_mut().take())),
            told: Cell::new(0),
            drawn_lines: Cell::new(0),
            label: self.label.clone(),
            revision: Cell::new(0),
        });
        let revision = cx.state(StateValue::Int(0));
        let mut list = None;
        let mut probe = None;
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                size: self.size,
                style: BoxStyle::solid(self.theme.background),
                ..FlexStyle::default()
            },
            |cx| {
                // The cell's measure: a run of the face's characters in a
                // scroll box no room is given.
                cx.absolute(
                    AbsoluteStyle {
                        size: Size::fixed(0.0, 0.0),
                        ..AbsoluteStyle::default()
                    },
                    |cx| {
                        probe = Some(cx.scroll(
                            ScrollStyle {
                                axis: Axis::Row,
                                size: Size::fixed(1.0, 1.0),
                                bars: viso_ui::scroll_bar::ScrollBars::Hidden,
                                ..ScrollStyle::default()
                            },
                            |cx| {
                                let leaf = cx.leaf(LeafStyle {
                                    size: Size {
                                        width: Length::MaxContent,
                                        height: Length::Fixed(lh),
                                    },
                                    style: BoxStyle::NONE,
                                });
                                cx.text_request(
                                    leaf,
                                    text_request(
                                        &shared,
                                        PROBE.into(),
                                        shared.theme.text,
                                        Vec::new(),
                                    ),
                                );
                                cx.semantics(leaf, Semantics::role(Role::Label).with_hidden(true));
                            },
                        ));
                    },
                );
                let count = shared.session.borrow().shown_lines();
                let keys = Rc::clone(&shared);
                let rows = Rc::clone(&shared);
                list = Some(cx.virtual_list_keyed(
                    VirtualListStyle {
                        axis: Axis::Column,
                        across: !self.config.wrap,
                        size: Size::fill(),
                        overscan: 6,
                        estimated_row: lh,
                        ..VirtualListStyle::default()
                    },
                    count,
                    move |i| {
                        let s = keys.session.borrow();
                        let line = s.line_of_row(i);
                        ItemKey(u64::from(s.doc().line_id(line)))
                    },
                    move |i, cx| build_row(&rows, i, cx),
                ));
            },
        );
        let (Some(list_handle), Some(probe)) = (list, probe) else {
            return;
        };
        let list = list_handle.id();
        let root_id = root.id();
        shared.nodes.set(Some(Nodes {
            root: root_id,
            list,
            revision,
        }));
        cx.focusable(root, true);
        cx.count_clicks(root);
        cx.cursor(root, viso_ui::CursorIcon::Text);
        cx.semantics(
            root,
            Semantics::role(Role::TextField).with_label(self.label.clone()),
        );
        let keys = Rc::clone(&shared);
        cx.on_key(root, move |ev| on_key(&keys, ev));
        let pointer = Rc::clone(&shared);
        cx.on_pointer(root, move |ev| on_pointer(&pointer, ev));
        // The cell's width, once the probe is shaped.
        let measure = Rc::clone(&shared);
        cx.watch(
            probe,
            NoticeKinds::SCROLL,
            Box::new(move |notice, states| {
                if let Notice::Scroll { content, .. } = notice {
                    let cell = content.x / PROBE.len() as f32;
                    let mut m = measure.metrics.get();
                    if cell > 0.0 && (cell - m.cell).abs() > 0.001 || !m.measured && cell > 0.0 {
                        m.cell = cell;
                        m.measured = true;
                        measure.metrics.set(m);
                        measure.bump(states);
                    }
                }
            }),
        );
        // The viewport's size: what wraps and what a page is.
        let view = Rc::clone(&shared);
        cx.watch(
            list_handle,
            NoticeKinds::SCROLL,
            Box::new(move |notice, states| {
                if let Notice::Scroll {
                    offset, viewport, ..
                } = *notice
                {
                    view.scroll.set(offset);
                    if view.viewport.get() != viewport {
                        view.viewport.set(viewport);
                        view.bump(states);
                    }
                }
            }),
        );
        let hook = Rc::clone(&shared);
        cx.structure(|s| {
            s.store.add_structure_hook([revision], move |s, _| {
                if s.store.arena().is_live(list) {
                    update(&hook, s);
                }
            });
        });
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(EditorHandle(Rc::clone(&shared)));
        }
    }
}

/// The text request of `text` in the editor's face.
fn text_request(shared: &Shared, text: String, color: Rgba, spans: Vec<TextSpan>) -> TextRequest {
    let m = shared.metrics.get();
    TextRequest {
        text,
        font_size: FontSize::Fixed(m.font_size),
        color,
        soft_wrap: false,
        locale: None,
        style: TextStyle {
            family: Some(shared.family.clone()),
            line_height: Some(LengthTerms::dp(m.line_height)),
            ..TextStyle::default()
        },
        spans,
    }
}

/// Builds the row for list row `index` under the host the list gave it.
fn build_row(shared: &Rc<Shared>, index: usize, cx: &mut BuildCx<'_>) {
    let (line, line_id, rows) = {
        let s = shared.session.borrow();
        let line = s.line_of_row(index);
        (line, s.doc().line_id(line), s.row_count(line))
    };
    let lh = shared.metrics.get().line_height;
    let fill = AbsoluteStyle {
        size: Size::fill(),
        ..AbsoluteStyle::default()
    };
    let (mut marks, mut text, mut over) = (None, None, None);
    let row = cx.absolute(
        AbsoluteStyle {
            size: Size {
                width: Length::fill(),
                height: Length::Fixed(rows as f32 * lh),
            },
            ..AbsoluteStyle::default()
        },
        |cx| {
            marks = Some(cx.absolute(fill, |_| {}).id());
            text = Some(cx.absolute(fill, |_| {}).id());
            over = Some(cx.absolute(fill, |_| {}).id());
        },
    );
    let (Some(marks), Some(text), Some(over)) = (marks, text, over) else {
        return;
    };
    let mut view = RowView {
        row: row.id(),
        marks,
        text,
        over,
        line_id,
        line,
        rows,
        key: TextKey::default(),
        segs: Vec::new(),
        mark_nodes: Vec::new(),
        over_nodes: Vec::new(),
    };
    cx.structure(|s| {
        let mut session = shared.session.borrow_mut();
        draw_row(shared, &mut session, s, &mut view, line);
    });
    let mut rows = shared.rows.borrow_mut();
    rows.retain(|r| r.row != view.row);
    rows.push(view);
}

/// Draws what changed since the last pass.
fn update(shared: &Rc<Shared>, s: &mut StructureCx<'_>) {
    let Some(nodes) = shared.nodes.get() else {
        return;
    };
    let mut session = shared.session.borrow_mut();
    let m = shared.metrics.get();
    // What the viewport holds, in rows and cells.
    let viewport = shared.viewport.get();
    if viewport.y > 0.0 {
        session.set_page_rows(
            ((viewport.y / m.line_height).floor() as usize)
                .saturating_sub(1)
                .max(1),
        );
    }
    if viewport.x > 0.0 {
        let cells = ((viewport.x - m.text_x() - m.cell) / m.cell)
            .floor()
            .max(8.0) as u32;
        session.set_wrap_cells(cells);
    }
    let pending = session.take_pending();
    let lines = session.doc().line_count();
    if pending.rows || shared.drawn_lines.get() != lines {
        relist(&session, s, nodes.list, m);
    }
    // Across: as wide as the widest line, when lines do not wrap.
    if !session.config().wrap {
        let widest = session.widest_cells();
        let across = m.x(widest) + m.cell * 2.0 + m.pad;
        s.store.set_list_across(nodes.list, across);
    }
    // The rows on screen, each found by its line's id.
    let delta = lines.abs_diff(shared.drawn_lines.get()) + 64;
    let mut rows = shared.rows.borrow_mut();
    rows.retain(|r| s.store.arena().is_live(r.row));
    for view in rows.iter_mut() {
        let Some(line) = find_line(session.doc(), view.line_id, view.line, delta) else {
            continue;
        };
        if session.folds().is_hidden(line) {
            continue;
        }
        draw_row(shared, &mut session, s, view, line);
    }
    drop(rows);
    shared.drawn_lines.set(lines);
    if pending.reveal {
        reveal(shared, &session, s, nodes);
    }
    // Where the input method's window goes, and what a reader reads.
    let caret = caret_rect(shared, &session, s.store, nodes);
    s.store.set_text_entry(
        nodes.root,
        (!session.config().read_only).then_some(TextEntryRequest {
            caret,
            kind: InputKind::Text,
            multiline: true,
        }),
    );
    let head = session.selections().primary().head;
    let line = session.doc().line_of(head);
    s.store.set_semantics(
        nodes.root,
        Semantics::role(Role::TextField)
            .with_label(shared.label.clone())
            .with_value(session.doc().line(line).into_owned()),
    );
}

/// The line whose id is `id`, looked for outward from `near`, at most
/// `reach` lines away.
fn find_line(doc: &crate::Document, id: u32, near: usize, reach: usize) -> Option<usize> {
    let lines = doc.line_count();
    let near = near.min(lines - 1);
    if doc.line_id(near) == id {
        return Some(near);
    }
    for d in 1..=reach {
        if near + d < lines && doc.line_id(near + d) == id {
            return Some(near + d);
        }
        if d <= near && doc.line_id(near - d) == id {
            return Some(near - d);
        }
        if near + d >= lines && d > near {
            break;
        }
    }
    None
}

/// Lists the rows again: their count, and, when lines wrap, each one's
/// height.
fn relist(session: &Session, s: &mut StructureCx<'_>, list: NodeId, m: Metrics) {
    let count = session.shown_lines();
    let old = s.lists.get(list).map_or(0, |l| l.item_count());
    if session.config().wrap
        || s.lists.get(list).is_some_and(|l| {
            l.total_extent() > 0.0
                && old > 0
                && (l.total_extent() / old as f32 - m.line_height).abs() > 0.01
        })
    {
        let extents: Vec<f32> = (0..count)
            .map(|r| session.row_count(session.line_of_row(r)) as f32 * m.line_height)
            .collect();
        viso_ui::virtual_list::splice_items(s.lists, list, 0, old, &extents);
    } else {
        viso_ui::virtual_list::set_item_count(s.lists, list, count);
    }
}

/// Scrolls the primary caret into view.
fn reveal(shared: &Shared, session: &Session, s: &mut StructureCx<'_>, nodes: Nodes) {
    let m = shared.metrics.get();
    let head = session.selections().primary().head;
    let (line, row, col) = session.point_of(head);
    let index = session.row_of_line(line);
    let viewport = shared.viewport.get();
    let scroll = s.store.scroll(nodes.list);
    // Down the list: the caret's line.
    let _ = row;
    if let Some(state) = s.lists.get_mut(nodes.list) {
        state.show_index(index);
    }
    // Across, when lines do not wrap.
    if !session.config().wrap && viewport.x > 0.0 {
        let x = m.x(col);
        let left = scroll.x + m.text_x();
        let right = scroll.x + viewport.x - m.cell * 2.0;
        let to = if x < left {
            Some((x - m.text_x() - m.cell * 4.0).max(0.0))
        } else if x > right {
            Some(x - viewport.x + m.cell * 4.0)
        } else {
            None
        };
        if let Some(to) = to {
            s.store.request_scroll(
                nodes.list,
                viso_ui::ScrollRequest::To(Vec2 {
                    x: to.max(0.0),
                    y: scroll.y,
                }),
            );
        }
    }
}

/// The primary caret's line box, in the root's space.
fn caret_rect(
    shared: &Shared,
    session: &Session,
    store: &viso_ui::NodeStore,
    nodes: Nodes,
) -> Rect {
    let m = shared.metrics.get();
    let head = session.selections().primary().head;
    let (line, row, col) = session.point_of(head);
    let root = store.world(nodes.root);
    let rows = shared.rows.borrow();
    let at = rows
        .iter()
        .find(|r| r.line == line && store.arena().is_live(r.row))
        .map(|r| store.world(r.row));
    match at {
        Some(w) => Rect {
            x: w.x - root.x + m.x(col),
            y: w.y - root.y + row as f32 * m.line_height,
            w: 1.0,
            h: m.line_height,
        },
        None => Rect {
            x: m.text_x(),
            y: 0.0,
            w: 1.0,
            h: m.line_height,
        },
    }
}

/// Draws line `line` into `view`, touching only what changed.
fn draw_row(
    shared: &Shared,
    session: &mut Session,
    s: &mut StructureCx<'_>,
    view: &mut RowView,
    line: usize,
) {
    let m = shared.metrics.get();
    let theme = &shared.theme;
    let lh = m.line_height;
    let tab = session.rules().tab;
    let text = session.doc().line(line).into_owned();
    let start = session.doc().line_start(line);
    let inl = session.line_inlays(line);
    let rows = layout::rows(&text, &inl.spans, session.rules());
    view.line = line;
    view.line_id = session.doc().line_id(line);
    if view.rows != rows.len() {
        view.rows = rows.len();
        s.store.set_fixed_size(
            view.row,
            Size {
                width: Length::fill(),
                height: Length::Fixed(rows.len() as f32 * lh),
            },
        );
    }
    // The text.
    let mut tokens = Vec::new();
    session.tokens(line, &mut tokens);
    let preedit = session
        .preedit()
        .filter(|p| session.doc().line_of(p.at) == line);
    let key = TextKey {
        text,
        tokens,
        inlays: session
            .line_inlay_list(line)
            .iter()
            .map(|i| (i.at - start, i.text.len()))
            .map(|(a, b)| (a, b as u32))
            .collect(),
        preedit: preedit.map(|p| (p.at - start, p.text.clone())),
        folded: session.folds().at(line).is_some(),
        rows: rows.len(),
        cell: m.cell,
        font_size: m.font_size,
    };
    if key != view.key {
        let mut segs = Vec::new();
        let inlay_list = session.line_inlay_list(line).to_vec();
        for (r, span) in rows.iter().enumerate() {
            let y = r as f32 * lh;
            let mut run: Option<(usize, u32)> = None;
            let flush = |run: &mut Option<(usize, u32)>, end: usize, segs: &mut Vec<Seg>| {
                if let Some((a, col)) = run.take()
                    && end > a
                {
                    segs.push(Seg {
                        x: m.x(col),
                        y,
                        text: key.text[a..end].to_owned(),
                        color: theme.text,
                        spans: token_spans(theme, &key.tokens, a..end),
                    });
                }
            };
            for item in layout::items(&key.text, *span, &inl.spans, tab) {
                match item.kind {
                    ItemKind::Grid => {
                        if run.is_none() {
                            run = Some((item.start, item.col));
                        }
                    }
                    ItemKind::Tab => flush(&mut run, item.start, &mut segs),
                    ItemKind::Glyph | ItemKind::Control => {
                        flush(&mut run, item.start, &mut segs);
                        let g = &key.text[item.start..item.end];
                        let shown = match item.kind {
                            ItemKind::Control => control_picture(g),
                            _ => g.to_owned(),
                        };
                        let kind = crate::highlight::kind_at(&key.tokens, item.start);
                        let look = theme.token(kind);
                        segs.push(Seg {
                            x: m.x(item.col),
                            y,
                            text: shown,
                            color: if item.kind == ItemKind::Control {
                                theme.line_number
                            } else {
                                look.color
                            },
                            spans: Vec::new(),
                        });
                    }
                    ItemKind::Inlay(k) => {
                        flush(&mut run, item.start, &mut segs);
                        let (text, color, underline) = match inl.sources[k] {
                            InlaySource::Inlay(i) => {
                                let local = i - session_inlay_base(session, line);
                                (
                                    inlay_list
                                        .get(local)
                                        .map(|i| i.text.clone())
                                        .unwrap_or_default(),
                                    theme.inlay_text,
                                    false,
                                )
                            }
                            InlaySource::Preedit => (
                                preedit.map(|p| p.text.clone()).unwrap_or_default(),
                                theme.text,
                                true,
                            ),
                            InlaySource::Fold => (" \u{22ef} ".to_owned(), theme.fold_text, false),
                        };
                        let len = text.len();
                        let pad = if matches!(inl.sources[k], InlaySource::Inlay(_)) {
                            m.cell * 0.5
                        } else {
                            0.0
                        };
                        segs.push(Seg {
                            x: m.x(item.col) + pad,
                            y,
                            text,
                            color,
                            spans: if underline {
                                vec![TextSpan {
                                    range: 0..len,
                                    style: SpanStyle {
                                        underline: true,
                                        ..SpanStyle::default()
                                    },
                                }]
                            } else {
                                Vec::new()
                            },
                        });
                    }
                }
            }
            flush(&mut run, span.end, &mut segs);
        }
        sync_segs(shared, s, view.text, &mut view.segs, segs);
        view.key = key;
    }
    // Backgrounds and what goes over the text.
    let mut marks = Vec::new();
    let mut over = Vec::new();
    row_marks(
        shared, session, line, &rows, &inl.spans, &mut marks, &mut over,
    );
    sync_marks(s, view.marks, &mut view.mark_nodes, marks);
    sync_marks(s, view.over, &mut view.over_nodes, over);
}

/// Where line `line`'s inlays start in the session's list.
fn session_inlay_base(session: &Session, line: usize) -> usize {
    let start = session.doc().line_start(line);
    session.inlays().partition_point(|i| i.at < start)
}

/// The symbol a control character draws as.
fn control_picture(g: &str) -> String {
    let c = g.chars().next().unwrap_or(' ') as u32;
    let pic = match c {
        0..=0x1f => char::from_u32(0x2400 + c),
        0x7f => Some('\u{2421}'),
        _ => None,
    };
    pic.unwrap_or('\u{fffd}').to_string()
}

/// The token spans of bytes `range` of a line, in its own offsets: plain
/// text left to the text's own color.
fn token_spans(theme: &Theme, tokens: &[Token], range: std::ops::Range<usize>) -> Vec<TextSpan> {
    let mut out = Vec::new();
    for (r, kind) in crate::highlight::spans(tokens, range.end) {
        if kind == TokenKind::Text || r.end <= range.start {
            continue;
        }
        if r.start >= range.end {
            break;
        }
        let look = theme.token(kind);
        let (a, b) = (r.start.max(range.start), r.end.min(range.end));
        if b <= a {
            continue;
        }
        out.push(TextSpan {
            range: a - range.start..b - range.start,
            style: SpanStyle {
                color: Some(look.color),
                slant: look.italic.then_some(FontSlant::Italic),
                weight: look.bold.then_some(FontWeight::BOLD),
                ..SpanStyle::default()
            },
        });
    }
    out
}

/// The rectangles of line `line`: its backgrounds into `marks`, its carets
/// and underlines into `over`.
fn row_marks(
    shared: &Shared,
    session: &Session,
    line: usize,
    rows: &[layout::RowSpan],
    inlays: &[layout::InlaySpan],
    marks: &mut Vec<Mark>,
    over: &mut Vec<Mark>,
) {
    let m = shared.metrics.get();
    let theme = &shared.theme;
    let lh = m.line_height;
    let tab = session.rules().tab;
    let doc = session.doc();
    let range = doc.line_range(line);
    let text = doc.line(line);
    let focused = shared.focused.get();
    let sel = session.selections();
    let last_line = if let Some(f) = session.folds().at(line) {
        f.end
    } else {
        line
    };
    let line_end = doc.line_end(last_line);
    // The current line, behind all.
    let current = sel.list().iter().any(|s| {
        let l = doc.line_of(s.head);
        s.is_empty() && l >= line && l <= last_line
    });
    if current && focused {
        marks.push(Mark {
            x: 0.0,
            y: 0.0,
            w: None,
            h: rows.len() as f32 * lh,
            color: theme.current_line,
            radius: 0.0,
        });
    }
    let col = |at: usize, r: usize| layout::col_of(&text, rows[r], inlays, at - range.start, tab);
    // Selections.
    let color = if focused {
        theme.selection
    } else {
        theme.selection_unfocused
    };
    for s in sel.list() {
        if s.is_empty() || s.end() < range.start || s.start() > line_end {
            continue;
        }
        for (r, span) in rows.iter().enumerate() {
            let (a, b) = (range.start + span.start, range.start + span.end);
            let last_row = r + 1 == rows.len();
            if s.end() < a || s.start() > b || (s.end() == a && r > 0) {
                continue;
            }
            let x0 = if s.start() <= a {
                span.indent.min(if r == 0 { 0 } else { span.indent })
            } else {
                col(s.start(), r)
            };
            let mut x1 = if s.end() >= b {
                span.cells
            } else {
                col(s.end(), r)
            };
            // The break selected too: a cell past the end.
            if last_row && s.end() > range.end {
                x1 += 1;
            }
            if x1 > x0 {
                marks.push(Mark {
                    x: m.x(x0),
                    y: r as f32 * lh,
                    w: Some((x1 - x0) as f32 * m.cell),
                    h: lh,
                    color,
                    radius: 0.0,
                });
            }
        }
    }
    // Carets.
    if focused && shared.blink_on.get() {
        for s in sel.list() {
            if s.head < range.start || s.head > range.end {
                continue;
            }
            let at = s.head - range.start;
            let r = layout::row_of(rows, at);
            let mut c = layout::col_of(&text, rows[r], inlays, at, tab);
            let mut x = m.x(c);
            // Inside a composition, at its own caret.
            if let Some(p) = session.preedit()
                && p.at == s.head
            {
                c += crate::text::cells(&p.text[..p.caret], 0, tab);
                x = m.x(c);
            }
            over.push(Mark {
                x: x - 1.0,
                y: r as f32 * lh,
                w: Some(2.0),
                h: lh,
                color: theme.caret,
                radius: 0.0,
            });
        }
    }
}

/// Makes the text nodes under `parent` draw `want`, reusing the ones there.
fn sync_segs(
    shared: &Shared,
    s: &mut StructureCx<'_>,
    parent: NodeId,
    have: &mut Vec<(NodeId, Seg)>,
    want: Vec<Seg>,
) {
    let lh = shared.metrics.get().line_height;
    let mut want = want.into_iter();
    let mut i = 0;
    for seg in want.by_ref() {
        if i < have.len() {
            let (node, old) = &mut have[i];
            if old.text != seg.text || old.spans != seg.spans || old.color != seg.color {
                s.store.set_text(
                    *node,
                    text_request(shared, seg.text.clone(), seg.color, seg.spans.clone()),
                );
            }
            if old.x != seg.x || old.y != seg.y {
                s.store.set_translate(*node, Vec2 { x: seg.x, y: seg.y });
            }
            *old = seg;
        } else {
            let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, parent);
            let leaf = cx.leaf(LeafStyle {
                size: Size {
                    width: Length::MaxContent,
                    height: Length::Fixed(lh),
                },
                style: BoxStyle::NONE,
            });
            cx.text_request(
                leaf,
                text_request(shared, seg.text.clone(), seg.color, seg.spans.clone()),
            );
            let node = leaf.id();
            s.store.set_hittable(node, false);
            s.store.set_translate(node, Vec2 { x: seg.x, y: seg.y });
            have.push((node, seg));
        }
        i += 1;
    }
    let mut freed = Vec::new();
    for (node, _) in have.drain(i..) {
        s.store.free_tree(node, s.effects, &mut freed);
    }
}

/// Makes the rectangles under `parent` draw `want`, reusing the ones there.
fn sync_marks(
    s: &mut StructureCx<'_>,
    parent: NodeId,
    have: &mut Vec<(NodeId, Mark)>,
    want: Vec<Mark>,
) {
    let size = |m: &Mark| Size {
        width: m.w.map_or(Length::fill(), Length::Fixed),
        height: Length::Fixed(m.h),
    };
    let mut i = 0;
    for mark in want {
        if i < have.len() {
            let (node, old) = &mut have[i];
            if old.w != mark.w || old.h != mark.h {
                s.store.set_fixed_size(*node, size(&mark));
            }
            if old.x != mark.x || old.y != mark.y {
                s.store.set_translate(
                    *node,
                    Vec2 {
                        x: mark.x,
                        y: mark.y,
                    },
                );
            }
            if old.color != mark.color {
                s.store.set_fill(*node, mark.color);
            }
            *old = mark;
        } else {
            let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, parent);
            let leaf = cx.leaf(LeafStyle {
                size: size(&mark),
                style: BoxStyle {
                    fill: mark.color,
                    radius: mark.radius,
                    ..BoxStyle::NONE
                },
            });
            let node = leaf.id();
            s.store.set_hittable(node, false);
            s.store.set_translate(
                node,
                Vec2 {
                    x: mark.x,
                    y: mark.y,
                },
            );
            have.push((node, mark));
        }
        i += 1;
    }
    let mut freed = Vec::new();
    for (node, _) in have.drain(i..) {
        s.store.free_tree(node, s.effects, &mut freed);
    }
}

// ----- input -----

/// The editor's key handler: keys, text, the focus, the caret's blink.
fn on_key(shared: &Rc<Shared>, ev: &mut EventCx<'_>) {
    if let Some(change) = ev.focus_change() {
        let gained = change == viso_ui::FocusChange::Gained;
        shared.focused.set(gained);
        shared.blink_on.set(true);
        if gained {
            arm_blink(shared, ev);
        }
        {
            let mut s = shared.session.borrow_mut();
            s.pending_cursors();
        }
        shared.bump(ev);
        return;
    }
    if let Some(token) = ev.woken() {
        if token == shared.blink_token.get() && shared.focused.get() {
            shared.blink_on.set(!shared.blink_on.get());
            shared.session.borrow_mut().pending_cursors();
            shared.bump(ev);
            arm_blink(shared, ev);
        }
        return;
    }
    if let Some(ime) = ev.ime() {
        let ime = ime.clone();
        {
            let mut s = shared.session.borrow_mut();
            match ime {
                ImeEvent::Preedit { text, caret } => s.set_preedit(&text, caret),
                ImeEvent::Commit { text } => {
                    s.set_preedit("", 0);
                    // Keys that are commands come as keys.
                    if !text.chars().all(char::is_control) {
                        s.type_text(&text);
                    }
                }
                ImeEvent::Paste { text } => s.paste(&text),
            }
        }
        ev.stop_propagation();
        changed(shared, ev);
        return;
    }
    if let Some(data) = ev.pasted() {
        if let Some(text) = data.text.clone() {
            shared.session.borrow_mut().paste(&text);
            changed(shared, ev);
        }
        return;
    }
    let Some(key) = ev.key().copied() else {
        return;
    };
    if !key.pressed {
        return;
    }
    // A composition takes its keys.
    if shared.session.borrow().preedit().is_some() {
        ev.stop_propagation();
        return;
    }
    let Some(command) = command_for(&key, viso_ui::primary_is_logo()) else {
        return;
    };
    if run(shared, ev, command) {
        ev.stop_propagation();
    }
}

/// Runs `command`; returns whether it was the editor's to run.
fn run(shared: &Rc<Shared>, ev: &mut EventCx<'_>, command: Command) -> bool {
    let mut s = shared.session.borrow_mut();
    let taken = match command {
        Command::Move { motion, extend } => {
            s.move_cursors(motion, extend);
            true
        }
        Command::Backspace(reach) => {
            s.backspace(reach);
            true
        }
        Command::Delete(reach) => {
            s.delete(reach);
            true
        }
        Command::Newline => {
            s.newline();
            true
        }
        Command::OpenLine { above } => {
            s.open_line(above);
            true
        }
        Command::Tab => {
            s.tab();
            true
        }
        Command::Outdent => {
            s.outdent_lines();
            true
        }
        Command::IndentLines => {
            s.indent_lines();
            true
        }
        Command::ToggleComment => s.toggle_comment(),
        Command::MoveLines(dir) => {
            s.move_lines(dir);
            true
        }
        Command::DuplicateLines { above } => {
            s.duplicate_lines(above);
            true
        }
        Command::DeleteLines => {
            s.delete_lines();
            true
        }
        Command::SelectAll => {
            s.select_all();
            true
        }
        Command::SelectLines => {
            s.select_lines();
            true
        }
        Command::AddNextMatch => {
            s.add_next_match();
            true
        }
        Command::SelectAllMatches => s.select_all_matches(),
        Command::AddCursor(dir) => {
            s.add_cursor_vertically(dir);
            true
        }
        Command::Escape => s.collapse(),
        Command::Undo => {
            s.undo();
            true
        }
        Command::Redo => {
            s.redo();
            true
        }
        Command::Copy => {
            if let Some(text) = s.copy_text() {
                ev.copy_text(text);
            }
            true
        }
        Command::Cut => {
            if let Some(text) = s.cut() {
                ev.copy_text(text);
            }
            true
        }
        Command::Fold => {
            let line = s.doc().line_of(s.selections().primary().head);
            s.fold_line(line);
            true
        }
        Command::Unfold => {
            let line = s.doc().line_of(s.selections().primary().head);
            s.unfold(line);
            true
        }
        Command::FoldAll => {
            s.fold_all();
            true
        }
        Command::UnfoldAll => {
            s.unfold_all();
            true
        }
        Command::Find { .. } | Command::FindNext { .. } => false,
        Command::Complete | Command::GoToDefinition | Command::Hover => false,
    };
    drop(s);
    if taken {
        shared.blink_on.set(true);
        changed(shared, ev);
    }
    taken
}

/// After an edit or a move: draw it, keep the caret lit, tell the owner of
/// a new text.
fn changed(shared: &Rc<Shared>, ev: &mut EventCx<'_>) {
    shared.blink_on.set(true);
    arm_blink(shared, ev);
    shared.bump(ev);
    let version = shared.session.borrow().doc().version();
    if version != shared.told.get() {
        shared.told.set(version);
        let mut on_change = shared.on_change.borrow_mut();
        if let Some(f) = on_change.as_mut() {
            f(ev, &shared.session.borrow());
        }
    }
}

/// Starts the caret's next blink, dropping the one in flight.
fn arm_blink(shared: &Shared, ev: &mut EventCx<'_>) {
    let Some(nodes) = shared.nodes.get() else {
        return;
    };
    let token = shared.blink_token.get().wrapping_add(1);
    shared.blink_token.set(token);
    ev.wake_after(nodes.root, BLINK, token);
}

/// The position under window point `(x, y)`: the row under it, or the
/// nearest one on screen.
fn hit(shared: &Shared, ev: &EventCx<'_>, x: f32, y: f32) -> Option<(usize, usize, f32)> {
    let m = shared.metrics.get();
    let rows = shared.rows.borrow();
    let mut best: Option<(f32, usize, Rect)> = None;
    for r in rows.iter() {
        let Some(rect) = ev.rect_of(r.row) else {
            continue;
        };
        let d = if y < rect.y {
            rect.y - y
        } else if y >= rect.y + rect.h {
            y - (rect.y + rect.h) + 0.001
        } else {
            0.0
        };
        if best.as_ref().is_none_or(|b| d < b.0) {
            best = Some((d, r.line, rect));
        }
    }
    let (_, line, rect) = best?;
    let row = ((y - rect.y) / m.line_height).floor().max(0.0) as usize;
    let cells = (x - rect.x - m.text_x()) / m.cell;
    Some((line, row, cells))
}

/// The editor's pointer handler: placing, adding and dragging cursors.
fn on_pointer(shared: &Rc<Shared>, ev: &mut EventCx<'_>) {
    if ev.phase() == DispatchPhase::Capture {
        return;
    }
    let Some(p) = ev.pointer().copied() else {
        return;
    };
    let Some(nodes) = shared.nodes.get() else {
        return;
    };
    let apple = viso_ui::primary_is_logo();
    match p.phase {
        PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
            ev.request_focus(nodes.root);
            ev.capture_pointer(nodes.root);
            let Some((line, row, cells)) = hit(shared, ev, p.x, p.y) else {
                return;
            };
            let clicks = ev.clicks();
            let m = p.modifiers;
            let add = m.alt && !m.shift && (apple || !m.control);
            let boxed = m.alt && m.shift;
            let mut s = shared.session.borrow_mut();
            let at = s.position_at(line, row, cells);
            let drag = if boxed {
                s.select_box((line, cells), (line, cells));
                Drag::Box(line, cells)
            } else if add {
                s.toggle_cursor(at);
                Drag::Chars
            } else {
                match clicks {
                    2 => {
                        let word = s.word_at(at);
                        s.select_word(at, None);
                        Drag::Words(word)
                    }
                    n if n >= 3 => {
                        let start = s.doc().line_start(line);
                        let end = if line + 1 < s.doc().line_count() {
                            s.doc().line_start(line + 1)
                        } else {
                            s.doc().len()
                        };
                        s.select_line_at(at, None);
                        Drag::Lines(start..end)
                    }
                    _ => {
                        s.click(at, m.shift);
                        Drag::Chars
                    }
                }
            };
            drop(s);
            *shared.drag.borrow_mut() = Some(drag);
            changed(shared, ev);
        }
        PointerPhase::Move if p.buttons.contains(PointerButtons::PRIMARY) => {
            let Some(drag) = shared.drag.borrow().clone() else {
                return;
            };
            let Some((line, row, cells)) = hit(shared, ev, p.x, p.y) else {
                return;
            };
            let mut s = shared.session.borrow_mut();
            let at = s.position_at(line, row, cells);
            match drag {
                Drag::Chars => {
                    let mut primary = s.selections().primary();
                    if primary.head != at {
                        primary.head = at;
                        primary.goal = None;
                        let list: Vec<Selection> = s
                            .selections()
                            .list()
                            .iter()
                            .enumerate()
                            .map(|(i, x)| {
                                if i == s.selections().primary_index() {
                                    primary
                                } else {
                                    *x
                                }
                            })
                            .collect();
                        let index = s.selections().primary_index();
                        s.set_selections(crate::Selections::new(list, index));
                    }
                }
                Drag::Words(word) => s.select_word(at, Some(word)),
                Drag::Lines(lines) => s.select_line_at(at, Some(lines)),
                Drag::Box(from, x) => s.select_box((from, x), (line, cells)),
            }
            drop(s);
            // Past the viewport's edge, scroll toward the pointer.
            if let Some(rect) = ev.rect_of(nodes.list) {
                let lh = shared.metrics.get().line_height;
                let dy = if p.y < rect.y {
                    -lh
                } else if p.y > rect.y + rect.h {
                    lh
                } else {
                    0.0
                };
                if dy != 0.0 {
                    ev.scroll(
                        nodes.list,
                        viso_ui::ScrollRequest::By(Vec2 { x: 0.0, y: dy }),
                        viso_ui::ScrollBehavior::Instant,
                    );
                }
            }
            changed(shared, ev);
        }
        PointerPhase::Up => {
            *shared.drag.borrow_mut() = None;
            ev.release_pointer();
        }
        _ => {}
    }
}

impl Session {
    /// Asks for the cursors to be drawn again: the focus or the blink
    /// changed.
    pub(crate) fn pending_cursors(&mut self) {
        self.pending.cursors = true;
    }
}
