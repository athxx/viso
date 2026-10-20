//! `viso inspect`: what the app shows and holds (`Viso_CLI.md` §24).
//!
//! The app is the one the project's `viso run` session runs, asked through
//! the session's control endpoint, or a headless run of it built for the
//! inspection, which plays a scenario first when one is named. Either way the
//! app gives its census and the tool the source table of the views it
//! mounts: the session from the compiles it plans edits against, the
//! headless path from the package's sources. `tree` shows the two joined,
//! `query` the nodes a selector selects in them; `node` asks the app what
//! it holds for those nodes and `frame` what a window's last frame drew,
//! named back to the source through the same table. A selector is parsed
//! before anything runs.

mod events;

pub(crate) use events::{Streamed, stream};

use std::path::Path;
use std::time::Duration;

use viso_dev_protocol::headless::HeadlessSurface;
use viso_dev_protocol::inspect::{
    Census, EventLog, FileSources, FrameDamage, FrameDetail, InspectAsk, Inspected, Inspection,
    InspectorCaps, MAX_ASKED_NODES, NodeDetail, NodeDetails, Rect, Selector, SourceTable,
    StateSource, WindowDetail, dirty_names, resolve, state_names,
};
use viso_dsl::Diagnostic;
use viso_ende::JsonWriter;
use viso_project::{LockGuard, LockKind, Target};

use super::check;
use super::scenario::{self, Case, Ended, Unplanned};
use crate::args::{
    FrameArgs, InspectArgs, InspectCommand, InspectedApp, NodeArgs, QueryArgs, TreeArgs,
};
use crate::context::Context;
use crate::dev::control;
use crate::dev::{ControlAsk, ControlReply};
use crate::error::{
    self, CLI_USAGE, ENV_NO_DEV_SESSION, ENV_TEST_INPUT, INSPECT_ANSWER_INVALID, INSPECT_SELECTOR,
    INSPECT_UNSUPPORTED, RUN_TEST_CRASHED, SUCCESS, TEST_EXPECTATION, TEST_INPUT_INVALID,
};
use crate::output::{Output, Source, human};

/// How long the session has to answer: the app answers at its next frame.
const WAIT: Duration = Duration::from_secs(10);

/// What a headless run's inspector tells: everything, its events as a
/// journal of the whole run.
const HEADLESS: InspectorCaps = InspectorCaps::TREE
    .with(InspectorCaps::NODES)
    .with(InspectorCaps::FRAME)
    .with(InspectorCaps::GPU_TIME)
    .with(InspectorCaps::EVENTS);

pub fn run(ctx: &Context<'_>, args: &InspectArgs, out: &mut Output) -> u8 {
    let code = match &args.command {
        None => events::attach(ctx, &args.attach, out),
        Some(InspectCommand::Tree(args)) => tree(ctx, args, out),
        Some(InspectCommand::Node(args)) => node(ctx, args, out),
        Some(InspectCommand::Frame(args)) => frame(ctx, args, out),
        Some(InspectCommand::Events(args)) => events::events(ctx, args, out),
        Some(InspectCommand::Query(args)) => query(ctx, args, out),
    };
    match code {
        Ok(()) => SUCCESS,
        Err(code) => code,
    }
}

/// The app asked, its census and the table of its views.
struct App {
    census: Census,
    sources: SourceTable,
    inspector: InspectorCaps,
    asked: Asked,
}

/// How to ask the app more.
enum Asked {
    /// Through the control endpoint of its session.
    Session(String),
    /// A headless run, gone: what it wrote of its first window at its end.
    Headless {
        detail: Option<WindowDetail>,
        events: Option<EventLog>,
    },
}

/// What a headless run writes at its end besides its census.
#[derive(Clone, Copy)]
struct Wants {
    detail: bool,
    events: bool,
}

impl Wants {
    const CENSUS: Wants = Wants {
        detail: false,
        events: false,
    };
    const DETAIL: Wants = Wants {
        detail: true,
        events: false,
    };
    const EVENTS: Wants = Wants {
        detail: false,
        events: true,
    };
}

