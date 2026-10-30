//! The dev session service of `viso run`: for the
//! lifetime of the command it owns the session's identity and lock, the
//! project watcher, the source graph and its compiler, and the connections to
//! the running apps, and turns each settled batch of edits into one candidate
//! revision.
//!
//! One source graph serves every runtime connected: the parse, the module
//! graph and the type check of an edit are shared, and runtimes that mount a
//! file alike — the same grants, catalogs and assets — share its compile;
//! the plan, the shader packing for its GPU backend, the assets it can take
//! and the answer are each runtime's own. Each runtime has its own revision
//! and its own patch in flight, so a target that refuses an edit stays on its
//! last good while the others advance, and each one's status — revisions,
//! connection, transport, the latest patch, the Rust edits — is reported as a
//! `runtime` event whenever it changes. The first runtime connected is the
//! app the session launched, which a Rust edit restarts.
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
//! against the compiled view its mounts run: the patch carries the
//! candidate in its release form and the typed plan that moves the mounts
//! onto it, so the app applies it with no compiler present. A view whose
//! module declares systems is also classified against the game its mounts
//! run: the tier is reported as an `E5103` note and sent as the
//! file's `system` entry beside its view, so the game swaps with the view in
//! one revision. The views that compile are sent as one patch, and the
//! runtime's ACK or NACK closes the candidate; an ACK makes each candidate the
//! last-good its next edit is planned against. One patch is in flight to a
//! runtime at a time: what is saved meanwhile waits for its answer and goes
//! to it as the next batch.
//!
//! With `--no-hot-reload` the session still watches, compiles and
//! reports each candidate, but sends the app nothing: a candidate that
//! compiles is `held`.
//!
//! Across a Rust warm restart the session asks the old app for its
//! snapshot and keeps it in memory, never written, printed or logged,
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

pub mod build;
pub mod cargo;
pub mod control;
mod kept;
pub mod link;
mod profile;
pub mod report;
mod sources;
pub mod watch;

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{BuildHasher, Hasher as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use viso_dev_protocol::headless::Reported;
use viso_dev_protocol::inspect::{InspectAsk, SourceTable};
use viso_dev_protocol::wire::{
    BlobRef, Blobs, BuildId, CommitCounts, Domain, FileId, GameAnswer, GameAsk, GameQuery,
    GameRefusal, GameReply, HostMessage, MAX_FRAME, MAX_REDACTED, PatchAck, PatchBundle, PatchNack,
    PatchSection, Rebuild, Redaction, ResourceAsset, ResourcePatch, RestoreBundle, RestoreReport,
    Revision, RuntimeDiagnostic, RuntimeHello, RuntimeLabel, RuntimeMessage, RuntimeSessionId,
    RuntimeTarget, ShaderLang, SourceAt, Stage, SystemPatch, UiPatch, WindowGeometry, blob_chunks,
    source_hash,
};
use viso_dsl::Diagnostic;
use viso_dsl::assets::{ASSET_ERROR, ResourceId};
use viso_dsl::behavior::probe::state_json;
use viso_dsl::hotreload::{CandidatePlan, game_patch, view_patch};
use viso_project::LockGuard;
use viso_text::font_format;

pub use control::{Control, ControlAsk, ControlCall, ControlDeliver, ControlReply};
pub use link::{Deliver, Expect, LinkEvent, Traffic};
pub use watch::Change;

use crate::build::cargo::BuildFailure;
use crate::report::{DevIds, Reporter, Source};
use build::Build;
use cargo::RustScope;
use report::{
    Outcome, PatchStatus, RebuildEvent, ReloadEvent, RuntimeStatus, RustState, intern_code,
    rejection_stage,
};
use sources::{AssetRef, Changed, Memo, Mounts, Sources, failure_lines};
pub use watch::WatchScope;
use watch::Watcher;

/// How long the session waits after the latest change of a batch before it
/// takes the batch: five times the widest arrival spread measured for
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

/// A running app connected to the session.
struct Runtime {
    hello: RuntimeHello,
    /// Its `RuntimeSessionId` as the events spell it.
    id: String,
    /// The build it runs, as the events spell it.
    build: String,
    writer: Sender<HostMessage>,
    /// What its connection carried.
    traffic: Arc<Traffic>,
    /// The revision it matches.
    revision: Revision,
    in_flight: Option<InFlight>,
    /// The questions of `viso game` it was sent and has not answered.
    asked: Vec<Asked>,
    /// The questions of `viso inspect` it was sent and has not answered.
    inspecting: Vec<(u32, Sender<ControlReply>)>,
    /// The scenarios it was sent to play, with the report lines it sent of
    /// each so far.
    playing: Vec<(u32, Sender<ControlReply>, Vec<String>)>,
    /// Scenarios asked for before it reported what it mounts, which its
    /// selectors resolve against: sent once it does.
    held_scenarios: Vec<(String, Sender<ControlReply>)>,
    /// The capture of its frames under way, and those it was asked to end
    /// whose last frames are still to come.
    capture: Option<profile::Capture>,
    ended: Vec<profile::Capture>,
    /// Whether it reported what it mounts.
    mounted: bool,
    /// The chunks of the snapshot it is sending.
    blobs: Blobs,
    /// What it runs of the source graph.
    mounts: Mounts,
    /// How the latest patch it answered went.
    patch: Option<PatchStatus>,
}

/// The snapshot of an app, kept for the app launched after it.
/// When the phases of the restart under way ended.
#[derive(Debug, Default)]
struct RestartClock {
    /// The new executable's prewarm run started.
    prewarm: Option<Instant>,
    /// The old app was asked for its snapshot.
    asked: Option<Instant>,
    /// Its snapshot arrived.
    held: Option<Instant>,
    /// The new app launched.
    launched: Option<Instant>,
    /// The new app reported its mounts.
    mounted: Option<Instant>,
}

/// The microseconds from `from` to `to`.
fn micros(from: Instant, to: Instant) -> u64 {
    u64::try_from(to.saturating_duration_since(from).as_micros()).unwrap_or(u64::MAX)
}

pub struct HeldSnapshot {
    bytes: Arc<[u8]>,
    /// The view each file of the app ran: what the restore plans from.
    views: Vec<HeldView>,
}

/// A view an app ran, as a held snapshot keeps it.
struct HeldView {
    path: PathBuf,
    /// The text of it the app ran, when the session read it.
    text: Option<String>,
    /// Its compile; for state a session before kept, compiled as the app
    /// launched to restore it mounts the file.
    plan: Option<CandidatePlan>,
}

/// What `--keep-state` keeps across sessions.
struct Keep {
    /// The dev cache the kept state is under.
    dev: PathBuf,
    /// The runtimes asked for their state as the session stops, with the
    /// target each is kept for.
    asked: Vec<(RuntimeSessionId, BuildId, String)>,
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
        for (_, reply) in self.inspecting.drain(..) {
            let _ = reply.send(ControlReply::refused(
                control::ENV_NO_DEV_SESSION,
                "the app exited before it answered",
            ));
        }
        let held = self.held_scenarios.drain(..).map(|(_, reply)| reply);
        for reply in self
            .playing
            .drain(..)
            .map(|(_, reply, _)| reply)
            .chain(held)
        {
            let _ = reply.send(ControlReply::refused(
                control::ENV_NO_DEV_SESSION,
                "the app exited before it played the scenario",
            ));
        }
    }
}

/// A runtime's diagnostic as the session reports it.
enum Placed {
    /// A compiler diagnostic in the source of a view, by index.
    Source(usize, Diagnostic),
    /// One the session could place in no source it holds.
    Bare {
        code: String,
        message: String,
        notes: Vec<String>,
    },
}

/// A question of `viso game` relayed to a runtime.
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
    /// When it was sent, and its compile and plan before.
    sent: Instant,
    compile_us: u64,
    /// What the connection had carried to the runtime before it.
    bytes_before: u64,
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

/// What one pass over the runtimes shares: the revision its candidates take,
/// the compiles and asset decodes of runtimes alike, and what it reported.
#[derive(Default)]
struct Pass {
    next: Option<Revision>,
    memo: Memo,
    decoded: BTreeMap<(AssetRef, u64), Result<(), String>>,
    /// The views and assets, at their content, whose errors or notes were
    /// reported.
    reported: BTreeSet<(usize, u64)>,
    reported_notes: BTreeSet<(usize, u64)>,
    reported_assets: BTreeSet<(AssetRef, u64)>,
}

/// The dev session.
pub struct DevSession {
    root: PathBuf,
    /// The session id, and the first lane's build id, as the `dev` events
    /// spell them.
    session: String,
    build: String,
    /// The first lane's build, whose app takes what names no runtime.
    lead: Option<BuildId>,
    /// The dev lock of each lane.
    locks: Vec<LockGuard>,
    _watcher: Watcher,
    sources: Sources,
    /// The hash of `Viso.toml` as the build read it.
    manifest: Option<u64>,
    /// The runtimes connected, in the order they connected; a Rust edit
    /// restarts the app a lane launched, the first runtime of its build.
    runtimes: Vec<Runtime>,
    batch: Option<Batch>,
    /// Whether a batch is to be taken now: a runtime reported its mounts
    /// or answered a patch.
    ready: bool,
    /// The revision the next candidate takes.
    next_revision: u64,
    /// Whether candidates are sent to the app; not with `--no-hot-reload`.
    apply: bool,
    /// The id the next question of `viso game` takes.
    next_query: u32,
    /// Where the Rust edits are.
    rust: RustState,
    /// The latest snapshot the app sent.
    held: Option<HeldSnapshot>,
    /// The runtime asked for the snapshot, and the build of the lane the
    /// restart relaunches.
    snapshot_from: Option<RuntimeSessionId>,
    restart_build: Option<BuildId>,
    /// Whether the next app of that target to report its mounts was launched
    /// to restore it.
    restore_next: bool,
    /// The runtime the snapshot was sent to.
    restoring: Option<RuntimeSessionId>,
    /// Whether the app launched to restore has yet to: from its launch until
    /// its restore lands, or it is refused, closes or exits first.
    landing: bool,
    /// The event of the Rust build the app is restarting on, reported once
    /// the restart lands.
    restarting: Option<RebuildEvent>,
    /// Where the app's windows were when it sent its snapshot.
    windows: Vec<WindowGeometry>,
    clock: RestartClock,
    /// Whether each runtime's frames are streamed, `--profile-frame`.
    profile_frames: bool,
    /// What `--keep-state` keeps; `None` without it.
    keep: Option<Keep>,
}

impl DevSession {
    /// Starts the session watching `scope`, holding each lane's lock in
    /// `locks`; the watcher hands each change to `deliver`. `apply` is
    /// whether candidates are sent to the apps.
    pub fn start(
        scope: WatchScope,
        expect: &Expect,
        locks: Vec<LockGuard>,
        apply: bool,
        deliver: impl FnMut(Change) -> bool + Send + 'static,
    ) -> Self {
        let root = scope.project.clone();
        let watcher = Watcher::spawn(scope, deliver);
        DevSession {
            root,
            session: expect.dev_session.to_hex(),
            build: expect
                .builds
                .first()
                .map_or_else(String::new, |build| build.to_hex()),
            lead: expect.builds.first().copied(),
            locks,
            _watcher: watcher,
            sources: Sources::default(),
            manifest: None,
            runtimes: Vec::new(),
            batch: None,
            ready: false,
            next_revision: Revision::LAUNCH.0 + 1,
            apply,
            next_query: 0,
            rust: RustState::Idle,
            held: None,
            snapshot_from: None,
            restart_build: None,
            restore_next: false,
            restoring: None,
            landing: false,
            restarting: None,
            windows: Vec::new(),
            clock: RestartClock::default(),
            profile_frames: false,
            keep: None,
        }
    }

    /// Keeps the app's state across sessions under the dev cache `dev`:
    /// `viso run --keep-state`.
    pub fn keep_state(&mut self, dev: PathBuf) {
        self.keep = Some(Keep {
            dev,
            asked: Vec::new(),
        });
    }

