//! A language server client: the editor's [`LanguageService`] spoken as the
//! Language Server Protocol to a server process, or to any pair of streams.
//!
//! Two threads carry the conversation. A writer sends what the editor asks
//! in the order it asks, so the UI thread never waits on a pipe; a reader
//! answers each reply's [`Reply`] and takes the server's diagnostics.
//! Positions are worked out against the [`Snapshot`] a request was made on,
//! in the encoding the server agreed to (UTF-8 when it can, else UTF-16),
//! so an answer comes back as bytes of the text it was about.
//!
//! The text is kept in step the way the server asks: each edit's changes,
//! last first so each holds against the text before it, for incremental
//! sync; or, for full sync, the whole text, sent from the writer thread
//! once for however many edits came since the last — a fast typist does not
//! send the file once a key. Until the server answers `initialize`, edits
//! only move the text it will be opened with and requests wait.

mod wire;

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufReader, Read, Write};
use std::ops::Range;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

use viso_ende::JsonValue;

use crate::decoration::{Decoration, Severity};
use crate::document::{ChangeSet, normalize};
use crate::rope::Rope;
use crate::service::{
    Completion, CompletionKind, Definition, Diagnostics, Hover, LanguageService, Reply, Responder,
    Snapshot, channel,
};
use crate::session::{EditEvent, Inlay, InlayKind};

pub use wire::{Encoding, file_uri};
use wire::{J, obj};

/// How many texts the server was sent are kept, to read what it says of
/// one it has not yet caught up from.
const SENT_KEPT: usize = 32;

/// How the server wants the text kept in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sync {
    None,
    Full,
    Incremental,
}

/// What the server said it does.
#[derive(Debug, Clone)]
struct Caps {
    encoding: wire::Encoding,
    sync: Sync,
    completion: bool,
    triggers: Vec<char>,
    hover: bool,
    definition: bool,
    inlay_hints: bool,
    name: Option<String>,
}

/// What reads a reply's result: `None` for an error.
type Answer = Box<dyn FnOnce(Option<&JsonValue>, wire::Encoding) + Send>;

/// What makes a request once the server's capabilities are known: `None`
/// when the server does not serve it.
type Make = Box<dyn FnOnce(&mut State, &Caps) -> Option<String> + Send>;

/// A request made before the server was ready, sent once it is.
struct Waiting {
    version: u64,
    make: Make,
}

struct State {
    uri: String,
    language: String,
    next_id: i64,
    pending: HashMap<i64, Answer>,
    caps: Option<Caps>,
    waiting: Vec<Waiting>,
    /// The text as the editor has it.
    latest: Snapshot,
    /// The texts the server was sent, oldest first.
    sent: VecDeque<Snapshot>,
    /// Diagnostics not yet asked for, and a wait for them.
    diagnostics: Option<Diagnostics>,
    listener: Option<Responder<Diagnostics>>,
    /// Whether the server's process ended.
    gone: bool,
}

impl State {
    fn sent_at(&self, version: Option<u64>) -> Option<&Snapshot> {
        match version {
            Some(v) => self.sent.iter().rev().find(|s| s.version == v),
            None => None,
        }
        .or_else(|| self.sent.back())
    }

    fn remember(&mut self, snap: Snapshot) {
        if self.sent.len() == SENT_KEPT {
            self.sent.pop_front();
        }
        self.sent.push_back(snap);
    }
}

/// What the writer thread is given.
enum Out {
    Message(String),
    /// The whole text, if the server lacks the latest.
    FullText,
    /// End: the writer closes the server's input.
    Close,
}

struct Shared {
    state: Mutex<State>,
    out: Sender<Out>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, message: String) {
        let _ = self.out.send(Out::Message(message));
    }
}

/// A language server client for one document.
pub struct LspClient {
    shared: Arc<Shared>,
    child: Option<Child>,
}

/// What a client opens its document as.
#[derive(Debug, Clone)]
pub struct OpenDocument<'a> {
    /// The workspace's root, a `file://` URI.
    pub root: Option<&'a str>,
    /// The document's URI.
    pub uri: &'a str,
    /// Its language, as the protocol names them: `rust`, `viso`.
    pub language: &'a str,
    pub text: &'a str,
}

impl LspClient {
    /// Starts `command` with `args` as a server over its stdin and stdout,
    /// opening `doc` in it.
    ///
    /// # Errors
    ///
    /// The process does not start.
    pub fn spawn(command: &Path, args: &[&str], doc: OpenDocument<'_>) -> io::Result<LspClient> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let mut client = LspClient::connect(output, input, doc);
        client.child = Some(child);
        Ok(client)
    }

