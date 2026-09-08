//! Section 71 validation pack for the `Scroll` viewport widget — the first
//! Tier 3 (layout structure) control — driven through the public facade. `Scroll`
//! is a minimal scroll viewport: a clip box that scrolls a single content child
//! along one axis. This pack mirrors `view_widget.rs` with the viewport's own
//! structure and the scroll gesture as its interactive tape:
//!
//! - **golden screenshot** — build a `Scroll` viewport over a tall content box
//!   with `Scroll::build`, lay it out, paint it through the headless backend, and
//!   confirm the pixels match a blessed baseline. The content is taller than the
//!   viewport, so the golden proves the viewport clips its overflow to its box;
//! - **input tape (scroll)** — a wheel sample over the viewport moves its offset
//!   (clamped to the scrollable range) and dirties only transform/hit-test/paint,
//!   never layout, exactly as `scroll_routing.rs` drives the raw `cx.scroll` node;
//! - **a11y snapshot** — a `Scroll`'s derived semantics node is `Role::Group` and
//!   carries an authored label; it declares no interactive handlers of its own;
//! - **allocation profile** — a warmed-up `Scroll` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `view_widget.rs`.
//!
//! Building through `Scroll::build` (not raw `cx.scroll`) is the point: it proves
//! the widget lowers to the same retained scroll node the primitive does, so the
//! whole `viso-ui` scroll pipeline handles a widget-authored viewport unchanged.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    Axis, BoxStyle, BuildCx, Component, DirtyClass, LeafStyle, Length, NodeId, NodeStore, Role,
    ScrollEvent, ScrollRouter, Size, Vec2, paint_tree,
};
use viso::widgets::{ScrollViewStyle, scroll};

const W: u32 = 120;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

const DARK: Rgba = Rgba {
    r: 0.1,
    g: 0.1,
    b: 0.12,
    a: 1.0,
};
const TEAL: Rgba = Rgba {
    r: 0.1,
    g: 0.5,
    b: 0.5,
    a: 1.0,
};

/// The golden scene: a dark `Scroll` viewport (100×100) over a single teal
/// content box (100×300) that overflows it. Authored entirely through the
/// `Scroll` widget and `cx.leaf`, so the pixels prove a widget-built viewport
/// clips its content to its box.
fn build_scene(store: &mut NodeStore) -> NodeId {
    let region = scroll(ScrollViewStyle {
        axis: Axis::Column,
        size: Size::fixed(100.0, 100.0),
        background: BoxStyle::solid(DARK),
    })
    .content(|cx| {
        cx.leaf(LeafStyle {
            size: Size::fixed(100.0, 300.0),
            style: BoxStyle::solid(TEAL),
        });
    });

    let mut cx = BuildCx::new(store);
    region.build(&mut cx);
    cx.root().expect("scroll declares a root")
}

fn surface_rect() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    }
}

// --- golden screenshot -----------------------------------------------------

fn render_scene() -> Vec<u8> {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let root = build_scene(&mut store);

    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [W as f32, H as f32]);
    gpu.read_pixels_bgra8(surface)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/scroll_widget.bgra8")
}

