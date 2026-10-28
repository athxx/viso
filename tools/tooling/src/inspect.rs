//! The inspector client: what the app a dev session runs shows and holds,
//! asked through the session's control endpoint, and a window's journal
//! followed as it grows.
//!
//! The app answers at its next frame, with the session's source table of the
//! views it mounts beside its answer and what its inspector tells.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use viso_dev_protocol::inspect::{
    Census, EventKind, EventLog, FrameDetail, InspectAsk, Inspected, InspectorCaps,
    MAX_ASKED_NODES, NodeDetail, NodeDetails, SourceTable, resolve,
};
use viso_project::{Layout, LockGuard, LockKind, Target};

use crate::dev::control;
use crate::dev::{ControlAsk, ControlReply};

/// How long the session has to answer: the app answers at its next frame.
pub const WAIT: Duration = Duration::from_secs(10);

/// How often a follower asks for what is new.
pub const POLL: Duration = Duration::from_millis(100);

/// The least time between two censuses a follower takes to name new nodes.
const RECENSUS: Duration = Duration::from_millis(500);

/// Why asking a session failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskError {
    /// The session is not there, or no longer: the reason.
    Gone(String),
    /// The session refused: its code, message and notes.
    Refused {
        code: String,
        message: String,
        notes: Vec<String>,
    },
    /// What came back is not what was asked: why.
    Invalid(String),
}

/// What the session answered: the app's answer, the session's source table,
/// both encoded, and what the app's inspector tells.
pub struct Answer {
    pub answer: Vec<u8>,
    pub sources: Vec<u8>,
    pub inspector: InspectorCaps,
}

/// Asks the session at `endpoint` what `ask` asks.
pub fn ask(endpoint: &str, ask: InspectAsk) -> Result<Answer, AskError> {
    match control::call(endpoint, ControlAsk::Inspect(ask), Some(WAIT)) {
        Ok(ControlReply::Inspected {
            answer,
            sources,
            inspector,
        }) => Ok(Answer {
            answer,
            sources,
            inspector,
        }),
        Ok(ControlReply::Refused {
            code,
            message,
            notes,
        }) => Err(AskError::Refused {
            code,
            message,
            notes,
        }),
        Ok(_) => Err(AskError::Invalid(
            "`viso run` answered another question".to_owned(),
        )),
        Err(message) => Err(AskError::Gone(message)),
    }
}

/// The app a session runs, its census taken.
#[derive(Clone)]
pub struct Censused {
    pub census: Census,
    /// The session's source table of the views it mounts.
    pub sources: SourceTable,
    /// What its inspector tells.
    pub inspector: InspectorCaps,
}

/// The census of the app the session at `endpoint` runs.
pub fn census(endpoint: &str) -> Result<Censused, AskError> {
    let answer = ask(endpoint, InspectAsk::Census)?;
    let census = Census::from_bytes(&answer.answer)
        .map_err(|error| AskError::Invalid(format!("the app's census does not decode: {error}")))?;
    let sources = SourceTable::from_bytes(&answer.sources).map_err(|error| {
        AskError::Invalid(format!(
            "the session's source table does not decode: {error}"
        ))
    })?;
    Ok(Censused {
        census,
        sources,
        inspector: answer.inspector,
    })
}

/// The detail the app holds of `nodes` of window `window`, each an index
/// and a generation, asked as many at a time as an ask takes; a node gone
/// since the census has none.
pub fn nodes(
    endpoint: &str,
    window: u32,
    nodes: &[(u32, u32)],
) -> Result<Vec<NodeDetail>, AskError> {
    let mut found = Vec::new();
    for chunk in nodes.chunks(MAX_ASKED_NODES) {
        let ask_nodes = InspectAsk::Nodes {
            window,
            nodes: chunk.to_vec(),
        };
        let answer = ask(endpoint, ask_nodes)?;
        let details = NodeDetails::from_bytes(&answer.answer).map_err(|error| {
            AskError::Invalid(format!("the app's node detail does not decode: {error}"))
        })?;
        found.extend(details.nodes);
    }
    Ok(found)
}

/// What window `window`'s last frame drew and holds.
pub fn frame(endpoint: &str, window: u32) -> Result<FrameDetail, AskError> {
    let answer = ask(endpoint, InspectAsk::Frame { window })?;
    FrameDetail::from_bytes(&answer.answer).map_err(|error| {
        AskError::Invalid(format!("the app's frame detail does not decode: {error}"))
    })
}

/// What window `window`'s journal holds after event `after`.
pub fn events(endpoint: &str, window: u32, after: u64) -> Result<EventLog, AskError> {
    let answer = ask(endpoint, InspectAsk::Events { window, after })?;
    EventLog::from_bytes(&answer.answer)
        .map_err(|error| AskError::Invalid(format!("the app's journal does not decode: {error}")))
}

