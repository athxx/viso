//! `viso snapshot capture|compare|update [<scenario>|<component>]`: the
//! images, with the tree, layout and paint that drew them, the UI scenarios
//! under `tests/ui/` take with `expect snapshot "<name>"` steps, and those of
//! components shown alone, held to the goldens under `tests/snapshots/`
//! (`Viso_CLI.md` §23).
//!
//! A name that is no scenario's names a component, `Name` or `module::Name`:
//! it is mounted alone in a headless run, with its inputs' defaults, and its
//! golden records which component it shows, so `compare` and `update` take
//! it again with the scenarios.
//!
//! A scenario that takes no snapshot of its own is held at its end to the
//! golden named after it, when it has one; `capture` of such a scenario by
//! name takes that snapshot. `capture` writes every snapshot as its golden;
//! `compare` holds each to its golden, one `test` event a golden, and leaves
//! what departs under `target/viso/snapshots/`; `update` rewrites only the
//! goldens that are missing or no longer match. A golden is never rewritten
//! but by `capture` or `update`. A snapshot is taken only from a scenario
//! that played through.

pub(super) mod component;
mod golden;

pub(crate) use golden::SNAPSHOT_VERSION;
pub(super) use golden::{Held, Store, Taken};

use std::collections::HashMap;
use std::fs;

use viso_dev_protocol::headless::HeadlessSurface;
use viso_dsl::Diagnostic;

use super::check;
use super::scenario::{self, Case, Ended, Kind, Played, SCENARIOS, TAPE, Unplanned};
use super::test::{invalid, line_range, relative, report};
use crate::args::{SnapshotCommand, TestTarget};
use crate::context::Context;
use crate::error::{
    DIAGNOSTICS, ENV_SNAPSHOT_TARGET, ENV_SNAPSHOT_WRITE, ENV_TEST_INPUT, RUN_SNAPSHOT_FAILED,
    RUN_TEST_CRASHED, RUNTIME, SUCCESS, TEST_EXPECTATION, TEST_SNAPSHOT_INVALID,
    TEST_SNAPSHOT_MISMATCH, TEST_SNAPSHOT_MISSING,
};
use crate::output::sayln;
use crate::output::{Artifact, Output, Source, TestStatus, human};

/// The target snapshots are taken on.
const TARGET: &str = "headless";

/// What the command does with each snapshot.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Capture,
    Compare,
    Update,
}

