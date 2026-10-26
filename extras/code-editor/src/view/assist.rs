//! What a language service adds to the editor: a completion list at the
//! caret, a hover box over the text, diagnostics and inlay hints kept up as
//! the text changes, and a jump to where a name is declared.
//!
//! Requests go out from handlers and from the draw pass as tasks on the UI
//! loop; an answer comes back as a task's continuation, is carried over the
//! edits made since it was asked through the edit log, and is drawn on the
//! next pass. One inlay request and one diagnostics wait are in flight at a
//! time, so a fast typist never queues up answers to text long gone. The
//! diagnostics wait and the edit log are the session's, so a view built
//! again on the session polls the wait the last one left.
//!
//! The hover box needs no service: a pointer resting on a diagnostic shows
//! its message, with what the service says of the text there below it.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use viso_ui::anchored::{AnchorAlign, Anchoring, Side};
use viso_ui::context::UpdateCx;
use viso_ui::{
    Axis, Border, BoxStyle, BuildCx, Continuation, EventCx, FlexStyle, Handle, Inset, Key,
    KeyEvent, LeafStyle, Length, NodeId, PointerPhase, Role, Semantics, Sizing, StateWriter,
    StructureCx, TaskFuture, Vec2,
};

use super::input::changed;
use super::{Nodes, Shared, text_request};
use crate::decoration::DIAGNOSTICS;
use crate::history::EditKind;
use crate::selection::{Selection, Selections};
use crate::service::{
    Completion, Definition, Diagnostics, EditLog, Hover, LanguageService, Reply, Snapshot,
};
use crate::session::{ServiceLink, Session};
use crate::text::{CharClass, char_class};

/// The rows the completion list shows at once.
const LIST_ROWS: usize = 10;

/// How long the pointer rests before the hover box shows.
const HOVER_DELAY: Duration = Duration::from_millis(500);

/// The bit that marks a wake as the hover's, not the caret's blink.
pub(super) const HOVER_WAKE: u32 = 1 << 31;

/// The lines past the screen's either side inlay hints are asked for.
const HINT_MARGIN: usize = 100;

/// What the editor calls when a name is declared in another document.
pub type OnDefinition = Box<dyn FnMut(&mut dyn StateWriter, &Definition)>;

/// The service, and what the editor keeps of what it asked and was told.
pub(crate) struct Assist {
    pub service: Option<Rc<dyn LanguageService>>,
    pub log: Rc<RefCell<EditLog>>,
    /// The session's wait for the service's next diagnostics.
    diagnostics: Rc<RefCell<Option<Reply<Diagnostics>>>>,
    /// Whether this view polls the wait for diagnostics.
    listening: Cell<bool>,
    /// Whether an inlay request is in flight, and what the last asked for:
    /// the version and the lines.
    hints_busy: Cell<bool>,
    hints_asked: Cell<Option<(u64, usize, usize)>>,
    /// Bumped by every completion request: an answer to an older one is
    /// dropped.
    asked: Cell<u64>,
    completing: RefCell<Option<Completing>>,
    hover: RefCell<HoverState>,
    pub on_definition: RefCell<Option<OnDefinition>>,
    popups: RefCell<Option<Popups>>,
}

impl Assist {
    /// What the editor keeps of the session's `link`: its service, its
    /// edit log and its wait for diagnostics are the session's.
    pub fn new(link: Option<&ServiceLink>, on_definition: Option<OnDefinition>) -> Self {
        Assist {
            service: link.map(|l| Rc::clone(&l.service)),
            log: link.map_or_else(Rc::default, |l| Rc::clone(&l.log)),
            diagnostics: link.map_or_else(Rc::default, |l| Rc::clone(&l.diagnostics)),
            listening: Cell::new(false),
            hints_busy: Cell::new(false),
            hints_asked: Cell::new(None),
            asked: Cell::new(0),
            completing: RefCell::new(None),
            hover: RefCell::new(HoverState::default()),
            on_definition: RefCell::new(on_definition),
            popups: RefCell::new(None),
        }
    }

    /// Whether the completion list shows.
    pub fn completing(&self) -> bool {
        self.completing.borrow().is_some()
    }

