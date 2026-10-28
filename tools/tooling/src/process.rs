//! Every program the CLI runs: an explicit program and its arguments as a
//! list, never a shell string; under `--verbose` the command line shown, the
//! program apart from its arguments and every secret hidden; what it wrote
//! and how it exited captured as data; a timeout and the first interrupt
//! ending it with everything it started.

use std::ffi::OsStr;
use std::fmt;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::{interrupt, secret};

/// How `--verbose` shows the commands run: not at all, on stderr, or as
/// `log` events the JSON stream takes from [`echoed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Echo {
    Off,
    Stderr,
    Events,
}

static ECHO: AtomicU8 = AtomicU8::new(Echo::Off as u8);
static ECHOED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Shows each command run from now on as `echo` says.
pub fn echo_to(echo: Echo) {
    ECHO.store(echo as u8, Ordering::Relaxed);
}

/// The command lines shown since the last call, for the JSON stream.
pub fn echoed() -> Vec<String> {
    if ECHO.load(Ordering::Relaxed) != Echo::Events as u8 {
        return Vec::new();
    }
    std::mem::take(&mut *ECHOED.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Shows `command` as `--verbose` asks.
pub fn echo(command: &Command) {
    match ECHO.load(Ordering::Relaxed) {
        e if e == Echo::Stderr as u8 => eprintln!("[viso] running {}", shown(command)),
        e if e == Echo::Events as u8 => ECHOED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("running {}", shown(command))),
        _ => {}
    }
}

/// `command` as it may be shown: the variables it sets, its program, then
/// its arguments, each quoted where it would not read back as one, every
/// secret value hidden.
pub fn shown(command: &Command) -> String {
    let mut line = String::new();
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            line.push_str(&hidden_or_quoted(secret::pair(
                &name.to_string_lossy(),
                &value.to_string_lossy(),
            )));
            line.push(' ');
        }
    }
    line.push_str(&quoted(&command.get_program().to_string_lossy()));
    let mut hide = false;
    for arg in command.get_args() {
        let arg = arg.to_string_lossy();
        line.push(' ');
        if hide {
            line.push_str(secret::HIDDEN);
        } else {
            line.push_str(&hidden_or_quoted(secret::argument(&arg)));
        }
        hide = !hide && secret::names_next(&arg);
    }
    if let Some(dir) = command.get_current_dir() {
        line.push_str(&format!(" (in {})", dir.display()));
    }
    line
}

/// `word` with a secret hidden as is, otherwise [`quoted`].
fn hidden_or_quoted(word: String) -> String {
    if word.ends_with(secret::HIDDEN) {
        word
    } else {
        quoted(&word)
    }
}

/// `word`, single-quoted when it holds what a shell would split or expand.
fn quoted(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%^".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// The name a program is reported by: its file name.
pub fn name(program: &OsStr) -> String {
    std::path::Path::new(program)
        .file_name()
        .unwrap_or(program)
        .to_string_lossy()
        .into_owned()
}

/// Starts `command` as it is set up, shown first.
pub fn spawn(command: &mut Command) -> std::io::Result<Child> {
    echo(command);
    command.spawn()
}

/// Starting a command the way every program the CLI runs starts: shown
/// first.
pub trait Launch {
    fn launch(&mut self) -> std::io::Result<Child>;
}

impl Launch for Command {
    fn launch(&mut self) -> std::io::Result<Child> {
        spawn(self)
    }
}

/// What a program that ran to its end wrote, and how it exited.
#[derive(Debug)]
pub struct Captured {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    /// What it said about itself: its stderr, else its stdout, trimmed.
    pub fn said(&self) -> &str {
        match self.stderr.trim() {
            "" => self.stdout.trim(),
            said => said,
        }
    }
}

/// How long a program asked a quick question is given.
pub const QUICK: Duration = Duration::from_secs(30);

/// Why a program did not run to its end.
#[derive(Debug)]
pub enum Failure {
    /// It is not installed where it was looked for.
    NotFound(String),
    /// It could not start, or be waited for.
    Io(String, std::io::Error),
    /// It ran past its time and was ended.
    TimedOut(String, Duration),
    /// An interrupt ended it.
    Interrupted(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NotFound(name) => write!(f, "`{name}` is not installed"),
            Failure::Io(name, error) => write!(f, "cannot run `{name}`: {error}"),
            Failure::TimedOut(name, after) => {
                write!(f, "`{name}` did not finish in {} s", after.as_secs())
            }
            Failure::Interrupted(name) => write!(f, "`{name}` was interrupted"),
        }
    }
}

/// Runs `command` with no input to its end, or until `timeout` passes or an
/// interrupt arrives, then ends it and what it started; what it wrote is
/// read as it writes, so neither pipe fills.
pub fn capture(command: &mut Command, timeout: Option<Duration>) -> Result<Captured, Failure> {
    let name = name(command.get_program());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own group only while interrupts are caught: otherwise the
    // terminal's Ctrl-C must reach it as it reaches the CLI.
    let started = if interrupt::caught() {
        ProcessTree::spawn(command)
    } else {
        spawn(command).map(ProcessTree::lone)
    };
    let mut tree = match started {
        Ok(tree) => tree,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Failure::NotFound(name));
        }
        Err(error) => return Err(Failure::Io(name, error)),
    };
    let readers = [
        tree.child
            .stdout
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
        tree.child
            .stderr
            .take()
            .map(|e| Box::new(e) as Box<dyn Read + Send>),
    ]
    .map(|stream| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    });
    let until = timeout.map(|timeout| Instant::now() + timeout);
    let mut wait = Duration::from_millis(1);
    let status = loop {
        match tree.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if interrupt::count() > 0 => {
                tree.kill();
                return Err(Failure::Interrupted(name));
            }
            Ok(None) if until.is_some_and(|until| Instant::now() >= until) => {
                tree.kill();
                return Err(Failure::TimedOut(name, timeout.unwrap_or_default()));
            }
            Ok(None) => {
                thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_millis(20));
            }
            Err(error) => {
                tree.kill();
                return Err(Failure::Io(name, error));
            }
        }
    };
    let [stdout, stderr] = readers.map(|reader| reader.join().unwrap_or_default());
    Ok(Captured {
        status,
        stdout,
        stderr,
    })
}

