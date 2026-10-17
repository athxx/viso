//! UI scenarios played by headless runs of the app, for `viso test ui` and
//! `viso snapshot` (`Viso_CLI.md` §22.2, §23).
//!
//! The scenarios under `tests/ui/` are planned and parsed first, so one that
//! does not parse fails at its line without a build. The app is then built
//! headless and every scenario runs in a process of its own, up to `--jobs`
//! at once, in a fresh directory of its own under `target/viso/test/`: its
//! surface (the scenario's, else `[test.surface]`), the scenario, the path of
//! its report and the directory its snapshots go to reach the run in the
//! environment. Each run is handed back, in name order, as soon as the runs
//! before it are.
//!
//! A scenario that finds a node by an inspector selector gets the source
//! table of the package's views beside it, which the build writes once for
//! every run; a case can ask its run for the census of what it showed once
//! the scenario ends.
//!
//! A case may instead be a component alone: its view package goes to the run
//! beside a scenario of one snapshot step, and the run mounts it in place of
//! the app's view.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use viso_project::Target;
use viso_view::headless::{
    Action, CENSUS_ENV, DETAIL_ENV, EVENTS_ENV, Expectation, FRAMES_ENV, HeadlessSurface,
    REPORT_ENV, Reported, SCRIPT_ENV, SNAPSHOT_ENV, SOURCES_ENV, SURFACE_ENV, Script, Selector,
    VIEW_ENV, is_snapshot_name,
};
use viso_view::inspect::SourceTable;
use viso_view::profile::PROFILE_ENV;

use crate::command::build::{self as build_command, Check};
use crate::context::Context;
use crate::error::{INTERRUPTED, RUN_TEST_CRASHED, SUCCESS};
use crate::interrupt;
use crate::output::Output;
use crate::process::ProcessTree;

/// The directory below the project root that holds UI scenarios.
pub(super) const SCENARIOS: &str = "tests/ui";

/// The extension of a scenario.
pub(super) const TAPE: &str = "tape";

/// How often a run is checked for its end, Ctrl-C and a stop.
const POLL: Duration = Duration::from_millis(10);

/// The variables a run must not inherit: the launch of a headless run, and
/// a dev session's.
const WITHHELD: &[&str] = &[
    SURFACE_ENV,
    FRAMES_ENV,
    SCRIPT_ENV,
    REPORT_ENV,
    SNAPSHOT_ENV,
    VIEW_ENV,
    SOURCES_ENV,
    CENSUS_ENV,
    DETAIL_ENV,
    EVENTS_ENV,
    PROFILE_ENV,
];

/// The name of the file a played scenario is written to when it differs
/// from its source.
const PLAYED: &str = "scenario.tape";

/// The name of the file a component's view package is written to.
const VIEW: &str = "view.pkg";

/// The name of the file the source table is written to, in the directory of
/// every run.
const SOURCES: &str = "sources.bin";

/// The name of the file a run writes its census to.
const CENSUS: &str = "census.bin";

/// The name of the file a run writes the detail of its window to.
const DETAIL: &str = "detail.bin";

/// The name of the file a run writes the journal of its window to.
const EVENTS: &str = "events.bin";

/// The name of the file a run writes the samples of its frames to.
const PROFILE: &str = "profile.ende";

/// Why no scenario was planned.
pub(super) enum Unplanned {
    /// There is nothing to play, and why.
    Nothing(String),
    /// `tests/ui` could not be read.
    Unreadable(String),
}

/// The scenarios to play: `name` alone, else every one `keeps` keeps.
pub(super) fn plan(
    root: &Path,
    name: Option<&str>,
    keeps: impl Fn(&str) -> bool,
    pattern: Option<&str>,
) -> Result<Vec<(String, PathBuf)>, Unplanned> {
    let dir = root.join(SCENARIOS);
    let planned = match name {
        Some(name) => {
            let path = dir.join(format!("{name}.{TAPE}"));
            if !path.is_file() {
                return Err(Unplanned::Nothing(format!(
                    "no scenario `{name}`: `{SCENARIOS}/{name}.{TAPE}` does not exist"
                )));
            }
            vec![(name.to_owned(), path)]
        }
        None if !dir.is_dir() => {
            return Err(Unplanned::Nothing(format!("no `{SCENARIOS}` directory")));
        }
        None => match scenarios(&dir) {
            Ok(found) => found.into_iter().filter(|(name, _)| keeps(name)).collect(),
            Err(error) => {
                return Err(Unplanned::Unreadable(format!(
                    "cannot read `{SCENARIOS}`: {error}"
                )));
            }
        },
    };
    if planned.is_empty() {
        return Err(Unplanned::Nothing(match pattern {
            Some(pattern) => format!("no scenario in `{SCENARIOS}` matches `{pattern}`"),
            None => format!("`{SCENARIOS}` holds no `.{TAPE}` scenario"),
        }));
    }
    Ok(planned)
}

