//! `viso explain`: what a diagnostic code means, why it is raised, an example
//! that raises it and how to fix it. Compiler codes come from the compiler's
//! registry, configuration codes from the project model and the CLI's own from
//! its code table; this command only finds the code and renders it. It needs
//! no project.

use std::fmt::Write as _;

use viso_dsl::Severity;
use viso_dsl::diag::codes::{self, Example};
use viso_ende::JsonWriter;
use viso_project::ConfigCode;

use crate::args::ExplainArgs;
use crate::error::{self, EXPLAIN_UNKNOWN, SUCCESS};
use crate::output::Output;

/// Who raises a code.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Producer {
    /// The `.vs` compiler and the runtime it targets.
    Compiler,
    /// The project model, reading `Viso.toml` and the environment.
    Config,
    /// The CLI itself.
    Cli,
}

impl Producer {
    fn name(self) -> &'static str {
        match self {
            Producer::Compiler => "compiler",
            Producer::Config => "config",
            Producer::Cli => "cli",
        }
    }
}

/// One code, whoever raises it.
struct Entry {
    code: &'static str,
    producer: Producer,
    /// The severity a compiler code is raised with; a CLI or configuration
    /// code's depends on where it is raised.
    severity: Option<&'static str>,
    /// The compiler stage that raises it, for a compiler code.
    stage: Option<&'static str>,
    title: &'static str,
    explanation: &'static str,
    /// A source that raises it, and whether it is a whole `.vs` file the
    /// compiler is checked to reject with it.
    example: Option<(&'static str, bool)>,
    fix: &'static str,
    /// The example as the fix leaves it.
    fixed: Option<&'static str>,
    commands: &'static [&'static str],
    /// Specified, and not raised yet.
    reserved: bool,
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
    }
}

/// Every code: the compiler's, the configuration's, then the CLI's.
fn entries() -> impl Iterator<Item = Entry> {
    let compiler = codes::CODES.iter().map(|info| Entry {
        code: info.code,
        producer: Producer::Compiler,
        severity: Some(severity_name(info.severity)),
        stage: Some(info.stage.name()),
        title: info.title,
        explanation: info.explanation,
        example: Some(match info.example {
            Example::Source(source) => (source, true),
            Example::Text(text) => (text, false),
        }),
        fix: info.fix,
        fixed: info.fixed,
        commands: info.commands,
        reserved: info.reserved,
    });
    let config = ConfigCode::ALL.iter().map(|&code| Entry {
        code: code.as_str(),
        producer: Producer::Config,
        severity: None,
        stage: None,
        title: code.title(),
        explanation: code.explanation(),
        example: None,
        fix: code.fix(),
        fixed: None,
        commands: &["viso config validate"],
        reserved: false,
    });
    let cli = error::ALL.iter().map(|code| Entry {
        code: code.name,
        producer: Producer::Cli,
        severity: None,
        stage: None,
        title: code.summary,
        explanation: code.explanation,
        example: None,
        fix: code.fix,
        fixed: None,
        commands: code.commands,
        reserved: false,
    });
    compiler.chain(config).chain(cli)
}

pub fn run(args: &ExplainArgs, out: &mut Output) -> u8 {
    if args.list {
        let all: Vec<Entry> = entries().collect();
        out.result(|w| write_list(w, &all), || list_text(&all));
        return SUCCESS;
    }
    let asked = args.code.as_deref().unwrap_or_default().trim();
    let wanted = asked.to_ascii_uppercase();
    if let Some(entry) = entries().find(|e| e.code == wanted) {
        out.result(|w| write_entry(w, &entry), || entry_text(&entry));
        return SUCCESS;
    }
    let all: Vec<&'static str> = entries().map(|e| e.code).collect();
    let mut notes: Vec<String> = codes::nearest(&wanted, all.iter().copied())
        .into_iter()
        .map(|code| format!("did you mean `{code}`?"))
        .collect();
    let rust = wanted.len() == 5
        && wanted.starts_with("E0")
        && wanted[1..].bytes().all(|b| b.is_ascii_digit());
    let next = if rust {
        notes.insert(0, format!("`{wanted}` is a Rust compiler code"));
        vec![format!("rustc --explain {wanted}")]
    } else {
        vec!["viso explain --list".to_owned()]
    };
    out.failure_next(
        EXPLAIN_UNKNOWN,
        &format!("no diagnostic code is named `{asked}`"),
        &notes,
        &next,
    )
}

