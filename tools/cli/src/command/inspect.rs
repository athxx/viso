//! `viso inspect query`: the nodes a selector selects in what the app shows
//! (`Viso_CLI.md` §24.3).
//!
//! The app is the one the project's `viso run` session runs, asked through
//! the session's control endpoint, or a headless run of it built for the
//! inspection, which plays a scenario first when one is named. Either way the
//! app gives its census and the tool the source table of the views it
//! mounts: the session from the compiles it plans edits against, the
//! headless path from the package's sources. The selector is parsed before
//! anything runs and evaluated over the two joined.

use std::path::Path;
use std::time::Duration;

use viso_dsl::Diagnostic;
use viso_ende::JsonWriter;
use viso_project::{LockGuard, LockKind, Target};
use viso_view::headless::HeadlessSurface;
use viso_view::inspect::{Census, Inspected, Selector, SourceTable, resolve};

use super::check;
use super::scenario::{self, Case, Ended, Unplanned};
use crate::args::{InspectCommand, QueryArgs};
use crate::context::Context;
use crate::dev::control;
use crate::dev::{ControlAsk, ControlReply};
use crate::error::{
    self, CLI_USAGE, ENV_NO_DEV_SESSION, ENV_TEST_INPUT, INSPECT_ANSWER_INVALID, INSPECT_SELECTOR,
    RUN_TEST_CRASHED, SUCCESS, TEST_EXPECTATION, TEST_INPUT_INVALID,
};
use crate::output::{Output, Source};

/// How long the session has to answer: the app takes its census at its
/// next frame.
const WAIT: Duration = Duration::from_secs(10);

pub fn run(ctx: &Context<'_>, command: &InspectCommand, out: &mut Output) -> u8 {
    match command {
        InspectCommand::Query(args) => query(ctx, args, out),
    }
}

fn query(ctx: &Context<'_>, args: &QueryArgs, out: &mut Output) -> u8 {
    let selector = match Selector::parse(&args.selector) {
        Ok(selector) => selector,
        Err(error) => {
            let caret = " ".repeat(
                args.selector[..error.at.min(args.selector.len())]
                    .chars()
                    .count(),
            );
            return out.failure(
                INSPECT_SELECTOR,
                &format!("`{}` is not a selector: {}", args.selector, error.message),
                &[args.selector.clone(), format!("{caret}^")],
            );
        }
    };
    let target = args.target.map_or(Target::Host, |target| target.target());
    if target == Target::Headless {
        if args.device.is_some() {
            return out.failure(CLI_USAGE, "a headless run has no `--device`", &[]);
        }
        return headless(ctx, &selector, args.scenario.as_deref(), out);
    }
    if args.scenario.is_some() {
        return out.failure(
            CLI_USAGE,
            "`--scenario` plays in a headless run: add `--target headless`",
            &[],
        );
    }
    let endpoint = match endpoint(ctx, target, args.device.as_deref(), out) {
        Ok(endpoint) => endpoint,
        Err(code) => return code,
    };
    match control::call(&endpoint, ControlAsk::Inspect, Some(WAIT)) {
        Ok(ControlReply::Inspected { census, sources }) => {
            let census = Census::from_bytes(&census);
            let sources = SourceTable::from_bytes(&sources);
            match (census, sources) {
                (Ok(census), Ok(sources)) => report(&selector, &census, &sources, out),
                (Err(error), _) | (_, Err(error)) => out.failure(
                    INSPECT_ANSWER_INVALID,
                    &format!("the session's answer does not decode: {error}"),
                    &[],
                ),
            }
        }
        Ok(ControlReply::Refused {
            code,
            message,
            notes,
        }) => out.failure(
            error::by_name(&code).unwrap_or(INSPECT_ANSWER_INVALID),
            &message,
            &notes,
        ),
        Ok(_) => out.failure(
            INSPECT_ANSWER_INVALID,
            "`viso run` answered another question",
            &[],
        ),
        Err(message) => out.failure(ENV_NO_DEV_SESSION, &message, &[]),
    }
}

