//! The command grammar (`Viso_CLI.md` sections 1 and 6).

use std::ffi::OsString;
use std::net::IpAddr;
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
    /// Manage the Android Emulator profiles `viso run android` runs on.
    #[command(subcommand)]
    Android(AndroidCommand),
    /// Check the project's `.vs` sources and `Viso.toml` without building.
    Check,
    /// Read a state of, or record a tape from, a game the running `viso run`
    /// session's app plays.
    #[command(subcommand)]
    Game(GameCommand),
    /// Manage the iOS Simulator profiles `viso run ios` runs on.
    #[command(subcommand)]
    Ios(IosCommand),
    /// Build the project as a dev artifact and run it on this machine, a
    /// simulator, an emulator or a browser, hot reloading its `.vs` views,
    /// assets and Rust code as they are saved.
    Run(RunArgs),
    /// Show the typed schema of a widget, native library, function or type.
    Schema(SchemaArgs),
    /// Build the project as a dev artifact for a browser and serve it,
    /// hot reloading it as `viso run` does, without a browser of its own.
    Serve(ServeArgs),
    /// Run the project's headless tests of a domain.
    Test(TestArgs),
}

/// What `viso android` manages.
#[derive(Debug, Subcommand)]
pub enum AndroidCommand {
    /// The emulator profiles.
    #[command(subcommand)]
    Emulator(DeviceCommand),
}

/// What `viso ios` manages.
#[derive(Debug, Subcommand)]
pub enum IosCommand {
    /// The simulator profiles.
    #[command(subcommand)]
    Simulator(DeviceCommand),
}

/// What is done with a simulator or emulator profile.
#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    /// List the profiles and whether each is booted.
    List,
    /// Create a profile and its virtual device.
    Create(CreateArgs),
    /// Boot a profile's device.
    Start(StartArgs),
    /// Shut a profile's device down.
    Stop(DeviceArgs),
    /// Delete a profile and its virtual device.
    Delete(DeviceArgs),
}

/// The form factor of a new profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum PresetArg {
    #[default]
    Phone,
    Tablet,
}

/// What `create` makes.
#[derive(Debug, Args)]
pub struct CreateArgs {
    /// The profile ID: lowercase letters, digits and `-`.
    pub id: String,
    /// The device's form factor.
    #[arg(long, value_enum, default_value_t)]
    pub preset: PresetArg,
    /// The Android API level of its system image (the newest installed when
    /// omitted); Android only.
    #[arg(long, value_name = "LEVEL")]
    pub api: Option<String>,
}

/// What `start` boots.
#[derive(Debug, Args)]
pub struct StartArgs {
    /// The profile ID.
    pub id: String,
    /// Boot the emulator without its window (Android only).
    #[arg(long)]
    pub no_window: bool,
}

/// The profile a command acts on.
#[derive(Debug, Args)]
pub struct DeviceArgs {
    /// The profile ID.
    pub id: String,
}

/// What `viso game` does with a running game (section 22.3).
#[derive(Debug, Subcommand)]
pub enum GameCommand {
    /// Print a state of the running game as JSON.
    Peek(PeekArgs),
    /// Restart the running game's World and record the next stretch of
    /// play as an input tape.
    Record(RecordArgs),
}

/// What `viso game peek` reads.
#[derive(Debug, Args)]
pub struct PeekArgs {
    /// The game, by its `.vs` file: a path from the project root, with or
    /// without the extension, or the file's stem.
    pub game: String,
    /// The state, as `System.state`.
    #[arg(value_name = "SYSTEM.STATE")]
    pub state: String,
}