    /// The labels the completion list shows, the chosen one's index.
    pub fn completion_labels(&self) -> Option<(Vec<String>, usize)> {
        let c = self.completing.borrow();
        let c = c.as_ref()?;
        Some((
            c.shown.iter().map(|&i| c.items[i].label.clone()).collect(),
            c.selected,
        ))
    }

    /// What the hover box says, if it shows.
    pub fn hover_text(&self) -> Option<String> {
        let h = self.hover.borrow();
        h.shown.then(|| h.text())
    }
}

/// A completion list open at the caret.
struct Completing {
    items: Vec<Completion>,
    /// The version the items' ranges are in.
    version: u64,
    /// Where the word being completed starts, and the version that is in.
    start: usize,
    start_version: u64,
    /// The items the typed word matches, best first, and the one chosen.
    shown: Vec<usize>,
    selected: usize,
    /// What the rows show now.
    drawn: Vec<(String, String, String, bool)>,
}

/// The hover box: what it is about and what it says.
#[derive(Default)]
struct HoverState {
    /// The byte the pointer rests on, the window point it rests at.
    at: Option<(usize, Vec2)>,
    /// The token of the wake in flight.
    token: u32,
    shown: bool,
    /// The diagnostics' messages there, and what the service said.
    messages: Vec<String>,
    service: Option<String>,
    /// The bytes the box is about: the pointer leaving them closes it.
    range: Option<Range<usize>>,
    drawn: String,
}

impl HoverState {
    fn text(&self) -> String {
        let mut parts: Vec<&str> = self.messages.iter().map(String::as_str).collect();
        if let Some(s) = &self.service {
            parts.push(s);
        }
        parts.join("\n\n")
    }
}

/// The nodes of the completion list and the hover box.
struct Popups {
    list: NodeId,
    rows: Vec<PopupRow>,
    hover: NodeId,
    hover_text: NodeId,
}

struct PopupRow {
    node: NodeId,
    tag: NodeId,
    label: NodeId,
    detail: NodeId,
}

/// Builds the completion list and the hover box, hidden, as layers
/// anchored to the editor at the caret.
pub(super) fn build_popups(shared: &Rc<Shared>, cx: &mut BuildCx<'_>, root: Handle) {
    let theme = &shared.theme;
    let m = shared.metrics.get();
    let lh = m.line_height;
    let panel = BoxStyle {
        fill: theme.popup,
        radius: 4.0,
        border: Border {
            width: 1.0,
            color: theme.popup_border,
        },
        ..BoxStyle::NONE
    };
    let fit = Sizing {
        width: Length::Fit,
        height: Length::Fit,
    };
    let text_leaf = |cx: &mut BuildCx<'_>, color| {
        let leaf = cx.leaf(LeafStyle {
            size: Sizing {
                width: Length::MaxContent,
                height: Length::Fixed(lh),
            },
            style: BoxStyle::NONE,
        });
        cx.text_request(leaf, text_request(shared, " ".into(), color, Vec::new()));
        leaf.id()
    };
    let mut rows = Vec::new();
    let list = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            padding: Inset::all(2.0),
            size: fit,
            style: panel,
            ..FlexStyle::default()
        },
        |cx| {
            for i in 0..LIST_ROWS {
                let (mut tag, mut label, mut detail) = (None, None, None);
                let row = cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: m.cell,
                        padding: Inset {
                            left: 4.0,
                            right: 8.0,
                            top: 0.0,
                            bottom: 0.0,
                        },
                        size: Sizing {
                            width: Length::fill(),
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE.with_radius(3.0),
                        ..FlexStyle::default()
                    },
                    |cx| {
                        tag = Some(text_leaf(cx, theme.line_number));
                        label = Some(text_leaf(cx, theme.text));
                        detail = Some(text_leaf(cx, theme.line_number));
                    },
                );
                cx.semantics(row, Semantics::role(Role::ListOption));
                let sh = Rc::clone(shared);
                cx.on_pointer(row, move |ev| {
                    if ev.pointer().is_some_and(|p| p.phase == PointerPhase::Up) {
                        choose_row(&sh, ev, i);
                    }
                    ev.stop_propagation();
                });
                cx.set_hidden(row, true);
                rows.push(PopupRow {
                    node: row.id(),
                    tag: tag.expect("built"),
                    label: label.expect("built"),
                    detail: detail.expect("built"),
                });
            }
        },
    );
    cx.anchored(
        list,
        root,
        Anchoring {
            side: Side::Bottom,
            align: AnchorAlign::Start,
            gap: 2.0,
            ..Anchoring::default()
        },
    );
    cx.semantics(
        list,
        Semantics::role(Role::ListBox).with_label("Completions"),
    );
    cx.set_hidden(list, true);
    let mut hover_text = None;
    let hover = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            padding: Inset::all(6.0),
            size: fit,
            style: panel,
            ..FlexStyle::default()
        },
        |cx| {
            let leaf = cx.leaf(LeafStyle {
                size: fit,
                style: BoxStyle::NONE,
            });
            cx.text_request(
                leaf,
                text_request(shared, " ".into(), theme.text, Vec::new()),
            );
            hover_text = Some(leaf.id());
        },
    );
    cx.anchored(
        hover,
        root,
        Anchoring {
            side: Side::Top,
            align: AnchorAlign::Start,
            gap: 2.0,
            ..Anchoring::default()
        },
    );
    cx.semantics(hover, Semantics::role(Role::Tooltip));
    cx.set_hidden(hover, true);
    *shared.assist.popups.borrow_mut() = Some(Popups {
        list: list.id(),
        rows,
        hover: hover.id(),
        hover_text: hover_text.expect("built"),
    });
}

