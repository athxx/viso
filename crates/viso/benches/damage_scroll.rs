//! `damage_scroll_list_partial_present`: one scroll frame of a 100k-row
//! virtual list beside an empty pane, from the offset write to the present.
//!
//! The scroll moves the list's transform roots and repaints nothing; the
//! submit diffs what the surface pass draws against the last presented
//! frame, clears and redraws only the list's moved rows into the drawable it
//! kept, and presents that damage. The rest of the surface is not touched.
//!
//! Before timing it asserts the frame repaints no primitive and is presented
//! in part, and prints its damage and draws.
//!
//! Run release (`cargo bench -p viso --bench damage_scroll`).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, SurfaceId};
use viso_render::damage::Region;
use viso_render::{Primitive, Rect, Renderer, Rgba, TransformUpdate};
use viso_ui::{
    Axis, BindingTable, BoxStyle, BuildCx, DirtyClass, EffectStore, Length, NodeId, NodeStore,
    SemanticProjector, Sizing, StateStore, TextEdits, Vec2, VirtualListStyle, VirtualLists,
    virtual_list,
};

const W: f32 = 800.0;
const H: f32 = 600.0;
const LIST_W: f32 = 240.0;
const ROW_H: f32 = 20.0;
const CLEAR: [f32; 4] = [0.1, 0.1, 0.1, 1.0];

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
    target: SurfaceId,
    renderer: Renderer,
}

/// A list of rows `ROW_H` tall, each a card of three solid leaves, mounted,
/// painted and presented until it settles.
fn setup() -> Harness {
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
                size: Sizing {
                    width: Length::Fixed(LIST_W),
                    height: Length::Fixed(H),
                },
                overscan: 6,
                estimated_row: ROW_H,
                style: BoxStyle::NONE,
                ..Default::default()
            },
            100_000,
            move |i, cx| {
                let shade = (i % 7) as f32 / 10.0;
                cx.flex(
                    viso_ui::FlexStyle {
                        axis: Axis::Row,
                        gap: 4.0,
                        size: Sizing {
                            width: Length::Fixed(LIST_W),
                            height: Length::Fixed(ROW_H - 1.0),
                        },
                        style: BoxStyle::solid(Rgba::new(0.1, 0.1, 0.12 + shade, 1.0)),
                        ..Default::default()
                    },
                    |cx| {
                        for c in 0..3 {
                            cx.leaf(viso_ui::LeafStyle {
                                size: Sizing {
                                    width: Length::Fixed(40.0 + c as f32 * 20.0),
                                    height: Length::Fixed(ROW_H - 3.0),
                                },
                                style: BoxStyle::solid(Rgba::new(0.3, 0.5, 0.8, 1.0)),
                            });
                        }
                    },
                );
            },
        )
        .id()
    };
    store.mark_dirty(
        viewport,
        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
    );
    let mut gpu = CpuRaster::new();
    let target = gpu.create_surface(RawWindowHandle::Headless, W as u32, H as u32);
    let format = gpu.surface_format(target);
    let mut renderer = Renderer::new(&mut gpu, format);
    renderer.set_surface_size([W, H], 1.0);
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
            w: W,
            h: H,
        },
        scratch: Vec::new(),
        redo: Vec::new(),
        primitives: Vec::new(),
        gpu,
        target,
        renderer,
    };
    h.store.layout(h.viewport, h.surface, &mut h.scratch);
    for _ in 0..3 {
        frame(&mut h);
    }
    h
}

/// One frame, Layout through Present. Returns how many primitives it
/// painted.
fn frame(h: &mut Harness) -> u32 {
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
    if h.store.transforms_pending() {
        h.store.update_transforms();
    }
    let painted = h.store.repaint_dirty(h.viewport, &mut h.primitives).count();
    if painted > 0
        || h.renderer.set_transforms(h.store.transforms().places()) == TransformUpdate::NeedsUpload
    {
        h.renderer
            .upload_with_transforms(&mut h.gpu, &h.primitives, h.store.transforms().places());
    }
    h.store.clear_dirty();
    h.renderer.submit(&mut h.gpu, h.target, CLEAR, [W, H]);
    painted
}

fn bench_damage_scroll(c: &mut Criterion) {
    let mut h = setup();
    let mut dy = -1.0;
    // Down and back up a pixel: the roots move, nothing mounts.
    for i in 0..8 {
        dy = -dy;
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: dy });
        let before = h.renderer.damage_stats();
        assert_eq!(frame(&mut h), 0, "frame {i}: a scroll repainted");
        let after = h.renderer.damage_stats();
        assert_eq!(
            after.partial_frames,
            before.partial_frames + 1,
            "frame {i}: the scroll was not presented in part"
        );
    }
    let stats = h.renderer.damage_stats();
    let rects = match h.renderer.last_damage() {
        Region::Rects(rects) => rects.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>(),
        Region::Full => panic!("the scroll damaged the whole surface"),
        Region::Empty => panic!("the scroll damaged nothing"),
    };
    eprintln!(
        "damage_scroll_list_partial_present: {} rows mounted; a scroll frame redraws {} of {} px \
         in {} rects {} with {} draws",
        h.lists.get(h.viewport).unwrap().mounted_count(),
        stats.damage_px,
        (W * H) as u64,
        stats.damage_rects,
        rects.join(" "),
        h.renderer.frame_stats().draw_calls,
    );
    c.bench_function("damage_scroll_list_partial_present", |b| {
        b.iter(|| {
            dy = -dy;
            h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: dy });
            black_box(frame(black_box(&mut h)));
        });
    });
}

criterion_group!(benches, bench_damage_scroll);
criterion_main!(benches);
