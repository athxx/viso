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

use std::collections::HashMap;

use viso_ui::{NodeId, NodeStore, Size, StateStore, StateValue, Vec2};

use super::build::{MIN_PANE, RedockIntents, SEAM_SIZE, SeamRec, SharedZones};
use super::drag::RedockIntent;
use super::semantics;
use super::tree::{DockTree, DropPart, PanelKey};

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

/// Drain every committed [`RedockIntent`] the drag handlers pushed, apply its pure
/// tree edit, remount the moved panel's built-once node under its new region, then
/// hide the drop hint and refresh the zone rects from resolved bounds.
///
/// This is the structural half of the reconcile step (a redock genuinely changes
/// tree structure, AGENTS section 8.1), run before [`reconcile_seams`] rewrites the
/// live geometry. Unlike the seam path it does not rebuild anything: a panel keeps
/// its identity — its built-once content subtree and its reactive cells — as it
/// moves, so a **center/tab-bar join** is a pure arena move (detach the panel node,
/// re-append it under the target group's panel-area flex — the same bounded-alloc
/// node move the recycle path uses, never a rebuild). An **edge split** part records
/// the logical tree edit (so the tree stays the source of truth), but authoring the
/// new split container and wiring its seam needs the build walk's `BuildCx`, which a
/// store-only reconcile step does not hold; the container authoring rides the next
/// build, leaving the moved node's identity and state intact meanwhile.
///
/// After applying every intent the drop hint is hidden (a drop is done, so the
/// transient highlight leaves layout) and each zone's rect is refreshed from its
/// node's resolved world bounds, so the next drag hit-tests live geometry rather
/// than a stale build-time rect.
///
/// A no-op when the queue is empty — the common case — so a reconcile with no
/// pending redock touches nothing.
pub(super) fn reconcile_redock(
    store: &mut NodeStore,
    tree: &mut DockTree,
    panels: &HashMap<PanelKey, NodeId>,
    intents: &RedockIntents,
    zones: &SharedZones,
    hint: NodeId,
) {
    let drained: Vec<RedockIntent> = intents.borrow_mut().drain(..).collect();
    if drained.is_empty() {
        return;
    }

    for intent in drained {
        let RedockIntent::Dock { key, target, part } = intent;
        // Apply the pure tree edit first; a false return (absent target or a
        // self-drop) leaves the tree and the nodes untouched.
        if !tree.dock(key, target, part) {
            continue;
        }
        // A center/tab-bar join moves the panel into the target's group in place:
        // detach the moving panel's node and re-append it under the same parent the
        // target's node lives under (the target group's panel-area flex). An edge
        // split part changes containers the build walk must author, so the node move
        // rides the next build — the logical edit above already recorded it.
        if matches!(part, DropPart::Center | DropPart::TabBar)
            && let (Some(&moved), Some(&anchor)) = (panels.get(&key), panels.get(&target))
            && let Some(area) = store.parent(anchor)
        {
            store.arena_detach(moved);
            store.arena_append_child(area, moved);
        }
    }

    // The drop is done: hide the transient hint and refresh each zone's rect from its
    // node's resolved world bounds so the next drag hit-tests live geometry.
    store.set_hidden(hint, true);
    let mut reg = zones.borrow_mut();
    for zone in reg.iter_mut() {
        zone.rect = store.world(zone.node);
    }
}

/// A live floating-panel host the float reconcile step mints and reuses across
/// reconciles: the two overlay nodes ([`NodeStore::alloc_fixed_host`] returns a
/// host/mount pair — the outer host the canvas positions, the inner mount holding
/// the panel's exact rectangle) plus the last rect it was positioned at, so a
/// reconcile only re-touches geometry when the float actually moved or resized.
///
/// Warm state (one entry per floating panel, minted on the reconcile that first
/// sees the panel in [`DockTree::floating`], torn down when it leaves), never a
/// per-frame path.
pub(super) struct FloatHost {
    /// The outer host under the floats canvas; the canvas positions it by row offset
    /// (vertical) and a translate (horizontal), since the canvas only lays rows out
    /// on its main axis.
    host: NodeId,
    /// The inner mount holding the panel's exact `w × h` rectangle; the panel node
    /// re-parents under it. Resized in place when the float's rect changes.
    mount: NodeId,
    /// The rect the host was last positioned/sized at; a reconcile skips the geometry
    /// writes when it is unchanged.
    rect: viso_ui::Rect,
}

