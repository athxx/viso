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
    GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureDesc, TextureFormat, TextureId,
};
use viso_render::{
    AnalyticCapsule, AnalyticCapsuleInstance, AnalyticEllipse, AnalyticEllipseInstance,
    AnalyticLine, AnalyticLineInstance, AnalyticRRect, AnalyticRRectInstance, Border, Corners,
    ExtendMode, FrameStats, GlyphRunDraw, Gradient, GradientKind, GradientStop, ImageDraw,
    InterpolationSpace, LineCap, LineJoin, Point, Primitive, Quad, Rect, Renderer, Rgba,
    SpriteRegion, test_glyphs, test_scene, test_texture,
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

/// Which analytic family a per-family grid is built from. Each grid is
/// homogeneous — one family only — so that family's pool is the sole participant
/// in `sync`, letting the hover proof pin the upload to exactly one instance of
/// that family's stride (mirroring the pure-quad proof's isolation).
#[derive(Debug, Clone, Copy)]
enum Family {
    RRect,
    Ellipse,
    Capsule,
    Line,
}

impl Family {
    /// The byte stride of this family's GPU instance — the exact upload a
    /// one-primitive hover must produce.
    fn instance_stride(self) -> usize {
        match self {
            Family::RRect => size_of::<AnalyticRRectInstance>(),
            Family::Ellipse => size_of::<AnalyticEllipseInstance>(),
            Family::Capsule => size_of::<AnalyticCapsuleInstance>(),
            Family::Line => size_of::<AnalyticLineInstance>(),
        }
    }

    /// Build one primitive of this family for grid cell `i` at pixel `(x, y)`,
    /// with the per-cell hue [`grid_scene`] uses so a recolor is observable.
    fn primitive(self, i: usize, x: f32, y: f32) -> Primitive {
        let color = Rgba {
            r: (i % 7) as f32 / 7.0,
            g: (i % 13) as f32 / 13.0,
            b: (i % 5) as f32 / 5.0,
            a: 1.0,
        };
        let rect = Rect {
            x,
            y,
            w: 2.0,
            h: 2.0,
        };
        match self {
            Family::RRect => Primitive::AnalyticRRect(AnalyticRRect {
                rect,
                color,
                radius: Corners::uniform(0.5),
                border: Border::NONE,
            }),
            Family::Ellipse => Primitive::AnalyticEllipse(AnalyticEllipse {
                rect,
                color,
                border: Border::NONE,
            }),
            Family::Capsule => Primitive::AnalyticCapsule(AnalyticCapsule {
                rect,
                color,
                border: Border::NONE,
            }),
            Family::Line => Primitive::AnalyticLine(AnalyticLine {
                p0: Point { x, y },
                p1: Point {
                    x: x + 2.0,
                    y: y + 2.0,
                },
                width: 1.0,
                color,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                miter_limit: 4.0,
                border: Border::NONE,
            }),
        }
    }

    /// Recolor a primitive of this family in place (a paint-plane change).
    fn recolor(self, p: &mut Primitive, color: Rgba) {
        match (self, p) {
            (Family::RRect, Primitive::AnalyticRRect(r)) => r.color = color,
            (Family::Ellipse, Primitive::AnalyticEllipse(e)) => e.color = color,
            (Family::Capsule, Primitive::AnalyticCapsule(c)) => c.color = color,
            (Family::Line, Primitive::AnalyticLine(l)) => l.color = color,
            _ => unreachable!("grid is homogeneous in its family"),
        }
    }

    /// Shift a primitive of this family up-left by a pixel (a transform-plane
    /// change) — a scroll.
    fn scroll(self, p: &mut Primitive) {
        match (self, p) {
            (Family::RRect, Primitive::AnalyticRRect(r)) => {
                r.rect.x -= 1.0;
                r.rect.y -= 1.0;
            }
            (Family::Ellipse, Primitive::AnalyticEllipse(e)) => {
                e.rect.x -= 1.0;
                e.rect.y -= 1.0;
            }
            (Family::Capsule, Primitive::AnalyticCapsule(c)) => {
                c.rect.x -= 1.0;
                c.rect.y -= 1.0;
            }
            (Family::Line, Primitive::AnalyticLine(l)) => {
                l.p0.x -= 1.0;
                l.p0.y -= 1.0;
                l.p1.x -= 1.0;
                l.p1.y -= 1.0;
            }
            _ => unreachable!("grid is homogeneous in its family"),
        }
    }
}

/// A homogeneous grid of `count` primitives of one analytic `family`, laid on the
/// same fixed pitch as [`grid_scene`]. Used by the per-family hover/scroll proofs
/// so each new analytic pool is exercised in isolation.
fn family_grid_scene(family: Family, count: usize) -> Vec<Primitive> {
    let cols = (count as f64).sqrt().ceil() as usize;
    let mut scene = Vec::with_capacity(count);
    for i in 0..count {
        let col = (i % cols) as f32;
        let row = (i / cols) as f32;
        scene.push(family.primitive(i, col * 3.0, row * 3.0));
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

/// A [`Harness`] over a homogeneous [`family_grid_scene`]. Same isolation as
/// [`setup_grid`]: no image/glyph resources, and one family only, so that
/// family's pool is the sole `sync` participant.
fn setup_family_grid(family: Family, count: usize) -> Harness {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);
    let scene = family_grid_scene(family, count);
    Harness {
        gpu,
        renderer,
        surface,
        scene,
    }
}

// ---------------------------------------------------------------------------
// D2 gate (§31 `## D2`): many gradients, image grid, sprite atlas, and
// texture-binding pressure. Each scene proves the same steady-state contract
// the D0/D1 gates prove for quads/analytics, plus the two D2-specific ones:
// an unchanged gradient bakes no LUT row (0 upload), and no image draw creates a
// per-primitive texture.
// ---------------------------------------------------------------------------

/// The D2 gradient/image grid size — smaller than the 10k analytic grids because
/// each cell here is heavier (a baked LUT row or a textured draw), but still large
/// enough that a whole-scene re-upload would dwarf a one-cell change.
const GRID_1K: usize = 1_000;

/// The distinct 3-stop gradient palette a [`gradient_grid_scene`] draws from.
/// Kept well under `GRADIENT_LUT_ROWS` (64) because a real UI reuses a bounded
/// gradient palette across many elements: many *gradient draws*, few distinct
/// *LUT rows*. A palette larger than the atlas would overflow and thrash the LUT
/// every frame — that is the overflow path, not steady state.
const LUT_PALETTE: usize = 16;

/// The 3-stop ramp for palette `slot` (`0..LUT_PALETTE`). A pure function of
/// `slot`, so two cells with the same slot produce byte-identical stops and
/// therefore the same [`LutKey`] — one shared baked row.
fn lut_palette_stops(slot: usize) -> Vec<GradientStop> {
    let t = slot as f32 / LUT_PALETTE as f32;
    let a = Rgba {
        r: t,
        g: 1.0 - t,
        b: 0.5,
        a: 1.0,
    };
    let mid = Rgba {
        r: 0.5,
        g: t,
        b: 1.0 - t,
        a: 1.0,
    };
    let b = Rgba {
        r: 1.0 - t,
        g: 0.5,
        b: t,
        a: 1.0,
    };
    vec![
        GradientStop {
            offset: 0.0,
            color: a,
        },
        GradientStop {
            offset: 0.5,
            color: mid,
        },
        GradientStop {
            offset: 1.0,
            color: b,
        },
    ]
}

/// A grid of `count` gradient fills laid on the same fixed pitch as
/// [`grid_scene`]. Every third cell is a 3-stop gradient (which bakes a LUT row);
/// the rest are inline 2-stop gradients (no LUT). The 3-stop cells cycle a fixed
/// palette of [`LUT_PALETTE`] distinct [`LutKey`]s, so the scene has many
/// gradient draws over a bounded, atlas-sized set of baked rows — the real "many
/// gradients" steady-state workload (§31 D2), not a one-ramp scene and not an
/// overflow thrash. `palette_shift` offsets the palette so a caller can compose
/// a distinct-but-still-bounded variant.
fn gradient_grid_scene(count: usize) -> Vec<Primitive> {
    let cols = (count as f64).sqrt().ceil() as usize;
    let mut scene = Vec::with_capacity(count);
    for i in 0..count {
        let col = (i % cols) as f32;
        let row = (i / cols) as f32;
        let rect = Rect {
            x: col * 3.0,
            y: row * 3.0,
            w: 2.0,
            h: 2.0,
        };
        let stops = if i % 3 == 0 {
            // A 3-stop gradient drawn from the bounded palette: its key depends
            // only on `slot`, so at most LUT_PALETTE distinct rows are baked no
            // matter how many such cells the grid has.
            let slot = (i / 3) % LUT_PALETTE;
            lut_palette_stops(slot)
        } else {
            // An inline 2-stop gradient: distinct per cell, but 2-stop gradients
            // take the inline fast path and bake no LUT row, so their variety
            // costs the atlas nothing.
            let a = Rgba {
                r: (i % 7) as f32 / 7.0,
                g: (i % 13) as f32 / 13.0,
                b: (i % 5) as f32 / 5.0,
                a: 1.0,
            };
            let b = Rgba {
                r: (i % 5) as f32 / 5.0,
                g: (i % 7) as f32 / 7.0,
                b: (i % 11) as f32 / 11.0,
                a: 1.0,
            };
            vec![
                GradientStop {
                    offset: 0.0,
                    color: a,
                },
                GradientStop {
                    offset: 1.0,
                    color: b,
                },
            ]
        };
        scene.push(Primitive::Gradient(Gradient {
            rect,
            kind: GradientKind::Linear,
            extend: ExtendMode::Clamp,
            p0: Point {
                x: rect.x,
                y: rect.y,
            },
            p1: Point {
                x: rect.x + rect.w,
                y: rect.y + rect.h,
            },
            stops,
            interp: InterpolationSpace::LinearRgb,
        }));
    }
    scene
}

/// A grid of `count` image draws, all sampling one shared `texture` with one
/// shared sampler. This is the "image grid" workload: because every draw shares
/// the same (texture, sampler), the whole grid batches into one image draw call
/// with a single texture binding — the renderer must not create a texture per
/// draw, and the binding set must not churn.
fn image_grid_scene(texture: TextureId, count: usize) -> Vec<Primitive> {
    let cols = (count as f64).sqrt().ceil() as usize;
    let mut scene = Vec::with_capacity(count);
    for i in 0..count {
        let col = (i % cols) as f32;
        let row = (i / cols) as f32;
        scene.push(Primitive::Image(ImageDraw::new(
            Rect {
                x: col * 3.0,
                y: row * 3.0,
                w: 2.0,
                h: 2.0,
            },
            texture,
        )));
    }
    scene
}

/// A grid of `count` sprite draws, each a distinct 1×1-texel sub-region of one
/// shared atlas `texture` (via [`SpriteRegion`], which inherits the half-texel
/// bleed guard), lowered to [`ImageDraw`]s. The "sprite atlas" workload: many
/// visually distinct sprites from a single texture and a single binding.
fn sprite_atlas_scene(texture: TextureId, atlas_size: [u32; 2], count: usize) -> Vec<Primitive> {
    let cols = (count as f64).sqrt().ceil() as usize;
    let cells = atlas_size[0].max(1);
    let mut scene = Vec::with_capacity(count);
    for i in 0..count {
        let col = (i % cols) as f32;
        let row = (i / cols) as f32;
        // Cycle through the atlas's texel cells so adjacent sprites differ.
        let cx = (i as u32 % cells) as f32;
        let cy = ((i as u32 / cells) % cells) as f32;
        let sprite = SpriteRegion::new(
            texture,
            atlas_size,
            Rect {
                x: cx,
                y: cy,
                w: 1.0,
                h: 1.0,
            },
            Rect {
                x: col * 3.0,
                y: row * 3.0,
                w: 2.0,
                h: 2.0,
            },
        );
        scene.push(Primitive::Image(sprite.to_image_draw()));
    }
    scene
}

/// A grid of `count` image draws spread across `textures.len()` distinct
/// textures (round-robin), each a separate binding. The "texture-binding
/// pressure" workload: distinct textures cannot share a bind group, so the
/// planner emits one image batch per contiguous run of the same texture. This
/// scene groups draws by texture so the binding count is bounded and known.
fn texture_pressure_scene(textures: &[TextureId], per_texture: usize) -> Vec<Primitive> {
    let total = textures.len() * per_texture;
    let cols = (total as f64).sqrt().ceil() as usize;
    let mut scene = Vec::with_capacity(total);
    let mut i = 0usize;
    for &texture in textures {
        for _ in 0..per_texture {
            let col = (i % cols) as f32;
            let row = (i / cols) as f32;
            scene.push(Primitive::Image(ImageDraw::new(
                Rect {
                    x: col * 3.0,
                    y: row * 3.0,
                    w: 2.0,
                    h: 2.0,
                },
                texture,
            )));
            i += 1;
        }
    }
    scene
}

/// A [`Harness`] whose scene is prebuilt, plus any textures it references so the
/// caller can mutate the scene. Same cold-path setup as [`setup`].
fn setup_scene(scene: Vec<Primitive>) -> Harness {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);
    Harness {
        gpu,
        renderer,
        surface,
        scene,
    }
}

/// Create and upload one BGRA8 checkerboard texture on `gpu`, returning its id.
fn upload_test_texture(gpu: &mut HeadlessRaster, label: &'static str) -> (TextureId, [u32; 2]) {
    let (tw, th, texels) = test_texture();
    let texture = gpu.create_texture(&TextureDesc {
        width: tw,
        height: th,
        format: TextureFormat::Bgra8Unorm,
        render_target: false,
        label,
    });
    gpu.write_texture(texture, 0, 0, tw, th, &texels);
    (texture, [tw, th])
}

/// §12/§31 D2 proof — "many gradients": an unchanged gradient grid bakes no LUT
/// row and uploads nothing, and recoloring one gradient re-bakes exactly that
/// gradient (one LUT row + one instance), never the whole scene.
fn assert_gradient_grid_steady_and_local() {
    let mut h = setup_scene(gradient_grid_scene(GRID_1K));
    frame(&mut h);
    frame(&mut h);

    let textures = h.gpu.texture_count();

    // Steady baseline: an unchanged gradient scene rebakes no LUT row, so it
    // uploads nothing (§31 D2: "0 gradient-LUT rebuild unless the gradient
    // changed").
    h.renderer.upload(&mut h.gpu, &h.scene);
    let steady = h.renderer.frame_stats();
    assert_eq!(
        steady.uploaded_ranges, 0,
        "an unchanged gradient grid must upload zero ranges — no LUT rebake, no \
         instance re-upload (§31 D2)"
    );
    assert_eq!(
        steady.gpu_upload_bytes, 0,
        "an unchanged gradient grid must upload zero bytes (§31 D2)"
    );
    assert_eq!(
        h.gpu.texture_count(),
        textures,
        "a steady gradient frame must create no texture — the LUT atlas is \
         persistent (§17.4)"
    );

    // Recolor one 3-stop gradient (i % 3 == 0 → has a LUT row) so the change
    // both re-bakes its LUT row and re-uploads its instance.
    let target = (GRID_1K / 2 / 3) * 3;
    let Primitive::Gradient(g) = &mut h.scene[target] else {
        unreachable!("every third cell is a gradient with a middle stop");
    };
    assert!(g.stops.len() >= 3, "target must be a LUT-baked gradient");
    g.stops[0].color = Rgba {
        r: 0.9,
        g: 0.1,
        b: 0.4,
        a: 1.0,
    };

    h.renderer.upload(&mut h.gpu, &h.scene);
    let changed = h.renderer.frame_stats();
    assert_eq!(
        changed.dirty_primitives, 1,
        "recoloring one gradient must dirty exactly one primitive (§8.4)"
    );
    assert!(
        changed.gpu_upload_bytes > 0,
        "a changed LUT gradient must upload its rebaked ramp + instance"
    );
    assert_eq!(
        h.gpu.texture_count(),
        textures,
        "recoloring a gradient rewrites the LUT atlas in place — no new texture \
         (§17.4)"
    );
}

/// §12.6/§31 D2 proof — "image grid" and "sprite atlas": a grid of image/sprite
/// draws over one shared texture is steady (0 upload unchanged), creates no
/// per-draw texture, and collapses to a single texture binding (the whole grid
/// shares one (texture, sampler), so the planner emits one binding, and a steady
/// frame's binding set never churns).
fn assert_image_grid_shares_one_binding(build: impl Fn(TextureId, [u32; 2]) -> Vec<Primitive>) {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);
    let (texture, size) = upload_test_texture(&mut gpu, "bench-image-grid");
    let scene = build(texture, size);
    let mut h = Harness {
        gpu,
        renderer,
        surface,
        scene,
    };

    frame(&mut h);
    frame(&mut h);

    let textures = h.gpu.texture_count();

    h.renderer.upload(&mut h.gpu, &h.scene);
    let steady = h.renderer.frame_stats();
    assert_eq!(
        steady.uploaded_ranges, 0,
        "an unchanged image grid must upload zero ranges (§9.1)"
    );
    assert_eq!(
        steady.gpu_upload_bytes, 0,
        "an unchanged image grid must upload zero bytes (§9.1)"
    );
    assert_eq!(
        h.gpu.texture_count(),
        textures,
        "an image grid must create no per-draw texture — every draw shares the \
         one uploaded texture (§31 D2: no per-primitive texture creation)"
    );
    // One shared (texture, sampler) ⇒ the image draws form one contiguous batch
    // with a single texture binding, entered once. The scene has no other
    // textured family, so there is exactly one binding switch (the entry) and no
    // churn between adjacent draws.
    assert_eq!(
        steady.texture_binding_switches, 1,
        "an image grid over one shared texture must bind that texture exactly \
         once, not once per draw (§16.2/§31)"
    );
}

