//! The `layout` benchmark category at scale: a card list of 10k nodes (100
//! cards of 9 rows of 10 boxes) laid out once, then timed on the frame's
//! incremental relayout after 1% or 100% of its nodes change a size, and a
//! 1k-node list timed on the relayout of a frame that changed nothing.
//!
//! A row is fixed on both axes, so a box's new size stays inside its row:
//! the 1% change re-lays the rows it reached, the 100% change (every box
//! resized, every container's gap rewritten) re-lays the whole tree.
//!
//! Run release (`cargo bench -p viso-ui --bench layout_scale`); debug timing
//! is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_render::Rect;
use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, FlexStyle, Inset, LeafStyle, Length, NodeId, NodeStore, Rgba,
    Sizing,
};

const ROWS_PER_CARD: usize = 9;
const BOXES_PER_ROW: usize = 10;

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 1200.0,
    h: 800.0,
};

const CARD: Rgba = Rgba {
    r: 0.15,
    g: 0.16,
    b: 0.20,
    a: 1.0,
};
const CHIP: Rgba = Rgba {
    r: 0.3,
    g: 0.5,
    b: 0.8,
    a: 1.0,
};

/// The laid-out list: its root, every box, and every container.
struct Tree {
    store: NodeStore,
    root: NodeId,
    boxes: Vec<NodeId>,
    containers: Vec<NodeId>,
}

/// The size box `i` takes on frame parity `odd`.
fn chip(i: usize, odd: bool) -> Sizing {
    let w = 20.0 + (i % 7) as f32 * 4.0;
    Sizing::fixed(if odd { w + 2.0 } else { w }, 16.0)
}

/// `cards` cards of `ROWS_PER_CARD` fixed rows of `BOXES_PER_ROW` boxes under
/// a fit-height column, laid out once, its dirt cleared.
fn build(cards: usize) -> Tree {
    let mut store = NodeStore::new();
    let mut boxes = Vec::with_capacity(cards * ROWS_PER_CARD * BOXES_PER_ROW);
    let mut containers = Vec::with_capacity(1 + cards * (1 + ROWS_PER_CARD));
    let root = {
        let mut cx = BuildCx::new(&mut store);
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 8.0,
                padding: Inset::all(8.0),
                size: Sizing {
                    width: Length::fill(),
                    height: Length::Fit,
                },
                ..FlexStyle::default()
            },
            |cx| {
                for _ in 0..cards {
                    let card = cx.flex(
                        FlexStyle {
                            axis: Axis::Column,
                            gap: 4.0,
                            padding: Inset::all(8.0),
                            size: Sizing {
                                width: Length::fill(),
                                height: Length::Fit,
                            },
                            style: BoxStyle::solid(CARD).with_radius(6.0),
                            ..FlexStyle::default()
                        },
                        |cx| {
                            for _ in 0..ROWS_PER_CARD {
                                let row = cx.flex(
                                    FlexStyle {
                                        gap: 4.0,
                                        align: Align::Center,
                                        size: Sizing::fixed(1168.0, 24.0),
                                        ..FlexStyle::default()
                                    },
                                    |cx| {
                                        for _ in 0..BOXES_PER_ROW {
                                            let i = boxes.len();
                                            boxes.push(
                                                cx.leaf(LeafStyle {
                                                    size: chip(i, false),
                                                    style: BoxStyle::solid(CHIP).with_radius(3.0),
                                                })
                                                .id(),
                                            );
                                        }
                                    },
                                );
                                containers.push(row.id());
                            }
                        },
                    );
                    containers.push(card.id());
                }
            },
        );
        containers.push(root.id());
        root.id()
    };
    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);
    store.clear_dirty();
    Tree {
        store,
        root,
        boxes,
        containers,
    }
}

/// Resize every `step`th box, and rewrite every container's gap when
/// `containers` is set, to the sizes of frame parity `odd`. Returns how many
/// nodes it wrote.
fn change(t: &mut Tree, step: usize, containers: bool, odd: bool) -> usize {
    let mut wrote = 0;
    for (i, &b) in t.boxes.iter().enumerate().step_by(step) {
        t.store.set_fixed_size(b, chip(i, odd));
        wrote += 1;
    }
    if containers {
        let gap = if odd { 5.0 } else { 4.0 };
        for &c in &t.containers {
            t.store.set_gap(c, gap);
            wrote += 1;
        }
    }
    wrote
}

/// Time one dirty frame of the 10k list: the writes, then the relayout. Checked
/// once first: every write marks its node and the relayout places nodes.
fn dirty_bench(c: &mut Criterion, name: &str, step: usize, containers: bool) {
    let mut t = build(100);
    let nodes = t.store.arena().live_count();
    let mut scratch = Vec::new();
    let mut roots = Vec::new();

    let wrote = change(&mut t, step, containers, true);
    let dirty = t.store.dirty_count();
    assert!(dirty >= wrote, "{name}: every write marks its node dirty");
    let (measured, laid_out) = t
        .store
        .relayout_dirty(t.root, SURFACE, &mut scratch, &mut roots);
    assert!(laid_out > 0, "{name}: the relayout places nodes");
    eprintln!(
        "{name}: {nodes} nodes, {wrote} written ({dirty} dirty), the relayout measures \
         {measured} and places {laid_out} from {} roots",
        roots.len()
    );
    t.store.clear_dirty();

    let mut odd = false;
    c.bench_function(name, |b| {
        b.iter(|| {
            change(&mut t, step, containers, odd);
            odd = !odd;
            let work = t
                .store
                .relayout_dirty(t.root, SURFACE, &mut scratch, &mut roots);
            t.store.clear_dirty();
            black_box(work)
        })
    });
}

fn layout_10k_dirty(c: &mut Criterion) {
    // 9000 boxes, every 90th: 100 nodes, 1% of the tree.
    dirty_bench(c, "layout_10k_dirty_1pct", 90, false);
    dirty_bench(c, "layout_10k_dirty_100pct", 1, true);
}

fn layout_1k_static(c: &mut Criterion) {
    let mut t = build(10);
    let nodes = t.store.arena().live_count();
    let mut scratch = Vec::new();
    let mut roots = Vec::new();
    assert!(!t.store.any_dirty(), "a settled tree carries no dirt");
    let work = t
        .store
        .relayout_dirty(t.root, SURFACE, &mut scratch, &mut roots);
    assert_eq!(work, (0, 0), "a frame that changed nothing re-lays nothing");
    eprintln!("layout_1k_static: {nodes} nodes, nothing dirty, the relayout places none");
    c.bench_function("layout_1k_static", |b| {
        b.iter(|| {
            let work = t
                .store
                .relayout_dirty(t.root, SURFACE, &mut scratch, &mut roots);
            t.store.clear_dirty();
            black_box(work)
        })
    });
}

criterion_group!(benches, layout_10k_dirty, layout_1k_static);
criterion_main!(benches);
