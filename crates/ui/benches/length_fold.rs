//! The `layout` benchmark category for environment lengths: a 10k-leaf tree
//! whose leaves mix `dp`, `sp` and `em` widths, timed on a steady re-layout (no
//! environment change), a text-scale change, and a font-size change on one row,
//! each against the same tree with every width folded to `dp`.
//!
//! Run release (`cargo bench -p viso-ui --bench length_fold`); debug timing is
//! not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_render::Rect;
use viso_ui::{
    Axis, BuildCx, FlexStyle, LeafStyle, Length, LengthEnv, LengthTerms, NodeId, NodeLengths,
    NodeStore, Size,
};

const ROWS: usize = 100;
const LEAVES: usize = 100;

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 4000.0,
    h: 3000.0,
};

/// `ROWS` rows of `LEAVES` leaves; with `bound`, half the leaves read `sp`, a
/// quarter `em` and each row sets a font size. Returns the store, the root and
/// the rows.
fn build(bound: bool) -> (NodeStore, NodeId, Vec<NodeId>) {
    let mut store = NodeStore::new();
    let mut rows = Vec::with_capacity(ROWS);
    let root = {
        let mut cx = BuildCx::new(&mut store);
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                for _ in 0..ROWS {
                    let row = cx.flex(
                        FlexStyle {
                            size: Size {
                                width: Length::fill(),
                                height: Length::Fit,
                            },
                            ..FlexStyle::default()
                        },
                        |cx| {
                            for leaf in 0..LEAVES {
                                let handle = cx.leaf(LeafStyle {
                                    size: Size::fixed(20.0, 10.0),
                                    ..LeafStyle::default()
                                });
                                let width = match leaf % 4 {
                                    0 | 1 => LengthTerms::sp(20.0),
                                    2 => LengthTerms::em(1.5),
                                    _ => continue,
                                };
                                if bound {
                                    cx.bind_lengths(
                                        handle,
                                        NodeLengths {
                                            width: Some(width),
                                            ..NodeLengths::default()
                                        },
                                    );
                                }
                            }
                        },
                    );
                    if bound {
                        cx.bind_lengths(
                            row,
                            NodeLengths {
                                font_size: Some(LengthTerms::dp(14.0)),
                                ..NodeLengths::default()
                            },
                        );
                    }
                    rows.push(row.id());
                }
            },
        )
        .id()
    };
    store.layout(root, SURFACE, &mut Vec::new());
    store.clear_dirty();
    (store, root, rows)
}

fn steady(c: &mut Criterion) {
    for (name, bound) in [("length_steady_dp", false), ("length_steady_bound", true)] {
        let (mut store, root, _) = build(bound);
        let mut scratch = Vec::new();
        c.bench_function(name, |b| {
            b.iter(|| {
                store.layout(black_box(root), SURFACE, &mut scratch);
            })
        });
    }
}

fn text_scale(c: &mut Criterion) {
    let (mut store, root, _) = build(true);
    let (mut scratch, mut redo) = (Vec::new(), Vec::new());
    let mut scale = 1.0;
    c.bench_function("length_text_scale_refold_10k", |b| {
        b.iter(|| {
            scale = if scale == 1.0 { 1.25 } else { 1.0 };
            store.set_length_env(LengthEnv {
                text_scale: scale,
                ..LengthEnv::default()
            });
            store.relayout_dirty(black_box(root), SURFACE, &mut scratch, &mut redo);
            store.clear_dirty();
        })
    });
}

fn font_size(c: &mut Criterion) {
    let (mut store, root, rows) = build(true);
    let (mut scratch, mut redo) = (Vec::new(), Vec::new());
    let mut size = 14.0;
    c.bench_function("length_row_font_refold", |b| {
        b.iter(|| {
            size = if size == 14.0 { 16.0 } else { 14.0 };
            store.bind_lengths(
                rows[ROWS / 2],
                NodeLengths {
                    font_size: Some(LengthTerms::dp(size)),
                    ..NodeLengths::default()
                },
            );
            store.relayout_dirty(black_box(root), SURFACE, &mut scratch, &mut redo);
            store.clear_dirty();
        })
    });
}

criterion_group!(benches, steady, text_scale, font_size);
criterion_main!(benches);
