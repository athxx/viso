//! Steady-state renderer microbenchmarks and the hot-path allocation/dispatch
//! invariant (§7.1, §17.4).
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
    Border, FrameStats, GlyphRunDraw, Primitive, Quad, Rect, Renderer, Rgba, test_glyphs,
    test_scene, test_texture,
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
/// the checkerboard image texture and the A8 glyph coverage pool are created and
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

    // A8 glyph coverage pool + the assembled run.
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

/// The D0.4 large-scene sizes: a moderate grid the steady-state / hover / scroll
/// proofs run against, and a large one the criterion timing bench uses to show
/// the per-frame cost is O(dirty), not O(scene). Kept as counts so the grid
/// dimensions are derived, not hand-tuned.
const GRID_10K: usize = 10_000;
const GRID_100K: usize = 100_000;

/// A pure-quad grid of `count` solid rects laid out in a near-square lattice,
/// each an opaque colored tile with no border and no corner radius.
///
/// Deliberately quads only: the hover/scroll proofs need the quad pool to be the
/// sole participant in `sync`, so a single changed tile is provably one upload
/// range (§9.1) with no other family's traffic mixed in. The tiles are laid on a
/// fixed pitch starting at the origin; the surface stays [`W`]×[`H`] (most tiles
/// fall outside it, which is irrelevant — this measures the CPU data path
/// (lower + diff + coalesce + upload), not rasterizer coverage).
fn grid_scene(count: usize) -> Vec<Primitive> {
    let cols = (count as f64).sqrt().ceil() as usize;
    let mut scene = Vec::with_capacity(count);
    for i in 0..count {
        let col = (i % cols) as f32;
        let row = (i / cols) as f32;
        scene.push(Primitive::Quad(Quad {
            rect: Rect {
                x: col * 3.0,
                y: row * 3.0,
                w: 2.0,
                h: 2.0,
            },
            // A per-tile hue so adjacent tiles differ, keeping the diff honest
            // (a uniform fill would make a color change indistinguishable).
            color: Rgba {
                r: (i % 7) as f32 / 7.0,
                g: (i % 13) as f32 / 13.0,
                b: (i % 5) as f32 / 5.0,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        }));
    }
    scene
}

/// A [`Harness`] over a pure-quad [`grid_scene`] of `count` tiles. Same backend /
/// renderer / surface setup as [`setup`], but no image/glyph resources — the
/// grid needs none, so the quad pool is the only one that ever uploads.
fn setup_grid(count: usize) -> Harness {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);
    let scene = grid_scene(count);
    Harness {
        gpu,
        renderer,
        surface,
        scene,
    }
}

/// §9.1 proof — a paint-only change to one tile uploads exactly one coalesced
/// range of one instance, never the whole scene.
///
/// Warms two frames of a 10k-tile grid (frame one grows + fills the pool; frame
/// two is the clean steady baseline), asserts the steady frame uploads nothing,
/// then recolors a single tile and asserts the next `upload`:
///   - syncs exactly one range (`uploaded_ranges == 1`), not one per slot;
///   - uploads exactly one `QuadInstance`'s worth of bytes;
///   - bumps `dirty_primitives` by one and re-tessellates no path (a color change
///     is a paint-plane event — geometry/tessellation untouched, §8.4).
fn assert_hover_uploads_one_range() {
    let mut h = setup_grid(GRID_10K);
    frame(&mut h);
    frame(&mut h);

    // Steady baseline: an unchanged frame uploads nothing.
    h.renderer.upload(&mut h.gpu, &h.scene);
    let steady = h.renderer.frame_stats();
    assert_eq!(
        steady.uploaded_ranges, 0,
        "an unchanged 10k grid must upload zero ranges (§9.1)"
    );
    assert_eq!(
        steady.gpu_upload_bytes, 0,
        "an unchanged 10k grid must upload zero bytes (§9.1)"
    );

    // Hover: recolor exactly one tile in the middle of the scene.
    let target = GRID_10K / 2;
    let Primitive::Quad(q) = &mut h.scene[target] else {
        unreachable!("grid is pure quads");
    };
    q.color = Rgba {
        r: 0.123,
        g: 0.456,
        b: 0.789,
        a: 1.0,
    };

    h.renderer.upload(&mut h.gpu, &h.scene);
    let hover = h.renderer.frame_stats();

    assert_eq!(
        hover.uploaded_ranges, 1,
        "a one-tile paint change must coalesce to exactly one upload range, \
         not one per slot and not a full-scene re-upload (§9.1/§9.3)"
    );
    assert_eq!(
        hover.gpu_upload_bytes,
        size_of::<viso_render::QuadInstance>(),
        "a one-tile paint change must upload exactly one instance's bytes (§9.1)"
    );
    assert_eq!(
        hover.dirty_primitives, 1,
        "a one-tile paint change must dirty exactly one primitive (§8.4)"
    );
    assert_eq!(
        hover.path_tessellations, 0,
        "a paint-only change must re-tessellate nothing — quads never tessellate, \
         and no geometry plane moved (§8.4)"
    );
}

