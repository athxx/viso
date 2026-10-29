//! The command grammar.

use std::ffi::{OsStr, OsString};
use std::marker::PhantomData;
use std::net::IpAddr;
use std::path::PathBuf;

use clap::builder::{EnumValueParser, PossibleValue, TypedValueParser};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum, ValueHint};
use clap_complete::ArgValueCandidates;

use crate::complete;

/// Build, check and run Viso applications.
#[derive(Debug, Parser)]
#[command(name = "viso", version, disable_help_subcommand = true)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

/// Options every command takes where they apply, before or after the
/// subcommand.
#[derive(Debug, Default, Args)]
#[command(next_help_heading = "Global options")]
pub struct Global {
    /// Use the project at PATH, a directory or its `Viso.toml`, not the one
    /// found upward.
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub project: Option<PathBuf>,
    /// Write JSON events to stdout, one per line, instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// Print only errors, requested paths and the summary.
    #[arg(long, short, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Also print the resolved configuration, the tools run and stage times.
    #[arg(long, short, global = true)]
    pub verbose: bool,
    /// When to color human output; `auto` is a terminal without `NO_COLOR`.
    #[arg(long, global = true, value_enum, value_name = "WHEN", default_value_t)]
    pub color: ColorChoice,
    /// Use no network: Cargo and downloads work from what is on disk.
    #[arg(long, global = true)]
    pub offline: bool,
    /// Require `Cargo.lock` to be up to date.
    #[arg(long, global = true)]
    pub locked: bool,
    /// Run at most N jobs at once.
    #[arg(long, global = true, value_name = "N", value_parser = clap::value_parser!(u16).range(1..))]
    pub jobs: Option<u16>,
    /// The build profile.
    #[arg(long, global = true, value_enum, value_name = "NAME")]
    pub profile: Option<ProfileArg>,
    /// Put build output under PATH instead of `target/`.
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub target_dir: Option<PathBuf>,
}

/// When human output is colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum ColorChoice {
    /// On a terminal, unless `NO_COLOR` is set.
    #[default]
    Auto,
    /// Always.
    Always,
    /// Never.
    Never,
}

/// The build profiles `--profile` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ProfileArg {
    /// Unoptimized, with the Dev Runtime and hot reload.
    Dev,
    /// Optimized, without the Dev Runtime.
    Release,
    /// Optimized for delivery: smallest, stripped.
    Shipping,
}

impl ProfileArg {
    /// The profile it names.
    pub fn profile(self) -> viso_project::Profile {
        match self {
            ProfileArg::Dev => viso_project::Profile::Dev,
            ProfileArg::Release => viso_project::Profile::Release,
            ProfileArg::Shipping => viso_project::Profile::Shipping,
        }
    }
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the Android SDK and the emulator profiles `viso run android` runs on.
    #[command(subcommand)]
    Android(AndroidCommand),
    /// Build the app for a target and write it to `dist/`, without running it.
    Build(BuildArgs),
    /// Check the project's `.vs` sources and `Viso.toml` without building.
    Check(CheckArgs),
    /// Remove `target/viso/`: builds, caches and dev state, never `dist/`.
    Clean,
    /// Examine this machine for developing Viso apps: toolchains, the project, a target's SDK.
    Doctor(DoctorArgs),
    /// Write the script that completes `viso` commands in a shell.
    Completion(CompletionArgs),
    /// Print one of the compiler's IRs of the project: syntax tree, HIR, view, behavior, shader, systems.
    Dump(DumpArgs),
    /// Explain a diagnostic code: why it is raised and how to fix it.
    Explain(ExplainArgs),
    /// Generate source a web project owns from the app's view: plain HTML and CSS, or a SolidJS component.
    #[command(subcommand)]
    Export(ExportCommand),
    /// Lay out `.vs` sources in the one canonical format.
    Fmt(FmtArgs),
    /// Show where each configuration value comes from, or validate it.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Read a state of, or record input from, the game `viso run` is playing.
    #[command(subcommand)]
    Game(GameCommand),
    /// Inspect the running app: its nodes and their source, what it holds for them, what it drew.
    Inspect(InspectArgs),
    /// Manage the iOS Simulator runtimes and the profiles `viso run ios` runs on.
    #[command(subcommand)]
    Ios(IosCommand),
    /// Start the `.vs` language server for an editor, speaking LSP over stdio.
    Lsp(LspArgs),
    /// Create a Viso project from a template: an app, a game, a web app or a library.
    New(NewArgs),
    /// Build the app as its platform ships it, signed: an `.app` and `.dmg`, an `.ipa`, an APK and app bundle, an archive and an MSIX.
    Package(PackageArgs),
    /// Show generated `.vs` code alone in a sandboxed headless run: only the preview grant, a native outside it faulting `E6103`.
    Preview(PreviewArgs),
    /// Sample the frames the running app draws: their time, phase by phase, and the work they count.
    Profile(ProfileArgs),
    /// Build a dev artifact and run it, hot reloading edits as they are saved.
    Run(RunArgs),
    /// Show the typed schema of a widget, component, native library, function or type.
    Schema(SchemaArgs),
    /// Build a dev artifact for a browser and serve it, hot reloading it.
    Serve(ServeArgs),
    /// Take the snapshots UI scenarios take, and hold them to their goldens.
    #[command(subcommand)]
    Snapshot(SnapshotCommand),
    /// Run the project's tests: unit, UI, game.
    Test(TestArgs),
}

/// A positional target: one of the built-in targets `T` names, or a device
/// target `Viso.toml` declares as `[target.<name>]`, by its name. Whether the
/// manifest declares it is the project's question, answered when the
/// configuration resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetArg<T> {
    /// A built-in target.
    Builtin(T),
    /// A device target's name.
    Device(String),
}

impl<T: Copy> TargetArg<T> {
    /// The target it names, a built-in one as `builtin` maps it.
    pub fn target(&self, builtin: impl FnOnce(T) -> viso_project::Target) -> viso_project::Target {
        match self {
            TargetArg::Builtin(target) => builtin(*target),
            TargetArg::Device(name) => viso_project::Target::device(name),
        }
    }
}

/// Parses a [`TargetArg`]: a value `T` names is the built-in target, any other
/// word a device target's name could be is that name. A built-in target's
/// name `T` leaves out, a platform's (`macos`), or a word no device target
/// could take, is the usage error `T`'s own parser gives, listing `T`'s
/// values.
#[derive(Clone)]
pub struct TargetParser<T>(PhantomData<T>);

impl<T> TargetParser<T> {
    pub fn new() -> Self {
        TargetParser(PhantomData)
    }
}

impl<T> Default for TargetParser<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ValueEnum + Clone + Send + Sync + 'static> TypedValueParser for TargetParser<T> {
    type Value = TargetArg<T>;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &OsStr,
    ) -> Result<TargetArg<T>, clap::Error> {
        if let Some(text) = value.to_str() {
            let builtin = T::value_variants().iter().find(|variant| {
                variant
                    .to_possible_value()
                    .is_some_and(|possible| possible.matches(text, false))
            });
            if let Some(builtin) = builtin {
                return Ok(TargetArg::Builtin(builtin.clone()));
            }
            if viso_project::device_target_name(text).is_ok() {
                return Ok(TargetArg::Device(text.to_owned()));
            }
        }
        EnumValueParser::<T>::new()
            .parse_ref(cmd, arg, value)
            .map(TargetArg::Builtin)
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        Some(Box::new(
            T::value_variants()
                .iter()
                .filter_map(ValueEnum::to_possible_value),
        ))
    }
}