impl App {
    /// The detail of `nodes` of window `window`, in the order asked; a node
    /// gone since the census has none.
    fn nodes(
        &self,
        window: u32,
        nodes: &[(u32, u32)],
        out: &mut Output,
    ) -> Result<Vec<NodeDetail>, u8> {
        self.needs(InspectorCaps::NODES, "the detail of a node", out)?;
        let found = match &self.asked {
            Asked::Session(endpoint) => {
                let mut found = Vec::new();
                for chunk in nodes.chunks(MAX_ASKED_NODES) {
                    let ask = InspectAsk::Nodes {
                        window,
                        nodes: chunk.to_vec(),
                    };
                    let answer = ask_session(endpoint, ask, out)?;
                    let details = NodeDetails::from_bytes(&answer).map_err(|error| {
                        invalid(
                            out,
                            &format!("the app's node detail does not decode: {error}"),
                        )
                    })?;
                    found.extend(details.nodes);
                }
                found
            }
            Asked::Headless { detail, .. } => match (window, detail) {
                (0, Some(detail)) => detail.nodes.nodes.clone(),
                _ => Vec::new(),
            },
        };
        Ok(nodes
            .iter()
            .filter_map(|&(index, generation)| {
                found
                    .iter()
                    .find(|d| d.index == index && d.generation == generation)
                    .cloned()
            })
            .collect())
    }

    /// What window `window`'s last frame drew and holds.
    fn frame(&self, window: u32, out: &mut Output) -> Result<FrameDetail, u8> {
        self.needs(InspectorCaps::FRAME, "the detail of a frame", out)?;
        match &self.asked {
            Asked::Session(endpoint) => {
                let answer = ask_session(endpoint, InspectAsk::Frame { window }, out)?;
                FrameDetail::from_bytes(&answer).map_err(|error| {
                    invalid(
                        out,
                        &format!("the app's frame detail does not decode: {error}"),
                    )
                })
            }
            Asked::Headless {
                detail: Some(detail),
                ..
            } if window == 0 => Ok(detail.frame.clone()),
            Asked::Headless { .. } => Err(invalid(out, "the run wrote no detail of that window")),
        }
    }

    /// Fails unless the app's inspector tells `what`, which `cap` names.
    fn needs(&self, cap: InspectorCaps, what: &str, out: &mut Output) -> Result<(), u8> {
        if self.inspector.contains(cap) {
            return Ok(());
        }
        Err(out.failure(
            INSPECT_UNSUPPORTED,
            &format!("the app's inspector does not tell {what}"),
            &[format!(
                "it tells: {}",
                self.inspector.names().collect::<Vec<_>>().join(", ")
            )],
        ))
    }
}

/// Asks the session at `endpoint` what `ask` asks: the app's answer.
fn ask_session(endpoint: &str, ask: InspectAsk, out: &mut Output) -> Result<Vec<u8>, u8> {
    match control::call(endpoint, ControlAsk::Inspect(ask), Some(WAIT)) {
        Ok(ControlReply::Inspected { answer, .. }) => Ok(answer),
        Ok(reply) => Err(refused(reply, out)),
        Err(message) => Err(out.failure(ENV_NO_DEV_SESSION, &message, &[])),
    }
}

/// The failure a reply other than the one asked for is.
fn refused(reply: ControlReply, out: &mut Output) -> u8 {
    match reply {
        ControlReply::Refused {
            code,
            message,
            notes,
        } => out.failure(
            error::by_name(&code).unwrap_or(INSPECT_ANSWER_INVALID),
            &message,
            &notes,
        ),
        _ => invalid(out, "`viso run` answered another question"),
    }
}

fn invalid(out: &mut Output, message: &str) -> u8 {
    out.failure(INSPECT_ANSWER_INVALID, message, &[])
}

/// The app `app` names, its census taken: the session's, or a headless run
/// built and played for it, which writes what `wants` names too.
fn connect(
    ctx: &Context<'_>,
    app: &InspectedApp,
    wants: Wants,
    out: &mut Output,
) -> Result<App, u8> {
    let target = app.target.map_or(Target::Host, |target| target.target());
    if target == Target::Headless {
        if app.device.is_some() {
            return Err(out.failure(CLI_USAGE, "a headless run has no `--device`", &[]));
        }
        return headless(ctx, app.scenario.as_deref(), wants, out);
    }
    if app.scenario.is_some() {
        return Err(out.failure(
            CLI_USAGE,
            "`--scenario` plays in a headless run: add `--target headless`",
            &[],
        ));
    }
    let endpoint = endpoint(ctx, target, app.device.as_deref(), out)?;
    match control::call(
        &endpoint,
        ControlAsk::Inspect(InspectAsk::Census),
        Some(WAIT),
    ) {
        Ok(ControlReply::Inspected {
            answer,
            sources,
            inspector,
        }) => {
            let census = Census::from_bytes(&answer);
            let sources = SourceTable::from_bytes(&sources);
            match (census, sources) {
                (Ok(census), Ok(sources)) => Ok(App {
                    census,
                    sources,
                    inspector,
                    asked: Asked::Session(endpoint),
                }),
                (Err(error), _) | (_, Err(error)) => Err(invalid(
                    out,
                    &format!("the session's answer does not decode: {error}"),
                )),
            }
        }
        Ok(reply) => Err(refused(reply, out)),
        Err(message) => Err(out.failure(ENV_NO_DEV_SESSION, &message, &[])),
    }
}