/// The document as it is now.
fn snapshot(session: &Session) -> Snapshot {
    Snapshot {
        text: session.doc().text().clone(),
        version: session.doc().version(),
    }
}

/// Runs `reply` as a task of the editor's, handing its answer to `then` at
/// the next frame's start.
fn spawn<T: 'static>(
    s: &mut StructureCx<'_>,
    nodes: Nodes,
    reply: Reply<T>,
    then: impl FnOnce(&mut UpdateCx<'_>, Option<T>) + 'static,
) {
    let task: TaskFuture = Box::pin(async move {
        let got = reply.await;
        Some(Box::new(move |cx: &mut UpdateCx<'_>| then(cx, got)) as Continuation)
    });
    s.store.spawn_task(nodes.root, task);
}

/// The draw pass's part: keeps the waits for diagnostics and inlay hints
/// going, filters the completion list by what was typed, and draws both
/// popups where the caret and the pointer are.
pub(super) fn update(
    shared: &Rc<Shared>,
    session: &mut Session,
    s: &mut StructureCx<'_>,
    nodes: Nodes,
) {
    let assist = &shared.assist;
    if let Some(service) = assist.service.clone() {
        if !assist.listening.get() {
            assist.listening.set(true);
            // The wait is the session's: one a view built before started
            // is polled on, not asked again.
            let wait = Rc::clone(&assist.diagnostics);
            wait.borrow_mut()
                .get_or_insert_with(|| service.diagnostics());
            let reply: Reply<Diagnostics> = Box::pin(std::future::poll_fn(move |cx| {
                let mut wait = wait.borrow_mut();
                let Some(reply) = wait.as_mut() else {
                    return std::task::Poll::Ready(None);
                };
                let got = std::task::ready!(reply.as_mut().poll(cx));
                *wait = None;
                std::task::Poll::Ready(got)
            }));
            let sh = Rc::clone(shared);
            spawn(s, nodes, reply, move |cx, got| {
                sh.assist.listening.set(false);
                if let Some(d) = got {
                    let now = sh.session.borrow().doc().version();
                    let log = sh.assist.log.borrow();
                    let list = d
                        .list
                        .into_iter()
                        .filter_map(|mut dec| {
                            dec.range = log.map(d.version, now, dec.range)?;
                            Some(dec)
                        })
                        .collect();
                    drop(log);
                    sh.session
                        .borrow_mut()
                        .decorations_mut()
                        .set(DIAGNOSTICS, list);
                }
                sh.bump(cx);
            });
        }
        ask_hints(shared, session, s, nodes, &service);
    }
    filter_completions(shared, session);
    draw_popups(shared, s);
}

/// Asks for the inlay hints around the lines on screen, when the text or
/// the lines changed since they were last asked for.
fn ask_hints(
    shared: &Rc<Shared>,
    session: &Session,
    s: &mut StructureCx<'_>,
    nodes: Nodes,
    service: &Rc<dyn LanguageService>,
) {
    let assist = &shared.assist;
    if assist.hints_busy.get() {
        return;
    }
    let rows = shared.rows.borrow();
    let (Some(first), Some(last)) = (
        rows.iter().map(|r| r.line).min(),
        rows.iter().map(|r| r.line).max(),
    ) else {
        return;
    };
    drop(rows);
    let version = session.doc().version();
    let covered = assist
        .hints_asked
        .get()
        .is_some_and(|(v, a, b)| v == version && a <= first && last <= b);
    if covered {
        return;
    }
    let lines = session.doc().line_count();
    let a = first.saturating_sub(HINT_MARGIN);
    let b = (last + HINT_MARGIN).min(lines - 1);
    assist.hints_asked.set(Some((version, a, b)));
    assist.hints_busy.set(true);
    let doc = snapshot(session);
    let range = session.doc().line_start(a)..session.doc().line_end(b);
    let sh = Rc::clone(shared);
    spawn(
        s,
        nodes,
        service.inlay_hints(&doc, range),
        move |cx, got| {
            sh.assist.hints_busy.set(false);
            if let Some(hints) = got {
                let mut session = sh.session.borrow_mut();
                let now = session.doc().version();
                let log = sh.assist.log.borrow();
                let hints = hints
                    .into_iter()
                    .filter_map(|mut h| {
                        h.at = log.map(doc.version, now, h.at..h.at)?.start;
                        Some(h)
                    })
                    .collect();
                drop(log);
                session.set_inlays(hints);
            }
            sh.bump(cx);
        },
    );
}

// ----- completion -----

/// Where the word before byte `at` starts.
fn word_start(session: &Session, at: usize) -> usize {
    let doc = session.doc();
    let line = doc.line_of(at);
    let start = doc.line_start(line);
    let text = doc.line(line);
    let mut a = at - start;
    for (i, c) in text[..a].char_indices().rev() {
        if char_class(c) != CharClass::Word {
            break;
        }
        a = i;
    }
    start + a
}

/// Asks for completions at the primary caret; `typed` is the character
/// that asked, if one did.
pub(super) fn complete(shared: &Rc<Shared>, ev: &mut EventCx<'_>, typed: Option<char>) {
    let assist = &shared.assist;
    let Some(service) = assist.service.clone() else {
        return;
    };
    let session = shared.session.borrow();
    let p = session.selections().primary();
    if !p.is_empty() {
        return;
    }
    if let Some(c) = typed {
        let word = char_class(c) == CharClass::Word;
        let trigger = service.completion_triggers().contains(&c);
        // A word typed on in an open list filters it.
        if !trigger && (!word || assist.completing()) {
            return;
        }
    }
    let at = p.head;
    let start = word_start(&session, at);
    let doc = snapshot(&session);
    drop(session);
    let n = assist.asked.get() + 1;
    assist.asked.set(n);
    let reply = service.completions(&doc, at);
    let sh = Rc::clone(shared);
    ev.spawn_then(reply, move |cx, got: Option<Vec<Completion>>| {
        if sh.assist.asked.get() != n {
            return;
        }
        let items = got.unwrap_or_default();
        if items.is_empty() {
            *sh.assist.completing.borrow_mut() = None;
        } else {
            *sh.assist.completing.borrow_mut() = Some(Completing {
                items,
                version: doc.version,
                start,
                start_version: doc.version,
                shown: Vec::new(),
                selected: 0,
                drawn: Vec::new(),
            });
        }
        sh.bump(cx);
    });
}

/// Filters the open list by the word typed since it opened; closes it when
/// the caret left the word or nothing matches.
fn filter_completions(shared: &Shared, session: &Session) {
    let assist = &shared.assist;
    let mut completing = assist.completing.borrow_mut();
    let Some(c) = completing.as_mut() else {
        return;
    };
    let now = session.doc().version();
    let p = session.selections().primary();
    let start = assist
        .log
        .borrow()
        .map(c.start_version, now, c.start..c.start)
        .map(|r| r.start);
    let open = start.is_some_and(|start| {
        p.is_empty()
            && start <= p.head
            && session.doc().line_of(start) == session.doc().line_of(p.head)
            && session
                .doc()
                .slice(start..p.head)
                .chars()
                .all(|ch| char_class(ch) == CharClass::Word)
    });
    let Some(start) = start.filter(|_| open) else {
        *completing = None;
        return;
    };
    c.start = start;
    c.start_version = now;
    let query = session.doc().slice(start..p.head).into_owned();
    let mut scored: Vec<(i32, usize)> = c
        .items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let key = if item.filter.is_empty() {
                &item.label
            } else {
                &item.filter
            };
            score(key, &query).map(|s| (s, i))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            let key = |i: usize| {
                let it = &c.items[i];
                if it.sort.is_empty() {
                    it.label.clone()
                } else {
                    it.sort.clone()
                }
            };
            key(a.1).cmp(&key(b.1))
        })
    });
    let shown: Vec<usize> = scored.into_iter().map(|(_, i)| i).collect();
    if shown.is_empty() {
        *completing = None;
        return;
    }
    if shown != c.shown {
        c.selected = 0;
        c.shown = shown;
    }
}

