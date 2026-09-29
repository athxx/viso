//! Schema queries: the typed schema of a built-in widget, a native library, a
//! native function or a native handle type, as the one object tools read
//! (`viso schema`, section 139).
//!
//! A query names a symbol by its full path (`viso::widgets::Button`,
//! `viso::text::upper`) or by any `::`-suffix of it (`Button`, `text::upper`),
//! optionally followed by `.member` to narrow the answer to one property, event
//! or method (`Button.text`, `Stopwatch.elapsed_ms`). Cold path: this runs once
//! per tool request.

use viso_behavior::native::{NativeEntry, NativeFunction, NativeLibrary, NativeTypeEntry};
pub use viso_behavior::native::{NativeKind, Natives, Ownership, ThreadDomain};
use viso_ende::JsonWriter;

use crate::diag::Diagnostic;
use crate::hir::widget::{self, WidgetSchema};
use crate::ir::{DirtyClass, property_dirty_class};
use crate::resolve::suggest::{Candidate, nearest};
use crate::syntax::TextRange;

/// The module path the built-in widgets live under.
pub const WIDGETS: &str = "viso::widgets";

/// The schema version of the built-in widget baseline.
const WIDGET_VERSION: &str = "1.0";

/// The dirty classes in canonical order, with their names.
const DIRTY_NAMES: [(DirtyClass, &str); 8] = [
    (DirtyClass::STRUCTURE, "STRUCTURE"),
    (DirtyClass::STYLE, "STYLE"),
    (DirtyClass::MEASURE, "MEASURE"),
    (DirtyClass::LAYOUT, "LAYOUT"),
    (DirtyClass::TRANSFORM, "TRANSFORM"),
    (DirtyClass::PAINT, "PAINT"),
    (DirtyClass::HIT_TEST, "HIT_TEST"),
    (DirtyClass::SEMANTICS, "SEMANTICS"),
];

/// What a schema describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaKind {
    /// A node type: its inputs, events, slots and parts.
    Component,
    /// A native library: its functions and handle types.
    Library,
    /// One native function; its parameters are the inputs.
    Function,
    /// A native handle type and its methods.
    Type,
}

impl SchemaKind {
    /// The `kind` the schema object reports.
    pub fn name(self) -> &'static str {
        match self {
            SchemaKind::Component => "component",
            SchemaKind::Library => "native_library",
            SchemaKind::Function => "native_function",
            SchemaKind::Type => "native_type",
        }
    }
}

/// A property of a node type or a parameter of a native function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSchema {
    /// Its name; a grouped property is written `group.member`.
    pub name: String,
    /// Its type's source name.
    pub ty: String,
    /// Whether a use must give it.
    pub required: bool,
    /// Its default in source form, when the schema records one.
    pub default: Option<String>,
    /// The dirty classes a binding of it invalidates, in canonical order; empty
    /// for a function parameter.
    pub invalidates: Vec<&'static str>,
    /// Whether it may be the left side of `bind`.
    pub two_way: bool,
}

/// An event a node type raises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSchema {
    /// Its name.
    pub name: String,
    /// The record its payload is, if it carries one.
    pub payload: Option<String>,
    /// Whether it bubbles up the ancestor route after its target.
    pub bubbles: bool,
    /// Whether a handler may stop it before the default action; the routed
    /// input events are, a node's own events are not.
    pub cancelable: bool,
}

/// A native function or method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSchema {
    /// Its name within its library or type.
    pub name: String,
    /// Its full path.
    pub symbol: String,
    /// Whether it is a query (`fn`), an `action` or a `task`.
    pub kind: NativeKind,
    /// Its parameters; a method's first is its receiver.
    pub params: Vec<InputSchema>,
    /// Its return type's source name.
    pub returns: String,
    /// Whether it is called on a handle, `receiver.name(..)`.
    pub method: bool,
    /// The capabilities a caller needs.
    pub capabilities: Vec<String>,
    /// The thread it runs on.
    pub thread: ThreadDomain,
    /// Whether equal arguments always give an equal result.
    pub deterministic: bool,
    /// Whether it neither allocates nor blocks.
    pub realtime_safe: bool,
    /// Its budget cost per call.
    pub cost: u32,
}

