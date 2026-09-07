//! The dock layout tree — a pure, owned data model of how panels are arranged,
//! plus the pure tree transforms that redock, undock, float, and unfloat them.
//!
//! This module holds **no** `cx`/store types: it is plain data (`DockNode`,
//! `DockTree`, `PanelKey`, `DropPart`) and total functions over it, so every
//! structural edit is unit-testable without building a single node (AGENTS
//! section 35). The build walk in [`super::build`] reads this tree to author
//! nodes; the reconcile step in [`super::reconcile`] applies committed edits and
//! turns them into live geometry.
//!
//! The model is an **owned `Box`-linked enum tree**, not a flat
//! `HashMap<Id, Node>`: Viso already owns a generational node arena and forbids a
//! second synthetic-id map on any traversed path (AGENTS sections 8.2, 29, 45). A
//! dock tree is small (a few dozen nodes), changes only on discrete drag/command
//! actions, and is walked whole at reconcile — so a `Box`-linked tree is
//! cache-friendly, edits in `O(depth)`, and hashes nothing.
//!
//! There is deliberately no `Leaf` variant: a single un-tabbed panel is a
//! [`DockNode::Tabs`] with one panel and `strip: false` (its tab strip hidden).
//! One variant fewer means one build path and one edit path, and it matches the
//! reference dock's `hide_tab_bar` on a lone panel.

use viso_ui::Axis;

/// A stable, `Copy` identity for a dockable panel. The application assigns one per
/// panel when registering its content builder; the tree references panels **only**
/// by key, never by position, so a panel keeps its identity — and its built-once
/// content subtree and reactive cells — as it moves between docked areas
/// (AGENTS section 8.6, 21.8). A small integer, not a string: identity on a
/// traversed path is an ID, never a hashed name (AGENTS section 29).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PanelKey(pub u32);

/// One node of the dock layout tree: either a binary split of two child regions
/// along an axis, or a tabbed group of panels.
///
/// N-way splits are expressed as nested binary `Split`s — the same way a binary
/// tree expresses an arbitrary partition — so there is exactly one split arm to
/// build, edit, and reconcile. `fraction` is the leading child's share of the
/// split's main-axis extent, in `0.0..=1.0`, mirroring the reference splitter's
/// fraction model.
#[derive(Debug, Clone, PartialEq)]
pub enum DockNode {
    /// A binary split: child `a` (leading) takes `fraction` of the main-axis
    /// extent along `axis`, child `b` (trailing) takes the rest, with a draggable
    /// seam between them.
    Split {
        /// The split axis: [`Axis::Row`] places the children side by side,
        /// [`Axis::Column`] stacks them.
        axis: Axis,
        /// Child `a`'s share of the main-axis extent, in `0.0..=1.0`.
        fraction: f32,
        /// The leading child region.
        a: Box<DockNode>,
        /// The trailing child region.
        b: Box<DockNode>,
    },
    /// A tabbed group of one or more panels, of which `selected` is shown. With a
    /// single panel and `strip: false` this is a lone un-tabbed panel; with more
    /// than one panel (or `strip: true`) it shows a tab strip.
    Tabs {
        /// The panels in this group, referenced by stable key.
        panels: Vec<PanelKey>,
        /// The index into `panels` of the shown panel.
        selected: usize,
        /// Whether to show the tab strip. Forced on when there is more than one
        /// panel; a lone panel may hide it.
        strip: bool,
    },
}

/// Where, relative to a target dock region, a dragged panel would drop — the
/// edge bands and center of the target, plus its tab strip. Resolved from the
/// pointer's position within the target's bounds during a panel drag; committed
/// as the redock edit that wraps or joins the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropPart {
    /// Dock to the left of the target (a new `Row` split, dropped panel leading).
    Left,
    /// Dock to the right of the target (a new `Row` split, dropped panel trailing).
    Right,
    /// Dock above the target (a new `Column` split, dropped panel leading).
    Top,
    /// Dock below the target (a new `Column` split, dropped panel trailing).
    Bottom,
    /// Merge into the target's tab group as a new tab.
    Center,
    /// Merge into the target's tab group, same as [`Center`](DropPart::Center) —
    /// distinguished only so a drop onto the visible strip reads as "add a tab"
    /// rather than "split", regardless of the strip's position within the band.
    TabBar,
}