/// §31 D2 proof — "texture-binding pressure": `n` distinct textures each backing
/// a run of image draws force exactly `n` texture bindings (one per texture, none
/// per draw), and the scene is still steady (0 upload unchanged) with no
/// per-draw texture creation.
fn assert_texture_pressure_binds_once_per_texture() {
    const N_TEX: usize = 8;
    const PER_TEX: usize = 32;

    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);

    let textures: Vec<TextureId> = (0..N_TEX)
        .map(|_| upload_test_texture(&mut gpu, "bench-texture-pressure").0)
        .collect();
    let scene = texture_pressure_scene(&textures, PER_TEX);
    let mut h = Harness {
        gpu,
        renderer,
        surface,
        scene,
    };

    frame(&mut h);
    frame(&mut h);

    let tex_count = h.gpu.texture_count();

    h.renderer.upload(&mut h.gpu, &h.scene);
    let steady = h.renderer.frame_stats();
    assert_eq!(
        steady.uploaded_ranges, 0,
        "an unchanged multi-texture grid must upload zero ranges (§9.1)"
    );
    assert_eq!(
        h.gpu.texture_count(),
        tex_count,
        "a steady multi-texture frame must create no texture (§17.4/§31 D2)"
    );
    // Draws are grouped by texture, so each texture is bound once for its run:
    // exactly N_TEX binding switches, bounded by the distinct-texture count, not
    // the draw count (N_TEX * PER_TEX).
    assert_eq!(
        steady.texture_binding_switches, N_TEX as u32,
        "distinct-texture draws must bind once per texture, not once per draw — \
         binding count scales with distinct textures (§31 D2)"
    );
}

