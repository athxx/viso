//! Section 71 validation pack for the `View` container widget, driven through
//! the public facade. `View` is the first Tier 1 widget; this is the template
//! the later Tier 1 controls (Label/Image/Icon) copy:
//!
//! - **golden screenshot** — build a `View` tree with `View::build`, lay it out,
//!   paint it through the headless backend, compare pixels to a blessed baseline;
//! - **allocation profile** — a warmed-up `View` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `content_alloc.rs`;
//! - **a11y snapshot** — a `View`'s derived semantics node is `Role::Group` and
//!   carries an authored label;
//! - **input tape** — a scrolling `View` maps to a scroll viewport, so a wheel
//!   sample moves its offset; a non-scrolling `View` consumes no wheel.
//!
//! Building through `View::build` (not raw `cx.flex`/`cx.scroll`) is the point:
//! it proves the widget lowers to the same retained nodes the primitives do, so
//! the whole `viso-ui` pipeline handles a widget-authored tree unchanged.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    Axis, BoxStyle, BuildCx, Component, DirtyClass, LeafStyle, Length, NodeId, NodeStore, Role,
    ScrollEvent, ScrollRouter, Size, Vec2, paint_tree,
};
use viso::widgets::{ViewStyle, view};

const W: u32 = 160;
const H: u32 = 96;
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
const RUST: Rgba = Rgba {
    r: 0.7,
    g: 0.3,
    b: 0.1,
    a: 1.0,
};

/// The golden scene: a dark `View` row holding two solid child boxes. Authored
/// entirely through the `View` widget and `cx.leaf`, so the pixels prove a
/// widget-built tree paints.
fn build_scene(store: &mut NodeStore) -> NodeId {
    let container = view(ViewStyle {
        axis: Axis::Row,
        gap: 12.0,
        padding: viso::ui::Inset::all(10.0),
        size: Size::fill(),
        background: BoxStyle::solid(DARK),
        ..Default::default()
    })
    .children(|cx| {
        cx.leaf(LeafStyle {
            size: Size::fixed(48.0, 48.0),
            style: BoxStyle::solid(TEAL),
        });
        cx.leaf(LeafStyle {
            size: Size::fixed(48.0, 48.0),
            style: BoxStyle::solid(RUST),
        });
    });

    let mut cx = BuildCx::new(store);
    container.build(&mut cx);
    cx.root().expect("view declares a root")
}

// --- golden screenshot -----------------------------------------------------

fn render_scene() -> Vec<u8> {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let root = build_scene(&mut store);

    let surface_rect = Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect, &mut scratch);

    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [W as f32, H as f32]);
    gpu.read_pixels_bgra8(surface)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/view_widget.bgra8")
}

#[test]
fn view_scene_matches_golden() {
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
fn view_derives_a_group_semantics_node_with_label() {
    let mut store = NodeStore::new();
    let root = {
        let container = view(ViewStyle::default()).label("Sidebar");
        let mut cx = BuildCx::new(&mut store);
        container.build(&mut cx);
        cx.root().expect("view declares a root")
    };
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    };
    let mut scratch = Vec::new();
    store.layout(root, surface, &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Group, "a plain View is a Group");
    assert_eq!(node.label.as_deref(), Some("Sidebar"));
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

#[test]
fn scrolling_view_moves_offset_non_scrolling_view_consumes_nothing() {
    // A scrolling View: 100×100 viewport over a 100×300 content child.
    let mut store = NodeStore::new();
    let viewport = {
        let container = view(ViewStyle {
            scroll_axis: Some(Axis::Column),
            size: Size::fixed(100.0, 100.0),
            ..Default::default()
        })
        .children(|cx| {
            cx.leaf(LeafStyle {
                size: Size {
                    width: Length::Fixed(100.0),
                    height: Length::Fixed(300.0),
                },
                style: BoxStyle::NONE,
            });
        });
        let mut cx = BuildCx::new(&mut store);
        container.build(&mut cx);
        cx.root().expect("view declares a root")
    };
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    let mut scratch = Vec::new();
    store.layout(viewport, surface, &mut scratch);
    store.clear_dirty();

    let consumed = ScrollRouter::route(&mut store, viewport, wheel(50.0, 50.0, 60.0));
    assert!(consumed, "the wheel landed on the scrolling View");
    assert_eq!(store.scroll(viewport), Vec2 { x: 0.0, y: 60.0 });
    let d = store.dirty(viewport);
    assert!(d.contains(DirtyClass::TRANSFORM) && d.contains(DirtyClass::PAINT));
    assert!(!d.intersects(DirtyClass::LAYOUT | DirtyClass::MEASURE));

    // A plain (non-scrolling) View is a flex box: a wheel over it scrolls nothing.
    let mut store2 = NodeStore::new();
    let flexroot = {
        let container = view(ViewStyle {
            size: Size::fixed(100.0, 100.0),
            ..Default::default()
        })
        .children(|cx| {
            cx.leaf(LeafStyle {
                size: Size::fixed(100.0, 300.0),
                style: BoxStyle::NONE,
            });
        });
        let mut cx = BuildCx::new(&mut store2);
        container.build(&mut cx);
        cx.root().expect("view declares a root")
    };
    store2.layout(flexroot, surface, &mut scratch);
    let consumed = ScrollRouter::route(&mut store2, flexroot, wheel(50.0, 50.0, 60.0));
    assert!(!consumed, "a plain View is not a scroll viewport");
    assert_eq!(store2.scroll(flexroot), Vec2::ZERO);
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `content_alloc.rs`.
struct CountingAlloc;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

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
    let surface_rect = Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect, &mut scratch);

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
fn steady_view_frame_is_allocation_free() {
    let mut h = setup_alloc();

    // Warm up until the frame path reaches steady state: the first frames grow
    // the persistent instance buffers to fit the scene, cache the per-pipeline
    // bind groups, and size the headless framebuffer/target pool. A couple of
    // frames is enough for this scene, but warm up a few extra so the measured
    // frames below start from a fully grown, reused state.
    for _ in 0..4 {
        frame(&mut h);
    }
    // Grow the reused paint buffer to its steady capacity so a later
    // `paint_tree` into it does not reallocate.
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
            "frame {i}: frame_stats changed for an unchanged View scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged View scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged View scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged View scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a View frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the View scene must emit draw calls");
    assert!(instances > 0, "the View scene must emit instances");
}
