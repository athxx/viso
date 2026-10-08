//! Human-readable output (`Viso_CLI.md` section 37): diagnostics to stderr, the
//! summary to stdout.
//!
//! A diagnostic prints its header, its location and the source line under a caret
//! underline, then its related spans, notes and fix titles. A related span in
//! another file follows a `:::` line naming it:
//!
//! ```text
//! error[E2001]: imported name is not exported: `lib::Point`
//!  --> src/app.vs:1:15
//!   |
//! 1 | import lib::{ Point };
//!   |               ^^^^^
//!   |
//!  ::: src/lib.vs:1:8
//!   |
//! 1 | record Point { x: I64; }
//!   |        ----- declared here without `export`
//!   = help: export `Point` from `lib`
//! ```

use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::ops::Range;

use crate::dev::report::{Outcome, ReloadEvent};

use super::{Location, Report, Source};

/// Writes `report` to stderr, followed by a blank line.
pub(super) fn print(report: &Report<'_>) {
    let mut text = render(report);
    text.push('\n');
    let _ = std::io::stderr().lock().write_all(text.as_bytes());
}

/// The line a hot reload attempt of `file` prints.
pub(super) fn dev(file: &str, event: &ReloadEvent) -> String {
    let ms = event.elapsed_us as f64 / 1000.0;
    let revision = event.candidate_revision;
    match event.outcome {
        Outcome::Rejected => format!(
            "hot reload: {file} revision {revision} rejected at {}; keeping revision {} ({ms:.1} ms)",
            event.stage.as_str(),
            event.last_good_revision
        ),
        outcome => format!(
            "hot reload: {file} revision {revision} {} to {} mount(s) in {ms:.1} ms",
            if outcome == Outcome::ScopedReset {
                "applied with a scoped reset"
            } else {
                "applied"
            },
            event.counts.mounts
        ),
    }
}

fn render(report: &Report<'_>) -> String {
    let mut out = String::new();
    // Writing to a `String` cannot fail.
    let _ = write(&mut out, report);
    out
}

fn write(out: &mut String, report: &Report<'_>) -> fmt::Result {
    writeln!(
        out,
        "{}[{}]: {}",
        report.severity, report.code, report.message
    )?;
    let mut gutter = 0;
    match &report.location {
        Location::Span(source, range) => {
            let last = std::iter::once(source.position(range.start).0)
                .chain(
                    report
                        .related
                        .iter()
                        .map(|r| r.source.position(r.range.start).0),
                )
                .max()
                .unwrap_or(1);
            gutter = last.to_string().len();
            let (line, column) = source.position(range.start);
            writeln!(out, "{:gutter$}--> {}:{line}:{column}", "", source.name)?;
            writeln!(out, "{:gutter$} |", "")?;
            snippet(out, source, range, '^', "", gutter)?;
            let mut shown: &Source<'_> = source;
            for related in &report.related {
                if !std::ptr::eq(related.source, shown) {
                    shown = related.source;
                    let (line, column) = shown.position(related.range.start);
                    writeln!(out, "{:gutter$} |", "")?;
                    writeln!(out, "{:gutter$}::: {}:{line}:{column}", "", shown.name)?;
                    writeln!(out, "{:gutter$} |", "")?;
                }
                snippet(out, shown, &related.range, '-', related.label, gutter)?;
            }
        }
        Location::Point { file, line, column } => writeln!(out, " --> {file}:{line}:{column}")?,
        Location::None => {}
    }
    for note in report.notes {
        writeln!(out, "{:gutter$} = note: {note}", "")?;
    }
    for fix in &report.fixes {
        writeln!(out, "{:gutter$} = help: {}", "", fix.title)?;
    }
    Ok(())
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
    let start = source.clamp(range.start);
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

/// The one-line summary of a check.
pub(super) fn summary(package: &str, files: usize, errors: usize, warnings: usize) -> String {
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
    use std::path::Path;

    use viso_dsl::diag::{Applicability, Fix, Related};
    use viso_dsl::{Diagnostic, TextRange, TextSize};

    use super::*;
    use crate::output::compiler_report;

    fn range(text: &str, needle: &str) -> TextRange {
        let start = text.find(needle).unwrap();
        TextRange::new(
            TextSize::new(start as u32),
            TextSize::new((start + needle.len()) as u32),
        )
    }

    fn render_in(source: &Source<'_>, diagnostic: &Diagnostic) -> String {
        render(&compiler_report(Some(source), &[], diagnostic))
    }

    #[test]
    fn a_diagnostic_underlines_its_span_by_characters() {
        let text = "const a = \"日本\";\n\tstate p: Pointt;\n";
        let root = Path::new("/p");
        let source = Source::new(Path::new("/p/src/app.vs"), root, text);
        let mut error = Diagnostic::error("E2001", range(text, "Pointt"), "unresolved `Pointt`");
        error
            .related
            .push(Related::new(range(text, "\"日本\""), "declared here"));
        error.notes.push("names are case-sensitive".to_string());
        error.fixes.push(Fix {
            title: "replace with `Point`".to_string(),
            applicability: Applicability::MachineApplicable,
            edits: Vec::new(),
        });
        assert_eq!(
            render_in(&source, &error),
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
    fn a_related_span_in_another_file_follows_a_line_naming_it() {
        let app = "import lib::{ Point };\n";
        let lib = "record Point { x: I64; }\n";
        let root = Path::new("/p");
        let package = [
            Source::new(Path::new("/p/src/app.vs"), root, app).of_module("app".to_string()),
            Source::new(Path::new("/p/src/lib.vs"), root, lib).of_module("lib".to_string()),
        ];
        let mut error = Diagnostic::error("E2001", range(app, "Point"), "not exported");
        error.related.push(Related::in_module(
            "lib",
            range(lib, "Point"),
            "declared here",
        ));
        error
            .related
            .push(Related::in_module("gone", range(lib, "x"), "dropped"));
        assert_eq!(
            render(&compiler_report(Some(&package[0]), &package, &error)),
            "error[E2001]: not exported\n \
             --> src/app.vs:1:15\n  \
             |\n\
             1 | import lib::{ Point };\n  \
             |               ^^^^^\n  \
             |\n \
             ::: src/lib.vs:1:8\n  \
             |\n\
             1 | record Point { x: I64; }\n  \
             |        ----- declared here\n"
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
        assert!(render_in(&source, &wide).contains("1 | ab\n  |  ^\n"));
        let empty = Diagnostic::error("E1", TextRange::empty(TextSize::new(5)), "m");
        assert!(render_in(&source, &empty).contains("2 | cd\n  |   ^\n"));
    }

    #[test]
    fn a_diagnostic_without_a_location_is_its_header_and_notes() {
        let notes = ["try `--help`".to_string()];
        let text = render(&Report {
            severity: "error",
            code: "CLI_USAGE",
            message: "unexpected argument",
            location: Location::None,
            related: Vec::new(),
            expected: &[],
            actual: None,
            notes: &notes,
            fixes: Vec::new(),
        });
        assert_eq!(
            text,
            "error[CLI_USAGE]: unexpected argument\n = note: try `--help`\n"
        );
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