/// The `.tape` scenarios under `dir`, by name.
pub(super) fn scenarios(dir: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let mut found: Vec<(String, PathBuf)> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|e| e == TAPE))
        .filter_map(|path| Some((path.file_stem()?.to_string_lossy().into_owned(), path)))
        .collect();
    found.sort();
    Ok(found)
}

/// A scenario that parsed, or a component, to play.
pub(super) struct Case {
    pub name: String,
    /// The scenario, or the source declaring the component.
    pub path: PathBuf,
    pub text: String,
    pub surface: HeadlessSurface,
    /// The snapshots its steps take, each with its line, in step order.
    pub snapshots: Vec<(String, u32)>,
    /// Whether it is played with a snapshot of its end, named after it,
    /// appended.
    pub ends_in_snapshot: bool,
    /// Whether a step finds its node by an inspector selector.
    pub queries: bool,
    /// Whether its run writes its census once the scenario ends.
    pub census: bool,
    /// Whether its run writes the detail of its window's nodes and last
    /// frame once the scenario ends.
    pub detail: bool,
    /// Whether its run keeps the journal of its window from its start and
    /// writes it once the scenario ends.
    pub events: bool,
    /// Whether its run writes the samples of every frame it draws.
    pub profile: bool,
    pub kind: Kind,
}

/// What a case plays.
pub(super) enum Kind {
    /// The scenario at the case's path.
    Scenario,
    /// Nothing: the app as it settles.
    Bare,
    /// A component alone: its module path and name, the view package
    /// mounting it, and the line of its declaration, which every report
    /// about it points at.
    Component {
        path: String,
        view: Vec<u8>,
        line: u32,
    },
}

impl Case {
    /// The case for the scenario `text` at `path`; `end_snapshot` says
    /// whether a scenario that takes no snapshot takes one of its end.
    ///
    /// # Errors
    ///
    /// The line and message of the step that does not parse.
    pub fn parse(
        name: String,
        path: PathBuf,
        text: String,
        surface: HeadlessSurface,
        end_snapshot: impl FnOnce(&str) -> bool,
    ) -> Result<Case, (u32, String)> {
        let script = Script::parse(&text).map_err(|error| (error.line, error.message))?;
        let mut snapshots: Vec<(String, u32)> = script
            .steps
            .iter()
            .filter_map(|step| match &step.action {
                Action::Snapshot(name) => Some((name.clone(), step.line)),
                _ => None,
            })
            .collect();
        let ends_in_snapshot =
            snapshots.is_empty() && is_snapshot_name(&name) && end_snapshot(&name);
        let queries = script.steps.iter().any(|step| {
            let selector = match &step.action {
                Action::Click(selector)
                | Action::Hover(selector)
                | Action::Scroll(selector, ..) => selector,
                Action::Expect(
                    Expectation::Shown(selector)
                    | Expectation::Absent(selector)
                    | Expectation::State(selector, _),
                ) => selector,
                _ => return false,
            };
            matches!(selector, Selector::Query(_))
        });
        if ends_in_snapshot {
            let line = text.lines().count() as u32 + 1;
            snapshots.push((name.clone(), line));
        }
        Ok(Case {
            surface: script.surface.unwrap_or(surface),
            name,
            path,
            text,
            snapshots,
            ends_in_snapshot,
            queries,
            census: false,
            detail: false,
            events: false,
            profile: false,
            kind: Kind::Scenario,
        })
    }

    /// The case that mounts `view`, the component `component` declared on
    /// `line` of the source `text` at `path`, alone at `surface` and takes
    /// one snapshot of it named `name`.
    pub fn component(
        name: String,
        component: String,
        (path, text, line): (PathBuf, String, u32),
        view: Vec<u8>,
        surface: HeadlessSurface,
    ) -> Case {
        Case {
            snapshots: vec![(name.clone(), 1)],
            name,
            path,
            text,
            surface,
            ends_in_snapshot: true,
            queries: false,
            census: false,
            detail: false,
            events: false,
            profile: false,
            kind: Kind::Component {
                path: component,
                view,
                line,
            },
        }
    }