/// What `viso build` builds.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// What to build for: this machine when omitted, or a device target of
    /// `Viso.toml` by its name.
    #[arg(
        value_name = "TARGET",
        value_parser = TargetParser::<BuildTarget>::new(),
        add = ArgValueCandidates::new(complete::targets::<BuildTarget>),
    )]
    pub target: Option<TargetArg<BuildTarget>>,
    /// Build the release profile: `--profile release`.
    #[arg(long)]
    pub release: bool,
}

/// The targets `viso build` builds for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum BuildTarget {
    /// No window and no device, for tests and tooling.
    Headless,
    /// The iOS Simulator.
    Ios,
    /// The Android Emulator.
    Android,
    /// A browser, the Viso renderer on WebGPU.
    WebGpu,
    /// A browser, DOM output.
    WebDom,
    /// A browser, DOM with WebGPU islands.
    WebHybrid,
}

impl BuildTarget {
    /// The target it names.
    pub fn target(self) -> viso_project::Target {
        use viso_project::Target;
        match self {
            BuildTarget::Headless => Target::Headless,
            BuildTarget::Ios => Target::Ios,
            BuildTarget::Android => Target::Android,
            BuildTarget::WebGpu => Target::WebGpu,
            BuildTarget::WebDom => Target::WebDom,
            BuildTarget::WebHybrid => Target::WebHybrid,
        }
    }
}

/// What `viso export` generates.
#[derive(Debug, Subcommand)]
pub enum ExportCommand {
    /// Semantic HTML and CSS, with vanilla JavaScript for what changes.
    Html(ExportHtmlArgs),
    /// A Vite project whose SolidJS component is the view.
    Solid(ExportSolidArgs),
}

/// Where and how `viso export html` writes.
#[derive(Debug, Args)]
pub struct ExportHtmlArgs {
    /// The component to export, `Name` or `module::Name`: the one `src/app.vs`
    /// exports when omitted.
    #[arg(value_name = "COMPONENT")]
    pub component: Option<String>,
    /// The directory to write: `export.html.out_dir`, else `dist-html`.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub out: Option<PathBuf>,
    /// Write no JavaScript: a view that needs it to change is an error.
    #[arg(long = "static")]
    pub static_only: bool,
    /// Replace the directory even where it holds files the export did not write or that were edited since.
    #[arg(long)]
    pub force: bool,
}

/// Where `viso export solid` writes.
#[derive(Debug, Args)]
pub struct ExportSolidArgs {
    /// The component to export, `Name` or `module::Name`: the one `src/app.vs`
    /// exports when omitted.
    #[arg(value_name = "COMPONENT")]
    pub component: Option<String>,
    /// The directory to write: `export.solid.out_dir`, else `web-solid`.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub out: Option<PathBuf>,
    /// Replace the directory even where it holds files the export did not write or that were edited since.
    #[arg(long)]
    pub force: bool,
}

/// What `viso package` packages.
#[derive(Debug, Args)]
pub struct PackageArgs {
    /// What to package for: this machine when omitted.
    #[arg(value_enum)]
    pub target: Option<PackageTarget>,
    /// How the package is signed: with the identity configured or found, else as far as the platform allows with a warning (`auto`); only with a real one (`required`); not at all (`off`).
    #[arg(long, value_enum, default_value_t = SigningPolicy::Auto)]
    pub signing: SigningPolicy,
    /// Print what the package would be (its metadata, signing, steps and files) and build nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// The targets `viso package` packages for, beside this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PackageTarget {
    /// An `.ipa` for iOS devices.
    Ios,
    /// An APK and an app bundle (`.aab`).
    Android,
    /// A web bundle and its zip, the Viso renderer on WebGPU.
    WebGpu,
    /// A web bundle and its zip, DOM output.
    WebDom,
    /// A web bundle and its zip, DOM with WebGPU islands.
    WebHybrid,
}

impl PackageTarget {
    /// The target it names.
    pub fn target(self) -> viso_project::Target {
        use viso_project::Target;
        match self {
            PackageTarget::Ios => Target::Ios,
            PackageTarget::Android => Target::Android,
            PackageTarget::WebGpu => Target::WebGpu,
            PackageTarget::WebDom => Target::WebDom,
            PackageTarget::WebHybrid => Target::WebHybrid,
        }
    }
}

/// How `viso package` signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SigningPolicy {
    /// With the identity configured or found; else ad hoc, with the debug keystore or unsigned, and a warning.
    Auto,
    /// Only with a real identity, keystore or certificate; without one the command fails.
    Required,
    /// Not at all.
    Off,
}

/// What `viso doctor` examines.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// A target whose toolchain is examined, beside Rust and the project; the
    /// host's tools and a word on each target's when none is named.
    #[arg(value_enum)]
    pub target: Option<BuildTarget>,
    /// Make the local, reversible fixes the checks name (a Rust target added
    /// with `rustup`), each said before it runs; nothing else is installed.
    #[arg(long)]
    pub fix: bool,
}

/// What `viso new` creates.
#[derive(Debug, Args)]
pub struct NewArgs {
    /// The directory to create the project in, made with its parents when
    /// missing; `.` for the current one.
    #[arg(value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub path: PathBuf,
    /// What the project starts as.
    #[arg(long, value_enum, value_name = "NAME", default_value_t)]
    pub template: Template,
    /// The package name; the directory's name when omitted.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// The Rust edition the package is written in.
    #[arg(long, value_enum, value_name = "YEAR", default_value_t)]
    pub edition: Edition,
    /// Do not make the directory a Git repository.
    #[arg(long)]
    pub no_git: bool,
    /// Create the project in a directory that has files already; none of
    /// them is replaced, and one the project would write is an error.
    #[arg(long)]
    pub force_empty_dir: bool,
    /// Depend on the framework at this path (a checkout, or its `crates/viso`)
    /// rather than on the release of this CLI's version.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub framework: Option<PathBuf>,
}

/// The templates `viso new` creates a project from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Template {
    /// A cross-platform app: a view, its state and a UI test.
    #[default]
    App,
    /// A game: a system on the fixed tick, its HUD and a game test.
    Game,
    /// An app whose first target is the browser's DOM.
    Web,
    /// Components for other apps to mount, with no binary.
    Library,
}

impl Template {
    /// Its name on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Template::App => "app",
            Template::Game => "game",
            Template::Web => "web",
            Template::Library => "library",
        }
    }
}

/// The Rust editions a new package can be written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Edition {
    #[value(name = "2021")]
    E2021,
    #[default]
    #[value(name = "2024")]
    E2024,
}

impl Edition {
    /// Its year, as `Cargo.toml` writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Edition::E2021 => "2021",
            Edition::E2024 => "2024",
        }
    }
}

/// What `viso check` checks.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// The target the sources are checked for: its configuration and the
    /// input its devices have; a device target of `Viso.toml` by its name. The
    /// host and the targets `Viso.toml` configures when none is named.
    #[arg(
        value_name = "TARGET",
        value_parser = TargetParser::<BuildTarget>::new(),
        add = ArgValueCandidates::new(complete::targets::<BuildTarget>),
    )]
    pub target: Option<TargetArg<BuildTarget>>,
    /// Check again after every change to the sources, until Ctrl-C.
    #[arg(long)]
    pub watch: bool,
    /// Check the Cargo package's Rust code too, with `cargo check`.
    #[arg(long)]
    pub cargo: bool,
    /// Report accessibility findings as errors (`strict`) or warnings.
    #[arg(long, value_enum, value_name = "LEVEL")]
    pub a11y: Option<Strictness>,
    /// Report localization findings as errors, literal text too (`strict`),
    /// or as warnings.
    #[arg(long, value_enum, value_name = "LEVEL")]
    pub i18n: Option<Strictness>,
}

