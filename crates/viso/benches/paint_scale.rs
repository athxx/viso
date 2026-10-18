//! `paint_scale`: what a frame that changes a little costs as the tree grows
//! (Rendering §9.7).
//!
//! - `hover/cards_N`: one leaf of a page of `N` cards (a row with a
//!   background and three leaves each) changes color; the frame repaints and
//!   uploads. A frame's cost should follow what changed, not the page.
//! - `mount/rows_N`: a virtual list showing about `N` rows scrolls by exactly
//!   one row, so one row mounts and one unmounts.
//!
//! Each frame runs Layout through UploadGpuChanges, as the facade does, but
//! submits nothing: the raster cost is the damage's, measured elsewhere.
//!
//! Run release (`cargo bench -p viso --bench paint_scale`).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle};
use viso_render::{Primitive, Rect, Renderer, Rgba};
use viso_ui::{
    Axis, BindingTable, BoxStyle, BuildCx, DirtyClass, EffectStore, FlexStyle, LeafStyle, Length,
    NodeId, NodeStore, Painted, SemanticProjector, Size, StateStore, TextEdits, Vec2,
    VirtualListStyle, VirtualLists, virtual_list,
};

const W: f32 = 400.0;
const H: f32 = 600.0;
const CARD_H: f32 = 24.0;

struct Harness {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    root: NodeId,
    surface: Rect,
    scratch: Vec<u32>,
    redo: Vec<NodeId>,
    primitives: Vec<Primitive>,
    gpu: CpuRaster,
    renderer: Renderer,
}

fn harness(
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    lists: VirtualLists,
    root: NodeId,
) -> Harness {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W as u32, H as u32);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);
    let mut h = Harness {
        store,
        states,
        bindings,
        effects: EffectStore::new(),
        lists,
        root,
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
        renderer,
    };
    h.store.mark_dirty(
        root,
        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
    );
    h.store.layout(h.root, h.surface, &mut h.scratch);
    for _ in 0..3 {
        frame(&mut h);
    }
    h
}

/// The card's cells, by index across the page.
fn card(cx: &mut BuildCx<'_>, i: usize, cells: &mut Vec<NodeId>) {
    let shade = (i % 7) as f32 / 10.0;
    cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: 4.0,
            size: Size {
                width: Length::fill(),
                height: Length::Fixed(CARD_H),
            },
            style: BoxStyle::solid(Rgba::new(0.1, 0.1, 0.12 + shade, 1.0)),
            ..FlexStyle::default()
        },
        |cx| {
            for c in 0..3 {
                let leaf = cx.leaf(LeafStyle {
                    size: Size {
                        width: Length::Fixed(60.0 + c as f32 * 20.0),
                        height: Length::Fixed(CARD_H - 6.0),
                    },
                    style: BoxStyle::solid(Rgba::new(0.3, 0.5, 0.8, 1.0)),
                });
                cells.push(leaf.id());
            }
        },
    );
}

/// A page of `cards` cards in a column, laid out and painted once; with the
/// leaf the hover recolors (one in the first screenful).
fn page(cards: usize) -> (Harness, NodeId) {
    let mut store = NodeStore::new();
    let mut cells = Vec::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 2.0,
                size: Size {
                    width: Length::Fixed(W),
                    height: Length::Fixed(H),
                },
                ..FlexStyle::default()
            },
            |cx| {
                for i in 0..cards {
                    card(cx, i, &mut cells);
                }
            },
        )
        .id()
    };
    let target = cells[3 * 5 + 1];
    (
        harness(
            store,
            StateStore::new(),
            BindingTable::new(),
            VirtualLists::new(),
            root,
        ),
        target,
    )
}

