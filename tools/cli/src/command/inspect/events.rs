//! What happens in a window, followed as it happens: `viso inspect events`,
//! a bare `viso inspect` attached to a session, and `viso run --inspect`.
//!
//! The app keeps a journal of the window once an inspector asks for it:
//! each input event and the nodes it was routed through, focus moving, and
//! each node marked dirty with why. A follower asks for what is new every
//! tenth of a second and names the nodes after the app's census, taken again
//! when an event names a node it does not hold; a headless run keeps its
//! journal from its start and writes it at its end.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use viso_dev_protocol::inspect::{
    Cause, EventKind, EventLog, InspectEvent, InspectorCaps, SourceTable, StateSource, dirty_names,
};
use viso_ende::JsonWriter;
use viso_project::{LockGuard, Target};

use super::{App, Asked, connect, endpoint, failed, invalid, write_inspector};
use crate::args::{AttachArgs, EventsArgs};
use crate::context::Context;
use crate::error::{CLI_USAGE, ENV_NO_DEV_SESSION, INSPECT_UNSUPPORTED, RUN_APP_FAILED};
use crate::interrupt;
use crate::output::Output;
use crate::process::Launch;
use viso_tooling::inspect::{self, Labels, POLL, Streamed};

/// How long `--run` waits for the session it starts to connect its app: a
/// first build of a mobile target takes minutes.
const LAUNCH: Duration = Duration::from_secs(900);

/// How long the session `--run` started has to stop once asked.
const STOP: Duration = Duration::from_secs(10);

pub(super) fn events(ctx: &Context<'_>, args: &EventsArgs, out: &mut Output) -> Result<(), u8> {
    let headless = args
        .app
        .target
        .is_some_and(|target| target.target() == Target::Headless);
    if headless && args.seconds.is_some() {
        return Err(out.failure(
            CLI_USAGE,
            "a headless run reports every event of its scenario: it takes no `--seconds`",
            &[],
        ));
    }
    let app = connect(ctx, &args.app, super::Wants::EVENTS, out)?;
    needs_events(&app, out)?;
    match &app.asked {
        Asked::Headless { events, .. } => {
            let Some(log) = events else {
                return Err(invalid(out, "the run wrote no journal of its window"));
            };
            if args.window != 0 {
                return Err(out.failure(
                    CLI_USAGE,
                    &format!("a headless run has one window, not {}", args.window),
                    &[],
                ));
            }
            let labels = Labels::new(&app.census, &app.sources, 0);
            report(out, &labels, &app.sources, log);
            finish(out, log.events.len() as u64, None);
            Ok(())
        }
        Asked::Session(endpoint) => {
            let endpoint = endpoint.clone();
            let mut follower = Follower::new(app, args.window);
            follower.follow(&endpoint, args.seconds.map(Duration::from_secs_f64), out)
        }
    }
}

/// A bare `viso inspect`: the session attached, its app described and its
/// first window followed until interrupted.
pub(super) fn attach(ctx: &Context<'_>, args: &AttachArgs, out: &mut Output) -> Result<(), u8> {
    let target = args.target.map_or(Target::Host, |target| target.target());
    if args.run {
        return run_and_attach(ctx, target, args.device.as_deref(), out);
    }
    let endpoint = match &args.attach {
        Some(session) => session_endpoint(ctx, session, out)?,
        None => endpoint(ctx, target, args.device.as_deref(), out)?,
    };
    let app = inspect::census(&endpoint).map_err(|error| failed(error, out))?;
    let app = App::session(app, endpoint.clone());
    attached(app, &endpoint, out)
}

/// Describes the app at `endpoint` and follows its first window.
fn attached(app: App, endpoint: &str, out: &mut Output) -> Result<(), u8> {
    needs_events(&app, out)?;
    let windows = app.census.windows.len();
    let nodes: usize = app.census.windows.iter().map(|w| w.nodes.len()).sum();
    let inspector = app.inspector;
    out.inspect(
        |w| {
            w.begin_object();
            w.name("kind");
            w.string("attached");
            w.name("windows");
            w.uint(windows as u64);
            w.name("nodes");
            w.uint(nodes as u64);
            write_inspector(w, inspector);
            w.end_object();
        },
        || {
            format!(
                "attached: {windows} window{}, {nodes} nodes; following window 0 \
                 (interrupt to stop)",
                if windows == 1 { "" } else { "s" }
            )
        },
    );
    let mut follower = Follower::new(app, 0);
    follower.follow(endpoint, None, out)
}

