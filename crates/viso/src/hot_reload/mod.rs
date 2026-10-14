//! The development session behind the `hot-reload` feature: the app's side of
//! `viso run`. Every `view!` a window mounts is adopted here and reported to
//! the host as a [`MountEntry`]; the host watches, compiles and plans the
//! project, and each edit it accepts arrives as a typed patch that commits to
//! the running windows as one hot reload transaction at the next frame
//! boundary (`Viso_Hot_Reload.md` §3, §9, §35–§37).
//!
//! The app watches no file, reads no project source and links no compiler. A
//! patch's `ui` section carries, for each reloaded file, the candidate view in
//! its release form and the plan that moves the file's mounts onto it. The
//! session loads every view of a patch when it stages the patch — each
//! behavior module decoded and verified once — so a patch that does not load
//! changes nothing and is NACKed, and one that does commits each view to each
//! of its mounts at the frame boundary, moves the revision and is ACKed with
//! what the commit kept and lost. A file mounted again after a patch committed
//! it (a window opened later) is rebuilt from the view the file now runs. An
//! edit the host rejected arrives as a failure, shown over the last-good UI of
//! each window mounting the file until the host clears it or a patch commits
//! the file. A patch's `system` section names the files whose games swap
//! with their views: staging links each candidate module, and the commit
//! proves the swap on every session running the file before anything else
//! commits — systems first, so a candidate whose start or smoke tick faults
//! refuses the whole revision with every game and view at its last good —
//! then commits the views and the swaps together at the tick boundary the
//! frame is at. The faults the games raise reach the host as log lines.
//! The host's questions about a running game (`viso game peek` and `record`)
//! are answered at the frame boundary from the first session running the
//! file; a recording restarts the World and answers with its tape once the
//! game ran the ticks asked for.
//! A patch's `resource` section names the assets it replaces, whose bytes
//! the link assembled and decoded off the loop: one that did not load
//! refuses the revision with the old resource kept, and the commit — after
//! the views, so a node a reloaded view adds takes the new resource too —
//! hands each to the app's assets, which every window showing it takes.
//! The shaders a patch's views paint with that no window's renderer holds
//! compile off the loop, with each window's compiler, before the patch is
//! committed: a program its layout cannot bind refuses the revision at
//! staging (`gpu-validate`), one the backend refuses when its compile ends
//! (`shader-compile`), and once all are built the commit hands them to the
//! renderers before the views commit, so the frame installs them without
//! compiling and the old pipelines retire through the fence.
//! Across a Rust warm restart the host asks for a [`DevSnapshot`], captured
//! at the next frame boundary after the patches commit: every view mount's
//! states, cells and node state, every game's snapshot, every window's size
//! and position, and what the app registered with `AppCx::dev_state`. The
//! channel opens with the launch window, so an app that mounts no view
//! carries its windows and extensions too. The process `viso run` launches
//! next waits, a bounded time, for the restore the host sends once it has
//! the mounts of the launch window: each mount of a window opened
//! before the first frame boundary then takes the view of the snapshot at
//! the same file, window and mount ordinal through the host's plan from the
//! view the old process ran, and its game the game's snapshot, and the
//! first frame boundary reports what carried.
//! Without `viso run` there is no session: the mounts are dropped.
//! Without the feature this module is not compiled and a `view!` records
//! nothing.

mod compile;
mod link;
pub(crate) mod overlay;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use viso_behavior::game::{GameSnapshot, PreparedSwap};
use viso_behavior::{Module, Vm};
use viso_gpu::GpuBackend;
use viso_platform::{Instant, LoopWaker, WindowId};
use viso_render::asset::{ResourceId, ResourceRevision};
use viso_runtime::RuntimeCx;
use viso_text::{PackagedFonts, ReplacedFiles};
use viso_ui::NodeId;
use viso_ui::context::DevState;
use viso_ui::state::{StateId, StateKey};
use viso_ui::state_origin;
use viso_ui::{AssetBytes, AssetPayload};
use viso_view::dev::commit::dirty_counts;
use viso_view::dev::commit::{Candidate, CommitReport, LiveRuntime, commit, restore, static_nodes};
use viso_view::dev::snapshot::{
    DevSnapshot, Extension, GameEntry, MountRef, RustCell, ViewSnapshot, WindowCells, capture_view,
};
use viso_view::dev::wire::{
    CommitCounts, DEV_RESTORE_ENV, DEV_WINDOW_ENV, DirtyCounts, Domain, Domains, FileCommit,
    FileId, GameAnswer, GameAsk, GameCommit, GameQuery, GameRefusal, GameReply, GameRestore,
    GameTier, LogLevel, MAX_FRAME, MAX_NOTICES, MAX_RECORD_TICKS, MAX_SCOPED_RESETS, MountEntry,
    NACK_REPLAYING, NACK_SYSTEM_FAULT, NACK_UNBINDABLE_SHADER, NACK_UNCOMPILABLE_SHADER,
    NACK_UNKNOWN_FILE, NACK_UNKNOWN_SESSION, NACK_UNLOADABLE_ASSET, NACK_UNLOADABLE_SYSTEM,
    NACK_UNLOADABLE_VIEW, NACK_UNRESTORABLE_SNAPSHOT, Notice, PatchAck, PatchBundle, PatchNack,
    PatchSection, PatchTimings, RESET_NOTICE, RecordSpan, ReloadPlan, ResourceCommit,
    RestoreBundle, RestoreReport, Revision, RuntimeIdentity, RuntimeMessage, SchemaFingerprint,
    ScopedReset, Stage, WindowGeometry,
};
use viso_view::{
    GameReloadError, GameSession, MountRecord, ViewHost, describe_fault, game_session, take_mounts,
};

use crate::WindowState;
use crate::assets::Assets;
use compile::{Built, ShaderCompiles, Work};
use link::{DevLink, Incoming, Loaded, LoadedAsset};

/// `config` for the launch window: in a process `viso run` restarted, where
/// and how large the first window of the process before it was.
pub(crate) fn launch_window(
    mut config: viso_platform::WindowConfig,
) -> viso_platform::WindowConfig {
    let geometry = viso_platform::launch_var(DEV_WINDOW_ENV)
        .and_then(|value| WindowGeometry::from_env(&value));
    if let Some(geometry) = geometry {
        config.logical_size = geometry.size;
        config.position = geometry.position;
    }
    config
}

/// How long a process launched to restore waits for the restore before it
/// runs fresh.
const RESTORE_WAIT: Duration = Duration::from_secs(5);

/// The compiler schema every view of the build compiled against.
const SCHEMA: SchemaFingerprint = SchemaFingerprint(viso_ui_macros::__schema_fingerprint!());

/// The domains this runtime applies.
const APPLIES: Domains = Domains::NONE
    .with(Domain::Ui)
    .with(Domain::System)
    .with(Domain::Resource);

/// The hot reload session of an app: its mounted views and their files, and
/// the dev channel to `viso run`, opened with the launch window.
#[derive(Default)]
pub(crate) struct HotReloadSession {
    /// Whether the environment was read for a dev channel.
    started: bool,
    link: Option<DevLink>,
    /// Who the host accepted this launch as, and the revision it matches.
    identity: Option<RuntimeIdentity>,
    /// Patches loaded and waiting for the frame boundary, in revision order.
    patches: Vec<StagedPatch>,
    /// Indexed by [`FileId`].
    files: Vec<ViewFile>,
    views: Vec<LiveView>,
    /// Whether a file's failure changed since the overlays were shown.
    failures_changed: bool,
    /// Scratch the mount queue drains into.
    records: Vec<MountRecord>,
    /// Scratch a commit frees subtrees with.
    scratch: Vec<NodeId>,
    /// The host's questions about games, answered at the frame boundary.
    queries: Vec<GameQuery>,
    /// The games being recorded for the host.
    recordings: Vec<Recording>,
    /// What wakes the loop, kept from the launch window.
    waker: Option<LoopWaker>,
    /// The thread compiling staged patches' shaders, started with the first.
    compiles: Option<ShaderCompiles>,
    /// Whether the host asked for a snapshot, taken at the next frame
    /// boundary.
    snapshot_asked: bool,
    /// Whether `viso run` launched this process to restore the one before it
    /// and the restore is still to be waited for.
    awaiting_restore: bool,
    /// The restore being applied, until the first frame boundary.
    restoring: Option<Restoring>,
    /// In a browser, which cannot wait: the launch window's adoption held
    /// for the restore.
    #[cfg(target_arch = "wasm32")]
    held: Option<HeldLaunch>,
    /// What the host delivered while the loop waited for the restore, taken
    /// with the next wakeup.
    deferred: VecDeque<Incoming>,
    /// What the app keeps across a restart, by name.
    dev_states: Vec<DevState>,
    /// The windows that mounted a view so far.
    windows_mounted: u32,
    /// Each window's cells allocated in Rust, outside its views, by where
    /// they were allocated, in the order the windows opened.
    cells: Vec<(WindowId, Vec<(StateId, StateKey)>)>,
    /// Scratch the allocation queue drains into.
    allocated: Vec<(StateId, StateKey)>,
    /// Whether the launch window was adopted.
    launched: bool,
    /// Where each window is on the desktop, read for the snapshot asked for.
    positions: Vec<(WindowId, Option<(f64, f64)>)>,
    /// The fonts the build packages.
    packaged: PackagedFonts,
    /// The packaged font files patches replaced, served to every window,
    /// one opened later included.
    fonts: ReplacedFiles,
}

/// A restore being applied: each window mounting a view before the first
/// frame boundary takes its mounts' state from the snapshot.
struct Restoring {
    snapshot: DevSnapshot,
    /// Each file the restore has a view for, with its plan from the view
    /// the old process ran.
    views: Vec<StagedView>,
    /// Whether a mount took each view of the snapshot.
    taken: Vec<bool>,
    report: RestoreReport,
    /// The time restoring took so far.
    spent: Duration,
}

/// A game recorded for the host's query `id` until it ran `until` ticks.
struct Recording {
    id: u32,
    session: Weak<RefCell<GameSession>>,
    until: u64,
}

/// A patch the runtime loaded, to commit at the next frame boundary.
struct StagedPatch {
    base_revision: Revision,
    next_revision: Revision,
    domains: Domains,
    views: Vec<StagedView>,
    games: Vec<StagedGame>,
    assets: Vec<StagedAsset>,
    fonts: Vec<StagedFont>,
    decode: Duration,
    stage: Duration,
    /// Whether its shaders are still compiling: it commits once they are not.
    compiling: bool,
    /// The programs its views paint with, built for the windows whose
    /// renderers did not hold them.
    programs: Vec<Built>,
}

/// An asset of a staged patch, decoded.
struct StagedAsset {
    id: ResourceId,
    bytes: AssetBytes,
    payload: AssetPayload,
}

/// A font of a staged patch, every face of it parsed.
struct StagedFont {
    id: ResourceId,
    /// Its path under `assets/`.
    path: String,
    bytes: Arc<[u8]>,
}

/// A game of a staged patch: its file, the tier it swaps at, and the
/// candidate module its view carries, linked once at stage time.
struct StagedGame {
    file: usize,
    tier: GameTier,
    module: Rc<Module>,
    /// The VM staging linked, for the first session; each other session of
    /// the file links its own at commit.
    vm: Option<Vm>,
}

/// A session's swap proved at commit, waiting for the rest of the revision.
struct ProvedSwap {
    game: usize,
    session: Rc<RefCell<GameSession>>,
    prepared: PreparedSwap,
}

/// A view of a staged patch: its file, its loaded candidate and the plan
/// that moves the file's mounts onto it.
struct StagedView {
    file: usize,
    candidate: Rc<Candidate>,
    plan: ReloadPlan,
}

/// A mounted `.vs` file.
struct ViewFile {
    /// What the file was reported to the host as.
    entry: MountEntry,
    /// The static shape of the view the build mounts.
    statics: &'static [u32],
    /// The view the file's mounts run once a patch committed one; `None`
    /// while they run the build's.
    current: Option<Rc<Candidate>>,
    /// The overlay lines of the latest edit while the host rejects it.
    failure: Option<Vec<String>>,
}

/// The rest of the launch window's adoption, held in a browser until the
/// restore arrives or [`RESTORE_WAIT`] passes: the window, its first view,
/// the cells its build allocated, and the restore and its snapshot's bytes
/// once they arrived.
#[cfg(target_arch = "wasm32")]
struct HeldLaunch {
    window: WindowId,
    first: usize,
    cells: Vec<(StateId, StateKey)>,
    deadline: Instant,
    restore: Option<Box<RestoreBundle>>,
    bytes: Option<Arc<[u8]>>,
}

/// One mount of a file in a window.
struct LiveView {
    window: WindowId,
    file: usize,
    root: NodeId,
    /// The live node of each static node, by static index; empty until the
    /// first commit names them for a view whose build recorded none.
    nodes: Vec<Option<NodeId>>,
    host: Option<Rc<RefCell<ViewHost>>>,
    /// Each state cell by its durable key.
    cells: Vec<(StateKey, StateId)>,
}

impl HotReloadSession {
    /// Adopts the views `ws`'s build just mounted, opening the dev channel
    /// `viso run` named with `waker` with the launch window's, whether or not
    /// it mounts a view, and reports the files not mounted before. A mount of
    /// a file a patch already committed is rebuilt from the view the file
    /// runs.
    pub(crate) fn adopt(&mut self, waker: impl FnOnce() -> LoopWaker, ws: &mut WindowState) {
        take_mounts(&mut self.records);
        state_origin::take_allocations(&mut self.allocated);
        let launch = !std::mem::replace(&mut self.launched, true);
        if !self.started {
            self.started = true;
            let waker = waker();
            self.waker = Some(waker.clone());
            self.link = DevLink::from_launch(waker, SCHEMA, APPLIES);
            self.awaiting_restore =
                self.link.is_some() && viso_platform::launch_var(DEV_RESTORE_ENV).is_some();
        }
        let Some(link) = &mut self.link else {
            self.records.clear();
            self.allocated.clear();
            return;
        };
        // A window opened after a font patch draws with the patched font too.
        for (file, bytes) in self.fonts.files() {
            ws.replace_packaged_font(file, Arc::clone(bytes), &mut self.scratch);
        }
        self.scratch.clear();
        let first = self.views.len();
        let mut mounted = Vec::new();
        for record in self.records.drain(..) {
            let file = match self
                .files
                .iter()
                .position(|file| file.entry.path == record.file)
            {
                Some(file) => file,
                None => {
                    let entry = entry(self.files.len(), &record);
                    mounted.push(entry.clone());
                    self.files.push(ViewFile {
                        entry,
                        statics: record.statics,
                        current: None,
                        failure: None,
                    });
                    self.files.len() - 1
                }
            };
            for &(key, id) in &record.cells {
                ws.states.bind_key(id, key);
            }
            let mut view = LiveView {
                window: ws.window,
                file,
                root: record.root,
                nodes: record.nodes,
                host: record.host,
                cells: record.cells,
            };
            if let Some(current) = &self.files[file].current {
                let plan = ReloadPlan::fresh(&current.package);
                commit_view(
                    ws,
                    &mut view,
                    record.statics,
                    current,
                    &plan,
                    None,
                    &mut self.scratch,
                );
            }
            self.views.push(view);
        }
        // The launch reports its mounts even when it has none: the host
        // sends a restore once it has them.
        if !mounted.is_empty() || launch {
            link.send(RuntimeMessage::Mounts(mounted));
        }
        if std::mem::take(&mut self.awaiting_restore) {
            // A page cannot wait for the restore: the rest of the adoption
            // waits for it instead, and the first frame boundary after it
            // arrives restores the window (`resume_restore`).
            #[cfg(target_arch = "wasm32")]
            {
                self.held = Some(HeldLaunch {
                    window: ws.window,
                    first,
                    cells: std::mem::take(&mut self.allocated),
                    deadline: Instant::now() + RESTORE_WAIT,
                    restore: None,
                    bytes: None,
                });
                return;
            }
            #[cfg(not(target_arch = "wasm32"))]
            self.await_restore();
        }
        if self.views.len() > first {
            self.restore_window(ws, first);
            self.windows_mounted += 1;
        }
        self.adopt_cells(ws, first);
    }

    /// Keeps the cells `ws`'s build allocated outside the views it mounted,
    /// `self.views[first..]`, and, restoring, gives each the value of the
    /// snapshot's cell of the same window, key and ordinal when it holds the
    /// same kind of value.
    fn adopt_cells(&mut self, ws: &mut WindowState, first: usize) {
        let views = &self.views[first..];
        let mut cells = std::mem::take(&mut self.allocated);
        cells.retain(|&(id, _)| {
            !views
                .iter()
                .any(|view| view.cells.iter().any(|&(_, cell)| cell == id))
        });
        if let Some(restoring) = &mut self.restoring {
            let started = Instant::now();
            let window = self.cells.len() as u32;
            if let Some(old) = restoring
                .snapshot
                .cells
                .iter()
                .find(|old| old.window == window)
            {
                let mut ordinals: Vec<(StateKey, u32)> = Vec::new();
                for &(id, key) in &cells {
                    let nth = next_key_ordinal(&mut ordinals, key);
                    let Some(now) = ws.states.get(id) else {
                        continue;
                    };
                    let value = old
                        .cells
                        .iter()
                        .find(|cell| cell.key == key && cell.nth == nth)
                        .map(|cell| cell.value)
                        .filter(|value| {
                            std::mem::discriminant(value) == std::mem::discriminant(&now)
                        });
                    if let Some(value) = value {
                        ws.states.set(id, value);
                        restoring.report.cells += 1;
                    }
                }
            }
            restoring.spent += started.elapsed();
        }
        self.cells.push((ws.window, cells));
    }

    /// Whether the host asked for a snapshot, which the next frame boundary
    /// takes with each window's position.
    pub(crate) fn snapshot_asked(&self) -> bool {
        self.snapshot_asked
    }

    /// Where each window is on the desktop, for the snapshot asked for.
    pub(crate) fn set_positions(&mut self, positions: Vec<(WindowId, Option<(f64, f64)>)>) {
        self.positions = positions;
    }

    /// Keeps what the app registered with `AppCx::dev_state`.
    pub(crate) fn set_dev_states(&mut self, states: Vec<DevState>) {
        self.dev_states = states;
    }

    /// Keeps the fonts the build packages, which a patch's fonts replace.
    pub(crate) fn set_packaged_fonts(&mut self, packaged: PackagedFonts) {
        self.packaged = packaged;
    }

    /// Waits, at most [`RESTORE_WAIT`], for the restore the host sends once
    /// it has the first mounts, keeping what else arrives for the next
    /// wakeup; without one the app runs fresh.
    #[cfg(not(target_arch = "wasm32"))]
    fn await_restore(&mut self) {
        let Some(link) = &self.link else {
            return;
        };
        let deadline = Instant::now() + RESTORE_WAIT;
        let mut restore = None;
        while let Some(incoming) = link.wait(deadline) {
            match incoming {
                Incoming::Connected(identity) => self.identity = Some(identity),
                Incoming::Restore(bundle, bytes) => {
                    restore = Some((bundle, bytes));
                    break;
                }
                other => self.deferred.push_back(other),
            }
        }
        match restore {
            Some((bundle, bytes)) => self.load_restore(*bundle, bytes),
            None => self.log(
                LogLevel::Warn,
                "no restore arrived in time: the app runs fresh".into(),
            ),
        }
    }

