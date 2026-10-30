//! `viso schema`: the typed schema of a widget, a project component, record
//! or enum, a native library, function or handle type, a search over every symbol and member,
//! or every symbol of one group. The schema model and its JSON object come
//! from `viso-dsl`; this command only finds the project and picks the form.

use std::fmt::Write as _;
use std::sync::Arc;

use viso_dsl::Diagnostic;
use viso_dsl::hir::TargetProfile;
use viso_dsl::native_package;
use viso_dsl::package::{PackageManifest, load_package_in};
use viso_dsl::schema::{
    self, Catalog, FunctionSchema, ListKind, Natives, Schema, SchemaKind, SearchHit,
};

use crate::args::{SchemaArgs, SchemaGroup};
use crate::command::check::cargo_manifest;
use crate::context::{Context, in_its_file};
use crate::error::{DIAGNOSTICS, ENV_CARGO, SUCCESS};
use crate::output::Output;

pub fn run(ctx: &Context<'_>, args: &SchemaArgs, out: &mut Output) -> u8 {
    let catalog = match catalog(ctx, out) {
        Ok(catalog) => catalog,
        Err(code) => return code,
    };
    if let Some(group) = args.list {
        let hits = schema::list(list_kind(group), &catalog);
        let name = group_name(group);
        out.result(
            |w| write_hits(w, "list", name, &hits),
            || list_text(name, &hits),
        );
        return SUCCESS;
    }
    if let Some(term) = &args.search {
        let hits = schema::search(term, &catalog);
        out.result(
            |w| write_hits(w, "query", term, &hits),
            || hits_text(term, &hits),
        );
        return SUCCESS;
    }
    let symbol = args.symbol.as_deref().unwrap_or_default();
    match schema::query(symbol, &catalog) {
        Ok(schema) => {
            out.result(|w| schema.write_json(w), || schema_text(&schema));
            SUCCESS
        }
        Err(diagnostic) => {
            out.source(None, &[], &diagnostic);
            DIAGNOSTICS
        }
    }
}

/// The framework's symbols, and the components, records and enums of the
/// project the command runs in, if it runs in one.
fn catalog(ctx: &Context<'_>, out: &mut Output) -> Result<Catalog, u8> {
    let Some(project) = ctx.project_if_any(out)? else {
        return Ok(Catalog::framework(Natives::standard()));
    };
    let name = project.name().map_err(|diagnostic| {
        in_its_file(out, &diagnostic);
        diagnostic.code.exit_code()
    })?;
    // The natives a project's code can call: the standard libraries and its
    // crate's native packages.
    let natives = match cargo_manifest(&project.root).map(|m| native_package::discover(&m)) {
        Some(Ok(packages)) => native_package::registry(&packages).map_err(|conflict| {
            let at = viso_dsl::TextRange::empty(viso_dsl::TextSize::from(0));
            out.source(
                None,
                &[],
                &Diagnostic::error("E6101", at, conflict.to_string()),
            );
            DIAGNOSTICS
        })?,
        Some(Err(why)) => {
            out.warning(
                ENV_CARGO,
                &format!("the native packages are not known: {why}"),
                &["the schema holds the standard libraries alone".to_owned()],
            );
            Natives::standard()
        }
        None => Natives::standard(),
    };
    let catalog = Catalog::framework(Arc::clone(&natives));
    let package = load_package_in(
        &project.root,
        PackageManifest {
            name,
            language: None,
            profile: TargetProfile::default(),
        },
        natives,
    );
    let version = project
        .manifest
        .package
        .version
        .as_ref()
        .map_or("0.0.0", |version| version.value.as_str());
    Ok(catalog.with_package(schema::package_schemas(&package, &project.root, version)))
}

fn list_kind(group: SchemaGroup) -> ListKind {
    match group {
        SchemaGroup::Widgets => ListKind::Widgets,
        SchemaGroup::Components => ListKind::Components,
        SchemaGroup::Natives => ListKind::Natives,
        SchemaGroup::Types => ListKind::Types,
    }
}

fn group_name(group: SchemaGroup) -> &'static str {
    match group {
        SchemaGroup::Widgets => "widgets",
        SchemaGroup::Components => "components",
        SchemaGroup::Natives => "natives",
        SchemaGroup::Types => "types",
    }
}

/// The search or list payload: what was asked, under `key`, and every match.
fn write_hits(w: &mut viso_ende::JsonWriter, key: &str, asked: &str, hits: &[SearchHit]) {
    w.begin_object();
    w.name("schema_version");
    w.string(schema::SCHEMA_VERSION);
    w.name(key);
    w.string(asked);
    w.name("matches");
    w.begin_array();
    for hit in hits {
        w.begin_object();
        w.name("path");
        w.string(&hit.path);
        w.name("kind");
        w.string(hit.kind.name());
        w.name("detail");
        w.string(&hit.detail);
        w.name("origin");
        w.string(hit.origin.as_deref().unwrap_or("framework"));
        w.end_object();
    }
    w.end_array();
    w.end_object();
}

fn hits_text(term: &str, hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return format!("no schema symbol or member matches `{term}`\n");
    }
    rows_text(hits)
}

fn list_text(group: &str, hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return format!("no {group}\n");
    }
    rows_text(hits)
}

