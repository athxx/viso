//! The [`Document`] widget: a Markdown or HTML document drawn as retained
//! nodes.
//!
//! [`markdown`] reads CommonMark with the GitHub extensions: headings,
//! paragraphs, emphasis, strong, strikethrough, inline code, links, block
//! quotes and GitHub alerts, ordered, bullet and task lists, fenced and
//! indented code, tables with their column alignment, rules, footnotes,
//! pictures and the inline HTML the GitHub sanitizer keeps (`<br>`, `<sub>`,
//! `<sup>`, `<kbd>`, `<mark>` and the styles); block HTML shows nothing.
//! [`html`] reads a safe subset of HTML into the same blocks: `p`, `h1`–`h6`,
//! the inline styles, `a`, `ul`/`ol`/`li`, `blockquote`, `pre`/`code`,
//! `br`, `hr`, `img` and tables; it runs no script and loads nothing, and
//! the tags it does not know show their text. Each top-level block is one
//! node: a paragraph a wrapping label, a list a column of rows, a table a
//! grid that scrolls sideways, a code block a monospace label in a box that
//! scrolls sideways.
//!
//! The source is a [`DocumentText`], which the app can write as the document
//! shows it — a reply streaming in, say. A write re-parses only from the
//! last block when it appends (and, in Markdown, there are no link reference
//! definitions), and either way rebuilds only the blocks whose source
//! changed, so a stream of appends rebuilds just its last block each time. A
//! link's activation reaches [`on_link`](Document::on_link), else the
//! ancestors; a task box toggles when [`on_task`](Document::on_task) is set,
//! writing its new state into the source.
//!
//! ```
//! use viso_widgets::{DocumentText, markdown};
//!
//! let reply = DocumentText::new("# Hello\n\nSome *text* with a [link](https://viso.dev).");
//! let doc = markdown(&reply)
//!     .selectable()
//!     .on_link(|_ev, url| {
//!         let _ = url;
//!     });
//! ```
//!
//! It needs a [`BuildCx::with_reactive`] cx.

mod blocks;
mod build;
mod html;
mod markdown;

use std::cell::{Cell, Ref, RefCell};
use std::ops::Range;
use std::rc::Rc;

use viso_ui::{
    Asset, BuildCx, CheckMark, CheckShape, Component, DirtyClass, EventCx, NodeId, NodeStore, Rgba,
    SemanticState, Size, StateId, StateValue, StateWriter, StructureCx,
};

use blocks::{Parsed, Task, Top};
use build::{Env, Toggle};

/// The look of a [`Document`]: its body's size and ink, and the
/// colors of its links, code, rules and tables.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DocumentStyle {
    /// The body's font size in logical pixels; headings scale from it.
    pub font_size: f32,
    /// Each line's height, of the font size.
    pub line_height: f32,
    /// The room between blocks, of the font size.
    pub block_gap: f32,
    /// The body's ink.
    pub color: Rgba,
    /// The ink of quotes, footnote marks and missing pictures.
    pub muted: Rgba,
    pub link: Rgba,
    /// The fill behind inline code and code blocks.
    pub code_background: Rgba,
    /// Inline code's and code blocks' size, of the font size.
    pub code_scale: f32,
    /// Rules, the bar of a quote and the lines of a table.
    pub rule: Rgba,
    pub table_header: Rgba,
    pub table_cell: Rgba,
    /// The document's own size within its parent: the parent's width and its
    /// content's height by default.
    pub size: Size,
}

impl Default for DocumentStyle {
    fn default() -> Self {
        DocumentStyle {
            font_size: 14.0,
            line_height: 1.5,
            block_gap: 0.75,
            color: Rgba {
                r: 0.12,
                g: 0.14,
                b: 0.16,
                a: 1.0,
            },
            muted: Rgba {
                r: 0.35,
                g: 0.38,
                b: 0.42,
                a: 1.0,
            },
            link: Rgba {
                r: 0.04,
                g: 0.41,
                b: 0.85,
                a: 1.0,
            },
            code_background: Rgba {
                r: 0.5,
                g: 0.53,
                b: 0.56,
                a: 0.12,
            },
            code_scale: 0.875,
            rule: Rgba {
                r: 0.82,
                g: 0.84,
                b: 0.86,
                a: 1.0,
            },
            table_header: Rgba {
                r: 0.96,
                g: 0.97,
                b: 0.98,
                a: 1.0,
            },
            table_cell: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            size: Size {
                width: viso_ui::Length::Fill { weight: 1.0 },
                height: viso_ui::Length::Fit,
            },
        }
    }
}