/// The handle ownership and thread of a native type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandleSchema {
    /// Whether a handle may be stored or only borrowed for one call.
    pub ownership: Ownership,
    /// The thread its methods run on.
    pub thread: ThreadDomain,
}

/// The section 139 schema object of one symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    /// What it describes.
    pub kind: SchemaKind,
    /// Its full path.
    pub symbol: String,
    /// The version of the schema that declares it.
    pub version: String,
    /// The member the query narrowed to, if it named one.
    pub member: Option<String>,
    /// A node type's properties or a function's parameters.
    pub inputs: Vec<InputSchema>,
    /// A node type's events.
    pub events: Vec<EventSchema>,
    /// A node type's named slots.
    pub slots: Vec<String>,
    /// A node type's named parts.
    pub parts: Vec<String>,
    /// The capabilities using it needs: a function's own, or every function's of
    /// a library or type.
    pub capabilities: Vec<String>,
    /// A library's functions, a type's methods, or the function itself.
    pub functions: Vec<FunctionSchema>,
    /// A library's handle types, by full path.
    pub types: Vec<String>,
    /// A handle type's ownership and thread.
    pub handle: Option<HandleSchema>,
}

/// One match of a schema search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The symbol, or `symbol.member` for a member.
    pub path: String,
    /// What the symbol is.
    pub kind: SchemaKind,
    /// A one-line signature: a type, a payload or a function signature.
    pub detail: String,
}

