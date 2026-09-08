//! The file tree's imperative command surface: [`FileTreeHandle`], a cheap-to-clone
//! handle an application captures to drive a built [`FileTree`](super::FileTree) —
//! expand, collapse, or toggle a folder — plus the reconcile intent-drain that turns
//! committed expand/collapse intents into a reflattened, re-driven virtual list.
//!
//! The tree's arrangement is a *forest* of owned [`TreeNode`]s plus a
//! [`NodeKey`]-keyed open set, which a reactive scalar cell cannot hold, so it is the
//! control's owned **warm** state: built once, read by the build walk and the
//! reconcile step, edited only by discrete expand/collapse actions (module docs,
//! [ADR 0024](../../../../docs/adr/0024-file-tree-flattened-visible-model.md)). The
//! handle owns that state behind an `Rc<RefCell<..>>` (the same ownership shape the
//! dock's `DockHandle` uses for its tree) alongside the shared warm cells the build
//! walk produced: the flattened visible rows and the keyed row-node map.
//!
//! One kind of command, one effect: an expand/collapse edits the open set. Like the
//! dock's structural commands, a command holds no node store (an
//! [`EventCx`](viso_ui::EventCx) cannot mutate the arena), so it only pushes an
//! [`Intent`] onto a shared queue; the reconcile step ([`reconcile`](super::reconcile))
//! reads the edited open set, reflattens the visible rows into the shared cell, and
//! drives the keyed virtual list's item count — the handler-writes-intent /
//! reconcile-mutates-store split (ADR 0023). The keyed reconcile then diffs by
//! [`NodeKey`], mounting rows that entered the window, recycling those that left, and
//! reusing every survivor's host (architecture section 12.4).

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{EventCx, NodeId, NodeStore, VirtualLists};

use super::model::{NodeKey, TreeNode};
use super::{RowNodes, SelectMode, VisibleRows};

/// One committed expand/collapse edit, pushed by a command and drained by the
/// reconcile step. `Toggle` flips a folder's open state, `Expand` opens it, and
/// `Collapse` closes it — an intent naming a file or an absent key is a harmless
/// no-op once the reconcile reflattens (a file never discloses, so opening it adds no
/// rows). Kept a small `Copy` value: an intent is a discrete action, never a
/// per-frame allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Intent {
    /// Flip the folder's open state: open it if closed, close it if open.
    Toggle(NodeKey),
    /// Open the folder (a no-op if already open).
    Expand(NodeKey),
    /// Close the folder (a no-op if already closed).
    Collapse(NodeKey),
}

/// The file tree's warm, owned state, shared between the build walk (which fills the
/// `visible`/`row_nodes` cells), the [`FileTreeHandle`] (which reads and edits it on a
/// command), and the reconcile step (which reflattens against it). Not a bag of
/// reactive scalar cells — a scalar cell cannot hold a tree (module docs, ADR 0024).
/// Written once at build, then read and edited only on discrete expand/collapse
/// actions, never a per-frame path, so the `RefCell` borrow is uncontended.
pub(super) struct FileTreeState {
    /// The tree forest — the source of truth the reconcile step reflattens against.
    pub(super) roots: Vec<TreeNode>,
    /// The open folders, by key: which directories the flatten descends into. Edited
    /// by the reconcile step from drained intents.
    pub(super) open: std::collections::HashSet<NodeKey>,
    /// Whether the tree allows single or multiple selection. Carried on the warm
    /// state so the keyboard/selection section can read it; unused by this version's
    /// expand/collapse-only reconcile.
    #[allow(dead_code)]
    pub(super) select_mode: SelectMode,
    /// The keyed virtual-list viewport node the build walk produced, so the reconcile
    /// step can drive this list's item count.
    pub(super) viewport: NodeId,
    /// The shared flattened visible-row cell the build walk's keyed closures read
    /// live; the reconcile step reflattens into it on an expand/collapse.
    pub(super) visible: VisibleRows,
    /// The keyed row-node map the build walk fills, so a later section can address a
    /// row by key without searching the arena (a discrete-action lookup, section 45).
    #[allow(dead_code)]
    pub(super) row_nodes: RowNodes,
}

