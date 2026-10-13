//! `viso run` (`Viso_CLI.md` section 13) on the desktop host: take the dev
//! session's lock, read the Rust scope from Cargo, start the watcher, build
//! the project's Cargo package as a dev artifact, launch it with the dev
//! channel's loopback address and session identity, and run the dev session
//! (`crate::dev`) until the app exits: each batch of edits is compiled on the
//! host, reported as `diagnostic` and `dev` events, and what compiles is
//! patched into the running app. A Rust edit rebuilds the artifact in the
//! background while the app keeps running; once the build succeeds the app
//! is asked for its snapshot, asked to close, and the new executable is
//! launched to restore it (a warm restart).
//!
//! Ctrl-C is the command's own: the first kills the build and asks the app to
//! close, a second kills the app, and the command exits 130.
//!
//! The app connects back to a listener bound to `127.0.0.1` on a free port and
//! must open with a hello carrying the token it was launched with; a connection
//! that does not is dropped. Under `--json` the app's stdout and stderr, and the
//! build's stderr, reach the stream only as `log` events (section 37).
//!
//! A second loopback listener, the control endpoint, is named with its own
//! token in the session lock: `viso game peek` and `viso game record` reach
//! the running app through it (section 22.3).

use std::collections::hash_map::RandomState;
use std::ffi::OsString;
use std::hash::{BuildHasher, Hasher as _};
use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use viso_project::{
    ArtifactKind, Layout, LockGuard, LockKind, Overrides, Profile, ProjectFingerprint, Target,
    Toolchain,
};
use viso_view::dev::wire::{
    self, DEV_BUILD_ENV, DEV_RESTORE_ENV, DEV_RUNTIME_ENV, DEV_SESSION_ENV, DEV_TOKEN_ENV,
    DevSessionId, ProjectFingerprint as WireProject, SchemaFingerprint,
};

use super::check::{config_in_its_file, failure_code};
use super::{
    BUILD, BUILD_FAILED, ENV_CARGO, ENV_CARGO_MANIFEST, ENV_CURRENT_DIR, ENV_DEV_CHANNEL,
    ENV_NO_EXECUTABLE, ENVIRONMENT, INTERRUPTED, RUN_APP_FAILED, RUNTIME, SUCCESS,
};
use crate::args::{Global, RunArgs};
use crate::dev::build::{BuildCommand, BuildEvent, BuildFailure, RustBuild};
use crate::dev::cargo::RustScope;
use crate::dev::{
    self, Control, ControlCall, ControlDeliver, Deliver, DevSession, Expect, LinkEvent, WatchScope,
    build_failure,
};
use crate::interrupt;
use crate::output::Output;

/// How long the session waits for a message before it polls the app and the
/// listener again.
const POLL: Duration = Duration::from_millis(50);

/// How long a warm restart waits for the old app's snapshot, then for it to
/// close before it is killed.
const SNAPSHOT_WAIT: Duration = Duration::from_secs(3);
const CLOSE_WAIT: Duration = Duration::from_secs(3);

/// Where a warm restart onto `exe` is.
enum Restart {
    /// The old app was asked for its snapshot at `since`.
    Snapshot { exe: PathBuf, since: Instant },
    /// The old app was asked to close at `since`.
    Closing { exe: PathBuf, since: Instant },
}

impl Restart {
    fn exe_mut(&mut self) -> &mut PathBuf {
        match self {
            Restart::Snapshot { exe, .. } | Restart::Closing { exe, .. } => exe,
        }
    }
}

/// How the app is launched: the first time and on each warm restart.
struct Launch<'a> {
    root: &'a Path,
    args: &'a [OsString],
    address: String,
    token: &'a str,
    session: &'a str,
    build_id: &'a str,
    piped: bool,
}

impl Launch<'_> {
    /// Starts `exe`, to restore the snapshot of the app before it when
    /// `restore`.
    fn spawn(&self, exe: &Path, restore: bool) -> std::io::Result<Child> {
        let mut launch = Command::new(exe);
        launch
            .args(self.args)
            .current_dir(self.root)
            .env(DEV_RUNTIME_ENV, &self.address)
            .env(DEV_TOKEN_ENV, self.token)
            .env(DEV_SESSION_ENV, self.session)
            .env(DEV_BUILD_ENV, self.build_id);
        if restore {
            launch.env(DEV_RESTORE_ENV, "1");
        }
        if self.piped {
            launch.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        launch.spawn()
    }
}

/// What the session's threads hand the main loop.
enum Incoming {
    /// A line the build or the app wrote.
    Log {
        level: &'static str,
        source: &'static str,
        line: String,
    },
    /// What a build of the dev artifact said.
    Build(BuildEvent),
    /// A file of the watch scope changed.
    Change(dev::Change),
    /// What a dev connection delivered.
    Link(LinkEvent),
    /// What `viso game` asked through the control endpoint.
    Control(ControlCall),
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

