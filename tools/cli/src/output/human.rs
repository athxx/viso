//! Human-readable output (`Viso_CLI.md` section 37): diagnostics and progress to
//! stderr, the summary to stdout.
//!
//! A diagnostic prints its header, its location and the source line under a caret
//! underline, then its related spans, notes and fix titles:
//!
//! ```text
//! error[E2001]: unresolved name `Pointt`
//!  --> src/app.vs:3:14
//!   |
//! 3 |     state p: Pointt;
//!   |              ^^^^^^
//!   = help: replace with `Point`
//! ```

use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::ops::Range;
use std::path::Path;

use viso_dsl::{Diagnostic, LineIndex, Severity, TextSize};
use viso_project::ConfigDiagnostic;

/// A file diagnostics can point into: where it is, how to name it, and its text.
pub struct Source<'a> {
    path: &'a Path,
    display: String,
    text: &'a str,
    lines: LineIndex,
}

impl<'a> Source<'a> {
    /// A source at `path`, named relative to `root` when it lies below it.
    pub fn new(path: &'a Path, root: &Path, text: &'a str) -> Self {
        Self {
            path,
            display: path
                .strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string(),
            text,
            lines: LineIndex::new(text),
        }
    }

    /// The 1-based line and character column of a byte offset.
    fn position(&self, offset: usize) -> (u32, u32) {
        let at = self.lines.line_col_scalar(TextSize::new(offset as u32));
        (at.line + 1, at.column + 1)
    }

    /// The line holding `offset`, without its terminator, and its start offset.
    fn line_at(&self, offset: usize) -> (&'a str, usize) {
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
    /// A position rendered ahead of time, when the text is not at hand.
    Point(String),
    /// Nowhere in particular.
    None,
}

/// One diagnostic in the renderer's terms, whichever service raised it.
struct Report<'a> {
    severity: &'static str,
    code: &'a str,
    message: &'a str,
    location: Location<'a>,
    related: Vec<(Range<usize>, &'a str)>,
    notes: &'a [String],
    helps: Vec<&'a str>,
}

impl Report<'_> {
    fn render(&self) -> String {
        let mut out = String::new();
        // Writing to a `String` cannot fail.
        let _ = self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) -> fmt::Result {
        writeln!(out, "{}[{}]: {}", self.severity, self.code, self.message)?;
        let mut gutter = 0;
        match &self.location {
            Location::Span(source, range) => {
                let last = std::iter::once(range)
                    .chain(self.related.iter().map(|(range, _)| range))
                    .map(|range| source.position(range.start).0)
                    .max()
                    .unwrap_or(1);
                gutter = last.to_string().len();
                let (line, column) = source.position(range.start);
                writeln!(out, "{:gutter$}--> {}:{line}:{column}", "", source.display)?;
                writeln!(out, "{:gutter$} |", "")?;
                snippet(out, source, range, '^', "", gutter)?;
                for (range, label) in &self.related {
                    snippet(out, source, range, '-', label, gutter)?;
                }
            }
            Location::Point(point) => writeln!(out, " --> {point}")?,
            Location::None => {}
        }
        for note in self.notes {
            writeln!(out, "{:gutter$} = note: {note}", "")?;
        }
        for help in &self.helps {
            writeln!(out, "{:gutter$} = help: {help}", "")?;
        }
        Ok(())
    }
}

/// One source line with `range` underlined by `mark`, and an optional label after
/// the underline. A range running past the line is underlined to the line's end.
fn snippet(
    out: &mut String,
    source: &Source<'_>,
    range: &Range<usize>,
    mark: char,
    label: &str,
    gutter: usize,
) -> fmt::Result {
    let start = range.start.min(source.text.len());
    let (text, line_start) = source.line_at(start);
    let (line, _) = source.position(start);
    let line_end = line_start + text.len();
    let end = range.end.clamp(start, line_end);
    // Tabs are copied into the padding so the underline lines up however the
    // terminal expands them.
    let pad: String = source.text[line_start..start.min(line_end)]
        .chars()
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    let width = source.text[start.min(line_end)..end].chars().count().max(1);
    let underline: String = std::iter::repeat_n(mark, width).collect();
    writeln!(out, "{line:>gutter$} | {text}")?;
    let label = if label.is_empty() {
        String::new()
    } else {
        format!(" {label}")
    };
    writeln!(out, "{:gutter$} | {pad}{underline}{label}", "")
}

/// The human output of one command run: counts what it prints so the summary and
/// the exit code agree with it.
pub struct Human {
    quiet: bool,
    errors: usize,
    warnings: usize,
}

