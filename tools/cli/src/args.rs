//! The command grammar (`Viso_CLI.md` sections 1 and 6).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Build, check and run Viso applications.
#[derive(Debug, Parser)]
#[command(name = "viso", version)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

/// Options every project command takes, before or after the subcommand
/// (section 4.1).
#[derive(Debug, Args)]
pub struct Global {
    /// Use the project at PATH (a directory or its `Viso.toml`) instead of
    /// searching upward from the current directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub project: Option<PathBuf>,
    /// Print only errors and the final summary.
    #[arg(long, short, global = true)]
    pub quiet: bool,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check the project's `.vs` sources and `Viso.toml` without building.
    Check,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_grammar_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn project_is_one_option_on_either_side_of_the_subcommand() {
        for argv in [
            ["viso", "--project", "app", "check"],
            ["viso", "check", "--project", "app"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert_eq!(cli.global.project, Some(PathBuf::from("app")), "{argv:?}");
        }
    }
}