    /// The case that plays nothing, named `name`: the app at `surface` as it
    /// settles.
    pub fn bare(name: String, surface: HeadlessSurface) -> Case {
        Case {
            name,
            path: PathBuf::new(),
            text: String::new(),
            surface,
            snapshots: Vec::new(),
            ends_in_snapshot: false,
            queries: false,
            census: false,
            detail: false,
            events: false,
            profile: false,
            kind: Kind::Bare,
        }
    }

    /// The line a report about the step on `line` points at: the scenario's
    /// last step for the snapshot of its end, which is on no line of it.
    pub fn shown(&self, line: u32) -> u32 {
        if let Kind::Component { line, .. } = self.kind {
            return line;
        }
        if !self.ends_in_snapshot || self.snapshot_at(line).is_none() {
            return line;
        }
        let last = self.text.lines().enumerate().filter(|(_, text)| {
            let text = text.trim();
            !text.is_empty() && !text.starts_with('#')
        });
        last.last().map_or(1, |(n, _)| n as u32 + 1)
    }

    /// Whether the step on `line` takes a snapshot.
    pub fn snapshot_at(&self, line: u32) -> Option<&str> {
        self.snapshots
            .iter()
            .find(|(_, at)| *at == line)
            .map(|(name, _)| name.as_str())
    }
}

/// How a case's run ended.
pub(super) enum Ended {
    Passed,
    /// The step on `line` failed.
    Failed {
        line: u32,
        message: String,
    },
    /// The run did not report its end.
    Crashed(String),
    /// Ctrl-C or a stop ended it.
    Stopped,
}

/// A snapshot a run took: its image and tree are `<name>.png` and
/// `<name>.tree` in [`Played::snapshots`].
pub(super) struct Shot {
    pub name: String,
    pub line: u32,
}

/// A case's run, and what it wrote.
pub(super) struct Played {
    index: usize,
    pub ended: Ended,
    pub duration: Duration,
    pub output: String,
    /// The run's own directory.
    pub dir: PathBuf,
    /// The snapshots it took, in step order.
    pub shots: Vec<Shot>,
}

impl Played {
    /// The directory the run's snapshots are in.
    pub fn snapshots(&self) -> PathBuf {
        self.dir.join("snapshots")
    }

    /// Where the run wrote its census, when its case asked for one.
    pub fn census(&self) -> PathBuf {
        self.dir.join(CENSUS)
    }

    /// Where the run wrote the detail of its window, when its case asked
    /// for it.
    pub fn detail(&self) -> PathBuf {
        self.dir.join(DETAIL)
    }

    /// Where the run wrote the journal of its window, when its case asked
    /// for it.
    pub fn events(&self) -> PathBuf {
        self.dir.join(EVENTS)
    }

    /// Where the run wrote the samples of its frames, when its case asked
    /// for them.
    pub fn profile(&self) -> PathBuf {
        self.dir.join(PROFILE)
    }

