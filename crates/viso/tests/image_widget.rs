//! Section 71 validation pack for the `Image` texture control, driven through
//! the public facade. `Image` is the next content control after `Label`; this
//! mirrors the `label_widget.rs` template plus the texture-fixture bypass
//! `content_scene.rs` uses:
//!
//! - **golden screenshot + measure** — build an `Image` with `Image::build`
//!   (which goes through the real `cx.image` content seam), backed by the shared
//!   deterministic checkerboard texture (integration tests have no image decode
//!   path, so they use the `viso-render` test fixtures — the same
//!   font/atlas-stable path `content_scene.rs` uses), lay it out, and confirm
//!   both that the `Fixed` leaf measured to the texture's intrinsic size and that
//!   the pixels match a blessed baseline;
//! - **a11y snapshot** — an `Image`'s derived semantics node is `Role::Group`
//!   (a presentational, non-interactive element; a dedicated `Role::Image` is a
//!   later slice);
//! - **allocation profile** — a warmed-up `Image` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `label_widget.rs`.
//!
//! Building through `Image::build` is the point: it proves the widget lowers to
//! a content-bearing leaf the `viso-ui` image seam and `paint_tree` handle
//! unchanged. Unlike `Label`, there is no shaping step — the texture is already
//! resident, so `cx.image` writes the `Content::Image` payload directly.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso::render::{FrameStats, Rect, Renderer, Rgba, test_texture};
use viso::ui::{
    Axis, BoxStyle, BuildCx, Component, Inset, NodeId, NodeStore, Role, Size, TextureId, Vec2,
    paint_tree,
};
use viso::widgets::{ViewStyle, image, view};

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

/// Upload the shared deterministic checkerboard texture and return its resident
/// id plus its intrinsic pixel size — the fixture bypass every content golden
/// uses so the baseline is stable without an image decode path.
fn upload_texture(gpu: &mut HeadlessRaster) -> (TextureId, Vec2) {
    let (tw, th, texels) = test_texture();
    let texture = gpu.create_texture(&TextureDesc {
        width: tw,
        height: th,
        format: TextureFormat::Bgra8Unorm,
        render_target: false,
        label: "image-checkerboard",
    });
    gpu.write_texture(texture, 0, 0, tw, th, &texels);
    (
        texture,
        Vec2 {
            x: tw as f32,
            y: th as f32,
        },
    )
}

/// Build a padded dark `View` wrapping a single `Image`, and return the root
/// plus the image's leaf. Wrapping in a container keeps the surface from being
/// filled by the leaf itself, so the image sits at its intrinsic size inside the
/// padding. The image is the container's only child, so its leaf is the root's
/// first child.
fn build_scene(store: &mut NodeStore, texture: TextureId, natural: Vec2) -> (NodeId, NodeId) {
    let container = view(ViewStyle {
        axis: Axis::Row,
        padding: Inset::all(10.0),
        size: Size::fill(),
        background: BoxStyle::solid(DARK),
        ..Default::default()
    })
    .children(move |cx| {
        image(texture, natural.x, natural.y).build(cx);
    });

    let root = {
        let mut cx = BuildCx::new(store);
        container.build(&mut cx);
        cx.root().expect("scene has a root")
    };
    let image_id = store
        .arena()
        .links(root)
        .and_then(|l| l.first_child)
        .expect("the view has the image leaf as its only child");
    (root, image_id)
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
fn image_fixed_leaf_measures_to_texture_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let (texture, natural) = upload_texture(&mut gpu);
    let mut store = NodeStore::new();
    let (root, image_id) = build_scene(&mut store, texture, natural);

    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    // The image leaf sized to the texture's intrinsic extent (default `Fixed`).
    let bounds = store.bounds(image_id);
    assert!(
        (bounds.w - natural.x).abs() <= 0.5 && (bounds.h - natural.y).abs() <= 0.5,
        "image leaf {}x{} did not measure to texture natural {}x{}",
        bounds.w,
        bounds.h,
        natural.x,
        natural.y
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/image_widget.bgra8")
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn image_derives_a_group_semantics_node() {
    let mut store = NodeStore::new();
    let root = {
        let widget = image(TextureId(0), 32.0, 32.0);
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        cx.root().expect("image declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Group, "an Image is a presentational Group");
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `label_widget.rs`.
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

    let (texture, natural) = upload_texture(&mut gpu);
    let mut store = NodeStore::new();
    let (root, _image_id) = build_scene(&mut store, texture, natural);

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
fn steady_image_frame_is_allocation_free() {
    let mut h = setup_alloc();

    // Warm up until the frame path reaches steady state: the first frames grow
    // the persistent instance buffers to fit the scene, cache the per-pipeline
    // bind groups, and size the headless framebuffer/target pool. Warm a few
    // extra so the measured frames start from a fully grown, reused state.
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
            "frame {i}: frame_stats changed for an unchanged Image scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Image scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Image scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Image scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "an Image frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the Image scene must emit draw calls");
    assert!(instances > 0, "the Image scene must emit instances");
}
