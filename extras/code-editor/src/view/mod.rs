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
//! characters is shaped in a scroll box, and each pass reads the box's
//! extent; the box reports a change of it, so a run shaped late is read.
//! Every handler edits the session and bumps one state cell; the structure
//! hook on that cell draws the rows on screen again.
//!
//! Each row carries its own piece of the gutter — the line's number, its
//! fold marker, its worst diagnostic — over its left end, moved by the list's
//! sideways scroll in the pass that scrolls, so the gutter stays put while
//! the text scrolls under it.

mod assist;
mod bar;
mod input;
mod row;

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use viso_ui::virtual_list::{ItemKey, watch_across};
use viso_ui::{
    AbsoluteStyle, Axis, BoxStyle, BuildCx, Component, EventCx, FlexStyle, FontFamily, FontSize,
    Handle, InputKind, LeafStyle, Length, LengthTerms, NodeId, Notice, NoticeKinds, Rect, Rgba,
    Role, ScrollStyle, Semantics, Size, StateId, StateValue, StateWriter, StructureCx,
    TextEntryRequest, TextRequest, TextSpan, TextStyle, Vec2, VirtualListStyle,
};

use crate::highlight::Language;
use crate::keymap::Command;
use crate::service::{Definition, LanguageService};
use crate::session::{Config, Session};
use crate::theme::Theme;

use assist::Assist;
pub use assist::OnDefinition;
use bar::Bar;
use row::{RowView, build_row, draw_row};

/// How often a caret blinks, on and off.
const BLINK: Duration = Duration::from_millis(530);

/// The characters the cell is measured over.
const PROBE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The characters past ASCII a run keeps on the grid, a few of each kind:
/// Latin letters, Greek, Cyrillic and box drawing. Whether the face draws
/// them a cell each is measured over them.
const GRID_PROBE: &str = "\u{e0}\u{e9}\u{f1}\u{f6}\u{df}\u{f8}\u{107}\u{161}\u{142}\u{3b1}\u{3bb}\u{3a9}\u{436}\u{44f}\u{429}\u{2500}\u{2502}\u{250c}\u{2518}\u{253c}";

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
        gutter: true,
        line_numbers: true,
        slot: None,
        on_change: None,
        setup: None,
        service: None,
        on_definition: None,
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
    gutter: bool,
    line_numbers: bool,
    slot: Option<EditorSlot>,
    on_change: Option<RefCell<Option<OnChange>>>,
    setup: Option<RefCell<Option<Setup>>>,
    service: Option<Rc<dyn LanguageService>>,
    on_definition: Option<RefCell<Option<OnDefinition>>>,
}

/// What sets a session up before it is first drawn.
type Setup = Box<dyn FnOnce(&mut Session)>;

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

    /// Show the gutter: line numbers, fold markers, diagnostics. On by
    /// default.
    pub fn gutter(mut self, show: bool) -> Self {
        self.gutter = show;
        self
    }

    /// Number the lines in the gutter. On by default.
    pub fn line_numbers(mut self, show: bool) -> Self {
        self.line_numbers = show;
        self
    }

    /// Put its handle in `slot` when built.
    pub fn handle(mut self, slot: &EditorSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }

    /// Ask `service` for completions, hovers, definitions, inlay hints and
    /// diagnostics, and tell it every edit.
    pub fn service(mut self, service: Rc<dyn LanguageService>) -> Self {
        self.service = Some(service);
        self
    }

    /// Call `f` when a name the service is asked about is declared in
    /// another document.
    pub fn on_definition(
        mut self,
        f: impl FnMut(&mut dyn StateWriter, &Definition) + 'static,
    ) -> Self {
        self.on_definition = Some(RefCell::new(Some(Box::new(f))));
        self
    }

    /// Set the session up with `f` before it is first drawn: its cursors,
    /// folds, inlays, decorations, a listener for its edits.
    pub fn setup(mut self, f: impl FnOnce(&mut Session) + 'static) -> Self {
        self.setup = Some(RefCell::new(Some(Box::new(f))));
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
    /// Whether the face draws the letters and box drawing past ASCII a
    /// cell each, so they run with ASCII; else each is drawn alone at its
    /// cell.
    pub grid_wide: bool,
    /// The gutter's width.
    pub gutter: f32,
    /// The room between the gutter and the text.
    pub pad: f32,
    /// The digits the line numbers take.
    pub digits: u32,
}

