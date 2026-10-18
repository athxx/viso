//! One `cargo build` of the app: the profile's flags, the Rust target and
//! its toolchain, run until it ends or is cancelled, with `cargo` and every
//! `rustc`, linker and build script under it killed together — the process
//! group on Unix, a job object on Windows.
//!
//! Cargo's JSON messages give the executable and each compiler diagnostic.
//! On a terminal the diagnostic's rendered text and Cargo's progress go to
//! stderr as Cargo writes them; read as events, each diagnostic arrives
//! whole, with its spans, and every other line Cargo writes as a log line.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::process::ProcessTree;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use viso_ende::JsonValue;
use viso_project::{
    Layout, LockError, LockGuard, LockKind, OptLevel, Origin, Profile, ProfileSettings, Sourced,
    Target,
};

/// How long a build waits on its child between checks that it ended, when
/// Cargo's output has not closed to say so.
const EXIT_POLL: Duration = Duration::from_millis(100);

/// How often a build waiting for the build cache's lock tries it again.
const LOCK_POLL: Duration = Duration::from_millis(250);

/// Where a build hands what Cargo says; `false` back stops the reading.
pub(crate) type Sink = Arc<dyn Fn(CargoEvent) -> bool + Send + Sync>;

/// What wraps a built executable for the device it runs on — an `.app`, an
/// APK, a web bundle — on the build's thread: the artifact to launch, or why
/// there is none.
pub(crate) type Package = Arc<dyn Fn(&Path) -> Result<PathBuf, String> + Send + Sync>;

/// What a build says while it runs.
#[derive(Debug)]
pub(crate) enum CargoEvent {
    /// A line Cargo wrote, at the level its prefix gives.
    Log { level: &'static str, line: String },
    /// A compiler diagnostic, read as an event.
    Diagnostic(RustDiagnostic),
}

/// The facade's feature that compiles the Dev Runtime's hot reload in.
pub(crate) const HOT_RELOAD: &str = "viso/hot-reload";

/// The facade's feature that makes the build headless.
pub(crate) const HEADLESS: &str = "viso/headless";

/// The facade's feature that links the behavior interpreter a debug build
/// runs.
pub(crate) const VM: &str = "viso/vm";

/// The facade's feature that shows an embedded profile's panel in a desktop
/// window.
pub(crate) const EMULATE: &str = "viso/emulate";

/// Whether a dev build of `target` carries [`HOT_RELOAD`]: not a headless
/// build, which is for tests and tooling that reload nothing, nor a device
/// target's, deployed to a board no dev session reaches.
pub(crate) fn reloads(target: Target) -> bool {
    !matches!(target, Target::Headless | Target::Device(_))
}

/// How the app is built.
#[derive(Clone)]
pub(crate) struct BuildCommand {
    /// The `cargo` to run.
    pub(crate) cargo: PathBuf,
    /// The app package's manifest, a canonical path.
    pub(crate) manifest: PathBuf,
    /// Whether Cargo prints only its errors.
    pub(crate) quiet: bool,
    /// Whether Cargo's output is read as events rather than written to the
    /// terminal.
    pub(crate) piped: bool,
    /// Where Cargo builds, when not where it chooses.
    pub(crate) target_dir: Option<PathBuf>,
    /// The options every Cargo command runs with, `cargo metadata` too.
    pub(crate) cargo_args: Vec<OsString>,
    /// The options `cargo build` runs with besides those.
    pub(crate) build_args: Vec<OsString>,
    /// The Viso profile, which the facade's build script reads.
    pub(crate) profile: Profile,
    /// The Cargo profile and its settings: [`profile_args`].
    pub(crate) profile_args: Vec<String>,
    /// The features of the app's dependencies turned on: [`HOT_RELOAD`],
    /// [`HEADLESS`].
    pub(crate) features: Vec<&'static str>,
    /// Whether the app's default features are off, and so the facade's
    /// where the app turns them on through its own.
    pub(crate) no_default_features: bool,
    /// The Rust target, when not the host.
    pub(crate) target: Option<String>,
    /// The arguments after `cargo rustc --`; the build is `cargo build` when
    /// there are none.
    pub(crate) rustc_args: Vec<String>,
    /// The variables Cargo runs with besides `VISO_PROFILE`.
    pub(crate) env: Vec<(String, OsString)>,
    /// What wraps the executable; it is the artifact as built when `None`.
    pub(crate) package: Option<Package>,
    /// The build cache's lock, taken for the build and waited for while
    /// another command holds it.
    pub(crate) lock: Option<(Layout, Target)>,
}

impl BuildCommand {
    /// Every argument of the Cargo command line: `cargo check` when
    /// `check`, which takes no `rustc` arguments.
    fn args(&self, check: bool) -> Vec<OsString> {
        let rustc = !check && !self.rustc_args.is_empty();
        let mut args: Vec<OsString> = vec![
            match (check, rustc) {
                (true, _) => "check",
                (false, true) => "rustc",
                (false, false) => "build",
            }
            .into(),
        ];
        args.extend(self.cargo_args.iter().cloned());
        args.extend(self.build_args.iter().cloned());
        args.push("--manifest-path".into());
        args.push(self.manifest.clone().into_os_string());
        args.extend(self.profile_args.iter().map(OsString::from));
        if self.no_default_features {
            args.push("--no-default-features".into());
        }
        if !self.features.is_empty() {
            args.extend(["--features".into(), self.features.join(",").into()]);
        }
        if let Some(target) = &self.target {
            args.extend(["--target".into(), target.into()]);
        }
        args.push("--message-format=json".into());
        if self.quiet {
            args.push("--quiet".into());
        }
        if rustc {
            // The arguments are the executable's, whatever else the package
            // builds.
            args.push("--bins".into());
            args.push("--".into());
            args.extend(self.rustc_args.iter().map(OsString::from));
        }
        args
    }
}

impl std::fmt::Debug for BuildCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildCommand")
            .field("cargo", &self.cargo)
            .field("manifest", &self.manifest)
            .field("profile", &self.profile)
            .field("target", &self.target)
            .field("packaged", &self.package.is_some())
            .finish_non_exhaustive()
    }
}

