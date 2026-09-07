//! The dock's imperative command surface: [`DockHandle`], a cheap-to-clone handle
//! an application captures to drive a built [`Dock`](super::Dock) — select a tab,
//! redock, undock, float, or close a panel — plus the reconcile intent-drain that
//! turns committed tree edits into live geometry.
//!
//! The dock's arrangement is a *tree* ([`DockTree`]), which a reactive scalar cell
//! cannot hold, so the tree is the dock's owned **warm** state: built once, read by
//! the build walk and the reconcile step, edited only by discrete command/drag
//! actions (module docs, [ADR 0023](../../../../docs/adr/0023-dock-live-relayout-and-tree-state.md)).
//! The handle owns that tree behind an `Rc<RefCell<..>>` (the same ownership shape
//! the navigation stack's `NavHandle` uses for its page list) alongside the keyed
//! panel-node map and the seam records the build walk produced.
//!
//! Two kinds of command, two effects:
//!
//! - **[`select_tab`](DockHandle::select_tab)** shows one tab and hides its
//!   siblings. Like the navigation stack's push/pop, it edits the tree's `selected`
//!   and defers a `set_hidden` flip per sibling panel node through the
//!   [`EventCx`](viso_ui::EventCx) — a handler holds no node store, so the router
//!   applies the flips after the handler returns. No structural change: a hidden
//!   panel folds out of layout and paint through the retained `hidden` flag
//!   (AGENTS section 8.4 / 11).
//! - **[`dock`](DockHandle::dock)/[`undock`](DockHandle::undock)/[`float`](DockHandle::float)/[`close`](DockHandle::close)**
//!   edit the tree's *structure* (via the pure transforms in [`tree`](super::tree)).
//!   The reconcile step ([`reconcile`](super::reconcile)) reads the edited tree and
//!   the seam fractions and rewrites live geometry; the node remounting a redocked
//!   panel's built-once subtree is the drag-to-redock section's job. A closed or
//!   undocked-into-nothing panel's node is hidden so it leaves layout immediately.
//!
//! The reconcile intent-drain [`reconcile`](DockHandle::reconcile) is what a host
//! calls after an input transaction with `&mut NodeStore`: it rewrites every seam's
//! pane weights from its committed fraction (the seam-drag section's live-relayout
//! path) — the caller `reconcile_seams` needs so it is no longer dead code.

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{EventCx, NodeStore, StateStore};

use super::build::{PanelNodes, SeamRec};
use super::tree::{DockTree, DropPart, PanelKey};

/// The dock's warm, owned state, shared between the build walk (which fills it),
/// the [`DockHandle`] (which edits it on a command), and the reconcile step (which
/// reads it). Not a bag of reactive scalar cells — a scalar cell cannot hold a tree
/// (module docs, ADR 0023). Written once at build, then read and edited only on
/// discrete command/drag actions, never a per-frame path, so the `RefCell` borrow
/// is uncontended.
pub(super) struct DockState {
    /// The layout tree — the source of truth the build walk reads and commands edit.
    pub(super) tree: DockTree,
    /// The seam records a live drag drives; the reconcile step rewrites each seam's
    /// pane weights from its fraction cell.
    pub(super) seams: Vec<SeamRec>,
}

/// A shared handle to the dock's warm state.
pub(super) type DockStateCell = Rc<RefCell<DockState>>;

/// A shared slot an application creates, passes to [`Dock::handle`](super::Dock::handle),
/// and reads after `build` to obtain the control's [`DockHandle`]. The tree, seams,
/// and panel-node map are minted inside `build`, so the handle cannot be returned by
/// the builder chain; the app supplies this slot up front and `build` fills it once
/// they exist — the same deferred-fill idiom as the navigation stack's handle slot.
/// Written once during build, read once after.
pub type DockHandleSlot = Rc<RefCell<Option<DockHandle>>>;

/// A handle an application captures to drive a built [`Dock`](super::Dock)
/// programmatically. Cheap to clone (it holds only shared cells). Call
/// [`select_tab`](DockHandle::select_tab) to show a tab, and
/// [`dock`](DockHandle::dock)/[`undock`](DockHandle::undock)/[`float`](DockHandle::float)/[`close`](DockHandle::close)
/// to edit the arrangement — all from within an [`EventCx`](viso_ui::EventCx) (an
/// event handler), since a selection defers `hidden` flips and a structural edit is
/// applied by the reconcile step. A command naming an absent panel/target is a
/// no-op.
#[derive(Clone)]
pub struct DockHandle {
    /// The dock's warm state: the layout tree and the seam records.
    state: DockStateCell,
    /// The keyed panel-node map, filled by the build walk. A command resolves a
    /// panel's built-once node here to flip its visibility (a discrete-action cold
    /// lookup, AGENTS section 45).
    panels: PanelNodes,
}

