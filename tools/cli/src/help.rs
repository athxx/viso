//! The help text the grammar's doc comments do not carry: the top-level
//! command list in its groups, and each command's examples and notes.
//!
//! Every page reads in one order — purpose, usage, examples, arguments,
//! options, notes — and every command, at every level, has its examples. The
//! tables are keyed by command path and checked against the grammar, so a
//! command cannot be left out of its group or without examples, and an
//! example cannot drift from what parses.

use std::fmt::Write as _;

use clap::{Arg, ArgAction, Command};

/// The top-level groups in order, each with its commands in order.
const GROUPS: &[(&str, &[&str])] = &[
    ("PROJECT", &["config"]),
    ("MOBILE DEV ENVIRONMENT", &["android", "ios"]),
    ("DEVELOP", &["run", "serve"]),
    ("LANGUAGE", &["fmt", "check", "schema"]),
    ("TEST / DEBUG", &["test", "game"]),
    ("DELIVERY", &[]),
    ("MAINTENANCE", &["clean", "completion"]),
];

/// Each command's examples: the command line and what it does.
const EXAMPLES: &[(&str, &[(&str, &str)])] = &[
    (
        "",
        &[
            ("viso run", "build and run the app here, hot reloading it"),
            ("viso run ios", "the same on an iOS Simulator"),
            ("viso check --json", "check the sources, as JSON events"),
        ],
    ),
    (
        "android",
        &[
            ("viso android emulator list", "list the emulator profiles"),
            (
                "viso android emulator create pixel",
                "create a phone profile",
            ),
        ],
    ),
    (
        "android emulator",
        &[
            ("viso android emulator list", "list the profiles"),
            (
                "viso android emulator start pixel --no-window",
                "boot one without its window",
            ),
        ],
    ),
    (
        "android emulator list",
        &[
            (
                "viso android emulator list",
                "each profile and whether it is booted",
            ),
            ("viso android emulator list --json", "as JSON events"),
        ],
    ),
    (
        "android emulator create",
        &[
            (
                "viso android emulator create pixel",
                "a phone on the newest installed system",
            ),
            (
                "viso android emulator create tab --preset tablet --api 35",
                "a tablet on API 35",
            ),
        ],
    ),
    (
        "android emulator start",
        &[
            ("viso android emulator start pixel", "boot `pixel`"),
            (
                "viso android emulator start pixel --no-window",
                "boot it without its window",
            ),
        ],
    ),
    (
        "android emulator stop",
        &[
            ("viso android emulator stop pixel", "shut `pixel` down"),
            ("viso android emulator stop pixel --json", "as JSON events"),
        ],
    ),
    (
        "android emulator delete",
        &[
            (
                "viso android emulator delete pixel",
                "delete `pixel` and its device",
            ),
            (
                "viso android emulator delete pixel --json",
                "as JSON events",
            ),
        ],
    ),
    (
        "check",
        &[
            ("viso check", "check the project here"),
            ("viso check --project apps/demo", "check another project"),
            ("viso check --json", "as JSON events"),
        ],
    ),
    (
        "clean",
        &[
            ("viso clean", "remove the project's `target/viso/`"),
            (
                "viso clean --target-dir /tmp/out",
                "the one under another `target/`",
            ),
            ("viso clean --json", "as JSON events"),
        ],
    ),
    (
        "completion",
        &[
            ("viso completion zsh", "the zsh script"),
            ("viso completion bash", "the bash script"),
            ("viso completion fish", "the fish script"),
        ],
    ),
    (
        "fmt",
        &[
            ("viso fmt", "format the project's sources"),
            ("viso fmt src/app.vs", "one file"),
            (
                "viso fmt --check",
                "fail on an unformatted file, writing nothing",
            ),
        ],
    ),
    (
        "config",
        &[
            ("viso config show", "every key, its value and its origin"),
            ("viso config get profile.release.opt_level", "one key"),
            ("viso config validate", "every problem of the configuration"),
        ],
    ),
    (
        "config show",
        &[
            ("viso config show", "as a dev build sees it"),
            (
                "viso config show --profile release",
                "as a release build sees it",
            ),
            ("viso config show --json", "as JSON events"),
        ],
    ),
    (
        "config get",
        &[
            ("viso config get package.name", "the package name"),
            (
                "viso config get opt_level --profile shipping",
                "a shipping build's optimization level",
            ),
        ],
    ),
    (
        "config path",
        &[
            ("viso config path", "the `Viso.toml` in use here"),
            (
                "viso config path --project apps/demo",
                "the one of another project",
            ),
        ],
    ),
    (
        "config validate",
        &[
            ("viso config validate", "report every problem"),
            ("viso config validate --json", "as JSON events"),
        ],
    ),
    (
        "game",
        &[
            (
                "viso game peek movement Player.score",
                "read a state of the running game",
            ),
            (
                "viso game record movement --output runs/jump.tape",
                "record ten seconds of play",
            ),
        ],
    ),
    (
        "game peek",
        &[
            (
                "viso game peek movement Player.score",
                "a state, by the game's file stem",
            ),
            (
                "viso game peek src/levels/one Player.lives --json",
                "by its path, as JSON events",
            ),
        ],
    ),
    (
        "game record",
        &[
            (
                "viso game record movement --output runs/jump.tape",
                "ten seconds of play",
            ),
            (
                "viso game record movement --output runs/long.tape --seconds 30",
                "thirty seconds",
            ),
            (
                "viso game record movement --output runs/short.tape --ticks 600",
                "600 fixed ticks",
            ),
        ],
    ),
    (
        "ios",
        &[
            ("viso ios simulator list", "list the simulator profiles"),
            ("viso ios simulator create iphone", "create a phone profile"),
        ],
    ),
    (
        "ios simulator",
        &[
            ("viso ios simulator list", "list the profiles"),
            ("viso ios simulator start iphone", "boot one"),
        ],
    ),
    (
        "ios simulator list",
        &[
            (
                "viso ios simulator list",
                "each profile and whether it is booted",
            ),
            ("viso ios simulator list --json", "as JSON events"),
        ],
    ),
    (
        "ios simulator create",
        &[
            (
                "viso ios simulator create iphone",
                "a phone on the newest installed system",
            ),
            ("viso ios simulator create ipad --preset tablet", "a tablet"),
        ],
    ),
    (
        "ios simulator start",
        &[
            ("viso ios simulator start iphone", "boot `iphone`"),
            ("viso ios simulator start iphone --json", "as JSON events"),
        ],
    ),
    (
        "ios simulator stop",
        &[
            ("viso ios simulator stop iphone", "shut `iphone` down"),
            ("viso ios simulator stop iphone --json", "as JSON events"),
        ],
    ),
    (
        "ios simulator delete",
        &[
            (
                "viso ios simulator delete iphone",
                "delete `iphone` and its device",
            ),
            ("viso ios simulator delete iphone --json", "as JSON events"),
        ],
    ),
    (
        "run",
        &[
            ("viso run", "on this machine"),
            (
                "viso run ios --device iphone",
                "on the simulator profile `iphone`",
            ),
            (
                "viso run web-gpu --browser none",
                "serve it, opening no browser",
            ),
        ],
    ),
    (
        "schema",
        &[
            ("viso schema Button", "a widget's properties and events"),
            (
                "viso schema Button.text --json",
                "one member, as JSON events",
            ),
            (
                "viso schema --search text",
                "every symbol whose path has `text`",
            ),
        ],
    ),
    (
        "serve",
        &[
            ("viso serve", "the target `[web] default_target` names"),
            ("viso serve web-gpu --open", "and open it in the browser"),
            (
                "viso serve web-gpu --lan --port 9000",
                "to the other devices of the network",
            ),
        ],
    ),
    (
        "test",
        &[
            ("viso test game", "every scenario under `tests/game/`"),
            (
                "viso test game movement --frames 600 --seed 1234",
                "one, for 600 ticks with seed 1234",
            ),
            (
                "viso test game movement --tape runs/jump.tape",
                "one, replaying a recorded tape",
            ),
        ],
    ),
];

