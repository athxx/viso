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
//! the file. Without `viso run` there is no session: the mounts are dropped.
//! Without the feature this module is not compiled and a `view!` records
//! nothing.

mod link;
pub(crate) mod overlay;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use viso_platform::{LoopWaker, WindowId};
use viso_runtime::RuntimeCx;
use viso_ui::NodeId;
use viso_ui::state::{StateId, StateKey};
use viso_view::dev::commit::{Candidate, CommitReport, LiveRuntime, commit, static_nodes};
use viso_view::dev::wire::{
    CommitCounts, Domain, Domains, FileCommit, FileId, MAX_NOTICES, MountEntry, NACK_UNKNOWN_FILE,
    NACK_UNLOADABLE_VIEW, Notice, PatchAck, PatchBundle, PatchNack, PatchSection, PatchTimings,
    RESET_NOTICE, ReloadPlan, Revision, RuntimeIdentity, RuntimeMessage, SchemaFingerprint, Stage,
};
use viso_view::{MountRecord, ViewHost, take_mounts};

use crate::WindowState;
use link::{DevLink, Incoming};

/// The domains this runtime applies.
const APPLIES: Domains = Domains::NONE.with(Domain::Ui);

/// The hot reload session of an app: its mounted views and their files, and
/// the dev channel to `viso run`, opened with the first mount.
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
}

