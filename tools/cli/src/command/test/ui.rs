//! The `ui` domain: each scenario under `tests/ui/` played by its own run of
//! a headless build of the app (`Viso_CLI.md` §22.2).
//!
//! The scenarios are parsed first, so one that does not parse fails at its
//! line without a build. The app is then built headless and every scenario
//! runs in a process of its own, up to `--jobs` at once, in a fresh directory
//! of its own under `target/viso/test/`: its surface (the scenario's, else
//! `[test.surface]`), the scenario and the path of its report reach the run
//! in the environment. A case passes when its report ends with every step
//! passed; a failed step fails it at the step's line; a run that ends
//! without its report crashed. What a case wrote is shown when it fails, or
//! with `--nocapture`; the directory of a passed case is removed.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use viso_dsl::Diagnostic;
use viso_project::Target;
use viso_view::headless::{
    FRAMES_ENV, HeadlessSurface, REPORT_ENV, Reported, SCRIPT_ENV, SURFACE_ENV, Script,
};

use super::{Run, TAPE, invalid, line_range, relative, report, scenarios};
use crate::args::TestTarget;
use crate::build::cargo::ProcessTree;
use crate::command::build::{self as build_command, Check};
use crate::command::check::Checked;
use crate::context::Context;
use crate::error::{
    DIAGNOSTICS, ENV_TEST_INPUT, ENV_TEST_TARGET, INTERRUPTED, RUN_TEST_CRASHED, RUNTIME, SUCCESS,
    TEST_EXPECTATION,
};
use crate::interrupt;
use crate::output::{Output, Source, TestStatus};

/// The directory below the project root that holds UI scenarios.
const SCENARIOS: &str = "tests/ui";

/// How often a case's run is checked for its end, Ctrl-C and `--fail-fast`.
const POLL: Duration = Duration::from_millis(10);

/// The variables a case's run must not inherit: the launch of a headless
/// run, and a dev session's.
const WITHHELD: &[&str] = &[SURFACE_ENV, FRAMES_ENV, SCRIPT_ENV, REPORT_ENV];

/// A scenario that parsed, to play.
struct Case {
    name: String,
    path: PathBuf,
    text: String,
    surface: HeadlessSurface,
}

/// How a case's run ended.
enum Ended {
    Passed,
    /// The step on `line` failed.
    Failed {
        line: u32,
        message: String,
    },
    /// The run did not report its end.
    Crashed(String),
    /// Ctrl-C or `--fail-fast` stopped it.
    Stopped,
}

/// A case's run, and what it wrote.
struct Played {
    index: usize,
    ended: Ended,
    duration: Duration,
    output: String,
    dir: PathBuf,
}

