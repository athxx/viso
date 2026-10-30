//! `viso inspect` against a running `viso run` session, end to end:
//! the app's census, node detail and frame taken at its next frame and
//! joined with the views the session compiled, before an edit and after it
//! reloads.
//!
//! It builds a probe app against this repository and opens its window, so it
//! is ignored by default:
//! `cargo test -p viso-cli --test inspect_session -- --ignored --nocapture`.
#![cfg(unix)]

mod support;

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use support::Run;

const VIEW: &str = r#"component Probe {
    state count = 0;
    view {
        Column {
            width: 200dp;
            height: 40dp;
            node total: Text("{count}") { width: 200dp; height: 40dp }
        }
    }
}
"#;

const MAIN: &str = r#"use viso::prelude::*;

struct Probe;

impl Application for Probe {
    fn new(_cx: &mut AppCx) -> Self {
        Probe
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("probe.vs")(cx);
    }
}

fn main() {
    viso::run::<Probe>();
}
"#;

fn viso(app: &Path, args: &[&str]) -> Output {
    let target = app.parent().expect("the scratch directory").join("target");
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(app)
        .env("CARGO_TARGET_DIR", target)
        .output()
        .expect("viso runs")
}

fn query(app: &Path, selector: &str) -> Output {
    viso(app, &["inspect", "query", selector, "--json"])
}

/// The `result` line of a `viso` run that exits 0.
fn answer(app: &Path, args: &[&str]) -> String {
    let out = viso(app, args);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{args:?}: {text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    text.lines()
        .find(|line| line.contains("\"type\":\"result\""))
        .unwrap_or_else(|| panic!("{args:?}: no result: {text}"))
        .to_owned()
}

/// The `result` line of a query for `selector` once it holds `wanted`,
/// asked again while the session is still taking the app's mounts.
fn answered(app: &Path, selector: &str, wanted: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let out = query(app, selector);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let result = text
            .lines()
            .find(|line| line.contains("\"type\":\"result\""))
            .unwrap_or_default()
            .to_owned();
        if out.status.code() == Some(0) && result.contains(wanted) {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "no {wanted} for {selector}: {text}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "builds a probe app and opens its window"]
fn a_running_app_is_queried_before_and_after_an_edit() {
    let app = support::project(
        "inspect_session",
        &[
            ("src/main.rs", MAIN.as_bytes()),
            ("src/probe.vs", VIEW.as_bytes()),
        ],
    );
    let run = Run::start(&app, &["--inspect"], &[]);
    run.connected();

    let result = answered(&app, "Probe > #total", r#""name":"total""#);
    assert!(
        result.contains(r#""symbol":"probe::Probe::total""#),
        "{result}"
    );
    assert!(
        result.contains(r#""file":"src/probe.vs""#) && result.contains(r#""line":7"#),
        "{result}"
    );
    assert!(result.contains(r#""label":"0""#), "{result}");
    assert!(result.contains(r#""width":200"#), "{result}");

    // The app answers what it holds for the node and its last frame.
    let node = answer(&app, &["inspect", "node", "#total", "--json"]);
    assert!(
        node.contains(r#""properties":["text"],"state":"count""#),
        "{node}"
    );
    assert!(node.contains(r#""value":"0""#), "{node}");
    assert!(node.contains(r#""layout":{"kind":"leaf""#), "{node}");
    assert!(
        node.contains(r#""inspector":["tree","nodes","frame""#),
        "{node}"
    );
    let frame = answer(&app, &["inspect", "frame", "--json"]);
    assert!(frame.contains(r#""draw_calls":"#), "{frame}");
    assert!(
        frame.contains(r#""group":"gpu","name":"pipelines""#),
        "{frame}"
    );

    // An inspector follows the window through the edit, and the session
    // reports the same events itself.
    let target = app.parent().expect("the scratch directory").join("target");
    let follower = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(["inspect", "events", "--seconds", "8", "--json"])
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", &target)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("viso runs");
    std::thread::sleep(Duration::from_secs(1));

    // The edit reloads; the query names the node by its new name.
    std::fs::write(
        app.join("src/probe.vs"),
        VIEW.replace(
            "node total: Text { width: 200dp;",
            "node shown: Text { width: 180dp;",
        ),
    )
    .expect("probe.vs");
    run.until(Duration::from_secs(120), "the reload", |line| {
        support::dev_event(line, "runtime-commit", "applied")
    });
    let result = answered(&app, "#shown", r#""name":"shown""#);
    assert!(
        result.contains(r#""symbol":"probe::Probe::shown""#),
        "{result}"
    );
    run.until(
        Duration::from_secs(30),
        "the session's inspect events",
        |line| {
            line.contains(r#""type":"inspect""#) && line.contains(r#""cause":{"kind":"reload"}"#)
        },
    );
    let followed = follower.wait_with_output().expect("the follower ends");
    assert_eq!(followed.status.code(), Some(0));
    let json = String::from_utf8_lossy(&followed.stdout);
    assert!(
        json.lines().any(|line| line.contains(r#""type":"inspect""#)
            && line.contains(r#""cause":{"kind":"reload"}"#)),
        "{json}"
    );

    assert_eq!(run.stop(), Some(130));
}