    interrupt::catch();
    // The Rust the artifact is built from, as Cargo resolves it; without it
    // the session still runs, and a Rust edit needs `viso run` again.
    let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
    let manifest = canonical.join("Cargo.toml");
    let rust = RustScope::read(&manifest).unwrap_or_else(|why| {
        out.log(
            "warn",
            "tool",
            &format!("Rust edits do not rebuild the app: {why}"),
        );
        RustScope::default()
    });
    // The watcher starts before the build, so an edit saved while the app
    // builds reaches the session.
    let (tx, rx) = mpsc::channel();
    let watched = tx.clone();
    let scope = WatchScope {
        project: canonical,
        rust,
    };
    let mut dev = DevSession::start(scope, &expect, lock, !args.no_hot_reload, move |change| {
        watched.send(Incoming::Change(change)).is_ok()
    });
    // `viso game` finds the endpoint in the lock; a session without one
    // still runs, and says why `viso game` cannot reach it.
    let control = Control::bind(random_hex())
        .and_then(|control| Ok((control.endpoint()?, control)))
        .map_err(|error| error.to_string())
        .and_then(|(endpoint, control)| {
            dev.lock_mut()
                .publish(&endpoint)
                .map(|()| control)
                .map_err(|diagnostic| diagnostic.message)
        });
    let control = match control {
        Ok(control) => Some(control),
        Err(why) => {
            out.log(
                "warn",
                "tool",
                &format!("`viso game` cannot reach this session: {why}"),
            );
            None
        }
    };

