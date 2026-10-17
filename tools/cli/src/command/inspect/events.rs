//! What happens in a window, followed as it happens: `viso inspect events`,
//! a bare `viso inspect` attached to a session, and `viso run --inspect`.
//!
//! The app keeps a journal of the window once an inspector asks for it:
//! each input event and the nodes it was routed through, focus moving, and
//! each node marked dirty with why. A follower asks for what is new every
//! tenth of a second and names the nodes after the app's census, taken again
//! when an event names a node it does not hold; a headless run keeps its
//! journal from its start and writes it at its end.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use viso_ende::JsonWriter;
use viso_project::{LockGuard, Target};
use viso_view::inspect::{
    Cause, Census, EventKind, EventLog, InspectAsk, InspectEvent, Inspected, InspectorCaps,
    SourceTable, StateSource, dirty_names, resolve,
};

use super::{App, Asked, connect, endpoint, invalid, write_inspector};
use crate::args::{AttachArgs, EventsArgs};
use crate::context::Context;
use crate::dev::control;
use crate::dev::{ControlAsk, ControlReply};
use crate::error::{CLI_USAGE, ENV_NO_DEV_SESSION, INSPECT_UNSUPPORTED, RUN_APP_FAILED};
use crate::interrupt;
use crate::output::Output;
use crate::process::Launch;

/// How often a follower asks for what is new.
const POLL: Duration = Duration::from_millis(100);

/// The least time between two censuses a follower takes to name new nodes.
const RECENSUS: Duration = Duration::from_millis(500);

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
    let app = census(&endpoint).map_err(|failure| failure.report(out))?;
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

/// Why asking a session failed.
enum Failure {
    /// The session is not there, or no longer: the reason.
    Gone(String),
    /// The session refused: its code, message and notes.
    Refused(String, String, Vec<String>),
    /// What came back is not what was asked.
    Invalid(String),
}

impl Failure {
    fn report(self, out: &mut Output) -> u8 {
        match self {
            Failure::Gone(message) => out.failure(ENV_NO_DEV_SESSION, &message, &[]),
            Failure::Refused(code, message, notes) => super::refused(
                ControlReply::Refused {
                    code,
                    message,
                    notes,
                },
                out,
            ),
            Failure::Invalid(message) => invalid(out, &message),
        }
    }
}

/// Asks the session at `endpoint` what `ask` asks: the app's answer, the
/// session's source table and what the app's inspector tells.
fn ask(endpoint: &str, ask: InspectAsk) -> Result<(Vec<u8>, Vec<u8>, InspectorCaps), Failure> {
    match control::call(endpoint, ControlAsk::Inspect(ask), Some(super::WAIT)) {
        Ok(ControlReply::Inspected {
            answer,
            sources,
            inspector,
        }) => Ok((answer, sources, inspector)),
        Ok(ControlReply::Refused {
            code,
            message,
            notes,
        }) => Err(Failure::Refused(code, message, notes)),
        Ok(_) => Err(Failure::Invalid(
            "`viso run` answered another question".to_owned(),
        )),
        Err(message) => Err(Failure::Gone(message)),
    }
}

/// The census of the app at `endpoint`.
fn census(endpoint: &str) -> Result<App, Failure> {
    let (answer, sources, inspector) = ask(endpoint, InspectAsk::Census)?;
    let census = Census::from_bytes(&answer)
        .map_err(|error| Failure::Invalid(format!("the app's census does not decode: {error}")))?;
    let sources = SourceTable::from_bytes(&sources).map_err(|error| {
        Failure::Invalid(format!(
            "the session's source table does not decode: {error}"
        ))
    })?;
    Ok(App {
        census,
        sources,
        inspector,
        asked: Asked::Session(endpoint.to_owned()),
    })
}

/// A window followed through a session.
pub(crate) struct Follower {
    app: App,
    window: u32,
    labels: Labels,
    /// The number of the last event reported.
    after: u64,
    census_at: Instant,
    reported: u64,
}

