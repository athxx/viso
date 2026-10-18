//! `list_100k_scroll`: one scroll frame of a 100k-row virtual list, from the
//! offset write to the renderer taking the moved displacements (§21, ADR 0043).
//!
//! A within-row scroll mounts nothing, so the frame is: reconcile (a no-op),
//! relayout (nothing dirty), recompute the transform roots, find nothing to
//! repaint, and move the retained draws. Its cost must not depend on how many
//! rows are on screen: the bench runs the same frame with ~30 and ~160 mounted
//! rows, and a startup check asserts the frame repaints no primitive and
//! uploads nothing.
//!
//! Run release (`cargo bench -p viso --bench list_scroll`).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle};
use viso_render::{Primitive, Rect, Renderer, Rgba, TransformUpdate};
use viso_ui::{
    Axis, BindingTable, BoxStyle, BuildCx, DirtyClass, EffectStore, Length, NodeId, NodeStore,
    SemanticProjector, Size, StateStore, TextEdits, Vec2, VirtualListStyle, VirtualLists,
    virtual_list,
};

const VIEWPORT_H: f32 = 600.0;
const ITEM_COUNT: usize = 100_000;
const OVERSCAN: u32 = 6;

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
    primitives: Vec<Primitive>,
    gpu: CpuRaster,
    renderer: Renderer,
}

/// A vertical list of rows `row_h` tall, each a solid leaf, mounted, painted
/// and uploaded once.
fn setup(row_h: f32) -> Harness {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let viewport = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        cx.virtual_list(
            VirtualListStyle {
                axis: Axis::Column,
                size: Size {
                    width: Length::Fixed(300.0),
                    height: Length::Fixed(VIEWPORT_H),
                },
                overscan: OVERSCAN,
                estimated_row: row_h,
                style: BoxStyle::NONE,
            },
            ITEM_COUNT,
            move |_i, cx| {
                cx.leaf(viso_ui::LeafStyle {
                    size: Size {
                        width: Length::fill(),
                        height: Length::Fixed(row_h - 1.0),
                    },
                    style: BoxStyle::solid(Rgba::new(0.2, 0.4, 0.8, 1.0)),
                });
            },
        )
        .id()
    };
    store.mark_dirty(
        viewport,
        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
    );
    let mut gpu = CpuRaster::new();
    let surface_id = gpu.create_surface(RawWindowHandle::Headless, 300, VIEWPORT_H as u32);
    let format = gpu.surface_format(surface_id);
    let renderer = Renderer::new(&mut gpu, format);
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
        primitives: Vec::new(),
        gpu,
        renderer,
    };
    h.store.layout(h.viewport, h.surface, &mut h.scratch);
    // Mount, then settle the measured rows, painting and uploading each frame.
    for _ in 0..3 {
        frame(&mut h);
    }
    h
}

/// One frame, Layout through UploadGpuChanges. Returns how many primitives it
/// painted and whether a transform root moved.
fn frame(h: &mut Harness) -> (u32, bool) {
    virtual_list::reconcile(
        &mut h.store,
        &mut h.lists,
        &mut h.states,
        &mut h.bindings,
        &mut h.effects,
    );
    h.store
        .relayout_dirty(h.viewport, h.surface, &mut h.scratch, &mut h.redo);
    virtual_list::absorb_measurements(&h.store, &mut h.lists);
    let moved = h.store.any_dirty_class(DirtyClass::TRANSFORM) && h.store.update_transforms();
    let painted = h.store.repaint_dirty(h.viewport, &mut h.primitives);
    if painted > 0 {
        h.renderer.upload_with_transforms(
            &mut h.gpu,
            &h.primitives,
            h.store.transforms().painted(),
        );
    }
    let update = h.renderer.set_transforms(h.store.transforms().current());
    assert_ne!(update, TransformUpdate::NeedsUpload);
    h.store.clear_dirty();
    (painted, moved)
}

/// The benched scroll — down and back up by one device pixel, so the roots
/// move and neither edge of the viewport crosses a row — mounts nothing and
/// repaints nothing.
fn assert_a_scroll_frame_paints_nothing(row_h: f32) -> usize {
    let mut h = setup(row_h);
    let mut dy = -1.0;
    for i in 0..8 {
        dy = -dy;
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: dy });
        assert_eq!(
            frame(&mut h),
            (0, true),
            "frame {i}: a scroll repainted or moved nothing"
        );
    }
    h.lists.get(h.viewport).unwrap().mounted_count()
}

fn bench_list_scroll(c: &mut Criterion) {
    let few = assert_a_scroll_frame_paints_nothing(20.0);
    let many = assert_a_scroll_frame_paints_nothing(4.0);
    assert!(
        many > 4 * few,
        "{many} rows vs {few}: the two lists differ in size"
    );

    for (name, row_h) in [
        ("list_100k_scroll/rows_30", 20.0),
        ("list_100k_scroll/rows_160", 4.0),
    ] {
        c.bench_function(name, |b| {
            let mut h = setup(row_h);
            let mut dy = -1.0;
            b.iter(|| {
                // Down and back up a pixel: the roots move, nothing mounts.
                dy = -dy;
                h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: dy });
                black_box(frame(black_box(&mut h)));
            });
        });
    }
}

criterion_group!(benches, bench_list_scroll);
criterion_main!(benches);