/// How well `candidate` matches `query`, typed letters in order, case
/// aside; `None` when it does not. A start, a run and the case typed
/// count for more.
fn score(candidate: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let mut score = 0;
    let mut chars = candidate.char_indices().peekable();
    let mut last: Option<usize> = None;
    let mut prev: Option<char> = None;
    for q in query.chars() {
        let mut found = None;
        for (i, c) in chars.by_ref() {
            let hit = c == q || c.to_lowercase().eq(q.to_lowercase());
            if hit {
                found = Some((i, c));
                break;
            }
            prev = Some(c);
        }
        let (i, c) = found?;
        if i == 0 {
            score += 8;
        }
        if last.is_some_and(|l| candidate[l..i].chars().count() == 1) {
            score += 4;
        }
        let boundary = prev.is_none_or(|p| p == '_' || (p.is_lowercase() && c.is_uppercase()));
        if boundary {
            score += 2;
        }
        if c == q {
            score += 1;
        }
        last = Some(i);
        prev = Some(c);
    }
    Some(score - candidate.len() as i32 / 8)
}

/// Takes the keys an open completion list answers; returns whether it
/// took `key`.
pub(super) fn completion_key(shared: &Rc<Shared>, ev: &mut EventCx<'_>, key: &KeyEvent) -> bool {
    let m = key.modifiers;
    if m.control || m.alt || m.logo {
        return false;
    }
    let step: i64 = match key.key {
        Key::Up => -1,
        Key::Down => 1,
        Key::PageUp => -(LIST_ROWS as i64),
        Key::PageDown => LIST_ROWS as i64,
        Key::Enter | Key::Tab if !m.shift => {
            accept(shared, ev);
            return true;
        }
        Key::Escape => {
            *shared.assist.completing.borrow_mut() = None;
            shared.bump(ev);
            return true;
        }
        _ => return false,
    };
    let mut completing = shared.assist.completing.borrow_mut();
    let Some(c) = completing.as_mut() else {
        return false;
    };
    let n = c.shown.len() as i64;
    let at = c.selected as i64 + step;
    c.selected = if step.abs() == 1 {
        at.rem_euclid(n)
    } else {
        at.clamp(0, n - 1)
    } as usize;
    drop(completing);
    shared.bump(ev);
    true
}