/// A symbol a query can name.
#[derive(Clone, Copy)]
enum Symbol<'a> {
    Widget(&'static str),
    Library(&'static NativeLibrary),
    Function(&'a NativeEntry),
    Type(&'a NativeTypeEntry),
}

impl Symbol<'_> {
    fn path(&self) -> String {
        match self {
            Symbol::Widget(name) => format!("{WIDGETS}::{name}"),
            Symbol::Library(library) => library.path.to_owned(),
            Symbol::Function(entry) => entry.path.to_string(),
            Symbol::Type(entry) => entry.path.to_string(),
        }
    }
}

/// Every symbol a query can name, in a stable order: widgets, then each
/// library, its functions and its types.
fn symbols(natives: &Natives) -> Vec<Symbol<'_>> {
    let mut all: Vec<Symbol<'_>> = widget::BUILTIN_NAMES
        .iter()
        .map(|&name| Symbol::Widget(name))
        .collect();
    for &library in natives.libraries() {
        all.push(Symbol::Library(library));
        let under = |path: &str| {
            path.strip_prefix(library.path)
                .is_some_and(|rest| rest.starts_with("::") && !rest[2..].contains("::"))
        };
        all.extend(
            natives
                .functions()
                .iter()
                .filter(|f| f.owner.is_none() && under(&f.path))
                .map(Symbol::Function),
        );
        all.extend(
            natives
                .types()
                .iter()
                .filter(|t| under(&t.path))
                .map(Symbol::Type),
        );
    }
    all
}

/// Whether `path` is `name` or ends in `::name`.
fn names(path: &str, name: &str) -> bool {
    path == name
        || path
            .strip_suffix(name)
            .is_some_and(|head| head.ends_with("::"))
}

/// The schema `query` names, narrowed to its `.member` if it has one.
///
/// # Errors
///
/// An `E2001` diagnostic, with the nearest symbol or member names as notes, if
/// nothing matches; an `E2001` listing every match if a bare name is ambiguous.
pub fn query(query: &str, natives: &Natives) -> Result<Schema, Box<Diagnostic>> {
    let query = query.trim();
    let (name, member) = match query.split_once('.') {
        Some((name, member)) => (name, Some(member)),
        None => (query, None),
    };
    let all = symbols(natives);
    let matches: Vec<_> = all.iter().filter(|s| names(&s.path(), name)).collect();
    let symbol = match matches.as_slice() {
        [one] => **one,
        [] => {
            let paths: Vec<String> = all.iter().map(Symbol::path).collect();
            let last = name.rsplit("::").next().unwrap_or(name);
            let leaves = paths.iter().map(|p| Candidate {
                name: p.rsplit("::").next().unwrap_or(p),
                declared_at: None,
            });
            let near: Vec<String> = nearest(last, leaves)
                .iter()
                .filter_map(|c| paths.iter().find(|p| names(p, c.name)).cloned())
                .collect();
            return Err(unknown(
                format!("no schema symbol is named `{name}`"),
                &near,
            ));
        }
        many => {
            let paths: Vec<String> = many.iter().map(|s| s.path()).collect();
            return Err(unknown(
                format!("`{name}` names more than one schema symbol; give its full path"),
                &paths,
            ));
        }
    };
    let schema = schema_of(symbol, natives);
    match member {
        None => Ok(schema),
        Some(member) => narrow(schema, member),
    }
}

/// An unknown-symbol diagnostic suggesting `candidates`.
fn unknown(message: String, candidates: &[String]) -> Box<Diagnostic> {
    let mut diagnostic = Diagnostic::error("E2001", TextRange::empty(0.into()), message);
    diagnostic
        .notes
        .extend(candidates.iter().map(|c| format!("did you mean `{c}`?")));
    Box::new(diagnostic)
}

/// `schema` keeping only its input, event or function `member`.
fn narrow(mut schema: Schema, member: &str) -> Result<Schema, Box<Diagnostic>> {
    let known = schema.inputs.iter().any(|i| i.name == member)
        || schema.events.iter().any(|e| e.name == member)
        || schema.functions.iter().any(|f| f.name == member);
    if !known {
        let members: Vec<&str> = schema
            .inputs
            .iter()
            .map(|i| i.name.as_str())
            .chain(schema.events.iter().map(|e| e.name.as_str()))
            .chain(schema.functions.iter().map(|f| f.name.as_str()))
            .collect();
        let near: Vec<String> = nearest(
            member,
            members.iter().map(|&name| Candidate {
                name,
                declared_at: None,
            }),
        )
        .iter()
        .map(|c| format!("{}.{}", schema.symbol, c.name))
        .collect();
        return Err(unknown(
            format!("`{}` has no member `{member}`", schema.symbol),
            &near,
        ));
    }
    schema.inputs.retain(|i| i.name == member);
    schema.events.retain(|e| e.name == member);
    schema.functions.retain(|f| f.name == member);
    schema.capabilities = union(schema.functions.iter().map(|f| f.capabilities.as_slice()));
    schema.member = Some(member.to_owned());
    Ok(schema)
}

/// Every symbol or member whose path contains `term`, ignoring case.
pub fn search(term: &str, natives: &Natives) -> Vec<SearchHit> {
    let term = term.to_lowercase();
    let mut hits = Vec::new();
    for symbol in symbols(natives) {
        let schema = schema_of(symbol, natives);
        let mut hit = |path: String, detail: String| {
            if path.to_lowercase().contains(&term) {
                hits.push(SearchHit {
                    path,
                    kind: schema.kind,
                    detail,
                });
            }
        };
        let detail = match symbol {
            Symbol::Function(entry) => signature(&function(entry, false)),
            _ => schema.kind.name().to_owned(),
        };
        hit(schema.symbol.clone(), detail);
        if schema.kind == SchemaKind::Function {
            continue;
        }
        for input in &schema.inputs {
            hit(
                format!("{}.{}", schema.symbol, input.name),
                input.ty.clone(),
            );
        }
        for event in &schema.events {
            let payload = event.payload.clone().unwrap_or_else(|| "()".to_owned());
            hit(format!("{}.{}", schema.symbol, event.name), payload);
        }
        if schema.kind == SchemaKind::Type {
            for f in &schema.functions {
                hit(format!("{}.{}", schema.symbol, f.name), signature(f));
            }
        }
    }
    hits
}

/// A function's one-line signature, `action name(a: T) -> R`.
pub fn signature(function: &FunctionSchema) -> String {
    let params: Vec<String> = function
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, p.ty))
        .collect();
    format!(
        "{} {}({}) -> {}",
        function.kind.keyword(),
        function.name,
        params.join(", "),
        function.returns
    )
}