    /// Restores the state a session before kept for `target`, removing it,
    /// into the app the lane of build `lane` launches first; whether there
    /// is any to restore.
    pub fn restore_kept(&mut self, lane: BuildId, target: &str, out: &mut dyn Reporter) -> bool {
        let Some(keep) = &self.keep else {
            return false;
        };
        let kept = match kept::Kept::take(&kept::path(&keep.dev, target)) {
            None => return false,
            Some(Ok(kept)) => kept,
            Some(Err(why)) => {
                out.log(
                    "warn",
                    "tool",
                    &format!("the state the last session kept is not restored: {why}"),
                );
                return false;
            }
        };
        out.log(
            "info",
            "tool",
            &format!(
                "restoring the state the last session kept ({} bytes)",
                kept.snapshot.len()
            ),
        );
        self.held = Some(HeldSnapshot {
            bytes: kept.snapshot.into(),
            views: kept
                .views
                .into_iter()
                .map(|(path, text)| HeldView {
                    path,
                    text: Some(text),
                    plan: None,
                })
                .collect(),
        });
        self.windows = kept.windows;
        self.restart_build = Some(lane);
        self.restore_next = true;
        self.landing = true;
        true
    }

    /// Asks the app the lane of build `lane` launched for its state, to keep
    /// for `target` as the session stops: every `@sensitive` state and
    /// secure field's text left out, and every state of a view whose
    /// version the session does not know. Whether it was asked; the session
    /// is to wait for [`kept`](Self::kept) before closing it.
    pub fn keep_on_stop(&mut self, lane: BuildId, target: &str) -> bool {
        let Some(keep) = &mut self.keep else {
            return false;
        };
        let Some(app) = self.runtimes.iter().find(|r| r.hello.build_id == lane) else {
            return false;
        };
        let redactions = app
            .mounts
            .views
            .values()
            .filter_map(|mount| {
                let plan = mount.last_good.as_ref()?;
                let mut states: Vec<_> = plan
                    .sensitive
                    .iter()
                    .map(|symbol| viso_schema::StateKey::from_parts(symbol.hi, symbol.lo))
                    .collect();
                let mut nodes = plan.sensitive_nodes();
                states.truncate(MAX_REDACTED);
                nodes.truncate(MAX_REDACTED);
                Some(Redaction {
                    file: mount.file,
                    states,
                    nodes,
                })
            })
            .collect();
        if app
            .writer
            .send(HostMessage::Snapshot(Some(redactions)))
            .is_err()
        {
            return false;
        }
        let asked = (app.hello.runtime_session, lane, target.to_owned());
        keep.asked.push(asked);
        true
    }

    /// Whether the state the app of the lane of build `lane` was asked for
    /// as the session stops is kept, or no longer awaited.
    pub fn kept(&self, lane: BuildId) -> bool {
        self.keep
            .as_ref()
            .is_none_or(|keep| !keep.asked.iter().any(|&(_, build, _)| build == lane))
    }

    /// Writes the state runtime `at` sent as the session stops, for the
    /// target it was asked for.
    fn keep_snapshot(
        &mut self,
        at: usize,
        bytes: Option<Arc<[u8]>>,
        windows: Vec<WindowGeometry>,
        out: &mut dyn Reporter,
    ) {
        let launch = self.runtimes[at].hello.runtime_session;
        let Some(keep) = &mut self.keep else {
            return;
        };
        let Some(asked) = keep.asked.iter().position(|&(r, _, _)| r == launch) else {
            return;
        };
        let (_, _, target) = keep.asked.swap_remove(asked);
        let Some(bytes) = bytes else {
            return out.log(
                "warn",
                "tool",
                "the app's state did not arrive whole: none is kept for the next session",
            );
        };
        let runtime = &self.runtimes[at];
        let views = runtime
            .mounts
            .views
            .values()
            .filter_map(|mount| {
                let (view, text, _) = self.sources.running(&runtime.mounts, mount.file)?;
                Some((self.sources.views[view].path.clone(), text.to_owned()))
            })
            .collect();
        let kept = kept::Kept {
            windows,
            views,
            snapshot: bytes.to_vec(),
        };
        let path = kept::path(&keep.dev, &target);
        match kept.write(&path) {
            Ok(()) => out.log(
                "info",
                "tool",
                &format!(
                    "kept the app's state for the next session: {} bytes",
                    kept.snapshot.len()
                ),
            ),
            Err(error) => out.log(
                "warn",
                "tool",
                &format!(
                    "cannot keep the app's state in `{}`: {error}",
                    path.display()
                ),
            ),
        }
    }

    /// Streams the frames of every runtime that connects from now on, the
    /// frame time drawn over its windows: `viso run --profile-frame`.
    pub fn stream_profiles(&mut self) {
        self.profile_frames = true;
    }

    /// When the session next wants [`tick`](Self::tick)ed.
    pub fn deadline(&self) -> Option<Instant> {
        match (self.batch_deadline(), self.profile_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// When the batch of edits is due.
    fn batch_deadline(&self) -> Option<Instant> {
        if self.ready {
            return Some(Instant::now());
        }
        let batch = self.batch.as_ref()?;
        Some((batch.last + COALESCE).min(batch.first + COALESCE_LIMIT))
    }

    /// Takes the latest content of a file of the scope.
    pub fn change(&mut self, change: Change, out: &mut dyn Reporter) {
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
    pub fn rescope(&self, rust: RustScope) {
        self._watcher.rescope(rust);
    }

    /// A Rust source changed: a build of it is due.
    pub fn rust_changed(&mut self, out: &mut dyn Reporter) {
        self.set_rust(RustState::Building, out);
    }

    /// Reports `build`, a build of the Rust sources after the launch: a
    /// failure keeps the app running; one that relinked nothing changes
    /// nothing. Whether the app is to restart on the new executable: its
    /// event waits for the restart to land.
    pub fn rebuilt(&mut self, lane: BuildId, build: &Build, out: &mut dyn Reporter) -> bool {
        if build.unchanged {
            out.log("info", "tool", "the Rust edit changed nothing the app runs");
            self.settle_rust(out);
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
                    &format!("{}; the app keeps running", failure.message()),
                );
                let codes = match failure {
                    BuildFailure::Failed { codes, .. } => codes.clone(),
                    _ => Vec::new(),
                };
                (Outcome::Rejected, codes)
            }
        };
        let app = self.app_of(lane);
        let revision = app.map_or(Revision::LAUNCH, |at| self.runtimes[at].revision);
        let candidate = self.next_revision;
        self.next_revision += 1;
        let files = build
            .files
            .iter()
            .map(|path| Source::new(path, &self.root, "").name().to_owned())
            .collect();
        let event = RebuildEvent {
            files,
            base_revision: revision.0,
            candidate_revision: candidate,
            last_good_revision: revision.0,
            outcome,
            codes,
            restored: false,
            prewarm_us: None,
            snapshot_us: None,
            close_us: None,
            launch_us: None,
            restore_us: None,
            restart_us: None,
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
            self.set_rust(RustState::Restarting, out);
            return true;
        }
        out.rebuild(&self.ids_of(app.map(|at| &self.runtimes[at])), &event);
        self.settle_rust(out);
        false
    }

    /// A build that restarts no app ended: the Rust state is idle, unless
    /// another lane's restart runs.
    fn settle_rust(&mut self, out: &mut dyn Reporter) {
        if self.restarting.is_none() {
            self.set_rust(RustState::Idle, out);
        }
    }

    /// The runtime of the app the lane of build `lane` launched: the first
    /// connected of its build.
    fn app_of(&self, lane: BuildId) -> Option<usize> {
        self.runtimes.iter().position(|r| r.hello.build_id == lane)
    }

    /// The app restarted on the new executable: with no snapshot to restore
    /// it runs fresh, which reports the restart now; otherwise the restore
    /// reports it.
    pub fn relaunched(&mut self, out: &mut dyn Reporter) {
        self.clock.launched = Some(Instant::now());
        self.snapshot_from = None;
        // The old app's channels may outlive it (a page's closes once the
        // new build is served): they are no longer the session's.
        let lane = self.restart_build;
        let mut at = 0;
        while at < self.runtimes.len() {
            if lane.is_none_or(|b| b == self.runtimes[at].hello.build_id) {
                let runtime = self.runtimes.remove(at);
                profile::gone(&runtime);
                self.report_status(&runtime, false, out);
            } else {
                at += 1;
            }
        }
        // The new build embeds every asset as it is now; the runtimes of
        // other lanes keep what theirs embedded.
        self.sources
            .rebuilt(self.runtimes.iter_mut().map(|runtime| &mut runtime.mounts));
        if self.restore_next {
            self.landing = true;
        } else {
            self.finish_restart(Outcome::WarmRestarted, false, Vec::new(), out);
        }
    }

    /// The app is gone and the session ends: a restart that has not landed
    /// is reported as it ended — the restarted app without its state, or,
    /// before the old one closed for it, no restart.
    pub fn ended(&mut self, out: &mut dyn Reporter) {
        if self.landing {
            out.log(
                "warn",
                "tool",
                "the restarted app exited before restoring its state",
            );
            self.finish_restart(Outcome::WarmRestarted, false, Vec::new(), out);
        } else if self.restarting.is_some() {
            out.log("warn", "tool", "the session ended before the app restarted");
            self.finish_restart(Outcome::Rejected, false, Vec::new(), out);
        }
    }

    /// The restart did not land: the new executable did not launch.
    pub fn restart_failed(&mut self, why: &str, out: &mut dyn Reporter) {
        out.log("error", "tool", why);
        self.restore_next = false;
        self.finish_restart(Outcome::Rejected, false, Vec::new(), out);
    }

    /// Reports the restart under way with `outcome`.
    fn finish_restart(
        &mut self,
        outcome: Outcome,
        restored: bool,
        codes: Vec<String>,
        out: &mut dyn Reporter,
    ) {
        self.landing = false;
        let restoring = self.restoring.take();
        let lane = self.restart_build.take();
        let Some(mut event) = self.restarting.take() else {
            return;
        };
        event.outcome = outcome;
        event.restored = restored;
        let clock = std::mem::take(&mut self.clock);
        event.prewarm_us = clock
            .prewarm
            .zip(clock.asked)
            .map(|(prewarm, asked)| micros(prewarm, asked));
        if let Some(asked) = clock.asked {
            let now = Instant::now();
            let ready = clock.held.unwrap_or(asked);
            event.snapshot_us = clock.held.map(|held| micros(asked, held));
            event.close_us = clock.launched.map(|launched| micros(ready, launched));
            event.launch_us = clock
                .launched
                .zip(clock.mounted)
                .map(|(launched, mounted)| micros(launched, mounted));
            event.restart_us = Some(micros(asked, now));
        }
        event.codes.extend(codes);
        event.last_good_revision = match outcome {
            Outcome::Rejected => event.base_revision,
            _ => Revision::LAUNCH.0,
        };
        let runtime = restoring
            .and_then(|id| self.index(id))
            .or_else(|| lane.and_then(|lane| self.app_of(lane)))
            .or((!self.runtimes.is_empty()).then_some(0))
            .map(|at| &self.runtimes[at]);
        out.rebuild(&self.ids_of(runtime), &event);
        if self.rust == RustState::Restarting {
            self.set_rust(RustState::Idle, out);
        }
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
    pub fn link(&mut self, event: LinkEvent, out: &mut dyn Reporter) {
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
                self.runtimes.push(Runtime {
                    id: hello.runtime_session.to_hex(),
                    build: hello.build_id.to_hex(),
                    revision: hello.current_revision,
                    hello,
                    writer: connection.writer,
                    traffic: connection.traffic,
                    in_flight: None,
                    asked: Vec::new(),
                    inspecting: Vec::new(),
                    playing: Vec::new(),
                    held_scenarios: Vec::new(),
                    capture: None,
                    ended: Vec::new(),
                    mounted: false,
                    blobs: Blobs::default(),
                    mounts: Mounts::default(),
                    patch: None,
                });
                self.status(self.runtimes.len() - 1, out);
                if self.profile_frames {
                    self.stream_frames(self.runtimes.len() - 1);
                }
            }
            LinkEvent::Runtime(from, message) => {
                if let Some(at) = self.index(from) {
                    self.runtime_message(at, message, out);
                }
            }
            LinkEvent::Closed(from) => {
                let Some(at) = self.index(from) else {
                    return;
                };
                let runtime = self.runtimes.remove(at);
                profile::gone(&runtime);
                self.report_status(&runtime, false, out);
                if self.landing && self.restarts(runtime.hello.build_id) {
                    out.log(
                        "warn",
                        "tool",
                        "the restarted app closed its dev channel before restoring its state",
                    );
                    self.finish_restart(Outcome::WarmRestarted, false, Vec::new(), out);
                }
            }
            LinkEvent::Refused(reason) => {
                out.log("warn", "tool", &reason);
                // The app launched to restore is the one refused: it runs
                // the new build fresh, out of the session's reach.
                let connected = self
                    .runtimes
                    .iter()
                    .any(|r| self.restarts(r.hello.build_id));
                if self.landing && !connected {
                    self.restore_next = false;
                    self.finish_restart(Outcome::WarmRestarted, false, Vec::new(), out);
                }
            }
        }
    }

