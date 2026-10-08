//! The dev session service of `viso run` (`Viso_Hot_Reload.md` §3, §4): for the
//! lifetime of the command it owns the session's identity and lock, the
//! project watcher, the source graph and its compiler, and the connection to
//! the running app, and turns each settled batch of edits into one candidate
//! revision.
//!
//! The watcher starts before the build, so the session knows every file of
//! the scope as it was when the build read it, and an edit saved while the
//! app builds is just a file whose text the runtime does not run yet. Changes
//! are coalesced: a batch is taken once no change has arrived for
//! [`COALESCE`] (or [`COALESCE_LIMIT`] after its first), so several files
//! written together (an AI applying a multi-file edit, a catalog and the view
//! using its new key) form one candidate. Each candidate view compiles on the
//! host exactly as the build compiled it; a view that does not compile is
//! reported with its diagnostics, its failure is sent to the app to show over
//! the last-good UI, and it is not sent. The views that compile are sent as
//! one patch, and the runtime's ACK or NACK closes the candidate. One patch
//! is in flight at a time: what is saved meanwhile waits for the answer and
//! goes as the next batch.

pub mod link;
pub mod report;
mod sources;
pub mod watch;

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use viso_dsl::Diagnostic;
use viso_project::LockGuard;
use viso_view::dev::wire::{
    CommitCounts, HostMessage, PatchBundle, PatchSection, Revision, RuntimeHello, RuntimeMessage,
    Stage, UiSources, ViewSource, source_hash,
};

pub use link::{Deliver, Expect, LinkEvent};
pub use watch::Change;

use crate::output::{Output, Source};
use report::{Outcome, ReloadEvent, intern_code, rejection_stage};
use sources::{Changed, Sources, failure_lines};
use watch::Watcher;

/// How long the session waits after the latest change of a batch before it
/// takes the batch (§7.1): five times the widest arrival spread measured for
/// 16 files written back to back (`watch::tests::multi_file_save_spread`),
/// and what a single save waits for it.
pub const COALESCE: Duration = Duration::from_millis(5);

/// How long a batch that keeps growing waits at most after its first change.
pub const COALESCE_LIMIT: Duration = Duration::from_millis(50);

/// A batch of changes being coalesced.
struct Batch {
    first: Instant,
    last: Instant,
}

/// The running app the session patches.
struct Runtime {
    hello: RuntimeHello,
    writer: Sender<HostMessage>,
    /// The revision it matches.
    revision: Revision,
    in_flight: Option<InFlight>,
}

/// A patch sent and not yet answered.
struct InFlight {
    base: Revision,
    next: Revision,
    /// Each view sent: its index, and the text sent.
    views: Vec<(usize, String)>,
    /// Each catalog sent and its revision.
    catalogs: Vec<(usize, u64)>,
    started: Instant,
}

/// The dev session.
pub struct DevSession {
    root: PathBuf,
    /// The session and build ids as the `dev` events spell them.
    session: String,
    build: String,
    _lock: LockGuard,
    _watcher: Watcher,
    sources: Sources,
    /// The hash of `Viso.toml` as the build read it.
    manifest: Option<u64>,
    runtime: Option<Runtime>,
    batch: Option<Batch>,
    /// Whether a batch is to be taken now: the runtime reported its mounts
    /// or answered a patch.
    ready: bool,
    /// The revision the next candidate takes.
    next_revision: u64,
}

impl DevSession {
    /// Starts the session of the project rooted at `root`, a canonical path,
    /// holding `lock`; the watcher hands each change to `deliver`.
    pub fn start(
        root: PathBuf,
        expect: &Expect,
        lock: LockGuard,
        deliver: impl FnMut(Change) -> bool + Send + 'static,
    ) -> Self {
        let watcher = Watcher::spawn(root.clone(), deliver);
        DevSession {
            root,
            session: expect.dev_session.to_hex(),
            build: expect.build_id.to_hex(),
            _lock: lock,
            _watcher: watcher,
            sources: Sources::default(),
            manifest: None,
            runtime: None,
            batch: None,
            ready: false,
            next_revision: Revision::LAUNCH.0 + 1,
        }
    }

    /// When the session next wants [`tick`](Self::tick)ed.
    pub fn deadline(&self) -> Option<Instant> {
        if self.ready {
            return Some(Instant::now());
        }
        let batch = self.batch.as_ref()?;
        Some((batch.last + COALESCE).min(batch.first + COALESCE_LIMIT))
    }