fn schema_of(symbol: Symbol<'_>, natives: &Natives) -> Schema {
    let empty = |kind, version: String| Schema {
        kind,
        symbol: symbol.path(),
        version,
        member: None,
        inputs: Vec::new(),
        events: Vec::new(),
        slots: Vec::new(),
        parts: Vec::new(),
        capabilities: Vec::new(),
        functions: Vec::new(),
        types: Vec::new(),
        handle: None,
    };
    match symbol {
        Symbol::Widget(name) => {
            let widget: WidgetSchema = widget::builtin(name).expect("a listed widget");
            Schema {
                inputs: widget
                    .properties()
                    .map(|(path, p)| InputSchema {
                        invalidates: dirty_names(property_dirty_class(&path)),
                        name: path,
                        ty: p.kind.name().to_owned(),
                        required: false,
                        default: None,
                        two_way: p.two_way,
                    })
                    .collect(),
                events: widget
                    .event_specs()
                    .map(|e| EventSchema {
                        name: e.name.to_owned(),
                        payload: e.payload.map(str::to_owned),
                        bubbles: e.bubbles,
                        cancelable: e.bubbles,
                    })
                    .collect(),
                ..empty(SchemaKind::Component, WIDGET_VERSION.to_owned())
            }
        }
        Symbol::Library(library) => {
            let prefix = format!("{}::", library.path);
            let functions: Vec<FunctionSchema> = natives
                .functions()
                .iter()
                .filter(|f| f.owner.is_none() && f.path.starts_with(&prefix))
                .map(|f| function(f, false))
                .collect();
            Schema {
                capabilities: union(functions.iter().map(|f| f.capabilities.as_slice())),
                functions,
                types: natives
                    .types()
                    .iter()
                    .filter(|t| t.path.starts_with(&prefix))
                    .map(|t| t.path.to_string())
                    .collect(),
                ..empty(SchemaKind::Library, library.version.to_string())
            }
        }
        Symbol::Function(entry) => {
            let function = function(entry, false);
            Schema {
                inputs: function.params.clone(),
                capabilities: function.capabilities.clone(),
                functions: vec![function],
                ..empty(SchemaKind::Function, entry.library.version.to_string())
            }
        }
        Symbol::Type(entry) => {
            let methods: Vec<FunctionSchema> = entry
                .ty
                .methods
                .iter()
                .filter_map(|m| {
                    let method = natives.method(entry.id, m.name)?;
                    Some(function(method, method.is_method(natives)))
                })
                .collect();
            Schema {
                capabilities: union(methods.iter().map(|f| f.capabilities.as_slice())),
                functions: methods,
                handle: Some(HandleSchema {
                    ownership: entry.ty.ownership,
                    thread: entry.ty.thread,
                }),
                ..empty(SchemaKind::Type, entry.library.version.to_string())
            }
        }
    }
}

fn function(entry: &NativeEntry, method: bool) -> FunctionSchema {
    let f: &NativeFunction = entry.function;
    FunctionSchema {
        name: f.name.to_owned(),
        symbol: entry.path.to_string(),
        kind: f.kind,
        params: f
            .params
            .iter()
            .map(|p| InputSchema {
                name: p.name.to_owned(),
                ty: p.ty.to_string(),
                required: true,
                default: None,
                invalidates: Vec::new(),
                two_way: false,
            })
            .collect(),
        returns: f.ret.to_string(),
        method,
        capabilities: f.capabilities.iter().map(|&c| c.to_owned()).collect(),
        thread: f.thread,
        deterministic: f.deterministic,
        realtime_safe: f.realtime_safe,
        cost: f.cost,
    }
}

/// The sorted union of capability lists.
fn union<'a>(lists: impl Iterator<Item = &'a [String]>) -> Vec<String> {
    let mut all: Vec<String> = lists.flatten().cloned().collect();
    all.sort();
    all.dedup();
    all
}