/// §9.1 proof for an analytic `family`: a paint-only change to one primitive
/// uploads exactly one coalesced range of one instance of that family's stride,
/// never the whole scene — the same guarantee [`assert_hover_uploads_one_range`]
/// proves for quads, extended to each new analytic pool.
fn assert_family_hover_uploads_one_range(family: Family) {
    let mut h = setup_family_grid(family, GRID_10K);
    frame(&mut h);
    frame(&mut h);

    h.renderer.upload(&mut h.gpu, &h.scene);
    let steady = h.renderer.frame_stats();
    assert_eq!(
        steady.uploaded_ranges, 0,
        "{family:?}: an unchanged 10k grid must upload zero ranges (§9.1)"
    );
    assert_eq!(
        steady.gpu_upload_bytes, 0,
        "{family:?}: an unchanged 10k grid must upload zero bytes (§9.1)"
    );

    let target = GRID_10K / 2;
    family.recolor(
        &mut h.scene[target],
        Rgba {
            r: 0.123,
            g: 0.456,
            b: 0.789,
            a: 1.0,
        },
    );

    h.renderer.upload(&mut h.gpu, &h.scene);
    let hover = h.renderer.frame_stats();

    assert_eq!(
        hover.uploaded_ranges, 1,
        "{family:?}: a one-primitive paint change must coalesce to exactly one \
         upload range (§9.1/§9.3)"
    );
    assert_eq!(
        hover.gpu_upload_bytes,
        family.instance_stride(),
        "{family:?}: a one-primitive paint change must upload exactly one \
         instance's bytes (§9.1)"
    );
    assert_eq!(
        hover.dirty_primitives, 1,
        "{family:?}: a one-primitive paint change must dirty exactly one \
         primitive (§8.4)"
    );
    assert_eq!(
        hover.path_tessellations, 0,
        "{family:?}: analytic families never tessellate, and no geometry plane \
         moved (§8.4)"
    );
}

