//! Section 71 validation pack for the `Label` static-text control, driven
//! through the public facade. `Label` is the first content control after
//! `View`; this mirrors the `view_widget.rs` template plus the content-payload
//! fixture bypass `content_scene.rs`/`content_alloc.rs` use:
//!
//! - **golden screenshot + measure** — build a `Label` with `Label::build`,
//!   attach a deterministic glyph run to its leaf (the facade `TextShaper` is
//!   `pub(crate)`, so integration tests shape via the shared `viso-render` test
//!   fixtures rather than a real font stack — the same font/atlas-stable path
//!   `content_scene.rs` uses), lay it out, and confirm both that the `Fit` leaf
//!   measured to the glyph run's natural extent and that the pixels match a
//!   blessed baseline;
//! - **a11y snapshot** — a `Label`'s derived semantics node is `Role::Label`
//!   and its accessible name is the visible text;
//! - **allocation profile** — a warmed-up `Label` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `view_widget.rs`.
//!
//! Building through `Label::build` is the point: it proves the widget lowers to
//! a content-bearing leaf the `viso-ui` text seam and `paint_tree` handle
//! unchanged.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso::render::{FrameStats, GlyphInstanceData, Rect, Renderer, Rgba, test_glyphs};
use viso::ui::{
    Axis, BoxStyle, BuildCx, Component, Content, Inset, NodeId, NodeStore, Role, Size, Vec2,
    paint_tree,
};
use viso::widgets::{ViewStyle, label, view};

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
const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// The intrinsic extent of the glyph run, so its `Fit` leaf sizes to it.
fn glyph_run_natural(glyphs: &[GlyphInstanceData]) -> Vec2 {
    let mut n = Vec2::ZERO;
    for g in glyphs {
        n.x = n.x.max(g.rect.x + g.rect.w);
        n.y = n.y.max(g.rect.y + g.rect.h);
    }
    n
}

/// Build a padded dark `View` wrapping a single `Label`, and return the root
/// plus the label's leaf. Wrapping in a container is what makes the label leaf
/// `Fit` mean "shrink to content" (a `Fit` node laid out *as the root* fills the
/// surface instead). The label is the container's only child, so its leaf is the
/// root's first child. The leaf is `Fit` on both axes, so once its deterministic
/// glyph run is attached it measures to the run's natural extent.
fn build_scene(store: &mut NodeStore) -> (NodeId, NodeId) {
    let container = view(ViewStyle {
        axis: Axis::Row,
        padding: Inset::all(10.0),
        size: Size::fill(),
        background: BoxStyle::solid(DARK),
        ..Default::default()
    })
    .children(|cx| {
        label("Hi").color(WHITE).build(cx);
    });

    let root = {
        let mut cx = BuildCx::new(store);
        container.build(&mut cx);
        cx.root().expect("scene has a root")
    };
    let label_id = store
        .arena()
        .links(root)
        .and_then(|l| l.first_child)
        .expect("the view has the label leaf as its only child");
    (root, label_id)
}

/// Attach the shared deterministic glyph run to the label's leaf and return the
/// atlas texture — the fixture bypass every content golden uses so the baseline
/// is font/atlas-stable without a real font stack.
fn attach_glyphs(gpu: &mut HeadlessRaster, store: &mut NodeStore, label_id: NodeId) -> Vec2 {
    let tg = test_glyphs([0.0, 0.0], 26.0);
    let atlas = gpu.create_texture(&TextureDesc {
        width: tg.atlas_size,
        height: tg.atlas_size,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "label-glyph-atlas",
    });
    gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);

    let natural = glyph_run_natural(&tg.glyphs);
    store.set_content_payload(
        label_id,
        Content::Text {
            glyphs: tg.glyphs.clone(),
            atlas,
            color: WHITE,
            natural,
        },
    );
    natural
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
fn label_fit_leaf_measures_to_glyph_run_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let (root, label_id) = build_scene(&mut store);
    let natural = attach_glyphs(&mut gpu, &mut store, label_id);

    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    // The Fit label leaf sized to the shaped run's natural extent.
    let bounds = store.bounds(label_id);
    assert!(
        (bounds.w - natural.x).abs() <= 0.5 && (bounds.h - natural.y).abs() <= 0.5,
        "Fit label leaf {}x{} did not measure to glyph natural {}x{}",
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/label_widget.bgra8")
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn label_derives_a_label_semantics_node_with_the_visible_text() {
    let mut store = NodeStore::new();
    let root = {
        let widget = label("Save");
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        cx.root().expect("label declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Label, "a Label is a Label role");
    assert_eq!(node.label.as_deref(), Some("Save"));
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `view_widget.rs`.
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
    let (root, label_id) = build_scene(&mut store);
    attach_glyphs(&mut gpu, &mut store, label_id);

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
fn steady_label_frame_is_allocation_free() {
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
            "frame {i}: frame_stats changed for an unchanged Label scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Label scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Label scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Label scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a Label frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the Label scene must emit draw calls");
    assert!(instances > 0, "the Label scene must emit instances");
}
