//! The builds of the dev artifact: the launch build and each rebuild a Rust
//! edit asks for, run by Cargo on a thread while the app keeps running.
//!
//! A Rust change starts a build once no change has arrived for the session's
//! coalescing window, so the files of one edit build together. A change that
//! arrives while a build runs kills it — `cargo` and every `rustc`, linker
//! and build script under it, the process group on Unix and a job object on
//! Windows — and the next build starts with the newest sources, so the
//! newest build that succeeds is the one the app runs. A build that changed
//! a `Cargo.toml` first reads the Rust scope again, as a path dependency may
//! have come or gone. The old app is never stopped for a build: a failure
//! keeps it, and a build that relinked nothing (an edit undone before it
//! built) has nothing to run.
//!
//! Cargo's JSON messages give the executable and each compiler diagnostic,
//! its code and its rendered text; the text and Cargo's progress go to the
//! terminal, or each line as a log message under `--json`.

use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use viso_ende::JsonValue;
use viso_project::Profile;

use super::cargo::RustScope;
use super::{COALESCE, COALESCE_LIMIT};

/// How long a build thread waits on its child between checks that it ended,
/// when Cargo's output has not closed to say so.
const EXIT_POLL: Duration = Duration::from_millis(100);

/// Where a build hands its events.
type Sink = Arc<dyn Fn(BuildEvent) -> bool + Send + Sync>;

/// What wraps a built executable for the device it runs on — an `.app`, an
/// APK, a web bundle — on the build's thread: the artifact to launch, or why
/// there is none. It leaves an artifact it already made from the same
/// executable as it is.
pub(crate) type Package = Arc<dyn Fn(&Path) -> Result<PathBuf, String> + Send + Sync>;

/// How the dev artifact is built.
#[derive(Clone)]
pub(crate) struct BuildCommand {
    /// The `cargo` to run.
    pub(crate) cargo: PathBuf,
    /// The app package's manifest, a canonical path.
    pub(crate) manifest: PathBuf,
    /// Whether Cargo prints only its errors.
    pub(crate) quiet: bool,
    /// Whether Cargo's rendered output is read as log lines rather than
    /// written to the terminal.
    pub(crate) piped: bool,
    /// Where Cargo builds, when not where it chooses.
    pub(crate) target_dir: Option<PathBuf>,
    /// The Rust target, when not the host.
    pub(crate) target: Option<String>,
    /// The arguments after `cargo rustc --`; the build is `cargo build` when
    /// there are none.
    pub(crate) rustc_args: Vec<String>,
    /// The variables Cargo runs with besides the dev profile's.
    pub(crate) env: Vec<(String, std::ffi::OsString)>,
    /// What wraps the executable; it runs as built when `None`.
    pub(crate) package: Option<Package>,
}

impl std::fmt::Debug for BuildCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildCommand")
            .field("cargo", &self.cargo)
            .field("manifest", &self.manifest)
            .field("target", &self.target)
            .field("packaged", &self.package.is_some())
            .finish_non_exhaustive()
    }
}

/// What the build threads hand the session.
#[derive(Debug)]
pub(crate) enum BuildEvent {
    /// A line of Cargo's rendered output, at the level its prefix gives.
    Log { level: &'static str, line: String },
    /// The Rust scope read again before a build, or why it could not be.
    Rescoped(Result<RustScope, String>),
    /// A build ended.
    Finished(Finished),
}

/// How a build ended.
#[derive(Debug)]
pub(crate) struct Finished {
    generation: u64,
    pub(crate) result: Result<PathBuf, BuildFailure>,
    /// From the start of the build to its end.
    pub(crate) elapsed: Duration,
}

/// Why a build gave no executable.
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
    /// A newer change, or the session's end, killed it.
    Cancelled,
}

