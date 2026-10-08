//! The `viso` command-line tool.
//!
//! This crate parses arguments, resolves the project, hands the work to the service
//! that owns it (`viso-project` for the project model, `viso-dsl` for the compiler),
//! renders the result and picks the exit code (`Viso_CLI.md` section 7). It never
//! reimplements a service.

mod args;
mod command;
mod dev;
mod output;

use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;

fn main() -> ExitCode {
    // A usage error exits 2 and `--help`/`--version` exit 0 before any project or
    // compiler work (section 64).
    let cli = match args::Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return ExitCode::from(usage(error)),
    };
    ExitCode::from(command::run(&cli))
}

/// Reports a command line that did not parse and returns its exit code. Help and
/// version requests print as usual; under `--json` a usage error is a diagnostic
/// and a summary on stdout (section 37).
fn usage(error: clap::Error) -> u8 {
    let argv: Vec<_> = std::env::args_os().skip(1).collect();
    let requested = matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    );
    if requested || !args::wants_json(&argv) {
        error.exit();
    }
    let text = error.render().to_string();
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let message = lines.next().unwrap_or("invalid command line");
    let message = message.strip_prefix("error: ").unwrap_or(message);
    let notes: Vec<String> = lines.map(str::to_string).collect();
    command::usage_failure(args::named_command(&argv).as_deref(), message, &notes)
}
