//! `viso doctor [<target>]`: what this machine has for developing Viso apps,
//! one check at a time, without changing anything.
//!
//! Without a target the checks are the host's: the Rust toolchain, Cargo,
//! the project here, the framework it links, the linker, the GPU backend,
//! the web tools, the places builds write, Git, the language server editors
//! run, and a word on the Android and iOS environments. A target adds its own toolchain's checks to the
//! toolchain and the project, and a gap there is `missing` rather than a
//! warning: what `viso build <target>` would stop at.
//!
//! The checks run at once, each its own thread, and a tool that does not
//! answer in time is an `error` naming the timeout, not a hung doctor. Each
//! is reported as it stands — a `doctor_check` event, or a line — with the
//! command that fixes it, when one does. `--fix` runs the fixes that are
//! local and reversible (a Rust target added with `rustup`), each said
//! before it runs, and checks again; it never installs an SDK. Exit 3 when
//! a check is `missing` or `error`.

mod probes;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use viso_project::{Project, Target};

use crate::args::{BuildTarget, DoctorArgs};
use crate::context::Context;
use crate::error::{ENVIRONMENT, SUCCESS};
use crate::output::Output;

/// How long a check may take, its tools included, before it is an error.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a fix may take: `rustup` downloads the target's library.
const FIX_TIMEOUT: Duration = Duration::from_secs(600);

/// How a check stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Ok,
    /// Works, or is not needed yet, but something is worth knowing.
    Warn,
    /// Something a build needs is not installed or selected.
    Missing,
    /// Something is there and broken, or could not be checked.
    Error,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Missing => "missing",
            Status::Error => "error",
        }
    }

    /// Whether the doctor fails for it.
    fn fails(self) -> bool {
        matches!(self, Status::Missing | Status::Error)
    }
}

/// A local, reversible change that makes a check pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fix {
    /// `rustup target add <triple>`.
    RustTarget(&'static str),
}

impl Fix {
    /// The command line it runs.
    fn command_line(&self) -> String {
        match self {
            Fix::RustTarget(triple) => format!("rustup target add {triple}"),
        }
    }
}

/// What a check found.
#[derive(Debug, Clone)]
pub(crate) struct Found {
    pub status: Status,
    /// What it found, or what is wrong.
    pub message: String,
    /// The diagnostic code of a gap.
    pub code: Option<&'static str>,
    /// The command to run next.
    pub suggestion: Option<String>,
    /// The fix `--fix` makes.
    pub fix: Option<Fix>,
}

impl Found {
    pub(crate) fn ok(message: impl Into<String>) -> Found {
        Found {
            status: Status::Ok,
            message: message.into(),
            code: None,
            suggestion: None,
            fix: None,
        }
    }

    pub(crate) fn warn(message: impl Into<String>) -> Found {
        Found {
            status: Status::Warn,
            ..Found::ok(message)
        }
    }

    pub(crate) fn missing(code: &'static str, message: impl Into<String>) -> Found {
        Found {
            status: Status::Missing,
            code: Some(code),
            ..Found::ok(message)
        }
    }

    pub(crate) fn error(code: &'static str, message: impl Into<String>) -> Found {
        Found {
            status: Status::Error,
            code: Some(code),
            ..Found::ok(message)
        }
    }

    pub(crate) fn suggest(mut self, command: impl Into<String>) -> Found {
        self.suggestion = Some(command.into());
        self
    }

    pub(crate) fn fixed_by(mut self, fix: Fix) -> Found {
        self.suggestion = Some(fix.command_line());
        self.fix = Some(fix);
        self
    }

    /// Itself, or a warning in place of a gap when what it checks is not
    /// required: the host doctor's word on a target's tools.
    pub(crate) fn required(self, required: bool) -> Found {
        match self.status {
            Status::Missing if !required => Found {
                status: Status::Warn,
                code: None,
                ..self
            },
            _ => self,
        }
    }
}

/// One check: what it is called, and the probe that runs it.
#[derive(Clone, Copy)]
pub(crate) struct Probe {
    /// Its stable name, as `doctor_check` reports it.
    pub name: &'static str,
    /// What it checks, for a reader.
    pub title: &'static str,
    /// The probe; `None` when the check does not apply here (no project to
    /// look into, say).
    pub run: fn(&Machine) -> Option<Found>,
}

/// What the checks look at: the project here, when there is one, and the
/// target asked about.
pub(crate) struct Machine {
    /// The project, or why none was found.
    pub project: Result<Project, Vec<viso_project::ConfigDiagnostic>>,
    /// Whether the command line named the project.
    pub named: bool,
    /// The target, `None` for the host doctor.
    pub target: Option<Target>,
}

impl Machine {
    /// The project's `Cargo.lock`, when it has one.
    pub(crate) fn lock(&self) -> Option<String> {
        let project = self.project.as_ref().ok()?;
        std::fs::read_to_string(project.root.join("Cargo.lock")).ok()
    }
}

/// A check run: its probe and what it found.
struct Check {
    probe: Probe,
    found: Found,
}

pub fn run(ctx: &Context<'_>, args: &DoctorArgs, out: &mut Output) -> u8 {
    let cwd = match ctx.cwd(out) {
        Ok(cwd) => cwd,
        Err(code) => return code,
    };
    let machine = Arc::new(Machine {
        project: Project::locate(ctx.global.project.as_deref(), &cwd),
        named: ctx.global.project.is_some(),
        target: args.target.map(BuildTarget::target),
    });
    let probes = probes::plan(machine.target);
    let mut checks = examine(&probes, &machine);
    if args.fix {
        fix(&mut checks, &machine, out);
    }
    report(&checks, out)
}