/// A build the session took: what it built and the files whose changes it
/// carries.
#[derive(Debug)]
pub(crate) struct Build {
    pub(crate) result: Result<PathBuf, BuildFailure>,
    pub(crate) elapsed: Duration,
    /// The Rust files changed since the build before, as the watcher named
    /// them; none for the launch build.
    pub(crate) files: Vec<PathBuf>,
    /// Whether the executable is the one running: the build relinked
    /// nothing.
    pub(crate) unchanged: bool,
}

/// The build running, and the files it carries.
struct Running {
    generation: u64,
    cancel: Sender<()>,
    thread: JoinHandle<()>,
    files: Vec<PathBuf>,
}

/// Rust changes waiting to build.
struct Pending {
    first: Instant,
    last: Instant,
}

/// The session's builds of the dev artifact.
pub(crate) struct RustBuild {
    command: Arc<BuildCommand>,
    sink: Sink,
    running: Option<Running>,
    /// Builds killed and not yet ended, joined before the session ends.
    killed: Vec<JoinHandle<()>>,
    pending: Option<Pending>,
    /// The files the next build carries.
    files: Vec<PathBuf>,
    /// Whether the next build reads the Rust scope first.
    rescope: bool,
    generation: u64,
    /// The executable the app runs and its stamp when it was launched.
    launched: Option<(PathBuf, Stamp)>,
}

/// What tells two builds of an executable apart: its length and
/// modification time.
pub(crate) type Stamp = Option<(u64, SystemTime)>;

pub(crate) fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

impl RustBuild {
    /// The builds `command` runs, each event handed to `sink`.
    pub(crate) fn new(
        command: BuildCommand,
        sink: impl Fn(BuildEvent) -> bool + Send + Sync + 'static,
    ) -> Self {
        RustBuild {
            command: Arc::new(command),
            sink: Arc::new(sink),
            running: None,
            killed: Vec::new(),
            pending: None,
            files: Vec::new(),
            rescope: false,
            generation: 0,
            launched: None,
        }
    }

    /// Starts a build now, killing the one running.
    pub(crate) fn start(&mut self) {
        self.kill();
        self.pending = None;
        self.generation += 1;
        let (cancel, cancelled) = mpsc::channel();
        let command = Arc::clone(&self.command);
        let sink = Arc::clone(&self.sink);
        let rescope = std::mem::take(&mut self.rescope);
        let generation = self.generation;
        let thread = thread::Builder::new()
            .name("viso-dev-build".into())
            .spawn(move || {
                let started = Instant::now();
                if rescope {
                    sink(BuildEvent::Rescoped(RustScope::read(
                        &command.manifest,
                        command.target.as_deref(),
                    )));
                }
                let result = build(&command, &cancelled, &sink);
                sink(BuildEvent::Finished(Finished {
                    generation,
                    result,
                    elapsed: started.elapsed(),
                }));
            })
            .expect("a build thread");
        self.running = Some(Running {
            generation,
            cancel,
            thread,
            files: std::mem::take(&mut self.files),
        });
    }