/// A shared handle to the file tree's warm state.
pub(super) type FileTreeStateCell = Rc<RefCell<FileTreeState>>;

/// A shared slot an application creates, passes to
/// [`FileTree::handle`](super::FileTree::handle), and reads after `build` to obtain
/// the control's [`FileTreeHandle`]. The warm state, the visible cell, and the keyed
/// row-node map are minted inside `build`, so the handle cannot be returned by the
/// builder chain; the app supplies this slot up front and `build` fills it once they
/// exist — the same deferred-fill idiom as the dock's `DockHandleSlot`. Written once
/// during build, read once after.
pub type FileTreeHandleSlot = Rc<RefCell<Option<FileTreeHandle>>>;

/// A handle an application captures to drive a built [`FileTree`](super::FileTree)
/// programmatically. Cheap to clone (it holds only shared cells). Call
/// [`toggle`](FileTreeHandle::toggle)/[`expand`](FileTreeHandle::expand)/[`collapse`](FileTreeHandle::collapse)
/// from within an [`EventCx`](viso_ui::EventCx) (an event handler) to record an
/// expand/collapse, then call [`reconcile`](FileTreeHandle::reconcile) after the input
/// transaction with `&mut NodeStore`/`&mut VirtualLists` to apply it. A command naming
/// an absent or non-directory key is a no-op (a file never discloses).
#[derive(Clone)]
pub struct FileTreeHandle {
    /// The file tree's warm state: the forest, the open set, and the shared cells.
    state: FileTreeStateCell,
    /// The shared expand/collapse intent queue a command pushes onto; the reconcile
    /// step drains it, edits the open set, and re-drives the list.
    intents: Rc<RefCell<Vec<Intent>>>,
}

impl FileTreeHandle {
    /// Flip the open state of the folder `key`: open it if collapsed, collapse it if
    /// open. Pushes a [`Toggle`](Intent::Toggle) intent the
    /// [`reconcile`](FileTreeHandle::reconcile) step applies. A no-op (once reconciled)
    /// if `key` names a file or is absent. Call from within an event handler.
    pub fn toggle(&self, _ev: &mut EventCx<'_>, key: NodeKey) {
        self.intents.borrow_mut().push(Intent::Toggle(key));
    }

    /// Open the folder `key`, revealing its children on the next reconcile. Pushes an
    /// [`Expand`](Intent::Expand) intent; a no-op (once reconciled) if the folder is
    /// already open, names a file, or is absent. Call from within an event handler.
    pub fn expand(&self, _ev: &mut EventCx<'_>, key: NodeKey) {
        self.intents.borrow_mut().push(Intent::Expand(key));
    }

    /// Close the folder `key`, hiding its descendants on the next reconcile. Pushes a
    /// [`Collapse`](Intent::Collapse) intent; a no-op (once reconciled) if the folder
    /// is already closed or absent. The folder keeps its descendants' open state — a
    /// stable [`NodeKey`] set survives the collapse, so reopening restores the prior
    /// expansion (module docs). Call from within an event handler.
    pub fn collapse(&self, _ev: &mut EventCx<'_>, key: NodeKey) {
        self.intents.borrow_mut().push(Intent::Collapse(key));
    }

    /// The reconcile intent-drain: apply every committed expand/collapse intent
    /// against the warm open set, reflatten the visible rows into the shared cell, and
    /// drive the keyed virtual list's item count so the substrate diffs by
    /// [`NodeKey`] on its next pass. A host calls this after an input transaction with
    /// `&mut NodeStore`/`&mut VirtualLists` — the handler-writes-intent /
    /// reconcile-mutates-store split (ADR 0023).
    ///
    /// Delegates to [`reconcile_open`](super::reconcile::reconcile_open): a structural
    /// change (AGENTS section 8.1) that runs before the frame's
    /// [`virtual_list::reconcile`](viso_ui::virtual_list::reconcile) geometry pass, the
    /// same phase relationship the dock's reconcile uses.
    pub fn reconcile(&self, store: &mut NodeStore, lists: &mut VirtualLists) {
        let mut state = self.state.borrow_mut();
        super::reconcile::reconcile_open(&mut state, &self.intents, store, lists);
    }
}

