//! The build walk: author the retained node subtree for a [`DockTree`].
//!
//! [`build_tree`] walks the owned [`DockNode`] tree (see [`super::tree`]) once and
//! authors flex containers, seams, tab strips, and panel content into the
//! [`BuildCx`], recording the built node ids the later drag/reconcile steps need.
//! This is the tree→node half of the "tree-as-warm-state" model: the tree is the
//! source of truth, the node arena is its rendered projection, and a structural
//! edit re-authors only the changed subtree (a later section) rather than
//! rebuilding the arena each frame.
//!
//! A panel's content builder runs exactly once, the first time its panel is
//! authored, and its built node id is recorded in the keyed `panels` map so a
//! later section can remount the *existing* node when the panel moves between
//! docked areas — the panel keeps its identity, its subtree, and its reactive
//! cells across a redock (AGENTS section 8.1: no rebuild for a move). Section 1
//! authors a static arrangement with no drag or command wiring; the seam and zone
//! records exist so sections 2+ can drive live geometry without re-walking.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use viso_ui::{
    Align, BoxStyle, BuildCx, DirtyClass, FlexStyle, Inset, Justify, LeafStyle, Length, NodeId,
    Size, StateValue,
};

use super::semantics;
use super::tree::{DockNode, PanelKey};
use super::{DockStyle, PanelContent};

/// The default seam thickness on the split's main axis, matching the reference
/// splitter's `BAR_SIZE` so a dock seam and a standalone splitter read alike.
pub(super) const SEAM_SIZE: f32 = 6.0;

/// The smallest extent, in logical pixels, a pane may be dragged down to before a
/// seam stops — the minimum-pane-size floor (AGENTS section 15: the same floor
/// bounds a keyboard step). Applied by the drag/reconcile sections; defined here
/// beside `SEAM_SIZE` as the shared seam geometry constant.
// Consumed by the seam-drag clamp in section 2 (live seam drag + min sizes); the
// static build in section 1 authors geometry but never clamps.
#[allow(dead_code)]
pub(super) const MIN_PANE: f32 = 50.0;

/// The map from a panel's stable [`PanelKey`] to the node id its content was built
/// into. Filled during the build walk (written once), read by the drag and
/// reconcile sections at event time to remount an existing panel node when it
/// moves. Keyed lookup on a *discrete* action (a redock), never a per-frame path,
/// so a `HashMap` is the right structure here (AGENTS section 45).
pub(super) type PanelNodes = Rc<RefCell<HashMap<PanelKey, NodeId>>>;

/// One built seam: the flex container holding a split's two panes and its bar, plus
/// the two pane containers whose fill weights a live drag rewrites, and the seam's
/// three reactive drag cells. Recorded during the walk so a later section can drive
/// the seam without re-walking the tree — the seam's warm side-record.
///
/// The three cells mirror the reference splitter's drag model: `fraction` is the
/// leading pane's share (bound to the container's `PAINT`), `down_pos` and
/// `start_frac` anchor a delta drag. Section 1 authors them and binds `fraction`;
/// the drag handler and the store-mutating reconcile step land in section 2.
// `container`/`fraction`/`down_pos`/`start_frac` are read by the seam-drag handler
// and the weight-rewriting reconcile step in section 2; section 1 records them.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub(super) struct SeamRec {
    /// The split container node (a flex whose two fill children are the panes).
    pub container: NodeId,
    /// The leading pane container, whose fill weight is the split fraction.
    pub pane_a: NodeId,
    /// The trailing pane container, whose fill weight is one minus the fraction.
    pub pane_b: NodeId,
    /// The split axis, so a drag knows which pointer coordinate is the main axis.
    pub axis: viso_ui::Axis,
    /// The reactive cell holding the leading pane's fraction, bound to `PAINT`.
    pub fraction: viso_ui::StateId,
    /// The reactive cell recording the pointer's main-axis position at press.
    pub down_pos: viso_ui::StateId,
    /// The reactive cell recording the fraction at press, for a delta drag.
    pub start_frac: viso_ui::StateId,
}