pub fn run(ctx: &Context<'_>, command: &SnapshotCommand, out: &mut Output) -> u8 {
    let (mode, args, output, filter) = match command {
        SnapshotCommand::Capture(capture) => {
            (Mode::Capture, &capture.common, capture.output.clone(), None)
        }
        SnapshotCommand::Compare(args) => (Mode::Compare, args, None, None),
        SnapshotCommand::Update(update) => {
            (Mode::Update, &update.common, None, update.filter.as_deref())
        }
    };
    if let Some(target) = args.target
        && target != TestTarget::Headless
    {
        return out.failure(
            ENV_SNAPSHOT_TARGET,
            &format!(
                "snapshots are taken `headless` in this build of viso, not on `{}`",
                target.target()
            ),
            &[],
        );
    }
    let output = match output {
        Some(dir) => match ctx.cwd(out) {
            Ok(cwd) => Some(cwd.join(dir)),
            Err(code) => return code,
        },
        None => None,
    };
    let checked = match check::load(ctx, out) {
        Ok(checked) => checked,
        Err(code) => return code,
    };
    let code = checked.code(out);
    if code != SUCCESS {
        return code;
    }
    let root = checked.project.root.clone();
    let layout = ctx.layout(&root);
    let store = Store::new(&root, output, &layout.viso_dir(), TARGET);
    let keeps = |name: &str| filter.is_none_or(|pattern| name.contains(pattern));

    let (width, height, scale) = checked.project.manifest.test.surface.resolved();
    let surface = HeadlessSurface {
        width,
        height,
        scale: scale.get(),
    };
    let named = args.scenario.is_some();
    let wanted_component = args.scenario.as_deref().filter(|name| {
        !root
            .join(SCENARIOS)
            .join(format!("{name}.{TAPE}"))
            .is_file()
    });
    // The goldens of components, which are taken again with the scenarios.
    let components: Vec<(String, String)> = if named {
        Vec::new()
    } else {
        store
            .names()
            .into_iter()
            .filter_map(|name| Some((store.component(&name)?, name)))
            .collect()
    };
    let planned = match wanted_component {
        Some(_) => Vec::new(),
        None => match scenario::plan(&root, args.scenario.as_deref(), |_| true, None) {
            Ok(planned) => planned,
            Err(Unplanned::Nothing(_)) if !components.is_empty() => Vec::new(),
            Err(Unplanned::Nothing(why) | Unplanned::Unreadable(why)) => {
                return out.failure(ENV_TEST_INPUT, &why, &[]);
            }
        },
    };
    let mut code = SUCCESS;
    let mut cases = Vec::new();
    let mut taken_by: HashMap<String, String> = HashMap::new();
    let mut lost: Vec<(String, String)> = Vec::new();
    let alone = Alone {
        checked: &checked,
        surface,
    };
    if let Some(name) = wanted_component {
        match component::find(&checked.package, name) {
            Ok(Some(found)) => match alone.case(out, &found.name.clone(), found) {
                Ok(case) => cases.push(case),
                Err(failed) => return failed,
            },
            Ok(None) => {
                return out.failure_next(
                    ENV_TEST_INPUT,
                    &format!(
                        "no scenario `{name}` (`{SCENARIOS}/{name}.{TAPE}`) and no component `{name}`"
                    ),
                    &["a component is named `Name` or `module::Name`".to_owned()],
                    &[],
                );
            }
            Err(why) => {
                invalid(out, None, None, &why);
                return DIAGNOSTICS;
            }
        }
    }
    for (path, name) in &components {
        match component::find(&checked.package, path) {
            Ok(Some(found)) => match alone.case(out, name, found) {
                Ok(case) => {
                    if taken_twice(out, &root, &case, &mut taken_by).is_some() {
                        code = DIAGNOSTICS;
                    } else if keeps(name) {
                        cases.push(case);
                    }
                }
                Err(failed) => code = code.max(failed),
            },
            Ok(None) => {
                taken_by.insert(name.clone(), path.clone());
                if keeps(name) {
                    lost.push((name.clone(), path.clone()));
                }
            }
            Err(why) => {
                invalid(out, None, None, &why);
                code = DIAGNOSTICS;
            }
        }
    }
    for (name, path) in planned {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                return out.failure(
                    ENV_TEST_INPUT,
                    &format!(
                        "cannot read `{}`: {error}",
                        relative(&root, &path).display()
                    ),
                    &[],
                );
            }
        };
        let ends = |stem: &str| (mode == Mode::Capture && named) || store.has(stem);
        let case = match Case::parse(name, path.clone(), text.clone(), surface, ends) {
            Ok(case) => case,
            Err((line, message)) => {
                let source = Source::new(&path, &root, &text);
                invalid(out, Some(&source), Some(line_range(&text, line)), &message);
                code = DIAGNOSTICS;
                continue;
            }
        };
        if taken_twice(out, &root, &case, &mut taken_by).is_some() {
            code = DIAGNOSTICS;
            continue;
        }
        if case.snapshots.iter().any(|(name, _)| keeps(name)) {
            cases.push(case);
        }
    }

    let orphans: Vec<String> = if named {
        Vec::new()
    } else {
        store
            .names()
            .into_iter()
            .filter(|name| !taken_by.contains_key(name) && keeps(name))
            .collect()
    };
    // A scenario that does not parse stops the command before the build: a
    // golden is taken, compared or rewritten from scenarios that all read.
    if code != SUCCESS {
        return code;
    }
    if cases.is_empty() && ((orphans.is_empty() && lost.is_empty()) || mode == Mode::Capture) {
        let why = match &args.scenario {
            Some(name) => format!("the scenario `{name}` takes no snapshot"),
            None => format!("no scenario in `{SCENARIOS}` takes a snapshot"),
        };
        let why = match filter {
            Some(pattern) => format!("{why} whose name contains `{pattern}`"),
            None => why,
        };
        return out.failure_next(
            ENV_TEST_INPUT,
            &why,
            &["a scenario takes one with an `expect snapshot \"<name>\"` step".to_owned()],
            &[],
        );
    }

    let mut tally = Tally::default();
    if !cases.is_empty() {
        let sources = cases
            .iter()
            .any(|case| case.queries)
            .then(|| checked.sources());
        let build = match scenario::build(ctx, sources, out) {
            Ok(build) => build,
            Err(failed) => return code.max(failed),
        };
        let played = scenario::play(
            ctx,
            &build,
            cases,
            out,
            |_| false,
            |out, case, played| {
                let ended = ended(out, &root, case, &played.ended);
                let held = hold(
                    out, &root, &store, mode, case, &played, &build, &keeps, &mut tally,
                );
                if ended == SUCCESS {
                    played.discard();
                } else {
                    out.case_output(&case.name, &played.output);
                }
                ended.max(held)
            },
        );
        code = code.max(played);
    }
    let untaken = orphans
        .iter()
        .map(|name| (name, None))
        .chain(lost.iter().map(|(name, path)| (name, Some(path))));
    for (name, path) in untaken {
        let golden = store.golden_name(name);
        let (message, notes) = match path {
            None => (
                format!("the golden `{golden}` is taken by no scenario"),
                [format!(
                    "remove it, or take it with an `expect snapshot \"{name}\"` step"
                )],
            ),
            Some(path) => (
                format!(
                    "the golden `{golden}` shows the component `{path}`, which the package no longer declares"
                ),
                ["remove it, or capture the component under its new name".to_owned()],
            ),
        };
        if mode == Mode::Compare {
            out.failure(TEST_SNAPSHOT_MISSING, &message, &notes);
            report(
                out,
                "snapshot",
                name,
                TestStatus::Fail,
                0,
                Some(&message),
                |w| {
                    w.name("golden");
                    w.string(&golden);
                },
            );
            code = code.max(DIAGNOSTICS);
        } else {
            out.warning(TEST_SNAPSHOT_MISSING, &message, &notes);
        }
    }
    match mode {
        Mode::Compare => out.tally(code == SUCCESS),
        Mode::Capture | Mode::Update if out.human() => {
            let word = if mode == Mode::Capture {
                "captured"
            } else {
                "updated"
            };
            sayln!(
                "{} snapshot{} {word}; {} unchanged",
                tally.written,
                if tally.written == 1 { "" } else { "s" },
                tally.unchanged
            );
        }
        _ => {}
    }
    code
}

