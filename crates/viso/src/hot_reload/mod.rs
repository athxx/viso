//! The development session behind the `hot-reload` feature: every `view!` a
//! window mounts is adopted here, its `.vs` file is watched, and each settled
//! edit is committed to the running windows as one hot reload transaction at
//! the next frame boundary.
//!
//! A file is compiled once per edit and the candidate is committed to each of
//! its mounts in turn; a candidate that does not compile changes no mount, and
//! the file keeps its last-good candidate and revision. Without the feature
//! this module is not compiled and a `view!` records nothing.

mod watch;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{
    CandidatePlan, LiveAnchors, LiveRuntime, plan_view, static_nodes, transact,
};
use viso_dsl::ir::binding_ir::NodeKey;
use viso_dsl::{Diagnostic, LineIndex};
use viso_platform::{LoopWaker, WindowId};
use viso_runtime::RuntimeCx;
use viso_ui::NodeId;
use viso_ui::state::{StateId, StateKey};
use viso_view::{MountRecord, ViewHost, take_mounts};

use crate::WindowState;
use watch::{Watcher, content_hash};

/// The hot reload session of an app: its mounted views, their files and the
/// watcher thread, started with the first mount.
#[derive(Default)]
pub(crate) struct HotReloadSession {
    watcher: Option<Watcher>,
    files: Vec<ViewFile>,
    views: Vec<LiveView>,
    /// Scratch the mount queue drains into.
    records: Vec<MountRecord>,
    /// Scratch a commit frees subtrees with.
    scratch: Vec<NodeId>,
}