fn needs_events(app: &App, out: &mut Output) -> Result<(), u8> {
    if app.inspector.contains(InspectorCaps::EVENTS) {
        return Ok(());
    }
    Err(out.failure(
        INSPECT_UNSUPPORTED,
        "the app's inspector does not tell what happens in it",
        &[format!(
            "it tells: {}",
            app.inspector.names().collect::<Vec<_>>().join(", ")
        )],
    ))
}

/// The endpoint of the dev session of this project whose ID starts with
/// `session`.
fn session_endpoint(ctx: &Context<'_>, session: &str, out: &mut Output) -> Result<String, u8> {
    let project = ctx.project(out)?;
    let layout = ctx.layout(&project.root);
    let wanted = session.to_ascii_lowercase();
    let matching: Vec<_> = LockGuard::held(&layout)
        .into_iter()
        .filter(|held| {
            held.holder
                .session
                .as_deref()
                .is_some_and(|id| id.starts_with(&wanted))
        })
        .collect();
    match &matching[..] {
        [held] => held.holder.endpoint.clone().ok_or_else(|| {
            out.failure(
                ENV_NO_DEV_SESSION,
                &format!("the session `{session}` takes no requests"),
                &[],
            )
        }),
        [] => Err(out.failure(
            ENV_NO_DEV_SESSION,
            &format!("no `viso run` session of this project has the ID `{session}`"),
            &["`viso run` reports its session ID in its `dev` events".to_owned()],
        )),
        _ => Err(out.failure(
            CLI_USAGE,
            &format!("several sessions have IDs starting with `{session}`: give more of it"),
            &[],
        )),
    }
}

/// A window followed through a session, and how many events it reported.
pub(crate) struct Follower {
    following: inspect::Follower,
    reported: u64,
}

impl Follower {
    fn new(app: App, window: u32) -> Self {
        Follower {
            following: inspect::Follower::new(app.censused(), window),
            reported: 0,
        }
    }