/// A floating panel: a single panel detached from the docked tree, drawn over the
/// scene at an absolute rectangle. Floating panels are a Viso addition over the
/// reference dock (which has none); a float can be dragged back onto a drop band
/// to redock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Floating {
    /// The detached panel.
    pub panel: PanelKey,
    /// Its position and size over the scene, in the dock container's coordinates.
    pub rect: viso_ui::Rect,
}

/// The whole dock arrangement: the docked region tree plus any floating panels.
/// This is the dock's owned **warm** state (built once, read by build and
/// reconcile) — not a bag of reactive scalar cells, which could not hold a tree.
#[derive(Debug, Clone, PartialEq)]
pub struct DockTree {
    /// The root docked region.
    pub root: DockNode,
    /// Panels detached into floating windows over the docked tree.
    pub floating: Vec<Floating>,
}

impl DockNode {
    /// A lone un-tabbed panel: a `Tabs` of one panel with its strip hidden.
    pub fn panel(key: PanelKey) -> DockNode {
        DockNode::Tabs {
            panels: vec![key],
            selected: 0,
            strip: false,
        }
    }

    /// A tabbed group of the given panels, with the first selected and the strip
    /// shown. An empty slice is treated as a single implicit gap — callers pass at
    /// least one panel in normal use.
    pub fn tabs(panels: Vec<PanelKey>) -> DockNode {
        DockNode::Tabs {
            selected: 0,
            strip: panels.len() > 1,
            panels,
        }
    }

