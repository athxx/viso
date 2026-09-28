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
    /// Write a stream of JSON events to stdout, one per line, instead of text.
    #[arg(long, global = true)]
    pub json: bool,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check the project's `.vs` sources and `Viso.toml` without building.
    Check,
}

impl Command {
    /// The command path the JSON envelope reports.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Check => "check",
        }
    }
}

/// Whether `--json` appears among `args` (the arguments after the program name)
/// before any `--`, for a command line too broken to parse.
pub fn wants_json<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> bool {
    args.iter()
        .map(AsRef::as_ref)
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json")
}

/// The first of `args` that names a subcommand, for a command line too broken to
/// parse.
pub fn named_command<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Option<String> {
    use clap::CommandFactory;
    let grammar = Cli::command();
    args.iter()
        .map(AsRef::as_ref)
        .take_while(|arg| *arg != "--")
        .filter_map(|arg| arg.to_str())
        .find(|arg| grammar.find_subcommand(arg).is_some())
        .map(str::to_string)
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

    #[test]
    fn json_and_the_command_are_found_in_a_broken_command_line() {
        assert!(wants_json(&["check", "--bogus", "--json"]));
        assert!(!wants_json(&["check", "--", "--json"]));
        assert_eq!(
            named_command(&["--project", "app", "check", "--bogus"]).as_deref(),
            Some("check")
        );
        assert_eq!(named_command(&["chekc"]), None);
    }
}
