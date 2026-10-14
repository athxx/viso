//! The `viso` command-line tool.
//!
//! This crate parses arguments, resolves the project, hands the work to the service
//! that owns it (`viso-project` for the project model, `viso-dsl` for the compiler),
//! renders the result and picks the exit code. It never reimplements a service.

mod args;
mod command;
mod complete;
mod context;
mod deploy;
mod dev;
mod device;
mod error;
mod interrupt;
mod output;

use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;

fn main() -> ExitCode {
    // A shell asking for completions is answered, and the run ends, here.
    complete::answer();
    // A usage error exits 2 and `--help`/`--version` exit 0 before any project or
    // compiler work.
    let cli = match args::Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return ExitCode::from(usage(error)),
    };
    ExitCode::from(command::run(&cli))
}

/// Reports a command line that did not parse and returns its exit code. Help and
/// version requests print as usual; under `--json` a usage error is a diagnostic
/// and a summary on stdout.
fn usage(error: clap::Error) -> u8 {
    let argv: Vec<_> = std::env::args_os().skip(1).collect();
    let requested = matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    );
    let json = args::wants_json(&argv);
    let named = args::named_command(&argv);
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
        );
    }
    if requested || !json {
        error.exit();
    }
    let text = error.render().to_string();
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let message = lines.next().unwrap_or("invalid command line");
    let message = message.strip_prefix("error: ").unwrap_or(message);
    let notes: Vec<String> = lines.map(str::to_string).collect();
    command::usage_failure(true, named.as_deref(), message, &notes)
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