/// How a class of findings reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Strictness {
    /// As warnings, which leave the exit code alone.
    Warn,
    /// As errors.
    Strict,
}

/// What `viso fmt` formats.
#[derive(Debug, Args)]
pub struct FmtArgs {
    /// The files and directories to format: the project's `src/` and `tests/`
    /// when none is named.
    #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,
    /// Write nothing; fail when a file is not formatted.
    #[arg(long)]
    pub check: bool,
}

/// What `viso completion` writes.
#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// The shell.
    #[arg(value_enum)]
    pub shell: Shell,
}

/// The shells `viso completion` writes for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

/// What `viso config` shows.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Every configuration key, its value and the layer it came from.
    Show,
    /// One configuration key, its value and the layer it came from.
    Get(ConfigGetArgs),
    /// The path of the `Viso.toml` in use.
    Path,
    /// Check `Viso.toml` and the `VISO_*` environment, reporting every
    /// problem.
    Validate,
}

/// The key `viso config get` looks up.
#[derive(Debug, Args)]
pub struct ConfigGetArgs {
    /// The dotted key, as `viso config show` lists it: `package.name`,
    /// `profile.release.opt_level`.
    #[arg(add = ArgValueCandidates::new(complete::config_keys))]
    pub key: String,
}

/// What `viso android` manages.
#[derive(Debug, Subcommand)]
pub enum AndroidCommand {
    /// List the supported API levels, what each installs and which is the
    /// default.
    List,
    /// Install an API level's tested component set and make it the default.
    Use(AndroidUseArgs),
    /// Check the Android SDK, emulator, acceleration and profiles.
    Doctor(PlatformDoctorArgs),
    /// Run the SDK's `adb`, on a profile's running emulator.
    Adb(AdbArgs),
    /// The emulator profiles.
    #[command(subcommand)]
    Emulator(EmulatorCommand),
}

/// What `viso android use` installs.
#[derive(Debug, Args)]
pub struct AndroidUseArgs {
    /// The API level.
    pub api: String,
    /// Accept the licenses of what is downloaded without being asked.
    #[arg(long)]
    pub accept_licenses: bool,
}

/// What `viso ios use` chooses.
#[derive(Debug, Args)]
pub struct IosUseArgs {
    /// The runtime version: `26.3`, `27`, `iOS 27.0`.
    pub runtime: String,
    /// Have Xcode download the runtime, when it is not installed, without
    /// being asked.
    #[arg(long)]
    pub download: bool,
}

/// What `viso android doctor` / `viso ios doctor` may fix.
#[derive(Debug, Args)]
pub struct PlatformDoctorArgs {
    /// Make the local, reversible fixes the checks name.
    #[arg(long)]
    pub fix: bool,
}

/// What `viso android adb` runs.
#[derive(Debug, Args)]
pub struct AdbArgs {
    /// The profile whose running emulator `adb` talks to.
    #[arg(long, add = ArgValueCandidates::new(complete::device_ids))]
    pub device: Option<String>,
    /// The arguments for `adb`, passed on as they are.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "ARGS"
    )]
    pub args: Vec<String>,
}

/// What `viso ios` manages.
#[derive(Debug, Subcommand)]
pub enum IosCommand {
    /// List the Simulator runtimes installed and the one Xcode can install,
    /// and which is the default.
    List,
    /// Make a Simulator runtime the default, having Xcode install it first
    /// when it is not.
    Use(IosUseArgs),
    /// Check Xcode, its license, the Simulator runtime and the profiles.
    Doctor(PlatformDoctorArgs),
    /// The simulator profiles.
    #[command(subcommand)]
    Simulator(SimulatorCommand),
}

/// What is done with an emulator profile.
#[derive(Debug, Subcommand)]
pub enum EmulatorCommand {
    /// List the profiles and whether each is booted.
    List,
    /// Create a profile and its virtual device.
    Create(EmulatorCreateArgs),
    /// Boot a profile's device.
    Start(StartArgs),
    /// Shut a profile's device down.
    Stop(DeviceArgs),
    /// Delete a profile and its virtual device.
    Delete(DeviceArgs),
}

/// What is done with a simulator profile.
#[derive(Debug, Subcommand)]
pub enum SimulatorCommand {
    /// List the profiles and whether each is booted.
    List,
    /// Create a profile and its virtual device.
    Create(CreateArgs),
    /// Boot a profile's device.
    Start(DeviceArgs),
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
}

/// What `viso android emulator create` makes.
#[derive(Debug, Args)]
pub struct EmulatorCreateArgs {
    #[command(flatten)]
    pub profile: CreateArgs,
    /// The Android API level of its system image: the one `viso android
    /// use` chose, else the newest installed, when omitted.
    #[arg(long, value_name = "LEVEL")]
    pub api: Option<String>,
}

/// What `start` boots.
#[derive(Debug, Args)]
pub struct StartArgs {
    /// The profile ID.
    #[arg(add = ArgValueCandidates::new(complete::device_ids))]
    pub id: String,
    /// Boot the emulator without its window.
    #[arg(long)]
    pub no_window: bool,
}

/// The profile a command acts on.
#[derive(Debug, Args)]
pub struct DeviceArgs {
    /// The profile ID.
    #[arg(add = ArgValueCandidates::new(complete::device_ids))]
    pub id: String,
}

/// `viso inspect`: a command, or, without one, the app attached and
/// followed.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct InspectArgs {
    #[command(subcommand)]
    pub command: Option<InspectCommand>,
    #[command(flatten)]
    pub attach: AttachArgs,
}

/// Which session a bare `viso inspect` attaches to.
#[derive(Debug, Args)]
pub struct AttachArgs {
    /// The session's target: the `viso run` session on this machine when
    /// omitted.
    #[arg(long, value_enum)]
    pub target: Option<SessionTarget>,
    /// The simulator or emulator profile of the session (`ios` and
    /// `android` only): the only session of the target when omitted.
    #[arg(long, value_name = "ID", add = ArgValueCandidates::new(complete::device_ids))]
    pub device: Option<String>,
    /// The session, by the ID `viso run` reports (`dev` events' `session`),
    /// or its first characters.
    #[arg(long, value_name = "SESSION", conflicts_with_all = ["target", "device", "run"])]
    pub attach: Option<String>,
    /// Start `viso run` for the target first, and attach once its app
    /// connects; the session stops with the inspector.
    #[arg(long)]
    pub run: bool,
}

/// The targets of a `viso run` session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SessionTarget {
    /// The session's iOS Simulator.
    Ios,
    /// The session's Android Emulator.
    Android,
    /// The session's browser page, on WebGPU.
    WebGpu,
    /// The session's browser page, DOM output.
    WebDom,
    /// The session's browser page, DOM with WebGPU islands.
    WebHybrid,
}