    /// Takes the latest content of a file of the scope.
    pub fn change(&mut self, change: Change, out: &mut Output) {
        let now = Instant::now();
        if change.path == self.root.join(viso_project::MANIFEST_NAME) {
            let hash = source_hash(&change.text);
            match self.manifest {
                None => self.manifest = Some(hash),
                Some(built) if built != hash => out.log(
                    "warn",
                    "tool",
                    "`Viso.toml` changed: the running app keeps the configuration it was \
                     built with; restart `viso run` to apply it",
                ),
                Some(_) => {}
            }
            return;
        }
        if let Changed::Other = self.sources.change(&change.path, change.text) {
            return;
        }
        match &mut self.batch {
            Some(batch) => batch.last = now,
            None => {
                self.batch = Some(Batch {
                    first: now,
                    last: now,
                })
            }
        }
    }

    /// Takes what a connection thread delivered.
    pub fn link(&mut self, event: LinkEvent, out: &mut Output) {
        match event {
            LinkEvent::Connected(connection) => {
                let hello = connection.hello;
                out.log(
                    "info",
                    "tool",
                    &format!(
                        "dev runtime connected at r{} ({})",
                        hello.current_revision.0,
                        hello.target.as_str()
                    ),
                );
                self.sources.unmount();
                self.runtime = Some(Runtime {
                    revision: hello.current_revision,
                    hello,
                    writer: connection.writer,
                    in_flight: None,
                });
            }
            LinkEvent::Runtime(message) => self.runtime_message(message, out),
            LinkEvent::Closed => {
                self.runtime = None;
                self.sources.unmount();
            }
            LinkEvent::Refused(reason) => out.log("warn", "tool", &reason),
        }
    }

    /// Takes the batch when it is due.
    pub fn tick(&mut self, out: &mut Output) {
        if self.deadline().is_none_or(|due| due > Instant::now()) {
            return;
        }
        self.batch = None;
        self.ready = false;
        self.flush(out);
    }

    fn runtime_message(&mut self, message: RuntimeMessage, out: &mut Output) {
        match message {
            RuntimeMessage::Mounts(entries) => {
                for entry in &entries {
                    self.sources.mount(entry);
                }
                self.ready = true;
            }
            RuntimeMessage::Ack(ack) => {
                let Some(sent) = self.answered(ack.revision) else {
                    return out.log(
                        "warn",
                        "tool",
                        &format!("the app applied r{} unasked", ack.revision.0),
                    );
                };
                let elapsed = elapsed_us(sent.started);
                for (at, text) in &sent.views {
                    let view = &mut self.sources.views[*at];
                    let Some(mount) = view.mount.as_mut() else {
                        continue;
                    };
                    let hash = source_hash(text);
                    mount.running = hash;
                    mount.settled = Some(hash);
                    mount.failing = false;
                    let file = mount.file;
                    let counts = ack
                        .files
                        .iter()
                        .find(|commit| commit.file == file)
                        .map_or_else(CommitCounts::default, |commit| commit.counts);
                    let source = Source::new(&view.path, &self.root, text);
                    let mut codes = Vec::new();
                    for notice in ack.notices.iter().filter(|n| n.file == file) {
                        let Some(code) = intern_code(&notice.code) else {
                            continue;
                        };
                        let range =
                            viso_dsl::TextRange::new(notice.start.into(), notice.end.into());
                        let diagnostic = Diagnostic::warning(code, range, notice.message.clone());
                        if range.end().to_usize() <= text.len() {
                            out.source(Some(&source), &[], &diagnostic);
                        }
                        codes.push(notice.code.clone());
                    }
                    let event = ReloadEvent {
                        base_revision: sent.base.0,
                        candidate_revision: sent.next.0,
                        last_good_revision: ack.revision.0,
                        outcome: Outcome::of_commit(&counts),
                        stage: Stage::RuntimeCommit,
                        elapsed_us: elapsed,
                        counts,
                        codes,
                    };
                    out.dev(source.name(), &self.session, &self.build, &event);
                }
                for (catalog, revision) in sent.catalogs {
                    self.sources.catalogs[catalog].sent = Some(revision);
                }
            }
            RuntimeMessage::Nack(nack) => {
                let Some(sent) = self.answered(nack.candidate_revision) else {
                    return out.log(
                        "warn",
                        "tool",
                        &format!(
                            "the app refused a frame at {}: {}",
                            nack.stage.as_str(),
                            nack.diagnostic_codes.join(", ")
                        ),
                    );
                };
                if let Some(runtime) = &mut self.runtime {
                    runtime.revision = nack.last_good_revision;
                }
                let elapsed = elapsed_us(sent.started);
                for (at, text) in &sent.views {
                    let view = &mut self.sources.views[*at];
                    if let Some(mount) = view.mount.as_mut() {
                        mount.settled = Some(source_hash(text));
                    }
                    let event = ReloadEvent {
                        base_revision: sent.base.0,
                        candidate_revision: sent.next.0,
                        last_good_revision: nack.last_good_revision.0,
                        outcome: Outcome::Rejected,
                        stage: nack.stage,
                        elapsed_us: elapsed,
                        counts: CommitCounts::default(),
                        codes: nack.diagnostic_codes.clone(),
                    };
                    let name = Source::new(&view.path, &self.root, "").name().to_owned();
                    out.dev(&name, &self.session, &self.build, &event);
                }
            }
            RuntimeMessage::Log { level, line } => out.log(level.as_str(), "app", &line),
            RuntimeMessage::Dropped { count } => out.log(
                "warn",
                "tool",
                &format!("the app dropped {count} dev messages: `viso run` read too slowly"),
            ),
            // The link thread delivers no second hello.
            RuntimeMessage::Hello(_) => {}
        }
    }