/// A 100k-row virtual list of `row_h`-tall cards, mounted and settled.
fn list(row_h: f32) -> Harness {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let root = {
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
                    width: Length::Fixed(W),
                    height: Length::Fixed(H),
                },
                overscan: 2,
                estimated_row: row_h,
                style: BoxStyle::NONE,
            },
            100_000,
            move |i, cx| {
                let shade = (i % 7) as f32 / 10.0;
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: 4.0,
                        size: Size {
                            width: Length::fill(),
                            height: Length::Fixed(row_h - 1.0),
                        },
                        style: BoxStyle::solid(Rgba::new(0.1, 0.1, 0.12 + shade, 1.0)),
                        ..FlexStyle::default()
                    },
                    |cx| {
                        for c in 0..3 {
                            cx.leaf(LeafStyle {
                                size: Size {
                                    width: Length::Fixed(60.0 + c as f32 * 20.0),
                                    height: Length::Fixed((row_h - 3.0).max(1.0)),
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
    harness(store, states, bindings, lists, root)
}

/// One frame, Layout through UploadGpuChanges. Returns how many primitives
/// it painted.
fn frame(h: &mut Harness) -> u32 {
    virtual_list::reconcile(
        &mut h.store,
        &mut h.lists,
        &mut h.states,
        &mut h.bindings,
        &mut h.effects,
    );
    h.store
        .relayout_dirty(h.root, h.surface, &mut h.scratch, &mut h.redo);
    virtual_list::absorb_measurements(&h.store, &mut h.lists);
    if h.store.any_dirty_class(DirtyClass::TRANSFORM) {
        h.store.update_transforms();
    }
    let painted = h.store.repaint_dirty(h.root, &mut h.primitives);
    upload(h, painted);
    h.renderer.set_transforms(h.store.transforms().current());
    h.store.clear_dirty();
    painted.count()
}

/// The UploadGpuChanges phase as the facade runs it: a patch when only paint
/// changed and the renderer takes it, the stream whole otherwise.
fn upload(h: &mut Harness, painted: Painted) {
    let whole = match painted {
        Painted::Nothing | Painted::Patched(0) => false,
        Painted::Whole(_) => true,
        Painted::Patched(_) => !h
            .renderer
            .patch(&mut h.gpu, &h.primitives, h.store.patched()),
    };
    if whole {
        h.renderer.upload_with_transforms(
            &mut h.gpu,
            &h.primitives,
            h.store.transforms().painted(),
        );
    }
}

const COLORS: [Rgba; 2] = [Rgba::new(0.3, 0.5, 0.8, 1.0), Rgba::new(0.9, 0.6, 0.2, 1.0)];

fn bench_paint_scale(c: &mut Criterion) {
    for cards in [100, 2000] {
        let (mut h, target) = page(cards);
        let mut flip = 0;
        h.store.set_fill(target, COLORS[1]);
        eprintln!(
            "hover/cards_{cards}: {} nodes, the frame paints {} primitives",
            h.store.arena().live_count(),
            frame(&mut h)
        );
        c.bench_function(&format!("hover/cards_{cards}/paint"), |b| {
            b.iter_custom(|iters| {
                let mut spent = std::time::Duration::ZERO;
                for _ in 0..iters {
                    flip ^= 1;
                    h.store.set_fill(target, COLORS[flip]);
                    h.store
                        .relayout_dirty(h.root, h.surface, &mut h.scratch, &mut h.redo);
                    let start = std::time::Instant::now();
                    let painted = black_box(h.store.repaint_dirty(h.root, &mut h.primitives));
                    spent += start.elapsed();
                    upload(&mut h, painted);
                    h.store.clear_dirty();
                }
                spent
            });
        });
        c.bench_function(&format!("hover/cards_{cards}/upload"), |b| {
            b.iter_custom(|iters| {
                let mut spent = std::time::Duration::ZERO;
                for _ in 0..iters {
                    flip ^= 1;
                    h.store.set_fill(target, COLORS[flip]);
                    h.store
                        .relayout_dirty(h.root, h.surface, &mut h.scratch, &mut h.redo);
                    let painted = h.store.repaint_dirty(h.root, &mut h.primitives);
                    let start = std::time::Instant::now();
                    upload(&mut h, painted);
                    spent += start.elapsed();
                    h.store.clear_dirty();
                }
                spent
            });
        });
        c.bench_function(&format!("hover/cards_{cards}"), |b| {
            b.iter(|| {
                flip ^= 1;
                h.store.set_fill(target, COLORS[flip]);
                black_box(frame(black_box(&mut h)));
            });
        });
    }
    for (rows, row_h) in [(30, 20.0), (160, 4.0)] {
        let mut h = list(row_h);
        let mut dy = -row_h;
        dy = -dy;
        h.store.scroll_by(h.root, Vec2 { x: 0.0, y: dy });
        eprintln!(
            "mount/rows_{rows}: {} rows mounted, the frame paints {} primitives",
            h.lists.get(h.root).unwrap().mounted_count(),
            frame(&mut h)
        );
        c.bench_function(&format!("mount/rows_{rows}"), |b| {
            b.iter(|| {
                // A row down and back: one row mounts, one unmounts.
                dy = -dy;
                h.store.scroll_by(h.root, Vec2 { x: 0.0, y: dy });
                black_box(frame(black_box(&mut h)));
            });
        });
    }
}

criterion_group!(benches, bench_paint_scale);
criterion_main!(benches);