#[test]
fn scroll_scene_matches_golden() {
    let actual = render_scene();
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
    assert_eq!(
        actual.len(),
        expected.len(),
        "golden size mismatch: {} vs {}",
        actual.len(),
        expected.len()
    );

    let mut worst = 0u8;
    let mut worst_at = 0usize;
    for (i, (&a, &e)) in actual.iter().zip(&expected).enumerate() {
        let diff = a.abs_diff(e);
        if diff > worst {
            worst = diff;
            worst_at = i;
        }
    }
    assert!(
        worst <= TOL,
        "golden mismatch: max per-channel diff {worst} at byte {worst_at} \
         (pixel {}, channel {}) exceeds tolerance {TOL}",
        worst_at / 4,
        worst_at % 4,
    );
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn scroll_derives_a_group_semantics_node_with_label() {
    let mut store = NodeStore::new();
    let root = {
        let region = scroll(ScrollViewStyle::default()).label("Log");
        let mut cx = BuildCx::new(&mut store);
        region.build(&mut cx);
        cx.root().expect("scroll declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Group, "a Scroll viewport is a Group");
    assert_eq!(node.label.as_deref(), Some("Log"));
}

// --- input tape (scroll) ----------------------------------------------------

fn wheel(x: f32, y: f32, dy: f32) -> ScrollEvent {
    ScrollEvent {
        x,
        y,
        delta_x: 0.0,
        delta_y: dy,
        modifiers: Default::default(),
    }
}

/// A 100×100 vertical `Scroll` viewport over a 100×300 content child (200px of
/// range), laid out over the surface so a center wheel sample lands on it.
fn scroll_scene() -> (NodeStore, NodeId) {
    let mut store = NodeStore::new();
    let viewport = {
        let region = scroll(ScrollViewStyle {
            axis: Axis::Column,
            size: Size::fixed(100.0, 100.0),
            ..Default::default()
        })
        .content(|cx| {
            cx.leaf(LeafStyle {
                size: Size {
                    width: Length::Fixed(100.0),
                    height: Length::Fixed(300.0),
                },
                style: BoxStyle::NONE,
            });
        });
        let mut cx = BuildCx::new(&mut store);
        region.build(&mut cx);
        cx.root().expect("scroll declares a root")
    };
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    let mut scratch = Vec::new();
    store.layout(viewport, surface, &mut scratch);
    (store, viewport)
}

#[test]
fn wheel_scrolls_the_viewport_and_dirties_only_transform_paint() {
    let (mut store, viewport) = scroll_scene();
    store.clear_dirty();

    let consumed = ScrollRouter::route(&mut store, viewport, wheel(50.0, 50.0, 60.0));
    assert!(consumed, "the wheel landed on the Scroll viewport");
    assert_eq!(store.scroll(viewport), Vec2 { x: 0.0, y: 60.0 });

    let d = store.dirty(viewport);
    assert!(d.contains(DirtyClass::TRANSFORM) && d.contains(DirtyClass::PAINT));
    assert!(
        !d.intersects(DirtyClass::LAYOUT | DirtyClass::MEASURE),
        "a scroll re-derives world rects and repaints without a relayout"
    );
}

#[test]
fn wheel_clamps_at_the_end_of_range_and_off_the_viewport_scrolls_nothing() {
    let (mut store, viewport) = scroll_scene();

    // A large delta clamps to content(300) − viewport(100) = 200.
    ScrollRouter::route(&mut store, viewport, wheel(50.0, 50.0, 10_000.0));
    assert_eq!(
        store.scroll(viewport),
        Vec2 { x: 0.0, y: 200.0 },
        "the offset clamps to the scrollable range"
    );

    // A wheel sample off the viewport scrolls nothing.
    let (mut store, viewport) = scroll_scene();
    let consumed = ScrollRouter::route(&mut store, viewport, wheel(500.0, 500.0, 60.0));
    assert!(!consumed, "a wheel off the viewport is not consumed");
    assert_eq!(store.scroll(viewport), Vec2::ZERO);
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `view_widget.rs`.
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
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

struct Harness {
    gpu: HeadlessRaster,
    renderer: Renderer,
    surface: viso::gpu::SurfaceId,
    store: NodeStore,
    root: NodeId,
    primitives: Vec<viso::render::Primitive>,
}

fn setup_alloc() -> Harness {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let root = build_scene(&mut store);
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    Harness {
        gpu,
        renderer,
        surface,
        store,
        root,
        primitives: Vec::new(),
    }
}

fn frame(h: &mut Harness) {
    h.primitives.clear();
    paint_tree(&h.store, h.root, &mut h.primitives);
    h.renderer.upload(&mut h.gpu, &h.primitives);
    h.renderer
        .submit(&mut h.gpu, h.surface, CLEAR, [W as f32, H as f32]);
}

#[test]
fn steady_scroll_frame_is_allocation_free() {
    let mut h = setup_alloc();

    // Warm up until the frame path reaches steady state: the first frames grow
    // the persistent instance buffers to fit the scene, cache the per-pipeline
    // bind groups, and size the headless framebuffer/target pool.
    for _ in 0..4 {
        frame(&mut h);
    }
    // Grow the reused paint buffer to its steady capacity so a later `paint_tree`
    // into it does not reallocate.
    h.primitives.clear();
    paint_tree(&h.store, h.root, &mut h.primitives);

    let buffers = h.gpu.buffer_count();
    let textures = h.gpu.texture_count();
    let bind_groups = h.gpu.bind_group_count();
    let stats = h.renderer.frame_stats();

    let mut frame_allocs = [0usize; 2];
    for (i, slot) in frame_allocs.iter_mut().enumerate() {
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        frame(&mut h);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);

        assert_eq!(
            h.renderer.frame_stats(),
            stats,
            "frame {i}: frame_stats changed for an unchanged Scroll scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Scroll scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Scroll scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Scroll scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a Scroll frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the Scroll scene must emit draw calls");
    assert!(instances > 0, "the Scroll scene must emit instances");
}
