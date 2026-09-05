//! Section 71 validation pack for the `Icon` vector control, driven through the
//! public facade. `Icon` is the next content control after `Image`; this mirrors
//! the `image_widget.rs` template, but needs no texture fixture — the geometry is
//! authored inline and the renderer rasterizes the path directly:
//!
//! - **golden screenshot + measure** — build an `Icon` with `Icon::build` (which
//!   goes through the real `cx.path` content seam) carrying a deterministic inline
//!   outline, lay it out, and confirm both that the `Fixed` leaf measured to the
//!   icon's intrinsic size and that the pixels (the tessellated fill) match a
//!   blessed baseline;
//! - **a11y snapshot** — an `Icon`'s derived semantics node is `Role::Group` (a
//!   presentational, non-interactive element; a dedicated `Role::Icon` is a later
//!   slice);
//! - **allocation profile** — a warmed-up `Icon` frame allocates nothing per frame
//!   (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `image_widget.rs`.
//!
//! Building through `Icon::build` is the point: it proves the widget lowers to a
//! content-bearing leaf the `viso-ui` path seam and `paint_tree` handle unchanged.
//! Unlike `Image` there is no resident-texture fixture — the outline commands are
//! deterministic and the renderer tessellates them, so the golden draws real
//! pixels with no decode path.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    Axis, BoxStyle, BuildCx, Component, Inset, NodeId, NodeStore, PathCmd, Point, Role, Size,
    paint_tree,
};
use viso::widgets::{ViewStyle, icon, view};

const W: u32 = 160;
const H: u32 = 96;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// The icon's intrinsic size in physical pixels.
const ICON: f32 = 48.0;

const DARK: Rgba = Rgba {
    r: 0.1,
    g: 0.1,
    b: 0.12,
    a: 1.0,
};

/// A bright fill so the tessellated shape stands out against the dark container.
const ACCENT: Rgba = Rgba {
    r: 0.2,
    g: 0.6,
    b: 1.0,
    a: 1.0,
};

/// A deterministic filled diamond in the icon's `ICON`x`ICON` local space — the
/// inline geometry the golden rasterizes. No texture/decode path is involved.
fn diamond() -> Vec<PathCmd> {
    let half = ICON / 2.0;
    vec![
        PathCmd::MoveTo(Point::new(half, 0.0)),
        PathCmd::LineTo(Point::new(ICON, half)),
        PathCmd::LineTo(Point::new(half, ICON)),
        PathCmd::LineTo(Point::new(0.0, half)),
        PathCmd::Close,
    ]
}

/// Build a padded dark `View` wrapping a single `Icon`, and return the root plus
/// the icon's leaf. Wrapping in a container keeps the surface from being filled by
/// the leaf itself, so the icon sits at its intrinsic size inside the padding. The
/// icon is the container's only child, so its leaf is the root's first child.
fn build_scene(store: &mut NodeStore) -> (NodeId, NodeId) {
    let container = view(ViewStyle {
        axis: Axis::Row,
        padding: Inset::all(10.0),
        size: Size::fill(),
        background: BoxStyle::solid(DARK),
        ..Default::default()
    })
    .children(move |cx| {
        icon(diamond(), ICON, ICON).fill(ACCENT).build(cx);
    });

    let root = {
        let mut cx = BuildCx::new(store);
        container.build(&mut cx);
        cx.root().expect("scene has a root")
    };
    let icon_id = store
        .arena()
        .links(root)
        .and_then(|l| l.first_child)
        .expect("the view has the icon leaf as its only child");
    (root, icon_id)
}

fn surface_rect() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    }
}

// --- golden screenshot + measure -------------------------------------------

#[test]
fn icon_fixed_leaf_measures_to_natural_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let (root, icon_id) = build_scene(&mut store);

    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    // The icon leaf sized to the outline's intrinsic extent (default `Fixed`).
    let bounds = store.bounds(icon_id);
    assert!(
        (bounds.w - ICON).abs() <= 0.5 && (bounds.h - ICON).abs() <= 0.5,
        "icon leaf {}x{} did not measure to natural {ICON}x{ICON}",
        bounds.w,
        bounds.h,
    );

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

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/icon_widget.bgra8")
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn icon_derives_a_group_semantics_node() {
    let mut store = NodeStore::new();
    let root = {
        let widget = icon(diamond(), ICON, ICON);
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        cx.root().expect("icon declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Group, "an Icon is a presentational Group");
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `image_widget.rs`.
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
    let (root, _icon_id) = build_scene(&mut store);

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
fn steady_icon_frame_is_allocation_free() {
    let mut h = setup_alloc();

    // Warm up until the frame path reaches steady state: the first frames grow
    // the persistent instance/mesh buffers to fit the scene, cache the
    // per-pipeline bind groups, and size the headless framebuffer/target pool.
    // Warm a few extra so the measured frames start from a fully grown, reused
    // state.
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
            "frame {i}: frame_stats changed for an unchanged Icon scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Icon scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Icon scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Icon scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "an Icon frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the Icon scene must emit draw calls");
    assert!(instances > 0, "the Icon scene must emit instances");
}