/// Chooses row `row` of the list on screen.
fn choose_row(shared: &Rc<Shared>, ev: &mut EventCx<'_>, row: usize) {
    {
        let mut completing = shared.assist.completing.borrow_mut();
        let Some(c) = completing.as_mut() else {
            return;
        };
        let first = window_start(c.selected, c.shown.len());
        if first + row >= c.shown.len() {
            return;
        }
        c.selected = first + row;
    }
    accept(shared, ev);
}

/// Puts in the chosen completion, over the word typed so far.
fn accept(shared: &Rc<Shared>, ev: &mut EventCx<'_>) {
    let Some(c) = shared.assist.completing.borrow_mut().take() else {
        return;
    };
    let item = &c.items[c.shown[c.selected]];
    let mut session = shared.session.borrow_mut();
    let now = session.doc().version();
    let caret = session.selections().primary().head;
    let log = shared.assist.log.borrow();
    let range = match &item.range {
        Some(r) => match log.map(c.version, now, r.clone()) {
            Some(r) => r.start.min(caret)..r.end.max(caret),
            None => c.start..caret,
        },
        None => c.start..caret,
    };
    drop(log);
    let text = plain_insert(if item.insert.is_empty() {
        &item.label
    } else {
        &item.insert
    });
    let end = range.start + text.len();
    session.apply(
        EditKind::Other,
        crate::ChangeSet::one(crate::Change::replace(range, text)),
        Some(Selections::one(Selection::caret(end))),
        true,
    );
    drop(session);
    changed(shared, ev);
}

