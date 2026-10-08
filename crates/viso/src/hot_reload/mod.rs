//! The development session behind the `hot-reload` feature: the app's side of
//! `viso run`. Every `view!` a window mounts is adopted here and reported to
//! the host as a [`MountEntry`]; the host watches and compiles the project,
//! and each edit it accepts arrives as a patch that commits to the running
//! windows as one hot reload transaction at the next frame boundary
//! (`Viso_Hot_Reload.md` §3, §35–§37).
//!
//! The app watches no file and reads no project source. A patch's `ui`
//! section carries the views and catalogs the host accepted; the session plans
//! every view of the patch before it commits any, so a patch that does not
//! plan changes nothing and is NACKed, and one that does commits each view to
//! each of its mounts, moves the revision and is ACKed with what the commit
//! kept and lost. An edit the host rejected arrives as a failure, shown over
//! the last-good UI of each window mounting the file until the host clears it
//! or a patch commits the file. Without `viso run` there is no session: the
//! mounts are dropped. Without the feature this module is not compiled and a
//! `view!` records nothing.

mod link;
pub(crate) mod overlay;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use viso_dsl::frontend::Origin;
use viso_dsl::hir::{CapabilitySet, TargetProfile};
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, plan_view_for, static_nodes, transact};
use viso_dsl::i18n::{CatalogFile, Messages};
use viso_dsl::ir::binding_ir::NodeKey;
use viso_dsl::{Diagnostic, Severity};
use viso_platform::{LoopWaker, WindowId};
use viso_runtime::RuntimeCx;
use viso_ui::NodeId;
use viso_ui::state::{StateId, StateKey};
use viso_view::dev::wire::{
    CatalogSource, CommitCounts, Domain, Domains, FileCommit, FileId, MAX_CODE, MAX_CODES,
    MAX_NOTICES, MountEntry, NACK_UNKNOWN_FILE, Notice, PatchAck, PatchBundle, PatchNack,
    PatchSection, PatchTimings, RuntimeIdentity, RuntimeMessage, SchemaFingerprint, Stage,
    UiSources, source_hash,
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
    /// Patches checked against the runtime and waiting for the frame
    /// boundary, in revision order.
    patches: Vec<StagedPatch>,
    /// Indexed by [`FileId`].
    files: Vec<ViewFile>,
    catalogs: Vec<CatalogDir>,
    views: Vec<LiveView>,
    /// Whether a file's failure changed since the overlays were shown.
    failures_changed: bool,
    /// Scratch the mount queue drains into.
    records: Vec<MountRecord>,
    /// Scratch a commit frees subtrees with.
    scratch: Vec<NodeId>,
}

/// A patch the runtime accepted, to commit at the next frame boundary.
struct StagedPatch {
    bundle: Box<PatchBundle>,
    decode: Duration,
    stage: Duration,
}

/// A package's message catalogs, as the host last sent them.
struct CatalogDir {
    source: &'static str,
    dir: &'static str,
    /// `None` until the host sends them, or when the directory holds none.
    messages: Option<Rc<Messages>>,
}

/// A mounted `.vs` file and the candidate its mounts currently match.
struct ViewFile {
    path: &'static str,
    /// The source the running build embedded.
    embedded: &'static str,
    origin: Origin,
    /// The grants the build checked the file against.
    capabilities: &'static [&'static str],
    /// The package's catalogs, by index into the session's.
    catalog: Option<usize>,
    /// The candidate the mounts match, compiled from `embedded` with the
    /// first patch of the file.
    last_good: Option<CandidatePlan>,
    /// The overlay lines of the latest edit while it is rejected.
    failure: Option<Vec<String>>,
}

impl ViewFile {
    /// The profile the build compiled the file with, its catalogs `messages`.
    fn profile(&self, messages: Option<Rc<Messages>>) -> TargetProfile {
        let mut capabilities = CapabilitySet::new();
        for &capability in self.capabilities {
            capabilities.insert(capability);
        }
        TargetProfile {
            capabilities,
            messages,
            ..TargetProfile::default()
        }
    }
}

/// One mount of a file in a window.
struct LiveView {
    window: WindowId,
    file: usize,
    root: NodeId,
    /// The live node of each static template slot, ascending by key; empty
    /// until the first reload seeds it.
    nodes: Vec<(NodeKey, NodeId)>,
    host: Option<Rc<RefCell<ViewHost>>>,
    /// Each state cell by its durable key.
    cells: Vec<(StateKey, StateId)>,
}