impl Follower {
    fn new(app: App, window: u32) -> Self {
        let labels = Labels::new(&app.census, &app.sources, window);
        Follower {
            app,
            window,
            labels,
            after: 0,
            census_at: Instant::now(),
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
            match self.poll(endpoint) {
                Ok(log) => self.report(&log, out),
                Err(Failure::Gone(why)) => break Some(why),
                Err(failure) => return Err(failure.report(out)),
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

    /// What is new in the window, its nodes named.
    fn poll(&mut self, endpoint: &str) -> Result<EventLog, Failure> {
        let ask_events = InspectAsk::Events {
            window: self.window,
            after: self.after,
        };
        let (answer, _, _) = ask(endpoint, ask_events)?;
        let log = EventLog::from_bytes(&answer).map_err(|error| {
            Failure::Invalid(format!("the app's journal does not decode: {error}"))
        })?;
        if !self.labels.names_all(&log) && self.census_at.elapsed() >= RECENSUS {
            self.census_at = Instant::now();
            if let Ok(app) = census(endpoint) {
                self.labels = Labels::new(&app.census, &app.sources, self.window);
                self.app = app;
            }
        }
        Ok(log)
    }

    /// Reports `log`, and asks after it next.
    pub(crate) fn report(&mut self, log: &EventLog, out: &mut Output) {
        report(out, &self.labels, &self.app.sources, log);
        self.reported += log.events.len() as u64;
        self.after = log.next.saturating_sub(1).max(self.after);
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

/// What each node of a window is called in a report: its type, name and
/// styles where a view declares it, else what it draws.
#[derive(Clone)]
pub(crate) struct Labels {
    by_id: HashMap<(u32, u32), String>,
}

impl Labels {
    pub(crate) fn new(census: &Census, sources: &SourceTable, window: u32) -> Labels {
        let inspection = resolve(census, sources);
        let by_id = inspection
            .nodes
            .iter()
            .filter(|node| node.window == window)
            .map(|node| ((node.node.index, node.node.generation), short(node)))
            .collect();
        Labels { by_id }
    }

    /// Whether every node `log` names is one it names.
    fn names_all(&self, log: &EventLog) -> bool {
        log.events.iter().all(|event| match &event.kind {
            EventKind::Input { route, .. } => {
                route.last().is_none_or(|id| self.by_id.contains_key(id))
            }
            EventKind::Focus { from, to } => [from, to]
                .into_iter()
                .flatten()
                .all(|id| self.by_id.contains_key(id)),
            EventKind::Dirty { node, .. } => self.by_id.contains_key(node),
        })
    }

    /// Node `id` as a report shows it: `Button#save 12:1`, or the ID alone.
    fn text(&self, (index, generation): (u32, u32)) -> String {
        match self.by_id.get(&(index, generation)) {
            Some(label) => format!("{label} {index}:{generation}"),
            None => format!("{index}:{generation}"),
        }
    }

    fn write(&self, w: &mut JsonWriter, id: (u32, u32)) {
        w.begin_object();
        w.name("node_id");
        w.string(&format!("{}:{}", id.0, id.1));
        w.name("label");
        match self.by_id.get(&id) {
            Some(label) => w.string(label),
            None => w.null(),
        }
        w.end_object();
    }

    fn write_option(&self, w: &mut JsonWriter, id: Option<(u32, u32)>) {
        match id {
            Some(id) => self.write(w, id),
            None => w.null(),
        }
    }
}

/// What a node is called: `Button#save.Primary`, or what it draws.
fn short(node: &Inspected<'_>) -> String {
    match node.source {
        Some((_, source)) => {
            let mut what = source.type_name.clone();
            if let Some(name) = &source.name {
                what.push('#');
                what.push_str(name);
            }
            for style in &source.styles {
                what.push('.');
                what.push_str(style);
            }
            what
        }
        None => node.node.kind.label().to_owned(),
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
            labels.write_option(w, route.last().copied());
            w.name("route");
            w.begin_array();
            for &id in route {
                labels.write(w, id);
            }
            w.end_array();
        }
        EventKind::Focus { from, to } => {
            w.name("from");
            labels.write_option(w, *from);
            w.name("to");
            labels.write_option(w, *to);
        }
        EventKind::Dirty { node, class, cause } => {
            w.name("node");
            labels.write(w, *node);
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
            && let Ok(app) = census(&endpoint)
            && !app.census.windows.is_empty()
        {
            break (app, endpoint);
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
        #[cfg(unix)]
        // SAFETY: signalling a child this process spawned and still owns.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGINT);
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
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

/// `viso run --inspect`: the session's own first runtime followed, its
/// events reported among the session's.
pub(crate) fn stream(endpoint: String, sink: impl Fn(Streamed) -> bool + Send + 'static) {
    std::thread::spawn(move || {
        let mut follower: Option<Follower> = None;
        loop {
            let next = match &mut follower {
                None => match census(&endpoint) {
                    Ok(app) if app.inspector.contains(InspectorCaps::EVENTS) => {
                        follower = Some(Follower::new(app, 0));
                        None
                    }
                    Ok(_) => return,
                    Err(_) => None,
                },
                Some(following) => match following.poll(&endpoint) {
                    Ok(log) => Some(Streamed {
                        labels: following.labels.clone(),
                        sources: following.app.sources.clone(),
                        after: log.next.saturating_sub(1),
                        log,
                    }),
                    // The app left: the next one that connects is followed.
                    Err(_) => {
                        follower = None;
                        None
                    }
                },
            };
            if let Some(streamed) = next {
                if let Some(following) = &mut follower {
                    following.after = streamed.after.max(following.after);
                }
                let news = !streamed.log.events.is_empty() || streamed.log.dropped > 0;
                if news && !sink(streamed) {
                    return;
                }
            }
            std::thread::sleep(if follower.is_some() {
                POLL
            } else {
                Duration::from_millis(500)
            });
        }
    });
}

/// Events a stream brought, with what names their nodes.
pub(crate) struct Streamed {
    labels: Labels,
    sources: SourceTable,
    after: u64,
    log: EventLog,
}

impl Streamed {
    /// Reports them on `out`.
    pub(crate) fn report(&self, out: &mut Output) {
        report(out, &self.labels, &self.sources, &self.log);
    }
}
