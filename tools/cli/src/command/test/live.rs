//! The `ui` domain played by the app live: on a simulator or emulator as
//! `viso run <ios|android>` runs it, or in a headless browser as `viso run
//! web-gpu` serves it, launched fresh for each case.
//!
//! The case's session is a `viso run` of this executable, its events read as
//! JSON; in a browser the page is opened headless once the session serves
//! it. Once the app connects, the scenario goes to it through the session's
//! control endpoint and the app plays it against its first window as a
//! headless run would — input posted as the platform's, expectations read
//! from its semantics — and reports each step back. A snapshot step is taken
//! headless only and reports skipped here; a component case mounts alone
//! headless only and is skipped. A session that ends before its app connects
//! ends the domain with its own diagnostic and exit code: the build, the
//! device, the SDK or the browser failed, for every case alike.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use viso_dev_protocol::headless::{Reported, Script};
use viso_ende::JsonValue;
use viso_project::{DeviceProfile, LockGuard, LockKind, Platform, Target};

use super::{Run, relative, report};
use crate::command::scenario::{Case, Ended, Kind};
use crate::command::snapshot;
use crate::context::Context;
use crate::deploy::web;
use crate::dev::control;
use crate::dev::{ControlAsk, ControlReply};
use crate::error::{ENV_NO_DEV_SESSION, INTERRUPTED, RUN_TEST_CRASHED, SUCCESS};
use crate::interrupt;
use crate::output::{Output, TestStatus};
use crate::process::Launch;

/// How long a case's app has to play one step, on top of a base.
const STEP_WAIT: Duration = Duration::from_secs(10);
const PLAY_WAIT: Duration = Duration::from_secs(60);

/// How long a session has to end once asked to.
const END_WAIT: Duration = Duration::from_secs(15);

/// How often the session is checked on while it starts.
const POLL: Duration = Duration::from_millis(100);

/// Where the app plays the cases.
pub(super) enum Venue<'a> {
    /// The simulator or emulator of `profile`, of `platform`.
    Device {
        platform: Platform,
        profile: &'a DeviceProfile,
    },
    /// The page `viso run web-gpu` serves, in the headless `browser`.
    Browser { browser: PathBuf },
}

impl Venue<'_> {
    /// The target `viso run` is given.
    fn target(&self) -> Target {
        match self {
            Venue::Device {
                platform: Platform::Ios,
                ..
            } => Target::Ios,
            Venue::Device {
                platform: Platform::Android,
                ..
            } => Target::Android,
            Venue::Browser { .. } => Target::WebGpu,
        }
    }

    /// What it is called in a message.
    fn name(&self) -> &str {
        match self {
            Venue::Device { profile, .. } => &profile.id,
            Venue::Browser { .. } => "the browser",
        }
    }

    /// The arguments of the `viso run` of a case.
    fn run_args(&self) -> Vec<&str> {
        match self {
            Venue::Device { platform, profile } => {
                vec!["run", platform.as_str(), "--device", &profile.id, "--json"]
            }
            Venue::Browser { .. } => vec!["run", "web-gpu", "--browser", "none", "--json"],
        }
    }

    /// The session lock its `viso run` holds.
    fn lock(&self) -> LockKind<'_> {
        LockKind::DevSession {
            target: self.target(),
            device: match self {
                Venue::Device { profile, .. } => Some(&profile.id),
                Venue::Browser { .. } => None,
            },
        }
    }
}

