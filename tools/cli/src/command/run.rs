//! `viso run` (`Viso_CLI.md`) on the desktop host, an iOS Simulator, an
//! Android Emulator or a browser page: take the dev session's lock, read the
//! Rust scope from Cargo, start the watcher, build the project's Cargo
//! package as a dev artifact (packaged for the device, which boots while it
//! builds), launch it with the dev channel's loopback address and session
//! identity (`crate::deploy`), and run the dev session
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
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use viso_package::android::Sdk;
use viso_project::{
    ArtifactKind, DeviceProfile, Layout, LockGuard, LockKind, Overrides, Platform, Profile,
    ProjectFingerprint, Target, Toolchain,
};
use viso_view::dev::wire::{
    self, DevSessionId, ProjectFingerprint as WireProject, SchemaFingerprint,
};

use super::check::{config_in_its_file, failure_code};
use super::{
    BUILD, BUILD_FAILED, CLI_USAGE, ENV_CARGO, ENV_CARGO_MANIFEST, ENV_CURRENT_DIR,
    ENV_DEV_CHANNEL, ENV_NO_EXECUTABLE, ENVIRONMENT, INTERRUPTED, RUN_APP_FAILED, RUNTIME, SUCCESS,
    USAGE,
};
use crate::args::{Global, RunArgs, RunTarget};
use crate::deploy::{self, App, Deploy, Ended, Lines, Vars, web};
use crate::dev::build::{BuildCommand, BuildEvent, BuildFailure, RustBuild};
use crate::dev::cargo::RustScope;
use crate::dev::{
    self, Control, ControlCall, ControlDeliver, Deliver, DevSession, Expect, LinkEvent, WatchScope,
    build_failure,
};
use crate::device::{self as devices, ENV_DEVICE, emulator, simulator};
use crate::interrupt;
use crate::output::Output;

/// How long the session waits for a message before it polls the app and the
/// listener again.
const POLL: Duration = Duration::from_millis(50);

/// How often the session polls the old app while it closes for a restart:
/// the new one starts as soon as it is gone.
const CLOSING_POLL: Duration = Duration::from_millis(1);

/// How long the prewarm run of a new executable may take before the restart
/// goes on without it.
const PREWARM_WAIT: Duration = Duration::from_secs(10);

/// How long a warm restart waits for the old app's snapshot, then for it to
/// close before it is killed.
const SNAPSHOT_WAIT: Duration = Duration::from_secs(3);
const CLOSE_WAIT: Duration = Duration::from_secs(3);

/// Where a warm restart onto `exe` is.
enum Restart {
    /// The new executable runs once, from `since`, to exit as it starts: the
    /// OS checks a newly linked executable at its first launch, which takes
    /// most of a second on macOS, while the old app runs on.
    Prewarm {
        exe: PathBuf,
        run: Child,
        since: Instant,
    },
    /// The old app was asked for its snapshot at `since`.
    Snapshot { exe: PathBuf, since: Instant },
    /// The old app was asked to close at `since`.
    Closing { exe: PathBuf, since: Instant },
}