/// Build the handle the [`FileTree`](super::FileTree) fills its slot with, taking
/// ownership of the warm forest and open set and sharing the visible-row cell and the
/// keyed row-node map the build walk produced. Called once at the end of `build`, when
/// the viewport id exists. The argument order mirrors the `build` call site: the warm
/// data first (roots, open, select mode), then the built substrate (viewport, visible,
/// row nodes).
pub(super) fn make_handle(
    roots: Vec<TreeNode>,
    open: std::collections::HashSet<NodeKey>,
    select_mode: SelectMode,
    viewport: NodeId,
    visible: VisibleRows,
    row_nodes: RowNodes,
) -> FileTreeHandle {
    FileTreeHandle {
        state: Rc::new(RefCell::new(FileTreeState {
            roots,
            open,
            select_mode,
            viewport,
            visible,
            row_nodes,
        })),
        intents: Rc::new(RefCell::new(Vec::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::file_tree::build::build_tree;
    use crate::controls::file_tree::model::{TreeNode, VisibleRow, flatten};
    use crate::controls::file_tree::{FileTreeStyle, Labels};
    use std::collections::{HashMap, HashSet};
    use viso_ui::virtual_list;
    use viso_ui::{
        Axis, BindingTable, BuildCx, EffectStore, NodeStore, Rect, SemanticProjector, StateStore,
        TextEdits, VirtualLists,
    };

    const ROOT: NodeKey = NodeKey(0);
    const A: NodeKey = NodeKey(1);
    const B: NodeKey = NodeKey(2);
    const A1: NodeKey = NodeKey(11);
    const A2: NodeKey = NodeKey(12);

    /// A two-level fixture: a `root` dir over an `a` dir (children `a1`, `a2`) and a
    /// `b` file.
    fn fixture() -> Vec<TreeNode> {
        vec![TreeNode::dir(
            ROOT,
            "root",
            vec![
                TreeNode::dir(
                    A,
                    "a",
                    vec![TreeNode::file(A1, "a1"), TreeNode::file(A2, "a2")],
                ),
                TreeNode::file(B, "b"),
            ],
        )]
    }

    /// The reactive stores a keyed virtual-list build writes into, plus the built
    /// handle — enough to drive a command, run the reconcile, and then run a real
    /// `virtual_list::reconcile` + layout to observe surviving-row reuse.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        text_edits: TextEdits,
        #[allow(dead_code)]
        projectors: SemanticProjector,
        effects: EffectStore,
        viewport: NodeId,
        row_nodes: RowNodes,
        visible: VisibleRows,
        handle: FileTreeHandle,
        scratch: Vec<u32>,
        redo: Vec<NodeId>,
    }

    impl Reactive {
        /// Build the tree directly (capturing the viewport, the shared visible cell,
        /// and the keyed row map) and wrap it in a handle, so a test drives commands
        /// against the same warm state the build authored.
        fn build(roots: &[TreeNode], open: &HashSet<NodeKey>) -> Self {
            let mut store = NodeStore::new();
            let mut states = StateStore::new();
            let mut bindings = BindingTable::new();
            let mut lists = VirtualLists::new();
            let mut text_edits = TextEdits::new();
            let mut projectors = SemanticProjector::new();
            let visible: VisibleRows = Rc::new(RefCell::new(flatten(roots, open)));
            let labels: Labels = Rc::new(crate::controls::file_tree::build::label_index(roots));
            let row_nodes: RowNodes = Rc::new(RefCell::new(HashMap::new()));
            let style = FileTreeStyle::default();
            let viewport;
            {
                let mut cx = BuildCx::with_reactive(
                    &mut store,
                    &mut states,
                    &mut bindings,
                    &mut lists,
                    &mut text_edits,
                    &mut projectors,
                );
                viewport = build_tree(&mut cx, &visible, &labels, &style, &row_nodes);
                let _ = cx.root();
            }
            let handle = make_handle(
                roots.to_vec(),
                open.clone(),
                SelectMode::default(),
                viewport,
                Rc::clone(&visible),
                Rc::clone(&row_nodes),
            );
            Reactive {
                store,
                states,
                bindings,
                lists,
                text_edits: TextEdits::new(),
                projectors,
                effects: EffectStore::new(),
                viewport,
                row_nodes,
                visible,
                handle,
                scratch: Vec::new(),
                redo: Vec::new(),
            }
            .also_keep(text_edits)
        }

        /// Fold the leftover `text_edits` store into the struct without a warning —
        /// `BuildCx::with_reactive` borrows it during build; we keep it alive as a
        /// field so it outlives any list state that referenced it.
        fn also_keep(mut self, text_edits: TextEdits) -> Self {
            self.text_edits = text_edits;
            self
        }

        /// The surface the viewport lays out in — a tall-enough column that the whole
        /// visible window mounts.
        fn surface(&self) -> Rect {
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 400.0,
            }
        }

        /// Run one command against a throwaway `EventCx`, then drain the intents into
        /// the open set and re-drive the list — the widget's own reconcile step.
        fn drive(&mut self, act: impl FnOnce(&FileTreeHandle, &mut EventCx<'_>)) {
            let ev = still_pointer();
            {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                act(&self.handle, &mut cx);
            }
            self.handle.reconcile(&mut self.store, &mut self.lists);
        }

        /// Run the frame's virtual-list reconcile + incremental layout the way the
        /// facade's Layout phase does, so mounts/recycles the widget's reconcile
        /// requested actually happen. Seeds a full layout on the first pass so the
        /// viewport has a box before reconcile reads its size.
        fn frame(&mut self) {
            let surface = self.surface();
            if self.store.bounds_main(self.viewport, Axis::Column) <= 0.0 {
                self.store.layout(self.viewport, surface, &mut self.scratch);
            }
            virtual_list::reconcile(
                &mut self.store,
                &mut self.lists,
                &mut self.states,
                &mut self.bindings,
                &mut self.effects,
            );
            self.store
                .relayout_dirty(self.viewport, surface, &mut self.scratch, &mut self.redo);
        }

        /// The host node the walk mounted for the row keyed `key`, if a row is
        /// currently mounted for it.
        fn host_of(&self, key: NodeKey) -> Option<NodeId> {
            self.row_nodes.borrow().get(&key).copied()
        }

        /// The current flattened visible-row list.
        fn rows(&self) -> Vec<VisibleRow> {
            self.visible.borrow().clone()
        }

        /// The list's mounted-row count this frame.
        fn mounted(&self) -> usize {
            self.lists
                .get(self.viewport)
                .expect("registered list state")
                .mounted_count()
        }

        /// The list's total canvas extent (rows * row height).
        fn extent(&self) -> f32 {
            self.lists
                .get(self.viewport)
                .expect("registered list state")
                .total_extent()
        }
    }

    /// A still (no-button) pointer sample for driving a command that reads no pointer.
    fn still_pointer() -> viso_ui::PointerEvent {
        viso_ui::PointerEvent {
            x: 0.0,
            y: 0.0,
            phase: viso_ui::PointerPhase::Move,
            buttons: viso_ui::PointerButtons::NONE,
            modifiers: viso_ui::Modifiers::default(),
        }
    }

    /// Test 3 (input tape): expanding a folder grows the visible rows and mounts the
    /// entrants, while a folder that stays open keeps its host node — the keyed reuse
    /// contract. Collapsing then removes the children again.
    #[test]
    fn expand_grows_rows_and_reuses_survivor_hosts() {
        let roots = fixture();
        let open: HashSet<NodeKey> = [ROOT].into_iter().collect();
        let mut rx = Reactive::build(&roots, &open);
        // First frame mounts the initial window: ROOT, A (collapsed), B => 3 rows.
        rx.frame();
        assert_eq!(rx.rows().len(), 3, "root open, A collapsed => 3 rows");
        assert_eq!(rx.mounted(), 3, "the whole small window mounts");
        let root_host = rx.host_of(ROOT).expect("root row mounted");
        let a_host = rx.host_of(A).expect("A row mounted");

        // Expand A: its two children appear, growing the list to 5 rows.
        rx.drive(|h, ev| h.toggle(ev, A));
        assert_eq!(rx.rows().len(), 5, "expanding A adds a1, a2");
        rx.frame();
        assert_eq!(rx.mounted(), 5, "the two entrants mount");
        // ROOT and A survived the reflatten: their hosts are unchanged (keyed reuse).
        assert_eq!(
            rx.host_of(ROOT),
            Some(root_host),
            "ROOT survives the expand with the same host"
        );
        assert_eq!(
            rx.host_of(A),
            Some(a_host),
            "A survives the expand with the same host — no rebuild"
        );
        assert!(rx.host_of(A1).is_some(), "a1 mounted as an entrant");

        // Collapse A: its children leave; ROOT and A stay with their hosts.
        rx.drive(|h, ev| h.collapse(ev, A));
        assert_eq!(rx.rows().len(), 3, "collapsing A removes a1, a2");
        rx.frame();
        assert_eq!(rx.mounted(), 3, "the children recycle out");
        assert_eq!(rx.host_of(ROOT), Some(root_host), "ROOT still reused");
        assert_eq!(rx.host_of(A), Some(a_host), "A still reused");
    }

    /// Expand/collapse addressed by key survives an unrelated collapse: opening A
    /// while ROOT is open, collapsing ROOT (hiding everything), and reopening ROOT
    /// brings A back expanded — the stable-key contract at the command layer.
    #[test]
    fn open_state_survives_a_parent_collapse() {
        let roots = fixture();
        let open: HashSet<NodeKey> = [ROOT, A].into_iter().collect();
        let mut rx = Reactive::build(&roots, &open);
        rx.frame();
        assert_eq!(rx.rows().len(), 5, "ROOT and A open => 5 rows");

        rx.drive(|h, ev| h.collapse(ev, ROOT));
        assert_eq!(rx.rows().len(), 1, "closing ROOT hides everything below");
        rx.frame();

        rx.drive(|h, ev| h.expand(ev, ROOT));
        assert_eq!(
            rx.rows().len(),
            5,
            "A was never removed from the open set, so it returns expanded"
        );
    }

    /// Test 5 (golden/bounds): after an expand, the list's canvas extent is the new
    /// row count times the row height, and each mounted row is indented by its depth —
    /// the virtualization + structural-reconcile geometry.
    #[test]
    fn extent_and_indent_track_the_flattened_rows() {
        let roots = fixture();
        let open: HashSet<NodeKey> = [ROOT].into_iter().collect();
        let mut rx = Reactive::build(&roots, &open);
        rx.frame();
        let row_h = FileTreeStyle::default().row_height;
        assert_eq!(rx.extent(), row_h * 3.0, "3 rows before expand");

        rx.drive(|h, ev| h.toggle(ev, A));
        rx.frame();
        assert_eq!(rx.extent(), row_h * 5.0, "5 rows after expand");

        // The flattened rows carry the depth the build walk indents by: ROOT at 0, A
        // at 1, a1/a2 at 2, B at 1.
        let rows = rx.rows();
        let depths: Vec<(NodeKey, u16)> = rows.iter().map(|r| (r.key, r.depth)).collect();
        assert_eq!(
            depths,
            vec![(ROOT, 0), (A, 1), (A1, 2), (A2, 2), (B, 1)],
            "pre-order depths drive the per-row indent"
        );
    }

    /// A toggle of a file key is a no-op: a file never discloses, so the flatten is
    /// unchanged and no row enters or leaves.
    #[test]
    fn toggling_a_file_changes_nothing() {
        let roots = fixture();
        let open: HashSet<NodeKey> = [ROOT].into_iter().collect();
        let mut rx = Reactive::build(&roots, &open);
        rx.frame();
        let before = rx.rows().len();
        rx.drive(|h, ev| h.toggle(ev, B));
        assert_eq!(rx.rows().len(), before, "toggling a file adds no rows");
    }
}
