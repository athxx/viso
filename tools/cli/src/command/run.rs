//! `viso run` (`Viso_CLI.md` section 13) on the desktop host: take the dev
//! session's lock, start its watcher, build the project's Cargo package as a
//! dev artifact, launch it with the dev channel's loopback address and session
//! identity, and run the dev session (`crate::dev`) until the app exits: each
//! batch of edits is compiled on the host, reported as `diagnostic` and `dev`
//! events, and what compiles is patched into the running app.
//!
//! The app connects back to a listener bound to `127.0.0.1` on a free port and
//! must open with a hello carrying the token it was launched with; a connection
//! that does not is dropped. Under `--json` the app's stdout and stderr, and the
//! build's stderr, reach the stream only as `log` events (section 37).

use std::collections::hash_map::RandomState;
use std::ffi::OsString;
use std::hash::{BuildHasher, Hasher as _};
use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use viso_project::{
    ArtifactKind, Layout, LockGuard, LockKind, Overrides, Profile, ProjectFingerprint, Target,
    Toolchain,
};
use viso_view::dev::wire::{
    self, DEV_BUILD_ENV, DEV_RUNTIME_ENV, DEV_SESSION_ENV, DEV_TOKEN_ENV, DevSessionId,
    ProjectFingerprint as WireProject, SchemaFingerprint,
};

use super::check::{config_in_its_file, failure_code};
use super::{
    BUILD, BUILD_FAILED, ENV_CARGO, ENV_CARGO_MANIFEST, ENV_CURRENT_DIR, ENV_DEV_CHANNEL,
    ENV_NO_EXECUTABLE, ENVIRONMENT, RUN_APP_FAILED, RUNTIME, SUCCESS,
};
use crate::args::{Global, RunArgs};
use crate::dev::{self, Deliver, DevSession, Expect, LinkEvent};
use crate::output::Output;

/// How long the session waits for a message before it polls the app and the
/// listener again.
const POLL: Duration = Duration::from_millis(50);

/// What the session's threads hand the main loop.
enum Incoming {
    /// A line the build or the app wrote.
    Log {
        level: &'static str,
        source: &'static str,
        line: String,
    },
    /// The executable a build artifact message named.
    Executable(PathBuf),
    /// A file of the watch scope changed.
    Change(dev::Change),
    /// What a dev connection delivered.
    Link(LinkEvent),
}