/// The control endpoint of the project's dev session for `target`: on
/// `device`, else the only one of the target.
pub(super) fn endpoint(
    ctx: &Context<'_>,
    target: Target,
    device: Option<&str>,
    out: &mut Output,
) -> Result<String, u8> {
    let project = ctx.project(out)?;
    let layout = ctx.layout(&project.root);
    let run = match target {
        Target::Host => "viso run".to_owned(),
        target => format!("viso run {}", target.as_str()),
    };
    let holder = match device {
        Some(device) => LockGuard::holder(
            &layout,
            LockKind::DevSession {
                target,
                device: Some(device),
            },
        ),
        None => {
            let bare = format!("dev-{}.lock", target.as_str());
            let on = format!("dev-{}-", target.as_str());
            let mut sessions: Vec<_> = LockGuard::held(&layout)
                .into_iter()
                .filter(|held| {
                    held.path.file_name().is_some_and(|name| {
                        let name = name.to_string_lossy();
                        name == bare || name.starts_with(&on)
                    })
                })
                .collect();
            if sessions.len() > 1 {
                let devices: Vec<String> = sessions
                    .iter()
                    .filter_map(|held| {
                        let name = held.path.file_stem()?.to_string_lossy().into_owned();
                        Some(format!("`{}`", name.strip_prefix(&on)?.to_owned()))
                    })
                    .collect();
                return Err(out.failure(
                    CLI_USAGE,
                    &format!("several `{run}` sessions are running; name one with `--device`"),
                    &[format!("on {}", devices.join(", "))],
                ));
            }
            sessions.pop().map(|held| held.holder)
        }
    };
    match holder {
        Some(holder) => holder.endpoint.clone().ok_or_else(|| {
            out.failure(
                ENV_NO_DEV_SESSION,
                &format!("the `{run}` session of this project takes no requests"),
                &[format!(
                    "it is starting, or failed to open its control endpoint ({holder})"
                )],
            )
        }),
        None => Err(out.failure_next(
            ENV_NO_DEV_SESSION,
            &format!("no `{run}` session is running for this project"),
            &["`viso inspect` inspects the app a dev session runs, or a headless run of it with `--target headless`".to_owned()],
            &[run, "viso inspect query <selector> --target headless".to_owned()],
        )),
    }
}

/// Builds the app headless, plays `scenario` (or lets the app settle) and
/// reports what `selector` selects in what the run showed at its end.
fn headless(
    ctx: &Context<'_>,
    selector: &Selector,
    scenario: Option<&str>,
    out: &mut Output,
) -> u8 {
    let checked = match check::load(ctx, out) {
        Ok(checked) => checked,
        Err(code) => return code,
    };
    let code = checked.code(out);
    if code != SUCCESS {
        return code;
    }
    let root = checked.project.root.clone();
    let (width, height, scale) = checked.project.manifest.test.surface.resolved();
    let surface = HeadlessSurface {
        width,
        height,
        scale: scale.get(),
    };
    let mut case = match case(&root, "inspect", scenario, surface, out) {
        Ok(case) => case,
        Err(code) => return code,
    };
    case.census = true;
    let sources = checked.sources();
    let build = match scenario::build(ctx, Some(sources.clone()), out) {
        Ok(build) => build,
        Err(code) => return code,
    };
    let mut census = None;
    let code = scenario::play(
        ctx,
        &build,
        vec![case],
        out,
        |_| false,
        |out, case, played| {
            let code = match &played.ended {
                Ended::Passed => SUCCESS,
                Ended::Failed { line, message } => out.failure(
                    TEST_EXPECTATION,
                    &format!(
                        "the scenario `{}` failed at line {line}: {message}",
                        case.name
                    ),
                    &[],
                ),
                Ended::Crashed(why) => {
                    let mut notes = vec![why.clone()];
                    notes.extend(played.output.lines().map(str::to_owned));
                    out.failure(RUN_TEST_CRASHED, "the headless run crashed", &notes)
                }
                Ended::Stopped => SUCCESS,
            };
            if code == SUCCESS {
                census = Some(std::fs::read(played.census()));
            }
            played.discard();
            code
        },
    );
    if code != SUCCESS {
        return code;
    }
    match census {
        Some(Ok(bytes)) => match Census::from_bytes(&bytes) {
            Ok(census) => report(selector, &census, &sources, out),
            Err(error) => out.failure(
                INSPECT_ANSWER_INVALID,
                &format!("the run's census does not decode: {error}"),
                &[],
            ),
        },
        Some(Err(error)) => out.failure(
            INSPECT_ANSWER_INVALID,
            &format!("the run wrote no census: {error}"),
            &[],
        ),
        None => code,
    }
}

