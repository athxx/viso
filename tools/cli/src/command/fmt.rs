//! `viso fmt`: lays out `.vs` sources with the formatter the language server
//! uses, and normalizes `Viso.toml`'s whitespace — the project's (`src/`,
//! `tests/` and the manifest) or the files and directories named — rewriting,
//! whole and in place, only the files whose text changes. A file that does not
//! parse is reported and left as it is. `--check` writes nothing and fails on
//! any file that is not formatted.

use std::fs;
use std::path::Path;

use viso_dsl::format::try_format;
use viso_project::discovery::MANIFEST_NAME;
use viso_project::format_manifest;

use crate::args::FmtArgs;
use crate::atomic;
use crate::context::Context;
use crate::error::{DIAGNOSTICS, ENV_SOURCE_UNREADABLE, ENV_SOURCE_WRITE, ENVIRONMENT, SUCCESS};
use crate::output::{Output, Source};

/// The project directories `viso fmt` formats when given no path.
const PROJECT_DIRS: [&str; 2] = ["src", "tests"];

pub fn run(ctx: &Context<'_>, args: &FmtArgs, out: &mut Output) -> u8 {
    let cwd = match ctx.cwd(out) {
        Ok(cwd) => cwd,
        Err(code) => return code,
    };
    // Names are relative to the project root when there is one, else to the
    // current directory.
    let (root, roots) = if args.paths.is_empty() {
        let project = match ctx.project(out) {
            Ok(project) => project,
            Err(code) => return code,
        };
        let mut roots = PROJECT_DIRS.map(|dir| project.root.join(dir)).to_vec();
        roots.push(project.manifest_path());
        (project.root, roots)
    } else {
        let root = viso_project::find_root(&cwd).unwrap_or_else(|| cwd.clone());
        (root, args.paths.iter().map(|p| cwd.join(p)).collect())
    };

    let mut files = Vec::new();
    let mut unreadable = Vec::new();
    for path in &roots {
        if path.is_dir() {
            viso_dsl::package::collect_sources(path, &mut files, &mut unreadable);
        } else if path.exists() || !args.paths.is_empty() {
            // A named file is formatted whatever its extension, and one that
            // is not there is unreadable.
            files.push(path.clone());
        }
    }
    files.sort();
    files.dedup();

    let mut tally = Tally::default();
    for (path, error) in &unreadable {
        out.failure(
            ENV_SOURCE_UNREADABLE,
            &format!("cannot read `{}`: {error}", path.display()),
            &[],
        );
        tally.environment = true;
    }
    for path in &files {
        file(path, &root, args.check, out, &mut tally);
    }
    tally.report(args.check, out)
}

/// What the files came to.
#[derive(Default)]
struct Tally {
    formatted: usize,
    unchanged: usize,
    /// Files that do not parse.
    broken: usize,
    /// A file could not be read or written.
    environment: bool,
}

impl Tally {
    fn report(&self, check: bool, out: &mut Output) -> u8 {
        if out.human() {
            let total = self.formatted + self.unchanged + self.broken;
            let verb = if check { "to format" } else { "formatted" };
            let mut line = format!("{} of {total} files {verb}", self.formatted);
            if self.broken > 0 {
                line.push_str(&format!(", {} not parsed", self.broken));
            }
            eprintln!("{line}");
        }
        if self.environment {
            ENVIRONMENT
        } else if self.broken > 0 || (check && self.formatted > 0) {
            DIAGNOSTICS
        } else {
            SUCCESS
        }
    }
}

fn file(path: &Path, root: &Path, check: bool, out: &mut Output, tally: &mut Tally) {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            out.failure(
                ENV_SOURCE_UNREADABLE,
                &format!("cannot read `{}`: {error}", path.display()),
                &[],
            );
            tally.environment = true;
            return;
        }
    };
    let source = Source::new(path, root, &text);
    let manifest = path.file_name().is_some_and(|name| name == MANIFEST_NAME);
    let formatted = if manifest {
        match format_manifest(path, &text) {
            Ok(formatted) => formatted,
            Err(error) => {
                out.config(Some(&source), &error);
                tally.broken += 1;
                return;
            }
        }
    } else {
        match try_format(&text) {
            Ok(formatted) => formatted,
            Err(errors) => {
                for error in &errors {
                    out.source(Some(&source), &[], error);
                }
                tally.broken += 1;
                return;
            }
        }
    };
    if formatted == text {
        tally.unchanged += 1;
        return;
    }
    let line = first_difference(&text, &formatted);
    if !check && let Err(error) = atomic::write(path, formatted.as_bytes()) {
        out.failure(
            ENV_SOURCE_WRITE,
            &format!("cannot write `{}`: {error}", path.display()),
            &[],
        );
        tally.environment = true;
        return;
    }
    tally.formatted += 1;
    let name = source.name().to_owned();
    out.result(
        |w| {
            w.begin_object();
            w.name("file");
            w.string(&name);
            w.name("changed");
            w.bool(true);
            w.name("line");
            w.uint(u64::from(line));
            w.end_object();
        },
        || {
            if check {
                format!("{name}: not formatted from line {line}\n")
            } else {
                format!("formatted {name}\n")
            }
        },
    );
}

/// The 1-based line where `formatted` first differs from `text`.
fn first_difference(text: &str, formatted: &str) -> u32 {
    let mut a = text.split_inclusive('\n');
    let mut b = formatted.split_inclusive('\n');
    let mut line = 1;
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if x == y => line += 1,
            _ => return line,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_difference_is_a_line_number() {
        assert_eq!(first_difference("a\nb\n", "a\nc\n"), 2);
        assert_eq!(first_difference("a\nb", "a\nb\n"), 2);
        assert_eq!(first_difference("x", "y"), 1);
    }
}