impl SessionTarget {
    pub fn target(self) -> viso_project::Target {
        use viso_project::Target;
        match self {
            SessionTarget::Ios => Target::Ios,
            SessionTarget::Android => Target::Android,
            SessionTarget::WebGpu => Target::WebGpu,
            SessionTarget::WebDom => Target::WebDom,
            SessionTarget::WebHybrid => Target::WebHybrid,
        }
    }
}

/// What `viso inspect` looks at.
#[derive(Debug, Subcommand)]
pub enum InspectCommand {
    /// Show every node the app shows, window by window, with its box,
    /// semantics and the source that declares it.
    Tree(TreeArgs),
    /// Show everything the app holds for the nodes a selector selects:
    /// resolved layout, style and content, transform chain, dirty classes,
    /// interaction states, event route, bound states and paint primitives.
    Node(NodeArgs),
    /// Show what a window's last frame drew and holds: its counters, draw
    /// batches, GPU resources and caches.
    Frame(FrameArgs),
    /// Follow what happens in a window: each input event and the nodes it
    /// reached, focus moving, and each node marked dirty with why.
    Events(EventsArgs),
    /// Find the nodes a selector selects, with their box, semantics and the
    /// source that declares them.
    Query(QueryArgs),
}

/// The app `viso inspect` asks.
#[derive(Debug, Args)]
pub struct InspectedApp {
    /// The app to inspect: the `viso run` session on this machine when
    /// omitted, the session of a target, or `headless`, a headless run of
    /// the app built for it.
    #[arg(long, value_enum)]
    pub target: Option<InspectTarget>,
    /// The simulator or emulator profile of the session (`ios` and
    /// `android` only): the only session of the target when omitted.
    #[arg(long, value_name = "ID", add = ArgValueCandidates::new(complete::device_ids))]
    pub device: Option<String>,
    /// The UI scenario a headless run plays before it is inspected, by its
    /// name under `tests/ui/` (`--target headless` only).
    #[arg(long, value_name = "NAME")]
    pub scenario: Option<String>,
}

/// Which app's tree `viso inspect tree` shows.
#[derive(Debug, Args)]
pub struct TreeArgs {
    #[command(flatten)]
    pub app: InspectedApp,
}

/// What `viso inspect node` details, and in which app.
#[derive(Debug, Args)]
pub struct NodeArgs {
    /// The selector, as `viso inspect query` takes it, or a node ID
    /// (`<index>:<generation>`).
    #[arg(value_name = "SELECTOR")]
    pub selector: String,
    #[command(flatten)]
    pub app: InspectedApp,
}

/// Which window's frame `viso inspect frame` shows.
#[derive(Debug, Args)]
pub struct FrameArgs {
    /// The window, by its index in the tree: the first when omitted.
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub window: u32,
    #[command(flatten)]
    pub app: InspectedApp,
}

/// Which window's events `viso inspect events` follows, and for how long.
#[derive(Debug, Args)]
pub struct EventsArgs {
    /// The window, by its index in the tree: the first when omitted.
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub window: u32,
    /// How long to follow, in seconds: until interrupted when omitted. A
    /// headless run reports every event of its scenario instead.
    #[arg(long, value_name = "SECONDS", value_parser = parse_seconds)]
    pub seconds: Option<f64>,
    #[command(flatten)]
    pub app: InspectedApp,
}

/// What `viso inspect query` selects, and in which app.
#[derive(Debug, Args)]
pub struct QueryArgs {
    /// The selector: a type (`Button`, a component), `#name`, `.Style`,
    /// `[attribute]` or `[attribute=value]`, joined by a space (descendant)
    /// or `>` (child); several separated by `,`.
    #[arg(value_name = "SELECTOR")]
    pub selector: String,
    #[command(flatten)]
    pub app: InspectedApp,
}

/// What `viso profile` samples, and for how long.
#[derive(Debug, Args)]
pub struct ProfileArgs {
    /// The app to profile: the `viso run` session on this machine when
    /// omitted, the session of a target, or `headless`, a headless run of
    /// the app built for it.
    #[arg(value_enum)]
    pub target: Option<InspectTarget>,
    /// The simulator or emulator profile of the session (`ios` and
    /// `android` only): the only session of the target when omitted.
    #[arg(long, value_name = "ID", add = ArgValueCandidates::new(complete::device_ids))]
    pub device: Option<String>,
    /// How many frames to sample: the capture ends once the app drew them.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=100_000))]
    pub frames: Option<u32>,
    /// How long to sample, in seconds: 5 when neither this nor `--frames`
    /// is given; with both, whichever ends first.
    #[arg(long, value_name = "SECONDS", value_parser = parse_seconds)]
    pub seconds: Option<f64>,
    /// Where to write the trace, in Ende: under `target/viso/traces/` when
    /// omitted.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub output: Option<PathBuf>,
    /// Also write the trace as Chrome trace-event JSON, for `chrome://tracing`
    /// or Perfetto.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub chrome: Option<PathBuf>,
    /// The UI scenario a headless run plays while it is profiled, by its
    /// name under `tests/ui/` (`headless` only): the app settling when
    /// omitted.
    #[arg(long, value_name = "NAME")]
    pub scenario: Option<String>,
    /// Puts the app's launch window through a change a step each frame while
    /// it is sampled, so the frames are what it costs: its size
    /// (`resize`), its scale factor (`rescale`) or its surface, lost and
    /// made again (`recreate`). Not for `headless`, whose surface is a
    /// buffer and whose GPU nothing times.
    #[arg(long, value_enum, value_name = "CHANGE")]
    pub drive: Option<ProfileDriveArg>,
}

/// What `viso profile --drive` puts the launch window through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ProfileDriveArg {
    Resize,
    Rescale,
    Recreate,
}

impl ProfileDriveArg {
    pub fn drive(self) -> viso_dev_protocol::profile::ProfileDrive {
        use viso_dev_protocol::profile::ProfileDrive;
        match self {
            Self::Resize => ProfileDrive::Resize,
            Self::Rescale => ProfileDrive::Rescale,
            Self::Recreate => ProfileDrive::Recreate,
        }
    }
}

/// A `--seconds` value: positive, at most an hour.
fn parse_seconds(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(seconds) if seconds > 0.0 && seconds <= 3600.0 => Ok(seconds),
        _ => Err(format!(
            "`{text}` is not a number of seconds above 0, at most 3600"
        )),
    }
}

/// The apps `viso inspect` inspects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum InspectTarget {
    /// A headless run, built and played for the inspection.
    Headless,
    /// The session's iOS Simulator.
    Ios,
    /// The session's Android Emulator.
    Android,
    /// The session's browser page, on WebGPU.
    WebGpu,
    /// The session's browser page, DOM output.
    WebDom,
    /// The session's browser page, DOM with WebGPU islands.
    WebHybrid,
}

impl InspectTarget {
    pub fn target(self) -> viso_project::Target {
        use viso_project::Target;
        match self {
            InspectTarget::Headless => Target::Headless,
            InspectTarget::Ios => Target::Ios,
            InspectTarget::Android => Target::Android,
            InspectTarget::WebGpu => Target::WebGpu,
            InspectTarget::WebDom => Target::WebDom,
            InspectTarget::WebHybrid => Target::WebHybrid,
        }
    }
}