    /// Takes a change of Rust file `path` at `now`: a build running is
    /// killed, and the next starts once the changes have settled.
    pub(crate) fn change(&mut self, path: PathBuf, now: Instant) {
        if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            self.rescope = true;
        }
        if let Some(running) = &mut self.running {
            self.files.splice(0..0, running.files.drain(..));
            self.kill();
        }
        if !self.files.contains(&path) {
            self.files.push(path);
        }
        match &mut self.pending {
            Some(pending) => pending.last = now,
            None => {
                self.pending = Some(Pending {
                    first: now,
                    last: now,
                })
            }
        }
    }

    /// When the next build is to start.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        let pending = self.pending.as_ref()?;
        Some((pending.last + COALESCE).min(pending.first + COALESCE_LIMIT))
    }

    /// Starts the next build when it is due.
    pub(crate) fn tick(&mut self, now: Instant) {
        if self.deadline().is_some_and(|due| due <= now) {
            self.start();
        }
    }

    /// The build `finished` ended, unless a newer one replaced it.
    pub(crate) fn finished(&mut self, finished: Finished) -> Option<Build> {
        self.killed.retain(|thread| !thread.is_finished());
        if self
            .running
            .as_ref()
            .is_none_or(|running| running.generation != finished.generation)
        {
            return None;
        }
        let running = self.running.take().expect("the build running");
        let _ = running.thread.join();
        let unchanged = match (&finished.result, &self.launched) {
            (Ok(exe), Some((launched, at))) => exe == launched && stamp(exe) == *at,
            _ => false,
        };
        Some(Build {
            result: finished.result,
            elapsed: finished.elapsed,
            files: running.files,
            unchanged,
        })
    }

    /// Records that the app runs executable `exe` as it is now.
    pub(crate) fn launched(&mut self, exe: &Path) {
        self.launched = Some((exe.to_owned(), stamp(exe)));
    }

    /// Kills the build running and waits for every killed build to end;
    /// what changes next waits for [`start`](Self::start).
    pub(crate) fn stop(&mut self) {
        self.kill();
        self.pending = None;
        for thread in self.killed.drain(..) {
            let _ = thread.join();
        }
    }

    /// Kills the build running; its end is not reported.
    fn kill(&mut self) {
        if let Some(running) = self.running.take() {
            let _ = running.cancel.send(());
            self.killed.push(running.thread);
        }
    }
}