/// What `viso game record` records.
#[derive(Debug, Args)]
pub struct RecordArgs {
    /// The game, by its `.vs` file: a path from the project root, with or
    /// without the extension, or the file's stem.
    pub game: String,
    /// Where to write the tape: its text form, or its binary form when the
    /// run took live patches.
    #[arg(long, short, value_name = "PATH")]
    pub output: PathBuf,
    /// How many seconds of play to record.
    #[arg(
        long,
        value_name = "S",
        default_value_t = 10.0,
        conflicts_with = "ticks"
    )]
    pub seconds: f64,
    /// How many fixed ticks to record, instead of `--seconds`.
    #[arg(long, value_name = "N")]
    pub ticks: Option<u64>,
}

/// The test domains `viso test` runs (section 22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TestDomain {
    /// Deterministic game scenarios under `tests/game/` (section 22.3).
    Game,
}

/// What `viso test` runs (section 22).
#[derive(Debug, Args)]
pub struct TestArgs {
    /// The test domain.
    pub domain: TestDomain,
    /// The scenario, `tests/game/<SCENARIO>.tape`; every scenario there when
    /// omitted.
    pub scenario: Option<String>,
    /// The frames to run, one fixed tick each; by default through the tape
    /// and the scenario's last expectation.
    #[arg(long, value_name = "N")]
    pub frames: Option<u64>,
    /// The world's random seed, over the tape's.
    #[arg(long, value_name = "SEED")]
    pub seed: Option<u64>,
    /// Replay the input tape at PATH, binary or text, instead of the
    /// scenario's.
    #[arg(long, value_name = "PATH")]
    pub tape: Option<PathBuf>,
}

/// Where `viso run` runs the app besides this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RunTarget {
    /// An iOS Simulator.
    Ios,
    /// An Android Emulator.
    Android,
    /// A browser, the Viso renderer on WebGPU.
    WebGpu,
    /// A browser, DOM output.
    WebDom,
    /// A browser, DOM with WebGPU islands.
    WebHybrid,
}

/// What `viso run` runs, and passes on.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Where to run: this machine when omitted.
    #[arg(value_enum)]
    pub target: Option<RunTarget>,
    /// The simulator or emulator profile (`ios` and `android` only).
    #[arg(long, value_name = "ID")]
    pub device: Option<String>,
    /// The browser to open (`web-*` only): `chrome`, `safari`, `firefox`, or
    /// `none` to only serve.
    #[arg(long, value_name = "NAME")]
    pub browser: Option<String>,
    /// Keep the dev session up but apply no edit to the app: each is compiled
    /// and reported, not sent.
    #[arg(long)]
    pub no_hot_reload: bool,
    /// Arguments for the app, after `--`.
    #[arg(last = true, value_name = "APP_ARGS")]
    pub app_args: Vec<OsString>,
}

/// The web targets `viso serve` serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum WebTarget {
    /// The Viso renderer on WebGPU.
    #[value(name = "web-gpu")]
    Gpu,
    /// DOM output.
    #[value(name = "web-dom")]
    Dom,
    /// DOM with WebGPU islands.
    #[value(name = "web-hybrid")]
    Hybrid,
}

/// What `viso serve` serves, and where.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// The web target: `[web] default_target` when omitted.
    #[arg(value_enum)]
    pub target: Option<WebTarget>,
    /// The address to listen at; one off the loopback interface needs
    /// `--lan`, and `0.0.0.0` with it listens at every interface.
    #[arg(long, value_name = "IP")]
    pub host: Option<IpAddr>,
    /// The port, which alone will do; `[web.serve] port` or the next free
    /// one after it when omitted.
    #[arg(long, value_name = "N")]
    pub port: Option<u16>,
    /// Open the page in the system's browser.
    #[arg(long)]
    pub open: bool,
    /// Serve the network too: other devices load the page, and connect to
    /// the session, at this machine's private address.
    #[arg(long)]
    pub lan: bool,
    /// Keep the dev session up but apply no edit to the app.
    #[arg(long)]
    pub no_hot_reload: bool,
}