    /// Restores the launch window held for the restore once it arrived, or
    /// adopts it fresh once [`RESTORE_WAIT`] passed without one.
    #[cfg(target_arch = "wasm32")]
    fn resume_restore(&mut self, windows: &mut [WindowState]) {
        let Some(held) = &self.held else {
            return;
        };
        if held.restore.is_none() && Instant::now() < held.deadline {
            return;
        }
        let mut held = self.held.take().expect("held");
        match held.restore.take() {
            Some(bundle) => self.load_restore(*bundle, held.bytes.take()),
            None => self.log(
                LogLevel::Warn,
                "no restore arrived in time: the app runs fresh".into(),
            ),
        }
        let Some(ws) = windows.iter_mut().find(|ws| ws.window == held.window) else {
            return;
        };
        if self.views.len() > held.first {
            self.restore_window(ws, held.first);
            self.windows_mounted += 1;
        }
        self.allocated = held.cells;
        self.adopt_cells(ws, held.first);
    }

    /// Takes the restore `bundle` and its snapshot's `bytes`, for the
    /// windows opened before the first frame boundary: each of its views
    /// loaded, and each extension the app registers given its bytes. One that
    /// is not for this launch, or whose snapshot or a view does not load, is
    /// NACKed and the app runs fresh.
    fn load_restore(&mut self, bundle: RestoreBundle, bytes: Option<Arc<[u8]>>) {
        let started = Instant::now();
        let Some(identity) = self.identity.clone() else {
            return;
        };
        let refused = match self.restorable(&bundle, &identity, bytes) {
            Ok((snapshot, views)) => {
                let mut report = RestoreReport::default();
                for extension in &snapshot.extensions {
                    match self
                        .dev_states
                        .iter_mut()
                        .find(|state| state.name == extension.name)
                    {
                        Some(state) => {
                            let restore = std::mem::replace(&mut state.restore, Box::new(|_| {}));
                            restore(&extension.bytes);
                            report.extensions += 1;
                        }
                        None => report.extensions_lost += 1,
                    }
                }
                self.restoring = Some(Restoring {
                    taken: vec![false; snapshot.views.len()],
                    snapshot,
                    views,
                    report,
                    spent: started.elapsed(),
                });
                return;
            }
            Err(refused) => refused,
        };
        let (code, line) = refused;
        self.log(LogLevel::Error, format!("{line}; the app runs fresh"));
        let last_good = identity.current_revision;
        if let Some(link) = &mut self.link {
            link.send(RuntimeMessage::Nack(PatchNack {
                base_revision: Revision::LAUNCH,
                candidate_revision: Revision::LAUNCH,
                stage: Stage::SnapshotRestore,
                diagnostic_codes: vec![code.into()],
                last_good_revision: last_good,
            }));
        }
    }

    /// The snapshot and loaded views of `bundle`, or the code and line
    /// refusing it.
    fn restorable(
        &self,
        bundle: &RestoreBundle,
        identity: &RuntimeIdentity,
        bytes: Option<Arc<[u8]>>,
    ) -> Result<(DevSnapshot, Vec<StagedView>), (&'static str, String)> {
        if bundle.dev_session != identity.dev_session
            || bundle.target_runtime != identity.runtime_session
        {
            return Err((
                NACK_UNKNOWN_SESSION,
                "the restore is for another launch".into(),
            ));
        }
        let bytes = bytes.ok_or((
            NACK_UNRESTORABLE_SNAPSHOT,
            "the snapshot did not arrive whole".into(),
        ))?;
        let snapshot = DevSnapshot::decode(&bytes).map_err(|error| {
            (
                NACK_UNRESTORABLE_SNAPSHOT,
                format!("the snapshot does not decode: {error}"),
            )
        })?;
        let mut views = Vec::with_capacity(bundle.ui.views.len());
        for view in &bundle.ui.views {
            let file = view.file.0 as usize;
            let Some(entry) = self.files.get(file).map(|file| &file.entry) else {
                return Err((
                    NACK_UNKNOWN_FILE,
                    "the restore names an unmounted file".into(),
                ));
            };
            let candidate = Candidate::load(view.package.clone()).map_err(|error| {
                (
                    NACK_UNLOADABLE_VIEW,
                    format!("{}: the restored view does not load: {error}", entry.path),
                )
            })?;
            views.push(StagedView {
                file,
                candidate: Rc::new(candidate),
                plan: view.plan.clone(),
            });
        }
        Ok((snapshot, views))
    }

    /// Restores the mounts `ws`'s build just made, `self.views[first..]`:
    /// each takes the view of the snapshot at its file, window ordinal and
    /// mount ordinal through its file's plan, and its game the game's
    /// snapshot. A mount with no counterpart, or of a file the restore has no
    /// view for, runs fresh.
    fn restore_window(&mut self, ws: &mut WindowState, first: usize) {
        let Some(restoring) = &mut self.restoring else {
            return;
        };
        let started = Instant::now();
        let window = self.windows_mounted;
        let mut ordinals: Vec<(usize, u32)> = Vec::new();
        for live in &mut self.views[first..] {
            let file = live.file;
            let at = MountRef {
                path: self.files[file].entry.path.clone(),
                window,
                mount: next_ordinal(&mut ordinals, file),
            };
            let report = &mut restoring.report;
            let counts = match report.files.iter().position(|f| f.file.0 as usize == file) {
                Some(index) => &mut report.files[index].counts,
                None => {
                    report.files.push(FileCommit {
                        file: FileId(file as u32),
                        counts: CommitCounts::default(),
                    });
                    &mut report.files.last_mut().expect("pushed").counts
                }
            };
            counts.mounts += 1;
            let Some(view) = restoring.views.iter().find(|view| view.file == file) else {
                continue;
            };
            let Some(index) = (0..restoring.snapshot.views.len())
                .find(|&i| !restoring.taken[i] && restoring.snapshot.views[i].mount == at)
            else {
                continue;
            };
            restoring.taken[index] = true;
            let committed = commit_view(
                ws,
                live,
                self.files[file].statics,
                &view.candidate,
                &view.plan,
                Some(&restoring.snapshot.views[index]),
                &mut self.scratch,
            );
            tally(
                counts,
                &mut report.notices,
                &mut report.state_resets,
                file,
                committed,
            );
            let Some(game) = restoring.snapshot.games.iter().find(|g| g.mount == at) else {
                continue;
            };
            let Some(session) = game_session(&ws.store, live.root) else {
                continue;
            };
            let Ok(snapshot) = GameSnapshot::decode(&game.snapshot) else {
                continue;
            };
            let restored = session.borrow_mut().scheduler_mut().restore(&snapshot);
            let file = FileId(file as u32);
            let game = match report.games.iter().position(|g| g.file == file) {
                Some(index) => &mut report.games[index],
                None => {
                    report.games.push(GameRestore {
                        file,
                        sessions: 0,
                        tick: snapshot.tick(),
                        states: 0,
                        reset: 0,
                        degraded: false,
                    });
                    report.games.last_mut().expect("pushed")
                }
            };
            game.sessions += 1;
            game.states += restored.states;
            game.reset += restored.mismatched + restored.missing;
            game.degraded |= restored.degraded;
        }
        restoring.spent += started.elapsed();
    }

    /// Reports the restore applied to the windows opened before the first
    /// frame boundary, which ends it.
    fn finish_restore(&mut self) {
        let Some(restoring) = self.restoring.take() else {
            return;
        };
        let mut report = restoring.report;
        report.views_lost = restoring.taken.iter().filter(|&&taken| !taken).count() as u32;
        let held: usize = restoring.snapshot.cells.iter().map(|w| w.cells.len()).sum();
        report.cells_lost = (held as u32).saturating_sub(report.cells);
        report.restore_us = micros(restoring.spent);
        if let Some(link) = &mut self.link {
            link.send(RuntimeMessage::Restored(report));
        }
    }

    /// The app's development state now, at the frame boundary, to the host:
    /// each window that mounts a view, in the order they opened, with its
    /// mounts in the order they were adopted and their games, then each
    /// extension the app registered.
    fn capture(&mut self, windows: &[WindowState]) {
        let started = Instant::now();
        let Some(identity) = &self.identity else {
            return;
        };
        let mut snapshot = DevSnapshot {
            source_build: identity.build_id,
            app_schema: SCHEMA,
            revision: identity.current_revision,
            ..DevSnapshot::default()
        };
        let mut ordinals: Vec<(usize, u32)> = Vec::new();
        let mut geometry = Vec::with_capacity(windows.len());
        let mut window = 0;
        for ws in windows {
            let scale = if ws.dpi > 0.0 { f64::from(ws.dpi) } else { 1.0 };
            let (w, h) = ws.surface_size;
            geometry.push(WindowGeometry {
                size: (f64::from(w) / scale, f64::from(h) / scale),
                position: self
                    .positions
                    .iter()
                    .find(|(id, _)| *id == ws.window)
                    .and_then(|&(_, at)| at),
            });
            ordinals.clear();
            for live in self.views.iter().filter(|live| live.window == ws.window) {
                let file = &self.files[live.file];
                let at = MountRef {
                    path: file.entry.path.clone(),
                    window,
                    mount: next_ordinal(&mut ordinals, live.file),
                };
                let nodes = if live.nodes.is_empty() {
                    static_nodes(&ws.store, live.root, file.statics)
                } else {
                    live.nodes.clone()
                };
                let host = live.host.as_ref().map(|host| host.borrow());
                snapshot.views.push(capture_view(
                    &ws.store,
                    &ws.states,
                    &ws.text_edits,
                    host.as_deref(),
                    &nodes,
                    &live.cells,
                    at.clone(),
                ));
                if let Some(session) = game_session(&ws.store, live.root) {
                    snapshot.games.push(GameEntry {
                        mount: at,
                        snapshot: session.borrow().scheduler().snapshot().encode(),
                    });
                }
            }
            if !ordinals.is_empty() {
                window += 1;
            }
        }
        // Each window's Rust cells, by its order among the windows open.
        let open = windows
            .iter()
            .filter_map(|ws| Some((ws, &self.cells.iter().find(|(id, _)| *id == ws.window)?.1)));
        for (window, (ws, cells)) in open.enumerate() {
            let mut ordinals: Vec<(StateKey, u32)> = Vec::new();
            let cells = cells
                .iter()
                .filter_map(|&(id, key)| {
                    let nth = next_key_ordinal(&mut ordinals, key);
                    Some(RustCell {
                        key,
                        nth,
                        value: ws.states.get(id)?,
                    })
                })
                .collect::<Vec<_>>();
            if !cells.is_empty() {
                snapshot.cells.push(WindowCells {
                    window: window as u32,
                    cells,
                });
            }
        }
        self.positions.clear();
        for state in &self.dev_states {
            snapshot.extensions.push(Extension {
                name: state.name.clone(),
                bytes: (state.capture)(),
            });
        }
        let bytes = snapshot.encode();
        let capture_us = micros(started.elapsed());
        if let Some(link) = &mut self.link {
            link.send_snapshot(bytes, geometry, capture_us);
        }
    }

    /// Sends `line` to the host's log.
    fn log(&mut self, level: LogLevel, line: String) {
        if let Some(link) = &mut self.link {
            link.send(RuntimeMessage::Log { level, line });
        }
    }

    /// Takes what the dev channel delivered and what the shader compiles
    /// finished, and requests a frame for each window with a mount when a
    /// patch became ready to commit or a failure changed, and for every
    /// window when the host asked for a snapshot, which captures them all.
    /// `windows` compile the shaders a patch brings.
    pub(crate) fn wakeup(&mut self, cx: &mut RuntimeCx<'_>, windows: &[WindowState]) {
        if !self.receive(windows) {
            return;
        }
        if self.snapshot_asked {
            for ws in windows {
                cx.request_redraw(ws.window);
            }
            return;
        }
        let mut asked: Vec<WindowId> = Vec::new();
        for view in &self.views {
            if !asked.contains(&view.window) {
                asked.push(view.window);
                cx.request_redraw(view.window);
            }
        }
    }

    /// Takes what the dev channel delivered: the host's acceptance, patches,
    /// each checked against the revision the runtime will match and loaded
    /// before it is staged, its new shaders validated against and handed to
    /// the compilers of `windows`, and NACKed when any fails (§36, §38), and
    /// failures; then the patches whose compiles finished. Returns whether a
    /// patch became ready to commit or a failure changed.
    fn receive(&mut self, windows: &[WindowState]) -> bool {
        let Some(link) = &mut self.link else {
            return false;
        };
        let mut changed = false;
        while let Some(incoming) = self.deferred.pop_front().or_else(|| link.poll()) {
            match incoming {
                Incoming::Connected(identity) => self.identity = Some(identity),
                Incoming::Patch(bundle, assets, decode) => {
                    let Some(identity) = &self.identity else {
                        continue;
                    };
                    let started = Instant::now();
                    // A patch may chain onto one staged before it.
                    let mut expected = identity.clone();
                    if let Some(last) = self.patches.last() {
                        expected.current_revision = last.next_revision;
                    }
                    let staged = expected.check(&bundle).and_then(|()| {
                        for asset in &assets {
                            if let Err(why) = &asset.loaded {
                                link.send(RuntimeMessage::Log {
                                    level: LogLevel::Error,
                                    line: format!(
                                        "assets/{}: {why}; the app keeps the last good",
                                        asset.path
                                    ),
                                });
                            }
                        }
                        let mut staged = stage(*bundle, &self.files, assets)?;
                        let work = shader_work(&staged, windows, &self.files).map_err(
                            |(nack, line)| {
                                link.send(RuntimeMessage::Log {
                                    level: LogLevel::Error,
                                    line,
                                });
                                nack
                            },
                        )?;
                        if !work.is_empty() {
                            if self.compiles.is_none() {
                                let waker = self.waker.clone().unwrap_or_else(LoopWaker::inert);
                                self.compiles = ShaderCompiles::start(waker);
                            }
                            // Without a compile thread the frame compiles
                            // them, as it does a window's first.
                            staged.compiling = self
                                .compiles
                                .as_ref()
                                .is_some_and(|c| c.submit(staged.next_revision, work));
                        }
                        Ok(staged)
                    });
                    match staged {
                        Ok(mut staged) => {
                            staged.decode = decode;
                            staged.stage = started.elapsed();
                            changed |= !staged.compiling;
                            self.patches.push(staged);
                        }
                        Err(mut nack) => {
                            nack.last_good_revision = identity.current_revision;
                            link.send(RuntimeMessage::Nack(nack));
                        }
                    }
                }
                Incoming::Failure { file, lines } => {
                    let Some(file) = self.files.get_mut(file.0 as usize) else {
                        continue;
                    };
                    let failure = (!lines.is_empty()).then_some(lines);
                    if file.failure != failure {
                        file.failure = failure;
                        self.failures_changed = true;
                        changed = true;
                    }
                }
                Incoming::Game(query) => {
                    self.queries.push(query);
                    changed = true;
                }
                Incoming::Snapshot => {
                    self.snapshot_asked = true;
                    changed = true;
                }
                // The launch window waits for it in a browser.
                #[cfg(target_arch = "wasm32")]
                Incoming::Restore(bundle, bytes) if self.held.is_some() => {
                    if let Some(held) = &mut self.held {
                        held.restore = Some(bundle);
                        held.bytes = bytes;
                    }
                    changed = true;
                }
                // The app runs already: the state it would restore is not
                // the state it shows.
                Incoming::Restore(..) => {
                    if let Some(identity) = &self.identity {
                        link.send(RuntimeMessage::Log {
                            level: LogLevel::Warn,
                            line: "the restore arrived after the first frame: the app runs fresh"
                                .into(),
                        });
                        link.send(RuntimeMessage::Nack(PatchNack {
                            base_revision: Revision::LAUNCH,
                            candidate_revision: Revision::LAUNCH,
                            stage: Stage::SnapshotRestore,
                            diagnostic_codes: vec![NACK_UNRESTORABLE_SNAPSHOT.into()],
                            last_good_revision: identity.current_revision,
                        }));
                    }
                }
                Incoming::Undecodable(error) => {
                    if let Some(identity) = &self.identity {
                        link.send(RuntimeMessage::Nack(identity.undecodable(error)));
                    }
                }
            }
        }
        let Some(compiles) = &self.compiles else {
            return changed;
        };
        while let Some(finished) = compiles.poll() {
            let Some(at) = self
                .patches
                .iter()
                .position(|p| p.compiling && p.next_revision == finished.revision)
            else {
                continue;
            };
            match finished.built {
                Ok(built) => {
                    let staged = &mut self.patches[at];
                    staged.compiling = false;
                    staged.programs = built;
                }
                Err(refused) => {
                    let staged = self.patches.remove(at);
                    let last_good = self
                        .identity
                        .as_ref()
                        .map_or(Revision::default(), |identity| identity.current_revision);
                    link.send(RuntimeMessage::Log {
                        level: LogLevel::Error,
                        line: format!(
                            "{}: {}; the app keeps the last good",
                            self.files[refused.file].entry.path, refused.message
                        ),
                    });
                    link.send(RuntimeMessage::Nack(PatchNack {
                        base_revision: staged.base_revision,
                        candidate_revision: staged.next_revision,
                        stage: Stage::ShaderCompile,
                        diagnostic_codes: vec![NACK_UNCOMPILABLE_SHADER.into()],
                        last_good_revision: last_good,
                    }));
                }
            }
            changed = true;
        }
        changed
    }

    /// Drops the mounts of a closed window.
    pub(crate) fn close(&mut self, window: WindowId) {
        self.views.retain(|view| view.window != window);
        self.cells.retain(|&(cells, _)| cells != window);
    }

    /// Commits the staged patches in revision order at the frame boundary
    /// before the windows flush, answering each, up to the first whose
    /// shaders still compile; shows the failures that changed, and answers the
    /// host's questions about games. A mount recorded since the last adoption
    /// is not a window's build (a list row) and is not tracked.
    pub(crate) fn reload(&mut self, windows: &mut [WindowState], assets: &mut Assets) {
        #[cfg(target_arch = "wasm32")]
        self.resume_restore(windows);
        take_mounts(&mut self.records);
        self.records.clear();
        state_origin::clear_allocations();
        self.finish_restore();
        let ready = self.patches.iter().take_while(|p| !p.compiling).count();
        if ready > 0 {
            self.views.retain(|view| {
                windows
                    .iter()
                    .any(|ws| ws.window == view.window && ws.store.arena().is_live(view.root))
            });
        }
        let ready: Vec<StagedPatch> = self.patches.drain(..ready).collect();
        for staged in ready {
            let answer = self.commit_patch(staged, windows, assets);
            if let Some(link) = &mut self.link {
                link.send(answer);
            }
        }
        if std::mem::take(&mut self.failures_changed) {
            show_failures(&self.files, &self.views, windows, &mut self.scratch);
        }
        for query in std::mem::take(&mut self.queries) {
            if let Some(answer) = self.ask(query, windows)
                && let Some(link) = &mut self.link
            {
                link.send(RuntimeMessage::Game(answer));
            }
        }
        if !self.recordings.is_empty() {
            self.finish_recordings();
        }
        if std::mem::take(&mut self.snapshot_asked) {
            self.capture(windows);
        }
    }

    /// The sessions the mounts of `file` run, in mount order.
    fn sessions(&self, file: usize, windows: &[WindowState]) -> Vec<Rc<RefCell<GameSession>>> {
        let mut sessions: Vec<Rc<RefCell<GameSession>>> = Vec::new();
        for live in self.views.iter().filter(|live| live.file == file) {
            let Some(ws) = windows.iter().find(|ws| ws.window == live.window) else {
                continue;
            };
            if let Some(session) = game_session(&ws.store, live.root)
                && !sessions.iter().any(|s| Rc::ptr_eq(s, &session))
            {
                sessions.push(session);
            }
        }
        sessions
    }