/// The Cargo profile a Viso profile builds with, and the settings Viso
/// gives it: `--profile` and a `--config` per setting that is not already
/// Cargo's own for that profile. `shipping` is a Cargo profile of Viso's,
/// inheriting `release`. A web build's debug info follows its web
/// settings, and its symbols are stripped by `wasm-bindgen`. An unoptimized
/// headless build still optimizes the crates that draw its frames in
/// software ([`RASTER`]), which would otherwise take most of a second each.
pub(crate) fn profile_args(
    profile: Profile,
    settings: &ProfileSettings,
    target: Target,
) -> Vec<String> {
    let web = target.is_web();
    let name = profile.as_str();
    let mut args = vec!["--profile".to_owned(), name.to_owned()];
    let mut config = |key: &str, value: String| {
        args.push("--config".to_owned());
        args.push(format!("profile.{name}.{key}={value}"));
    };
    if profile == Profile::Shipping {
        config("inherits", "\"release\"".to_owned());
    }
    // What Cargo's own `dev` and `release` profiles build with.
    let release = profile != Profile::Dev;
    let cargo_opt = if release {
        OptLevel::Three
    } else {
        OptLevel::Zero
    };
    let opt = settings.opt_level;
    if named(opt) || opt.value != cargo_opt {
        let value = match opt.value {
            OptLevel::Size => "\"s\"".to_owned(),
            level => level.as_str().to_owned(),
        };
        config("opt-level", value);
    }
    if target == Target::Headless && opt.value == OptLevel::Zero {
        for package in RASTER {
            config(&format!("package.{package}.opt-level"), "3".to_owned());
        }
    }
    let debug = if web {
        settings.web.source_maps
    } else {
        settings.source_maps
    };
    if named(debug) || debug.value == release {
        config("debug", debug.value.to_string());
    }
    let strip = settings.strip;
    if !web && (named(strip) || strip.value) {
        config("strip", strip.value.to_string());
    }
    args
}

/// The crates of a headless build's software raster: the backend, the CPU
/// shader programs it runs, and the renderer feeding it.
const RASTER: [&str; 3] = ["viso-gpu", "viso-shader", "viso-render"];

/// Whether a configuration layer named the value, rather than Viso's
/// default giving it.
fn named<T>(value: Sourced<T>) -> bool {
    value.origin != Origin::FrameworkDefault
}