    /// Removes the run's directory.
    pub fn discard(&self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The headless build the cases run in.
pub(super) struct Build {
    exe: PathBuf,
    /// Where each run gets its directory.
    work: PathBuf,
    /// The source table handed to every run, when a case selects by it.
    sources: Option<PathBuf>,
    /// The build's identity and profile.
    pub build_id: String,
    pub profile: &'static str,
}

/// Builds the app headless for `cases`, handing every run `sources` when
/// given.
///
/// # Errors
///
/// The exit code of the failed build, reported.
pub(super) fn build(
    ctx: &Context<'_>,
    sources: Option<SourceTable>,
    out: &mut Output,
) -> Result<Build, u8> {
    let built = build_command::stage(ctx, Target::Headless, None, Check::Done, out)?;
    let Some(exe) = built
        .staged
        .artifacts
        .iter()
        .find(|(kind, _)| *kind == "binary")
        .map(|(_, path)| built.staged.out.join(path))
    else {
        return Err(out.failure(
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
    let sources = match sources {
        Some(table) => {
            let path = work.join(SOURCES);
            let written =
                fs::create_dir_all(&work).and_then(|()| fs::write(&path, table.to_bytes()));
            if let Err(error) = written {
                return Err(out.failure(
                    RUN_TEST_CRASHED,
                    &format!("cannot write `{}`: {error}", path.display()),
                    &[],
                ));
            }
            Some(path)
        }
        None => None,
    };
    Ok(Build {
        exe,
        work,
        sources,
        build_id: built.build_id,
        profile: built.profile.as_str(),
    })
}

/// Plays every case in `build`, up to `--jobs` at once, handing each run to
/// `each` in case order with the exit code its outcome asks for; `stop`
/// says, between runs, whether to stop the rest. The highest code, or
/// [`INTERRUPTED`] after Ctrl-C.
pub(super) fn play(
    ctx: &Context<'_>,
    build: &Build,
    cases: Vec<Case>,
    out: &mut Output,
    stop: impl Fn(&Output) -> bool,
    mut each: impl FnMut(&mut Output, &Case, Played) -> u8,
) -> u8 {
    if cases.is_empty() {
        return SUCCESS;
    }
    let jobs = ctx
        .global
        .jobs
        .map(usize::from)
        .or_else(|| thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1)
        .max(1);

    interrupt::catch();
    let stopped = Arc::new(Mutex::new(false));
    let (tx, rx) = mpsc::channel::<Played>();
    let queue = Arc::new(Mutex::new(0..cases.len()));
    let cases = Arc::new(cases);
    let workers: Vec<_> = (0..jobs.min(cases.len()))
        .map(|_| {
            let (tx, queue, cases, stopped) =
                (tx.clone(), queue.clone(), cases.clone(), stopped.clone());
            let (exe, work, sources) =
                (build.exe.clone(), build.work.clone(), build.sources.clone());
            thread::spawn(move || {
                loop {
                    let Some(index) = queue.lock().unwrap().next() else {
                        break;
                    };
                    let at = Launch {
                        exe: &exe,
                        work: &work,
                        sources: sources.as_deref(),
                    };
                    let played = run(&at, index, &cases[index], &stopped);
                    if tx.send(played).is_err() {
                        break;
                    }
                }
            })
        })
        .collect();
    drop(tx);

    let mut code = SUCCESS;
    let mut done: Vec<Option<Played>> = (0..cases.len()).map(|_| None).collect();
    let mut next = 0;
    loop {
        if interrupt::count() > 0 || stop(out) {
            *stopped.lock().unwrap() = true;
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
            if matches!(played.ended, Ended::Stopped) {
                // A stopped case is not reported: the run did not finish it.
                played.discard();
            } else {
                code = code.max(each(out, &cases[next], played));
            }
            next += 1;
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    if let Some(sources) = &build.sources {
        let _ = fs::remove_file(sources);
    }
    let _ = fs::remove_dir(&build.work);
    if let Some(parent) = build.work.parent() {
        let _ = fs::remove_dir(parent);
    }
    if interrupt::count() > 0 {
        return INTERRUPTED;
    }
    code
}

/// Where runs are launched from: the executable, the directory each run
/// gets its own under, and the source table they are handed.
struct Launch<'a> {
    exe: &'a Path,
    work: &'a Path,
    sources: Option<&'a Path>,
}

/// Plays `case` in a run of the build in a fresh directory of its own.
fn run(at: &Launch<'_>, index: usize, case: &Case, stop: &Mutex<bool>) -> Played {
    let (exe, started) = (at.exe, Instant::now());
    let dir = at.work.join(&case.name);
    let played = |ended, output, shots| Played {
        index,
        ended,
        duration: started.elapsed(),
        output,
        dir: dir.clone(),
        shots,
    };
    if *stop.lock().unwrap() {
        return played(Ended::Stopped, String::new(), Vec::new());
    }
    let _ = fs::remove_dir_all(&dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        return played(
            Ended::Crashed(format!("cannot create `{}`: {error}", dir.display())),
            String::new(),
            Vec::new(),
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
                Vec::new(),
            );
        }
    };
    let end = format!("expect snapshot \"{}\"\n", case.name);
    let script = match &case.kind {
        Kind::Component { .. } => Some(end),
        Kind::Bare => Some(String::new()),
        Kind::Scenario if case.ends_in_snapshot => {
            let mut text = case.text.clone();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&end);
            Some(text)
        }
        Kind::Scenario => None,
    };
    let view = match &case.kind {
        Kind::Component { view, .. } => Some((dir.join(VIEW), view.as_slice())),
        Kind::Scenario | Kind::Bare => None,
    };
    let script = script.map(|text| (dir.join(PLAYED), text));
    let written = [
        script.as_ref().map(|(path, text)| (path, text.as_bytes())),
        view.as_ref().map(|(path, bytes)| (path, *bytes)),
    ];
    for (path, bytes) in written.into_iter().flatten() {
        if let Err(error) = fs::write(path, bytes) {
            return played(
                Ended::Crashed(format!("cannot write `{}`: {error}", path.display())),
                String::new(),
                Vec::new(),
            );
        }
    }
    let script = match script {
        Some((path, _)) => path,
        None => case
            .path
            .canonicalize()
            .unwrap_or_else(|_| case.path.clone()),
    };
    let mut command = Command::new(exe);
    for name in WITHHELD {
        command.env_remove(name);
    }
    if let Some((path, _)) = &view {
        command.env(VIEW_ENV, path);
    }
    if let Some(sources) = at.sources {
        command.env(SOURCES_ENV, sources);
    }
    if case.census {
        command.env(CENSUS_ENV, dir.join(CENSUS));
    }
    if case.detail {
        command.env(DETAIL_ENV, dir.join(DETAIL));
    }
    if case.events {
        command.env(EVENTS_ENV, dir.join(EVENTS));
    }
    if case.profile {
        command.env(PROFILE_ENV, dir.join(PROFILE));
    }
    command
        .env(SURFACE_ENV, case.surface.to_env())
        .env(SCRIPT_ENV, &script)
        .env(REPORT_ENV, &report)
        .env(SNAPSHOT_ENV, dir.join("snapshots"))
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
                Vec::new(),
            );
        }
    };
    let status = loop {
        if *stop.lock().unwrap() {
            tree.kill();
            let _ = tree.child.wait();
            return played(Ended::Stopped, read(&log), Vec::new());
        }
        match tree.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(POLL),
            Err(error) => {
                tree.kill();
                return played(
                    Ended::Crashed(format!("cannot wait for the run: {error}")),
                    read(&log),
                    Vec::new(),
                );
            }
        }
    };
    let (ended, shots) = ended(&read(&report), status);
    played(ended, read(&log), shots)
}