    /// A binary split of two regions.
    pub fn split(axis: Axis, fraction: f32, a: DockNode, b: DockNode) -> DockNode {
        DockNode::Split {
            axis,
            fraction: fraction.clamp(0.0, 1.0),
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    /// Every panel key reachable under this node, in left-to-right / top-to-bottom
    /// order. Used by the build walk and by tests; cold (walks the whole subtree),
    /// never a per-frame path.
    pub fn panels(&self, out: &mut Vec<PanelKey>) {
        match self {
            DockNode::Split { a, b, .. } => {
                a.panels(out);
                b.panels(out);
            }
            DockNode::Tabs { panels, .. } => out.extend_from_slice(panels),
        }
    }

    /// Whether any panel under this node matches `key`.
    pub fn contains(&self, key: PanelKey) -> bool {
        match self {
            DockNode::Split { a, b, .. } => a.contains(key) || b.contains(key),
            DockNode::Tabs { panels, .. } => panels.contains(&key),
        }
    }
}

/// The axis and leading/trailing side a [`DropPart`] edge implies, or `None` for a
/// center/tab-bar drop (which merges rather than splits).
fn split_of(part: DropPart) -> Option<(Axis, bool)> {
    match part {
        DropPart::Left => Some((Axis::Row, true)),
        DropPart::Right => Some((Axis::Row, false)),
        DropPart::Top => Some((Axis::Column, true)),
        DropPart::Bottom => Some((Axis::Column, false)),
        DropPart::Center | DropPart::TabBar => None,
    }
}

impl DockTree {
    /// A dock holding a single lone panel.
    pub fn single(key: PanelKey) -> DockTree {
        DockTree {
            root: DockNode::panel(key),
            floating: Vec::new(),
        }
    }

    /// A dock whose root is the given region.
    pub fn new(root: DockNode) -> DockTree {
        DockTree {
            root,
            floating: Vec::new(),
        }
    }

    /// Every panel key in the whole tree (docked then floating), in order.
    pub fn panels(&self) -> Vec<PanelKey> {
        let mut out = Vec::new();
        self.root.panels(&mut out);
        out.extend(self.floating.iter().map(|f| f.panel));
        out
    }

    /// Remove `key` wherever it appears — from its tab group (collapsing an
    /// emptied split into its surviving sibling) or from the floating list. A no-op
    /// if the key is absent. Returns whether anything was removed. The shared first
    /// half of undock/float/redock: a panel always leaves its current home before
    /// joining a new one.
    pub fn remove(&mut self, key: PanelKey) -> bool {
        if let Some(i) = self.floating.iter().position(|f| f.panel == key) {
            self.floating.remove(i);
            return true;
        }
        remove_from(&mut self.root, key)
    }

    /// Detach `key` into a floating panel at `rect`, removing it from wherever it
    /// currently lives. A no-op returning `false` if the key is absent.
    pub fn float(&mut self, key: PanelKey, rect: viso_ui::Rect) -> bool {
        if !self.contains(key) {
            return false;
        }
        self.remove(key);
        self.floating.push(Floating { panel: key, rect });
        true
    }

    /// Whether `key` appears anywhere in the tree.
    pub fn contains(&self, key: PanelKey) -> bool {
        self.root.contains(key) || self.floating.iter().any(|f| f.panel == key)
    }

    /// Redock `key` relative to the region containing `target`, per `part`: an edge
    /// part wraps the target's region in a new split with `key` on the named side;
    /// a center/tab-bar part joins `key` into the target's tab group. `key` may
    /// already live in the tree (a move between regions) or be a fresh panel being
    /// docked in for the first time; either way it is first detached from any
    /// current home so it is never duplicated. A no-op returning `false` if
    /// `target` is absent from the docked tree, or if `key` and `target` are the
    /// same panel.
    pub fn dock(&mut self, key: PanelKey, target: PanelKey, part: DropPart) -> bool {
        if key == target || !self.root.contains(target) {
            return false;
        }
        // Detach the moving panel first so wrapping the target's region never
        // re-captures the moving panel (it may have lived under the target); a
        // no-op when `key` is a fresh panel that lived nowhere.
        self.remove(key);
        match split_of(part) {
            None => join_tabs(&mut self.root, target, key),
            Some((axis, key_leading)) => {
                wrap_target(&mut self.root, target, key, axis, key_leading)
            }
        }
    }
}

/// Remove `key` from the tab group that holds it, collapsing an emptied `Split`
/// into its surviving sibling in place. Returns whether the key was found. Walks
/// the tree once; the collapse is why this is not a simple `Vec::retain`.
fn remove_from(node: &mut DockNode, key: PanelKey) -> bool {
    match node {
        DockNode::Tabs {
            panels, selected, ..
        } => {
            let Some(i) = panels.iter().position(|&k| k == key) else {
                return false;
            };
            panels.remove(i);
            // Keep `selected` in range and pointing at a still-present tab.
            if *selected >= panels.len() {
                *selected = panels.len().saturating_sub(1);
            }
            true
        }
        DockNode::Split { a, b, .. } => {
            // The key lives in at most one subtree; try each, and collapse this
            // split if removing it left one side an empty orphan.
            if remove_from(a, key) || remove_from(b, key) {
                collapse_if_empty(node);
                true
            } else {
                false
            }
        }
    }
}

/// Whether a node is an empty tab group (no panels) — an orphan left after its
/// last panel was removed, to be collapsed away.
fn is_empty_tabs(node: &DockNode) -> bool {
    matches!(node, DockNode::Tabs { panels, .. } if panels.is_empty())
}

/// If `node` is a `Split` with one now-empty child, replace `node` with the
/// surviving child in place (the split's chrome and seam go away). A no-op for a
/// split whose children are both non-empty.
fn collapse_if_empty(node: &mut DockNode) {
    let DockNode::Split { a, b, .. } = node else {
        return;
    };
    if is_empty_tabs(a) {
        let survivor = std::mem::replace(b.as_mut(), DockNode::tabs(Vec::new()));
        *node = survivor;
    } else if is_empty_tabs(b) {
        let survivor = std::mem::replace(a.as_mut(), DockNode::tabs(Vec::new()));
        *node = survivor;
    }
}

/// Join `key` into the tab group that holds `target`, selecting the newly added
/// tab and showing the strip. Returns whether the target was found.
fn join_tabs(node: &mut DockNode, target: PanelKey, key: PanelKey) -> bool {
    match node {
        DockNode::Tabs {
            panels,
            selected,
            strip,
        } => {
            if panels.contains(&target) {
                panels.push(key);
                *selected = panels.len() - 1;
                *strip = true;
                true
            } else {
                false
            }
        }
        DockNode::Split { a, b, .. } => join_tabs(a, target, key) || join_tabs(b, target, key),
    }
}

/// Replace the region containing `target` with a new `Split` along `axis`, placing
/// a lone-panel region for `key` on the leading (`key_leading`) or trailing side
/// and the former region on the other. Returns whether the target was found.
///
/// The `target` may be the whole tree root, in which case the root itself is
/// wrapped — matching the reference dock, where dropping onto the root's edge
/// splits the entire dock.
fn wrap_target(
    node: &mut DockNode,
    target: PanelKey,
    key: PanelKey,
    axis: Axis,
    key_leading: bool,
) -> bool {
    // The region *containing* target is the smallest subtree that holds it; wrap
    // the whole `Tabs` group (not just the one panel) so its other tabs ride along.
    match node {
        DockNode::Tabs { panels, .. } if panels.contains(&target) => {
            wrap_here(node, key, axis, key_leading);
            true
        }
        DockNode::Tabs { .. } => false,
        DockNode::Split { a, b, .. } => {
            // If a whole child region is the target group, wrap that child; else
            // recurse. Checking the child as a unit lets an edge-drop onto a tabbed
            // child split beside the child rather than inside it.
            if matches!(a.as_ref(), DockNode::Tabs { panels, .. } if panels.contains(&target)) {
                wrap_here(a, key, axis, key_leading);
                true
            } else if matches!(b.as_ref(), DockNode::Tabs { panels, .. } if panels.contains(&target))
            {
                wrap_here(b, key, axis, key_leading);
                true
            } else {
                wrap_target(a, target, key, axis, key_leading)
                    || wrap_target(b, target, key, axis, key_leading)
            }
        }
    }
}

/// Replace `slot` in place with a `Split` of a new lone-panel region for `key` and
/// the former contents of `slot`, ordered by `key_leading`. The split's fraction
/// starts centered.
fn wrap_here(slot: &mut DockNode, key: PanelKey, axis: Axis, key_leading: bool) {
    // Move the existing region out so it becomes one side of the new split.
    let former = std::mem::replace(slot, DockNode::tabs(Vec::new()));
    let new_panel = DockNode::panel(key);
    let (a, b) = if key_leading {
        (new_panel, former)
    } else {
        (former, new_panel)
    };
    *slot = DockNode::split(axis, 0.5, a, b);
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PanelKey = PanelKey(0);
    const B: PanelKey = PanelKey(1);
    const C: PanelKey = PanelKey(2);

    #[test]
    fn lone_panel_hides_strip() {
        let DockNode::Tabs { panels, strip, .. } = DockNode::panel(A) else {
            panic!("panel() must build a Tabs node");
        };
        assert_eq!(panels, vec![A]);
        assert!(!strip, "a lone panel hides its tab strip");
    }

    #[test]
    fn left_drop_wraps_target_in_row_split() {
        // Two lone panels A|B side by side; drop C onto B's left edge.
        let mut tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(A),
            DockNode::panel(B),
        ));
        assert!(tree.dock(C, B, DropPart::Left));
        // B's region is now a Row split with C leading and B trailing.
        let DockNode::Split { b: right, .. } = &tree.root else {
            panic!("root stays a split");
        };
        let DockNode::Split {
            axis,
            a: inner_a,
            b: inner_b,
            ..
        } = right.as_ref()
        else {
            panic!("B's region wrapped into a split");
        };
        assert_eq!(*axis, Axis::Row);
        assert_eq!(**inner_a, DockNode::panel(C));
        assert_eq!(**inner_b, DockNode::panel(B));
    }

