//! The `layout` benchmark category for a long column: 2000 rows, each an
//! icon and a label sized to its text, timed on a relayout after one label's
//! text changes. The redo root is the column, so the relayout lays out every
//! row; what it re-measures is what the change reached.
//!
//! Run release (`cargo bench -p viso-ui --bench column_relayout`); debug
//! timing is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_render::{Rect, TextureId};
use viso_ui::{
    Axis, BuildCx, Content, FlexStyle, LeafStyle, Length, NodeId, NodeStore, Rgba, Sizing, Vec2,
    WidthUse,
};

const ROWS: usize = 2000;

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 800.0,
    h: 600.0,
};

/// A label's shaped text, `n` glyphs wide.
fn text(n: u32) -> Content {
    let width = n as f32 * 7.0;
    Content::Text {
        glyphs: Vec::new(),
        atlas: TextureId::new(1),
        color_glyphs: Vec::new(),
        color_atlas: None,
        color: Rgba::TRANSPARENT,
        natural: Vec2 { x: width, y: 16.0 },
        baseline: 12.0,
        shaped_at_width: None,
        width_use: WidthUse::Ignored,
        min_content: width,
        max_content: width,
        ink: None,
    }
}

/// The column, laid out, and its labels.
fn build() -> (NodeStore, NodeId, Vec<NodeId>) {
    let mut store = NodeStore::new();
    let mut labels = Vec::with_capacity(ROWS);
    let root = {
        let mut cx = BuildCx::new(&mut store);
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 2.0,
                ..FlexStyle::default()
            },
            |cx| {
                for _ in 0..ROWS {
                    cx.flex(
                        FlexStyle {
                            gap: 4.0,
                            size: Sizing {
                                width: Length::fill(),
                                height: Length::Fit,
                            },
                            ..FlexStyle::default()
                        },
                        |cx| {
                            cx.leaf(LeafStyle {
                                size: Sizing::fixed(16.0, 16.0),
                                ..LeafStyle::default()
                            });
                            labels.push(
                                cx.leaf(LeafStyle {
                                    size: Sizing {
                                        width: Length::Fit,
                                        height: Length::Fit,
                                    },
                                    ..LeafStyle::default()
                                })
                                .id(),
                            );
                        },
                    );
                }
            },
        )
        .id()
    };
    for (i, &label) in labels.iter().enumerate() {
        store.set_content_payload(label, text(4 + (i % 9) as u32));
    }
    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);
    store.clear_dirty();
    (store, root, labels)
}

fn bench(c: &mut Criterion) {
    let (mut store, root, labels) = build();
    let mut scratch = Vec::new();
    let mut roots = Vec::new();
    let mut n = 0u32;
    c.bench_function("column_2000_one_label_relayout", |b| {
        b.iter(|| {
            n = n.wrapping_add(1);
            store.set_content_payload(labels[ROWS / 2], text(3 + n % 5));
            let work = store.relayout_dirty(root, SURFACE, &mut scratch, &mut roots);
            store.clear_dirty();
            black_box(work)
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