/// `text` as a selector, or the failure it is.
fn selector(text: &str, out: &mut Output) -> Result<Selector, u8> {
    Selector::parse(text).map_err(|error| {
        let caret = " ".repeat(text[..error.at.min(text.len())].chars().count());
        out.failure(
            INSPECT_SELECTOR,
            &format!("`{text}` is not a selector: {}", error.message),
            &[text.to_owned(), format!("{caret}^")],
        )
    })
}

/// `text` as a node ID, `<index>:<generation>`.
fn node_id(text: &str) -> Option<(u32, u32)> {
    let (index, generation) = text.split_once(':')?;
    Some((index.parse().ok()?, generation.parse().ok()?))
}

/// What the app's inspector does not tell, by name.
fn unavailable(inspector: InspectorCaps) -> Vec<&'static str> {
    InspectorCaps::ALL
        .iter()
        .filter(|(cap, _)| !inspector.contains(*cap))
        .map(|(_, name)| *name)
        .collect()
}

/// The `inspector` and `unavailable` members of a result.
fn write_inspector(w: &mut JsonWriter, inspector: InspectorCaps) {
    w.name("inspector");
    w.begin_array();
    for name in inspector.names() {
        w.string(name);
    }
    w.end_array();
    w.name("unavailable");
    w.begin_array();
    for name in unavailable(inspector) {
        w.string(name);
    }
    w.end_array();
}

fn query(ctx: &Context<'_>, args: &QueryArgs, out: &mut Output) -> Result<(), u8> {
    let selector = selector(&args.selector, out)?;
    let app = connect(ctx, &args.app, Wants::CENSUS, out)?;
    let inspection = resolve(&app.census, &app.sources);
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
            lines.push_str(&count_line(matches.len(), &text));
            lines
        },
    );
    Ok(())
}

/// The last line of a report of `n` nodes `selector` selects.
fn count_line(n: usize, selector: &str) -> String {
    match n {
        0 => format!("no node matches `{selector}`\n"),
        1 => format!("1 node matches `{selector}`\n"),
        n => format!("{n} nodes match `{selector}`\n"),
    }
}

fn tree(ctx: &Context<'_>, args: &TreeArgs, out: &mut Output) -> Result<(), u8> {
    let app = connect(ctx, &args.app, Wants::CENSUS, out)?;
    let inspection = resolve(&app.census, &app.sources);
    let depths = depths(&inspection);
    let windows = app.census.windows.len();
    out.result(
        |w| {
            w.begin_object();
            w.name("windows");
            w.begin_array();
            for window in 0..windows as u32 {
                w.begin_object();
                w.name("window");
                w.uint(u64::from(window));
                w.name("nodes");
                w.begin_array();
                for node in inspection.nodes.iter().filter(|n| n.window == window) {
                    w.begin_object();
                    write_fields(w, node);
                    w.name("parent");
                    match node.parent {
                        Some(parent) => w.string(&inspection.nodes[parent].id()),
                        None => w.null(),
                    }
                    w.end_object();
                }
                w.end_array();
                w.end_object();
            }
            w.end_array();
            write_inspector(w, app.inspector);
            w.end_object();
        },
        || {
            let mut lines = String::new();
            for window in 0..windows as u32 {
                if windows > 1 {
                    lines.push_str(&format!("window {window}\n"));
                }
                for (at, node) in inspection.nodes.iter().enumerate() {
                    if node.window == window {
                        lines.push_str(&"  ".repeat(depths[at]));
                        lines.push_str(&describe(node));
                        lines.push('\n');
                    }
                }
            }
            let n = inspection.nodes.len();
            lines.push_str(&match (n, windows) {
                (1, _) => "1 node\n".to_owned(),
                (n, 0 | 1) => format!("{n} nodes\n"),
                (n, w) => format!("{n} nodes in {w} windows\n"),
            });
            lines
        },
    );
    Ok(())
}