    /// Closes the patch in flight if it is the one moving to `next`, and
    /// takes the next batch now.
    fn answered(&mut self, next: Revision) -> Option<InFlight> {
        let runtime = self.runtime.as_mut()?;
        if runtime.in_flight.as_ref()?.next != next {
            return None;
        }
        let sent = runtime.in_flight.take()?;
        runtime.revision = next;
        self.ready = true;
        Some(sent)
    }

    /// Compiles the batch and sends what compiled.
    fn flush(&mut self, out: &mut Output) {
        for failed in self.sources.compile_catalogs() {
            for (file, diagnostic) in &failed.diagnostics {
                let (path, text) = &failed.files[*file];
                out.source(Some(&Source::new(path, &self.root, text)), &[], diagnostic);
            }
        }
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        if runtime.in_flight.is_some() {
            return;
        }
        // A rejected edit reverted to what the runtime runs: clear its failure.
        for view in &mut self.sources.views {
            let hash = view.hash();
            if let Some(mount) = view.mount.as_mut()
                && mount.failing
                && hash == Some(mount.running)
            {
                mount.failing = false;
                mount.settled = None;
                let _ = runtime.writer.send(HostMessage::Failure {
                    file: mount.file,
                    lines: Vec::new(),
                });
            }
        }
        let candidates = self.sources.candidates();
        if candidates.is_empty() {
            return;
        }
        let started = Instant::now();
        let base = runtime.revision;
        let next = Revision(self.next_revision);
        self.next_revision += 1;
        let mut views = Vec::new();
        for at in candidates {
            let compiled = self.sources.compile(at);
            let view = &mut self.sources.views[at];
            let (Some(text), Some(mount)) = (view.text.clone(), view.mount.as_mut()) else {
                continue;
            };
            let diagnostics = match compiled {
                Ok(()) => {
                    views.push((at, text));
                    continue;
                }
                Err(diagnostics) => diagnostics,
            };
            mount.settled = Some(source_hash(&text));
            mount.failing = true;
            let source = Source::new(&view.path, &self.root, &text);
            for diagnostic in &diagnostics {
                out.source(Some(&source), &[], diagnostic);
            }
            let _ = runtime.writer.send(HostMessage::Failure {
                file: mount.file,
                lines: failure_lines(source.name(), &text, &diagnostics),
            });
            let event = ReloadEvent {
                base_revision: base.0,
                candidate_revision: next.0,
                last_good_revision: base.0,
                outcome: Outcome::Rejected,
                stage: rejection_stage(&diagnostics),
                elapsed_us: elapsed_us(started),
                counts: CommitCounts::default(),
                codes: diagnostics.iter().map(|d| d.code.to_owned()).collect(),
            };
            out.dev(source.name(), &self.session, &self.build, &event);
        }
        if views.is_empty() {
            return;
        }
        let mut ui = UiSources::default();
        let mut catalogs: Vec<(usize, u64)> = Vec::new();
        for (at, text) in &views {
            let mount = self.sources.views[*at]
                .mount
                .as_ref()
                .expect("a candidate is mounted");
            ui.views.push(ViewSource {
                file: mount.file,
                source: text.clone(),
            });
            if let Some(c) = mount.catalog {
                let catalog = &self.sources.catalogs[c];
                if catalog.sent != Some(catalog.revision)
                    && !catalogs.iter().any(|&(at, _)| at == c)
                {
                    ui.catalogs.push(catalog.wire());
                    catalogs.push((c, catalog.revision));
                }
            }
        }
        let patch = PatchBundle {
            dev_session: runtime.hello.dev_session,
            target_runtime: runtime.hello.runtime_session,
            base_revision: base,
            next_revision: next,
            build_id: runtime.hello.build_id,
            sections: vec![PatchSection::Ui(ui)],
        };
        if runtime
            .writer
            .send(HostMessage::Patch(Box::new(patch)))
            .is_err()
        {
            return;
        }
        runtime.in_flight = Some(InFlight {
            base,
            next,
            views,
            catalogs,
            started,
        });
    }
}

