//! Command handlers. Each resolves what it needs, calls the owning service and
//! returns the process exit code.

mod check;

use crate::args::{Cli, Command, Global};
use crate::output::Output;

/// Nothing failed.
pub const SUCCESS: u8 = 0;
/// A source, check or test diagnostic failed.
pub const DIAGNOSTICS: u8 = 1;
/// The command line did not parse.
pub const USAGE: u8 = 2;
/// The environment could not provide what the command needs.
pub const ENVIRONMENT: u8 = 3;

/// The code of a command line that did not parse.
pub const CLI_USAGE: &str = "CLI_USAGE";
/// The current directory could not be read.
pub const ENV_CURRENT_DIR: &str = "ENV_CURRENT_DIR";
/// A source file the command needs could not be read.
pub const ENV_SOURCE_UNREADABLE: &str = "ENV_SOURCE_UNREADABLE";

/// Runs the parsed command, ends its output with the summary, and returns its exit
/// code.
pub fn run(cli: &Cli) -> u8 {
    let mut out = Output::new(&cli.global, Some(cli.command.name()));
    let code = match cli.command {
        Command::Check => check::run(&cli.global, &mut out),
    };
    out.finish(code)
}

/// Reports a usage error as JSON events for `command`, when the command line
/// names one, and returns its exit code.
pub fn usage_failure(command: Option<&str>, message: &str, notes: &[String]) -> u8 {
    let global = Global {
        project: None,
        quiet: false,
        json: true,
    };
    let mut out = Output::new(&global, command);
    out.failure(CLI_USAGE, message, notes);
    out.finish(USAGE)
}