/// The depth of each node of `inspection` in its window's tree.
fn depths(inspection: &Inspection<'_>) -> Vec<usize> {
    let mut depths: Vec<usize> = Vec::with_capacity(inspection.nodes.len());
    for node in &inspection.nodes {
        // A census lists a window in pre-order: a parent comes first.
        let depth = node.parent.map_or(0, |parent| depths[parent] + 1);
        depths.push(depth);
    }
    depths
}

fn node(ctx: &Context<'_>, args: &NodeArgs, out: &mut Output) -> Result<(), u8> {
    let id = node_id(&args.selector);
    let selector = match id {
        Some(_) => None,
        None => Some(selector(&args.selector, out)?),
    };
    let app = connect(ctx, &args.app, Wants::DETAIL, out)?;
    let inspection = resolve(&app.census, &app.sources);
    let matches: Vec<usize> = match (&selector, id) {
        (Some(selector), _) => selector.select(&inspection),
        (None, Some((index, generation))) => (0..inspection.nodes.len())
            .filter(|&at| {
                let node = inspection.nodes[at].node;
                node.index == index && node.generation == generation
            })
            .collect(),
        (None, None) => Vec::new(),
    };
    let mut details: Vec<Option<NodeDetail>> = vec![None; matches.len()];
    if !matches.is_empty() {
        let mut windows: Vec<u32> = matches
            .iter()
            .map(|&at| inspection.nodes[at].window)
            .collect();
        windows.dedup();
        for window in windows {
            let asked: Vec<(usize, (u32, u32))> = matches
                .iter()
                .enumerate()
                .filter(|(_, at)| inspection.nodes[**at].window == window)
                .map(|(n, &at)| {
                    (
                        n,
                        (
                            inspection.nodes[at].node.index,
                            inspection.nodes[at].node.generation,
                        ),
                    )
                })
                .collect();
            let ids: Vec<(u32, u32)> = asked.iter().map(|(_, id)| *id).collect();
            for found in app.nodes(window, &ids, out)? {
                if let Some((n, _)) = asked
                    .iter()
                    .find(|(_, id)| *id == (found.index, found.generation))
                {
                    details[*n] = Some(found);
                }
            }
        }
    }
    let text = selector
        .as_ref()
        .map_or_else(|| args.selector.clone(), Selector::to_string);
    let rows: Vec<(&Inspected<'_>, Option<&NodeDetail>)> = matches
        .iter()
        .zip(&details)
        .map(|(&at, detail)| (&inspection.nodes[at], detail.as_ref()))
        .collect();
    out.result(
        |w| {
            w.begin_object();
            w.name("selector");
            w.string(&text);
            w.name("nodes");
            w.begin_array();
            for (node, detail) in &rows {
                w.begin_object();
                write_fields(w, node);
                w.name("detail");
                match detail {
                    Some(detail) => write_detail(w, &app.sources, node, detail),
                    None => w.null(),
                }
                w.end_object();
            }
            w.end_array();
            write_inspector(w, app.inspector);
            w.end_object();
        },
        || {
            let mut lines = String::new();
            for (node, detail) in &rows {
                lines.push_str(&describe(node));
                lines.push('\n');
                match detail {
                    Some(detail) => lines.push_str(&describe_detail(&app.sources, node, detail)),
                    None => lines.push_str("  gone since the census\n"),
                }
            }
            lines.push_str(&count_line(rows.len(), &text));
            lines
        },
    );
    Ok(())
}

/// What a binding of `node` to the cell of durable identity `key` is in
/// the source: the source, and the properties bound to it.
fn bound<'a>(
    sources: &'a SourceTable,
    node: &Inspected<'a>,
    key: Option<(u64, u64)>,
) -> (Option<(&'a FileSources, &'a StateSource)>, Vec<&'a str>) {
    let Some(key) = key else {
        return (None, Vec::new());
    };
    let own = node.source.map(|(file, _)| file);
    let state = own
        .and_then(|file| Some((file, file.state(key)?)))
        .or_else(|| {
            sources
                .files
                .iter()
                .find_map(|file| Some((file, file.state(key)?)))
        });
    let properties = node
        .source
        .map(|(_, source)| {
            source
                .bindings
                .iter()
                .filter(|(_, bound)| *bound == key)
                .map(|(property, _)| property.as_str())
                .collect()
        })
        .unwrap_or_default();
    (state, properties)
}

