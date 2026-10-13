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
//! the last-good UI, and it is not sent. Each view that compiles is planned
//! against the compiled view its mounts run (§11): the patch carries the
//! candidate in its release form and the typed plan that moves the mounts
//! onto it, so the app applies it with no compiler present. A view whose
//! module declares systems is also classified against the game its mounts
//! run (§16–§17): the tier is reported as an `E5103` note and sent as the
//! file's `system` entry beside its view, so the game swaps with the view in
//! one revision. The views that compile are sent as one patch, and the runtime's ACK or NACK closes the
//! candidate; an ACK makes each candidate the last-good its next edit is
//! planned against. One patch is in flight at a time: what is saved meanwhile
//! waits for the answer and goes as the next batch.
//!
//! With `--no-hot-reload` (§2.1) the session still watches, compiles and
//! reports each candidate, but sends the app nothing: a candidate that
//! compiles is `held`.
//!
//! Across a Rust warm restart the session asks the old app for its
//! snapshot and keeps it in memory, never written, printed or logged (§32),
//! with the view each file of the old app ran. When the app launched to
//! restore reports its first mounts, each file's view as the new build
//! embeds it is planned from the one the old app ran — the plan a patch
//! between the two would carry — and the snapshot goes to the app with the
//! plans.
//!
//! The session also relays what `viso game` asks through its control
//! endpoint ([`control`]): the game is found among the mounted views by its
//! file, the question goes to the app, and a peeked state comes back written
//! as JSON by its type, as the last good of the file declares it.

pub(crate) mod build;
pub(crate) mod cargo;
pub mod control;
pub mod link;
pub mod report;
mod sources;
pub mod watch;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use viso_dsl::Diagnostic;
use viso_dsl::assets::{ASSET_ERROR, ResourceId};
use viso_dsl::behavior::probe::state_json;
use viso_dsl::hotreload::{CandidatePlan, game_patch, view_patch};
use viso_project::LockGuard;
use viso_view::dev::wire::{
    BlobRef, Blobs, CommitCounts, Domain, GameAnswer, GameAsk, GameQuery, GameRefusal, GameReply,
    HostMessage, PatchBundle, PatchSection, Rebuild, ResourceAsset, ResourcePatch, RestoreBundle,
    RestoreReport, Revision, RuntimeHello, RuntimeMessage, RuntimeSessionId, ShaderLang, Stage,
    SystemPatch, UiPatch, WindowGeometry, blob_chunks, source_hash,
};

pub use control::{Control, ControlCall, ControlDeliver, ControlReply};
pub use link::{Deliver, Expect, LinkEvent};
pub use watch::Change;

use crate::output::{Output, Source};
use build::{Build, BuildFailure};
use cargo::RustScope;
use report::{Outcome, RebuildEvent, ReloadEvent, intern_code, rejection_stage};
use sources::{AssetRef, Changed, Sources, failure_lines};
pub(crate) use watch::WatchScope;
use watch::Watcher;

/// How long the session waits after the latest change of a batch before it
/// takes the batch (§7.1): five times the widest arrival spread measured for
/// 16 files written back to back (`watch::tests::multi_file_save_spread`),
/// and what a single save waits for it.
pub const COALESCE: Duration = Duration::from_millis(5);

/// How long a batch that keeps growing waits at most after its first change.
pub const COALESCE_LIMIT: Duration = Duration::from_millis(50);

/// What a World Rebuild keeps of a running game: the player stays where the
/// developer left them while the level around them is built again.
const KEEP: Rebuild = Rebuild::KeepCharacters;

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
    /// The questions of `viso game` it was sent and has not answered.
    asked: Vec<Asked>,
    /// The chunks of the snapshot it is sending.
    blobs: Blobs,
}

/// The snapshot of an app, kept for the app launched after it.
pub(crate) struct HeldSnapshot {
    bytes: Arc<[u8]>,
    /// The view each file of the app ran, by path: what the restore plans
    /// from.
    views: Vec<(PathBuf, CandidatePlan)>,
}

impl Drop for Runtime {
    /// A runtime gone answers nothing more.
    fn drop(&mut self) {
        for asked in self.asked.drain(..) {
            let _ = asked.reply.send(ControlReply::refused(
                control::GAME_NOT_RUNNING,
                "the app exited before it answered",
            ));
        }
    }
}

/// A question of `viso game` relayed to the runtime.
struct Asked {
    id: u32,
    /// The view of the game asked about, by its index in the source graph.
    at: usize,
    ask: GameAsk,
    reply: std::sync::mpsc::Sender<ControlReply>,
}

/// A patch sent and not yet answered.
struct InFlight {
    base: Revision,
    next: Revision,
    views: Vec<SentView>,
    assets: Vec<SentAsset>,
    started: Instant,
}

/// An asset a patch carries.
struct SentAsset {
    asset: AssetRef,
    /// The hash and length of the bytes sent.
    hash: u64,
    len: usize,
}

/// A view a patch carries.
struct SentView {
    /// Its index in the source graph.
    at: usize,
    /// The text sent, and its compiled view.
    text: String,
    candidate: CandidatePlan,
    /// The codes of the notes the host reported compiling it.
    notes: Vec<String>,
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
    /// Whether candidates are sent to the app; not with `--no-hot-reload`.
    apply: bool,
    /// The id the next question of `viso game` takes.
    next_query: u32,
    /// The latest snapshot the app sent.
    held: Option<HeldSnapshot>,
    /// Whether the next app to connect was launched to restore it.
    restore_next: bool,
    /// The event of the Rust build the app is restarting on, reported once
    /// the restart lands.
    restarting: Option<RebuildEvent>,
    /// Where the app's windows were when it sent its snapshot.
    windows: Vec<WindowGeometry>,
}

