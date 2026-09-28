//! The JSON event stream (`Viso_CLI.md` sections 34–36): one compact JSON object
//! per stdout line, each in the common envelope, ending with exactly one
//! `summary`.
//!
//! A `diagnostic` payload is the `Viso_DSL_1.0.md` section 138 object, for
//! compiler, configuration and CLI diagnostics alike. Lines and columns are
//! 1-based; columns count UTF-16 code units, and every range is also given in
//! bytes.

use std::io::Write as _;
use std::ops::Range;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use viso_ende::JsonWriter;

use super::{Location, Report, Source};

/// The envelope's schema name.
const SCHEMA: &str = "viso.cli.event";
/// The envelope version, shared by every payload but the diagnostic.
const SCHEMA_VERSION: u64 = 1;
/// The section 138 diagnostic object's own version.
const DIAGNOSTIC_SCHEMA_VERSION: &str = "1.0";

/// One command run's event stream.
pub(super) struct Stream {
    seq: u64,
    session: String,
    command: Option<String>,
    started: Instant,
}

impl Stream {
    /// A stream for `command`, or for a command line too broken to name one.
    pub(super) fn new(command: Option<&str>) -> Self {
        Self {
            seq: 0,
            session: session_id(),
            command: command.map(str::to_string),
            started: Instant::now(),
        }
    }

    /// Writes a `diagnostic` event.
    pub(super) fn diagnostic(&mut self, report: &Report<'_>) {
        self.event("diagnostic", |w| diagnostic(w, report));
    }

    /// Writes the closing `summary` event. `checked` is the package and file count
    /// of a check that got as far as loading the package.
    pub(super) fn summary(
        &mut self,
        code: u8,
        errors: usize,
        warnings: usize,
        checked: Option<(&str, usize)>,
    ) {
        let elapsed = self.started.elapsed().as_millis() as u64;
        self.event("summary", |w| {
            w.begin_object();
            w.name("status");
            w.string(if code == 0 { "success" } else { "failure" });
            w.name("exit_code");
            w.uint(u64::from(code));
            w.name("elapsed_ms");
            w.uint(elapsed);
            w.name("warning_count");
            w.uint(warnings as u64);
            w.name("error_count");
            w.uint(errors as u64);
            w.name("artifact_count");
            w.uint(0);
            if let Some((package, files)) = checked {
                w.name("package");
                w.string(package);
                w.name("file_count");
                w.uint(files as u64);
            }
            w.end_object();
        });
    }

    /// Writes one event line: the envelope around the payload `payload` writes.
    fn event(&mut self, kind: &str, payload: impl FnOnce(&mut JsonWriter)) {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.name("schema");
        w.string(SCHEMA);
        w.name("schema_version");
        w.uint(SCHEMA_VERSION);
        w.name("seq");
        w.uint(self.seq);
        w.name("type");
        w.string(kind);
        w.name("timestamp_ms");
        w.uint(now_ms());
        w.name("session_id");
        w.string(&self.session);
        w.name("command");
        match &self.command {
            Some(command) => w.string(command),
            None => w.null(),
        }
        w.name("payload");
        payload(&mut w);
        w.end_object();
        let mut line = w.into_string();
        line.push('\n');
        self.seq += 1;
        // A consumer that closed the pipe gets no more events; the exit code still
        // says how the command went.
        let _ = std::io::stdout().lock().write_all(line.as_bytes());
    }
}