/// What finds a picture's pixels from where it is.
pub(crate) type Pictures = dyn Fn(&str) -> Option<Asset>;

type LinkHandler = Rc<RefCell<dyn FnMut(&mut EventCx<'_>, &str)>>;
type TaskHandler = Rc<RefCell<dyn FnMut(&mut EventCx<'_>, usize, bool)>>;

/// The source of a [`Document`], shared by the documents showing
/// it and whatever writes it. Cloning it shares it.
#[derive(Clone)]
pub struct DocumentText(Rc<Source>);

struct Source {
    text: RefCell<String>,
    /// The cells the documents showing it re-parse on.
    cells: RefCell<Vec<StateId>>,
    revision: Cell<i32>,
}

impl DocumentText {
    pub fn new(text: impl Into<String>) -> Self {
        DocumentText(Rc::new(Source {
            text: RefCell::new(text.into()),
            cells: RefCell::new(Vec::new()),
            revision: Cell::new(0),
        }))
    }

    /// The source as it is.
    pub fn text(&self) -> Ref<'_, str> {
        Ref::map(self.0.text.borrow(), String::as_str)
    }

    /// Replace the source with `text`.
    pub fn set(&self, cx: &mut impl StateWriter, text: impl Into<String>) {
        *self.0.text.borrow_mut() = text.into();
        self.changed(cx);
    }

    /// Append `more` to the source, as a stream arriving does.
    pub fn push_str(&self, cx: &mut impl StateWriter, more: &str) {
        if more.is_empty() {
            return;
        }
        self.0.text.borrow_mut().push_str(more);
        self.changed(cx);
    }

    fn changed(&self, cx: &mut impl StateWriter) {
        let revision = self.0.revision.get().wrapping_add(1);
        self.0.revision.set(revision);
        self.0
            .cells
            .borrow_mut()
            .retain(|&cell| cx.set(cell, StateValue::Int(revision)));
    }
}

impl From<&str> for DocumentText {
    fn from(text: &str) -> Self {
        DocumentText::new(text)
    }
}

impl From<String> for DocumentText {
    fn from(text: String) -> Self {
        DocumentText::new(text)
    }
}

impl From<&DocumentText> for DocumentText {
    fn from(text: &DocumentText) -> Self {
        text.clone()
    }
}

/// How a document's source reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Markdown,
    Html,
}

impl Format {
    /// Parse `src`, which starts at byte `base` of the document.
    fn parse(self, src: &str, base: usize, look: &DocumentStyle) -> Parsed {
        match self {
            Format::Markdown => markdown::parse(src, base, look),
            Format::Html => html::parse(src, base, look),
        }
    }

    /// Where a parse that leaves the blocks before the one starting at
    /// `at` of `src` as they are starts; `None` when a definition might
    /// reach them from what follows.
    fn resume(self, src: &str, at: usize) -> Option<usize> {
        match self {
            Format::Markdown => {
                let from = line_start(src, at);
                (!src[from..].contains("]:")).then_some(from)
            }
            Format::Html => Some(at),
        }
    }
}

/// A Markdown or HTML document; see the [module docs](self).
pub struct Document {
    format: Format,
    source: DocumentText,
    style: DocumentStyle,
    selectable: bool,
    on_link: Option<LinkHandler>,
    on_task: Option<TaskHandler>,
    pictures: Option<Rc<Pictures>>,
}

/// Construct a [`Document`] of Markdown `source`: a text, or a
/// [`DocumentText`] to write as it shows.
pub fn markdown(source: impl Into<DocumentText>) -> Document {
    Document::new(Format::Markdown, source.into())
}

/// Construct a [`Document`] of HTML `source`: a text, or a [`DocumentText`]
/// to write as it shows.
pub fn html(source: impl Into<DocumentText>) -> Document {
    Document::new(Format::Html, source.into())
}

