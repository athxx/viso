//! The `viso` command-line tool.
//!
//! This crate parses arguments, resolves the project, hands the work to the service
//! that owns it (`viso-project` for the project model, `viso-dsl` for the compiler),
//! renders the result and picks the exit code (`Viso_CLI.md` section 7). It never
//! reimplements a service.

mod args;
mod command;
mod output;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    // A usage error exits 2 and `--help`/`--version` exit 0 inside `parse`, before
    // any project or compiler work (section 64).
    let cli = args::Cli::parse();
    ExitCode::from(command::run(&cli))
}
