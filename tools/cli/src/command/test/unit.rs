//! The `unit` domain: the Cargo package's own tests, through `cargo test`.
//!
//! What the compiler says arrives as diagnostics, as in `viso build`; each
//! test the harness reports is a case — a passed or ignored one as soon as
//! it ends, a failed one once its binary has printed what it wrote.
//! `--filter` and `--exact` go to the harness; without `--fail-fast` every
//! test binary runs whatever another's tests did.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use viso_ende::JsonValue;
use viso_project::Profile;

use super::{Run, report};
use crate::build::cargo::{self, RustDiagnostic};
use crate::command::check::Checked;
use crate::context::Context;
use crate::error::{
    BUILD_FAILED, DIAGNOSTICS, ENV_CARGO, ENV_CARGO_MANIFEST, INTERRUPTED, RUN_TEST_CRASHED,
    SUCCESS,
};
use crate::interrupt;
use crate::output::{Output, TestStatus};
use crate::process::ProcessTree;

/// How often the run is checked for Ctrl-C.
const POLL: Duration = Duration::from_millis(50);

/// What `cargo test` writes, line by line.
enum Line {
    Out(String),
    Err(String),
}

pub(super) fn run(ctx: &Context<'_>, checked: &Checked, run: &Run<'_>, out: &mut Output) -> u8 {
    let root = &checked.project.root;
    let manifest = root.join("Cargo.toml");
    if !manifest.is_file() {
        return run.nothing(
            out,
            "unit",
            ENV_CARGO_MANIFEST,
            "no `Cargo.toml` at the project root",
        );
    }
    let manifest = manifest.canonicalize().unwrap_or(manifest);
    let mut command = Command::new(cargo::program());
    command
        .arg("test")
        .args(ctx.cargo_args())
        .args(ctx.cargo_build_args())
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--message-format=json");
    if !run.args.fail_fast {
        command.arg("--no-fail-fast");
    }
    command.arg("--");
    if let Some(pattern) = &run.filter.pattern {
        command.arg(pattern);
        if run.filter.exact {
            command.arg("--exact");
        }
    }
    if run.args.nocapture {
        command.arg("--nocapture");
    }
    if let Some(jobs) = ctx.global.jobs {
        command.arg(format!("--test-threads={jobs}"));
    }
    if let Some(dir) = ctx.target_dir() {
        command.env("CARGO_TARGET_DIR", dir);
    }
    command
        .env("VISO_PROFILE", Profile::Dev.as_str())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    out.progress("test", "testing the Cargo package");

    interrupt::catch();
    let mut tree = match ProcessTree::spawn(&mut command) {
        Ok(tree) => tree,
        Err(error) => return out.failure(ENV_CARGO, &format!("cannot run `cargo`: {error}"), &[]),
    };
    let (tx, rx) = mpsc::channel();
    let readers: Vec<_> = [
        tree.child
            .stdout
            .take()
            .map(|o| (Box::new(o) as Box<dyn std::io::Read + Send>, true)),
        tree.child
            .stderr
            .take()
            .map(|e| (Box::new(e) as Box<dyn std::io::Read + Send>, false)),
    ]
    .into_iter()
    .flatten()
    .map(|(stream, stdout)| {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else {
                    break;
                };
                let line = if stdout {
                    Line::Out(line)
                } else {
                    Line::Err(line)
                };
                if tx.send(line).is_err() {
                    break;
                }
            }
        })
    })
    .collect();
    drop(tx);

    let mut harness = Harness::default();
    loop {
        if interrupt::count() > 0 {
            tree.kill();
        }
        match rx.recv_timeout(POLL) {
            Ok(Line::Out(line)) => harness.take(&line, &manifest, root, run, out),
            Ok(Line::Err(line)) => out.log("info", "tool", &line),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    for reader in readers {
        let _ = reader.join();
    }
    let status = tree.child.wait();
    harness.flush(out);
    if interrupt::count() > 0 {
        return INTERRUPTED;
    }
    match status {
        Ok(status) if status.success() => SUCCESS,
        _ if harness.compile_errors > 0 => out.failure(
            BUILD_FAILED,
            "the Cargo package's tests do not compile",
            &[],
        ),
        _ if harness.failed > 0 => DIAGNOSTICS,
        Ok(status) => out.failure(
            RUN_TEST_CRASHED,
            &format!("`cargo test` exited with {status} without a failed test"),
            &[],
        ),
        Err(error) => out.failure(
            RUN_TEST_CRASHED,
            &format!("cannot wait for `cargo test`: {error}"),
            &[],
        ),
    }
}

/// What the harness output read so far says.
#[derive(Default)]
struct Harness {
    /// Failed tests whose output their binary has not finished printing.
    failing: Vec<(String, String)>,
    /// The failed test whose output is being read.
    capturing: Option<usize>,
    failed: u64,
    compile_errors: u64,
}

impl Harness {
    /// Takes one line of `cargo test`'s standard output.
    fn take(&mut self, line: &str, manifest: &Path, root: &Path, run: &Run<'_>, out: &mut Output) {
        if line.starts_with('{')
            && let Ok(message) = JsonValue::parse(line)
        {
            if message.get("reason").and_then(JsonValue::as_str) == Some("compiler-message")
                && let Some(diagnostic) = message
                    .get("message")
                    .and_then(|m| RustDiagnostic::read(m, manifest))
            {
                if diagnostic.severity == "error" {
                    self.compile_errors += 1;
                }
                out.rust(root, &diagnostic);
            }
            return;
        }
        if let Some((name, outcome)) = case(line) {
            self.capturing = None;
            match outcome {
                Outcome::Passed => report(out, "unit", name, TestStatus::Pass, 0, None, |_| {}),
                Outcome::Ignored(why) => {
                    report(out, "unit", name, TestStatus::Skip, 0, why, |_| {});
                }
                Outcome::Failed => {
                    self.failed += 1;
                    self.failing.push((name.to_owned(), String::new()));
                }
            }
            return;
        }
        if let Some(name) = line
            .strip_prefix("---- ")
            .and_then(|rest| rest.strip_suffix(" stdout ----"))
        {
            self.capturing = self.failing.iter().position(|(n, _)| n == name);
            return;
        }
        if line == "failures:" {
            self.capturing = None;
            return;
        }
        if line.starts_with("test result: ") {
            self.flush(out);
            return;
        }
        match self.capturing {
            Some(at) => {
                let text = &mut self.failing[at].1;
                text.push_str(line);
                text.push('\n');
            }
            None if run.args.nocapture && !line.starts_with("running ") && !line.is_empty() => {
                out.log("info", "test", line);
            }
            None => {}
        }
    }

    /// Reports the failed tests read so far, each with what it wrote.
    fn flush(&mut self, out: &mut Output) {
        self.capturing = None;
        for (name, text) in self.failing.drain(..) {
            let text = text.trim();
            let message = (!text.is_empty()).then_some(text);
            report(out, "unit", &name, TestStatus::Fail, 0, message, |_| {});
            out.case_output(&name, text);
        }
    }
}

/// How the harness says a test ended.
#[derive(Debug, PartialEq, Eq)]
enum Outcome<'a> {
    Passed,
    Failed,
    /// Ignored, and why when it says.
    Ignored(Option<&'a str>),
}

/// The test a harness line reports the end of: `test <name> ... <outcome>`.
fn case(line: &str) -> Option<(&str, Outcome<'_>)> {
    let rest = line.strip_prefix("test ")?;
    let (name, outcome) = rest.rsplit_once(" ... ")?;
    let outcome = match outcome {
        "ok" => Outcome::Passed,
        "FAILED" => Outcome::Failed,
        "ignored" => Outcome::Ignored(None),
        other => Outcome::Ignored(Some(other.strip_prefix("ignored, ")?)),
    };
    let name = name.strip_suffix(" - should panic").unwrap_or(name);
    Some((name, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_harness_line_names_the_test_and_how_it_ended() {
        assert_eq!(
            case("test tests::adds ... ok"),
            Some(("tests::adds", Outcome::Passed))
        );
        assert_eq!(
            case("test tests::panics - should panic ... FAILED"),
            Some(("tests::panics", Outcome::Failed))
        );
        assert_eq!(
            case("test slow ... ignored, takes a minute"),
            Some(("slow", Outcome::Ignored(Some("takes a minute"))))
        );
        assert_eq!(
            case("test src/lib.rs - answer (line 3) ... ok"),
            Some(("src/lib.rs - answer (line 3)", Outcome::Passed))
        );
        assert_eq!(case("test result: ok. 1 passed; 0 failed"), None);
        assert_eq!(case("running 2 tests"), None);
    }
}
