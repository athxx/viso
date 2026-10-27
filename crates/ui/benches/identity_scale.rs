//! Identity at scale, two contracts measured rather than claimed:
//!
//! 1. **Dense property lookup.** A node's hot data resolves by its
//!    generational `NodeId` indexing a dense column, generation-checked —
//!    never a hash probe. One million `size_request` reads over a laid-out
//!    100k-node tree, against a `HashMap<NodeId, Sizing>` holding the same
//!    values, so the gap is a number.
//! 2. **Keyed reconcile.** A keyed list of 100k items gains one at its head
//!    each frame, the newest-first feed shape: every mounted row's key moves
//!    down one index, so the reconcile matches each survivor by key and
//!    re-anchors it, mounts the one new row and parks the one pushed out.
//!
//! Run release (`cargo bench -p viso-ui --bench identity_scale`); debug timing
//! is not a performance result.

use std::cell::Cell;
use std::collections::HashMap;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_render::Rect;
use viso_ui::virtual_list::ItemKey;
use viso_ui::{
    Axis, BindingTable, BoxStyle, BuildCx, DirtyClass, EffectStore, FlexStyle, LeafStyle, Length,
    NodeId, NodeStore, SemanticProjector, Sizing, StateStore, TextEdits, VirtualListStyle,
    VirtualLists, virtual_list,
};

/// Rows under the root, and boxes in each: 100,001 nodes.
const ROWS: usize = 1000;
const BOXES_PER_ROW: usize = 99;
/// Lookups each property bench runs an iteration.
const LOOKUPS: usize = 1_000_000;

/// The 100k tree laid out, and every node id in build order.
fn build_tree() -> (NodeStore, Vec<NodeId>) {
    let mut store = NodeStore::new();
    let mut ids = Vec::with_capacity(1 + ROWS * (1 + BOXES_PER_ROW));
    let root = {
        let mut cx = BuildCx::new(&mut store);
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                size: Sizing {
                    width: Length::fill(),
                    height: Length::Fit,
                },
                ..FlexStyle::default()
            },
            |cx| {
                for _ in 0..ROWS {
                    let row = cx.flex(
                        FlexStyle {
                            size: Sizing::fixed(1200.0, 12.0),
                            ..FlexStyle::default()
                        },
                        |cx| {
                            for b in 0..BOXES_PER_ROW {
                                ids.push(
                                    cx.leaf(LeafStyle {
                                        size: Sizing::fixed(4.0 + (b % 5) as f32, 10.0),
                                        ..LeafStyle::default()
                                    })
                                    .id(),
                                );
                            }
                        },
                    );
                    ids.push(row.id());
                }
            },
        )
        .id()
    };
    ids.push(root);
    let mut scratch = Vec::new();
    store.layout(
        root,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1200.0,
            h: 800.0,
        },
        &mut scratch,
    );
    store.clear_dirty();
    (store, ids)
}

/// A fixed probe order: `LOOKUPS` ids cycling through the population in build
/// order, the order a tree walk reads them.
fn probe_ids(ids: &[NodeId]) -> Vec<NodeId> {
    (0..LOOKUPS).map(|i| ids[i % ids.len()]).collect()
}

/// The fixed width a request carries, 0 for any other length.
fn width(size: Sizing) -> f32 {
    match size.width {
        Length::Fixed(w) => w,
        _ => 0.0,
    }
}

fn identity_dense_property_lookup(c: &mut Criterion) {
    let (store, ids) = build_tree();
    let probes = probe_ids(&ids);
    let map: HashMap<NodeId, Sizing> = ids
        .iter()
        .map(|&id| (id, store.size_request(id).expect("a built node is live")))
        .collect();

    let dense = |probes: &[NodeId]| {
        let mut sum = 0.0f32;
        for &id in probes {
            if let Some(size) = store.size_request(black_box(id)) {
                sum += width(size);
            }
        }
        sum
    };
    let hashed = |probes: &[NodeId]| {
        let mut sum = 0.0f32;
        for id in probes {
            if let Some(&size) = map.get(black_box(id)) {
                sum += width(size);
            }
        }
        sum
    };
    // Checked once: both paths read every probe and agree on what they read.
    assert_eq!(
        dense(&probes).to_bits(),
        hashed(&probes).to_bits(),
        "the dense column and the map hold the same sizes"
    );
    assert!(
        probes.iter().all(|&id| store.size_request(id).is_some()),
        "every probe resolves"
    );
    eprintln!(
        "identity_dense_property_lookup_1m: {LOOKUPS} generation-checked size reads over {} \
         live nodes, against a HashMap of the same {} entries",
        store.arena().live_count(),
        map.len()
    );

    c.bench_function("identity_dense_property_lookup_1m", |b| {
        b.iter(|| black_box(dense(&probes)))
    });
    c.bench_function("identity_property_lookup_hashmap_1m", |b| {
        b.iter(|| black_box(hashed(&probes)))
    });
}