fn elapsed_us(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use viso_project::{Layout, LockKind, Target};
    use viso_view::dev::wire::{
        BuildId, DevSessionId, Domains, FileCommit, FileId, MountEntry, PatchAck, PatchNack,
        PatchTimings, ProjectFingerprint, RuntimeSessionId, RuntimeTarget, SchemaFingerprint,
    };

    use super::link::Connection;
    use super::*;
    use crate::args::Global;

    const VIEW: &str = "component Counter {\n    state count = 0;\n    view {\n        Column {\n            Text { width: 120dp; }\n        }\n    }\n}\n";

    struct Fixture {
        dev: DevSession,
        out: Output,
        sent: Receiver<HostMessage>,
        path: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("viso-dev-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let lock = LockGuard::acquire(
                &Layout::new(&root),
                LockKind::DevSession {
                    target: Target::Host,
                    device: None,
                },
            )
            .unwrap();
            let expect = Expect {
                token: "t".into(),
                dev_session: DevSessionId(1),
                build_id: BuildId(2),
                project: ProjectFingerprint(3),
                schema: SchemaFingerprint(4),
            };
            let mut dev = DevSession::start(root.clone(), &expect, lock, |_| true);
            let global = Global {
                project: None,
                quiet: true,
                json: true,
            };
            let mut out = Output::new(&global, Some("run"));
            let (writer, sent) = mpsc::channel();
            let hello = RuntimeHello {
                protocol_version: viso_view::dev::wire::DEV_PROTOCOL_VERSION,
                token: "t".into(),
                dev_session: DevSessionId(1),
                runtime_session: RuntimeSessionId(9),
                build_id: BuildId(2),
                current_revision: Revision::LAUNCH,
                schema_fingerprint: SchemaFingerprint(4),
                capabilities: Domains::NONE,
                target: RuntimeTarget::DesktopHost,
            };
            dev.link(LinkEvent::Connected(Connection { hello, writer }), &mut out);
            let path = root.join("view.vs");
            let mut fixture = Fixture {
                dev,
                out,
                sent,
                path,
            };
            fixture.save(VIEW);
            fixture.runtime(RuntimeMessage::Mounts(vec![MountEntry {
                file: FileId(0),
                path: fixture.path.display().to_string(),
                package: "app".into(),
                module: vec!["view".into()],
                language: None,
                catalog: None,
                capabilities: Vec::new(),
                source_hash: source_hash(VIEW),
            }]));
            fixture
        }

        fn save(&mut self, text: &str) {
            let change = Change {
                path: self.path.clone(),
                text: text.into(),
            };
            self.dev.change(change, &mut self.out);
        }

        fn runtime(&mut self, message: RuntimeMessage) {
            self.dev.link(LinkEvent::Runtime(message), &mut self.out);
        }

        /// Ticks the session once its batch is due and returns what it sent.
        fn settle(&mut self) -> Vec<HostMessage> {
            if let Some(due) = self.dev.deadline() {
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
            self.dev.tick(&mut self.out);
            self.sent.try_iter().collect()
        }

        fn ack(&mut self, revision: u64) {
            self.runtime(RuntimeMessage::Ack(PatchAck {
                revision: Revision(revision),
                applied_domains: Domains::NONE.with(viso_view::dev::wire::Domain::Ui),
                files: vec![FileCommit {
                    file: FileId(0),
                    counts: CommitCounts {
                        mounts: 1,
                        ..CommitCounts::default()
                    },
                }],
                notices: Vec::new(),
                timings: PatchTimings::default(),
            }));
        }
    }

    /// The revisions of `message`, a patch, and the source it sets file 0 to.
    fn patch(message: &HostMessage) -> (u64, u64, &str) {
        let HostMessage::Patch(patch) = message else {
            panic!("not a patch: {message:?}");
        };
        let PatchSection::Ui(ui) = &patch.sections[0];
        assert_eq!(ui.views[0].file, FileId(0));
        (
            patch.base_revision.0,
            patch.next_revision.0,
            &ui.views[0].source,
        )
    }

    #[test]
    fn what_the_runtime_runs_is_not_sent() {
        let mut f = Fixture::new("running");
        assert!(f.settle().is_empty());
        f.save(VIEW);
        assert!(f.settle().is_empty(), "the same content is no change");
    }

    #[test]
    fn an_edit_saved_while_the_app_built_is_the_first_patch() {
        let mut f = Fixture::new("during-build");
        let edited = VIEW.replace("120dp", "130dp");
        f.save(&edited);
        // The runtime reports again, as after a relaunch: it runs the build.
        let sent = f.settle();
        assert_eq!(sent.len(), 1);
        assert_eq!(patch(&sent[0]), (1, 2, edited.as_str()));
    }

    #[test]
    fn one_patch_is_in_flight_and_later_edits_go_after_its_answer() {
        let mut f = Fixture::new("in-flight");
        let first = VIEW.replace("120dp", "140dp");
        f.save(&first);
        let sent = f.settle();
        assert_eq!(patch(&sent[0]), (1, 2, first.as_str()));

        let second = VIEW.replace("120dp", "160dp");
        f.save(&second);
        assert!(f.settle().is_empty(), "waits for the answer");
        f.ack(2);
        let sent = f.settle();
        assert_eq!(patch(&sent[0]), (2, 3, second.as_str()));
        f.ack(3);
        assert!(f.settle().is_empty());
    }

    #[test]
    fn a_rejected_edit_is_reported_shown_and_cleared_when_reverted() {
        let mut f = Fixture::new("rejected");
        let errors = f.out.errors();
        f.save(&VIEW.replace("state count = 0;", "state count = ;"));
        let sent = f.settle();
        let [HostMessage::Failure { file, lines }] = &sent[..] else {
            panic!("not one failure: {sent:?}");
        };
        assert_eq!(*file, FileId(0));
        assert!(lines[0].starts_with("view.vs:2:"), "{lines:?}");
        assert!(f.out.errors() > errors, "the diagnostics are reported");
        assert!(
            f.settle().is_empty(),
            "a rejected edit is not compiled again"
        );

        f.save(VIEW);
        let sent = f.settle();
        assert!(
            matches!(&sent[..], [HostMessage::Failure { lines, .. }] if lines.is_empty()),
            "{sent:?}"
        );

        // A good edit after a rejected one is sent, revisions skipping the
        // rejected candidate's.
        let good = VIEW.replace("120dp", "150dp");
        f.save(&good);
        let sent = f.settle();
        assert_eq!(patch(&sent[0]), (1, 3, good.as_str()));
    }

    #[test]
    fn a_nack_keeps_the_runtimes_revision_and_is_not_resent() {
        let mut f = Fixture::new("nacked");
        let edited = VIEW.replace("120dp", "170dp");
        f.save(&edited);
        f.settle();
        f.runtime(RuntimeMessage::Nack(PatchNack {
            base_revision: Revision(1),
            candidate_revision: Revision(2),
            stage: Stage::RuntimeStage,
            diagnostic_codes: vec!["E2103".into()],
            last_good_revision: Revision(1),
        }));
        assert!(f.settle().is_empty(), "the refused content is not resent");
        let next = VIEW.replace("120dp", "180dp");
        f.save(&next);
        let sent = f.settle();
        assert_eq!(patch(&sent[0]), (1, 3, next.as_str()));
    }

    #[test]
    fn a_closed_runtime_is_forgotten() {
        let mut f = Fixture::new("closed");
        f.dev.link(LinkEvent::Closed, &mut f.out);
        f.save(&VIEW.replace("120dp", "190dp"));
        assert!(f.settle().is_empty());
    }
}