    /// Follows the window through the session at `endpoint` for `bound`,
    /// or until interrupted or the session ends.
    fn follow(
        &mut self,
        endpoint: &str,
        bound: Option<Duration>,
        out: &mut Output,
    ) -> Result<(), u8> {
        interrupt::catch();
        let started = Instant::now();
        let ended = loop {
            match self.following.poll(endpoint) {
                Ok(log) => {
                    let following = &self.following;
                    report(out, following.labels(), &following.app().sources, &log);
                    self.reported += log.events.len() as u64;
                }
                Err(inspect::AskError::Gone(why)) => break Some(why),
                Err(error) => return Err(failed(error, out)),
            }
            let due = Instant::now() + POLL;
            while Instant::now() < due {
                if interrupt::count() > 0 || bound.is_some_and(|b| started.elapsed() >= b) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if interrupt::count() > 0 || bound.is_some_and(|b| started.elapsed() >= b) {
                break None;
            }
        };
        finish(out, self.reported, ended.as_deref());
        Ok(())
    }
}

/// The last line of a follow: how many events, and why it ended when the
/// session did.
fn finish(out: &mut Output, events: u64, ended: Option<&str>) {
    out.result(
        |w| {
            w.begin_object();
            w.name("events");
            w.uint(events);
            w.name("ended");
            match ended {
                Some(why) => w.string(why),
                None => w.null(),
            }
            w.end_object();
        },
        || {
            let mut text = match events {
                1 => "1 event\n".to_owned(),
                n => format!("{n} events\n"),
            };
            if let Some(why) = ended {
                text.push_str(&format!("the session ended: {why}\n"));
            }
            text
        },
    );
}

/// Node `id` and what `labels` call it.
fn write_label(w: &mut JsonWriter, labels: &Labels, id: (u32, u32)) {
    w.begin_object();
    w.name("node_id");
    w.string(&format!("{}:{}", id.0, id.1));
    w.name("label");
    match labels.get(id) {
        Some(label) => w.string(label),
        None => w.null(),
    }
    w.end_object();
}

fn write_label_option(w: &mut JsonWriter, labels: &Labels, id: Option<(u32, u32)>) {
    match id {
        Some(id) => write_label(w, labels, id),
        None => w.null(),
    }
}

/// The source the cell of durable identity `key` is, in any view.
fn state_source(sources: &SourceTable, key: (u64, u64)) -> Option<(&str, &StateSource)> {
    sources
        .files
        .iter()
        .find_map(|file| Some((file.path.as_str(), file.state(key)?)))
}

/// Reports each event of `log`, after what it dropped.
pub(crate) fn report(out: &mut Output, labels: &Labels, sources: &SourceTable, log: &EventLog) {
    if log.dropped > 0 {
        let (window, dropped) = (log.window, log.dropped);
        out.inspect(
            |w| {
                w.begin_object();
                w.name("kind");
                w.string("dropped");
                w.name("window");
                w.uint(u64::from(window));
                w.name("count");
                w.uint(dropped);
                w.end_object();
            },
            || format!("… {dropped} events dropped: the journal holds the newest only"),
        );
    }
    for event in &log.events {
        out.inspect(
            |w| write_event(w, labels, sources, log.window, event),
            || describe_event(labels, sources, event),
        );
    }
}

fn describe_event(labels: &Labels, sources: &SourceTable, event: &InspectEvent) -> String {
    let head = format!("#{:<5} f{:<5}", event.seq, event.frame);
    match &event.kind {
        EventKind::Input { input, at, route } => {
            let mut line = format!("{head} input  {input}");
            if let Some([x, y]) = at {
                line.push_str(&format!(" ({x}, {y})"));
            }
            match route.last() {
                Some(&target) => line.push_str(&format!(" → {}", labels.text(target))),
                None => line.push_str(" → no node"),
            }
            line
        }
        EventKind::Focus { from, to } => {
            let name = |id: &Option<(u32, u32)>| {
                id.map_or_else(|| "none".to_owned(), |id| labels.text(id))
            };
            format!("{head} focus  {} → {}", name(from), name(to))
        }
        EventKind::Dirty { node, class, cause } => {
            let classes: Vec<&str> = dirty_names(*class).collect();
            let why = match cause {
                Cause::State { cell, key, value } => {
                    match key.and_then(|key| state_source(sources, key)) {
                        Some((path, state)) => format!(
                            "state {} = {value}  {path}:{}:{}",
                            state.name, state.start.0, state.start.1
                        ),
                        None => format!("state {}:{} = {value}", cell.0, cell.1),
                    }
                }
                cause => cause.name().to_owned(),
            };
            format!(
                "{head} dirty  {}  {}  ← {why}",
                labels.text(*node),
                classes.join(", ")
            )
        }
    }
}

fn write_event(
    w: &mut JsonWriter,
    labels: &Labels,
    sources: &SourceTable,
    window: u32,
    event: &InspectEvent,
) {
    w.begin_object();
    w.name("kind");
    w.string(match event.kind {
        EventKind::Input { .. } => "input",
        EventKind::Focus { .. } => "focus",
        EventKind::Dirty { .. } => "dirty",
    });
    w.name("seq");
    w.uint(event.seq);
    w.name("frame");
    w.uint(event.frame);
    w.name("window");
    w.uint(u64::from(window));
    match &event.kind {
        EventKind::Input { input, at, route } => {
            w.name("input");
            w.string(input);
            w.name("at");
            match at {
                Some([x, y]) => {
                    w.begin_array();
                    w.number(f64::from(*x));
                    w.number(f64::from(*y));
                    w.end_array();
                }
                None => w.null(),
            }
            w.name("target");
            write_label_option(w, labels, route.last().copied());
            w.name("route");
            w.begin_array();
            for &id in route {
                write_label(w, labels, id);
            }
            w.end_array();
        }
        EventKind::Focus { from, to } => {
            w.name("from");
            write_label_option(w, labels, *from);
            w.name("to");
            write_label_option(w, labels, *to);
        }
        EventKind::Dirty { node, class, cause } => {
            w.name("node");
            write_label(w, labels, *node);
            w.name("classes");
            w.begin_array();
            for name in dirty_names(*class) {
                w.string(name);
            }
            w.end_array();
            w.name("cause");
            w.begin_object();
            w.name("kind");
            w.string(cause.name());
            if let Cause::State { cell, key, value } = cause {
                let state = key.and_then(|key| state_source(sources, key));
                w.name("cell");
                w.string(&format!("{}:{}", cell.0, cell.1));
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
                w.name("value");
                w.string(value);
                w.name("declared");
                match state {
                    Some((path, state)) => {
                        w.begin_object();
                        w.name("file");
                        w.string(path);
                        w.name("line");
                        w.uint(u64::from(state.start.0));
                        w.name("column_utf16");
                        w.uint(u64::from(state.start.1));
                        w.end_object();
                    }
                    None => w.null(),
                }
            }
            w.end_object();
        }
    }
    w.end_object();
}

/// `viso inspect --run`: starts `viso run` for `target`, attaches once its
/// app connects, and stops the session when the inspector stops.
fn run_and_attach(
    ctx: &Context<'_>,
    target: Target,
    device: Option<&str>,
    out: &mut Output,
) -> Result<(), u8> {
    let project = ctx.project(out)?;
    let layout = ctx.layout(&project.root);
    let dir = layout.dev();
    let log = dir.join("inspect-run.log");
    let file = fs::create_dir_all(&dir)
        .and_then(|()| fs::File::create(&log))
        .map_err(|error| {
            out.failure(
                RUN_APP_FAILED,
                &format!("cannot write `{}`: {error}", log.display()),
                &[],
            )
        })?;
    let exe = std::env::current_exe().map_err(|error| {
        out.failure(
            RUN_APP_FAILED,
            &format!("cannot find this `viso`: {error}"),
            &[],
        )
    })?;
    let mut command = Command::new(exe);
    command.arg("--project").arg(&project.root);
    if ctx.global.offline {
        command.arg("--offline");
    }
    if ctx.global.locked {
        command.arg("--locked");
    }
    command.arg("run");
    if target != Target::Host {
        command.arg(target.as_str());
    }
    if let Some(device) = device {
        command.arg("--device").arg(device);
    }
    let err = file.try_clone().map_err(|error| {
        out.failure(
            RUN_APP_FAILED,
            &format!("cannot write `{}`: {error}", log.display()),
            &[],
        )
    })?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err));
    let child = command.launch().map_err(|error| {
        out.failure(
            RUN_APP_FAILED,
            &format!("cannot start `viso run`: {error}"),
            &[],
        )
    })?;
    let mut session = Session { child, log };
    out.step(
        "inspect",
        &format!(
            "starting `viso run`; its output goes to {}",
            session.log.display()
        ),
    );
    interrupt::catch();
    let started = Instant::now();
    let (app, endpoint) = loop {
        if let Ok(Some(status)) = session.child.try_wait() {
            return Err(out.failure(
                RUN_APP_FAILED,
                &format!("`viso run` ended before its app connected ({status})"),
                &session.tail(),
            ));
        }
        if interrupt::count() > 0 {
            session.stop();
            return Ok(());
        }
        if started.elapsed() > LAUNCH {
            session.stop();
            return Err(out.failure(
                ENV_NO_DEV_SESSION,
                "the app `viso run` started did not connect in time",
                &session.tail(),
            ));
        }
        let held = LockGuard::holder(
            &layout,
            viso_project::LockKind::DevSession { target, device },
        );
        if let Some(endpoint) = held.and_then(|holder| holder.endpoint)
            && let Ok(app) = inspect::census(&endpoint)
            && !app.census.windows.is_empty()
        {
            break (App::session(app, endpoint.clone()), endpoint);
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let followed = attached(app, &endpoint, out);
    session.stop();
    followed
}

/// The `viso run` an inspector started.
struct Session {
    child: Child,
    log: PathBuf,
}

impl Session {
    /// Asks the session to stop as an interrupt does, then forces it.
    fn stop(&mut self) {
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        crate::process::interrupt(&mut self.child);
        let asked = Instant::now();
        while asked.elapsed() < STOP {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// The last lines the session wrote, for a failure's notes.
    fn tail(&self) -> Vec<String> {
        let text = fs::read_to_string(&self.log).unwrap_or_default();
        let mut lines: Vec<String> = text.lines().rev().take(12).map(str::to_owned).collect();
        lines.reverse();
        lines.push(format!("all of it is in {}", self.log.display()));
        lines
    }
}

/// Reports what a stream of `viso run --inspect` brought on `out`.
pub(crate) fn report_streamed(out: &mut Output, streamed: &Streamed) {
    report(out, &streamed.labels, &streamed.sources, &streamed.log);
}