/// The keyed live float-host map — one [`FloatHost`] per currently-floating panel,
/// keyed by the panel it holds. Owned beside the tree as warm state so hosts survive
/// across reconciles rather than being re-authored every drain.
pub(super) type FloatHosts = HashMap<PanelKey, FloatHost>;

/// Mount every floating panel into the overlay canvas and position it, and un-mount
/// every panel that stopped floating.
///
/// The docked half of the tree is nodes the build walk authored; a floating panel,
/// by contrast, has no build-time home — the float overlay canvas is authored empty
/// ([`BuildOut::empty`](super::build::BuildOut::empty)) and this step re-parents a
/// floated panel's built-once node under a freshly-minted host in it. This mirrors
/// the redock path's arena move (never a rebuild — the panel keeps its identity, its
/// content subtree, and its reactive cells as it floats and docks back), extended to
/// the overlay: the tree's [`DockTree::floating`] list is the source of truth and
/// this step reconciles the store to it.
///
/// For each panel in `tree.floating`:
/// - **not yet hosted** — mint a host/mount pair under `floats_canvas` via
///   [`NodeStore::alloc_fixed_host`], re-parent the panel's node under the mount,
///   name the host a floating [`Region`](viso_ui::Role::Region)
///   ([`semantics::floating`]), and position + size it;
/// - **already hosted at a changed rect** — re-position the host and resize the mount
///   in place (a moved/resized float);
/// - **already hosted, unchanged** — nothing.
///
/// For each hosted panel no longer in `tree.floating` (it docked back or closed):
/// tear the host down. The redock drain runs first and, for a center/tab-bar join,
/// has already re-parented the panel node under its new docked area — so this step
/// detaches the panel from the mount **only if it is still parented there** (an edge
/// re-dock, or a close, that left it under the mount), leaving an already-remounted
/// node untouched, then removes the empty host chrome.
///
/// A no-op when nothing floats and nothing was hosted — the common case.
pub(super) fn reconcile_floats(
    store: &mut NodeStore,
    tree: &DockTree,
    panels: &HashMap<PanelKey, NodeId>,
    floats_canvas: NodeId,
    hosts: &mut FloatHosts,
) {
    // Mount / reposition every currently-floating panel.
    for float in &tree.floating {
        let Some(&panel_node) = panels.get(&float.panel) else {
            continue;
        };
        match hosts.get_mut(&float.panel) {
            Some(existing) => {
                if existing.rect != float.rect {
                    position_host(store, existing, float.rect);
                }
            }
            None => {
                let (host, mount) =
                    store.alloc_fixed_host(floats_canvas, Size::fixed(float.rect.w, float.rect.h));
                store.arena_detach(panel_node);
                store.arena_append_child(mount, panel_node);
                store.set_semantics(host, semantics::floating(float.panel));
                let mut rec = FloatHost {
                    host,
                    mount,
                    rect: float.rect,
                };
                position_host(store, &mut rec, float.rect);
                hosts.insert(float.panel, rec);
            }
        }
    }

    // Tear down hosts whose panel stopped floating (docked back or closed).
    hosts.retain(|key, rec| {
        if tree.floating.iter().any(|f| f.panel == *key) {
            return true;
        }
        // The panel left the floating list. If its node is still parented under this
        // host's mount, the redock drain has not remounted it (an edge re-dock or a
        // close), so detach it here — a close hid it, an edge re-dock's build will
        // remount it. If it is parented elsewhere, the redock drain already remounted
        // it; leave it be. Either way remove the now-empty host chrome.
        if let Some(&panel_node) = panels.get(key)
            && store.parent(panel_node) == Some(rec.mount)
        {
            store.arena_detach(panel_node);
        }
        store.arena_detach(rec.host);
        false
    });
}

/// Position a float host at `rect`: the floats canvas is an `AbsoluteRows` canvas
/// that lays each hosted row out along its main axis by a per-row offset and forces
/// the cross extent, so the vertical position is the host's row offset and the
/// horizontal position a translate the canvas does not itself apply. The mount is
/// resized to the rect's `w × h` (a no-op inside the store if unchanged). Records the
/// applied rect so a later reconcile can skip an unchanged float.
fn position_host(store: &mut NodeStore, rec: &mut FloatHost, rect: viso_ui::Rect) {
    store.set_fixed_size(rec.mount, Size::fixed(rect.w, rect.h));
    store.set_row_offset(rec.host, rect.y);
    store.set_translate(rec.host, Vec2 { x: rect.x, y: 0.0 });
    rec.rect = rect;
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
                let mut out = BuildOut::empty(&mut cx);
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