impl HotReloadSession {
    /// Adopts the views `ws`'s build just mounted, opening the dev channel
    /// `viso run` named with `waker` on the first, and reports the files not
    /// mounted before.
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
            let file = match self.files.iter().position(|file| file.path == record.file) {
                Some(file) => file,
                None => {
                    let catalog = record.catalog.map(|(source, dir)| {
                        self.catalogs
                            .iter()
                            .position(|c| c.dir == dir)
                            .unwrap_or_else(|| {
                                self.catalogs.push(CatalogDir {
                                    source,
                                    dir,
                                    messages: None,
                                });
                                self.catalogs.len() - 1
                            })
                    });
                    let file = ViewFile {
                        path: record.file,
                        embedded: record.source,
                        origin: Origin {
                            package: record.package.into(),
                            module: record.module.iter().map(|&m| m.into()).collect(),
                            language: record.language.map(Into::into),
                        },
                        capabilities: record.capabilities,
                        catalog,
                        last_good: None,
                        failure: None,
                    };
                    mounted.push(entry(self.files.len(), &file, record.catalog));
                    self.files.push(file);
                    self.files.len() - 1
                }
            };
            for &(key, id) in &record.cells {
                ws.states.bind_key(id, key);
            }
            self.views.push(LiveView {
                window: ws.window,
                file,
                root: record.root,
                nodes: record
                    .nodes
                    .iter()
                    .map(|&(key, node)| (NodeKey(key), node))
                    .collect(),
                host: record.host,
                cells: record.cells,
            });
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
    /// each checked against the revision the runtime will match before it is
    /// staged and NACKed when it fails (§36), and failures. Returns whether a
    /// patch was staged or a failure changed.
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
                        expected.current_revision = last.bundle.next_revision;
                    }
                    match expected.check(&bundle) {
                        Ok(()) => {
                            self.patches.push(StagedPatch {
                                bundle,
                                decode,
                                stage: started.elapsed(),
                            });
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
            let answer = self.commit_patch(&staged, windows);
            if let Some(link) = &mut self.link {
                link.send(answer);
            }
        }
        if std::mem::take(&mut self.failures_changed) {
            show_failures(&self.files, &self.views, windows, &mut self.scratch);
        }
    }

    /// Commits one staged patch, or none of it: the ACK or NACK to send.
    fn commit_patch(
        &mut self,
        staged: &StagedPatch,
        windows: &mut [WindowState],
    ) -> RuntimeMessage {
        let started = Instant::now();
        let Some(identity) = &self.identity else {
            unreachable!("a patch is staged only once the host accepted the launch");
        };
        // A patch staged behind one that failed no longer chains.
        if let Err(nack) = identity.check(&staged.bundle) {
            return RuntimeMessage::Nack(nack);
        }
        let current = identity.current_revision;
        let bundle = &staged.bundle;
        let mut planned = Vec::new();
        let mut catalogs = Vec::new();
        for section in &bundle.sections {
            match section {
                PatchSection::Ui(ui) => {
                    if let Err(nack) = self.plan_ui(ui, &mut planned, &mut catalogs) {
                        // Nothing commits: the views planned so far keep
                        // their last-good.
                        for (index, last_good, _) in planned {
                            self.files[index].last_good = Some(last_good);
                        }
                        return RuntimeMessage::Nack(PatchNack {
                            base_revision: bundle.base_revision,
                            candidate_revision: bundle.next_revision,
                            last_good_revision: current,
                            ..nack
                        });
                    }
                }
            }
        }
        // Everything planned: commit.
        for (index, messages) in catalogs {
            self.catalogs[index].messages = messages;
        }
        let mut files = Vec::with_capacity(planned.len());
        let mut notices = Vec::new();
        for (file, last_good, candidate) in planned {
            let mut candidate = candidate;
            let mut counts = CommitCounts::default();
            for view in self.views.iter_mut().filter(|view| view.file == file) {
                let Some(ws) = windows.iter_mut().find(|ws| ws.window == view.window) else {
                    continue;
                };
                let report = commit_view(ws, view, &last_good, candidate, &mut self.scratch);
                candidate = report.candidate;
                counts.mounts += 1;
                counts.migrated += report.migrated;
                counts.reset += report.reset;
                counts.focus_lost += report.focus_lost;
                counts.scroll_lost += report.scroll_lost;
                counts.handlers_lost += report.handlers_lost;
                for notice in report.notices {
                    let notice = Notice {
                        file: FileId(file as u32),
                        code: notice.code.to_owned(),
                        start: notice.primary.start().to_u32(),
                        end: notice.primary.end().to_u32(),
                        message: notice.message,
                    };
                    if notices.len() < MAX_NOTICES && !notices.contains(&notice) {
                        notices.push(notice);
                    }
                }
            }
            files.push(FileCommit {
                file: FileId(file as u32),
                counts,
            });
            let file = &mut self.files[file];
            file.last_good = Some(candidate);
            if file.failure.take().is_some() {
                self.failures_changed = true;
            }
        }
        if let Some(identity) = &mut self.identity {
            identity.current_revision = bundle.next_revision;
        }
        let micros = |d: Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        RuntimeMessage::Ack(PatchAck {
            revision: bundle.next_revision,
            applied_domains: bundle
                .sections
                .iter()
                .fold(Domains::NONE, |domains, s| domains.with(s.domain())),
            files,
            notices,
            timings: PatchTimings {
                decode_us: micros(staged.decode),
                stage_us: micros(staged.stage),
                commit_us: micros(started.elapsed()),
            },
        })
    }

    /// Plans every view of `ui` against its last-good candidate, compiling
    /// the catalogs it carries first, without changing anything: each planned
    /// view's last-good and candidate go to `planned` and the catalogs to
    /// adopt to `catalogs`. A view or catalog that does not plan is the
    /// NACK's stage and codes, and the caller puts the last-goods back.
    #[allow(clippy::type_complexity)]
    fn plan_ui(
        &mut self,
        ui: &UiSources,
        planned: &mut Vec<(usize, CandidatePlan, CandidatePlan)>,
        catalogs: &mut Vec<(usize, Option<Rc<Messages>>)>,
    ) -> Result<(), PatchNack> {
        let refuse = |codes: Vec<String>| PatchNack {
            base_revision: Default::default(),
            candidate_revision: Default::default(),
            stage: Stage::RuntimeStage,
            diagnostic_codes: codes,
            last_good_revision: Default::default(),
        };
        // The catalogs as the views of this patch see them.
        let mut seen: Vec<Option<Rc<Messages>>> =
            self.catalogs.iter().map(|c| c.messages.clone()).collect();
        for sent in &ui.catalogs {
            let Some(index) = self.catalogs.iter().position(|c| c.dir == sent.dir) else {
                continue;
            };
            let messages = compile_catalogs(self.catalogs[index].source, sent)
                .map_err(|code| refuse(vec![code.to_owned()]))?;
            seen[index] = messages.clone();
            catalogs.push((index, messages));
        }
        for view in &ui.views {
            let index = view.file.0 as usize;
            let Some(file) = self.files.get_mut(index) else {
                return Err(refuse(vec![NACK_UNKNOWN_FILE.to_owned()]));
            };
            let profile = file.profile(file.catalog.and_then(|c| seen[c].clone()));
            let last_good = match file.last_good.take() {
                Some(plan) => plan,
                None => plan_view_for(file.embedded, &file.origin, profile.clone())
                    .map_err(|diagnostics| refuse(codes(&diagnostics)))?,
            };
            match plan_view_for(&view.source, &file.origin, profile) {
                Ok(candidate) => planned.push((index, last_good, candidate)),
                Err(diagnostics) => {
                    file.last_good = Some(last_good);
                    file.failure = Some(overlay::failure_lines(
                        file.path,
                        &view.source,
                        &diagnostics,
                    ));
                    self.failures_changed = true;
                    return Err(refuse(codes(&diagnostics)));
                }
            }
        }
        Ok(())
    }
}