/// The case a headless run named `name` plays: the scenario `scenario`
/// names under `tests/ui/`, or the app settling at `surface` without one.
pub(super) fn case(
    root: &Path,
    name: &str,
    scenario: Option<&str>,
    surface: HeadlessSurface,
    out: &mut Output,
) -> Result<Case, u8> {
    let Some(scenario) = scenario else {
        return Ok(Case::bare(name.to_owned(), surface));
    };
    let path = match scenario::plan(root, Some(scenario), |_| true, None) {
        Ok(mut planned) => match planned.pop() {
            Some((_, path)) => path,
            None => {
                return Err(out.failure(ENV_TEST_INPUT, &format!("no scenario `{scenario}`"), &[]));
            }
        },
        Err(Unplanned::Nothing(why) | Unplanned::Unreadable(why)) => {
            return Err(out.failure(ENV_TEST_INPUT, &why, &[]));
        }
    };
    let text = std::fs::read_to_string(&path).map_err(|error| {
        out.failure(
            ENV_TEST_INPUT,
            &format!("cannot read `{}`: {error}", path.display()),
            &[],
        )
    })?;
    Case::parse(
        scenario.to_owned(),
        path.clone(),
        text.clone(),
        surface,
        |_| false,
    )
    .map_err(|(line, message)| {
        let source = Source::new(&path, root, &text);
        let start = line_start(&text, line);
        let diagnostic = Diagnostic::error(
            TEST_INPUT_INVALID.name,
            viso_dsl::TextRange::new(start.into(), start.into()),
            message,
        );
        out.source(Some(&source), &[], &diagnostic);
        TEST_INPUT_INVALID.exit
    })
}

/// The byte the 1-based `line` of `text` starts at.
fn line_start(text: &str, line: u32) -> u32 {
    let mut at = 0;
    for (n, part) in text.split_inclusive('\n').enumerate() {
        if n as u32 + 1 >= line {
            break;
        }
        at += part.len();
    }
    at as u32
}

/// Reports what `selector` selects in `census`, joined with `sources`.
fn report(selector: &Selector, census: &Census, sources: &SourceTable, out: &mut Output) -> u8 {
    let inspection = resolve(census, sources);
    let matches: Vec<&Inspected<'_>> = selector
        .select(&inspection)
        .into_iter()
        .map(|at| &inspection.nodes[at])
        .collect();
    let text = selector.to_string();
    out.result(
        |w| {
            w.begin_object();
            w.name("selector");
            w.string(&text);
            w.name("matches");
            w.begin_array();
            for node in &matches {
                write_match(w, node);
            }
            w.end_array();
            w.end_object();
        },
        || {
            let mut lines = String::new();
            for node in &matches {
                lines.push_str(&describe(node));
                lines.push('\n');
            }
            match matches.len() {
                0 => lines.push_str(&format!("no node matches `{text}`\n")),
                1 => lines.push_str(&format!("1 node matches `{text}`\n")),
                n => lines.push_str(&format!("{n} nodes match `{text}`\n")),
            }
            lines
        },
    );
    SUCCESS
}

