//! The dock's store-mutating reconcile step: turn committed drag fractions into
//! live pane geometry.
//!
//! A seam's pointer/key handler ([`drag`](super::drag)) only writes the seam's
//! `fraction` cell — it holds no node geometry. [`reconcile_seams`] runs after the
//! state flush, holding `&mut NodeStore`, and for each seam reads that fraction,
//! clamps it against the seam container's *resolved* extent and the minimum-pane
//! floor, and rewrites both pane containers' fill weights via
//! [`NodeStore::set_flex_child_weight`](viso_ui::NodeStore::set_flex_child_weight).
//! Because both panes are fill children, the parent redistributes leftover space by
//! weight, so pane A takes `fraction` of the container and pane B the remainder —
//! the split resizes live and proportionally, and scales with the container.
//!
//! This is the "tree-as-warm-state + reconcile drives live re-layout" pattern the
//! module's ADR records: the same handler-writes-intent / reconcile-mutates-store
//! split the virtual list uses, generalized to a control that owns a store-mutating
//! reconcile step. The step is allocation-free and touches only the seams it is
//! given (it rewrites two `Length`s per seam in place); it is not a per-frame path
//! — it runs when a seam's fraction changed.

use viso_ui::{NodeStore, StateStore, StateValue};

use super::build::{MIN_PANE, SEAM_SIZE, SeamRec};

/// Rewrite every seam's pane weights from its current `fraction` cell, clamping the
/// fraction to the minimum-pane floor against each seam container's resolved extent.
///
/// Reads the fraction from `states` (where the drag handler committed it) rather
/// than from an `EventCx`, since the reconcile step runs outside event dispatch.
/// The container's extent is its resolved main-axis bound at the time of the call
/// (a dock lays out to fill its parent, so the extent is only known after a layout
/// pass) — call this after the enclosing layout has resolved the seam containers'
/// bounds, or with the surface extent when driving a headless test.
///
/// A seam whose container is not yet laid out (zero extent) is clamped to the plain
/// `[0, 1]` range, so an initial reconcile before the first layout still produces a
/// sane split; the next reconcile after layout applies the true floor.
///
/// Driven by [`DockHandle::reconcile`](super::command::DockHandle::reconcile), the
/// imperative reconcile intent-drain a host runs after an input transaction.
pub(super) fn reconcile_seams(store: &mut NodeStore, states: &StateStore, seams: &[SeamRec]) {
    for seam in seams {
        let raw = match states.get(seam.fraction) {
            Some(StateValue::Float(v)) => v,
            _ => continue,
        };
        let frac = clamp_to_floor(store, seam, raw);
        store.set_flex_child_weight(seam.pane_a, seam.axis, frac);
        store.set_flex_child_weight(seam.pane_b, seam.axis, (1.0 - frac).max(0.0));
    }
}