impl Restart {
    /// The executable the app restarts on, its prewarm run stopped.
    fn into_exe(self) -> PathBuf {
        match self {
            Restart::Prewarm { exe, mut run, .. } => {
                let _ = run.kill();
                let _ = run.wait();
                exe
            }
            Restart::Snapshot { exe, .. } | Restart::Closing { exe, .. } => exe,
        }
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
    /// The device booted: the emulator's adb serial, empty for a simulator;
    /// or why it did not.
    Booted(Result<String, String>),
}

/// Where a run goes, as the command line asks: the target, and the device
/// platform of a simulator or emulator target.
fn target_of(args: &RunArgs) -> (Target, Option<Platform>) {
    match args.target {
        None => (Target::Host, None),
        Some(RunTarget::Ios) => (Target::Ios, Some(Platform::Ios)),
        Some(RunTarget::Android) => (Target::Android, Some(Platform::Android)),
        Some(RunTarget::WebGpu) => (Target::WebGpu, None),
        Some(RunTarget::WebDom) => (Target::WebDom, None),
        Some(RunTarget::WebHybrid) => (Target::WebHybrid, None),
    }
}

/// The usage error of an option the target does not take.
fn misplaced(args: &RunArgs, platform: Option<Platform>, target: Target) -> Option<String> {
    let web = matches!(target, Target::WebGpu | Target::WebDom | Target::WebHybrid);
    if args.device.is_some() && platform.is_none() {
        return Some("`--device` takes a simulator or emulator profile: `viso run ios|android --device <id>`".into());
    }
    if args.browser.is_some() && !web {
        return Some("`--browser` is a web option: `viso run web-gpu --browser <name>`".into());
    }
    None
}

pub fn run(global: &Global, args: &RunArgs, out: &mut Output) -> u8 {
    let (target, platform) = target_of(args);
    if let Some(why) = misplaced(args, platform, target) {
        out.failure(CLI_USAGE, &why, &[]);
        return USAGE;
    }
    if let Err(diagnostic) = target.require_available() {
        out.config(None, &diagnostic);
        return diagnostic.code.exit_code();
    }
    if platform == Some(Platform::Ios) && !cfg!(target_os = "macos") {
        out.failure(ENV_DEVICE, "the iOS Simulator runs on macOS only", &[]);
        return ENVIRONMENT;
    }
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
    // `viso run` always builds a dev artifact.
    let flags = Overrides {
        target: Some(target),
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
    // The simulator or emulator profile, by `--device`, the default or the
    // only one.
    let profile = match platform {
        Some(platform) => {
            let store = match devices::store(out) {
                Ok(store) => store,
                Err(code) => return code,
            };
            match devices::select(out, &store, platform, args.device.as_deref(), &root) {
                Ok(profile) => Some(profile),
                Err(code) => return code,
            }
        }
        None => None,
    };
    let sdk = match platform {
        Some(Platform::Android) => match Sdk::locate() {
            Ok(sdk) => Some(sdk),
            Err(why) => {
                out.failure(ENV_DEVICE, &why, &[]);
                return ENVIRONMENT;
            }
        },
        _ => None,
    };

    // One dev session per project, target and device at a time.
    let lock = match LockGuard::acquire(
        &Layout::new(&root),
        LockKind::DevSession {
            target,
            device: profile.as_ref().map(|p| p.id.as_str()),
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

    // The Rust the artifact is built from, as Cargo resolves it for the
    // device; without it the session still runs, and a Rust edit needs
    // `viso run` again.
    let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
    let manifest = canonical.join("Cargo.toml");
    let triple = match platform {
        Some(Platform::Ios) => Some(viso_package::apple::simulator_triple()),
        Some(Platform::Android) => Some(viso_package::android::Abi::host().triple()),
        None if target == Target::WebGpu => Some(viso_package::web::TRIPLE),
        None => None,
    };
    let rust = match RustScope::read(&manifest, triple) {
        Ok(rust) => rust,
        Err(why) => {
            out.log(
                "warn",
                "tool",
                &format!("Rust edits do not rebuild the app: {why}"),
            );
            RustScope::default()
        }
    };
    let (tx, rx) = mpsc::channel();
    // The device boots while the app builds.
    if let Some(profile) = &profile {
        boot(profile.clone(), sdk.clone(), tx.clone(), out);
    }
    let deliver: Deliver = {
        let tx = tx.clone();
        Arc::new(move |event| tx.send(Incoming::Link(event)).is_ok())
    };
    let mut deploy = match (&profile, sdk) {
        (None, _) if target == Target::WebGpu => {
            let port = project
                .manifest
                .web
                .serve
                .port
                .as_ref()
                .map_or(web::DEFAULT_PORT, |port| port.value);
            match web::WebServer::bind(port, expect.clone(), Arc::clone(&deliver)) {
                Ok(server) => Deploy::Browser {
                    server,
                    browser: args.browser.clone().filter(|b| b != "none"),
                    open: args.browser.as_deref() != Some("none")
                        && (!global.json || args.browser.is_some()),
                    launched: false,
                },
                Err(error) => {
                    out.failure(
                        ENV_DEV_CHANNEL,
                        &format!("cannot open the web dev server: {error}"),
                        &[],
                    );
                    return ENVIRONMENT;
                }
            }
        }
        (None, _) => Deploy::Host { root: root.clone() },
        (Some(profile), _) if profile.platform == Platform::Ios => Deploy::Simulator {
            profile: profile.clone(),
        },
        (Some(_), Some(sdk)) => match deploy::android_shell(&rust.linked) {
            Ok(shell) => Deploy::Emulator {
                device: emulator::Device {
                    adb: sdk.adb.clone(),
                    serial: String::new(),
                },
                sdk,
                shell,
                reversed: None,
            },
            Err(why) => {
                out.failure(ENV_DEVICE, &why, &[]);
                return ENVIRONMENT;
            }
        },
        (Some(_), None) => unreachable!("an emulator profile has its SDK"),
    };
    if matches!(deploy, Deploy::Emulator { .. } | Deploy::Browser { .. })
        && !args.app_args.is_empty()
    {
        out.log(
            "warn",
            "tool",
            "an Android or web app takes no command-line arguments; those after `--` are not passed",
        );
    }
    // The watcher starts before the build, so an edit saved while the app
    // builds reaches the session.
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

    let plan = deploy.plan();
    let built = Mutex::new(tx.clone());
    let mut builds = RustBuild::new(
        BuildCommand {
            cargo: std::env::var_os("CARGO").map_or_else(|| "cargo".into(), PathBuf::from),
            manifest,
            quiet: global.quiet,
            piped: global.json,
            target_dir: None,
            target: plan.target,
            rustc_args: plan.rustc_args,
            env: plan.env,
            package: plan.package,
        },
        move |event| {
            built
                .lock()
                .is_ok_and(|tx| tx.send(Incoming::Build(event)).is_ok())
        },
    );
    out.progress("build", "building the dev artifact");
    builds.start();
    let (exe, serial) = match launch_build(out, &mut dev, &mut builds, &rx, profile.as_ref()) {
        Ok(launch) => launch,
        Err(code) => return code,
    };
    builds.launched(&exe);
    if let Deploy::Emulator { device, .. } = &mut deploy {
        device.serial = serial;
    }

    // A page's dev channel comes through the web dev server; any other app
    // connects to a loopback listener of its own.
    let listener = match &deploy {
        Deploy::Browser { .. } => None,
        _ => match TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .and_then(|listener| listener.set_nonblocking(true).map(|()| listener))
        {
            Ok(listener) => Some(listener),
            Err(error) => {
                out.failure(
                    ENV_DEV_CHANNEL,
                    &format!("cannot open the dev channel on the loopback interface: {error}"),
                    &[],
                );
                return ENVIRONMENT;
            }
        },
    };
    let address = match (&listener, &deploy) {
        (Some(listener), _) => listener.local_addr(),
        (None, Deploy::Browser { server, .. }) => Ok(server.addr()),
        (None, _) => unreachable!("only a page has no listener"),
    };
    let address = match address {
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
    let vars = Vars {
        address: address.to_string(),
        token: &token,
        session: &session,
        build_id: &build_id,
    };
    let device_event = |out: &mut Output, state: &str| {
        if let Some(profile) = &profile {
            out.device(
                profile.platform.as_str(),
                &profile.id,
                &profile.device,
                state,
            );
        }
    };
    let mut app = match deploy.launch(&exe, &args.app_args, &vars, false, None, global.json) {
        Ok(app) => app,
        Err(why) => {
            deploy.ended();
            out.failure(RUN_APP_FAILED, &why, &[]);
            return RUNTIME;
        }
    };
    device_event(out, "launched");
    if let Deploy::Browser { server, .. } = &deploy {
        let addr = server.addr();
        out.server(&server.url(), &addr.ip().to_string(), addr.port(), false);
    }

    let mut threads = pipe_logs(&mut app, &tx);
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
        if let Some(listener) = &listener {
            dev::link::accept(listener, &expect, &deliver, &mut threads);
        }
        if let Some(control) = &control {
            control.accept(&ask, &mut asking);
        }
        let caught = interrupt::count();
        if caught > interrupts {
            if interrupts == 0 {
                out.progress("cancel", "stopping the session");
                builds.stop();
                restart = None;
                app.close();
            }
            if caught >= 2 {
                app.kill();
            }
            interrupts = caught;
        }
        let poll = match restart {
            Some(Restart::Prewarm { .. } | Restart::Closing { .. }) => CLOSING_POLL,
            _ => POLL,
        };
        let wait = [dev.deadline(), builds.deadline()]
            .into_iter()
            .flatten()
            .min()
            .map_or(poll, |due| {
                due.saturating_duration_since(Instant::now()).min(poll)
            });
        match rx.recv_timeout(wait) {
            Ok(message) => {
                if let Some(exe) = relay(out, &mut dev, &mut builds, message)
                    && interrupts == 0
                {
                    restart = Some(match restart.take() {
                        // A newer build while the restart runs: the app
                        // restarts on it, prewarmed if the old app still runs.
                        Some(Restart::Prewarm { mut run, .. }) => {
                            let _ = run.kill();
                            let _ = run.wait();
                            prewarm(&deploy, &args.app_args, exe, &mut dev, &app, out)
                        }
                        Some(Restart::Snapshot { since, .. }) => Restart::Snapshot { exe, since },
                        Some(Restart::Closing { since, .. }) => Restart::Closing { exe, since },
                        None => prewarm(&deploy, &args.app_args, exe, &mut dev, &app, out),
                    });
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
            Some(Restart::Prewarm {
                exe,
                mut run,
                since,
            }) => {
                let done = matches!(run.try_wait(), Ok(Some(_)) | Err(_));
                if done || since.elapsed() >= PREWARM_WAIT {
                    let _ = run.kill();
                    let _ = run.wait();
                    Some(snapshot(exe, &mut dev, &app, out))
                } else {
                    Some(Restart::Prewarm { exe, run, since })
                }
            }
            Some(Restart::Snapshot { exe, since })
                if dev.has_snapshot() || since.elapsed() >= SNAPSHOT_WAIT =>
            {
                app.close();
                Some(Restart::Closing {
                    exe,
                    since: Instant::now(),
                })
            }
            Some(Restart::Closing { exe, since }) => {
                if since.elapsed() >= CLOSE_WAIT {
                    app.kill();
                }
                Some(Restart::Closing { exe, since })
            }
            other => other,
        };
        match app.try_wait() {
            Ok(Some(status)) => {
                // The old app closed for the restart: the new one starts.
                let Some(restart) = restart.take().filter(|_| interrupts == 0) else {
                    break Ok(status);
                };
                let exe = restart.into_exe();
                let restore = dev.restore_next();
                out.progress(
                    "launch",
                    &format!("restarting `{}`", relative(&root, &exe).display()),
                );
                match deploy.launch(
                    &exe,
                    &args.app_args,
                    &vars,
                    restore,
                    dev.launch_window(),
                    global.json,
                ) {
                    Ok(next) => {
                        app = next;
                        threads.extend(pipe_logs(&mut app, &tx));
                        builds.launched(&exe);
                        device_event(out, "launched");
                        dev.relaunched(out);
                    }
                    Err(why) => {
                        dev.restart_failed(&why, out);
                        break Ok(status);
                    }
                }
            }
            Ok(None) => {}
            Err(error) => break Err(error),
        }
    };
    // The session's end stops a prewarm run and a build still running.
    if let Some(restart) = restart.take() {
        restart.into_exe();
    }
    builds.stop();
    deploy.ended();
    // The app is gone: a connection it opened is waiting in the listener's
    // backlog or being read to its end, and its pipes are at their end.
    if let Some(listener) = &listener {
        dev::link::accept(listener, &expect, &deliver, &mut threads);
    }
    drop(app);
    drop(deliver);
    drop(tx);
    for thread in threads {
        let _ = thread.join();
    }
    while let Ok(message) = rx.try_recv() {
        let _ = relay(out, &mut dev, &mut builds, message);
    }
    dev.ended(out);
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

/// Boots the device of `profile` on a thread, saying so, and hands the
/// session [`Incoming::Booted`] once it is ready.
fn boot(profile: DeviceProfile, sdk: Option<Sdk>, tx: Sender<Incoming>, out: &mut Output) {
    out.device(
        profile.platform.as_str(),
        &profile.id,
        &profile.device,
        "booting",
    );
    thread::spawn(move || {
        let booted = match (profile.platform, sdk) {
            (Platform::Ios, _) => simulator::boot(&profile).map(|()| String::new()),
            (Platform::Android, Some(sdk)) => emulator::boot(&sdk, &profile, true),
            (Platform::Android, None) => Err("no Android SDK".into()),
        };
        let _ = tx.send(Incoming::Booted(booted));
    });
}

/// Waits for the launch build, which a Rust edit while it runs starts again,
/// and for the boot of the device of `booting`, and returns the build's
/// executable and the device's serial, or the exit code of the failure it
/// reported. The edits of the other domains wait for the app.
fn launch_build(
    out: &mut Output,
    dev: &mut DevSession,
    builds: &mut RustBuild,
    rx: &Receiver<Incoming>,
    mut booting: Option<&DeviceProfile>,
) -> Result<(PathBuf, String), u8> {
    let mut built = None;
    let mut serial = String::new();
    loop {
        if interrupt::count() > 0 {
            return Err(INTERRUPTED);
        }
        if booting.is_none()
            && let Some(exe) = built.take()
        {
            return Ok((exe, serial));
        }
        let wait = builds.deadline().map_or(POLL, |due| {
            due.saturating_duration_since(Instant::now()).min(POLL)
        });
        match rx.recv_timeout(wait) {
            Ok(Incoming::Booted(booted)) => {
                let Some(profile) = booting.take() else {
                    continue;
                };
                match booted {
                    Ok(booted) => {
                        serial = booted;
                        let (platform, id) = (profile.platform.as_str(), &profile.id);
                        out.device(platform, id, &profile.device, "ready");
                    }
                    Err(why) => {
                        out.failure(ENV_DEVICE, &why, &[]);
                        return Err(ENVIRONMENT);
                    }
                }
            }
            Ok(Incoming::Build(BuildEvent::Finished(finished))) => {
                let Some(build) = builds.finished(finished) else {
                    continue;
                };
                built = Some(build.result.map_err(|failure| {
                    let code = match &failure {
                        BuildFailure::Spawn(_) => ENV_CARGO,
                        BuildFailure::Failed { .. } | BuildFailure::Cancelled => BUILD_FAILED,
                        BuildFailure::Executables(_) => ENV_NO_EXECUTABLE,
                        BuildFailure::Package(_) => ENV_DEVICE,
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
                })?);
                continue;
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

/// Starts the restart on `exe`: its prewarm run while the old app runs on,
/// or, where there is none or it does not start, the snapshot right away.
fn prewarm(
    deploy: &Deploy,
    args: &[OsString],
    exe: PathBuf,
    dev: &mut DevSession,
    app: &App,
    out: &mut Output,
) -> Restart {
    match deploy.prewarm(&exe, args) {
        Some(Ok(run)) => {
            dev.prewarm_started();
            out.progress("restart", "starting the new build once");
            Restart::Prewarm {
                exe,
                run,
                since: Instant::now(),
            }
        }
        Some(Err(_)) | None => snapshot(exe, dev, app, out),
    }
}

/// Asks the old app for its snapshot before the restart on `exe`; with no
/// app connected to ask, asks it to close.
fn snapshot(exe: PathBuf, dev: &mut DevSession, app: &App, out: &mut Output) -> Restart {
    out.progress("restart", "taking the app's snapshot");
    let since = Instant::now();
    if dev.request_snapshot() {
        Restart::Snapshot { exe, since }
    } else {
        app.close();
        Restart::Closing { exe, since }
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
fn pipe_logs(app: &mut App, tx: &Sender<Incoming>) -> Vec<JoinHandle<()>> {
    app.outputs()
        .into_iter()
        .map(|(output, lines)| line_reader(output, "app", lines, tx.clone()))
        .collect()
}

/// Starts a thread that sends each line of `input` as a `log` message from
/// `source`, read as `lines` reads it.
fn line_reader(
    input: impl Read + Send + 'static,
    source: &'static str,
    lines: Lines,
    tx: Sender<Incoming>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(input).lines() {
            let Ok(line) = line else {
                return;
            };
            let Some((level, line)) = lines.read(line) else {
                continue;
            };
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
        Incoming::Booted(_) => {}
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
fn exit_code(out: &mut Output, status: std::io::Result<Ended>) -> u8 {
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