/// One match as a line: what it is, where it is declared, its box and its
/// semantics.
fn describe(node: &Inspected<'_>) -> String {
    let census = node.node;
    let mut line = match node.source {
        Some((file, source)) => {
            let mut what = source.type_name.clone();
            if let Some(name) = &source.name {
                what.push('#');
                what.push_str(name);
            }
            for style in &source.styles {
                what.push('.');
                what.push_str(style);
            }
            for component in &source.components {
                what.push_str(&format!(" ({component})"));
            }
            format!(
                "{what}  {}:{}:{}",
                file.path, source.start.0, source.start.1
            )
        }
        None => format!("{} {}", census.kind.label(), node.id()),
    };
    let b = census.bounds;
    line.push_str(&format!("  [{}, {} {}×{}]", b.x, b.y, b.w, b.h));
    if !census.shown {
        line.push_str("  hidden");
    }
    if let Some(semantics) = &census.semantics {
        line.push_str("  ");
        line.push_str(viso_view::source_name(semantics.role));
        if let Some(label) = &semantics.label {
            line.push_str(&format!(" {label:?}"));
        }
    }
    line
}

fn write_match(w: &mut JsonWriter, node: &Inspected<'_>) {
    let census = node.node;
    let optional = |w: &mut JsonWriter, value: Option<&str>| match value {
        Some(value) => w.string(value),
        None => w.null(),
    };
    w.begin_object();
    w.name("node_id");
    w.string(&node.id());
    w.name("window");
    w.uint(u64::from(node.window));
    let source = node.source.map(|(_, source)| source);
    w.name("type");
    optional(w, source.map(|s| s.type_name.as_str()));
    w.name("name");
    optional(w, source.and_then(|s| s.name.as_deref()));
    w.name("styles");
    w.begin_array();
    for style in source.iter().flat_map(|s| &s.styles) {
        w.string(style);
    }
    w.end_array();
    w.name("components");
    w.begin_array();
    for component in source.iter().flat_map(|s| &s.components) {
        w.string(component);
    }
    w.end_array();
    w.name("symbol");
    optional(w, node.symbol().as_deref());
    w.name("source_span");
    match node.source {
        Some((file, source)) => {
            w.begin_object();
            w.name("file");
            w.string(&file.path);
            w.name("byte_start");
            w.uint(u64::from(source.bytes.0));
            w.name("byte_end");
            w.uint(u64::from(source.bytes.1));
            w.name("line");
            w.uint(u64::from(source.start.0));
            w.name("column_utf16");
            w.uint(u64::from(source.start.1));
            w.name("end_line");
            w.uint(u64::from(source.end.0));
            w.name("end_column_utf16");
            w.uint(u64::from(source.end.1));
            w.end_object();
        }
        None => w.null(),
    }
    w.name("bounds");
    w.begin_object();
    let b = census.bounds;
    for (name, value) in [("x", b.x), ("y", b.y), ("width", b.w), ("height", b.h)] {
        w.name(name);
        w.number(f64::from(value));
    }
    w.end_object();
    w.name("kind");
    w.string(census.kind.label());
    w.name("shown");
    w.bool(census.shown);
    w.name("focusable");
    w.bool(census.focusable);
    w.name("focused");
    w.bool(census.focused);
    w.name("semantics");
    match &census.semantics {
        None => w.null(),
        Some(semantics) => {
            w.begin_object();
            w.name("role");
            w.string(viso_view::source_name(semantics.role));
            w.name("label");
            optional(w, semantics.label.as_deref());
            w.name("hint");
            optional(w, semantics.hint.as_deref());
            w.name("value");
            optional(w, semantics.value.as_deref());
            w.name("heading_level");
            match semantics.heading_level {
                Some(level) => w.uint(u64::from(level)),
                None => w.null(),
            }
            let state = &semantics.state;
            for (name, value) in [
                ("checked", state.checked),
                ("selected", state.selected),
                ("expanded", state.expanded),
            ] {
                w.name(name);
                match value {
                    Some(value) => w.bool(value),
                    None => w.null(),
                }
            }
            w.name("range_value");
            match state.value {
                Some(value) => w.number(f64::from(value)),
                None => w.null(),
            }
            w.name("range");
            match state.range {
                Some((min, max)) => {
                    w.begin_array();
                    w.number(f64::from(min));
                    w.number(f64::from(max));
                    w.end_array();
                }
                None => w.null(),
            }
            w.end_object();
        }
    }
    w.end_object();
}