impl DockHandle {
    /// Show the tab `key` in its tab group and hide its siblings. Edits the group's
    /// `selected` and defers a `set_hidden` flip per sibling panel node (hide every
    /// sibling, show `key`) that the router applies after the handler returns. A
    /// no-op if `key` is not in any tab group. Call from within an event handler.
    pub fn select_tab(&self, ev: &mut EventCx<'_>, key: PanelKey) {
        let Some((siblings, _selected)) = self.state.borrow_mut().tree.root.select(key) else {
            return;
        };
        let panels = self.panels.borrow();
        for sib in siblings {
            if let Some(&node) = panels.get(&sib) {
                ev.set_hidden(node, sib != key);
            }
        }
    }

    /// Redock `key` relative to `target` per `part` (an edge splits, a center/tab
    /// joins a tab group). Edits the tree; the reconcile step turns the edit into
    /// live geometry. A no-op if `target` is absent or `key == target`.
    pub fn dock(&self, _ev: &mut EventCx<'_>, key: PanelKey, target: PanelKey, part: DropPart) {
        self.state.borrow_mut().tree.dock(key, target, part);
    }

    /// Detach `key` into a floating panel at `rect`, removing it from its current
    /// home. Edits the tree; the reconcile step remounts the node into the overlay.
    /// A no-op if `key` is absent.
    pub fn float(&self, _ev: &mut EventCx<'_>, key: PanelKey, rect: viso_ui::Rect) {
        self.state.borrow_mut().tree.float(key, rect);
    }

    /// Remove `key` from the docked tree, collapsing an emptied split into its
    /// surviving sibling, and hide its panel node so it leaves layout at once. This
    /// is both "undock" (the panel leaves the arrangement) and "close" — the caller
    /// decides whether to re-dock or drop the panel afterward. A no-op if `key` is
    /// absent. Call from within an event handler.
    pub fn close(&self, ev: &mut EventCx<'_>, key: PanelKey) {
        if !self.state.borrow_mut().tree.remove(key) {
            return;
        }
        if let Some(&node) = self.panels.borrow().get(&key) {
            ev.set_hidden(node, true);
        }
    }

    /// Undock `key`: alias for [`close`](DockHandle::close) — the panel leaves the
    /// docked tree and its node folds out of layout. Distinguished by name so an app
    /// reads "undock this panel" at the call site; a later section may reattach an
    /// undocked panel as a float.
    pub fn undock(&self, ev: &mut EventCx<'_>, key: PanelKey) {
        self.close(ev, key);
    }

    /// The reconcile intent-drain: rewrite every seam's pane weights from its
    /// committed fraction against the store's resolved extents. A host calls this
    /// after an input transaction with `&mut NodeStore` and the state store the drag
    /// handlers wrote into. This is the caller [`reconcile_seams`](super::reconcile::reconcile_seams)
    /// needs — the handler-writes-intent / reconcile-mutates-store split (ADR 0023).
    pub fn reconcile(&self, store: &mut NodeStore, states: &StateStore) {
        super::reconcile::reconcile_seams(store, states, &self.state.borrow().seams);
    }
}

/// Build the handle the [`Dock`](super::Dock) fills its slot with, taking ownership
/// of the warm tree and seams the build walk produced and sharing the panel-node
/// map. Called once at the end of `build`, when the ids exist.
pub(super) fn make_handle(tree: DockTree, seams: Vec<SeamRec>, panels: PanelNodes) -> DockHandle {
    DockHandle {
        state: Rc::new(RefCell::new(DockState { tree, seams })),
        panels,
    }
}

/// A test-only view of a panel node's resolved id in the keyed map — lets a handle
/// test assert which node a command targeted without reaching into the private map.
#[cfg(test)]
impl DockHandle {
    fn node_of(&self, key: PanelKey) -> Option<viso_ui::NodeId> {
        self.panels.borrow().get(&key).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::dock::build::{BuildOut, build_tree};
    use crate::controls::dock::tree::DockNode;
    use crate::controls::dock::{DockStyle, PanelContent};
    use std::collections::HashMap;
    use viso_ui::{
        Axis, BindingTable, BoxStyle, BuildCx, LeafStyle, NodeId, NodeStore, SemanticProjector,
        Size, StateStore, TextEdits, VirtualLists,
    };

    /// A fill-leaf content builder for a registered panel.
    fn fill_panel() -> PanelContent {
        Box::new(|cx: &mut BuildCx<'_>| {
            cx.leaf(LeafStyle {
                size: Size::fill(),
                style: BoxStyle::NONE,
            });
        })
    }

