//! `viso test` (`Viso_CLI.md` section 22). The `game` domain (section 22.3)
//! runs each scenario under `tests/game/` headless on the package's systems:
//! its input tape, or the one `--tape` names, for `--frames` frames on a world
//! seeded with the tape's seed or `--seed`, each tick's `@probe` states traced
//! and checked against the scenario's expectations.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use viso_dsl::scenario::{self, DEFAULT_SEED, GameRun, RunError, Scenario, read_tape};
use viso_dsl::{Diagnostic, TextRange, TextSize};
use viso_ende::JsonWriter;

use super::check::{self, Checked};
use super::{
    DIAGNOSTICS, ENV_TEST_INPUT, ENVIRONMENT, SUCCESS, TEST_EXPECTATION, TEST_INPUT_INVALID,
};
use crate::args::{Global, TestArgs, TestDomain};
use crate::output::{Output, Source, Test};

/// The directory below the project root that holds game scenarios.
const SCENARIOS: &str = "tests/game";

/// The extension of a scenario or tape file.
const TAPE: &str = "tape";

/// The faults of one run reported one by one; the rest are counted.
const FAULTS_SHOWN: usize = 8;

pub fn run(global: &Global, args: &TestArgs, out: &mut Output) -> u8 {
    let checked = match check::load(global, out) {
        Ok(checked) => checked,
        Err(code) => return code,
    };
    let code = checked.code(out);
    if code != SUCCESS {
        return code;
    }
    match args.domain {
        TestDomain::Game => game(&checked, args, out),
    }
}

/// A scenario to run: its name and its file, if it has one.
struct Planned {
    name: String,
    file: Option<PathBuf>,
}

fn game(checked: &Checked, args: &TestArgs, out: &mut Output) -> u8 {
    let root = &checked.project.root;
    let planned = match plan(root, args) {
        Ok(planned) => planned,
        Err(message) => {
            out.failure(ENV_TEST_INPUT, &message, &[]);
            return ENVIRONMENT;
        }
    };
    let tick_rate = check::profile(&checked.project).tick_rate;
    let (mut passed, mut failed) = (0, 0);
    let mut code = SUCCESS;
    for planned in &planned {
        match scenario(root, planned, args, tick_rate, checked, out) {
            Ok(true) => passed += 1,
            Ok(false) => {
                failed += 1;
                code = code.max(DIAGNOSTICS);
            }
            Err(environment) => {
                failed += 1;
                code = code.max(environment);
            }
        }
    }
    out.tally(passed, failed);
    code
}

/// The scenarios the command line asks for.
fn plan(root: &Path, args: &TestArgs) -> Result<Vec<Planned>, String> {
    let dir = root.join(SCENARIOS);
    let named = |name: &str| dir.join(format!("{name}.{TAPE}"));
    if let Some(name) = &args.scenario {
        let file = named(name);
        if !file.is_file() && args.tape.is_none() {
            return Err(format!(
                "no scenario `{name}`: `{SCENARIOS}/{name}.{TAPE}` does not exist"
            ));
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
    let entries =
        fs::read_dir(&dir).map_err(|error| format!("cannot read `{SCENARIOS}`: {error}"))?;
    let mut planned: Vec<Planned> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|e| e == TAPE))
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().into_owned();
            Some(Planned {
                name,
                file: Some(path),
            })
        })
        .collect();
    if planned.is_empty() {
        return Err(format!("`{SCENARIOS}` holds no `.{TAPE}` scenario"));
    }
    planned.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(planned)
}

/// Runs one scenario and reports it; whether it passed, or the exit code
/// of an input that could not be read.
fn scenario(
    root: &Path,
    planned: &Planned,
    args: &TestArgs,
    tick_rate: u32,
    checked: &Checked,
    out: &mut Output,
) -> Result<bool, u8> {
    let read = |path: &Path| {
        fs::read(path)
            .map_err(|error| format!("cannot read `{}`: {error}", relative(root, path).display()))
    };
    let text = match &planned.file {
        Some(path) => match read(path) {
            Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Err(message) => {
                out.failure(ENV_TEST_INPUT, &message, &[]);
                return Err(ENVIRONMENT);
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
                out.failure(ENV_TEST_INPUT, &message, &[]);
                return Err(ENVIRONMENT);
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
                RunError::Start(fault) => out.failure(fault.code, &message, &[]),
                RunError::UnknownProbe(e) => {
                    let at = text.as_deref().map(|t| line_range(t, e.line));
                    invalid(out, source.as_ref(), at, &message);
                }
                RunError::Build(_) | RunError::Tape(_) => {
                    out.failure(TEST_INPUT_INVALID, &message, &[]);
                }
            }
            report(out, name, false, duration_ms, Some(&message), None);
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
        out.failure(fault.code, &message, &[]);
    }
    if run.faults.len() > FAULTS_SHOWN {
        let message = format!("{} more faults", run.faults.len() - FAULTS_SHOWN);
        out.failure(run.faults[FAULTS_SHOWN].code, &message, &[]);
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
            &Diagnostic::error(TEST_EXPECTATION, at, message.clone()),
        );
        first.get_or_insert(message);
    }
    let passed = run.failures.is_empty() && run.faults.is_empty();
    let message = first.or_else(|| {
        run.faults
            .first()
            .map(|f| format!("a system faulted at tick {}", f.tick))
    });
    report(
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
fn report(
    out: &mut Output,
    name: &str,
    passed: bool,
    duration_ms: u64,
    message: Option<&str>,
    run: Option<&GameRun>,
) {
    let test = Test {
        name,
        domain: "game",
        passed,
        duration_ms,
        message,
    };
    out.test(
        &test,
        |w| {
            if let Some(run) = run {
                game_fields(w, run);
            }
        },
        || {
            let status = if passed { "ok" } else { "FAILED" };
            match run {
                Some(run) => format!(
                    "test game {name} ... {status} ({} frames, seed {}, snapshot {:016x})\n",
                    run.ticks, run.seed, run.snapshot_hash
                ),
                None => format!("test game {name} ... {status}\n"),
            }
        },
    );
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

/// Reports a scenario that does not parse or names no probe, at `at` in
/// `source` when known.
fn invalid(out: &mut Output, source: Option<&Source<'_>>, at: Option<TextRange>, message: &str) {
    match (source, at) {
        (Some(source), Some(at)) => out.source(
            Some(source),
            &[],
            &Diagnostic::error(TEST_INPUT_INVALID, at, message),
        ),
        _ => out.failure(TEST_INPUT_INVALID, message, &[]),
    }
}

/// The byte range of 1-based line `line` of `text`, without its terminator.
fn line_range(text: &str, line: u32) -> TextRange {
    let mut start = 0;
    for (n, content) in text.split('\n').enumerate() {
        if n + 1 == line as usize {
            let content = content.strip_suffix('\r').unwrap_or(content);
            return TextRange::new(
                TextSize::new(start as u32),
                TextSize::new((start + content.len()) as u32),
            );
        }
        start += content.len() + 1;
    }
    TextRange::empty(TextSize::new(text.len() as u32))
}

fn relative<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}