/// The canonical names of the classes in `class`.
fn dirty_names(class: DirtyClass) -> Vec<&'static str> {
    DIRTY_NAMES
        .iter()
        .filter(|(c, _)| class.contains(*c))
        .map(|&(_, name)| name)
        .collect()
}

impl Schema {
    /// Writes the section 139 schema object.
    pub fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("kind");
        w.string(self.kind.name());
        w.name("symbol");
        w.string(&self.symbol);
        w.name("version");
        w.string(&self.version);
        if let Some(member) = &self.member {
            w.name("member");
            w.string(member);
        }
        w.name("inputs");
        w.begin_array();
        for input in &self.inputs {
            write_input(w, input);
        }
        w.end_array();
        w.name("events");
        w.begin_array();
        for event in &self.events {
            w.begin_object();
            w.name("name");
            w.string(&event.name);
            w.name("payload");
            optional(w, event.payload.as_deref());
            w.name("bubbles");
            w.bool(event.bubbles);
            w.name("cancelable");
            w.bool(event.cancelable);
            w.end_object();
        }
        w.end_array();
        w.name("slots");
        strings(w, &self.slots);
        w.name("parts");
        strings(w, &self.parts);
        w.name("capabilities");
        strings(w, &self.capabilities);
        if self.kind != SchemaKind::Component {
            w.name("functions");
            w.begin_array();
            for function in &self.functions {
                write_function(w, function);
            }
            w.end_array();
        }
        if self.kind == SchemaKind::Library {
            w.name("types");
            strings(w, &self.types);
        }
        if let Some(handle) = self.handle {
            w.name("ownership");
            w.string(handle.ownership.name());
            w.name("thread");
            w.string(handle.thread.name());
        }
        w.end_object();
    }
}

fn write_input(w: &mut JsonWriter, input: &InputSchema) {
    w.begin_object();
    w.name("name");
    w.string(&input.name);
    w.name("type");
    w.string(&input.ty);
    w.name("required");
    w.bool(input.required);
    w.name("default");
    optional(w, input.default.as_deref());
    w.name("invalidates");
    w.begin_array();
    for class in &input.invalidates {
        w.string(class);
    }
    w.end_array();
    if input.two_way {
        w.name("two_way");
        w.bool(true);
    }
    w.end_object();
}

fn write_function(w: &mut JsonWriter, function: &FunctionSchema) {
    w.begin_object();
    w.name("name");
    w.string(&function.name);
    w.name("symbol");
    w.string(&function.symbol);
    w.name("kind");
    w.string(function.kind.keyword());
    w.name("method");
    w.bool(function.method);
    w.name("params");
    w.begin_array();
    for param in &function.params {
        w.begin_object();
        w.name("name");
        w.string(&param.name);
        w.name("type");
        w.string(&param.ty);
        w.end_object();
    }
    w.end_array();
    w.name("returns");
    w.string(&function.returns);
    w.name("capabilities");
    strings(w, &function.capabilities);
    w.name("thread");
    w.string(function.thread.name());
    w.name("deterministic");
    w.bool(function.deterministic);
    w.name("realtime_safe");
    w.bool(function.realtime_safe);
    w.name("cost");
    w.uint(u64::from(function.cost));
    w.end_object();
}

fn optional(w: &mut JsonWriter, value: Option<&str>) {
    match value {
        Some(value) => w.string(value),
        None => w.null(),
    }
}

