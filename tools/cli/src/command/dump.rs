//! `viso dump`: one compiler IR of the project's package — the syntax tree,
//! the typed HIR, the UI IR, the reactive IR, the Behavior IR, the shader IR,
//! the system IR or the module graph — as text or JSON. The dumps come from
//! `viso-dsl`; this command only loads the package, picks the part to dump
//! and writes it. Every diagnostic the package or the dumped files raise is
//! reported first, then what lowered is dumped: an invalid file still dumps.

use std::path::{Path, PathBuf};

use viso_dsl::dump::{self, Dump, DumpKind as IrKind, Selection};
use viso_ende::JsonWriter;

use super::check::{self, Checked};
use crate::args::{DumpArgs, DumpKind};
use crate::context::Context;
use crate::error::{DIAGNOSTICS, DUMP_INPUT, ENV_DUMP_WRITE};
use crate::output::{Output, Source};

pub fn run(ctx: &Context<'_>, args: &DumpArgs, out: &mut Output) -> u8 {
    let checked = match check::load(ctx, out) {
        Ok(checked) => checked,
        Err(code) => return code,
    };
    let (file, symbol) = match args.input.as_deref() {
        Some(input) if names_a_file(input) => match file_of(ctx, &checked, input, out) {
            Ok(file) => (Some(file), args.symbol.as_deref()),
            Err(code) => return code,
        },
        Some(input) => (None, Some(args.symbol.as_deref().unwrap_or(input))),
        None => (None, args.symbol.as_deref()),
    };
    let package = &checked.package;
    let root = &checked.project.root;
    let kind = ir_kind(args.kind);
    let dumped = match dump::dump(
        package,
        root,
        &checked.profile,
        kind,
        Selection { file, symbol },
    ) {
        Ok(dumped) => dumped,
        Err(diagnostic) => {
            out.source(None, &[], &diagnostic);
            return DIAGNOSTICS;
        }
    };
    let sources: Vec<Source<'_>> = package
        .files
        .iter()
        .map(|f| Source::new(&f.path, root, &f.source).of_module(f.module.join("::")))
        .collect();
    for (index, diagnostic) in &dumped.diagnostics {
        let reported = package.files[*index].diagnostics.iter().any(|d| {
            d.code == diagnostic.code
                && d.primary == diagnostic.primary
                && d.message == diagnostic.message
        });
        if !reported {
            out.source(Some(&sources[*index]), &sources, diagnostic);
        }
    }
    let input = args.input.as_deref();
    match &args.out {
        Some(path) => {
            let text = if ctx.global.json {
                let mut w = JsonWriter::new();
                dumped.write_json(&mut w);
                let mut text = w.into_string();
                text.push('\n');
                text
            } else {
                let mut text = String::new();
                dumped.write_text(&mut text);
                text
            };
            if let Err(error) = crate::atomic::write(path, text.as_bytes()) {
                return out.failure(
                    ENV_DUMP_WRITE,
                    &format!("cannot write `{}`: {error}", path.display()),
                    &[],
                );
            }
            out.result(
                |w| {
                    payload(
                        w,
                        kind,
                        input,
                        |w| w.string(&path.display().to_string()),
                        "out",
                    )
                },
                || format!("wrote the {} dump to {}\n", kind.name(), path.display()),
            );
        }
        None => out.result(
            |w| payload(w, kind, input, |w| dumped.write_json(w), "ir"),
            || {
                let mut text = String::new();
                dumped.write_text(&mut text);
                text
            },
        ),
    }
    checked.code(out)
}

/// `{kind, input, <key>}`, the value under `key` written by `value`.
fn payload(
    w: &mut JsonWriter,
    kind: IrKind,
    input: Option<&str>,
    value: impl FnOnce(&mut JsonWriter),
    key: &str,
) {
    w.begin_object();
    w.name("kind");
    w.string(kind.name());
    w.name("input");
    match input {
        Some(input) => w.string(input),
        None => w.null(),
    }
    w.name(key);
    value(w);
    w.end_object();
}

fn ir_kind(kind: DumpKind) -> IrKind {
    match kind {
        DumpKind::Ast => IrKind::Ast,
        DumpKind::Hir => IrKind::Hir,
        DumpKind::UiIr => IrKind::UiIr,
        DumpKind::ReactiveIr => IrKind::ReactiveIr,
        DumpKind::BehaviorIr => IrKind::BehaviorIr,
        DumpKind::ShaderIr => IrKind::ShaderIr,
        DumpKind::SystemIr => IrKind::SystemIr,
        DumpKind::ModuleGraph => IrKind::ModuleGraph,
    }
}

/// Whether an input names a file rather than a declaration.
fn names_a_file(input: &str) -> bool {
    input.ends_with(".vs") || input.contains('/') || input.contains('\\')
}

/// The index of the package file `input`, a path relative to the working
/// directory or to the project root.
fn file_of(
    ctx: &Context<'_>,
    checked: &Checked,
    input: &str,
    out: &mut Output,
) -> Result<usize, u8> {
    let cwd = ctx.cwd(out)?;
    let root = &checked.project.root;
    let canonical = |path: &Path| path.canonicalize().ok();
    let wanted: Vec<PathBuf> = [cwd.join(input), root.join(input)]
        .iter()
        .filter_map(|p| canonical(p))
        .collect();
    let found = checked
        .package
        .files
        .iter()
        .position(|f| canonical(&f.path).is_some_and(|p| wanted.contains(&p)));
    found.ok_or_else(|| {
        out.failure_next(
            DUMP_INPUT,
            &format!("`{input}` is not a `.vs` source of the project"),
            &[format!(
                "the package's sources are the `.vs` files under `{}/`",
                viso_dsl::package::SOURCE_ROOT
            )],
            &["viso dump --help".to_owned()],
        )
    })
}
