//! The language server client against real servers: `viso-lsp` run in
//! this process over pipes, and rust-analyzer when one is installed.

#![cfg(all(
    feature = "lsp",
    not(any(target_arch = "wasm32", target_os = "ios", target_os = "android"))
))]

mod common;

use std::io::BufReader;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

use common::*;
use viso_code_editor::decoration::{DIAGNOSTICS, Severity};
use viso_code_editor::lsp::{Encoding, LspClient, OpenDocument};
use viso_code_editor::service::{Definition, LanguageService, Reply, Snapshot};
use viso_code_editor::session::EditEvent;
use viso_code_editor::{Change, ChangeSet, CodeEditor, Document};

/// Waits for `reply` on this thread.
fn block_on<T>(mut reply: Reply<T>) -> Option<T> {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Poll::Ready(v) = reply.as_mut().poll(&mut cx) {
            return v;
        }
        assert!(Instant::now() < deadline, "no answer in time");
        std::thread::park_timeout(Duration::from_millis(50));
    }
}

fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ok() {
        assert!(Instant::now() < deadline, "{what} in time");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `viso-lsp` serving over a pair of pipes on a thread of its own.
fn viso_lsp(uri: &str, text: &str) -> LspClient {
    let (server_in, client_out) = std::io::pipe().unwrap();
    let (client_in, server_out) = std::io::pipe().unwrap();
    std::thread::spawn(move || {
        let mut from = BufReader::new(server_in);
        let mut to = server_out;
        let mut server = viso_lsp::Server::new();
        while let Ok(Some(msg)) = viso_lsp::rpc::read_message(&mut from) {
            let (out, exit) = server.handle(&msg);
            for m in out.reply.iter().chain(&out.notifications) {
                if viso_lsp::rpc::write_message(&mut to, m).is_err() {
                    return;
                }
            }
            if exit {
                return;
            }
        }
    });
    let client = LspClient::connect(
        client_in,
        client_out,
        OpenDocument {
            root: None,
            uri,
            language: "viso",
            text,
        },
    );
    until("initialize answered", || client.ready());
    client
}

const SOURCE: &str = "component C {\n  state count = 0;\n  computed d = count;\n  view { Text(missing) }\n}\n";

fn snapshot(doc: &Document) -> Snapshot {
    Snapshot {
        text: doc.text().clone(),
        version: doc.version(),
    }
}

#[test]
fn viso_lsp_reports_diagnostics_hovers_and_definitions_and_follows_edits() {
    let client = viso_lsp("file:///work/c.vs", SOURCE);
    assert_eq!(client.encoding(), Some(Encoding::Utf16));
    let mut doc = Document::new(SOURCE);
    // The diagnostic on `missing`, as bytes of the text.
    let d = block_on(client.diagnostics()).expect("diagnostics");
    assert_eq!(d.version, 0);
    let unknown = d
        .list
        .iter()
        .find(|d| &SOURCE[d.range.clone()] == "missing")
        .expect("an unknown name");
    eprintln!("diagnostic: {:?}", unknown.message);
    let unknown_message = unknown.message.clone();
    // A hover on it explains it; a definition from the use lands on the
    // declaration.
    let at = SOURCE.find("missing").unwrap() + 2;
    let hover = block_on(client.hover(&snapshot(&doc), at)).expect("a hover");
    assert!(hover.text.contains("E2001"), "{}", hover.text);
    let use_at = SOURCE.find("= count").unwrap() + 3;
    let defs = block_on(client.definition(&snapshot(&doc), use_at)).expect("a definition");
    let Definition::Here(range) = &defs[0] else {
        panic!("{defs:?}");
    };
    assert_eq!(&SOURCE[range.clone()], "count");
    assert!(range.start < SOURCE.find("computed").unwrap());
    // No completions from a server that has none.
    assert!(block_on(client.completions(&snapshot(&doc), 0)).is_none());
    // An edit fixing the name: the server gets the text, and the error goes.
    let before = doc.text().clone();
    let cs = ChangeSet::one(Change::replace(
        SOURCE.find("missing").unwrap()..SOURCE.find("missing").unwrap() + 7,
        "\"hi\"",
    ));
    doc.apply(&cs);
    client.changed(&EditEvent {
        before: &before,
        changes: &cs,
        after: &doc,
    });
    let d = block_on(client.diagnostics()).expect("new diagnostics");
    assert!(
        !d.list.iter().any(|x| x.message == unknown_message),
        "{:?}",
        d.list
    );
    until("the server has the edit", || {
        client
            .server_text()
            .is_some_and(|(v, t)| v == doc.version() && t == doc.to_text())
    });
}

thread_local! {
    static CLIENT: std::cell::RefCell<Option<Rc<LspClient>>> = const { std::cell::RefCell::new(None) };
}

/// The editor served by the test's one `viso-lsp`, whichever build of the
/// view asks.
fn served(e: CodeEditor) -> CodeEditor {
    let client = CLIENT.with(|c| c.borrow().clone()).expect("a client");
    e.service(client)
}

fn wait(ms: u64) -> Vec<RawEvent> {
    (0..ms.div_ceil(16) + 1)
        .flat_map(|_| [RawEvent::Wakeup, redraw()])
        .collect()
}

#[test]
fn the_editor_shows_what_viso_lsp_says() {
    let client = Rc::new(viso_lsp("file:///work/e.vs", SOURCE));
    CLIENT.with(|c| *c.borrow_mut() = Some(client));
    setup(Some(served));
    let _app = run(SOURCE, &[focus(), wait(200)]);
    shows_the_report();
    // A view built again shows the session the last one showed, with what
    // the server reported to it: the server does not report again.
    drop(_app);
    let _app = run_again(&[wait(50)]);
    shows_the_report();
    // Built again before the server answers, the view takes up the wait
    // the session has in flight.
    let client = Rc::new(viso_lsp("file:///work/e.vs", SOURCE));
    CLIENT.with(|c| *c.borrow_mut() = Some(client));
    let first = run(SOURCE, &[]);
    drop(first);
    let _app = run_again(&[wait(200)]);
    shows_the_report();
    setup(None);
    CLIENT.with(|c| *c.borrow_mut() = None);
}

/// Whether the editor shows `viso-lsp`'s error at `missing`, squiggled.
fn shows_the_report() {
    let h = handle();
    let errors = h.with(|s| {
        s.decorations()
            .get(DIAGNOSTICS)
            .iter()
            .filter(|d| {
                matches!(
                    d.style,
                    viso_code_editor::decoration::DecorationStyle::Diagnostic(Severity::Error)
                )
            })
            .map(|d| s.slice(d.range.clone()))
            .collect::<Vec<_>>()
    });
    assert!(errors.iter().any(|e| e == "missing"), "{errors:?}");
    let drawn = h.drawn();
    assert!(
        drawn[3].over.iter().any(|o| o.3 < 0.0),
        "a squiggle on line 4"
    );
}

/// rust-analyzer, from `VISO_RUST_ANALYZER` or the editors that ship it.
fn rust_analyzer() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("VISO_RUST_ANALYZER") {
        return Some(p.into());
    }
    let home = std::env::var_os("HOME")?;
    let ext = std::path::Path::new(&home).join(".vscode/extensions");
    std::fs::read_dir(ext)
        .ok()?
        .flatten()
        .map(|e| e.path().join("server/rust-analyzer"))
        .find(|p| p.is_file())
}

