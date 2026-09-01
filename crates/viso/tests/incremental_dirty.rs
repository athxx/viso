//! Incremental recompute: marking a node dirty must recompute only the affected
//! subtree, and a paint-only change must never make an ancestor layout dirty.
//!
//! These drive `NodeStore` directly (the reactive write path that will feed
//! `mark_dirty` lands in a later slice), asserting the layered-propagation and
//! incremental-recompute contract with the `FrameRecompute` counters rather than
//! pixels — the pipeline's pixel correctness is covered by `headless_scene`.

use viso::render::{Rect, Rgba};
use viso::ui::{
    Align, Axis, BoxStyle, BuildCx, DirtyClass, FlexStyle, Inset, LeafStyle, NodeId, NodeStore,
    Size,
};

const W: f32 = 200.0;
const H: f32 = 120.0;

const FILL: Rgba = Rgba {
    r: 0.5,
    g: 0.5,
    b: 0.5,
    a: 1.0,
};

/// The surface the tree lays out against.
fn surface() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W,
        h: H,
    }
}

/// A fill-sized Row container so it is *not* fixed-on-both-axes — a rising
/// MEASURE invalidation from a child passes through it rather than stopping.
fn flex_style() -> FlexStyle {
    FlexStyle {
        axis: Axis::Row,
        gap: 8.0,
        padding: Inset::all(12.0),
        align: Align::Center,
        size: Size::fill(),
        style: BoxStyle::solid(FILL),
    }
}

fn leaf(w: f32, h: f32) -> LeafStyle {
    LeafStyle {
        size: Size::fixed(w, h),
        style: BoxStyle::solid(FILL),
    }
}

/// Build a two-branch tree and hand back the ids that matter:
///
/// ```text
/// root (Row, fill)
/// ├── left (Row, fill)
/// │   ├── a (leaf)
/// │   └── b (leaf)
/// └── right (Row, fill)
///     └── c (leaf)
/// ```
struct Tree {
    root: NodeId,
    left: NodeId,
    a: NodeId,
    b: NodeId,
    right: NodeId,
    c: NodeId,
}

fn build_tree(store: &mut NodeStore) -> Tree {
    let mut left = None;
    let mut right = None;
    let mut a = None;
    let mut b = None;
    let mut c = None;

    let mut cx = BuildCx::new(store);
    let root = cx.flex(flex_style(), |cx| {
        left = Some(cx.flex(flex_style(), |cx| {
            a = Some(cx.leaf(leaf(40.0, 30.0)).id());
            b = Some(cx.leaf(leaf(40.0, 30.0)).id());
        }));
        right = Some(cx.flex(flex_style(), |cx| {
            c = Some(cx.leaf(leaf(40.0, 30.0)).id());
        }));
    });

    Tree {
        root: root.id(),
        left: left.unwrap().id(),
        a: a.unwrap(),
        b: b.unwrap(),
        right: right.unwrap().id(),
        c: c.unwrap(),
    }
}

/// Lay out the whole tree once (the "already-painted steady state") and clear
/// dirt, so a subsequent mark measures only that mark's incremental work.
fn settle(store: &mut NodeStore, root: NodeId) {
    let mut scratch = Vec::new();
    store.layout(root, surface(), &mut scratch);
    store.clear_dirty();
}

#[test]
fn clean_frame_recomputes_nothing() {
    let mut store = NodeStore::new();
    let t = build_tree(&mut store);
    settle(&mut store, t.root);

    let mut scratch = Vec::new();
    let mut redo = Vec::new();
    let (measured, laid_out) = store.relayout_dirty(t.root, surface(), &mut scratch, &mut redo);
    let mut prims = Vec::new();
    let painted = store.repaint_dirty(t.root, &mut prims);

    assert_eq!(measured, 0, "idle frame measures nothing");
    assert_eq!(laid_out, 0, "idle frame lays out nothing");
    assert_eq!(painted, 0, "idle frame repaints nothing");
}

#[test]
fn layout_dirt_recomputes_only_that_subtree() {
    let mut store = NodeStore::new();
    let t = build_tree(&mut store);
    settle(&mut store, t.root);

    // A LAYOUT-only mark on `left` re-places `left` and its two leaves (3 nodes)
    // and nothing on the `right` branch.
    store.mark_dirty(t.left, DirtyClass::LAYOUT);

    let mut scratch = Vec::new();
    let mut redo = Vec::new();
    let (_measured, laid_out) = store.relayout_dirty(t.root, surface(), &mut scratch, &mut redo);

    assert_eq!(
        laid_out, 3,
        "only the left subtree (left + a + b) re-placed"
    );

    // A LAYOUT mark on the container does not set its children's own dirty bits —
    // they are re-placed by the top-down walk, not by carrying LAYOUT themselves.
    assert!(
        store.dirty(t.a).is_empty(),
        "child a not independently dirtied"
    );
    assert!(
        store.dirty(t.b).is_empty(),
        "child b not independently dirtied"
    );
    // `right` and `c` carry no dirt at all.
    assert!(store.dirty(t.right).is_empty(), "right branch untouched");
    assert!(store.dirty(t.c).is_empty(), "right leaf untouched");
}