    #[test]
    fn root_edge_drop_wraps_whole_root() {
        // A single lone panel A as the whole dock; drop B onto its top edge.
        let mut tree = DockTree::single(A);
        assert!(tree.dock(B, A, DropPart::Top));
        let DockNode::Split { axis, a, b, .. } = &tree.root else {
            panic!("the whole root is wrapped into a split");
        };
        assert_eq!(*axis, Axis::Column);
        assert_eq!(
            **a,
            DockNode::panel(B),
            "Top puts the dropped panel leading"
        );
        assert_eq!(**b, DockNode::panel(A));
    }

    #[test]
    fn center_drop_joins_tab_group() {
        let mut tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(A),
            DockNode::panel(B),
        ));
        assert!(tree.dock(C, B, DropPart::Center));
        let DockNode::Split { b: right, .. } = &tree.root else {
            panic!("root stays a split");
        };
        let DockNode::Tabs {
            panels,
            selected,
            strip,
        } = right.as_ref()
        else {
            panic!("B's region became a tab group");
        };
        assert_eq!(*panels, vec![B, C]);
        assert_eq!(*selected, 1, "the newly joined tab is selected");
        assert!(*strip, "a multi-panel group shows its strip");
    }

    #[test]
    fn undock_collapses_orphaned_split_into_survivor() {
        // A|B split; remove A. The split collapses into B alone.
        let mut tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(A),
            DockNode::panel(B),
        ));
        assert!(tree.remove(A));
        assert_eq!(tree.root, DockNode::panel(B), "the split collapses into B");
    }

    #[test]
    fn move_between_regions_preserves_panel_key() {
        // A|B; move A into B's tab group. A must keep its key (identity), not be
        // rebuilt — the reconcile relies on this to remount the existing node.
        let mut tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(A),
            DockNode::panel(B),
        ));
        assert!(tree.dock(A, B, DropPart::Center));
        // The A|B split collapsed (A left its side), leaving B's group holding both.
        let DockNode::Tabs { panels, .. } = &tree.root else {
            panic!("root collapsed into B's tab group");
        };
        assert!(panels.contains(&A) && panels.contains(&B));
        assert_eq!(panels.len(), 2, "A moved, it was not duplicated");
    }

    #[test]
    fn nested_binary_splits_express_n_way() {
        // Three panels in a row: Split(A, Split(B, C)). panels() flattens in order.
        let tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.33,
            DockNode::panel(A),
            DockNode::split(Axis::Row, 0.5, DockNode::panel(B), DockNode::panel(C)),
        ));
        assert_eq!(tree.panels(), vec![A, B, C]);
    }

    #[test]
    fn float_removes_from_dock_and_can_be_absent() {
        let mut tree = DockTree::new(DockNode::split(
            Axis::Row,
            0.5,
            DockNode::panel(A),
            DockNode::panel(B),
        ));
        let rect = viso_ui::Rect {
            x: 10.0,
            y: 10.0,
            w: 200.0,
            h: 150.0,
        };
        assert!(tree.float(A, rect));
        assert_eq!(tree.root, DockNode::panel(B), "A left the docked tree");
        assert_eq!(tree.floating.len(), 1);
        assert_eq!(tree.floating[0].panel, A);
        assert!(!tree.float(C, rect), "floating an absent panel is a no-op");
    }

    #[test]
    fn dock_rejects_self_and_absent_target() {
        let mut tree = DockTree::single(A);
        assert!(
            !tree.dock(A, A, DropPart::Left),
            "cannot dock a panel onto itself"
        );
        // An absent *target* is a no-op: there is nothing to wrap or join.
        assert!(
            !tree.dock(A, C, DropPart::Left),
            "cannot dock relative to a target that is not in the tree"
        );
        // A fresh `key` (one not yet in the tree) is not rejected — docking a
        // brand-new panel relative to a present target is the primary case.
        assert!(tree.dock(B, A, DropPart::Left));
        assert!(tree.contains(B), "the fresh panel joined the tree");
    }
}