fn strings(w: &mut JsonWriter, values: &[String]) {
    w.begin_array();
    for value in values {
        w.string(value);
    }
    w.end_array();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standard() -> std::sync::Arc<Natives> {
        Natives::standard()
    }

    #[test]
    fn a_widget_is_found_by_name_or_full_path() {
        let natives = standard();
        let short = query("Button", &natives).expect("Button");
        let full = query("viso::widgets::Button", &natives).expect("full path");
        assert_eq!(short, full);
        assert_eq!(short.kind, SchemaKind::Component);
        let text = short
            .inputs
            .iter()
            .find(|i| i.name == "text")
            .expect("text");
        assert_eq!(text.ty, "String");
        assert_eq!(
            text.invalidates,
            ["MEASURE", "LAYOUT", "PAINT", "SEMANTICS"]
        );
        let click = short
            .events
            .iter()
            .find(|e| e.name == "click")
            .expect("click");
        assert_eq!(click.payload.as_deref(), Some("ClickEvent"));
        assert!(click.bubbles && click.cancelable);
        assert!(short.inputs.iter().any(|i| i.name == "semantics.label"));
    }

    #[test]
    fn a_member_query_narrows_to_that_member() {
        let natives = standard();
        let text = query("Button.text", &natives).expect("Button.text");
        assert_eq!(text.member.as_deref(), Some("text"));
        assert_eq!(text.inputs.len(), 1);
        assert!(text.events.is_empty());

        let error = query("Button.txt", &natives).expect_err("no member");
        assert_eq!(error.code, "E2001");
        assert_eq!(error.notes, ["did you mean `viso::widgets::Button.text`?"]);
    }

    #[test]
    fn a_native_function_reports_kind_thread_and_capabilities() {
        let natives = standard();
        let upper = query("text::upper", &natives).expect("upper");
        assert_eq!(upper.kind, SchemaKind::Function);
        assert_eq!(upper.symbol, "viso::text::upper");
        let f = &upper.functions[0];
        assert_eq!(f.kind, NativeKind::Fn);
        assert!(f.deterministic);
        assert_eq!(signature(f), "fn upper(text: String) -> String");

        let write = query("viso::clipboard::write_text", &natives).expect("write_text");
        assert_eq!(write.functions[0].kind, NativeKind::Action);
        assert_eq!(write.capabilities, ["clipboard.write"]);
    }

    #[test]
    fn a_native_type_lists_its_methods_and_ownership() {
        let natives = standard();
        let watch = query("Stopwatch", &natives).expect("Stopwatch");
        assert_eq!(watch.kind, SchemaKind::Type);
        let handle = watch.handle.expect("handle");
        assert_eq!(handle.ownership, Ownership::Shared);
        assert_eq!(handle.thread, ThreadDomain::Ui);
        let methods: Vec<_> = watch
            .functions
            .iter()
            .map(|f| (&*f.name, f.method))
            .collect();
        assert_eq!(methods, [("start", false), ("elapsed_ms", true)]);

        let library = query("viso::clipboard", &natives).expect("library");
        assert_eq!(library.kind, SchemaKind::Library);
        assert_eq!(library.capabilities, ["clipboard.read", "clipboard.write"]);
    }

    #[test]
    fn an_unknown_symbol_suggests_the_nearest() {
        let natives = standard();
        let error = query("Buton", &natives).expect_err("unknown");
        assert_eq!(error.code, "E2001");
        assert_eq!(error.notes, ["did you mean `viso::widgets::Button`?"]);
    }

    #[test]
    fn search_matches_symbols_and_members() {
        let natives = standard();
        let hits = search("elapsed", &natives);
        let paths: Vec<_> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["viso::time::Stopwatch.elapsed_ms"]);
        assert_eq!(hits[0].detail, "action elapsed_ms(this: Stopwatch) -> F64");
        assert!(
            search("UPPER", &natives)
                .iter()
                .any(|h| h.path == "viso::text::upper")
        );
    }

    #[test]
    fn the_json_object_has_the_section_139_fields() {
        let natives = standard();
        let mut w = JsonWriter::new();
        query("Button.text", &natives).unwrap().write_json(&mut w);
        assert_eq!(
            w.as_str(),
            concat!(
                r#"{"kind":"component","symbol":"viso::widgets::Button","version":"1.0","#,
                r#""member":"text","inputs":[{"name":"text","type":"String","required":false,"#,
                r#""default":null,"invalidates":["MEASURE","LAYOUT","PAINT","SEMANTICS"]}],"#,
                r#""events":[],"slots":[],"parts":[],"capabilities":[]}"#
            )
        );
    }
}
