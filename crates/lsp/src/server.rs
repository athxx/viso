//! The language-server request dispatcher: JSON-RPC message → engine call → JSON-RPC
//! reply, plus the open-document bookkeeping the protocol requires.
//!
//! This is the thin translation layer the crate's two-layer design puts *above* the
//! pure [`engine`](crate::engine). It owns no analysis logic: every request is turned
//! into byte offsets (via [`crate::position`]), handed to an engine function, and the
//! resulting spans/edits are turned back into UTF-16 protocol shapes. It also tracks
//! the URI→[`FileId`](crate::source_map::FileId) mapping and drives the
//! [`SourceMap`](crate::source_map::SourceMap) on `didOpen`/`didChange`/`didClose`.
//!
//! [`Server::handle`] is a pure function of `(server state, request) → optional
//! reply` plus any diagnostics to publish, so the whole protocol surface is unit-
//! testable without a transport. The bin only wires stdin/stdout framing to it.
//!
//! Cold-path throughout (AGENTS 7.2): a handful of these run per editor keystroke.

use std::collections::BTreeMap;
use std::collections::HashMap;

use viso_dsl::diag::{Diagnostic, Severity};

use crate::engine;
use crate::position::{self, LspPosition, LspRange};
use crate::rpc::Json;
use crate::source_map::{FileId, SourceMap};

/// What the server produced from one incoming message: an optional reply (requests
/// get one, notifications do not) and any `publishDiagnostics` notifications to send.
#[derive(Debug, Default)]
pub struct Outbound {
    /// The JSON-RPC response to the request, if the message was a request.
    pub reply: Option<Json>,
    /// `textDocument/publishDiagnostics` notifications to send after the reply.
    pub notifications: Vec<Json>,
}

/// The running language-server state: the open-document store and the URI↔id map.
#[derive(Default)]
pub struct Server {
    docs: SourceMap,
    /// The document URI each open [`FileId`] was opened under, for reverse lookup.
    uri_of: HashMap<FileId, String>,
    /// The [`FileId`] each open URI maps to.
    id_of: HashMap<String, FileId>,
}

