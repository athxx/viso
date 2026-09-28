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
use viso_dsl::{Diagnostic, LineIndex, Severity, TextSize};
use viso_project::ConfigDiagnostic;

use crate::args::Global;

/// A file diagnostics can point into: where it is, how to name it, and its text.
pub struct Source<'a> {
    path: &'a Path,
    /// The path relative to the project root when it lies below it, with `/`
    /// separators on every host.
    name: String,
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
            text,
            lines: LineIndex::new(text),
        }
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

impl<'a> Location<'a> {
    fn source(&self) -> Option<&'a Source<'a>> {
        match self {
            Location::Span(source, _) => Some(source),
            _ => None,
        }
    }
}

/// One diagnostic in the renderers' terms, whichever service raised it.
struct Report<'a> {
    severity: &'static str,
    code: &'a str,
    message: &'a str,
    location: Location<'a>,
    /// Secondary ranges in the primary location's source, each with a label.
    related: Vec<(Range<usize>, &'a str)>,
    /// What would have been accepted and what was found, for a mismatch. Only the
    /// event stream carries them; human text leaves them to the message, which
    /// already says them.
    expected: &'a [String],
    actual: Option<&'a str>,
    notes: &'a [String],
    /// Suggested edits, in the primary location's source.
    fixes: &'a [Fix],
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
    pub fn source(&mut self, source: Option<&Source<'_>>, diagnostic: &Diagnostic) {
        let severity = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        let location = match source {
            Some(source) => Location::Span(source, diagnostic.primary.as_usize()),
            None => Location::None,
        };
        // Related ranges only mean something next to the text they index.
        let related = match source {
            Some(_) => diagnostic
                .related
                .iter()
                .map(|(range, label)| (range.as_usize(), label.as_str()))
                .collect(),
            None => Vec::new(),
        };
        self.emit(
            diagnostic.severity == Severity::Error,
            &Report {
                severity,
                code: diagnostic.code,
                message: &diagnostic.message,
                location,
                related,
                expected: &diagnostic.expected,
                actual: diagnostic.actual.as_deref(),
                notes: &diagnostic.notes,
                fixes: &diagnostic.fixes,
            },
        );
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
                fixes: &[],
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
                fixes: &[],
            },
        );
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
