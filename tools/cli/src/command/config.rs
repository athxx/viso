//! `viso config`: the project's configuration as every layer resolved it —
//! each key with its value and where it came from — the manifest in use, and
//! a validation that reports every problem at once. It never writes:
//! `Viso.toml` is the source of truth, edited and reviewed as text.

use viso_ende::JsonWriter;
use viso_project::{ArtifactKind, Entry, KEYS, Origin, Target, Value, nearest};

use crate::args::{ConfigCommand, ConfigGetArgs};
use crate::context::{Context, Loaded};
use crate::error::{CLI_USAGE, SUCCESS};
use crate::output::Output;

pub fn run(ctx: &Context<'_>, command: &ConfigCommand, out: &mut Output) -> u8 {
    match command {
        ConfigCommand::Show => show(ctx, out),
        ConfigCommand::Get(args) => get(ctx, args, out),
        ConfigCommand::Path => path(ctx, out),
        ConfigCommand::Validate => validate(ctx, out),
    }
}

fn entries(ctx: &Context<'_>, out: &mut Output) -> Result<Vec<Entry>, u8> {
    let Loaded {
        project, resolved, ..
    } = ctx.load(Target::Host, ArtifactKind::Build, out)?;
    Ok(viso_project::settings(
        &project,
        &resolved.config,
        ctx.env(),
    ))
}

fn show(ctx: &Context<'_>, out: &mut Output) -> u8 {
    let entries = match entries(ctx, out) {
        Ok(entries) => entries,
        Err(code) => return code,
    };
    out.result(
        |w| {
            w.begin_object();
            for entry in &entries {
                w.name(entry.key);
                sourced(w, entry);
            }
            w.end_object();
        },
        || table(&entries),
    );
    SUCCESS
}

fn get(ctx: &Context<'_>, args: &ConfigGetArgs, out: &mut Output) -> u8 {
    if !KEYS.contains(&args.key.as_str()) {
        let notes = match nearest(&args.key, KEYS) {
            Some(key) => vec![format!("did you mean `{key}`?")],
            None => Vec::new(),
        };
        return out.failure_next(
            CLI_USAGE,
            &format!("`{}` is not a configuration key", args.key),
            &notes,
            &["viso config show".to_owned()],
        );
    }
    let entries = match entries(ctx, out) {
        Ok(entries) => entries,
        Err(code) => return code,
    };
    let entry = entries
        .into_iter()
        .find(|e| e.key == args.key)
        .expect("every key is listed");
    out.result(
        |w| {
            w.begin_object();
            w.name("key");
            w.string(entry.key);
            w.name("value");
            value(w, &entry.value);
            origin(w, entry.origin);
            w.end_object();
        },
        || format!("{entry}\n"),
    );
    SUCCESS
}

fn path(ctx: &Context<'_>, out: &mut Output) -> u8 {
    let project = match ctx.project(out) {
        Ok(project) => project,
        Err(code) => return code,
    };
    let path = project.manifest_path().display().to_string();
    out.result(
        |w| {
            w.begin_object();
            w.name("path");
            w.string(&path);
            w.end_object();
        },
        || format!("{path}\n"),
    );
    SUCCESS
}

/// Every diagnostic of the manifest and the environment, errors and warnings
/// alike; the exit code of the first error.
fn validate(ctx: &Context<'_>, out: &mut Output) -> u8 {
    let Loaded { project, .. } = match ctx.load(Target::Host, ArtifactKind::Build, out) {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };
    let path = project.manifest_path().display().to_string();
    out.result(
        |w| {
            w.begin_object();
            w.name("path");
            w.string(&path);
            w.end_object();
        },
        || format!("{path}: valid\n"),
    );
    SUCCESS
}

/// `{value, origin, from?}`.
fn sourced(w: &mut JsonWriter, entry: &Entry) {
    w.begin_object();
    w.name("value");
    value(w, &entry.value);
    origin(w, entry.origin);
    w.end_object();
}

/// The `origin` layer and, where it has one, the place it names as `from`.
fn origin(w: &mut JsonWriter, origin: Origin) {
    w.name("origin");
    w.string(origin.layer());
    if matches!(
        origin,
        Origin::Env(_) | Origin::ManifestProfile { .. } | Origin::ManifestDefault { .. }
    ) {
        w.name("from");
        w.string(&origin.to_string());
    }
}

fn value(w: &mut JsonWriter, value: &Value) {
    match value {
        Value::Text(text) => w.string(text),
        Value::Integer(n) => w.int(*n),
        Value::Bool(b) => w.bool(*b),
        Value::List(items) => {
            w.begin_array();
            for item in items {
                w.string(item);
            }
            w.end_array();
        }
        Value::Unset => w.null(),
    }
}

/// The human table: key, value and origin, in columns.
fn table(entries: &[Entry]) -> String {
    let rows: Vec<(String, String, String)> = entries
        .iter()
        .map(|e| (e.key.to_owned(), e.value.to_string(), e.origin.to_string()))
        .collect();
    let key = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let value = rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(0);
    let mut text = String::new();
    for (k, v, o) in &rows {
        let pad = value - v.chars().count();
        text.push_str(&format!("{k:key$}  {v}{:pad$}  {o}\n", ""));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults `config show` reports are those the consumers use.
    #[test]
    fn the_reported_defaults_are_the_ones_in_effect() {
        use viso_project::settings::defaults;
        assert_eq!(
            defaults::TICK_RATE,
            viso_dsl::hir::TargetProfile::default().tick_rate
        );
        assert_eq!(defaults::I18N_SOURCE, viso_dsl::i18n::DEFAULT_SOURCE);
        assert_eq!(defaults::ANDROID_MIN_SDK, viso_package::android::MIN_API);
        assert_eq!(defaults::IOS_MINIMUM_OS, viso_package::apple::MIN_IOS);
    }

    #[test]
    fn the_table_aligns_its_columns() {
        let entries = [
            Entry {
                key: "profile",
                value: Value::Text("dev".into()),
                origin: Origin::FrameworkDefault,
            },
            Entry {
                key: "web.serve.port",
                value: Value::Integer(8080),
                origin: Origin::Env("VISO_X"),
            },
        ];
        assert_eq!(
            table(&entries),
            "profile         dev   default\nweb.serve.port  8080  env VISO_X\n"
        );
    }

    #[test]
    fn a_value_is_written_with_its_json_type() {
        let json = |v: Value| {
            let mut w = JsonWriter::new();
            value(&mut w, &v);
            w.into_string()
        };
        assert_eq!(json(Value::Text("dev".into())), "\"dev\"");
        assert_eq!(json(Value::Integer(8)), "8");
        assert_eq!(json(Value::Bool(true)), "true");
        assert_eq!(json(Value::List(vec!["a".into()])), "[\"a\"]");
        assert_eq!(json(Value::Unset), "null");
    }
}
