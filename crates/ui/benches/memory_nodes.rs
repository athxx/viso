//! The footprint of a retained tree: a typical 10k-node card list (100 cards
//! of 9 rows of 10 styled boxes) built through `BuildCx`, its build timed.
//!
//! Before timing, it prints the heap the built tree holds — counted by this
//! binary's allocator, from before the store exists to after the tree is
//! built and again after its first layout — per node, with the total bytes
//! and allocations the build made along the way and the store's own account
//! of its hot / warm / cold columns.
//!
//! Run release (`cargo bench -p viso-ui --bench memory_nodes`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use viso_render::Rect;
use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, FlexStyle, Inset, LeafStyle, Length, NodeId, NodeStore, Rgba,
    Sizing,
};

struct CountingAlloc;

/// Bytes held now, the most held since the last reset, bytes and blocks
/// handed out since the last reset, all while armed.
static HELD: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

fn grow(bytes: usize) {
    let held = HELD.fetch_add(bytes as isize, Ordering::Relaxed) + bytes as isize;
    PEAK.fetch_max(held, Ordering::Relaxed);
    TOTAL.fetch_add(bytes, Ordering::Relaxed);
    ALLOCS.fetch_add(1, Ordering::Relaxed);
}

/// A block freed while armed may predate arming, so the count is signed.
fn shrink(bytes: usize) {
    HELD.fetch_sub(bytes as isize, Ordering::Relaxed);
}

/// Bytes held since arming, as a count.
fn held() -> usize {
    HELD.load(Ordering::Relaxed).max(0) as usize
}

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is relaxed counter updates while armed.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            grow(layout.size());
        }
        // SAFETY: `layout` is the caller's, valid for `alloc`; the system allocator gets it unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.load(Ordering::Relaxed) {
            shrink(layout.size());
        }
        // SAFETY: `ptr` was allocated by `System` with `layout`, through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            shrink(layout.size());
            grow(new_size);
        }
        // SAFETY: `ptr` was allocated by `System` with `layout`, and the caller upholds `realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// Start counting from zero.
fn arm() {
    HELD.store(0, Ordering::Relaxed);
    PEAK.store(0, Ordering::Relaxed);
    TOTAL.store(0, Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
}

const CARDS: usize = 100;
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

/// The card list in a new store, unlaid.
fn build() -> (NodeStore, NodeId) {
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        cx.flex(
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
                for _ in 0..CARDS {
                    cx.flex(
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
                                cx.flex(
                                    FlexStyle {
                                        gap: 4.0,
                                        align: Align::Center,
                                        size: Sizing::fixed(1168.0, 24.0),
                                        ..FlexStyle::default()
                                    },
                                    |cx| {
                                        for b in 0..BOXES_PER_ROW {
                                            cx.leaf(LeafStyle {
                                                size: Sizing::fixed(20.0 + b as f32 * 4.0, 16.0),
                                                style: BoxStyle::solid(CHIP).with_radius(3.0),
                                            });
                                        }
                                    },
                                );
                            }
                        },
                    );
                }
            },
        )
        .id()
    };
    (store, root)
}

fn memory_10k_nodes(c: &mut Criterion) {
    arm();
    let (mut store, root) = build();
    let built = held();
    let total = TOTAL.load(Ordering::Relaxed);
    let allocs = ALLOCS.load(Ordering::Relaxed);
    let peak = PEAK.load(Ordering::Relaxed);
    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);
    drop(scratch);
    let laid = held();
    ARMED.store(false, Ordering::Relaxed);

    let nodes = store.arena().live_count();
    assert_eq!(
        nodes,
        1 + CARDS * (1 + ROWS_PER_CARD * (1 + BOXES_PER_ROW)),
        "the build mounts every node"
    );
    assert!(built > 0, "the counter sees the build");
    let m = store.memory();
    eprintln!(
        "memory_10k_nodes: {nodes} nodes hold {built} heap bytes built ({} per node), {laid} \
         laid out ({} per node); the build allocated {total} bytes in {allocs} blocks, peak \
         {peak}; columns hot {} / warm {} / cold {} per node",
        built / nodes,
        laid / nodes,
        m.per_node(m.hot),
        m.per_node(m.warm),
        m.per_node(m.cold),
    );
    drop(store);

    c.bench_function("memory_10k_nodes", |b| {
        b.iter_with_large_drop(|| black_box(build()))
    });
}

criterion_group!(benches, memory_10k_nodes);
criterion_main!(benches);
