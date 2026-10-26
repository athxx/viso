//! An editing session: a document with its cursors, undo history,
//! highlighting, folds, wrapping and inlays — everything the editor knows
//! but how it is drawn. Every command the editor runs is a method here, so
//! a session edits the same with no view at all.
//!
//! An edit goes through one path, [`Session::apply`]: the document changes,
//! the cursors follow, the history records it, and every array kept a line
//! at a time — highlight states, folds, wrap counts — takes its line
//! splices, so nothing is rebuilt for the lines it did not touch. What the
//! view must redo is left in [`Pending`] for it to take.
//!
//! A session's language service is its own too: the service hears every
//! edit from the session, and what it reported — the diagnostics among the
//! decorations, the edits its answers are carried over, the wait for its
//! next diagnostics — stays with the session, whichever view shows it.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use crate::document::{Assoc, Change, ChangeSet, Document, LineSplice};
use crate::fold::{Fold, Folds};
use crate::highlight::{Language, LineStates, PlainText, Token, TokenKind};
use crate::history::{EditKind, History};
use crate::layout::{self, InlaySpan, LayoutRules, RowSpan};
use crate::rope::Rope;
use crate::search::Search;
use crate::selection::{Selection, Selections};
use crate::service::{Diagnostics, EditLog, LanguageService, Reply};

/// How a session edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The cells between tab stops.
    pub tab_size: u32,
    /// Whether Tab and indentation put in spaces rather than tabs.
    pub insert_spaces: bool,
    /// Whether long lines wrap at the view's width.
    pub wrap: bool,
    /// Whether typing an opener puts in its closer.
    pub auto_pairs: bool,
    /// Whether a new line takes the indentation of the line it breaks.
    pub auto_indent: bool,
    /// Whether the text can be read and selected but not edited.
    pub read_only: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tab_size: 4,
            insert_spaces: true,
            wrap: false,
            auto_pairs: true,
            auto_indent: true,
            read_only: false,
        }
    }
}

/// What kind of text an inlay is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InlayKind {
    /// A type, after the name it is of.
    #[default]
    Type,
    /// A parameter's name, before the argument it takes.
    Parameter,
    /// Anything else a language service shows.
    Other,
}

/// Text drawn in a line that is not in it: an inlay hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inlay {
    /// The byte of the document it sits before.
    pub at: usize,
    pub text: String,
    pub kind: InlayKind,
}

/// An input method's composition, shown at the primary caret until it
/// commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preedit {
    /// Where it shows: the primary caret.
    pub at: usize,
    pub text: String,
    /// The composition's own caret, a byte of `text`.
    pub caret: usize,
}

/// What changed since the view last drew.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pending {
    /// Lines came or went, or folding or wrapping changed which rows show:
    /// the view lists its rows again.
    pub rows: bool,
    /// Some text, its highlighting or its inlays changed.
    pub text: bool,
    /// The cursors moved.
    pub cursors: bool,
    /// The primary caret should be brought into view.
    pub reveal: bool,
    /// The search, its matches or its current one changed.
    pub search: bool,
    /// The decorations changed.
    pub decorations: bool,
}

impl Pending {
    pub(crate) fn all() -> Pending {
        Pending {
            rows: true,
            text: true,
            cursors: true,
            reveal: false,
            search: true,
            decorations: true,
        }
    }

    /// Whether anything changed.
    pub fn any(&self) -> bool {
        *self != Pending::default()
    }
}

/// An edit, told to whoever listens for edits: a language client keeping a
/// server's copy of the text.
pub struct EditEvent<'a> {
    /// The text before it.
    pub before: &'a Rope,
    /// The changes, in the offsets of `before`.
    pub changes: &'a ChangeSet,
    /// The document after.
    pub after: &'a Document,
}