/// Why a build gave no artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuildFailure {
    /// `cargo` did not run.
    Spawn(String),
    /// Cargo reported failure: its exit status and the codes of its errors,
    /// each once, in order.
    Failed { status: String, codes: Vec<String> },
    /// The package built no executable, or more than one.
    Executables(Vec<PathBuf>),
    /// The executable could not be packaged for its device.
    Package(String),
    /// The package could not be signed.
    Sign(String),
    /// A tool the packaging needs is not installed.
    Tool(String),
    /// The build's output could not be written.
    Write(String),
    /// The build cache's lock could not be taken.
    Lock(String),
    /// A newer change, or the command's end, killed it.
    Cancelled,
}

impl BuildFailure {
    /// What the failure reads as.
    pub(crate) fn message(&self) -> String {
        match self {
            BuildFailure::Spawn(why)
            | BuildFailure::Lock(why)
            | BuildFailure::Tool(why)
            | BuildFailure::Write(why) => why.clone(),
            BuildFailure::Failed { status, .. } => format!("`cargo build` failed ({status})"),
            BuildFailure::Executables(executables) if executables.is_empty() => {
                "the package builds no executable".to_owned()
            }
            BuildFailure::Executables(_) => {
                "the package builds more than one executable".to_owned()
            }
            BuildFailure::Package(why) => format!("cannot package the app: {why}"),
            BuildFailure::Sign(why) => format!("cannot sign the package: {why}"),
            BuildFailure::Cancelled => "the build was cancelled".to_owned(),
        }
    }
}

/// The `cargo` to run: `$CARGO`, else the one on the path.
pub(crate) fn program() -> PathBuf {
    std::env::var_os("CARGO").map_or_else(|| "cargo".into(), PathBuf::from)
}

/// Whether `cancelled` fired, or its sender is gone.
pub(crate) fn is_cancelled(cancelled: &mpsc::Receiver<()>) -> bool {
    cancelled.try_recv() != Err(mpsc::TryRecvError::Empty)
}

