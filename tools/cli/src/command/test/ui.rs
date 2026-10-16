//! The `ui` domain: each scenario under `tests/ui/` played by its own run of
//! a headless build of the app (`Viso_CLI.md` §22.2).
//!
//! A case passes when its report ends with every step passed and each
//! snapshot it took holds to its golden, as `viso snapshot compare` holds it;
//! with `--update-snapshots` a snapshot that is missing its golden or departs
//! from it rewrites the golden instead. A failed step fails the case at the
//! step's line; a run that ends without its report crashed. What a case
//! wrote is shown when it fails, or with `--nocapture`; the directory of a
//! passed case is removed.

use std::fs;
use std::path::Path;

use viso_dsl::Diagnostic;
use viso_view::headless::HeadlessSurface;

use super::{Run, invalid, line_range, relative, report};
use viso_project::Platform;

use crate::args::TestTarget;
use crate::command::check::Checked;
use crate::command::scenario::{self, Build, Case, Ended, Played, Unplanned};
use crate::command::snapshot::{self, Held, Store, Taken, Tally};
use crate::context::Context;
use crate::error::{
    Code, DIAGNOSTICS, ENV_IOS_HOST, ENV_SNAPSHOT_WRITE, ENV_TEST_INPUT, ENV_TEST_TARGET,
    RUN_SNAPSHOT_FAILED, SUCCESS, TEST_SNAPSHOT_INVALID, TEST_SNAPSHOT_MISMATCH,
    TEST_SNAPSHOT_MISSING,
};
use crate::output::{Output, Source, TestStatus};

pub(super) fn run(ctx: &Context<'_>, checked: &Checked, run: &Run<'_>, out: &mut Output) -> u8 {
    let root = &checked.project.root;
    let live = match run.args.target {
        Some(TestTarget::Ios) => Some(Live::Device(Platform::Ios)),
        Some(TestTarget::Android) => Some(Live::Device(Platform::Android)),
        Some(TestTarget::WebGpu) => Some(Live::Browser),
        _ => None,
    };
    if let Some(target) = run.args.target
        && target != TestTarget::Headless
        && live.is_none()
    {
        if run.every && matches!(target, TestTarget::WebDom | TestTarget::WebHybrid) {
            return SUCCESS;
        }
        return run.nothing(
            out,
            "ui",
            ENV_TEST_TARGET,
            &format!(
                "UI scenarios run `headless`, `ios`, `android` or `web-gpu` in this build of viso, not on `{}`",
                target.target()
            ),
        );
    }
    let planned = match scenario::plan(
        root,
        run.args.scenario.as_deref(),
        |name| run.filter.keeps(name),
        run.filter.pattern.as_deref(),
    ) {
        Ok(planned) => planned,
        Err(Unplanned::Nothing(why)) => return run.nothing(out, "ui", ENV_TEST_INPUT, &why),
        Err(Unplanned::Unreadable(why)) => return out.failure(ENV_TEST_INPUT, &why, &[]),
    };

    // Every scenario is read and parsed before anything is built.
    let (width, height, scale) = checked.project.manifest.test.surface.resolved();
    let surface = HeadlessSurface {
        width,
        height,
        scale: scale.get(),
    };
    let store = Store::new(root, None, &ctx.layout(root).viso_dir(), "headless");
    let mut code = SUCCESS;
    let mut cases = Vec::new();
    let mut taken_by = std::collections::HashMap::new();
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
        let parsed = Case::parse(name.clone(), path.clone(), text.clone(), surface, |stem| {
            store.has(stem)
        });
        let failed = match parsed {
            Ok(case) => match snapshot::taken_twice(out, root, &case, &mut taken_by) {
                Some(why) => Some(why),
                None => {
                    cases.push(case);
                    None
                }
            },
            Err((line, message)) => {
                let source = Source::new(&path, root, &text);
                invalid(out, Some(&source), Some(line_range(&text, line)), &message);
                Some(message)
            }
        };
        if let Some(message) = failed {
            report(
                out,
                "ui",
                &name,
                TestStatus::Fail,
                0,
                Some(&message),
                |_| {},
            );
            code = DIAGNOSTICS;
        }
        if run.stopped(out) {
            return code;
        }
    }
    if cases.is_empty() {
        return code;
    }
    match live {
        Some(Live::Device(platform)) => {
            return code.max(on_device(ctx, root, run, platform, cases, out));
        }
        Some(Live::Browser) => return code.max(in_browser(ctx, root, run, cases, out)),
        None => {}
    }

    let sources = cases
        .iter()
        .any(|case| case.queries)
        .then(|| checked.sources());
    let build = match scenario::build(ctx, sources, out) {
        Ok(build) => build,
        Err(failed) => return code.max(failed),
    };
    let mut tally = Tally::default();
    code.max(scenario::play(
        ctx,
        &build,
        cases,
        out,
        |out| run.stopped(out),
        |out, case, played| reported(out, root, &store, &build, case, played, run, &mut tally),
    ))
}

