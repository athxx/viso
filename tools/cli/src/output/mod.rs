//! How results reach the user: human text or the JSON event stream.
//!
//! Both forms render one [`Report`] model, so a diagnostic says the same thing in
//! either, and [`Output`] counts what it reports so the summary and the exit code
//! agree with it.

pub mod human;
pub mod json;

/// Writes to stdout as `print!` does, through [`write_stdout`].
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::output::write_stdout(format_args!($($arg)*))
    };
}

/// Writes a line to stdout as `println!` does, through [`write_stdout`].
macro_rules! sayln {
    () => {
        $crate::output::write_stdout(format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::output::write_stdout(format_args!("{}\n", format_args!($($arg)*)))
    };
}

pub(crate) use sayln;

/// Whether stdout's reader has gone.
static STDOUT_GONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Writes `text` to stdout. A reader that has gone — `viso … | head`, a
/// closed pipe — is no failure of the command: what it would have read is
/// dropped, and the command finishes its work and cleans up as it would.
pub(crate) fn write_stdout(text: std::fmt::Arguments<'_>) {
    use std::io::Write;
    use std::sync::atomic::Ordering;
    if STDOUT_GONE.load(Ordering::Relaxed) {
        return;
    }
    if std::io::stdout().lock().write_fmt(text).is_err() {
        STDOUT_GONE.store(true, Ordering::Relaxed);
    }
}

use std::borrow::Cow;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use viso_dsl::diag::Fix;
use viso_dsl::{Diagnostic, LineIndex, Severity, TextSize};
use viso_ende::JsonWriter;
use viso_project::ConfigDiagnostic;

use crate::args::{ColorChoice, Global};
use crate::build::cargo::RustDiagnostic;
use crate::dev::report::{RebuildEvent, ReloadEvent, RuntimeStatus};
use crate::error::Code;

/// Whom a dev session's event is about: the session, the runtime it
/// concerns and the build.
#[derive(Debug, Clone, Copy)]
pub struct DevIds<'a> {
    pub session: &'a str,
    /// The runtime's `RuntimeSessionId` in hex; empty with none connected.
    pub runtime_session: &'a str,
    /// The runtime's target; `None` with none connected.
    pub target: Option<&'a str>,
    pub build_id: &'a str,
    /// Whether human text names the target: the session serves more than
    /// one runtime.
    pub named: bool,
}

impl DevIds<'_> {
    /// What a human line starts with: the target, when it is named.
    fn prefix(&self) -> String {
        match self.target {
            Some(target) if self.named => format!("[{target}] "),
            _ => String::new(),
        }
    }
}

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

    /// The name diagnostics use for the file.
    pub fn name(&self) -> &str {
        &self.name
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

/// A suggested fix, each edit resolved to the source it changes. One with no
/// edit is a command to run next, its title `run `<command>``.
struct Suggestion<'a> {
    title: Cow<'a, str>,
    applicability: &'static str,
    edits: Vec<Edit<'a>>,
}

impl<'a> Suggestion<'a> {
    /// The suggestion to run `command` next. Running it is the user's call:
    /// it may change the machine, so it is never applied unattended.
    fn command(command: &str) -> Self {
        Self {
            title: Cow::Owned(format!("run `{command}`")),
            applicability: "maybe-incorrect",
            edits: Vec::new(),
        }
    }
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
    /// `unit`, `ui`, `game`, `web`.
    pub domain: &'a str,
    pub status: TestStatus,
    pub duration_ms: u64,
    /// Why it failed, or was skipped.
    pub message: Option<&'a str>,
}

/// How a test case ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    Pass,
    Fail,
    Skip,
}

impl TestStatus {
    /// The status as an event names it.
    pub fn as_str(self) -> &'static str {
        match self {
            TestStatus::Pass => "pass",
            TestStatus::Fail => "fail",
            TestStatus::Skip => "skip",
        }
    }

    /// The status as a line of human output ends.
    pub fn word(self) -> &'static str {
        match self {
            TestStatus::Pass => "ok",
            TestStatus::Fail => "FAILED",
            TestStatus::Skip => "skipped",
        }
    }
}

/// How many test cases passed, failed and were skipped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TestCounts {
    pub pass: u64,
    pub fail: u64,
    pub skip: u64,
}

