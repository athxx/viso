//! A language service plugged into the editor, driven through the facade:
//! completions typed and chosen, diagnostics and inlay hints drawn as they
//! come, the hover box over a diagnostic, and a jump to a definition.

mod common;

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use common::*;
use viso_code_editor::CodeEditor;
use viso_code_editor::decoration::{Decoration, Severity};
use viso_code_editor::service::{
    Completion, CompletionKind, Definition, Diagnostics, Hover, LanguageService, Reply, Responder,
    Snapshot, channel,
};
use viso_code_editor::session::{EditEvent, Inlay, InlayKind};

thread_local! {
    static EDITS: Cell<usize> = const { Cell::new(0) };
    static ASKED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// A service that answers at once from what the text says.
#[derive(Default)]
struct Fake {
    /// The waits for diagnostics after the first, kept pending.
    later: RefCell<Vec<Responder<Diagnostics>>>,
    sent: Cell<bool>,
    version: Cell<u64>,
    text: RefCell<String>,
}

fn ready<T: 'static>(value: Option<T>) -> Reply<T> {
    let (tx, rx) = channel();
    tx.send(value);
    rx
}

fn find(text: &str, word: &str) -> Range<usize> {
    let at = text.find(word).expect("in the text");
    at..at + word.len()
}

impl LanguageService for Fake {
    fn changed(&self, event: &EditEvent<'_>) {
        EDITS.with(|e| e.set(e.get() + 1));
        self.version.set(event.after.version());
        *self.text.borrow_mut() = event.after.to_text();
    }

    fn diagnostics(&self) -> Reply<Diagnostics> {
        if self.sent.replace(true) {
            let (tx, rx) = channel();
            self.later.borrow_mut().push(tx);
            return rx;
        }
        ready(Some(Diagnostics {
            version: 0,
            list: vec![Decoration::diagnostic(
                14..17,
                Severity::Error,
                "no such name `bad`",
            )],
        }))
    }

    fn completions(&self, _: &Snapshot, at: usize) -> Reply<Vec<Completion>> {
        ASKED.with(|a| a.borrow_mut().push(format!("complete {at}")));
        let item = |label: &str| Completion {
            label: label.into(),
            insert: label.into(),
            kind: CompletionKind::Function,
            detail: Some(format!("fn {label}()")),
            ..Completion::default()
        };
        ready(Some(vec![item("push"), item("println"), item("print")]))
    }

    fn hover(&self, doc: &Snapshot, at: usize) -> Reply<Hover> {
        ASKED.with(|a| a.borrow_mut().push(format!("hover {at}")));
        let _ = doc;
        ready(Some(Hover {
            text: "let bad: Missing".into(),
            range: None,
        }))
    }

    fn definition(&self, doc: &Snapshot, _: usize) -> Reply<Vec<Definition>> {
        let text = doc.text.slice(0..doc.text.len()).into_owned();
        ready(Some(vec![Definition::Here(find(&text, "main"))]))
    }

    fn inlay_hints(&self, doc: &Snapshot, _: Range<usize>) -> Reply<Vec<Inlay>> {
        let text = doc.text.slice(0..doc.text.len()).into_owned();
        let hints = text.find("let x").map(|at| Inlay {
            at: at + 5,
            text: ": i32".into(),
            kind: InlayKind::Type,
        });
        ready(Some(hints.into_iter().collect()))
    }

    fn completion_triggers(&self) -> Vec<char> {
        vec!['.']
    }
}

fn served(e: CodeEditor) -> CodeEditor {
    e.service(Rc::new(Fake::default()))
}

const TEXT: &str = "fn main() {\n  bad;\n  let x = 1;\n  \n}";

/// Frames for `ms` milliseconds, a wakeup before each.
fn wait(ms: u64) -> Vec<RawEvent> {
    (0..ms.div_ceil(16) + 1)
        .flat_map(|_| [RawEvent::Wakeup, redraw()])
        .collect()
}

#[test]
fn completions_open_as_a_word_is_typed_filter_and_go_in() {
    setup(Some(served));
    EDITS.with(|e| e.set(0));
    let _app = run(TEXT, &[focus(), click(at(3, 2.0)), typed("p")]);
    let h = handle();
    let (labels, chosen) = h.completions().expect("the list is open");
    assert_eq!(chosen, 0);
    assert_eq!(labels.len(), 3);
    assert!(EDITS.with(Cell::get) >= 1, "the service heard the edit");
    // Typing on filters; Down chooses the next; Enter puts it in.
    let _app = run(
        TEXT,
        &[
            focus(),
            click(at(3, 2.0)),
            typed("p"),
            typed("r"),
            typed("i"),
        ],
    );
    let (labels, _) = handle().completions().expect("still open");
    assert_eq!(labels, ["print", "println"]);
    let _app = run(
        TEXT,
        &[
            focus(),
            click(at(3, 2.0)),
            typed("p"),
            typed("r"),
            typed("i"),
            key(Key::Down),
            key(Key::Enter),
        ],
    );
    let h = handle();
    assert!(h.completions().is_none());
    assert_eq!(h.text(), "fn main() {\n  bad;\n  let x = 1;\n  println\n}");
    assert_eq!(carets(&h), [41]);
    // A trigger character asks too; Escape closes the list.
    let _app = run(TEXT, &[focus(), click(at(3, 2.0)), typed("x"), typed(".")]);
    assert!(handle().completions().is_some());
    assert!(ASKED.with(|a| a.borrow().iter().any(|q| q == "complete 36")));
    let _app = run(
        TEXT,
        &[focus(), click(at(3, 2.0)), typed("p"), key(Key::Escape)],
    );
    assert!(handle().completions().is_none());
    setup(None);
}