impl Document {
    fn new(format: Format, source: DocumentText) -> Self {
        Document {
            format,
            source,
            style: DocumentStyle::default(),
            selectable: false,
            on_link: None,
            on_task: None,
            pictures: None,
        }
    }

    /// Replace the whole [`DocumentStyle`].
    pub fn style(mut self, style: DocumentStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the body's font size in logical pixels.
    pub fn font_size(mut self, font_size: f32) -> Self {
        self.style.font_size = font_size;
        self
    }

    /// Set the body's ink.
    pub fn color(mut self, color: Rgba) -> Self {
        self.style.color = color;
        self
    }

    /// Set the document's own size within its parent.
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Let the text of each block be selected and copied, as a selectable
    /// label's is.
    pub fn selectable(mut self) -> Self {
        self.selectable = true;
        self
    }

    /// Run `handler` with where a link leads when a click or Enter activates
    /// one, in place of the system opening it. Without one the activation
    /// reaches the document's ancestors, and the link opens unless one of
    /// them keeps it ([`EventCx::prevent_default`]).
    pub fn on_link(mut self, handler: impl FnMut(&mut EventCx<'_>, &str) + 'static) -> Self {
        self.on_link = Some(Rc::new(RefCell::new(handler)));
        self
    }

    /// Let the task boxes toggle: a click, Enter or Space flips one, writes
    /// its new state into the source, and runs `handler` with its place among
    /// the document's tasks and whether it is now checked. Without one the
    /// boxes show their state and take nothing.
    pub fn on_task(mut self, handler: impl FnMut(&mut EventCx<'_>, usize, bool) + 'static) -> Self {
        self.on_task = Some(Rc::new(RefCell::new(handler)));
        self
    }

    /// Find each picture's pixels with `resolve`, given where the picture
    /// is; a picture it finds none for shows its text alternative.
    pub fn pictures(mut self, resolve: impl Fn(&str) -> Option<Asset> + 'static) -> Self {
        self.pictures = Some(Rc::new(resolve));
        self
    }
}

/// One shown document: what it builds with and the blocks it shows.
struct Shown {
    format: Format,
    source: DocumentText,
    style: DocumentStyle,
    selectable: bool,
    pictures: Option<Rc<Pictures>>,
    on_task: Option<TaskHandler>,
    doc: RefCell<Doc>,
}

/// The blocks a document shows and the source they show.
#[derive(Default)]
struct Doc {
    root: Option<NodeId>,
    seen: String,
    defs: String,
    hosts: Vec<Host>,
    /// The task boxes toggled since the hook last ran.
    toggled: Vec<(NodeId, bool)>,
}

/// A top-level block's node, the source it shows, and its task boxes.
struct Host {
    node: NodeId,
    range: Range<usize>,
    tasks: Vec<(NodeId, Task)>,
}

impl Host {
    fn new(node: NodeId, top: &Top, boxes: Vec<NodeId>) -> Self {
        let mut host = Host {
            node,
            range: 0..0,
            tasks: boxes
                .into_iter()
                .map(|n| {
                    (
                        n,
                        Task {
                            checked: false,
                            at: 0,
                        },
                    )
                })
                .collect(),
        };
        host.moved(top);
        host
    }

    /// The same block, now at `top`'s place in the source.
    fn moved(&mut self, top: &Top) {
        self.range = top.range.clone();
        let mut tasks = Vec::new();
        top.block.tasks(&mut tasks);
        for ((_, task), at) in self.tasks.iter_mut().zip(tasks) {
            *task = at;
        }
    }
}

impl Component for Document {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let shown = Rc::new(Shown {
            format: self.format,
            source: self.source.clone(),
            style: self.style,
            selectable: self.selectable,
            pictures: self.pictures.clone(),
            on_task: self.on_task.clone(),
            doc: RefCell::new(Doc::default()),
        });
        let toggle: Option<Toggle> = self.on_task.is_some().then(|| {
            let shown = Rc::clone(&shown);
            Rc::new(move |ev: &mut EventCx<'_>, node: NodeId| shown.toggle(ev, node)) as Toggle
        });
        let cell = cx.state(StateValue::Int(self.source.0.revision.get()));
        self.source.0.cells.borrow_mut().push(cell);

        let source = self.source.0.text.borrow();
        let parsed = self.format.parse(&source, 0, &self.style);
        let mut hosts = Vec::with_capacity(parsed.blocks.len());
        let root = {
            let mut env = shown.env(toggle.as_ref());
            let style = viso_ui::FlexStyle {
                size: self.style.size,
                ..doc_column(&self.style)
            };
            cx.flex(style, |cx| {
                for top in &parsed.blocks {
                    let node = env.block(cx, &top.block);
                    hosts.push(Host::new(node, top, std::mem::take(&mut env.tasks)));
                }
            })
        };
        {
            let mut doc = shown.doc.borrow_mut();
            doc.root = Some(root.id());
            doc.seen.push_str(&source);
            doc.defs = parsed.defs;
            doc.hosts = hosts;
        }
        drop(source);

        if let Some(on_link) = self.on_link.clone() {
            cx.on_key(root, move |ev| {
                if let Some(target) = ev.link() {
                    ev.stop_propagation();
                    ev.prevent_default();
                    (on_link.borrow_mut())(ev, target);
                }
            });
        }
        cx.structure(|s| {
            s.store.add_structure_hook([cell], move |cx, _| {
                shown.update(cx, toggle.as_ref());
            });
        });
    }
}

/// The document's column: its blocks one under another, as wide as it.
fn doc_column(style: &DocumentStyle) -> viso_ui::FlexStyle {
    viso_ui::FlexStyle {
        axis: viso_ui::Axis::Column,
        gap: style.font_size * style.block_gap,
        align: viso_ui::Align::Stretch,
        ..viso_ui::FlexStyle::default()
    }
}

impl Shown {
    fn env<'a>(&'a self, toggle: Option<&'a Toggle>) -> Env<'a> {
        Env {
            look: &self.style,
            selectable: self.selectable,
            pictures: self.pictures.as_deref(),
            toggle,
            tasks: Vec::new(),
            color: self.style.color,
            depth: 0,
        }
    }

    /// Flip task box `node`, in the document and in its source.
    fn toggle(&self, ev: &mut EventCx<'_>, node: NodeId) {
        let (place, checked) = {
            let mut doc = self.doc.borrow_mut();
            let doc = &mut *doc;
            let mut place = 0;
            let mut found = None;
            for (n, task) in doc.hosts.iter_mut().flat_map(|h| h.tasks.iter_mut()) {
                if *n == node {
                    found = Some(task);
                    break;
                }
                place += 1;
            }
            let Some(task) = found else { return };
            task.checked = !task.checked;
            let mut source = self.source.0.text.borrow_mut();
            let end = task.at + 3;
            if source.as_bytes().get(..end) == doc.seen.as_bytes().get(..end) {
                flip(&mut source, task.at, task.checked);
            }
            flip(&mut doc.seen, task.at, task.checked);
            doc.toggled.push((node, task.checked));
            (place, task.checked)
        };
        self.source.changed(ev);
        if let Some(on_task) = &self.on_task {
            (on_task.borrow_mut())(ev, place, checked);
        }
    }

    /// Bring the document up to its source.
    fn update(&self, cx: &mut StructureCx<'_>, toggle: Option<&Toggle>) {
        let mut doc = self.doc.borrow_mut();
        let Some(root) = doc.root.filter(|&r| cx.store.arena().is_live(r)) else {
            return;
        };
        for (node, checked) in doc.toggled.drain(..) {
            cx.store.set_check_mark(
                node,
                Some(CheckMark {
                    shape: CheckShape::Box,
                    checked,
                }),
            );
            cx.store
                .set_semantic_state(node, SemanticState::checked(checked));
        }
        let source = self.source.0.text.borrow();
        if *source == doc.seen {
            return;
        }
        let doc = &mut *doc;
        // An append changes only the last block, unless a link reference
        // definition reaches across blocks.
        let appended = doc
            .hosts
            .last()
            .filter(|_| doc.defs.is_empty() && source.starts_with(doc.seen.as_str()))
            .and_then(|h| self.format.resume(&source, h.range.start));
        let (kept, parsed) = match appended {
            Some(from) => (
                doc.hosts.len() - 1,
                self.format.parse(&source[from..], from, &self.style),
            ),
            None => (0, self.format.parse(&source, 0, &self.style)),
        };
        let all = appended.is_none() && parsed.defs != doc.defs;
        if appended.is_none() {
            doc.defs = parsed.defs;
        }
        let tops = parsed.blocks;
        let old = &doc.hosts[kept..];
        let same = |o: &Host, n: &Top| !all && doc.seen[o.range.clone()] == source[n.range.clone()];
        let mut prefix = 0;
        while prefix < old.len().min(tops.len()) && same(&old[prefix], &tops[prefix]) {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < (old.len() - prefix).min(tops.len() - prefix)
            && same(&old[old.len() - 1 - suffix], &tops[tops.len() - 1 - suffix])
        {
            suffix += 1;
        }

        let (start, end) = (kept + prefix, doc.hosts.len() - suffix);
        let focus = cx.store.focused().and_then(|f| {
            (start..end).find_map(|at| {
                let place = focus_place(cx.store, doc.hosts[at].node, f)?;
                Some((at - start, place))
            })
        });
        let mut scratch = Vec::new();
        for host in doc.hosts.drain(start..end) {
            cx.store.free_tree(host.node, cx.effects, &mut scratch);
        }
        let anchor = doc.hosts.get(start).map(|h| h.node);
        let mut env = self.env(toggle);
        let mut built = Vec::with_capacity(tops.len() - prefix - suffix);
        for top in &tops[prefix..tops.len() - suffix] {
            let node = {
                let mut b = BuildCx::with_parent(cx.store, cx.states, cx.bindings, root)
                    .with_lists(cx.lists);
                env.block(&mut b, &top.block)
            };
            if anchor.is_some() {
                cx.store.arena_detach(node);
                cx.store.arena_insert_before(root, node, anchor);
            }
            built.push(Host::new(node, top, std::mem::take(&mut env.tasks)));
        }
        if let Some((at, place)) = focus {
            let refocus = built
                .get(at)
                .and_then(|h| nth_focusable(cx.store, h.node, place));
            cx.store.set_focused(refocus);
        }
        doc.hosts.splice(start..start, built);
        for (host, top) in doc.hosts[kept..kept + prefix].iter_mut().zip(&tops) {
            host.moved(top);
        }
        let tail = doc.hosts.len() - suffix;
        for (host, top) in doc.hosts[tail..]
            .iter_mut()
            .zip(&tops[tops.len() - suffix..])
        {
            host.moved(top);
        }
        cx.store.mark_dirty(
            root,
            DirtyClass::MEASURE
                | DirtyClass::LAYOUT
                | DirtyClass::PAINT
                | DirtyClass::HIT_TEST
                | DirtyClass::SEMANTICS,
        );
        doc.seen.clear();
        doc.seen.push_str(&source);
    }
}

/// Write a task box at `at` of `text` checked or not, when one is there.
fn flip(text: &mut String, at: usize, checked: bool) {
    if matches!(text.get(at..at + 3), Some("[ ]" | "[x]" | "[X]")) {
        text.replace_range(at + 1..at + 2, if checked { "x" } else { " " });
    }
}

/// Where the line holding byte `at` of `text` starts.
fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// Which focusable node, in order, under `root` (itself included) `focused`
/// is.
fn focus_place(store: &NodeStore, root: NodeId, focused: NodeId) -> Option<usize> {
    let mut place = 0;
    let mut found = None;
    walk(store, root, &mut |n| {
        if n == focused {
            found = Some(place);
            return false;
        }
        place += usize::from(store.focusable(n));
        true
    });
    found
}

/// The `place`th focusable node under `root`, itself included.
fn nth_focusable(store: &NodeStore, root: NodeId, place: usize) -> Option<NodeId> {
    let mut left = place;
    let mut found = None;
    walk(store, root, &mut |n| {
        if !store.focusable(n) {
            return true;
        }
        if left == 0 {
            found = Some(n);
            return false;
        }
        left -= 1;
        true
    });
    found
}

/// Visit `root` and its subtree in order until `visit` says stop.
fn walk(store: &NodeStore, root: NodeId, visit: &mut impl FnMut(NodeId) -> bool) -> bool {
    if !visit(root) {
        return false;
    }
    store.arena().children(root).all(|c| walk(store, c, visit))
}

#[cfg(test)]
mod tests;