/// A patch the runtime loaded, to commit at the next frame boundary.
struct StagedPatch {
    base_revision: Revision,
    next_revision: Revision,
    domains: Domains,
    views: Vec<StagedView>,
    decode: Duration,
    stage: Duration,
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
    /// `viso run` named with `waker` on the first, and reports the files not
    /// mounted before. A mount of a file a patch already committed is
    /// rebuilt from the view the file runs.
    pub(crate) fn adopt(&mut self, waker: impl FnOnce() -> LoopWaker, ws: &mut WindowState) {
        take_mounts(&mut self.records);
        if self.records.is_empty() {
            return;
        }
        if !self.started {
            self.started = true;
            let schema = SchemaFingerprint(self.records[0].schema);
            self.link = DevLink::from_env(waker(), schema, APPLIES);
        }
        let Some(link) = &mut self.link else {
            self.records.clear();
            return;
        };
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
                    &mut self.scratch,
                );
            }
            self.views.push(view);
        }
        if !mounted.is_empty() {
            link.send(RuntimeMessage::Mounts(mounted));
        }
    }

    /// Takes what the dev channel delivered and requests a frame for each
    /// window with a mount when a patch was staged or a failure changed.
    pub(crate) fn wakeup(&mut self, cx: &mut RuntimeCx<'_>) {
        if !self.receive() {
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
    /// before it is staged, and NACKed when either fails (§36, §38), and
    /// failures. Returns whether a patch was staged or a failure changed.
    fn receive(&mut self) -> bool {
        let Some(link) = &mut self.link else {
            return false;
        };
        let mut changed = false;
        while let Some(incoming) = link.poll() {
            match incoming {
                Incoming::Connected(identity) => self.identity = Some(identity),
                Incoming::Patch(bundle, decode) => {
                    let Some(identity) = &self.identity else {
                        continue;
                    };
                    let started = Instant::now();
                    // A patch may chain onto one staged before it.
                    let mut expected = identity.clone();
                    if let Some(last) = self.patches.last() {
                        expected.current_revision = last.next_revision;
                    }
                    let staged = expected
                        .check(&bundle)
                        .and_then(|()| stage(*bundle, &self.files));
                    match staged {
                        Ok(mut staged) => {
                            staged.decode = decode;
                            staged.stage = started.elapsed();
                            self.patches.push(staged);
                            changed = true;
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
                Incoming::Undecodable(error) => {
                    if let Some(identity) = &self.identity {
                        link.send(RuntimeMessage::Nack(identity.undecodable(error)));
                    }
                }
            }
        }
        changed
    }

    /// Drops the mounts of a closed window.
    pub(crate) fn close(&mut self, window: WindowId) {
        self.views.retain(|view| view.window != window);
    }

    /// Commits the staged patches in revision order at the frame boundary
    /// before the windows flush, answering each, and shows the failures that
    /// changed. A mount recorded since the last adoption is not a window's
    /// build (a list row) and is not tracked.
    pub(crate) fn reload(&mut self, windows: &mut [WindowState]) {
        take_mounts(&mut self.records);
        self.records.clear();
        if !self.patches.is_empty() {
            self.views.retain(|view| {
                windows
                    .iter()
                    .any(|ws| ws.window == view.window && ws.store.arena().is_live(view.root))
            });
        }
        for staged in std::mem::take(&mut self.patches) {
            let answer = self.commit_patch(staged, windows);
            if let Some(link) = &mut self.link {
                link.send(answer);
            }
        }
        if std::mem::take(&mut self.failures_changed) {
            show_failures(&self.files, &self.views, windows, &mut self.scratch);
        }
    }

    /// Commits one staged patch: each view to each mount of its file. The
    /// ACK to send, or the NACK of a patch staged behind one that failed.
    fn commit_patch(&mut self, staged: StagedPatch, windows: &mut [WindowState]) -> RuntimeMessage {
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
        identity.current_revision = staged.next_revision;
        let mut files = Vec::with_capacity(staged.views.len());
        let mut notices = Vec::new();
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
                    &mut self.scratch,
                );
                counts.mounts += 1;
                counts.migrated += report.migrated;
                counts.reset += report.reset;
                counts.focus_lost += u32::from(report.focus_lost);
                counts.scroll_lost += report.scroll_lost;
                counts.handlers_lost += u32::from(report.handlers_lost);
                for notice in report.notices {
                    let notice = Notice {
                        file: FileId(view.file as u32),
                        code: RESET_NOTICE.to_owned(),
                        start: notice.start,
                        end: notice.end,
                        message: notice.message,
                    };
                    if notices.len() < MAX_NOTICES && !notices.contains(&notice) {
                        notices.push(notice);
                    }
                }
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
        let micros = |d: Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        RuntimeMessage::Ack(PatchAck {
            revision: staged.next_revision,
            applied_domains: staged.domains,
            files,
            notices,
            timings: PatchTimings {
                decode_us: micros(staged.decode),
                stage_us: micros(staged.stage),
                commit_us: micros(started.elapsed()),
            },
        })
    }
}

/// Loads every view of `bundle`, which the runtime checked against its
/// revision: a view naming a file the runtime did not report, or whose
/// behavior does not load, refuses the whole patch.
fn stage(bundle: PatchBundle, files: &[ViewFile]) -> Result<StagedPatch, PatchNack> {
    let refuse = |code: &str| PatchNack {
        base_revision: bundle.base_revision,
        candidate_revision: bundle.next_revision,
        stage: Stage::RuntimeStage,
        diagnostic_codes: vec![code.to_owned()],
        last_good_revision: Revision::default(),
    };
    let domains = bundle
        .sections
        .iter()
        .fold(Domains::NONE, |domains, s| domains.with(s.domain()));
    let mut views = Vec::new();
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
        }
    }
    Ok(StagedPatch {
        base_revision: bundle.base_revision,
        next_revision: bundle.next_revision,
        domains,
        views,
        decode: Duration::ZERO,
        stage: Duration::ZERO,
    })
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
    }
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

/// Commits `candidate` by `plan` to the mount `view` in `ws`; `statics` is
/// the static shape of the build's view, which names the static nodes of a
/// mount that recorded none.
fn commit_view(
    ws: &mut WindowState,
    view: &mut LiveView,
    statics: &[u32],
    candidate: &Candidate,
    plan: &ReloadPlan,
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
    let report = commit(&mut live, candidate, plan);
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
        HostMessage, NACK_BUILD_MISMATCH, NACK_MALFORMED, NACK_REVISION_MISMATCH,
        NACK_UNKNOWN_SESSION, UiPatch, source_hash,
    };

    use super::link::fake::{self, Host};
    use super::*;

    const COUNTER: &str = include_str!("../../tests/fixtures/counter.vs");
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
            session.receive();
            assert!(Instant::now() < deadline, "the session never got there");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The host's side of one file: its edits compiled and planned the way
    /// `viso run` does, against the view the runtime last ACKed.
    struct Edits {
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
                origin,
                profile,
                last_good,
                sent: None,
            }
        }

        /// The `ui` section moving file 0 to `source`.
        fn section(&mut self, source: &str) -> PatchSection {
            let candidate = plan_view_for(source, &self.origin, self.profile.clone())
                .expect("the edit compiles");
            let view = view_patch(FileId(0), &self.last_good, &candidate);
            self.sent = Some(candidate);
            PatchSection::Ui(UiPatch { views: vec![view] })
        }

        /// A patch from `base` to `next` moving file 0 to `source`.
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

    /// Sends `patch`, commits it at the next frame boundary of `ws` and
    /// returns the runtime's answer.
    fn apply(
        session: &mut HotReloadSession,
        host: &mut Host,
        ws: &mut WindowState,
        patch: &HostMessage,
    ) -> RuntimeMessage {
        host.send(patch);
        if let Some(answer) = answer_or_staged(session, host) {
            return answer;
        }
        session.reload(std::slice::from_mut(ws));
        host.read().expect("an answer")
    }

    /// Receives until the session staged a patch (`None`) or answered the
    /// host (the answer).
    fn answer_or_staged(session: &mut HotReloadSession, host: &mut Host) -> Option<RuntimeMessage> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = host.stream.as_ref().unwrap();
        stream.set_nonblocking(true).unwrap();
        let answered = loop {
            session.receive();
            if !session.patches.is_empty() {
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

    /// Commits `source` as the next revision and returns the ACK.
    fn accept(
        session: &mut HotReloadSession,
        host: &mut Host,
        ws: &mut WindowState,
        edits: &mut Edits,
        source: &str,
    ) -> PatchAck {
        let base = revision(session);
        let patch = edits.patch(host, base, base + 1, source);
        match apply(session, host, ws, &patch) {
            RuntimeMessage::Ack(ack) => {
                edits.acked();
                ack
            }
            other => panic!("not an ACK: {other:?}"),
        }
    }

    fn revision(session: &HotReloadSession) -> u64 {
        session.identity.as_ref().unwrap().current_revision.0
    }

    /// The live value of the counter's `count`.
    fn count(ws: &WindowState, session: &HotReloadSession) -> Option<StateValue> {
        let (_, id) = session.views[0]
            .cells
            .iter()
            .copied()
            .find(|&(_, id)| matches!(ws.states.get(id), Some(StateValue::Int(_))))?;
        ws.states.get(id)
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
        assert_eq!(ack.applied_domains, APPLIES);
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
        accept(&mut session, &mut host, &mut ws, &mut edits, &edited);
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
        accept(&mut session, &mut host, &mut ws, &mut edits, &labelled("B"));
        assert!(dirty(&ws).iter().all(|d| d.is_empty()), "{:?}", dirty(&ws));
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

    /// The VM value of the logger's `log`.
    fn log(session: &HotReloadSession) -> Option<String> {
        let host = session.views[0].host.as_ref()?.borrow();
        Some(host.state(host.state_slot("log")?)?.as_str()?.to_owned())
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
        session.reload(std::slice::from_mut(&mut ws));
        assert!(ws.dev_overlay.is_some(), "the failure is shown");
        assert_eq!((ws.root, revision(&session)), (Some(root), 1));
        assert_eq!(subtree(&ws.store, root), nodes, "no node changed");

        host.send(&HostMessage::Failure {
            file: FileId(0),
            lines: Vec::new(),
        });
        until(&mut session, |s| s.failures_changed);
        session.reload(std::slice::from_mut(&mut ws));
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
        let PatchSection::Ui(ui) = &mut patch.sections[0];
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

    #[test]
    fn a_patch_naming_an_unreported_file_changes_nothing() {
        let mut ws = counter();
        let (mut session, mut host, mut edits) = session(&mut ws, COUNTER);
        let mut patch = edits.patch(&host, 1, 2, COUNTER);
        let HostMessage::Patch(bundle) = &mut patch else {
            unreachable!()
        };
        let PatchSection::Ui(ui) = &mut bundle.sections[0];
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

    /// The live values of the view's states.
    fn values(ws: &WindowState, session: &HotReloadSession) -> Vec<StateValue> {
        let cells = &session.views[0].cells;
        cells
            .iter()
            .filter_map(|&(_, id)| ws.states.get(id))
            .collect()
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
            let view = view_patch(FileId(0), &edits.last_good, &candidate);
            let planned = Instant::now();
            let HostMessage::Patch(mut patch) = host.patch(base, base + 1) else {
                unreachable!()
            };
            patch.sections = vec![PatchSection::Ui(UiPatch { views: vec![view] })];
            let body = viso_view::dev::wire::encode(&HostMessage::Patch(patch));
            let encoded = Instant::now();
            host.send_raw(&body);
            while session.patches.is_empty() {
                session.receive();
                std::hint::spin_loop();
            }
            let staged = Instant::now();
            session.reload(std::slice::from_mut(&mut ws));
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
        session.reload(&mut []);
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
        session.reload(&mut []);
        assert!(matches!(host.read(), Some(RuntimeMessage::Ack(a)) if a.revision == Revision(2)));
    }

    /// Receives until the session answered the host, and the answer.
    fn answer(session: &mut HotReloadSession, host: &mut Host) -> RuntimeMessage {
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = host.stream.as_ref().unwrap();
        stream.set_nonblocking(true).unwrap();
        while !matches!(stream.peek(&mut [0]), Ok(1)) {
            session.receive();
            assert!(Instant::now() < deadline, "no answer");
            std::thread::sleep(Duration::from_millis(1));
        }
        stream.set_nonblocking(false).unwrap();
        host.read().expect("an answer")
    }
}