    /// The index of the runtime `launch` names.
    fn index(&self, launch: RuntimeSessionId) -> Option<usize> {
        self.runtimes
            .iter()
            .position(|runtime| runtime.hello.runtime_session == launch)
    }

    /// Whether a runtime of build `build` runs the app the restart under
    /// way relaunches.
    fn restarts(&self, build: BuildId) -> bool {
        self.restart_build.is_none_or(|b| b == build)
    }

    /// Whom an event about `runtime` names.
    fn ids_of<'a>(&'a self, runtime: Option<&'a Runtime>) -> DevIds<'a> {
        let build = runtime.map_or(self.build.as_str(), |r| r.build.as_str());
        let runtime = runtime.map(|r| (r.id.as_str(), r.hello.target));
        ids(&self.session, build, runtime, self.runtimes.len() > 1)
    }

    /// Reports the status of runtime `at`.
    fn status(&self, at: usize, out: &mut dyn Reporter) {
        self.report_status(&self.runtimes[at], true, out);
    }

    /// Reports the status of `runtime`, connected or let go.
    fn report_status(&self, runtime: &Runtime, connected: bool, out: &mut dyn Reporter) {
        let failing = runtime
            .mounts
            .views
            .iter()
            .filter(|(_, mount)| mount.failing)
            .map(|(&at, _)| self.name(&self.sources.views[at].path))
            .collect();
        let status = RuntimeStatus {
            connected,
            revision: runtime.revision.0,
            candidate_revision: runtime.in_flight.as_ref().map(|sent| sent.next.0),
            failing,
            rust: self.rust,
            bytes_sent: runtime.traffic.sent(),
            bytes_received: runtime.traffic.received(),
            patch: runtime.patch.as_ref(),
        };
        out.runtime(&self.ids_of(Some(runtime)), &status);
    }

    /// Moves the Rust edits to `state`, reporting each runtime when it is a
    /// change.
    fn set_rust(&mut self, state: RustState, out: &mut dyn Reporter) {
        if self.rust == state {
            return;
        }
        self.rust = state;
        for at in 0..self.runtimes.len() {
            self.status(at, out);
        }
    }

    /// `path` as the project names it.
    fn name(&self, path: &std::path::Path) -> String {
        Source::new(path, &self.root, "").name().to_owned()
    }

    /// Takes the batch when it is due, and ends the profiles whose time is
    /// up.
    pub fn tick(&mut self, out: &mut dyn Reporter) {
        self.expire_profiles();
        if self.batch_deadline().is_none_or(|due| due > Instant::now()) {
            return;
        }
        self.batch = None;
        self.ready = false;
        self.flush(out);
    }