/// One built dock region and its screen node, for drop-target hit resolution. A
/// panel drag resolves the pointer against these warm rectangles (an `EventCx`
/// cannot hit-test arbitrary nodes): section 1 records the region node and the
/// primary panel key it holds, and the drag-to-redock section fills `rect` from the
/// node's resolved bounds before a drag begins. Recorded per tab group (the smallest
/// droppable region).
#[derive(Debug, Clone, Copy)]
pub(super) struct ZoneRec {
    /// The tab-group container node this zone covers.
    pub node: NodeId,
    /// A representative panel in the group — the drop's target for a redock edit.
    pub target: PanelKey,
    /// The zone's screen rectangle, refreshed from the node's resolved bounds when
    /// a panel drag begins. Zero until first filled — a drag reads the live bounds
    /// through the store rather than trusting a stale build-time rect.
    pub rect: viso_ui::Rect,
}

/// The shared drop-zone registry a panel-drag handler hit-tests against. Filled from
/// [`BuildOut::zones`] after the walk (the walk collects the zones in order; the
/// registry is shared with the built handlers so a drag reads the live rects the
/// reconcile step refreshes). Written on a discrete action (a drag start), read on a
/// drag move — never a per-frame path.
pub(super) type SharedZones = Rc<RefCell<Vec<ZoneRec>>>;

/// The queue a panel-drag handler pushes a redock intent onto at drop; the reconcile
/// step drains it, edits the tree, and remounts the moved panel node. The
/// handler-writes-intent / reconcile-mutates-store split (a tree edit needs
/// `&mut NodeStore`, which an [`EventCx`](viso_ui::EventCx) does not hold), the same
/// shape the seam drag uses for its `fraction` cell — a structural edit cannot ride
/// a scalar state cell, so it rides this queue instead.
pub(super) type RedockIntents = Rc<RefCell<Vec<super::drag::RedockIntent>>>;

/// What the build walk records for the sections that follow: the seams to drive,
/// the zones to hit-test, the keyed panel nodes to remount, plus the shared drag
/// channel (drop-hint node, redock intent queue, shared zone registry) the
/// drag-to-redock handlers close over. All warm (written once at build, read on
/// discrete actions), never per-frame hot state.
pub(super) struct BuildOut {
    /// Every split's seam record, in walk order.
    pub seams: Vec<SeamRec>,
    /// Every tab group's drop-zone record, in walk order.
    pub zones: Vec<ZoneRec>,
    /// The pre-authored drop-hint overlay leaf a drag toggles visible over the zone
    /// the pointer is over. Hidden at build; shown by a deferred `set_hidden`.
    pub hint: NodeId,
    /// The pre-authored overlay canvas that floating panels mount into. An
    /// [`LayoutInput::AbsoluteRows`] canvas over the whole dock, marked overlay so a
    /// floated panel paints above the docked tree; empty at build, the float
    /// reconcile step re-parents a floated panel's node under it and positions it.
    pub floats: NodeId,
    /// The redock intent queue a drop pushes onto; the reconcile step drains it.
    pub intents: RedockIntents,
    /// The shared zone registry the handlers hit-test; filled after the walk.
    pub zones_shared: SharedZones,
}

/// The drop-hint overlay's translucent tint — a faint highlight the drag toggles
/// visible over the zone the pointer is over, the same shape the modal's scrim uses
/// (an overlay leaf, hidden at build, shown by a deferred `set_hidden`).
const HINT_TINT: viso_ui::Rgba = viso_ui::Rgba {
    r: 0.30,
    g: 0.55,
    b: 0.95,
    a: 0.30,
};