/// The inventory entry of file `index`, as the build recorded it.
fn entry(index: usize, file: &ViewFile, catalog: Option<(&str, &str)>) -> MountEntry {
    MountEntry {
        file: FileId(index as u32),
        path: file.path.into(),
        package: file.origin.package.clone(),
        module: file.origin.module.clone(),
        language: file.origin.language.clone(),
        catalog: catalog.map(|(source, dir)| (source.into(), dir.into())),
        capabilities: file.capabilities.iter().map(|&c| c.into()).collect(),
        source_hash: source_hash(file.embedded),
    }
}

/// The catalogs the host sent for one directory, compiled; the code of the
/// first error when they have one.
fn compile_catalogs(
    source: &str,
    sent: &CatalogSource,
) -> Result<Option<Rc<Messages>>, &'static str> {
    if sent.files.is_empty() {
        return Ok(None);
    }
    let files: Vec<CatalogFile> = sent
        .files
        .iter()
        .map(|file| CatalogFile {
            locale: std::path::Path::new(&file.path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_owned(),
            path: file.path.clone(),
            text: file.text.clone(),
        })
        .collect();
    let messages = Messages::compile(source, &files);
    if messages.issues().iter().any(|issue| issue.error) {
        return Err(viso_dsl::i18n::CatalogIssue::CODE);
    }
    Ok(Some(Rc::new(messages)))
}