/// One artifact a build wrote.
pub struct Artifact<'a> {
    /// `binary`, `app-bundle`, `apk`, `wasm`, `js`, `html`, `brotli`,
    /// `gzip`, `manifest`.
    pub kind: &'a str,
    /// Relative to the project root, with `/` separators.
    pub path: &'a str,
    /// The platform the host target resolves to, or the target.
    pub target: &'a str,
    /// `None` for an export, which no profile builds.
    pub profile: Option<&'a str>,
    pub build_id: &'a str,
    pub size: u64,
    /// `sha256:<hex>`.
    pub hash: &'a str,
    /// Whether it is the one an earlier build left, unchanged.
    pub cached: bool,
}

/// The output of one command run.
pub struct Output {
    form: Form,
    quiet: bool,
    verbose: bool,
    /// Whether human diagnostics are colored.
    color: bool,
    errors: usize,
    warnings: usize,
    artifacts: usize,
    /// The test cases reported, when any is.
    tests: Option<TestCounts>,
    checked: Option<(String, usize)>,
    /// The phase [`Output::advance`] reports, while one is under way.
    advancing: Option<Advancing>,
}

/// A phase reporting how far it has come.
struct Advancing {
    began: Instant,
    /// When it last said so; `None` before the first time.
    said: Option<Instant>,
    /// Whether a live line is drawn on stderr.
    drawn: bool,
}

enum Form {
    Human,
    Json(json::Stream),
}