pub fn run(global: &Global, args: &RunArgs, out: &mut Output) -> u8 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            out.failure(
                ENV_CURRENT_DIR,
                &format!("cannot read the current directory: {error}"),
                &[],
            );
            return ENVIRONMENT;
        }
    };
    // `viso run` always builds the host dev artifact (sections 13.1 and 13.6).
    let flags = Overrides {
        target: Some(Target::Host),
        profile: Some(Profile::Dev),
        ..Overrides::default()
    };
    let (project, resolved) =
        match viso_project::load(global.project.as_deref(), &cwd, ArtifactKind::Build, &flags) {
            Ok(loaded) => loaded,
            Err(diagnostics) => {
                for diagnostic in &diagnostics {
                    config_in_its_file(out, diagnostic);
                }
                return failure_code(&diagnostics);
            }
        };
    for warning in &resolved.warnings {
        config_in_its_file(out, warning);
    }
    let root = project.root.clone();
    let cargo_manifest = root.join("Cargo.toml");
    if !cargo_manifest.is_file() {
        out.failure(
            ENV_CARGO_MANIFEST,
            &format!(
                "no `Cargo.toml` next to `{}`",
                project.manifest_path().display()
            ),
            &["`viso run` builds the Cargo package at the project root".to_string()],
        );
        return ENVIRONMENT;
    }

    // One dev session per project and target at a time (§4.1).
    let lock = match LockGuard::acquire(
        &Layout::new(&root),
        LockKind::DevSession {
            target: Target::Host,
            device: None,
        },
    ) {
        Ok(lock) => lock,
        Err(error) => {
            let diagnostic = error.diagnostic();
            out.config(None, &diagnostic);
            return diagnostic.code.exit_code();
        }
    };
    let fingerprint = match ProjectFingerprint::compute(&root) {
        Ok(fingerprint) => fingerprint,
        Err(diagnostic) => {
            config_in_its_file(out, &diagnostic);
            return diagnostic.code.exit_code();
        }
    };
    let build_identity = resolved.config.build_id(fingerprint, &toolchain());
    let build_id = build_identity.to_hex();
    let token = random_hex();
    let session = random_hex();
    let wide = |hash: viso_project::Hash128| (u128::from(hash.hi) << 64) | u128::from(hash.lo);
    let expect = Expect {
        token: token.clone(),
        dev_session: DevSessionId::from_hex(&session).expect("32 hex digits"),
        build_id: wire::BuildId(wide(build_identity.hash())),
        project: WireProject(wide(fingerprint.hash())),
        schema: SchemaFingerprint(viso_dsl::hotreload::schema_fingerprint()),
    };

    // The watcher starts before the build, so an edit saved while the app
    // builds reaches the session.
    let (tx, rx) = mpsc::channel();
    let watched = tx.clone();
    let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
    let mut dev = DevSession::start(
        canonical,
        &expect,
        lock,
        !args.no_hot_reload,
        move |change| watched.send(Incoming::Change(change)).is_ok(),
    );

    out.progress("build", "building the dev artifact");
    let exe = match build(global, &cargo_manifest, out) {
        Ok(exe) => exe,
        Err(code) => return code,
    };

    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.set_nonblocking(true).map(|()| listener))
    {
        Ok(listener) => listener,
        Err(error) => {
            out.failure(
                ENV_DEV_CHANNEL,
                &format!("cannot open the dev channel on the loopback interface: {error}"),
                &[],
            );
            return ENVIRONMENT;
        }
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => {
            out.failure(
                ENV_DEV_CHANNEL,
                &format!("cannot read the dev channel's address: {error}"),
                &[],
            );
            return ENVIRONMENT;
        }
    };
    out.progress(
        "launch",
        &format!("launching `{}`", relative(&root, &exe).display()),
    );
    let mut launch = Command::new(&exe);
    launch
        .args(&args.app_args)
        .current_dir(&root)
        .env(DEV_RUNTIME_ENV, address.to_string())
        .env(DEV_TOKEN_ENV, &token)
        .env(DEV_SESSION_ENV, &session)
        .env(DEV_BUILD_ENV, &build_id);
    if global.json {
        launch.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let mut child = match launch.spawn() {
        Ok(child) => child,
        Err(error) => {
            out.failure(
                RUN_APP_FAILED,
                &format!("cannot launch `{}`: {error}", exe.display()),
                &[],
            );
            return RUNTIME;
        }
    };

    let mut threads = pipe_logs(&mut child, &tx);
    let deliver: Deliver = {
        let tx = tx.clone();
        Arc::new(move |event| tx.send(Incoming::Link(event)).is_ok())
    };
    let status = loop {
        dev::link::accept(&listener, &expect, &deliver, &mut threads);
        let wait = dev.deadline().map_or(POLL, |due| {
            due.saturating_duration_since(Instant::now()).min(POLL)
        });
        match rx.recv_timeout(wait) {
            Ok(message) => relay(out, &mut dev, message),
            Err(RecvTimeoutError::Timeout) => {}
            // The loop holds a sender, so the channel cannot close.
            Err(RecvTimeoutError::Disconnected) => {}
        }
        dev.tick(out);
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => break Err(error),
        }
    };
    // The app is gone: a connection it opened is waiting in the listener's
    // backlog or being read to its end, and its pipes are at their end.
    dev::link::accept(&listener, &expect, &deliver, &mut threads);
    drop(deliver);
    drop(tx);
    for thread in threads {
        let _ = thread.join();
    }
    while let Ok(message) = rx.try_recv() {
        relay(out, &mut dev, message);
    }
    exit_code(out, status)
}