/// How components are captured alone: in the package as checked, at the
/// test surface.
struct Alone<'c> {
    checked: &'c check::Checked,
    surface: HeadlessSurface,
}

impl Alone<'_> {
    /// The case showing `found` alone, its snapshot named `name`; the exit
    /// code when it cannot be compiled to.
    fn case(&self, out: &mut Output, name: &str, found: component::Found) -> Result<Case, u8> {
        let project = &self.checked.project;
        let profile = self.checked.view_profile();
        let language = self.checked.language();
        let source = Source::new(&found.file, &project.root, &found.source);
        match component::package(&found, &self.checked.name, language, profile) {
            Ok(view) => Ok(Case::component(
                name.to_owned(),
                found.path.clone(),
                (found.file.clone(), found.source.clone(), found.line),
                view,
                self.surface,
            )),
            Err(component::Unbuilt::Source(diagnostics)) => {
                for diagnostic in &diagnostics {
                    out.source(Some(&source), &[], diagnostic);
                }
                Err(DIAGNOSTICS)
            }
            Err(component::Unbuilt::Alone(why)) => {
                invalid(
                    out,
                    Some(&source),
                    Some(line_range(&found.source, found.line)),
                    &format!(
                        "the component `{}` cannot be shown alone: {}",
                        found.path,
                        why.join("; ")
                    ),
                );
                Err(DIAGNOSTICS)
            }
        }
    }
}

/// What a capture or an update wrote.
#[derive(Default)]
pub(super) struct Tally {
    pub written: usize,
    pub unchanged: usize,
}

/// Reports a snapshot of `case` that another scenario of `taken_by`, by
/// snapshot, takes too, and returns what it said; else records `case`'s.
pub(super) fn taken_twice(
    out: &mut Output,
    root: &std::path::Path,
    case: &Case,
    taken_by: &mut HashMap<String, String>,
) -> Option<String> {
    for (name, line) in &case.snapshots {
        if let Some(other) = taken_by.get(name) {
            let source = Source::new(&case.path, root, &case.text);
            let message = format!("the scenario `{other}` takes a snapshot named `{name}` too");
            invalid(
                out,
                Some(&source),
                Some(line_range(&case.text, case.shown(*line))),
                &message,
            );
            return Some(message);
        }
    }
    for (name, _) in &case.snapshots {
        taken_by.insert(name.clone(), case.name.clone());
    }
    None
}

/// Reports how `case`'s run ended when it did not pass; the exit code.
pub(super) fn ended(out: &mut Output, root: &std::path::Path, case: &Case, ended: &Ended) -> u8 {
    match ended {
        Ended::Passed | Ended::Stopped => SUCCESS,
        Ended::Failed { line, message } => {
            let snapshot = case.snapshot_at(*line).is_some();
            let code = if snapshot {
                RUN_SNAPSHOT_FAILED.name
            } else {
                TEST_EXPECTATION.name
            };
            let source = Source::new(&case.path, root, &case.text);
            out.source(
                Some(&source),
                &[],
                &Diagnostic::error(
                    code,
                    line_range(&case.text, case.shown(*line)),
                    message.clone(),
                ),
            );
            if snapshot { RUNTIME } else { DIAGNOSTICS }
        }
        Ended::Crashed(why) => {
            out.error(RUN_TEST_CRASHED.name, &format!("{}: {why}", case.name), &[]);
            RUNTIME
        }
    }
}