pub(super) fn run(ctx: &Context<'_>, checked: &Checked, run: &Run<'_>, out: &mut Output) -> u8 {
    let root = &checked.project.root;
    if let Some(target) = run.args.target
        && target != TestTarget::Headless
    {
        if run.every
            && matches!(
                target,
                TestTarget::WebGpu | TestTarget::WebDom | TestTarget::WebHybrid
            )
        {
            return SUCCESS;
        }
        return run.nothing(
            out,
            "ui",
            ENV_TEST_TARGET,
            &format!(
                "UI scenarios run `headless` in this build of viso, not on `{}`",
                target.target()
            ),
        );
    }
    let dir = root.join(SCENARIOS);
    let planned = match &run.args.scenario {
        Some(name) => {
            let path = dir.join(format!("{name}.{TAPE}"));
            if !path.is_file() {
                return run.nothing(
                    out,
                    "ui",
                    ENV_TEST_INPUT,
                    &format!("no scenario `{name}`: `{SCENARIOS}/{name}.{TAPE}` does not exist"),
                );
            }
            vec![(name.clone(), path)]
        }
        None if !dir.is_dir() => {
            return run.nothing(
                out,
                "ui",
                ENV_TEST_INPUT,
                &format!("no `{SCENARIOS}` directory"),
            );
        }
        None => match scenarios(&dir) {
            Ok(found) => found
                .into_iter()
                .filter(|(name, _)| run.filter.keeps(name))
                .collect(),
            Err(error) => {
                return out.failure(
                    ENV_TEST_INPUT,
                    &format!("cannot read `{SCENARIOS}`: {error}"),
                    &[],
                );
            }
        },
    };
    if planned.is_empty() {
        let why = match &run.filter.pattern {
            Some(pattern) => format!("no scenario in `{SCENARIOS}` matches `{pattern}`"),
            None => format!("`{SCENARIOS}` holds no `.{TAPE}` scenario"),
        };
        return run.nothing(out, "ui", ENV_TEST_INPUT, &why);
    }

    // Every scenario is read and parsed before anything is built.
    let (width, height, scale) = checked.project.manifest.test.surface.resolved();
    let surface = HeadlessSurface {
        width,
        height,
        scale: scale.get(),
    };
    let mut code = SUCCESS;
    let mut cases = Vec::new();
    for (name, path) in planned {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                return out.failure(
                    ENV_TEST_INPUT,
                    &format!("cannot read `{}`: {error}", relative(root, &path).display()),
                    &[],
                );
            }
        };
        match Script::parse(&text) {
            Ok(script) => cases.push(Case {
                surface: script.surface.unwrap_or(surface),
                name,
                path,
                text,
            }),
            Err(error) => {
                let source = Source::new(&path, root, &text);
                invalid(
                    out,
                    Some(&source),
                    Some(line_range(&text, error.line)),
                    &error.message,
                );
                report(
                    out,
                    "ui",
                    &name,
                    TestStatus::Fail,
                    0,
                    Some(&error.message),
                    |_| {},
                );
                code = DIAGNOSTICS;
            }
        }
        if run.stopped(out) {
            return code;
        }
    }
    if cases.is_empty() {
        return code;
    }

    let built = match build_command::stage(ctx, Target::Headless, None, Check::Done, out) {
        Ok(built) => built,
        Err(failed) => return code.max(failed),
    };
    let Some(exe) = built
        .staged
        .artifacts
        .iter()
        .find(|(kind, _)| *kind == "binary")
        .map(|(_, path)| built.staged.out.join(path))
    else {
        return code.max(out.failure(
            RUN_TEST_CRASHED,
            "the headless build has no executable",
            &[],
        ));
    };
    let work = built
        .layout
        .tests()
        .join(std::process::id().to_string())
        .join("ui");
    let jobs = ctx
        .global
        .jobs
        .map(usize::from)
        .or_else(|| thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1)
        .max(1);

    interrupt::catch();
    let stop = Arc::new(Mutex::new(false));
    let (tx, rx) = mpsc::channel::<Played>();
    let queue = Arc::new(Mutex::new(0..cases.len()));
    let cases = Arc::new(cases);
    let workers: Vec<_> = (0..jobs.min(cases.len()))
        .map(|_| {
            let (tx, queue, cases, stop) = (tx.clone(), queue.clone(), cases.clone(), stop.clone());
            let (exe, work) = (exe.clone(), work.clone());
            thread::spawn(move || {
                loop {
                    let Some(index) = queue.lock().unwrap().next() else {
                        break;
                    };
                    let played = play(&exe, &work, index, &cases[index], &stop);
                    if tx.send(played).is_err() {
                        break;
                    }
                }
            })
        })
        .collect();
    drop(tx);

    // Reported in name order, each as soon as the cases before it are.
    let mut done: Vec<Option<Played>> = (0..cases.len()).map(|_| None).collect();
    let mut next = 0;
    loop {
        if interrupt::count() > 0 || run.stopped(out) {
            *stop.lock().unwrap() = true;
        }
        match rx.recv_timeout(POLL) {
            Ok(played) => {
                let index = played.index;
                done[index] = Some(played);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        while let Some(played) = done.get_mut(next).and_then(Option::take) {
            code = code.max(reported(out, root, &cases[next], played, run));
            next += 1;
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    let _ = fs::remove_dir(&work);
    if let Some(parent) = work.parent() {
        let _ = fs::remove_dir(parent);
    }
    if interrupt::count() > 0 {
        return INTERRUPTED;
    }
    code
}

/// Plays `case` in a run of `exe` in a fresh directory under `work`.
fn play(exe: &Path, work: &Path, index: usize, case: &Case, stop: &Mutex<bool>) -> Played {
    let started = Instant::now();
    let dir = work.join(&case.name);
    let played = |ended, output| Played {
        index,
        ended,
        duration: started.elapsed(),
        output,
        dir: dir.clone(),
    };
    if *stop.lock().unwrap() {
        return played(Ended::Stopped, String::new());
    }
    let _ = fs::remove_dir_all(&dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        return played(
            Ended::Crashed(format!("cannot create `{}`: {error}", dir.display())),
            String::new(),
        );
    }
    let report = dir.join("report.jsonl");
    let log = dir.join("output.log");
    let output = match File::create(&log).and_then(|file| Ok((file.try_clone()?, file))) {
        Ok(files) => files,
        Err(error) => {
            return played(
                Ended::Crashed(format!("cannot create `{}`: {error}", log.display())),
                String::new(),
            );
        }
    };
    let script = case
        .path
        .canonicalize()
        .unwrap_or_else(|_| case.path.clone());
    let mut command = Command::new(exe);
    for name in WITHHELD {
        command.env_remove(name);
    }
    command
        .env(SURFACE_ENV, case.surface.to_env())
        .env(SCRIPT_ENV, &script)
        .env(REPORT_ENV, &report)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output.0))
        .stderr(Stdio::from(output.1));
    let mut tree = match ProcessTree::spawn(&mut command) {
        Ok(tree) => tree,
        Err(error) => {
            return played(
                Ended::Crashed(format!("cannot run `{}`: {error}", exe.display())),
                String::new(),
            );
        }
    };
    let status = loop {
        if *stop.lock().unwrap() {
            tree.kill();
            let _ = tree.child.wait();
            return played(Ended::Stopped, read(&log));
        }
        match tree.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(POLL),
            Err(error) => {
                tree.kill();
                return played(
                    Ended::Crashed(format!("cannot wait for the run: {error}")),
                    read(&log),
                );
            }
        }
    };
    let ended = ended(&read(&report), status);
    played(ended, read(&log))
}