const ITEM_COUNT: usize = 100_000;
const VIEWPORT_H: f32 = 600.0;
const ROW_H: f32 = 30.0;

/// A keyed 100k-item list mounted and laid out, and the head key its rows
/// count down from: row `i` is item `head - i`.
struct Feed {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    viewport: NodeId,
    surface: Rect,
    scratch: Vec<u32>,
    redo: Vec<NodeId>,
    head: Rc<Cell<u64>>,
}

fn setup_feed() -> Feed {
    let head = Rc::new(Cell::new(u64::MAX / 2));
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
        let key_head = head.clone();
        cx.virtual_list_keyed(
            VirtualListStyle {
                axis: Axis::Column,
                size: Sizing {
                    width: Length::Fixed(300.0),
                    height: Length::Fixed(VIEWPORT_H),
                },
                overscan: 6,
                estimated_row: ROW_H,
                style: BoxStyle::NONE,
                ..Default::default()
            },
            ITEM_COUNT,
            move |i| ItemKey(key_head.get() - i as u64),
            move |_i, cx| {
                cx.leaf(LeafStyle {
                    size: Sizing {
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
    let mut f = Feed {
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
        head,
    };
    f.store.layout(f.viewport, f.surface, &mut f.scratch);
    frame(&mut f);
    frame(&mut f);
    f
}

/// One frame's Layout phase: reconcile, incremental relayout, absorb. Returns
/// the rows (re)bound.
fn frame(f: &mut Feed) -> u32 {
    let bound = virtual_list::reconcile(
        &mut f.store,
        &mut f.lists,
        &mut f.states,
        &mut f.bindings,
        &mut f.effects,
    );
    f.store
        .relayout_dirty(f.viewport, f.surface, &mut f.scratch, &mut f.redo);
    virtual_list::absorb_measurements(&f.store, &mut f.lists);
    f.store.clear_dirty();
    bound
}

/// One item arrives at the head.
fn prepend(f: &mut Feed) -> u32 {
    f.head.set(f.head.get() + 1);
    f.lists
        .get_mut(f.viewport)
        .expect("the feed is registered")
        .mark_data_dirty();
    frame(f)
}

/// The host mounted for row `logical`, if any.
fn host_at(f: &Feed, logical: usize) -> Option<NodeId> {
    let list = f.lists.get(f.viewport).expect("the feed is registered");
    list.mounted_rows()
        .find_map(|(i, host)| (i == logical).then_some(host))
}

fn identity_keyed_reconcile(c: &mut Criterion) {
    let mut f = setup_feed();
    let mounted = f
        .lists
        .get(f.viewport)
        .expect("the feed is registered")
        .mounted_count();
    assert!(
        (20..=40).contains(&mounted),
        "the feed mounts a window's worth of rows, got {mounted}"
    );
    // Checked once: the item at row 5 keeps its host as it moves to row 6.
    let survivor = host_at(&f, 5).expect("row 5 is mounted");
    let bound = prepend(&mut f);
    assert_eq!(
        host_at(&f, 6),
        Some(survivor),
        "a prepend moves the survivor's host with its key"
    );
    assert!(
        bound < mounted as u32,
        "a prepend re-anchors the survivors instead of rebinding the window ({bound} bound)"
    );
    eprintln!(
        "identity_keyed_reconcile_100k: {ITEM_COUNT} keyed items, {mounted} rows mounted, a \
         prepend binds {bound} rows and re-anchors the rest by key"
    );

    c.bench_function("identity_keyed_reconcile_100k", |b| {
        b.iter(|| black_box(prepend(black_box(&mut f))))
    });
}

criterion_group!(
    benches,
    identity_dense_property_lookup,
    identity_keyed_reconcile
);
criterion_main!(benches);
