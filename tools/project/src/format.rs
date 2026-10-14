//! `Viso.toml` normalization, for `viso fmt`: only the whitespace TOML gives
//! no meaning to and no comment lives in. One space each side of `=`, no
//! trailing whitespace, one space before a trailing comment, blank-line runs
//! folded to one, one newline at the end. Keys, values, their order, every
//! comment, array and inline-table layout and indentation are kept as written.
//!
//! The result is checked before it is returned: both texts must hold the same
//! document, so a normalization that would change a value is never written.

use std::path::Path;

use toml_edit::visit_mut::{self, VisitMut};
use toml_edit::{Decor, DocumentMut, Item, KeyMut, RawString, Table, Value};

use crate::diag::{ConfigCode, ConfigDiagnostic, Span};

/// The normalized text of the manifest `text` read from `path`, or the
/// diagnostic of a manifest that does not parse.
pub fn format_manifest(path: &Path, text: &str) -> Result<String, ConfigDiagnostic> {
    let mut doc: DocumentMut = text.parse().map_err(|err: toml_edit::TomlError| {
        let mut diag =
            ConfigDiagnostic::error(ConfigCode::ManifestSyntax, err.message().trim().to_string())
                .at(path);
        if let Some(range) = err.span() {
            diag = diag.span(Span::from_range(text, range));
        }
        diag
    })?;
    Normalize.visit_document_mut(&mut doc);
    let trailing = doc.trailing().as_str().unwrap_or_default().to_owned();
    doc.set_trailing(lines(&trailing));
    let mut out = doc.to_string();
    let end = out.trim_end_matches(['\n', '\r', ' ', '\t']).len();
    out.truncate(end);
    if !out.is_empty() {
        out.push('\n');
    }
    if same_document(text, &out) {
        Ok(out)
    } else {
        // Never reached by the rules above; a guard, not a path.
        Ok(text.to_owned())
    }
}

struct Normalize;

impl VisitMut for Normalize {
    fn visit_table_mut(&mut self, table: &mut Table) {
        let decor = table.decor_mut();
        normalize_prefix(decor);
        visit_mut::visit_table_mut(self, table);
    }

    fn visit_table_like_kv_mut(&mut self, mut key: KeyMut<'_>, node: &mut Item) {
        if let Item::Value(value) = node {
            let decor = key.leaf_decor_mut();
            normalize_prefix(decor);
            decor.set_suffix(" ");
            let decor = value.decor_mut();
            decor.set_prefix(" ");
            let suffix = decor
                .suffix()
                .and_then(RawString::as_str)
                .unwrap_or_default();
            let suffix = match suffix.find('#') {
                Some(at) => format!(" {}", suffix[at..].trim_end()),
                None => String::new(),
            };
            decor.set_suffix(suffix);
            // Inside arrays and inline tables nothing is touched.
            return;
        }
        visit_mut::visit_table_like_kv_mut(self, key, node);
    }

    // The layout inside an array or inline table is the author's.
    fn visit_array_mut(&mut self, _: &mut toml_edit::Array) {}
    fn visit_inline_table_mut(&mut self, _: &mut toml_edit::InlineTable) {}
}

/// The text before a key or header: its comment lines without trailing
/// whitespace, blank-line runs folded to one, and the indentation of the line
/// it ends on kept.
fn normalize_prefix(decor: &mut Decor) {
    let Some(prefix) = decor.prefix().and_then(RawString::as_str) else {
        return;
    };
    let (body, indent) = match prefix.rfind('\n') {
        Some(at) => (&prefix[..=at], &prefix[at + 1..]),
        None => ("", prefix),
    };
    let indent = if indent.trim().is_empty() {
        indent
    } else {
        // Not indentation after all; leave it.
        return;
    };
    let normalized = format!("{}{indent}", lines(body));
    decor.set_prefix(normalized);
}

/// `text`'s lines without trailing whitespace, runs of blank lines folded to
/// one.
fn lines(text: &str) -> String {
    let mut out = String::new();
    let mut blank = false;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end();
        if content.is_empty() {
            if blank {
                continue;
            }
            blank = true;
        } else {
            blank = false;
        }
        out.push_str(content);
        if line.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Whether `a` and `b` hold the same document: the same keys in the same
/// order with the same values, whatever the whitespace and comments.
fn same_document(a: &str, b: &str) -> bool {
    match (a.parse::<DocumentMut>(), b.parse::<DocumentMut>()) {
        (Ok(a), Ok(b)) => same_table(a.as_table(), b.as_table()),
        _ => false,
    }
}

fn same_table(a: &Table, b: &Table) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((ka, a), (kb, b))| ka == kb && same_item(a, b))
}

fn same_item(a: &Item, b: &Item) -> bool {
    match (a, b) {
        (Item::None, Item::None) => true,
        (Item::Value(a), Item::Value(b)) => same_value(a, b),
        (Item::Table(a), Item::Table(b)) => same_table(a, b),
        (Item::ArrayOfTables(a), Item::ArrayOfTables(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_table(a, b))
        }
        _ => false,
    }
}

fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Float(a), Value::Float(b)) => a.value().to_bits() == b.value().to_bits(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::Datetime(a), Value::Datetime(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_value(a, b))
        }
        (Value::InlineTable(a), Value::InlineTable(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|((ka, a), (kb, b))| ka == kb && same_value(a, b))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(text: &str) -> String {
        format_manifest(Path::new("Viso.toml"), text).unwrap()
    }

    #[test]
    fn spacing_trailing_whitespace_and_blank_runs_are_normalized() {
        let text = "# The demo.  \n[package]\nname=\"demo\"   \nversion   =  \"1.0\"  # pinned  \n\n\n\n[web.serve]\n  port = 9000\n\n\n";
        assert_eq!(
            format(text),
            "# The demo.\n[package]\nname = \"demo\"\nversion = \"1.0\" # pinned\n\n[web.serve]\n  port = 9000\n"
        );
    }

    #[test]
    fn arrays_inline_tables_and_strings_are_kept_as_written() {
        let text = "[package]\ncapabilities = [ \"camera\",\n    \"mic\" ]\nmeta = {a=1,  b = 2}\nnote = '''\n  two  \n   lines  '''\n";
        assert_eq!(format(text), text);
    }

    #[test]
    fn formatting_is_idempotent() {
        let text = "[package]\nname=\"demo\"\n\n\n[profile.release]\nstrip=true # small\n";
        let once = format(text);
        assert_eq!(format(&once), once);
    }

    #[test]
    fn a_manifest_that_does_not_parse_is_a_diagnostic() {
        let error = format_manifest(Path::new("Viso.toml"), "[package\nname = 1\n").unwrap_err();
        assert_eq!(error.code, ConfigCode::ManifestSyntax);
        assert!(error.span.is_some());
    }
}
