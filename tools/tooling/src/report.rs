//! How the services report what they do: each front end — the `viso`
//! command, Studio — implements [`Reporter`] and renders what it is told its
//! own way, as human text, an event stream or a panel.

use std::path::Path;

use viso_dsl::{Diagnostic, LineIndex, TextSize};
use viso_ende::JsonWriter;
use viso_project::ConfigDiagnostic;

use crate::dev::report::{RebuildEvent, ReloadEvent, RuntimeStatus};
use crate::error::Code;

/// What the services report to the front end running them.
pub trait Reporter {
    /// The errors reported so far.
    fn errors(&self) -> usize;

    /// Whether what it reports reaches a person rather than a program.
    fn human(&self) -> bool;

    /// Reports a compiler diagnostic, in `source` when it has a source
    /// position. `package` holds the package's module sources, which its
    /// related ranges and fixes may point into.
    fn source(
        &mut self,
        source: Option<&Source<'_>>,
        package: &[Source<'_>],
        diagnostic: &Diagnostic,
    );

    /// Reports a project or configuration diagnostic, with its source line
    /// when `manifest` is the file it points into.
    fn config(&mut self, manifest: Option<&Source<'_>>, diagnostic: &ConfigDiagnostic);

    /// Reports a failure under `code` with the commands to run next, and
    /// returns the exit it ends the command with.
    fn failure_next(&mut self, code: Code, message: &str, notes: &[String], next: &[String]) -> u8;

    /// Reports a failure under `code`, and returns the exit it ends the
    /// command with.
    fn failure(&mut self, code: Code, message: &str, notes: &[String]) -> u8 {
        self.failure_next(code, message, notes, &[])
    }

    /// Reports an error under a code a service owns, such as a runtime fault;
    /// the caller decides what it ends.
    fn error(&mut self, code: &str, message: &str, notes: &[String]);

    /// Reports a line from `source` (`app`, `device` or `tool`) at `level`.
    fn log(&mut self, level: &str, source: &str, message: &str);

    /// Reports where the virtual device `id` (`name`, as the platform calls
    /// it) of `platform` is.
    fn device(&mut self, platform: &str, id: &str, name: &str, state: &str);

    /// Reports a hot reload attempt of `file` on the runtime `ids` names. Its
    /// diagnostics are reported on their own.
    fn dev(&mut self, file: &str, ids: &DevIds<'_>, event: &ReloadEvent);

    /// Reports a build of the Rust sources the app `ids` names restarted on,
    /// or did not.
    fn rebuild(&mut self, ids: &DevIds<'_>, event: &RebuildEvent);

    /// Reports the status of the runtime `ids` names.
    fn runtime(&mut self, ids: &DevIds<'_>, status: &RuntimeStatus<'_>);

    /// Reports what a capture of the app's frames measured, as `payload`
    /// writes it, or the text `text` gives, if any.
    fn profile(
        &mut self,
        payload: &mut dyn FnMut(&mut JsonWriter),
        text: &mut dyn FnMut() -> Option<String>,
    );
}

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
    pub fn prefix(&self) -> String {
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

    /// Where the file is.
    pub fn path(&self) -> &'a Path {
        self.path
    }

    /// The file's text.
    pub fn text(&self) -> &'a str {
        self.text
    }

    /// The `::`-joined path of the module the file holds, for a package
    /// source.
    pub fn module(&self) -> Option<&str> {
        self.module.as_deref()
    }

    /// An offset clamped into the text.
    pub fn clamp(&self, offset: usize) -> usize {
        offset.min(self.text.len())
    }

    /// The 1-based line and character column of a byte offset.
    pub fn position(&self, offset: usize) -> (u32, u32) {
        let at = self
            .lines
            .line_col_scalar(TextSize::new(self.clamp(offset) as u32));
        (at.line + 1, at.column + 1)
    }

    /// The 1-based line and UTF-16 column of a byte offset.
    pub fn position_utf16(&self, offset: usize) -> (u32, u32) {
        let at = self
            .lines
            .line_col_utf16(TextSize::new(self.clamp(offset) as u32));
        (at.line + 1, at.column + 1)
    }

    /// The line holding `offset`, without its terminator, and its start offset.
    pub fn line_at(&self, offset: usize) -> (&'a str, usize) {
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

/// A [`Reporter`] that keeps nothing but the count of errors, for the
/// services' own tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct Counted {
    pub errors: usize,
}

#[cfg(test)]
impl Reporter for Counted {
    fn errors(&self) -> usize {
        self.errors
    }

    fn human(&self) -> bool {
        false
    }

    fn source(&mut self, _: Option<&Source<'_>>, _: &[Source<'_>], diagnostic: &Diagnostic) {
        if diagnostic.severity == viso_dsl::Severity::Error {
            self.errors += 1;
        }
    }

    fn config(&mut self, _: Option<&Source<'_>>, diagnostic: &ConfigDiagnostic) {
        if diagnostic.is_error() {
            self.errors += 1;
        }
    }

    fn failure_next(&mut self, code: Code, _: &str, _: &[String], _: &[String]) -> u8 {
        self.errors += 1;
        code.exit
    }

    fn error(&mut self, _: &str, _: &str, _: &[String]) {
        self.errors += 1;
    }

    fn log(&mut self, _: &str, _: &str, _: &str) {}

    fn device(&mut self, _: &str, _: &str, _: &str, _: &str) {}

    fn dev(&mut self, _: &str, _: &DevIds<'_>, _: &ReloadEvent) {}

    fn rebuild(&mut self, _: &DevIds<'_>, _: &RebuildEvent) {}

    fn runtime(&mut self, _: &DevIds<'_>, _: &RuntimeStatus<'_>) {}

    fn profile(
        &mut self,
        _: &mut dyn FnMut(&mut JsonWriter),
        _: &mut dyn FnMut() -> Option<String>,
    ) {
    }
}