impl Human {
    /// Output that prints warnings and notes unless `quiet`.
    pub fn new(quiet: bool) -> Self {
        Self {
            quiet,
            errors: 0,
            warnings: 0,
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
        let report = Report {
            severity,
            code: diagnostic.code,
            message: &diagnostic.message,
            location: match source {
                Some(source) => Location::Span(source, diagnostic.primary.as_usize()),
                None => Location::None,
            },
            related: diagnostic
                .related
                .iter()
                .map(|(range, label)| (range.as_usize(), label.as_str()))
                .collect(),
            notes: &diagnostic.notes,
            helps: diagnostic.fixes.iter().map(|f| f.title.as_str()).collect(),
        };
        self.emit(diagnostic.severity == Severity::Error, &report);
    }

    /// Reports a project or configuration diagnostic, with its source line when
    /// `manifest` is the file it points into.
    pub fn config(&mut self, manifest: Option<&Source<'_>>, diagnostic: &ConfigDiagnostic) {
        let code = diagnostic.code.as_str();
        let location = match (&diagnostic.path, diagnostic.span) {
            (Some(path), Some(span)) => match manifest.filter(|m| m.path == path) {
                Some(manifest) => Location::Span(manifest, span.start as usize..span.end as usize),
                None => Location::Point(format!("{}:{span}", path.display())),
            },
            (Some(path), None) => Location::Point(path.display().to_string()),
            (None, _) => Location::None,
        };
        let report = Report {
            severity: diagnostic.severity.as_str(),
            code,
            message: &diagnostic.message,
            location,
            related: Vec::new(),
            notes: &diagnostic.notes,
            helps: Vec::new(),
        };
        self.emit(diagnostic.is_error(), &report);
    }

    /// Reports an error no service raised as a diagnostic.
    pub fn failure(&mut self, message: impl fmt::Display) {
        self.errors += 1;
        eprintln!("error: {message}");
    }

    /// Prints the one-line summary of a check to stdout.
    pub fn summary(&self, package: &str, files: usize) {
        println!("{}", summary(package, files, self.errors, self.warnings));
    }

    fn emit(&mut self, error: bool, report: &Report<'_>) {
        if error {
            self.errors += 1;
        } else {
            self.warnings += 1;
            if self.quiet {
                return;
            }
        }
        let mut text = report.render();
        text.push('\n');
        let _ = std::io::stderr().lock().write_all(text.as_bytes());
    }
}

fn summary(package: &str, files: usize, errors: usize, warnings: usize) -> String {
    let count = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    let mut line = format!("checked `{package}`: {}", count(files, "file"));
    match (errors, warnings) {
        (0, 0) => line.push_str(", no problems"),
        _ => {
            for (n, what) in [(errors, "error"), (warnings, "warning")] {
                if n > 0 {
                    line.push_str(", ");
                    line.push_str(&count(n, what));
                }
            }
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use viso_dsl::TextRange;
    use viso_dsl::diag::{Applicability, Fix};

    use super::*;

    fn range(text: &str, needle: &str) -> TextRange {
        let start = text.find(needle).unwrap();
        TextRange::new(
            TextSize::new(start as u32),
            TextSize::new((start + needle.len()) as u32),
        )
    }

    fn render(source: &Source<'_>, diagnostic: &Diagnostic) -> String {
        let report = Report {
            severity: "error",
            code: diagnostic.code,
            message: &diagnostic.message,
            location: Location::Span(source, diagnostic.primary.as_usize()),
            related: diagnostic
                .related
                .iter()
                .map(|(range, label)| (range.as_usize(), label.as_str()))
                .collect(),
            notes: &diagnostic.notes,
            helps: diagnostic.fixes.iter().map(|f| f.title.as_str()).collect(),
        };
        report.render()
    }

    #[test]
    fn a_diagnostic_underlines_its_span_by_characters() {
        let text = "const a = \"日本\";\n\tstate p: Pointt;\n";
        let root = Path::new("/p");
        let source = Source::new(Path::new("/p/src/app.vs"), root, text);
        let mut error = Diagnostic::error("E2001", range(text, "Pointt"), "unresolved `Pointt`");
        error
            .related
            .push((range(text, "\"日本\""), "declared here".to_string()));
        error.notes.push("names are case-sensitive".to_string());
        error.fixes.push(Fix {
            title: "replace with `Point`".to_string(),
            applicability: Applicability::MachineApplicable,
            edits: Vec::new(),
        });
        assert_eq!(
            render(&source, &error),
            "error[E2001]: unresolved `Pointt`\n \
             --> src/app.vs:2:11\n  \
             |\n\
             2 | \tstate p: Pointt;\n  \
             | \t         ^^^^^^\n\
             1 | const a = \"日本\";\n  \
             |           ---- declared here\n  \
             = note: names are case-sensitive\n  \
             = help: replace with `Point`\n"
        );
    }

    #[test]
    fn a_span_past_its_line_stops_at_the_line_end_and_an_empty_one_is_one_mark() {
        let text = "ab\ncd";
        let source = Source::new(Path::new("x.vs"), Path::new(""), text);
        let wide = Diagnostic::error(
            "E1",
            TextRange::new(TextSize::new(1), TextSize::new(5)),
            "m",
        );
        assert!(render(&source, &wide).contains("1 | ab\n  |  ^\n"));
        let empty = Diagnostic::error("E1", TextRange::empty(TextSize::new(5)), "m");
        assert!(render(&source, &empty).contains("2 | cd\n  |   ^\n"));
    }

    #[test]
    fn the_summary_counts_what_was_reported() {
        assert_eq!(
            summary("app", 1, 0, 0),
            "checked `app`: 1 file, no problems"
        );
        assert_eq!(
            summary("app", 3, 2, 1),
            "checked `app`: 3 files, 2 errors, 1 warning"
        );
        assert_eq!(
            summary("app", 0, 0, 2),
            "checked `app`: 0 files, 2 warnings"
        );
    }
}