/// §8.7 proof for an analytic `family`: a scroll (every primitive shifted) is a
/// transform-plane event that re-tessellates nothing and grows no buffer.
fn assert_family_scroll_is_transform_only(family: Family) {
    let mut h = setup_family_grid(family, GRID_10K);
    frame(&mut h);
    frame(&mut h);

    let buffers = h.gpu.buffer_count();

    for p in &mut h.scene {
        family.scroll(p);
    }

    h.renderer.upload(&mut h.gpu, &h.scene);
    let scroll = h.renderer.frame_stats();

    assert_eq!(
        scroll.dirty_primitives, GRID_10K as u32,
        "{family:?}: a scroll moves every primitive, so every one is dirty on the \
         transform plane"
    );
    assert_eq!(
        scroll.path_tessellations, 0,
        "{family:?}: a transform-only change must re-tessellate nothing (§8.7)"
    );
    assert_eq!(
        scroll.instance_rebuilds, 0,
        "{family:?}: a scroll rewrites existing slots in place — no buffer is \
         rebuilt/grown (§8.7/§9.1)"
    );
    assert_eq!(
        h.gpu.buffer_count(),
        buffers,
        "{family:?}: a scroll must not allocate a new GPU buffer (§17.4)"
    );
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

    // D1 gate: each analytic family's pool holds the same two invariants in
    // isolation — a one-primitive hover uploads exactly one instance of that
    // family's stride, and a scroll is transform-only with no buffer growth.
    for family in [
        Family::RRect,
        Family::Ellipse,
        Family::Capsule,
        Family::Line,
    ] {
        assert_family_hover_uploads_one_range(family);
        assert_family_scroll_is_transform_only(family);
    }

    // D2 gate (§31 `## D2`): many gradients, image grid, sprite atlas, and
    // texture-binding pressure. Steady state rebakes no gradient LUT (0 upload),
    // creates no per-primitive texture, and binds once per distinct texture; a
    // one-gradient recolor rebakes exactly that gradient.
    assert_gradient_grid_steady_and_local();
    assert_image_grid_shares_one_binding(|tex, _size| image_grid_scene(tex, GRID_1K));
    assert_image_grid_shares_one_binding(|tex, size| sprite_atlas_scene(tex, size, GRID_1K));
    assert_texture_pressure_binds_once_per_texture();

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

    // D2 timings (§31 `## D2`). A steady `upload` of each workload is the
    // diff-and-coalesce cost with zero GPU work; the gradient recolor is the
    // one-LUT-row-rebake path. All scale with the dirty set, not the scene.
    let mut grad = setup_scene(gradient_grid_scene(GRID_1K));
    frame(&mut grad);
    frame(&mut grad);
    c.bench_function("gradient_grid_upload_steady", |b| {
        b.iter(|| {
            grad.renderer
                .upload(black_box(&mut grad.gpu), black_box(&grad.scene))
        });
    });

    // Recolor one LUT-baked gradient (i % 3 == 0) before each iteration, so each
    // measured upload is one LUT-row rebake plus one instance write.
    let grad_target = (GRID_1K / 2 / 3) * 3;
    c.bench_function("gradient_grid_recolor", |b| {
        b.iter(|| {
            if let Primitive::Gradient(g) = &mut grad.scene[grad_target] {
                g.stops[0].color.r = 1.0 - g.stops[0].color.r;
            }
            grad.renderer
                .upload(black_box(&mut grad.gpu), black_box(&grad.scene));
        });
    });

    // Image grid over one shared texture: steady upload with one texture binding.
    let mut img_gpu = HeadlessRaster::new();
    let img_surface = img_gpu.create_surface(RawWindowHandle::Headless, W, H);
    let img_format = img_gpu.surface_format(img_surface);
    let img_renderer = Renderer::new(&mut img_gpu, img_format);
    let (img_tex, _img_size) = upload_test_texture(&mut img_gpu, "bench-image-timing");
    let mut img = Harness {
        gpu: img_gpu,
        renderer: img_renderer,
        surface: img_surface,
        scene: image_grid_scene(img_tex, GRID_1K),
    };
    frame(&mut img);
    frame(&mut img);
    c.bench_function("image_grid_upload_steady", |b| {
        b.iter(|| {
            img.renderer
                .upload(black_box(&mut img.gpu), black_box(&img.scene))
        });
    });
}

criterion_group!(benches, bench_steady_state);
criterion_main!(benches);
