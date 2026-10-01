//! Command handlers. Each resolves what it needs, calls the owning service and
//! returns the process exit code.

mod check;
mod run;
mod schema;

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
/// The build failed.
pub const BUILD: u8 = 4;
/// The app crashed or exited with a failure.
pub const RUNTIME: u8 = 5;

/// The code of a command line that did not parse.
pub const CLI_USAGE: &str = "CLI_USAGE";
/// The current directory could not be read.
pub const ENV_CURRENT_DIR: &str = "ENV_CURRENT_DIR";
/// A source file the command needs could not be read.
pub const ENV_SOURCE_UNREADABLE: &str = "ENV_SOURCE_UNREADABLE";
/// The project root has no `Cargo.toml` to build.
pub const ENV_CARGO_MANIFEST: &str = "ENV_CARGO_MANIFEST";
/// Cargo could not be run.
pub const ENV_CARGO: &str = "ENV_CARGO";
/// The build produced no executable, or more than one.
pub const ENV_NO_EXECUTABLE: &str = "ENV_NO_EXECUTABLE";
/// The dev channel could not be opened on the loopback interface.
pub const ENV_DEV_CHANNEL: &str = "ENV_DEV_CHANNEL";
/// `cargo build` failed.
pub const BUILD_FAILED: &str = "BUILD_FAILED";
/// The app could not be launched, crashed, or exited with a failure.
pub const RUN_APP_FAILED: &str = "RUN_APP_FAILED";

/// Runs the parsed command, ends its output with the summary, and returns its exit
/// code.
pub fn run(cli: &Cli) -> u8 {
    let mut out = Output::new(&cli.global, Some(cli.command.name()));
    let code = match &cli.command {
        Command::Check => check::run(&cli.global, &mut out),
        Command::Run(args) => run::run(&cli.global, args, &mut out),
        Command::Schema(args) => schema::run(args, &mut out),
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