/// Why a project has no dev session to ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoSession {
    /// None runs for the target.
    None,
    /// Several run, on these devices.
    Several(Vec<String>),
    /// It runs but takes no requests: it is starting, or failed to open its
    /// control endpoint. The lock's holder, as it reads.
    Closed(String),
}

/// The control endpoint of the dev session the project at `layout` runs
/// for `target`: on `device`, else the only one of the target.
pub fn endpoint(
    layout: &Layout,
    target: Target,
    device: Option<&str>,
) -> Result<String, NoSession> {
    let holder = match device {
        Some(device) => LockGuard::holder(
            layout,
            LockKind::DevSession {
                target,
                device: Some(device),
            },
        ),
        None => {
            let bare = format!("dev-{}.lock", target.as_str());
            let on = format!("dev-{}-", target.as_str());
            let mut sessions: Vec<_> = LockGuard::held(layout)
                .into_iter()
                .filter(|held| {
                    held.path.file_name().is_some_and(|name| {
                        let name = name.to_string_lossy();
                        name == bare || name.starts_with(&on)
                    })
                })
                .collect();
            if sessions.len() > 1 {
                let devices = sessions
                    .iter()
                    .filter_map(|held| {
                        let name = held.path.file_stem()?.to_string_lossy().into_owned();
                        Some(name.strip_prefix(&on)?.to_owned())
                    })
                    .collect();
                return Err(NoSession::Several(devices));
            }
            sessions.pop().map(|held| held.holder)
        }
    };
    match holder {
        Some(holder) => holder
            .endpoint
            .clone()
            .ok_or_else(|| NoSession::Closed(holder.to_string())),
        None => Err(NoSession::None),
    }
}

/// What each node of a window is called in a report: its type, name and
/// styles where a view declares it, else what it draws.
#[derive(Clone, Debug, Default)]
pub struct Labels {
    by_id: HashMap<(u32, u32), String>,
}

impl Labels {
    /// The nodes of window `window` of `census`, named after `sources`.
    pub fn new(census: &Census, sources: &SourceTable, window: u32) -> Labels {
        let inspection = resolve(census, sources);
        let by_id = inspection
            .nodes
            .iter()
            .filter(|node| node.window == window)
            .map(|node| ((node.node.index, node.node.generation), short(node)))
            .collect();
        Labels { by_id }
    }

    /// What node `id` is called, if it is one it names.
    pub fn get(&self, id: (u32, u32)) -> Option<&str> {
        self.by_id.get(&id).map(String::as_str)
    }

    /// Node `id` as a report shows it: `Button#save 12:1`, or the ID alone.
    pub fn text(&self, (index, generation): (u32, u32)) -> String {
        match self.get((index, generation)) {
            Some(label) => format!("{label} {index}:{generation}"),
            None => format!("{index}:{generation}"),
        }
    }

    /// Whether every node `log` names is one it names.
    pub fn names_all(&self, log: &EventLog) -> bool {
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
}

/// What a node is called: `Button#save.Primary`, or what it draws.
pub fn short(node: &Inspected<'_>) -> String {
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

/// A window of the app a session runs, its journal followed: each poll
/// asks for what is new, and takes the census again, at most twice a
/// second, when an event names a node it does not name.
pub struct Follower {
    app: Censused,
    window: u32,
    labels: Labels,
    /// The number of the last event taken.
    after: u64,
    census_at: Instant,
}

impl Follower {
    /// Follows window `window` of `app`.
    pub fn new(app: Censused, window: u32) -> Self {
        let labels = Labels::new(&app.census, &app.sources, window);
        Follower {
            app,
            window,
            labels,
            after: 0,
            census_at: Instant::now(),
        }
    }

    /// The app, as its last census took it.
    pub fn app(&self) -> &Censused {
        &self.app
    }

    /// What its nodes are called.
    pub fn labels(&self) -> &Labels {
        &self.labels
    }

    /// What is new in the window of the app the session at `endpoint`
    /// runs; the next poll asks after it.
    pub fn poll(&mut self, endpoint: &str) -> Result<EventLog, AskError> {
        let log = events(endpoint, self.window, self.after)?;
        if !self.labels.names_all(&log) && self.census_at.elapsed() >= RECENSUS {
            self.census_at = Instant::now();
            if let Ok(app) = census(endpoint) {
                self.labels = Labels::new(&app.census, &app.sources, self.window);
                self.app = app;
            }
        }
        self.after = log.next.saturating_sub(1).max(self.after);
        Ok(log)
    }
}

/// Events a stream brought, with what names their nodes.
pub struct Streamed {
    pub labels: Labels,
    pub sources: SourceTable,
    pub log: EventLog,
}

/// Follows, on a thread of its own, the first window of each app the
/// session at `endpoint` runs in turn, handing `sink` each poll that
/// brings events, until `sink` returns `false` or an app's inspector does
/// not tell its events.
pub fn stream(endpoint: String, sink: impl Fn(Streamed) -> bool + Send + 'static) {
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