/// A snippet's text as plain text: its placeholders' text kept, its tab
/// stops left out.
fn plain_insert(snippet: &str) -> String {
    let mut out = String::with_capacity(snippet.len());
    let mut chars = snippet.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '$' if chars.peek().is_some_and(|n| n.is_ascii_digit()) => {
                while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                    chars.next();
                }
            }
            '$' if chars.peek() == Some(&'{') => {
                chars.next();
                // `${1:text}` keeps `text`; `${1}` nothing.
                while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                    chars.next();
                }
                if chars.peek() == Some(&':') {
                    chars.next();
                }
                let mut depth = 1;
                for n in chars.by_ref() {
                    match n {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    out.push(n);
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// The first of `shown` items the list's rows show, `selected` among them.
fn window_start(selected: usize, shown: usize) -> usize {
    if shown <= LIST_ROWS {
        0
    } else {
        selected
            .saturating_sub(LIST_ROWS / 2)
            .min(shown - LIST_ROWS)
    }
}

// ----- hover -----

/// The pointer moved over the text, at byte `at`: the hover box closes if
/// it left what the box is about, and shows after the pointer rests.
pub(super) fn pointer_moved(
    shared: &Rc<Shared>,
    ev: &mut EventCx<'_>,
    at: Option<usize>,
    point: Vec2,
) {
    let Some(nodes) = shared.nodes.get() else {
        return;
    };
    let mut h = shared.assist.hover.borrow_mut();
    let inside = matches!((&h.range, at), (Some(r), Some(at)) if r.start <= at && at <= r.end);
    if h.shown && !inside {
        h.shown = false;
        h.range = None;
        shared.bump(ev);
    }
    if h.shown || h.at.map(|a| a.0) == at {
        return;
    }
    h.at = at.map(|at| (at, point));
    h.token = (h.token + 1) & !HOVER_WAKE;
    if at.is_some() {
        ev.wake_after(nodes.root, HOVER_DELAY, h.token | HOVER_WAKE);
    }
}

/// The pointer rested: show what is said of the text under it.
pub(super) fn hover_woken(shared: &Rc<Shared>, ev: &mut EventCx<'_>, token: u32) {
    let (at, point) = {
        let h = shared.assist.hover.borrow();
        match h.at {
            Some(a) if h.token | HOVER_WAKE == token => a,
            _ => return,
        }
    };
    show_hover(shared, ev, at, point);
}

/// Shows the hover box for the primary caret.
pub(super) fn hover_at_caret(shared: &Rc<Shared>, ev: &mut EventCx<'_>) {
    let at = shared.session.borrow().selections().primary().head;
    let point = caret_point(shared).unwrap_or(Vec2::ZERO);
    show_hover(shared, ev, at, point);
}

/// Where the primary caret's line box starts, in the window.
fn caret_point(shared: &Shared) -> Option<Vec2> {
    shared.caret_window.get()
}

fn show_hover(shared: &Rc<Shared>, ev: &mut EventCx<'_>, at: usize, point: Vec2) {
    let session = shared.session.borrow();
    // The diagnostics there.
    let mut range: Option<Range<usize>> = None;
    let mut messages = Vec::new();
    for d in session.decorations().in_range(at..at + 1) {
        if d.range.start <= at
            && at <= d.range.end
            && let Some(msg) = &d.message
        {
            messages.push(msg.clone());
            range = Some(match range {
                Some(r) => r.start.min(d.range.start)..r.end.max(d.range.end),
                None => d.range.clone(),
            });
        }
    }
    let word = session.word_at(at);
    let doc = snapshot(&session);
    drop(session);
    {
        let mut h = shared.assist.hover.borrow_mut();
        h.at = Some((at, point));
        h.service = None;
        h.messages = messages;
        h.range = Some(range.clone().unwrap_or(word.clone()));
        h.shown = !h.messages.is_empty();
    }
    shared.bump(ev);
    let Some(service) = shared.assist.service.clone() else {
        return;
    };
    let sh = Rc::clone(shared);
    ev.spawn_then(service.hover(&doc, at), move |cx, got: Option<Hover>| {
        let mut h = sh.assist.hover.borrow_mut();
        if h.at.map(|a| a.0) != Some(at) {
            return;
        }
        if let Some(hover) = got.filter(|g| !g.text.trim().is_empty()) {
            h.service = Some(hover.text.trim().to_owned());
            if let Some(r) = hover.range {
                let now = sh.session.borrow().doc().version();
                if let Some(r) = sh.assist.log.borrow().map(doc.version, now, r) {
                    let old = h.range.clone().unwrap_or(r.clone());
                    h.range = Some(old.start.min(r.start)..old.end.max(r.end));
                }
            }
            h.shown = true;
        }
        drop(h);
        sh.bump(cx);
    });
}

/// Closes the popups: a key, a scroll, the focus gone.
pub(super) fn close_popups(shared: &Shared) -> bool {
    let a = &shared.assist;
    let had = a.completing.borrow_mut().take().is_some();
    let mut h = a.hover.borrow_mut();
    let shown = std::mem::take(&mut h.shown);
    h.range = None;
    had || shown
}

/// Closes the hover box alone.
pub(super) fn close_hover(shared: &Shared) -> bool {
    let mut h = shared.assist.hover.borrow_mut();
    h.range = None;
    std::mem::take(&mut h.shown)
}

/// Draws the completion list and the hover box as they are.
fn draw_popups(shared: &Shared, s: &mut StructureCx<'_>) {
    let popups = shared.assist.popups.borrow();
    let Some(p) = popups.as_ref() else {
        return;
    };
    let theme = &shared.theme;
    let m = shared.metrics.get();
    let caret = shared.caret_window.get();
    // The list, under the caret's line.
    let mut completing = shared.assist.completing.borrow_mut();
    match completing.as_mut() {
        Some(c) if caret.is_some() => {
            let first = window_start(c.selected, c.shown.len());
            let mut want = Vec::with_capacity(LIST_ROWS);
            for k in 0..LIST_ROWS {
                want.push(c.shown.get(first + k).map(|&i| {
                    let it = &c.items[i];
                    (
                        it.kind.tag().to_owned(),
                        it.label.clone(),
                        it.detail.clone().unwrap_or_default(),
                        first + k == c.selected,
                    )
                }));
            }
            for (k, row) in p.rows.iter().enumerate() {
                let w = want[k].clone();
                let had = c.drawn.get(k).cloned();
                match w {
                    None => s.store.set_hidden(row.node, true),
                    Some(w) => {
                        s.store.set_hidden(row.node, false);
                        if had.as_ref() != Some(&w) {
                            let set = |s: &mut StructureCx<'_>, node, text: &str, color| {
                                let text = if text.is_empty() { " " } else { text };
                                s.store.set_text(
                                    node,
                                    text_request(shared, text.to_owned(), color, Vec::new()),
                                );
                            };
                            set(s, row.tag, &w.0, theme.line_number);
                            set(s, row.label, &w.1, theme.text);
                            let detail: String = w.2.chars().take(60).collect();
                            set(s, row.detail, &detail, theme.line_number);
                            s.store.set_fill(
                                row.node,
                                if w.3 {
                                    theme.popup_selected
                                } else {
                                    viso_ui::Rgba::TRANSPARENT
                                },
                            );
                        }
                    }
                }
            }
            c.drawn = want.into_iter().map(|w| w.unwrap_or_default()).collect();
            let at = caret.unwrap_or(Vec2::ZERO);
            s.store.set_hidden(p.list, false);
            s.store.set_anchor_point(
                p.list,
                Some(Vec2 {
                    x: at.x - m.cell * 4.0,
                    y: at.y + m.line_height,
                }),
            );
        }
        _ => {
            if completing.is_some() {
                *completing = None;
            }
            s.store.set_hidden(p.list, true);
        }
    }
    drop(completing);
    // The hover box, over where it is about.
    let mut h = shared.assist.hover.borrow_mut();
    if h.shown {
        let text = h.text();
        let text = clip(&text);
        if h.drawn != text {
            s.store.set_text(
                p.hover_text,
                text_request(shared, text.clone(), theme.text, Vec::new()),
            );
            h.drawn = text;
        }
        let at = h.at.map_or(Vec2::ZERO, |a| a.1);
        s.store.set_hidden(p.hover, false);
        s.store.set_anchor_point(p.hover, Some(at));
    } else {
        s.store.set_hidden(p.hover, true);
    }
}

/// `text` cut to what a hover box shows: some lines, none too long.
fn clip(text: &str) -> String {
    const LINES: usize = 24;
    const COLUMNS: usize = 100;
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        if i == LINES {
            out.push('\u{2026}');
            break;
        }
        if i > 0 {
            out.push('\n');
        }
        for (n, c) in line.chars().enumerate() {
            if n == COLUMNS {
                out.push('\u{2026}');
                break;
            }
            out.push(c);
        }
    }
    if out.is_empty() { " ".into() } else { out }
}