/// How a run that exited with `status` ended, by its report.
fn ended(report: &str, status: ExitStatus) -> Ended {
    let mut failed = None;
    let mut finished = false;
    for line in report.lines() {
        match Reported::from_line(line) {
            Some(Reported::Failed { line, message }) => failed = Some((line, message)),
            Some(Reported::Done { .. }) => finished = true,
            Some(Reported::Passed { .. }) | None => {}
        }
    }
    match (finished, failed) {
        (true, Some((line, message))) => Ended::Failed { line, message },
        (true, None) if status.success() => Ended::Passed,
        (true, None) => Ended::Crashed(format!(
            "the run reported its end, then exited with {status}"
        )),
        (false, _) => Ended::Crashed(format!(
            "the run exited with {status} before it played every step"
        )),
    }
}

fn read(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Reports a played case; the exit code its end asks for.
fn reported(out: &mut Output, root: &Path, case: &Case, played: Played, run: &Run<'_>) -> u8 {
    let duration_ms = played.duration.as_millis() as u64;
    let (status, message, code) = match &played.ended {
        Ended::Passed => (TestStatus::Pass, None, SUCCESS),
        Ended::Failed { line, message } => {
            let source = Source::new(&case.path, root, &case.text);
            out.source(
                Some(&source),
                &[],
                &Diagnostic::error(
                    TEST_EXPECTATION.name,
                    line_range(&case.text, *line),
                    message.clone(),
                ),
            );
            (
                TestStatus::Fail,
                Some(format!(
                    "{}:{line}: {message}",
                    relative(root, &case.path).display()
                )),
                DIAGNOSTICS,
            )
        }
        Ended::Crashed(why) => {
            out.error(RUN_TEST_CRASHED.name, &format!("{}: {why}", case.name), &[]);
            (TestStatus::Fail, Some(why.clone()), RUNTIME)
        }
        // A stopped case is not reported: the run did not finish it.
        Ended::Stopped => {
            let _ = fs::remove_dir_all(&played.dir);
            return SUCCESS;
        }
    };
    report(
        out,
        "ui",
        &case.name,
        status,
        duration_ms,
        message.as_deref(),
        |_| {},
    );
    if status == TestStatus::Pass {
        if run.args.nocapture {
            out.case_output(&case.name, &played.output);
        }
        let _ = fs::remove_dir_all(&played.dir);
    } else {
        out.case_output(&case.name, &played.output);
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_and_a_headless_run_agree_on_surfaces() {
        use viso_project::{SurfaceScale, TestSurface};
        assert_eq!(TestSurface::MAX_EDGE, viso_view::headless::MAX_EDGE);
        assert_eq!(SurfaceScale::MAX, viso_view::headless::MAX_SCALE);
        let default = HeadlessSurface::default();
        assert_eq!(
            (
                TestSurface::WIDTH,
                TestSurface::HEIGHT,
                SurfaceScale::ONE.get()
            ),
            (default.width, default.height, default.scale)
        );
    }

    #[cfg(unix)]
    fn exited(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }

    #[cfg(unix)]
    #[test]
    fn a_run_ends_as_its_report_says() {
        let passed = format!(
            "{}\n{}\n",
            Reported::Passed { line: 1 }.to_line(),
            Reported::Done { frames: 9 }.to_line()
        );
        assert!(matches!(ended(&passed, exited(0)), Ended::Passed));
        let failed = format!(
            "{}\n{}\n",
            Reported::Failed {
                line: 4,
                message: "no \"Save\" is shown".into()
            }
            .to_line(),
            Reported::Done { frames: 9 }.to_line()
        );
        assert!(matches!(
            ended(&failed, exited(0)),
            Ended::Failed { line: 4, .. }
        ));
        let cut = Reported::Passed { line: 1 }.to_line();
        assert!(matches!(ended(&cut, exited(101)), Ended::Crashed(_)));
        assert!(matches!(ended(&passed, exited(3)), Ended::Crashed(_)));
    }
}