fn rect(r: Rect) -> String {
    format!("[{}, {} {}×{}]", r.x, r.y, r.w, r.h)
}

/// A node's detail as indented lines under its match line.
fn describe_detail(sources: &SourceTable, node: &Inspected<'_>, detail: &NodeDetail) -> String {
    let mut lines = String::new();
    for group in &detail.groups {
        if group.properties.is_empty() {
            continue;
        }
        let properties: Vec<String> = group
            .properties
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect();
        lines.push_str(&format!("  {:<9}{}\n", group.name, properties.join("; ")));
    }
    lines.push_str(&format!(
        "  {:<9}world {}  local {}\n",
        "box",
        rect(detail.world),
        rect(detail.bounds)
    ));
    if !detail.chain.is_empty() {
        let links: Vec<String> = detail
            .chain
            .iter()
            .map(|link| {
                let mut text = format!("{}:{}", link.index, link.generation);
                if link.translate != [0.0, 0.0] {
                    text.push_str(&format!(
                        " translate {} {}",
                        link.translate[0], link.translate[1]
                    ));
                }
                if link.scroll != [0.0, 0.0] {
                    text.push_str(&format!(" scroll {} {}", link.scroll[0], link.scroll[1]));
                }
                text
            })
            .collect();
        lines.push_str(&format!("  {:<9}{}\n", "chain", links.join(" > ")));
    }
    let dirty: Vec<&str> = dirty_names(detail.dirty).collect();
    lines.push_str(&format!(
        "  {:<9}{}\n",
        "dirty",
        if dirty.is_empty() {
            "clean".to_owned()
        } else {
            dirty.join(", ")
        }
    ));
    let states: Vec<&str> = state_names(detail.states).collect();
    if !states.is_empty() {
        lines.push_str(&format!("  {:<9}{}\n", "states", states.join(", ")));
    }
    if !detail.route.is_empty() {
        let stops: Vec<String> = detail
            .route
            .iter()
            .map(|stop| {
                let mut what = Vec::new();
                if stop.pointer {
                    what.push("pointer");
                }
                if stop.key {
                    what.push("key");
                }
                if stop.focusable {
                    what.push("focusable");
                }
                if stop.disabled {
                    what.push("disabled");
                }
                format!("{}:{} {}", stop.index, stop.generation, what.join("+"))
            })
            .collect();
        lines.push_str(&format!("  {:<9}{}\n", "route", stops.join(" > ")));
    }
    for binding in &detail.bindings {
        let (state, properties) = bound(sources, node, binding.key);
        let to = match state {
            Some((file, state)) => {
                let ty = state
                    .type_name
                    .as_deref()
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default();
                format!(
                    "{}{ty} = {}  {}:{}:{}",
                    state.name, binding.value, file.path, state.start.0, state.start.1
                )
            }
            None => format!(
                "cell {}:{} = {}",
                binding.cell.0, binding.cell.1, binding.value
            ),
        };
        let properties = if properties.is_empty() {
            String::new()
        } else {
            format!("{} ← ", properties.join(", "))
        };
        let classes: Vec<&str> = dirty_names(binding.class).collect();
        let dynamic = if binding.dynamic { ", dynamic" } else { "" };
        lines.push_str(&format!(
            "  {:<9}{properties}{to}  ({}{dynamic})\n",
            "binding",
            classes.join(", ")
        ));
    }
    let paint = &detail.paint;
    let own: Vec<String> = paint
        .own
        .iter()
        .map(|(kind, n)| {
            if *n == 1 {
                kind.clone()
            } else {
                format!("{kind}×{n}")
            }
        })
        .collect();
    let batches: Vec<String> = paint
        .batches
        .iter()
        .map(|&(batch, n)| {
            if n == 1 {
                format!("#{batch}")
            } else {
                format!("#{batch}×{n}")
            }
        })
        .collect();
    lines.push_str(&format!(
        "  {:<9}primitives {}..{}{}{}\n",
        "paint",
        paint.start,
        paint.start + paint.len,
        if own.is_empty() {
            String::new()
        } else {
            format!(", own: {}", own.join(", "))
        },
        if batches.is_empty() {
            String::new()
        } else {
            format!(", batches {}", batches.join(" "))
        }
    ));
    lines
}