/// How a run that exited with `status` ended, by its report, and the
/// snapshots it took.
fn ended(report: &str, status: ExitStatus) -> (Ended, Vec<Shot>) {
    let mut failed = None;
    let mut finished = false;
    let mut shots = Vec::new();
    for line in report.lines() {
        match Reported::from_line(line) {
            Some(Reported::Failed { line, message }) => failed = Some((line, message)),
            Some(Reported::Done { .. }) => finished = true,
            Some(Reported::Snapshot { line, name, .. }) => shots.push(Shot { name, line }),
            Some(Reported::Passed { .. } | Reported::Skipped { .. }) | None => {}
        }
    }
    let ended = match (finished, failed) {
        (true, Some((line, message))) => Ended::Failed { line, message },
        (true, None) if status.success() => Ended::Passed,
        (true, None) => Ended::Crashed(format!(
            "the run reported its end, then exited with {status}"
        )),
        (false, _) => Ended::Crashed(format!(
            "the run exited with {status} before it played every step"
        )),
    };
    (ended, shots)
}

fn read(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
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
            "{}\n{}\n{}\n",
            Reported::Passed { line: 1 }.to_line(),
            Reported::Snapshot {
                line: 2,
                name: "home".into(),
                width: 4,
                height: 3
            }
            .to_line(),
            Reported::Done { frames: 9 }.to_line()
        );
        let (ended_as, shots) = ended(&passed, exited(0));
        assert!(matches!(ended_as, Ended::Passed));
        assert_eq!(
            shots.iter().map(|s| (&*s.name, s.line)).collect::<Vec<_>>(),
            [("home", 2)]
        );
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
            ended(&failed, exited(0)).0,
            Ended::Failed { line: 4, .. }
        ));
        let cut = Reported::Passed { line: 1 }.to_line();
        assert!(matches!(ended(&cut, exited(101)).0, Ended::Crashed(_)));
        assert!(matches!(ended(&passed, exited(3)).0, Ended::Crashed(_)));
    }

    #[test]
    fn a_scenario_without_snapshots_may_end_in_one() {
        let parse = |text: &str, end: bool| {
            Case::parse(
                "home".into(),
                PathBuf::from("home.tape"),
                text.into(),
                HeadlessSurface::default(),
                |_| end,
            )
            .unwrap()
        };
        let plain = parse("click \"Save\"\n# the end\n", true);
        assert!(plain.ends_in_snapshot);
        assert_eq!(plain.snapshots, [("home".to_owned(), 3)]);
        assert_eq!(plain.shown(3), 1);
        assert!(!parse("click \"Save\"\n", false).ends_in_snapshot);
        let taken = parse("expect snapshot \"saved\"\n", true);
        assert!(!taken.ends_in_snapshot);
        assert_eq!(taken.snapshot_at(1), Some("saved"));
    }
}