/// The `result` payload of one code.
fn write_entry(w: &mut JsonWriter, entry: &Entry) {
    w.begin_object();
    w.name("code");
    w.string(entry.code);
    w.name("producer");
    w.string(entry.producer.name());
    if let Some(severity) = entry.severity {
        w.name("severity");
        w.string(severity);
    }
    if let Some(stage) = entry.stage {
        w.name("stage");
        w.string(stage);
    }
    w.name("title");
    w.string(entry.title);
    w.name("explanation");
    w.string(entry.explanation);
    w.name("example");
    match entry.example {
        Some((example, _)) => w.string(example),
        None => w.null(),
    }
    if let Some((_, source)) = entry.example {
        w.name("example_kind");
        w.string(if source { "source" } else { "text" });
    }
    w.name("fix");
    w.string(entry.fix);
    w.name("fixed");
    match entry.fixed {
        Some(fixed) => w.string(fixed),
        None => w.null(),
    }
    w.name("suggested_commands");
    w.begin_array();
    for command in entry.commands {
        w.string(command);
    }
    w.end_array();
    if entry.reserved {
        w.name("reserved");
        w.bool(true);
    }
    w.end_object();
}

fn entry_text(entry: &Entry) -> String {
    let mut text = format!("{} {}\n", entry.code, entry.title);
    let mut traits: Vec<&str> = entry.severity.into_iter().collect();
    traits.push(entry.producer.name());
    traits.extend(entry.stage);
    if entry.reserved {
        traits.push("reserved: specified, not raised yet");
    }
    let _ = writeln!(text, "{}", traits.join(", "));
    let _ = write!(text, "\n{}\n", entry.explanation);
    if let Some((example, _)) = entry.example {
        text.push_str("\nExample:\n");
        code(&mut text, example);
    }
    let _ = write!(text, "\nFix:\n{}\n", indent(entry.fix));
    if let Some(fixed) = entry.fixed {
        text.push('\n');
        code(&mut text, fixed);
    }
    if !entry.commands.is_empty() {
        text.push_str("\nSuggested actions:\n");
        for command in entry.commands {
            let _ = writeln!(text, "  {command}");
        }
    }
    text
}

/// Appends `source`, each line indented four spaces.
fn code(text: &mut String, source: &str) {
    for line in source.trim_end().lines() {
        if line.is_empty() {
            text.push('\n');
        } else {
            let _ = writeln!(text, "    {line}");
        }
    }
}

/// `text` with every line indented two spaces.
fn indent(text: &str) -> String {
    text.trim_end()
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `--list` payload.
fn write_list(w: &mut JsonWriter, all: &[Entry]) {
    w.begin_object();
    w.name("codes");
    w.begin_array();
    for entry in all {
        w.begin_object();
        w.name("code");
        w.string(entry.code);
        w.name("producer");
        w.string(entry.producer.name());
        if let Some(severity) = entry.severity {
            w.name("severity");
            w.string(severity);
        }
        w.name("title");
        w.string(entry.title);
        if entry.reserved {
            w.name("reserved");
            w.bool(true);
        }
        w.end_object();
    }
    w.end_array();
    w.end_object();
}

fn list_text(all: &[Entry]) -> String {
    let width = all.iter().map(|e| e.code.len()).max().unwrap_or(0);
    let mut text = String::new();
    for entry in all {
        let reserved = if entry.reserved { "  (reserved)" } else { "" };
        let _ = writeln!(text, "{:<width$}  {}{reserved}", entry.code, entry.title);
    }
    text
}