/// Builds the dev artifact of the Cargo package at `manifest` and returns its
/// executable, or the exit code of the failure it reported.
fn build(global: &Global, manifest: &Path, out: &mut Output) -> Result<PathBuf, u8> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(&cargo);
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(manifest)
        .args(["--features", "viso/hot-reload"])
        // The facade's build script refuses `hot-reload` in a release or
        // shipping artifact; this one is a dev artifact.
        .env("VISO_PROFILE", Profile::Dev.as_str())
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::piped());
    if global.quiet {
        command.arg("--quiet");
    }
    if global.json {
        command.stderr(Stdio::piped());
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            out.failure(
                ENV_CARGO,
                &format!("cannot run `{}`: {error}", cargo.to_string_lossy()),
                &[],
            );
            return Err(ENVIRONMENT);
        }
    };
    let (tx, rx) = mpsc::channel();
    let mut threads = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        threads.push(artifact_reader(stdout, tx.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        threads.push(line_reader(stderr, "tool", cargo_level, tx.clone()));
    }
    drop(tx);
    let mut executables = Vec::new();
    for message in rx {
        match message {
            Incoming::Executable(exe) => executables.push(exe),
            Incoming::Log {
                level,
                source,
                line,
            } => out.log(level, source, &line),
            Incoming::Change(_) | Incoming::Link(_) => {}
        }
    }
    for thread in threads {
        let _ = thread.join();
    }
    match child.wait() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            out.failure(
                BUILD_FAILED,
                &format!("`cargo build` failed ({status})"),
                &[],
            );
            return Err(BUILD);
        }
        Err(error) => {
            out.failure(
                ENV_CARGO,
                &format!("cannot wait for `cargo build`: {error}"),
                &[],
            );
            return Err(ENVIRONMENT);
        }
    }
    match <[PathBuf; 1]>::try_from(executables) {
        Ok([exe]) => Ok(exe),
        Err(executables) => {
            let message = if executables.is_empty() {
                "the package builds no executable".to_string()
            } else {
                "the package builds more than one executable".to_string()
            };
            let notes = executables
                .iter()
                .map(|exe| format!("built `{}`", exe.display()))
                .collect::<Vec<_>>();
            out.failure(ENV_NO_EXECUTABLE, &message, &notes);
            Err(ENVIRONMENT)
        }
    }
}

/// The compiler identity of the build: `rustc -V`, or `unknown` when it does
/// not run.
fn toolchain() -> Toolchain {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let version = Command::new(rustc)
        .arg("-V")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |v| v.trim().to_string());
    Toolchain::new(version, Toolchain::current_host())
}

/// 128 bits from the process's randomly keyed hasher, as 32 hex digits. The
/// keys come from the operating system's random source; the hash is not a
/// cryptographic generator, which a loopback session token does not need.
fn random_hex() -> String {
    let state = RandomState::new();
    let word = |salt: u8| {
        let mut hasher = state.build_hasher();
        hasher.write_u8(salt);
        hasher.finish()
    };
    format!("{:016x}{:016x}", word(0), word(1))
}

/// Starts a thread per piped output of the app, turning each line into a `log`
/// message.
fn pipe_logs(child: &mut Child, tx: &Sender<Incoming>) -> Vec<JoinHandle<()>> {
    let mut threads = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        threads.push(line_reader(stdout, "app", |_| "info", tx.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        threads.push(line_reader(stderr, "app", |_| "warn", tx.clone()));
    }
    threads
}

/// Starts a thread that sends each line of `input` as a `log` message from
/// `source`, at the level `level` gives it.
fn line_reader(
    input: impl Read + Send + 'static,
    source: &'static str,
    level: fn(&str) -> &'static str,
    tx: Sender<Incoming>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(input).lines() {
            let Ok(line) = line else {
                return;
            };
            let level = level(&line);
            if tx
                .send(Incoming::Log {
                    level,
                    source,
                    line,
                })
                .is_err()
            {
                return;
            }
        }
    })
}

/// The level of a line of Cargo's rendered output.
fn cargo_level(line: &str) -> &'static str {
    if line.starts_with("error") {
        "error"
    } else if line.starts_with("warning") {
        "warn"
    } else {
        "info"
    }
}

