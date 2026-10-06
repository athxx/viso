//! Code actions and structured edits (§143) over the protocol: the quick fixes
//! the compiler's diagnostics carry, the structured edits a cursor position
//! fully determines, and the `viso/structuredEdit` request an agent sends a
//! [`StructuredEdit`] through as JSON.
//!
//! Every edit here is checked by [`Document::apply`] before it reaches the
//! client; nothing is sent that would add an error.

use viso_dsl::diag::{Applicability, Diagnostic};
use viso_dsl::edit::{Created, Document, StructuredEdit, SyntaxId};
use viso_dsl::frontend::Origin;
use viso_dsl::resolve::SymbolId;
use viso_dsl::{TextRange, TextSize};

use crate::rpc::Json;

/// One code action: a title, its LSP kind, the edits, and the diagnostic it
/// fixes, if it is a quick fix.
#[derive(Debug, Clone)]
pub struct CodeAction {
    /// What the action does.
    pub title: String,
    /// `quickfix` or `refactor.rewrite`.
    pub kind: &'static str,
    /// The replacements, in the document's byte coordinates.
    pub edits: Vec<(TextRange, String)>,
    /// The diagnostic the action fixes.
    pub fixes: Option<Diagnostic>,
    /// Whether the client may apply it as the preferred fix.
    pub preferred: bool,
}

/// The origin an open document compiles under: its module named after the
/// file, in the anonymous package the single-document server resolves.
pub fn origin(module: &[String]) -> Origin {
    Origin {
        package: String::new(),
        module: module.to_vec(),
        language: None,
    }
}

/// The code actions for `range` of `doc`: each fix of a diagnostic touching
/// the range whose edits stay in this file, then the structured edits the
/// declaration under the range's start determines.
pub fn code_actions(doc: &Document, range: TextRange) -> Vec<CodeAction> {
    let touches =
        |d: &Diagnostic| d.primary.start() <= range.end() && range.start() <= d.primary.end();
    let mut actions: Vec<CodeAction> = doc
        .diagnostics()
        .iter()
        .filter(|d| touches(d))
        .flat_map(|d| {
            d.fixes
                .iter()
                .filter(|f| f.edits.iter().all(|e| e.module.is_none()))
                .map(move |f| CodeAction {
                    title: f.title.clone(),
                    kind: "quickfix",
                    edits: f
                        .edits
                        .iter()
                        .map(|e| (e.range, e.replacement.clone()))
                        .collect(),
                    fixes: Some(d.clone()),
                    preferred: f.applicability == Applicability::MachineApplicable,
                })
        })
        .collect();
    for (title, edit) in doc.suggestions_at(range.start()) {
        if let Ok(applied) = doc.apply(&edit) {
            actions.push(CodeAction {
                title,
                kind: "refactor.rewrite",
                edits: applied
                    .edits
                    .into_iter()
                    .map(|e| (e.range, e.replacement))
                    .collect(),
                fixes: None,
                preferred: false,
            });
        }
    }
    actions
}

/// The addresses at `offset`: the innermost declaration's Symbol ID and the
/// innermost view node's Syntax ID, as their text forms.
pub fn addresses(doc: &Document, offset: TextSize) -> Json {
    let text = |s: Option<String>| s.map_or(Json::Null, Json::Str);
    Json::object([
        ("symbol", text(doc.symbol_at(offset).map(|s| s.to_string()))),
        (
            "syntax",
            text(doc.syntax_id_at(offset).map(|s| s.to_string())),
        ),
    ])
}

/// A [`StructuredEdit`] from its JSON form: `{"kind": "<Variant>", ...}` with
/// the variant's fields in camelCase, Symbol and Syntax IDs as their text.
pub fn parse_edit(json: &Json) -> Result<StructuredEdit, String> {
    let kind = json
        .get("kind")
        .and_then(Json::as_str)
        .ok_or("an edit names its `kind`")?;
    let text = |key: &str| -> Result<String, String> {
        json.get(key)
            .and_then(Json::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("`{kind}` needs the string `{key}`"))
    };
    let optional = |key: &str| json.get(key).and_then(Json::as_str).map(str::to_string);
    let symbol = |key: &str| -> Result<SymbolId, String> {
        text(key)?
            .parse()
            .map_err(|()| format!("`{key}` is no Symbol ID"))
    };
    let syntax = |key: &str| -> Result<SyntaxId, String> {
        text(key)?
            .parse()
            .map_err(|()| format!("`{key}` is no Syntax ID"))
    };
    Ok(match kind {
        "AddImport" => StructuredEdit::AddImport {
            path: text("path")?,
        },
        "CreateComponent" => StructuredEdit::CreateComponent {
            name: text("name")?,
            export: matches!(json.get("export"), Some(Json::Bool(true))),
        },
        "AddInput" => StructuredEdit::AddInput {
            component: symbol("component")?,
            name: text("name")?,
            ty: text("ty")?,
            default: optional("default"),
        },
        "AddState" => StructuredEdit::AddState {
            component: symbol("component")?,
            name: text("name")?,
            ty: optional("ty"),
            init: text("init")?,
        },
        "AddAction" => StructuredEdit::AddAction {
            component: symbol("component")?,
            name: text("name")?,
            params: optional("params").unwrap_or_default(),
            body: optional("body").unwrap_or_default(),
        },
        "InsertNode" => StructuredEdit::InsertNode {
            parent: syntax("parent")?,
            index: json
                .get("index")
                .and_then(Json::as_u32)
                .map_or(usize::MAX, |i| i as usize),
            node: text("node")?,
        },
        "SetPropertyBinding" => StructuredEdit::SetPropertyBinding {
            node: syntax("node")?,
            property: text("property")?,
            value: text("value")?,
        },
        "AttachEventHandler" => StructuredEdit::AttachEventHandler {
            node: syntax("node")?,
            event: text("event")?,
            pattern: optional("pattern"),
            body: optional("body").unwrap_or_default(),
        },
        "WrapInKeyedFor" => StructuredEdit::WrapInKeyedFor {
            node: syntax("node")?,
            item: text("item")?,
            list: text("list")?,
            key: text("key")?,
        },
        "ConvertTaskToResource" => StructuredEdit::ConvertTaskToResource {
            task: symbol("task")?,
            name: text("name")?,
            args: json
                .get("args")
                .and_then(Json::as_arr)
                .unwrap_or_default()
                .iter()
                .map(|a| a.as_str().map(str::to_string))
                .collect::<Option<_>>()
                .ok_or("`args` is a list of strings")?,
            key: text("key")?,
        },
        "AddTraitImpl" => StructuredEdit::AddTraitImpl {
            trait_name: text("trait")?,
            ty: text("ty")?,
            body: optional("body").unwrap_or_default(),
        },
        other => return Err(format!("no structured edit is named `{other}`")),
    })
}

/// The text form of what an edit created.
pub fn created_json(created: Option<&Created>) -> Json {
    match created {
        Some(Created::Symbol(id)) => Json::object([("symbol", Json::Str(id.to_string()))]),
        Some(Created::Syntax(id)) => Json::object([("syntax", Json::Str(id.to_string()))]),
        None => Json::Null,
    }
}
