//! The `viso` command-line tool.
//!
//! This crate parses arguments, resolves the project, hands the work to the service
//! that owns it (`viso-project` for the project model, `viso-dsl` for the compiler),
//! renders the result and picks the exit code. It never reimplements a service.

mod args;
mod atomic;
mod command;
mod complete;
mod context;
mod deploy;
mod dev;
mod device;
mod error;
mod help;
mod interrupt;
mod output;

use std::process::ExitCode;

use clap::error::ErrorKind;

fn main() -> ExitCode {
    // A shell asking for completions is answered, and the run ends, here.
    complete::answer();
    // A usage error exits 2 and `--help`/`--version` exit 0 before any project or
    // compiler work.
    let cli = match args::parse() {
        Ok(cli) => cli,
        Err(error) => return ExitCode::from(usage(error)),
    };
    ExitCode::from(command::run(&cli))
}

/// Answers a command line that did not parse — a help or version request, or
/// a usage error — and returns its exit code. A usage error is a `CLI_USAGE`
/// diagnostic like any other, with the command's `--help` to run next.
fn usage(error: clap::Error) -> u8 {
    let argv: Vec<_> = std::env::args_os().skip(1).collect();
    let json = args::wants_json(&argv);
    let named = args::named_command(&argv);
    match error.kind() {
        ErrorKind::DisplayHelp => error.exit(),
        ErrorKind::DisplayVersion => return command::version(json),
        _ => {}
    }
    let spelled = match &named {
        Some(command) => format!("viso {command}"),
        None => "viso".to_owned(),
    };
    let help = vec![format!("{spelled} --help")];
    if let Some(command) = named.as_deref().filter(|_| target_flag(&error))
        && args::takes_positional_target(command)
    {
        // `--target` belongs to the commands whose argument is something else;
        // these name their target as the argument itself.
        return command::usage_failure(
            json,
            Some(command),
            &format!("`viso {command}` names its target as an argument, not with `--target`"),
            &[format!(
                "write `viso {command} <target>`, as in `viso {command} web-gpu`"
            )],
            &help,
        );
    }
    if error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        if !json {
            // The page itself says best what the command takes.
            error.exit();
        }
        return command::usage_failure(
            json,
            named.as_deref(),
            &format!("`{spelled}` needs a command"),
            &[],
            &help,
        );
    }
    let (message, notes) = clap_message(&error.render().to_string());
    command::usage_failure(json, named.as_deref(), &message, &notes, &help)
}

/// The message and notes of a rendered parser error: its first paragraph,
/// then its tips, possible values and usage; the closing pointer to `--help`
/// is the next command instead.
fn clap_message(text: &str) -> (String, Vec<String>) {
    let mut paragraphs = text
        .split("\n\n")
        .map(|p| {
            p.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|p| !p.is_empty());
    let first = paragraphs.next().unwrap_or_default();
    let head = first.first().copied().unwrap_or("invalid command line");
    let head = head.strip_prefix("error: ").unwrap_or(head);
    // "the following required arguments were not provided:" and its list.
    let message = match &first[1.min(first.len())..] {
        [] => head.to_owned(),
        rest => format!("{} {}", head, rest.join(", ")),
    };
    let mut notes: Vec<String> = Vec::new();
    for paragraph in paragraphs {
        let line = paragraph.join(" | ");
        if line.starts_with("For more information") {
            continue;
        }
        notes.push(match line.strip_prefix("Usage: ") {
            Some(usage) => format!("usage: {usage}"),
            None => line.strip_prefix("tip: ").unwrap_or(&line).to_owned(),
        });
    }
    (message, notes)
}

/// Whether `error` is about an unexpected `--target`.
fn target_flag(error: &clap::Error) -> bool {
    use clap::error::{ContextKind, ContextValue};
    error.kind() == ErrorKind::UnknownArgument
        && matches!(
            error.get(ContextKind::InvalidArg),
            Some(ContextValue::String(arg)) if arg == "--target" || arg.starts_with("--target=")
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parser_error_becomes_a_message_and_notes() {
        let text = "error: unexpected argument '--bogus' found\n\n  \
                    tip: to pass '--bogus' as a value, use '-- --bogus'\n\n\
                    Usage: viso check [OPTIONS]\n\n\
                    For more information, try '--help'.\n";
        let (message, notes) = clap_message(text);
        assert_eq!(message, "unexpected argument '--bogus' found");
        assert_eq!(
            notes,
            [
                "to pass '--bogus' as a value, use '-- --bogus'",
                "usage: viso check [OPTIONS]"
            ]
        );
    }

    #[test]
    fn every_parser_error_of_the_grammar_is_a_message() {
        for argv in [
            &["viso", "chekc"][..],
            &["viso", "check", "--bogus"],
            &["viso", "run", "macos"],
            &["viso", "config", "get"],
            &["viso", "-q", "-v", "check"],
            &["viso", "schema"],
        ] {
            let error = args::command().try_get_matches_from(argv).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{argv:?}");
            let (message, notes) = clap_message(&error.render().to_string());
            assert!(
                !message.is_empty() && !message.starts_with("error"),
                "{argv:?}"
            );
            assert!(
                notes.iter().all(|n| !n.contains("For more information")),
                "{argv:?}: {notes:?}"
            );
        }
    }
}