#[test]
fn diagnostics_and_inlay_hints_draw_and_a_resting_pointer_shows_the_message() {
    setup(Some(served));
    let theme = viso_code_editor::Theme::default();
    let _app = run(TEXT, &[focus(), wait(100)]);
    let h = handle();
    let drawn = h.drawn();
    let squiggle = drawn[1].over.iter().find(|o| o.3 < 0.0);
    assert_eq!(squiggle.map(|o| o.4), Some(theme.error));
    let texts: Vec<&str> = drawn[2].text.iter().map(|t| t.3.as_str()).collect();
    assert_eq!(texts, ["  let x", ": i32", " = 1;"]);
    // The pointer resting on `bad` shows its message and the service's word.
    let p = at(1, 3.0);
    let rest = vec![mouse(
        p.0,
        p.1,
        RawButtons::NONE,
        RawPhase::Moved,
        RawModifiers::default(),
    )];
    let _app = run(TEXT, &[focus(), wait(100), rest.clone(), wait(800)]);
    assert_eq!(
        handle().hover_text().as_deref(),
        Some("no such name `bad`\n\nlet bad: Missing")
    );
    // A key closes it.
    let _app = run(
        TEXT,
        &[focus(), wait(100), rest, wait(800), key(Key::Right)],
    );
    assert!(handle().hover_text().is_none());
    setup(None);
}

#[test]
fn going_to_a_definition_selects_it() {
    setup(Some(served));
    let _app = run(TEXT, &[focus(), click(at(1, 3.0)), key(Key::F12), wait(50)]);
    assert_eq!(handle().with(|s| s.selections().primary().range()), 3..7);
    setup(None);
}

/// A service whose diagnostics come when the test sends them.
#[derive(Default)]
struct Held {
    waits: RefCell<Vec<Responder<Diagnostics>>>,
}

impl LanguageService for Held {
    fn changed(&self, _: &EditEvent<'_>) {}

    fn diagnostics(&self) -> Reply<Diagnostics> {
        let (tx, rx) = channel();
        self.waits.borrow_mut().push(tx);
        rx
    }
}

thread_local! {
    static HELD: RefCell<Option<Rc<Held>>> = const { RefCell::new(None) };
}

fn held(e: CodeEditor) -> CodeEditor {
    let service = HELD.with(|h| h.borrow().clone()).expect("a service");
    e.service(service)
}

#[test]
fn a_view_built_again_keeps_the_session_and_what_its_service_reports() {
    let service = Rc::new(Held::default());
    HELD.with(|h| *h.borrow_mut() = Some(Rc::clone(&service)));
    setup(Some(held));
    // An edit before `bad`, and the service asked once, not yet answered.
    let first = run(TEXT, &[focus(), click(at(1, 0.0)), typed("q"), wait(50)]);
    assert_eq!(service.waits.borrow().len(), 1);
    drop(first);
    // The answer comes, about the text before the edit, while no view
    // shows the session; the view built again from the text it was built
    // from shows the edited session and the answer carried over the edit.
    let tx = service.waits.borrow_mut().pop().expect("a wait");
    tx.send(Some(Diagnostics {
        version: 0,
        list: vec![Decoration::diagnostic(
            14..17,
            Severity::Error,
            "no such name `bad`",
        )],
    }));
    let _app = run_again(&[wait(50)]);
    let h = handle();
    let marked = h.with(|s| {
        assert!(
            s.text().starts_with("fn main() {\nq  bad;"),
            "the edit stays"
        );
        s.decorations()
            .get(viso_code_editor::decoration::DIAGNOSTICS)
            .iter()
            .map(|d| s.slice(d.range.clone()))
            .collect::<Vec<_>>()
    });
    assert_eq!(marked, ["bad"]);
    assert_eq!(
        service.waits.borrow().len(),
        1,
        "asked again only after the answer"
    );
    // Built again on another service, the session drops the old one's report.
    HELD.with(|h| *h.borrow_mut() = Some(Rc::new(Held::default())));
    let _app = run_again(&[wait(50)]);
    assert!(handle().with(|s| {
        s.decorations()
            .get(viso_code_editor::decoration::DIAGNOSTICS)
            .is_empty()
    }));
    setup(None);
    HELD.with(|h| *h.borrow_mut() = None);
}
