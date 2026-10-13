//! `viso run` across a Rust edit, end to end: a broken build leaves the
//! running app alone; a good one restarts it on the new executable, which
//! takes the old app's state back before its first frame.
//!
//! It builds a probe app against this repository and opens its window, so it
//! is ignored by default:
//! `cargo test -p viso-cli --test warm_restart -- --ignored --nocapture`.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const VIEW: &str = r#"component Probe {
    state count = 0;
    view {
        Column {
            width: 200dp;
            height: 40dp;
            Text { width: 200dp; height: 40dp; text: format("{}", count); }
        }
    }
}
"#;

/// The app: it says which process it is at launch, and keeps that across a
/// restart as its own state.
fn main_rs(edit: &str) -> String {
    format!(
        r#"use viso::prelude::*;

struct Probe;

impl Application for Probe {{
    fn new(cx: &mut AppCx) -> Self {{
        let pid = std::process::id();
        eprintln!("probe launched {{pid}}");
        cx.dev_state(
            "probe",
            move || pid.to_string().into_bytes(),
            |bytes| eprintln!("probe restored {{}}", String::from_utf8_lossy(bytes)),
        );
        Probe
    }}

    fn build(&mut self, cx: &mut BuildCx<'_>) {{
        viso::view!("probe.vs")(cx);
    }}
}}

fn main() {{
    viso::run::<Probe>();
}}
{edit}
"#
    )
}

/// The probe project, written fresh under the test's scratch directory and
/// depending on this repository's crates by path.
fn project() -> PathBuf {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = repo.canonicalize().expect("the repository");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("warm_restart");
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).expect("the project directory");
    let crate_path = |name: &str| repo.join("crates").join(name).display().to_string();
    let manifest = format!(
        "[package]\nname = \"warm-restart-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         publish = false\n\n[dependencies]\nviso = {{ path = {:?} }}\n\
         viso-ui = {{ path = {:?} }}\nviso-view = {{ path = {:?} }}\n\n[workspace]\n",
        crate_path("viso"),
        crate_path("ui"),
        crate_path("view"),
    );
    std::fs::write(app.join("Cargo.toml"), manifest).expect("Cargo.toml");
    std::fs::write(app.join("Viso.toml"), "[package]\nname = \"probe\"\n").expect("Viso.toml");
    std::fs::write(app.join("src/probe.vs"), VIEW).expect("probe.vs");
    std::fs::write(app.join("src/main.rs"), main_rs("")).expect("main.rs");
    app
}

/// `viso run --json` in `app`, and the lines it writes.
struct Run {
    child: Child,
    lines: Receiver<String>,
}

impl Run {
    fn start(app: &Path) -> Run {
        let target = app.parent().expect("the scratch directory").join("target");
        let mut child = Command::new(env!("CARGO_BIN_EXE_viso"))
            .args(["run", "--json"])
            .current_dir(app)
            .env("CARGO_TARGET_DIR", target)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("viso runs");
        let stdout = child.stdout.take().expect("piped");
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                println!("{line}");
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Run { child, lines }
    }

    /// The next line `matches` accepts, within `within`.
    fn until(&self, within: Duration, what: &str, matches: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) if matches(&line) => return line,
                Ok(_) => {}
                Err(_) => panic!("no {what} within {within:?}"),
            }
        }
    }

    /// The pid the probe printed after `said` (`launched` or `restored`).
    fn probe(&self, within: Duration, said: &str) -> u32 {
        let line = self.until(within, &format!("probe {said}"), |line| {
            probe_pid(line, said).is_some()
        });
        probe_pid(&line, said).expect("matched")
    }

    /// Stops the session as Ctrl-C does; its exit code.
    fn stop(mut self) -> Option<i32> {
        let pid = libc::pid_t::try_from(self.child.id()).expect("a pid");
        // SAFETY: a plain call on the child not yet reaped.
        unsafe { libc::kill(pid, libc::SIGINT) };
        self.child.wait().ok()?.code()
    }
}

/// The pid in `line` if it is the probe's log saying `said`.
fn probe_pid(line: &str, said: &str) -> Option<u32> {
    let marker = format!("\"message\":\"probe {said} ");
    let at = line.find(&marker)? + marker.len();
    line[at..].split('"').next()?.parse().ok()
}

/// Whether the process `pid` runs.
fn alive(pid: u32) -> bool {
    let pid = libc::pid_t::try_from(pid).expect("a pid");
    // SAFETY: signal 0 only checks the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn dev_event(line: &str, outcome: &str) -> bool {
    line.contains("\"type\":\"dev\"")
        && line.contains("\"stage\":\"warm-restart\"")
        && line.contains(&format!("\"outcome\":\"{outcome}\""))
}

#[test]
#[ignore = "builds a probe app and opens its window"]
fn a_rust_edit_restarts_the_app_with_its_state_and_a_broken_one_keeps_it() {
    let app = project();
    let run = Run::start(&app);
    let first = run.probe(Duration::from_secs(900), "launched");
    run.until(Duration::from_secs(60), "the dev channel", |line| {
        line.contains("dev runtime connected")
    });

    // A build that fails: the app keeps running, nothing restarts.
    std::fs::write(app.join("src/main.rs"), main_rs("fn broken() {")).expect("main.rs");
    let rejected = run.until(Duration::from_secs(300), "the rejected build", |line| {
        dev_event(line, "rejected") || dev_event(line, "warm_restarted")
    });
    assert!(dev_event(&rejected, "rejected"), "{rejected}");
    assert!(alive(first), "the old app runs on");

    // A build that succeeds: the new app takes the old one's state back.
    // The app's own lines and the session's events arrive in either order.
    std::fs::write(app.join("src/main.rs"), main_rs("// edited")).expect("main.rs");
    let (mut second, mut restored, mut restarted) = (None, None, None);
    let deadline = Instant::now() + Duration::from_secs(300);
    while second.is_none() || restored.is_none() || restarted.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = run.lines.recv_timeout(left).unwrap_or_else(|_| {
            panic!("the restart did not land: {second:?} {restored:?} {restarted:?}")
        });
        second = second.or(probe_pid(&line, "launched"));
        restored = restored.or(probe_pid(&line, "restored"));
        if dev_event(&line, "warm_restarted") {
            restarted = Some(line);
        }
    }
    let (second, restarted) = (second.expect("seen"), restarted.expect("seen"));
    assert_ne!(second, first, "a new process");
    assert_eq!(restored, Some(first), "the old process's state");
    assert!(restarted.contains("\"restored\":true"), "{restarted}");
    assert!(
        restarted.contains("\"patch_class\":\"WARM_RESTART_REQUIRED\""),
        "{restarted}"
    );
    assert!(!alive(first), "the old app is gone");

    assert_eq!(run.stop(), Some(130));
    assert!(!alive(second), "the session took the app with it");
}
