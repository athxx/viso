//! Configuration diagnostics — stable codes, spans, and actionable notes.
//!
//! A configuration problem is a user error, not an internal invariant failure, so
//! it never panics and never degrades to a generic message (AGENTS 30). Every
//! diagnostic carries a stable code the CLI can print, machine-emit, and explain
//! (`viso explain <code>`, `Viso_CLI.md` section 19), a primary span into the
//! manifest where one exists, and notes that say what to do.
//!
//! The codes are a published contract: once a code names a condition it keeps
//! naming that condition. Adding a condition adds a code; it never re-points an
//! existing one. [`tests`] pins the full list so a rename cannot pass review.
//!
//! This is deliberately *not* `viso_dsl::Diagnostic`. A resolved configuration is
//! plain data with no compiler in reach (see the crate manifest), and the CLI's
//! output layer is the one place that needs a single rendering of both kinds.

use std::fmt;
use std::path::{Path, PathBuf};

/// A 1-based line/column position in a manifest, derived from a byte range.
///
/// Columns count `char`s, not bytes, so a diagnostic under a non-ASCII value
/// points where the user's editor puts the caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// 1-based line.
    pub line: u32,
    /// 1-based column, counted in characters.
    pub column: u32,
}

impl Span {
    /// Resolves a byte offset into `text` to a line/column.
    ///
    /// An offset past the end clamps to the last position rather than failing —
    /// a span is a hint for the reader, and a clamped hint beats no diagnostic.
    pub fn from_offset(text: &str, offset: usize) -> Self {
        let offset = offset.min(text.len());
        let mut line = 1u32;
        let mut column = 1u32;
        for ch in text[..offset].chars() {
            if ch == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }
        Self { line, column }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// Whether a diagnostic stops the command or only informs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The command cannot proceed with this configuration.
    Error,
    /// The configuration resolved, but something in it is questionable.
    Warning,
}

impl Severity {
    /// The lowercase wire name used in machine output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// Stable configuration diagnostic codes.
///
/// The numeric range `C0001..` belongs to project/config resolution. Source
/// diagnostics from the `.vs` frontend live in their own range, so a code never
/// means two things across the two producers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigCode {
    /// No `Viso.toml` between the starting directory and the filesystem root.
    ManifestNotFound,
    /// A `Viso.toml` was found but could not be read.
    ManifestUnreadable,
    /// `Viso.toml` is not valid TOML.
    ManifestSyntax,
    /// A key that no version of the schema defines.
    UnknownKey,
    /// A key whose value has the wrong TOML type.
    WrongType,
    /// A required key is absent.
    MissingRequired,
    /// A key of the right type whose value is not one of the accepted ones.
    InvalidValue,
    /// Hot reload requested in a release or shipping profile.
    HotReloadInRelease,
    /// A `[profile.X]` table whose name is not a known profile.
    UnknownProfile,
    /// A `VISO_*` environment variable whose value does not parse.
    EnvInvalid,
    /// A target named in configuration or on the command line that this build
    /// cannot produce.
    TargetUnavailable,
    /// A workspace member path that does not contain a manifest.
    MemberMissing,
}

impl ConfigCode {
    /// The stable printed code.
    pub const fn as_str(self) -> &'static str {
        match self {
            ConfigCode::ManifestNotFound => "C0001",
            ConfigCode::ManifestUnreadable => "C0002",
            ConfigCode::ManifestSyntax => "C0003",
            ConfigCode::UnknownKey => "C0004",
            ConfigCode::WrongType => "C0005",
            ConfigCode::MissingRequired => "C0006",
            ConfigCode::InvalidValue => "C0007",
            ConfigCode::HotReloadInRelease => "C0008",
            ConfigCode::UnknownProfile => "C0009",
            ConfigCode::EnvInvalid => "C0010",
            ConfigCode::TargetUnavailable => "C0011",
            ConfigCode::MemberMissing => "C0012",
        }
    }

    /// A one-line title, the body of `viso explain <code>`.
    pub const fn title(self) -> &'static str {
        match self {
            ConfigCode::ManifestNotFound => "no Viso.toml found",
            ConfigCode::ManifestUnreadable => "Viso.toml could not be read",
            ConfigCode::ManifestSyntax => "Viso.toml is not valid TOML",
            ConfigCode::UnknownKey => "unknown configuration key",
            ConfigCode::WrongType => "configuration value has the wrong type",
            ConfigCode::MissingRequired => "required configuration key is missing",
            ConfigCode::InvalidValue => "configuration value is not one of the accepted values",
            ConfigCode::HotReloadInRelease => "hot reload cannot be enabled in a shipping profile",
            ConfigCode::UnknownProfile => "unknown build profile",
            ConfigCode::EnvInvalid => "VISO_* environment variable could not be parsed",
            ConfigCode::TargetUnavailable => "target is not available in this build",
            ConfigCode::MemberMissing => "workspace member has no Viso.toml",
        }
    }

    /// The exit code a command uses when this diagnostic is fatal
    /// (`Viso_CLI.md` section 7).
    ///
    /// Configuration problems are source-shaped (`1`) except the two that are
    /// really about the environment (`3`): a target this build cannot produce,
    /// and a manifest the process cannot read.
    pub const fn exit_code(self) -> u8 {
        match self {
            ConfigCode::TargetUnavailable | ConfigCode::ManifestUnreadable => 3,
            _ => 1,
        }
    }

    /// Every code, in order — the list [`tests`] pins.
    pub const ALL: &'static [ConfigCode] = &[
        ConfigCode::ManifestNotFound,
        ConfigCode::ManifestUnreadable,
        ConfigCode::ManifestSyntax,
        ConfigCode::UnknownKey,
        ConfigCode::WrongType,
        ConfigCode::MissingRequired,
        ConfigCode::InvalidValue,
        ConfigCode::HotReloadInRelease,
        ConfigCode::UnknownProfile,
        ConfigCode::EnvInvalid,
        ConfigCode::TargetUnavailable,
        ConfigCode::MemberMissing,
    ];
}