impl DevSession {
    /// Starts the session watching `scope`, holding `lock`; the watcher
    /// hands each change to `deliver`. `apply` is whether candidates are sent
    /// to the app.
    pub(crate) fn start(
        scope: WatchScope,
        expect: &Expect,
        lock: LockGuard,
        apply: bool,
        deliver: impl FnMut(Change) -> bool + Send + 'static,
    ) -> Self {
        let root = scope.project.clone();
        let watcher = Watcher::spawn(scope, deliver);
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
            apply,
            next_query: 0,
            held: None,
            restore_next: false,
            restarting: None,
            windows: Vec::new(),
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
        if let Some(dir) = watch::asset_dir(&self.root, &change.path) {
            let dir = dir.to_owned();
            self.sources.change_asset(&dir, &change.path, change.bytes);
            self.batched(now);
            return;
        }
        // A source or catalog that is not UTF-8 is not one the build reads.
        let Ok(text) = String::from_utf8(change.bytes) else {
            return;
        };
        if change.path == self.root.join(viso_project::MANIFEST_NAME) {
            let hash = source_hash(&text);
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
        if let Changed::Other = self.sources.change(&change.path, text) {
            return;
        }
        self.batched(now);
    }

    /// Watches the Rust scope `rust` from now on.
    pub(crate) fn rescope(&self, rust: RustScope) {
        self._watcher.rescope(rust);
    }

    /// Reports `build`, a build of the Rust sources after the launch: a
    /// failure keeps the app running; one that relinked nothing changes
    /// nothing. Whether the app is to restart on the new executable: its
    /// event waits for the restart to land.
    pub(crate) fn rebuilt(&mut self, build: &Build, out: &mut Output) -> bool {
        if build.unchanged {
            out.log("info", "tool", "the Rust edit changed nothing the app runs");
            return false;
        }
        let (outcome, codes) = match &build.result {
            Ok(_) if self.apply => (Outcome::WarmRestarted, Vec::new()),
            Ok(_) => (Outcome::Held, Vec::new()),
            Err(BuildFailure::Cancelled) => return false,
            Err(failure) => {
                out.log(
                    "error",
                    "tool",
                    &format!("{}; the app keeps running", build_failure(failure)),
                );
                let codes = match failure {
                    BuildFailure::Failed { codes, .. } => codes.clone(),
                    _ => Vec::new(),
                };
                (Outcome::Rejected, codes)
            }
        };
        let revision = self
            .runtime
            .as_ref()
            .map_or(Revision::LAUNCH, |r| r.revision);
        let candidate = self.next_revision;
        self.next_revision += 1;
        let files = build
            .files
            .iter()
            .map(|path| Source::new(path, &self.root, "").name().to_owned())
            .collect();
        let runtime_session = self
            .runtime
            .as_ref()
            .map_or_else(String::new, |r| r.hello.runtime_session.to_hex());
        let event = RebuildEvent {
            files,
            base_revision: revision.0,
            candidate_revision: candidate,
            last_good_revision: revision.0,
            outcome,
            codes,
            build_us: u64::try_from(build.elapsed.as_micros()).unwrap_or(u64::MAX),
        };
        if outcome == Outcome::WarmRestarted {
            // A restart already under way takes the newer build: its event
            // names every file since the app last restarted.
            let mut event = event;
            if let Some(earlier) = self.restarting.take() {
                event.base_revision = earlier.base_revision;
                for file in earlier.files {
                    if !event.files.contains(&file) {
                        event.files.push(file);
                    }
                }
            }
            self.restarting = Some(event);
            return true;
        }
        out.rebuild(&self.session, &runtime_session, &self.build, &event);
        false
    }

    /// The app restarted on the new executable: with no snapshot to restore
    /// it runs fresh, which reports the restart now; otherwise the restore
    /// reports it.
    pub(crate) fn relaunched(&mut self, out: &mut Output) {
        if !self.restore_next {
            self.finish_restart(Outcome::WarmRestarted, Vec::new(), out);
        }
    }

    /// The restart did not land: the new executable did not launch.
    pub(crate) fn restart_failed(&mut self, why: &str, out: &mut Output) {
        out.log("error", "tool", why);
        self.restore_next = false;
        self.finish_restart(Outcome::Rejected, Vec::new(), out);
    }

    /// Reports the restart under way with `outcome`.
    fn finish_restart(&mut self, outcome: Outcome, codes: Vec<String>, out: &mut Output) {
        let Some(mut event) = self.restarting.take() else {
            return;
        };
        event.outcome = outcome;
        event.codes.extend(codes);
        event.last_good_revision = match outcome {
            Outcome::Rejected => event.base_revision,
            _ => Revision::LAUNCH.0,
        };
        let runtime_session = self
            .runtime
            .as_ref()
            .map_or_else(String::new, |r| r.hello.runtime_session.to_hex());
        out.rebuild(&self.session, &runtime_session, &self.build, &event);
    }

    /// Adds a change at `now` to the batch being coalesced.
    fn batched(&mut self, now: Instant) {
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
                    asked: Vec::new(),
                    blobs: Blobs::default(),
                });
            }
            LinkEvent::Runtime(from, message) => {
                if self.runs(from) {
                    self.runtime_message(message, out);
                }
            }
            LinkEvent::Closed(from) => {
                if self.runs(from) {
                    self.runtime = None;
                    self.sources.unmount();
                }
            }
            LinkEvent::Refused(reason) => out.log("warn", "tool", &reason),
        }
    }

    /// Whether `launch` is the runtime connected.
    fn runs(&self, launch: RuntimeSessionId) -> bool {
        self.runtime
            .as_ref()
            .is_some_and(|runtime| runtime.hello.runtime_session == launch)
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
                let mounted: Vec<usize> = entries
                    .iter()
                    .map(|entry| self.sources.mount(entry))
                    .collect();
                // Each mount's last-good compiles now, off the first edit's
                // path; one that cannot is retried, and reported, then.
                self.compile_catalogs(out);
                for &at in &mounted {
                    let _ = self.sources.baseline(at);
                }
                if std::mem::take(&mut self.restore_next) {
                    self.send_restore(&mounted, out);
                }
                self.ready = true;
            }
            RuntimeMessage::Blob(chunk) => {
                if let Some(runtime) = &mut self.runtime {
                    runtime.blobs.add(chunk);
                }
            }
            RuntimeMessage::Snapshot {
                blob,
                windows,
                capture_us,
            } => {
                let Some(runtime) = &mut self.runtime else {
                    return;
                };
                self.windows = windows;
                let bytes = runtime.blobs.take(blob);
                runtime.blobs.clear();
                let Some(bytes) = bytes else {
                    return out.log(
                        "warn",
                        "tool",
                        "the app's snapshot did not arrive whole: the next launch runs fresh",
                    );
                };
                let views = self
                    .sources
                    .views
                    .iter()
                    .filter_map(|view| {
                        let last_good = view.mount.as_ref()?.last_good.as_ref()?;
                        Some((view.path.clone(), last_good.clone()))
                    })
                    .collect();
                out.log(
                    "info",
                    "tool",
                    &format!(
                        "snapshot taken: {} bytes in {:.1} ms",
                        bytes.len(),
                        capture_us as f64 / 1000.0
                    ),
                );
                self.held = Some(HeldSnapshot { bytes, views });
            }
            RuntimeMessage::Restored(report) => self.restored(&report, out),
            RuntimeMessage::Ack(ack) => {
                let Some(sent) = self.answered(ack.revision) else {
                    return out.log(
                        "warn",
                        "tool",
                        &format!("the app applied r{} unasked", ack.revision.0),
                    );
                };
                let elapsed = elapsed_us(sent.started);
                let runtime_session = self
                    .runtime
                    .as_ref()
                    .map_or_else(String::new, |r| r.hello.runtime_session.to_hex());
                for SentView {
                    at,
                    text,
                    candidate,
                    notes,
                    ..
                } in sent.views
                {
                    let view = &mut self.sources.views[at];
                    let Some(mount) = view.mount.as_mut() else {
                        continue;
                    };
                    let hash = source_hash(&text);
                    mount.running = hash;
                    mount.settled = Some(hash);
                    mount.failing = false;
                    mount.last_good = Some(candidate);
                    let file = mount.file;
                    let counts = ack
                        .files
                        .iter()
                        .find(|commit| commit.file == file)
                        .map_or_else(CommitCounts::default, |commit| commit.counts);
                    let source = Source::new(&view.path, &self.root, &text);
                    let mut codes = notes;
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
                        game: ack.games.iter().find(|game| game.file == file).copied(),
                    };
                    out.dev(
                        source.name(),
                        &self.session,
                        &runtime_session,
                        &self.build,
                        &event,
                    );
                }
                for SentAsset { asset, hash, .. } in sent.assets {
                    self.sources.asset_applied(&asset, hash);
                    let id = ResourceId::of(&asset.path);
                    let dirty = ack
                        .resources
                        .iter()
                        .find(|commit| commit.id == id)
                        .map(|commit| commit.dirty)
                        .unwrap_or_default();
                    let event = ReloadEvent {
                        base_revision: sent.base.0,
                        candidate_revision: sent.next.0,
                        last_good_revision: ack.revision.0,
                        outcome: Outcome::Applied,
                        stage: Stage::RuntimeCommit,
                        elapsed_us: elapsed,
                        counts: CommitCounts {
                            dirty,
                            ..CommitCounts::default()
                        },
                        codes: Vec::new(),
                        game: None,
                    };
                    let path = self.sources.asset_path(&asset);
                    let name = Source::new(&path, &self.root, "").name().to_owned();
                    out.dev(&name, &self.session, &runtime_session, &self.build, &event);
                }
            }
            RuntimeMessage::Nack(nack) if nack.stage == Stage::SnapshotRestore => {
                out.log(
                    "warn",
                    "tool",
                    &format!(
                        "the app did not restore its state ({}): it runs fresh",
                        nack.diagnostic_codes.join(", ")
                    ),
                );
                self.finish_restart(Outcome::WarmRestarted, nack.diagnostic_codes, out);
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
                let runtime_session = match &mut self.runtime {
                    Some(runtime) => {
                        runtime.revision = nack.last_good_revision;
                        runtime.hello.runtime_session.to_hex()
                    }
                    None => String::new(),
                };
                let elapsed = elapsed_us(sent.started);
                for sent_view in &sent.views {
                    let view = &mut self.sources.views[sent_view.at];
                    // Its mounts still run their last-good: the next edit is
                    // planned against it.
                    if let Some(mount) = view.mount.as_mut() {
                        mount.settled = Some(source_hash(&sent_view.text));
                    }
                    let mut codes = sent_view.notes.clone();
                    codes.extend(nack.diagnostic_codes.iter().cloned());
                    let event = ReloadEvent {
                        base_revision: sent.base.0,
                        candidate_revision: sent.next.0,
                        last_good_revision: nack.last_good_revision.0,
                        outcome: Outcome::Rejected,
                        stage: nack.stage,
                        elapsed_us: elapsed,
                        counts: CommitCounts::default(),
                        codes,
                        game: None,
                    };
                    let name = Source::new(&view.path, &self.root, "").name().to_owned();
                    out.dev(&name, &self.session, &runtime_session, &self.build, &event);
                }
                for SentAsset { asset, hash, .. } in &sent.assets {
                    // It runs its last good: the next edit of it goes again.
                    self.sources.asset_settled(asset, *hash);
                    let event = ReloadEvent {
                        base_revision: sent.base.0,
                        candidate_revision: sent.next.0,
                        last_good_revision: nack.last_good_revision.0,
                        outcome: Outcome::Rejected,
                        stage: nack.stage,
                        elapsed_us: elapsed,
                        counts: CommitCounts::default(),
                        codes: nack.diagnostic_codes.clone(),
                        game: None,
                    };
                    let path = self.sources.asset_path(asset);
                    let name = Source::new(&path, &self.root, "").name().to_owned();
                    out.dev(&name, &self.session, &runtime_session, &self.build, &event);
                }
            }
            RuntimeMessage::Log { level, line } => out.log(level.as_str(), "app", &line),
            RuntimeMessage::Dropped { count } => out.log(
                "warn",
                "tool",
                &format!("the app dropped {count} dev messages: `viso run` read too slowly"),
            ),
            RuntimeMessage::Game(answer) => self.answer(answer),
            // The link thread delivers no second hello.
            RuntimeMessage::Hello(_) => {}
        }
    }

    /// Asks the app for its snapshot, dropping the one held; whether an app
    /// runs to be asked.
    pub(crate) fn request_snapshot(&mut self) -> bool {
        self.held = None;
        self.windows.clear();
        self.runtime
            .as_ref()
            .is_some_and(|runtime| runtime.writer.send(HostMessage::Snapshot).is_ok())
    }

    /// Where the next app's launch window opens: where the first window of
    /// the app was when it sent its snapshot.
    pub(crate) fn launch_window(&self) -> Option<WindowGeometry> {
        self.windows.first().copied()
    }

    /// Whether the app sent the snapshot asked for.
    pub(crate) fn has_snapshot(&self) -> bool {
        self.held.is_some()
    }

    /// Restores the held snapshot into the next app to connect, once it
    /// reports its mounts; whether one is held.
    pub(crate) fn restore_next(&mut self) -> bool {
        self.restore_next = self.held.is_some();
        self.restore_next
    }

    /// Sends the held snapshot to the app, with a view for each file of it
    /// the app just reported mounting that the old app ran: the view the
    /// build embeds and the plan to it from the old one.
    fn send_restore(&mut self, mounted: &[usize], out: &mut Output) {
        let (Some(runtime), Some(held)) = (&self.runtime, &self.held) else {
            return;
        };
        let langs = [runtime.hello.shader_lang, ShaderLang::None];
        let mut ui = UiPatch::default();
        for &at in mounted {
            let view = &self.sources.views[at];
            let Some(mount) = &view.mount else {
                continue;
            };
            let old = held.views.iter().find(|(path, _)| *path == view.path);
            let (Some((_, old)), Some(new)) = (old, &mount.last_good) else {
                continue;
            };
            match view_patch(mount.file, Some(old), new, &langs) {
                Ok(patch) => ui.views.push(patch),
                Err(error) => {
                    let name = Source::new(&view.path, &self.root, "").name().to_owned();
                    out.log(
                        "warn",
                        "tool",
                        &format!("`{name}`: its view does not restore: {error}; it runs fresh"),
                    );
                }
            }
        }
        for chunk in blob_chunks(&held.bytes) {
            if runtime.writer.send(HostMessage::Blob(chunk)).is_err() {
                return;
            }
        }
        let _ = runtime
            .writer
            .send(HostMessage::Restore(Box::new(RestoreBundle {
                dev_session: runtime.hello.dev_session,
                target_runtime: runtime.hello.runtime_session,
                snapshot: BlobRef::of(&held.bytes),
                ui,
            })));
    }

    /// Reports what the restore carried, which lands the restart.
    fn restored(&mut self, report: &RestoreReport, out: &mut Output) {
        let mut migrated = 0;
        let mut reset = 0;
        let mut mounts = 0;
        for file in &report.files {
            mounts += file.counts.mounts;
            migrated += file.counts.migrated;
            reset += file.counts.reset;
        }
        let games: u32 = report.games.iter().map(|game| game.sessions).sum();
        out.log(
            "info",
            "tool",
            &format!(
                "restored {mounts} view mounts ({migrated} states carried, {reset} reset, {} \
                 lost), {games} games, {} extensions ({} lost) in {:.1} ms",
                report.views_lost,
                report.extensions,
                report.extensions_lost,
                report.restore_us as f64 / 1000.0
            ),
        );
        self.finish_restart(Outcome::WarmRestarted, Vec::new(), out);
    }

    /// The lock this session holds, to publish its control endpoint in.
    pub fn lock_mut(&mut self) -> &mut LockGuard {
        &mut self._lock
    }

    /// Relays what `viso game` asks about the game `call.game` names to the
    /// app, or refuses it.
    pub fn control(&mut self, call: ControlCall, out: &mut Output) {
        let refuse = |code: &str, message: String| {
            let _ = call.reply.send(ControlReply::refused(code, message));
        };
        let Some(runtime) = &self.runtime else {
            return refuse(
                control::GAME_NOT_RUNNING,
                "the app is not connected to `viso run`".into(),
            );
        };
        if !runtime.hello.capabilities.contains(Domain::System) {
            return refuse(
                control::GAME_NOT_RUNNING,
                "the app runs no game sessions".into(),
            );
        }
        let at = match self.find_game(&call.game) {
            Ok(at) => at,
            Err(reply) => {
                let _ = call.reply.send(reply);
                return;
            }
        };
        let view = &self.sources.views[at];
        let file = view.mount.as_ref().expect("a mounted view").file;
        if let GameAsk::Record { .. } = call.ask {
            let name = Source::new(&view.path, &self.root, "").name().to_owned();
            out.log(
                "info",
                "tool",
                &format!("recording `{name}` from a restarted World"),
            );
        }
        let id = self.next_query;
        self.next_query = self.next_query.wrapping_add(1);
        let runtime = self.runtime.as_mut().expect("connected");
        let query = GameQuery {
            id,
            file,
            ask: call.ask.clone(),
        };
        if runtime.writer.send(HostMessage::Game(query)).is_err() {
            return refuse(
                control::GAME_NOT_RUNNING,
                "the app is not connected to `viso run`".into(),
            );
        }
        runtime.asked.push(Asked {
            id,
            at,
            ask: call.ask,
            reply: call.reply,
        });
    }

    /// The mounted view `game` names: its path from the project root, with
    /// or without the extension, or its file's stem.
    fn find_game(&self, game: &str) -> Result<usize, ControlReply> {
        let game = game.trim_start_matches("./").replace('\\', "/");
        let mut named = Vec::new();
        let mut mounted = Vec::new();
        for (at, view) in self.sources.views.iter().enumerate() {
            if view.mount.is_none() {
                continue;
            }
            let name = Source::new(&view.path, &self.root, "").name().to_owned();
            let bare = name.strip_suffix(".vs").unwrap_or(&name);
            let stem = bare.rsplit('/').next().unwrap_or(bare);
            if name == game || bare == game || stem == game {
                named.push((at, name.clone()));
            }
            mounted.push(name);
        }
        match &named[..] {
            [(at, _)] => Ok(*at),
            [] => Err(ControlReply::Refused {
                code: control::GAME_NOT_RUNNING.into(),
                message: format!("no mounted view is `{game}`"),
                notes: if mounted.is_empty() {
                    vec!["the app mounts no `.vs` view".into()]
                } else {
                    vec![format!("the app mounts {}", quoted(&mounted))]
                },
            }),
            several => Err(ControlReply::Refused {
                code: control::GAME_AMBIGUOUS.into(),
                message: format!("`{game}` names more than one mounted view"),
                notes: vec![format!(
                    "name one by its path: {}",
                    quoted(&several.iter().map(|(_, n)| n.clone()).collect::<Vec<_>>())
                )],
            }),
        }
    }

    /// Hands the runtime's answer to the `viso game` that asked.
    fn answer(&mut self, answer: GameAnswer) {
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        let Some(index) = runtime.asked.iter().position(|a| a.id == answer.id) else {
            return;
        };
        let asked = runtime.asked.swap_remove(index);
        let view = &self.sources.views[asked.at];
        let file = Source::new(&view.path, &self.root, "").name().to_owned();
        let program = view
            .mount
            .as_ref()
            .and_then(|m| m.last_good.as_ref())
            .and_then(|plan| plan.game.as_ref());
        let reply = match (answer.reply, &asked.ask) {
            (
                Ok(GameReply::State {
                    value,
                    tick,
                    sessions,
                }),
                GameAsk::Peek { system, state },
            ) => match state_json(program, system, state, &value) {
                Ok(json) => ControlReply::State {
                    file,
                    json,
                    tick,
                    sessions,
                },
                Err(error) => ControlReply::refused(
                    control::GAME_ANSWER_INVALID,
                    format!("the app sent a malformed state: {error}"),
                ),
            },
            (Ok(GameReply::Tape { tape }), GameAsk::Record { .. }) => {
                ControlReply::Tape { file, tape }
            }
            (Ok(_), _) => ControlReply::refused(
                control::GAME_ANSWER_INVALID,
                "the app answered another question",
            ),
            (Err(refusal), ask) => refusal_reply(&refusal, &file, ask, program),
        };
        let _ = asked.reply.send(reply);
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

    /// Compiles the catalogs that changed, reporting each error.
    fn compile_catalogs(&mut self, out: &mut Output) {
        for failed in self.sources.compile_catalogs() {
            for (file, diagnostic) in &failed.diagnostics {
                let (path, text) = &failed.files[*file];
                out.source(Some(&Source::new(path, &self.root, text)), &[], diagnostic);
            }
        }
    }

    /// Compiles the batch and sends what compiled.
    fn flush(&mut self, out: &mut Output) {
        self.compile_catalogs(out);
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        if runtime.in_flight.is_some() {
            return;
        }
        let runtime_session = runtime.hello.runtime_session.to_hex();
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
        if candidates.is_empty() && self.sources.asset_candidates(&[]).is_empty() {
            return;
        }
        let started = Instant::now();
        let base = runtime.revision;
        let next = Revision(self.next_revision);
        self.next_revision += 1;
        let mut views = Vec::new();
        for at in candidates {
            let Some(compiled) = self.sources.compile(at) else {
                continue;
            };
            let view = &mut self.sources.views[at];
            let (Some(text), Some(mount)) = (view.text.clone(), view.mount.as_mut()) else {
                continue;
            };
            let diagnostics = match compiled {
                Ok(candidate) if self.apply => {
                    views.push((at, text, candidate));
                    continue;
                }
                Ok(_) => {
                    mount.settled = Some(source_hash(&text));
                    let event = ReloadEvent {
                        base_revision: base.0,
                        candidate_revision: next.0,
                        last_good_revision: base.0,
                        outcome: Outcome::Held,
                        stage: Stage::Transport,
                        elapsed_us: elapsed_us(started),
                        counts: CommitCounts::default(),
                        codes: Vec::new(),
                        game: None,
                    };
                    let name = Source::new(&view.path, &self.root, "").name().to_owned();
                    out.dev(&name, &self.session, &runtime_session, &self.build, &event);
                    continue;
                }
                Err(diagnostics) => diagnostics,
            };
            mount.settled = Some(source_hash(&text));
            let source = Source::new(&view.path, &self.root, &text);
            for diagnostic in &diagnostics {
                out.source(Some(&source), &[], diagnostic);
            }
            if self.apply {
                mount.failing = true;
                let _ = runtime.writer.send(HostMessage::Failure {
                    file: mount.file,
                    lines: failure_lines(source.name(), &text, &diagnostics),
                });
            }
            let event = ReloadEvent {
                base_revision: base.0,
                candidate_revision: next.0,
                last_good_revision: base.0,
                outcome: Outcome::Rejected,
                stage: rejection_stage(&diagnostics),
                elapsed_us: elapsed_us(started),
                counts: CommitCounts::default(),
                codes: diagnostics.iter().map(|d| d.code.to_owned()).collect(),
                game: None,
            };
            out.dev(
                source.name(),
                &self.session,
                &runtime_session,
                &self.build,
                &event,
            );
        }
        // The assets to send: each the app holds that changed, and each a
        // candidate names that it does not hold yet. Each is decoded here as
        // the app would: one that does not decode is reported against its
        // path and not sent, and the app keeps the last good.
        let mut named = Vec::new();
        for (at, _, candidate) in &views {
            if let Some(dir) = self.sources.views[*at]
                .mount
                .as_ref()
                .and_then(|m| m.assets)
            {
                named.extend(candidate.assets.iter().map(|asset| AssetRef {
                    dir,
                    path: asset.path.clone(),
                }));
            }
        }
        let replaces_assets = runtime.hello.capabilities.contains(Domain::Resource);
        let mut assets = Vec::new();
        for asset in self.sources.asset_candidates(&named) {
            let file = self.sources.asset(&asset);
            let hash = file.hash;
            let path = self.sources.asset_path(&asset);
            let name = Source::new(&path, &self.root, "").name().to_owned();
            let decoded = viso_svg::decode_asset(&file.bytes);
            let (outcome, stage, codes) = match decoded {
                Ok(_) if self.apply && replaces_assets => {
                    assets.push(SentAsset {
                        asset,
                        hash,
                        len: file.bytes.len(),
                    });
                    continue;
                }
                Ok(_) => {
                    if self.apply {
                        out.log(
                            "warn",
                            "tool",
                            &format!(
                                "`{name}`: the app does not replace assets, so it keeps the \
                                 one it was built with"
                            ),
                        );
                    }
                    (Outcome::Held, Stage::Transport, Vec::new())
                }
                Err(error) => {
                    out.failure(
                        ASSET_ERROR,
                        &format!("`{name}` does not decode: {error}"),
                        &["the app keeps showing its last good version".to_owned()],
                    );
                    (
                        Outcome::Rejected,
                        Stage::ResourceBuild,
                        vec![ASSET_ERROR.to_owned()],
                    )
                }
            };
            self.sources.asset_settled(&asset, hash);
            let event = ReloadEvent {
                base_revision: base.0,
                candidate_revision: next.0,
                last_good_revision: base.0,
                outcome,
                stage,
                elapsed_us: elapsed_us(started),
                counts: CommitCounts::default(),
                codes,
                game: None,
            };
            out.dev(&name, &self.session, &runtime_session, &self.build, &event);
        }
        if views.is_empty() && assets.is_empty() {
            return;
        }
        let swaps_games = runtime.hello.capabilities.contains(Domain::System);
        let mut ui = UiPatch::default();
        let mut systems = SystemPatch::default();
        let mut sent = Vec::with_capacity(views.len());
        for (at, text, candidate) in views {
            let name = Source::new(&self.sources.views[at].path, &self.root, "")
                .name()
                .to_owned();
            if let Err(reason) = self.sources.baseline(at) {
                out.log(
                    "warn",
                    "tool",
                    &format!("`{name}`: {reason}, so its mounts rebuild with fresh state"),
                );
            }
            let mount = self.sources.views[at]
                .mount
                .as_ref()
                .expect("a candidate is mounted");
            // What the build embeds: the runtime's backend's and the
            // reference interpreter's, so a shader the edit left as it was
            // packs to the bytes of the program the app runs and loads as it.
            let langs = [
                runtime.hello.shader_lang,
                viso_view::dev::wire::ShaderLang::None,
            ];
            match view_patch(mount.file, mount.last_good.as_ref(), &candidate, &langs) {
                Ok(view) => ui.views.push(view),
                Err(error) => {
                    out.failure(
                        "E3711",
                        &format!(
                            "`{name}`: internal: a shader it paints with does not build: {error}"
                        ),
                        &["the app keeps showing its last good version".to_owned()],
                    );
                    continue;
                }
            }
            let mut notes = Vec::new();
            if let Some((entry, reload)) =
                game_patch(mount.file, mount.last_good.as_ref(), &candidate, KEEP)
            {
                if let Some(note) = reload.as_ref().and_then(|r| r.diagnostic()) {
                    let source = Source::new(&self.sources.views[at].path, &self.root, &text);
                    out.source(Some(&source), &[], &note);
                    notes.push(note.code.to_owned());
                }
                if swaps_games {
                    systems.games.push(entry);
                } else {
                    out.log(
                        "warn",
                        "tool",
                        &format!(
                            "`{name}`: the app does not swap game systems, so its game keeps \
                             running the systems it started with"
                        ),
                    );
                }
            }
            sent.push(SentView {
                at,
                text,
                candidate,
                notes,
            });
        }
        let mut sections = Vec::new();
        if !ui.views.is_empty() {
            sections.push(PatchSection::Ui(ui));
        }
        if !systems.games.is_empty() {
            sections.push(PatchSection::System(systems));
        }
        if !assets.is_empty() {
            // Each asset's bytes go ahead of the patch, once per content.
            let mut blobs: Vec<u64> = Vec::with_capacity(assets.len());
            for sent in &assets {
                if blobs.contains(&sent.hash) {
                    continue;
                }
                blobs.push(sent.hash);
                let bytes = &self.sources.asset(&sent.asset).bytes;
                for chunk in blob_chunks(bytes) {
                    if runtime.writer.send(HostMessage::Blob(chunk)).is_err() {
                        return;
                    }
                }
            }
            sections.push(PatchSection::Resource(ResourcePatch {
                assets: assets
                    .iter()
                    .map(|sent| ResourceAsset {
                        path: sent.asset.path.clone(),
                        hash: sent.hash,
                        len: sent.len as u32,
                    })
                    .collect(),
            }));
        }
        let patch = PatchBundle {
            dev_session: runtime.hello.dev_session,
            target_runtime: runtime.hello.runtime_session,
            base_revision: base,
            next_revision: next,
            build_id: runtime.hello.build_id,
            sections,
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
            views: sent,
            assets,
            started,
        });
    }
}