/// Plays `cases` at `venue`; the exit code.
pub(super) fn run(
    ctx: &Context<'_>,
    root: &Path,
    run: &Run<'_>,
    venue: &Venue<'_>,
    cases: Vec<Case>,
    out: &mut Output,
) -> u8 {
    let mut code = SUCCESS;
    for case in cases {
        if let Kind::Component { .. } = case.kind {
            report(
                out,
                "ui",
                &case.name,
                TestStatus::Skip,
                0,
                Some("a component is mounted alone in a headless run only"),
                |_| {},
            );
            continue;
        }
        let started = Instant::now();
        let played = match play(ctx, root, venue, &case) {
            Ok(played) => played,
            Err(Stopped::Interrupted) => return INTERRUPTED,
            Err(Stopped::Session {
                code: ended,
                said,
                output,
            }) => {
                let why = said.map_or_else(
                    || {
                        format!(
                            "`viso run {}` ended before the app connected",
                            venue.target().as_str()
                        )
                    },
                    |(name, message)| format!("{message} ({name})"),
                );
                out.case_output(&case.name, &output);
                report(
                    out,
                    "ui",
                    &case.name,
                    TestStatus::Fail,
                    started.elapsed().as_millis() as u64,
                    Some(&why),
                    |_| {},
                );
                return code.max(ended);
            }
        };
        let duration_ms = started.elapsed().as_millis() as u64;
        let case_code = snapshot::ended(out, root, &case, &played.ended);
        let (status, message) = match &played.ended {
            Ended::Failed { line, message } => (
                TestStatus::Fail,
                Some(format!(
                    "{}:{line}: {message}",
                    relative(root, &case.path).display()
                )),
            ),
            Ended::Crashed(why) => (TestStatus::Fail, Some(why.clone())),
            Ended::Passed | Ended::Stopped => (TestStatus::Pass, None),
        };
        code = code.max(case_code);
        for (line, why) in &played.skipped {
            out.log(
                "info",
                "tool",
                &format!(
                    "{}:{line}: {why}; skipped in {}",
                    relative(root, &case.path).display(),
                    match venue {
                        Venue::Device { profile, .. } => format!("`{}`", profile.id),
                        Venue::Browser { .. } => venue.name().to_owned(),
                    }
                ),
            );
        }
        report(
            out,
            "ui",
            &case.name,
            status,
            duration_ms,
            message.as_deref(),
            |_| {},
        );
        if status != TestStatus::Pass || run.args.nocapture {
            out.case_output(&case.name, &played.output);
        }
        if run.stopped(out) {
            break;
        }
    }
    code
}

/// How a case ended that ran.
struct Played {
    ended: Ended,
    /// The steps not played on the device, by line, and why.
    skipped: Vec<(u32, String)>,
    /// What the app logged.
    output: String,
}

/// Why no case runs any more.
enum Stopped {
    Interrupted,
    /// The session ended before its app connected, with the exit code it
    /// asked for; the code and message of the last diagnostic it reported,
    /// and what it logged.
    Session {
        code: u8,
        said: Option<(String, String)>,
        output: String,
    },
}

/// A line of the session's event stream.
enum Event {
    /// The session serves the page at this URL.
    Serving(String),
    Connected,
    Log(String),
    /// A diagnostic it reported: its code and message.
    Diagnostic(String, String),
    Ended(u8),
}

fn event(line: &str) -> Option<Event> {
    let json = JsonValue::parse(line).ok()?;
    let payload = json.get("payload")?;
    let text = |name: &str| payload.get(name).and_then(JsonValue::as_str).unwrap_or("");
    Some(match json.get("type")?.as_str()? {
        "runtime" if text("connection") == "connected" => Event::Connected,
        "server" => Event::Serving(text("url").to_owned()),
        "log" => Event::Log(text("message").to_owned()),
        "diagnostic" => Event::Diagnostic(text("code").to_owned(), text("message").to_owned()),
        "summary" => Event::Ended(payload.get("exit_code")?.as_f64()? as u8),
        _ => return None,
    })
}

/// The session of one case: `viso run` of this executable, and the
/// headless browser showing its page.
struct Session {
    child: Child,
    events: Receiver<Event>,
    output: String,
    page: Option<(Child, PathBuf)>,
}