/// The notes that close a command's page: what holds for some targets, and
/// where to go next.
const NOTES: &[(&str, &str)] = &[
    (
        "",
        "Run `viso <command> --help` for a command's arguments and options.",
    ),
    (
        "android",
        "Profiles are kept in `~/.viso/` (`$VISO_HOME`); `viso run android --device <id>`\n\
         runs on one.",
    ),
    (
        "clean",
        "A lock a running `viso` holds stops it before anything is removed.",
    ),
    (
        "completion",
        "Install it from the shell's startup file:\n  \
         bash        source <(viso completion bash)\n  \
         zsh         source <(viso completion zsh)\n  \
         fish        viso completion fish | source\n  \
         powershell  viso completion powershell | Out-String | Invoke-Expression",
    ),
    (
        "config",
        "`Viso.toml` is the source of truth: `viso config` reads it and never writes it.\n\
         A value comes from the first of: a flag, a `VISO_*` variable, the profile's\n\
         table, the manifest, the default.",
    ),
    (
        "fmt",
        "A file that does not parse is reported and left as it is. Rust is `cargo fmt`'s.",
    ),
    (
        "game",
        "Both ask the app of this project's running `viso run` session: start one first.",
    ),
    (
        "ios",
        "Profiles are kept in `~/.viso/` (`$VISO_HOME`); `viso run ios --device <id>`\n\
         runs on one.",
    ),
    (
        "run",
        "`ios` and `android` run on a simulator or emulator profile: see\n\
         `viso ios simulator list` and `viso android emulator list`. The web targets\n\
         build for a browser and serve it as `viso serve` does. The app's own arguments\n\
         follow `--`.",
    ),
    (
        "serve",
        "`viso run web-gpu` serves the same and opens a browser too.",
    ),
    (
        "test",
        "A scenario is `tests/game/<name>.tape`; a failed expectation exits 1.",
    ),
];