/// How `command`, run as [`capture`] runs it, exited; what it wrote is
/// dropped.
pub fn status(command: &mut Command, timeout: Duration) -> Result<ExitStatus, Failure> {
    capture(command, Some(timeout)).map(|captured| captured.status)
}

/// Runs `command` as [`capture`] does and returns what it wrote to stdout;
/// when it fails, why, with what it said.
pub fn run(command: &mut Command, timeout: Option<Duration>) -> Result<String, String> {
    let captured = capture(command, timeout).map_err(|failure| failure.to_string())?;
    if captured.status.success() {
        return Ok(captured.stdout);
    }
    Err(format!(
        "`{}` failed ({}): {}",
        name(command.get_program()),
        captured.status,
        captured.said()
    ))
}

/// A child and every process it starts, killed together: its own process
/// group on Unix, a job object on Windows.
pub struct ProcessTree {
    pub child: Child,
    /// Whether the child leads a group of its own.
    grouped: bool,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl ProcessTree {
    /// Starts `command` in a tree of its own, shown first.
    #[cfg(unix)]
    pub fn spawn(command: &mut Command) -> std::io::Result<ProcessTree> {
        use std::os::unix::process::CommandExt;
        // Its own group, which the terminal's Ctrl-C does not reach: the
        // command ending it kills it.
        command.process_group(0);
        Ok(ProcessTree {
            child: spawn(command)?,
            grouped: true,
        })
    }

    #[cfg(windows)]
    pub fn spawn(command: &mut Command) -> std::io::Result<ProcessTree> {
        let child = spawn(command)?;
        let job = job::Job::holding(&child);
        Ok(ProcessTree {
            child,
            grouped: true,
            job,
        })
    }

    #[cfg(not(any(unix, windows)))]
    pub fn spawn(command: &mut Command) -> std::io::Result<ProcessTree> {
        Ok(ProcessTree {
            child: spawn(command)?,
            grouped: false,
        })
    }

    /// `child`, which shares this process's group, killed alone.
    fn lone(child: Child) -> ProcessTree {
        ProcessTree {
            child,
            grouped: false,
            #[cfg(windows)]
            job: None,
        }
    }

    /// Kills the tree and reaps the child.
    pub fn kill(&mut self) {
        #[cfg(unix)]
        if self.grouped
            && let Ok(group) = libc::pid_t::try_from(self.child.id())
        {
            // SAFETY: a plain call; the child is not yet reaped, so its id
            // still names its group.
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        #[cfg(not(unix))]
        let _ = self.grouped;
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

/// Asks `child` to stop as the user's interrupt does: `SIGINT` on Unix,
/// where it can clean up; elsewhere it is ended at once.
pub fn interrupt(child: &mut Child) {
    #[cfg(unix)]
    // SAFETY: `kill` signals the pid of a child this process spawned and has
    // not reaped, so the pid is still its.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    #[cfg(not(unix))]
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_shown_with_its_secrets_hidden() {
        let mut command = Command::new("/usr/bin/tool");
        command.env("API_TOKEN", "t0p").env("MODE", "fast").args([
            "--password=hunter2",
            "two words",
            "plain",
            "--es",
            "VISO_DEV_TOKEN",
            "s3cr3t",
            "--token",
            "abc",
            "--es",
            "MODE",
            "fast",
        ]);
        let line = shown(&command);
        assert!(line.contains("API_TOKEN=<hidden>"), "{line}");
        assert!(line.contains("MODE=fast"), "{line}");
        assert!(
            line.starts_with(
                "API_TOKEN=<hidden> MODE=fast /usr/bin/tool --password=<hidden> 'two words' plain"
            ),
            "{line}"
        );
        assert!(
            line.ends_with("--es VISO_DEV_TOKEN <hidden> --token <hidden> --es MODE fast"),
            "{line}"
        );
        assert!(
            !["hunter2", "t0p", "s3cr3t", "abc"]
                .iter()
                .any(|secret| line.contains(secret)),
            "{line}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_capture_holds_what_ran_and_how_it_ended() {
        let captured = capture(
            Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]),
            None,
        )
        .expect("sh runs");
        assert_eq!(captured.status.code(), Some(3));
        assert_eq!(captured.stdout, "out\n");
        assert_eq!(captured.said(), "err");
        let failed = run(
            Command::new("sh").args(["-c", "echo nope >&2; exit 1"]),
            None,
        )
        .expect_err("it fails");
        assert!(
            failed.starts_with("`sh` failed") && failed.ends_with("nope"),
            "{failed}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_program_past_its_time_is_ended() {
        let started = Instant::now();
        let failure = capture(
            Command::new("sh").args(["-c", "sleep 30"]),
            Some(Duration::from_millis(100)),
        )
        .expect_err("it times out");
        assert!(matches!(failure, Failure::TimedOut(..)), "{failure}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let missing = capture(&mut Command::new("/no/such/program"), None).expect_err("missing");
        assert_eq!(missing.to_string(), "`program` is not installed");
    }
}