/// What `viso schema` looks up (section 18.1).
#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// The symbol, by full path or a `::`-suffix of it, optionally with
    /// `.member`: `Button`, `viso::widgets::Button`, `Button.text`,
    /// `text::upper`.
    #[arg(required_unless_present = "search", conflicts_with = "search")]
    pub symbol: Option<String>,
    /// List every symbol and member whose path contains TERM, ignoring case.
    #[arg(long, value_name = "TERM")]
    pub search: Option<String>,
}

impl Command {
    /// The command path the JSON envelope reports.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Android(AndroidCommand::Emulator(command)) => match command {
                DeviceCommand::List => "android emulator list",
                DeviceCommand::Create(_) => "android emulator create",
                DeviceCommand::Start(_) => "android emulator start",
                DeviceCommand::Stop(_) => "android emulator stop",
                DeviceCommand::Delete(_) => "android emulator delete",
            },
            Command::Check => "check",
            Command::Game(GameCommand::Peek(_)) => "game peek",
            Command::Game(GameCommand::Record(_)) => "game record",
            Command::Ios(IosCommand::Simulator(command)) => match command {
                DeviceCommand::List => "ios simulator list",
                DeviceCommand::Create(_) => "ios simulator create",
                DeviceCommand::Start(_) => "ios simulator start",
                DeviceCommand::Stop(_) => "ios simulator stop",
                DeviceCommand::Delete(_) => "ios simulator delete",
            },
            Command::Run(_) => "run",
            Command::Schema(_) => "schema",
            Command::Serve(_) => "serve",
            Command::Test(_) => "test",
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
    fn serve_takes_a_web_target_and_where_to_listen() {
        let cli = Cli::try_parse_from([
            "viso",
            "serve",
            "web-gpu",
            "--host",
            "192.168.1.5",
            "--lan",
            "--port",
            "9000",
        ])
        .unwrap();
        assert!(matches!(
            &cli.command,
            Command::Serve(ServeArgs {
                target: Some(WebTarget::Gpu),
                host: Some(_),
                port: Some(9000),
                lan: true,
                open: false,
                ..
            })
        ));
        assert!(Cli::try_parse_from(["viso", "serve", "ios"]).is_err());
        assert!(Cli::try_parse_from(["viso", "serve", "--host", "lan.example"]).is_err());
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

    #[test]
    fn run_passes_what_follows_the_separator_to_the_app() {
        let cli =
            Cli::try_parse_from(["viso", "run", "--json", "--", "--open", "demo.vs"]).unwrap();
        assert!(cli.global.json);
        assert!(
            matches!(&cli.command, Command::Run(RunArgs { app_args, no_hot_reload: false, target: None, .. }) if app_args == &["--open", "demo.vs"])
        );
        let cli = Cli::try_parse_from(["viso", "run", "--no-hot-reload"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Run(RunArgs {
                no_hot_reload: true,
                ..
            })
        ));
        assert!(Cli::try_parse_from(["viso", "run", "host"]).is_err());
        assert!(Cli::try_parse_from(["viso", "run", "headless"]).is_err());
        assert!(Cli::try_parse_from(["viso", "run", "--target", "ios"]).is_err());
        let cli =
            Cli::try_parse_from(["viso", "run", "android", "--device", "pixel-local"]).unwrap();
        assert!(matches!(
            &cli.command,
            Command::Run(RunArgs { target: Some(RunTarget::Android), device: Some(d), .. }) if d == "pixel-local"
        ));
        let cli = Cli::try_parse_from(["viso", "run", "web-gpu", "--browser", "none"]).unwrap();
        assert!(matches!(
            &cli.command,
            Command::Run(RunArgs {
                target: Some(RunTarget::WebGpu),
                browser: Some(_),
                ..
            })
        ));
        assert!(!wants_json(&["run", "--", "--json"]));
    }

    #[test]
    fn device_profiles_are_managed_per_platform() {
        let cli = Cli::try_parse_from([
            "viso", "android", "emulator", "create", "pixel35", "--api", "35",
        ])
        .unwrap();
        assert_eq!(cli.command.name(), "android emulator create");
        let Command::Android(AndroidCommand::Emulator(DeviceCommand::Create(args))) = cli.command
        else {
            panic!("a create");
        };
        assert_eq!((&*args.id, args.api.as_deref()), ("pixel35", Some("35")));
        assert_eq!(args.preset, PresetArg::Phone);
        let cli = Cli::try_parse_from([
            "viso",
            "ios",
            "simulator",
            "create",
            "ipad-local",
            "--preset",
            "tablet",
        ])
        .unwrap();
        assert_eq!(cli.command.name(), "ios simulator create");
        let cli = Cli::try_parse_from(["viso", "ios", "simulator", "list", "--json"]).unwrap();
        assert_eq!(cli.command.name(), "ios simulator list");
        assert!(Cli::try_parse_from(["viso", "ios", "emulator", "list"]).is_err());
    }

    #[test]
    fn schema_takes_a_symbol_or_a_search() {
        let cli = Cli::try_parse_from(["viso", "schema", "Button.text", "--json"]).unwrap();
        assert!(
            matches!(cli.command, Command::Schema(SchemaArgs { symbol: Some(s), search: None }) if s == "Button.text")
        );
        assert!(cli.global.json);
        let cli = Cli::try_parse_from(["viso", "schema", "--search", "text"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Schema(SchemaArgs {
                symbol: None,
                search: Some(_)
            })
        ));
        assert!(Cli::try_parse_from(["viso", "schema"]).is_err());
        assert!(Cli::try_parse_from(["viso", "schema", "Button", "--search", "x"]).is_err());
    }

    #[test]
    fn test_takes_a_domain_a_scenario_and_a_run_shape() {
        let cli = Cli::try_parse_from([
            "viso",
            "test",
            "game",
            "movement",
            "--frames",
            "600",
            "--seed",
            "1234",
            "--tape",
            "runs/jump.tape",
        ])
        .unwrap();
        let Command::Test(args) = cli.command else {
            panic!("a test command");
        };
        assert_eq!(args.domain, TestDomain::Game);
        assert_eq!(args.scenario.as_deref(), Some("movement"));
        assert_eq!((args.frames, args.seed), (Some(600), Some(1234)));
        assert_eq!(args.tape, Some(PathBuf::from("runs/jump.tape")));
        assert!(Cli::try_parse_from(["viso", "test", "game"]).is_ok());
        assert!(Cli::try_parse_from(["viso", "test", "web"]).is_err());
    }

    #[test]
    fn game_peeks_a_state_or_records_a_stretch_of_play() {
        let cli =
            Cli::try_parse_from(["viso", "game", "peek", "movement", "Player.score"]).unwrap();
        let Command::Game(GameCommand::Peek(args)) = &cli.command else {
            panic!("a peek");
        };
        assert_eq!((&*args.game, &*args.state), ("movement", "Player.score"));
        assert_eq!(cli.command.name(), "game peek");

        let cli =
            Cli::try_parse_from(["viso", "game", "record", "movement", "-o", "runs/jump.tape"])
                .unwrap();
        let Command::Game(GameCommand::Record(args)) = cli.command else {
            panic!("a recording");
        };
        assert_eq!(args.output, PathBuf::from("runs/jump.tape"));
        assert_eq!((args.seconds, args.ticks), (10.0, None));
        let ticks = ["viso", "game", "record", "m", "-o", "t", "--ticks", "600"];
        assert!(Cli::try_parse_from(ticks).is_ok());
        let both = [
            "viso",
            "game",
            "record",
            "m",
            "-o",
            "t",
            "--ticks",
            "6",
            "--seconds",
            "1",
        ];
        assert!(Cli::try_parse_from(both).is_err());
        assert!(
            Cli::try_parse_from(["viso", "game", "record", "m"]).is_err(),
            "-o is needed"
        );
    }
}