#[test]
fn paint_only_never_dirties_ancestor_layout() {
    let mut store = NodeStore::new();
    let t = build_tree(&mut store);
    settle(&mut store, t.root);

    // A pure PAINT change on a deep leaf.
    store.mark_dirty(t.a, DirtyClass::PAINT);

    // The leaf itself carries PAINT.
    assert!(store.dirty(t.a).contains(DirtyClass::PAINT));
    // No ancestor may carry PAINT, LAYOUT, or MEASURE — paint is strictly local.
    for ancestor in [t.left, t.root] {
        let d = store.dirty(ancestor);
        assert!(
            !d.intersects(DirtyClass::PAINT | DirtyClass::LAYOUT | DirtyClass::MEASURE),
            "paint-only leaf must not bubble to ancestor {ancestor:?}, got {d:?}"
        );
    }

    // The frame repaints (paint is pending) but re-places nothing.
    let mut scratch = Vec::new();
    let mut redo = Vec::new();
    let (measured, laid_out) = store.relayout_dirty(t.root, surface(), &mut scratch, &mut redo);
    assert_eq!(measured, 0, "paint-only frame measures nothing");
    assert_eq!(laid_out, 0, "paint-only frame lays out nothing");

    let mut prims = Vec::new();
    let painted = store.repaint_dirty(t.root, &mut prims);
    assert!(painted > 0, "paint-only frame still rebuilds primitives");
}

#[test]
fn measure_dirt_bubbles_through_flexible_ancestor() {
    let mut store = NodeStore::new();
    let t = build_tree(&mut store);
    settle(&mut store, t.root);

    // A MEASURE change on leaf `a` can change `a`'s natural size, so it must rise
    // through the fill-sized `left` (whose size depends on its content) up toward
    // the root — none of these ancestors are fixed-on-both-axes.
    store.mark_dirty(t.a, DirtyClass::MEASURE);

    assert!(store.dirty(t.a).contains(DirtyClass::MEASURE));
    assert!(
        store.dirty(t.left).contains(DirtyClass::MEASURE),
        "MEASURE rises through the flexible left container"
    );
    assert!(
        store.dirty(t.root).contains(DirtyClass::MEASURE),
        "MEASURE rises to the root"
    );
    // The sibling branch is unaffected.
    assert!(
        store.dirty(t.right).is_empty(),
        "MEASURE does not cross into the sibling branch"
    );

    // Relayout redoes from the root (a's parent chain rose to it), which covers
    // the whole 6-node tree once.
    let mut scratch = Vec::new();
    let mut redo = Vec::new();
    let (_measured, laid_out) = store.relayout_dirty(t.root, surface(), &mut scratch, &mut redo);
    assert_eq!(
        laid_out, 6,
        "the whole tree re-placed once, no double count"
    );
}

#[test]
fn measure_dirt_stops_below_fixed_ancestor() {
    // A leaf inside a fixed-on-both-axes container: that container's box cannot
    // change when the leaf's natural size does, so it is the boundary — MEASURE
    // rises no further and the container itself carries no MEASURE (nothing above
    // the boundary needs to re-measure). The leaf still re-measures locally.
    let mut store = NodeStore::new();

    let mut inner_leaf = None;
    let mut cx = BuildCx::new(&mut store);
    let root = cx.flex(flex_style(), |cx| {
        // A fixed-on-both-axes container.
        cx.flex(
            FlexStyle {
                size: Size::fixed(80.0, 60.0),
                ..flex_style()
            },
            |cx| {
                inner_leaf = Some(cx.leaf(leaf(40.0, 30.0)).id());
            },
        );
    });
    let root = root.id();
    drop(cx);
    let inner_leaf = inner_leaf.unwrap();
    // The fixed container is `inner_leaf`'s parent.
    let fixed = store.arena().links(inner_leaf).unwrap().parent.unwrap();

    settle(&mut store, root);

    store.mark_dirty(inner_leaf, DirtyClass::MEASURE);

    assert!(
        store.dirty(inner_leaf).contains(DirtyClass::MEASURE),
        "the leaf itself still re-measures"
    );
    assert!(
        !store.dirty(fixed).intersects(DirtyClass::MEASURE),
        "the fixed container's box is content-independent — it does not re-measure"
    );
    assert!(
        !store.dirty(root).intersects(DirtyClass::MEASURE),
        "MEASURE never rises past the fixed boundary to the root"
    );
}