    /// Answers `query` from the first session running its file; a recording
    /// it starts is answered once the game ran the ticks asked for.
    fn ask(&mut self, query: GameQuery, windows: &[WindowState]) -> Option<GameAnswer> {
        let answer = |reply| {
            Some(GameAnswer {
                id: query.id,
                reply,
            })
        };
        let sessions = self.sessions(query.file.0 as usize, windows);
        let Some(session) = sessions.first() else {
            return answer(Err(GameRefusal::NotRunning));
        };
        match query.ask {
            GameAsk::Peek { system, state } => {
                let session = session.borrow();
                answer(match session.state(&system, &state) {
                    Some(value) => Ok(GameReply::State {
                        value: value.encode(),
                        tick: session.scheduler().clock().tick(),
                        sessions: sessions.len() as u32,
                    }),
                    None => Err(GameRefusal::UnknownState),
                })
            }
            GameAsk::Record { span } => {
                let mut game = session.borrow_mut();
                if game.scheduler().replaying() {
                    return answer(Err(GameRefusal::Replaying));
                }
                if game.scheduler().recording() {
                    return answer(Err(GameRefusal::Recording));
                }
                let ticks = match span {
                    RecordSpan::Ticks(ticks) => ticks,
                    RecordSpan::Millis(ms) => {
                        let rate = u64::from(game.module().tick_rate());
                        ms.saturating_mul(rate).div_ceil(1000)
                    }
                };
                if let Err(fault) = game.record() {
                    let line = describe_fault(game.module(), &fault);
                    return answer(Err(GameRefusal::Fault(line)));
                }
                self.recordings.push(Recording {
                    id: query.id,
                    session: Rc::downgrade(session),
                    until: ticks.clamp(1, MAX_RECORD_TICKS),
                });
                None
            }
        }
    }

    /// Answers each recording whose game ran its ticks, or ended.
    fn finish_recordings(&mut self) {
        let mut answers = Vec::new();
        self.recordings.retain(|recording| {
            let reply = match recording.session.upgrade() {
                Some(session) => {
                    let mut session = session.borrow_mut();
                    if session.scheduler().clock().tick() < recording.until {
                        return true;
                    }
                    match session.stop_recording() {
                        // What the frame holds besides the tape fits in a
                        // few bytes.
                        Some(tape) => match tape.encode() {
                            tape if tape.len() <= MAX_FRAME - 64 => Ok(GameReply::Tape { tape }),
                            _ => Err(GameRefusal::TooLarge),
                        },
                        None => Err(GameRefusal::Ended),
                    }
                }
                None => Err(GameRefusal::Ended),
            };
            answers.push(GameAnswer {
                id: recording.id,
                reply,
            });
            false
        });
        if let Some(link) = &mut self.link {
            for answer in answers {
                link.send(RuntimeMessage::Game(answer));
            }
        }
    }

    /// Commits one staged patch: each view to each mount of its file. The
    /// ACK to send, or the NACK of a patch staged behind one that failed.
    fn commit_patch(
        &mut self,
        staged: StagedPatch,
        windows: &mut [WindowState],
        assets: &mut Assets,
    ) -> RuntimeMessage {
        let started = Instant::now();
        let Some(identity) = &mut self.identity else {
            unreachable!("a patch is staged only once the host accepted the launch");
        };
        if identity.current_revision != staged.base_revision {
            return RuntimeMessage::Nack(PatchNack {
                base_revision: staged.base_revision,
                candidate_revision: staged.next_revision,
                stage: Stage::RuntimeStage,
                diagnostic_codes: vec![viso_view::dev::wire::NACK_REVISION_MISMATCH.into()],
                last_good_revision: identity.current_revision,
            });
        }
        let last_good = identity.current_revision;
        let mut games = staged.games;
        // Systems first: every session's swap is proved before anything
        // commits, so a fault refuses the revision with nothing changed.
        // A game replaying a tape is deterministic replay: a live patch would
        // change what the tape reproduces.
        if let Some(path) = self.replaying(&games, windows) {
            let line = format!("{path}: the game replays a tape, so it takes no live patch");
            if let Some(link) = &mut self.link {
                link.send(RuntimeMessage::Log {
                    level: LogLevel::Warn,
                    line,
                });
            }
            return RuntimeMessage::Nack(PatchNack {
                base_revision: staged.base_revision,
                candidate_revision: staged.next_revision,
                stage: Stage::RuntimeStage,
                diagnostic_codes: vec![NACK_REPLAYING.into()],
                last_good_revision: last_good,
            });
        }
        let proved = match self.prove_games(&mut games, windows) {
            Ok(proved) => proved,
            Err(line) => {
                if let Some(link) = &mut self.link {
                    link.send(RuntimeMessage::Log {
                        level: LogLevel::Error,
                        line,
                    });
                }
                return RuntimeMessage::Nack(PatchNack {
                    base_revision: staged.base_revision,
                    candidate_revision: staged.next_revision,
                    stage: Stage::RuntimeCommit,
                    diagnostic_codes: vec![NACK_SYSTEM_FAULT.into()],
                    last_good_revision: last_good,
                });
            }
        };
        // Nothing refuses the revision from here: the renderers take the
        // programs its views paint with, which the frame installs.
        for built in staged.programs {
            if let Some(gpu) = windows
                .iter_mut()
                .find(|ws| ws.window == built.window)
                .and_then(|ws| ws.gpu.as_mut())
            {
                gpu.renderer
                    .adopt_program(&mut gpu.backend, &built.program, built.compiled);
            }
        }
        let Some(identity) = &mut self.identity else {
            unreachable!("checked above");
        };
        identity.current_revision = staged.next_revision;
        let revision = staged.next_revision;
        let mut files = Vec::with_capacity(staged.views.len());
        let mut notices = Vec::new();
        let mut state_resets = Vec::new();
        for view in staged.views {
            let mut counts = CommitCounts::default();
            let statics = self.files[view.file].statics;
            for live in self.views.iter_mut().filter(|live| live.file == view.file) {
                let Some(ws) = windows.iter_mut().find(|ws| ws.window == live.window) else {
                    continue;
                };
                let report = commit_view(
                    ws,
                    live,
                    statics,
                    &view.candidate,
                    &view.plan,
                    None,
                    &mut self.scratch,
                );
                counts.mounts += 1;
                tally(
                    &mut counts,
                    &mut notices,
                    &mut state_resets,
                    view.file,
                    report,
                );
            }
            files.push(FileCommit {
                file: FileId(view.file as u32),
                counts,
            });
            let file = &mut self.files[view.file];
            file.current = Some(view.candidate);
            if file.failure.take().is_some() {
                self.failures_changed = true;
            }
        }
        // The views committed: each asset replaces its last good in every
        // window, a node the views just mounted showing it included.
        let resource_revision = ResourceRevision(u32::try_from(revision.0).unwrap_or(u32::MAX));
        let mut resources = Vec::with_capacity(staged.assets.len());
        for asset in staged.assets {
            assets.replace(
                asset.id,
                resource_revision,
                asset.bytes,
                asset.payload,
                windows.iter_mut().map(|ws| &mut ws.store),
            );
            let mut commit = ResourceCommit {
                id: asset.id,
                nodes: 0,
                dirty: DirtyCounts::default(),
            };
            for ws in windows.iter_mut() {
                if let Some(gpu) = &mut ws.gpu {
                    assets.upload(&mut ws.store, &mut gpu.backend, &mut gpu.renderer);
                }
                let mut nodes = 0;
                let shown = ws.store.asset_nodes(asset.id).inspect(|_| nodes += 1);
                commit.dirty = commit.dirty.merge(&dirty_counts(&ws.store, shown));
                commit.nodes += nodes;
            }
            resources.push(commit);
        }
        // Each font replaces the packaged file at its path in every window:
        // only the paragraphs that laid out with a face of it shape again.
        for font in staged.fonts {
            let mut commit = ResourceCommit {
                id: font.id,
                nodes: 0,
                dirty: DirtyCounts::default(),
            };
            let Some(file) = self.packaged.file_for(&font.path) else {
                self.log(
                    LogLevel::Info,
                    format!(
                        "{} is not a font this build packages; the app uses it once rebuilt",
                        font.path
                    ),
                );
                resources.push(commit);
                continue;
            };
            self.fonts.replace(file, Arc::clone(&font.bytes));
            for ws in windows.iter_mut() {
                self.scratch.clear();
                ws.replace_packaged_font(file, Arc::clone(&font.bytes), &mut self.scratch);
                commit.nodes += u32::try_from(self.scratch.len()).unwrap_or(u32::MAX);
                commit.dirty = commit
                    .dirty
                    .merge(&dirty_counts(&ws.store, self.scratch.iter().copied()));
            }
            self.scratch.clear();
            resources.push(commit);
        }
        // The views committed: the games swap with them.
        let mut swapped: Vec<GameCommit> = Vec::with_capacity(games.len());
        for proof in proved {
            let game = &games[proof.game];
            let swap = proof.session.borrow_mut().commit(
                Rc::clone(&game.module),
                proof.prepared,
                revision.0,
            );
            let file = FileId(game.file as u32);
            let commit = match swapped.iter_mut().find(|c| c.file == file) {
                Some(commit) => commit,
                None => {
                    swapped.push(GameCommit {
                        file,
                        tier: game.tier,
                        sessions: 0,
                        tick: swap.tick,
                        carried: 0,
                        reset: 0,
                        characters: 0,
                    });
                    swapped.last_mut().expect("pushed")
                }
            };
            commit.sessions += 1;
            commit.carried += swap.carried;
            commit.reset += swap.reset;
            commit.characters += swap.characters;
        }
        RuntimeMessage::Ack(PatchAck {
            revision: staged.next_revision,
            applied_domains: staged.domains,
            files,
            notices,
            state_resets,
            games: swapped,
            resources,
            timings: PatchTimings {
                decode_us: micros(staged.decode),
                stage_us: micros(staged.stage),
                commit_us: micros(started.elapsed()),
            },
        })
    }
}

impl HotReloadSession {
    /// The path of a file of `games` one of whose sessions replays a tape.
    fn replaying(&self, games: &[StagedGame], windows: &[WindowState]) -> Option<&str> {
        let game = games.iter().find(|game| {
            self.views
                .iter()
                .filter(|live| live.file == game.file)
                .filter_map(|live| {
                    let ws = windows.iter().find(|ws| ws.window == live.window)?;
                    game_session(&ws.store, live.root)
                })
                .any(|session| session.borrow().scheduler().replaying())
        })?;
        Some(&self.files[game.file].entry.path)
    }

    /// Proves the swap of each game of a patch on every session running its
    /// file, in the windows' order: the proved swaps, or the line saying
    /// which game faulted, every swap proved before it put back.
    fn prove_games(
        &self,
        games: &mut [StagedGame],
        windows: &[WindowState],
    ) -> Result<Vec<ProvedSwap>, String> {
        let mut proved: Vec<ProvedSwap> = Vec::new();
        for (index, game) in games.iter_mut().enumerate() {
            for live in self.views.iter().filter(|live| live.file == game.file) {
                let Some(ws) = windows.iter().find(|ws| ws.window == live.window) else {
                    continue;
                };
                // A mount whose game did not start gets one with its view.
                let Some(session) = game_session(&ws.store, live.root) else {
                    continue;
                };
                if proved.iter().any(|p| Rc::ptr_eq(&p.session, &session)) {
                    continue;
                }
                let vm = match game.vm.take() {
                    Some(vm) => Ok(vm),
                    None => GameSession::stage(&game.module),
                };
                let prepared = vm
                    .map_err(GameReloadError::Link)
                    .and_then(|vm| session.borrow_mut().prepare(vm, game.tier.rebuild()));
                match prepared {
                    Ok(prepared) => proved.push(ProvedSwap {
                        game: index,
                        session,
                        prepared,
                    }),
                    Err(error) => {
                        for proof in proved.into_iter().rev() {
                            proof.session.borrow_mut().abort(proof.prepared);
                        }
                        let path = &self.files[game.file].entry.path;
                        let why = match &error {
                            GameReloadError::Game(viso_behavior::game::GameError::Fault(fault)) => {
                                describe_fault(&game.module, fault)
                            }
                            error => error.to_string(),
                        };
                        return Err(format!("{path}: the game was not swapped: {why}"));
                    }
                }
            }
        }
        Ok(proved)
    }

    /// Sends the host `lines`, the faults the games raised this frame: the
    /// lines it could not send, there being no dev channel.
    pub(crate) fn forward_game_faults(&mut self, lines: Vec<String>) -> Vec<String> {
        let Some(link) = &mut self.link else {
            return lines;
        };
        for line in lines {
            link.send(RuntimeMessage::Log {
                level: LogLevel::Error,
                line,
            });
        }
        Vec::new()
    }
}

/// Loads every view of `bundle`, which the runtime checked against its
/// revision, and takes the assets its `resource` section names, as the link
/// decoded them: a view naming a file the runtime did not report or whose
/// behavior does not load, or an asset that did not load, refuses the whole
/// patch.
fn stage(
    bundle: PatchBundle,
    files: &[ViewFile],
    loaded: Vec<LoadedAsset>,
) -> Result<StagedPatch, PatchNack> {
    let refuse_at = |stage, code: &str| PatchNack {
        base_revision: bundle.base_revision,
        candidate_revision: bundle.next_revision,
        stage,
        diagnostic_codes: vec![code.to_owned()],
        last_good_revision: Revision::default(),
    };
    let refuse = |code: &str| refuse_at(Stage::RuntimeStage, code);
    let mut assets = Vec::with_capacity(loaded.len());
    let mut fonts = Vec::new();
    for asset in loaded {
        match asset.loaded {
            Ok(Loaded::Image(bytes, payload)) => assets.push(StagedAsset {
                id: asset.id,
                bytes,
                payload,
            }),
            Ok(Loaded::Font(bytes)) => fonts.push(StagedFont {
                id: asset.id,
                path: asset.path,
                bytes,
            }),
            Err(_) => return Err(refuse_at(Stage::ResourceBuild, NACK_UNLOADABLE_ASSET)),
        }
    }
    let domains = bundle
        .sections
        .iter()
        .fold(Domains::NONE, |domains, s| domains.with(s.domain()));
    let mut views = Vec::new();
    let mut games = Vec::new();
    for section in bundle.sections {
        match section {
            PatchSection::Ui(ui) => {
                for view in ui.views {
                    let file = view.file.0 as usize;
                    if file >= files.len() {
                        return Err(refuse(NACK_UNKNOWN_FILE));
                    }
                    let candidate =
                        Candidate::load(view.package).map_err(|_| refuse(NACK_UNLOADABLE_VIEW))?;
                    views.push(StagedView {
                        file,
                        candidate: Rc::new(candidate),
                        plan: view.plan,
                    });
                }
            }
            // Loaded above, from the blobs the link assembled.
            PatchSection::Resource(_) => {}
            PatchSection::System(systems) => {
                for game in systems.games {
                    let file = game.file.0 as usize;
                    if file >= files.len() {
                        return Err(refuse(NACK_UNKNOWN_FILE));
                    }
                    // A game swaps with its view, whose behavior is its
                    // candidate module.
                    let module = views
                        .iter()
                        .find(|view| view.file == file)
                        .and_then(|view| view.candidate.module())
                        .filter(|module| !module.systems().is_empty())
                        .filter(|_| !games.iter().any(|g: &StagedGame| g.file == file))
                        .cloned()
                        .ok_or_else(|| refuse(NACK_UNLOADABLE_SYSTEM))?;
                    let vm =
                        GameSession::stage(&module).map_err(|_| refuse(NACK_UNLOADABLE_SYSTEM))?;
                    games.push(StagedGame {
                        file,
                        tier: game.tier,
                        module,
                        vm: Some(vm),
                    });
                }
            }
        }
    }
    Ok(StagedPatch {
        base_revision: bundle.base_revision,
        next_revision: bundle.next_revision,
        domains,
        views,
        games,
        assets,
        fonts,
        decode: Duration::ZERO,
        stage: Duration::ZERO,
        compiling: false,
        programs: Vec::new(),
    })
}

/// The compiles `staged` waits for: each program its views paint with that
/// the renderer of a window of `windows` does not hold, for that window. A
/// program a window's renderer could not draw refuses the patch, with the
/// line saying why.
fn shader_work(
    staged: &StagedPatch,
    windows: &[WindowState],
    files: &[ViewFile],
) -> Result<Vec<Work>, (PatchNack, String)> {
    let mut work: Vec<Work> = Vec::new();
    for view in &staged.views {
        for shader in &view.candidate.package.shaders {
            let program = shader.program();
            for ws in windows {
                let Some(gpu) = &ws.gpu else {
                    continue;
                };
                let renderer = &gpu.renderer;
                if renderer.holds_program::<viso_gpu::Backend>(program)
                    || work
                        .iter()
                        .any(|w| w.window == ws.window && Arc::ptr_eq(&w.program, program))
                {
                    continue;
                }
                let format = renderer.surface_format();
                if let Err(error) = program.desc(viso_gpu::Backend::SHADER_LANG, format) {
                    let nack = PatchNack {
                        base_revision: staged.base_revision,
                        candidate_revision: staged.next_revision,
                        stage: Stage::GpuValidate,
                        diagnostic_codes: vec![NACK_UNBINDABLE_SHADER.into()],
                        last_good_revision: Revision::default(),
                    };
                    let line = format!(
                        "{}: shader `{}`: {error}; the app keeps the last good",
                        files[view.file].entry.path, program.name
                    );
                    return Err((nack, line));
                }
                work.push(Work {
                    window: ws.window,
                    compiler: gpu.backend.program_compiler(),
                    format,
                    program: Arc::clone(program),
                    file: view.file,
                });
            }
        }
    }
    Ok(work)
}

/// The inventory entry of file `index`, as the build recorded it.
fn entry(index: usize, record: &MountRecord) -> MountEntry {
    MountEntry {
        file: FileId(index as u32),
        path: record.file.into(),
        package: record.package.into(),
        module: record.module.iter().map(|&m| m.into()).collect(),
        language: record.language.map(Into::into),
        catalog: record
            .catalog
            .map(|(source, dir)| (source.into(), dir.into())),
        capabilities: record.capabilities.iter().map(|&c| c.into()).collect(),
        source_hash: record.source_hash,
        assets: record.assets.map(|(dir, held)| (dir.into(), held.to_vec())),
    }
}

/// Adds what committing or restoring a mount of `file` kept and lost to the
/// file's `counts`, and its notices and scoped resets, each up to its bound.
fn tally(
    counts: &mut CommitCounts,
    notices: &mut Vec<Notice>,
    resets: &mut Vec<ScopedReset>,
    file: usize,
    report: CommitReport,
) {
    counts.migrated += report.migrated;
    counts.reset += report.reset;
    counts.focus_lost += u32::from(report.focus_lost);
    counts.scroll_lost += report.scroll_lost;
    counts.handlers_lost += u32::from(report.handlers_lost);
    counts.dirty = counts.dirty.merge(&report.dirty);
    let file = FileId(file as u32);
    for notice in report.notices {
        let notice = Notice {
            file,
            code: RESET_NOTICE.to_owned(),
            start: notice.start,
            end: notice.end,
            message: notice.message,
        };
        if notices.len() < MAX_NOTICES && !notices.contains(&notice) {
            notices.push(notice);
        }
    }
    for reset in report.state_resets {
        if resets.len() < MAX_SCOPED_RESETS {
            resets.push(ScopedReset {
                file,
                owner: reset.owner,
                code: reset.code.to_owned(),
            });
        }
    }
}

/// The next ordinal of a cell of `key` among `ordinals`, the cells counted
/// so far.
fn next_key_ordinal(ordinals: &mut Vec<(StateKey, u32)>, key: StateKey) -> u32 {
    match ordinals.iter_mut().find(|(k, _)| *k == key) {
        Some((_, next)) => {
            *next += 1;
            *next - 1
        }
        None => {
            ordinals.push((key, 1));
            0
        }
    }
}

