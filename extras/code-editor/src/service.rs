//! The language-service hook: what the editor asks of whatever knows the
//! language — completions, hovers, where a name is declared, inlay hints —
//! and the diagnostics it reports.
//!
//! A service answers asynchronously: each request hands over a [`Snapshot`]
//! of the document, cheap to take, and gets back a [`Reply`] that resolves
//! on the UI thread's task loop with offsets into that snapshot. The editor
//! carries what comes back over the edits made since, so a slow answer lands
//! on the text it was about. A service is told every edit, in order, before
//! it is asked about the text the edit made.
//!
//! [`channel`] makes a reply a service resolves from any thread: a language
//! server's reader, a worker, or right away.

use std::future::Future;
use std::ops::Range;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use crate::decoration::Decoration;
use crate::document::{Assoc, ChangeSet};
use crate::rope::Rope;
use crate::session::{EditEvent, Inlay};

/// The document's text at one version.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub text: Rope,
    pub version: u64,
}

/// An answer to come: `None` when the service has none to give.
pub type Reply<T> = Pin<Box<dyn Future<Output = Option<T>>>>;

/// A reply with no answer.
pub fn no_reply<T: 'static>() -> Reply<T> {
    Box::pin(std::future::ready(None))
}

/// What kind of thing a completion puts in, for its icon and its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionKind {
    #[default]
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Unit,
    Value,
    Enum,
    Keyword,
    Snippet,
    Color,
    File,
    Reference,
    Folder,
    EnumMember,
    Constant,
    Struct,
    Event,
    Operator,
    TypeParameter,
}

impl CompletionKind {
    /// A short tag for it, as a list shows.
    pub fn tag(self) -> &'static str {
        use CompletionKind as K;
        match self {
            K::Method | K::Function | K::Constructor => "fn",
            K::Field | K::Property => "fld",
            K::Variable | K::Value | K::Reference => "var",
            K::Class | K::Struct | K::Interface => "type",
            K::Enum | K::EnumMember => "enum",
            K::Module | K::Folder | K::File => "mod",
            K::Keyword => "kw",
            K::Snippet => "snip",
            K::Constant => "const",
            K::TypeParameter => "T",
            K::Operator => "op",
            K::Text | K::Unit | K::Color | K::Event => "",
        }
    }
}

/// Something completion can put in at the caret.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Completion {
    /// What the list shows.
    pub label: String,
    /// More about it: its type or signature.
    pub detail: Option<String>,
    pub kind: CompletionKind,
    /// What it puts in.
    pub insert: String,
    /// The bytes of the snapshot it replaces; `None` for the word before
    /// the caret.
    pub range: Option<Range<usize>>,
    /// What typing filters it by; the label when empty.
    pub filter: String,
    /// What it sorts by; the label when empty.
    pub sort: String,
}

/// What a service says of the text under a point.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Hover {
    /// Plain text, lines apart.
    pub text: String,
    /// The bytes of the snapshot it is about.
    pub range: Option<Range<usize>>,
}

/// Where a name is declared.
#[derive(Debug, Clone, PartialEq)]
pub enum Definition {
    /// In this document: bytes of the snapshot.
    Here(Range<usize>),
    /// In another document, at a zero-based line and a column counted as the
    /// service counts them (for a language server, in the encoding it
    /// agreed to: UTF-8 bytes or UTF-16 units).
    Elsewhere { uri: String, line: u32, column: u32 },
}

/// Diagnostics a service reported, over bytes of the text at `version`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Diagnostics {
    pub version: u64,
    pub list: Vec<Decoration>,
}