/// The section 138 diagnostic object.
fn diagnostic(w: &mut JsonWriter, report: &Report<'_>) {
    let source = report.location.source();
    w.begin_object();
    w.name("schema_version");
    w.string(DIAGNOSTIC_SCHEMA_VERSION);
    w.name("severity");
    w.string(report.severity);
    w.name("code");
    w.string(report.code);
    w.name("message");
    w.string(report.message);
    w.name("primary");
    match &report.location {
        Location::Span(source, range) => {
            w.begin_object();
            position(w, source, range);
            w.end_object();
        }
        // Without the text there is no UTF-16 column to give, and the object has
        // no partial form.
        Location::Point { .. } | Location::None => w.null(),
    }
    w.name("related");
    w.begin_array();
    if let Some(source) = source {
        for (range, label) in &report.related {
            w.begin_object();
            position(w, source, range);
            w.name("message");
            w.string(label);
            w.end_object();
        }
    }
    w.end_array();
    w.name("expected");
    w.begin_array();
    for expected in report.expected {
        w.string(expected);
    }
    w.end_array();
    w.name("actual");
    match report.actual {
        Some(actual) => w.string(actual),
        None => w.null(),
    }
    w.name("notes");
    w.begin_array();
    for note in report.notes {
        w.string(note);
    }
    w.end_array();
    w.name("fixes");
    w.begin_array();
    for fix in report.fixes {
        w.begin_object();
        w.name("title");
        w.string(&fix.title);
        w.name("applicability");
        w.string(fix.applicability.as_str());
        w.name("edits");
        w.begin_array();
        for edit in &fix.edits {
            w.begin_object();
            w.name("file");
            match source {
                Some(source) => w.string(&source.name),
                None => w.null(),
            }
            w.name("byte_start");
            w.uint(u64::from(edit.range.start().to_u32()));
            w.name("byte_end");
            w.uint(u64::from(edit.range.end().to_u32()));
            w.name("replacement");
            w.string(&edit.replacement);
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
    w.end_array();
    w.end_object();
}

/// The members of a location object: the file, the byte range, and the 1-based
/// line and UTF-16 column of each end.
fn position(w: &mut JsonWriter, source: &Source<'_>, range: &Range<usize>) {
    let start = source.clamp(range.start);
    let end = source.clamp(range.end).max(start);
    let (line, column) = source.position_utf16(start);
    let (end_line, end_column) = source.position_utf16(end);
    w.name("file");
    w.string(&source.name);
    w.name("byte_start");
    w.uint(start as u64);
    w.name("byte_end");
    w.uint(end as u64);
    w.name("line");
    w.uint(u64::from(line));
    w.name("column_utf16");
    w.uint(u64::from(column));
    w.name("end_line");
    w.uint(u64::from(end_line));
    w.name("end_column_utf16");
    w.uint(u64::from(end_column));
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// An ID no other run on this machine shares: the process ID and the start time.
fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{:x}-{nanos:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use viso_dsl::diag::{Applicability, Fix, TextEdit};
    use viso_dsl::{TextRange, TextSize};

    use super::*;

    fn range(start: usize, end: usize) -> TextRange {
        TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
    }

    fn written(report: &Report<'_>) -> String {
        let mut w = JsonWriter::new();
        diagnostic(&mut w, report);
        w.into_string()
    }

    #[test]
    fn a_diagnostic_is_the_section_138_object_with_utf16_columns() {
        // `😀` is 4 bytes, 1 char and 2 UTF-16 units, so the byte, char and UTF-16
        // columns of `Badgee` all differ.
        let text = "const s = \"😀\"; input b: Badgee;\n";
        let at = text.find("Badgee").unwrap();
        let source = Source::new(Path::new("/p/src/app.vs"), Path::new("/p"), text);
        let notes = ["names are case-sensitive".to_string()];
        let expected = ["Badge".to_string(), "Bridge".to_string()];
        let fixes = [Fix {
            title: "replace with `Badge`".to_string(),
            applicability: Applicability::MaybeIncorrect,
            edits: vec![TextEdit {
                range: range(at, at + 6),
                replacement: "Badge".to_string(),
            }],
        }];
        let report = Report {
            severity: "error",
            code: "E2001",
            message: "unresolved \"Badgee\"",
            location: Location::Span(&source, at..at + 6),
            related: vec![(0..5, "declared here")],
            expected: &expected,
            actual: Some("Badgee"),
            notes: &notes,
            fixes: &fixes,
        };
        assert_eq!(
            written(&report),
            concat!(
                r#"{"schema_version":"1.0","severity":"error","code":"E2001","#,
                r#""message":"unresolved \"Badgee\"","#,
                r#""primary":{"file":"src/app.vs","byte_start":27,"byte_end":33,"#,
                r#""line":1,"column_utf16":26,"end_line":1,"end_column_utf16":32},"#,
                r#""related":[{"file":"src/app.vs","byte_start":0,"byte_end":5,"#,
                r#""line":1,"column_utf16":1,"end_line":1,"end_column_utf16":6,"#,
                r#""message":"declared here"}],"#,
                r#""expected":["Badge","Bridge"],"actual":"Badgee","#,
                r#""notes":["names are case-sensitive"],"#,
                r#""fixes":[{"title":"replace with `Badge`","#,
                r#""applicability":"maybe-incorrect","#,
                r#""edits":[{"file":"src/app.vs","byte_start":27,"byte_end":33,"#,
                r#""replacement":"Badge"}]}]}"#,
            )
        );
    }

    #[test]
    fn a_diagnostic_without_its_text_has_a_null_primary() {
        let report = Report {
            severity: "warning",
            code: "C0004",
            message: "unknown key",
            location: Location::Point {
                file: "Viso.toml".to_string(),
                line: 3,
                column: 1,
            },
            related: Vec::new(),
            expected: &[],
            actual: None,
            notes: &[],
            fixes: &[],
        };
        assert_eq!(
            written(&report),
            r#"{"schema_version":"1.0","severity":"warning","code":"C0004","message":"unknown key","primary":null,"related":[],"expected":[],"actual":null,"notes":[],"fixes":[]}"#
        );
    }
}
