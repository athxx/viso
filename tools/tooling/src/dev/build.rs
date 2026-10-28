//! The builds of the dev artifact: the launch build and each rebuild a Rust
//! edit asks for, run by Cargo on a thread while the app keeps running.
//!
//! A Rust change starts a build once no change has arrived for the session's
//! coalescing window, so the files of one edit build together. A change that
//! arrives while a build runs kills it (`crate::build::cargo`), and the next
//! build starts with the newest sources, so the newest build that succeeds is
//! the one the app runs. A build that changed a `Cargo.toml` first reads the
//! Rust scope again, as a path dependency may have come or gone. The old app
//! is never stopped for a build: a failure keeps it, and a build that
//! relinked nothing (an edit undone before it built) has nothing to run.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use super::cargo::RustScope;
use super::{COALESCE, COALESCE_LIMIT};
use crate::build::cargo::{self, BuildCommand, BuildFailure, CargoEvent};

/// Where a build hands its events.
type Sink = Arc<dyn Fn(BuildEvent) -> bool + Send + Sync>;

/// What the build threads hand the session.
#[derive(Debug)]
pub enum BuildEvent {
    /// What Cargo said.
    Cargo(CargoEvent),
    /// The Rust scope read again before a build, or why it could not be.
    Rescoped(Result<RustScope, String>),
    /// A build ended.
    Finished(Finished),
}

/// How a build ended.
#[derive(Debug)]
pub struct Finished {
    generation: u64,
    pub result: Result<PathBuf, BuildFailure>,
    /// From the start of the build to its end.
    pub elapsed: Duration,
}

/// A build the session took: what it built and the files whose changes it
/// carries.
#[derive(Debug)]
pub struct Build {
    pub result: Result<PathBuf, BuildFailure>,
    pub elapsed: Duration,
    /// The Rust files changed since the build before, as the watcher named
    /// them; none for the launch build.
    pub files: Vec<PathBuf>,
    /// Whether the executable is the one running: the build relinked
    /// nothing.
    pub unchanged: bool,
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
pub struct RustBuild {
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
pub type Stamp = Option<(u64, SystemTime)>;

pub fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

impl RustBuild {
    /// The builds `command` runs, each event handed to `sink`.
    pub fn new(
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
    pub fn start(&mut self) {
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
                        &command.cargo_args,
                    )));
                }
                let cargo_sink: cargo::Sink = {
                    let sink = Arc::clone(&sink);
                    Arc::new(move |event| sink(BuildEvent::Cargo(event)))
                };
                let result = cargo::build(&command, &cancelled, &cargo_sink);
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
    pub fn change(&mut self, path: PathBuf, now: Instant) {
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
    pub fn deadline(&self) -> Option<Instant> {
        let pending = self.pending.as_ref()?;
        Some((pending.last + COALESCE).min(pending.first + COALESCE_LIMIT))
    }

    /// Starts the next build when it is due.
    pub fn tick(&mut self, now: Instant) {
        if self.deadline().is_some_and(|due| due <= now) {
            self.start();
        }
    }

    /// The build `finished` ended, unless a newer one replaced it.
    pub fn finished(&mut self, finished: Finished) -> Option<Build> {
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
    pub fn launched(&mut self, exe: &Path) {
        self.launched = Some((exe.to_owned(), stamp(exe)));
    }

    /// Kills the build running and waits for every killed build to end;
    /// what changes next waits for [`start`](Self::start).
    pub fn stop(&mut self) {
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc::RecvTimeoutError;

    use super::*;

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
            cargo_args: Vec::new(),
            build_args: Vec::new(),
            profile: viso_project::Profile::Dev,
            profile_args: Vec::new(),
            features: vec![crate::build::cargo::HOT_RELOAD],
            no_default_features: false,
            target: None,
            rustc_args: Vec::new(),
            env: Vec::new(),
            package: None,
            lock: None,
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
                Ok(BuildEvent::Cargo(CargoEvent::Log { line, .. })) => lines.push(line),
                Ok(BuildEvent::Cargo(CargoEvent::Diagnostic(d))) => {
                    lines.push(format!("{}[{}]: {}", d.severity, d.code, d.message))
                }
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