/// §8.7 proof — a scroll (every tile's position shifted) is a transform-plane
/// event that never re-tessellates and never grows a buffer.
///
/// Shifting `rect_pos` on every tile dirties the transform plane for all N, so
/// the frame re-uploads instances (position lives in the instance) — but it must
/// do so by rewriting existing slots, not by rebuilding/growing buffers, and it
/// must re-tessellate nothing (quads carry no tessellation; no geometry plane
/// moved). This is the §8.7 "transform ≠ layout/geometry" contract at the data
/// path: a scroll touches transform only.
fn assert_scroll_is_transform_only() {
    let mut h = setup_grid(GRID_10K);
    frame(&mut h);
    frame(&mut h);

    let buffers = h.gpu.buffer_count();

    // Scroll: shift every tile up-left by a whole pixel.
    for p in &mut h.scene {
        let Primitive::Quad(q) = p else {
            unreachable!("grid is pure quads");
        };
        q.rect.x -= 1.0;
        q.rect.y -= 1.0;
    }

    h.renderer.upload(&mut h.gpu, &h.scene);
    let scroll = h.renderer.frame_stats();

    assert_eq!(
        scroll.dirty_primitives, GRID_10K as u32,
        "a scroll moves every tile, so every primitive is dirty on the transform plane"
    );
    assert_eq!(
        scroll.path_tessellations, 0,
        "a transform-only change must re-tessellate nothing (§8.7)"
    );
    assert_eq!(
        scroll.instance_rebuilds, 0,
        "a scroll rewrites existing slots in place — no buffer is rebuilt/grown (§8.7/§9.1)"
    );
    assert_eq!(
        h.gpu.buffer_count(),
        buffers,
        "a scroll must not allocate a new GPU buffer (§17.4)"
    );
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
    // The first frame is also cold for the retained scene: every primitive is a
    // fresh append and reads as dirty, and the path is tessellated. A second
    // frame re-visits the same store slots unchanged, so its stats are the steady
    // baseline the loop below must reproduce.
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
        assert_eq!(
            h.gpu.retired_count(),
            0,
            "frame {i}: a resource was retired for an unchanged scene \
             (steady frames grow no buffer, so nothing is destroyed; and any \
             earlier retire must have drained as its frame completed)"
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
        ..
    } = stats;
    assert!(draw_calls > 0, "the test scene must emit draw calls");
    assert!(instances > 0, "the test scene must emit instances");
}

fn bench_steady_state(c: &mut Criterion) {
    assert_steady_state_is_allocation_free();
    // D0.4 gate: the persistent data path holds a local change to a local upload
    // (§9.1) and a scroll to the transform plane (§8.7), at 10k tiles.
    assert_hover_uploads_one_range();
    assert_scroll_is_transform_only();

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

    // D0.4 large-scene timing: at 100k tiles, a steady (unchanged) `upload` is the
    // diff-and-coalesce cost with zero uploads, and a one-tile hover `upload` is
    // that plus a single-instance write. Both must scale with the dirty set, not
    // the scene — this bench is the regression sentinel for that (§9.1). A leading
    // warm frame moves the pool past its one-time growth.
    let mut big = setup_grid(GRID_100K);
    frame(&mut big);
    frame(&mut big);

    c.bench_function("upload_100k_steady", |b| {
        b.iter(|| {
            big.renderer
                .upload(black_box(&mut big.gpu), black_box(&big.scene))
        });
    });

    // Recolor one tile before each measured iteration, so every iteration uploads
    // exactly one instance range against a 100k-tile scene.
    let hover_target = GRID_100K / 2;
    c.bench_function("upload_100k_hover", |b| {
        b.iter(|| {
            if let Primitive::Quad(q) = &mut big.scene[hover_target] {
                q.color.g = 1.0 - q.color.g;
            }
            big.renderer
                .upload(black_box(&mut big.gpu), black_box(&big.scene));
        });
    });
}

criterion_group!(benches, bench_steady_state);
criterion_main!(benches);