/// What `viso --version` reports: the CLI's version and commit, and the
/// versions of what it speaks — the language, the event stream, the
/// diagnostic object and the dev protocol. Constants only: nothing is loaded
/// to say them.
pub struct Version {
    pub version: &'static str,
    pub commit: &'static str,
    pub language: &'static str,
    pub event_schema: u64,
    pub diagnostic_schema: &'static str,
    pub dev_protocol: u16,
}

/// This build's [`Version`].
pub const VERSION: Version = Version {
    version: env!("CARGO_PKG_VERSION"),
    commit: env!("VISO_GIT_COMMIT"),
    language: viso_dsl::frontend::LANGUAGE_VERSION,
    event_schema: crate::output::json::SCHEMA_VERSION,
    diagnostic_schema: crate::output::json::DIAGNOSTIC_SCHEMA_VERSION,
    dev_protocol: viso_view::dev::wire::DEV_PROTOCOL_VERSION,
};

impl Version {
    /// The human text.
    pub fn text(&self) -> String {
        format!(
            "viso {} ({})
language {}
event schema {}
diagnostic schema {}
dev protocol {}
",
            self.version,
            self.commit,
            self.language,
            self.event_schema,
            self.diagnostic_schema,
            self.dev_protocol
        )
    }

    /// The `result` payload.
    pub fn write(&self, w: &mut viso_ende::JsonWriter) {
        w.begin_object();
        w.name("version");
        w.string(self.version);
        w.name("commit");
        w.string(self.commit);
        w.name("language");
        w.string(self.language);
        w.name("event_schema");
        w.uint(self.event_schema);
        w.name("diagnostic_schema");
        w.string(self.diagnostic_schema);
        w.name("dev_protocol");
        w.uint(u64::from(self.dev_protocol));
        w.end_object();
    }
}

/// The page of every command but the top level.
const TEMPLATE: &str = "\
{about-with-newline}
{usage-heading} {usage}

{before-help}{all-args}{after-help}";

/// `command` with its help pages: the grouped top-level list, and every
/// command's examples and notes in the order of [`TEMPLATE`].
pub fn decorate(command: Command) -> Command {
    let root = format!(
        "{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{{before-help}}{}\
         Options:\n{{options}}{{after-help}}",
        groups(&command)
    );
    // One page per command: `--help` is `-h`, every value listed inline
    // rather than a paragraph each.
    let help = Arg::new("help")
        .short('h')
        .long("help")
        .global(true)
        .action(ArgAction::HelpShort)
        .help_heading(None::<&str>)
        .help("Print help");
    pages(command, "")
        .help_template(root)
        .disable_help_flag(true)
        .arg(help)
}

/// `command`, at `path`, and every command under it, with its examples and
/// notes.
fn pages(command: Command, path: &str) -> Command {
    let names: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    let mut command = command.help_template(TEMPLATE);
    if let Some(examples) = examples_of(path) {
        command = command.before_help(examples);
    }
    if let Some(notes) = lookup(NOTES, path) {
        command = command.after_help(format!("Notes:\n{}", indent(notes)));
    }
    for name in names {
        let sub_path = if path.is_empty() {
            name.clone()
        } else {
            format!("{path} {name}")
        };
        command = command.mut_subcommand(&name, |sub| pages(sub, &sub_path));
    }
    command
}

/// The `Examples:` section of the command at `path`, its comments aligned.
fn examples_of(path: &str) -> Option<String> {
    let examples = lookup(EXAMPLES, path)?;
    let width = examples.iter().map(|(line, _)| line.len()).max()?;
    let mut text = String::from("Examples:\n");
    for (line, what) in examples {
        let _ = writeln!(text, "  {line:width$}  # {what}");
    }
    Some(text.trim_end().to_owned())
}