/// One row per hit: its path, its detail, and where a project declares it.
fn rows_text(hits: &[SearchHit]) -> String {
    if hits.iter().all(|h| h.origin.is_none()) {
        let rows: Vec<[&str; 2]> = hits.iter().map(|h| [&*h.path, &*h.detail]).collect();
        return table(&rows, "");
    }
    let rows: Vec<[&str; 3]> = hits
        .iter()
        .map(|h| {
            [
                &*h.path,
                &*h.detail,
                h.origin.as_deref().unwrap_or("framework"),
            ]
        })
        .collect();
    table(&rows, "")
}

/// The human form of a schema.
fn schema_text(schema: &Schema) -> String {
    let mut text = schema.symbol.clone();
    if let Some(member) = &schema.member {
        let _ = write!(text, ".{member}");
    }
    match (schema.kind, &schema.origin) {
        (SchemaKind::Component, _) => {}
        (kind, Some(_)) => {
            let _ = write!(text, "  ({})", kind.name());
        }
        (kind, None) => {
            let _ = write!(
                text,
                "  ({}, version {})",
                kind.name().replace('_', " "),
                schema.version
            );
        }
    }
    text.push('\n');
    if let Some(origin) = &schema.origin {
        let _ = writeln!(
            text,
            "  {}  {}  version {}  revision {:032x}",
            origin.location(),
            origin.visibility(),
            schema.version,
            origin.revision
        );
    }
    if let Some(handle) = schema.handle {
        let _ = writeln!(
            text,
            "  ownership: {}  thread: {}",
            handle.ownership.name(),
            handle.thread.name()
        );
    }
    let mut section = |title: &str, body: String| {
        if !body.is_empty() {
            let _ = write!(text, "\n{title}\n{body}");
        }
    };
    if schema.kind == SchemaKind::Component {
        let rows: Vec<[String; 3]> = schema
            .inputs
            .iter()
            .map(|i| {
                let ty = match &i.default {
                    Some(default) => format!("{} = {default}", i.ty),
                    None => i.ty.clone(),
                };
                let bind = if i.two_way { "  bind" } else { "" };
                let primary = if i.primary { "  primary" } else { "" };
                let dirty = format!("invalidates: {}{bind}{primary}", i.invalidates.join("|"));
                [i.name.clone(), ty, dirty]
            })
            .collect();
        section("Properties", table(&rows, "  "));
        let rows: Vec<[String; 2]> = schema
            .events
            .iter()
            .map(|e| {
                let payload = e.payload.clone().unwrap_or_else(|| "()".to_owned());
                let bubbles = if e.bubbles { "  bubbles" } else { "" };
                [e.name.clone(), format!("{payload}{bubbles}")]
            })
            .collect();
        section("Events", table(&rows, "  "));
        let slots: Vec<[&str; 3]> = schema
            .slots
            .iter()
            .map(|s| {
                let default = if s.default { "default" } else { "" };
                [&*s.name, s.cardinality.type_name(), default]
            })
            .collect();
        section("Slots", table(&slots, "  "));
        let parts: Vec<[&str; 1]> = schema.parts.iter().map(|p| [&**p]).collect();
        section("Parts", table(&parts, "  "));
    } else if schema.kind == SchemaKind::Record {
        let rows: Vec<[String; 2]> = schema
            .fields
            .iter()
            .map(|f| match &f.default {
                Some(default) => [f.name.clone(), format!("{} = {default}", f.ty)],
                None => [f.name.clone(), f.ty.clone()],
            })
            .collect();
        section("Fields", table(&rows, "  "));
    } else if schema.kind == SchemaKind::Enum {
        let rows: Vec<[&str; 2]> = schema
            .variants
            .iter()
            .map(|v| [&*v.name, v.payload.as_deref().unwrap_or("")])
            .collect();
        section("Variants", table(&rows, "  "));
    } else {
        let title = match schema.kind {
            SchemaKind::Type => "Methods",
            SchemaKind::Library => "Functions",
            _ => "Signature",
        };
        let body: String = schema.functions.iter().map(function_text).collect();
        section(title, body);
        let types: Vec<[&str; 1]> = schema.types.iter().map(|t| [&**t]).collect();
        section("Types", table(&types, "  "));
    }
    let capabilities: Vec<[&str; 1]> = schema.capabilities.iter().map(|c| [&**c]).collect();
    section("Capabilities", table(&capabilities, "  "));
    text
}

/// A function's signature line and its traits.
fn function_text(function: &FunctionSchema) -> String {
    let mut traits = vec![format!("thread: {}", function.thread.name())];
    if function.deterministic {
        traits.push("deterministic".to_owned());
    }
    if function.realtime_safe {
        traits.push("realtime-safe".to_owned());
    }
    traits.push(format!("cost: {}", function.cost));
    if !function.capabilities.is_empty() {
        traits.push(format!("requires: {}", function.capabilities.join(", ")));
    }
    format!(
        "  {}\n      {}\n",
        schema::signature(function),
        traits.join("  ")
    )
}

/// Rows with every column but the last padded to its widest cell, and no
/// line ending in spaces.
fn table<const N: usize, S: AsRef<str>>(rows: &[[S; N]], indent: &str) -> String {
    let mut widths = [0; N];
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.as_ref().chars().count());
        }
    }
    let mut text = String::new();
    for row in rows {
        let start = text.len();
        text.push_str(indent);
        for (column, cell) in row.iter().enumerate() {
            let cell = cell.as_ref();
            if column + 1 == N {
                text.push_str(cell);
            } else {
                let _ = write!(text, "{cell:<width$}  ", width = widths[column]);
            }
        }
        // An empty last cell leaves no padding behind.
        let end = start + text[start..].trim_end().len();
        text.truncate(end);
        text.push('\n');
    }
    text
}