/// The next mount ordinal of `file` among `ordinals`, the mounts counted so
/// far by file.
fn next_ordinal(ordinals: &mut Vec<(usize, u32)>, file: usize) -> u32 {
    match ordinals.iter_mut().find(|(f, _)| *f == file) {
        Some((_, next)) => {
            *next += 1;
            *next - 1
        }
        None => {
            ordinals.push((file, 1));
            0
        }
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Shows over each window that mounts a file the failures of the files it
/// mounts, or removes its overlay when none fails.
fn show_failures(
    files: &[ViewFile],
    views: &[LiveView],
    windows: &mut [WindowState],
    scratch: &mut Vec<NodeId>,
) {
    let mut mounted: Vec<usize> = Vec::new();
    for ws in windows {
        mounted.clear();
        for view in views.iter().filter(|view| view.window == ws.window) {
            if !mounted.contains(&view.file) {
                mounted.push(view.file);
            }
        }
        if mounted.is_empty() {
            continue;
        }
        let lines: Vec<&str> = mounted
            .iter()
            .filter_map(|&file| files[file].failure.as_deref())
            .flatten()
            .map(String::as_str)
            .collect();
        overlay::show(ws, &lines, scratch);
    }
}

/// Commits `candidate` by `plan` to the mount `view` in `ws`, or restores it
/// from `snapshot` by the plan from the snapshot's view; `statics` is the
/// static shape of the build's view, which names the static nodes of a mount
/// that recorded none.
fn commit_view(
    ws: &mut WindowState,
    view: &mut LiveView,
    statics: &[u32],
    candidate: &Candidate,
    plan: &ReloadPlan,
    snapshot: Option<&ViewSnapshot>,
    scratch: &mut Vec<NodeId>,
) -> CommitReport {
    // The store maps a durable key to one cell; a file mounted twice points the
    // keys at the mount being committed.
    for &(key, id) in &view.cells {
        ws.states.bind_key(id, key);
    }
    if view.nodes.is_empty() {
        view.nodes = static_nodes(&ws.store, view.root, statics);
    }
    let old_root = view.root;
    let mut live = LiveRuntime {
        store: &mut ws.store,
        states: &mut ws.states,
        bindings: &mut ws.bindings,
        effects: &mut ws.effects,
        lists: &mut ws.virtual_lists,
        text_edits: &mut ws.text_edits,
        projectors: &mut ws.projectors,
        root: Some(old_root),
        nodes: &mut view.nodes,
        scratch,
        view: &mut view.host,
    };
    let report = match snapshot {
        Some(snapshot) => restore(&mut live, candidate, plan, snapshot),
        None => commit(&mut live, candidate, plan),
    };
    let root = live.root;
    if ws.root == Some(old_root) {
        ws.root = root;
    }
    if let Some(root) = root {
        view.root = root;
    }
    // The view's states in declaration order, then the other sources.
    let states = candidate.package.states.iter().map(|state| state.key);
    let others = plan
        .states
        .iter()
        .map(|state| state.key)
        .filter(|key| !candidate.package.states.iter().any(|s| s.key == *key));
    view.cells = states
        .chain(others)
        .filter_map(|key| Some((key, ws.states.id_for_key(key)?)))
        .collect();
    report
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use viso_dsl::frontend::Origin;
    use viso_dsl::hir::{CapabilitySet, TargetProfile};
    use viso_dsl::hotreload::{CandidatePlan, plan_view_for, view_patch};
    use viso_ui::{
        BuildCx, DirtyClass, NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter,
        Rect, StateValue,
    };
    use viso_view::dev::wire::{
        BlobRef, HostMessage, NACK_BUILD_MISMATCH, NACK_MALFORMED, NACK_REVISION_MISMATCH,
        ResourceAsset, ResourcePatch, UiPatch, blob_chunks, content_hash, source_hash,
    };

    use viso_shader::program::PackedCode;
    use viso_ui::shader::{PackedBrush, ShaderLang};

    use super::link::fake::{self, Host};
    use super::*;

    /// The language this target's backend reads shaders in.
    const LANG: viso_view::dev::wire::ShaderLang =
        <viso_gpu::Backend as viso_gpu::GpuBackend>::SHADER_LANG;

    const COUNTER: &str = include_str!("../../tests/fixtures/counter.vs");
    const SHADER_CARD: &str = include_str!("../../tests/fixtures/shader_card.vs");
    const LOGGER: &str = include_str!("../../tests/fixtures/logger.vs");
    const SCROLLER: &str = include_str!("../../tests/fixtures/scroller.vs");

    fn children(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut child = store.arena().links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = store.arena().links(c).and_then(|l| l.next_sibling);
        }
        out
    }

    /// Every node of the subtree at `root`, in pre-order.
    fn subtree(store: &NodeStore, root: NodeId) -> Vec<NodeId> {
        let mut out = vec![root];
        for child in children(store, root) {
            out.extend(subtree(store, child));
        }
        out
    }

    fn build(ws: &mut WindowState, mount: impl FnOnce(&mut BuildCx<'_>) -> NodeId) -> NodeId {
        let mut cx = BuildCx::with_reactive(
            &mut ws.store,
            &mut ws.states,
            &mut ws.bindings,
            &mut ws.virtual_lists,
            &mut ws.text_edits,
            &mut ws.projectors,
        );
        mount(&mut cx)
    }

    fn mount_counter(cx: &mut BuildCx<'_>) -> NodeId {
        viso_ui_macros::view!("../../tests/fixtures/counter.vs")(cx).id()
    }

    /// A window that mounts the counter view.
    fn counter() -> WindowState {
        mounted(mount_counter)
    }

    /// A window whose build `mount`s one view, its mount recorded.
    fn mounted(mount: impl FnOnce(&mut BuildCx<'_>) -> NodeId) -> WindowState {
        let mut ws = WindowState::new(WindowId(1));
        ws.root = Some(build(&mut ws, mount));
        ws
    }

    /// Polls `session` until `done`, for at most ten seconds.
    fn until(session: &mut HotReloadSession, mut done: impl FnMut(&HotReloadSession) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(session) {
            session.receive(&[]);
            assert!(Instant::now() < deadline, "the session never got there");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The host's side of one file: its edits compiled and planned the way
    /// `viso run` does, against the view the runtime last ACKed.
    struct Edits {
        file: FileId,
        origin: Origin,
        profile: TargetProfile,
        last_good: CandidatePlan,
        /// The candidate of the patch in flight.
        sent: Option<CandidatePlan>,
    }

    impl Edits {
        /// The host's view of `entry`, which the build compiled from `source`.
        fn new(entry: &MountEntry, source: &str) -> Edits {
            let origin = Origin {
                package: entry.package.clone(),
                module: entry.module.clone(),
                language: entry.language.clone(),
            };
            let mut capabilities = CapabilitySet::new();
            for capability in &entry.capabilities {
                capabilities.insert(capability);
            }
            let profile = TargetProfile {
                capabilities,
                ..TargetProfile::default()
            };
            assert_eq!(entry.source_hash, source_hash(source));
            let last_good =
                plan_view_for(source, &origin, profile.clone()).expect("the build compiles");
            Edits {
                file: entry.file,
                origin,
                profile,
                last_good,
                sent: None,
            }
        }

        /// The `ui` section moving this file to `source`.
        fn section(&mut self, source: &str) -> PatchSection {
            let candidate = plan_view_for(source, &self.origin, self.profile.clone())
                .expect("the edit compiles");
            let langs = [LANG, ShaderLang::None];
            let view = view_patch(self.file, Some(&self.last_good), &candidate, &langs)
                .expect("the shaders build");
            self.sent = Some(candidate);
            PatchSection::Ui(UiPatch { views: vec![view] })
        }

        /// A patch from `base` to `next` moving this file to `source`.
        fn patch(&mut self, host: &Host, base: u64, next: u64, source: &str) -> HostMessage {
            let HostMessage::Patch(mut patch) = host.patch(base, next) else {
                unreachable!()
            };
            patch.sections = vec![self.section(source)];
            HostMessage::Patch(patch)
        }

        /// The runtime ACKed the patch in flight.
        fn acked(&mut self) {
            self.last_good = self.sent.take().expect("a patch in flight");
        }
    }

    /// A session linked to a fake host that accepted it, having adopted the
    /// mounts of `ws` (if any) and reported them.
    fn linked(ws: Option<&mut WindowState>) -> (HotReloadSession, Host, Vec<MountEntry>) {
        let mut host = Host::bind();
        let link = DevLink::connect(host.addr(), fake::hello(), LoopWaker::new(|| {}));
        let mut session = HotReloadSession {
            started: true,
            link: Some(link),
            ..HotReloadSession::default()
        };
        host.accept();
        host.welcome();
        until(&mut session, |s| s.identity.is_some());
        let mut entries = Vec::new();
        if let Some(ws) = ws {
            session.adopt(|| unreachable!("the link is open"), ws);
            match host.read() {
                Some(RuntimeMessage::Mounts(mounts)) => entries = mounts,
                other => panic!("not an inventory: {other:?}"),
            }
        }
        (session, host, entries)
    }

    /// [`linked`] for `ws`, which mounts the file `source` builds, with the
    /// host's view of the file.
    fn session(ws: &mut WindowState, source: &str) -> (HotReloadSession, Host, Edits) {
        let (session, host, entries) = linked(Some(ws));
        let edits = Edits::new(&entries[0], source);
        (session, host, edits)
    }

    /// Sends `patch`, commits it at the next frame boundary of `windows` and
    /// returns the runtime's answer.
    fn apply_on(
        session: &mut HotReloadSession,
        host: &mut Host,
        windows: &mut [WindowState],
        patch: &HostMessage,
    ) -> RuntimeMessage {
        apply_with(session, host, windows, &mut Assets::default(), patch)
    }

    /// [`apply_on`] with the app's `assets`.
    fn apply_with(
        session: &mut HotReloadSession,
        host: &mut Host,
        windows: &mut [WindowState],
        assets: &mut Assets,
        patch: &HostMessage,
    ) -> RuntimeMessage {
        host.send(patch);
        if let Some(answer) = answer_or_ready(session, host, windows) {
            return answer;
        }
        session.reload(windows, assets);
        host.read().expect("an answer")
    }

    /// [`apply_on`] for the single window `ws`.
    fn apply(
        session: &mut HotReloadSession,
        host: &mut Host,
        ws: &mut WindowState,
        patch: &HostMessage,
    ) -> RuntimeMessage {
        apply_on(session, host, std::slice::from_mut(ws), patch)
    }

    /// Receives until the session staged a patch (`None`) or answered the
    /// host (the answer).
    fn answer_or_staged(session: &mut HotReloadSession, host: &mut Host) -> Option<RuntimeMessage> {
        answer_or_ready(session, host, &[])
    }

    /// Receives until the session holds patches ready to commit, their
    /// shaders compiled for `windows` (`None`), or answered the host (the
    /// answer).
    fn answer_or_ready(
        session: &mut HotReloadSession,
        host: &mut Host,
        windows: &[WindowState],
    ) -> Option<RuntimeMessage> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = host.stream.as_ref().unwrap();
        stream.set_nonblocking(true).unwrap();
        let answered = loop {
            session.receive(windows);
            if !session.patches.is_empty() && session.patches.iter().all(|p| !p.compiling) {
                break false;
            }
            if matches!(stream.peek(&mut [0]), Ok(1)) {
                break true;
            }
            assert!(Instant::now() < deadline, "no answer");
            std::thread::sleep(Duration::from_millis(1));
        };
        stream.set_nonblocking(false).unwrap();
        answered.then(|| host.read().expect("an answer"))
    }

    /// Commits `source` as the next revision over `windows` and returns the
    /// ACK.
    fn accept_on(
        session: &mut HotReloadSession,
        host: &mut Host,
        windows: &mut [WindowState],
        edits: &mut Edits,
        source: &str,
    ) -> PatchAck {
        let base = revision(session);
        let patch = edits.patch(host, base, base + 1, source);
        match apply_on(session, host, windows, &patch) {
            RuntimeMessage::Ack(ack) => {
                edits.acked();
                ack
            }
            other => panic!("not an ACK: {other:?}"),
        }
    }

    /// [`accept_on`] for the single window `ws`.
    fn accept(
        session: &mut HotReloadSession,
        host: &mut Host,
        ws: &mut WindowState,
        edits: &mut Edits,
        source: &str,
    ) -> PatchAck {
        accept_on(session, host, std::slice::from_mut(ws), edits, source)
    }

    fn revision(session: &HotReloadSession) -> u64 {
        session.identity.as_ref().unwrap().current_revision.0
    }

    /// The live value of the counter's `count`, among the cells of view `at`.
    fn count_at(ws: &WindowState, session: &HotReloadSession, at: usize) -> Option<StateValue> {
        let (_, id) = session.views[at].cells.iter().copied().find(|&(_, id)| {
            matches!(
                ws.states.get(id),
                Some(StateValue::Int(_) | StateValue::Float(_))
            )
        })?;
        ws.states.get(id)
    }

    /// [`count_at`] for the first view.
    fn count(ws: &WindowState, session: &HotReloadSession) -> Option<StateValue> {
        count_at(ws, session, 0)
    }

    #[test]
    fn mounts_are_reported_once_per_file() {
        let mut ws = counter();
        let (mut session, mut host, entries) = linked(Some(&mut ws));
        let [entry] = &entries[..] else {
            panic!("one entry: {entries:?}");
        };
        assert_eq!(entry.file, FileId(0));
        assert!(entry.path.ends_with("counter.vs"), "{}", entry.path);
        assert_eq!(entry.source_hash, source_hash(COUNTER));
        assert_eq!(session.files[0].statics, [2, 0, 0]);
        // A second mount of the same file is not reported again.
        let root = build(&mut ws, mount_counter);
        assert!(ws.store.arena().is_live(root));
        session.adopt(|| unreachable!(), &mut ws);
        assert_eq!(session.views.len(), 2);
        session
            .link
            .as_mut()
            .unwrap()
            .send(RuntimeMessage::Dropped { count: 0 });
        assert_eq!(host.read(), Some(RuntimeMessage::Dropped { count: 0 }));
    }

    #[test]
    fn without_viso_run_the_mounts_are_dropped() {
        assert!(std::env::var_os(viso_view::dev::wire::DEV_RUNTIME_ENV).is_none());
        let mut ws = counter();
        let mut session = HotReloadSession::default();
        session.adopt(|| LoopWaker::new(|| {}), &mut ws);
        assert!(session.link.is_none());
        assert!(session.views.is_empty() && session.files.is_empty());
    }

    #[test]
    fn an_accepted_edit_commits_to_the_mount_and_keeps_its_state() {
        let mut ws = counter();
        let (mut session, mut host, mut edits) = session(&mut ws, COUNTER);
        let (_, cell) = session.views[0].cells[0];
        ws.states.set(cell, StateValue::Int(5));
        let column = ws.root.unwrap();
        assert_eq!(children(&ws.store, column).len(), 2);

        let edited = COUNTER.replace(
            "Text { visible: enabled; }",
            "Text { visible: enabled; }\n            Text { }",
        );
        let ack = accept(&mut session, &mut host, &mut ws, &mut edits, &edited);
        assert_eq!(ack.revision, Revision(2));
        assert_eq!(ack.applied_domains, Domains::NONE.with(Domain::Ui));
        let [commit] = &ack.files[..] else {
            panic!("one view: {:?}", ack.files);
        };
        assert_eq!(commit.file, FileId(0));
        assert_eq!((commit.counts.mounts, ack.scoped_resets()), (1, 0));
        assert!(commit.counts.migrated >= 1);
        let root = ws.root.expect("the window keeps a root");
        assert_eq!(root, session.views[0].root);
        assert_eq!(children(&ws.store, root).len(), 3);
        assert_eq!(count(&ws, &session), Some(StateValue::Int(5)));
    }

    #[test]
    fn a_property_edit_dirties_exactly_its_property_on_its_node() {
        let mut ws = counter();
        ws.surface_size = (800, 600);
        let (mut session, mut host, mut edits) = session(&mut ws, COUNTER);
        let (_, cell) = session.views[0].cells[0];
        ws.states.set(cell, StateValue::Int(5));
        let mut changed = Vec::new();
        ws.states.take_pending(&mut changed);
        ws.store.flush_state_transactions(&changed, &ws.bindings);
        ws.relayout_and_paint();
        let root = ws.root.unwrap();
        let nodes = subtree(&ws.store, root);
        // What the frame's later passes (paint, semantics) leave is theirs.
        ws.store.clear_dirty();

        let edited = COUNTER.replace("width: 120dp;", "width: 140dp;");
        let ack = accept(&mut session, &mut host, &mut ws, &mut edits, &edited);
        assert_eq!(ws.root, Some(root), "the same process, the same tree");
        assert_eq!(subtree(&ws.store, root), nodes, "every node kept");
        let dirty = |ws: &WindowState| -> Vec<DirtyClass> {
            nodes.iter().map(|&n| ws.store.dirty(n)).collect()
        };
        assert_eq!(
            dirty(&ws),
            [
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
                DirtyClass::EMPTY,
                DirtyClass::EMPTY
            ],
            "the column's size request moved, nothing else"
        );
        assert_eq!(
            ack.files[0].counts.dirty,
            viso_view::dev::wire::DirtyCounts {
                measure: 1,
                layout: 1,
                paint: 1,
                ..Default::default()
            },
            "the ACK names the same one node the live tree shows dirty"
        );
        assert_eq!(count(&ws, &session), Some(StateValue::Int(5)));
        assert_eq!(
            ws.store.size_request(root).map(|size| size.width),
            Some(viso_ui::Length::Fixed(140.0))
        );
        ws.relayout_and_paint();

        // A label edit touches its own text node.
        let labelled = |label: &str| {
            edited.replace(
                "Text { visible: enabled; }",
                &format!("Text {{ visible: enabled; text: \"{label}\"; }}"),
            )
        };
        accept(&mut session, &mut host, &mut ws, &mut edits, &labelled("A"));
        ws.relayout_and_paint();
        let mut requests = Vec::new();
        ws.store.take_text_requests(&mut requests);
        ws.store.clear_dirty();
        let ack = accept(&mut session, &mut host, &mut ws, &mut edits, &labelled("B"));
        assert!(dirty(&ws).iter().all(|d| d.is_empty()), "{:?}", dirty(&ws));
        assert!(
            ack.files[0].counts.dirty.is_empty(),
            "the text request carries the reshape, not a dirty mark: {:?}",
            ack.files[0].counts.dirty
        );
        requests.clear();
        ws.store.take_text_requests(&mut requests);
        let shaped: Vec<(NodeId, &str)> = requests
            .iter()
            .map(|(node, request)| (*node, request.text.as_str()))
            .collect();
        assert_eq!(shaped, [(nodes[2], "B")], "one label reshapes");
        assert_eq!(subtree(&ws.store, root), nodes, "every node kept");
    }

    #[test]
    fn a_property_edit_keeps_state_focus_and_scroll_offset() {
        let mut ws =
            mounted(|cx| viso_ui_macros::view!("../../tests/fixtures/scroller.vs")(cx).id());
        let (mut session, mut host, mut edits) = session(&mut ws, SCROLLER);
        let root = ws.root.unwrap();
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        ws.store.layout(root, surface, &mut Vec::new());
        let nodes = subtree(&ws.store, root);
        let [scroll, text] = nodes[..] else {
            panic!("Scroll + Text: {nodes:?}");
        };
        let (_, cell) = session.views[0].cells[0];
        ws.states.set(cell, StateValue::Int(7));
        ws.store.set_focusable(text, true);
        ws.store.set_focused(Some(text));
        ws.store
            .set_scroll(scroll, viso_ui::Vec2 { x: 0.0, y: 50.0 });
        assert_eq!(ws.store.scroll(scroll).y, 50.0, "the container scrolls");
        let process = std::process::id();

        let edited = SCROLLER.replace("height: 400dp;", "height: 420dp;");
        let ack = accept(&mut session, &mut host, &mut ws, &mut edits, &edited);
        assert_eq!(ack.scoped_resets(), 0, "{:?}", ack.files);
        assert_eq!(std::process::id(), process);
        assert_eq!(subtree(&ws.store, root), nodes, "every node kept");
        assert_eq!(count(&ws, &session), Some(StateValue::Int(7)));
        assert_eq!(ws.store.focused(), Some(text));
        assert_eq!(ws.store.scroll(scroll).y, 50.0);
        ws.store.layout(root, surface, &mut Vec::new());
        assert_eq!(
            ws.store.scroll(scroll).y,
            50.0,
            "the offset survives the layout"
        );
    }

    /// A primary click inside the logger's leaf, then the state flush.
    fn click(ws: &mut WindowState) {
        let root = ws.root.expect("mounted");
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        ws.store.layout(root, surface, &mut Vec::new());
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x: 20.0,
                y: 20.0,
                phase,
                buttons: PointerButtons::PRIMARY,
                modifiers: Default::default(),
            };
            PointerRouter::route(
                &mut ws.store,
                &mut ws.states,
                &ws.bindings,
                root,
                event,
                &mut chain,
            );
        }
        let mut changed = Vec::new();
        ws.states.take_pending(&mut changed);
        ws.store.flush_state_transactions(&changed, &ws.bindings);
    }

    /// The VM value of the logger's `log`, for the view `at`.
    fn log_at(session: &HotReloadSession, at: usize) -> Option<String> {
        let host = session.views[at].host.as_ref()?.borrow();
        Some(host.state(host.state_slot("log")?)?.as_str()?.to_owned())
    }

    /// [`log_at`] for the first view.
    fn log(session: &HotReloadSession) -> Option<String> {
        log_at(session, 0)
    }

    #[test]
    fn a_handler_body_edit_reloads_with_every_state_kept() {
        let mut ws = mounted(|cx| viso_ui_macros::view!("../../tests/fixtures/logger.vs")(cx).id());
        let (mut session, mut host, mut edits) = session(&mut ws, LOGGER);
        click(&mut ws);
        assert_eq!(count(&ws, &session), Some(StateValue::Int(1)));
        assert_eq!(log(&session).as_deref(), Some("one"));

        let edited = LOGGER.replace(
            "on click { count += 1; log = \"one\"; }",
            "on click { count += 10; }",
        );
        assert_ne!(edited, LOGGER);
        accept(&mut session, &mut host, &mut ws, &mut edits, &edited);

        assert_eq!(count(&ws, &session), Some(StateValue::Int(1)));
        assert_eq!(log(&session).as_deref(), Some("one"));

        click(&mut ws);
        assert_eq!(count(&ws, &session), Some(StateValue::Int(11)));
        assert_eq!(log(&session).as_deref(), Some("one"));
    }

    #[test]
    fn a_host_rejection_is_shown_until_cleared_and_changes_nothing() {
        let mut ws = counter();
        let (mut session, mut host, _) = linked(Some(&mut ws));
        let root = ws.root.unwrap();
        let nodes = subtree(&ws.store, root);
        host.send(&HostMessage::Failure {
            file: FileId(0),
            lines: vec!["counter.vs:3:19: E1405 expected an expression".into()],
        });
        until(&mut session, |s| s.failures_changed);
        session.reload(std::slice::from_mut(&mut ws), &mut Assets::default());
        assert!(ws.dev_overlay.is_some(), "the failure is shown");
        assert_eq!((ws.root, revision(&session)), (Some(root), 1));
        assert_eq!(subtree(&ws.store, root), nodes, "no node changed");

        host.send(&HostMessage::Failure {
            file: FileId(0),
            lines: Vec::new(),
        });
        until(&mut session, |s| s.failures_changed);
        session.reload(std::slice::from_mut(&mut ws), &mut Assets::default());
        assert!(ws.dev_overlay.is_none(), "a reverted edit clears it");
    }

    #[test]
    fn a_patch_that_does_not_load_is_nacked_and_the_next_one_applies() {
        let mut ws = counter();
        let (mut session, mut host, mut edits) = session(&mut ws, COUNTER);
        let root = ws.root.unwrap();
        let mut broken = edits.patch(
            &host,
            1,
            2,
            &COUNTER.replace("width: 120dp;", "width: 1dp;"),
        );
        let HostMessage::Patch(patch) = &mut broken else {
            unreachable!()
        };
        let PatchSection::Ui(ui) = &mut patch.sections[0] else {
            unreachable!("a ui section")
        };
        ui.views[0].package.behavior = vec![0xEE; 8];
        match apply(&mut session, &mut host, &mut ws, &broken) {
            RuntimeMessage::Nack(nack) => {
                assert_eq!(nack.stage, Stage::RuntimeStage);
                assert_eq!(nack.diagnostic_codes, [NACK_UNLOADABLE_VIEW]);
                assert_eq!(
                    (nack.candidate_revision, nack.last_good_revision),
                    (Revision(2), Revision(1))
                );
            }
            other => panic!("not a NACK: {other:?}"),
        }
        assert_eq!(revision(&session), 1);
        assert_eq!(ws.root, Some(root));
        assert_eq!(children(&ws.store, root).len(), 2);
        assert!(session.files[0].current.is_none(), "the build's view runs");

        let ack = accept(
            &mut session,
            &mut host,
            &mut ws,
            &mut edits,
            &COUNTER.replace("width: 120dp;", "width: 140dp;"),
        );
        assert_eq!(ack.revision, Revision(2));
    }

    /// A window mounting the shader card and drawing with a GPU, its shaders
    /// installed.
    fn shaded() -> WindowState {
        let mut ws =
            mounted(|cx| viso_ui_macros::view!("../../tests/fixtures/shader_card.vs")(cx).id());
        let mut backend = viso_gpu::create_device();
        let renderer =
            viso_render::Renderer::new(&mut backend, viso_gpu::TextureFormat::Bgra8Unorm);
        ws.gpu = Some(crate::GpuState {
            backend,
            renderer,
            surface: None,
            size: (200, 100),
        });
        install(&mut ws);
        ws
    }

    /// Brings `ws`'s renderer to the programs its nodes paint with, as the
    /// frame does after the settle.
    fn install(ws: &mut WindowState) {
        let gpu = ws.gpu.as_mut().expect("a GPU");
        ws.shaders
            .sync(ws.store.shaders(), &mut gpu.backend, &mut gpu.renderer);
    }

    /// The slot the card's shader view paints with.
    fn painting(ws: &WindowState) -> viso_ui::shader::ProgramSlot {
        let view = children(&ws.store, ws.root.unwrap())[0];
        match ws.store.content_payload(view) {
            Some(viso_ui::Content::Shader { program, .. }) => *program,
            other => panic!("{other:?}"),
        }
    }

    fn renderer(ws: &WindowState) -> &viso_render::Renderer {
        &ws.gpu.as_ref().expect("a GPU").renderer
    }

    /// The card, its shader filling with `fill`.
    fn card(fill: &str) -> String {
        SHADER_CARD.replace("Vec4F32(tint.r, tint.g, tint.b, 1.0)", fill)
    }

    #[test]
    fn a_shader_edit_compiles_off_the_loop_and_the_frame_installs_it_without_compiling() {
        let mut ws = shaded();
        let (mut session, mut host, mut edits) = session(&mut ws, SHADER_CARD);
        let compiles = renderer(&ws).program_compiles();
        assert_eq!(compiles, 1, "the build's shader, at the first frame");
        let (slot, pipeline) = (painting(&ws), renderer(&ws).program_pipeline(painting(&ws)));

        // An edit leaving the shader as it was compiles nothing and keeps it.
        let taller = SHADER_CARD.replace("height: 100dp;", "height: 120dp;");
        let patch = edits.patch(&host, 1, 2, &taller);
        assert!(matches!(
            apply(&mut session, &mut host, &mut ws, &patch),
            RuntimeMessage::Ack(_)
        ));
        edits.acked();
        install(&mut ws);
        assert_eq!(painting(&ws), slot);
        assert_eq!(renderer(&ws).program_pipeline(slot), pipeline);
        assert_eq!(renderer(&ws).program_compiles(), compiles);

        // An edit of its code: the patch waits for the compile thread, the
        // commit hands the program to the renderer, and the frame installs it
        // without compiling, the old pipeline retired.
        let dimmer = card("Vec4F32(tint.r * 0.5, tint.g * 0.5, tint.b * 0.5, 1.0)");
        let patch = edits.patch(&host, 2, 3, &dimmer);
        assert!(matches!(
            apply(&mut session, &mut host, &mut ws, &patch),
            RuntimeMessage::Ack(_)
        ));
        edits.acked();
        install(&mut ws);
        let now = renderer(&ws).program_pipeline(painting(&ws));
        assert!(now.is_some() && now != pipeline, "the new program draws");
        assert_eq!(
            renderer(&ws).program_compiles(),
            compiles,
            "nothing on the frame"
        );
        assert_eq!(
            renderer(&ws).frame_stats().programs,
            1,
            "the old slot emptied"
        );

        // An edit of its interface: the node's instance is encoded anew from
        // the view's value, by the new layout.
        let glowing = dimmer
            .replace(
                "instance rings: F32;",
                "instance rings: F32;\n    instance glow: F32;",
            )
            .replace("rings: rings }", "rings: rings, glow: 0.25 }");
        let patch = edits.patch(&host, 3, 4, &glowing);
        assert!(matches!(
            apply(&mut session, &mut host, &mut ws, &patch),
            RuntimeMessage::Ack(_)
        ));
        install(&mut ws);
        let view = children(&ws.store, ws.root.unwrap())[0];
        let Some(viso_ui::Content::Shader {
            program, params, ..
        }) = ws.store.content_payload(view)
        else {
            unreachable!()
        };
        let layout = &ws.store.shaders().get(*program).unwrap().layout;
        assert_eq!(params.len(), layout.stride as usize);
        let glow = layout.params[layout.param("glow").unwrap()].offset as usize;
        assert_eq!(
            f32::from_le_bytes(params[glow..glow + 4].try_into().unwrap()),
            0.25
        );
        assert!(renderer(&ws).program_pipeline(*program).is_some());
    }

    #[test]
    fn a_shader_the_renderer_cannot_draw_refuses_the_revision_at_staging() {
        let mut ws = shaded();
        let (mut session, mut host, mut edits) = session(&mut ws, SHADER_CARD);
        let (slot, root) = (painting(&ws), ws.root);
        let dimmer = card("Vec4F32(tint.r * 0.5, tint.g * 0.5, tint.b * 0.5, 1.0)");
        let mut patch = edits.patch(&host, 1, 2, &dimmer);
        // Its code in a language the backend does not read.
        let other = match LANG {
            ShaderLang::Wgsl => ShaderLang::Msl,
            _ => ShaderLang::Wgsl,
        };
        swap_code(&mut patch, |_| {
            vec![PackedCode::Text(other, "fn main() {}".into())]
        });
        let (line, nack) = refused(&mut session, &mut host, &mut ws, &patch);
        assert!(
            line.contains("shader `Ripple`: the program carries no"),
            "{line}"
        );
        assert_eq!(nack.stage, Stage::GpuValidate);
        assert_eq!(nack.diagnostic_codes, [NACK_UNBINDABLE_SHADER]);
        assert_eq!(nack.last_good_revision, Revision(1));
        assert_eq!(revision(&session), 1);
        assert_eq!((painting(&ws), ws.root), (slot, root), "the view kept");

        let ack = accept(&mut session, &mut host, &mut ws, &mut edits, &dimmer);
        assert_eq!(ack.revision, Revision(2));
    }

    /// Replaces the code of the first shader `patch`'s view paints with by
    /// what `code` makes of it.
    fn swap_code(patch: &mut HostMessage, code: impl FnOnce(Vec<PackedCode>) -> Vec<PackedCode>) {
        use viso_ende::{Decode, Encode};

        let HostMessage::Patch(patch) = patch else {
            unreachable!()
        };
        let PatchSection::Ui(ui) = &mut patch.sections[0] else {
            unreachable!("a ui section")
        };
        let shaders = &mut ui.views[0].package.shaders;
        let mut brush = PackedBrush::decode_from_slice(shaders[0].bytes()).unwrap();
        brush.code = code(brush.code);
        shaders[0] = viso_view::ViewShader::new(brush.encode_to_vec()).unwrap();
    }

    /// Sends `patch` to a session that refuses it: the error line saying
    /// why, and the NACK after it.
    fn refused(
        session: &mut HotReloadSession,
        host: &mut Host,
        ws: &mut WindowState,
        patch: &HostMessage,
    ) -> (String, PatchNack) {
        let RuntimeMessage::Log {
            level: LogLevel::Error,
            line,
        } = apply(session, host, ws, patch)
        else {
            panic!("not an error line");
        };
        match host.read() {
            Some(RuntimeMessage::Nack(nack)) => (line, nack),
            other => panic!("not a NACK: {other:?}"),
        }
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_shader_the_backend_refuses_refuses_the_revision_with_its_view() {
        let mut ws = shaded();
        let (mut session, mut host, mut edits) = session(&mut ws, SHADER_CARD);
        let (slot, root) = (painting(&ws), ws.root);
        let pipeline = renderer(&ws).program_pipeline(slot);
        let dimmer = card("Vec4F32(tint.r * 0.5, tint.g * 0.5, tint.b * 0.5, 1.0)");
        let mut patch = edits.patch(&host, 1, 2, &dimmer);
        swap_code(&mut patch, |code| {
            code.into_iter()
                .map(|code| match code {
                    PackedCode::Text(ShaderLang::Msl, _) => {
                        PackedCode::Text(ShaderLang::Msl, "not metal".into())
                    }
                    code => code,
                })
                .collect()
        });
        let (line, nack) = refused(&mut session, &mut host, &mut ws, &patch);
        assert!(line.contains("shader `Ripple`: program_source"), "{line}");
        assert_eq!(nack.stage, Stage::ShaderCompile);
        assert_eq!(nack.diagnostic_codes, [NACK_UNCOMPILABLE_SHADER]);
        assert_eq!(nack.last_good_revision, Revision(1));
        assert_eq!(revision(&session), 1);
        install(&mut ws);
        assert_eq!((painting(&ws), ws.root), (slot, root), "the view kept");
        assert_eq!(
            renderer(&ws).program_pipeline(slot),
            pipeline,
            "and its pipeline"
        );

        let before = draw(&mut ws);
        let ack = accept(&mut session, &mut host, &mut ws, &mut edits, &dimmer);
        assert_eq!(ack.revision, Revision(2));
        install(&mut ws);
        assert_ne!(renderer(&ws).program_pipeline(painting(&ws)), pipeline);
        // Metal draws the new program at the next frame: the tint at half.
        let after = draw(&mut ws);
        let blue = |picture: &[u8]| picture[((40 * 200 + 60) * 4) as usize];
        assert!(blue(&before) >= 250, "{}", blue(&before));
        assert!((120..=135).contains(&blue(&after)), "{}", blue(&after));
    }

    /// `ws`'s card drawn by its renderer into a 200×100 target, read back as
    /// BGRA8.
    #[cfg(target_vendor = "apple")]
    fn draw(ws: &mut WindowState) -> Vec<u8> {
        let root = ws.root.unwrap();
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 100.0,
        };
        ws.store.layout(root, surface, &mut Vec::new());
        let mut prims = Vec::new();
        viso_ui::paint_tree(&ws.store, root, &mut prims);
        let gpu = ws.gpu.as_mut().unwrap();
        gpu.renderer.set_surface_size([200.0, 100.0]);
        gpu.renderer.upload(&mut gpu.backend, &prims);
        let target = gpu.backend.create_texture(&viso_gpu::TextureDesc {
            width: 200,
            height: 100,
            format: viso_gpu::TextureFormat::Bgra8Unorm,
            render_target: true,
            label: "card",
        });
        gpu.renderer
            .render_to_texture(&mut gpu.backend, target, [1.0; 4], [200.0, 100.0]);
        gpu.backend.read_texture(target)
    }

    #[test]
    fn a_patch_naming_an_unreported_file_changes_nothing() {
        let mut ws = counter();
        let (mut session, mut host, mut edits) = session(&mut ws, COUNTER);
        let mut patch = edits.patch(&host, 1, 2, COUNTER);
        let HostMessage::Patch(bundle) = &mut patch else {
            unreachable!()
        };
        let PatchSection::Ui(ui) = &mut bundle.sections[0] else {
            unreachable!("a ui section")
        };
        let mut stray = ui.views[0].clone();
        stray.file = FileId(9);
        ui.views.push(stray);
        match apply(&mut session, &mut host, &mut ws, &patch) {
            RuntimeMessage::Nack(nack) => assert_eq!(nack.diagnostic_codes, [NACK_UNKNOWN_FILE]),
            other => panic!("not a NACK: {other:?}"),
        }
        assert_eq!(revision(&session), 1);
        assert!(session.files[0].current.is_none(), "nothing committed");
    }

    /// The live values of view `at`'s states.
    fn values_at(ws: &WindowState, session: &HotReloadSession, at: usize) -> Vec<StateValue> {
        let cells = &session.views[at].cells;
        cells
            .iter()
            .filter_map(|&(_, id)| ws.states.get(id))
            .collect()
    }

    /// [`values_at`] for the first view.
    fn values(ws: &WindowState, session: &HotReloadSession) -> Vec<StateValue> {
        values_at(ws, session, 0)
    }

    #[test]
    fn label_and_state_type_edits_keep_unrelated_state() {
        let mut ws = counter();
        let (mut session, mut host, mut edits) = session(&mut ws, COUNTER);
        for &(_, id) in &session.views[0].cells {
            let edited = match ws.states.get(id) {
                Some(StateValue::Int(_)) => StateValue::Int(5),
                _ => StateValue::Bool(false),
            };
            ws.states.set(id, edited);
        }

        let labelled = COUNTER.replace(
            "Text { visible: enabled; }",
            "Text { visible: enabled; text: \"Saved\"; }",
        );
        accept(&mut session, &mut host, &mut ws, &mut edits, &labelled);
        let kept = values(&ws, &session);
        assert!(kept.contains(&StateValue::Int(5)) && kept.contains(&StateValue::Bool(false)));

        let retyped = labelled.replace("state count = 0;", "state count: F64 = 0.0;");
        let ack = accept(&mut session, &mut host, &mut ws, &mut edits, &retyped);
        assert_eq!(ack.revision, Revision(3));
        let kept = values(&ws, &session);
        assert!(kept.contains(&StateValue::Float(5.0)), "{kept:?}");
        assert!(kept.contains(&StateValue::Bool(false)), "{kept:?}");
    }

    /// H1 "Done": a label, a handler body and a state's type each arrive as a
    /// typed patch from a dev session and apply to a running mount without
    /// losing the other mount's state; a broken edit in between leaves the
    /// last-good UI running and the revision in place; the next edit applies
    /// normally. The app links no compiler
    /// (`cargo xtask check-release-absence`).
    #[test]
    fn a_label_a_handler_and_a_state_type_edit_each_apply_without_losing_state() {
        let mut counter_ws = counter();
        let (mut session, mut host, entries) = linked(Some(&mut counter_ws));
        let mut counter_edits = Edits::new(&entries[0], COUNTER);

        // Built, and adopted, only now: a window's pending mount is claimed
        // whole by the next `adopt`, so building it before the counter's own
        // adopt (above) would hand its record to the wrong window.
        let mut logger_ws = WindowState::new(WindowId(2));
        logger_ws.root = Some(build(&mut logger_ws, |cx| {
            viso_ui_macros::view!("../../tests/fixtures/logger.vs")(cx).id()
        }));
        session.adopt(|| unreachable!("the link is open"), &mut logger_ws);
        let logger_entries = match host.read() {
            Some(RuntimeMessage::Mounts(mounts)) => mounts,
            other => panic!("not an inventory: {other:?}"),
        };
        let mut logger_edits = Edits::new(&logger_entries[0], LOGGER);
        let mut windows = vec![counter_ws, logger_ws];

        // Unrelated state a label edit must not disturb.
        let (_, cell) = session.views[0].cells[0];
        windows[0].states.set(cell, StateValue::Int(5));

        // 1. Label edit, on the counter's mount: a literal text added to it.
        let labelled = COUNTER.replace(
            "Text { visible: enabled; }",
            "Text { visible: enabled; text: \"Saved\"; }",
        );
        accept_on(
            &mut session,
            &mut host,
            &mut windows,
            &mut counter_edits,
            &labelled,
        );
        assert_eq!(count_at(&windows[0], &session, 0), Some(StateValue::Int(5)));

        // 2. State type edit, on the same mount: `count` widens from Int to
        //    Float, the label stays.
        let retyped = labelled.replace("state count = 0;", "state count: F64 = 0.0;");
        accept_on(
            &mut session,
            &mut host,
            &mut windows,
            &mut counter_edits,
            &retyped,
        );
        assert_eq!(
            count_at(&windows[0], &session, 0),
            Some(StateValue::Float(5.0))
        );

        // 3. Handler body edit, on the logger's mount: the counter's edits
        //    above left this mount, and its own unrelated `log`, untouched.
        click(&mut windows[1]);
        assert_eq!(count_at(&windows[1], &session, 1), Some(StateValue::Int(1)));
        assert_eq!(log_at(&session, 1).as_deref(), Some("one"));
        let edited = LOGGER.replace(
            "on click { count += 1; log = \"one\"; }",
            "on click { count += 10; }",
        );
        accept_on(
            &mut session,
            &mut host,
            &mut windows,
            &mut logger_edits,
            &edited,
        );
        assert_eq!(
            count_at(&windows[1], &session, 1),
            Some(StateValue::Int(1)),
            "unrelated state kept across the handler edit"
        );
        assert_eq!(log_at(&session, 1).as_deref(), Some("one"));
        click(&mut windows[1]);
        assert_eq!(
            count_at(&windows[1], &session, 1),
            Some(StateValue::Int(11))
        );
        assert_eq!(
            log_at(&session, 1).as_deref(),
            Some("one"),
            "the dropped assignment no longer runs"
        );

        // 4. A broken edit NACKs: the app links no compiler, so a candidate
        //    the host never should have sent is caught as an unloadable
        //    view, not parsed — the running revision and every node of the
        //    counter's mount stay exactly as they were.
        let root = windows[0].root.unwrap();
        let nodes = subtree(&windows[0].store, root);
        let before = revision(&session);
        let mut broken = counter_edits.patch(
            &host,
            before,
            before + 1,
            &retyped.replace("width: 120dp;", "width: 150dp;"),
        );
        let HostMessage::Patch(patch) = &mut broken else {
            unreachable!()
        };
        let PatchSection::Ui(ui) = &mut patch.sections[0] else {
            unreachable!("a ui section")
        };
        ui.views[0].package.behavior = vec![0xEE; 8];
        match apply_on(&mut session, &mut host, &mut windows, &broken) {
            RuntimeMessage::Nack(nack) => {
                assert_eq!(nack.stage, Stage::RuntimeStage);
                assert_eq!(nack.diagnostic_codes, [NACK_UNLOADABLE_VIEW]);
            }
            other => panic!("not a NACK: {other:?}"),
        }
        assert_eq!(
            revision(&session),
            before,
            "the last-good revision keeps running"
        );
        assert_eq!(windows[0].root, Some(root));
        assert_eq!(subtree(&windows[0].store, root), nodes, "no node changed");

        // 5. The next edit on the counter's mount applies normally.
        let ack = accept_on(
            &mut session,
            &mut host,
            &mut windows,
            &mut counter_edits,
            &retyped.replace("width: 120dp;", "width: 150dp;"),
        );
        assert_eq!(ack.files[0].counts.mounts, 1);
    }

    /// `Counter`'s source with `states` and the `Column`'s `children`
    /// substituted in, so an edit can retype a state or reshape the view
    /// without fragile substring surgery on the fixture text.
    fn counter_source(states: &str, children: &str) -> String {
        format!(
            "component Counter {{\n    {states}\n    view {{\n        Column {{\n            width: 120dp;\n            {children}\n        }}\n    }}\n}}"
        )
    }

    const COUNTER_STATES: &str = "state count = 0;\n    state enabled = true;";
    const COUNTER_CHILDREN: &str =
        "Text { text: format(\"{}\", count); }\n            Text { visible: enabled; }";

    #[test]
    fn insert_remove_reorder_and_a_scoped_reset_each_apply_without_disturbing_the_rest() {
        let mut counter_ws = counter();
        let (mut session, mut host, entries) = linked(Some(&mut counter_ws));
        let mut counter_edits = Edits::new(&entries[0], COUNTER);

        let mut logger_ws = WindowState::new(WindowId(2));
        logger_ws.root = Some(build(&mut logger_ws, |cx| {
            viso_ui_macros::view!("../../tests/fixtures/logger.vs")(cx).id()
        }));
        session.adopt(|| unreachable!("the link is open"), &mut logger_ws);
        match host.read() {
            Some(RuntimeMessage::Mounts(_)) => {}
            other => panic!("not an inventory: {other:?}"),
        }
        let mut windows = vec![counter_ws, logger_ws];

        // A running value on each mount, so "untouched" below checks something.
        let (_, cell) = session.views[0].cells[0];
        windows[0].states.set(cell, StateValue::Int(5));
        click(&mut windows[1]);
        assert_eq!(count_at(&windows[1], &session, 1), Some(StateValue::Int(1)));

        let root = windows[0].root.unwrap();
        let before = subtree(&windows[0].store, root);
        assert_eq!(before.len(), 3, "Column + two Texts");

        // 1. Insert a sibling: both existing Texts keep their NodeId, and the
        //    logger mount is not touched at all.
        let inserted = counter_source(
            COUNTER_STATES,
            &format!("{COUNTER_CHILDREN}\n            Text {{ text: \"new\"; }}"),
        );
        accept_on(
            &mut session,
            &mut host,
            &mut windows,
            &mut counter_edits,
            &inserted,
        );
        let after_insert = subtree(&windows[0].store, root);
        assert_eq!(
            after_insert[..3],
            before[..],
            "the two originals keep their NodeId"
        );
        assert_eq!(after_insert.len(), 4, "Column + three Texts");
        assert_eq!(count_at(&windows[0], &session, 0), Some(StateValue::Int(5)));
        assert_eq!(
            count_at(&windows[1], &session, 1),
            Some(StateValue::Int(1)),
            "the logger mount is untouched"
        );

        // 2. Remove it again: the originals carry the very same NodeId they
        //    were first mounted with.
        accept_on(
            &mut session,
            &mut host,
            &mut windows,
            &mut counter_edits,
            &counter_source(COUNTER_STATES, COUNTER_CHILDREN),
        );
        assert_eq!(
            subtree(&windows[0].store, root),
            before,
            "back to the original three nodes, unchanged"
        );

        // 3. Name the two siblings in place (naming a previously-anonymous
        //    node is its own identity-affecting edit, so it is a separate
        //    step from the reorder this is building up to), then swap them
        //    by name. A two-way swap has no single-node anchor an LCS diff
        //    can order against, so — with no `Move` op in the current
        //    `StructuralOp` set — one sibling keeps its identity across the
        //    reorder and the other rebuilds fresh (`diff.rs`'s own
        //    `swapped_siblings_keep_one_and_rebuild_the_other`); this checks
        //    that invariant holds through the full session, not just inside
        //    the diff.
        accept_on(
            &mut session,
            &mut host,
            &mut windows,
            &mut counter_edits,
            &counter_source(
                COUNTER_STATES,
                "node first: Text { text: format(\"{}\", count); }\n            node second: Text { visible: enabled; }",
            ),
        );
        let [_, _, named_second] = subtree(&windows[0].store, root)[..] else {
            panic!("Column + two Texts");
        };
        let named = counter_source(
            COUNTER_STATES,
            "node second: Text { visible: enabled; }\n            node first: Text { text: format(\"{}\", count); }",
        );
        accept_on(
            &mut session,
            &mut host,
            &mut windows,
            &mut counter_edits,
            &named,
        );
        let [_, swapped_first, swapped_second] = subtree(&windows[0].store, root)[..] else {
            panic!("Column + two Texts");
        };
        assert_eq!(
            swapped_first, named_second,
            "the sibling moved ahead keeps its identity"
        );
        assert_ne!(
            swapped_second, swapped_first,
            "the two swapped siblings are still two distinct live nodes"
        );

        // 4. An incompatible retype resets only its own state: `enabled`
        //    becomes an `I64`, which a `Bool` does not convert into; `count`
        //    is untouched, and the ACK names exactly the state that reset.
        let retyped = counter_source(
            "state count = 0;\n    state enabled: I64 = 0;",
            "node second: Text { }\n            node first: Text { text: format(\"{}\", count); }",
        );
        let ack = accept_on(
            &mut session,
            &mut host,
            &mut windows,
            &mut counter_edits,
            &retyped,
        );
        let enabled = counter_edits.last_good.symbol_for_name("enabled").unwrap();
        let count = counter_edits.last_good.symbol_for_name("count").unwrap();
        let enabled_key = StateKey::from_parts(enabled.hi, enabled.lo);
        let count_key = StateKey::from_parts(count.hi, count.lo);
        assert!(
            ack.state_resets.iter().any(|r| r.owner == enabled_key),
            "enabled's own reset is named: {:?}",
            ack.state_resets
        );
        assert!(
            !ack.state_resets.iter().any(|r| r.owner == count_key),
            "count is not reported reset"
        );
        assert_eq!(
            count_at(&windows[0], &session, 0),
            Some(StateValue::Int(5)),
            "count keeps its value across an unrelated state's reset"
        );
    }

    const PHOTO: &[u8] = include_bytes!("../../../render/src/fixtures/two_tone.jpg");

    /// An SVG of `w × h` filled with `fill`.
    fn star(w: u32, h: u32, fill: &str) -> Vec<u8> {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\">\
             <rect width=\"{w}\" height=\"{h}\" fill=\"{fill}\"/></svg>"
        )
        .into_bytes()
    }

    /// Sends the blobs of `assets` (path and bytes) as the host does, once
    /// per content, ahead of `patch`, and returns `patch` with the `resource`
    /// section naming them.
    fn carrying(host: &mut Host, patch: HostMessage, assets: &[(&str, &[u8])]) -> HostMessage {
        let HostMessage::Patch(mut patch) = patch else {
            unreachable!()
        };
        for (at, (_, bytes)) in assets.iter().enumerate() {
            if assets[..at].iter().any(|(_, sent)| sent == bytes) {
                continue;
            }
            for chunk in blob_chunks(bytes) {
                host.send(&HostMessage::Blob(chunk));
            }
        }
        patch.sections.push(PatchSection::Resource(ResourcePatch {
            assets: assets
                .iter()
                .map(|(path, bytes)| ResourceAsset {
                    path: (*path).into(),
                    hash: content_hash(bytes),
                    len: bytes.len() as u32,
                })
                .collect(),
        }));
        HostMessage::Patch(patch)
    }

    /// The draw and natural size of the asset node `node` shows.
    fn shown(ws: &WindowState, node: NodeId) -> (viso_ui::AssetDraw, [f32; 2]) {
        match ws.store.content_payload(node) {
            Some(viso_ui::Content::Asset { draw, natural, .. }) => {
                (draw.clone(), [natural.x, natural.y])
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_asset_patch_swaps_the_resource_and_dirties_only_the_nodes_showing_it() {
        let mut ws = counter();
        ws.surface_size = (800, 600);
        let mut assets = Assets::default();
        let (mut session, mut host, mut edits) = session(&mut ws, COUNTER);
        edits.profile.assets = Some(Rc::new(viso_dsl::assets::AssetCatalog::of(
            "/app/assets".into(),
            [
                (
                    "icons/star.svg".to_owned(),
                    viso_dsl::assets::AssetFormat::Svg,
                ),
                (
                    "photos/day.jpg".to_owned(),
                    viso_dsl::assets::AssetFormat::Jpeg,
                ),
            ],
        )));

        // A view edit naming assets the build did not embed carries them.
        let named = COUNTER.replace(
            "Text { visible: enabled; }",
            "Text { visible: enabled; }\n            \
             Svg { source: asset(\"icons/star.svg\"); width: 24dp; height: 24dp; }\n            \
             Image { source: asset(\"photos/day.jpg\"); }",
        );
        let patch = edits.patch(&host, 1, 2, &named);
        let red = star(10, 20, "#f00");
        let patch = carrying(
            &mut host,
            patch,
            &[("icons/star.svg", &red), ("photos/day.jpg", PHOTO)],
        );
        let RuntimeMessage::Ack(ack) = apply_with(
            &mut session,
            &mut host,
            std::slice::from_mut(&mut ws),
            &mut assets,
            &patch,
        ) else {
            panic!("not an ACK");
        };
        edits.acked();
        assert!(ack.applied_domains.contains(Domain::Resource));
        assert_eq!(ack.resources.len(), 2);
        assert!(ack.resources.iter().all(|r| r.nodes == 1), "{ack:?}");
        let root = ws.root.unwrap();
        let nodes = children(&ws.store, root);
        let (icon, photo) = (nodes[2], nodes[3]);
        let (draw, size) = shown(&ws, icon);
        assert!(matches!(draw, viso_ui::AssetDraw::Vector(_)), "{draw:?}");
        assert_eq!(size, [10.0, 20.0]);
        assert_eq!(
            shown(&ws, photo).1,
            [8.0, 4.0],
            "sized before it is resident"
        );
        ws.relayout_and_paint();
        ws.store.clear_dirty();

        // A new color: the icon repaints, nothing else.
        let dirty = |ws: &WindowState| -> Vec<DirtyClass> {
            subtree(&ws.store, root)
                .iter()
                .map(|&n| ws.store.dirty(n))
                .collect()
        };
        let patch = host.patch(2, 3);
        let patch = carrying(
            &mut host,
            patch,
            &[("icons/star.svg", &star(10, 20, "#00f"))],
        );
        let RuntimeMessage::Ack(ack) = apply_with(
            &mut session,
            &mut host,
            std::slice::from_mut(&mut ws),
            &mut assets,
            &patch,
        ) else {
            panic!("not an ACK");
        };
        let mut expected = vec![DirtyClass::EMPTY; 5];
        expected[3] = DirtyClass::PAINT;
        assert_eq!(dirty(&ws), expected, "only the icon");
        assert_eq!(
            ack.resources[0].dirty,
            viso_view::dev::wire::DirtyCounts {
                paint: 1,
                ..Default::default()
            }
        );
        assert!(ack.files.is_empty(), "no view changed");
        ws.relayout_and_paint();
        ws.store.clear_dirty();

        // A new size: the icon remeasures.
        let patch = host.patch(3, 4);
        let patch = carrying(
            &mut host,
            patch,
            &[("icons/star.svg", &star(30, 20, "#00f"))],
        );
        let RuntimeMessage::Ack(ack) = apply_with(
            &mut session,
            &mut host,
            std::slice::from_mut(&mut ws),
            &mut assets,
            &patch,
        ) else {
            panic!("not an ACK");
        };
        expected[3] = DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT;
        // Its column, sized by its content, measures again around it.
        expected[0] = DirtyClass::MEASURE;
        assert_eq!(dirty(&ws), expected);
        assert_eq!(
            ack.resources[0].dirty,
            viso_view::dev::wire::DirtyCounts {
                measure: 1,
                layout: 1,
                paint: 1,
                ..Default::default()
            }
        );
        assert_eq!(shown(&ws, icon).1, [30.0, 20.0]);
        ws.store.clear_dirty();

        // A corrupt photo, or one whose bytes went missing, refuses the
        // revision: the last good stays.
        let corrupt = host.patch(4, 5);
        let corrupt = carrying(
            &mut host,
            corrupt,
            &[("photos/day.jpg", &PHOTO[..PHOTO.len() / 2])],
        );
        host.send(&corrupt);
        let line = match answer_or_staged(&mut session, &mut host) {
            Some(RuntimeMessage::Log { line, .. }) => line,
            other => panic!("not a log line: {other:?}"),
        };
        assert!(line.starts_with("assets/photos/day.jpg: "), "{line}");
        let nack = match host.read() {
            Some(RuntimeMessage::Nack(nack)) => nack,
            other => panic!("not a NACK: {other:?}"),
        };
        assert_eq!(nack.stage, Stage::ResourceBuild);
        assert_eq!(nack.diagnostic_codes, [NACK_UNLOADABLE_ASSET]);
        assert_eq!(nack.last_good_revision, Revision(4));
        let HostMessage::Patch(mut missing) = host.patch(4, 5) else {
            unreachable!()
        };
        missing.sections.push(PatchSection::Resource(ResourcePatch {
            assets: vec![ResourceAsset {
                path: "photos/day.jpg".into(),
                hash: content_hash(b"never sent"),
                len: 10,
            }],
        }));
        host.send(&HostMessage::Patch(missing));
        assert!(matches!(
            answer_or_staged(&mut session, &mut host),
            Some(RuntimeMessage::Log { .. })
        ));
        assert!(matches!(
            host.read(),
            Some(RuntimeMessage::Nack(n)) if n.stage == Stage::ResourceBuild
        ));
        assert_eq!(shown(&ws, photo).1, [8.0, 4.0]);
        assert!(dirty(&ws).iter().all(|d| d.is_empty()), "nothing changed");

        // A window opened later shows the newest icon, from the bytes the app
        // keeps.
        let mut later = WindowState::new(WindowId(2));
        later.root = Some(build(&mut later, mount_counter));
        session.adopt(|| unreachable!(), &mut later);
        let icon = children(&later.store, later.root.unwrap())[2];
        assets.serve::<viso_gpu::HeadlessRaster>(&mut later.store, None, &LoopWaker::inert);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(shown(&later, icon).0, viso_ui::AssetDraw::Vector(_)) {
            assert!(Instant::now() < deadline, "never served");
            std::thread::sleep(Duration::from_millis(1));
            assets.provide(std::iter::once(&mut later.store));
        }
        assert_eq!(shown(&later, icon).1, [30.0, 20.0], "the newest");
    }

    const FONT: &[u8] = include_bytes!("../../fixtures/DejaVuSans-subset.ttf");

    #[test]
    fn a_font_patch_replaces_the_packaged_file_and_a_broken_font_refuses() {
        static PACKAGED: [viso_text::PackagedFace; 1] = [viso_text::PackagedFace {
            file: "Body.ttf",
            file_index: 0,
            bytes: FONT,
            face_index: 0,
            family: "DejaVu Sans",
            weight: 400,
            width: 5,
            slant: viso_text::FontSlant::Normal,
            color: false,
            mono: false,
            scripts: &["Latn"],
        }];
        let mut ws = counter();
        let mut assets = Assets::default();
        let (mut session, mut host, _edits) = session(&mut ws, COUNTER);
        session.set_packaged_fonts(PackagedFonts::__new(&PACKAGED));

        // A packaged font replaces its file; one the build does not package
        // waits for the next build.
        let patch = host.patch(1, 2);
        let patch = carrying(
            &mut host,
            patch,
            &[("fonts/Body.ttf", FONT), ("fonts/Added.ttf", FONT)],
        );
        host.send(&patch);
        let answer = answer_or_ready(&mut session, &mut host, std::slice::from_mut(&mut ws));
        assert!(answer.is_none(), "answered before the commit: {answer:?}");
        session.reload(std::slice::from_mut(&mut ws), &mut assets);
        let line = match host.read() {
            Some(RuntimeMessage::Log {
                level: LogLevel::Info,
                line,
            }) => line,
            other => panic!("not an info line: {other:?}"),
        };
        assert!(line.starts_with("fonts/Added.ttf is not a font"), "{line}");
        let Some(RuntimeMessage::Ack(ack)) = host.read() else {
            panic!("not an ACK");
        };
        assert!(ack.applied_domains.contains(Domain::Resource));
        assert_eq!(ack.resources.len(), 2);
        assert!(ack.resources.iter().all(|r| r.nodes == 0), "no text drew");
        let replaced: Vec<u32> = session.fonts.files().map(|(file, _)| file).collect();
        assert_eq!(replaced, [0]);

        // A font a face of which does not parse refuses the revision.
        let broken = host.patch(2, 3);
        let broken = carrying(&mut host, broken, &[("fonts/Body.ttf", &FONT[..64])]);
        host.send(&broken);
        let line = match answer_or_staged(&mut session, &mut host) {
            Some(RuntimeMessage::Log { line, .. }) => line,
            other => panic!("not a log line: {other:?}"),
        };
        assert!(
            line.starts_with("assets/fonts/Body.ttf: does not decode"),
            "{line}"
        );
        let nack = match host.read() {
            Some(RuntimeMessage::Nack(nack)) => nack,
            other => panic!("not a NACK: {other:?}"),
        };
        assert_eq!(nack.diagnostic_codes, [NACK_UNLOADABLE_ASSET]);
    }

    #[test]
    fn a_window_opened_after_a_patch_mounts_the_patched_view() {
        let mut ws = counter();
        let (mut session, mut host, mut edits) = session(&mut ws, COUNTER);
        let edited = COUNTER.replace(
            "Text { visible: enabled; }",
            "Text { visible: enabled; }\n            Text { }",
        );
        accept(&mut session, &mut host, &mut ws, &mut edits, &edited);
        let mut later = WindowState::new(WindowId(2));
        later.root = Some(build(&mut later, mount_counter));
        session.adopt(|| unreachable!(), &mut later);
        let root = later.root.unwrap();
        assert_eq!(session.views[1].root, root);
        assert_eq!(children(&later.store, root).len(), 3, "the patched view");

        // The next edit commits to both mounts from the same view.
        let ack = accept(
            &mut session,
            &mut host,
            &mut ws,
            &mut edits,
            &edited.replace("width: 120dp;", "width: 140dp;"),
        );
        assert_eq!(ack.files[0].counts.mounts, 1, "only the window passed in");
    }

    /// Edit-to-pixels latency of a one-property edit with the host round
    /// trip, by phase: the host's compile, its plan (diff, migration, the
    /// candidate's release form), the frame's encode, the transport (write,
    /// read, decode, check, load and verify), the commit, and the relayout
    /// and repaint (without a GPU upload). A release measurement:
    /// `cargo test --release -p viso --features hot-reload --lib -- --ignored
    /// patch_to_pixels --nocapture`.
    #[test]
    #[ignore = "a release measurement"]
    fn patch_to_pixels() {
        const EDITS: usize = 60;
        let mut ws = counter();
        ws.surface_size = (800, 600);
        let (mut session, mut host, mut edits) = session(&mut ws, COUNTER);
        // A child's width, so the edit moves its box. The first patch adds
        // the property and is not sampled; the first frame lays out the tree.
        let source = |width: f32| {
            COUNTER.replace(
                "Text { visible: enabled; }",
                &format!("Text {{ visible: enabled; width: {width}dp; }}"),
            )
        };
        accept(&mut session, &mut host, &mut ws, &mut edits, &source(30.0));
        let root = ws.root.unwrap();
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        ws.store.layout(root, surface, &mut Vec::new());
        let mut phases: [Vec<Duration>; 6] = Default::default();
        let mut bytes = 0;
        for n in 0..EDITS {
            let width = if n % 2 == 0 { 40.0 } else { 30.0 };
            let base = revision(&session);
            let started = Instant::now();
            let candidate = plan_view_for(&source(width), &edits.origin, edits.profile.clone())
                .expect("the edit compiles");
            let compiled = Instant::now();
            let view = view_patch(FileId(0), Some(&edits.last_good), &candidate, &[LANG])
                .expect("the shaders build");
            let planned = Instant::now();
            let HostMessage::Patch(mut patch) = host.patch(base, base + 1) else {
                unreachable!()
            };
            patch.sections = vec![PatchSection::Ui(UiPatch { views: vec![view] })];
            let body = viso_view::dev::wire::encode(&HostMessage::Patch(patch));
            let encoded = Instant::now();
            host.send_raw(&body);
            while session.patches.is_empty() {
                session.receive(&[]);
                std::hint::spin_loop();
            }
            let staged = Instant::now();
            session.reload(std::slice::from_mut(&mut ws), &mut Assets::default());
            let committed = Instant::now();
            ws.relayout_and_paint();
            let painted = Instant::now();
            assert!(matches!(host.read(), Some(RuntimeMessage::Ack(_))));
            edits.last_good = candidate;
            assert_eq!(ws.store.bounds(children(&ws.store, root)[1]).w, width);
            let marks = [
                started, compiled, planned, encoded, staged, committed, painted,
            ];
            for (phase, pair) in phases.iter_mut().zip(marks.windows(2)) {
                phase.push(pair[1] - pair[0]);
            }
            bytes = body.len();
        }
        let summary = |name: &str, samples: &mut [Duration]| {
            samples.sort();
            let ms = |d: Duration| d.as_secs_f64() * 1e3;
            println!(
                "{name}: min {:.3} ms, median {:.3} ms, p95 {:.3} ms, max {:.3} ms",
                ms(samples[0]),
                ms(samples[samples.len() / 2]),
                ms(samples[samples.len() * 95 / 100]),
                ms(samples[samples.len() - 1]),
            );
        };
        let mut total: Vec<Duration> = (0..EDITS)
            .map(|i| phases.iter().map(|phase| phase[i]).sum())
            .collect();
        let names = [
            "compile",
            "plan",
            "encode",
            "transport",
            "commit",
            "repaint",
        ];
        for (name, phase) in names.iter().zip(&mut phases) {
            summary(name, phase);
        }
        summary("compile-to-pixels", &mut total);
        println!("patch frame: {bytes} bytes");
    }

    /// Asks `session` for its snapshot, which it takes at the frame boundary
    /// of `windows`, and returns the bytes as the host assembles them.
    fn snapshot_of(
        session: &mut HotReloadSession,
        host: &mut Host,
        windows: &mut [WindowState],
    ) -> (Vec<u8>, Vec<WindowGeometry>) {
        host.send(&HostMessage::Snapshot);
        until(session, |s| s.snapshot_asked);
        session.reload(windows, &mut Assets::default());
        let mut blobs = viso_view::dev::wire::Blobs::default();
        loop {
            match host.read() {
                Some(RuntimeMessage::Blob(chunk)) => blobs.add(chunk),
                Some(RuntimeMessage::Snapshot { blob, windows, .. }) => {
                    return (blobs.take(blob).expect("whole").to_vec(), windows);
                }
                other => panic!("not the snapshot: {other:?}"),
            }
        }
    }

    /// A process launched to restore, linked to a fake host that accepted
    /// it and sent the restore of `ui` with the snapshot `bytes` ahead of it
    /// (`None`: none arrive), having adopted the mounts of `ws`.
    fn restoring(
        ws: &mut WindowState,
        bytes: Option<&[u8]>,
        ui: UiPatch,
        dev_states: Vec<DevState>,
    ) -> (HotReloadSession, Host) {
        let mut host = Host::bind();
        let link = DevLink::connect(host.addr(), fake::hello(), LoopWaker::new(|| {}));
        let mut session = HotReloadSession {
            started: true,
            link: Some(link),
            awaiting_restore: true,
            dev_states,
            ..HotReloadSession::default()
        };
        host.accept();
        host.welcome();
        let hello = host.hello.clone().expect("accepted");
        for chunk in bytes.into_iter().flat_map(blob_chunks) {
            host.send(&HostMessage::Blob(chunk));
        }
        let snapshot = BlobRef::of(bytes.unwrap_or(b"never sent"));
        host.send(&HostMessage::Restore(Box::new(RestoreBundle {
            dev_session: hello.dev_session,
            target_runtime: hello.runtime_session,
            snapshot,
            ui,
        })));
        session.adopt(|| unreachable!("the link is open"), ws);
        assert!(matches!(host.read(), Some(RuntimeMessage::Mounts(_))));
        (session, host)
    }

    /// The restore of the file `edits` names: the view `source` compiles to,
    /// planned from the view the old process ran.
    fn restore_of(edits: &mut Edits, source: &str) -> UiPatch {
        let PatchSection::Ui(ui) = edits.section(source) else {
            unreachable!("a view section")
        };
        ui
    }

    #[test]
    fn a_restarted_app_takes_its_state_back_before_its_first_frame() {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        let scroller = |cx: &mut BuildCx<'_>| {
            viso_ui_macros::view!("../../tests/fixtures/scroller.vs")(cx).id()
        };
        let mut old = mounted(scroller);
        let (mut session, mut host, mut edits) = session(&mut old, SCROLLER);
        let root = old.root.expect("built");
        old.store.layout(root, surface, &mut Vec::new());
        let [scroll, _] = subtree(&old.store, root)[..] else {
            panic!("Scroll + Text");
        };
        let (_, cell) = session.views[0].cells[0];
        old.states.set(cell, StateValue::Int(7));
        old.store
            .set_scroll(scroll, viso_ui::Vec2 { x: 0.0, y: 50.0 });
        session.dev_states = vec![DevState {
            name: "route".into(),
            capture: Box::new(|| b"settings".to_vec()),
            restore: Box::new(|_| unreachable!("the old process restores nothing")),
        }];
        old.surface_size = (800, 500);
        old.dpi = 2.0;
        session.set_positions(vec![(old.window, Some((120.0, 64.0)))]);
        let (bytes, windows) = snapshot_of(&mut session, &mut host, std::slice::from_mut(&mut old));
        let snapshot = DevSnapshot::decode(&bytes).expect("the snapshot decodes");
        assert_eq!(snapshot.views.len(), 1, "{snapshot:?}");
        assert_eq!(
            windows,
            [WindowGeometry {
                size: (400.0, 250.0),
                position: Some((120.0, 64.0)),
            }]
        );
        assert_eq!(snapshot.extensions[0].bytes, b"settings");
        drop((session, host, old));

        // The new process builds its window fresh, then takes the snapshot
        // before its first frame.
        let route = Rc::new(RefCell::new(Vec::new()));
        let restored = Rc::clone(&route);
        let states = vec![DevState {
            name: "route".into(),
            capture: Box::new(Vec::new),
            restore: Box::new(move |bytes| *restored.borrow_mut() = bytes.to_vec()),
        }];
        let mut new = mounted(scroller);
        let ui = restore_of(&mut edits, SCROLLER);
        let (mut session, mut host) = restoring(&mut new, Some(&bytes), ui, states);
        let root = new.root.expect("built");
        new.store.layout(root, surface, &mut Vec::new());
        let [scroll, _] = subtree(&new.store, root)[..] else {
            panic!("Scroll + Text");
        };
        assert_eq!(count(&new, &session), Some(StateValue::Int(7)));
        assert_eq!(new.store.scroll(scroll).y, 50.0);
        assert_eq!(*route.borrow(), b"settings");

        session.reload(std::slice::from_mut(&mut new), &mut Assets::default());
        let Some(RuntimeMessage::Restored(report)) = host.read() else {
            panic!("no restore report");
        };
        let counts = report.files[0].counts;
        assert_eq!(counts.mounts, 1);
        assert!(counts.migrated >= 1, "{report:?}");
        assert_eq!(counts.scoped_resets(), 0, "{report:?}");
        assert_eq!(
            (report.views_lost, report.extensions, report.extensions_lost),
            (0, 1, 0)
        );
        assert!(session.restoring.is_none(), "the first frame ends it");
    }

    #[test]
    fn a_restarted_window_takes_back_the_cells_its_rust_allocated() {
        // Two cells from one call site, one from another, beside a view whose
        // own cells the view's restore carries.
        let ids = RefCell::new(Vec::new());
        let build = |cx: &mut BuildCx<'_>, flag: StateValue| {
            let mut allocated = Vec::new();
            for _ in 0..2 {
                allocated.push(cx.state(StateValue::Int(0)));
            }
            allocated.push(cx.state(flag));
            *ids.borrow_mut() = allocated;
            mount_counter(cx)
        };
        let mut old = mounted(|cx| build(cx, StateValue::Bool(false)));
        let (mut session, mut host, _) = linked(Some(&mut old));
        let set = |ws: &mut WindowState, at: usize, value| {
            let id = ids.borrow()[at];
            ws.states.set(id, value);
        };
        set(&mut old, 0, StateValue::Int(5));
        set(&mut old, 1, StateValue::Int(6));
        set(&mut old, 2, StateValue::Bool(true));
        let (bytes, _) = snapshot_of(&mut session, &mut host, std::slice::from_mut(&mut old));
        let snapshot = DevSnapshot::decode(&bytes).expect("the snapshot decodes");
        let [window] = &snapshot.cells[..] else {
            panic!("{:?}", snapshot.cells);
        };
        assert_eq!(window.cells.len(), 3, "not the view's: {:?}", window.cells);
        assert_eq!(
            window.cells.iter().map(|cell| cell.nth).collect::<Vec<_>>(),
            [0, 1, 0]
        );

        // The new build allocates its flag as a number: that one starts over.
        let mut new = mounted(|cx| build(cx, StateValue::Int(0)));
        let (mut session, mut host) =
            restoring(&mut new, Some(&bytes), UiPatch::default(), Vec::new());
        let value = |at: usize| new.states.get(ids.borrow()[at]);
        assert_eq!(value(0), Some(StateValue::Int(5)));
        assert_eq!(value(1), Some(StateValue::Int(6)));
        assert_eq!(value(2), Some(StateValue::Int(0)), "another kind of value");
        session.reload(std::slice::from_mut(&mut new), &mut Assets::default());
        let Some(RuntimeMessage::Restored(report)) = host.read() else {
            panic!("no restore report");
        };
        assert_eq!((report.cells, report.cells_lost), (2, 1));
    }

    #[test]
    fn an_app_that_mounts_no_view_carries_its_window_and_extensions() {
        let rust = |cx: &mut BuildCx<'_>| viso_ui_macros::ui! { Text { text: "rust"; } }(cx).id();
        let mut old = mounted(rust);
        let (mut session, mut host, entries) = linked(Some(&mut old));
        assert!(entries.is_empty(), "the launch reports no mount");
        session.dev_states = vec![DevState {
            name: "route".into(),
            capture: Box::new(|| b"inbox".to_vec()),
            restore: Box::new(|_| unreachable!("the old process restores nothing")),
        }];
        old.surface_size = (640, 480);
        old.dpi = 1.0;
        session.set_positions(vec![(old.window, Some((10.0, 20.0)))]);
        let (bytes, windows) = snapshot_of(&mut session, &mut host, std::slice::from_mut(&mut old));
        assert_eq!(
            windows,
            [WindowGeometry {
                size: (640.0, 480.0),
                position: Some((10.0, 20.0)),
            }]
        );
        let snapshot = DevSnapshot::decode(&bytes).expect("the snapshot decodes");
        assert!(snapshot.views.is_empty());

        let route = Rc::new(RefCell::new(Vec::new()));
        let restored = Rc::clone(&route);
        let states = vec![DevState {
            name: "route".into(),
            capture: Box::new(Vec::new),
            restore: Box::new(move |bytes| *restored.borrow_mut() = bytes.to_vec()),
        }];
        let mut new = mounted(rust);
        let (mut session, mut host) = restoring(&mut new, Some(&bytes), UiPatch::default(), states);
        assert_eq!(*route.borrow(), b"inbox");
        session.reload(std::slice::from_mut(&mut new), &mut Assets::default());
        let Some(RuntimeMessage::Restored(report)) = host.read() else {
            panic!("no restore report");
        };
        assert!(report.files.is_empty(), "{report:?}");
        assert_eq!((report.extensions, report.extensions_lost), (1, 0));
    }

    #[test]
    fn a_snapshot_that_did_not_arrive_whole_is_refused_and_the_app_runs_fresh() {
        let mut ws = counter();
        let (mut session, mut host) = restoring(&mut ws, None, UiPatch::default(), Vec::new());
        assert!(matches!(
            host.read(),
            Some(RuntimeMessage::Log {
                level: LogLevel::Error,
                ..
            })
        ));
        let Some(RuntimeMessage::Nack(nack)) = host.read() else {
            panic!("not refused");
        };
        assert_eq!(
            (nack.stage, &nack.diagnostic_codes[..]),
            (
                Stage::SnapshotRestore,
                &[NACK_UNRESTORABLE_SNAPSHOT.to_owned()][..]
            )
        );
        assert!(session.restoring.is_none());
        assert_eq!(count(&ws, &session), Some(StateValue::Int(0)));
        // Nothing to report at the first frame.
        session.reload(std::slice::from_mut(&mut ws), &mut Assets::default());
        session.log(LogLevel::Info, "after".into());
        assert!(matches!(host.read(), Some(RuntimeMessage::Log { line, .. }) if line == "after"));
    }

    #[test]
    fn a_mount_whose_window_closed_is_dropped() {
        let mut ws = counter();
        let (mut session, _host, _) = linked(Some(&mut ws));
        session.close(ws.window);
        assert!(session.views.is_empty());
    }

    /// Sends `patches` and receives until the session staged them all.
    fn stage_all(session: &mut HotReloadSession, host: &mut Host, patches: &[HostMessage]) {
        let want = session.patches.len() + patches.len();
        for patch in patches {
            host.send(patch);
        }
        until(session, |s| s.patches.len() == want);
    }

    #[test]
    fn patches_commit_in_revision_order_at_the_frame_boundary() {
        let (mut session, mut host, _) = linked(None);
        let sent = [host.patch(1, 2), host.patch(2, 3)];
        stage_all(&mut session, &mut host, &sent);
        assert_eq!(session.patches.len(), 2, "chained onto the staged one");
        assert_eq!(
            revision(&session),
            1,
            "nothing commits before the frame boundary"
        );
        session.reload(&mut [], &mut Assets::default());
        for next in [2, 3] {
            match host.read() {
                Some(RuntimeMessage::Ack(ack)) => {
                    assert_eq!(ack.revision, Revision(next));
                    assert_eq!(ack.applied_domains, Domains::NONE);
                }
                other => panic!("not an ACK: {other:?}"),
            }
        }
        assert_eq!(revision(&session), 3);
    }

    #[test]
    fn an_out_of_order_or_foreign_patch_is_nacked_and_last_good_kept() {
        let (mut session, mut host, _) = linked(None);
        let foreign = |edit: fn(&mut PatchBundle)| {
            let HostMessage::Patch(mut patch) = host.patch(1, 2) else {
                unreachable!()
            };
            edit(&mut patch);
            HostMessage::Patch(patch)
        };
        let cases = [
            (
                host.patch(2, 3),
                Stage::RuntimeStage,
                NACK_REVISION_MISMATCH,
            ),
            (
                foreign(|p| p.target_runtime.0 ^= 1),
                Stage::Transport,
                NACK_UNKNOWN_SESSION,
            ),
            (
                foreign(|p| p.build_id.0 ^= 1),
                Stage::Transport,
                NACK_BUILD_MISMATCH,
            ),
        ];
        for (patch, stage, code) in cases {
            host.send(&patch);
            match answer(&mut session, &mut host) {
                RuntimeMessage::Nack(nack) => {
                    assert_eq!(
                        (nack.stage, nack.diagnostic_codes[0].as_str()),
                        (stage, code)
                    );
                    assert_eq!(nack.last_good_revision, Revision::LAUNCH);
                }
                other => panic!("not a NACK: {other:?}"),
            }
        }
        // A frame that does not decode is NACKed, and the next patch applies.
        host.send_raw(&[0xEE]);
        assert!(matches!(
            answer(&mut session, &mut host),
            RuntimeMessage::Nack(n) if n.diagnostic_codes == [NACK_MALFORMED]
        ));
        assert!(session.patches.is_empty());
        let patch = host.patch(1, 2);
        stage_all(&mut session, &mut host, &[patch]);
        session.reload(&mut [], &mut Assets::default());
        assert!(matches!(host.read(), Some(RuntimeMessage::Ack(a)) if a.revision == Revision(2)));
    }

    /// Receives until the session answered the host, and the answer.
    fn answer(session: &mut HotReloadSession, host: &mut Host) -> RuntimeMessage {
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = host.stream.as_ref().unwrap();
        stream.set_nonblocking(true).unwrap();
        while !matches!(stream.peek(&mut [0]), Ok(1)) {
            session.receive(&[]);
            assert!(Instant::now() < deadline, "no answer");
            std::thread::sleep(Duration::from_millis(1));
        }
        stream.set_nonblocking(false).unwrap();
        host.read().expect("an answer")
    }

    mod games {
        use viso_behavior::Value;
        use viso_dsl::hotreload::game_patch;
        use viso_view::dev::wire::{GameTier, Rebuild, SystemPatch};

        use super::*;

        const GAME: &str = include_str!("../../tests/fixtures/game.vs");
        const TICK: f64 = 1.0 / 60.0;

        fn mount_game(cx: &mut BuildCx<'_>) -> NodeId {
            viso_ui_macros::view!("../../tests/fixtures/game.vs")(cx).id()
        }

        /// A patch moving the game file to `source`: its view, and its game
        /// at the tier the host classifies.
        fn game_patch_to(
            edits: &mut Edits,
            host: &Host,
            (base, next): (u64, u64),
            source: &str,
        ) -> HostMessage {
            let HostMessage::Patch(mut patch) = edits.patch(host, base, next, source) else {
                unreachable!()
            };
            let candidate = edits.sent.as_ref().expect("in flight");
            if let Some((entry, _)) = game_patch(
                edits.file,
                Some(&edits.last_good),
                candidate,
                Rebuild::KeepCharacters,
            ) {
                patch
                    .sections
                    .push(PatchSection::System(SystemPatch { games: vec![entry] }));
            }
            HostMessage::Patch(patch)
        }

        fn state(ws: &WindowState, name: &str) -> Value {
            let root = ws.root.expect("built");
            let session = game_session(&ws.store, root).expect("a game");
            session.borrow().state("Runner", name).expect("a state")
        }

        fn width(ws: &WindowState) -> Option<viso_ui::Length> {
            let root = ws.root.expect("built");
            ws.store.size_request(root).map(|size| size.width)
        }

        /// The Game integration case: run tick N, a patch arrives while it
        /// runs; the old code completes N, the new one runs from N + 1, and
        /// the World and its entity ids are the same.
        #[test]
        fn a_patch_arriving_during_a_tick_applies_from_the_next_with_the_world_kept() {
            let mut ws = mounted(mount_game);
            let (mut session, mut host, mut edits) = session(&mut ws, GAME);
            let game = game_session(&ws.store, ws.root.expect("built")).expect("a game");
            for _ in 0..2 {
                ws.store.run_frame_clients(TICK);
            }
            let world = |game: &Rc<RefCell<GameSession>>| {
                let game = game.borrow();
                let world = game.scheduler().world();
                let ids = world.entities();
                let at: Vec<_> = ids
                    .iter()
                    .map(|&id| world.position(id).expect("alive").to_array())
                    .collect();
                (ids, at)
            };
            let (ids, _) = world(&game);

            // The host sends while tick 2 runs; the link thread takes it in
            // concurrently, but nothing commits mid-frame.
            let faster = GAME.replace("steps += 1;", "steps += 10;");
            host.send(&game_patch_to(&mut edits, &host, (1, 2), &faster));
            ws.store.run_frame_clients(TICK);
            assert_eq!(
                state(&ws, "steps"),
                Value::Int(3),
                "the old code completed tick 2"
            );
            let (_, walked) = world(&game);

            // The next frame boundary stages and commits it, then tick 3 runs.
            assert!(
                answer_or_staged(&mut session, &mut host).is_none(),
                "staged"
            );
            session.reload(std::slice::from_mut(&mut ws), &mut Assets::default());
            let Some(RuntimeMessage::Ack(ack)) = host.read() else {
                panic!("not applied");
            };
            edits.acked();
            assert_eq!(
                ack.applied_domains,
                Domains::NONE.with(Domain::Ui).with(Domain::System)
            );
            let [commit] = ack.games[..] else {
                panic!("one game: {:?}", ack.games);
            };
            assert_eq!(
                (commit.tier, commit.sessions, commit.tick, commit.reset),
                (GameTier::Logic, 1, 3, 0)
            );
            assert_eq!(commit.carried, 2, "steps and jumps");
            assert_eq!(
                world(&game),
                (ids.clone(), walked),
                "the swap moved nothing"
            );
            ws.store.run_frame_clients(TICK);
            assert_eq!(
                state(&ws, "steps"),
                Value::Int(13),
                "the new code runs tick 3"
            );
            assert_eq!(world(&game).0, ids, "the same World, the same entities");
            assert_eq!(game.borrow().scheduler().clock().tick(), 4);
            assert!(
                Rc::ptr_eq(
                    &game,
                    &game_session(&ws.store, ws.root.expect("built")).expect("a game")
                ),
                "the same session"
            );
        }

        /// The phase's acceptance case: a movement speed edit changes the
        /// game at the next tick with the World, its entities and the score
        /// kept, and a broken edit after it keeps that last good.
        #[test]
        fn a_speed_edit_applies_next_tick_and_a_broken_one_keeps_the_last_good() {
            let mut ws = mounted(mount_game);
            let (mut session, mut host, mut edits) = session(&mut ws, GAME);
            let game = game_session(&ws.store, ws.root.expect("built")).expect("a game");
            let player = || {
                let game = game.borrow();
                let world = game.scheduler().world();
                let ids = world.entities();
                (
                    ids.clone(),
                    world.position(ids[0]).expect("alive").to_array()[0],
                )
            };
            // A tick's step along x once the walk has settled.
            let step = |ws: &mut WindowState| {
                let (_, before) = player();
                ws.store.run_frame_clients(TICK);
                player().1 - before
            };
            for _ in 0..30 {
                step(&mut ws);
            }
            let slow = step(&mut ws);
            assert!(slow > 0.0, "the player walks");
            let (ids, _) = player();
            assert_eq!(state(&ws, "steps"), Value::Int(31));

            let faster = GAME.replace("walk(id, 2.0, 0.0f32)", "walk(id, 4.0, 0.0f32)");
            let patch = game_patch_to(&mut edits, &host, (1, 2), &faster);
            let RuntimeMessage::Ack(ack) = apply(&mut session, &mut host, &mut ws, &patch) else {
                panic!("not applied");
            };
            edits.acked();
            assert_eq!(
                (ack.games[0].tier, ack.games[0].tick),
                (GameTier::Logic, 31)
            );
            let mut fast = 0.0;
            for _ in 0..30 {
                fast = step(&mut ws);
            }
            assert!(
                (fast / slow - 2.0).abs() < 0.05,
                "twice the speed: {slow} then {fast}"
            );
            assert_eq!(player().0, ids, "the same World and entities");
            assert_eq!(
                state(&ws, "steps"),
                Value::Int(61),
                "the score kept counting"
            );

            // A broken edit: its start faults, so nothing of it applies.
            let broken = faster.replace("cx.spawn(", "let zero = 0; let _ = 1 / zero; cx.spawn(");
            let patch = game_patch_to(&mut edits, &host, (2, 3), &broken);
            host.send(&patch);
            assert!(
                answer_or_staged(&mut session, &mut host).is_none(),
                "staged"
            );
            session.reload(std::slice::from_mut(&mut ws), &mut Assets::default());
            assert!(matches!(
                host.read(),
                Some(RuntimeMessage::Log {
                    level: LogLevel::Error,
                    ..
                })
            ));
            let Some(RuntimeMessage::Nack(nack)) = host.read() else {
                panic!("not refused");
            };
            assert_eq!(nack.last_good_revision, Revision(2));
            assert!(
                (step(&mut ws) / fast - 1.0).abs() < 0.05,
                "the last good runs on"
            );
            assert_eq!(player().0, ids);
            assert_eq!(state(&ws, "steps"), Value::Int(62));
        }

        #[test]
        fn a_faulting_candidate_refuses_the_revision_with_everything_at_its_last_good() {
            let mut ws = mounted(mount_game);
            let (mut session, mut host, mut edits) = session(&mut ws, GAME);
            ws.store.run_frame_clients(TICK);
            let before = width(&ws);

            // The view widens and the next tick divides by zero.
            let broken = GAME
                .replace("steps += 1;", "steps += 1 / (steps - steps);")
                .replace("200dp", "300dp");
            let patch = game_patch_to(&mut edits, &host, (1, 2), &broken);
            host.send(&patch);
            assert!(
                answer_or_staged(&mut session, &mut host).is_none(),
                "staged"
            );
            session.reload(std::slice::from_mut(&mut ws), &mut Assets::default());
            match host.read() {
                Some(RuntimeMessage::Log { level, line }) => {
                    assert_eq!(level, LogLevel::Error);
                    assert!(line.contains("`Runner` faulted at tick"), "{line}");
                }
                other => panic!("not the fault: {other:?}"),
            }
            match host.read() {
                Some(RuntimeMessage::Nack(nack)) => {
                    assert_eq!(nack.stage, Stage::RuntimeCommit);
                    assert_eq!(nack.diagnostic_codes, [NACK_SYSTEM_FAULT]);
                    assert_eq!(nack.last_good_revision, Revision(1));
                }
                other => panic!("not refused: {other:?}"),
            }
            assert_eq!(width(&ws), before, "the view is the last good");
            ws.store.run_frame_clients(TICK);
            assert_eq!(state(&ws, "steps"), Value::Int(2), "so is the game");

            // The fix goes on from the last good revision.
            let fixed = GAME.replace("200dp", "300dp");
            let patch = game_patch_to(&mut edits, &host, (1, 3), &fixed);
            let answer = apply(&mut session, &mut host, &mut ws, &patch);
            assert!(matches!(answer, RuntimeMessage::Ack(_)), "{answer:?}");
        }

        #[test]
        fn a_start_edit_rebuilds_the_world_keeping_the_player() {
            let mut ws = mounted(mount_game);
            let (mut session, mut host, mut edits) = session(&mut ws, GAME);
            for _ in 0..5 {
                ws.store.run_frame_clients(TICK);
            }
            let moved = GAME.replace("0.0f32, 1.0f32, 0.0f32", "4.0f32, 1.0f32, 0.0f32");
            let patch = game_patch_to(&mut edits, &host, (1, 2), &moved);
            let RuntimeMessage::Ack(ack) = apply(&mut session, &mut host, &mut ws, &patch) else {
                panic!("not applied");
            };
            assert_eq!(
                ack.games[0].tier,
                GameTier::WorldRebuild {
                    keep: Rebuild::KeepCharacters
                }
            );
            assert_eq!((ack.games[0].tick, ack.games[0].characters), (0, 1));
            assert_eq!(state(&ws, "steps"), Value::Int(0), "the World starts over");
        }

        #[test]
        fn a_game_replaying_a_tape_takes_no_live_patch() {
            let mut ws = mounted(mount_game);
            let (mut session, mut host, mut edits) = session(&mut ws, GAME);
            let game = game_session(&ws.store, ws.root.expect("built")).expect("a game");
            let tape = viso_behavior::game::InputTape {
                ticks: 600,
                ..viso_behavior::game::InputTape::new(0, 60)
            };
            game.borrow_mut().scheduler_mut().play(tape).expect("plays");
            ws.store.run_frame_clients(TICK);

            let faster = GAME.replace("steps += 1;", "steps += 10;");
            let patch = game_patch_to(&mut edits, &host, (1, 2), &faster);
            host.send(&patch);
            assert!(
                answer_or_staged(&mut session, &mut host).is_none(),
                "staged"
            );
            session.reload(std::slice::from_mut(&mut ws), &mut Assets::default());
            assert!(
                matches!(host.read(), Some(RuntimeMessage::Log { level: LogLevel::Warn, line }) if line.contains("replays a tape")),
            );
            let Some(RuntimeMessage::Nack(nack)) = host.read() else {
                panic!("not refused");
            };
            assert_eq!(
                (nack.stage, &nack.diagnostic_codes[..]),
                (Stage::RuntimeStage, &[NACK_REPLAYING.to_owned()][..])
            );
            ws.store.run_frame_clients(TICK);
            assert_eq!(
                state(&ws, "steps"),
                Value::Int(2),
                "the recorded code runs on"
            );
        }

        fn ask(
            session: &mut HotReloadSession,
            host: &mut Host,
            ws: &mut WindowState,
            query: GameQuery,
        ) {
            host.send(&HostMessage::Game(query));
            until(session, |s| !s.queries.is_empty());
            session.reload(std::slice::from_mut(ws), &mut Assets::default());
        }

        fn answered(host: &mut Host) -> GameAnswer {
            match host.read() {
                Some(RuntimeMessage::Game(answer)) => answer,
                other => panic!("not an answer: {other:?}"),
            }
        }

        #[test]
        fn a_peek_reads_any_state_of_the_running_game() {
            let mut ws = mounted(mount_game);
            let (mut session, mut host, edits) = session(&mut ws, GAME);
            for _ in 0..3 {
                ws.store.run_frame_clients(TICK);
            }
            let peek = |id, system: &str, state: &str| GameQuery {
                id,
                file: edits.file,
                ask: GameAsk::Peek {
                    system: system.into(),
                    state: state.into(),
                },
            };
            ask(&mut session, &mut host, &mut ws, peek(7, "Runner", "steps"));
            let answer = answered(&mut host);
            assert_eq!(answer.id, 7);
            let Ok(GameReply::State {
                value,
                tick,
                sessions,
            }) = answer.reply
            else {
                panic!("no state: {answer:?}");
            };
            assert_eq!(Value::decode(&value), Ok(Value::Int(3)));
            assert_eq!((tick, sessions), (3, 1));

            ask(&mut session, &mut host, &mut ws, peek(8, "Runner", "speed"));
            assert_eq!(answered(&mut host).reply, Err(GameRefusal::UnknownState));
            let elsewhere = GameQuery {
                file: FileId(9),
                ..peek(9, "Runner", "steps")
            };
            ask(&mut session, &mut host, &mut ws, elsewhere);
            assert_eq!(answered(&mut host).reply, Err(GameRefusal::NotRunning));
        }

        #[test]
        fn a_recording_restarts_the_world_and_answers_with_a_tape_that_replays_it() {
            let mut ws = mounted(mount_game);
            let (mut session, mut host, edits) = session(&mut ws, GAME);
            let game = game_session(&ws.store, ws.root.expect("built")).expect("a game");
            for _ in 0..5 {
                ws.store.run_frame_clients(TICK);
            }
            let record = |id, span| GameQuery {
                id,
                file: edits.file,
                ask: GameAsk::Record { span },
            };
            // 50 ms at 60 Hz: three ticks.
            ask(
                &mut session,
                &mut host,
                &mut ws,
                record(1, RecordSpan::Millis(50)),
            );
            assert_eq!(state(&ws, "steps"), Value::Int(0), "the World restarted");
            ask(
                &mut session,
                &mut host,
                &mut ws,
                record(2, RecordSpan::Ticks(9)),
            );
            assert_eq!(answered(&mut host).reply, Err(GameRefusal::Recording));

            ws.store.run_frame_clients(TICK);
            ws.store.frame_clients_key(viso_ui::input::Key::Space, true);
            ws.store.run_frame_clients(TICK);
            session.reload(std::slice::from_mut(&mut ws), &mut Assets::default());
            assert_eq!(session.recordings.len(), 1, "two ticks of three");
            ws.store
                .frame_clients_key(viso_ui::input::Key::Space, false);
            ws.store.run_frame_clients(TICK);
            session.reload(std::slice::from_mut(&mut ws), &mut Assets::default());
            let answer = answered(&mut host);
            let Ok(GameReply::Tape { tape }) = answer.reply else {
                panic!("no tape: {answer:?}");
            };
            let tape = viso_behavior::game::InputTape::decode(&tape).expect("a tape");
            assert_eq!((answer.id, tape.ticks), (1, 3));
            assert!(!game.borrow().scheduler().recording());

            // The tape replays the run on a fresh game of the same build.
            let module = Rc::clone(game.borrow().module());
            let vm = GameSession::stage(&module).expect("links");
            let mut replay =
                viso_behavior::game::Scheduler::with_seed(vm, tape.seed).expect("starts");
            replay.play(tape).expect("fits");
            replay.step(3);
            let jumps = replay.instance(0).states()[1].clone();
            assert_eq!(jumps, Value::Int(1), "the jump replays");
            assert_eq!(state(&ws, "jumps"), Value::Int(1));
        }

        #[test]
        fn a_restarted_game_resumes_at_its_tick_with_its_states() {
            let mut old = mounted(mount_game);
            let (mut session, mut host, mut edits) = session(&mut old, GAME);
            for _ in 0..3 {
                old.store.run_frame_clients(TICK);
            }
            let steps = state(&old, "steps");
            assert_ne!(steps, Value::Int(0));
            let (bytes, _) = snapshot_of(&mut session, &mut host, std::slice::from_mut(&mut old));
            drop((session, host, old));

            let mut new = mounted(mount_game);
            assert_eq!(state(&new, "steps"), Value::Int(0), "a fresh build");
            let ui = restore_of(&mut edits, GAME);
            let (mut session, mut host) = restoring(&mut new, Some(&bytes), ui, Vec::new());
            assert_eq!(state(&new, "steps"), steps);
            session.reload(std::slice::from_mut(&mut new), &mut Assets::default());
            let Some(RuntimeMessage::Restored(report)) = host.read() else {
                panic!("no restore report");
            };
            let [game] = report.games[..] else {
                panic!("{report:?}");
            };
            assert_eq!((game.sessions, game.reset), (1, 0), "{game:?}");
            assert!(game.states >= 2 && game.tick > 0, "{game:?}");
        }

        #[test]
        fn a_game_without_its_view_in_the_patch_does_not_load() {
            let mut ws = mounted(mount_game);
            let (mut session, mut host, edits) = session(&mut ws, GAME);
            let HostMessage::Patch(mut patch) = host.patch(1, 2) else {
                unreachable!()
            };
            patch.sections = vec![PatchSection::System(SystemPatch {
                games: vec![viso_view::dev::wire::GamePatch {
                    file: edits.file,
                    tier: GameTier::Logic,
                }],
            })];
            let answer = apply(&mut session, &mut host, &mut ws, &HostMessage::Patch(patch));
            let RuntimeMessage::Nack(nack) = answer else {
                panic!("not refused: {answer:?}");
            };
            assert_eq!(
                (nack.stage, &nack.diagnostic_codes[..]),
                (
                    Stage::RuntimeStage,
                    &[NACK_UNLOADABLE_SYSTEM.to_owned()][..]
                )
            );
        }
    }
}