impl BuildOut {
    /// Mint an empty walk record: no seams or zones yet, empty intent/zone queues,
    /// the pre-authored drop-hint overlay leaf (a fill-size overlay, hidden at
    /// build — the modal scrim shape), and the pre-authored floating-panel overlay
    /// canvas. The build walk fills the seams and zones as it recurses; after the
    /// walk the caller copies `zones` into `zones_shared` so the drag handlers
    /// hit-test the same rects the reconcile step refreshes.
    ///
    /// The floats canvas is an [`LayoutInput::AbsoluteRows`] container over the whole
    /// dock, marked overlay so a floated panel paints above the docked tree. It is
    /// empty at build — a panel floats by the float reconcile step re-parenting the
    /// panel's built-once node under this canvas and driving its row offset — so an
    /// un-floated dock authors the canvas but mounts nothing in it (the canvas skips
    /// any child with no row offset, so an empty canvas lays out to nothing).
    pub fn empty(cx: &mut BuildCx<'_>) -> Self {
        let hint = cx.leaf(LeafStyle {
            size: Size::fill(),
            style: BoxStyle::solid(HINT_TINT),
        });
        cx.set_overlay(hint, true);
        cx.set_hidden(hint, true);
        // The floating-panel overlay canvas: an absolute-rows canvas filling the
        // dock, over the docked tree. Empty until a panel floats; the float reconcile
        // step remounts a floated panel's node here and positions it by row offset.
        let floats = cx.absolute_rows(viso_ui::Axis::Column, Size::fill(), |_cx| {});
        cx.set_overlay(floats, true);
        BuildOut {
            seams: Vec::new(),
            zones: Vec::new(),
            hint: hint.id(),
            floats: floats.id(),
            intents: Rc::new(RefCell::new(Vec::new())),
            zones_shared: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

/// Author the node subtree for `node` into `cx`, returning the built region's
/// container handle. Recurses on splits (authoring a seam container of two panes
/// and a bar) and builds tab groups at the leaves (a strip of tab buttons over the
/// selected panel). `panels` is filled with each panel's built node id; `out`
/// collects the seam and zone records.
///
/// The panel content in `contents` is looked up by key: a `Split` never holds a
/// panel directly, and a `Tabs` holds panel keys whose content builders live in
/// the dock's `contents` map. A key with no registered content builds an empty
/// group (a defensive default; a well-formed dock registers every key it names).
pub(super) fn build_tree(
    cx: &mut BuildCx<'_>,
    node: &DockNode,
    style: &DockStyle,
    contents: &HashMap<PanelKey, PanelContent>,
    panels: &PanelNodes,
    out: &mut BuildOut,
) -> viso_ui::Handle {
    match node {
        DockNode::Split {
            axis,
            fraction,
            a,
            b,
        } => build_split(cx, *axis, *fraction, a, b, style, contents, panels, out),
        DockNode::Tabs {
            panels: keys,
            selected,
            strip,
        } => build_tabs(cx, keys, *selected, *strip, style, contents, panels, out),
    }
}

/// Author a binary split: a flex along `axis` holding pane A (fill weight
/// `fraction`), a fixed-thickness bar leaf, and pane B (fill weight
/// `1 - fraction`). Both panes are fill children so the split scales with its
/// container; the reconcile section rewrites the weights live from the drag cell.
#[allow(clippy::too_many_arguments)]
fn build_split(
    cx: &mut BuildCx<'_>,
    axis: viso_ui::Axis,
    fraction: f32,
    a: &DockNode,
    b: &DockNode,
    style: &DockStyle,
    contents: &HashMap<PanelKey, PanelContent>,
    panels: &PanelNodes,
    out: &mut BuildOut,
) -> viso_ui::Handle {
    // Three drag cells per seam, mirroring the reference splitter: the leading
    // pane's fraction (bound to PAINT below), and the press anchors for a delta
    // drag. Section 1 authors and binds them; section 2 drives them.
    let frac0 = fraction.clamp(0.0, 1.0);
    let cell_fraction = cx.state(StateValue::Float(frac0));
    let cell_down = cx.state(StateValue::Float(0.0));
    let cell_start = cx.state(StateValue::Float(frac0));

    // Panes fill their share of the main axis; both are fill children so the split
    // scales with the container. Weight is the fraction split — a fill child's
    // measured size is zero, so the parent distributes leftover space by weight.
    let pane_a_size = size_on(axis, Length::Fill { weight: frac0 }, Length::fill());
    let pane_b_size = size_on(
        axis,
        Length::Fill {
            weight: (1.0 - frac0).max(0.0),
        },
        Length::fill(),
    );
    let bar_size = size_on(axis, Length::Fixed(SEAM_SIZE), Length::fill());

    // Filled inside the build closure once the pane containers exist; the seam
    // record is pushed after the container builds (a nested seam records itself
    // during the recursion, so records land in inner-to-outer order — lookup is
    // by node id, not index, so the order is not load-bearing).
    let mut pane_a_id: Option<NodeId> = None;
    let mut pane_b_id: Option<NodeId> = None;
    let mut bar_handle: Option<viso_ui::Handle> = None;

    let container = cx.flex(
        FlexStyle {
            axis,
            gap: 0.0,
            padding: Inset::all(0.0),
            align: Align::Stretch,
            justify: Justify::Start,
            size: Size::fill(),
            style: BoxStyle::NONE,
        },
        |cx| {
            // Pane A: a fill container holding the leading region's subtree.
            let pane_a = cx.flex(
                FlexStyle {
                    axis,
                    gap: 0.0,
                    padding: Inset::all(0.0),
                    align: Align::Stretch,
                    justify: Justify::Start,
                    size: pane_a_size,
                    style: BoxStyle::NONE,
                },
                |cx| {
                    build_tree(cx, a, style, contents, panels, out);
                },
            );
            pane_a_id = Some(pane_a.id());

            // The seam bar: a fixed-thickness leaf across the cross axis, named for
            // an assistive technology (the keyboard-resize target, section 2).
            let bar = cx.leaf(LeafStyle {
                size: bar_size,
                style: style.seam,
            });
            cx.focusable(bar, true);
            cx.semantics(bar, semantics::seam());
            bar_handle = Some(bar);

            // Pane B: a fill container holding the trailing region's subtree.
            let pane_b = cx.flex(
                FlexStyle {
                    axis,
                    gap: 0.0,
                    padding: Inset::all(0.0),
                    align: Align::Stretch,
                    justify: Justify::Start,
                    size: pane_b_size,
                    style: BoxStyle::NONE,
                },
                |cx| {
                    build_tree(cx, b, style, contents, panels, out);
                },
            );
            pane_b_id = Some(pane_b.id());
        },
    );

    // The fraction cell repaints the split container on a drag (the seam moves);
    // the reconcile section additionally rewrites the pane weights (LAYOUT).
    cx.bind(cell_fraction, container, DirtyClass::PAINT);

    // Record the seam now that the pane ids are known. Both panes always build
    // (the closure runs synchronously), so the ids are present.
    let seam = SeamRec {
        container: container.id(),
        pane_a: pane_a_id.expect("pane A built"),
        pane_b: pane_b_id.expect("pane B built"),
        axis,
        fraction: cell_fraction,
        down_pos: cell_down,
        start_frac: cell_start,
    };

    // Wire the seam's pointer and key drag handlers to its bar (built above, so its
    // handle is in scope). The handlers write only the drag cells; the reconcile
    // step turns the committed fraction into live pane geometry. The bar always
    // builds (the closure ran synchronously), so its handle is present.
    super::drag::wire_seam(cx, bar_handle.expect("bar built"), &seam);

    out.seams.push(seam);

    container
}

/// Author a tab group: a column of a tab strip (shown when `strip`) over the
/// selected panel's content. Panels build once; the unselected ones are hidden at
/// build time (folded out of layout and paint) and revealed by a later section's
/// deferred `hidden` flip — the reference tabs discipline, keyed here by panel.
#[allow(clippy::too_many_arguments)]
fn build_tabs(
    cx: &mut BuildCx<'_>,
    keys: &[PanelKey],
    selected: usize,
    strip: bool,
    style: &DockStyle,
    contents: &HashMap<PanelKey, PanelContent>,
    panels: &PanelNodes,
    out: &mut BuildOut,
) -> viso_ui::Handle {
    let selected = if keys.is_empty() {
        0
    } else {
        selected.min(keys.len() - 1)
    };
    // The zone's representative redock target is the group's first panel; the
    // zone record is pushed after the container builds (its node id is known then).
    let target = keys.first().copied().unwrap_or(PanelKey(0));

    let show_strip = strip && keys.len() > 1;

    // Each tab leaf, paired with its panel key, so the drag-to-redock handlers can
    // be wired after the zone container's node id is known (a panel drag reports
    // its destination against the group container, built below).
    let mut tab_handles: Vec<(PanelKey, viso_ui::Handle)> = Vec::new();

    let container = cx.flex(
        FlexStyle {
            axis: viso_ui::Axis::Column,
            gap: if show_strip { 4.0 } else { 0.0 },
            padding: Inset::all(0.0),
            align: Align::Stretch,
            justify: Justify::Start,
            size: Size::fill(),
            style: BoxStyle::NONE,
        },
        |cx| {
            if show_strip {
                // A tab strip labelled as a `TabList`; each tab a named `Tab`. The
                // selection/switch wiring is section 3 (imperative) — section 1
                // authors the strip shape and roles so the a11y snapshot is stable.
                let strip_handle = cx.flex(
                    FlexStyle {
                        axis: viso_ui::Axis::Row,
                        gap: 4.0,
                        padding: Inset::all(0.0),
                        align: Align::Center,
                        justify: Justify::Start,
                        size: Size {
                            width: Length::fill(),
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                    },
                    |cx| {
                        for &key in keys {
                            let tab = cx.leaf(LeafStyle {
                                size: Size {
                                    width: Length::Fit,
                                    height: Length::Fit,
                                },
                                style: style.tab,
                            });
                            cx.focusable(tab, true);
                            cx.semantics(tab, semantics::tab(contents, key));
                            tab_handles.push((key, tab));
                        }
                    },
                );
                cx.semantics(strip_handle, semantics::strip());
            }

            // The panel area: every panel builds once; only the selected shows.
            cx.flex(
                FlexStyle {
                    axis: viso_ui::Axis::Column,
                    gap: 0.0,
                    padding: Inset::all(0.0),
                    align: Align::Stretch,
                    justify: Justify::Start,
                    size: Size::fill(),
                    style: BoxStyle::NONE,
                },
                |cx| {
                    let mut map = panels.borrow_mut();
                    for (i, &key) in keys.iter().enumerate() {
                        let panel = cx.flex(
                            FlexStyle {
                                axis: viso_ui::Axis::Column,
                                gap: 0.0,
                                padding: Inset::all(0.0),
                                align: Align::Stretch,
                                justify: Justify::Start,
                                size: Size::fill(),
                                style: BoxStyle::NONE,
                            },
                            |cx| {
                                if let Some(content) = contents.get(&key) {
                                    content(cx);
                                }
                            },
                        );
                        cx.set_hidden(panel, i != selected);
                        cx.semantics(panel, semantics::panel(contents, key));
                        map.insert(key, panel.id());
                    }
                },
            );
        },
    );

    // A single lone panel is a mute container; a multi-panel group is a named
    // region an assistive technology can navigate to. The role-mapping policy
    // lives in the `semantics` module (the a11y contract in one file).
    cx.semantics(
        container,
        semantics::tab_group(contents, keys.first().copied(), show_strip),
    );

    out.zones.push(ZoneRec {
        node: container.id(),
        target,
        rect: viso_ui::Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
    });

    // Wire drag-to-redock onto each tab leaf now that the group container exists:
    // dragging a tab records a redock intent against the drop zone the pointer ends
    // over, resolved by the drag section from the warm zone rects. The strip shows
    // no tabs when a group is lone, so a lone panel has nothing to grab — its redock
    // path is a future section's floating-panel chrome.
    for (key, tab) in tab_handles {
        super::drag::wire_panel_drag(cx, tab, key, out.hint, &out.intents, &out.zones_shared);
    }

    container
}

/// A [`Size`] whose main-axis length is `main` and cross-axis length is `cross`
/// for the given split `axis` (the reference splitter's `size_on` helper: `Size`
/// stores width/height, not main/cross).
fn size_on(axis: viso_ui::Axis, main: Length, cross: Length) -> Size {
    match axis {
        viso_ui::Axis::Row => Size {
            width: main,
            height: cross,
        },
        viso_ui::Axis::Column => Size {
            width: cross,
            height: main,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::dock::tree::{DockNode, DockTree};
    use crate::controls::dock::{Dock, dock};
    use viso_ui::{
        Axis, BindingTable, Component, NodeStore, Role, SemanticProjector, StateStore, TextEdits,
        VirtualLists,
    };

    /// The reactive stores a dock build writes into, kept together so a test can
    /// build a dock and then inspect the retained subtree it authored. Mirrors the
    /// splitter/tabs test harness.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        text_edits: TextEdits,
        projectors: SemanticProjector,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
                text_edits: TextEdits::new(),
                projectors: SemanticProjector::new(),
            }
        }

        /// Build a dock through a reactive cx (a dock authors seam state, so a plain
        /// `BuildCx::new` would panic) and return its root node — the container
        /// region the `Component::build` authors.
        fn build(&mut self, d: Dock) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            d.build(&mut cx);
            cx.root().expect("dock declares a root node")
        }
    }

    /// Collect a node's direct children by walking the arena sibling chain.
    fn children(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
        let arena = store.arena();
        let mut out = Vec::new();
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        out
    }

    /// Two panel keys, and a dock over a single row split of two lone panels, each
    /// panel registering a fixed leaf so the panel subtree is non-empty.
    fn two_panel_dock() -> (PanelKey, PanelKey, Dock) {
        let left = PanelKey(0);
        let right = PanelKey(1);
        let d = dock(DockTree::new(DockNode::split(
            Axis::Row,
            0.25,
            DockNode::panel(left),
            DockNode::panel(right),
        )))
        .panel(left, |cx| {
            cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                style: BoxStyle::NONE,
            });
        })
        .panel(right, |cx| {
            cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                style: BoxStyle::NONE,
            });
        });
        (left, right, d)
    }

    /// The build authors a `Region` container over a split: a seam flex of pane A,
    /// the bar, and pane B. The bar is focusable and named `resize`; each pane holds
    /// a lone-panel group. One `SeamRec` and two `ZoneRec`s are recorded.
    #[test]
    fn dock_builds_a_region_over_a_split_of_two_panes_and_a_bar() {
        let mut rx = Reactive::new();
        let (_left, _right, d) = two_panel_dock();

        // Capture the walk's side-records by rebuilding the tree directly, so the
        // test sees the same `out` the `Component::build` fills internally.
        let (seams, zones) = {
            let panels: PanelNodes = Rc::new(RefCell::new(HashMap::new()));
            let tree = DockTree::new(DockNode::split(
                Axis::Row,
                0.25,
                DockNode::panel(PanelKey(0)),
                DockNode::panel(PanelKey(1)),
            ));
            let contents: HashMap<PanelKey, PanelContent> = HashMap::new();
            let style = DockStyle::default();
            let mut cx = BuildCx::with_reactive(
                &mut rx.store,
                &mut rx.states,
                &mut rx.bindings,
                &mut rx.lists,
                &mut rx.text_edits,
                &mut rx.projectors,
            );
            let mut out = BuildOut::empty(&mut cx);
            build_tree(&mut cx, &tree.root, &style, &contents, &panels, &mut out);
            (out.seams, out.zones)
        };
        assert_eq!(seams.len(), 1, "one split authors one seam");
        assert_eq!(zones.len(), 2, "each lone panel is one drop zone");
        // The seam names the split container and its two panes distinctly.
        let seam = seams[0];
        assert_eq!(seam.axis, Axis::Row);
        assert_ne!(seam.pane_a, seam.pane_b, "the two panes are distinct nodes");

        // Now build the whole dock and inspect the composed subtree.
        let mut rx = Reactive::new();
        let root = rx.build(d);
        assert_eq!(
            rx.store.semantics(root).expect("root semantics").role,
            Role::Region,
            "the dock container is a named landmark region",
        );

        // The region holds one child: the split container.
        let region_children = children(&rx.store, root);
        assert_eq!(region_children.len(), 1, "the region wraps the split");
        let split = region_children[0];

        // The split holds three children in order: pane A, the bar, pane B.
        let split_children = children(&rx.store, split);
        assert_eq!(
            split_children.len(),
            3,
            "a split composes pane A, the bar, and pane B",
        );
        let (_pane_a, bar, _pane_b) = (split_children[0], split_children[1], split_children[2]);
        assert!(rx.store.focusable(bar), "the seam bar is focusable");
        let bar_sem = rx.store.semantics(bar).expect("bar semantics");
        assert_eq!(bar_sem.role, Role::Group, "the seam is a Group");
        assert_eq!(
            bar_sem.label.as_deref(),
            Some("resize"),
            "the seam names its keyboard-resize equivalent",
        );
    }

    /// Each panel builds exactly once, into a node recorded in the keyed panel map;
    /// distinct keys map to distinct nodes. Both lone panels are shown (a lone tab
    /// group hides its strip and shows its single panel).
    #[test]
    fn panels_build_once_into_the_keyed_map() {
        let mut rx = Reactive::new();
        let (left, right, d) = two_panel_dock();

        // Rebuild capturing the panel map (Component::build drops it internally).
        let panels: PanelNodes = Rc::new(RefCell::new(HashMap::new()));
        {
            let mut cx = BuildCx::with_reactive(
                &mut rx.store,
                &mut rx.states,
                &mut rx.bindings,
                &mut rx.lists,
                &mut rx.text_edits,
                &mut rx.projectors,
            );
            let mut out = BuildOut::empty(&mut cx);
            let contents = {
                let mut c: HashMap<PanelKey, PanelContent> = HashMap::new();
                c.insert(left, Box::new(|_cx: &mut BuildCx<'_>| {}));
                c.insert(right, Box::new(|_cx: &mut BuildCx<'_>| {}));
                c
            };
            let tree = DockTree::new(DockNode::split(
                Axis::Row,
                0.25,
                DockNode::panel(left),
                DockNode::panel(right),
            ));
            build_tree(
                &mut cx,
                &tree.root,
                &DockStyle::default(),
                &contents,
                &panels,
                &mut out,
            );
        }
        let map = panels.borrow();
        assert_eq!(map.len(), 2, "one node per registered panel key");
        assert!(map.contains_key(&left) && map.contains_key(&right));
        assert_ne!(
            map[&left], map[&right],
            "distinct keys map to distinct panel nodes",
        );
        drop(map);

        // Building the whole dock succeeds and exposes the same two-key shape (a
        // lone panel reveals its single panel; selection-driven hiding is exercised
        // by the multi-panel tab-group test).
        let mut rx2 = Reactive::new();
        let root = rx2.build(d);
        assert!(
            !children(&rx2.store, root).is_empty(),
            "the dock builds a body"
        );
    }

    /// A multi-panel tab group authors a `TabList` strip of `Tab`s over the panel
    /// area, hides the unselected panels, and names the group a `Region`.
    #[test]
    fn a_tab_group_authors_a_named_tablist_and_hides_unselected_panels() {
        let a = PanelKey(0);
        let b = PanelKey(1);
        let c = PanelKey(2);
        let d = dock(DockTree::new(DockNode::Tabs {
            panels: vec![a, b, c],
            selected: 1,
            strip: true,
        }))
        .panel(a, |cx| {
            cx.leaf(LeafStyle {
                size: Size::fixed(1.0, 1.0),
                style: BoxStyle::NONE,
            });
        })
        .panel(b, |cx| {
            cx.leaf(LeafStyle {
                size: Size::fixed(1.0, 1.0),
                style: BoxStyle::NONE,
            });
        })
        .panel(c, |cx| {
            cx.leaf(LeafStyle {
                size: Size::fixed(1.0, 1.0),
                style: BoxStyle::NONE,
            });
        });

        let mut rx = Reactive::new();
        let root = rx.build(d);
        // The region wraps the tab-group container.
        let group = children(&rx.store, root)[0];
        assert_eq!(
            rx.store.semantics(group).expect("group semantics").role,
            Role::Region,
            "a multi-panel tab group is a named region",
        );

        // The group is a column of the strip over the panel area.
        let group_children = children(&rx.store, group);
        assert_eq!(group_children.len(), 2, "a strip over a panel area");
        let (strip, area) = (group_children[0], group_children[1]);
        assert_eq!(
            rx.store.semantics(strip).expect("strip semantics").role,
            Role::TabList,
            "the strip is a TabList",
        );
        let tabs = children(&rx.store, strip);
        assert_eq!(tabs.len(), 3, "one tab per panel");
        for tab in &tabs {
            assert_eq!(
                rx.store.semantics(*tab).expect("tab semantics").role,
                Role::Tab,
            );
            assert!(rx.store.focusable(*tab), "each tab is focusable");
        }

        let panes = children(&rx.store, area);
        assert_eq!(panes.len(), 3, "every panel builds once");
        assert!(rx.store.hidden(panes[0]), "panel 0 is unselected → hidden");
        assert!(!rx.store.hidden(panes[1]), "panel 1 is selected → shown");
        assert!(rx.store.hidden(panes[2]), "panel 2 is unselected → hidden");
    }

    /// The a11y snapshot: every role and label the dock authors, pinned. A split of
    /// two lone panels reads as Region → (pane group, seam group, pane group), each
    /// pane a `Group` named by its panel.
    #[test]
    fn a11y_snapshot_pins_roles_and_labels() {
        let mut rx = Reactive::new();
        let (_left, _right, d) = two_panel_dock();
        let root = rx.build(d);

        // Root: a named landmark region (unnamed; panels carry the names).
        let root_sem = rx.store.semantics(root).expect("root semantics");
        assert_eq!(root_sem.role, Role::Region);
        assert_eq!(root_sem.label, None);

        let split = children(&rx.store, root)[0];
        let sc = children(&rx.store, split);
        let (pane_a, bar, pane_b) = (sc[0], sc[1], sc[2]);

        // The seam: a Group named `resize`.
        let bar_sem = rx.store.semantics(bar).expect("bar semantics");
        assert_eq!(bar_sem.role, Role::Group);
        assert_eq!(bar_sem.label.as_deref(), Some("resize"));

        // Each pane container holds one lone-panel tab group, dropped to a Group
        // named by its single panel.
        let group_a = children(&rx.store, pane_a)[0];
        let ga = rx.store.semantics(group_a).expect("group A semantics");
        assert_eq!(ga.role, Role::Group);
        assert_eq!(ga.label.as_deref(), Some("Panel 0"));

        let group_b = children(&rx.store, pane_b)[0];
        let gb = rx.store.semantics(group_b).expect("group B semantics");
        assert_eq!(gb.role, Role::Group);
        assert_eq!(gb.label.as_deref(), Some("Panel 1"));
    }
}