/// Captures, compares or updates each snapshot `played` took of `case`, as
/// `mode` says; the exit code.
#[allow(clippy::too_many_arguments)]
fn hold(
    out: &mut Output,
    root: &std::path::Path,
    store: &Store,
    mode: Mode,
    case: &Case,
    played: &Played,
    build: &scenario::Build,
    keeps: &dyn Fn(&str) -> bool,
    tally: &mut Tally,
) -> u8 {
    let passed = matches!(played.ended, Ended::Passed);
    let duration_ms = played.duration.as_millis() as u64;
    let mut code = SUCCESS;
    for (name, line) in case.snapshots.iter().filter(|(name, _)| keeps(name)) {
        let shot = played.shots.iter().find(|shot| shot.name == *name);
        let at = |out: &mut Output, code: crate::error::Code, message: &str, notes: Vec<String>| {
            let source = Source::new(&case.path, root, &case.text);
            let mut diagnostic = Diagnostic::error(
                code.name,
                line_range(&case.text, case.shown(*line)),
                message,
            );
            diagnostic.notes = notes;
            out.source(Some(&source), &[], &diagnostic);
        };
        let taken = match shot.map(|_| Taken::read(&played.snapshots(), name, case.surface)) {
            Some(Ok(taken)) => match &case.kind {
                Kind::Component { path, .. } => taken.of_component(path),
                Kind::Scenario | Kind::Bare => taken,
            },
            Some(Err(why)) => {
                out.failure(RUN_SNAPSHOT_FAILED, &why, &[]);
                code = code.max(RUNTIME);
                continue;
            }
            None => {
                if mode == Mode::Compare {
                    let message = format!(
                        "the scenario `{}` ended before its snapshot `{name}` (line {})",
                        case.name,
                        case.shown(*line)
                    );
                    report(
                        out,
                        "snapshot",
                        name,
                        TestStatus::Fail,
                        duration_ms,
                        Some(&message),
                        |_| {},
                    );
                    code = code.max(DIAGNOSTICS);
                }
                continue;
            }
        };
        match mode {
            Mode::Capture | Mode::Update if !passed => {}
            Mode::Capture => code = code.max(write(out, store, &taken, build, tally)),
            Mode::Update => match store.hold(&taken) {
                Ok(Held::Matched) => tally.unchanged += 1,
                Ok(_) => code = code.max(write(out, store, &taken, build, tally)),
                Err(why) => code = code.max(out.failure(ENV_SNAPSHOT_WRITE, &why, &[])),
            },
            Mode::Compare => {
                let golden = store.golden_name(name);
                let (message, extra): (Option<String>, Option<golden::Departure>) =
                    match store.hold(&taken) {
                        Ok(Held::Matched) => (None, None),
                        Ok(Held::Missing) => {
                            let message = format!("the snapshot `{name}` has no golden `{golden}`");
                            at(
                                out,
                                TEST_SNAPSHOT_MISSING,
                                &message,
                                vec![format!(
                                    "write it with `viso snapshot update {}`",
                                    case.name
                                )],
                            );
                            (Some(message), None)
                        }
                        Ok(Held::Invalid(why)) => {
                            at(out, TEST_SNAPSHOT_INVALID, &why, Vec::new());
                            (Some(why), None)
                        }
                        Ok(Held::Differs(departure)) => {
                            let message = departure.message(name);
                            at(out, TEST_SNAPSHOT_MISMATCH, &message, departure.notes());
                            (Some(message), Some(departure))
                        }
                        Err(why) => {
                            code = code.max(out.failure(ENV_SNAPSHOT_WRITE, &why, &[]));
                            continue;
                        }
                    };
                let status = if message.is_some() {
                    code = code.max(DIAGNOSTICS);
                    TestStatus::Fail
                } else {
                    TestStatus::Pass
                };
                report(
                    out,
                    "snapshot",
                    name,
                    status,
                    duration_ms,
                    message.as_deref(),
                    |w| {
                        w.name("golden");
                        w.string(&golden);
                        if let Some(departure) = &extra {
                            departure.write_json(w);
                        }
                    },
                );
            }
        }
    }
    code
}

/// Writes `taken` as its golden, reporting each file; the exit code.
pub(super) fn write(
    out: &mut Output,
    store: &Store,
    taken: &Taken,
    build: &scenario::Build,
    tally: &mut Tally,
) -> u8 {
    match store.write(taken, &build.build_id) {
        Ok(written) => {
            tally.written += 1;
            for file in &written {
                out.artifact(
                    &Artifact {
                        kind: file.kind,
                        path: &file.path,
                        target: TARGET,
                        profile: Some(build.profile),
                        build_id: &build.build_id,
                        size: file.size,
                        hash: &file.hash,
                        cached: false,
                    },
                    || {
                        format!(
                            "  {:<10} {}  {}\n",
                            file.kind,
                            file.path,
                            human::size(file.size)
                        )
                    },
                );
            }
            SUCCESS
        }
        Err(why) => out.failure(ENV_SNAPSHOT_WRITE, &why, &[]),
    }
}
