//! The "large list" benchmark category: a virtualized list of 100k logical rows
//! driven through the public frame seam (reconcile → relayout → absorb), the same
//! sequence the facade runs each frame.
//!
//! Two things are measured, both through the public API (benches are an external
//! crate and cannot touch crate internals, so we drive whole frames via
//! `NodeStore` + the virtual-list reconcile):
//!
//! 1. A startup assertion that a warmed-up list, scrolled within its mounted
//!    window, stays on the zero-work steady path: each frame rebinds no rows and
//!    the reused scratch buffers do not grow. This runs once at startup so a
//!    hot-path regression fails the bench binary immediately (mirroring
//!    `render/benches/renderer_steady_state.rs`).
//! 2. The per-frame cost of a steady (within-row) reconcile and of a
//!    boundary-crossing reconcile that recycles a handful of hosts — baselines to
//!    catch regressions in the virtualization hot path.
//!
//! Run release (`cargo bench -p viso-ui`); criterion defaults to a release
//! profile. Debug timing is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_render::Rect;
use viso_ui::{
    Axis, BindingTable, BoxStyle, BuildCx, DirtyClass, EffectStore, Length, NodeId, NodeStore,
    Size, StateStore, Vec2, VirtualListStyle, VirtualLists, virtual_list,
};

const VIEWPORT_H: f32 = 600.0;
const ROW_H: f32 = 30.0;
const ITEM_COUNT: usize = 100_000;
const OVERSCAN: u32 = 6;

/// Everything a frame needs: the node store, the sibling reactive stores, the
/// list registry, the viewport id, and the reusable layout scratch.
struct Harness {
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

/// Author a vertical 100k-row list, each row a single fixed-height leaf, then run
/// the first (full-layout) frame so the harness is mounted and warm.
fn setup() -> Harness {
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
                    width: Length::Fixed(300.0),
                    height: Length::Fixed(VIEWPORT_H),
                },
                overscan: OVERSCAN,
                estimated_row: ROW_H,
                style: BoxStyle::NONE,
            },
            ITEM_COUNT,
            move |_i, cx| {
                cx.leaf(viso_ui::LeafStyle {
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
    store.mark_dirty(
        viewport,
        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
    );
    let mut h = Harness {
        store,
        states,
        bindings,
        effects: EffectStore::new(),
        lists,
        viewport,
        surface: Rect {
            x: 0.0,
            y: 0.0,
            w: 300.0,
            h: VIEWPORT_H,
        },
        scratch: Vec::new(),
        redo: Vec::new(),
    };
    // First frame: full layout to place the viewport, then the initial mount.
    h.store.layout(h.viewport, h.surface, &mut h.scratch);
    frame(&mut h);
    h
}

/// One frame's Layout phase: reconcile, incremental relayout, absorb. Returns the
/// number of rows (re)bound this frame.
fn frame(h: &mut Harness) -> u32 {
    let bound = virtual_list::reconcile(
        &mut h.store,
        &mut h.lists,
        &mut h.states,
        &mut h.bindings,
        &mut h.effects,
    );
    h.store
        .relayout_dirty(h.viewport, h.surface, &mut h.scratch, &mut h.redo);
    virtual_list::absorb_measurements(&h.store, &mut h.lists);
    h.store.clear_dirty();
    bound
}

/// The steady-path invariant, checked before benchmarking: a warmed-up list
/// scrolled within its mounted window rebinds no rows and grows no scratch.
fn assert_steady_state_is_allocation_free() {
    let mut h = setup();
    // A second frame fully warms the reconcile/layout scratch capacities.
    frame(&mut h);

    let scratch_cap = h.scratch.capacity();
    let redo_cap = h.redo.capacity();

    for i in 0..8 {
        // 1px each frame — always within the same row, always a no-op reconcile.
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 1.0 });
        let bound = frame(&mut h);
        assert_eq!(
            bound, 0,
            "frame {i}: a sub-row scroll rebound rows (steady path is not zero-work)"
        );
        assert_eq!(
            h.scratch.capacity(),
            scratch_cap,
            "frame {i}: the layout scratch grew on the steady path (hidden allocation)"
        );
        assert_eq!(
            h.redo.capacity(),
            redo_cap,
            "frame {i}: the redo-roots scratch grew on the steady path (hidden allocation)"
        );
    }

    // Sanity: the list actually mounted a window (not zero rows, not all 100k), so
    // the invariant is not trivially satisfied by an empty list.
    let mounted = h.lists.get(h.viewport).unwrap().mounted_count();
    assert!(
        (20..=40).contains(&mounted),
        "the list must mount ~a window's worth of rows, got {mounted}"
    );
}

fn bench_large_list(c: &mut Criterion) {
    assert_steady_state_is_allocation_free();

    // Steady reconcile: a within-row scroll, the zero-rebind hot path.
    c.bench_function("reconcile_steady_within_row", |b| {
        let mut h = setup();
        frame(&mut h);
        b.iter(|| {
            h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 1.0 });
            black_box(frame(black_box(&mut h)));
        });
    });

    // Boundary-crossing reconcile: advance exactly one row each iteration, so a
    // handful of hosts recycle every frame.
    c.bench_function("reconcile_crossing_one_row", |b| {
        let mut h = setup();
        // Scroll into the middle so the window is overscan-bounded on both sides.
        h.store.scroll_by(
            h.viewport,
            Vec2 {
                x: 0.0,
                y: 30_000.0,
            },
        );
        frame(&mut h);
        b.iter(|| {
            h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: ROW_H });
            black_box(frame(black_box(&mut h)));
        });
    });
}

criterion_group!(benches, bench_large_list);
criterion_main!(benches);