fn write_rect(w: &mut JsonWriter, r: Rect) {
    w.begin_object();
    for (name, value) in [("x", r.x), ("y", r.y), ("width", r.w), ("height", r.h)] {
        w.name(name);
        w.number(f64::from(value));
    }
    w.end_object();
}

fn write_detail(
    w: &mut JsonWriter,
    sources: &SourceTable,
    node: &Inspected<'_>,
    detail: &NodeDetail,
) {
    w.begin_object();
    for group in &detail.groups {
        w.name(&group.name);
        w.begin_object();
        for (name, value) in &group.properties {
            w.name(name);
            w.string(value);
        }
        w.end_object();
    }
    w.name("world");
    write_rect(w, detail.world);
    w.name("local");
    write_rect(w, detail.bounds);
    w.name("transform_chain");
    w.begin_array();
    for link in &detail.chain {
        w.begin_object();
        w.name("node_id");
        w.string(&format!("{}:{}", link.index, link.generation));
        for (name, [x, y]) in [("translate", link.translate), ("scroll", link.scroll)] {
            w.name(name);
            w.begin_array();
            w.number(f64::from(x));
            w.number(f64::from(y));
            w.end_array();
        }
        w.end_object();
    }
    w.end_array();
    w.name("dirty");
    w.begin_array();
    for name in dirty_names(detail.dirty) {
        w.string(name);
    }
    w.end_array();
    w.name("states");
    w.begin_array();
    for name in state_names(detail.states) {
        w.string(name);
    }
    w.end_array();
    w.name("event_route");
    w.begin_array();
    for stop in &detail.route {
        w.begin_object();
        w.name("node_id");
        w.string(&format!("{}:{}", stop.index, stop.generation));
        w.name("pointer");
        w.bool(stop.pointer);
        w.name("key");
        w.bool(stop.key);
        w.name("focusable");
        w.bool(stop.focusable);
        w.name("disabled");
        w.bool(stop.disabled);
        w.end_object();
    }
    w.end_array();
    w.name("bindings");
    w.begin_array();
    for binding in &detail.bindings {
        let (state, properties) = bound(sources, node, binding.key);
        w.begin_object();
        w.name("properties");
        w.begin_array();
        for property in properties {
            w.string(property);
        }
        w.end_array();
        w.name("state");
        match state {
            Some((_, state)) => w.string(&state.name),
            None => w.null(),
        }
        w.name("type");
        match state.and_then(|(_, state)| state.type_name.as_deref()) {
            Some(ty) => w.string(ty),
            None => w.null(),
        }
        w.name("declared");
        match state {
            Some((file, state)) => {
                w.begin_object();
                w.name("file");
                w.string(&file.path);
                w.name("line");
                w.uint(u64::from(state.start.0));
                w.name("column_utf16");
                w.uint(u64::from(state.start.1));
                w.end_object();
            }
            None => w.null(),
        }
        w.name("cell");
        w.string(&format!("{}:{}", binding.cell.0, binding.cell.1));
        w.name("value");
        w.string(&binding.value);
        w.name("invalidates");
        w.begin_array();
        for name in dirty_names(binding.class) {
            w.string(name);
        }
        w.end_array();
        w.name("dynamic");
        w.bool(binding.dynamic);
        w.end_object();
    }
    w.end_array();
    w.name("paint");
    w.begin_object();
    w.name("start");
    w.uint(u64::from(detail.paint.start));
    w.name("len");
    w.uint(u64::from(detail.paint.len));
    w.name("own");
    w.begin_array();
    for (kind, n) in &detail.paint.own {
        w.begin_object();
        w.name("kind");
        w.string(kind);
        w.name("count");
        w.uint(u64::from(*n));
        w.end_object();
    }
    w.end_array();
    w.name("batches");
    w.begin_array();
    for &(batch, n) in &detail.paint.batches {
        w.begin_object();
        w.name("batch");
        w.uint(u64::from(batch));
        w.name("draws");
        w.uint(u64::from(n));
        w.end_object();
    }
    w.end_array();
    w.end_object();
    w.end_object();
}