    let built = Mutex::new(tx.clone());
    let mut builds = RustBuild::new(
        BuildCommand {
            cargo: std::env::var_os("CARGO").map_or_else(|| "cargo".into(), PathBuf::from),
            manifest,
            quiet: global.quiet,
            piped: global.json,
            target_dir: None,
        },
        move |event| {
            built
                .lock()
                .is_ok_and(|tx| tx.send(Incoming::Build(event)).is_ok())
        },
    );
    out.progress("build", "building the dev artifact");
    builds.start();
    let exe = match launch_build(out, &mut dev, &mut builds, &rx) {
        Ok(exe) => exe,
        Err(code) => return code,
    };
    builds.launched(&exe);

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
    let launch = Launch {
        root: &root,
        args: &args.app_args,
        address: address.to_string(),
        token: &token,
        session: &session,
        build_id: &build_id,
        piped: global.json,
    };
    let mut child = match launch.spawn(&exe, false) {
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
    let ask: ControlDeliver = {
        let tx = tx.clone();
        Arc::new(move |call| tx.send(Incoming::Control(call)).is_ok())
    };
    let mut asking = Vec::new();
    // The interrupts acted on: the first stops the session, the second kills
    // the app.
    let mut interrupts = 0;
    let mut restart: Option<Restart> = None;
    let status = loop {
        dev::link::accept(&listener, &expect, &deliver, &mut threads);
        if let Some(control) = &control {
            control.accept(&ask, &mut asking);
        }
        let caught = interrupt::count();
        if caught > interrupts {
            if interrupts == 0 {
                out.progress("cancel", "stopping the session");
                builds.stop();
                restart = None;
                close(&child);
            }
            if caught >= 2 {
                let _ = child.kill();
            }
            interrupts = caught;
        }
        let wait = [dev.deadline(), builds.deadline()]
            .into_iter()
            .flatten()
            .min()
            .map_or(POLL, |due| {
                due.saturating_duration_since(Instant::now()).min(POLL)
            });
        match rx.recv_timeout(wait) {
            Ok(message) => {
                if let Some(exe) = relay(out, &mut dev, &mut builds, message)
                    && interrupts == 0
                {
                    match &mut restart {
                        // A newer build while the restart runs: the app
                        // restarts on it.
                        Some(restart) => *restart.exe_mut() = exe,
                        None => {
                            out.progress("restart", "taking the app's snapshot");
                            let since = Instant::now();
                            restart = Some(if dev.request_snapshot() {
                                Restart::Snapshot { exe, since }
                            } else {
                                close(&child);
                                Restart::Closing { exe, since }
                            });
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The loop holds a sender, so the channel cannot close.
            Err(RecvTimeoutError::Disconnected) => {}
        }
        dev.tick(out);
        if interrupts == 0 {
            builds.tick(Instant::now());
        }
        restart = match restart.take() {
            Some(Restart::Snapshot { exe, since })
                if dev.has_snapshot() || since.elapsed() >= SNAPSHOT_WAIT =>
            {
                close(&child);
                Some(Restart::Closing {
                    exe,
                    since: Instant::now(),
                })
            }
            Some(Restart::Closing { exe, since }) => {
                if since.elapsed() >= CLOSE_WAIT {
                    let _ = child.kill();
                }
                Some(Restart::Closing { exe, since })
            }
            other => other,
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                // The old app closed for the restart: the new one starts.
                let Some(restart) = restart.take().filter(|_| interrupts == 0) else {
                    break Ok(status);
                };
                let exe = match restart {
                    Restart::Snapshot { exe, .. } | Restart::Closing { exe, .. } => exe,
                };
                let restore = dev.restore_next();
                out.progress(
                    "launch",
                    &format!("restarting `{}`", relative(&root, &exe).display()),
                );
                match launch.spawn(&exe, restore) {
                    Ok(next) => {
                        child = next;
                        threads.extend(pipe_logs(&mut child, &tx));
                        builds.launched(&exe);
                        dev.relaunched(out);
                    }
                    Err(error) => {
                        dev.restart_failed(
                            &format!("cannot launch `{}`: {error}", exe.display()),
                            out,
                        );
                        break Ok(status);
                    }
                }
            }
            Ok(None) => {}
            Err(error) => break Err(error),
        }
    };
    // The session's end kills a build still running.
    builds.stop();
    // The app is gone: a connection it opened is waiting in the listener's
    // backlog or being read to its end, and its pipes are at their end.
    dev::link::accept(&listener, &expect, &deliver, &mut threads);
    drop(deliver);
    drop(tx);
    for thread in threads {
        let _ = thread.join();
    }
    while let Ok(message) = rx.try_recv() {
        let _ = relay(out, &mut dev, &mut builds, message);
    }
    // A question that arrives now finds the queue gone and is refused.
    drop(ask);
    drop(rx);
    for thread in asking {
        let _ = thread.join();
    }
    if interrupts > 0 {
        return INTERRUPTED;
    }
    exit_code(out, status)
}

/// Waits for the launch build, which a Rust edit while it runs starts again,
/// and returns its executable, or the exit code of the failure it reported.
/// The edits of the other domains wait for the app.
fn launch_build(
    out: &mut Output,
    dev: &mut DevSession,
    builds: &mut RustBuild,
    rx: &Receiver<Incoming>,
) -> Result<PathBuf, u8> {
    loop {
        if interrupt::count() > 0 {
            return Err(INTERRUPTED);
        }
        let wait = builds.deadline().map_or(POLL, |due| {
            due.saturating_duration_since(Instant::now()).min(POLL)
        });
        match rx.recv_timeout(wait) {
            Ok(Incoming::Build(BuildEvent::Finished(finished))) => {
                let Some(build) = builds.finished(finished) else {
                    continue;
                };
                return build.result.map_err(|failure| {
                    let code = match &failure {
                        BuildFailure::Spawn(_) => ENV_CARGO,
                        BuildFailure::Failed { .. } | BuildFailure::Cancelled => BUILD_FAILED,
                        BuildFailure::Executables(_) => ENV_NO_EXECUTABLE,
                    };
                    let notes = match &failure {
                        BuildFailure::Executables(executables) => executables
                            .iter()
                            .map(|exe| format!("built `{}`", exe.display()))
                            .collect(),
                        _ => Vec::new(),
                    };
                    out.failure(code, &build_failure(&failure), &notes);
                    if code == BUILD_FAILED {
                        BUILD
                    } else {
                        ENVIRONMENT
                    }
                });
            }
            Ok(message) => {
                let _ = relay(out, dev, builds, message);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {}
        }
        builds.tick(Instant::now());
    }
}

/// Asks the app to close: a termination request, which it handles as a
/// close of its windows.
fn close(child: &Child) {
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: a plain call; the child is not yet reaped, so its id names
        // it.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    // The console's Ctrl-C reaches the app itself.
    #[cfg(not(unix))]
    let _ = child;
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

/// Hands one message of the running session to where it goes; the executable
/// of a build the app is to restart on.
fn relay(
    out: &mut Output,
    dev: &mut DevSession,
    builds: &mut RustBuild,
    message: Incoming,
) -> Option<PathBuf> {
    match message {
        Incoming::Log {
            level,
            source,
            line,
        } => out.log(level, source, &line),
        Incoming::Change(change) if change.rust => builds.change(change.path, Instant::now()),
        Incoming::Change(change) => dev.change(change, out),
        Incoming::Link(event) => dev.link(event, out),
        Incoming::Control(call) => dev.control(call, out),
        Incoming::Build(BuildEvent::Log { level, line }) => out.log(level, "tool", &line),
        Incoming::Build(BuildEvent::Rescoped(Ok(rust))) => dev.rescope(rust),
        Incoming::Build(BuildEvent::Rescoped(Err(why))) => out.log(
            "warn",
            "tool",
            &format!("the Rust scope stays as it was: {why}"),
        ),
        Incoming::Build(BuildEvent::Finished(finished)) => {
            let build = builds.finished(finished)?;
            if dev.rebuilt(&build, out) {
                return build.result.ok();
            }
        }
    }
    None
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