/// Reports a played case, its snapshots held to their goldens; the exit
/// code its end asks for.
#[allow(clippy::too_many_arguments)]
fn reported(
    out: &mut Output,
    root: &Path,
    store: &Store,
    build: &Build,
    case: &Case,
    played: Played,
    run: &Run<'_>,
    tally: &mut Tally,
) -> u8 {
    let duration_ms = played.duration.as_millis() as u64;
    let mut code = snapshot::ended(out, root, case, &played.ended);
    let mut message = match &played.ended {
        Ended::Failed { line, message } => Some(format!(
            "{}:{line}: {message}",
            relative(root, &case.path).display()
        )),
        Ended::Crashed(why) => Some(why.clone()),
        Ended::Passed | Ended::Stopped => None,
    };
    for shot in &played.shots {
        let failed = held(out, root, store, build, case, &played, shot, run, tally);
        if let Some((failed, why)) = failed {
            code = code.max(failed);
            message.get_or_insert(why);
        }
    }
    let status = if code == SUCCESS {
        TestStatus::Pass
    } else {
        TestStatus::Fail
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
        played.discard();
    } else {
        out.case_output(&case.name, &played.output);
    }
    code
}

/// Holds the snapshot `shot` of `case` to its golden, or rewrites the golden
/// under `--update-snapshots`; the exit code and message of a failure.
#[allow(clippy::too_many_arguments)]
fn held(
    out: &mut Output,
    root: &Path,
    store: &Store,
    build: &Build,
    case: &Case,
    played: &Played,
    shot: &scenario::Shot,
    run: &Run<'_>,
    tally: &mut Tally,
) -> Option<(u8, String)> {
    let at = |out: &mut Output, code: Code, message: &str, notes: Vec<String>| {
        let source = Source::new(&case.path, root, &case.text);
        let mut diagnostic = Diagnostic::error(
            code.name,
            line_range(&case.text, case.shown(shot.line)),
            message,
        );
        diagnostic.notes = notes;
        out.source(Some(&source), &[], &diagnostic);
        Some((code.exit, message.to_owned()))
    };
    let taken = match Taken::read(&played.snapshots(), &shot.name, case.surface) {
        Ok(taken) => taken,
        Err(why) => return at(out, RUN_SNAPSHOT_FAILED, &why, Vec::new()),
    };
    let held = match store.hold(&taken) {
        Ok(held) => held,
        Err(why) => return Some((out.failure(ENV_SNAPSHOT_WRITE, &why, &[]), why)),
    };
    let rewrite = run.args.update_snapshots && matches!(played.ended, Ended::Passed);
    match held {
        Held::Matched => None,
        _ if rewrite => match snapshot::write(out, store, &taken, build, tally) {
            SUCCESS => None,
            failed => Some((
                failed,
                format!("the golden of `{}` was not written", shot.name),
            )),
        },
        Held::Missing => at(
            out,
            TEST_SNAPSHOT_MISSING,
            &format!(
                "the snapshot `{}` has no golden `{}`",
                shot.name,
                store.golden_name(&shot.name)
            ),
            vec!["write it with `viso test ui --update-snapshots`".to_owned()],
        ),
        Held::Invalid(why) => at(out, TEST_SNAPSHOT_INVALID, &why, Vec::new()),
        Held::Differs(departure) => at(
            out,
            TEST_SNAPSHOT_MISMATCH,
            &departure.message(&shot.name),
            departure.notes(),
        ),
    }
}

/// Where a run off headless plays its cases.
#[derive(Clone, Copy)]
enum Live {
    Device(Platform),
    Browser,
}

/// Plays `cases` in a headless Chromium browser, or skips each when there is
/// none.
fn in_browser(
    ctx: &Context<'_>,
    root: &Path,
    run: &Run<'_>,
    cases: Vec<Case>,
    out: &mut Output,
) -> u8 {
    let Some(browser) = crate::deploy::web::headless_browser() else {
        let why = format!(
            "no Chromium browser to run `web-gpu` headless (install Chrome, or name one in `{}`)",
            crate::deploy::web::CHROME_ENV
        );
        for case in &cases {
            report(
                out,
                "ui",
                &case.name,
                TestStatus::Skip,
                0,
                Some(&why),
                |_| {},
            );
        }
        return SUCCESS;
    };
    super::live::run(
        ctx,
        root,
        run,
        &super::live::Venue::Browser { browser },
        cases,
        out,
    )
}

/// Plays `cases` on the simulator or emulator of `platform` that
/// `--device`, the project's default or the sole profile names.
fn on_device(
    ctx: &Context<'_>,
    root: &Path,
    run: &Run<'_>,
    platform: Platform,
    cases: Vec<Case>,
    out: &mut Output,
) -> u8 {
    if platform == Platform::Ios && !cfg!(target_os = "macos") {
        return out.failure(ENV_IOS_HOST, "the iOS Simulator runs on macOS only", &[]);
    }
    let store = match crate::device::store(out) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let profile =
        match crate::device::select(out, &store, platform, run.args.device.as_deref(), root) {
            Ok(profile) => profile,
            Err(code) => return code,
        };
    super::live::run(
        ctx,
        root,
        run,
        &super::live::Venue::Device {
            platform,
            profile: &profile,
        },
        cases,
        out,
    )
}
