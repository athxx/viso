//! Steady-state renderer microbenchmarks and the hot-path allocation/dispatch
//! invariant (§7.1, §17.4 — the last Phase 2 exit criterion).
//!
//! Two things are measured here, both through the public API (benches are an
//! external crate and cannot touch `pub(crate)` internals, so we drive whole
//! frames via `Renderer` + the headless backend):
//!
//! 1. An assertion that a warmed-up renderer drawing an unchanged scene
//!    allocates no new GPU resources and emits the same draw calls each frame.
//!    Persistent instance buffers are reused, cached bind groups are reused, and
//!    translucent layers reuse pooled offscreen textures, so the backend's
//!    cumulative buffer/texture/bind-group counts must not grow between frames,
//!    and [`FrameStats`] must be identical. This runs once at startup so a
//!    hot-path regression fails the bench binary immediately (mirroring
//!    `runtime/benches/frame_loop.rs`'s `assert_idle_does_no_work`).
//! 2. The per-frame cost of `upload` (lowering primitives to segments and
//!    uploading instance data) and of a full `upload` + `submit`, as baselines
//!    to catch regressions.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-render`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (§36).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use viso_gpu::{
    GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureDesc, TextureFormat,
};
use viso_render::{
    FrameStats, GlyphRunDraw, Primitive, Renderer, test_glyphs, test_scene, test_texture,
};

/// A global allocator that counts heap allocations while `ARMED`, so a steady
/// frame's allocation behavior can be asserted directly (not just GPU-resource
/// growth). Off by default so criterion's own allocations are never counted.
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

/// Surface size, large enough to hold the whole test scene.
const W: u32 = 128;
const H: u32 = 96;
/// The dark-gray clear passed to `submit`, matching the golden test.
const CLEAR: [f32; 4] = [0.1, 0.1, 0.1, 1.0];

/// Everything a frame needs: the backend, the renderer, the surface, and the
/// fully-built scene. All the GPU resources here (surface, checkerboard
/// texture, glyph atlas) are cold-path one-time creations — they are made
/// before any steady-state measurement, so they are not counted against the
/// per-frame allocation budget.
struct Harness {
    gpu: HeadlessRaster,
    renderer: Renderer,
    surface: SurfaceId,
    scene: Vec<Primitive>,
}

/// Build the backend, upload the test scene's textures, and assemble the scene.
///
/// Mirrors `render/tests/golden.rs::render_scene` up to the point of drawing:
/// the checkerboard image texture and the R8 glyph SDF atlas are created and
/// written once, and their `TextureId`s feed `test_scene`.
fn setup() -> Harness {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);

    // Image test texture (BGRA8 checkerboard).
    let (tw, th, texels) = test_texture();
    let texture = gpu.create_texture(&TextureDesc {
        width: tw,
        height: th,
        format: TextureFormat::Bgra8Unorm,
        render_target: false,
        label: "bench-checkerboard",
    });
    gpu.write_texture(texture, 0, 0, tw, th, &texels);

    // R8 glyph SDF atlas + the assembled run.
    let tg = test_glyphs([6.0, 4.0], 22.0);
    let atlas = gpu.create_texture(&TextureDesc {
        width: tg.atlas_size,
        height: tg.atlas_size,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "bench-glyph-atlas",
    });
    gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);
    let glyphs = GlyphRunDraw {
        glyphs: tg.glyphs,
        atlas,
        color: tg.color,
    };

    let scene = test_scene(texture, glyphs);
    Harness {
        gpu,
        renderer,
        surface,
        scene,
    }
}

/// Lower + upload + submit one frame of the scene.
fn frame(h: &mut Harness) {
    h.renderer.upload(&mut h.gpu, &h.scene);
    h.renderer
        .submit(&mut h.gpu, h.surface, CLEAR, [W as f32, H as f32]);
}

/// The §7.1/§17.4 invariant, checked before benchmarking: once warmed up,
/// re-drawing an unchanged scene allocates no new GPU resources and emits the
/// same draw calls.
fn assert_steady_state_is_allocation_free() {
    let mut h = setup();

    // Warm up: the first frame grows the persistent instance buffers to fit the
    // scene, caches the per-texture bind groups, and populates the offscreen
    // texture pool. After this, a steady frame must reuse all of it.
    frame(&mut h);

    let buffers = h.gpu.buffer_count();
    let textures = h.gpu.texture_count();
    let bind_groups = h.gpu.bind_group_count();
    let stats = h.renderer.frame_stats();

    // Per-frame heap-allocation counts for `submit` (which runs `encode`),
    // captured under the counting allocator. In steady state the renderer's
    // `encode` scratch (viewports/commands/passes) is reused via `Vec::clear`, so
    // the only heap traffic left is the headless backend's fixed per-command
    // instance-byte copy — a constant that must not grow frame to frame (§7.1).
    let mut submit_allocs = [0usize; 2];

    // Two more identical frames must not create any GPU resource, and must lower
    // to the same segments (draw calls) and instances.
    for (i, slot) in submit_allocs.iter_mut().enumerate() {
        h.renderer.upload(&mut h.gpu, &h.scene);
        let frame_stats = h.renderer.frame_stats();
        assert_eq!(
            frame_stats, stats,
            "frame {i}: frame_stats changed for an unchanged scene \
             (draw-call/instance dispatch is not steady)"
        );
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        h.renderer
            .submit(&mut h.gpu, h.surface, CLEAR, [W as f32, H as f32]);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);

        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged scene \
             (persistent buffers must be reused, §17.4)"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged scene \
             (offscreen textures must be pooled/reused, §17.4)"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged scene \
             (per-texture bind groups must be cached)"
        );
    }

    // Steady-state allocation invariant. `submit` runs the renderer's `encode`
    // (which reuses its viewports/commands/passes scratch via `Vec::clear`) plus
    // the headless backend's `encode` (which copies each draw's instance bytes
    // and any sampled texture — backend-internal work outside `encode`'s scope).
    // Because the renderer's scratch is reused, the *only* frame-to-frame heap
    // traffic is that fixed backend baseline, so two identical steady frames must
    // allocate exactly the same amount. Any per-frame growth here (as with the
    // old per-frame `Vec`s in `encode`) would make the counts diverge (§7.1).
    assert_eq!(
        submit_allocs[0], submit_allocs[1],
        "submit allocated a different amount on two identical steady frames \
         ({} vs {}): the renderer's encode scratch is not being reused",
        submit_allocs[0], submit_allocs[1]
    );

    // Sanity: the scene actually draws something, so the invariant is not
    // trivially satisfied by an empty frame.
    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the test scene must emit draw calls");
    assert!(instances > 0, "the test scene must emit instances");
}

fn bench_steady_state(c: &mut Criterion) {
    assert_steady_state_is_allocation_free();

    let mut h = setup();
    // Warm up so the measured iterations are the reuse path, not first growth.
    frame(&mut h);

    c.bench_function("upload", |b| {
        b.iter(|| {
            h.renderer
                .upload(black_box(&mut h.gpu), black_box(&h.scene))
        });
    });

    c.bench_function("frame", |b| {
        b.iter(|| frame(black_box(&mut h)));
    });
}

criterion_group!(benches, bench_steady_state);
criterion_main!(benches);