/// What `viso game` does with a running game.
#[derive(Debug, Subcommand)]
pub enum GameCommand {
    /// Print a state of the running game as JSON.
    Peek(PeekArgs),
    /// Restart the running game's World and record the next stretch of
    /// play as an input tape.
    Record(RecordArgs),
    /// Run the last seconds of play again on the code running now, as a
    /// logic reload left it.
    Replay(ReplayArgs),
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
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
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

/// What `viso game replay` replays.
#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// The game, by its `.vs` file: a path from the project root, with or
    /// without the extension, or the file's stem.
    pub game: String,
    /// How many seconds of play to run again; all the game kept by default.
    #[arg(long, value_name = "S")]
    pub seconds: Option<f64>,
}

/// The test domains `viso test` runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TestDomain {
    /// The Cargo package's own tests (`cargo test`).
    Unit,
    /// UI scenarios under `tests/ui/`, played on a headless build.
    Ui,
    /// Deterministic game scenarios under `tests/game/`.
    Game,
    /// Tests in a browser.
    Web,
    /// Every domain; one that has nothing to run here is skipped.
    All,
}

impl TestDomain {
    pub fn as_str(self) -> &'static str {
        match self {
            TestDomain::Unit => "unit",
            TestDomain::Ui => "ui",
            TestDomain::Game => "game",
            TestDomain::Web => "web",
            TestDomain::All => "all",
        }
    }
}

/// Where `viso test` runs its cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TestTarget {
    /// In this machine's memory, drawn by the software raster.
    Headless,
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

impl TestTarget {
    pub fn target(self) -> viso_project::Target {
        use viso_project::Target;
        match self {
            TestTarget::Headless => Target::Headless,
            TestTarget::Ios => Target::Ios,
            TestTarget::Android => Target::Android,
            TestTarget::WebGpu => Target::WebGpu,
            TestTarget::WebDom => Target::WebDom,
            TestTarget::WebHybrid => Target::WebHybrid,
        }
    }
}

/// What `viso test` runs.
#[derive(Debug, Args)]
pub struct TestArgs {
    /// The test domain; every domain when omitted.
    pub domain: Option<TestDomain>,
    /// One scenario of the `ui` or `game` domain, `tests/<domain>/<NAME>.tape`.
    #[arg(add = ArgValueCandidates::new(complete::scenarios))]
    pub scenario: Option<String>,
    /// Run only the cases whose name contains PATTERN (is PATTERN, with
    /// `--exact`).
    #[arg(long, value_name = "PATTERN")]
    pub filter: Option<String>,
    /// `--filter` names a case exactly.
    #[arg(long, requires = "filter")]
    pub exact: bool,
    /// Stop at the first failed case.
    #[arg(long)]
    pub fail_fast: bool,
    /// Show what every case writes, not only a failed case's.
    #[arg(long)]
    pub nocapture: bool,
    /// Rewrite the golden of each snapshot a UI scenario takes that is
    /// missing or no longer matches, instead of failing the case.
    #[arg(long)]
    pub update_snapshots: bool,
    /// Where the cases run: `headless` by default.
    #[arg(long, value_enum, value_name = "TARGET")]
    pub target: Option<TestTarget>,
    /// The simulator or emulator profile UI scenarios run on (`--target
    /// ios` and `android` only).
    #[arg(long, value_name = "ID", add = ArgValueCandidates::new(complete::device_ids))]
    pub device: Option<String>,
    /// The frames a game scenario runs, one fixed tick each; by default
    /// through the tape and the scenario's last expectation.
    #[arg(long, value_name = "N")]
    pub frames: Option<u64>,
    /// A game world's random seed, over the tape's.
    #[arg(long, value_name = "SEED")]
    pub seed: Option<u64>,
    /// Replay the game input tape at PATH, binary or text, instead of the
    /// scenario's.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub tape: Option<PathBuf>,
    /// Draw each game scenario's run as a sheet of its frames, a PNG at
    /// PATH, through a headless build of the app; with several scenarios,
    /// `<stem>-<scenario>.png` beside it each.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub sheet: Option<PathBuf>,
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
    /// Not a place to run: taken only to say what runs headless.
    #[value(hide = true)]
    Headless,
}

/// What `viso run` runs, and passes on.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Where to run: this machine when omitted; a device target of
    /// `Viso.toml` by its name, on the board its `deploy` names, or here
    /// when this machine is the board. Several run side by side in one
    /// session, each edit compiled once for them all.
    #[arg(
        value_name = "TARGET",
        value_parser = TargetParser::<RunTarget>::new(),
        add = ArgValueCandidates::new(complete::targets::<RunTarget>),
    )]
    pub targets: Vec<TargetArg<RunTarget>>,
    /// Run on this machine too, beside the targets named: its app is the
    /// session's first.
    #[arg(long)]
    pub desktop: bool,
    /// The simulator or emulator profile (`ios` and `android` only).
    #[arg(long, value_name = "ID", add = ArgValueCandidates::new(complete::device_ids))]
    pub device: Option<String>,
    /// The browser to open (`web-*` only): `chrome`, `safari`, `firefox`, or
    /// `none` to only serve.
    #[arg(long, value_name = "NAME")]
    pub browser: Option<String>,
    /// Keep the dev session up but apply no edit to the app: each is compiled
    /// and reported, not sent.
    #[arg(long)]
    pub no_hot_reload: bool,
    /// Draw the frame time over the app's windows, and report every frame
    /// it draws: a `profile` event each under `--json`, a line a second
    /// otherwise.
    #[arg(long)]
    pub profile_frame: bool,
    /// Follow what happens in the app's first window, as `viso inspect`
    /// does: an `inspect` event each under `--json`, a line each otherwise.
    #[arg(long)]
    pub inspect: bool,
    /// Open the page in a browser (`web-*` only), under `--json` too.
    #[arg(long)]
    pub open: bool,
    /// Run an `embedded-cpu` device target in a window on this Mac, with
    /// its panel's size, format and rotation, its budgets and its workers.
    #[arg(long)]
    pub emulate: bool,
    /// A variable the app is launched with; repeatable. A value whose name
    /// looks secret is never printed.
    #[arg(long = "env", value_name = "KEY=VALUE", value_parser = parse_env)]
    pub env: Vec<(String, String)>,
    /// The app's working directory (this machine only): the project root when
    /// omitted.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub cwd: Option<PathBuf>,
    /// Arguments for the app, after `--`.
    #[arg(last = true, value_name = "APP_ARGS")]
    pub app_args: Vec<OsString>,
}

/// A `--env` variable: a portable name (letters, digits and `_`, not
/// starting with a digit) outside the dev session's own `VISO_DEV_*`, `=`,
/// and its value.
fn parse_env(text: &str) -> Result<(String, String), String> {
    let (name, value) = text
        .split_once('=')
        .ok_or_else(|| format!("`{text}` is not `KEY=VALUE`"))?;
    let portable = name
        .chars()
        .enumerate()
        .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()));
    if name.is_empty() || !portable {
        return Err(format!(
            "`{name}` is not a variable name: letters, digits and `_`, not starting with a digit"
        ));
    }
    if name.starts_with("VISO_DEV_") {
        return Err(format!("`{name}` is the dev session's own variable"));
    }
    if value.contains('\0') {
        return Err(format!("the value of `{name}` holds a NUL"));
    }
    Ok((name.to_owned(), value.to_owned()))
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
    /// The web targets, each once and each served under its own path when
    /// there are several: `[web] default_target` when omitted.
    #[arg(value_enum)]
    pub targets: Vec<WebTarget>,
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