/// What knows the language: told every edit, asked what the editor shows.
/// Every request has a default that answers nothing, so a service gives
/// only what it has.
pub trait LanguageService {
    /// The document was edited: keep in step.
    fn changed(&self, event: &EditEvent<'_>);

    /// The next diagnostics, when the service has new ones.
    fn diagnostics(&self) -> Reply<Diagnostics> {
        no_reply()
    }

    /// What can go in at byte `at`.
    fn completions(&self, doc: &Snapshot, at: usize) -> Reply<Vec<Completion>> {
        let _ = (doc, at);
        no_reply()
    }

    /// What there is to say of the text at byte `at`.
    fn hover(&self, doc: &Snapshot, at: usize) -> Reply<Hover> {
        let _ = (doc, at);
        no_reply()
    }

    /// Where the name at byte `at` is declared.
    fn definition(&self, doc: &Snapshot, at: usize) -> Reply<Vec<Definition>> {
        let _ = (doc, at);
        no_reply()
    }

    /// The inlay hints over bytes `range`.
    fn inlay_hints(&self, doc: &Snapshot, range: Range<usize>) -> Reply<Vec<Inlay>> {
        let _ = (doc, range);
        no_reply()
    }

    /// The characters that ask for completions when typed, besides a
    /// word's.
    fn completion_triggers(&self) -> Vec<char> {
        Vec::new()
    }
}

/// The sending half of a [`channel`]: resolves its reply from any thread.
pub struct Responder<T> {
    slot: Arc<Mutex<Slot<T>>>,
}

struct Slot<T> {
    value: Option<Option<T>>,
    waker: Option<Waker>,
}

impl<T> Responder<T> {
    /// Resolves the reply with `value`.
    pub fn send(self, value: Option<T>) {
        let waker = {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            slot.value = Some(value);
            slot.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

impl<T> Drop for Responder<T> {
    fn drop(&mut self) {
        // Dropped unsent: the reply has no answer.
        let waker = {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            if slot.value.is_none() {
                slot.value = Some(None);
            }
            slot.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

struct Receive<T> {
    slot: Arc<Mutex<Slot<T>>>,
}

impl<T> Future for Receive<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        match slot.value.take() {
            Some(v) => Poll::Ready(v),
            None => {
                slot.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// A reply and the responder that resolves it.
pub fn channel<T: 'static>() -> (Responder<T>, Reply<T>) {
    let slot = Arc::new(Mutex::new(Slot {
        value: None,
        waker: None,
    }));
    (
        Responder {
            slot: Arc::clone(&slot),
        },
        Box::pin(Receive { slot }),
    )
}

/// The edits of the last versions, to carry what a service said of an
/// older text over to the text now.
#[derive(Debug, Default)]
pub struct EditLog {
    /// Each edit and the version it made, oldest first.
    edits: std::collections::VecDeque<(u64, ChangeSet)>,
}

/// How many edits an answer can lag by and still land.
const LOG_EDITS: usize = 256;

impl EditLog {
    /// Records an edit that made `version`.
    pub fn record(&mut self, version: u64, changes: &ChangeSet) {
        if self.edits.len() == LOG_EDITS {
            self.edits.pop_front();
        }
        self.edits.push_back((version, changes.clone()));
    }

    /// Carries `range` of the text at `from` to the text at `to`; `None`
    /// when the edits between are no longer kept.
    pub fn map(&self, from: u64, to: u64, range: Range<usize>) -> Option<Range<usize>> {
        if from == to {
            return Some(range);
        }
        if from > to {
            return None;
        }
        let first = self.edits.front().map_or(u64::MAX, |e| e.0);
        if from + 1 < first {
            return None;
        }
        let mut r = range;
        for (v, changes) in &self.edits {
            if *v > from && *v <= to {
                let start = changes.map(r.start, Assoc::After);
                let end = changes.map(r.end, Assoc::Before).max(start);
                r = start..end;
            }
        }
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Change, Document};

    #[test]
    fn a_reply_resolves_from_another_thread() {
        let (tx, rx) = channel::<u32>();
        std::thread::spawn(move || tx.send(Some(7)));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut rx = rx;
        let got = loop {
            if let Poll::Ready(v) = rx.as_mut().poll(&mut cx) {
                break v;
            }
            std::thread::yield_now();
        };
        assert_eq!(got, Some(7));
        let (tx, mut rx) = channel::<u32>();
        drop(tx);
        assert_eq!(rx.as_mut().poll(&mut cx), Poll::Ready(None));
    }

    #[test]
    fn ranges_are_carried_over_the_edits_since() {
        let mut doc = Document::new("let x = y;");
        let mut log = EditLog::default();
        let v0 = doc.version();
        let cs = ChangeSet::one(Change::insert(0, "// a\n"));
        doc.apply(&cs);
        log.record(doc.version(), &cs);
        let cs = ChangeSet::one(Change::insert(13, "z"));
        doc.apply(&cs);
        log.record(doc.version(), &cs);
        // "y" at 8..9 then; 13..14 after the line above, the z before it.
        assert_eq!(log.map(v0, doc.version(), 8..9), Some(14..15));
        assert_eq!(log.map(doc.version(), doc.version(), 1..2), Some(1..2));
    }
}