    fn runtime_message(&mut self, at: usize, message: RuntimeMessage, out: &mut dyn Reporter) {
        let launch = self.runtimes[at].hello.runtime_session;
        match message {
            RuntimeMessage::Mounts(entries) => {
                let runtime = &mut self.runtimes[at];
                let mounted: Vec<usize> = entries
                    .iter()
                    .map(|entry| self.sources.mount(&mut runtime.mounts, entry))
                    .collect();
                // Each mount's last-good compiles now, off the first edit's
                // path; one that cannot is retried, and reported, then.
                self.compile_catalogs(out);
                let runtime = &mut self.runtimes[at];
                self.sources.catalogs_checked(&mut runtime.mounts);
                for &view in &mounted {
                    let _ = self.sources.baseline(&mut runtime.mounts, view);
                }
                let build = runtime.hello.build_id;
                if self.restore_next && self.restarts(build) {
                    if self.clock.mounted.is_none() {
                        self.clock.mounted = Some(Instant::now());
                    }
                    self.restore_next = false;
                    self.restoring = Some(launch);
                    self.send_restore(at, &mounted, out);
                } else if self.landing && self.clock.mounted.is_none() {
                    self.clock.mounted = Some(Instant::now());
                }
                self.ready = true;
                self.runtimes[at].mounted = true;
                for (script, reply) in std::mem::take(&mut self.runtimes[at].held_scenarios) {
                    self.play(&script, reply);
                }
            }
            RuntimeMessage::Blob(chunk) => self.runtimes[at].blobs.add(chunk),
            RuntimeMessage::Snapshot {
                blob,
                windows,
                capture_us,
            } => {
                if self
                    .keep
                    .as_ref()
                    .is_some_and(|keep| keep.asked.iter().any(|&(runtime, _, _)| runtime == launch))
                {
                    let runtime = &mut self.runtimes[at];
                    let bytes = runtime.blobs.take(blob);
                    runtime.blobs.clear();
                    return self.keep_snapshot(at, bytes, windows, out);
                }
                if self.snapshot_from != Some(launch) {
                    return;
                }
                if self.clock.asked.is_some() {
                    self.clock.held = Some(Instant::now());
                }
                self.windows = windows;
                let runtime = &mut self.runtimes[at];
                let bytes = runtime.blobs.take(blob);
                runtime.blobs.clear();
                let Some(bytes) = bytes else {
                    return out.log(
                        "warn",
                        "tool",
                        "the app's snapshot did not arrive whole: the next launch runs fresh",
                    );
                };
                let views = runtime
                    .mounts
                    .views
                    .iter()
                    .filter_map(|(&view, mount)| {
                        let last_good = mount.last_good.as_ref()?;
                        Some(HeldView {
                            path: self.sources.views[view].path.clone(),
                            text: None,
                            plan: Some(last_good.clone()),
                        })
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
            RuntimeMessage::Restored(report) => {
                if self.restoring == Some(launch) {
                    self.restored(&report, out);
                }
            }
            RuntimeMessage::Ack(ack) => self.acked(at, &ack, out),
            RuntimeMessage::Nack(nack) if nack.stage == Stage::SnapshotRestore => {
                if self.restoring != Some(launch) {
                    return;
                }
                out.log(
                    "warn",
                    "tool",
                    &format!(
                        "the app did not restore its state ({}): it runs fresh",
                        nack.diagnostic_codes.join(", ")
                    ),
                );
                self.finish_restart(Outcome::WarmRestarted, false, nack.diagnostic_codes, out);
            }
            RuntimeMessage::Nack(nack) => self.nacked(at, &nack, out),
            RuntimeMessage::Diagnostic(raised) => self.diagnosed(at, raised, out),
            RuntimeMessage::Log { level, line } => out.log(level.as_str(), "app", &line),
            RuntimeMessage::Dropped { count } => out.log(
                "warn",
                "tool",
                &format!("the app dropped {count} dev messages: `viso run` read too slowly"),
            ),
            RuntimeMessage::Game(answer) => self.answer(at, answer),
            RuntimeMessage::Inspect { id, answer } => self.inspected(at, id, answer),
            RuntimeMessage::Scenario { id, line } => self.played(at, id, line),
            RuntimeMessage::Profile {
                id,
                frames,
                dropped,
                done,
            } => self.profiled(at, id, frames, dropped, done, out),
            // The link thread delivers no second hello.
            RuntimeMessage::Hello(_) => {}
        }
    }

    /// Takes runtime `at`'s ACK: each candidate it carried is the last good
    /// its next edit is planned against.
    fn acked(&mut self, at: usize, ack: &PatchAck, out: &mut dyn Reporter) {
        let Some(sent) = self.answered(at, ack.revision) else {
            return out.log(
                "warn",
                "tool",
                &format!("the app applied r{} unasked", ack.revision.0),
            );
        };
        let elapsed = elapsed_us(sent.started);
        let named = self.runtimes.len() > 1;
        let runtime = &mut self.runtimes[at];
        let id = runtime.id.clone();
        let build = runtime.build.clone();
        let ids = ids(
            &self.session,
            &build,
            Some((&id, runtime.hello.target)),
            named,
        );
        for SentView {
            at: view,
            text,
            candidate,
            notes,
        } in sent.views
        {
            let Some(mount) = runtime.mounts.views.get_mut(&view) else {
                continue;
            };
            let hash = source_hash(&text);
            mount.running = hash;
            mount.settled = Some(hash);
            mount.run_compiled();
            mount.failing = false;
            mount.last_good = Some(candidate);
            let file = mount.file;
            let counts = ack
                .files
                .iter()
                .find(|commit| commit.file == file)
                .map_or_else(CommitCounts::default, |commit| commit.counts);
            let path = &self.sources.views[view].path;
            let source = Source::new(path, &self.root, &text);
            let mut codes = notes;
            for notice in ack.notices.iter().filter(|n| n.file == file) {
                let Some(code) = intern_code(&notice.code) else {
                    continue;
                };
                let range = viso_dsl::TextRange::new(notice.start.into(), notice.end.into());
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
            out.dev(source.name(), &ids, &event);
        }
        for SentAsset { asset, hash, .. } in &sent.assets {
            runtime.mounts.asset_applied(asset, *hash);
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
            let path = self.sources.asset_path(asset);
            let name = Source::new(&path, &self.root, "").name().to_owned();
            out.dev(&name, &ids, &event);
        }
        let resets = ack.scoped_resets();
        runtime.patch = Some(PatchStatus {
            candidate_revision: sent.next.0,
            outcome: if resets > 0 {
                Outcome::ScopedReset
            } else {
                Outcome::Applied
            },
            stage: Stage::RuntimeCommit,
            scoped_resets: resets,
            compile_us: sent.compile_us,
            commit_us: Some(ack.timings.commit_us),
            round_trip_us: elapsed_us(sent.sent),
            bytes: runtime.traffic.sent().saturating_sub(sent.bytes_before),
        });
        self.status(at, out);
    }

    /// Takes runtime `at`'s NACK: it keeps its last good, and the candidates
    /// it refused are not sent to it again.
    /// Reports the diagnostic runtime `at` raised as a compiler's, in the
    /// file its first label points into.
    fn diagnosed(&mut self, at: usize, raised: RuntimeDiagnostic, out: &mut dyn Reporter) {
        match self.place(at, raised) {
            Placed::Source(view, diagnostic) => {
                let mounts = &self.runtimes[at].mounts;
                let file = mounts.views[&view].file;
                let Some((_, text, _)) = self.sources.running(mounts, file) else {
                    return;
                };
                let source = Source::new(&self.sources.views[view].path, &self.root, text);
                out.source(Some(&source), &[], &diagnostic);
            }
            Placed::Bare {
                code,
                message,
                notes,
            } => out.error(&code, &message, &notes),
        }
    }

    /// Places the labels of `raised`, which runtime `at` sent, in the sources
    /// that runtime runs: a span as it is, a state at the declaration the
    /// session's compile of its file holds. The first placed label is the
    /// primary span and the others in its file related spans; the rest
    /// become notes.
    fn place(&self, at: usize, raised: RuntimeDiagnostic) -> Placed {
        let mounts = &self.runtimes[at].mounts;
        let mut placed: Vec<(FileId, usize, viso_dsl::TextRange, String)> = Vec::new();
        let mut notes = Vec::new();
        for RuntimeLabel { at: place, label } in raised.labels {
            let range = match place {
                SourceAt::Span { file, start, end } => self
                    .sources
                    .running(mounts, file)
                    .filter(|&(_, text, _)| end as usize <= text.len())
                    .map(|(view, ..)| {
                        let range = viso_dsl::TextRange::new(start.into(), end.into());
                        (file, view, range)
                    }),
                SourceAt::State { file, key } => {
                    self.sources
                        .running(mounts, file)
                        .and_then(|(view, text, plan)| {
                            let symbol = viso_dsl::resolve::SymbolId::from_parts(key.hi, key.lo);
                            let (_, range) = plan?.declaration(symbol)?;
                            (range.end().to_usize() <= text.len()).then_some((file, view, range))
                        })
                }
                SourceAt::Point { path, line, column } => {
                    notes.push(format!("{label}: {path}:{line}:{column}"));
                    continue;
                }
            };
            match range {
                Some((file, view, range)) => placed.push((file, view, range, label)),
                None => notes.push(label),
            }
        }
        notes.extend(raised.notes);
        let (Some(code), Some(&(file, view, primary, _))) =
            (intern_code(&raised.code), placed.first())
        else {
            return Placed::Bare {
                code: raised.code,
                message: raised.message,
                notes,
            };
        };
        let mut diagnostic = Diagnostic::error(code, primary, raised.message);
        for (other, _, range, label) in placed.into_iter().skip(1) {
            if other == file {
                diagnostic = diagnostic.with_related(viso_dsl::diag::Related::new(range, label));
            } else {
                notes.push(label);
            }
        }
        diagnostic.notes = notes;
        Placed::Source(view, diagnostic)
    }

    fn nacked(&mut self, at: usize, nack: &PatchNack, out: &mut dyn Reporter) {
        let Some(sent) = self.answered(at, nack.candidate_revision) else {
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
        let named = self.runtimes.len() > 1;
        let runtime = &mut self.runtimes[at];
        runtime.revision = nack.last_good_revision;
        let id = runtime.id.clone();
        let build = runtime.build.clone();
        let ids = ids(
            &self.session,
            &build,
            Some((&id, runtime.hello.target)),
            named,
        );
        let elapsed = elapsed_us(sent.started);
        for sent_view in &sent.views {
            // Its mounts still run their last-good: the next edit is
            // planned against it.
            if let Some(mount) = runtime.mounts.views.get_mut(&sent_view.at) {
                mount.settled = Some(source_hash(&sent_view.text));
                mount.settle_compiled();
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
            let path = &self.sources.views[sent_view.at].path;
            let name = Source::new(path, &self.root, "").name().to_owned();
            out.dev(&name, &ids, &event);
        }
        for SentAsset { asset, hash, .. } in &sent.assets {
            // It runs its last good: the next edit of it goes again.
            runtime.mounts.asset_settled(asset, *hash);
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
            out.dev(&name, &ids, &event);
        }
        runtime.patch = Some(PatchStatus {
            candidate_revision: sent.next.0,
            outcome: Outcome::Rejected,
            stage: nack.stage,
            scoped_resets: 0,
            compile_us: sent.compile_us,
            commit_us: None,
            round_trip_us: elapsed_us(sent.sent),
            bytes: runtime.traffic.sent().saturating_sub(sent.bytes_before),
        });
        self.status(at, out);
    }

    /// Asks the app the lane of build `lane` launched for its snapshot,
    /// dropping the one held; whether that app runs to be asked.
    pub fn request_snapshot(&mut self, lane: BuildId) -> bool {
        self.held = None;
        self.windows.clear();
        self.clock = RestartClock {
            prewarm: self.clock.prewarm.take(),
            asked: Some(Instant::now()),
            ..RestartClock::default()
        };
        self.restart_build = Some(lane);
        let Some(at) = self.app_of(lane) else {
            return false;
        };
        let app = &self.runtimes[at];
        self.snapshot_from = Some(app.hello.runtime_session);
        app.writer.send(HostMessage::Snapshot(None)).is_ok()
    }

    /// Where the next app's launch window opens: where the first window of
    /// the app was when it sent its snapshot.
    pub fn launch_window(&self) -> Option<WindowGeometry> {
        self.windows.first().copied()
    }

    /// The new executable's prewarm run started, before the old app is asked
    /// for its snapshot.
    pub fn prewarm_started(&mut self) {
        self.clock = RestartClock {
            prewarm: Some(Instant::now()),
            ..RestartClock::default()
        };
    }

    /// Whether the app sent the snapshot asked for.
    pub fn has_snapshot(&self) -> bool {
        self.held.is_some()
    }

    /// Restores the held snapshot into the next app to connect, once it
    /// reports its mounts; whether one is held.
    pub fn restore_next(&mut self) -> bool {
        self.restore_next = self.held.is_some();
        self.restore_next
    }

    /// Sends the held snapshot to runtime `at`, with a view for each file of
    /// it the runtime just reported mounting that the old app ran: the view
    /// the build embeds and the plan to it from the old one.
    fn send_restore(&mut self, at: usize, mounted: &[usize], out: &mut dyn Reporter) {
        let (runtime, Some(held)) = (&self.runtimes[at], &self.held) else {
            return;
        };
        let langs = [runtime.hello.shader_lang, ShaderLang::None];
        let mut ui = UiPatch::default();
        for view in mounted {
            let Some(mount) = runtime.mounts.views.get(view) else {
                continue;
            };
            let path = &self.sources.views[*view].path;
            let Some(old) = held.views.iter().find(|old| old.path == *path) else {
                continue;
            };
            let Some(new) = &mount.last_good else {
                continue;
            };
            let compiled;
            let old = match (&old.plan, &old.text) {
                (Some(plan), _) => plan,
                (None, Some(text)) => match self.sources.compile_as(&runtime.mounts, *view, text) {
                    Ok(plan) => {
                        compiled = plan;
                        &compiled
                    }
                    Err(codes) => {
                        let name = Source::new(path, &self.root, "").name().to_owned();
                        out.log(
                            "warn",
                            "tool",
                            &format!(
                                "`{name}`: the version the last session ran does not compile \
                                 ({codes}); it runs fresh"
                            ),
                        );
                        continue;
                    }
                },
                (None, None) => continue,
            };
            match view_patch(mount.file, Some(old), new, &langs) {
                Ok(patch) => ui.views.push(patch),
                Err(error) => {
                    let name = Source::new(path, &self.root, "").name().to_owned();
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
    fn restored(&mut self, report: &RestoreReport, out: &mut dyn Reporter) {
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
                 lost), {} Rust state cells ({} lost), {games} games, {} extensions ({} lost) \
                 in {:.1} ms",
                report.views_lost,
                report.cells,
                report.cells_lost,
                report.extensions,
                report.extensions_lost,
                report.restore_us as f64 / 1000.0
            ),
        );
        if let Some(event) = &mut self.restarting {
            event.restore_us = Some(report.restore_us);
        }
        self.finish_restart(Outcome::WarmRestarted, true, Vec::new(), out);
    }

    /// The project root, canonical.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// The lane locks this session holds, to publish its control endpoint
    /// in.
    pub fn locks_mut(&mut self) -> &mut [LockGuard] {
        &mut self.locks
    }

    /// Whether a restart is under way, which a lane's restart waits for.
    pub fn restarting(&self) -> bool {
        self.restart_build.is_some() || self.restarting.is_some() || self.landing
    }

    /// Relays what `viso game` or `viso inspect` asks to the runtime it is
    /// for, or refuses it.
    pub fn control(&mut self, call: ControlCall, out: &mut dyn Reporter) {
        match call.ask {
            ControlAsk::Game { game, ask } => self.game(&game, ask, call.reply, out),
            ControlAsk::Inspect(ask) => self.inspect(ask, call.reply),
            ControlAsk::Scenario { script } => self.play(&script, call.reply),
            ControlAsk::Profile {
                frames,
                millis,
                drive,
            } => self.profile(frames, millis, drive, call.reply),
        }
    }

    /// The runtime that takes what names none: the first lane's app, or
    /// the first runtime connected.
    fn lead(&self) -> Option<usize> {
        self.lead
            .and_then(|lead| self.app_of(lead))
            .or((!self.runtimes.is_empty()).then_some(0))
    }

    /// Hands the first lane's app the scenario `script` to play,
    /// with the table of the views it mounts for its inspector selectors;
    /// [`played`](Self::played) answers once its report ends.
    fn play(&mut self, script: &str, reply: Sender<ControlReply>) {
        let refuse = |reply: Sender<ControlReply>| {
            let _ = reply.send(ControlReply::refused(
                control::ENV_NO_DEV_SESSION,
                "the app is not connected to `viso run`",
            ));
        };
        let Some(at) = self.lead() else {
            return refuse(reply);
        };
        if !self.runtimes[at].mounted {
            self.runtimes[at]
                .held_scenarios
                .push((script.to_owned(), reply));
            return;
        }
        let sources = self.source_table(at);
        let id = self.next_query;
        self.next_query = self.next_query.wrapping_add(1);
        let runtime = &mut self.runtimes[at];
        let message = HostMessage::Scenario {
            id,
            script: BlobRef::of(script.as_bytes()),
            sources: Some(BlobRef::of(&sources)),
        };
        let sent = blob_chunks(script.as_bytes())
            .chain(blob_chunks(&sources))
            .all(|chunk| runtime.writer.send(HostMessage::Blob(chunk)).is_ok())
            && runtime.writer.send(message).is_ok();
        if !sent {
            return refuse(reply);
        }
        runtime.playing.push((id, reply, Vec::new()));
    }

    /// Keeps the report line `line` of the scenario `id` runtime `at` plays,
    /// and answers its caller with the report once it ends.
    fn played(&mut self, at: usize, id: u32, line: String) {
        let runtime = &mut self.runtimes[at];
        let Some(index) = runtime.playing.iter().position(|(asked, ..)| *asked == id) else {
            return;
        };
        let done = matches!(Reported::from_line(&line), Some(Reported::Done { .. }));
        runtime.playing[index].2.push(line);
        if done {
            let (_, reply, lines) = runtime.playing.swap_remove(index);
            let _ = reply.send(ControlReply::Played { lines });
        }
    }

    /// The source table of the views runtime `at` mounts, in its wire form.
    fn source_table(&self, at: usize) -> Vec<u8> {
        let runtime = &self.runtimes[at];
        let files = runtime
            .mounts
            .views
            .keys()
            .filter_map(|&view| {
                let name = self.name(&self.sources.views[view].path);
                self.sources.file_sources(&runtime.mounts, view, name)
            })
            .collect();
        SourceTable { files }.to_bytes()
    }

    /// Asks the first lane's app what `ask` asks, which
    /// [`inspected`](Self::inspected) answers.
    fn inspect(&mut self, ask: InspectAsk, reply: Sender<ControlReply>) {
        let Some(runtime) = self.lead().map(|at| &mut self.runtimes[at]) else {
            let _ = reply.send(ControlReply::refused(
                control::ENV_NO_DEV_SESSION,
                "the app is not connected to `viso run`",
            ));
            return;
        };
        let id = self.next_query;
        self.next_query = self.next_query.wrapping_add(1);
        if runtime
            .writer
            .send(HostMessage::Inspect { id, ask })
            .is_err()
        {
            let _ = reply.send(ControlReply::refused(
                control::ENV_NO_DEV_SESSION,
                "the app is not connected to `viso run`",
            ));
            return;
        }
        runtime.inspecting.push((id, reply));
    }

    /// Answers the `viso inspect` that asked runtime `at` with the answer
    /// `answer` names, the table of the views it mounts and what its
    /// inspector can tell.
    fn inspected(&mut self, at: usize, id: u32, answer: BlobRef) {
        let runtime = &mut self.runtimes[at];
        let Some(index) = runtime
            .inspecting
            .iter()
            .position(|(asked, _)| *asked == id)
        else {
            return;
        };
        let (_, reply) = runtime.inspecting.swap_remove(index);
        let inspector = runtime.hello.inspector;
        let taken = runtime.blobs.take(answer);
        // An answer is read once; the same bytes may come again.
        runtime.blobs.release(answer);
        let Some(answer) = taken else {
            let _ = reply.send(ControlReply::refused(
                control::INSPECT_ANSWER_INVALID,
                "the app's answer did not arrive whole",
            ));
            return;
        };
        let sources = self.source_table(at);
        // What the reply holds besides the two fits in a few bytes.
        let reply_with = if answer.len() + sources.len() > MAX_FRAME - 64 {
            ControlReply::refused(
                control::INSPECT_ANSWER_INVALID,
                format!(
                    "the app's answer outgrew the control link: {} bytes",
                    answer.len() + sources.len()
                ),
            )
        } else {
            ControlReply::Inspected {
                answer: answer.to_vec(),
                sources,
                inspector,
            }
        };
        let _ = reply.send(reply_with);
    }

    /// Relays what `viso game` asks about the game `game` names to the
    /// first runtime that runs it, or refuses it.
    fn game(
        &mut self,
        game: &str,
        ask: GameAsk,
        reply: Sender<ControlReply>,
        out: &mut dyn Reporter,
    ) {
        let refuse = |code: &str, message: String| {
            let _ = reply.send(ControlReply::refused(code, message));
        };
        if self.runtimes.is_empty() {
            return refuse(
                control::GAME_NOT_RUNNING,
                "the app is not connected to `viso run`".into(),
            );
        }
        let mut found = None;
        let mut refusal = None;
        for (at, runtime) in self.runtimes.iter().enumerate() {
            if !runtime.hello.capabilities.contains(Domain::System) {
                continue;
            }
            match self.find_game(&runtime.mounts, game) {
                Ok(view) => {
                    found = Some((at, view));
                    break;
                }
                Err(reply) => {
                    refusal.get_or_insert(reply);
                }
            }
        }
        let Some((at, view)) = found else {
            return match refusal {
                Some(refusal) => {
                    let _ = reply.send(refusal);
                }
                None => refuse(
                    control::GAME_NOT_RUNNING,
                    "the app runs no game sessions".into(),
                ),
            };
        };
        let runtime = &mut self.runtimes[at];
        let file = runtime.mounts.views[&view].file;
        if let GameAsk::Record { .. } = ask {
            let name = Source::new(&self.sources.views[view].path, &self.root, "")
                .name()
                .to_owned();
            out.log(
                "info",
                "tool",
                &format!("recording `{name}` from a restarted World"),
            );
        }
        let id = self.next_query;
        self.next_query = self.next_query.wrapping_add(1);
        let query = GameQuery {
            id,
            file,
            ask: ask.clone(),
        };
        if runtime.writer.send(HostMessage::Game(query)).is_err() {
            return refuse(
                control::GAME_NOT_RUNNING,
                "the app is not connected to `viso run`".into(),
            );
        }
        runtime.asked.push(Asked {
            id,
            at: view,
            ask,
            reply,
        });
    }

    /// The view of `mounts` that `game` names: its path from the project
    /// root, with or without the extension, or its file's stem.
    fn find_game(&self, mounts: &Mounts, game: &str) -> Result<usize, ControlReply> {
        let game = game.trim_start_matches("./").replace('\\', "/");
        let mut named = Vec::new();
        let mut mounted = Vec::new();
        for &at in mounts.views.keys() {
            let name = self.name(&self.sources.views[at].path);
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

    /// Hands runtime `at`'s answer to the `viso game` that asked.
    fn answer(&mut self, at: usize, answer: GameAnswer) {
        let runtime = &mut self.runtimes[at];
        let Some(index) = runtime.asked.iter().position(|a| a.id == answer.id) else {
            return;
        };
        let asked = runtime.asked.swap_remove(index);
        let file = self.name(&self.sources.views[asked.at].path);
        let program = self.runtimes[at]
            .mounts
            .views
            .get(&asked.at)
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
            (
                Ok(GameReply::Replayed {
                    from,
                    ticks,
                    sessions,
                }),
                GameAsk::Replay { .. },
            ) => ControlReply::Replayed {
                file,
                from,
                ticks,
                sessions,
            },
            (Ok(_), _) => ControlReply::refused(
                control::GAME_ANSWER_INVALID,
                "the app answered another question",
            ),
            (Err(refusal), ask) => refusal_reply(&refusal, &file, ask, program),
        };
        let _ = asked.reply.send(reply);
    }

    /// Closes runtime `at`'s patch in flight if it is the one moving to
    /// `next`, and takes the next batch now.
    fn answered(&mut self, at: usize, next: Revision) -> Option<InFlight> {
        let runtime = &mut self.runtimes[at];
        if runtime.in_flight.as_ref()?.next != next {
            return None;
        }
        let sent = runtime.in_flight.take()?;
        runtime.revision = next;
        self.ready = true;
        Some(sent)
    }

    /// Compiles the catalogs that changed, reporting each error.
    fn compile_catalogs(&mut self, out: &mut dyn Reporter) {
        for failed in self.sources.compile_catalogs() {
            for (file, diagnostic) in &failed.diagnostics {
                let (path, text) = &failed.files[*file];
                out.source(Some(&Source::new(path, &self.root, text)), &[], diagnostic);
            }
        }
        for runtime in &mut self.runtimes {
            self.sources.catalogs_checked(&mut runtime.mounts);
        }
    }

    /// Compiles the batch for each runtime with no patch in flight and sends
    /// it what compiled for it. Runtimes that mount a file alike share its
    /// compile; each is planned, packed and answered on its own, so one
    /// whose target refuses an edit stays on its last good while the others
    /// advance.
    fn flush(&mut self, out: &mut dyn Reporter) {
        self.compile_catalogs(out);
        let mut pass = Pass::default();
        for at in 0..self.runtimes.len() {
            if self.runtimes[at].in_flight.is_none() {
                self.flush_runtime(at, &mut pass, out);
            }
        }
    }

    /// Compiles the batch for runtime `at` and sends it what compiled.
    fn flush_runtime(&mut self, at: usize, pass: &mut Pass, out: &mut dyn Reporter) {
        let named = self.runtimes.len() > 1;
        let runtime = &mut self.runtimes[at];
        let id = runtime.id.clone();
        let build = runtime.build.clone();
        let ids = ids(
            &self.session,
            &build,
            Some((&id, runtime.hello.target)),
            named,
        );
        let app = match ids.target {
            Some(target) if named => format!("the {target} app"),
            _ => "the app".to_owned(),
        };
        // A rejected edit reverted to what the runtime runs: clear its failure.
        let mut cleared = false;
        for (&view, mount) in &mut runtime.mounts.views {
            if mount.failing
                && self.sources.views[view].hash() == Some(mount.running)
                && self.sources.imports_running(mount)
            {
                mount.failing = false;
                mount.settled = None;
                cleared = true;
                let _ = runtime.writer.send(HostMessage::Failure {
                    file: mount.file,
                    lines: Vec::new(),
                });
            }
        }
        let candidates = self.sources.candidates(&runtime.mounts);
        if candidates.is_empty()
            && self
                .sources
                .asset_candidates(&runtime.mounts, &[])
                .is_empty()
        {
            if cleared {
                self.status(at, out);
            }
            return;
        }
        let started = Instant::now();
        let bytes_before = runtime.traffic.sent();
        let base = runtime.revision;
        let next = match pass.next {
            Some(next) => next,
            None => {
                let next = Revision(self.next_revision);
                self.next_revision += 1;
                pass.next = Some(next);
                next
            }
        };
        let mut views = Vec::new();
        let mut failed = cleared;
        for view in candidates {
            let Some(mount) = runtime.mounts.views.get_mut(&view) else {
                continue;
            };
            let Some(compiled) = self.sources.compile(view, mount, &mut pass.memo) else {
                continue;
            };
            let path = &self.sources.views[view].path;
            let Some(text) = self.sources.views[view].text.clone() else {
                continue;
            };
            let hash = source_hash(&text);
            let diagnostics = match compiled {
                Ok(candidate) if self.apply => {
                    views.push((view, text, candidate));
                    continue;
                }
                Ok(_) => {
                    mount.settled = Some(hash);
                    mount.settle_compiled();
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
                    let name = Source::new(path, &self.root, "").name().to_owned();
                    out.dev(&name, &ids, &event);
                    continue;
                }
                Err(diagnostics) => diagnostics,
            };
            mount.settled = Some(hash);
            mount.settle_compiled();
            let source = Source::new(path, &self.root, &text);
            if pass.reported.insert((view, hash)) {
                for diagnostic in &diagnostics {
                    out.source(Some(&source), &[], diagnostic);
                }
            }
            if self.apply {
                mount.failing = true;
                failed = true;
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
            out.dev(source.name(), &ids, &event);
        }
        // The assets to send: each the runtime holds that changed, each font
        // that changed, and each a candidate names that it does not hold yet.
        // Each is decoded here as the app would: one that does not decode is
        // reported against its path and not sent, and the app keeps the last
        // good.
        let mut named_assets = Vec::new();
        for (view, _, candidate) in &views {
            if let Some(dir) = runtime.mounts.views.get(view).and_then(|m| m.assets) {
                named_assets.extend(candidate.assets.iter().map(|asset| AssetRef {
                    dir,
                    path: asset.path.clone(),
                }));
            }
        }
        let replaces_assets = runtime.hello.capabilities.contains(Domain::Resource);
        let mut assets = Vec::new();
        for asset in self
            .sources
            .asset_candidates(&runtime.mounts, &named_assets)
        {
            let file = self.sources.asset(&asset);
            let hash = file.hash;
            let path = self.sources.asset_path(&asset);
            let name = Source::new(&path, &self.root, "").name().to_owned();
            let decoded = pass
                .decoded
                .entry((asset.clone(), hash))
                .or_insert_with(|| {
                    if file.font {
                        font_format::parses(&file.bytes)
                            .then_some(())
                            .ok_or_else(|| "a face of it does not parse".to_owned())
                    } else {
                        check_asset(&file.bytes)
                    }
                });
            let (outcome, stage, codes) = match decoded {
                Ok(()) if self.apply && replaces_assets => {
                    assets.push(SentAsset {
                        asset,
                        hash,
                        len: file.bytes.len(),
                    });
                    continue;
                }
                Ok(()) => {
                    if self.apply {
                        out.log(
                            "warn",
                            "tool",
                            &format!(
                                "`{name}`: {app} does not replace assets, so it keeps the \
                                 one it was built with"
                            ),
                        );
                    }
                    (Outcome::Held, Stage::Transport, Vec::new())
                }
                Err(error) => {
                    if pass.reported_assets.insert((asset.clone(), hash)) {
                        out.error(
                            ASSET_ERROR,
                            &format!("`{name}` does not decode: {error}"),
                            &["the app keeps showing its last good version".to_owned()],
                        );
                    }
                    (
                        Outcome::Rejected,
                        Stage::ResourceBuild,
                        vec![ASSET_ERROR.to_owned()],
                    )
                }
            };
            runtime.mounts.asset_settled(&asset, hash);
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
            out.dev(&name, &ids, &event);
        }
        if views.is_empty() && assets.is_empty() {
            if failed {
                self.status(at, out);
            }
            return;
        }
        let swaps_games = runtime.hello.capabilities.contains(Domain::System);
        // What the build embeds: the runtime's backend's and the reference
        // interpreter's, so a shader the edit left as it was packs to the
        // bytes of the program the app runs and loads as it.
        let langs = [runtime.hello.shader_lang, ShaderLang::None];
        let mut ui = UiPatch::default();
        let mut systems = SystemPatch::default();
        let mut sent = Vec::with_capacity(views.len());
        for (view, text, candidate) in views {
            let path = &self.sources.views[view].path;
            let name = Source::new(path, &self.root, "").name().to_owned();
            if let Err(reason) = self.sources.baseline(&mut runtime.mounts, view) {
                out.log(
                    "warn",
                    "tool",
                    &format!("`{name}`: {reason}, so its mounts rebuild with fresh state"),
                );
            }
            let mount = &runtime.mounts.views[&view];
            match view_patch(mount.file, mount.last_good.as_ref(), &candidate, &langs) {
                Ok(view) => ui.views.push(view),
                Err(error) => {
                    out.error(
                        "E3711",
                        &format!(
                            "`{name}`: internal: a shader it paints with does not build for \
                             {app}: {error}"
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
                    let source = Source::new(path, &self.root, &text);
                    if pass.reported_notes.insert((view, source_hash(&text))) {
                        out.source(Some(&source), &[], &note);
                    }
                    notes.push(note.code.to_owned());
                }
                if swaps_games {
                    systems.games.push(entry);
                } else {
                    out.log(
                        "warn",
                        "tool",
                        &format!(
                            "`{name}`: {app} does not swap game systems, so its game keeps \
                             running the systems it started with"
                        ),
                    );
                }
            }
            sent.push(SentView {
                at: view,
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
        let compile_us = elapsed_us(started);
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
            sent: Instant::now(),
            compile_us,
            bytes_before,
        });
        self.status(at, out);
    }
}

/// Whom an event about `runtime` (its id and target) of the session
/// `session` running build `build` names; `named` when the session serves
/// more than one runtime.
fn ids<'a>(
    session: &'a str,
    build: &'a str,
    runtime: Option<(&'a str, RuntimeTarget)>,
    named: bool,
) -> DevIds<'a> {
    DevIds {
        session,
        runtime_session: runtime.map_or("", |(id, _)| id),
        target: runtime.map(|(_, target)| target.as_str()),
        build_id: build,
        named,
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
        GameRefusal::NoHistory => control::GAME_NO_HISTORY,
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

fn elapsed_us(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// 128 bits from the process's randomly keyed hasher, as 32 hex digits. The
/// keys come from the operating system's random source; the hash is not a
/// cryptographic generator, which a loopback session token does not need.
pub fn random_hex() -> String {
    let state = RandomState::new();
    let word = |salt: u8| {
        let mut hasher = state.build_hasher();
        hasher.write_u8(salt);
        hasher.finish()
    };
    format!("{:016x}{:016x}", word(0), word(1))
}

/// Whether `bytes` decode as the app decodes an asset: a raster image to
/// texels, an SVG to a document; why not. A video's container is taken as
/// sniffed: the platform's player decodes it, only when it plays; a glTF
/// model as sniffed too, the app decoding it when a scene draws it.
fn check_asset(bytes: &[u8]) -> Result<(), String> {
    use viso_schema::resource::{AssetFormat, AssetKind};
    match AssetFormat::sniff(bytes).map(AssetFormat::kind) {
        Some(AssetKind::Raster) => viso_image::decode_raster(bytes)
            .map(drop)
            .map_err(|error| error.to_string()),
        Some(AssetKind::Vector) => viso_svg::parse(bytes)
            .map(drop)
            .map_err(|error| error.to_string()),
        Some(AssetKind::Video | AssetKind::Model) => Ok(()),
        None => Err(viso_image::AssetError::Format.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::mpsc::{self, Receiver};

    use viso_dev_protocol::wire::{
        BLOB_CHUNK, BuildId, DevSessionId, Domains, FileCommit, FileId, MountEntry, PatchTimings,
        ProjectFingerprint, ReloadPlan, SchemaFingerprint, ViewPatch,
    };
    use viso_dsl::aot::emit_view_package;
    use viso_project::{Layout, LockKind, Target};

    use super::link::Connection;
    use super::*;
    use crate::report::Counted;

    const VIEW: &str = "component Counter {\n    state count = 0;\n    view {\n        Column {\n            Text { width: 120dp; }\n        }\n    }\n}\n";

    struct Fixture {
        dev: DevSession,
        out: Counted,
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
                builds: vec![BuildId(2)],
                project: ProjectFingerprint(3),
                schema: SchemaFingerprint(4),
            };
            let scope = WatchScope {
                project: root.clone(),
                rust: RustScope::default(),
            };
            let mut dev = DevSession::start(scope, &expect, vec![lock], apply, |_| true);
            let mut out = Counted::default();
            let sent = connect(
                &mut dev,
                &mut out,
                hello(9, RuntimeTarget::DesktopHost, capabilities),
            );
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

        /// The app the session launched: the runtime connected first.
        fn launch(&self) -> RuntimeSessionId {
            self.dev
                .runtimes
                .first()
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
                applied_domains: Domains::NONE.with(viso_dev_protocol::wire::Domain::Ui),
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

    /// The hello of runtime `id` of `target`, applying `capabilities`.
    fn hello(id: u128, target: RuntimeTarget, capabilities: Domains) -> RuntimeHello {
        RuntimeHello {
            protocol_version: viso_dev_protocol::wire::DEV_PROTOCOL_VERSION,
            token: "t".into(),
            dev_session: DevSessionId(1),
            runtime_session: RuntimeSessionId(id),
            build_id: BuildId(2),
            current_revision: Revision::LAUNCH,
            schema_fingerprint: SchemaFingerprint(4),
            capabilities,
            target,
            shader_lang: match target {
                RuntimeTarget::Web => viso_dev_protocol::wire::ShaderLang::Wgsl,
                RuntimeTarget::AndroidEmulator => viso_dev_protocol::wire::ShaderLang::SpirV,
                _ => viso_dev_protocol::wire::ShaderLang::Msl,
            },
            inspector: viso_dev_protocol::inspect::InspectorCaps::TREE
                .with(viso_dev_protocol::inspect::InspectorCaps::NODES),
        }
    }

    /// Connects the runtime `hello` describes, and returns what it is sent.
    fn connect(
        dev: &mut DevSession,
        out: &mut dyn Reporter,
        hello: RuntimeHello,
    ) -> Receiver<HostMessage> {
        let (writer, sent) = mpsc::channel();
        let connection = Connection {
            hello,
            writer,
            traffic: Arc::default(),
        };
        dev.link(LinkEvent::Connected(connection), out);
        sent
    }

    /// `text` compiled as the build compiles `view.vs`.
    fn compiled(text: &str) -> CandidatePlan {
        let origin = viso_dsl::frontend::Origin {
            package: "app".into(),
            module: vec!["view".into()],
            language: None,
            ..Default::default()
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
                &[viso_dev_protocol::wire::ShaderLang::Msl],
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
                .map(|path| viso_dev_protocol::wire::ResourceCommit {
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
                hash: viso_dev_protocol::wire::content_hash(NIGHT),
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

    const FONT: &[u8] = include_bytes!("../../../../crates/viso/fixtures/DejaVuSans-subset.ttf");

    #[test]
    fn a_changed_font_goes_with_no_view_naming_it_and_a_broken_one_is_reported() {
        let mut f = Fixture::app_with(
            "font-edit",
            true,
            VIEW,
            VIEW,
            replaces_assets(),
            &[("fonts/Body.ttf", FONT, false)],
        );
        assert!(f.settle().is_empty(), "the app runs what the build read");

        // Trailing bytes change the file, not its faces.
        let edited = [FONT, &[0; 4]].concat();
        f.save_asset("fonts/Body.ttf", &edited);
        let sent = f.settle();
        let (blobs, patch) = resources(&sent);
        assert_eq!(blobs, [&edited[..]]);
        assert_eq!(patch.assets[0].path, "fonts/Body.ttf");
        acked_assets(&mut f, 2, &["fonts/Body.ttf"]);

        let errors = f.out.errors();
        f.save_asset("fonts/Body.ttf", &FONT[..64]);
        assert!(f.settle().is_empty(), "nothing is sent");
        assert!(f.out.errors() > errors, "the failure is reported");
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
    fn a_runtime_diagnostic_is_placed_in_the_source_the_app_runs() {
        const LOOPED: &str = "component Feedback {\n    state count = 0;\n    view {\n        \
            Text { width: 120dp; height: 40dp; on click { count += 1; } }\n    }\n    \
            effect echo when (count) run EffectRun::change {\n        \
            transaction { count += 1; }\n    }\n}\n";
        let f = Fixture::app("diagnosed", true, LOOPED, LOOPED, Domains::NONE);
        let symbol = compiled(LOOPED).symbol_for_name("count").expect("a state");
        let body = LOOPED.find("transaction").unwrap() as u32;
        let label = |at, label: &str| RuntimeLabel {
            at,
            label: label.into(),
        };
        let raised = RuntimeDiagnostic {
            code: "E4202".into(),
            message: "effects kept waking each other".into(),
            labels: vec![
                label(
                    SourceAt::State {
                        file: FileId(0),
                        key: viso_schema::StateKey::from_parts(symbol.hi, symbol.lo),
                    },
                    "`count` is written again",
                ),
                label(
                    SourceAt::Span {
                        file: FileId(0),
                        start: body,
                        end: body + 11,
                    },
                    "this effect writes what wakes it",
                ),
                label(
                    SourceAt::Point {
                        path: "src/main.rs".into(),
                        line: 3,
                        column: 9,
                    },
                    "this state is written again",
                ),
                label(
                    SourceAt::Span {
                        file: FileId(7),
                        start: 0,
                        end: 1,
                    },
                    "in a file the app does not mount",
                ),
            ],
            notes: vec!["the last writes stand".into()],
        };
        let Placed::Source(view, diagnostic) = f.dev.place(0, raised) else {
            panic!("the diagnostic is placed in the view");
        };
        assert_eq!(f.dev.sources.views[view].path, f.path);
        assert_eq!(diagnostic.code, "E4202");
        let text =
            |range: viso_dsl::TextRange| &LOOPED[range.start().to_usize()..range.end().to_usize()];
        assert!(
            text(diagnostic.primary).contains("count"),
            "{:?}",
            text(diagnostic.primary)
        );
        assert_eq!(diagnostic.related.len(), 1);
        assert_eq!(text(diagnostic.related[0].range), "transaction");
        assert_eq!(
            diagnostic.related[0].label,
            "this effect writes what wakes it"
        );
        assert_eq!(
            diagnostic.notes,
            [
                "this state is written again: src/main.rs:3:9",
                "in a file the app does not mount",
                "the last writes stand"
            ]
        );
        // One the session can place nowhere is reported bare.
        let lost = RuntimeDiagnostic {
            code: "E4202".into(),
            message: "lost".into(),
            labels: Vec::new(),
            notes: Vec::new(),
        };
        assert!(matches!(f.dev.place(0, lost), Placed::Bare { .. }));
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
            ask: ControlAsk::Game {
                game: game.into(),
                ask,
            },
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
    fn an_inspection_relays_the_census_with_the_source_of_the_views_mounted() {
        use viso_dev_protocol::inspect::{
            Census, CensusMount, CensusNode, InspectKind, NodeOrigin, Rect, Selector, WindowCensus,
            resolve,
        };
        let mut f = Fixture::app("inspect", true, GAME, GAME, Domains::NONE.with(Domain::Ui));
        let _ = f.settle();
        let (reply, answer) = mpsc::channel();
        f.dev.control(
            ControlCall {
                ask: ControlAsk::Inspect(InspectAsk::Census),
                reply,
            },
            &mut f.out,
        );
        let sent: Vec<HostMessage> = f.sent.try_iter().collect();
        let [
            HostMessage::Inspect {
                id,
                ask: InspectAsk::Census,
            },
        ] = &sent[..]
        else {
            panic!("not asked: {sent:?}");
        };
        let census = |file: &str| Census {
            windows: vec![WindowCensus {
                mounts: vec![CensusMount {
                    file: file.into(),
                    source_hash: 0,
                }],
                nodes: vec![CensusNode {
                    index: 3,
                    generation: 1,
                    parent: None,
                    kind: InspectKind::Container,
                    bounds: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 200.0,
                        h: 10.0,
                    },
                    shown: true,
                    focusable: false,
                    focused: false,
                    semantics: None,
                    origin: Some(NodeOrigin::Static { mount: 0, index: 0 }),
                }],
            }],
        };
        let bytes = census("elsewhere.vs").to_bytes();
        for chunk in blob_chunks(&bytes) {
            f.runtime(RuntimeMessage::Blob(chunk));
        }
        f.runtime(RuntimeMessage::Inspect {
            id: *id,
            answer: BlobRef::of(&bytes),
        });
        let Ok(ControlReply::Inspected {
            answer: relayed,
            sources,
            inspector,
        }) = answer.try_recv()
        else {
            panic!("not answered");
        };
        assert_eq!(relayed, bytes);
        assert_eq!(inspector, f.dev.runtimes[0].hello.inspector);
        let table = SourceTable::from_bytes(&sources).unwrap();
        let [file] = &table.files[..] else {
            panic!("not one view: {table:?}");
        };
        assert_eq!(file.path, "view.vs");
        assert_eq!(file.statics[0].type_name, "Column");
        assert_eq!(file.statics[0].owner, "Arena");
        let census = census(&file.file);
        let inspection = resolve(&census, &table);
        let selected = Selector::parse("Column").unwrap().select(&inspection);
        assert_eq!(selected, [0]);

        // The same answer again, for the next question, arrives whole too.
        let (reply, again) = mpsc::channel();
        f.dev.control(
            ControlCall {
                ask: ControlAsk::Inspect(InspectAsk::Census),
                reply,
            },
            &mut f.out,
        );
        let sent: Vec<HostMessage> = f.sent.try_iter().collect();
        let [HostMessage::Inspect { id, .. }] = &sent[..] else {
            panic!("not asked: {sent:?}");
        };
        for chunk in blob_chunks(&bytes) {
            f.runtime(RuntimeMessage::Blob(chunk));
        }
        f.runtime(RuntimeMessage::Inspect {
            id: *id,
            answer: BlobRef::of(&bytes),
        });
        assert!(
            matches!(again.try_recv(), Ok(ControlReply::Inspected { answer, .. }) if answer == bytes),
            "not answered again"
        );

        // An app that exits with the question open refuses it, and without
        // an app there is no one to ask.
        let (reply, open) = mpsc::channel();
        f.dev.control(
            ControlCall {
                ask: ControlAsk::Inspect(InspectAsk::Census),
                reply,
            },
            &mut f.out,
        );
        let launch = f.launch();
        f.dev.link(LinkEvent::Closed(launch), &mut f.out);
        let (reply, answer) = mpsc::channel();
        f.dev.control(
            ControlCall {
                ask: ControlAsk::Inspect(InspectAsk::Census),
                reply,
            },
            &mut f.out,
        );
        for answer in [open, answer] {
            let Ok(ControlReply::Refused { code, .. }) = answer.try_recv() else {
                panic!("not refused");
            };
            assert_eq!(code, control::ENV_NO_DEV_SESSION);
        }
    }

    #[test]
    fn a_scenario_goes_to_the_app_with_its_views_and_its_report_comes_back_whole() {
        let mut f = Fixture::app("scenario", true, GAME, GAME, Domains::NONE.with(Domain::Ui));
        let _ = f.settle();
        let script = "expect \"0 clicks\"\nclick \"Click me\"\n";
        let (reply, answer) = mpsc::channel();
        f.dev.control(
            ControlCall {
                ask: ControlAsk::Scenario {
                    script: script.to_owned(),
                },
                reply,
            },
            &mut f.out,
        );
        let sent: Vec<HostMessage> = f.sent.try_iter().collect();
        let Some(HostMessage::Scenario {
            id,
            script: sent_script,
            sources: Some(sources),
        }) = sent.last()
        else {
            panic!("not asked: {sent:?}");
        };
        assert_eq!(*sent_script, BlobRef::of(script.as_bytes()));
        let blobs = sent
            .iter()
            .filter(|message| matches!(message, HostMessage::Blob(_)))
            .count();
        assert!(blobs >= 2, "the script and the views go ahead: {sent:?}");
        assert!(sources.len > 0);

        let lines = [
            Reported::Passed { line: 1 }.to_line(),
            Reported::Passed { line: 2 }.to_line(),
        ];
        for line in &lines {
            f.runtime(RuntimeMessage::Scenario {
                id: *id,
                line: line.clone(),
            });
        }
        assert!(answer.try_recv().is_err(), "the report has not ended");
        let done = Reported::Done { frames: 4 }.to_line();
        f.runtime(RuntimeMessage::Scenario {
            id: *id,
            line: done.clone(),
        });
        let Ok(ControlReply::Played { lines: relayed }) = answer.try_recv() else {
            panic!("not answered");
        };
        assert_eq!(relayed, [lines[0].clone(), lines[1].clone(), done]);

        // An app that exits mid-scenario refuses it.
        let (reply, open) = mpsc::channel();
        f.dev.control(
            ControlCall {
                ask: ControlAsk::Scenario {
                    script: script.to_owned(),
                },
                reply,
            },
            &mut f.out,
        );
        let launch = f.launch();
        f.dev.link(LinkEvent::Closed(launch), &mut f.out);
        let Ok(ControlReply::Refused { code, .. }) = open.try_recv() else {
            panic!("not refused");
        };
        assert_eq!(code, control::ENV_NO_DEV_SESSION);
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
            span: viso_dev_protocol::wire::RecordSpan::Ticks(60),
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
            [viso_dev_protocol::wire::GamePatch {
                file: FileId(0),
                tier: viso_dev_protocol::wire::GameTier::Logic,
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
            viso_dev_protocol::wire::GameTier::LogicMigration
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
            viso_dev_protocol::wire::GameTier::WorldRebuild {
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
        assert!(
            f.dev.rebuilt(BuildId(2), &built, &mut f.out),
            "the app restarts on it"
        );
        assert!(f.dev.request_snapshot(BuildId(2)));
        assert!(matches!(f.sent.try_recv(), Ok(HostMessage::Snapshot(None))));
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
        f.dev.relaunched(&mut f.out);
        f.sent = connect(
            &mut f.dev,
            &mut f.out,
            hello(10, RuntimeTarget::DesktopHost, Domains::NONE),
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
    fn state_kept_as_the_session_stops_leaves_the_sensitive_out_and_restores_next_time() {
        const SECRET: &str = "component Counter {\n    state count = 0;\n    @sensitive state token = \"\";\n    view {\n        Column {\n            Text { width: 120dp; text: token; }\n        }\n    }\n}\n";
        let mut f = Fixture::app("keep", true, SECRET, SECRET, Domains::NONE);
        let _ = f.settle();
        let dev = f.dev.root().join("target/viso/dev");
        assert!(
            !f.dev.keep_on_stop(BuildId(2), "host"),
            "not without the flag"
        );
        f.dev.keep_state(dev.clone());
        assert!(f.dev.keep_on_stop(BuildId(2), "host"));
        let Ok(HostMessage::Snapshot(Some(redactions))) = f.sent.try_recv() else {
            panic!("a snapshot for the disk");
        };
        let [redaction] = &redactions[..] else {
            panic!("{redactions:?}");
        };
        assert_eq!(redaction.file, FileId(0));
        assert_eq!(redaction.states.len(), 1, "`token`");
        assert_eq!(redaction.nodes, [1], "the text reading it");
        assert!(!f.dev.kept(BuildId(2)));

        let bytes: Vec<u8> = (0..40).collect();
        for chunk in blob_chunks(&bytes) {
            f.runtime(RuntimeMessage::Blob(chunk));
        }
        let window = WindowGeometry {
            size: (640.0, 480.0),
            position: None,
        };
        f.runtime(RuntimeMessage::Snapshot {
            blob: BlobRef::of(&bytes),
            windows: vec![window],
            capture_us: 1,
        });
        assert!(f.dev.kept(BuildId(2)));
        assert!(!f.dev.has_snapshot(), "kept, not held for a restart");
        let file = kept::path(&dev, "host");
        assert!(file.exists());

        // The next session restores it into its first launch.
        let old = f.launch();
        f.dev.link(LinkEvent::Closed(old), &mut f.out);
        assert!(!f.dev.restore_kept(BuildId(2), "android", &mut f.out));
        assert!(f.dev.restore_kept(BuildId(2), "host", &mut f.out));
        assert!(!file.exists(), "taken");
        assert_eq!(f.dev.launch_window(), Some(window));
        f.sent = connect(
            &mut f.dev,
            &mut f.out,
            hello(10, RuntimeTarget::DesktopHost, Domains::NONE),
        );
        f.runtime(RuntimeMessage::Mounts(vec![MountEntry {
            file: FileId(0),
            path: f.path.display().to_string(),
            package: "app".into(),
            module: vec!["view".into()],
            language: None,
            catalog: None,
            capabilities: Vec::new(),
            source_hash: source_hash(SECRET),
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
        assert_eq!(blobs.take(restore.snapshot).as_deref(), Some(&bytes[..]));
        let [view] = &restore.ui.views[..] else {
            panic!("{:?}", restore.ui);
        };
        assert!(!view.plan.states.is_empty(), "planned from the kept text");
        f.runtime(RuntimeMessage::Restored(RestoreReport::default()));
        assert!(!f.dev.restarting(), "the restore landed");
        let _ = std::fs::remove_dir_all(f.dev.root());
    }

    /// `f`'s app restarting on a new build with a snapshot held, its old
    /// connection closed and the new one launched to restore.
    fn relaunched_to_restore(f: &mut Fixture) {
        let built = Build {
            result: Ok(PathBuf::from("app")),
            elapsed: Duration::from_millis(1),
            files: Vec::new(),
            unchanged: false,
        };
        assert!(f.dev.rebuilt(BuildId(2), &built, &mut f.out));
        assert!(f.dev.request_snapshot(BuildId(2)));
        let bytes = [1, 2, 3];
        for chunk in blob_chunks(&bytes) {
            f.runtime(RuntimeMessage::Blob(chunk));
        }
        f.runtime(RuntimeMessage::Snapshot {
            blob: BlobRef::of(&bytes),
            windows: Vec::new(),
            capture_us: 1,
        });
        let old = f.launch();
        f.dev.link(LinkEvent::Closed(old), &mut f.out);
        assert!(f.dev.restore_next());
        f.dev.relaunched(&mut f.out);
        assert!(f.dev.landing && f.dev.restarting.is_some());
    }

    #[test]
    fn a_restart_whose_app_never_restores_still_ends() {
        // Refused at the handshake: another Viso compiler built it.
        let mut f = Fixture::new("refused");
        relaunched_to_restore(&mut f);
        f.dev.link(LinkEvent::Refused("schema".into()), &mut f.out);
        assert!(!f.dev.landing && f.dev.restarting.is_none());
        assert!(!f.dev.restore_next, "no later app takes the snapshot");

        // Exited before it connected.
        let mut f = Fixture::new("exited");
        relaunched_to_restore(&mut f);
        f.dev.ended(&mut f.out);
        assert!(!f.dev.landing && f.dev.restarting.is_none());

        // The session ended while the old app was still closing.
        let mut f = Fixture::new("interrupted");
        let built = Build {
            result: Ok(PathBuf::from("app")),
            elapsed: Duration::from_millis(1),
            files: Vec::new(),
            unchanged: false,
        };
        assert!(f.dev.rebuilt(BuildId(2), &built, &mut f.out));
        f.dev.ended(&mut f.out);
        assert!(f.dev.restarting.is_none());
    }

    #[test]
    fn an_old_channel_closing_after_the_relaunch_leaves_the_restart_landing() {
        // A page's channel closes once the new build is served, after the
        // session relaunched it.
        let mut f = Fixture::new("page");
        let built = Build {
            result: Ok(PathBuf::from("app")),
            elapsed: Duration::from_millis(1),
            files: Vec::new(),
            unchanged: false,
        };
        assert!(f.dev.rebuilt(BuildId(2), &built, &mut f.out));
        assert!(f.dev.request_snapshot(BuildId(2)));
        let bytes = [1, 2, 3];
        for chunk in blob_chunks(&bytes) {
            f.runtime(RuntimeMessage::Blob(chunk));
        }
        f.runtime(RuntimeMessage::Snapshot {
            blob: BlobRef::of(&bytes),
            windows: Vec::new(),
            capture_us: 1,
        });
        let old = f.launch();
        assert!(f.dev.restore_next());
        f.dev.relaunched(&mut f.out);
        f.dev.link(LinkEvent::Closed(old), &mut f.out);
        assert!(f.dev.landing && f.dev.restarting.is_some(), "still landing");
    }

    #[test]
    fn a_closed_runtime_is_forgotten() {
        let mut f = Fixture::new("closed");
        let launch = f.launch();
        f.dev.link(LinkEvent::Closed(launch), &mut f.out);
        f.save(&VIEW.replace("120dp", "190dp"));
        assert!(f.settle().is_empty());
    }

    /// The mount of `view.vs` at `path` as the build of `built` reports it,
    /// with `catalog` its package's catalogs.
    fn mount_entry(path: &Path, built: &str, catalog: Option<(String, String)>) -> MountEntry {
        MountEntry {
            file: FileId(0),
            path: path.display().to_string(),
            package: "app".into(),
            module: vec!["view".into()],
            language: None,
            catalog,
            capabilities: Vec::new(),
            source_hash: source_hash(built),
            assets: None,
        }
    }

    /// `f` with a page of its web lane, a build of its own, connected
    /// beside its desktop app, mounting `view.vs` as built; returns what
    /// the page is sent.
    fn with_web(f: &mut Fixture) -> Receiver<HostMessage> {
        let page = RuntimeHello {
            build_id: BuildId(3),
            ..hello(11, RuntimeTarget::Web, Domains::NONE)
        };
        let sent = connect(&mut f.dev, &mut f.out, page);
        let entry = mount_entry(&f.path, VIEW, None);
        f.dev.link(
            LinkEvent::Runtime(RuntimeSessionId(11), RuntimeMessage::Mounts(vec![entry])),
            &mut f.out,
        );
        sent
    }

    fn ack_of(revision: u64) -> RuntimeMessage {
        RuntimeMessage::Ack(PatchAck {
            revision: Revision(revision),
            applied_domains: Domains::NONE.with(Domain::Ui),
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
        })
    }

    #[test]
    fn one_edit_reaches_every_runtime_and_a_refusal_keeps_only_that_one_on_its_last_good() {
        let mut f = Fixture::new("matrix");
        let web = with_web(&mut f);
        assert!(f.settle().is_empty() && web.try_iter().next().is_none());
        let (desktop, page) = (RuntimeSessionId(9), RuntimeSessionId(11));

        // One edit, one candidate revision, a patch to each runtime packed
        // for its own GPU backend.
        let first = VIEW.replace("120dp", "140dp");
        f.save(&first);
        let to_desktop = f.settle();
        let to_web: Vec<_> = web.try_iter().collect();
        patches(&to_desktop[0], (1, 2), VIEW, &first);
        let (base, next, view) = patch(&to_web[0]);
        assert_eq!((base, next), (1, 2), "the same candidate revision");
        assert_eq!(
            *view,
            view_patch(
                FileId(0),
                Some(&compiled(VIEW)),
                &compiled(&first),
                &[viso_dev_protocol::wire::ShaderLang::Wgsl],
            )
            .unwrap()
        );

        // The page's GPU refuses it; the desktop app commits it.
        f.dev
            .link(LinkEvent::Runtime(desktop, ack_of(2)), &mut f.out);
        f.dev.link(
            LinkEvent::Runtime(
                page,
                RuntimeMessage::Nack(PatchNack {
                    base_revision: Revision(1),
                    candidate_revision: Revision(2),
                    stage: Stage::ShaderCompile,
                    diagnostic_codes: vec!["E3701".into()],
                    last_good_revision: Revision(1),
                }),
            ),
            &mut f.out,
        );
        let [d, w] = &f.dev.runtimes[..] else {
            panic!("two runtimes");
        };
        assert_eq!((d.revision, w.revision), (Revision(2), Revision(1)));
        assert_eq!(
            w.patch.as_ref().map(|p| (p.outcome, p.stage)),
            Some((Outcome::Rejected, Stage::ShaderCompile))
        );
        assert_eq!(d.patch.as_ref().map(|p| p.outcome), Some(Outcome::Applied));
        assert!(f.settle().is_empty(), "nothing is resent");
        assert!(
            web.try_iter().next().is_none(),
            "the refused edit is settled"
        );

        // The next edit moves each from what it runs.
        let second = VIEW.replace("120dp", "160dp");
        f.save(&second);
        let to_desktop = f.settle();
        let to_web: Vec<_> = web.try_iter().collect();
        patches(&to_desktop[0], (2, 3), &first, &second);
        let (base, next, view) = patch(&to_web[0]);
        assert_eq!((base, next), (1, 3));
        assert_eq!(
            *view,
            view_patch(
                FileId(0),
                Some(&compiled(VIEW)),
                &compiled(&second),
                &[viso_dev_protocol::wire::ShaderLang::Wgsl],
            )
            .unwrap(),
            "planned from the page's last good"
        );
    }

    #[test]
    fn a_runtime_whose_patch_is_in_flight_takes_later_edits_after_its_answer() {
        let mut f = Fixture::new("staggered");
        let web = with_web(&mut f);
        let _ = f.settle();
        let first = VIEW.replace("120dp", "140dp");
        f.save(&first);
        let _ = f.settle();
        let _ = web.try_iter().count();
        // Only the desktop app answers before the next edit.
        f.ack(2);
        let second = VIEW.replace("120dp", "150dp");
        f.save(&second);
        patches(&f.settle()[0], (2, 3), &first, &second);
        assert!(
            web.try_iter().next().is_none(),
            "the page's patch is in flight"
        );
        f.dev.link(
            LinkEvent::Runtime(RuntimeSessionId(11), ack_of(2)),
            &mut f.out,
        );
        let _ = f.settle();
        let (base, next, _) = patch(&web.try_recv().expect("the page's next patch"));
        assert_eq!(
            (base, next),
            (2, 4),
            "the page catches up at the next revision"
        );
    }

    #[test]
    fn a_failure_a_runtime_alone_compiles_to_is_shown_on_it_alone() {
        // The desktop build bundles the catalogs the view names; the page's
        // does not.
        let mut f = Fixture::new("profiles");
        let i18n = f.path.parent().unwrap().join("i18n");
        std::fs::create_dir_all(&i18n).unwrap();
        let i18n = i18n.canonicalize().unwrap();
        let view = "import viso::i18n::tr;\ncomponent Counter {\n    view { Text { text: tr(\"title\"); } }\n}\n";
        let catalog = Some(("en".to_owned(), i18n.display().to_string()));
        f.dev.change(
            Change {
                path: i18n.join("en.toml"),
                bytes: b"title = \"Title\"\n".to_vec(),
                rust: false,
            },
            &mut f.out,
        );
        f.runtime(RuntimeMessage::Mounts(vec![mount_entry(
            &f.path, VIEW, catalog,
        )]));
        let web = with_web(&mut f);
        let _ = f.settle();
        f.save(view);
        let to_desktop = f.settle();
        let to_web: Vec<_> = web.try_iter().collect();
        assert!(
            matches!(to_desktop.last(), Some(HostMessage::Patch(_))),
            "{to_desktop:?}"
        );
        let [HostMessage::Failure { lines, .. }] = &to_web[..] else {
            panic!("{to_web:?}");
        };
        assert!(!lines.is_empty());
        let [d, w] = &f.dev.runtimes[..] else {
            panic!("two runtimes");
        };
        assert!(!d.mounts.views[&0].failing && w.mounts.views[&0].failing);
    }

    #[test]
    fn a_catalog_saved_on_disk_reaches_the_views_through_the_watcher() {
        let root =
            std::env::temp_dir().join(format!("viso-dev-{}-watched-catalog", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("i18n")).unwrap();
        let root = root.canonicalize().unwrap();
        let (path, catalog) = (root.join("view.vs"), root.join("i18n/en.toml"));
        let view = "import viso::i18n::tr;\ncomponent Counter {\n    view { Text { text: tr(\"title\"); } }\n}\n";
        std::fs::write(&path, view).unwrap();
        std::fs::write(&catalog, "title = \"Inbox\"\n").unwrap();
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
            builds: vec![BuildId(2)],
            project: ProjectFingerprint(3),
            schema: SchemaFingerprint(4),
        };
        let scope = WatchScope {
            project: root.clone(),
            rust: RustScope::default(),
        };
        let (deliver, changes) = mpsc::channel();
        let mut dev = DevSession::start(scope, &expect, vec![lock], true, move |change| {
            deliver.send(change).is_ok()
        });
        let mut out = Counted::default();
        let sent = connect(
            &mut dev,
            &mut out,
            hello(9, RuntimeTarget::DesktopHost, Domains::NONE),
        );
        // Hands the session what the watcher delivers until `path` arrives,
        // as the run loop does.
        let take = |dev: &mut DevSession, out: &mut Counted, path: &Path| loop {
            let change = changes
                .recv_timeout(Duration::from_secs(5))
                .expect("the watcher delivers the file");
            let done = change.path == path;
            dev.change(change, out);
            if done {
                break;
            }
        };
        // Ticks the session until it sends something, or a second passes.
        let settle = |dev: &mut DevSession, out: &mut Counted| {
            let end = Instant::now() + Duration::from_secs(1);
            loop {
                if let Some(due) = dev.deadline() {
                    std::thread::sleep(due.saturating_duration_since(Instant::now()));
                    dev.tick(out);
                }
                let got: Vec<_> = sent.try_iter().collect();
                if !got.is_empty() || Instant::now() > end {
                    return got;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        take(&mut dev, &mut out, &catalog);
        let i18n = catalog.parent().unwrap().display().to_string();
        dev.link(
            LinkEvent::Runtime(
                RuntimeSessionId(9),
                RuntimeMessage::Mounts(vec![mount_entry(&path, view, Some(("en".into(), i18n)))]),
            ),
            &mut out,
        );
        let _ = settle(&mut dev, &mut out);

        // Past the watcher's settle window, so the edit is a new stamp.
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&catalog, "title = \"Outbox\"\n").unwrap();
        take(&mut dev, &mut out, &catalog);
        let sent_now = settle(&mut dev, &mut out);
        let [HostMessage::Patch(_)] = &sent_now[..] else {
            panic!("the views recompiled against the catalog: {sent_now:?}");
        };
        let (_, _, patched) = patch(&sent_now[0]);
        assert!(
            patched.package.behavior.windows(6).any(|w| w == b"Outbox"),
            "the patch carries the saved message"
        );
        assert_eq!(out.errors, 0);
        dev.link(LinkEvent::Runtime(RuntimeSessionId(9), ack_of(2)), &mut out);

        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&catalog, "title = \"Out{box\"\n").unwrap();
        take(&mut dev, &mut out, &catalog);
        let sent_now = settle(&mut dev, &mut out);
        assert!(out.errors > 0, "the broken catalog is reported");
        assert!(
            !sent_now.iter().any(|m| matches!(m, HostMessage::Patch(_))),
            "and patches nothing: {sent_now:?}"
        );
    }

    #[test]
    fn a_restart_lets_go_of_the_runtimes_of_its_lane_alone() {
        let mut f = Fixture::new("restart-matrix");
        let web = with_web(&mut f);
        let built = Build {
            result: Ok(PathBuf::from("app")),
            elapsed: Duration::from_millis(1),
            files: Vec::new(),
            unchanged: false,
        };
        assert!(f.dev.rebuilt(BuildId(2), &built, &mut f.out));
        assert_eq!(f.dev.rust, RustState::Restarting);
        assert!(
            f.dev.request_snapshot(BuildId(2)),
            "the app launched is asked"
        );
        assert!(matches!(f.sent.try_recv(), Ok(HostMessage::Snapshot(None))));
        assert!(web.try_iter().next().is_none(), "the page is not");
        f.dev.relaunched(&mut f.out);
        let [only] = &f.dev.runtimes[..] else {
            panic!("one runtime");
        };
        assert_eq!(only.hello.target, RuntimeTarget::Web);
        assert_eq!(f.dev.rust, RustState::Idle, "it restarted fresh");
    }
}