/// Runs every probe at once and returns what each found, in `probes`' order;
/// one that does not finish within [`CHECK_TIMEOUT`] is an error.
fn examine(probes: &[Probe], machine: &Arc<Machine>) -> Vec<Check> {
    let (tx, rx) = mpsc::channel();
    for (at, probe) in probes.iter().enumerate() {
        let (sender, machine, run) = (tx.clone(), Arc::clone(machine), probe.run);
        let spawned = thread::Builder::new()
            .name(format!("viso-doctor-{}", probe.name))
            .spawn(move || {
                let _ = sender.send((at, run(&machine)));
            });
        if spawned.is_err() {
            let _ = tx.send((
                at,
                Some(Found::error(
                    crate::error::INTERNAL_PANIC.name,
                    "the check could not start",
                )),
            ));
        }
    }
    drop(tx);
    let mut found: Vec<Option<Option<Found>>> = vec![None; probes.len()];
    let until = Instant::now() + CHECK_TIMEOUT;
    while found.iter().any(Option::is_none) {
        let left = until.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((at, result)) => found[at] = Some(result),
            Err(_) => break,
        }
    }
    probes
        .iter()
        .zip(found)
        .filter_map(|(probe, found)| {
            let found = match found {
                Some(found) => found?,
                None => Found::error(
                    crate::error::ENV_TOOL.name,
                    format!(
                        "did not finish in {} s; a tool it runs hangs",
                        CHECK_TIMEOUT.as_secs()
                    ),
                ),
            };
            Some(Check {
                probe: *probe,
                found,
            })
        })
        .collect()
}

/// Makes the fix of each check that names one, saying it first, and checks
/// again.
fn fix(checks: &mut [Check], machine: &Arc<Machine>, out: &mut Output) {
    for check in checks.iter_mut() {
        let Some(fix) = check.found.fix.clone() else {
            continue;
        };
        if check.found.status == Status::Ok {
            continue;
        }
        let line = fix.command_line();
        out.progress("fix", &line);
        if out.human() {
            eprintln!("Running `{line}`");
        }
        let made = match &fix {
            Fix::RustTarget(triple) => {
                probes::tool("rustup", &["target", "add", triple], FIX_TIMEOUT).map(drop)
            }
        };
        match made {
            Ok(()) => {
                let again = examine(&[check.probe], machine);
                if let Some(again) = again.into_iter().next() {
                    check.found = again.found;
                }
            }
            Err(why) => {
                check.found.message = format!("{}; `{line}` failed: {why}", check.found.message);
            }
        }
    }
}

/// Reports every check and the commands that close its gaps; the exit code.
fn report(checks: &[Check], out: &mut Output) -> u8 {
    if out.human() {
        println!("Viso Doctor\n");
    }
    for check in checks {
        let found = &check.found;
        out.doctor_check(
            check.probe.name,
            found.status.as_str(),
            found.code,
            &found.message,
            found.suggestion.as_deref(),
            || {
                format!(
                    "[{}] {}: {}\n",
                    found.status.as_str(),
                    check.probe.title,
                    found.message
                )
            },
        );
    }
    let suggestions: Vec<&str> = checks
        .iter()
        .filter(|check| check.found.status != Status::Ok)
        .filter_map(|check| check.found.suggestion.as_deref())
        .collect();
    if out.human() && !suggestions.is_empty() {
        println!("\nSuggested actions:");
        for suggestion in suggestions {
            println!("  {suggestion}");
        }
    }
    if checks.iter().any(|check| check.found.status.fails()) {
        ENVIRONMENT
    } else {
        SUCCESS
    }
}

/// `path` with the home directory written `~`, for a message.
pub(crate) fn shown(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gap_not_required_is_a_warning_without_a_code() {
        let gap = Found::missing("ENV_TOOL", "not installed").suggest("cargo install x");
        let lenient = gap.clone().required(false);
        assert_eq!(lenient.status, Status::Warn);
        assert_eq!(lenient.code, None);
        assert_eq!(lenient.suggestion.as_deref(), Some("cargo install x"));
        assert_eq!(gap.required(true).status, Status::Missing);
        assert!(!Status::Warn.fails());
        assert!(Status::Missing.fails() && Status::Error.fails());
    }

    #[test]
    fn a_fix_suggests_the_command_it_runs() {
        let found =
            Found::missing("ENV_RUST_TARGET", "not installed").fixed_by(Fix::RustTarget("t"));
        assert_eq!(found.suggestion.as_deref(), Some("rustup target add t"));
    }

    #[test]
    fn checks_report_in_order_and_one_that_does_not_apply_is_left_out() {
        fn quick(_: &Machine) -> Option<Found> {
            Some(Found::ok("fine"))
        }
        fn absent(_: &Machine) -> Option<Found> {
            None
        }
        let machine = Arc::new(Machine {
            project: Err(Vec::new()),
            named: false,
            target: None,
        });
        let probes = [
            Probe {
                name: "quick",
                title: "Quick",
                run: quick,
            },
            Probe {
                name: "absent",
                title: "Absent",
                run: absent,
            },
        ];
        let checks = examine(&probes, &machine);
        let names: Vec<&str> = checks.iter().map(|c| c.probe.name).collect();
        assert_eq!(names, ["quick"], "a check that does not apply is left out");
        assert_eq!(checks[0].found.status, Status::Ok);
    }
}
