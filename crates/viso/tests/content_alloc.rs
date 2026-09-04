//! Steady-state allocation invariant for the paint-all-primitives slice: adding
//! the `content_payload` column and painting text + image content must not cost
//! a per-frame heap allocation on the warmed-up frame path (AGENTS section 7.1,
//! hot-path contract; architecture section 47 widget performance contract).
//!
//! This is the UI-side counterpart to `crates/render/benches/renderer_steady_state.rs`,
//! which asserts the same invariant for a hand-assembled render `test_scene`.
//! Here the scene is a real retained `NodeStore` with a text leaf and an image
//! leaf, lowered every frame through `paint_tree` before `upload` + `submit`, so
//! the measurement covers the UI paint path (traversal + content lowering), not
//! just the renderer. The content column is cold `Option<Box<Content>>` storage,
//! mostly `None`, and must not introduce frame-to-frame heap traffic.
//!
//! An integration test compiles as its own binary, so the counting global
//! allocator installed here is isolated to this test and never sees another
//! test's or criterion's allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso::render::{FrameStats, GlyphInstanceData, Rect, Renderer, Rgba};
use viso::render::{test_glyphs, test_texture};
use viso::ui::{
    Axis, BoxStyle, BuildCx, Content, FlexStyle, Inset, LeafStyle, Length, NodeId, NodeStore, Size,
    Vec2, paint_tree,
};

/// A global allocator that counts heap allocations while `ARMED`, so a steady
/// frame's allocation behavior can be asserted directly. Off by default so the
/// test harness's own allocations are never counted.
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

const W: u32 = 160;
const H: u32 = 96;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

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

/// Everything a frame needs: the backend, the renderer, the surface, and the
/// built content-bearing tree. The image texture and glyph atlas are cold-path
/// one-time creations made before any steady-state measurement, so they are not
/// counted against the per-frame budget.
struct Harness {
    gpu: HeadlessRaster,
    renderer: Renderer,
    surface: viso::gpu::SurfaceId,
    store: NodeStore,
    root: NodeId,
    primitives: Vec<viso::render::Primitive>,
}

/// The intrinsic extent of the glyph run, so its `Fit` leaf sizes to it.
fn glyph_run_natural(glyphs: &[GlyphInstanceData]) -> Vec2 {
    let mut n = Vec2::ZERO;
    for g in glyphs {
        n.x = n.x.max(g.rect.x + g.rect.w);
        n.y = n.y.max(g.rect.y + g.rect.h);
    }
    n
}

/// A padded Row on a dark background: a `Fit` text leaf, then a fixed image
/// leaf. Returns the root plus the two content leaves.
fn build(store: &mut NodeStore) -> (NodeId, NodeId, NodeId) {
    let mut text_id = None;
    let mut image_id = None;
    let mut cx = BuildCx::new(store);
    cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: 12.0,
            padding: Inset::all(10.0),
            size: Size::fill(),
            style: BoxStyle::solid(DARK),
            ..Default::default()
        },
        |cx| {
            text_id = Some(
                cx.leaf(LeafStyle {
                    size: Size {
                        width: Length::Fit,
                        height: Length::Fit,
                    },
                    style: BoxStyle::NONE,
                })
                .id(),
            );
            image_id = Some(
                cx.leaf(LeafStyle {
                    size: Size::fixed(48.0, 48.0),
                    style: BoxStyle::NONE,
                })
                .id(),
            );
        },
    );
    let root = cx.root().expect("scene has a root");
    (root, text_id.unwrap(), image_id.unwrap())
}

/// Build the backend, upload the test scene's textures, attach content payloads,
/// and lay the tree out once. Mirrors `content_scene.rs::render_scene` up to the
/// point of drawing.
fn setup() -> Harness {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);

    let (tw, th, texels) = test_texture();
    let texture = gpu.create_texture(&TextureDesc {
        width: tw,
        height: th,
        format: TextureFormat::Bgra8Unorm,
        render_target: false,
        label: "alloc-checkerboard",
    });
    gpu.write_texture(texture, 0, 0, tw, th, &texels);

    let tg = test_glyphs([0.0, 0.0], 26.0);
    let atlas = gpu.create_texture(&TextureDesc {
        width: tg.atlas_size,
        height: tg.atlas_size,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "alloc-glyph-atlas",
    });
    gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);

    let mut store = NodeStore::new();
    let (root, text_id, image_id) = build(&mut store);

    store.set_content_payload(
        text_id,
        Content::Text {
            glyphs: tg.glyphs.clone(),
            atlas,
            color: WHITE,
            natural: glyph_run_natural(&tg.glyphs),
        },
    );
    store.set_content_payload(
        image_id,
        Content::Image {
            texture,
            uv: Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            tint: WHITE,
            natural: Vec2 {
                x: tw as f32,
                y: th as f32,
            },
        },
    );

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

/// Lower the tree through `paint_tree` into the reused primitive buffer, upload,
/// and submit one frame. Layout is stable across frames, so it is not re-run —
/// this is the steady paint path a warmed-up frame takes when only paint data is
/// re-emitted from a retained, laid-out tree.
fn frame(h: &mut Harness) {
    h.primitives.clear();
    paint_tree(&h.store, h.root, &mut h.primitives);
    h.renderer.upload(&mut h.gpu, &h.primitives);
    h.renderer
        .submit(&mut h.gpu, h.surface, CLEAR, [W as f32, H as f32]);
}

#[test]
fn steady_content_frame_is_allocation_free() {
    let mut h = setup();

    // Warm up: the first frame grows the persistent instance buffers to fit the
    // scene, caches the per-texture bind groups (checkerboard + glyph atlas), and
    // populates any offscreen pool. After this, a steady frame must reuse all of
    // it and re-emit the same primitives.
    frame(&mut h);
    // Grow the reused paint buffer to its steady capacity so a later
    // `paint_tree` into it does not reallocate.
    h.primitives.clear();
    paint_tree(&h.store, h.root, &mut h.primitives);

    let buffers = h.gpu.buffer_count();
    let textures = h.gpu.texture_count();
    let bind_groups = h.gpu.bind_group_count();
    let stats = h.renderer.frame_stats();

    // Per-frame heap-allocation counts for a full lowered frame, captured under
    // the counting allocator. In steady state the renderer's encode scratch and
    // this test's paint buffer are reused via `Vec::clear`, so the only heap
    // traffic left is the headless backend's fixed per-command instance-byte
    // copy — a constant that must not grow frame to frame.
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
            "frame {i}: frame_stats changed for an unchanged content scene \
             (draw-call/instance dispatch is not steady)"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged content scene \
             (persistent buffers must be reused)"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged content scene \
             (content textures are cold one-time creations)"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged content scene \
             (per-texture bind groups must be cached)"
        );
    }

    // Steady-state allocation invariant: two identical content frames must
    // allocate exactly the same amount. Any per-frame growth from the content
    // column — a stray clone of the glyph run, a reallocated paint buffer, a
    // per-frame box — would make the counts diverge.
    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a content frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch or content lowering is not \
         allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    // Sanity: the scene actually draws content, so the invariant is not
    // trivially satisfied by an empty frame. Text + image lower to a quad
    // background plus a glyph run plus an image, so both counters are positive.
    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the content scene must emit draw calls");
    assert!(instances > 0, "the content scene must emit instances");
}