impl fmt::Display for ConfigCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One configuration problem, ready to render for a human or a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    /// The stable code.
    pub code: ConfigCode,
    /// Whether the command can proceed.
    pub severity: Severity,
    /// What is wrong, in one sentence, naming the key.
    pub message: String,
    /// The file the problem is in, when there is one.
    pub path: Option<PathBuf>,
    /// Where in that file, when the producer knows.
    pub span: Option<Span>,
    /// What to do about it.
    pub notes: Vec<String>,
}

impl ConfigDiagnostic {
    /// A fatal diagnostic with no location.
    pub fn error(code: ConfigCode, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            path: None,
            span: None,
            notes: Vec::new(),
        }
    }

    /// A non-fatal diagnostic with no location.
    pub fn warning(code: ConfigCode, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Warning,
            message: message.into(),
            path: None,
            span: None,
            notes: Vec::new(),
        }
    }

    /// Attaches the file the problem is in.
    #[must_use]
    pub fn at(mut self, path: impl AsRef<Path>) -> Self {
        self.path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Attaches the position within that file.
    #[must_use]
    pub fn span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// Adds an actionable note.
    #[must_use]
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Whether this diagnostic stops the command.
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl fmt::Display for ConfigDiagnostic {
    /// The human one-line form: `error[C0004]: unknown key `packge` (Viso.toml:3:1)`.
    ///
    /// Notes are the caller's to render — the CLI indents them under the line and
    /// the JSON writer emits them as an array (`Viso_CLI.md` section 36.1).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}[{}]: {}",
            self.severity.as_str(),
            self.code,
            self.message
        )?;
        if let Some(path) = &self.path {
            write!(f, " ({}", path.display())?;
            if let Some(span) = self.span {
                write!(f, ":{span}")?;
            }
            f.write_str(")")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published code list, pinned. A rename or a re-point is a contract break,
    /// so it has to break a test rather than quietly change what `viso explain`
    /// answers.
    #[test]
    fn every_code_is_distinct_and_pinned() {
        let printed: Vec<&str> = ConfigCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            printed,
            [
                "C0001", "C0002", "C0003", "C0004", "C0005", "C0006", "C0007", "C0008", "C0009",
                "C0010", "C0011", "C0012",
            ]
        );

        // Every code has a title and no title is a placeholder.
        for code in ConfigCode::ALL {
            assert!(!code.title().is_empty(), "{code} has no title");
            assert!(code.title().len() > 8, "{code} title is a stub");
        }
    }

    /// The two environment-shaped conditions exit 3; everything else is 1
    /// (`Viso_CLI.md` section 7). A diagnostic that exited 2 would be claiming the
    /// user's *command line* was malformed, which a manifest problem never is.
    #[test]
    fn exit_codes_separate_environment_from_source() {
        for code in ConfigCode::ALL {
            let expected = match code {
                ConfigCode::TargetUnavailable | ConfigCode::ManifestUnreadable => 3,
                _ => 1,
            };
            assert_eq!(code.exit_code(), expected, "{code}");
        }
    }

    #[test]
    fn a_span_counts_lines_and_characters_not_bytes() {
        let text = "a = 1\nb = \"日本語\"\nc = 3\n";
        assert_eq!(
            Span::from_offset(text, 0),
            Span { line: 1, column: 1 },
            "start of file"
        );

        // The byte offset of `c` — three multi-byte chars sit before it, so a
        // byte-counting column would be wrong on line 2 and the line number would
        // still be right. Checking `c`'s line proves the newline scan, and the
        // in-string offset below proves the character counting.
        let c_at = text.find("c = 3").unwrap();
        assert_eq!(Span::from_offset(text, c_at), Span { line: 3, column: 1 });

        let jp = text.find("日").unwrap();
        assert_eq!(Span::from_offset(text, jp), Span { line: 2, column: 6 });
        assert_eq!(
            Span::from_offset(text, jp + "日本".len()),
            Span { line: 2, column: 8 },
            "two characters in, not six bytes in"
        );
    }

    #[test]
    fn an_offset_past_the_end_clamps_instead_of_panicking() {
        let text = "x = 1\n";
        assert_eq!(
            Span::from_offset(text, 9999),
            Span { line: 2, column: 1 },
            "clamped to the position just past the trailing newline"
        );
    }

    #[test]
    fn the_human_form_names_the_code_the_file_and_the_position() {
        let d = ConfigDiagnostic::error(ConfigCode::UnknownKey, "unknown key `packge`")
            .at("Viso.toml")
            .span(Span { line: 3, column: 1 })
            .note("did you mean `package`?");
        assert_eq!(
            d.to_string(),
            "error[C0004]: unknown key `packge` (Viso.toml:3:1)"
        );
        assert_eq!(d.notes, ["did you mean `package`?"]);
        assert!(d.is_error());

        // Without a location the form degrades cleanly rather than printing an
        // empty parenthesis.
        let d = ConfigDiagnostic::warning(ConfigCode::UnknownProfile, "unknown profile `fast`");
        assert_eq!(d.to_string(), "warning[C0009]: unknown profile `fast`");
        assert!(!d.is_error());
    }
}