impl Metrics {
    /// Where the line numbers end, right-aligned there.
    #[inline]
    pub fn number_right(&self) -> f32 {
        (1 + self.digits) as f32 * self.cell
    }

    /// Where the fold markers' room starts.
    #[inline]
    pub fn fold_x(&self) -> f32 {
        self.number_right() + self.cell * 0.25
    }

    /// The gutter's width with `digits` digits of line numbers.
    pub fn gutter_for(&self, digits: u32) -> f32 {
        ((1 + digits) as f32 * self.cell + self.cell * 2.0).ceil()
    }

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
    /// The boxes a run of the face's ASCII, and of its letters past it,
    /// are measured in.
    pub probe: NodeId,
    pub grid_probe: NodeId,
    pub revision: StateId,
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
    /// Whether a pass has read both probes shaped.
    cell_read: Cell<bool>,
    pub scroll: Cell<Vec2>,
    /// How far the list is scrolled sideways: where the gutter sits.
    pub across: Cell<f32>,
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
    show_gutter: bool,
    line_numbers: bool,
    /// The bracket at the primary caret and its pair.
    brackets: RefCell<Option<(Range<usize>, Range<usize>)>>,
    /// The find bar.
    bar: RefCell<Option<Bar>>,
    /// Completions, hovers and what else a language service adds.
    assist: Assist,
    /// The primary caret's line box's top left, in the window.
    caret_window: Cell<Option<Vec2>>,
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
    /// Each caret or underline; a squiggle's width is negative.
    pub over: Vec<(NodeId, f32, f32, f32, Rgba)>,
    /// The gutter's left edge in the row, and its line number.
    pub gutter_x: f32,
    pub number: Option<(String, Rgba)>,
    /// The fold marker: `Some(true)` folded, `Some(false)` foldable.
    pub fold: Option<bool>,
    /// The gutter's marks: a diagnostic's.
    pub gutter_marks: Vec<(f32, f32, Rgba)>,
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
        input::run(&self.0, cx, command);
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
                    .map(|(n, m)| {
                        let w = m.w.unwrap_or(-1.0);
                        (*n, m.x, m.y, if m.squiggle { -w } else { w }, m.color)
                    })
                    .collect(),
                gutter_x: r.gutter_x,
                number: r.numbers.first().map(|(_, s)| (s.text.clone(), s.color)),
                fold: r.fold.map(|f| f.folded),
                gutter_marks: r
                    .gutter_marks
                    .iter()
                    .map(|(_, m)| (m.x, m.y, m.color))
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

    /// The gutter's width, in dp.
    pub fn gutter_width(&self) -> f32 {
        self.0.metrics.get().gutter
    }

    /// Opens the find bar, its replace row with `replace`, looking for the
    /// selection or the word at the caret.
    pub fn open_find(&self, cx: &mut EventCx<'_>, replace: bool) {
        bar::open(&self.0, cx, replace);
    }

    /// Closes the find bar and its search.
    pub fn close_find(&self, cx: &mut EventCx<'_>) {
        bar::close(&self.0, cx);
    }

    /// Whether the find bar shows, and whether with its replace row.
    pub fn find_bar(&self) -> Option<bool> {
        self.0.bar.borrow().as_ref().and_then(Bar::shown)
    }

    /// The find bar's nodes: its find field, its replace field, and its
    /// count: for tests and tools.
    #[doc(hidden)]
    pub fn find_bar_nodes(&self) -> Option<(NodeId, NodeId, NodeId)> {
        self.0
            .bar
            .borrow()
            .as_ref()
            .map(|b| (b.find, b.replace, b.count))
    }

    /// The completion list's labels, best first, and the chosen one's
    /// index, while it shows.
    pub fn completions(&self) -> Option<(Vec<String>, usize)> {
        self.0.assist.completion_labels()
    }

    /// What the hover box says, while it shows.
    pub fn hover_text(&self) -> Option<String> {
        self.0.assist.hover_text()
    }

    /// What the find bar's count says.
    pub fn find_count(&self) -> String {
        self.0
            .bar
            .borrow()
            .as_ref()
            .map_or_else(String::new, |b| b.count_text())
    }
}

