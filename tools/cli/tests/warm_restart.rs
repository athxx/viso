//! `viso run` across a Rust edit, end to end: a broken build leaves the
//! running app alone; a good one restarts it on the new executable, which
//! takes the old app's state back before its first frame.
//!
//! It builds a probe app against this repository and opens its window, so it
//! is ignored by default:
//! `cargo test -p viso-cli --test warm_restart -- --ignored --nocapture`.
#![cfg(unix)]

mod support;

use std::time::{Duration, Instant};

use support::Run;

const VIEW: &str = r#"component Probe {
    state count = 0;
    view {
        Column {
            width: 200dp;
            height: 40dp;
            Text("{count}") { width: 200dp; height: 40dp }
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

/// The probe project, its `main.rs` unedited.
fn project() -> std::path::PathBuf {
    support::project(
        "warm_restart",
        &[
            ("src/probe.vs", VIEW.as_bytes()),
            ("src/main.rs", main_rs("").as_bytes()),
        ],
    )
}

/// The pid the probe printed after `said` (`launched` or `restored`).
fn probe(run: &Run, within: Duration, said: &str) -> u32 {
    let line = run.until(within, &format!("probe {said}"), |line| {
        probe_pid(line, said).is_some()
    });
    probe_pid(&line, said).expect("matched")
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
    support::dev_event(line, "warm-restart", outcome)
}

#[test]
#[ignore = "builds a probe app and opens its window"]
fn a_rust_edit_restarts_the_app_with_its_state_and_a_broken_one_keeps_it() {
    let app = project();
    let run = Run::start(&app, &[], &[]);
    let first = probe(&run, Duration::from_secs(900), "launched");
    run.connected();

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