/// What `viso dump` prints.
#[derive(Debug, Args)]
pub struct DumpArgs {
    /// The IR to print.
    #[arg(value_enum)]
    pub kind: DumpKind,
    /// A `.vs` file of the project, or a declaration by its path or a
    /// `::`-suffix of it (`Counter`, `app::counter::Counter.increment`);
    /// every file when omitted.
    pub input: Option<String>,
    /// Narrow a file's dump to one of its declarations, by path or suffix,
    /// optionally with `.member`.
    #[arg(long, value_name = "PATH")]
    pub symbol: Option<String>,
    /// Write the dump to FILE instead of printing it.
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
}

/// An IR `viso dump` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DumpKind {
    /// The syntax tree, trivia aside.
    Ast,
    /// The typed HIR: members with their types, effects and reads.
    Hir,
    /// The retained-tree template of the view each file mounts.
    UiIr,
    /// The binding edges and what each reactive source invalidates.
    ReactiveIr,
    /// The Behavior IR the bodies compile to.
    BehaviorIr,
    /// The checked shader programs.
    ShaderIr,
    /// The systems in scheduler order, their hooks and states.
    SystemIr,
    /// The modules, their files, imports and declarations.
    ModuleGraph,
}

/// What `viso snapshot` does.
#[derive(Debug, Subcommand)]
pub enum SnapshotCommand {
    /// Render the scenarios headless and write each snapshot as its golden.
    Capture(CaptureArgs),
    /// Hold each snapshot to its golden; what departs goes to `target/viso/snapshots/`.
    Compare(SnapshotArgs),
    /// Rewrite only the goldens that are missing or no longer match.
    Update(UpdateArgs),
}

/// Which scenarios and components `viso snapshot` takes, and where.
#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// One scenario, `tests/ui/<NAME>.tape`, else a component shown alone,
    /// `Name` or `module::Name`; every scenario and component golden when
    /// omitted.
    #[arg(value_name = "NAME", add = ArgValueCandidates::new(complete::scenarios))]
    pub scenario: Option<String>,
    /// Where the snapshots are taken: `headless`, the default.
    #[arg(long, value_enum, value_name = "TARGET")]
    pub target: Option<TestTarget>,
}

/// What `viso preview` shows.
#[derive(Debug, Args)]
pub struct PreviewArgs {
    /// The `.vs` file of the generated code, compiled with the project's
    /// package as the module its name gives.
    #[arg(value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub file: PathBuf,
    /// The component it shows, `Name`; the one the file exports when omitted.
    #[arg(value_name = "COMPONENT")]
    pub component: Option<String>,
    /// A capability granted beside `ui.basic`, `gpu.draw.sandboxed` and
    /// `asset.read.package`; repeatable.
    #[arg(long = "grant", value_name = "CAPABILITY")]
    pub grants: Vec<String>,
    /// Write the snapshot into DIR instead of `target/viso/preview/`.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub output: Option<PathBuf>,
}

/// What `viso snapshot capture` captures.
#[derive(Debug, Args)]
pub struct CaptureArgs {
    #[command(flatten)]
    pub common: SnapshotArgs,
    /// Write the snapshots into DIR instead of `tests/snapshots/`.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub output: Option<PathBuf>,
}

/// What `viso snapshot update` rewrites.
#[derive(Debug, Args)]
pub struct UpdateArgs {
    #[command(flatten)]
    pub common: SnapshotArgs,
    /// Rewrite only the snapshots whose name contains PATTERN.
    #[arg(long, value_name = "PATTERN")]
    pub filter: Option<String>,
}

/// How `viso lsp` starts the language server.
#[derive(Debug, Args)]
pub struct LspArgs {
    /// Speak the protocol over stdin and stdout, the transport the server has.
    #[arg(long)]
    pub stdio: bool,
}

/// What `viso explain` explains.
#[derive(Debug, Args)]
pub struct ExplainArgs {
    /// The diagnostic code: `E3101`, `C0004`, `ENV_LOCK_HELD`.
    #[arg(required_unless_present = "list", conflicts_with = "list")]
    pub code: Option<String>,
    /// List every code with its title.
    #[arg(long)]
    pub list: bool,
}

/// What `viso schema` looks up.
#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// The symbol, by full path or a `::`-suffix of it, optionally with
    /// `.member`: `Button`, `viso::widgets::Button`, `Button.text`,
    /// `text::upper`.
    #[arg(
        required_unless_present_any = ["search", "list"],
        conflicts_with_all = ["search", "list"]
    )]
    pub symbol: Option<String>,
    /// List every symbol and member whose path contains TERM, ignoring case.
    #[arg(long, value_name = "TERM", conflicts_with = "list")]
    pub search: Option<String>,
    /// List every symbol of one group.
    #[arg(long, value_enum, value_name = "GROUP")]
    pub list: Option<SchemaGroup>,
}

/// A group of symbols `viso schema --list` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SchemaGroup {
    /// The framework's widgets.
    Widgets,
    /// The project's components, with where each is declared.
    Components,
    /// The native libraries and their functions.
    Natives,
    /// The native handle types.
    Types,
}

impl Command {
    /// The command path the JSON envelope reports.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Android(AndroidCommand::List) => "android list",
            Command::Android(AndroidCommand::Use(_)) => "android use",
            Command::Android(AndroidCommand::Doctor(_)) => "android doctor",
            Command::Android(AndroidCommand::Adb(_)) => "android adb",
            Command::Android(AndroidCommand::Emulator(command)) => match command {
                EmulatorCommand::List => "android emulator list",
                EmulatorCommand::Create(_) => "android emulator create",
                EmulatorCommand::Start(_) => "android emulator start",
                EmulatorCommand::Stop(_) => "android emulator stop",
                EmulatorCommand::Delete(_) => "android emulator delete",
            },
            Command::Build(_) => "build",
            Command::Check(_) => "check",
            Command::Clean => "clean",
            Command::Doctor(_) => "doctor",
            Command::Completion(_) => "completion",
            Command::Dump(_) => "dump",
            Command::Explain(_) => "explain",
            Command::Export(ExportCommand::Html(_)) => "export html",
            Command::Export(ExportCommand::Solid(_)) => "export solid",
            Command::Fmt(_) => "fmt",
            Command::Config(ConfigCommand::Show) => "config show",
            Command::Config(ConfigCommand::Get(_)) => "config get",
            Command::Config(ConfigCommand::Path) => "config path",
            Command::Config(ConfigCommand::Validate) => "config validate",
            Command::Game(GameCommand::Peek(_)) => "game peek",
            Command::Game(GameCommand::Record(_)) => "game record",
            Command::Game(GameCommand::Replay(_)) => "game replay",
            Command::Inspect(InspectArgs { command, .. }) => match command {
                None => "inspect",
                Some(InspectCommand::Tree(_)) => "inspect tree",
                Some(InspectCommand::Node(_)) => "inspect node",
                Some(InspectCommand::Frame(_)) => "inspect frame",
                Some(InspectCommand::Events(_)) => "inspect events",
                Some(InspectCommand::Query(_)) => "inspect query",
            },
            Command::Ios(IosCommand::List) => "ios list",
            Command::Ios(IosCommand::Use(_)) => "ios use",
            Command::Ios(IosCommand::Doctor(_)) => "ios doctor",
            Command::Ios(IosCommand::Simulator(command)) => match command {
                SimulatorCommand::List => "ios simulator list",
                SimulatorCommand::Create(_) => "ios simulator create",
                SimulatorCommand::Start(_) => "ios simulator start",
                SimulatorCommand::Stop(_) => "ios simulator stop",
                SimulatorCommand::Delete(_) => "ios simulator delete",
            },
            Command::Lsp(_) => "lsp",
            Command::New(_) => "new",
            Command::Package(_) => "package",
            Command::Preview(_) => "preview",
            Command::Profile(_) => "profile",
            Command::Run(_) => "run",
            Command::Schema(_) => "schema",
            Command::Serve(_) => "serve",
            Command::Snapshot(SnapshotCommand::Capture(_)) => "snapshot capture",
            Command::Snapshot(SnapshotCommand::Compare(_)) => "snapshot compare",
            Command::Snapshot(SnapshotCommand::Update(_)) => "snapshot update",
            Command::Test(_) => "test",
        }
    }
}

