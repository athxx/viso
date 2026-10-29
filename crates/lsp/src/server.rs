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
//! Cold-path throughout: a handful of these run per editor keystroke.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use viso_dsl::LineIndex;
use viso_dsl::diag::{Diagnostic, Severity, codes};

use crate::actions;
use crate::complete;
use crate::engine;
use crate::position::{self, LspPosition, LspRange};
use crate::rpc::Json;
use crate::semantic;
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
    /// Whether the client asked for `shutdown`, which makes the `exit` after it
    /// a clean one.
    shut_down: bool,
}

impl Server {
    /// A fresh server with no open documents.
    pub fn new() -> Self {
        Self::default()
    }

    /// The process exit code once the loop stops: `0` after `shutdown`, `1`
    /// for an `exit` or an end of input without it, as the protocol has it.
    pub fn exit_code(&self) -> u8 {
        if self.shut_down { 0 } else { 1 }
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
            "shutdown" => {
                self.shut_down = true;
                (
                    Outbound {
                        reply: Some(response(id, Json::Null)),
                        notifications: Vec::new(),
                    },
                    false,
                )
            }
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
            "textDocument/hover" => (
                Outbound {
                    reply: Some(response(id, self.hover(params))),
                    notifications: Vec::new(),
                },
                false,
            ),
            "textDocument/semanticTokens/full" => (
                Outbound {
                    reply: Some(response(id, self.semantic_tokens(params))),
                    notifications: Vec::new(),
                },
                false,
            ),
            "textDocument/completion" => (
                Outbound {
                    reply: Some(response(id, self.completion(params))),
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
            "textDocument/codeAction" => (
                Outbound {
                    reply: Some(response(id, self.code_action(params))),
                    notifications: Vec::new(),
                },
                false,
            ),
            "viso/syntaxId" => (
                Outbound {
                    reply: Some(response(id, self.syntax_id(params))),
                    notifications: Vec::new(),
                },
                false,
            ),
            "viso/structuredEdit" => {
                let reply = match self.structured_edit(params) {
                    Ok(result) => response(id, result),
                    Err(message) => error_response(id.unwrap_or(Json::Null), -32602, &message),
                };
                (
                    Outbound {
                        reply: Some(reply),
                        notifications: Vec::new(),
                    },
                    false,
                )
            }
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
            manifest_of(&uri).as_deref(),
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
            manifest_of(&uri).as_deref(),
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

    /// Every keyword and name of the document, classified.
    fn semantic_tokens(&self, params: Option<&Json>) -> Json {
        let Some(doc) = params
            .and_then(|p| p.get("textDocument")?.get("uri")?.as_str())
            .and_then(|uri| self.id_of.get(uri))
            .and_then(|&id| self.docs.get(id))
        else {
            return Json::Null;
        };
        let data = semantic::encode(&semantic::semantic_tokens(doc), &doc.line_index);
        let mut m = BTreeMap::new();
        m.insert(
            "data".to_string(),
            Json::Arr(data.into_iter().map(|n| Json::Num(f64::from(n))).collect()),
        );
        Json::Obj(m)
    }

    /// The names that fit at the cursor.
    fn completion(&self, params: Option<&Json>) -> Json {
        let Some((id, offset)) = self.locate(params) else {
            return Json::Null;
        };
        let doc = self.docs.get(id).expect("located doc is open");
        let items = complete::complete(doc, offset)
            .into_iter()
            .map(|c| {
                let mut m = BTreeMap::new();
                m.insert("label".to_string(), Json::Str(c.label));
                m.insert("kind".to_string(), Json::Num(f64::from(c.kind as u8)));
                if let Some(detail) = c.detail {
                    m.insert("detail".to_string(), Json::Str(detail));
                }
                if let Some(snippet) = c.snippet {
                    m.insert("insertText".to_string(), Json::Str(snippet));
                    // InsertTextFormat.Snippet.
                    m.insert("insertTextFormat".to_string(), Json::Num(2.0));
                }
                Json::Obj(m)
            })
            .collect();
        Json::Arr(items)
    }

    /// What each diagnostic under the cursor means and how to fix it, from
    /// the diagnostics registry `viso explain` reads.
    fn hover(&self, params: Option<&Json>) -> Json {
        let Some((id, offset)) = self.locate(params) else {
            return Json::Null;
        };
        let doc = self.docs.get(id).expect("located doc is open");
        let mut sections: Vec<String> = Vec::new();
        let mut range: Option<viso_dsl::TextRange> = None;
        for d in doc.diagnostics() {
            let primary = d.primary;
            if offset < primary.start() || offset > primary.end() {
                continue;
            }
            let Some(info) = codes::lookup(d.code) else {
                continue;
            };
            let section = format!(
                "**{}** {}\n\n{}\n\nFix: {}\n\n`viso explain {}`",
                info.code, info.title, info.explanation, info.fix, info.code
            );
            if !sections.contains(&section) {
                sections.push(section);
            }
            range = Some(match range {
                Some(r) => viso_dsl::TextRange::new(
                    r.start().min(primary.start()),
                    r.end().max(primary.end()),
                ),
                None => primary,
            });
        }
        let Some(range) = range else {
            return Json::Null;
        };
        let mut contents = BTreeMap::new();
        contents.insert("kind".to_string(), Json::Str("markdown".to_string()));
        contents.insert("value".to_string(), Json::Str(sections.join("\n\n---\n\n")));
        let mut m = BTreeMap::new();
        m.insert("contents".to_string(), Json::Obj(contents));
        m.insert(
            "range".to_string(),
            range_json(position::to_lsp_range(&doc.line_index, range)),
        );
        Json::Obj(m)
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

    /// `textDocument/codeAction`: the diagnostics' fixes and the structured
    /// edits the range determines, each a `CodeAction` with its edit.
    fn code_action(&self, params: Option<&Json>) -> Json {
        let Some((uri, doc, edit_doc)) = self.edit_doc(params) else {
            return Json::Arr(Vec::new());
        };
        let Some(range) = params
            .and_then(|p| p.get("range"))
            .and_then(|r| Some((r.get("start")?, r.get("end")?)))
            .and_then(|(start, end)| {
                let at = |p: &Json| {
                    Some(position::from_lsp_position(
                        &doc.source,
                        LspPosition {
                            line: p.get("line")?.as_u32()?,
                            character: p.get("character")?.as_u32()?,
                        },
                    ))
                };
                Some(viso_dsl::TextRange::new(at(start)?, at(end)?))
            })
        else {
            return Json::Arr(Vec::new());
        };
        let actions = actions::code_actions(&edit_doc, range)
            .into_iter()
            .map(|a| {
                let edits = a
                    .edits
                    .iter()
                    .map(|(range, text)| {
                        text_edit_json(position::to_lsp_range(&doc.line_index, *range), text)
                    })
                    .collect();
                let mut m = BTreeMap::new();
                m.insert("title".to_string(), Json::Str(a.title));
                m.insert("kind".to_string(), Json::Str(a.kind.to_string()));
                m.insert("edit".to_string(), workspace_edit(&uri, edits));
                if a.preferred {
                    m.insert("isPreferred".to_string(), Json::Bool(true));
                }
                if let Some(d) = &a.fixes {
                    m.insert(
                        "diagnostics".to_string(),
                        Json::Arr(vec![diagnostic_json(&uri, &doc.line_index, d)]),
                    );
                }
                Json::Obj(m)
            })
            .collect();
        Json::Arr(actions)
    }

    /// `viso/syntaxId`: the Symbol ID and Syntax ID at a position.
    fn syntax_id(&self, params: Option<&Json>) -> Json {
        let (Some((_, offset)), Some((_, _, edit_doc))) =
            (self.locate(params), self.edit_doc(params))
        else {
            return Json::Null;
        };
        actions::addresses(&edit_doc, offset)
    }

    /// `viso/structuredEdit`: `{textDocument, edit}` to `{edit: WorkspaceEdit,
    /// created}`, or the reason the edit was refused.
    fn structured_edit(&self, params: Option<&Json>) -> Result<Json, String> {
        let (uri, doc, edit_doc) = self.edit_doc(params).ok_or("the document is not open")?;
        let edit = params
            .and_then(|p| p.get("edit"))
            .ok_or("the request carries an `edit`")?;
        let edit = actions::parse_edit(edit)?;
        let applied = edit_doc.apply(&edit).map_err(|e| e.to_string())?;
        let edits = applied
            .edits
            .iter()
            .map(|e| {
                text_edit_json(
                    position::to_lsp_range(&doc.line_index, e.range),
                    &e.replacement,
                )
            })
            .collect();
        Ok(Json::object([
            ("edit", workspace_edit(&uri, edits)),
            ("created", actions::created_json(applied.created.as_ref())),
        ]))
    }

    /// The open document a request names, and its structured-edit view.
    fn edit_doc(
        &self,
        params: Option<&Json>,
    ) -> Option<(
        String,
        &crate::source_map::OpenDoc,
        viso_dsl::edit::Document,
    )> {
        let uri = params?.get("textDocument")?.get("uri")?.as_str()?;
        let &id = self.id_of.get(uri)?;
        let doc = self.docs.get(id)?;
        let origin = actions::origin(&module_path_of(uri));
        let edit_doc = viso_dsl::edit::Document::new(doc.source.clone(), &origin);
        Some((uri.to_string(), doc, edit_doc))
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
        for d in doc.diagnostics() {
            items.push(diagnostic_json(uri, &doc.line_index, d));
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
    caps.insert("hoverProvider".to_string(), Json::Bool(true));
    caps.insert("codeActionProvider".to_string(), Json::Bool(true));
    let names =
        |names: &[&str]| Json::Arr(names.iter().map(|n| Json::Str(n.to_string())).collect());
    let mut legend = BTreeMap::new();
    legend.insert("tokenTypes".to_string(), names(semantic::TOKEN_TYPES));
    legend.insert(
        "tokenModifiers".to_string(),
        names(semantic::TOKEN_MODIFIERS),
    );
    let mut tokens = BTreeMap::new();
    tokens.insert("legend".to_string(), Json::Obj(legend));
    tokens.insert("full".to_string(), Json::Bool(true));
    caps.insert("semanticTokensProvider".to_string(), Json::Obj(tokens));
    let mut completion = BTreeMap::new();
    completion.insert("triggerCharacters".to_string(), names(&[":"]));
    caps.insert("completionProvider".to_string(), Json::Obj(completion));
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

/// A `WorkspaceEdit` changing one document: `{ changes: { <uri>: [TextEdit] } }`.
fn workspace_edit(uri: &str, edits: Vec<Json>) -> Json {
    let mut per_uri = BTreeMap::new();
    per_uri.insert(uri.to_string(), Json::Arr(edits));
    Json::object([("changes", Json::Obj(per_uri))])
}

/// A protocol `TextEdit` (range + newText).
fn text_edit_json(range: LspRange, new_text: &str) -> Json {
    let mut m = BTreeMap::new();
    m.insert("range".to_string(), range_json(range));
    m.insert("newText".to_string(), Json::Str(new_text.to_string()));
    Json::Obj(m)
}

/// A protocol `Diagnostic` from a [`Diagnostic`] in the document `uri`, whichever
/// compiler raised it: its notes follow the message, and its related spans in
/// this document are its related information.
fn diagnostic_json(uri: &str, line_index: &LineIndex, d: &Diagnostic) -> Json {
    let mut m = BTreeMap::new();
    m.insert(
        "range".to_string(),
        range_json(position::to_lsp_range(line_index, d.primary)),
    );
    m.insert(
        "severity".to_string(),
        Json::Num(lsp_severity(d.severity) as f64),
    );
    m.insert("code".to_string(), Json::Str(d.code.to_string()));
    m.insert("source".to_string(), Json::Str("viso".to_string()));
    let mut message = d.message.clone();
    for note in &d.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    m.insert("message".to_string(), Json::Str(message));
    let related: Vec<Json> = d
        .related
        .iter()
        .filter(|r| r.module.is_none())
        .map(|r| {
            Json::object([
                (
                    "location",
                    location_json(uri, position::to_lsp_range(line_index, r.range)),
                ),
                ("message", Json::Str(r.label.clone())),
            ])
        })
        .collect();
    if !related.is_empty() {
        m.insert("relatedInformation".to_string(), Json::Arr(related));
    }
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
/// intra-document goto/references/rename this server provides. There is no
/// cross-module resolution across files.
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

/// The manifest of the Cargo package the `file:` document `uri` belongs to.
fn manifest_of(uri: &str) -> Option<PathBuf> {
    let path = uri.strip_prefix("file://")?;
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escaped = (bytes[at] == b'%')
            .then(|| bytes.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match escaped {
            Some(byte) => {
                decoded.push(byte);
                at += 3;
            }
            None => {
                decoded.push(bytes[at]);
                at += 1;
            }
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    // `file:///C:/…` on Windows names `C:/…`.
    let path = match path.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => &path[1..],
        _ => &path,
    };
    viso_dsl::native_package::manifest_of(Path::new(path))
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
        assert_eq!(caps.get("codeActionProvider"), Some(&Json::Bool(true)));
        let legend = caps
            .get("semanticTokensProvider")
            .and_then(|p| p.get("legend"));
        let types = legend.and_then(|l| l.get("tokenTypes")).cloned();
        assert!(matches!(types, Some(Json::Arr(t)) if t.len() == semantic::TOKEN_TYPES.len()));
        assert!(caps.get("completionProvider").is_some());
    }

    fn range_params(uri: &str, line: u32, character: u32) -> Json {
        json::parse(&format!(
            r#"{{"textDocument":{{"uri":"{uri}"}},"range":{{"start":{{"line":{line},"character":{character}}},"end":{{"line":{line},"character":{character}}}}}}}"#
        ))
        .unwrap()
    }

    fn result(out: Outbound) -> Json {
        out.reply.unwrap().get("result").cloned().unwrap()
    }

    const LOADER: &str = "import viso::time;\n\
        export component C {\n\
        \x20   state id = 1;\n\
        \x20   computed d = idd + 1;\n\
        \x20   task load_user(id: I64) -> Result<I64, String> {\n\
        \x20       await time::sleep(1s);\n\
        \x20       Ok(id)\n\
        \x20   }\n\
        \x20   view {\n\
        \x20       Column {\n\
        \x20           Text {}\n\
        \x20       }\n\
        \x20   }\n\
        }\n";

    #[test]
    fn code_actions_carry_the_diagnostic_fixes_and_the_structured_edits() {
        let mut s = Server::new();
        let uri = "file:///loader.vs";
        s.handle(&open(uri, LOADER));

        // On the misspelt `idd`: the nearest-name fix, tied to its diagnostic.
        let (out, _) = s.handle(&req(
            5,
            "textDocument/codeAction",
            range_params(uri, 3, "    computed d = i".len() as u32),
        ));
        let actions = result(out);
        let actions = actions.as_arr().unwrap();
        let fix = actions
            .iter()
            .find(|a| a.get("kind").and_then(Json::as_str) == Some("quickfix"))
            .expect("a quick fix");
        let edits = fix
            .get("edit")
            .and_then(|e| e.get("changes"))
            .and_then(|c| c.get(uri))
            .and_then(Json::as_arr)
            .unwrap();
        assert_eq!(edits[0].get("newText").and_then(Json::as_str), Some("id"));
        let diagnostic = &fix.get("diagnostics").and_then(Json::as_arr).unwrap()[0];
        assert_eq!(diagnostic.get("code").and_then(Json::as_str), Some("E2001"));

        // In the task: converting it to a resource loading by its parameter.
        let (out, _) = s.handle(&req(
            6,
            "textDocument/codeAction",
            range_params(uri, 4, "    task lo".len() as u32),
        ));
        let actions = result(out);
        let convert = actions
            .as_arr()
            .unwrap()
            .iter()
            .find(|a| a.get("kind").and_then(Json::as_str) == Some("refactor.rewrite"))
            .expect("the conversion");
        assert_eq!(
            convert.get("title").and_then(Json::as_str),
            Some("Convert task `load_user` to resource `user`")
        );
        let edits = convert
            .get("edit")
            .and_then(|e| e.get("changes"))
            .and_then(|c| c.get(uri))
            .and_then(Json::as_arr)
            .unwrap();
        assert!(
            edits[0]
                .get("newText")
                .and_then(Json::as_str)
                .unwrap()
                .contains("resource user: Resource<I64, String> {\n        load = load_user(id);\n        key = id;\n    }")
        );
    }

    #[test]
    fn a_structured_edit_addresses_a_node_by_its_syntax_id() {
        let mut s = Server::new();
        let uri = "file:///loader.vs";
        s.handle(&open(uri, LOADER));
        let (out, _) = s.handle(&req(7, "viso/syntaxId", pos_params(uri, 10, 13)));
        let ids = result(out);
        let syntax = ids
            .get("syntax")
            .and_then(Json::as_str)
            .unwrap()
            .to_string();
        assert!(syntax.ends_with("/AnonymousNode.0"), "{syntax}");
        assert!(ids.get("symbol").and_then(Json::as_str).is_some());

        let edit = |value: &str| {
            json::parse(&format!(
                r#"{{"textDocument":{{"uri":"{uri}"}},"edit":{{"kind":"SetPropertyBinding","node":"{syntax}","property":"text","value":{}}}}}"#,
                Json::Str(value.to_string())
            ))
            .unwrap()
        };
        let (out, _) = s.handle(&req(8, "viso/structuredEdit", edit("\"hi\"")));
        let applied = result(out);
        let edits = applied
            .get("edit")
            .and_then(|e| e.get("changes"))
            .and_then(|c| c.get(uri))
            .and_then(Json::as_arr)
            .unwrap();
        assert_eq!(
            edits[0].get("newText").and_then(Json::as_str),
            Some("\n                text: \"hi\";\n            ")
        );
        assert!(
            applied
                .get("created")
                .and_then(|c| c.get("syntax"))
                .and_then(Json::as_str)
                .is_some_and(|s| s.ends_with("/PropertyBinding.0"))
        );

        // An edit adding an error is refused with it.
        let (out, _) = s.handle(&req(9, "viso/structuredEdit", edit("missing")));
        let error = out.reply.unwrap().get("error").cloned().unwrap();
        assert_eq!(error.get("code").and_then(Json::as_i64), Some(-32602));
        assert!(
            error
                .get("message")
                .and_then(Json::as_str)
                .unwrap()
                .contains("E2001")
        );
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
        s.handle(&open(uri, "component C{\nstate count=0;}"));
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
        assert_eq!(s.exit_code(), 0);

        let mut s = Server::new();
        let (_, exit) = s.handle(&notif("exit", Json::Null));
        assert!(exit);
        assert_eq!(s.exit_code(), 1, "an exit without shutdown");
    }

    #[test]
    fn a_shader_compiler_diagnostic_publishes_with_its_related_span() {
        // The brush profile is checked by the shader compiler; its diagnostic
        // reaches the editor as the frontend's do.
        let Some(codes::Example::Source(src)) = codes::lookup("E8107").map(|c| c.example) else {
            panic!("E8107 has a source example");
        };
        let mut s = Server::new();
        let uri = "file:///glow.vs";
        let (out, _) = s.handle(&open(uri, src));
        let params = out.notifications[0].get("params").unwrap();
        let items = params.get("diagnostics").and_then(Json::as_arr).unwrap();
        let d = &items[0];
        assert_eq!(d.get("code").and_then(Json::as_str), Some("E8107"));
        let start = d.get("range").and_then(|r| r.get("start")).unwrap();
        assert_eq!(
            (
                start.get("line").and_then(Json::as_u32),
                start.get("character").and_then(Json::as_u32)
            ),
            (Some(0), Some(7)),
            "a missing member points at the shader's name"
        );
        let related = d.get("relatedInformation").and_then(Json::as_arr).unwrap();
        assert_eq!(
            related[0].get("message").and_then(Json::as_str),
            Some("`Glow` paints this view")
        );
        let at = related[0].get("location").unwrap();
        assert_eq!(at.get("uri").and_then(Json::as_str), Some(uri));
    }

    #[test]
    fn hover_on_a_diagnostic_explains_its_code() {
        let mut s = Server::new();
        let uri = "file:///h.vs";
        s.handle(&open(
            uri,
            "component C {\n    view { Text { text: missing; } }\n}\n",
        ));
        let at = |line: u32, character: u32| {
            json::parse(&format!(
                r#"{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":{line},"character":{character}}}}}"#
            ))
            .unwrap()
        };
        let (out, _) = s.handle(&req(5, "textDocument/hover", at(1, 25)));
        let result = out.reply.unwrap().get("result").cloned().unwrap();
        let value = result
            .get("contents")
            .and_then(|c| c.get("value"))
            .and_then(Json::as_str)
            .unwrap()
            .to_string();
        assert!(value.starts_with("**E2001** "), "{value}");
        assert!(value.contains("\n\nFix: "), "{value}");
        assert!(value.ends_with("`viso explain E2001`"), "{value}");
        assert!(result.get("range").is_some());

        let (out, _) = s.handle(&req(6, "textDocument/hover", at(0, 2)));
        assert_eq!(out.reply.unwrap().get("result"), Some(&Json::Null));
    }

    #[test]
    fn semantic_tokens_and_completion_answer_over_the_protocol() {
        let mut s = Server::new();
        let uri = "file:///t.vs";
        s.handle(&open(uri, "component C {\n    view { Column {  } }\n}\n"));
        let doc = json::parse(&format!(r#"{{"textDocument":{{"uri":"{uri}"}}}}"#)).unwrap();
        let (out, _) = s.handle(&req(8, "textDocument/semanticTokens/full", doc));
        let Some(Json::Arr(data)) = result(out).get("data").cloned() else {
            panic!("no token data");
        };
        // `component` is the first token: line 0, column 0, nine long, a keyword.
        let first: Vec<Option<i64>> = data[..5].iter().map(Json::as_i64).collect();
        assert_eq!(
            first,
            [0, 0, 9, semantic::TokenType::Keyword as i64, 0].map(Some)
        );
        assert_eq!(data.len() % 5, 0);

        let at = json::parse(&format!(
            r#"{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":1,"character":20}}}}"#
        ))
        .unwrap();
        let (out, _) = s.handle(&req(9, "textDocument/completion", at));
        let Json::Arr(items) = result(out) else {
            panic!("no completion list");
        };
        let gap = items
            .iter()
            .find(|i| i.get("label").and_then(Json::as_str) == Some("gap"))
            .expect("Column's `gap`");
        assert_eq!(
            gap.get("insertText").and_then(Json::as_str),
            Some("gap: $0;")
        );
        assert_eq!(gap.get("insertTextFormat").and_then(Json::as_i64), Some(2));
    }

    #[test]
    fn unknown_request_is_method_not_found() {
        let mut s = Server::new();
        let (out, _) = s.handle(&req(7, "textDocument/signatureHelp", Json::Null));
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
