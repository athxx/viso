//! How results reach the user: human text (`Viso_CLI.md` section 37) or the JSON
//! event stream (sections 34–36).
//!
//! Both forms render one [`Report`] model, so a diagnostic says the same thing in
//! either, and [`Output`] counts what it reports so the summary and the exit code
//! agree with it.

pub mod human;
pub mod json;

use std::ops::Range;
use std::path::Path;

use viso_dsl::diag::Fix;
use viso_dsl::hotreload::event::ReloadEvent;
use viso_dsl::{Diagnostic, LineIndex, Severity, TextSize};
use viso_ende::JsonWriter;
use viso_project::ConfigDiagnostic;

use crate::args::Global;

/// A file diagnostics can point into: where it is, how to name it, and its text.
pub struct Source<'a> {
    path: &'a Path,
    /// The path relative to the project root when it lies below it, with `/`
    /// separators on every host.
    name: String,
    /// The `::`-joined path of the module the file holds, for a package source.
    module: Option<String>,
    text: &'a str,
    lines: LineIndex,
}

impl<'a> Source<'a> {
    /// A source at `path`, named relative to `root` when it lies below it.
    pub fn new(path: &'a Path, root: &Path, text: &'a str) -> Self {
        let relative = path.strip_prefix(root).unwrap_or(path);
        let name = if relative.is_relative() {
            relative
                .iter()
                .map(|part| part.to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        } else {
            relative.display().to_string()
        };
        Self {
            path,
            name,
            module: None,
            text,
            lines: LineIndex::new(text),
        }
    }

    /// This source as the file of the module `module` (`::`-joined, `""` for the
    /// root module), so diagnostics of other files can point into it.
    pub fn of_module(mut self, module: String) -> Self {
        self.module = Some(module);
        self
    }

    /// An offset clamped into the text.
    fn clamp(&self, offset: usize) -> usize {
        offset.min(self.text.len())
    }

    /// The 1-based line and character column of a byte offset.
    fn position(&self, offset: usize) -> (u32, u32) {
        let at = self
            .lines
            .line_col_scalar(TextSize::new(self.clamp(offset) as u32));
        (at.line + 1, at.column + 1)
    }

    /// The 1-based line and UTF-16 column of a byte offset.
    fn position_utf16(&self, offset: usize) -> (u32, u32) {
        let at = self
            .lines
            .line_col_utf16(TextSize::new(self.clamp(offset) as u32));
        (at.line + 1, at.column + 1)
    }

    /// The line holding `offset`, without its terminator, and its start offset.
    fn line_at(&self, offset: usize) -> (&'a str, usize) {
        let offset = self.clamp(offset);
        let column = self
            .lines
            .line_col_utf8(TextSize::new(offset as u32))
            .column;
        let start = offset - column as usize;
        let rest = &self.text[start..];
        let line = rest.split('\n').next().unwrap_or(rest);
        (line.strip_suffix('\r').unwrap_or(line), start)
    }
}

/// Where a report points.
enum Location<'a> {
    /// A byte range of a source whose text is at hand.
    Span(&'a Source<'a>, Range<usize>),
    /// A place in a file whose text could not be read again to show it.
    Point {
        file: String,
        line: u32,
        column: u32,
    },
    /// Nowhere in particular.
    None,
}

/// A labeled secondary range, in its own source.
struct Secondary<'a> {
    source: &'a Source<'a>,
    range: Range<usize>,
    label: &'a str,
}

/// A suggested fix, each edit resolved to the source it changes.
struct Suggestion<'a> {
    title: &'a str,
    applicability: &'static str,
    edits: Vec<Edit<'a>>,
}

/// One edit of a [`Suggestion`]; `source` is `None` when the file it changes is
/// not at hand.
struct Edit<'a> {
    source: Option<&'a Source<'a>>,
    range: Range<usize>,
    replacement: &'a str,
}

/// One diagnostic in the renderers' terms, whichever service raised it.
struct Report<'a> {
    severity: &'static str,
    code: &'a str,
    message: &'a str,
    location: Location<'a>,
    /// Secondary ranges, each with a label, in the primary location's source or
    /// another file of the package.
    related: Vec<Secondary<'a>>,
    /// What would have been accepted and what was found, for a mismatch. Only the
    /// event stream carries them; human text leaves them to the message, which
    /// already says them.
    expected: &'a [String],
    actual: Option<&'a str>,
    notes: &'a [String],
    fixes: Vec<Suggestion<'a>>,
}