impl Session {
    fn start(root: &Path, venue: &Venue<'_>) -> Result<Session, String> {
        let exe = std::env::current_exe().map_err(|e| format!("cannot find `viso`: {e}"))?;
        let mut child = Command::new(exe)
            .args(venue.run_args())
            .current_dir(root)
            .env_remove("VISO_JSON")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .launch()
            .map_err(|e| format!("cannot start `viso run {}`: {e}", venue.target().as_str()))?;
        let stdout = child.stdout.take().expect("piped");
        let (send, events) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(event) = event(&line)
                    && send.send(event).is_err()
                {
                    return;
                }
            }
        });
        Ok(Session {
            child,
            events,
            output: String::new(),
            page: None,
        })
    }

    /// Opens the page at `url` in `browser`, headless, with a profile of its
    /// own.
    fn open(&mut self, browser: &Path, url: &str) -> Result<(), String> {
        static OPENED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = OPENED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let profile =
            std::env::temp_dir().join(format!("viso-test-browser-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&profile);
        let page = web::open_headless(browser, url, &profile)
            .map_err(|e| format!("cannot start `{}`: {e}", browser.display()))?;
        self.page = Some((page, profile));
        Ok(())
    }

    /// Takes the next event, waiting at most `wait`; `None` when none came.
    fn next(&mut self, wait: Duration) -> Result<Option<Event>, ()> {
        match self.events.recv_timeout(wait) {
            Ok(Event::Log(line)) => {
                self.output.push_str(&line);
                self.output.push('\n');
                Ok(None)
            }
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(()),
        }
    }

    /// Asks the session to end as Ctrl-C would, and waits for it; closes the
    /// browser.
    fn end(&mut self) {
        if let Some((mut page, profile)) = self.page.take() {
            let _ = page.kill();
            let _ = page.wait();
            let _ = std::fs::remove_dir_all(profile);
        }
        #[cfg(unix)]
        {
            // SAFETY: `kill` sends a signal to the child's pid, which this
            // session owns and has not reaped.
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGINT);
            }
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let deadline = Instant::now() + END_WAIT;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            let _ = self.next(POLL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.page.is_some() || matches!(self.child.try_wait(), Ok(None)) {
            self.end();
        }
    }
}

/// Plays `case` on a fresh launch of the app at `venue`.
fn play(ctx: &Context<'_>, root: &Path, venue: &Venue<'_>, case: &Case) -> Result<Played, Stopped> {
    let crashed = |why: String, output: String| Played {
        ended: Ended::Crashed(why),
        skipped: Vec::new(),
        output,
    };
    let mut session = match Session::start(root, venue) {
        Ok(session) => session,
        Err(why) => return Ok(crashed(why, String::new())),
    };
    let mut said = None;
    // The app connects once built, installed and launched; a page once
    // served and opened.
    loop {
        if interrupt::count() > 0 {
            session.end();
            return Err(Stopped::Interrupted);
        }
        match session.next(POLL) {
            Ok(Some(Event::Connected)) => break,
            Ok(Some(Event::Serving(url))) => {
                if let Venue::Browser { browser } = venue
                    && let Err(why) = session.open(browser, &url)
                {
                    session.end();
                    return Ok(crashed(why, std::mem::take(&mut session.output)));
                }
            }
            Ok(Some(Event::Diagnostic(code, message))) => said = Some((code, message)),
            Ok(Some(Event::Ended(code))) => {
                return Err(ended_early(session, said, code));
            }
            Ok(_) => {}
            Err(()) => {
                let code = session
                    .child
                    .wait()
                    .ok()
                    .and_then(|status| status.code())
                    .map_or(RUN_TEST_CRASHED.exit, |code| code as u8);
                return Err(ended_early(session, said, code));
            }
        }
    }
    let layout = ctx.layout(root);
    let kind = venue.lock();
    let endpoint = loop {
        if let Some(endpoint) = LockGuard::holder(&layout, kind).and_then(|holder| holder.endpoint)
        {
            break endpoint;
        }
        if session.next(POLL).is_err() {
            return Ok(crashed(
                "`viso run` ended before it took requests".to_owned(),
                std::mem::take(&mut session.output),
            ));
        }
    };
    let steps = Script::parse(&case.text).map_or(0, |script| script.steps.len());
    let wait = PLAY_WAIT + STEP_WAIT * steps as u32;
    let asked = ControlAsk::Scenario {
        script: case.text.clone(),
    };
    // The answer comes on its own thread so the app's log keeps arriving.
    let (send, answer) = mpsc::channel();
    thread::spawn(move || {
        let _ = send.send(control::call(&endpoint, asked, Some(wait)));
    });
    let reply = loop {
        if interrupt::count() > 0 {
            session.end();
            return Err(Stopped::Interrupted);
        }
        match answer.recv_timeout(POLL) {
            Ok(reply) => break reply,
            Err(_) => {
                let _ = session.next(Duration::ZERO);
            }
        }
    };
    session.end();
    let output = std::mem::take(&mut session.output);
    Ok(match reply {
        Ok(ControlReply::Played { lines }) => {
            let mut played = Played {
                ended: Ended::Crashed("the app's report ended before the scenario did".to_owned()),
                skipped: Vec::new(),
                output,
            };
            let mut failed = None;
            for line in &lines {
                match Reported::from_line(line) {
                    Some(Reported::Failed { line, message }) => failed = Some((line, message)),
                    Some(Reported::Skipped { line, message }) => {
                        played.skipped.push((line, message));
                    }
                    Some(Reported::Done { .. }) => {
                        played.ended = match failed.take() {
                            Some((line, message)) => Ended::Failed { line, message },
                            None => Ended::Passed,
                        };
                    }
                    _ => {}
                }
            }
            played
        }
        Ok(ControlReply::Refused { message, .. }) => crashed(message, output),
        Ok(_) => crashed("`viso run` answered another question".to_owned(), output),
        Err(why) => crashed(why, output),
    })
}

/// A session that ended before its app connected, with `code`.
fn ended_early(mut session: Session, said: Option<(String, String)>, code: u8) -> Stopped {
    let _ = session.child.wait();
    Stopped::Session {
        code: if code == SUCCESS {
            ENV_NO_DEV_SESSION.exit
        } else {
            code
        },
        said,
        output: std::mem::take(&mut session.output),
    }
}