fn frame(ctx: &Context<'_>, args: &FrameArgs, out: &mut Output) -> Result<(), u8> {
    let app = connect(ctx, &args.app, Wants::DETAIL, out)?;
    let windows = app.census.windows.len() as u32;
    if args.window >= windows.max(1) {
        return Err(out.failure(
            CLI_USAGE,
            &format!("the app has no window {}", args.window),
            &[match windows {
                0 | 1 => "it has one window, 0".to_owned(),
                n => format!("its windows are 0 to {}", n - 1),
            }],
        ));
    }
    let frame = app.frame(args.window, out)?;
    out.result(
        |w| {
            w.begin_object();
            w.name("window");
            w.uint(u64::from(frame.window));
            w.name("counters");
            w.begin_object();
            for (name, value) in &frame.counters {
                w.name(name);
                w.uint(*value);
            }
            w.end_object();
            w.name("batches");
            w.begin_array();
            for batch in &frame.batches {
                w.begin_object();
                w.name("pipeline");
                w.string(&batch.pipeline);
                w.name("start");
                w.uint(u64::from(batch.range.0));
                w.name("count");
                w.uint(u64::from(batch.range.1));
                w.name("clip");
                match batch.clip {
                    Some(clip) => write_rect(w, clip),
                    None => w.null(),
                }
                w.name("offscreen");
                w.bool(batch.offscreen);
                w.name("cost");
                w.string(&batch.cost);
                w.end_object();
            }
            w.end_array();
            w.name("damage");
            match &frame.damage {
                FrameDamage::None => w.null(),
                FrameDamage::Whole => w.string("whole"),
                FrameDamage::Rects(rects) => {
                    w.begin_array();
                    for &r in rects {
                        write_rect(w, r);
                    }
                    w.end_array();
                }
            }
            w.name("resources");
            w.begin_array();
            for resource in &frame.resources {
                w.begin_object();
                w.name("group");
                w.string(&resource.group);
                w.name("name");
                w.string(&resource.name);
                w.name("count");
                w.uint(resource.count);
                for (name, value) in [("bytes", resource.bytes), ("budget", resource.budget)] {
                    w.name(name);
                    match value {
                        Some(value) => w.uint(value),
                        None => w.null(),
                    }
                }
                w.name("note");
                match &resource.note {
                    Some(note) => w.string(note),
                    None => w.null(),
                }
                w.end_object();
            }
            w.end_array();
            write_inspector(w, app.inspector);
            w.end_object();
        },
        || describe_frame(&frame),
    );
    Ok(())
}

fn describe_frame(frame: &FrameDetail) -> String {
    let counter = |name: &str| {
        frame
            .counters
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0, |(_, v)| *v)
    };
    let mut lines = format!(
        "window {}: {} nodes, {} primitives, {} draw calls, {} batches\n",
        frame.window,
        counter("nodes"),
        counter("primitives"),
        counter("draw_calls"),
        frame.batches.len()
    );
    match &frame.damage {
        FrameDamage::None => lines.push_str("damage  none\n"),
        FrameDamage::Whole => lines.push_str("damage  whole surface\n"),
        FrameDamage::Rects(rects) => {
            let rects: Vec<String> = rects.iter().map(|&r| rect(r)).collect();
            lines.push_str(&format!("damage  {}\n", rects.join(" ")));
        }
    }
    lines.push_str("counters\n");
    let width = frame
        .counters
        .iter()
        .map(|(n, _)| n.len())
        .max()
        .unwrap_or(0);
    for (name, value) in &frame.counters {
        lines.push_str(&format!("  {name:<width$}  {value}\n"));
    }
    if !frame.batches.is_empty() {
        lines.push_str("batches\n");
        let width = frame
            .batches
            .iter()
            .map(|b| b.pipeline.len())
            .max()
            .unwrap_or(0);
        for (n, batch) in frame.batches.iter().enumerate() {
            let (start, count) = batch.range;
            let mut line = format!("  {n:>3}  {:<width$}  {start}+{count}", batch.pipeline);
            if let Some(clip) = batch.clip {
                line.push_str(&format!("  clip {}", rect(clip)));
            }
            if batch.offscreen {
                line.push_str("  offscreen");
            }
            line.push_str(&format!("  {}\n", batch.cost));
            lines.push_str(&line);
        }
    }
    if !frame.resources.is_empty() {
        lines.push_str("resources\n");
        let width = frame
            .resources
            .iter()
            .map(|r| r.name.len())
            .max()
            .unwrap_or(0);
        for resource in &frame.resources {
            let mut line = format!(
                "  {:<5}  {:<width$}  {:>5}",
                resource.group, resource.name, resource.count
            );
            match (resource.bytes, resource.budget) {
                (Some(bytes), Some(budget)) => line.push_str(&format!(
                    "  {} / {}",
                    human::size(bytes),
                    human::size(budget)
                )),
                (Some(bytes), None) => line.push_str(&format!("  {}", human::size(bytes))),
                (None, _) => {}
            }
            if let Some(note) = &resource.note {
                line.push_str(&format!("  {note}"));
            }
            line.push('\n');
            lines.push_str(&line);
        }
    }
    lines
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