/// Starts a thread that reads Cargo's JSON messages and sends the executable of
/// each artifact that has one.
fn artifact_reader(stdout: ChildStdout, tx: Sender<Incoming>) -> JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                return;
            };
            if let Some(exe) = artifact_executable(&line)
                && tx.send(Incoming::Executable(exe)).is_err()
            {
                return;
            }
        }
    })
}

/// The `executable` of a `compiler-artifact` message, when it is not `null`.
fn artifact_executable(line: &str) -> Option<PathBuf> {
    if !line.contains(r#""reason":"compiler-artifact""#) {
        return None;
    }
    let at = line.find(r#""executable":"#)? + r#""executable":"#.len();
    let value = line[at..].trim_start();
    let value = value.strip_prefix('"')?;
    json_string(value).map(PathBuf::from)
}

/// The JSON string whose opening quote precedes `text`, unescaped.
fn json_string(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = text.chars();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let first = hex_unit(&mut chars)?;
                    let units = if (0xd800..0xdc00).contains(&first) {
                        if (chars.next()?, chars.next()?) != ('\\', 'u') {
                            return None;
                        }
                        vec![first, hex_unit(&mut chars)?]
                    } else {
                        vec![first]
                    };
                    out.push_str(&String::from_utf16(&units).ok()?);
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
}

/// The UTF-16 unit of the four hex digits `chars` starts with.
fn hex_unit(chars: &mut std::str::Chars<'_>) -> Option<u16> {
    let mut unit = 0u16;
    for _ in 0..4 {
        unit = unit << 4 | chars.next()?.to_digit(16)? as u16;
    }
    Some(unit)
}

/// Hands one message of the running session to where it goes.
fn relay(out: &mut Output, dev: &mut DevSession, message: Incoming) {
    match message {
        Incoming::Log {
            level,
            source,
            line,
        } => out.log(level, source, &line),
        Incoming::Change(change) => dev.change(change, out),
        Incoming::Link(event) => dev.link(event, out),
        Incoming::Executable(_) => {}
    }
}

/// The exit code of an app that ended with `status`: 0 for a normal exit, 5
/// for a crash or a non-zero exit (section 13.9).
fn exit_code(out: &mut Output, status: std::io::Result<ExitStatus>) -> u8 {
    match status {
        Ok(status) if status.success() => SUCCESS,
        Ok(status) => {
            out.failure(
                RUN_APP_FAILED,
                &format!("the app exited with {status}"),
                &[],
            );
            RUNTIME
        }
        Err(error) => {
            out.failure(
                RUN_APP_FAILED,
                &format!("cannot wait for the app: {error}"),
                &[],
            );
            RUNTIME
        }
    }
}

fn relative<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use viso_view::dev::wire::same_token;

    use super::*;

    #[test]
    fn an_artifact_message_names_its_executable() {
        let bin = concat!(
            r#"{"reason":"compiler-artifact","package_id":"app 0.1.0","#,
            r#""filenames":["/t/debug/app"],"executable":"/t/debug/my \"app\"é","fresh":false}"#,
        );
        assert_eq!(
            artifact_executable(bin),
            Some(PathBuf::from("/t/debug/my \"app\"é"))
        );
        let lib = r#"{"reason":"compiler-artifact","executable":null,"fresh":true}"#;
        assert_eq!(artifact_executable(lib), None);
        let done = r#"{"reason":"build-finished","success":true}"#;
        assert_eq!(artifact_executable(done), None);
        assert_eq!(json_string(r#"😀""#).as_deref(), Some("😀"));
        assert_eq!(json_string(r#"\ud83d""#), None);
        assert_eq!(json_string("unterminated"), None);
    }

    #[test]
    fn tokens_are_distinct_hex() {
        let a = random_hex();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, random_hex());
        assert!(same_token(&a, &a.clone()));
        assert!(!same_token(&a, &random_hex()));
        assert!(!same_token("ab", "abc"));
    }
}