/// Told every edit.
pub type EditListener = Box<dyn FnMut(&EditEvent<'_>)>;

/// A session's language service and what the session keeps of it.
pub(crate) struct ServiceLink {
    pub service: Rc<dyn LanguageService>,
    /// The edits since, to carry an answer about an older text to the text
    /// now; shared with the view that asks.
    pub log: Rc<RefCell<EditLog>>,
    /// The wait for the service's next diagnostics, polled by whichever
    /// view shows the session: a view built again picks it up rather than
    /// asking anew, so a report already on its way is not lost.
    pub diagnostics: Rc<RefCell<Option<Reply<Diagnostics>>>>,
}

/// An editing session.
pub struct Session {
    pub(crate) doc: Document,
    pub(crate) sel: Selections,
    pub(crate) history: History,
    pub(crate) language: Rc<dyn Language>,
    pub(crate) states: LineStates,
    pub(crate) folds: Folds,
    pub(crate) config: Config,
    /// The cells a row holds when wrapping, as the view last measured.
    pub(crate) wrap_cells: u32,
    /// The rows each line wraps to, when wrapping.
    pub(crate) wraps: Vec<u16>,
    pub(crate) inlays: Vec<Inlay>,
    pub(crate) preedit: Option<Preedit>,
    pub(crate) search: Option<Search>,
    /// The rows the view shows: what a page up or down moves.
    pub(crate) page_rows: usize,
    pub(crate) pending: Pending,
    /// Closers typing put in, which typing them again steps over.
    pub(crate) closers: Vec<usize>,
    /// What the last copy of whole lines put on the clipboard.
    pub(crate) line_clip: Option<String>,
    /// A cut was made since the view last drew: the platform's second cut
    /// request for one shortcut is not another cut.
    pub(crate) cut_guard: bool,
    pub(crate) listeners: Vec<EditListener>,
    /// The language service, told every edit after the listeners.
    pub(crate) service: Option<ServiceLink>,
    /// Lexed tokens of a line, reused.
    pub(crate) scratch: Vec<Token>,
    pub(crate) decorations: crate::decoration::Decorations,
    /// The cells each line takes on one row, inlays with them.
    pub(crate) widths: Vec<u32>,
}

impl Session {
    /// A session editing `text` as plain text.
    pub fn new(text: &str) -> Session {
        let doc = Document::new(text);
        let lines = doc.line_count();
        Session {
            doc,
            sel: Selections::default(),
            history: History::new(),
            language: Rc::new(PlainText),
            states: LineStates::new(lines),
            folds: Folds::new(),
            config: Config::default(),
            wrap_cells: 80,
            wraps: Vec::new(),
            inlays: Vec::new(),
            preedit: None,
            search: None,
            page_rows: 30,
            pending: Pending::all(),
            closers: Vec::new(),
            line_clip: None,
            cut_guard: false,
            listeners: Vec::new(),
            service: None,
            scratch: Vec::new(),
            decorations: Default::default(),
            widths: Vec::new(),
        }
        .measured()
    }

    /// It with every line's width worked out.
    fn measured(mut self) -> Session {
        self.measure_all();
        self
    }

    /// Works out every line's width, in one pass over the text.
    pub(crate) fn measure_all(&mut self) {
        let tab = self.rules().tab;
        let mut widths = Vec::with_capacity(self.doc.line_count());
        if self.inlays.is_empty() {
            self.doc
                .text()
                .for_each_line(|line| widths.push(line_width(line, &[], tab)));
        } else {
            for l in 0..self.doc.line_count() {
                let spans = line_inlays(&self.doc, &self.inlays, l, None, tab).spans;
                widths.push(line_width(&self.doc.line(l), &spans, tab));
            }
        }
        self.widths = widths;
    }

    /// The cells the widest line takes.
    pub fn widest_cells(&self) -> u32 {
        self.widths.iter().copied().max().unwrap_or(0)
            + if self.folds.is_empty() { 0 } else { FOLD_CELLS }
    }

    /// The document.
    #[inline]
    pub fn doc(&self) -> &Document {
        &self.doc
    }

    /// The whole text, lines ending in `'\n'`.
    pub fn text(&self) -> String {
        self.doc.to_text()
    }

    /// The cursors.
    #[inline]
    pub fn selections(&self) -> &Selections {
        &self.sel
    }

    /// How it edits.
    #[inline]
    pub fn config(&self) -> Config {
        self.config
    }

    /// Edit as `config` says.
    pub fn set_config(&mut self, config: Config) {
        let relayout = config.tab_size != self.config.tab_size || config.wrap != self.config.wrap;
        let tab = config.tab_size != self.config.tab_size;
        self.config = config;
        if tab {
            self.measure_all();
        }
        if relayout {
            self.rewrap();
        }
    }

    /// The language it highlights as.
    pub fn language(&self) -> &Rc<dyn Language> {
        &self.language
    }

    /// Highlight as `language`.
    pub fn set_language(&mut self, language: Rc<dyn Language>) {
        self.language = language;
        self.states.reset(self.doc.line_count());
        self.pending.text = true;
    }

    /// The undo history.
    pub fn history(&self) -> &History {
        &self.history
    }

    /// The folds.
    pub fn folds(&self) -> &Folds {
        &self.folds
    }

    /// The input method's composition, if one is showing.
    pub fn preedit(&self) -> Option<&Preedit> {
        self.preedit.as_ref()
    }

    /// Takes what changed since the last call.
    pub fn take_pending(&mut self) -> Pending {
        self.cut_guard = false;
        std::mem::take(&mut self.pending)
    }

    /// Asks the view to bring the primary caret into view.
    pub fn reveal(&mut self) {
        self.pending.reveal = true;
    }

    /// Keeps `service` in step with the session from now on, or none. A
    /// service other than the one it had starts afresh: the diagnostics the
    /// old one reported go.
    pub(crate) fn set_service(&mut self, service: Option<Rc<dyn LanguageService>>) {
        let same = match (&self.service, &service) {
            (Some(link), Some(s)) => Rc::ptr_eq(&link.service, s),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if self.service.is_some() {
            self.decorations_mut().clear(crate::decoration::DIAGNOSTICS);
        }
        self.service = service.map(|service| ServiceLink {
            service,
            log: Rc::default(),
            diagnostics: Rc::default(),
        });
    }

    /// The language service and what the session keeps of it.
    pub(crate) fn service(&self) -> Option<&ServiceLink> {
        self.service.as_ref()
    }

    /// Calls `listener` with every edit from now on.
    pub fn listen(&mut self, listener: EditListener) {
        self.listeners.push(listener);
    }

    /// Replace the whole text, as a file read again: the history goes, and
    /// the cursors stay where they can.
    pub fn set_text(&mut self, text: &str) {
        let before = self.doc.text().clone();
        let applied = self.doc.reset(text);
        let all = ChangeSet::one(Change::replace(
            0..before.len(),
            crate::document::normalize(text),
        ));
        self.history.clear();
        self.sel = self.sel.clamp(self.doc.len());
        self.after_edit(&before, &all, &applied.splices);
        self.folds.clear();
        self.measure_all();
        self.pending = Pending::all();
    }

    /// Put the cursors at `selections`.
    pub fn set_selections(&mut self, selections: Selections) {
        let selections = selections.clamp(self.doc.len());
        if selections != self.sel {
            self.sel = selections;
            self.cursors_moved();
        }
    }

    /// The cursors moved by other than an edit: the typing run ends, the
    /// closers typed are no longer stepped over, a fold the primary caret
    /// went into opens.
    pub(crate) fn cursors_moved(&mut self) {
        self.history.close();
        self.closers.clear();
        self.pending.cursors = true;
        self.pending.reveal = true;
        let head = self.doc.line_of(self.sel.primary().head);
        if self.folds.is_hidden(head) && self.folds.reveal(head..=head) {
            self.relist();
        }
    }

    /// The rows changed: wrapping or folding.
    pub(crate) fn relist(&mut self) {
        self.pending.rows = true;
        self.pending.text = true;
        self.pending.cursors = true;
    }

    // ----- the one edit path -----

    /// Applies `changes` as an edit of `kind`, the cursors after it being
    /// `after` (where they map to when `None`). `breaks` starts a new undo
    /// step whatever the kind.
    pub fn apply(
        &mut self,
        kind: EditKind,
        changes: ChangeSet,
        after: Option<Selections>,
        breaks: bool,
    ) {
        if changes.is_empty() {
            if let Some(after) = after {
                self.set_selections(after);
            }
            return;
        }
        let before_sel = self.sel.clone();
        let before = self.doc.text().clone();
        let applied = self.doc.apply(&changes);
        let after = after.unwrap_or_else(|| self.sel.map(&changes));
        self.sel = after.clamp(self.doc.len());
        self.history.record(
            kind,
            &changes,
            applied.inverse,
            &before_sel,
            &self.sel,
            breaks,
        );
        for c in &mut self.closers {
            *c = changes.map(*c, Assoc::After);
        }
        self.after_edit(&before, &changes, &applied.splices);
    }

    /// Everything kept a line at a time, and every listener, follows an
    /// edit.
    fn after_edit(&mut self, before: &Rope, changes: &ChangeSet, splices: &[LineSplice]) {
        self.preedit = None;
        self.states.splice(splices);
        let lines_changed = splices.iter().any(|s| s.removed != s.added);
        if self.folds.splice(splices) {
            self.pending.rows = true;
        }
        if lines_changed {
            self.pending.rows = true;
        }
        // Inlays move with their text; one in a changed range goes.
        if !self.inlays.is_empty() {
            let cs = changes.changes();
            self.inlays.retain_mut(|i| {
                let touched = cs
                    .iter()
                    .any(|c| c.range.start < i.at && i.at < c.range.end);
                i.at = changes.map(i.at, Assoc::Before);
                !touched
            });
        }
        self.decorations.map(changes);
        {
            let tab = self.rules().tab;
            let doc = &self.doc;
            let inlays = &self.inlays;
            LineSplice::apply(splices, &mut self.widths, false, |l| {
                let spans = line_inlays(doc, inlays, l, None, tab).spans;
                line_width(&doc.line(l), &spans, tab)
            });
        }
        if self.config.wrap {
            let rules = self.rules();
            let doc = &self.doc;
            let inlays = &self.inlays;
            let mut changed = false;
            let old: Vec<u16> = splices
                .iter()
                .flat_map(|s| self.wraps[s.old..(s.old + s.removed).min(self.wraps.len())].to_vec())
                .collect();
            LineSplice::apply(splices, &mut self.wraps, false, |l| {
                let spans = line_inlays(doc, inlays, l, None, rules.tab).spans;
                layout::row_count(&doc.line(l), &spans, rules).min(u16::MAX as usize) as u16
            });
            let new: Vec<u16> = splices
                .iter()
                .flat_map(|s| self.wraps[s.new..s.new + s.added].to_vec())
                .collect();
            if old != new {
                changed = true;
            }
            if changed {
                self.pending.rows = true;
            }
        }
        if let Some(search) = &mut self.search {
            search.update(&self.doc, changes, splices);
            self.pending.search = true;
        }
        self.pending.text = true;
        self.pending.cursors = true;
        self.pending.reveal = true;
        if !self.listeners.is_empty() || self.service.is_some() {
            let event = EditEvent {
                before,
                changes,
                after: &self.doc,
            };
            for l in &mut self.listeners {
                l(&event);
            }
            // The service and the edit log hear every edit, in order.
            if let Some(link) = &self.service {
                link.service.changed(&event);
                link.log.borrow_mut().record(self.doc.version(), changes);
            }
        }
    }

    /// Takes back the last step.
    pub fn undo(&mut self) -> bool {
        self.replay(true)
    }

    /// Makes the last step taken back again.
    pub fn redo(&mut self) -> bool {
        self.replay(false)
    }

    fn replay(&mut self, undo: bool) -> bool {
        if self.config.read_only {
            return false;
        }
        let replay = if undo {
            self.history.undo()
        } else {
            self.history.redo()
        };
        let Some(replay) = replay else {
            return false;
        };
        let mut inverses = Vec::with_capacity(replay.edits.len());
        for edit in &replay.edits {
            let before = self.doc.text().clone();
            let applied = self.doc.apply(edit);
            inverses.push(applied.inverse);
            self.after_edit(&before, edit, &applied.splices);
        }
        if undo {
            self.history.undone(inverses);
        } else {
            self.history.redone(inverses);
        }
        self.sel = replay.selections.clamp(self.doc.len());
        self.closers.clear();
        // The first line it changed comes into view, unfolded.
        let head = self.doc.line_of(self.sel.primary().head);
        if self.folds.reveal(head..=head) {
            self.relist();
        }
        true
    }

    // ----- layout -----

    /// How lines lay out now.
    pub fn rules(&self) -> LayoutRules {
        LayoutRules {
            tab: self.config.tab_size.max(1),
            wrap: self.config.wrap.then_some(self.wrap_cells.max(8)),
        }
    }

    /// Wrap rows `cells` wide; a no-op when they are already.
    pub fn set_wrap_cells(&mut self, cells: u32) {
        let cells = cells.max(8);
        if cells != self.wrap_cells {
            self.wrap_cells = cells;
            if self.config.wrap {
                self.rewrap();
            }
        }
    }

    /// The rows a page up or down moves.
    pub fn set_page_rows(&mut self, rows: usize) {
        self.page_rows = rows.max(1);
    }

    /// Works out every line's rows again.
    pub(crate) fn rewrap(&mut self) {
        if self.config.wrap {
            let rules = self.rules();
            let width = rules.wrap.unwrap_or(u32::MAX);
            let mut wraps = Vec::with_capacity(self.doc.line_count());
            let count = |line: &str, spans: &[InlaySpan]| {
                let n = if spans.is_empty() && line.len() as u32 <= width && !line.contains('\t') {
                    1
                } else {
                    layout::row_count(line, spans, rules)
                };
                n.min(u16::MAX as usize) as u16
            };
            if self.inlays.is_empty() {
                self.doc
                    .text()
                    .for_each_line(|line| wraps.push(count(line, &[])));
            } else {
                for l in 0..self.doc.line_count() {
                    let spans = line_inlays(&self.doc, &self.inlays, l, None, rules.tab).spans;
                    wraps.push(count(&self.doc.line(l), &spans));
                }
            }
            self.wraps = wraps;
        } else {
            self.wraps = Vec::new();
        }
        self.relist();
    }

    /// The rows line `line` lays out to.
    pub fn row_count(&self, line: usize) -> usize {
        let base = if self.config.wrap {
            self.wraps.get(line).copied().unwrap_or(1) as usize
        } else {
            1
        };
        // A composition can wrap its line further.
        if let Some(p) = &self.preedit
            && self.config.wrap
            && self.doc.line_of(p.at) == line
        {
            return self.line_rows(line).len();
        }
        base
    }

    /// The inlays among line `line`'s bytes, a folded line's placeholder and
    /// the composition with them, in line bytes.
    pub fn inlay_spans(&self, line: usize) -> Vec<InlaySpan> {
        self.line_inlays(line).spans
    }

    /// The inlays among line `line`'s bytes and what each is.
    pub fn line_inlays(&self, line: usize) -> LineInlays {
        let mut out = line_inlays(
            &self.doc,
            &self.inlays,
            line,
            self.preedit.as_ref(),
            self.rules().tab,
        );
        if self.folds.at(line).is_some() {
            out.spans.push(InlaySpan {
                at: self.doc.line_range(line).len(),
                cells: FOLD_CELLS,
            });
            out.sources.push(InlaySource::Fold);
        }
        out
    }

    /// The inlays of line `line`, as stored.
    pub fn line_inlay_list(&self, line: usize) -> &[Inlay] {
        let range = self.doc.line_range(line);
        let a = self.inlays.partition_point(|i| i.at < range.start);
        let b = self.inlays.partition_point(|i| i.at <= range.end);
        &self.inlays[a..b]
    }

    /// The rows line `line` lays out to.
    pub fn line_rows(&self, line: usize) -> Vec<RowSpan> {
        let text = self.doc.line(line);
        layout::rows(&text, &self.inlay_spans(line), self.rules())
    }

    /// Show `inlays` (in any order), in place of the ones shown.
    pub fn set_inlays(&mut self, mut inlays: Vec<Inlay>) {
        inlays.retain(|i| i.at <= self.doc.len() && self.doc.text().is_char_boundary(i.at));
        inlays.sort_by_key(|i| i.at);
        if inlays == self.inlays {
            return;
        }
        // Only the lines whose inlays came or went are measured again.
        let mut lines: Vec<usize> = self
            .inlays
            .iter()
            .chain(&inlays)
            .map(|i| self.doc.line_of(i.at))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        self.inlays = inlays;
        self.pending.text = true;
        self.pending.cursors = true;
        if lines.len() > self.doc.line_count() / 8 {
            self.measure_all();
            if self.config.wrap {
                self.rewrap();
            }
            return;
        }
        let rules = self.rules();
        for &l in &lines {
            let spans = line_inlays(&self.doc, &self.inlays, l, None, rules.tab).spans;
            let text = self.doc.line(l);
            if let Some(w) = self.widths.get_mut(l) {
                *w = line_width(&text, &spans, rules.tab);
            }
            if self.config.wrap
                && let Some(n) = self.wraps.get_mut(l)
            {
                let rows = layout::row_count(&text, &spans, rules).min(u16::MAX as usize) as u16;
                if *n != rows {
                    *n = rows;
                    self.pending.rows = true;
                }
            }
        }
    }

    /// The inlays shown.
    pub fn inlays(&self) -> &[Inlay] {
        &self.inlays
    }

    /// Where position `at` is drawn: its line, the row of the line, and
    /// the cell in the row.
    pub fn point_of(&self, at: usize) -> (usize, usize, u32) {
        let line = self.doc.line_of(at);
        let start = self.doc.line_start(line);
        let text = self.doc.line(line);
        let spans = self.inlay_spans(line);
        let rows = layout::rows(&text, &spans, self.rules());
        let col_at = at - start;
        let r = layout::row_of(&rows, col_at);
        let col = layout::col_of(&text, rows[r], &spans, col_at, self.rules().tab);
        (line, r, col)
    }

    /// The position nearest cell `x` of row `row` of line `line`.
    pub fn position_at(&self, line: usize, row: usize, x: f32) -> usize {
        let line = line.min(self.doc.line_count() - 1);
        let start = self.doc.line_start(line);
        let text = self.doc.line(line);
        let spans = self.inlay_spans(line);
        let rows = layout::rows(&text, &spans, self.rules());
        let span = rows[row.min(rows.len() - 1)];
        let mut at = layout::byte_at(&text, span, &spans, x, self.rules().tab);
        // A click past a wrapped row's end stays on it.
        if at == span.end && span.end < text.len() && span.end > span.start {
            at = crate::text::prev_grapheme(&text, span.end).max(span.start);
        }
        start + at.min(text.len())
    }

    // ----- highlighting -----

    /// Line `line`'s tokens, into `out`.
    pub fn tokens(&mut self, line: usize, out: &mut Vec<Token>) {
        out.clear();
        let language = Rc::clone(&self.language);
        let doc = &self.doc;
        let scratch = &mut self.scratch;
        let start = self.states.start_of(line, |i, s| {
            scratch.clear();
            language.line(s, &doc.line(i), scratch)
        });
        language.line(start, &self.doc.line(line), out);
    }

    /// The kind of token at position `at`.
    pub fn kind_at(&mut self, at: usize) -> TokenKind {
        let line = self.doc.line_of(at);
        let mut tokens = std::mem::take(&mut self.scratch);
        self.tokens(line, &mut tokens);
        let kind = crate::highlight::kind_at(&tokens, at - self.doc.line_start(line));
        self.scratch = tokens;
        kind
    }

    // ----- folding -----

    /// Folds the range line `line` heads, by the language's indentation.
    pub fn fold_line(&mut self, line: usize) -> bool {
        let Some(fold) = crate::fold::indent_range(&self.doc, line, self.rules().tab) else {
            return false;
        };
        self.fold(fold)
    }

    /// Folds `fold`. The cursors in it go to its header line's end.
    pub fn fold(&mut self, fold: Fold) -> bool {
        if fold.end >= self.doc.line_count() || !self.folds.add(fold) {
            return false;
        }
        let end = self.doc.line_end(fold.start);
        let hidden = self.doc.line_start(fold.start + 1)..self.doc.line_end(fold.end);
        let sel = self.sel.map_each(|s| {
            let into = |p: usize| {
                if hidden.contains(&p) || p == hidden.end {
                    end
                } else {
                    p
                }
            };
            Selection::new(into(s.anchor), into(s.head))
        });
        self.sel = sel;
        self.relist();
        true
    }

    /// Unfolds the fold line `line` heads, or the one hiding it.
    pub fn unfold(&mut self, line: usize) -> bool {
        let head = self.folds.shown_line(line);
        if self.folds.remove(head) {
            self.relist();
            return true;
        }
        false
    }

    /// Folds line `line`'s range when it is open, and opens it when folded.
    pub fn toggle_fold(&mut self, line: usize) -> bool {
        if self.folds.at(line).is_some() {
            self.unfold(line)
        } else {
            self.fold_line(line)
        }
    }

    /// Folds every range that folds, outermost first.
    pub fn fold_all(&mut self) {
        let tab = self.rules().tab;
        let mut line = 0;
        let lines = self.doc.line_count();
        let mut any = false;
        while line < lines {
            match crate::fold::indent_range(&self.doc, line, tab) {
                Some(f) => {
                    any |= self.folds.add(f);
                    line = f.end + 1;
                }
                None => line += 1,
            }
        }
        if any {
            let sel = self.sel.map_each(|s| {
                let fix = |p: usize| {
                    let l = self.doc.line_of(p);
                    match self.folds.hiding(l) {
                        Some(f) => self.doc.line_end(f.start),
                        None => p,
                    }
                };
                Selection::new(fix(s.anchor), fix(s.head))
            });
            self.sel = sel;
            self.relist();
        }
    }

    /// Opens every fold.
    pub fn unfold_all(&mut self) {
        if self.folds.clear() {
            self.relist();
        }
    }

    /// The rows the view lists: one for each line no fold hides.
    pub fn shown_lines(&self) -> usize {
        self.folds.shown(self.doc.line_count())
    }

    /// The line row `row` of the list shows.
    pub fn line_of_row(&self, row: usize) -> usize {
        self.folds.line_of(row).min(self.doc.line_count() - 1)
    }

    /// The row of the list line `line` is shown on.
    pub fn row_of_line(&self, line: usize) -> usize {
        self.folds.row_of(line)
    }

    // ----- search -----

    /// The search, if one is open.
    pub fn search(&self) -> Option<&Search> {
        self.search.as_ref()
    }

    // ----- decorations -----

    /// The decorations.
    pub fn decorations(&self) -> &crate::decoration::Decorations {
        &self.decorations
    }

    /// The decorations, to change.
    pub fn decorations_mut(&mut self) -> &mut crate::decoration::Decorations {
        self.pending.decorations = true;
        &mut self.decorations
    }

    /// The text of `range`.
    pub fn slice(&self, range: Range<usize>) -> String {
        self.doc.slice(range).into_owned()
    }
}

/// The cells `line` takes on one row with `inlays`.
fn line_width(line: &str, inlays: &[InlaySpan], tab: u32) -> u32 {
    if inlays.is_empty() && line.is_ascii() && !line.contains('\t') {
        return line.len() as u32;
    }
    layout::line_cells(line, inlays, tab)
}

/// The cells a fold's placeholder takes.
pub const FOLD_CELLS: u32 = 3;

/// What an inlay span of a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlaySource {
    /// The session's inlay at this index.
    Inlay(usize),
    /// The input method's composition.
    Preedit,
    /// A folded line's placeholder.
    Fold,
}

/// The inlays of a line, in line bytes, and what each is.
#[derive(Debug, Clone, Default)]
pub struct LineInlays {
    pub spans: Vec<InlaySpan>,
    pub sources: Vec<InlaySource>,
}

/// The inlays of `inlays` (sorted, document bytes) on line `line`, with
/// `preedit` when it is on it, in line bytes and cells.
fn line_inlays(
    doc: &Document,
    inlays: &[Inlay],
    line: usize,
    preedit: Option<&Preedit>,
    tab: u32,
) -> LineInlays {
    let range = doc.line_range(line);
    let a = inlays.partition_point(|i| i.at < range.start);
    let b = inlays.partition_point(|i| i.at <= range.end);
    let mut out = LineInlays::default();
    if a == b && preedit.is_none() {
        return out;
    }
    for (k, i) in inlays[a..b].iter().enumerate() {
        out.spans.push(InlaySpan {
            at: i.at - range.start,
            cells: crate::text::cells(&i.text, 0, tab) + INLAY_PAD,
        });
        out.sources.push(InlaySource::Inlay(a + k));
    }
    if let Some(p) = preedit
        && range.start <= p.at
        && p.at <= range.end
    {
        let at = p.at - range.start;
        let i = out.spans.partition_point(|s| s.at < at);
        out.spans.insert(
            i,
            InlaySpan {
                at,
                cells: crate::text::cells(&p.text, 0, tab),
            },
        );
        out.sources.insert(i, InlaySource::Preedit);
    }
    out
}

/// The cells of room an inlay keeps from its neighbours.
pub const INLAY_PAD: u32 = 1;