/// Builds the app headless and plays `scenario` (or lets the app settle):
/// the census the run wrote at its end, and what `wants` names.
fn headless(
    ctx: &Context<'_>,
    scenario: Option<&str>,
    wants: Wants,
    out: &mut Output,
) -> Result<App, u8> {
    let checked = check::load(ctx, out)?;
    let code = checked.code(out);
    if code != SUCCESS {
        return Err(code);
    }
    let root = checked.project.root.clone();
    let (width, height, scale) = checked.project.manifest.test.surface.resolved();
    let surface = HeadlessSurface {
        width,
        height,
        scale: scale.get(),
    };
    let mut case = case(&root, "inspect", scenario, surface, out)?;
    case.census = true;
    case.detail = wants.detail;
    case.events = wants.events;
    let sources = checked.sources();
    let build = scenario::build(ctx, Some(sources.clone()), out)?;
    let mut written = None;
    let code = scenario::play(
        ctx,
        &build,
        vec![case],
        out,
        |_| false,
        |out, case, played| {
            let code = match &played.ended {
                Ended::Passed | Ended::Stopped => SUCCESS,
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
            };
            if code == SUCCESS {
                let detail = wants.detail.then(|| std::fs::read(played.detail()));
                let events = wants.events.then(|| std::fs::read(played.events()));
                written = Some((std::fs::read(played.census()), detail, events));
            }
            played.discard();
            code
        },
    );
    if code != SUCCESS {
        return Err(code);
    }
    let Some((census, detail, events)) = written else {
        return Err(code);
    };
    let census = census
        .map_err(|error| invalid(out, &format!("the run wrote no census: {error}")))
        .and_then(|bytes| {
            Census::from_bytes(&bytes).map_err(|error| {
                invalid(out, &format!("the run's census does not decode: {error}"))
            })
        })?;
    let detail = match detail {
        None => None,
        Some(Err(error)) => {
            return Err(invalid(
                out,
                &format!("the run wrote no detail of its window: {error}"),
            ));
        }
        Some(Ok(bytes)) => Some(WindowDetail::from_bytes(&bytes).map_err(|error| {
            invalid(
                out,
                &format!("the run's window detail does not decode: {error}"),
            )
        })?),
    };
    let events = match events {
        None => None,
        Some(Err(error)) => {
            return Err(invalid(
                out,
                &format!("the run wrote no journal of its window: {error}"),
            ));
        }
        Some(Ok(bytes)) => Some(EventLog::from_bytes(&bytes).map_err(|error| {
            invalid(out, &format!("the run's journal does not decode: {error}"))
        })?),
    };
    Ok(App {
        census,
        sources,
        inspector: HEADLESS,
        asked: Asked::Headless { detail, events },
    })
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
        line.push_str(viso_schema::view::source_name(semantics.role));
        if let Some(label) = &semantics.label {
            line.push_str(&format!(" {label:?}"));
        }
    }
    line
}

fn write_match(w: &mut JsonWriter, node: &Inspected<'_>) {
    w.begin_object();
    write_fields(w, node);
    w.end_object();
}

/// What a match is, as members of the object it is written in.
fn write_fields(w: &mut JsonWriter, node: &Inspected<'_>) {
    let census = node.node;
    let optional = |w: &mut JsonWriter, value: Option<&str>| match value {
        Some(value) => w.string(value),
        None => w.null(),
    };
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
            w.string(viso_schema::view::source_name(semantics.role));
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
}