    /// Talks to a server over `from` and `to`, opening `doc` in it.
    pub fn connect(
        from: impl Read + Send + 'static,
        to: impl Write + Send + 'static,
        doc: OpenDocument<'_>,
    ) -> LspClient {
        let (tx, rx) = mpsc::channel::<Out>();
        let text = normalize(doc.text).into_owned();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                uri: doc.uri.to_owned(),
                language: doc.language.to_owned(),
                next_id: 1,
                pending: HashMap::new(),
                caps: None,
                waiting: Vec::new(),
                latest: Snapshot {
                    text: Rope::from(text.as_str()),
                    version: 0,
                },
                sent: VecDeque::new(),
                diagnostics: None,
                listener: None,
                gone: false,
            }),
            out: tx,
        });
        // The writer: frames what it is given, the whole text when asked.
        let writer = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("lsp-writer".into())
            .spawn(move || {
                let mut to = io::BufWriter::new(to);
                for out in rx {
                    let body = match out {
                        Out::Message(m) => m,
                        Out::FullText => match full_text(&writer) {
                            Some(m) => m,
                            None => continue,
                        },
                        Out::Close => break,
                    };
                    if wire::write_frame(&mut to, &body).is_err() {
                        break;
                    }
                }
            })
            .expect("start the writer thread");
        // The reader: replies, the server's requests, its diagnostics.
        let reader = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("lsp-reader".into())
            .spawn(move || {
                let mut from = BufReader::new(from);
                while let Ok(Some(body)) = wire::read_frame(&mut from) {
                    if let Ok(msg) = JsonValue::parse(&body) {
                        incoming(&reader, &msg);
                    }
                }
                let mut state = reader.state();
                state.gone = true;
                // Every wait ends with no answer.
                let pending: Vec<Answer> = state.pending.drain().map(|(_, a)| a).collect();
                let listener = state.listener.take();
                drop(state);
                for answer in pending {
                    answer(None, wire::Encoding::Utf16);
                }
                drop(listener);
            })
            .expect("start the reader thread");
        let root = doc.root.map_or(J::Null, J::str);
        let folders = match doc.root {
            Some(r) => J::Arr(vec![obj([("uri", J::str(r)), ("name", J::str("root"))])]),
            None => J::Null,
        };
        let params = obj([
            ("processId", J::Int(i64::from(std::process::id()))),
            ("clientInfo", obj([("name", J::str("viso-code-editor"))])),
            ("rootUri", root),
            ("workspaceFolders", folders),
            ("capabilities", client_capabilities()),
        ]);
        let mut state = shared.state();
        let id = state.next_id;
        state.next_id += 1;
        let init = Arc::clone(&shared);
        state
            .pending
            .insert(id, Box::new(move |result, _| initialized(&init, result)));
        drop(state);
        shared.send(wire::request(id, "initialize", params));
        LspClient {
            shared,
            child: None,
        }
    }

    /// Whether the server answered `initialize`.
    pub fn ready(&self) -> bool {
        self.shared.state().caps.is_some()
    }

    /// The name the server gave, once ready.
    pub fn server_name(&self) -> Option<String> {
        self.shared
            .state()
            .caps
            .as_ref()
            .and_then(|c| c.name.clone())
    }

    /// The encoding the server counts columns in, once ready.
    pub fn encoding(&self) -> Option<Encoding> {
        self.shared.state().caps.as_ref().map(|c| c.encoding)
    }

    /// The text the server was last sent, and its version.
    pub fn server_text(&self) -> Option<(u64, String)> {
        let state = self.shared.state();
        state
            .sent
            .back()
            .map(|s| (s.version, s.text.slice(0..s.text.len()).into_owned()))
    }

    /// Sends `method` with `params` as a request, `read` turning its result
    /// into the answer, against the text at `doc`, once the server is
    /// ready; with `wanted` saying whether the server does it.
    fn ask<T: Send + 'static>(
        &self,
        doc: &Snapshot,
        method: &'static str,
        wanted: fn(&Caps) -> bool,
        params: impl FnOnce(&Rope, Encoding, &str) -> J + Send + 'static,
        read: impl FnOnce(&JsonValue, &Rope, Encoding, &str) -> Option<T> + Send + 'static,
    ) -> Reply<T> {
        let (tx, rx) = channel::<T>();
        let snap = doc.clone();
        let shared = Arc::clone(&self.shared);
        let make = move |state: &mut State, caps: &Caps| -> Option<String> {
            if !wanted(caps) {
                return None;
            }
            let id = state.next_id;
            state.next_id += 1;
            let uri = state.uri.clone();
            let text = snap.text.clone();
            let p = params(&text, caps.encoding, &uri);
            state.pending.insert(
                id,
                Box::new(move |result, enc| {
                    tx.send(result.and_then(|r| read(r, &text, enc, &uri)));
                }),
            );
            Some(wire::request(id, method, p))
        };
        let mut state = shared.state();
        if state.gone {
            return rx;
        }
        match state.caps.clone() {
            Some(caps) => {
                if let Some(m) = make(&mut state, &caps) {
                    // Full sync: the text goes before what is asked of it.
                    if caps.sync == Sync::Full {
                        let _ = shared.out.send(Out::FullText);
                    }
                    drop(state);
                    shared.send(m);
                }
            }
            None => state.waiting.push(Waiting {
                version: doc.version,
                make: Box::new(make),
            }),
        }
        rx
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let mut state = self.shared.state();
        let id = state.next_id;
        state.next_id += 1;
        drop(state);
        self.shared.send(wire::request(id, "shutdown", J::Null));
        self.shared.send(wire::notification("exit", J::Null));
        let _ = self.shared.out.send(Out::Close);
        if let Some(mut child) = self.child.take() {
            // A server slow to go is stopped.
            std::thread::spawn(move || {
                for _ in 0..40 {
                    if let Ok(Some(_)) = child.try_wait() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

/// What the client can take.
fn client_capabilities() -> J {
    obj([
        (
            "general",
            obj([(
                "positionEncodings",
                J::Arr(vec![J::str("utf-8"), J::str("utf-16")]),
            )]),
        ),
        (
            "textDocument",
            obj([
                (
                    "synchronization",
                    obj([("dynamicRegistration", J::Bool(false))]),
                ),
                (
                    "completion",
                    obj([("completionItem", obj([("snippetSupport", J::Bool(false))]))]),
                ),
                (
                    "hover",
                    obj([(
                        "contentFormat",
                        J::Arr(vec![J::str("plaintext"), J::str("markdown")]),
                    )]),
                ),
                ("definition", obj([("linkSupport", J::Bool(true))])),
                ("inlayHint", obj([("dynamicRegistration", J::Bool(false))])),
                (
                    "publishDiagnostics",
                    obj([("versionSupport", J::Bool(true))]),
                ),
            ]),
        ),
        ("window", obj([("workDoneProgress", J::Bool(false))])),
    ])
}

/// The server answered `initialize`: take what it does, open the document
/// with the latest text, and send what waited.
fn initialized(shared: &Arc<Shared>, result: Option<&JsonValue>) {
    let caps_json = result.and_then(|r| r.get("capabilities"));
    let has = |key: &str| {
        caps_json
            .and_then(|c| c.get(key))
            .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
    };
    let encoding = match caps_json
        .and_then(|c| c.get("positionEncoding"))
        .and_then(JsonValue::as_str)
    {
        Some("utf-8") => Encoding::Utf8,
        _ => Encoding::Utf16,
    };
    let sync_kind = caps_json
        .and_then(|c| c.get("textDocumentSync"))
        .and_then(|s| {
            s.as_f64()
                .or_else(|| s.get("change").and_then(JsonValue::as_f64))
        });
    let sync = match sync_kind {
        Some(2.0) => Sync::Incremental,
        Some(1.0) => Sync::Full,
        Some(_) => Sync::None,
        None => Sync::Full,
    };
    let triggers = caps_json
        .and_then(|c| c.get("completionProvider"))
        .and_then(|p| p.get("triggerCharacters"))
        .and_then(JsonValue::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str()?.chars().next())
                .collect()
        })
        .unwrap_or_default();
    let caps = Caps {
        encoding,
        sync,
        completion: has("completionProvider"),
        triggers,
        hover: has("hoverProvider"),
        definition: has("definitionProvider"),
        inlay_hints: has("inlayHintProvider"),
        name: result
            .and_then(|r| r.get("serverInfo"))
            .and_then(|i| i.get("name"))
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
    };
    let mut state = shared.state();
    let latest = state.latest.clone();
    let open = wire::notification(
        "textDocument/didOpen",
        obj([(
            "textDocument",
            obj([
                ("uri", J::str(state.uri.clone())),
                ("languageId", J::str(state.language.clone())),
                ("version", J::Int(latest.version as i64)),
                (
                    "text",
                    J::Str(latest.text.slice(0..latest.text.len()).into_owned()),
                ),
            ]),
        )]),
    );
    state.remember(latest.clone());
    state.caps = Some(caps.clone());
    let waiting = std::mem::take(&mut state.waiting);
    let mut out = vec![wire::notification("initialized", obj([])), open];
    for w in waiting {
        // One asked of a text since edited has no answer.
        if w.version == latest.version
            && let Some(m) = (w.make)(&mut state, &caps)
        {
            out.push(m);
        }
    }
    // Sent before the lock goes, so no edit's change goes ahead of them.
    for m in out {
        shared.send(m);
    }
    drop(state);
}

/// The whole text as a change, when the server lacks the latest.
fn full_text(shared: &Shared) -> Option<String> {
    let mut state = shared.state();
    let latest = state.latest.clone();
    if state
        .sent
        .back()
        .is_some_and(|s| s.version >= latest.version)
    {
        return None;
    }
    state.remember(latest.clone());
    let uri = state.uri.clone();
    drop(state);
    Some(wire::notification(
        "textDocument/didChange",
        obj([
            (
                "textDocument",
                obj([
                    ("uri", J::str(uri)),
                    ("version", J::Int(latest.version as i64)),
                ]),
            ),
            (
                "contentChanges",
                J::Arr(vec![obj([(
                    "text",
                    J::Str(latest.text.slice(0..latest.text.len()).into_owned()),
                )])]),
            ),
        ]),
    ))
}

/// One message from the server.
fn incoming(shared: &Arc<Shared>, msg: &JsonValue) {
    let method = msg.get("method").and_then(JsonValue::as_str);
    let id = msg.get("id");
    match (method, id) {
        // A reply.
        (None, Some(id)) => {
            let Some(id) = id.as_f64().map(|n| n as i64) else {
                return;
            };
            let mut state = shared.state();
            let answer = state.pending.remove(&id);
            let enc = state.caps.as_ref().map_or(Encoding::Utf16, |c| c.encoding);
            drop(state);
            if let Some(answer) = answer {
                let result = msg.get("result").filter(|_| msg.get("error").is_none());
                answer(result, enc);
            }
        }
        // A request of the server's: answered so it does not wait.
        (Some(method), Some(id)) => {
            let result = match method {
                "workspace/configuration" => {
                    let n = msg
                        .get("params")
                        .and_then(|p| p.get("items"))
                        .and_then(JsonValue::as_array)
                        .map_or(0, <[JsonValue]>::len);
                    J::Arr(vec![J::Null; n])
                }
                _ => J::Null,
            };
            shared.send(wire::response(id, result));
        }
        (Some("textDocument/publishDiagnostics"), None) => {
            if let Some(params) = msg.get("params") {
                diagnostics(shared, params);
            }
        }
        _ => {}
    }
}

/// The server's diagnostics for the document, read against the text it
/// says they are of.
fn diagnostics(shared: &Arc<Shared>, params: &JsonValue) {
    let mut state = shared.state();
    let uri = params.get("uri").and_then(JsonValue::as_str).unwrap_or("");
    if wire::uri_key(uri) != wire::uri_key(&state.uri) {
        return;
    }
    let version = params
        .get("version")
        .and_then(JsonValue::as_f64)
        .map(|v| v as u64);
    let Some(snap) = state.sent_at(version).cloned() else {
        return;
    };
    let enc = state.caps.as_ref().map_or(Encoding::Utf16, |c| c.encoding);
    let list = params
        .get("diagnostics")
        .and_then(JsonValue::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(|d| {
            let range = wire::span(&snap.text, d.get("range")?, enc)?;
            let severity = match d.get("severity").and_then(JsonValue::as_f64) {
                Some(2.0) => Severity::Warning,
                Some(3.0) => Severity::Info,
                Some(4.0) => Severity::Hint,
                _ => Severity::Error,
            };
            let mut message = d.get("message")?.as_str()?.to_owned();
            if let Some(source) = d.get("source").and_then(JsonValue::as_str) {
                message = format!("{message} ({source})");
            }
            Some(Decoration::diagnostic(range, severity, message))
        })
        .collect();
    let d = Diagnostics {
        version: snap.version,
        list,
    };
    match state.listener.take() {
        Some(tx) => {
            drop(state);
            tx.send(Some(d));
        }
        None => state.diagnostics = Some(d),
    }
}

/// The text of a hover's contents: plain or markdown, one or many.
fn hover_text(contents: &JsonValue) -> String {
    match contents {
        JsonValue::String(s) => strip_fences(s),
        JsonValue::Array(items) => items
            .iter()
            .map(hover_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        other => other
            .get("value")
            .and_then(JsonValue::as_str)
            .map(strip_fences)
            .unwrap_or_default(),
    }
}

/// Markdown's code fences taken out, their code kept.
fn strip_fences(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn completion_kind(n: f64) -> CompletionKind {
    use CompletionKind as K;
    const KINDS: [CompletionKind; 25] = [
        K::Text,
        K::Method,
        K::Function,
        K::Constructor,
        K::Field,
        K::Variable,
        K::Class,
        K::Interface,
        K::Module,
        K::Property,
        K::Unit,
        K::Value,
        K::Enum,
        K::Keyword,
        K::Snippet,
        K::Color,
        K::File,
        K::Reference,
        K::Folder,
        K::EnumMember,
        K::Constant,
        K::Struct,
        K::Event,
        K::Operator,
        K::TypeParameter,
    ];
    KINDS
        .get((n as usize).wrapping_sub(1))
        .copied()
        .unwrap_or_default()
}

fn completion(item: &JsonValue, text: &Rope, enc: Encoding) -> Option<Completion> {
    let label = item.get("label")?.as_str()?.to_owned();
    let get = |k: &str| item.get(k).and_then(JsonValue::as_str).map(str::to_owned);
    let edit = item.get("textEdit");
    let (range, insert) = match edit {
        Some(e) => {
            let r = e.get("range").or_else(|| e.get("replace"));
            (
                r.and_then(|r| wire::span(text, r, enc)),
                e.get("newText")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned),
            )
        }
        None => (None, None),
    };
    let insert = insert
        .or_else(|| get("insertText"))
        .unwrap_or_else(|| label.clone());
    Some(Completion {
        detail: get("detail"),
        kind: item
            .get("kind")
            .and_then(JsonValue::as_f64)
            .map_or_else(CompletionKind::default, completion_kind),
        insert,
        range,
        filter: get("filterText").unwrap_or_default(),
        sort: get("sortText").unwrap_or_default(),
        label,
    })
}

fn document_id(uri: &str) -> J {
    obj([("uri", J::str(uri))])
}

fn at_position(text: &Rope, at: usize, enc: Encoding, uri: &str) -> J {
    obj([
        ("textDocument", document_id(uri)),
        ("position", wire::position(text, at, enc)),
    ])
}

impl LanguageService for LspClient {
    fn changed(&self, event: &EditEvent<'_>) {
        let mut state = self.shared.state();
        let snap = Snapshot {
            text: event.after.text().clone(),
            version: event.after.version(),
        };
        state.latest = snap.clone();
        let Some(caps) = state.caps.clone() else {
            // Opened later with the latest text.
            return;
        };
        match caps.sync {
            Sync::None => {}
            Sync::Full => {
                drop(state);
                let _ = self.shared.out.send(Out::FullText);
            }
            Sync::Incremental => {
                let uri = state.uri.clone();
                state.remember(snap.clone());
                drop(state);
                let message = incremental(&uri, event.before, event.changes, &snap, caps.encoding);
                self.shared.send(message);
            }
        }
    }

    fn diagnostics(&self) -> Reply<Diagnostics> {
        let (tx, rx) = channel();
        let mut state = self.shared.state();
        if let Some(d) = state.diagnostics.take() {
            drop(state);
            tx.send(Some(d));
        } else if !state.gone {
            state.listener = Some(tx);
        }
        rx
    }

    fn completions(&self, doc: &Snapshot, at: usize) -> Reply<Vec<Completion>> {
        self.ask(
            doc,
            "textDocument/completion",
            |c| c.completion,
            move |text, enc, uri| at_position(text, at, enc, uri),
            |result, text, enc, _| {
                let items = result
                    .as_array()
                    .or_else(|| result.get("items").and_then(JsonValue::as_array))?;
                Some(
                    items
                        .iter()
                        .filter_map(|i| completion(i, text, enc))
                        .collect(),
                )
            },
        )
    }

    fn hover(&self, doc: &Snapshot, at: usize) -> Reply<Hover> {
        self.ask(
            doc,
            "textDocument/hover",
            |c| c.hover,
            move |text, enc, uri| at_position(text, at, enc, uri),
            |result, text, enc, _| {
                if result.is_null() {
                    return None;
                }
                Some(Hover {
                    text: hover_text(result.get("contents")?),
                    range: result.get("range").and_then(|r| wire::span(text, r, enc)),
                })
            },
        )
    }

    fn definition(&self, doc: &Snapshot, at: usize) -> Reply<Vec<Definition>> {
        self.ask(
            doc,
            "textDocument/definition",
            |c| c.definition,
            move |text, enc, uri| at_position(text, at, enc, uri),
            |result, text, enc, uri| {
                let list: Vec<&JsonValue> = match result {
                    JsonValue::Array(a) => a.iter().collect(),
                    JsonValue::Null => Vec::new(),
                    one => vec![one],
                };
                let here = wire::uri_key(uri);
                Some(
                    list.into_iter()
                        .filter_map(|l| {
                            let target = l.get("targetUri").or_else(|| l.get("uri"))?.as_str()?;
                            let range = l.get("targetSelectionRange").or_else(|| l.get("range"))?;
                            if wire::uri_key(target) == here {
                                return wire::span(text, range, enc).map(Definition::Here);
                            }
                            let start = range.get("start")?;
                            Some(Definition::Elsewhere {
                                uri: target.to_owned(),
                                line: start.get("line")?.as_f64()? as u32,
                                column: start.get("character")?.as_f64()? as u32,
                            })
                        })
                        .collect(),
                )
            },
        )
    }

    fn inlay_hints(&self, doc: &Snapshot, range: Range<usize>) -> Reply<Vec<Inlay>> {
        self.ask(
            doc,
            "textDocument/inlayHint",
            |c| c.inlay_hints,
            move |text, enc, uri| {
                obj([
                    ("textDocument", document_id(uri)),
                    ("range", wire::range(text, range, enc)),
                ])
            },
            |result, text, enc, _| {
                Some(
                    result
                        .as_array()
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(|h| {
                            let at = wire::offset(text, h.get("position")?, enc)?;
                            let label = match h.get("label")? {
                                JsonValue::String(s) => s.clone(),
                                JsonValue::Array(parts) => parts
                                    .iter()
                                    .filter_map(|p| p.get("value")?.as_str())
                                    .collect(),
                                _ => return None,
                            };
                            let kind = match h.get("kind").and_then(JsonValue::as_f64) {
                                Some(1.0) => InlayKind::Type,
                                Some(2.0) => InlayKind::Parameter,
                                _ => InlayKind::Other,
                            };
                            Some(Inlay {
                                at,
                                text: label.trim().to_owned(),
                                kind,
                            })
                        })
                        .collect(),
                )
            },
        )
    }

    fn completion_triggers(&self) -> Vec<char> {
        self.shared
            .state()
            .caps
            .as_ref()
            .map(|c| c.triggers.clone())
            .unwrap_or_default()
    }
}

/// An edit as incremental changes: last first, each in the text before
/// the edit, which the ones after it in the text leave where it was.
fn incremental(
    uri: &str,
    before: &Rope,
    changes: &ChangeSet,
    after: &Snapshot,
    enc: Encoding,
) -> String {
    let list = changes
        .changes()
        .iter()
        .rev()
        .map(|c| {
            obj([
                ("range", wire::range(before, c.range.clone(), enc)),
                ("text", J::str(c.text.as_str())),
            ])
        })
        .collect();
    wire::notification(
        "textDocument/didChange",
        obj([
            (
                "textDocument",
                obj([
                    ("uri", J::str(uri)),
                    ("version", J::Int(after.version as i64)),
                ]),
            ),
            ("contentChanges", J::Arr(list)),
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_markdown_keeps_the_code_without_its_fences() {
        let v = JsonValue::parse(
            r#"{"kind":"markdown","value":"```rust\nfn f() -> i32\n```\n---\nDoes f."}"#,
        )
        .unwrap();
        assert_eq!(hover_text(&v), "fn f() -> i32\n---\nDoes f.");
    }

    #[test]
    fn a_completion_reads_its_edit_against_the_text() {
        let text = Rope::from("let v = ve");
        let item = JsonValue::parse(
            r#"{"label":"vec!","kind":3,"textEdit":{"range":{"start":{"line":0,"character":8},"end":{"line":0,"character":10}},"newText":"vec![]"}}"#,
        )
        .unwrap();
        let c = completion(&item, &text, Encoding::Utf16).unwrap();
        assert_eq!(c.range, Some(8..10));
        assert_eq!(c.insert, "vec![]");
        assert_eq!(c.kind, CompletionKind::Function);
    }
}
