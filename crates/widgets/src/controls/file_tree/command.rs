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

use viso_ui::{EventCx, Key, NodeId, NodeStore, VirtualLists};

use super::model::{self, Nav, NodeKey, SelectOp, TreeNode};
use super::{RowNodes, SelectMode, VisibleRows};

/// One committed edit, pushed by a command and drained by the reconcile step in
/// order. The first three are the expand/collapse structural edits; the last two are
/// the focus/selection edits the keyboard and mouse produce. An intent naming a file
/// or an absent key is a harmless no-op once the reconcile applies it (a file never
/// discloses, so opening it adds no rows; focusing/selecting an off-list key is
/// dropped when the reconcile can't place it). Kept a small `Copy` value: an intent
/// is a discrete action, never a per-frame allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Intent {
    /// Flip the folder's open state: open it if closed, close it if open.
    Toggle(NodeKey),
    /// Open the folder (a no-op if already open).
    Expand(NodeKey),
    /// Close the folder (a no-op if already closed).
    Collapse(NodeKey),
    /// Move the focus cursor to this row.
    SetFocus(NodeKey),
    /// Edit the selection for this row with the given gesture (replace / toggle /
    /// range). Also moves the focus cursor to the row.
    Select(NodeKey, SelectOp),
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
    /// The selected rows, by key: the set the reconcile step edits from `Select`
    /// intents and (a later section) writes to each row's `aria-selected`. A stable
    /// key, so a row keeps its selected state across a collapse/reopen or a reorder.
    pub(super) selection: std::collections::HashSet<NodeKey>,
    /// The focus cursor — the row the keyboard steps from and a `Select` last touched.
    /// `None` before the first navigation. Warm, edited only on a discrete action.
    pub(super) focus: Option<NodeKey>,
    /// The range anchor — the fixed end a `Shift` range grows from, so a run of
    /// `Shift`+arrow extends one range from where it began. Set by every non-range
    /// selection, left in place by a range.
    pub(super) anchor: Option<NodeKey>,
    /// Whether the tree allows single or multiple selection. Read when a keyboard or
    /// mouse gesture chooses which [`SelectOp`] to apply.
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

    /// Interpret a keyboard event against the tree, pushing the resulting intents —
    /// the keyboard half of navigation and selection. Call it with the tree's
    /// [`KeyEvent`](viso_ui::KeyEvent) when the tree holds focus; it reads the current
    /// focus cursor and visible rows from the warm state (a borrow, no store) to
    /// decide the step, then records intents the [`reconcile`](FileTreeHandle::reconcile)
    /// step applies. Does nothing for a key release or a key the tree doesn't bind.
    ///
    /// The bindings (every one with a mouse equivalent, AGENTS section 15):
    /// - `Up`/`Down` move the focus cursor one visible row (skipping collapsed
    ///   subtrees, since a hidden row isn't visible);
    /// - `Right` discloses a collapsed folder, or steps into an open folder's first
    ///   child; `Left` collapses an open folder, or steps out to the parent;
    /// - `Home`/`End` focus the first/last visible row;
    /// - `Space` toggles the focused row's selection (in `Single` mode, replaces it);
    /// - holding `Shift` with an arrow/Home/End moves focus **and** range-selects from
    ///   the anchor to the new focus (only in `Multi` mode; a plain move otherwise).
    pub fn on_key(&self, _ev: &mut EventCx<'_>, key_event: &viso_ui::KeyEvent) {
        if !key_event.pressed {
            return;
        }
        let shift = key_event.modifiers.shift;
        // Read the warm state to resolve the step; borrow only, never mutate here.
        let state = self.state.borrow();
        let rows = state.visible.borrow();
        let focus = state.focus;
        let multi = matches!(state.select_mode, SelectMode::Multi);

        // Space selects/toggles the focused row in place; it never navigates.
        if key_event.key == Key::Space {
            if let Some(f) = focus {
                let op = if multi {
                    SelectOp::Toggle
                } else {
                    SelectOp::Replace
                };
                drop(rows);
                drop(state);
                self.intents.borrow_mut().push(Intent::Select(f, op));
            }
            return;
        }

        let nav = match key_event.key {
            Key::Up => model::focus_up(&rows, focus),
            Key::Down => model::focus_down(&rows, focus),
            Key::Right => model::focus_right(&rows, focus),
            Key::Left => model::focus_left(&rows, &state.roots, focus),
            Key::Home => model::focus_home(&rows),
            Key::End => model::focus_end(&rows),
            _ => return,
        };
        drop(rows);
        drop(state);
        let Some(nav) = nav else { return };
        let mut queue = self.intents.borrow_mut();
        match nav {
            // A move: focus the row, and if Shift is held in Multi mode, range-select
            // from the anchor to it in the same step.
            Nav::Focus(key) => {
                if shift && multi {
                    queue.push(Intent::Select(key, SelectOp::Range));
                } else {
                    queue.push(Intent::SetFocus(key));
                }
            }
            Nav::Expand(key) => queue.push(Intent::Expand(key)),
            Nav::Collapse(key) => queue.push(Intent::Collapse(key)),
        }
    }

    /// Move the focus cursor to `key` without changing the selection. The programmatic
    /// / mouse-hover equivalent of an arrow key that only moves focus. Pushes a
    /// [`SetFocus`](Intent::SetFocus) intent; a no-op (once reconciled) if `key` is not
    /// currently visible.
    pub fn focus(&self, _ev: &mut EventCx<'_>, key: NodeKey) {
        self.intents.borrow_mut().push(Intent::SetFocus(key));
    }

    /// Edit the selection for `key` with an explicit [`SelectOp`] — the general
    /// selection command every mouse/keyboard selection gesture funnels through.
    /// Pushes a [`Select`](Intent::Select) intent, which also moves the focus cursor to
    /// the row. Call from within an event handler.
    pub fn select(&self, _ev: &mut EventCx<'_>, key: NodeKey, op: SelectOp) {
        self.intents.borrow_mut().push(Intent::Select(key, op));
    }

    /// Handle a mouse click on the row `key` with the event's modifiers — the mouse
    /// equivalent of the keyboard selection gestures (AGENTS section 15). Chooses the
    /// [`SelectOp`] the way a file browser does: `Shift` extends a range; a platform
    /// multi-select modifier (`Ctrl`/`Cmd`) toggles the one row; a plain click replaces
    /// the selection. In `Single` mode every click replaces (only `Replace` is
    /// allowed). Pushes the corresponding [`Select`](Intent::Select) intent.
    pub fn click(&self, ev: &mut EventCx<'_>, key: NodeKey) {
        let op = if matches!(self.state.borrow().select_mode, SelectMode::Single) {
            SelectOp::Replace
        } else {
            let m = ev.pointer().map(|p| p.modifiers).unwrap_or_default();
            if m.shift {
                SelectOp::Range
            } else if m.control || m.logo {
                SelectOp::Toggle
            } else {
                SelectOp::Replace
            }
        };
        self.intents.borrow_mut().push(Intent::Select(key, op));
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
            selection: std::collections::HashSet::new(),
            focus: None,
            anchor: None,
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
            Self::build_mode(roots, open, SelectMode::default())
        }

        /// Same as [`build`](Self::build) but with an explicit [`SelectMode`], so a test
        /// can exercise `Multi` selection (Space toggles, Shift ranges).
        fn build_mode(roots: &[TreeNode], open: &HashSet<NodeKey>, mode: SelectMode) -> Self {
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
                mode,
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

        /// Press one key (with the given modifiers) through a throwaway key `EventCx`,
        /// let the handle interpret it into intents, then reconcile — the keyboard
        /// counterpart of [`drive`](Self::drive), so a test can play an input tape of
        /// key presses and read the focus/selection the reconcile applied.
        fn press(&mut self, key: viso_ui::Key, modifiers: viso_ui::Modifiers) {
            let ev = viso_ui::KeyEvent {
                key,
                pressed: true,
                repeat: false,
                modifiers,
            };
            {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                self.handle.on_key(&mut cx, &ev);
            }
            self.handle.reconcile(&mut self.store, &mut self.lists);
        }

        /// The focus cursor's current key, as the reconcile last set it.
        fn focus(&self) -> Option<NodeKey> {
            self.handle.state.borrow().focus
        }

        /// The current selection set, sorted by key for a stable assertion.
        fn selection(&self) -> Vec<NodeKey> {
            let mut keys: Vec<NodeKey> = self
                .handle
                .state
                .borrow()
                .selection
                .iter()
                .copied()
                .collect();
            keys.sort_by_key(|k| k.0);
            keys
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

    /// Test 4 (input tape): the keyboard half. Direction keys move the focus cursor over
    /// the visible rows, `Right`/`Left` disclose or collapse, `Home`/`End` jump to the
    /// extremes, and `Space`/`Shift` drive selection — every gesture reconciled into the
    /// warm focus/selection sets a semantics pass later reads.
    #[test]
    fn keyboard_navigates_focus_and_selects() {
        use viso_ui::{Key, Modifiers};
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };

        let roots = fixture();
        // ROOT and A both open: visible rows are ROOT, A, A1, A2, B.
        let open: HashSet<NodeKey> = [ROOT, A].into_iter().collect();
        let mut rx = Reactive::build_mode(&roots, &open, SelectMode::Multi);
        rx.frame();
        assert_eq!(rx.rows().len(), 5, "ROOT and A open => 5 visible rows");

        // Home focuses the first visible row; Down/End walk the flattened order.
        rx.press(Key::Home, none);
        assert_eq!(rx.focus(), Some(ROOT), "Home focuses the first row");
        rx.press(Key::Down, none);
        assert_eq!(rx.focus(), Some(A), "Down steps to the next visible row");
        rx.press(Key::End, none);
        assert_eq!(rx.focus(), Some(B), "End focuses the last visible row");
        rx.press(Key::Up, none);
        assert_eq!(rx.focus(), Some(A2), "Up steps back one visible row");

        // Left on an open dir collapses it; the rows shrink and focus stays put.
        rx.press(Key::Home, none);
        rx.press(Key::Down, none);
        assert_eq!(rx.focus(), Some(A), "focus on the open dir A");
        rx.press(Key::Left, none);
        assert_eq!(
            rx.rows().len(),
            3,
            "Left collapses the open dir A, hiding a1/a2"
        );
        // Right re-expands it; Right again descends into the first child.
        rx.press(Key::Right, none);
        assert_eq!(rx.rows().len(), 5, "Right re-expands A");
        rx.press(Key::Right, none);
        assert_eq!(
            rx.focus(),
            Some(A1),
            "Right on an open dir descends to first child"
        );
        // Left on a leaf steps out to the parent.
        rx.press(Key::Left, none);
        assert_eq!(
            rx.focus(),
            Some(A),
            "Left on a leaf steps out to the parent"
        );

        // Space toggles the focused row into the selection (Multi mode).
        rx.press(Key::Space, none);
        assert_eq!(rx.selection(), vec![A], "Space selects the focused row");
        // Move focus and Shift+Down range-selects from the anchor (A) to the new focus.
        rx.press(Key::Down, shift);
        assert_eq!(rx.focus(), Some(A1), "Shift+Down moves focus");
        assert_eq!(
            rx.selection(),
            vec![A, A1],
            "Shift+Down range-selects from the anchor to the new focus"
        );
        rx.press(Key::Down, shift);
        assert_eq!(
            rx.selection(),
            vec![A, A1, A2],
            "a second Shift+Down grows the same range from the fixed anchor"
        );
        // Space again toggles the focused row back out of the selection.
        rx.press(Key::Space, none);
        assert_eq!(
            rx.selection(),
            vec![A, A1],
            "Space toggles the focused row (A2) back out"
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
