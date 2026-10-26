//! Skeleton blocks' pixels and frame cost: an avatar, three lines and a
//! picture's block under a band of light match a blessed golden; a
//! warmed-up shimmering frame, its band moving on each frame, allocates
//! nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle};
use viso::render::{Rect, Renderer};
use viso::ui::{
    Axis, BindingTable, BuildCx, Component, FlexStyle, Inset, NodeId, NodeStore, SemanticProjector,
    Sizing, StateStore, StateValue, TextEdits, VirtualLists, paint_tree,
};
use viso::widgets::{placeholder, skeleton_circle, skeleton_rect, skeleton_text};

const W: u32 = 240;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.96, 0.96, 0.96, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

fn surface_rect() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    }
}

fn build_scene(store: &mut NodeStore) -> NodeId {
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let mut cx = BuildCx::with_reactive(
        store,
        &mut states,
        &mut bindings,
        &mut lists,
        &mut text_edits,
        &mut projectors,
    );
    let loading = cx.state(StateValue::Bool(true));
    cx.flex(
        FlexStyle {
            axis: Axis::Column,
            padding: Inset::all(12.0),
            size: Sizing::fixed(W as f32, H as f32),
            ..FlexStyle::default()
        },
        |cx| {
            placeholder(loading)
                .skeleton(|cx| {
                    cx.flex(
                        FlexStyle {
                            axis: Axis::Row,
                            gap: 12.0,
                            ..FlexStyle::default()
                        },
                        |cx| {
                            skeleton_circle(40.0).build(cx);
                            skeleton_text(3).build(cx);
                        },
                    );
                    cx.flex(
                        FlexStyle {
                            axis: Axis::Row,
                            size: Sizing::fixed(216.0, 12.0),
                            ..FlexStyle::default()
                        },
                        |_| {},
                    );
                    skeleton_rect(Sizing::fixed(216.0, 40.0)).build(cx);
                })
                .build(cx);
        },
    )
    .id()
}

/// Run the shimmer clock to `seconds`, a frame at a time.
fn run_clock(store: &mut NodeStore, seconds: f32) {
    let step = 0.04;
    for _ in 0..(seconds / step).round() as u32 {
        store.tick_shimmers(Duration::from_secs_f32(step));
    }
}

#[test]
fn skeleton_blocks_under_a_band_of_light_match_golden() {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let root = build_scene(&mut store);
    store.layout(root, surface_rect(), &mut Vec::new());
    // The band's centre at 120 dp, across the middle of the blocks.
    run_clock(&mut store, 0.24);

    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [W as f32, H as f32]);
    let actual = gpu.read_pixels_bgra8(surface);

    let path = golden_path();
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        eprintln!("blessed golden: {}", path.display());
        return;
    }
    let expected = std::fs::read(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with BLESS=1 to generate it",
            path.display()
        )
    });
    assert_eq!(actual.len(), expected.len(), "golden size mismatch");
    let (worst, at) = actual
        .iter()
        .zip(&expected)
        .enumerate()
        .map(|(i, (&a, &e))| (a.abs_diff(e), i))
        .max()
        .unwrap();
    assert!(
        worst <= TOL,
        "golden mismatch: max per-channel diff {worst} at pixel {} exceeds {TOL}",
        at / 4
    );
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/skeleton_widget.bgra8")
}

// --- allocation profile ---------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted.
struct CountingAlloc;
// Thread-local counters: cargo runs the `#[test]`s in this binary in parallel
// on separate threads, all sharing this one process-global allocator. A
// process-global armed flag / counter would let a sibling test's allocations,
// happening on another thread while this test is inside its armed measurement
// window, race into this test's count and make the steady-state assertion
// flaky. Scoping arm state and the count to the measuring thread makes each
// test see only its own allocations — the frame path under test is
// synchronous, so every allocation it performs is on the arming thread. The
// `.load`/`.store`/`.fetch_add` API and its `Ordering` argument are kept so the
// call sites and the `GlobalAlloc` impl below are unchanged (the ordering is
// irrelevant for thread-local state and is ignored).
thread_local! {
    static ALLOCS_CELL: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ARMED_CELL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
struct TlsBool;
struct TlsUsize;
impl TlsBool {
    fn load(&self, _: Ordering) -> bool {
        ARMED_CELL.with(std::cell::Cell::get)
    }
    fn store(&self, v: bool, _: Ordering) {
        ARMED_CELL.with(|c| c.set(v));
    }
}
impl TlsUsize {
    fn load(&self, _: Ordering) -> usize {
        ALLOCS_CELL.with(std::cell::Cell::get)
    }
    fn store(&self, v: usize, _: Ordering) {
        ALLOCS_CELL.with(|c| c.set(v));
    }
    fn fetch_add(&self, v: usize, _: Ordering) {
        ALLOCS_CELL.with(|c| c.set(c.get() + v));
    }
}
static ALLOCS: TlsUsize = TlsUsize;
static ARMED: TlsBool = TlsBool;

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is a relaxed counter increment on allocation while armed.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: `layout` is the caller's, valid for `alloc`; the system allocator gets it unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by `System` with `layout`, through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: `ptr` was allocated by `System` with `layout`, and the caller upholds `realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

#[test]
fn a_shimmering_frame_is_allocation_free() {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
    let mut store = NodeStore::new();
    let root = build_scene(&mut store);
    store.layout(root, surface_rect(), &mut Vec::new());
    let mut primitives = Vec::new();
    let mut frame = |store: &mut NodeStore, gpu: &mut CpuRaster| {
        store.tick_shimmers(Duration::from_millis(16));
        primitives.clear();
        paint_tree(store, root, &mut primitives);
        renderer.upload(gpu, &primitives);
        renderer.submit(gpu, surface, CLEAR, [W as f32, H as f32]);
        store.clear_dirty();
    };
    // Warm up across a whole wave, so every band count the blocks see has
    // been met.
    for _ in 0..120 {
        frame(&mut store, &mut gpu);
    }
    let buffers = gpu.buffer_count();
    let textures = gpu.texture_count();
    ALLOCS.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    for _ in 0..30 {
        frame(&mut store, &mut gpu);
    }
    ARMED.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "a shimmering frame allocated"
    );
    assert_eq!(gpu.buffer_count(), buffers);
    assert_eq!(gpu.texture_count(), textures);
}