impl Server {
    /// A fresh server with no open documents.
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles one incoming JSON-RPC message, returning the reply and any diagnostics
    /// to publish. Returns `(Outbound, should_exit)`.
    pub fn handle(&mut self, msg: &Json) -> (Outbound, bool) {
        let method = msg.get("method").and_then(Json::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params");

        match method {
            "initialize" => (
                Outbound {
                    reply: Some(response(id, initialize_result())),
                    notifications: Vec::new(),
                },
                false,
            ),
            "initialized" => (Outbound::default(), false),
            // `shutdown` acknowledges with a null result; the loop stops only on the
            // subsequent `exit` notification, per the LSP lifecycle.
            "shutdown" => (
                Outbound {
                    reply: Some(response(id, Json::Null)),
                    notifications: Vec::new(),
                },
                false,
            ),
            "exit" => (Outbound::default(), true),
            "textDocument/didOpen" => (self.did_open(params), false),
            "textDocument/didChange" => (self.did_change(params), false),
            "textDocument/didClose" => (self.did_close(params), false),
            "textDocument/definition" => (
                Outbound {
                    reply: Some(response(id, self.definition(params))),
                    notifications: Vec::new(),
                },
                false,
            ),
            "textDocument/references" => (
                Outbound {
                    reply: Some(response(id, self.references(params))),
                    notifications: Vec::new(),
                },
                false,
            ),
            "textDocument/rename" => (
                Outbound {
                    reply: Some(response(id, self.rename(params))),
                    notifications: Vec::new(),
                },
                false,
            ),
            "textDocument/formatting" => (
                Outbound {
                    reply: Some(response(id, self.formatting(params))),
                    notifications: Vec::new(),
                },
                false,
            ),
            // An unknown *request* (has an id) gets a MethodNotFound error; an unknown
            // notification is silently ignored, per the JSON-RPC spec.
            _ => {
                if let Some(id) = id {
                    (
                        Outbound {
                            reply: Some(error_response(id, -32601, "method not found")),
                            notifications: Vec::new(),
                        },
                        false,
                    )
                } else {
                    (Outbound::default(), false)
                }
            }
        }
    }

    // --- document lifecycle -------------------------------------------------

    fn did_open(&mut self, params: Option<&Json>) -> Outbound {
        let Some((uri, text)) = params.and_then(|p| {
            let td = p.get("textDocument")?;
            let uri = td.get("uri")?.as_str()?.to_string();
            let text = td.get("text")?.as_str()?.to_string();
            Some((uri, text))
        }) else {
            return Outbound::default();
        };
        let module_path = module_path_of(&uri);
        let id = self.docs.open(
            text,
            &module_path.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        self.uri_of.insert(id, uri.clone());
        self.id_of.insert(uri.clone(), id);
        self.publish_for(&uri)
    }

    fn did_change(&mut self, params: Option<&Json>) -> Outbound {
        let Some((uri, text)) = params.and_then(|p| {
            let uri = p.get("textDocument")?.get("uri")?.as_str()?.to_string();
            // Full-document sync: the last content change carries the whole text.
            let changes = p.get("contentChanges")?.as_arr()?;
            let text = changes.last()?.get("text")?.as_str()?.to_string();
            Some((uri, text))
        }) else {
            return Outbound::default();
        };
        let Some(&id) = self.id_of.get(&uri) else {
            return Outbound::default();
        };
        let module_path = module_path_of(&uri);
        self.docs.update(
            id,
            text,
            &module_path.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        self.publish_for(&uri)
    }

    fn did_close(&mut self, params: Option<&Json>) -> Outbound {
        let Some(uri) = params.and_then(|p| p.get("textDocument")?.get("uri")?.as_str()) else {
            return Outbound::default();
        };
        if let Some(id) = self.id_of.remove(uri) {
            self.docs.close(id);
            self.uri_of.remove(&id);
        }
        // On close, clear the client's diagnostics for the file.
        Outbound {
            reply: None,
            notifications: vec![publish_diagnostics(uri, Vec::new())],
        }
    }

    // --- language features --------------------------------------------------

    fn definition(&self, params: Option<&Json>) -> Json {
        let Some((id, offset)) = self.locate(params) else {
            return Json::Null;
        };
        let doc = self.docs.get(id).expect("located doc is open");
        match engine::goto_definition(doc, offset) {
            Some(loc) => {
                let uri = self.uri_of.get(&id).cloned().unwrap_or_default();
                location_json(&uri, position::to_lsp_range(&doc.line_index, loc.range))
            }
            None => Json::Null,
        }
    }

    fn references(&self, params: Option<&Json>) -> Json {
        let Some((id, offset)) = self.locate(params) else {
            return Json::Arr(Vec::new());
        };
        let include_decl = params
            .and_then(|p| p.get("context"))
            .and_then(|c| c.get("includeDeclaration"))
            .map(|v| matches!(v, Json::Bool(true)))
            .unwrap_or(true);
        let doc = self.docs.get(id).expect("located doc is open");
        let uri = self.uri_of.get(&id).cloned().unwrap_or_default();
        let locs: Vec<Json> = engine::find_references(doc, offset, include_decl)
            .into_iter()
            .map(|loc| location_json(&uri, position::to_lsp_range(&doc.line_index, loc.range)))
            .collect();
        Json::Arr(locs)
    }

    fn rename(&self, params: Option<&Json>) -> Json {
        let Some((id, offset)) = self.locate(params) else {
            return Json::Null;
        };
        let Some(new_name) = params.and_then(|p| p.get("newName")).and_then(Json::as_str) else {
            return Json::Null;
        };
        let doc = self.docs.get(id).expect("located doc is open");
        match engine::rename(doc, offset, new_name) {
            Ok(edits) => {
                let uri = self.uri_of.get(&id).cloned().unwrap_or_default();
                let text_edits: Vec<Json> = edits
                    .into_iter()
                    .map(|e| {
                        text_edit_json(
                            position::to_lsp_range(&doc.line_index, e.range),
                            &e.new_text,
                        )
                    })
                    .collect();
                // WorkspaceEdit: { changes: { <uri>: [TextEdit, ...] } }
                let mut per_uri = BTreeMap::new();
                per_uri.insert(uri, Json::Arr(text_edits));
                let mut changes = BTreeMap::new();
                changes.insert("changes".to_string(), Json::Obj(per_uri));
                Json::Obj(changes)
            }
            // A refused rename returns null (no edit); the message is not surfaced as a
            // protocol error, matching editor expectations for an invalid target.
            Err(_) => Json::Null,
        }
    }

    fn formatting(&self, params: Option<&Json>) -> Json {
        let Some(uri) = params.and_then(|p| p.get("textDocument")?.get("uri")?.as_str()) else {
            return Json::Arr(Vec::new());
        };
        let Some(&id) = self.id_of.get(uri) else {
            return Json::Arr(Vec::new());
        };
        let doc = self.docs.get(id).expect("mapped doc is open");
        let formatted = engine::format(&doc.source);
        if formatted == doc.source {
            return Json::Arr(Vec::new());
        }
        // A single edit replacing the whole document with its formatted text. The end
        // position is the document's end, computed from the current text.
        let end = position::to_lsp_position(
            &doc.line_index,
            viso_dsl::TextSize::new(doc.source.len() as u32),
        );
        let full = LspRange {
            start: LspPosition {
                line: 0,
                character: 0,
            },
            end,
        };
        Json::Arr(vec![text_edit_json(full, &formatted)])
    }

    // --- shared helpers -----------------------------------------------------

    /// Resolves a `textDocument`/`position` request to `(FileId, byte offset)`.
    fn locate(&self, params: Option<&Json>) -> Option<(FileId, viso_dsl::TextSize)> {
        let params = params?;
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let &id = self.id_of.get(uri)?;
        let pos = params.get("position")?;
        let line = pos.get("line")?.as_u32()?;
        let character = pos.get("character")?.as_u32()?;
        let doc = self.docs.get(id)?;
        let offset = position::from_lsp_position(&doc.source, LspPosition { line, character });
        Some((id, offset))
    }

    /// Builds the `publishDiagnostics` notification for one open URI from its current
    /// parse and resolve diagnostics.
    fn publish_for(&self, uri: &str) -> Outbound {
        let Some(&id) = self.id_of.get(uri) else {
            return Outbound::default();
        };
        let Some(doc) = self.docs.get(id) else {
            return Outbound::default();
        };
        let mut items = Vec::new();
        for d in doc.parse.errors.iter().chain(doc.resolved.errors.iter()) {
            items.push(diagnostic_json(
                position::to_lsp_range(&doc.line_index, d.primary),
                d,
            ));
        }
        Outbound {
            reply: None,
            notifications: vec![publish_diagnostics(uri, items)],
        }
    }
}

/// The `initialize` result advertising the capabilities this server implements.
fn initialize_result() -> Json {
    let mut caps = BTreeMap::new();
    // Full-document text sync (TextDocumentSyncKind.Full = 1).
    caps.insert("textDocumentSync".to_string(), Json::Num(1.0));
    caps.insert("definitionProvider".to_string(), Json::Bool(true));
    caps.insert("referencesProvider".to_string(), Json::Bool(true));
    caps.insert("renameProvider".to_string(), Json::Bool(true));
    caps.insert("documentFormattingProvider".to_string(), Json::Bool(true));
    let mut result = BTreeMap::new();
    result.insert("capabilities".to_string(), Json::Obj(caps));
    Json::Obj(result)
}

/// Wraps a successful result in a JSON-RPC response envelope.
fn response(id: Option<Json>, result: Json) -> Json {
    let mut m = BTreeMap::new();
    m.insert("jsonrpc".to_string(), Json::Str("2.0".to_string()));
    m.insert("id".to_string(), id.unwrap_or(Json::Null));
    m.insert("result".to_string(), result);
    Json::Obj(m)
}

/// Wraps an error in a JSON-RPC response envelope.
fn error_response(id: Json, code: i64, message: &str) -> Json {
    let mut err = BTreeMap::new();
    err.insert("code".to_string(), Json::Num(code as f64));
    err.insert("message".to_string(), Json::Str(message.to_string()));
    let mut m = BTreeMap::new();
    m.insert("jsonrpc".to_string(), Json::Str("2.0".to_string()));
    m.insert("id".to_string(), id);
    m.insert("error".to_string(), Json::Obj(err));
    Json::Obj(m)
}

/// A `textDocument/publishDiagnostics` notification for one URI.
fn publish_diagnostics(uri: &str, diagnostics: Vec<Json>) -> Json {
    let mut params = BTreeMap::new();
    params.insert("uri".to_string(), Json::Str(uri.to_string()));
    params.insert("diagnostics".to_string(), Json::Arr(diagnostics));
    let mut m = BTreeMap::new();
    m.insert("jsonrpc".to_string(), Json::Str("2.0".to_string()));
    m.insert(
        "method".to_string(),
        Json::Str("textDocument/publishDiagnostics".to_string()),
    );
    m.insert("params".to_string(), Json::Obj(params));
    Json::Obj(m)
}

/// A protocol `Position` from an [`LspPosition`].
fn position_json(pos: LspPosition) -> Json {
    let mut m = BTreeMap::new();
    m.insert("line".to_string(), Json::Num(pos.line as f64));
    m.insert("character".to_string(), Json::Num(pos.character as f64));
    Json::Obj(m)
}

/// A protocol `Range` from an [`LspRange`].
fn range_json(range: LspRange) -> Json {
    let mut m = BTreeMap::new();
    m.insert("start".to_string(), position_json(range.start));
    m.insert("end".to_string(), position_json(range.end));
    Json::Obj(m)
}

/// A protocol `Location` (uri + range).
fn location_json(uri: &str, range: LspRange) -> Json {
    let mut m = BTreeMap::new();
    m.insert("uri".to_string(), Json::Str(uri.to_string()));
    m.insert("range".to_string(), range_json(range));
    Json::Obj(m)
}

/// A protocol `TextEdit` (range + newText).
fn text_edit_json(range: LspRange, new_text: &str) -> Json {
    let mut m = BTreeMap::new();
    m.insert("range".to_string(), range_json(range));
    m.insert("newText".to_string(), Json::Str(new_text.to_string()));
    Json::Obj(m)
}

/// A protocol `Diagnostic` from a frontend [`Diagnostic`] and its converted range.
fn diagnostic_json(range: LspRange, d: &Diagnostic) -> Json {
    let mut m = BTreeMap::new();
    m.insert("range".to_string(), range_json(range));
    m.insert(
        "severity".to_string(),
        Json::Num(lsp_severity(d.severity) as f64),
    );
    m.insert("code".to_string(), Json::Str(d.code.to_string()));
    m.insert("source".to_string(), Json::Str("viso".to_string()));
    m.insert("message".to_string(), Json::Str(d.message.clone()));
    Json::Obj(m)
}

/// Maps a frontend [`Severity`] to the LSP DiagnosticSeverity integer.
fn lsp_severity(severity: Severity) -> u8 {
    match severity {
        Severity::Error => 1,
        Severity::Warning => 2,
        Severity::Note => 3,
    }
}

/// Derives a module path from a document URI.
///
/// The single-document server resolves each file as a one-segment module named after
/// its file stem (the last path component without extension), which is enough for the
/// intra-document goto/references/rename this slice provides. Cross-module resolution
/// across files is deferred (see the crate docs / todo).
fn module_path_of(uri: &str) -> Vec<String> {
    let stem = uri
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(uri)
        .rsplit_once('.')
        .map(|(name, _ext)| name)
        .unwrap_or(uri);
    vec![if stem.is_empty() {
        "root".to_string()
    } else {
        stem.to_string()
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::json;

    fn req(id: i64, method: &str, params: Json) -> Json {
        let mut m = BTreeMap::new();
        m.insert("jsonrpc".to_string(), Json::Str("2.0".to_string()));
        m.insert("id".to_string(), Json::Num(id as f64));
        m.insert("method".to_string(), Json::Str(method.to_string()));
        m.insert("params".to_string(), params);
        Json::Obj(m)
    }

    fn notif(method: &str, params: Json) -> Json {
        let mut m = BTreeMap::new();
        m.insert("jsonrpc".to_string(), Json::Str("2.0".to_string()));
        m.insert("method".to_string(), Json::Str(method.to_string()));
        m.insert("params".to_string(), params);
        Json::Obj(m)
    }

    fn open(uri: &str, text: &str) -> Json {
        notif(
            "textDocument/didOpen",
            json::parse(&format!(
                r#"{{"textDocument":{{"uri":"{uri}","text":{}}}}}"#,
                Json::Str(text.to_string())
            ))
            .unwrap(),
        )
    }

    fn pos_params(uri: &str, line: u32, character: u32) -> Json {
        json::parse(&format!(
            r#"{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":{line},"character":{character}}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn initialize_advertises_capabilities() {
        let mut s = Server::new();
        let (out, exit) = s.handle(&req(1, "initialize", Json::Null));
        assert!(!exit);
        let caps = out
            .reply
            .unwrap()
            .get("result")
            .unwrap()
            .get("capabilities")
            .unwrap()
            .clone();
        assert_eq!(caps.get("definitionProvider"), Some(&Json::Bool(true)));
        assert_eq!(caps.get("renameProvider"), Some(&Json::Bool(true)));
    }

    #[test]
    fn open_publishes_diagnostics_then_definition_resolves() {
        let mut s = Server::new();
        let uri = "file:///c.vs";
        let src = "component C {\n  state count = 0;\n  computed d = count;\n}\n";
        let (out, _) = s.handle(&open(uri, src));
        // didOpen publishes diagnostics (a clean file → empty list, but the
        // notification is still sent).
        assert_eq!(out.notifications.len(), 1);
        assert_eq!(
            out.notifications[0].get("method").and_then(Json::as_str),
            Some("textDocument/publishDiagnostics")
        );

        // Goto from the `count` use on line 2 (0-based), inside the computed.
        let line = 2;
        let character = "  computed d = ".len() as u32 + 1; // inside `count`
        let (out, _) = s.handle(&req(
            2,
            "textDocument/definition",
            pos_params(uri, line, character),
        ));
        let result = out.reply.unwrap().get("result").unwrap().clone();
        // Resolves to the declaration on line 1.
        let def_line = result
            .get("range")
            .unwrap()
            .get("start")
            .unwrap()
            .get("line")
            .unwrap()
            .as_u32();
        assert_eq!(
            def_line,
            Some(1),
            "definition lands on the state declaration line"
        );
    }

    #[test]
    fn rename_produces_a_workspace_edit() {
        let mut s = Server::new();
        let uri = "file:///c.vs";
        let src = "component C {\n  state count = 0;\n  computed d = count;\n}\n";
        s.handle(&open(uri, src));
        let mut params = match pos_params(uri, 1, "  state ".len() as u32 + 1) {
            Json::Obj(m) => m,
            _ => unreachable!(),
        };
        params.insert("newName".to_string(), Json::Str("total".to_string()));
        let (out, _) = s.handle(&req(3, "textDocument/rename", Json::Obj(params)));
        let edits = out
            .reply
            .unwrap()
            .get("result")
            .unwrap()
            .get("changes")
            .unwrap()
            .get(uri)
            .unwrap()
            .as_arr()
            .unwrap()
            .len();
        assert_eq!(edits, 2, "declaration + one use rewritten");
    }

    #[test]
    fn formatting_returns_a_full_document_edit() {
        let mut s = Server::new();
        let uri = "file:///c.vs";
        s.handle(&open(uri, "component C{state count=0;}"));
        let params = json::parse(&format!(r#"{{"textDocument":{{"uri":"{uri}"}}}}"#)).unwrap();
        let (out, _) = s.handle(&req(4, "textDocument/formatting", params));
        let edits = out
            .reply
            .unwrap()
            .get("result")
            .unwrap()
            .as_arr()
            .unwrap()
            .to_vec();
        assert_eq!(edits.len(), 1, "one full-document edit");
        assert!(
            edits[0]
                .get("newText")
                .and_then(Json::as_str)
                .unwrap()
                .contains("    state count = 0;"),
            "formatted text is normalized"
        );
    }

    #[test]
    fn shutdown_then_exit_signals_stop() {
        let mut s = Server::new();
        let (_, exit) = s.handle(&req(9, "shutdown", Json::Null));
        assert!(!exit, "shutdown alone does not stop the loop");
        let (_, exit) = s.handle(&notif("exit", Json::Null));
        assert!(exit, "exit stops the loop");
    }

    #[test]
    fn unknown_request_is_method_not_found() {
        let mut s = Server::new();
        let (out, _) = s.handle(&req(7, "textDocument/hover", Json::Null));
        let code = out
            .reply
            .unwrap()
            .get("error")
            .unwrap()
            .get("code")
            .unwrap()
            .as_i64();
        assert_eq!(code, Some(-32601));
    }
}