/// The grammar, as the parser and the completion engine both read it, with
/// its help pages.
pub fn command() -> clap::Command {
    crate::help::decorate(Cli::command())
}

/// Parses this process's command line.
pub fn parse() -> Result<Cli, clap::Error> {
    use clap::FromArgMatches;
    let mut command = command();
    let matches = command.try_get_matches_from_mut(std::env::args_os())?;
    Cli::from_arg_matches(&matches).map_err(|error| error.format(&mut command))
}

/// Whether `command` names its target as a positional argument, so a
/// `--target` there is the other commands' spelling.
pub fn takes_positional_target(command: &str) -> bool {
    matches!(
        command,
        "build" | "check" | "doctor" | "package" | "run" | "serve"
    )
}

/// Whether `--json` appears among `args` (the arguments after the program name)
/// before any `--`, for a command line too broken to parse.
pub fn wants_json<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> bool {
    args.iter()
        .map(AsRef::as_ref)
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json")
}

/// The command path `args` name, `config get` say, for a command line too
/// broken to parse: each subcommand of the one before, wherever it stands
/// among the options; `None` when `args` name no command.
pub fn named_command<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Option<String> {
    let mut grammar = Cli::command();
    let mut path: Vec<&str> = Vec::new();
    for arg in args
        .iter()
        .map(AsRef::as_ref)
        .take_while(|arg| *arg != "--")
        .filter_map(|arg| arg.to_str())
    {
        if let Some(sub) = grammar.find_subcommand(arg) {
            grammar = sub.clone();
            path.push(arg);
        }
    }
    (!path.is_empty()).then(|| path.join(" "))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn an_env_variable_is_a_portable_name_and_a_value() {
        assert_eq!(
            parse_env("GREETING=hi=there"),
            Ok(("GREETING".to_owned(), "hi=there".to_owned()))
        );
        assert_eq!(parse_env("_X1="), Ok(("_X1".to_owned(), String::new())));
        for bad in ["GREETING", "=x", "1X=y", "A-B=c", "VISO_DEV_TOKEN=t"] {
            assert!(parse_env(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_grammar_is_consistent() {
        command().debug_assert();
    }

    #[test]
    fn global_options_parse_on_either_side_of_the_subcommand() {
        let argv = [
            "viso",
            "-v",
            "--color",
            "never",
            "check",
            "--offline",
            "--locked",
            "--jobs",
            "4",
            "--profile",
            "release",
            "--target-dir",
            "out",
        ];
        let g = Cli::try_parse_from(argv).unwrap().global;
        assert!(g.verbose && g.offline && g.locked && !g.quiet);
        assert_eq!(g.color, ColorChoice::Never);
        assert_eq!(g.jobs, Some(4));
        assert_eq!(g.profile, Some(ProfileArg::Release));
        assert_eq!(g.target_dir, Some(PathBuf::from("out")));
        assert_eq!(
            Cli::try_parse_from(["viso", "check"]).unwrap().global.color,
            ColorChoice::Auto
        );
        for bad in [
            &["viso", "-q", "-v", "check"][..],
            &["viso", "check", "--profile", "fast"],
            &["viso", "check", "--jobs", "0"],
            &["viso", "check", "--color", "sometimes"],
        ] {
            let error = Cli::try_parse_from(bad).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{bad:?}");
        }
    }

    /// Short options are `-h`, `-V`, `-v` and `-q` only, and no command or
    /// option has an alias: a script reads the same as its documentation.
    #[test]
    fn the_only_short_options_are_help_version_verbose_and_quiet() {
        fn walk(command: &clap::Command, path: &str, shorts: &mut Vec<(String, char)>) {
            assert!(
                command.get_all_aliases().next().is_none(),
                "`{path}` has an alias"
            );
            for arg in command.get_arguments() {
                if let Some(short) = arg.get_short() {
                    shorts.push((path.to_owned(), short));
                }
                assert!(
                    arg.get_all_aliases().is_none_or(|a| a.is_empty()),
                    "`{path} --{}` has an alias",
                    arg.get_id()
                );
            }
            for sub in command.get_subcommands() {
                walk(sub, &format!("{path} {}", sub.get_name()), shorts);
            }
        }
        let mut command = Cli::command();
        command.build();
        let mut shorts = Vec::new();
        walk(&command, "viso", &mut shorts);
        for (path, short) in shorts {
            assert!(
                matches!(short, 'h' | 'V' | 'v' | 'q'),
                "`{path}` has `-{short}`"
            );
        }
    }

    #[test]
    fn config_shows_gets_paths_and_validates() {
        for (argv, name) in [
            (&["viso", "config", "show"][..], "config show"),
            (&["viso", "config", "get", "package.name"], "config get"),
            (&["viso", "config", "path"], "config path"),
            (&["viso", "config", "validate", "--json"], "config validate"),
        ] {
            assert_eq!(Cli::try_parse_from(argv).unwrap().command.name(), name);
        }
        assert!(Cli::try_parse_from(["viso", "config", "get"]).is_err());
        assert!(Cli::try_parse_from(["viso", "config", "set", "a", "b"]).is_err());
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
                targets,
                host: Some(_),
                port: Some(9000),
                lan: true,
                open: false,
                ..
            }) if targets[..] == [WebTarget::Gpu]
        ));
        let cli =
            Cli::try_parse_from(["viso", "serve", "web-gpu", "web-dom", "web-hybrid"]).unwrap();
        assert!(matches!(
            &cli.command,
            Command::Serve(ServeArgs { targets, .. })
                if targets[..] == [WebTarget::Gpu, WebTarget::Dom, WebTarget::Hybrid]
        ));
        assert!(Cli::try_parse_from(["viso", "serve", "ios"]).is_err());
        assert!(Cli::try_parse_from(["viso", "serve", "web-gpu", "ios"]).is_err());
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
        assert_eq!(
            named_command(&["--json", "config", "get"]).as_deref(),
            Some("config get")
        );
    }

    #[test]
    fn run_passes_what_follows_the_separator_to_the_app() {
        let cli =
            Cli::try_parse_from(["viso", "run", "--json", "--", "--open", "demo.vs"]).unwrap();
        assert!(cli.global.json);
        assert!(
            matches!(&cli.command, Command::Run(RunArgs { app_args, no_hot_reload: false, targets, .. }) if app_args == &["--open", "demo.vs"] && targets.is_empty())
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
        let cli = Cli::try_parse_from(["viso", "run", "web-gpu", "panel", "--desktop"]).unwrap();
        assert!(matches!(
            &cli.command,
            Command::Run(RunArgs { targets, desktop: true, .. })
                if targets[..] == [TargetArg::Builtin(RunTarget::WebGpu), TargetArg::Device("panel".into())]
        ));
        // Parsed only for the command to say where headless runs.
        assert!(matches!(
            Cli::try_parse_from(["viso", "run", "headless"])
                .unwrap()
                .command,
            Command::Run(RunArgs { targets, .. })
                if targets[..] == [TargetArg::Builtin(RunTarget::Headless)]
        ));
        assert!(Cli::try_parse_from(["viso", "run", "--target", "ios"]).is_err());
        let cli =
            Cli::try_parse_from(["viso", "run", "android", "--device", "pixel-local"]).unwrap();
        assert!(matches!(
            &cli.command,
            Command::Run(RunArgs { targets, device: Some(d), .. }) if d == "pixel-local" && targets[..] == [TargetArg::Builtin(RunTarget::Android)]
        ));
        let cli = Cli::try_parse_from(["viso", "run", "web-gpu", "--browser", "none"]).unwrap();
        assert!(matches!(
            &cli.command,
            Command::Run(RunArgs {
                targets,
                browser: Some(_),
                ..
            }) if targets[..] == [TargetArg::Builtin(RunTarget::WebGpu)]
        ));
        assert!(!wants_json(&["run", "--", "--json"]));
    }

    /// A word that is no built-in target is a device target's name, for the
    /// project to know; a built-in target the command does not take, and a
    /// word no device target could be named, are usage errors.
    #[test]
    fn a_positional_target_is_built_in_or_a_device_targets_name() {
        let cli = Cli::try_parse_from(["viso", "build", "panel", "--release"]).unwrap();
        assert!(matches!(
            &cli.command,
            Command::Build(BuildArgs { target: Some(TargetArg::Device(name)), release: true }) if name == "panel"
        ));
        let cli = Cli::try_parse_from(["viso", "build", "ios"]).unwrap();
        assert!(matches!(
            &cli.command,
            Command::Build(BuildArgs {
                target: Some(TargetArg::Builtin(BuildTarget::Ios)),
                ..
            })
        ));
        let cli = Cli::try_parse_from(["viso", "run", "panel"]).unwrap();
        assert!(matches!(
            &cli.command,
            Command::Run(RunArgs { targets, .. }) if targets[..] == [TargetArg::Device("panel".into())]
        ));
        let cli = Cli::try_parse_from(["viso", "check", "kiosk-2"]).unwrap();
        assert!(matches!(
            &cli.command,
            Command::Check(CheckArgs { target: Some(TargetArg::Device(name)), .. }) if name == "kiosk-2"
        ));
        for argv in [
            ["viso", "build", "host"],
            ["viso", "run", "linux"],
            ["viso", "build", "Panel"],
            ["viso", "run", "my/panel"],
        ] {
            let error = Cli::try_parse_from(argv).unwrap_err();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::InvalidValue,
                "{argv:?}"
            );
            assert!(error.to_string().contains("web-gpu"), "{argv:?}: {error}");
        }
        assert_eq!(
            TargetArg::<BuildTarget>::Device("panel".into())
                .target(BuildTarget::target)
                .as_str(),
            "panel"
        );
    }

    #[test]
    fn device_profiles_are_managed_per_platform() {
        let cli = Cli::try_parse_from([
            "viso", "android", "emulator", "create", "pixel35", "--api", "35",
        ])
        .unwrap();
        assert_eq!(cli.command.name(), "android emulator create");
        let Command::Android(AndroidCommand::Emulator(EmulatorCommand::Create(args))) = cli.command
        else {
            panic!("a create");
        };
        assert_eq!(
            (&*args.profile.id, args.api.as_deref()),
            ("pixel35", Some("35"))
        );
        assert_eq!(args.profile.preset, PresetArg::Phone);
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
        // What only an emulator has, a simulator does not take.
        let api = [
            "viso",
            "ios",
            "simulator",
            "create",
            "iphone",
            "--api",
            "35",
        ];
        assert!(Cli::try_parse_from(api).is_err());
        let window = ["viso", "ios", "simulator", "start", "iphone", "--no-window"];
        assert!(Cli::try_parse_from(window).is_err());
    }

    #[test]
    fn schema_takes_a_symbol_a_search_or_a_group() {
        let cli = Cli::try_parse_from(["viso", "schema", "Button.text", "--json"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Schema(SchemaArgs { symbol: Some(s), search: None, list: None }) if s == "Button.text"
        ));
        assert!(cli.global.json);
        let cli = Cli::try_parse_from(["viso", "schema", "--search", "text"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Schema(SchemaArgs {
                symbol: None,
                search: Some(_),
                list: None,
            })
        ));
        let cli = Cli::try_parse_from(["viso", "schema", "--list", "components"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Schema(SchemaArgs {
                list: Some(SchemaGroup::Components),
                ..
            })
        ));
        assert!(Cli::try_parse_from(["viso", "schema"]).is_err());
        assert!(Cli::try_parse_from(["viso", "schema", "Button", "--search", "x"]).is_err());
        assert!(Cli::try_parse_from(["viso", "schema", "Button", "--list", "types"]).is_err());
        assert!(Cli::try_parse_from(["viso", "schema", "--list", "things"]).is_err());
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
        assert_eq!(args.domain, Some(TestDomain::Game));
        assert_eq!(args.scenario.as_deref(), Some("movement"));
        assert_eq!((args.frames, args.seed), (Some(600), Some(1234)));
        assert_eq!(args.tape, Some(PathBuf::from("runs/jump.tape")));
        assert!(Cli::try_parse_from(["viso", "test", "game"]).is_ok());
        let cli = Cli::try_parse_from([
            "viso",
            "test",
            "--filter",
            "count",
            "--exact",
            "--fail-fast",
            "--target",
            "headless",
        ])
        .unwrap();
        let Command::Test(args) = cli.command else {
            panic!("a test command");
        };
        assert_eq!(args.domain, None);
        assert_eq!(args.filter.as_deref(), Some("count"));
        assert!(args.exact && args.fail_fast);
        assert_eq!(args.target, Some(TestTarget::Headless));
        assert!(Cli::try_parse_from(["viso", "test", "--exact"]).is_err());
        assert!(Cli::try_parse_from(["viso", "test", "movement"]).is_err());
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

        let cli = Cli::try_parse_from([
            "viso",
            "game",
            "record",
            "movement",
            "--output",
            "runs/jump.tape",
        ])
        .unwrap();
        let Command::Game(GameCommand::Record(args)) = cli.command else {
            panic!("a recording");
        };
        assert_eq!(args.output, PathBuf::from("runs/jump.tape"));
        assert_eq!((args.seconds, args.ticks), (10.0, None));
        let ticks = [
            "viso", "game", "record", "m", "--output", "t", "--ticks", "600",
        ];
        assert!(Cli::try_parse_from(ticks).is_ok());
        let both = [
            "viso",
            "game",
            "record",
            "m",
            "--output",
            "t",
            "--ticks",
            "6",
            "--seconds",
            "1",
        ];
        assert!(Cli::try_parse_from(both).is_err());
        assert!(
            Cli::try_parse_from(["viso", "game", "record", "m"]).is_err(),
            "--output is needed"
        );

        let cli = Cli::try_parse_from(["viso", "game", "replay", "m", "--seconds", "3"]).unwrap();
        let Command::Game(GameCommand::Replay(args)) = &cli.command else {
            panic!("a replay");
        };
        assert_eq!((&*args.game, args.seconds), ("m", Some(3.0)));
        assert_eq!(cli.command.name(), "game replay");
    }
}
