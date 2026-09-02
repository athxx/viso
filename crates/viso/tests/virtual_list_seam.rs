//! The virtual-list frame seam: a list built through the facade's `BuildCx`,
//! driven by the same reconcile → relayout → absorb sequence the driver's
//! `FramePhase::Layout` arm runs — but headless and display-free.
//!
//! Like the other facade tests this drives the `ui` stores directly (reachable
//! through `viso::ui`) instead of standing up a scheduler and a window. It
//! authors a 100k-row list with a tiny viewport, then folds the per-frame
//! reconcile over the retained tree exactly as the driver does, asserting the
//! virtualization contract end-to-end: mount ~a window's worth of nodes (not
//! 100k), keep the scroll range at the full logical extent, and stay on the
//! zero-relayout steady path for a scroll that does not cross a row boundary.

use viso::prelude::*;
use viso::render::Rect;
use viso::ui::{
    Axis, BindingTable, BoxStyle, EffectStore, Length, NodeStore, Size, StateStore, Vec2,
    VirtualListStyle, VirtualLists, virtual_list,
};

const VIEWPORT_H: f32 = 300.0;
const ROW_H: f32 = 30.0;
const ITEM_COUNT: usize = 100_000;
const OVERSCAN: u32 = 4;

/// The driver's frame-phase inputs for a virtual list: the node store, the three
/// sibling reactive stores, the list registry, the viewport id, and the reusable
/// layout scratch — the exact set `AppDriver` threads together each frame.
struct Seam {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    viewport: NodeId,
    surface: Rect,
    scratch: Vec<u32>,
    redo: Vec<NodeId>,
}

impl Seam {
    /// Author a vertical 100k-row list through the facade `BuildCx`, each row a
    /// single fixed-height leaf. Mirrors how an app's `build` declares a list.
    fn build() -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let viewport = {
            let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
            cx.virtual_list(
                VirtualListStyle {
                    axis: Axis::Column,
                    size: Size {
                        width: Length::Fixed(200.0),
                        height: Length::Fixed(VIEWPORT_H),
                    },
                    overscan: OVERSCAN,
                    estimated_row: ROW_H,
                    style: BoxStyle::NONE,
                },
                ITEM_COUNT,
                move |_i, cx| {
                    cx.leaf(LeafStyle {
                        size: Size {
                            width: Length::fill(),
                            height: Length::Fixed(ROW_H),
                        },
                        ..Default::default()
                    });
                },
            )
            .id()
        };
        // The launch path seeds the whole tree dirty so the first layout resolves
        // every box; do the same here.
        store.mark_dirty(
            viewport,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
        Seam {
            store,
            states,
            bindings,
            effects: EffectStore::new(),
            lists,
            viewport,
            surface: Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: VIEWPORT_H,
            },
            scratch: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// One frame's `Layout` phase, exactly as `AppDriver` runs it: reconcile the
    /// lists, incrementally relayout the invalidated subtrees, absorb the freshly
    /// measured row heights. Returns the number of rows (re)bound this frame.
    fn frame(&mut self) -> u32 {
        // The first frame needs a full layout to place the viewport before
        // reconcile can read its size (the driver's first frame runs a full
        // layout for the same reason).
        if self.store.bounds_main(self.viewport, Axis::Column) <= 0.0 {
            self.store
                .layout(self.viewport, self.surface, &mut self.scratch);
        }
        let bound = virtual_list::reconcile(
            &mut self.store,
            &mut self.lists,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
        );
        self.store.relayout_dirty(
            self.viewport,
            self.surface,
            &mut self.scratch,
            &mut self.redo,
        );
        virtual_list::absorb_measurements(&self.store, &mut self.lists);
        self.store.clear_dirty();
        bound
    }
}

/// The core virtualization contract, driven over a facade-built tree: a 100k-row
/// list mounts only a window's worth of nodes yet reports the full scroll range.
#[test]
fn a_facade_built_list_mounts_a_window_not_the_whole_collection() {
    let mut seam = Seam::build();
    seam.frame();

    let mounted = seam.lists.get(seam.viewport).unwrap().mounted_count();
    // Visible span ≈ 10 rows, plus 2·overscan → ~18 mounted, never 100k.
    assert!(
        (10..=20).contains(&mounted),
        "mounted {mounted} should be ~visible+2·overscan, not {ITEM_COUNT}"
    );
    // Scroll range is the full logical extent minus the viewport, despite the
    // handful of mounted nodes.
    assert_eq!(
        seam.store.scroll_range(seam.viewport, Axis::Column),
        ITEM_COUNT as f32 * ROW_H - VIEWPORT_H
    );
}

/// A scroll that stays within the mounted window is a pure transform: the frame's
/// reconcile rebinds nothing (the scroll already moved the world rects), so the
/// list stays on the zero-relayout steady path.
#[test]
fn a_sub_window_scroll_stays_on_the_steady_path() {
    let mut seam = Seam::build();
    seam.frame(); // initial mount
    let mounted_before = seam.lists.get(seam.viewport).unwrap().mounted_count();

    // Scroll 10px — less than a row — the visible window is unchanged.
    seam.store
        .scroll_by(seam.viewport, Vec2 { x: 0.0, y: 10.0 });
    let bound = seam.frame();
    assert_eq!(bound, 0, "a sub-row scroll rebinds nothing");
    assert_eq!(
        seam.lists.get(seam.viewport).unwrap().mounted_count(),
        mounted_before,
        "the mounted window is untouched"
    );
}

/// Crossing a row boundary recycles a bounded handful: advancing the window by
/// three rows binds exactly three, with the mounted count stable — the recycle
/// contract, not a full remount.
#[test]
fn crossing_a_boundary_recycles_a_bounded_handful() {
    let mut seam = Seam::build();
    seam.frame();
    // Scroll deep into the middle so the window is overscan-bounded on both sides.
    seam.store.scroll_by(
        seam.viewport,
        Vec2 {
            x: 0.0,
            y: 30_000.0,
        },
    );
    seam.frame();
    let mounted_before = seam.lists.get(seam.viewport).unwrap().mounted_count();

    // Advance exactly three rows.
    seam.store
        .scroll_by(seam.viewport, Vec2 { x: 0.0, y: 90.0 });
    let bound = seam.frame();
    assert_eq!(bound, 3, "advancing by 3 rows binds exactly 3");
    assert_eq!(
        seam.lists.get(seam.viewport).unwrap().mounted_count(),
        mounted_before,
        "the mounted window size is stable across a boundary crossing"
    );
}
