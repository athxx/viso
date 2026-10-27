//! The `hit_test` benchmark category at scale: one pointer hit-tested against
//! a laid-out tree of ~100k nodes, five levels deep with ten children a level
//! (nine boxes under each last container), axes alternating so every level
//! splits the space. Each point lands on a box at the bottom of the tree, so
//! the query descends all five levels, testing siblings topmost-first.
//!
//! Run release (`cargo bench -p viso-ui --bench hit_scale`); debug timing is
//! not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_render::Rect;
use viso_ui::{
    Axis, BoxStyle, BuildCx, FlexStyle, LeafStyle, NodeId, NodeStore, Rgba, Sizing, hit_test,
};

/// Containers under each container, for the levels above the boxes.
const FANOUT: usize = 10;
/// Container levels below the root.
const LEVELS: usize = 4;
/// Boxes under each bottom container.
const BOXES: usize = 9;
/// Points the bench cycles through.
const PROBES: usize = 1024;

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 2000.0,
    h: 2000.0,
};

const FILL: Rgba = Rgba {
    r: 0.3,
    g: 0.5,
    b: 0.8,
    a: 1.0,
};

fn axis_at(depth: usize) -> Axis {
    if depth.is_multiple_of(2) {
        Axis::Row
    } else {
        Axis::Column
    }
}

/// A container at `depth` and everything under it; boxes are collected.
fn level(cx: &mut BuildCx<'_>, depth: usize, boxes: &mut Vec<NodeId>) {
    cx.flex(
        FlexStyle {
            axis: axis_at(depth),
            size: Sizing::fill(),
            ..FlexStyle::default()
        },
        |cx| {
            if depth == LEVELS {
                for _ in 0..BOXES {
                    boxes.push(
                        cx.leaf(LeafStyle {
                            size: Sizing::fill(),
                            style: BoxStyle::solid(FILL),
                        })
                        .id(),
                    );
                }
            } else {
                for _ in 0..FANOUT {
                    level(cx, depth + 1, boxes);
                }
            }
        },
    );
}

/// The tree laid out into the surface, its root and every box.
fn build() -> (NodeStore, NodeId, Vec<NodeId>) {
    let mut store = NodeStore::new();
    let mut boxes = Vec::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        cx.flex(
            FlexStyle {
                axis: axis_at(0),
                size: Sizing::fixed(SURFACE.w, SURFACE.h),
                ..FlexStyle::default()
            },
            |cx| {
                for _ in 0..FANOUT {
                    level(cx, 1, &mut boxes);
                }
            },
        )
        .id()
    };
    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);
    store.clear_dirty();
    (store, root, boxes)
}

fn hit_test_100k(c: &mut Criterion) {
    let (store, root, boxes) = build();
    let nodes = store.arena().live_count();

    // Box centres spread over the tree by a fixed LCG, each checked to hit its
    // own box.
    let mut seed = 0x9e37_79b9_u32;
    let probes: Vec<(f32, f32)> = (0..PROBES)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let target = boxes[seed as usize % boxes.len()];
            let r = store.bounds(target);
            assert!(r.w > 0.0 && r.h > 0.0, "every box is laid out with an area");
            let p = (r.x + r.w * 0.5, r.y + r.h * 0.5);
            assert_eq!(
                hit_test(&store, root, p.0, p.1),
                Some(target),
                "a box centre hits that box"
            );
            p
        })
        .collect();
    let r = store.bounds(boxes[0]);
    eprintln!(
        "hit_test_100k: {nodes} nodes, {} boxes of {}x{} at depth {}, {PROBES} points each \
         hitting its box",
        boxes.len(),
        r.w,
        r.h,
        LEVELS + 1
    );

    let mut i = 0;
    c.bench_function("hit_test_100k", |b| {
        b.iter(|| {
            let (x, y) = probes[i];
            i = (i + 1) % PROBES;
            black_box(hit_test(&store, black_box(root), x, y))
        })
    });
}

criterion_group!(benches, hit_test_100k);
criterion_main!(benches);