/// The top-level command list, in its groups; a group with no command yet is
/// left out.
fn groups(command: &Command) -> String {
    let width = command
        .get_subcommands()
        .map(|sub| sub.get_name().len())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for (group, names) in GROUPS.iter().filter(|(_, names)| !names.is_empty()) {
        let _ = writeln!(text, "{group}");
        for name in *names {
            let about = command
                .find_subcommand(name)
                .and_then(Command::get_about)
                .map(ToString::to_string)
                .unwrap_or_default();
            let _ = writeln!(text, "  {name:width$}  {about}");
        }
        text.push('\n');
    }
    text
}

fn lookup<T: Copy>(table: &[(&str, T)], path: &str) -> Option<T> {
    table
        .iter()
        .find(|(key, _)| *key == path)
        .map(|(_, value)| *value)
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command path under `command`, `""` for the top level.
    fn paths(command: &Command, path: &str, out: &mut Vec<String>) {
        out.push(path.to_owned());
        for sub in command.get_subcommands() {
            let sub_path = if path.is_empty() {
                sub.get_name().to_owned()
            } else {
                format!("{path} {}", sub.get_name())
            };
            paths(sub, &sub_path, out);
        }
    }

    fn all_paths() -> Vec<String> {
        let mut out = Vec::new();
        paths(&crate::args::command(), "", &mut out);
        out
    }

    #[test]
    fn every_top_level_command_is_in_exactly_one_group() {
        let command = crate::args::command();
        let grouped: Vec<&str> = GROUPS
            .iter()
            .flat_map(|(_, names)| *names)
            .copied()
            .collect();
        for sub in command.get_subcommands() {
            let name = sub.get_name();
            let count = grouped.iter().filter(|n| **n == name).count();
            assert_eq!(count, 1, "`{name}` is in {count} groups");
        }
        for name in grouped {
            assert!(
                command.find_subcommand(name).is_some(),
                "the group lists `{name}`, which is not a command"
            );
        }
    }

    /// Two or three examples for every command, each one a command line of
    /// that command which parses.
    #[test]
    fn every_command_has_examples_that_parse() {
        let paths = all_paths();
        for path in &paths {
            let examples =
                lookup(EXAMPLES, path).unwrap_or_else(|| panic!("`{path}`: no examples"));
            assert!(
                (2..=3).contains(&examples.len()),
                "`{path}`: {} examples",
                examples.len()
            );
            for (line, what) in examples {
                assert!(
                    path.is_empty() || line.starts_with(&format!("viso {path}")),
                    "`{line}` is not an example of `viso {path}`"
                );
                assert!(
                    !what.is_empty() && !what.ends_with('.'),
                    "`{line}`: {what:?}"
                );
                let argv: Vec<&str> = line.split(' ').collect();
                if let Err(error) = crate::args::command().try_get_matches_from(&argv) {
                    panic!("`{line}` does not parse: {error}");
                }
            }
        }
        let keys = EXAMPLES
            .iter()
            .map(|(key, _)| key)
            .chain(NOTES.iter().map(|(key, _)| key));
        for key in keys {
            assert!(paths.iter().any(|p| p == key), "`{key}` is not a command");
        }
    }

    /// The top-level page reads purpose, usage, examples, the groups,
    /// options and notes, in that order.
    #[test]
    fn the_top_level_page_reads_in_order() {
        let help = crate::args::command().render_help().to_string();
        let at = |needle: &str| {
            help.find(needle)
                .unwrap_or_else(|| panic!("no `{needle}` in\n{help}"))
        };
        let order = [
            at("Build, check and run"),
            at("Usage: viso"),
            at("Examples:"),
            at("PROJECT\n"),
            at("MOBILE DEV ENVIRONMENT\n"),
            at("MAINTENANCE\n"),
            at("Options:"),
            at("Notes:"),
        ];
        assert!(order.is_sorted(), "{help}");
        assert!(!help.contains("DELIVERY"), "an empty group is left out");
    }

    /// A command page reads purpose, usage, examples, arguments, options and
    /// notes, the global options after its own.
    #[test]
    fn a_command_page_reads_in_order() {
        let help = crate::args::command()
            .try_get_matches_from(["viso", "run", "--help"])
            .unwrap_err()
            .render()
            .to_string();
        let at = |needle: &str| {
            help.find(needle)
                .unwrap_or_else(|| panic!("no `{needle}` in\n{help}"))
        };
        let order = [
            at("Build a dev artifact"),
            at("Usage: viso run"),
            at("Examples:"),
            at("Arguments:"),
            at("Options:"),
            at("Global options:"),
            at("Notes:"),
        ];
        assert!(order.is_sorted(), "{help}");
    }
}
