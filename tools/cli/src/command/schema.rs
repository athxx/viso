//! `viso schema` (`Viso_CLI.md` section 18): the typed schema of a widget, native
//! library, function or handle type, or a search over every symbol and member.
//! The schema model and its section 139 JSON object come from `viso-dsl`; this
//! command only picks the form.

use std::fmt::Write as _;

use viso_dsl::schema::{self, FunctionSchema, Natives, Schema, SchemaKind, SearchHit};

use super::{DIAGNOSTICS, SUCCESS};
use crate::args::SchemaArgs;
use crate::output::Output;

pub fn run(args: &SchemaArgs, out: &mut Output) -> u8 {
    let natives = Natives::standard();
    if let Some(term) = &args.search {
        let hits = schema::search(term, &natives);
        out.result(|w| write_hits(w, term, &hits), || hits_text(term, &hits));
        return SUCCESS;
    }
    let symbol = args.symbol.as_deref().unwrap_or_default();
    match schema::query(symbol, &natives) {
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

/// The search result payload: the term and every match.
fn write_hits(w: &mut viso_ende::JsonWriter, term: &str, hits: &[SearchHit]) {
    w.begin_object();
    w.name("query");
    w.string(term);
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
        w.end_object();
    }
    w.end_array();
    w.end_object();
}

fn hits_text(term: &str, hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return format!("no schema symbol or member matches `{term}`\n");
    }
    let rows: Vec<[&str; 2]> = hits.iter().map(|h| [&*h.path, &*h.detail]).collect();
    table(&rows, "")
}

/// The human form of a schema, in the section 18 layout.
fn schema_text(schema: &Schema) -> String {
    let mut text = schema.symbol.clone();
    if let Some(member) = &schema.member {
        let _ = write!(text, ".{member}");
    }
    if schema.kind != SchemaKind::Component {
        let _ = write!(
            text,
            "  ({}, version {})",
            schema.kind.name().replace('_', " "),
            schema.version
        );
    }
    text.push('\n');
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
                let dirty = format!("invalidates: {}{bind}", i.invalidates.join("|"));
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
        let slots: Vec<[&str; 2]> = schema.slots.iter().map(|s| [&**s, "optional"]).collect();
        section("Slots", table(&slots, "  "));
        let parts: Vec<[&str; 1]> = schema.parts.iter().map(|p| [&**p]).collect();
        section("Parts", table(&parts, "  "));
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

/// Rows with every column but the last padded to its widest cell.
fn table<const N: usize, S: AsRef<str>>(rows: &[[S; N]], indent: &str) -> String {
    let mut widths = [0; N];
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.as_ref().chars().count());
        }
    }
    let mut text = String::new();
    for row in rows {
        text.push_str(indent);
        for (column, cell) in row.iter().enumerate() {
            let cell = cell.as_ref();
            if column + 1 == N {
                text.push_str(cell);
            } else {
                let _ = write!(text, "{cell:<width$}  ", width = widths[column]);
            }
        }
        text.push('\n');
    }
    text
}