/// One test's outcome.
pub struct Test<'a> {
    pub name: &'a str,
    /// `game`, `ui`, ...
    pub domain: &'a str,
    pub passed: bool,
    pub duration_ms: u64,
    /// Why it failed.
    pub message: Option<&'a str>,
}

/// The output of one command run.
pub struct Output {
    form: Form,
    quiet: bool,
    errors: usize,
    warnings: usize,
    checked: Option<(String, usize)>,
}

enum Form {
    Human,
    Json(json::Stream),
}

impl Output {
    /// Output for `command` in the form and verbosity `global` asks for.
    pub fn new(global: &Global, command: Option<&str>) -> Self {
        Self {
            form: if global.json {
                Form::Json(json::Stream::new(command))
            } else {
                Form::Human
            },
            quiet: global.quiet,
            errors: 0,
            warnings: 0,
            checked: None,
        }
    }

    /// The errors reported so far.
    pub fn errors(&self) -> usize {
        self.errors
    }

    /// Reports a compiler diagnostic, in `source` when it has a source position.
    /// `package` holds the package's module sources, which its related ranges and
    /// fixes may point into.
    pub fn source(
        &mut self,
        source: Option<&Source<'_>>,
        package: &[Source<'_>],
        diagnostic: &Diagnostic,
    ) {
        let report = compiler_report(source, package, diagnostic);
        self.emit(diagnostic.severity == Severity::Error, &report);
    }

    /// Reports a project or configuration diagnostic, with its source line when
    /// `manifest` is the file it points into.
    pub fn config(&mut self, manifest: Option<&Source<'_>>, diagnostic: &ConfigDiagnostic) {
        let location = match (&diagnostic.path, diagnostic.span) {
            (Some(path), Some(span)) => match manifest.filter(|m| m.path == path) {
                Some(manifest) => Location::Span(manifest, span.start as usize..span.end as usize),
                None => Location::Point {
                    file: path.display().to_string(),
                    line: span.line,
                    column: span.column,
                },
            },
            _ => Location::None,
        };
        self.emit(
            diagnostic.is_error(),
            &Report {
                severity: diagnostic.severity.as_str(),
                code: diagnostic.code.as_str(),
                message: &diagnostic.message,
                location,
                related: Vec::new(),
                expected: &[],
                actual: None,
                notes: &diagnostic.notes,
                fixes: Vec::new(),
            },
        );
    }

    /// Reports an error the CLI itself raised, under its own `code`.
    pub fn failure(&mut self, code: &str, message: &str, notes: &[String]) {
        self.emit(
            true,
            &Report {
                severity: "error",
                code,
                message,
                location: Location::None,
                related: Vec::new(),
                expected: &[],
                actual: None,
                notes,
                fixes: Vec::new(),
            },
        );
    }

    /// Reports the command's result: a `result` event whose payload `payload`
    /// writes, or the human text `text` builds. A result is not a diagnostic, so
    /// `--quiet` keeps it.
    pub fn result(&mut self, payload: impl FnOnce(&mut JsonWriter), text: impl FnOnce() -> String) {
        match &mut self.form {
            Form::Json(stream) => stream.result(payload),
            Form::Human => print!("{}", text()),
        }
    }

    /// Reports a phase of a long command as a `progress` event. Human text
    /// leaves progress to the tools that print their own, and `--quiet` drops it.
    pub fn progress(&mut self, phase: &str, message: &str) {
        if let Form::Json(stream) = &mut self.form
            && !self.quiet
        {
            stream.progress(phase, message);
        }
    }

    /// Reports a line from `source` (`app`, `device` or `tool`) at `level`: a
    /// `log` event, which `--quiet` drops, or a line on stderr.
    pub fn log(&mut self, level: &str, source: &str, message: &str) {
        match &mut self.form {
            Form::Json(stream) => {
                if !self.quiet {
                    stream.log(level, source, message);
                }
            }
            Form::Human => eprintln!("[viso] {message}"),
        }
    }

    /// Reports a hot reload attempt of `file` in the dev session `session` of
    /// the build `build_id`: a `dev` event, or one line on stderr. Its
    /// diagnostics are reported on their own.
    pub fn dev(&mut self, file: &Source<'_>, session: &str, build_id: &str, event: &ReloadEvent) {
        match &mut self.form {
            Form::Json(stream) => stream.dev(&file.name, session, build_id, event),
            Form::Human => eprintln!("{}", human::dev(&file.name, event)),
        }
    }

    /// Reports the `@probe` states of `scenario` after `tick`, `probes` an
    /// encoded JSON object: a `trace` event, which human text leaves out.
    pub fn trace(&mut self, scenario: &str, tick: u64, probes: &str) {
        if let Form::Json(stream) = &mut self.form {
            stream.trace(scenario, tick, probes);
        }
    }

    /// Reports a test's outcome: a `test` event with the fields `extra`
    /// writes after the common ones, or the human text `text` builds.
    pub fn test(
        &mut self,
        test: &Test<'_>,
        extra: impl FnOnce(&mut JsonWriter),
        text: impl FnOnce() -> String,
    ) {
        match &mut self.form {
            Form::Json(stream) => stream.test(test, extra),
            Form::Human => print!("{}", text()),
        }
    }

    /// Reports how many tests passed and failed: a line of human text; the
    /// event stream has a `test` event for each.
    pub fn tally(&mut self, passed: u64, failed: u64) {
        if let Form::Human = self.form {
            let status = if failed == 0 { "ok" } else { "FAILED" };
            println!("test result: {status}. {passed} passed; {failed} failed");
        }
    }

    /// Records what a check covered, for the summary.
    pub fn checked(&mut self, package: &str, files: usize) {
        self.checked = Some((package.to_string(), files));
    }

    /// Ends the output with its summary and returns `code`, the process exit
    /// code the summary reports.
    pub fn finish(self, code: u8) -> u8 {
        let checked = self.checked.as_ref().map(|(p, n)| (p.as_str(), *n));
        match self.form {
            Form::Human => {
                if let Some((package, files)) = checked {
                    println!(
                        "{}",
                        human::summary(package, files, self.errors, self.warnings)
                    );
                }
            }
            Form::Json(mut stream) => {
                stream.summary(code, self.errors, self.warnings, checked);
            }
        }
        code
    }

    fn emit(&mut self, error: bool, report: &Report<'_>) {
        if error {
            self.errors += 1;
        } else {
            self.warnings += 1;
        }
        match &mut self.form {
            // Quiet keeps diagnostics in the event stream; it only drops progress
            // and log events there (section 6.2).
            Form::Json(stream) => stream.diagnostic(report),
            Form::Human if !error && self.quiet => {}
            Form::Human => human::print(report),
        }
    }
}

/// A compiler diagnostic as a [`Report`]: in `source` when it has one, its related
/// ranges and edits in the file of the module each names (`source` for none).
/// A range with no file at hand to show it in is dropped; an edit keeps its
/// place but names no file.
fn compiler_report<'a>(
    source: Option<&'a Source<'a>>,
    package: &'a [Source<'a>],
    diagnostic: &'a Diagnostic,
) -> Report<'a> {
    let severity = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
    };
    let file = |module: Option<&str>| match module {
        None => source,
        Some(module) => package.iter().find(|s| s.module.as_deref() == Some(module)),
    };
    let location = match source {
        Some(source) => Location::Span(source, diagnostic.primary.as_usize()),
        None => Location::None,
    };
    let related = diagnostic
        .related
        .iter()
        .filter_map(|r| {
            Some(Secondary {
                source: file(r.module.as_deref())?,
                range: r.range.as_usize(),
                label: &r.label,
            })
        })
        .collect();
    let fixes = diagnostic
        .fixes
        .iter()
        .map(|fix: &'a Fix| Suggestion {
            title: &fix.title,
            applicability: fix.applicability.as_str(),
            edits: fix
                .edits
                .iter()
                .map(|edit| Edit {
                    source: file(edit.module.as_deref()),
                    range: edit.range.as_usize(),
                    replacement: &edit.replacement,
                })
                .collect(),
        })
        .collect();
    Report {
        severity,
        code: diagnostic.code,
        message: &diagnostic.message,
        location,
        related,
        expected: &diagnostic.expected,
        actual: diagnostic.actual.as_deref(),
        notes: &diagnostic.notes,
        fixes,
    }
}