// ----- definition -----

/// Asks where the name at byte `at` is declared, and goes there: in this
/// document, selecting it; elsewhere, through the owner's callback.
pub(super) fn go_to_definition(shared: &Rc<Shared>, ev: &mut EventCx<'_>, at: usize) {
    let Some(service) = shared.assist.service.clone() else {
        return;
    };
    let doc = snapshot(&shared.session.borrow());
    let sh = Rc::clone(shared);
    ev.spawn_then(
        service.definition(&doc, at),
        move |cx, got: Option<Vec<Definition>>| {
            let Some(first) = got.and_then(|d| d.into_iter().next()) else {
                return;
            };
            match &first {
                Definition::Here(range) => {
                    let mut session = sh.session.borrow_mut();
                    let now = session.doc().version();
                    let Some(r) = sh.assist.log.borrow().map(doc.version, now, range.clone())
                    else {
                        return;
                    };
                    session.set_selections(Selections::one(Selection::new(r.start, r.end)));
                    session.reveal();
                }
                Definition::Elsewhere { .. } => {
                    if let Some(f) = sh.assist.on_definition.borrow_mut().as_mut() {
                        f(cx, &first);
                    }
                }
            }
            sh.bump(cx);
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_put_in_their_placeholders_text() {
        assert_eq!(plain_insert("push(${1:value})$0"), "push(value)");
        assert_eq!(
            plain_insert("fn ${1:name}($2) {\n\t$0\n}"),
            "fn name() {\n\t\n}"
        );
        assert_eq!(plain_insert(r"a\$b"), "a$b");
    }

    #[test]
    fn typed_letters_match_in_order_and_starts_count_for_more() {
        assert!(score("push_str", "ps").is_some());
        assert!(score("push_str", "sp").is_none());
        assert!(score("len", "le") > score("is_empty_len", "le"));
        assert!(score("toString", "tS") > score("tostring", "tS"));
    }

    #[test]
    fn the_rows_shown_keep_the_chosen_one_in_view() {
        assert_eq!(window_start(0, 5), 0);
        assert_eq!(window_start(3, 40), 0);
        assert_eq!(window_start(20, 40), 15);
        assert_eq!(window_start(39, 40), 30);
    }
}
