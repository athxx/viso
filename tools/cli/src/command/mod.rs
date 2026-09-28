//! Command handlers. Each resolves what it needs, calls the owning service and
//! returns the process exit code.

mod check;

use crate::args::{Cli, Command};

/// Nothing failed.
pub const SUCCESS: u8 = 0;
/// A source, check or test diagnostic failed.
pub const DIAGNOSTICS: u8 = 1;
/// The environment could not provide what the command needs.
pub const ENVIRONMENT: u8 = 3;

/// Runs the parsed command and returns its exit code.
pub fn run(cli: &Cli) -> u8 {
    match cli.command {
        Command::Check => check::run(&cli.global),
    }
}