    /// The reactive stores a dock build writes into, held so they outlive the build,
    /// plus the built handle and the node store — enough to drive a command and read
    /// the deferred visibility flips it made.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        #[allow(dead_code)]
        bindings: BindingTable,
        #[allow(dead_code)]
        lists: VirtualLists,
        #[allow(dead_code)]
        text_edits: TextEdits,
        #[allow(dead_code)]
        projectors: SemanticProjector,
        handle: DockHandle,
    }

    impl Reactive {
        /// Build the dock's tree directly (capturing seams and the panel map) and
        /// wrap it in a handle, so a test drives commands against the same warm state
        /// the build authored.
        fn build(tree: DockTree, contents: &HashMap<PanelKey, PanelContent>) -> Self {
            let mut store = NodeStore::new();
            let mut states = StateStore::new();
            let mut bindings = BindingTable::new();
            let mut lists = VirtualLists::new();
            let mut text_edits = TextEdits::new();
            let mut projectors = SemanticProjector::new();
            let panels: PanelNodes = Rc::new(RefCell::new(HashMap::new()));
            let seams;
            {
                let mut out = BuildOut {
                    seams: Vec::new(),
                    zones: Vec::new(),
                };
                let style = DockStyle::default();
                let mut cx = BuildCx::with_reactive(
                    &mut store,
                    &mut states,
                    &mut bindings,
                    &mut lists,
                    &mut text_edits,
                    &mut projectors,
                );
                build_tree(&mut cx, &tree.root, &style, contents, &panels, &mut out);
                seams = out.seams;
            }
            let handle = make_handle(tree, seams, panels);
            Reactive {
                store,
                states,
                bindings,
                lists,
                text_edits,
                projectors,
                handle,
            }
        }

        /// Run a command closure against a throwaway `EventCx`, apply the deferred
        /// `set_hidden` flips it recorded to the store (the router's job), and return
        /// them — the navigation stack's `drive` idiom.
        fn drive(
            &mut self,
            act: impl FnOnce(&DockHandle, &mut EventCx<'_>),
        ) -> Vec<(NodeId, bool)> {
            let ev = still_pointer();
            let flips = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                act(&self.handle, &mut cx);
                cx.__take_hidden_requests()
            };
            for (id, hidden) in &flips {
                self.store.set_hidden(*id, *hidden);
            }
            flips
        }

        fn hidden(&self, key: PanelKey) -> bool {
            self.store
                .hidden(self.handle.node_of(key).expect("panel node"))
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

    const A: PanelKey = PanelKey(0);
    const B: PanelKey = PanelKey(1);
    const C: PanelKey = PanelKey(2);

    /// A single tab group of three panels, all registered.
    fn tab_group() -> (DockTree, HashMap<PanelKey, PanelContent>) {
        let tree = DockTree::new(DockNode::tabs(vec![A, B, C]));
        let mut contents = HashMap::new();
        contents.insert(A, fill_panel());
        contents.insert(B, fill_panel());
        contents.insert(C, fill_panel());
        (tree, contents)
    }

    /// `select_tab` shows the chosen tab and hides its siblings: the build shows the
    /// first, and selecting the third flips the third visible and the first hidden.
    #[test]
    fn select_tab_shows_one_hides_siblings() {
        let (tree, contents) = tab_group();
        let mut rx = Reactive::build(tree, &contents);

        // The build authored A visible, B and C hidden.
        assert!(!rx.hidden(A), "A starts visible");
        assert!(rx.hidden(B), "B starts hidden");
        assert!(rx.hidden(C), "C starts hidden");

        let flips = rx.drive(|h, ev| h.select_tab(ev, C));
        // Three panels in the group -> three deferred flips.
        assert_eq!(flips.len(), 3, "one flip per sibling panel node");
        assert!(rx.hidden(A), "A hidden after selecting C");
        assert!(rx.hidden(B), "B hidden after selecting C");
        assert!(!rx.hidden(C), "C shown after selecting C");
    }

    /// `select_tab` records the selection into the tree too, so a rebuild (or the
    /// a11y projection) sees the chosen tab as selected.
    #[test]
    fn select_tab_updates_the_tree_selection() {
        let (tree, contents) = tab_group();
        let mut rx = Reactive::build(tree, &contents);

        rx.drive(|h, ev| h.select_tab(ev, B));
        let st = rx.handle.state.borrow();
        let DockNode::Tabs { selected, .. } = &st.tree.root else {
            panic!("root is a tab group");
        };
        assert_eq!(*selected, 1, "selecting B records index 1 in the tree");
    }

    /// `select_tab` on an absent key is a no-op: no flips, no selection change.
    #[test]
    fn select_tab_absent_key_is_a_noop() {
        let (tree, contents) = tab_group();
        let mut rx = Reactive::build(tree, &contents);
        let flips = rx.drive(|h, ev| h.select_tab(ev, PanelKey(99)));
        assert!(flips.is_empty(), "an absent key flips nothing");
        assert!(!rx.hidden(A), "the shown tab stays shown");
    }

    /// `close` removes a panel from the docked tree (collapsing the emptied split
    /// into its survivor) and hides its node so it leaves layout at once.
    #[test]
    fn close_removes_from_tree_and_hides_node() {
        // A|B row split; each a lone panel.
        let tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(A),
            DockNode::panel(B),
        ));
        let mut contents = HashMap::new();
        contents.insert(A, fill_panel());
        contents.insert(B, fill_panel());
        let mut rx = Reactive::build(tree, &contents);

        let flips = rx.drive(|h, ev| h.close(ev, A));
        assert_eq!(flips.len(), 1, "closing A hides exactly A's node");
        assert!(rx.hidden(A), "A's node is hidden after close");
        // The tree collapsed the split into B alone.
        let st = rx.handle.state.borrow();
        assert_eq!(
            st.tree.root,
            DockNode::panel(B),
            "the split collapsed into B"
        );
    }

    /// `dock` edits the tree via the pure transform: docking C onto B's left edge
    /// wraps B's region in a new row split with C leading.
    #[test]
    fn dock_edits_the_tree() {
        let tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(A),
            DockNode::panel(B),
        ));
        let mut contents = HashMap::new();
        contents.insert(A, fill_panel());
        contents.insert(B, fill_panel());
        let mut rx = Reactive::build(tree, &contents);

        rx.drive(|h, ev| h.dock(ev, C, B, DropPart::Left));
        let st = rx.handle.state.borrow();
        let DockNode::Split { b: right, .. } = &st.tree.root else {
            panic!("root stays a split");
        };
        let DockNode::Split { a: inner_a, .. } = right.as_ref() else {
            panic!("B's region wrapped into a split");
        };
        assert_eq!(
            **inner_a,
            DockNode::panel(C),
            "C docked leading on B's left"
        );
    }

    /// `float` detaches a panel out of the docked tree into the floating list.
    #[test]
    fn float_detaches_into_the_floating_list() {
        let tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(A),
            DockNode::panel(B),
        ));
        let mut contents = HashMap::new();
        contents.insert(A, fill_panel());
        contents.insert(B, fill_panel());
        let mut rx = Reactive::build(tree, &contents);

        let rect = viso_ui::Rect {
            x: 10.0,
            y: 10.0,
            w: 200.0,
            h: 150.0,
        };
        rx.drive(|h, ev| h.float(ev, A, rect));
        let st = rx.handle.state.borrow();
        assert_eq!(st.tree.root, DockNode::panel(B), "A left the docked tree");
        assert_eq!(st.tree.floating.len(), 1, "A is now floating");
        assert_eq!(st.tree.floating[0].panel, A);
    }

    /// The reconcile intent-drain rewrites the seam's pane weights from its fraction
    /// so the split resizes live — the caller `reconcile_seams` was missing. Driving
    /// the fraction to 0.7 and reconciling makes pane A take 0.7 of the free width.
    #[test]
    fn reconcile_drives_live_seam_geometry() {
        use viso_ui::{Rect, StateValue};
        let tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(A),
            DockNode::panel(B),
        ));
        let mut contents = HashMap::new();
        contents.insert(A, fill_panel());
        contents.insert(B, fill_panel());
        let mut rx = Reactive::build(tree, &contents);

        let seam = rx.handle.state.borrow().seams[0];
        let root = seam.container;
        rx.states.set(seam.fraction, StateValue::Float(0.7));

        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 400.0,
        };
        let mut scratch = Vec::new();
        rx.handle.reconcile(&mut rx.store, &rx.states);
        rx.store.layout(root, surface, &mut scratch);
        rx.handle.reconcile(&mut rx.store, &rx.states);
        rx.store.layout(root, surface, &mut scratch);

        let free = 1000.0 - super::super::build::SEAM_SIZE;
        let pane_a_w = rx.store.bounds_main(seam.pane_a, Axis::Row);
        assert!(
            (pane_a_w - 0.7 * free).abs() < 1.0,
            "reconcile drove pane A to 0.7 of the free width: got {pane_a_w}",
        );
    }
}