impl Drop for RustBuild {
    /// The session's end kills its build and waits for the kill, so no
    /// `rustc` outlives it.
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs `cargo build` of the dev artifact until it ends or `cancelled`
/// fires (or closes), its rendered lines handed to `sink`.
fn build(
    command: &BuildCommand,
    cancelled: &mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<PathBuf, BuildFailure> {
    let mut cargo = Command::new(&command.cargo);
    cargo
        .arg(if command.rustc_args.is_empty() {
            "build"
        } else {
            "rustc"
        })
        .arg("--manifest-path")
        .arg(&command.manifest)
        .args(["--features", "viso/hot-reload"])
        // The facade's build script refuses `hot-reload` in a release or
        // shipping artifact; this one is a dev artifact.
        .env("VISO_PROFILE", Profile::Dev.as_str())
        .arg("--message-format=json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    if command.quiet {
        cargo.arg("--quiet");
    }
    if command.piped {
        cargo.stderr(Stdio::piped());
    }
    if let Some(dir) = &command.target_dir {
        cargo.env("CARGO_TARGET_DIR", dir);
    }
    if let Some(target) = &command.target {
        cargo.args(["--target", target]);
    }
    cargo.envs(command.env.iter().map(|(k, v)| (k, v)));
    if !command.rustc_args.is_empty() {
        cargo.arg("--").args(&command.rustc_args);
    }
    let mut tree = ProcessTree::spawn(&mut cargo).map_err(|error| {
        BuildFailure::Spawn(format!("cannot run `{}`: {error}", command.cargo.display()))
    })?;
    // Cargo's JSON output closing says it is ending.
    let (closed, wait) = mpsc::channel();
    let messages = tree.child.stdout.take().map(|stdout| {
        let sink = command.piped.then(|| Arc::clone(sink));
        thread::spawn(move || {
            let messages = Messages::read(stdout, sink.as_deref());
            let _ = closed.send(());
            messages
        })
    });
    let rendered = tree.child.stderr.take().map(|stderr| {
        let sink = Arc::clone(sink);
        thread::spawn(move || lines(stderr, &*sink))
    });
    let status = loop {
        if cancelled.try_recv() != Err(mpsc::TryRecvError::Empty) {
            tree.kill();
            break None;
        }
        match wait.recv_timeout(EXIT_POLL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => match tree.child.wait() {
                Ok(status) => break Some(status),
                Err(error) => {
                    tree.kill();
                    return Err(BuildFailure::Spawn(format!(
                        "cannot wait for `cargo build`: {error}"
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
    let exe = match <[PathBuf; 1]>::try_from(messages.executables) {
        Ok([exe]) => exe,
        Err(executables) => return Err(BuildFailure::Executables(executables)),
    };
    match &command.package {
        Some(package) => package(&exe).map_err(BuildFailure::Package),
        None => Ok(exe),
    }
}

/// Hands each line of `input` to `sink` as a log line.
fn lines(input: impl Read, sink: &(dyn Fn(BuildEvent) -> bool + Send + Sync)) {
    for line in BufReader::new(input).lines() {
        let Ok(line) = line else {
            return;
        };
        let level = cargo_level(&line);
        if !sink(BuildEvent::Log { level, line }) {
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

/// What Cargo's JSON messages said.
#[derive(Debug, Default, PartialEq)]
struct Messages {
    /// The executable of each artifact that has one.
    executables: Vec<PathBuf>,
    /// The code of each error the compiler reported, each once.
    codes: Vec<String>,
}

impl Messages {
    /// Reads Cargo's JSON messages from `stdout` to its end, each
    /// diagnostic's text handed to `sink` line by line, or written to
    /// stderr without one.
    fn read(
        stdout: impl Read,
        sink: Option<&(dyn Fn(BuildEvent) -> bool + Send + Sync)>,
    ) -> Messages {
        let mut messages = Messages::default();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            let Some((level, rendered)) = messages.take(&line) else {
                continue;
            };
            match sink {
                Some(sink) => {
                    for line in rendered.lines() {
                        let line = line.to_owned();
                        sink(BuildEvent::Log { level, line });
                    }
                }
                None => {
                    let _ = std::io::stderr().lock().write_all(rendered.as_bytes());
                }
            }
        }
        messages
    }

    /// Takes one line of Cargo's JSON output; a compiler diagnostic gives
    /// its level and rendered text.
    fn take(&mut self, line: &str) -> Option<(&'static str, String)> {
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
                let level = diagnostic.get("level").and_then(JsonValue::as_str);
                if level == Some("error")
                    && let Some(code) = diagnostic
                        .get("code")
                        .and_then(|code| code.get("code"))
                        .and_then(JsonValue::as_str)
                    && !self.codes.iter().any(|c| c == code)
                {
                    self.codes.push(code.to_owned());
                }
                let rendered = diagnostic.get("rendered").and_then(JsonValue::as_str)?;
                let level = match level {
                    Some("error" | "error: internal compiler error") => "error",
                    Some("warning") => "warn",
                    _ => "info",
                };
                Some((level, rendered.to_owned()))
            }
            _ => None,
        }
    }
}

/// A child and every process it starts, killed together: its own process
/// group on Unix, a job object on Windows.
struct ProcessTree {
    child: Child,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl ProcessTree {
    #[cfg(unix)]
    fn spawn(command: &mut Command) -> std::io::Result<ProcessTree> {
        use std::os::unix::process::CommandExt;
        // Its own group, which the terminal's Ctrl-C does not reach: the
        // session kills it.
        command.process_group(0);
        Ok(ProcessTree {
            child: command.spawn()?,
        })
    }

    #[cfg(windows)]
    fn spawn(command: &mut Command) -> std::io::Result<ProcessTree> {
        let child = command.spawn()?;
        let job = job::Job::holding(&child);
        Ok(ProcessTree { child, job })
    }

    #[cfg(not(any(unix, windows)))]
    fn spawn(command: &mut Command) -> std::io::Result<ProcessTree> {
        Ok(ProcessTree {
            child: command.spawn()?,
        })
    }

    /// Kills the tree and reaps the child.
    fn kill(&mut self) {
        #[cfg(unix)]
        if let Ok(group) = libc::pid_t::try_from(self.child.id()) {
            // SAFETY: a plain call; the child is not yet reaped, so its id
            // still names its group.
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_core::PCWSTR;

    /// A job object holding a child and what it starts, which closing kills.
    pub(super) struct Job(HANDLE);

    impl Job {
        /// A job holding `child`, `None` when the system gives none.
        pub(super) fn holding(child: &Child) -> Option<Job> {
            // SAFETY: no attributes and no name; the handle is owned below.
            let job = Job(unsafe { CreateJobObjectW(None, PCWSTR::null()) }.ok()?);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` is the extended limit record the class names,
            // of the length given, and lives across the call; the process
            // handle is the child's, open while `child` lives.
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
                .ok()?;
                AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())).ok()?;
            }
            Some(job)
        }

        /// Ends every process of the job.
        pub(super) fn terminate(&self) {
            // SAFETY: the job handle is open while `self` lives.
            let _ = unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is owned and closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn cargo_messages_give_the_executable_and_the_error_codes() {
        let mut messages = Messages::default();
        let mut rendered = Vec::new();
        for line in [
            r#"{"reason":"compiler-artifact","executable":null,"fresh":true}"#,
            r#"{"reason":"compiler-artifact","executable":"/t/debug/my \"app\"é","fresh":false}"#,
            r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308","explanation":"…"},"rendered":"error[E0308]: mismatched types\n"}}"#,
            r#"{"reason":"compiler-message","message":{"level":"warning","code":{"code":"W1"},"rendered":"warning: unused\n"}}"#,
            r#"{"reason":"compiler-message","message":{"level":"error","code":null}}"#,
            r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"}}}"#,
            r#"{"reason":"build-finished","success":false}"#,
            "not json",
        ] {
            rendered.extend(messages.take(line));
        }
        assert_eq!(
            rendered,
            [
                ("error", "error[E0308]: mismatched types\n".to_owned()),
                ("warn", "warning: unused\n".to_owned())
            ]
        );
        assert_eq!(
            messages,
            Messages {
                executables: vec![PathBuf::from("/t/debug/my \"app\"é")],
                codes: vec!["E0308".into()],
            }
        );
    }

    /// A Cargo package `name` in a temporary directory, its binary's
    /// `main.rs` `main`.
    fn package(name: &str, main: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viso-build-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        // The `viso/hot-reload` feature the dev build asks for.
        std::fs::create_dir_all(dir.join("viso/src")).unwrap();
        std::fs::write(
            dir.join("viso/Cargo.toml"),
            "[package]\nname = \"viso\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
             [features]\nhot-reload = []\n",
        )
        .unwrap();
        std::fs::write(dir.join("viso/src/lib.rs"), "").unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
                 [dependencies]\nviso = {{ path = \"viso\" }}\n[workspace]\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src/main.rs"), main).unwrap();
        dir.canonicalize().unwrap()
    }

    /// The builds of the package at `dir` and the events they hand over.
    fn builds(dir: &Path) -> (RustBuild, mpsc::Receiver<BuildEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let command = BuildCommand {
            cargo: std::env::var_os("CARGO").map_or_else(|| "cargo".into(), PathBuf::from),
            manifest: dir.join("Cargo.toml"),
            quiet: false,
            piped: true,
            target_dir: Some(dir.join("target")),
            target: None,
            rustc_args: Vec::new(),
            env: Vec::new(),
            package: None,
        };
        let builds = RustBuild::new(command, move |event| tx.lock().unwrap().send(event).is_ok());
        (builds, rx)
    }

    /// The next build the session takes, the log lines before it, and the
    /// scopes read again.
    fn next(
        builds: &mut RustBuild,
        rx: &mpsc::Receiver<BuildEvent>,
    ) -> (Build, Vec<String>, Vec<RustScope>) {
        let (mut lines, mut scopes) = (Vec::new(), Vec::new());
        loop {
            let wait = builds.deadline().map_or(Duration::from_secs(120), |due| {
                due.saturating_duration_since(Instant::now())
            });
            match rx.recv_timeout(wait) {
                Ok(BuildEvent::Log { line, .. }) => lines.push(line),
                Ok(BuildEvent::Rescoped(scope)) => scopes.push(scope.unwrap()),
                Ok(BuildEvent::Finished(finished)) => {
                    if let Some(build) = builds.finished(finished) {
                        return (build, lines, scopes);
                    }
                }
                Err(RecvTimeoutError::Timeout) => builds.tick(Instant::now()),
                Err(RecvTimeoutError::Disconnected) => panic!("the builds ended"),
            }
        }
    }

    #[test]
    fn a_rebuild_relinks_what_changed_and_a_failure_names_its_codes() {
        let dir = package("rebuild", "fn main() {}\n");
        let (mut builds, rx) = builds(&dir);
        builds.start();
        let (launch, lines, _) = next(&mut builds, &rx);
        let exe = launch.result.unwrap();
        assert!(exe.is_file(), "{lines:#?}");
        builds.launched(&exe);

        // A build with nothing to relink restarts nothing.
        let main = dir.join("src/main.rs");
        builds.change(main.clone(), Instant::now());
        let (same, _, _) = next(&mut builds, &rx);
        assert!(same.unchanged);
        assert_eq!(same.files, std::slice::from_ref(&main));

        std::fs::write(&main, "fn main() { let x: u8 = \"no\"; }\n").unwrap();
        builds.change(main.clone(), Instant::now());
        let (failed, lines, _) = next(&mut builds, &rx);
        let Err(BuildFailure::Failed { codes, .. }) = failed.result else {
            panic!("{:?}", failed.result)
        };
        assert_eq!(codes, ["E0308"]);
        assert!(
            lines.iter().any(|line| line.starts_with("error[E0308]")),
            "{lines:#?}"
        );

        std::fs::write(&main, "fn main() { println!(\"new\"); }\n").unwrap();
        let manifest = dir.join("Cargo.toml");
        builds.change(main.clone(), Instant::now());
        builds.change(manifest.clone(), Instant::now());
        let (rebuilt, _, scopes) = next(&mut builds, &rx);
        assert_eq!(rebuilt.result.as_ref(), Ok(&exe));
        assert!(!rebuilt.unchanged, "relinked");
        assert_eq!(rebuilt.files, [main, manifest]);
        assert_eq!(scopes.len(), 1, "a manifest edit reads the scope again");
        assert_eq!(scopes[0].packages, [dir.clone(), dir.join("viso")]);
        assert!(builds.running.is_none() && builds.deadline().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_change_during_a_build_kills_its_whole_tree_and_builds_again() {
        let dir = package("killed", "fn main() {}\n");
        let pid = dir.join("script.pid");
        // A build script that outlives any build unless it is killed.
        std::fs::write(
            dir.join("build.rs"),
            format!(
                "fn main() {{\n    std::fs::write({:?}, std::process::id().to_string()).unwrap();\n    \
                 std::thread::sleep(std::time::Duration::from_secs(600));\n}}\n",
                pid
            ),
        )
        .unwrap();
        let (mut builds, rx) = builds(&dir);
        builds.start();
        let deadline = Instant::now() + Duration::from_secs(120);
        let script = loop {
            if let Ok(text) = std::fs::read_to_string(&pid)
                && let Ok(script) = text.trim().parse::<libc::pid_t>()
            {
                break script;
            }
            assert!(Instant::now() < deadline, "the build script never ran");
            thread::sleep(Duration::from_millis(20));
        };
        std::fs::write(dir.join("build.rs"), "fn main() {}\n").unwrap();
        builds.change(dir.join("build.rs"), Instant::now());
        let (build, lines, _) = next(&mut builds, &rx);
        assert!(build.result.is_ok(), "{:?} {lines:#?}", build.result);
        // Killed, the script is reaped once its parent, killed with it, is.
        let deadline = Instant::now() + Duration::from_secs(5);
        // SAFETY: a plain call with signal 0, which only checks.
        while unsafe { libc::kill(script, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "the killed build's script lives on"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