/// What `viso game` is told when the app refused its question about the
/// game of `file`.
fn refusal_reply(
    refusal: &GameRefusal,
    file: &str,
    ask: &GameAsk,
    program: Option<&viso_dsl::behavior::Program>,
) -> ControlReply {
    let code = match refusal {
        GameRefusal::NotRunning => control::GAME_NOT_RUNNING,
        GameRefusal::UnknownState => control::GAME_UNKNOWN_STATE,
        GameRefusal::Replaying | GameRefusal::Recording => control::GAME_BUSY,
        GameRefusal::Ended => control::GAME_ENDED,
        GameRefusal::Fault(_) => control::GAME_FAULT,
        GameRefusal::TooLarge => control::GAME_ANSWER_INVALID,
    };
    let mut notes = Vec::new();
    let message = match (refusal, ask) {
        (GameRefusal::UnknownState, GameAsk::Peek { system, state }) => {
            if let Some(program) = program {
                let systems = || program.systems.iter();
                let name = |s: &viso_dsl::behavior::ir::SystemLayout| {
                    program.components[s.component as usize].name.clone()
                };
                match systems().find(|s| name(s) == *system) {
                    Some(layout) => notes.push(format!(
                        "`{system}` has {}",
                        quoted(
                            &layout
                                .states
                                .iter()
                                .map(|s| s.name.clone())
                                .collect::<Vec<_>>()
                        )
                    )),
                    None => notes.push(format!(
                        "the game's systems are {}",
                        quoted(&systems().map(name).collect::<Vec<_>>())
                    )),
                }
            }
            format!("the game of `{file}` has no state `{system}.{state}`")
        }
        (refusal, _) => format!("`{file}`: {refusal}"),
    };
    ControlReply::Refused {
        code: code.to_owned(),
        message,
        notes,
    }
}