/// A watched `.vs` file and the candidate its mounts currently match.
struct ViewFile {
    path: &'static str,
    /// The source the running build embedded.
    embedded: &'static str,
    origin: Origin,
    /// The candidate the mounts match, compiled from `embedded` on the first
    /// edit.
    last_good: Option<CandidatePlan>,
    /// The number of edits committed.
    revision: u32,
    /// The latest settled edit, not yet committed.
    staged: Option<String>,
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
    /// Adopts the views `ws`'s build just mounted, starting the watcher with
    /// `waker` on the first.
    pub(crate) fn adopt(&mut self, waker: impl FnOnce() -> LoopWaker, ws: &mut WindowState) {
        take_mounts(&mut self.records);
        if self.records.is_empty() {
            return;
        }
        let watcher = self.watcher.get_or_insert_with(|| Watcher::spawn(waker()));
        for record in self.records.drain(..) {
            let file = match self.files.iter().position(|file| file.path == record.file) {
                Some(file) => file,
                None => {
                    watcher.watch(
                        self.files.len(),
                        PathBuf::from(record.file),
                        content_hash(record.source),
                    );
                    self.files.push(ViewFile {
                        path: record.file,
                        embedded: record.source,
                        origin: Origin {
                            package: record.package.into(),
                            module: record.module.iter().map(|&m| m.into()).collect(),
                            language: record.language.map(Into::into),
                        },
                        last_good: None,
                        revision: 0,
                        staged: None,
                    });
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
    }

    /// Stages the edits the watcher delivered and requests a frame for each
    /// window that mounts an edited file.
    pub(crate) fn wakeup(&mut self, cx: &mut RuntimeCx<'_>) {
        if !self.stage() {
            return;
        }
        let mut asked: Vec<WindowId> = Vec::new();
        for view in &self.views {
            if self.files[view.file].staged.is_some() && !asked.contains(&view.window) {
                asked.push(view.window);
                cx.request_redraw(view.window);
            }
        }
    }

    /// Stages the edits the watcher delivered, a later edit of a file replacing
    /// an earlier one not yet committed, and returns whether any arrived.
    fn stage(&mut self) -> bool {
        let Some(watcher) = &self.watcher else {
            return false;
        };
        let mut staged = false;
        while let Some(change) = watcher.try_recv() {
            self.files[change.file].staged = Some(change.source);
            staged = true;
        }
        staged
    }

    /// Drops the mounts of a closed window.
    pub(crate) fn close(&mut self, window: WindowId) {
        self.views.retain(|view| view.window != window);
    }

    /// Commits every staged edit to its mounts, at the frame boundary before
    /// the windows flush. A mount recorded since the last adoption is not a
    /// window's build (a list row) and is not tracked.
    pub(crate) fn reload(&mut self, windows: &mut [WindowState]) {
        take_mounts(&mut self.records);
        self.records.clear();
        if self.files.iter().all(|file| file.staged.is_none()) {
            return;
        }
        let Self {
            files,
            views,
            scratch,
            ..
        } = self;
        views.retain(|view| {
            windows
                .iter()
                .any(|ws| ws.window == view.window && ws.store.arena().is_live(view.root))
        });
        for (index, file) in files.iter_mut().enumerate() {
            let Some(source) = file.staged.take() else {
                continue;
            };
            let last_good = match file.last_good.take() {
                Some(plan) => plan,
                None => match plan_view(file.embedded, &file.origin) {
                    Ok(plan) => plan,
                    Err(diagnostics) => {
                        report(file.path, file.embedded, &diagnostics);
                        continue;
                    }
                },
            };
            let mut candidate = match plan_view(&source, &file.origin) {
                Ok(candidate) => candidate,
                Err(diagnostics) => {
                    report(file.path, &source, &diagnostics);
                    file.last_good = Some(last_good);
                    continue;
                }
            };
            let mut notices = Vec::new();
            for view in views.iter_mut().filter(|view| view.file == index) {
                let Some(ws) = windows.iter_mut().find(|ws| ws.window == view.window) else {
                    continue;
                };
                candidate = commit_view(ws, view, &last_good, candidate, scratch, &mut notices);
            }
            report(file.path, &source, &notices);
            file.last_good = Some(candidate);
            file.revision += 1;
        }
    }
}

/// Commits `candidate` to the mount `view` in `ws`, and returns it for the
/// file's next mount.
fn commit_view(
    ws: &mut WindowState,
    view: &mut LiveView,
    last_good: &CandidatePlan,
    candidate: CandidatePlan,
    scratch: &mut Vec<NodeId>,
    notices: &mut Vec<Diagnostic>,
) -> CandidatePlan {
    // The store maps a durable key to one cell; a file mounted twice points the
    // keys at the mount being committed.
    for &(key, id) in &view.cells {
        ws.states.bind_key(id, key);
    }
    if view.nodes.is_empty() {
        view.nodes = static_nodes(&ws.store, view.root, &last_good.tree);
    }
    let focused = ws.store.focused().and_then(|focused| {
        view.nodes
            .iter()
            .find(|&&(_, node)| node == focused)
            .map(|&(key, _)| key)
    });
    let anchors = LiveAnchors {
        focused,
        scrolled: Vec::new(),
    };
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
    let mut reload = transact(&mut live, last_good, candidate, &anchors);
    for notice in reload.report.notices.drain(..) {
        if !notices.contains(&notice) {
            notices.push(notice);
        }
    }
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
    reload.candidate
}

/// Reports the diagnostics of an edit: the errors of a candidate that does not
/// compile, whose mounts keep their last-good candidate, or the notices of one
/// committed.
fn report(path: &str, source: &str, diagnostics: &[Diagnostic]) {
    let lines = LineIndex::new(source);
    for diagnostic in diagnostics {
        let at = lines.line_col_utf8(diagnostic.primary.start());
        eprintln!(
            "[viso] hot reload: {path}:{}:{}: {} {}",
            at.line + 1,
            at.column + 1,
            diagnostic.code,
            diagnostic.message
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use viso_ui::{
        BuildCx, NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Rect,
        StateValue,
    };

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

    /// A window that mounts the counter view, recorded as if built from a
    /// temporary copy of its file, and that file.
    fn mounted(name: &str) -> (WindowState, PathBuf) {
        mounted_with(name, COUNTER, |cx| {
            viso_ui_macros::view!("../../tests/fixtures/counter.vs")(cx).id()
        })
    }

    /// A window whose build `mount`s one view of `source`, recorded as if
    /// built from a temporary copy of its file, and that file.
    fn mounted_with(
        name: &str,
        source: &str,
        mount: impl FnOnce(&mut BuildCx<'_>) -> NodeId,
    ) -> (WindowState, PathBuf) {
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
        let dir = std::env::temp_dir().join(format!("viso-session-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("view.vs");
        std::fs::write(&path, source).unwrap();
        let mut records = Vec::new();
        take_mounts(&mut records);
        let [mut record] = <[MountRecord; 1]>::try_from(records)
            .ok()
            .expect("one mount");
        record.file = Box::leak(path.to_str().unwrap().to_owned().into_boxed_str());
        viso_view::__mounted_view(record);
        (ws, path)
    }

    /// Stages the watcher's next delivery, waiting up to two seconds.
    fn staged(session: &mut HotReloadSession) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if session.stage() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
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
    fn a_saved_edit_reloads_the_mount_and_keeps_its_state() {
        let (mut ws, path) = mounted("edit");
        let mut session = HotReloadSession::default();
        session.adopt(|| LoopWaker::new(|| {}), &mut ws);
        assert_eq!(session.views.len(), 1);
        let (_, cell) = session.views[0].cells[0];
        ws.states.set(cell, StateValue::Int(5));
        let column = ws.root.unwrap();
        assert_eq!(children(&ws.store, column).len(), 2);

        let edited = COUNTER.replace(
            "Text { visible: enabled; }",
            "Text { visible: enabled; }\n            Text { }",
        );
        std::fs::write(&path, &edited).unwrap();
        assert!(staged(&mut session), "the watcher delivers the edit");
        session.reload(std::slice::from_mut(&mut ws));

        assert_eq!(session.files[0].revision, 1);
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
        let (mut ws, path) = mounted_with("handler", LOGGER, |cx| {
            viso_ui_macros::view!("../../tests/fixtures/logger.vs")(cx).id()
        });
        let mut session = HotReloadSession::default();
        session.adopt(|| LoopWaker::new(|| {}), &mut ws);
        click(&mut ws);
        assert_eq!(count(&ws, &session), Some(StateValue::Int(1)));
        assert_eq!(log(&session).as_deref(), Some("one"));

        let edited = LOGGER.replace(
            "on click { count += 1; log = \"one\"; }",
            "on click { count += 10; }",
        );
        assert_ne!(edited, LOGGER);
        std::fs::write(&path, &edited).unwrap();
        assert!(staged(&mut session));
        session.reload(std::slice::from_mut(&mut ws));
        assert_eq!(session.files[0].revision, 1);
        assert_eq!(count(&ws, &session), Some(StateValue::Int(1)));
        assert_eq!(log(&session).as_deref(), Some("one"));

        click(&mut ws);
        assert_eq!(count(&ws, &session), Some(StateValue::Int(11)));
        assert_eq!(log(&session).as_deref(), Some("one"));
    }

    #[test]
    fn a_broken_edit_changes_nothing() {
        let (mut ws, path) = mounted("broken");
        let mut session = HotReloadSession::default();
        session.adopt(|| LoopWaker::new(|| {}), &mut ws);
        let root = ws.root.unwrap();
        std::fs::write(
            &path,
            COUNTER.replace("state count = 0;", "state count = ;"),
        )
        .unwrap();
        assert!(staged(&mut session));
        session.reload(std::slice::from_mut(&mut ws));

        assert_eq!(session.files[0].revision, 0);
        assert_eq!(ws.root, Some(root));
        assert_eq!(children(&ws.store, root).len(), 2);
        assert!(
            session.files[0].last_good.is_some(),
            "the last-good is kept"
        );
    }

    #[test]
    fn a_mount_whose_window_closed_is_dropped() {
        let (mut ws, _) = mounted("closed");
        let mut session = HotReloadSession::default();
        session.adopt(|| LoopWaker::new(|| {}), &mut ws);
        session.close(ws.window);
        assert!(session.views.is_empty());
    }
}