/// Runs the build until it ends or `cancelled` fires (or closes), handing
/// what Cargo says to `sink`; the artifact, packaged when the command
/// packages it.
pub(crate) fn build(
    command: &BuildCommand,
    cancelled: &mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<PathBuf, BuildFailure> {
    let _lock = match &command.lock {
        Some((layout, target)) => Some(wait_for_lock(layout, *target, cancelled, sink)?),
        None => None,
    };
    let messages = cargo(command, false, cancelled, sink)?;
    let exe = match <[PathBuf; 1]>::try_from(messages.executables) {
        Ok([exe]) => exe,
        Err(executables) => return Err(BuildFailure::Executables(executables)),
    };
    match &command.package {
        Some(package) => package(&exe).map_err(BuildFailure::Package),
        None => Ok(exe),
    }
}

/// Checks the app `command` builds with `cargo check`, compiling nothing it
/// would run, until it is done or `cancelled` fires; what Cargo says goes to
/// `sink` as for a build.
pub(crate) fn check(
    command: &BuildCommand,
    cancelled: &mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<(), BuildFailure> {
    cargo(command, true, cancelled, sink).map(|_| ())
}

/// Runs Cargo for `command` (`cargo check` when `check`) and reads its
/// messages to their end.
fn cargo(
    command: &BuildCommand,
    check: bool,
    cancelled: &mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<Messages, BuildFailure> {
    let mut cargo = Command::new(&command.cargo);
    cargo
        .args(command.args(check))
        .env("VISO_PROFILE", command.profile.as_str())
        .envs(command.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    if command.piped {
        cargo.stderr(Stdio::piped());
    }
    if let Some(dir) = &command.target_dir {
        cargo.env("CARGO_TARGET_DIR", dir);
    }
    let mut tree = ProcessTree::spawn(&mut cargo).map_err(|error| {
        BuildFailure::Spawn(format!("cannot run `{}`: {error}", command.cargo.display()))
    })?;
    // Cargo's JSON output closing says it is ending.
    let (closed, wait) = mpsc::channel();
    let messages = tree.child.stdout.take().map(|stdout| {
        let sink = command.piped.then(|| Arc::clone(sink));
        let manifest = command.manifest.clone();
        thread::spawn(move || {
            let messages = Messages::read(stdout, &manifest, sink.as_deref());
            let _ = closed.send(());
            messages
        })
    });
    let rendered = tree.child.stderr.take().map(|stderr| {
        let sink = Arc::clone(sink);
        thread::spawn(move || lines(stderr, &*sink))
    });
    let status = loop {
        if is_cancelled(cancelled) {
            tree.kill();
            break None;
        }
        match wait.recv_timeout(EXIT_POLL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => match tree.child.wait() {
                Ok(status) => break Some(status),
                Err(error) => {
                    tree.kill();
                    return Err(BuildFailure::Spawn(format!(
                        "cannot wait for `cargo`: {error}"
                    )));
                }
            },
            Err(RecvTimeoutError::Timeout) => {
                if let Ok(Some(status)) = tree.child.try_wait() {
                    break Some(status);
                }
            }
        }
    };
    let messages = messages
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    if let Some(thread) = rendered {
        let _ = thread.join();
    }
    let Some(status) = status else {
        return Err(BuildFailure::Cancelled);
    };
    if !status.success() {
        return Err(BuildFailure::Failed {
            status: status.to_string(),
            codes: messages.codes,
        });
    }
    Ok(messages)
}

/// Takes the build cache's lock of `target`, waiting while another command
/// holds it — said once to `sink` — until it is free or `cancelled` fires.
fn wait_for_lock(
    layout: &Layout,
    target: Target,
    cancelled: &mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<LockGuard, BuildFailure> {
    let kind = LockKind::BuildCache { target };
    let mut said = false;
    loop {
        match LockGuard::acquire(layout, kind) {
            Ok(lock) => return Ok(lock),
            Err(LockError::Held { holder, .. }) => {
                if !std::mem::replace(&mut said, true) {
                    sink(CargoEvent::Log {
                        level: "info",
                        line: format!(
                            "waiting for the {}, held by process {}",
                            kind.description(),
                            holder.pid
                        ),
                    });
                }
            }
            Err(error) => return Err(BuildFailure::Lock(error.diagnostic().message)),
        }
        let since = Instant::now();
        while since.elapsed() < LOCK_POLL {
            if is_cancelled(cancelled) {
                return Err(BuildFailure::Cancelled);
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Hands each line of `input` to `sink` as a log line.
fn lines(input: impl Read, sink: &(dyn Fn(CargoEvent) -> bool + Send + Sync)) {
    for line in BufReader::new(input).lines() {
        let Ok(line) = line else {
            return;
        };
        let level = cargo_level(&line);
        if !sink(CargoEvent::Log { level, line }) {
            return;
        }
    }
}

/// The level of a line of Cargo's rendered output.
pub(crate) fn cargo_level(line: &str) -> &'static str {
    if line.starts_with("error") {
        "error"
    } else if line.starts_with("warning") {
        "warn"
    } else {
        "info"
    }
}

/// A compiler diagnostic as Cargo reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustDiagnostic {
    /// `error` or `warning`.
    pub(crate) severity: &'static str,
    /// The error code or lint name, `rustc` when there is none.
    pub(crate) code: String,
    pub(crate) message: String,
    /// The primary span first, then each labeled secondary one.
    pub(crate) spans: Vec<RustSpan>,
    /// Each note and help, with its kind: `help: …`.
    pub(crate) notes: Vec<String>,
}

/// A range of a source a diagnostic points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustSpan {
    /// The file, as found on disk when it is.
    pub(crate) file: PathBuf,
    pub(crate) byte_start: usize,
    pub(crate) byte_end: usize,
    pub(crate) line: u32,
    pub(crate) column: u32,
    pub(crate) label: Option<String>,
}

impl RustDiagnostic {
    /// The diagnostic of Cargo's `message` object, its files found from
    /// `manifest`, the package's manifest: Rust names a file relative to the
    /// workspace root, which is the package's directory or one above it.
    pub(crate) fn read(message: &JsonValue, manifest: &Path) -> Option<RustDiagnostic> {
        let severity = match message.get("level").and_then(JsonValue::as_str)? {
            "error" | "error: internal compiler error" => "error",
            "warning" => "warning",
            _ => return None,
        };
        let text = |value: &JsonValue, key: &str| {
            value
                .get(key)
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        };
        let code = message
            .get("code")
            .and_then(|code| text(code, "code"))
            .unwrap_or_else(|| "rustc".to_owned());
        let mut spans: Vec<(bool, RustSpan)> = Vec::new();
        for span in message.get("spans").and_then(JsonValue::as_array)? {
            let primary = span.get("is_primary").and_then(JsonValue::as_bool) == Some(true);
            let label = text(span, "label");
            if !primary && label.is_none() {
                continue;
            }
            let number = |key: &str| span.get(key).and_then(JsonValue::as_f64).map(|n| n as u64);
            let Some(name) = text(span, "file_name") else {
                continue;
            };
            spans.push((
                primary,
                RustSpan {
                    file: locate(&name, manifest),
                    byte_start: number("byte_start").unwrap_or(0) as usize,
                    byte_end: number("byte_end").unwrap_or(0) as usize,
                    line: number("line_start").unwrap_or(1) as u32,
                    column: number("column_start").unwrap_or(1) as u32,
                    label,
                },
            ));
        }
        // The primary span leads; a diagnostic with none (one about the
        // crate) has no position.
        spans.sort_by_key(|(primary, _)| !primary);
        if spans.first().is_some_and(|(primary, _)| !primary) {
            spans.clear();
        }
        let notes = message
            .get("children")
            .and_then(JsonValue::as_array)
            .map(|children| {
                children
                    .iter()
                    .filter_map(|child| {
                        let level = text(child, "level")?;
                        let message = text(child, "message")?;
                        Some(format!("{level}: {message}"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(RustDiagnostic {
            severity,
            code,
            message: text(message, "message")?,
            spans: spans.into_iter().map(|(_, span)| span).collect(),
            notes,
        })
    }
}

/// The file Rust names `name`: as it is when absolute, else under the
/// nearest directory above `manifest` where it exists.
fn locate(name: &str, manifest: &Path) -> PathBuf {
    let path = Path::new(name);
    if path.is_absolute() {
        return path.to_owned();
    }
    manifest
        .ancestors()
        .skip(1)
        .map(|dir| dir.join(path))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| path.to_owned())
}

/// What Cargo's JSON messages said.
#[derive(Debug, Default, PartialEq)]
struct Messages {
    /// The executable of each artifact that has one.
    executables: Vec<PathBuf>,
    /// The code of each error the compiler reported, each once.
    codes: Vec<String>,
}

impl Messages {
    /// Reads Cargo's JSON messages from `stdout` to its end: each
    /// diagnostic handed to `sink` whole, or its text written to stderr
    /// without one.
    fn read(
        stdout: impl Read,
        manifest: &Path,
        sink: Option<&(dyn Fn(CargoEvent) -> bool + Send + Sync)>,
    ) -> Messages {
        let mut messages = Messages::default();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            let Some(diagnostic) = messages.take(&line) else {
                continue;
            };
            match sink {
                Some(sink) => {
                    if let Some(diagnostic) = RustDiagnostic::read(&diagnostic, manifest) {
                        sink(CargoEvent::Diagnostic(diagnostic));
                    }
                }
                None => {
                    if let Some(rendered) = diagnostic.get("rendered").and_then(JsonValue::as_str) {
                        let _ = std::io::stderr().lock().write_all(rendered.as_bytes());
                    }
                }
            }
        }
        messages
    }

    /// Takes one line of Cargo's JSON output; a compiler diagnostic gives
    /// its message object.
    fn take(&mut self, line: &str) -> Option<JsonValue> {
        let message = JsonValue::parse(line).ok()?;
        match message.get("reason").and_then(JsonValue::as_str)? {
            "compiler-artifact" => {
                if let Some(exe) = message.get("executable").and_then(JsonValue::as_str) {
                    self.executables.push(PathBuf::from(exe));
                }
                None
            }
            "compiler-message" => {
                let diagnostic = message.get("message")?;
                if diagnostic.get("level").and_then(JsonValue::as_str) == Some("error")
                    && let Some(code) = diagnostic
                        .get("code")
                        .and_then(|code| code.get("code"))
                        .and_then(JsonValue::as_str)
                    && !self.codes.iter().any(|c| c == code)
                {
                    self.codes.push(code.to_owned());
                }
                Some(diagnostic.clone())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use viso_project::{Env, Manifest};

    use super::*;

    #[test]
    fn rustc_arguments_go_to_the_executable_alone() {
        let mut command = BuildCommand {
            cargo: PathBuf::from("cargo"),
            manifest: PathBuf::from("/p/Cargo.toml"),
            quiet: false,
            piped: true,
            target_dir: None,
            cargo_args: Vec::new(),
            build_args: Vec::new(),
            profile: Profile::Release,
            profile_args: Vec::new(),
            features: Vec::new(),
            no_default_features: false,
            target: None,
            rustc_args: Vec::new(),
            env: Vec::new(),
            package: None,
            lock: None,
        };
        let args = |command: &BuildCommand, check| -> Vec<String> {
            command
                .args(check)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(args(&command, false)[0], "build");
        assert!(!args(&command, false).contains(&"--bins".to_owned()));
        command.rustc_args = vec!["-Clink-arg=/b/resources.res".to_owned()];
        let rustc = args(&command, false);
        assert_eq!(rustc[0], "rustc");
        assert_eq!(
            &rustc[rustc.len() - 3..],
            ["--bins", "--", "-Clink-arg=/b/resources.res"]
        );
        assert_eq!(args(&command, true)[0], "check");
        assert!(!args(&command, true).contains(&"--".to_owned()));
        assert!(!args(&command, true).contains(&"--no-default-features".to_owned()));
        command.no_default_features = true;
        command.features = vec!["viso/cpu", "viso/kms"];
        let embedded = args(&command, true);
        let at = embedded.iter().position(|a| a == "--no-default-features");
        assert_eq!(
            embedded[at.unwrap() + 1..at.unwrap() + 3],
            ["--features", "viso/cpu,viso/kms"]
        );
    }

    fn rendered(messages: &mut Messages, lines: &[&str]) -> Vec<String> {
        lines
            .iter()
            .filter_map(|line| messages.take(line))
            .filter_map(|d| {
                d.get("rendered")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
            })
            .collect()
    }

    #[test]
    fn cargo_messages_give_the_executable_and_the_error_codes() {
        let mut messages = Messages::default();
        let texts = rendered(
            &mut messages,
            &[
                r#"{"reason":"compiler-artifact","executable":null,"fresh":true}"#,
                r#"{"reason":"compiler-artifact","executable":"/t/debug/my \"app\"é","fresh":false}"#,
                r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308","explanation":"…"},"rendered":"error[E0308]: mismatched types\n"}}"#,
                r#"{"reason":"compiler-message","message":{"level":"warning","code":{"code":"W1"},"rendered":"warning: unused\n"}}"#,
                r#"{"reason":"compiler-message","message":{"level":"error","code":null}}"#,
                r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"}}}"#,
                r#"{"reason":"build-finished","success":false}"#,
                "not json",
            ],
        );
        assert_eq!(
            texts,
            ["error[E0308]: mismatched types\n", "warning: unused\n"]
        );
        assert_eq!(
            messages,
            Messages {
                executables: vec![PathBuf::from("/t/debug/my \"app\"é")],
                codes: vec!["E0308".into()],
            }
        );
    }

    #[test]
    fn a_diagnostic_keeps_its_spans_notes_and_files() {
        let dir = std::env::temp_dir().join(format!("viso-rust-diag-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("app/src")).unwrap();
        std::fs::write(dir.join("app/src/main.rs"), "fn main() {}\n").unwrap();
        let manifest = dir.join("app/Cargo.toml");
        let message = JsonValue::parse(
            r#"{"level":"error","code":{"code":"E0308"},"message":"mismatched types",
            "spans":[
              {"file_name":"src/other.rs","is_primary":false,"label":null,"byte_start":0,"byte_end":1,"line_start":1,"column_start":1},
              {"file_name":"/abs/lib.rs","is_primary":false,"label":"expected because of this","byte_start":4,"byte_end":6,"line_start":2,"column_start":3},
              {"file_name":"src/main.rs","is_primary":true,"label":"expected `u8`","byte_start":3,"byte_end":7,"line_start":1,"column_start":4}],
            "children":[{"level":"help","message":"try a number","spans":[]}],
            "rendered":"…"}"#,
        )
        .unwrap();
        let diagnostic = RustDiagnostic::read(&message, &manifest).unwrap();
        assert_eq!(diagnostic.severity, "error");
        assert_eq!(diagnostic.code, "E0308");
        assert_eq!(diagnostic.notes, ["help: try a number"]);
        let files: Vec<&Path> = diagnostic.spans.iter().map(|s| s.file.as_path()).collect();
        assert_eq!(
            files,
            [
                dir.join("app/src/main.rs").as_path(),
                Path::new("/abs/lib.rs")
            ]
        );
        assert_eq!(diagnostic.spans[0].byte_start, 3);
        assert_eq!(diagnostic.spans[0].label.as_deref(), Some("expected `u8`"));

        // A lint without a code, about no place.
        let lint = JsonValue::parse(
            r#"{"level":"warning","code":null,"message":"2 warnings emitted","spans":[],"children":[]}"#,
        )
        .unwrap();
        let lint = RustDiagnostic::read(&lint, &manifest).unwrap();
        assert_eq!((lint.code.as_str(), lint.spans.len()), ("rustc", 0));
        let note = JsonValue::parse(r#"{"level":"note","message":"x","spans":[]}"#).unwrap();
        assert!(RustDiagnostic::read(&note, &manifest).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn settings(profile: Profile, manifest: &str) -> ProfileSettings {
        let viso_project::ParseOutcome::Parsed { manifest, .. } =
            Manifest::parse(Path::new("Viso.toml"), manifest)
        else {
            panic!("the manifest parses")
        };
        let mut errors = Vec::new();
        let settings = ProfileSettings::resolve(&manifest, profile, &Env::empty(), &mut errors);
        assert!(errors.is_empty());
        settings
    }

    #[test]
    fn a_profile_passes_only_what_differs_from_cargo() {
        let plain = "[package]\nname = \"app\"\n";
        assert_eq!(
            profile_args(Profile::Dev, &settings(Profile::Dev, plain), Target::Host),
            ["--profile", "dev"]
        );
        assert_eq!(
            profile_args(
                Profile::Release,
                &settings(Profile::Release, plain),
                Target::Host
            ),
            ["--profile", "release"]
        );
        assert_eq!(
            profile_args(
                Profile::Shipping,
                &settings(Profile::Shipping, plain),
                Target::Host
            ),
            [
                "--profile",
                "shipping",
                "--config",
                "profile.shipping.inherits=\"release\"",
                "--config",
                "profile.shipping.opt-level=\"s\"",
                "--config",
                "profile.shipping.strip=true",
            ]
        );
        // A web build strips with its glue, not with Cargo.
        assert_eq!(
            profile_args(
                Profile::Shipping,
                &settings(Profile::Shipping, plain),
                Target::WebGpu
            )[4..],
            ["--config", "profile.shipping.opt-level=\"s\""]
        );
        // An unoptimized headless build optimizes its software raster.
        assert_eq!(
            profile_args(
                Profile::Dev,
                &settings(Profile::Dev, plain),
                Target::Headless
            ),
            [
                "--profile",
                "dev",
                "--config",
                "profile.dev.package.viso-gpu.opt-level=3",
                "--config",
                "profile.dev.package.viso-shader.opt-level=3",
                "--config",
                "profile.dev.package.viso-render.opt-level=3",
            ]
        );
        assert_eq!(
            profile_args(
                Profile::Release,
                &settings(Profile::Release, plain),
                Target::Headless
            ),
            ["--profile", "release"]
        );
        let named = "[package]\nname = \"app\"\n[profile.release]\nopt_level = 3\nsource_maps = true\n\
                     [profile.release.web]\nsource_maps = false\n";
        let release = settings(Profile::Release, named);
        assert_eq!(
            profile_args(Profile::Release, &release, Target::Host),
            [
                "--profile",
                "release",
                "--config",
                "profile.release.opt-level=3",
                "--config",
                "profile.release.debug=true",
            ]
        );
        assert_eq!(
            profile_args(Profile::Release, &release, Target::WebGpu),
            [
                "--profile",
                "release",
                "--config",
                "profile.release.opt-level=3",
                "--config",
                "profile.release.debug=false",
            ]
        );
    }
}
