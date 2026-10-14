//! The `game` domain: each scenario under `tests/game/` run headless on the
//! package's systems: its input tape, or the one `--tape` names, for
//! `--frames` frames on a world seeded with the tape's seed or `--seed`, each
//! tick's `@probe` states traced and checked against the scenario's
//! expectations.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use viso_dsl::scenario::{self, DEFAULT_SEED, GameRun, RunError, Scenario, read_tape};
use viso_dsl::{Diagnostic, TextRange, TextSize};
use viso_ende::JsonWriter;

use super::{Run, TAPE, invalid, line_range, relative, report, scenarios};
use crate::command::check::{self, Checked};
use crate::error::{DIAGNOSTICS, ENV_TEST_INPUT, SUCCESS, TEST_EXPECTATION, TEST_INPUT_INVALID};
use crate::output::{Output, Source, TestStatus};

/// The directory below the project root that holds game scenarios.
const SCENARIOS: &str = "tests/game";

/// The faults of one run reported one by one; the rest are counted.
const FAULTS_SHOWN: usize = 8;

/// A scenario to run: its name and its file, if it has one.
struct Planned {
    name: String,
    file: Option<PathBuf>,
}

pub(super) fn run(checked: &Checked, run: &Run<'_>, out: &mut Output) -> u8 {
    let root = &checked.project.root;
    let planned = match plan(root, run) {
        Ok(planned) => planned,
        Err(Nothing::Missing(why)) => {
            return run.nothing(out, "game", ENV_TEST_INPUT, &why);
        }
        Err(Nothing::Unreadable(why)) => return out.failure(ENV_TEST_INPUT, &why, &[]),
    };
    let tick_rate = check::profile(&checked.project, &check::Options::default()).tick_rate;
    let mut code = SUCCESS;
    for planned in &planned {
        match scenario(root, planned, run, tick_rate, checked, out) {
            Ok(true) => {}
            Ok(false) => code = code.max(DIAGNOSTICS),
            Err(environment) => code = code.max(environment),
        }
        if run.stopped(out) {
            break;
        }
    }
    code
}

/// Why the domain runs no scenario.
enum Nothing {
    /// There is none to run, or none by that name.
    Missing(String),
    /// The directory cannot be read.
    Unreadable(String),
}

/// The scenarios the command line asks for.
fn plan(root: &Path, run: &Run<'_>) -> Result<Vec<Planned>, Nothing> {
    let args = run.args;
    let dir = root.join(SCENARIOS);
    let named = |name: &str| dir.join(format!("{name}.{TAPE}"));
    if let Some(name) = &args.scenario {
        let file = named(name);
        if !file.is_file() && args.tape.is_none() {
            return Err(Nothing::Missing(format!(
                "no scenario `{name}`: `{SCENARIOS}/{name}.{TAPE}` does not exist"
            )));
        }
        let file = file.is_file().then_some(file);
        return Ok(vec![Planned {
            name: name.clone(),
            file,
        }]);
    }
    if let Some(tape) = &args.tape {
        let name = tape
            .file_stem()
            .map_or_else(|| "tape".to_owned(), |s| s.to_string_lossy().into_owned());
        return Ok(vec![Planned { name, file: None }]);
    }
    if !dir.is_dir() {
        return Err(Nothing::Missing(format!("no `{SCENARIOS}` directory")));
    }
    let planned: Vec<Planned> = scenarios(&dir)
        .map_err(|error| Nothing::Unreadable(format!("cannot read `{SCENARIOS}`: {error}")))?
        .into_iter()
        .filter(|(name, _)| run.filter.keeps(name))
        .map(|(name, path)| Planned {
            name,
            file: Some(path),
        })
        .collect();
    if planned.is_empty() {
        return Err(Nothing::Missing(match &run.filter.pattern {
            Some(pattern) => format!("no scenario in `{SCENARIOS}` matches `{pattern}`"),
            None => format!("`{SCENARIOS}` holds no `.{TAPE}` scenario"),
        }));
    }
    Ok(planned)
}