#[test]
fn rust_analyzer_completes_hovers_hints_and_follows_incremental_edits() {
    let Some(ra) = rust_analyzer() else {
        eprintln!("no rust-analyzer: skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("viso-ra-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"ra_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    let source = "fn main() {\n    let names = vec![String::from(\"a\")];\n    let n = names.len();\n    println!(\"{n}\");\n}\n";
    std::fs::write(dir.join("src/main.rs"), source).unwrap();
    let root = viso_code_editor::lsp::file_uri(&dir);
    let uri = viso_code_editor::lsp::file_uri(&dir.join("src/main.rs"));
    let client = LspClient::spawn(
        &ra,
        &[],
        OpenDocument {
            root: Some(&root),
            uri: &uri,
            language: "rust",
            text: source,
        },
    )
    .expect("start rust-analyzer");
    until("rust-analyzer ready", || client.ready());
    eprintln!(
        "server {:?}, columns in {:?}",
        client.server_name(),
        client.encoding()
    );
    assert_eq!(client.encoding(), Some(Encoding::Utf8), "UTF-8 negotiated");
    let mut doc = Document::new(source);
    // A hover, retried while the workspace loads.
    let at = source.find("names.len").unwrap() + 2;
    let deadline = Instant::now() + Duration::from_secs(90);
    let hover = loop {
        if let Some(h) = block_on(client.hover(&snapshot(&doc), at))
            && h.text.contains("Vec")
        {
            break h;
        }
        assert!(Instant::now() < deadline, "a hover in time");
        std::thread::sleep(Duration::from_millis(300));
    };
    eprintln!("hover: {}", hover.text.lines().next().unwrap_or(""));
    // The inlay hint after `n` says its type.
    let hints = block_on(client.inlay_hints(&snapshot(&doc), 0..source.len())).unwrap_or_default();
    let n_at = source.find("let n ").unwrap() + 5;
    assert!(
        hints
            .iter()
            .any(|h| h.at == n_at && h.text.contains("usize")),
        "{hints:?}"
    );
    // Two edits made incrementally, then completions after the new `.`.
    let edit = |doc: &mut Document, change: Change| {
        let before = doc.text().clone();
        let cs = ChangeSet::one(change);
        doc.apply(&cs);
        client.changed(&EditEvent {
            before: &before,
            changes: &cs,
            after: doc,
        });
    };
    let line_end = source.find("println").unwrap();
    edit(&mut doc, Change::insert(line_end, "names.\n    "));
    edit(&mut doc, Change::insert(0, "// probe\n"));
    let dot = doc.to_text().find("names.\n").unwrap() + "names.".len();
    let items = block_on(client.completions(&snapshot(&doc), dot)).expect("completions");
    let labels: Vec<&str> = items.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.iter().any(|l| l.starts_with("push")), "{labels:?}");
    let len = items
        .iter()
        .find(|c| c.label.starts_with("len"))
        .expect("len");
    if let Some(r) = &len.range {
        assert_eq!(r.start, dot, "the edit starts after the dot");
    }
    // The server's copy of the text is the editor's: a definition from the
    // new line lands on `names` in the line moved down by the comment.
    let defs = block_on(client.definition(&snapshot(&doc), dot - 3)).unwrap_or_default();
    let text = doc.to_text();
    let Some(Definition::Here(r)) = defs.first() else {
        panic!("{defs:?}");
    };
    assert_eq!(&text[r.clone()], "names");
    assert_eq!(r.start, text.find("let names").unwrap() + 4);
    drop(client);
    let _ = std::fs::remove_dir_all(&dir);
}