/// The codes of the errors among `diagnostics`, each once, as a NACK carries
/// them.
fn codes(diagnostics: &[Diagnostic]) -> Vec<String> {
    let mut codes: Vec<String> = Vec::new();
    for diagnostic in diagnostics.iter().filter(|d| d.severity == Severity::Error) {
        let code = &diagnostic.code[..diagnostic.code.len().min(MAX_CODE)];
        if codes.len() < MAX_CODES && !codes.iter().any(|c| c == code) {
            codes.push(code.to_owned());
        }
    }
    codes
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

/// What committing a candidate to one mount kept and lost, and the candidate
/// for the file's next mount.
struct ViewCommit {
    candidate: CandidatePlan,
    migrated: u32,
    reset: u32,
    focus_lost: u32,
    scroll_lost: u32,
    handlers_lost: u32,
    notices: Vec<Diagnostic>,
}

/// Commits `candidate` to the mount `view` in `ws`.
fn commit_view(
    ws: &mut WindowState,
    view: &mut LiveView,
    last_good: &CandidatePlan,
    candidate: CandidatePlan,
    scratch: &mut Vec<NodeId>,
) -> ViewCommit {
    // The store maps a durable key to one cell; a file mounted twice points the
    // keys at the mount being committed.
    for &(key, id) in &view.cells {
        ws.states.bind_key(id, key);
    }
    if view.nodes.is_empty() {
        view.nodes = static_nodes(&ws.store, view.root, &last_good.tree);
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
    let reload = transact(&mut live, last_good, candidate);
    let root = live.root;
    if ws.root == Some(old_root) {
        ws.root = root;
    }
    if let Some(root) = root {
        view.root = root;
    }
    view.cells = reload
        .candidate
        .sources
        .iter()
        .filter_map(|symbol| {
            let key = StateKey::from_parts(symbol.hi, symbol.lo);
            Some((key, ws.states.id_for_key(key)?))
        })
        .collect();
    let report = reload.report;
    ViewCommit {
        candidate: reload.candidate,
        migrated: report.migrated,
        reset: report.reset,
        focus_lost: u32::from(report.focus_lost),
        scroll_lost: report.scroll_lost,
        handlers_lost: u32::from(report.handlers_lost),
        notices: report.notices,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use viso_ui::{
        BuildCx, NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Rect,
        StateValue,
    };
    use viso_view::dev::wire::{
        HostMessage, NACK_BUILD_MISMATCH, NACK_MALFORMED, NACK_REVISION_MISMATCH,
        NACK_UNKNOWN_SESSION, Revision, ViewSource,
    };

    use super::link::fake::{self, Host};
    use super::*;

    const COUNTER: &str = include_str!("../../tests/fixtures/counter.vs");
    const LOGGER: &str = include_str!("../../tests/fixtures/logger.vs");

    fn children(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut child = store.arena().links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = store.arena().links(c).and_then(|l| l.next_sibling);
        }
        out
    }

    /// A window that mounts the counter view.
    fn counter() -> WindowState {
        mounted(|cx| viso_ui_macros::view!("../../tests/fixtures/counter.vs")(cx).id())
    }

    /// A window whose build `mount`s one view, its mount recorded.
    fn mounted(mount: impl FnOnce(&mut BuildCx<'_>) -> NodeId) -> WindowState {
        let mut ws = WindowState::new(WindowId(1));
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut ws.store,
                &mut ws.states,
                &mut ws.bindings,
                &mut ws.virtual_lists,
                &mut ws.text_edits,
                &mut ws.projectors,
            );
            mount(&mut cx)
        };
        ws.root = Some(root);
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

    /// A `ui` patch from `base` to `next` setting file 0 to `source`.
    fn ui_patch(host: &Host, base: u64, next: u64, source: &str) -> HostMessage {
        let HostMessage::Patch(mut patch) = host.patch(base, next) else {
            unreachable!()
        };
        patch.sections = vec![PatchSection::Ui(UiSources {
            views: vec![ViewSource {
                file: FileId(0),
                source: source.into(),
            }],
            catalogs: Vec::new(),
        })];
        HostMessage::Patch(patch)
    }

    /// Sends `patch`, commits it at the next frame boundary of `ws` and
    /// returns the runtime's answer.
    fn apply(
        session: &mut HotReloadSession,
        host: &mut Host,
        ws: &mut WindowState,
        patch: &HostMessage,
    ) -> RuntimeMessage {
        let staged = session.patches.len() + 1;
        host.send(patch);
        until(session, |s| s.patches.len() == staged);
        session.reload(std::slice::from_mut(ws));
        host.read().expect("an answer")
    }

    /// Commits `source` as the next revision and returns the ACK.
    fn accept(
        session: &mut HotReloadSession,
        host: &mut Host,
        ws: &mut WindowState,
        source: &str,
    ) -> PatchAck {
        let base = session.identity.as_ref().unwrap().current_revision.0;
        let patch = ui_patch(host, base, base + 1, source);
        match apply(session, host, ws, &patch) {
            RuntimeMessage::Ack(ack) => ack,
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
        assert_eq!(entry.package, session.files[0].origin.package);
        // A second mount of the same file is not reported again.
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut ws.store,
                &mut ws.states,
                &mut ws.bindings,
                &mut ws.virtual_lists,
                &mut ws.text_edits,
                &mut ws.projectors,
            );
            viso_ui_macros::view!("../../tests/fixtures/counter.vs")(&mut cx).id()
        };
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
        let (mut session, mut host, _) = linked(Some(&mut ws));
        let (_, cell) = session.views[0].cells[0];
        ws.states.set(cell, StateValue::Int(5));
        let column = ws.root.unwrap();
        assert_eq!(children(&ws.store, column).len(), 2);

        let edited = COUNTER.replace(
            "Text { visible: enabled; }",
            "Text { visible: enabled; }\n            Text { }",
        );
        let ack = accept(&mut session, &mut host, &mut ws, &edited);
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
        let (mut session, mut host, _) = linked(Some(&mut ws));
        click(&mut ws);
        assert_eq!(count(&ws, &session), Some(StateValue::Int(1)));
        assert_eq!(log(&session).as_deref(), Some("one"));

        let edited = LOGGER.replace(
            "on click { count += 1; log = \"one\"; }",
            "on click { count += 10; }",
        );
        assert_ne!(edited, LOGGER);
        accept(&mut session, &mut host, &mut ws, &edited);
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
        let root = ws.root;
        host.send(&HostMessage::Failure {
            file: FileId(0),
            lines: vec!["counter.vs:3:19: E1405 expected an expression".into()],
        });
        until(&mut session, |s| s.failures_changed);
        session.reload(std::slice::from_mut(&mut ws));
        assert!(ws.dev_overlay.is_some(), "the failure is shown");
        assert_eq!((ws.root, revision(&session)), (root, 1));

        host.send(&HostMessage::Failure {
            file: FileId(0),
            lines: Vec::new(),
        });
        until(&mut session, |s| s.failures_changed);
        session.reload(std::slice::from_mut(&mut ws));
        assert!(ws.dev_overlay.is_none(), "a reverted edit clears it");
    }

    #[test]
    fn a_patch_that_does_not_plan_is_nacked_and_the_next_one_applies() {
        let mut ws = counter();
        let (mut session, mut host, _) = linked(Some(&mut ws));
        let root = ws.root.unwrap();
        let broken = ui_patch(
            &host,
            1,
            2,
            &COUNTER.replace("state count = 0;", "state count = ;"),
        );
        match apply(&mut session, &mut host, &mut ws, &broken) {
            RuntimeMessage::Nack(nack) => {
                assert_eq!(nack.stage, Stage::RuntimeStage);
                assert!(nack.diagnostic_codes.iter().any(|c| c.starts_with("E1")));
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
        assert!(
            session.files[0].last_good.is_some(),
            "the last-good is kept"
        );
        assert!(ws.dev_overlay.is_some(), "the failure is shown");

        let ack = accept(
            &mut session,
            &mut host,
            &mut ws,
            &COUNTER.replace("width: 120dp;", "width: 140dp;"),
        );
        assert_eq!(ack.revision, Revision(2));
        assert!(
            ws.dev_overlay.is_none(),
            "a committed edit clears the failure"
        );
    }

    #[test]
    fn a_patch_naming_an_unreported_file_changes_nothing() {
        let mut ws = counter();
        let (mut session, mut host, _) = linked(Some(&mut ws));
        let HostMessage::Patch(mut patch) = ui_patch(&host, 1, 2, COUNTER) else {
            unreachable!()
        };
        let PatchSection::Ui(ui) = &mut patch.sections[0];
        ui.views.push(ViewSource {
            file: FileId(9),
            source: String::new(),
        });
        match apply(&mut session, &mut host, &mut ws, &HostMessage::Patch(patch)) {
            RuntimeMessage::Nack(nack) => assert_eq!(nack.diagnostic_codes, [NACK_UNKNOWN_FILE]),
            other => panic!("not a NACK: {other:?}"),
        }
        assert_eq!(revision(&session), 1);
        assert!(
            session.files[0].last_good.is_some(),
            "the planned view is put back"
        );
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
        let (mut session, mut host, _) = linked(Some(&mut ws));
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
        accept(&mut session, &mut host, &mut ws, &labelled);
        let kept = values(&ws, &session);
        assert!(kept.contains(&StateValue::Int(5)) && kept.contains(&StateValue::Bool(false)));

        let retyped = labelled.replace("state count = 0;", "state count: F64 = 0.0;");
        let ack = accept(&mut session, &mut host, &mut ws, &retyped);
        assert_eq!(ack.revision, Revision(3));
        let kept = values(&ws, &session);
        assert!(kept.contains(&StateValue::Float(5.0)), "{kept:?}");
        assert!(kept.contains(&StateValue::Bool(false)), "{kept:?}");
    }

    /// Patch-to-pixels latency of a one-property edit over the loopback
    /// channel, split into the transport (send to staged: encode, write,
    /// read, decode, check) and the pipeline (plan, commit, relayout and
    /// repaint, without a GPU upload). A release measurement:
    /// `cargo test --release -p viso --features hot-reload --lib -- --ignored
    /// patch_to_pixels --nocapture`.
    #[test]
    #[ignore = "a release measurement"]
    fn patch_to_pixels() {
        const EDITS: usize = 60;
        let mut ws = counter();
        ws.surface_size = (800, 600);
        let (mut session, mut host, _) = linked(Some(&mut ws));
        // A child's width, so the edit moves pixels (the root fills the
        // surface). The first patch adds the property and is not sampled.
        let source = |width: f32| {
            COUNTER.replace(
                "Text { visible: enabled; }",
                &format!("Text {{ visible: enabled; width: {width}dp; }}"),
            )
        };
        accept(&mut session, &mut host, &mut ws, &source(30.0));
        ws.relayout_and_paint();
        let mut transport = Vec::with_capacity(EDITS);
        let mut pipeline = Vec::with_capacity(EDITS);
        for n in 0..EDITS {
            let width = if n % 2 == 0 { 40.0 } else { 30.0 };
            let base = revision(&session);
            let patch = ui_patch(&host, base, base + 1, &source(width));
            let sent = Instant::now();
            host.send(&patch);
            while session.patches.is_empty() {
                session.receive();
                std::hint::spin_loop();
            }
            let staged = Instant::now();
            session.reload(std::slice::from_mut(&mut ws));
            ws.relayout_and_paint();
            let painted = Instant::now();
            assert!(matches!(host.read(), Some(RuntimeMessage::Ack(_))));
            let root = ws.root.unwrap();
            assert_eq!(ws.store.bounds(children(&ws.store, root)[1]).w, width);
            transport.push(staged - sent);
            pipeline.push(painted - staged);
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
        let mut total: Vec<Duration> = transport
            .iter()
            .zip(&pipeline)
            .map(|(t, p)| *t + *p)
            .collect();
        summary("transport", &mut transport);
        summary("pipeline", &mut pipeline);
        summary("patch-to-pixels", &mut total);
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