impl Component for CodeEditor {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let mut session = Session::new(&self.text);
        session.set_config(self.config);
        if let Some(language) = &self.language {
            session.set_language(Rc::clone(language));
        }
        if let Some(setup) = self.setup.as_ref().and_then(|s| s.borrow_mut().take()) {
            setup(&mut session);
        }
        let assist = Assist::new(
            self.service.clone(),
            self.on_definition
                .as_ref()
                .and_then(|f| f.borrow_mut().take()),
        );
        if let Some(service) = &self.service {
            // The service and the edit log hear every edit, in order.
            let service = Rc::clone(service);
            let log = Rc::clone(&assist.log);
            session.listen(Box::new(move |event| {
                service.changed(event);
                log.borrow_mut()
                    .record(event.after.version(), event.changes);
            }));
        }
        let lh = (self.font_size * self.line_height).round();
        let mut metrics = Metrics {
            font_size: self.font_size,
            line_height: lh,
            // Until the probe is shaped.
            cell: (self.font_size * 0.6).max(1.0),
            grid_wide: true,
            gutter: 0.0,
            pad: (self.font_size * 0.5).round(),
            digits: 0,
        };
        if self.gutter {
            metrics.digits = digits_for(session.doc().line_count(), self.line_numbers);
            metrics.gutter = metrics.gutter_for(metrics.digits);
        }
        let shared = Rc::new(Shared {
            session: RefCell::new(session),
            theme: self.theme.clone(),
            family: self.family.clone(),
            metrics: Cell::new(metrics),
            nodes: Cell::new(None),
            rows: RefCell::new(Vec::new()),
            viewport: Cell::new(Vec2::ZERO),
            cell_read: Cell::new(false),
            scroll: Cell::new(Vec2::ZERO),
            across: Cell::new(0.0),
            focused: Cell::new(false),
            blink_on: Cell::new(true),
            blink_token: Cell::new(0),
            drag: RefCell::new(None),
            on_change: RefCell::new(self.on_change.as_ref().and_then(|c| c.borrow_mut().take())),
            told: Cell::new(0),
            drawn_lines: Cell::new(0),
            label: self.label.clone(),
            revision: Cell::new(0),
            show_gutter: self.gutter,
            line_numbers: self.line_numbers,
            brackets: RefCell::new(None),
            bar: RefCell::new(None),
            assist,
            caret_window: Cell::new(None),
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
                        probe = Some((
                            probe_box(&shared, cx, PROBE),
                            probe_box(&shared, cx, GRID_PROBE),
                        ));
                    },
                );
                let bar = bar::build(&shared, cx);
                *shared.bar.borrow_mut() = Some(bar);
                let count = shared.session.borrow().shown_lines();
                let keys = Rc::clone(&shared);
                let rows = Rc::clone(&shared);
                list = Some(cx.virtual_list_keyed(
                    VirtualListStyle {
                        axis: Axis::Column,
                        across: true,
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
                if let Some(list) = list {
                    assist::build_popups(&shared, cx, list);
                }
            },
        );
        let (Some(list_handle), Some((probe, grid_probe))) = (list, probe) else {
            return;
        };
        let list = list_handle.id();
        let root_id = root.id();
        shared.nodes.set(Some(Nodes {
            root: root_id,
            list,
            probe: probe.id(),
            grid_probe: grid_probe.id(),
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
        cx.on_key(root, move |ev| input::on_key(&keys, ev));
        let pointer = Rc::clone(&shared);
        cx.on_pointer(root, move |ev| input::on_pointer(&pointer, ev));
        // The probe's run shaped again, or shaped late: the next pass
        // measures the cell from it.
        for p in [probe, grid_probe] {
            let measure = Rc::clone(&shared);
            cx.watch(
                p,
                NoticeKinds::SCROLL,
                Box::new(move |notice, states| {
                    if let Notice::Scroll { .. } = notice {
                        measure.bump(states);
                    }
                }),
            );
        }
        // The viewport's size: what wraps and what a page is.
        let view = Rc::clone(&shared);
        // The first range shown comes after the first layout, which shaped
        // the probe: a pass then reads the cell, as the probe's own watch
        // reports only a later change.
        cx.watch(
            list_handle,
            NoticeKinds::SCROLL | NoticeKinds::VISIBLE_RANGE,
            Box::new(move |notice, states| match *notice {
                Notice::Scroll {
                    offset, viewport, ..
                } => {
                    view.scroll.set(offset);
                    if view.viewport.get() != viewport {
                        view.viewport.set(viewport);
                        view.bump(states);
                    }
                }
                Notice::VisibleRange { .. } if !view.cell_read.get() => view.bump(states),
                _ => {}
            }),
        );
        let hook = Rc::clone(&shared);
        let pin = Rc::clone(&shared);
        cx.structure(|s| {
            // The gutter keeps to the viewport's left edge as the text
            // scrolls sideways under it, moved in the pass that scrolls.
            watch_across(
                s.lists,
                list,
                Box::new(move |s, view, _| row::pin_gutters(&pin, s, view.offset)),
            );
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

/// A run of `text` in the editor's face, in a scroll box whose extent is
/// the run's width.
fn probe_box(shared: &Shared, cx: &mut BuildCx<'_>, text: &str) -> Handle {
    let lh = shared.metrics.get().line_height;
    cx.scroll(
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
                text_request(shared, text.into(), shared.theme.text, Vec::new()),
            );
            cx.semantics(leaf, Semantics::role(Role::Label).with_hidden(true));
        },
    )
}

/// The digits line numbers for `lines` lines take, at least two; none
/// without them.
fn digits_for(lines: usize, numbers: bool) -> u32 {
    if !numbers {
        return 0;
    }
    (lines.max(1).ilog10() + 1).max(2)
}

/// The text request of `text` in the editor's face.
pub(crate) fn text_request(
    shared: &Shared,
    text: String,
    color: Rgba,
    spans: Vec<TextSpan>,
) -> TextRequest {
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

/// Draws what changed since the last pass.
fn update(shared: &Rc<Shared>, s: &mut StructureCx<'_>) {
    let Some(nodes) = shared.nodes.get() else {
        return;
    };
    let mut session = shared.session.borrow_mut();
    let mut m = shared.metrics.get();
    // The cell as wide as the face draws a character. The probe is read
    // here, where the pass draws, not only when it reports a change: a watch
    // takes the extent it first sees as its start and says nothing of it.
    let cell = s.store.content(nodes.probe).x / PROBE.len() as f32;
    let grid = s.store.content(nodes.grid_probe).x / GRID_PROBE.chars().count() as f32;
    if cell > 0.0 {
        // A face lacking them draws them in a fallback's widths; until they
        // are shaped they are taken to keep to the grid.
        let grid_wide = grid <= 0.0 || (grid - cell).abs() <= cell * 0.01;
        shared.cell_read.set(grid > 0.0);
        if (cell - m.cell).abs() > 0.001 || grid_wide != m.grid_wide {
            m.cell = cell;
            m.grid_wide = grid_wide;
            shared.metrics.set(m);
        }
    }
    // The gutter as wide as the last line's number.
    if shared.show_gutter {
        let digits = digits_for(session.doc().line_count(), shared.line_numbers);
        let gutter = m.gutter_for(digits);
        if digits != m.digits || gutter != m.gutter {
            m.digits = digits;
            m.gutter = gutter;
            shared.metrics.set(m);
        }
    }
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
    let across = if session.config().wrap {
        0.0
    } else {
        m.x(session.widest_cells()) + m.cell * 2.0 + m.pad
    };
    s.store.set_list_across(nodes.list, across);
    if pending.cursors || pending.text {
        *shared.brackets.borrow_mut() = session.matching_bracket();
    }
    // The rows on screen, each found by its line's id.
    let delta = lines.abs_diff(shared.drawn_lines.get()) + 64;
    let mut rows = shared.rows.borrow_mut();
    // A row the list parked is built afresh when it comes back.
    rows.retain(|r| {
        let arena = s.store.arena();
        arena.is_live(r.row)
            && arena
                .parent(r.row)
                .and_then(|host| arena.parent(host))
                .is_some()
    });
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
    if let Some(bar) = shared.bar.borrow().as_ref() {
        bar.update(shared, &session, s);
    }
    // Where the input method's window goes, and what a reader reads.
    let caret = caret_rect(shared, &session, s.store, nodes);
    let root = s.store.world(nodes.root);
    shared.caret_window.set(Some(Vec2 {
        x: root.x + caret.x,
        y: root.y + caret.y,
    }));
    assist::update(shared, &mut session, s, nodes);
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