impl Output {
    /// Output for `command` in the form and verbosity `global` asks for.
    pub fn new(global: &Global, command: Option<&str>) -> Self {
        crate::process::echo_to(match (global.verbose, global.json) {
            (false, _) => crate::process::Echo::Off,
            (true, false) => crate::process::Echo::Stderr,
            (true, true) => crate::process::Echo::Events,
        });
        Self {
            form: if global.json {
                Form::Json(json::Stream::new(command))
            } else {
                Form::Human
            },
            quiet: global.quiet,
            verbose: global.verbose,
            color: !global.json && colored(global.color),
            errors: 0,
            warnings: 0,
            artifacts: 0,
            tests: None,
            checked: None,
            advancing: None,
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
                fixes: if diagnostic.is_error() && diagnostic.notes.is_empty() {
                    explained(diagnostic.code.as_str()).into_iter().collect()
                } else {
                    Vec::new()
                },
            },
        );
    }

    /// Reports a compiler diagnostic of the Rust build, with the lines its
    /// spans point at when their files can be read; names are relative to
    /// `root`.
    pub fn rust(&mut self, root: &Path, diagnostic: &RustDiagnostic) {
        let mut texts: Vec<(&PathBuf, String)> = Vec::new();
        for span in &diagnostic.spans {
            if !texts.iter().any(|(path, _)| *path == &span.file)
                && let Ok(text) = std::fs::read_to_string(&span.file)
            {
                texts.push((&span.file, text));
            }
        }
        let sources: Vec<Source<'_>> = texts
            .iter()
            .map(|(path, text)| Source::new(path, root, text))
            .collect();
        let find = |file: &Path| sources.iter().find(|source| source.path == file);
        let location = match diagnostic.spans.first() {
            Some(span) => match find(&span.file) {
                Some(source) => Location::Span(source, span.byte_start..span.byte_end),
                None => Location::Point {
                    file: Source::new(&span.file, root, "").name,
                    line: span.line,
                    column: span.column,
                },
            },
            None => Location::None,
        };
        let related = diagnostic
            .spans
            .iter()
            .skip(1)
            .filter_map(|span| {
                Some(Secondary {
                    source: find(&span.file)?,
                    range: span.byte_start..span.byte_end,
                    label: span.label.as_deref()?,
                })
            })
            .collect();
        // The primary span's label says what is wrong there.
        let notes: Vec<String> = diagnostic
            .spans
            .first()
            .and_then(|span| span.label.clone())
            .into_iter()
            .chain(diagnostic.notes.iter().cloned())
            .collect();
        self.emit(
            diagnostic.severity == "error",
            &Report {
                severity: diagnostic.severity,
                code: &diagnostic.code,
                message: &diagnostic.message,
                location,
                related,
                expected: &[],
                actual: None,
                notes: &notes,
                fixes: Vec::new(),
            },
        );
    }

    /// Reports an artifact a build wrote: an `artifact` event, or the human
    /// row `text` builds on stdout.
    pub fn artifact(&mut self, artifact: &Artifact<'_>, text: impl FnOnce() -> String) {
        self.artifacts += 1;
        match &mut self.form {
            Form::Json(stream) => stream.artifact(artifact),
            Form::Human => say!("{}", text()),
        }
    }

    /// Reports an error the CLI itself raised, under its own `code`, and
    /// returns the exit code the failure ends the command with.
    pub fn failure(&mut self, code: Code, message: &str, notes: &[String]) -> u8 {
        self.failure_next(code, message, notes, &[])
    }

    /// As [`failure`](Self::failure), with the commands to run next: `= help:`
    /// lines in human text, fixes without edits in the event stream.
    pub fn failure_next(
        &mut self,
        code: Code,
        message: &str,
        notes: &[String],
        next: &[String],
    ) -> u8 {
        self.report_error(code.name, message, notes, next);
        code.exit
    }

    /// Reports an error under a code a service owns, such as a runtime fault
    /// of the game a test runs; the caller decides what it ends.
    pub fn error(&mut self, code: &str, message: &str, notes: &[String]) {
        self.report_error(code, message, notes, &[]);
    }

    /// Reports a warning under the CLI's own `code`: something went short of
    /// what was asked, and the command still succeeds.
    pub fn warning(&mut self, code: Code, message: &str, notes: &[String]) {
        self.report(false, code.name, message, notes, &[]);
    }

    fn report_error(&mut self, code: &str, message: &str, notes: &[String], next: &[String]) {
        self.report(true, code, message, notes, next);
    }

    fn report(
        &mut self,
        error: bool,
        code: &str,
        message: &str,
        notes: &[String],
        next: &[String],
    ) {
        self.emit(
            error,
            &Report {
                severity: if error { "error" } else { "warning" },
                code,
                message,
                location: Location::None,
                related: Vec::new(),
                expected: &[],
                actual: None,
                notes,
                fixes: next
                    .iter()
                    .map(|command| Suggestion::command(command))
                    .collect(),
            },
        );
    }

    /// Reports what `--verbose` asks to see: a line on stderr, or a `log`
    /// event at `debug`. Nothing without `--verbose`.
    pub fn verbose(&mut self, message: impl FnOnce() -> String) {
        if !self.verbose {
            return;
        }
        match &mut self.form {
            Form::Json(stream) => stream.log("debug", "tool", &message()),
            Form::Human => eprintln!("[viso] {}", message()),
        }
    }

    /// Reports the command's result: a `result` event whose payload `payload`
    /// writes, or the human text `text` builds. A result is not a diagnostic, so
    /// `--quiet` keeps it.
    pub fn result(&mut self, payload: impl FnOnce(&mut JsonWriter), text: impl FnOnce() -> String) {
        match &mut self.form {
            Form::Json(stream) => stream.result(payload),
            Form::Human => say!("{}", text()),
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

    /// Reports a step of a long command: a `progress` event, or a line on
    /// stderr. `--quiet` drops it.
    pub fn step(&mut self, phase: &str, message: &str) {
        if self.quiet {
            return;
        }
        match &mut self.form {
            Form::Json(stream) => stream.progress(phase, message),
            Form::Human => eprintln!("[viso] {message}"),
        }
    }

    /// Reports how far the phase `phase` has come: redrawn in place on one
    /// stderr line at a terminal, with the time since it began; a `progress`
    /// event at most once a second under `--json`; a plain line at most every
    /// ten seconds otherwise; nothing under `--quiet`. [`Output::settle`] ends
    /// it.
    pub fn advance(&mut self, phase: &str, message: &str) {
        if self.quiet {
            return;
        }
        let live = self.live();
        let now = Instant::now();
        let advancing = self.advancing.get_or_insert(Advancing {
            began: now,
            said: None,
            drawn: false,
        });
        let every = match (&self.form, live) {
            (_, true) => Duration::from_millis(200),
            (Form::Json(_), _) => Duration::from_secs(1),
            (Form::Human, false) => Duration::from_secs(10),
        };
        if advancing
            .said
            .is_some_and(|said| now.duration_since(said) < every)
        {
            return;
        }
        advancing.said = Some(now);
        match &mut self.form {
            Form::Json(stream) => stream.progress(phase, message),
            Form::Human if live => {
                advancing.drawn = true;
                let elapsed = now.duration_since(advancing.began).as_secs();
                eprint!(
                    "\r[viso] {message} ({}:{:02})\x1b[K",
                    elapsed / 60,
                    elapsed % 60
                );
            }
            Form::Human => eprintln!("[viso] {message}"),
        }
    }

    /// Ends the phase [`Output::advance`] reports, clearing its live line.
    pub fn settle(&mut self) {
        if self.advancing.take().is_some_and(|a| a.drawn) {
            eprint!("\r\x1b[K");
        }
    }

    /// Whether a live line may be redrawn on stderr: human output to a
    /// terminal, without `--quiet`.
    pub fn live(&self) -> bool {
        use std::io::IsTerminal;
        self.human() && !self.quiet && std::io::stderr().is_terminal()
    }

    /// Reports where the virtual device `id` (`name`, as the platform
    /// calls it) of `platform` is: a `device` event, or a line on stderr.
    pub fn device(&mut self, platform: &str, id: &str, name: &str, state: &str) {
        match &mut self.form {
            Form::Json(stream) => stream.device(platform, id, name, state),
            Form::Human => {
                if !self.quiet {
                    eprintln!("[viso] {platform} {id}: {state}");
                }
            }
        }
    }

    /// Reports one check of `viso doctor`: a `doctor_check` event, or the
    /// line `text` gives on stdout.
    pub fn doctor_check(
        &mut self,
        name: &str,
        status: &str,
        code: Option<&str>,
        message: &str,
        suggestion: Option<&str>,
        text: impl FnOnce() -> String,
    ) {
        match &mut self.form {
            Form::Json(stream) => stream.doctor_check(name, status, code, message, suggestion),
            Form::Human => say!("{}", text()),
        }
    }

    /// Whether the output is for a person rather than a `--json` stream.
    pub fn human(&self) -> bool {
        matches!(self.form, Form::Human)
    }

    /// Reports the dev server's addresses, its own and the one other devices
    /// load the page at: a `server` event naming the latter when there is
    /// one, or a line on stderr.
    pub fn server(&mut self, local: std::net::SocketAddr, network: Option<std::net::SocketAddr>) {
        match &mut self.form {
            Form::Json(stream) => stream.server(network.unwrap_or(local), network.is_some()),
            Form::Human => match network {
                None => eprintln!("[viso] serving at http://{local}/"),
                Some(network) => {
                    eprintln!("[viso] serving at http://{local}/ and http://{network}/")
                }
            },
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

    /// Shows what the test case `name` wrote: each line a `log` event from
    /// `test`, or the whole under a header on stderr.
    pub fn case_output(&mut self, name: &str, text: &str) {
        let text = text.trim_end();
        if text.is_empty() {
            return;
        }
        match &mut self.form {
            Form::Json(stream) => {
                for line in text.lines() {
                    stream.log("info", "test", line);
                }
            }
            Form::Human => eprintln!("---- {name} output ----\n{text}"),
        }
    }

    /// Reports a hot reload attempt of `file` on the runtime `ids` name: a
    /// `dev` event, or one line on stderr. Its diagnostics are reported on
    /// their own.
    pub fn dev(&mut self, file: &str, ids: &DevIds<'_>, event: &ReloadEvent) {
        match &mut self.form {
            Form::Json(stream) => stream.dev(file, ids, event),
            Form::Human => eprintln!("{}{}", ids.prefix(), human::dev(file, event)),
        }
    }

    /// Reports a build of the Rust sources the app `ids` names restarted on,
    /// or did not: a `dev` event naming its files, or one line on stderr.
    pub fn rebuild(&mut self, ids: &DevIds<'_>, event: &RebuildEvent) {
        match &mut self.form {
            Form::Json(stream) => stream.rebuild(ids, event),
            Form::Human => eprintln!("{}{}", ids.prefix(), human::rebuild(event)),
        }
    }

    /// Reports the status of the runtime `ids` names: a `runtime` event,
    /// which human text leaves out — each change of it has its own line.
    pub fn runtime(&mut self, ids: &DevIds<'_>, status: &RuntimeStatus<'_>) {
        if let Form::Json(stream) = &mut self.form {
            stream.runtime(ids, status);
        }
    }

    /// Reports what a capture of the app's frames measured, as `payload`
    /// writes it: a `profile` event, or the text `text` gives on stdout, if
    /// any.
    pub fn profile(
        &mut self,
        payload: impl FnOnce(&mut JsonWriter),
        text: impl FnOnce() -> Option<String>,
    ) {
        match &mut self.form {
            Form::Json(stream) => stream.profile(payload),
            Form::Human => {
                if let Some(text) = text() {
                    say!("{text}");
                }
            }
        }
    }

    /// Reports what happened in the inspected app, as `payload` writes it:
    /// an `inspect` event, or the line `text` gives on stdout.
    pub fn inspect(
        &mut self,
        payload: impl FnOnce(&mut JsonWriter),
        text: impl FnOnce() -> String,
    ) {
        match &mut self.form {
            Form::Json(stream) => stream.inspect(payload),
            Form::Human => say!("{}", text()),
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
        let counts = self.tests.get_or_insert_with(TestCounts::default);
        match test.status {
            TestStatus::Pass => counts.pass += 1,
            TestStatus::Fail => counts.fail += 1,
            TestStatus::Skip => counts.skip += 1,
        }
        match &mut self.form {
            Form::Json(stream) => stream.test(test, extra),
            Form::Human => say!("{}", text()),
        }
    }

    /// How many test cases were reported in all.
    pub fn test_counts(&self) -> TestCounts {
        self.tests.unwrap_or_default()
    }

    /// Reports how many tests passed, failed and were skipped, and whether
    /// the run as a whole `succeeded`: a line of human text; the summary
    /// event carries the counts.
    pub fn tally(&mut self, succeeded: bool) {
        if let Form::Human = self.form {
            let TestCounts { pass, fail, skip } = self.test_counts();
            let status = if succeeded && fail == 0 {
                "ok"
            } else {
                "FAILED"
            };
            sayln!("test result: {status}. {pass} passed; {fail} failed; {skip} skipped");
        }
    }

    /// Records what a check covered, for the summary.
    pub fn checked(&mut self, package: &str, files: usize) {
        self.checked = Some((package.to_string(), files));
    }

    /// Starts a round of a command that repeats its work, `check --watch`:
    /// what it reports from now on is counted afresh, so the summary tells
    /// of its last round.
    pub fn round(&mut self) {
        self.errors = 0;
        self.warnings = 0;
        self.checked = None;
    }

    /// Ends a round with what it covered, and says the command waits for
    /// the next: a `progress{phase:"idle"}` event, which `--quiet` keeps as
    /// the end of the round, or the round's summary line and `message`.
    pub fn idle(&mut self, message: &str) {
        match &mut self.form {
            Form::Json(stream) => stream.progress("idle", message),
            Form::Human => {
                if let Some((package, files)) = &self.checked {
                    sayln!(
                        "{}",
                        human::summary(package, *files, self.errors, self.warnings)
                    );
                }
                if !self.quiet {
                    eprintln!("{message}");
                }
            }
        }
    }

    /// Ends the output with its summary and returns `code`, the process exit
    /// code the summary reports.
    pub fn finish(self, code: u8) -> u8 {
        let checked = self.checked.as_ref().map(|(p, n)| (p.as_str(), *n));
        match self.form {
            Form::Human => {
                if let Some((package, files)) = checked {
                    sayln!(
                        "{}",
                        human::summary(package, files, self.errors, self.warnings)
                    );
                }
            }
            Form::Json(mut stream) => {
                stream.summary(
                    code,
                    self.errors,
                    self.warnings,
                    self.artifacts,
                    self.tests,
                    checked,
                );
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
            // and log events there.
            Form::Json(stream) => stream.diagnostic(report),
            Form::Human if !error && self.quiet => {}
            Form::Human => human::print(report, self.color),
        }
    }
}

/// The suggestion to read `code`'s explanation, for an error that says no
/// fix of its own; `None` for a code `viso explain` does not know.
fn explained(code: &str) -> Option<Suggestion<'static>> {
    let known = viso_dsl::diag::codes::lookup(code).is_some()
        || viso_project::ConfigCode::ALL
            .iter()
            .any(|known| known.as_str() == code);
    known.then(|| Suggestion::command(&format!("viso explain {code}")))
}

/// Whether human output to stderr is colored under `choice`.
fn colored(choice: ColorChoice) -> bool {
    use std::io::IsTerminal;
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
                && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
                && std::io::stderr().is_terminal()
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
    let mut fixes: Vec<Suggestion<'a>> = diagnostic
        .fixes
        .iter()
        .map(|fix: &'a Fix| Suggestion {
            title: Cow::Borrowed(&fix.title),
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
    if diagnostic.severity == Severity::Error && fixes.is_empty() {
        fixes.extend(explained(diagnostic.code));
    }
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