/// `names`, each in backquotes, comma-separated.
fn quoted(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a build that gave no executable says of it.
pub(crate) fn build_failure(failure: &BuildFailure) -> String {
    match failure {
        BuildFailure::Spawn(why) => why.clone(),
        BuildFailure::Failed { status, .. } => format!("`cargo build` failed ({status})"),
        BuildFailure::Executables(executables) if executables.is_empty() => {
            "the package builds no executable".to_owned()
        }
        BuildFailure::Executables(_) => "the package builds more than one executable".to_owned(),
        BuildFailure::Cancelled => "the build was cancelled".to_owned(),
    }
}

fn elapsed_us(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use viso_dsl::aot::emit_view_package;
    use viso_project::{Layout, LockKind, Target};
    use viso_view::dev::wire::{
        BLOB_CHUNK, BuildId, DevSessionId, Domains, FileCommit, FileId, MountEntry, PatchAck,
        PatchNack, PatchTimings, ProjectFingerprint, ReloadPlan, RuntimeTarget, SchemaFingerprint,
        ViewPatch,
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
            Fixture::with(name, true, VIEW)
        }

        /// A session, applying candidates or not, whose app runs `built`
        /// while `view.vs` reads `VIEW`.
        fn with(name: &str, apply: bool, built: &str) -> Self {
            Fixture::app(name, apply, built, VIEW, Domains::NONE)
        }

        /// A session whose app, applying `capabilities`, runs `built` while
        /// `view.vs` reads `text`.
        fn app(name: &str, apply: bool, built: &str, text: &str, capabilities: Domains) -> Self {
            Fixture::app_with(name, apply, built, text, capabilities, &[])
        }

        /// [`app`](Self::app) with the package's `assets`: each one's path,
        /// bytes as the build read them, and whether the mount embeds it.
        fn app_with(
            name: &str,
            apply: bool,
            built: &str,
            text: &str,
            capabilities: Domains,
            assets: &[(&str, &[u8], bool)],
        ) -> Self {
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
            let scope = WatchScope {
                project: root.clone(),
                rust: RustScope::default(),
            };
            let mut dev = DevSession::start(scope, &expect, lock, apply, |_| true);
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
                capabilities,
                target: RuntimeTarget::DesktopHost,
                shader_lang: viso_view::dev::wire::ShaderLang::Msl,
            };
            dev.link(LinkEvent::Connected(Connection { hello, writer }), &mut out);
            let path = root.join("view.vs");
            let mut fixture = Fixture {
                dev,
                out,
                sent,
                path,
            };
            fixture.save(text);
            for (path, bytes, _) in assets {
                fixture.save_asset(path, bytes);
            }
            let held = assets
                .iter()
                .filter(|(_, _, held)| *held)
                .map(|(path, _, _)| ResourceId::of(path))
                .collect();
            let asset_dir = root.join("assets").display().to_string();
            fixture.runtime(RuntimeMessage::Mounts(vec![MountEntry {
                file: FileId(0),
                path: fixture.path.display().to_string(),
                package: "app".into(),
                module: vec!["view".into()],
                language: None,
                catalog: None,
                capabilities: Vec::new(),
                source_hash: source_hash(built),
                assets: Some((asset_dir, held)),
            }]));
            fixture
        }

        fn save(&mut self, text: &str) {
            let change = Change {
                path: self.path.clone(),
                bytes: text.into(),
                rust: false,
            };
            self.dev.change(change, &mut self.out);
        }

        /// Saves `bytes` as asset `path`.
        fn save_asset(&mut self, path: &str, bytes: &[u8]) {
            let root = self.path.parent().expect("in the root").to_owned();
            let change = Change {
                path: root.join("assets").join(path),
                bytes: bytes.to_vec(),
                rust: false,
            };
            self.dev.change(change, &mut self.out);
        }

        /// The launch connected.
        fn launch(&self) -> RuntimeSessionId {
            self.dev
                .runtime
                .as_ref()
                .expect("a runtime connected")
                .hello
                .runtime_session
        }

        fn runtime(&mut self, message: RuntimeMessage) {
            let launch = self.launch();
            self.dev
                .link(LinkEvent::Runtime(launch, message), &mut self.out);
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
                state_resets: Vec::new(),
                games: Vec::new(),
                resources: Vec::new(),
                timings: PatchTimings::default(),
            }));
        }
    }

    /// `text` compiled as the build compiles `view.vs`.
    fn compiled(text: &str) -> CandidatePlan {
        let origin = viso_dsl::frontend::Origin {
            package: "app".into(),
            module: vec!["view".into()],
            language: None,
        };
        viso_dsl::hotreload::plan_view_for(text, &origin, Default::default()).unwrap()
    }

    /// The revisions of `message`, a patch, and what it sets file 0 to.
    fn patch(message: &HostMessage) -> (u64, u64, &ViewPatch) {
        let HostMessage::Patch(patch) = message else {
            panic!("not a patch: {message:?}");
        };
        let PatchSection::Ui(ui) = &patch.sections[0] else {
            panic!("a ui section")
        };
        assert_eq!(ui.views[0].file, FileId(0));
        (patch.base_revision.0, patch.next_revision.0, &ui.views[0])
    }

    /// Whether `message` is the patch from `base` to `next` moving file 0
    /// from `last_good` to `text`.
    fn patches(message: &HostMessage, (base, next): (u64, u64), last_good: &str, text: &str) {
        let (b, n, view) = patch(message);
        assert_eq!((b, n), (base, next));
        assert_eq!(
            *view,
            view_patch(
                FileId(0),
                Some(&compiled(last_good)),
                &compiled(text),
                &[viso_view::dev::wire::ShaderLang::Msl],
            )
            .unwrap()
        );
    }

    const DAY: &[u8] = include_bytes!("../../../../examples/images/assets/photos/day.png");
    const NIGHT: &[u8] = include_bytes!("../../../../examples/images/assets/photos/night.jpg");
    const STAR: &[u8] = include_bytes!("../../../../examples/images/assets/icons/star.svg");

    /// The blobs of `sent`, reassembled, and the resource section of its
    /// patch, which ends it.
    fn resources(sent: &[HostMessage]) -> (Vec<Vec<u8>>, &ResourcePatch) {
        let Some((HostMessage::Patch(patch), blobs)) = sent.split_last() else {
            panic!("no patch last: {sent:?}");
        };
        let Some(PatchSection::Resource(resources)) = patch.sections.last() else {
            panic!("no resource section: {:?}", patch.sections);
        };
        let mut whole: Vec<Vec<u8>> = Vec::new();
        for blob in blobs {
            let HostMessage::Blob(chunk) = blob else {
                panic!("not a blob: {blob:?}");
            };
            if chunk.offset == 0 {
                whole.push(Vec::new());
            }
            whole.last_mut().unwrap().extend_from_slice(&chunk.bytes);
        }
        (whole, resources)
    }

    fn acked_assets(f: &mut Fixture, revision: u64, paths: &[&str]) {
        f.runtime(RuntimeMessage::Ack(PatchAck {
            revision: Revision(revision),
            applied_domains: Domains::NONE.with(Domain::Resource),
            files: Vec::new(),
            notices: Vec::new(),
            state_resets: Vec::new(),
            games: Vec::new(),
            resources: paths
                .iter()
                .map(|path| viso_view::dev::wire::ResourceCommit {
                    id: ResourceId::of(path),
                    nodes: 1,
                    dirty: Default::default(),
                })
                .collect(),
            timings: PatchTimings::default(),
        }));
    }

    fn replaces_assets() -> Domains {
        Domains::NONE.with(Domain::Ui).with(Domain::Resource)
    }

    #[test]
    fn a_changed_asset_the_app_holds_goes_as_blobs_ahead_of_its_patch() {
        let mut f = Fixture::app_with(
            "asset-edit",
            true,
            VIEW,
            VIEW,
            replaces_assets(),
            &[("photos/day.png", DAY, true), ("unused.png", DAY, false)],
        );
        assert!(f.settle().is_empty(), "the app runs what the build read");

        f.save_asset("unused.png", NIGHT);
        assert!(f.settle().is_empty(), "the app does not hold it");

        f.save_asset("photos/day.png", NIGHT);
        let sent = f.settle();
        let (blobs, patch) = resources(&sent);
        assert_eq!(blobs, [NIGHT]);
        assert_eq!(
            patch.assets,
            [ResourceAsset {
                path: "photos/day.png".into(),
                hash: viso_view::dev::wire::content_hash(NIGHT),
                len: NIGHT.len() as u32,
            }]
        );
        let HostMessage::Patch(bundle) = sent.last().unwrap() else {
            unreachable!()
        };
        assert_eq!(bundle.sections.len(), 1, "no view changed");
        acked_assets(&mut f, 2, &["photos/day.png"]);
        assert!(f.settle().is_empty());

        // Back to what it was: the app holds the edit now, so that goes too.
        f.save_asset("photos/day.png", DAY);
        let (blobs, _) = resources(&f.settle());
        assert_eq!(blobs, [DAY]);
    }

    #[test]
    fn an_asset_that_does_not_decode_is_reported_and_kept_from_the_app() {
        let mut f = Fixture::app_with(
            "asset-corrupt",
            true,
            VIEW,
            VIEW,
            replaces_assets(),
            &[("logo.png", DAY, true)],
        );
        let errors = f.out.errors();
        f.save_asset("logo.png", &DAY[..DAY.len() / 2]);
        assert!(f.settle().is_empty(), "nothing is sent");
        assert!(f.out.errors() > errors, "the failure is reported");
        assert!(f.settle().is_empty(), "nor retried");

        f.save_asset("logo.png", NIGHT);
        let (blobs, _) = resources(&f.settle());
        assert_eq!(blobs, [NIGHT], "the next good one goes");
    }

    #[test]
    fn a_view_naming_an_asset_the_app_lacks_carries_it_and_one_appearing_rechecks_it() {
        let mut f = Fixture::app_with(
            "asset-named",
            true,
            VIEW,
            VIEW,
            replaces_assets(),
            &[("icons/star.svg", STAR, false)],
        );
        let named = VIEW.replace(
            "Text { width: 120dp; }",
            "Svg { source: asset(\"icons/star.svg\"); }",
        );
        f.save(&named);
        let sent = f.settle();
        let (blobs, patch) = resources(&sent);
        assert_eq!(blobs, [STAR]);
        assert_eq!(patch.assets[0].path, "icons/star.svg");
        let HostMessage::Patch(bundle) = sent.last().unwrap() else {
            unreachable!()
        };
        assert!(matches!(bundle.sections[0], PatchSection::Ui(_)));
        acked_assets(&mut f, 2, &["icons/star.svg"]);

        // A view naming an asset not there yet is rejected, and compiles once
        // it appears.
        let late = named.replace("icons/star.svg", "late.svg");
        f.save(&late);
        let sent = f.settle();
        assert!(
            matches!(&sent[..], [HostMessage::Failure { .. }]),
            "{sent:?}"
        );
        f.save_asset("late.svg", STAR);
        let sent = f.settle();
        let (blobs, patch) = resources(&sent);
        assert_eq!(blobs, [STAR]);
        assert_eq!(patch.assets[0].path, "late.svg");
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
        patches(&sent[0], (1, 2), VIEW, &edited);
        assert!(patch(&sent[0]).2.plan.preserving);
    }

    #[test]
    fn one_patch_is_in_flight_and_later_edits_go_after_its_answer() {
        let mut f = Fixture::new("in-flight");
        let first = VIEW.replace("120dp", "140dp");
        f.save(&first);
        let sent = f.settle();
        patches(&sent[0], (1, 2), VIEW, &first);

        let second = VIEW.replace("120dp", "160dp");
        f.save(&second);
        assert!(f.settle().is_empty(), "waits for the answer");
        f.ack(2);
        let sent = f.settle();
        patches(&sent[0], (2, 3), &first, &second);
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
        patches(&sent[0], (1, 3), VIEW, &good);
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
        patches(&sent[0], (1, 3), VIEW, &next);
    }

    #[test]
    fn a_mount_whose_build_the_session_did_not_read_is_rebuilt_fresh() {
        let mut f = Fixture::with("unread", true, "component Old { view { Text {} } }\n");
        let sent = f.settle();
        let (_, _, view) = patch(&sent[0]);
        assert_eq!(view.package, emit_view_package(&compiled(VIEW)));
        assert_eq!(view.plan, ReloadPlan::fresh(&view.package));
    }

    #[test]
    fn without_hot_reload_a_candidate_is_compiled_and_held() {
        let mut f = Fixture::with("held", false, VIEW);
        let errors = f.out.errors();
        f.save(&VIEW.replace("120dp", "125dp"));
        assert!(f.settle().is_empty(), "nothing is sent");
        f.save(&VIEW.replace("state count = 0;", "state count = ;"));
        assert!(f.settle().is_empty(), "not even a failure");
        assert!(f.out.errors() > errors, "the diagnostics are reported");
    }

    const GAME: &str = "import viso::game::{FixedUpdate, FixedFrame, InputAction};\n\
        system Runner implements FixedUpdate {\n    state steps = 0;\n    \
        action fixed_update(frame: FixedFrame) { steps += 1; }\n}\n\
        export component Arena { view { Column { width: 200dp; } } }\n";

    /// The `system` section of `message`, a patch, if it has one.
    fn systems(message: &HostMessage) -> Option<&SystemPatch> {
        let HostMessage::Patch(patch) = message else {
            panic!("not a patch: {message:?}");
        };
        patch.sections.iter().find_map(|section| match section {
            PatchSection::System(systems) => Some(systems),
            PatchSection::Ui(_) | PatchSection::Resource(_) => None,
        })
    }

    /// Asks the session `ask` about `game` as `viso game` does: what it sent
    /// the app, and where its reply arrives.
    fn ask(
        f: &mut Fixture,
        game: &str,
        ask: GameAsk,
    ) -> (Vec<HostMessage>, Receiver<ControlReply>) {
        let (reply, answer) = mpsc::channel();
        let call = ControlCall {
            game: game.into(),
            ask,
            reply,
        };
        f.dev.control(call, &mut f.out);
        (f.sent.try_iter().collect(), answer)
    }

    fn peek(system: &str, state: &str) -> GameAsk {
        GameAsk::Peek {
            system: system.into(),
            state: state.into(),
        }
    }

    #[test]
    fn a_game_question_goes_to_the_app_and_its_answer_comes_back_typed() {
        let swaps = Domains::NONE.with(Domain::Ui).with(Domain::System);
        let mut f = Fixture::app("game-ask", true, GAME, GAME, swaps);
        let _ = f.settle();

        let (sent, answer) = ask(&mut f, "view", peek("Runner", "steps"));
        let [HostMessage::Game(query)] = &sent[..] else {
            panic!("not asked: {sent:?}");
        };
        assert_eq!(
            (query.file, &query.ask),
            (FileId(0), &peek("Runner", "steps"))
        );
        f.runtime(RuntimeMessage::Game(GameAnswer {
            id: query.id,
            // `Int(4)`: its tag, then the zig-zag varint.
            reply: Ok(GameReply::State {
                value: vec![1, 8],
                tick: 4,
                sessions: 1,
            }),
        }));
        assert_eq!(
            answer.try_recv(),
            Ok(ControlReply::State {
                file: "view.vs".into(),
                json: "4".into(),
                tick: 4,
                sessions: 1,
            })
        );

        // A state the game lacks is refused with the ones it has.
        let (sent, answer) = ask(&mut f, "view.vs", peek("Runner", "speed"));
        let [HostMessage::Game(query)] = &sent[..] else {
            panic!("not asked: {sent:?}");
        };
        f.runtime(RuntimeMessage::Game(GameAnswer {
            id: query.id,
            reply: Err(GameRefusal::UnknownState),
        }));
        let Ok(ControlReply::Refused { code, notes, .. }) = answer.try_recv() else {
            panic!("not refused");
        };
        assert_eq!(
            (&*code, &notes[..]),
            (
                control::GAME_UNKNOWN_STATE,
                &["`Runner` has `steps`".to_owned()][..]
            )
        );

        // A recording's tape comes back as the app wrote it.
        let record = GameAsk::Record {
            span: viso_view::dev::wire::RecordSpan::Ticks(60),
        };
        let (sent, answer) = ask(&mut f, "./view", record);
        let [HostMessage::Game(query)] = &sent[..] else {
            panic!("not asked: {sent:?}");
        };
        f.runtime(RuntimeMessage::Game(GameAnswer {
            id: query.id,
            reply: Ok(GameReply::Tape { tape: vec![7; 9] }),
        }));
        assert_eq!(
            answer.try_recv(),
            Ok(ControlReply::Tape {
                file: "view.vs".into(),
                tape: vec![7; 9],
            })
        );

        // A file the app does not mount is refused here, naming what it does.
        let (sent, answer) = ask(&mut f, "level", peek("Runner", "steps"));
        assert!(sent.is_empty());
        let Ok(ControlReply::Refused { code, notes, .. }) = answer.try_recv() else {
            panic!("not refused");
        };
        assert_eq!(
            (&*code, &notes[..]),
            (
                control::GAME_NOT_RUNNING,
                &["the app mounts `view.vs`".to_owned()][..]
            )
        );

        // The app exits with a question open: it is refused, not left hanging.
        let (_, answer) = ask(&mut f, "view", peek("Runner", "steps"));
        let launch = f.launch();
        f.dev.link(LinkEvent::Closed(launch), &mut f.out);
        assert!(matches!(
            answer.try_recv(),
            Ok(ControlReply::Refused { code, .. }) if code == control::GAME_NOT_RUNNING
        ));
    }

    #[test]
    fn an_app_that_runs_no_games_is_not_asked() {
        let mut f = Fixture::app(
            "game-none",
            true,
            GAME,
            GAME,
            Domains::NONE.with(Domain::Ui),
        );
        let (sent, answer) = ask(&mut f, "view", peek("Runner", "steps"));
        assert!(sent.is_empty());
        assert!(matches!(
            answer.try_recv(),
            Ok(ControlReply::Refused { code, .. }) if code == control::GAME_NOT_RUNNING
        ));
    }

    #[test]
    fn a_game_edit_swaps_the_game_beside_its_view_at_the_classified_tier() {
        let swaps = Domains::NONE.with(Domain::Ui).with(Domain::System);
        let mut f = Fixture::app("game", true, GAME, GAME, swaps);
        // Simulation code: a Logic swap, reported as the tier note.
        let faster = GAME.replace("steps += 1;", "steps += 2;");
        f.save(&faster);
        let sent = f.settle();
        patches(&sent[0], (1, 2), GAME, &faster);
        assert_eq!(
            systems(&sent[0]).expect("a system section").games,
            [viso_view::dev::wire::GamePatch {
                file: FileId(0),
                tier: viso_view::dev::wire::GameTier::Logic,
            }]
        );
        f.ack(2);

        // A view-only edit leaves the game alone.
        let wider = faster.replace("200dp", "240dp");
        f.save(&wider);
        let sent = f.settle();
        patches(&sent[0], (2, 3), &faster, &wider);
        assert_eq!(systems(&sent[0]), None);
        f.ack(3);

        // A new Simulation state migrates.
        let jumps = wider.replace("state steps = 0;", "state steps = 0;\n    state jumps = 0;");
        f.save(&jumps);
        let sent = f.settle();
        assert_eq!(
            systems(&sent[0]).expect("a system section").games[0].tier,
            viso_view::dev::wire::GameTier::LogicMigration
        );
    }

    #[test]
    fn a_game_the_session_cannot_classify_is_rebuilt_keeping_its_characters() {
        let swaps = Domains::NONE.with(Domain::Ui).with(Domain::System);
        let unread = GAME.replace("200dp", "210dp");
        let f = &mut Fixture::app("game-unread", true, &unread, GAME, swaps);
        let sent = f.settle();
        assert_eq!(
            systems(&sent[0]).expect("a system section").games[0].tier,
            viso_view::dev::wire::GameTier::WorldRebuild {
                keep: Rebuild::KeepCharacters
            }
        );
    }

    #[test]
    fn an_app_that_does_not_swap_games_gets_the_view_alone() {
        let mut f = Fixture::app("game-ui-only", true, GAME, GAME, Domains::NONE);
        f.save(&GAME.replace("steps += 1;", "steps += 3;"));
        let sent = f.settle();
        assert_eq!(systems(&sent[0]), None);
    }

    #[test]
    fn a_snapshot_is_held_and_restored_with_plans_into_the_app_launched_after_it() {
        let mut f = Fixture::new("restore");
        let _ = f.settle();
        let built = Build {
            result: Ok(PathBuf::from("app")),
            elapsed: Duration::from_millis(1),
            files: Vec::new(),
            unchanged: false,
        };
        assert!(f.dev.rebuilt(&built, &mut f.out), "the app restarts on it");
        assert!(f.dev.request_snapshot());
        assert!(matches!(f.sent.try_recv(), Ok(HostMessage::Snapshot)));
        assert!(!f.dev.has_snapshot());
        let bytes: Vec<u8> = (0..BLOB_CHUNK + 9).map(|i| i as u8).collect();
        for chunk in blob_chunks(&bytes) {
            f.runtime(RuntimeMessage::Blob(chunk));
        }
        let window = WindowGeometry {
            size: (640.0, 480.0),
            position: Some((90.0, 40.0)),
        };
        f.runtime(RuntimeMessage::Snapshot {
            blob: BlobRef::of(&bytes),
            windows: vec![window],
            capture_us: 800,
        });
        assert!(f.dev.has_snapshot());
        assert_eq!(f.dev.launch_window(), Some(window));

        // The old app closes; the new build's connects, launched to restore.
        let old = f.launch();
        f.dev.link(LinkEvent::Closed(old), &mut f.out);
        assert!(f.dev.restore_next());
        let (writer, sent) = mpsc::channel();
        f.sent = sent;
        let hello = RuntimeHello {
            protocol_version: viso_view::dev::wire::DEV_PROTOCOL_VERSION,
            token: "t".into(),
            dev_session: DevSessionId(1),
            runtime_session: RuntimeSessionId(10),
            build_id: BuildId(2),
            current_revision: Revision::LAUNCH,
            schema_fingerprint: SchemaFingerprint(4),
            capabilities: Domains::NONE,
            target: RuntimeTarget::DesktopHost,
            shader_lang: viso_view::dev::wire::ShaderLang::Msl,
        };
        f.dev.link(
            LinkEvent::Connected(Connection { hello, writer }),
            &mut f.out,
        );
        // What the old app said last is not the new one's.
        f.dev.link(
            LinkEvent::Runtime(old, RuntimeMessage::Mounts(Vec::new())),
            &mut f.out,
        );
        f.dev.link(LinkEvent::Closed(old), &mut f.out);
        assert_eq!(f.launch(), RuntimeSessionId(10));

        f.runtime(RuntimeMessage::Mounts(vec![MountEntry {
            file: FileId(0),
            path: f.path.display().to_string(),
            package: "app".into(),
            module: vec!["view".into()],
            language: None,
            catalog: None,
            capabilities: Vec::new(),
            source_hash: source_hash(VIEW),
            assets: None,
        }]));
        let mut blobs = Blobs::default();
        let mut restore = None;
        for message in f.sent.try_iter() {
            match message {
                HostMessage::Blob(chunk) => blobs.add(chunk),
                HostMessage::Restore(bundle) => restore = Some(bundle),
                other => panic!("{other:?}"),
            }
        }
        let restore = restore.expect("the restore");
        assert_eq!(restore.target_runtime, RuntimeSessionId(10));
        assert_eq!(blobs.take(restore.snapshot).as_deref(), Some(&bytes[..]));
        let [view] = &restore.ui.views[..] else {
            panic!("{:?}", restore.ui);
        };
        assert_eq!(view.file, FileId(0));
        assert!(!view.plan.states.is_empty(), "`count` is planned");

        f.runtime(RuntimeMessage::Restored(RestoreReport {
            files: vec![FileCommit {
                file: FileId(0),
                counts: CommitCounts {
                    mounts: 1,
                    migrated: 1,
                    ..CommitCounts::default()
                },
            }],
            ..RestoreReport::default()
        }));
        assert!(f.dev.restarting.is_none(), "the restart landed");
        assert!(!f.dev.restore_next, "one restore per launch");
    }

    #[test]
    fn a_closed_runtime_is_forgotten() {
        let mut f = Fixture::new("closed");
        let launch = f.launch();
        f.dev.link(LinkEvent::Closed(launch), &mut f.out);
        f.save(&VIEW.replace("120dp", "190dp"));
        assert!(f.settle().is_empty());
    }
}