/// Runs one scenario and reports it; whether it passed, or the exit code
/// of an input that could not be read.
fn scenario(
    root: &Path,
    planned: &Planned,
    run: &Run<'_>,
    tick_rate: u32,
    checked: &Checked,
    out: &mut Output,
) -> Result<bool, u8> {
    let args = run.args;
    let read = |path: &Path| {
        fs::read(path)
            .map_err(|error| format!("cannot read `{}`: {error}", relative(root, path).display()))
    };
    let text = match &planned.file {
        Some(path) => match read(path) {
            Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Err(message) => {
                return Err(out.failure(ENV_TEST_INPUT, &message, &[]));
            }
        },
        None => None,
    };
    let source = planned
        .file
        .as_deref()
        .zip(text.as_deref())
        .map(|(path, text)| Source::new(path, root, text));
    let mut scenario = match &text {
        Some(text) => match Scenario::parse(text, DEFAULT_SEED, tick_rate) {
            Ok(scenario) => scenario,
            Err(error) => {
                let at = error.line.map(|line| line_range(text, line));
                invalid(out, source.as_ref(), at, &error.message);
                return Ok(false);
            }
        },
        None => Scenario::empty(DEFAULT_SEED, tick_rate),
    };
    if let Some(path) = &args.tape {
        let bytes = match read(path) {
            Ok(bytes) => bytes,
            Err(message) => {
                return Err(out.failure(ENV_TEST_INPUT, &message, &[]));
            }
        };
        match read_tape(&bytes, DEFAULT_SEED, tick_rate) {
            Ok(tape) => scenario.tape = tape,
            Err(error) => {
                let name = relative(root, path).display().to_string();
                let message = match error.line {
                    Some(line) => format!("{name}:{line}: {}", error.message),
                    None => format!("{name}: {}", error.message),
                };
                out.failure(TEST_INPUT_INVALID, &message, &[]);
                return Ok(false);
            }
        }
    }
    if let Some(seed) = args.seed {
        scenario.tape.seed = seed;
    }
    let through = scenario.expectations.iter().map(|e| e.tick + 1).max();
    let frames = args
        .frames
        .unwrap_or_else(|| scenario.tape.ticks.max(through.unwrap_or(0)));

    let started = Instant::now();
    let program = &checked.package.hir.behavior;
    let name = planned.name.as_str();
    let result = scenario::run(program, &scenario, frames, |tick, probes| {
        out.trace(name, tick, probes);
    });
    let duration_ms = started.elapsed().as_millis() as u64;
    let run = match result {
        Ok(run) => run,
        Err(error) => {
            let message = error.to_string();
            match &error {
                RunError::Start(fault) => out.error(fault.code, &message, &[]),
                RunError::UnknownProbe(e) => {
                    let at = text.as_deref().map(|t| line_range(t, e.line));
                    invalid(out, source.as_ref(), at, &message);
                }
                RunError::Build(_) | RunError::Tape(_) => {
                    out.failure(TEST_INPUT_INVALID, &message, &[]);
                }
            }
            report_run(out, name, false, duration_ms, Some(&message), None);
            return Ok(false);
        }
    };
    let systems = &program.systems;
    for fault in run.faults.iter().take(FAULTS_SHOWN) {
        let system = systems.get(fault.system).map_or("?", |s| {
            program.components[s.component as usize].name.as_str()
        });
        let message = format!(
            "system `{system}` faulted at tick {}: {}",
            fault.tick, fault.fault.message
        );
        out.error(fault.code, &message, &[]);
    }
    if run.faults.len() > FAULTS_SHOWN {
        let message = format!("{} more faults", run.faults.len() - FAULTS_SHOWN);
        out.error(run.faults[FAULTS_SHOWN].code, &message, &[]);
    }
    let mut first = None;
    for failure in &run.failures {
        let e = &failure.expectation;
        let message = match &failure.actual {
            Some(actual) => format!(
                "after tick {}: expected `{}` == {}, found {actual}",
                e.tick, e.probe, e.value
            ),
            None => format!(
                "after tick {}: expected `{}` == {}, but the run ended after {} frames",
                e.tick, e.probe, e.value, run.ticks
            ),
        };
        let at = text
            .as_deref()
            .map_or(TextRange::empty(TextSize::new(0)), |t| {
                line_range(t, e.line)
            });
        out.source(
            source.as_ref(),
            &[],
            &Diagnostic::error(TEST_EXPECTATION.name, at, message.clone()),
        );
        first.get_or_insert(message);
    }
    let passed = run.failures.is_empty() && run.faults.is_empty();
    let message = first.or_else(|| {
        run.faults
            .first()
            .map(|f| format!("a system faulted at tick {}", f.tick))
    });
    report_run(
        out,
        name,
        passed,
        duration_ms,
        message.as_deref(),
        Some(&run),
    );
    Ok(passed)
}

/// Reports a scenario's `test` event.
fn report_run(
    out: &mut Output,
    name: &str,
    passed: bool,
    duration_ms: u64,
    message: Option<&str>,
    run: Option<&GameRun>,
) {
    let status = if passed {
        TestStatus::Pass
    } else {
        TestStatus::Fail
    };
    match run {
        Some(run) => {
            let test = crate::output::Test {
                name,
                domain: "game",
                status,
                duration_ms,
                message,
            };
            out.test(
                &test,
                |w| game_fields(w, run),
                || {
                    format!(
                        "test game {name} ... {} ({} frames, seed {}, snapshot {:016x})\n",
                        status.word(),
                        run.ticks,
                        run.seed,
                        run.snapshot_hash
                    )
                },
            );
        }
        None => report(out, "game", name, status, duration_ms, message, |_| {}),
    }
}

/// The game domain's fields of a `test` event: the run's shape, its snapshot
/// hash and its final entity snapshot.
fn game_fields(w: &mut JsonWriter, run: &GameRun) {
    w.name("seed");
    w.uint(run.seed);
    w.name("frames");
    w.uint(run.ticks);
    w.name("build");
    w.string(&format!("{:016x}", run.build));
    w.name("snapshot_hash");
    w.string(&format!("{:016x}", run.snapshot_hash));
    w.name("entities");
    w.begin_array();
    for entity in &run.entities {
        w.begin_object();
        w.name("id");
        w.begin_array();
        w.uint(u64::from(entity.index));
        w.uint(u64::from(entity.generation));
        w.end_array();
        w.name("kind");
        w.string(entity.kind);
        w.name("tags");
        w.uint(entity.tags);
        w.name("model");
        w.string(entity.model);
        for (field, v) in [
            ("position", entity.position),
            ("half_extents", entity.half_extents),
        ] {
            w.name(field);
            w.begin_array();
            for c in v {
                w.number(f64::from(c));
            }
            w.end_array();
        }
        w.end_object();
    }
    w.end_array();
    w.name("failures");
    w.uint(run.failures.len() as u64);
    w.name("faults");
    w.uint(run.faults.len() as u64);
}