/// Clamp `raw` (a 0..1 fraction) so neither pane falls below [`MIN_PANE`] logical
/// pixels, given the seam container's resolved extent. The free space a fraction
/// divides is the container's main-axis extent minus the seam bar; the floor as a
/// fraction of that free space bounds the drag on both sides. A degenerate extent
/// (not yet laid out, or smaller than two floors plus the bar) falls back to the
/// plain `[0, 1]` clamp so the split stays well-defined.
fn clamp_to_floor(store: &NodeStore, seam: &SeamRec, raw: f32) -> f32 {
    let extent = store.bounds_main(seam.container, seam.axis);
    let free = extent - SEAM_SIZE;
    if free <= 2.0 * MIN_PANE {
        // Too small (or unresolved) to honor both floors; keep the raw split.
        return raw.clamp(0.0, 1.0);
    }
    let lo = MIN_PANE / free;
    let hi = 1.0 - lo;
    raw.clamp(lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::dock::build::{BuildOut, PanelNodes};
    use crate::controls::dock::tree::{DockNode, DockTree};
    use crate::controls::dock::{Dock, DockStyle, PanelContent, dock};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use viso_ui::{
        Axis, BindingTable, BoxStyle, BuildCx, Component, LeafStyle, NodeId, NodeStore, Rect,
        SemanticProjector, Size, StateStore, StateValue, TextEdits, VirtualLists,
    };

    /// A surface rect at the origin with the given size (physical-pixel `Rect` has
    /// public `x`/`y`/`w`/`h` fields and no constructor).
    fn surface(w: f32, h: f32) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        }
    }

    /// The reactive stores a dock build writes into, plus the seam records the build
    /// walk produced — enough to build a dock, drive its seam cell, reconcile, lay
    /// out, and read the resulting pane geometry.
    struct Harness {
        store: NodeStore,
        states: StateStore,
        // The build cx borrows these reactive stores; the harness owns them so they
        // outlive the build, but the reconcile/layout path never reads them after.
        #[allow(dead_code)]
        bindings: BindingTable,
        #[allow(dead_code)]
        lists: VirtualLists,
        #[allow(dead_code)]
        text_edits: TextEdits,
        #[allow(dead_code)]
        projectors: SemanticProjector,
        seams: Vec<SeamRec>,
    }

    impl Harness {
        /// Build the dock's tree directly (so the seam records are captured) and
        /// return the region root the whole dock would wrap.
        fn build(
            tree: &DockTree,
            contents: &HashMap<super::super::PanelKey, PanelContent>,
        ) -> Self {
            let mut store = NodeStore::new();
            let mut states = StateStore::new();
            let mut bindings = BindingTable::new();
            let mut lists = VirtualLists::new();
            let mut text_edits = TextEdits::new();
            let mut projectors = SemanticProjector::new();
            let seams;
            {
                let mut out = BuildOut {
                    seams: Vec::new(),
                    zones: Vec::new(),
                };
                let panels: PanelNodes = Rc::new(RefCell::new(HashMap::new()));
                let style = DockStyle::default();
                let mut cx = BuildCx::with_reactive(
                    &mut store,
                    &mut states,
                    &mut bindings,
                    &mut lists,
                    &mut text_edits,
                    &mut projectors,
                );
                crate::controls::dock::build::build_tree(
                    &mut cx, &tree.root, &style, contents, &panels, &mut out,
                );
                seams = out.seams;
            }
            Harness {
                store,
                states,
                bindings,
                lists,
                text_edits,
                projectors,
                seams,
            }
        }

        fn root(&self) -> NodeId {
            // The build authored one top-level split container; its node is the
            // first seam's container.
            self.seams[0].container
        }

        /// Set the first seam's fraction cell directly (the drag handler's effect).
        fn set_fraction(&mut self, f: f32) {
            self.states
                .set(self.seams[0].fraction, StateValue::Float(f));
        }

        /// Reconcile the seams, then lay the tree out into `surface`.
        fn reconcile_and_layout(&mut self, surface: Rect) {
            reconcile_seams(&mut self.store, &self.states, &self.seams);
            let mut scratch = Vec::new();
            self.store.layout(self.root(), surface, &mut scratch);
            // A second reconcile now that the container has a resolved extent lets
            // the min-pane floor apply against the true extent, then re-layout.
            reconcile_seams(&mut self.store, &self.states, &self.seams);
            self.store.layout(self.root(), surface, &mut scratch);
        }
    }

    /// A row split of two lone panels, each panel a fixed leaf.
    fn row_split_tree() -> (DockTree, HashMap<super::super::PanelKey, PanelContent>) {
        use super::super::PanelKey;
        let a = PanelKey(0);
        let b = PanelKey(1);
        let tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(a),
            DockNode::panel(b),
        ));
        let mut contents: HashMap<PanelKey, PanelContent> = HashMap::new();
        contents.insert(
            a,
            Box::new(|cx: &mut BuildCx<'_>| {
                cx.leaf(LeafStyle {
                    size: Size::fill(),
                    style: BoxStyle::NONE,
                });
            }),
        );
        contents.insert(
            b,
            Box::new(|cx: &mut BuildCx<'_>| {
                cx.leaf(LeafStyle {
                    size: Size::fill(),
                    style: BoxStyle::NONE,
                });
            }),
        );
        (tree, contents)
    }

    /// Test 5 — live re-layout: driving a seam's fraction to 0.7 and reconciling
    /// makes pane A occupy 0.7 of the split's free space. This is the gap #1 proof —
    /// a build that only sizes panes once (the reference splitter) could not do it.
    #[test]
    fn reconcile_resizes_panes_live_from_the_fraction() {
        let (tree, contents) = row_split_tree();
        let mut h = Harness::build(&tree, &contents);
        // A wide surface so the 0.7 split is well clear of the min-pane floor.
        h.set_fraction(0.7);
        h.reconcile_and_layout(surface(1000.0, 400.0));

        let seam = h.seams[0];
        let free = 1000.0 - SEAM_SIZE;
        let pane_a_w = h.store.bounds_main(seam.pane_a, Axis::Row);
        assert!(
            (pane_a_w - 0.7 * free).abs() < 1.0,
            "pane A takes 0.7 of the free width: got {}, want {}",
            pane_a_w,
            0.7 * free,
        );
        let pane_b_w = h.store.bounds_main(seam.pane_b, Axis::Row);
        assert!(
            (pane_b_w - 0.3 * free).abs() < 1.0,
            "pane B takes the remaining 0.3: got {pane_b_w}",
        );
    }

    /// The minimum-pane floor bounds a drag: driving the fraction to 0 does not
    /// collapse pane A — it stops at the floor, so pane A stays at least `MIN_PANE`
    /// wide and pane B keeps the rest.
    #[test]
    fn reconcile_clamps_to_the_minimum_pane_floor() {
        let (tree, contents) = row_split_tree();
        let mut h = Harness::build(&tree, &contents);
        h.set_fraction(0.0);
        h.reconcile_and_layout(surface(1000.0, 400.0));

        let seam = h.seams[0];
        let pane_a_w = h.store.bounds_main(seam.pane_a, Axis::Row);
        assert!(
            pane_a_w >= MIN_PANE - 1.0,
            "pane A does not collapse below the floor: got {pane_a_w}",
        );
        assert!(
            pane_a_w <= MIN_PANE + 2.0,
            "pane A rests at the floor, not wider: got {pane_a_w}",
        );
    }

    /// A dock whose seam is never driven keeps its build-time split (0.5 here),
    /// so a reconcile with no fraction change is a stable no-op on geometry.
    #[test]
    fn reconcile_preserves_the_initial_split() {
        let (tree, contents) = row_split_tree();
        let mut h = Harness::build(&tree, &contents);
        h.reconcile_and_layout(surface(1000.0, 400.0));

        let seam = h.seams[0];
        let free = 1000.0 - SEAM_SIZE;
        let pane_a_w = h.store.bounds_main(seam.pane_a, Axis::Row);
        assert!(
            (pane_a_w - 0.5 * free).abs() < 1.0,
            "an undriven seam keeps its 0.5 split: got {pane_a_w}",
        );
    }

    /// A whole `Dock` builds (the `Component` path), proving the reconcile module's
    /// build assumptions hold against the public builder too.
    #[test]
    fn dock_component_builds() {
        let (tree, _c) = row_split_tree();
        let _ = tree;
        let d: Dock = dock(DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(super::super::PanelKey(0)),
            DockNode::panel(super::super::PanelKey(1)),
        )));
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        d.build(&mut cx);
        assert!(cx.root().is_some(), "the dock declares a root");
    }
}
