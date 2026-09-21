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
    AnalyticLine, AnalyticLineInstance, AnalyticRRect, AnalyticRRectInstance, AnalyticShadow,
    Blend, Border, ColorEffect, Corners, DashPattern, ExtendMode, FrameStats, GlyphRunDraw,
    Gradient, GradientKind, GradientStop, ImageDraw, InterpolationSpace, LayerClip, LineCap,
    LineJoin, Path, PathCmd, Point, Primitive, Quad, Rect, Renderer, Rgba, ShadowShape,
    SpriteRegion, Stroke, test_glyphs, test_scene, test_texture,
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

// ---------------------------------------------------------------------------
// C0.6 gate (§31 `## C0` compositing matrix): clip / layer / opacity / blend
// steady state, measured on the hot compositing path — `Primitive::Layer` +
// `Primitive::LayerEnd`. Two regimes, distinguished only by `opacity`:
//
// - `opacity == 1.0`: the layer is an in-pass hardware-scissor clip. A deep
//   nest of such layers must open *zero* offscreen passes, allocate no target,
//   and stay steady frame to frame (§14, §16.2).
// - `opacity < 1.0`: the `Layer..LayerEnd` subtree renders to an offscreen
//   target composited back as a modulated quad. Many translucent layers must
//   reuse a *pooled* target set — one pass per translucent layer, bounded
//   transient bytes, and **no** texture growth across steady frames (§17.4).
//
// These prove the compositing foundation pays only the cost its tier demands
// and never defaults to an offscreen — the whole point of C0 (§14).
// ---------------------------------------------------------------------------

/// A tile quad at pixel `(x, y)` with a per-index hue, the same shape a
/// [`grid_scene`] tile has so a layer's contents draw real geometry.
fn layer_tile(i: usize, x: f32, y: f32) -> Primitive {
    Primitive::Quad(Quad {
        rect: Rect {
            x,
            y,
            w: 2.0,
            h: 2.0,
        },
        color: Rgba {
            r: (i % 7) as f32 / 7.0,
            g: (i % 13) as f32 / 13.0,
            b: (i % 5) as f32 / 5.0,
            a: 1.0,
        },
        radius: 0.0,
        border: Border::NONE,
    })
}

/// A stack of `depth` nested `Layer(clip)` scopes, each at `opacity` and each
/// clip inset one pixel inside its parent, wrapping one tile per level. At
/// `opacity == 1.0` this is a deep in-pass scissor nest (no offscreen); at
/// `opacity < 1.0` every level is its own offscreen composite — the "deep clip"
/// and "nested opacity" rows of the §31 matrix, selected by `opacity`.
///
/// The innermost level carries a second, overlapping tile. Every outer level
/// already contains a nested layer, which makes its children's overlap unknowable
/// cheaply and so keeps its target; the innermost would otherwise hold a single
/// child and have its opacity folded into it (§3145), turning this fixture into a
/// measurement of `depth - 1` targets. The extra tile sits inside the first, so
/// the union — and every byte the nest allocates — is unchanged.
fn nested_layer_scene(depth: usize, opacity: f32) -> Vec<Primitive> {
    let mut scene = Vec::with_capacity(depth * 3);
    for level in 0..depth {
        let inset = level as f32;
        scene.push(Primitive::Layer(LayerClip {
            clip: Rect {
                x: inset,
                y: inset,
                w: (W as f32 - 2.0 * inset).max(1.0),
                h: (H as f32 - 2.0 * inset).max(1.0),
            },
            opacity,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }));
        scene.push(layer_tile(level, inset + 1.0, inset + 1.0));
        if level + 1 == depth {
            scene.push(Primitive::Quad(Quad {
                rect: Rect {
                    x: inset + 1.5,
                    y: inset + 1.5,
                    w: 1.0,
                    h: 1.0,
                },
                color: Rgba {
                    r: 0.2,
                    g: 0.7,
                    b: 0.4,
                    a: 1.0,
                },
                radius: 0.0,
                border: Border::NONE,
            }));
        }
    }
    for _ in 0..depth {
        scene.push(Primitive::LayerEnd);
    }
    scene
}

/// `count` sibling `Layer(clip)` scopes laid on a fixed pitch, each at `opacity`
/// and each wrapping one tile — the "blend stress" / many-layers row: a flat run
/// of independent compositing scopes rather than one deep nest, so the offscreen
/// pool is exercised in breadth (many concurrent same-size targets) at
/// `opacity < 1.0`.
fn sibling_layer_scene(count: usize, opacity: f32) -> Vec<Primitive> {
    blurred_sibling_layer_scene(count, opacity, 0.0)
}

/// [`sibling_layer_scene`] with each layer's content blurred at `sigma`: the
/// same flat run of small ROIs, but every scope now needs an offscreen target
/// *plus* its ladder's scratch. At `sigma > 0` this is the "many small-ROI
/// blurs" row of the §31 matrix.
///
/// Each scope wraps a tile *and* a smaller tile inside it. The second child is
/// what keeps the row measuring targets: a translucent group over one child has
/// provably non-overlapping children, so the planner would push its opacity down
/// and eliminate the layer entirely (§3145). Two overlapping children is the case
/// the group genuinely exists for, and because the inner tile is contained the
/// ROI, the target bytes, and the pool behaviour are exactly the single tile's.
fn blurred_sibling_layer_scene(count: usize, opacity: f32, sigma: f32) -> Vec<Primitive> {
    let cols = (count as f64).sqrt().ceil() as usize;
    let mut scene = Vec::with_capacity(count * 3);
    for i in 0..count {
        let col = (i % cols) as f32;
        let row = (i / cols) as f32;
        scene.push(Primitive::Layer(LayerClip {
            clip: Rect {
                x: col * 4.0,
                y: row * 4.0,
                w: 3.0,
                h: 3.0,
            },
            opacity,
            blur_sigma: sigma,
            backdrop_sigma: 0.0,
        }));
        scene.push(layer_tile(i, col * 4.0 + 0.5, row * 4.0 + 0.5));
        scene.push(Primitive::Quad(Quad {
            rect: Rect {
                x: col * 4.0 + 1.0,
                y: row * 4.0 + 1.0,
                w: 1.0,
                h: 1.0,
            },
            color: Rgba {
                r: 0.2,
                g: 0.7,
                b: 0.4,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        }));
        scene.push(Primitive::LayerEnd);
    }
    scene
}

/// C0.6 proof — a deep `opacity == 1.0` clip nest is pure in-pass scissoring: it
/// opens **no** offscreen pass, allocates no target, and holds an unchanged
/// frame steady (0 upload, 0 texture growth, identical [`FrameStats`]). This is
/// the §31 "deep Rect clip" row — clip depth must never escalate to an
/// offscreen (§14/§16.2).
fn assert_deep_clip_nest_opens_no_offscreen() {
    const DEPTH: usize = 32;
    let mut h = setup_scene(nested_layer_scene(DEPTH, 1.0));
    frame(&mut h);
    frame(&mut h);

    let textures = h.gpu.texture_count();

    h.renderer.upload(&mut h.gpu, &h.scene);
    let steady = h.renderer.frame_stats();
    assert_eq!(
        steady.offscreen_passes, 0,
        "a deep opacity==1 clip nest must open zero offscreen passes — it clips \
         in-pass with a hardware scissor, never an offscreen target (§14/§16.2)"
    );
    assert_eq!(
        steady.transient_target_bytes, 0,
        "an in-pass clip nest allocates no transient target bytes"
    );
    assert_eq!(
        steady.uploaded_ranges, 0,
        "an unchanged clip nest must upload zero ranges (§9.1)"
    );
    assert_eq!(
        steady.gpu_upload_bytes, 0,
        "an unchanged clip nest must upload zero bytes (§9.1)"
    );
    assert_eq!(
        h.gpu.texture_count(),
        textures,
        "a steady clip nest must create no texture — no offscreen is opened \
         (§17.4)"
    );
}

/// C0.6 proof — translucent layers pay one offscreen pass each, but reuse a
/// **pooled** target set: across steady frames the backend's texture count does
/// not grow, transient bytes are bounded by the (bounded) live-pass set, and
/// [`FrameStats`] is identical frame to frame. This is the §31 "nested opacity /
/// blend stress" row — the offscreen cost is paid by tier and reused, never
/// re-allocated per frame (§17.4).
fn assert_translucent_layers_reuse_pooled_targets(
    build: impl Fn() -> Vec<Primitive>,
    layers: usize,
) {
    let mut h = setup_scene(build());
    // First frame grows the offscreen pool to fit the live layers; the second is
    // the steady baseline every later frame must reproduce with no growth.
    frame(&mut h);
    frame(&mut h);

    let textures = h.gpu.texture_count();
    let baseline = h.renderer.frame_stats();

    assert_eq!(
        baseline.offscreen_passes, layers,
        "each translucent layer opens exactly one offscreen pass — bounded by \
         the layer count, not the frame index (§14.5)"
    );
    assert!(
        baseline.transient_target_bytes > 0,
        "translucent layers must allocate transient target bytes"
    );

    // Two more identical frames must reuse the pooled targets: no new texture,
    // no upload, and byte-identical stats.
    for i in 0..2 {
        h.renderer.upload(&mut h.gpu, &h.scene);
        let steady = h.renderer.frame_stats();
        assert_eq!(
            steady, baseline,
            "frame {i}: frame_stats changed for an unchanged translucent scene \
             (offscreen passes/bytes are not steady)"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a translucent frame created a texture — offscreen \
             targets must be pooled/reused across frames, not re-allocated \
             (§17.4)"
        );
        assert_eq!(
            steady.uploaded_ranges, 0,
            "frame {i}: an unchanged translucent scene must upload zero ranges \
             (§9.1)"
        );
    }
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

/// A grid of filled + stroked vector paths. Each cell is a closed convex outline
/// (a small pentagon) placed at its cell origin; position lives in the `cmds`, so
/// a scroll is a whole-scene translation of every path.
fn path_grid_scene(count: usize) -> Vec<Primitive> {
    let cols = (count as f32).sqrt().ceil() as usize;
    let step = 12.0;
    let mut scene = Vec::with_capacity(count);
    let fill = Rgba::new(0.2, 0.6, 0.35, 1.0);
    let stroke = Rgba::new(0.05, 0.1, 0.08, 1.0);
    for i in 0..count {
        let cx = (i % cols) as f32 * step + 4.0;
        let cy = (i / cols) as f32 * step + 4.0;
        scene.push(Primitive::Path(Path {
            cmds: vec![
                PathCmd::MoveTo(Point::new(cx + 4.0, cy)),
                PathCmd::LineTo(Point::new(cx + 8.0, cy + 3.0)),
                PathCmd::LineTo(Point::new(cx + 6.0, cy + 8.0)),
                PathCmd::LineTo(Point::new(cx + 2.0, cy + 8.0)),
                PathCmd::LineTo(Point::new(cx, cy + 3.0)),
                PathCmd::Close,
            ],
            fill: Some(fill),
            shadow: None,
            stroke: Some(Stroke::new(1.0, stroke)),
        }));
    }
    scene
}

/// §13.4 proof — the retained vector-mesh lane. A grid of filled+stroked paths is
/// tessellated once; thereafter an unchanged frame re-tessellates nothing and
/// uploads nothing, a scroll (whole-scene translation) reuses the cached geometry
/// and re-tessellates nothing, and a recolor re-tessellates nothing (color is
/// applied at lowering, not baked into the retained geometry).
fn assert_path_grid_is_retained() {
    const N: usize = 256;
    let mut h = setup_scene(path_grid_scene(N));
    // Frame one tessellates + grows the mesh pools; frame two is the clean steady
    // baseline (all geometry cached, shadow diff settled).
    frame(&mut h);
    frame(&mut h);

    let buffers = h.gpu.buffer_count();

    // Steady baseline: an unchanged path grid re-tessellates nothing and uploads
    // nothing (geometry + paint + transform all cached, §13.4 F4 steady state).
    h.renderer.upload(&mut h.gpu, &h.scene);
    let steady = h.renderer.frame_stats();
    assert_eq!(
        steady.path_tessellations, 0,
        "an unchanged path grid must re-tessellate nothing (§13.4)"
    );
    assert_eq!(
        steady.uploaded_ranges, 0,
        "an unchanged path grid must upload zero ranges (§13.4 F4 steady state)"
    );
    assert_eq!(
        steady.gpu_upload_bytes, 0,
        "an unchanged path grid must upload zero bytes (§13.4 F4 steady state)"
    );

    // Scroll: shift every path by a constant vector. Position lives in the cmds,
    // so this is a whole-scene translation — transform-only, geometry reused.
    for p in &mut h.scene {
        let Primitive::Path(path) = p else {
            unreachable!("grid is pure paths");
        };
        for cmd in &mut path.cmds {
            match cmd {
                PathCmd::MoveTo(pt) | PathCmd::LineTo(pt) => {
                    pt.x -= 1.0;
                    pt.y -= 1.0;
                }
                _ => {}
            }
        }
    }
    h.renderer.upload(&mut h.gpu, &h.scene);
    let scroll = h.renderer.frame_stats();
    assert_eq!(
        scroll.dirty_primitives, N as u32,
        "a scroll moves every path, so every one is dirty on the transform plane"
    );
    assert_eq!(
        scroll.path_tessellations, 0,
        "a transform-only change must re-tessellate nothing — the cached geometry \
         is reused, only the per-primitive transform moves (§13.4)"
    );
    assert_eq!(
        h.gpu.buffer_count(),
        buffers,
        "a scroll must not allocate a new GPU buffer (§17.4)"
    );

    // Recolor every path's fill. Color is applied at lowering over the cached
    // colorless geometry, so a recolor re-tessellates nothing.
    for p in &mut h.scene {
        let Primitive::Path(path) = p else {
            unreachable!("grid is pure paths");
        };
        path.fill = Some(Rgba::new(0.8, 0.2, 0.1, 1.0));
    }
    h.renderer.upload(&mut h.gpu, &h.scene);
    let recolor = h.renderer.frame_stats();
    assert_eq!(
        recolor.path_tessellations, 0,
        "a paint-only change must re-tessellate nothing — color is applied at \
         lowering, not baked into the retained geometry (§13.4)"
    );
}

/// A grid of curved, SVG-shaped paths — the geometry an `svg::parse_svg` scene
/// lowers to (closed outlines with quadratic + cubic segments, a fill and a
/// stroke). Each cell is a rounded-blob outline built from curve commands, so
/// the tessellator actually flattens curves (unlike the straight-edged
/// pentagon grid) — the realistic shape for the "small stable SVG-like paths"
/// and "large static path scene" gate scenarios.
fn svg_like_path_scene(count: usize) -> Vec<Primitive> {
    let cols = (count as f32).sqrt().ceil() as usize;
    let step = 16.0;
    let mut scene = Vec::with_capacity(count);
    let fill = Rgba::new(0.25, 0.5, 0.7, 1.0);
    let stroke = Rgba::new(0.02, 0.06, 0.1, 1.0);
    for i in 0..count {
        let x = (i % cols) as f32 * step + 4.0;
        let y = (i / cols) as f32 * step + 4.0;
        // A closed blob: cubic top, quadratic right, cubic bottom, line close.
        scene.push(Primitive::Path(Path {
            cmds: vec![
                PathCmd::MoveTo(Point::new(x, y + 5.0)),
                PathCmd::CubicTo(
                    Point::new(x, y),
                    Point::new(x + 10.0, y),
                    Point::new(x + 10.0, y + 5.0),
                ),
                PathCmd::QuadTo(Point::new(x + 10.0, y + 9.0), Point::new(x + 5.0, y + 10.0)),
                PathCmd::CubicTo(
                    Point::new(x + 2.0, y + 10.0),
                    Point::new(x, y + 8.0),
                    Point::new(x, y + 5.0),
                ),
                PathCmd::Close,
            ],
            fill: Some(fill),
            shadow: None,
            stroke: Some(Stroke::new(1.0, stroke)),
        }));
    }
    scene
}

/// A grid of open, dashed, thick-stroked polylines — the "stroke/dash heavy"
/// gate scenario. No fill; every path carries a multi-run dash pattern with a
/// phase offset, so stroke-geometry generation (dash expansion + caps/joins) is
/// the dominant tessellation cost.
fn dashed_stroke_scene(count: usize) -> Vec<Primitive> {
    let cols = (count as f32).sqrt().ceil() as usize;
    let step = 20.0;
    let mut scene = Vec::with_capacity(count);
    let color = Rgba::new(0.9, 0.4, 0.1, 1.0);
    for i in 0..count {
        let x = (i % cols) as f32 * step + 4.0;
        let y = (i / cols) as f32 * step + 4.0;
        let mut stroke = Stroke::new(3.0, color);
        stroke.cap = LineCap::Round;
        stroke.join = LineJoin::Round;
        stroke.dash = Some(DashPattern::new(&[6.0, 3.0, 2.0, 3.0], 1.5));
        scene.push(Primitive::Path(Path {
            cmds: vec![
                PathCmd::MoveTo(Point::new(x, y)),
                PathCmd::LineTo(Point::new(x + 12.0, y + 2.0)),
                PathCmd::LineTo(Point::new(x + 4.0, y + 10.0)),
                PathCmd::LineTo(Point::new(x + 14.0, y + 12.0)),
            ],
            fill: None,
            shadow: None,
            stroke: Some(stroke),
        }));
    }
    scene
}

/// §31 `## D3` gate — small stable SVG-like paths and stroke/dash-heavy paths.
/// Both are curve/dash workloads that tessellate once on the cold frame; a
/// subsequent unchanged frame must re-tessellate nothing and upload nothing
/// (the retained vector-mesh + stroke-geometry caches hold, §13.4).
fn assert_curve_and_dash_scenes_are_retained() {
    for (label, scene) in [
        ("svg-like", svg_like_path_scene(64)),
        ("dashed-stroke", dashed_stroke_scene(64)),
    ] {
        let mut h = setup_scene(scene);
        // Cold frame tessellates + grows pools; second frame settles the diff.
        frame(&mut h);
        let cold = h.renderer.frame_stats();
        assert!(
            cold.path_tessellations > 0,
            "{label}: the cold frame must tessellate the curved/dashed paths"
        );
        frame(&mut h);

        h.renderer.upload(&mut h.gpu, &h.scene);
        let steady = h.renderer.frame_stats();
        assert_eq!(
            steady.path_tessellations, 0,
            "{label}: an unchanged curve/dash scene must re-tessellate nothing (§13.4)"
        );
        assert_eq!(
            steady.uploaded_ranges, 0,
            "{label}: an unchanged curve/dash scene must upload zero ranges (§13.4)"
        );
        assert_eq!(
            steady.gpu_upload_bytes, 0,
            "{label}: an unchanged curve/dash scene must upload zero bytes (§13.4)"
        );
    }
}

/// §31 `## D3` gate — path churn. When a subset of paths change geometry each
/// frame, re-tessellation must scale with the churn set, not the whole scene:
/// the cache holds the untouched paths and only the mutated ones re-tessellate.
fn assert_path_churn_is_local() {
    const N: usize = 256;
    const CHURN: usize = 8;
    let mut h = setup_scene(svg_like_path_scene(N));
    frame(&mut h);
    frame(&mut h);

    // Steady baseline: nothing changed, nothing re-tessellates.
    h.renderer.upload(&mut h.gpu, &h.scene);
    assert_eq!(
        h.renderer.frame_stats().path_tessellations,
        0,
        "baseline: an unchanged scene re-tessellates nothing"
    );

    // Churn the first CHURN paths' *shape* (deform one anchor, not a uniform
    // translate — the cache fingerprint is translation-invariant, so only a
    // relative-shape change re-tessellates). The other N-CHURN stay cache hits.
    for p in h.scene.iter_mut().take(CHURN) {
        let Primitive::Path(path) = p else {
            unreachable!("scene is pure paths");
        };
        if let Some(PathCmd::CubicTo(c0, _, _)) = path.cmds.get_mut(1) {
            c0.x += 0.5;
            c0.y -= 0.5;
        }
    }
    h.renderer.upload(&mut h.gpu, &h.scene);
    let churn = h.renderer.frame_stats();
    assert_eq!(
        churn.path_tessellations, CHURN as u32,
        "path churn must re-tessellate exactly the changed paths, not the scene \
         ({CHURN} of {N} — the cache holds the rest, §13.4)"
    );
}

// ---------------------------------------------------------------------------
// E0.2 gate (§15.3 / §20.1): the fusion decision for a decorated shape
// (`shadow + fill + border`). The phase is benchmark-gated — a fused
// `DecoratedShape` pipeline is built only if the register-pressure / overdraw
// measurement justifies it. This gate measures the half that is observable in a
// headless CPU backend (draw-call / batch / pipeline-switch structure and the
// shaded-quad-area overdraw proxy) and records the verdict; the register-pressure
// / shaded-pixel-time half is a real-device measurement `FrameStats` +
// `HeadlessRaster` cannot express and is flagged, not asserted (§7.3).
// ---------------------------------------------------------------------------

/// The decorated-card grid size for the fusion gate: enough cards that the
/// per-card batch/switch structure is unambiguous, small enough to stay a
/// startup assertion.
const DECORATED_CARDS: usize = 256;

/// A grid of `count` decorated cards. Each card is a soft drop shadow drawn
/// *under* a rounded rect that carries both a fill and a border — the exact
/// `shadow + fill + border` triple §15.3 asks about. The shadow and the rrect
/// are emitted in paint order (shadow first) so the planner may not reorder them
/// across the family barrier: this is the *separate-draw* baseline the fusion
/// decision is measured against.
///
/// Deliberately no pure `Rect`: §15.3's hard constraint keeps a plain rect on the
/// shorter Quad pipeline, so it never enters the decorated path and is not part
/// of this measurement.
fn decorated_card_scene(count: usize) -> Vec<Primitive> {
    let cols = (count as f64).sqrt().ceil() as usize;
    let mut scene = Vec::with_capacity(count * 2);
    for i in 0..count {
        let col = (i % cols) as f32;
        let row = (i / cols) as f32;
        let rect = Rect {
            x: col * 8.0 + 4.0,
            y: row * 8.0 + 4.0,
            w: 4.0,
            h: 3.0,
        };
        let color = Rgba {
            r: (i % 7) as f32 / 7.0,
            g: (i % 13) as f32 / 13.0,
            b: (i % 5) as f32 / 5.0,
            a: 1.0,
        };
        let radius = Corners::uniform(0.75);
        // Shadow under the card: offset down-right, a soft blur, drawn first so it
        // sits below the fill in paint order.
        scene.push(Primitive::AnalyticShadow(AnalyticShadow {
            rect,
            color: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.35,
            },
            radius,
            offset: [0.5, 0.75],
            sigma: 1.0,
            spread: 0.0,
            shape: ShadowShape::RoundedBox,
            inner: false,
        }));
        // The card itself: fill + border in one rrect draw (the rrect family
        // already fuses fill and border in its fragment).
        scene.push(Primitive::AnalyticRRect(AnalyticRRect {
            rect,
            color,
            radius,
            border: Border {
                width: 0.5,
                color: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
            },
        }));
    }
    scene
}

/// The device-pixel footprint of one shadow's expanded instance quad, matching
/// the shader's vertex pad: `reach = 3*sigma + max(spread,0) + 1`, and the quad
/// spans `size + 2*(reach + |offset|)` on each axis. This is the overdraw proxy
/// — every fill/border pixel of a fused draw would be shaded through a fragment
/// this large, versus the tight rrect quad of the separate path.
fn shadow_quad_area(s: &AnalyticShadow) -> f32 {
    let reach = 3.0 * s.sigma + s.spread.max(0.0) + 1.0;
    let pad_x = reach + s.offset[0].abs();
    let pad_y = reach + s.offset[1].abs();
    (s.rect.w + 2.0 * pad_x) * (s.rect.h + 2.0 * pad_y)
}

/// E0.2 fusion gate. The decorated-card grid draws `shadow + fill + border` as
/// separate analytic draws today; this pins the structure the fusion decision
/// turns on and records the verdict.
///
/// Observable here (asserted): a shadow and its rrect are different families, so
/// the planner cannot merge across the per-card barrier — the separate path is
/// `2N` draws with a pipeline switch on every draw. A fused `DecoratedShape`
/// family would collapse that to `N` mergeable draws with a single switch.
///
/// Also observable (measured, not asserted): the shadow's expanded quad is far
/// larger than the tight fill quad, so a fused fragment would shade the whole
/// `shadow + fill + border` body over that larger area for every card. The area
/// ratio is the overdraw proxy.
///
/// NOT observable here (the other half of the §20.1 gate): real-Metal shader
/// register pressure and shaded-pixel time on device. `FrameStats` has no
/// overdraw or GPU-timing counter and `HeadlessRaster` is a CPU rasterizer, so
/// whether the fused fragment's extra ALU + the larger shaded area actually beat
/// the separate path's extra draw/switch cost is a device measurement, not a
/// headless one (§7.3). The fused pipeline is therefore deferred until that
/// measurement exists; this gate lands the measurable half and the barrier proof.
fn assert_decorated_fusion_gate() {
    let scene = decorated_card_scene(DECORATED_CARDS);
    let mut h = setup_scene(scene);
    frame(&mut h);
    frame(&mut h);

    let stats = h.renderer.frame_stats();

    // Separate-draw structure: one shadow + one rrect per card, neither mergeable
    // into the other's family, and the two families strictly alternate in paint
    // order — so every draw is a family transition.
    assert_eq!(
        stats.draw_calls,
        DECORATED_CARDS * 2,
        "decorated cards draw as separate shadow + rrect draws (a fused family \
         would be {DECORATED_CARDS})"
    );
    assert_eq!(
        stats.batches,
        DECORATED_CARDS * 2,
        "the shadow/rrect family barrier blocks merging: 2 batches per card"
    );
    assert_eq!(
        stats.pipeline_switches,
        (DECORATED_CARDS * 2) as u32,
        "alternating families switch the pipeline on every draw (a fused family \
         would switch once)"
    );

    // Overdraw proxy: the shadow's expanded quad dwarfs the tight fill quad, so a
    // fused fragment shades the decorated body over a much larger area. Assert the
    // proxy is real and large enough that the tradeoff is non-trivial — the exact
    // break-even is a device measurement, not this ratio.
    let card = AnalyticShadow {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            w: 4.0,
            h: 3.0,
        },
        color: Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.35,
        },
        radius: Corners::uniform(0.75),
        offset: [0.5, 0.75],
        sigma: 1.0,
        spread: 0.0,
        shape: ShadowShape::RoundedBox,
        inner: false,
    };
    let fill_area = card.rect.w * card.rect.h;
    let overdraw_ratio = shadow_quad_area(&card) / fill_area;
    assert!(
        overdraw_ratio > 2.0,
        "the shadow quad must be materially larger than the fill quad for the \
         overdraw tradeoff to matter (proxy ratio {overdraw_ratio:.1}x)"
    );
}

// ---------------------------------------------------------------------------
// E0.4 — §31 shadow gate. Two proofs the analytic-shadow fast lane scales the
// way §15/§20 promises, both through observable headless counters.
//
// 1. `assert_analytic_shadow_lane_scales`: a grid of 1k analytic shadows. Every
//    shadow is one instance on the single shared shadow family, so the whole
//    grid collapses to one mergeable batch behind one pipeline (no blur target,
//    no per-shadow offscreen, no family transition), and a warmed steady frame
//    uploads nothing and tessellates nothing.
// 2. `assert_path_shadow_reuse_is_local`: the general-path shadow's cached
//    coverage mask is keyed on {geometry, sigma, spread}, so a frame that
//    changes only the shadow's color/offset rebuilds no mask, while a frame
//    that changes the path's geometry rebuilds exactly its two slots. This is
//    the "reused when only color/offset change" contract of §15.4.
//
// NOT observable here (flagged, not asserted): real-device shaded-pixel time
// for a thousand overlapping soft shadows, and the visual quality of the sharp
// E0 path-shadow mask vs a true blurred one. `FrameStats` has no GPU-timing or
// overdraw counter and `HeadlessRaster` is a CPU rasterizer (§7.3/§36).
// ---------------------------------------------------------------------------

/// The analytic-shadow grid size for the §31 gate: 1k shadows, matching the D1/D2
/// grid gates so the shadow lane is measured at the same scale as its siblings.
const SHADOW_GRID: usize = 1_000;

/// A grid of `count` analytic drop shadows, no accompanying fill — this isolates
/// the shadow lane so its batch/pipeline structure is unambiguous. The rects tile
/// densely inside the surface; overlap is irrelevant to lowering, which emits one
/// instance per shadow regardless of where it lands.
fn analytic_shadow_grid_scene(count: usize) -> Vec<Primitive> {
    let cols = (count as f64).sqrt().ceil() as usize;
    let mut scene = Vec::with_capacity(count);
    for i in 0..count {
        let col = (i % cols) as f32;
        let row = (i / cols) as f32;
        scene.push(Primitive::AnalyticShadow(AnalyticShadow {
            rect: Rect {
                x: col * 3.0 + 2.0,
                y: row * 3.0 + 2.0,
                w: 2.0,
                h: 1.5,
            },
            color: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.35,
            },
            radius: Corners::uniform(0.5),
            offset: [0.5, 0.75],
            sigma: 1.0,
            spread: 0.0,
            shape: ShadowShape::RoundedBox,
            inner: false,
        }));
    }
    scene
}

/// E0.4 gate, part 1 (§15/§31): 1k analytic shadows stay one mergeable batch on
/// one pipeline with no offscreen, and a warmed steady frame is upload- and
/// tessellation-free.
fn assert_analytic_shadow_lane_scales() {
    let mut h = setup_scene(analytic_shadow_grid_scene(SHADOW_GRID));
    frame(&mut h);
    frame(&mut h);

    let textures = h.gpu.texture_count();
    let stats = h.renderer.frame_stats();

    // One shared family: 1k shadows merge into a single batch / single draw
    // behind one pipeline. A blur-target design would break this into per-shadow
    // offscreen passes; the analytic lane does not.
    assert_eq!(
        stats.batches, 1,
        "1k analytic shadows share one mergeable family: one batch"
    );
    assert_eq!(
        stats.draw_calls, 1,
        "one merged batch lowers to one draw call"
    );
    assert_eq!(
        stats.pipeline_switches, 1,
        "the shadow lane binds its pipeline once for the whole grid"
    );
    assert_eq!(
        stats.instances, SHADOW_GRID,
        "every shadow is exactly one instance on the shared pool"
    );
    assert_eq!(
        stats.clip_mask_builds, 0,
        "analytic shadows are closed-form coverage: they build no mask"
    );

    // Steady state: an unchanged grid uploads nothing, tessellates nothing, and
    // opens no offscreen texture (§9.1/§17.4).
    h.renderer.upload(&mut h.gpu, &h.scene);
    let steady = h.renderer.frame_stats();
    assert_eq!(
        steady.uploaded_ranges, 0,
        "an unchanged 1k-shadow grid uploads zero ranges (§9.1)"
    );
    assert_eq!(
        steady.gpu_upload_bytes, 0,
        "an unchanged 1k-shadow grid uploads zero bytes (§9.1)"
    );
    assert_eq!(
        steady.path_tessellations, 0,
        "analytic shadows never tessellate"
    );
    assert_eq!(
        h.gpu.texture_count(),
        textures,
        "the analytic-shadow lane opens no offscreen target (§16.2)"
    );
}

/// A triangle fan with a drop shadow, anchored at `(ox, oy)`; the shadow tint is
/// `alpha` and offset by `off`. Distinct anchors/geometry produce distinct
/// coverage masks; a changed tint or offset alone reuses the cached mask.
fn shadowed_path_at(ox: f32, oy: f32, alpha: f32, off: [f32; 2]) -> Primitive {
    use viso_render::PathShadow;
    Primitive::Path(Path {
        cmds: vec![
            PathCmd::MoveTo(Point::new(ox, oy)),
            PathCmd::LineTo(Point::new(ox + 16.0, oy + 8.0)),
            PathCmd::LineTo(Point::new(ox, oy + 16.0)),
            PathCmd::LineTo(Point::new(ox + 6.0, oy + 8.0)),
            PathCmd::Close,
        ],
        fill: Some(Rgba {
            r: 0.2,
            g: 0.4,
            b: 0.6,
            a: 1.0,
        }),
        stroke: None,
        shadow: Some(PathShadow {
            color: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: alpha,
            },
            offset: off,
            sigma: 2.0,
            spread: 0.0,
            inner: false,
        }),
    })
}

/// E0.4 gate, part 2 (§15.4): the path-shadow coverage cache is keyed on
/// {geometry, sigma, spread}, so re-tinting or re-offsetting the shadow reuses
/// the cached mask (0 rebuilds), while moving the path's geometry rebuilds
/// exactly its own two slots (shadow silhouette + fill) — never the scene.
fn assert_path_shadow_reuse_is_local() {
    let mut h = setup_scene(vec![
        shadowed_path_at(4.0, 4.0, 0.5, [3.0, 4.0]),
        shadowed_path_at(40.0, 40.0, 0.5, [3.0, 4.0]),
    ]);
    // Frame 1 is cold: each path builds a shadow-silhouette mask + a fill mask.
    frame(&mut h);
    assert_eq!(
        h.renderer.frame_stats().clip_mask_builds,
        4,
        "two shadowed paths build two masks each (silhouette + fill)"
    );
    // Frame 2 unchanged: every slot is a cache hit.
    frame(&mut h);
    assert_eq!(
        h.renderer.frame_stats().clip_mask_builds,
        0,
        "an unchanged shadowed-path scene rebuilds no mask"
    );

    // Re-tint and re-offset one shadow only. The key excludes color/offset, so
    // the cached coverage is reused — zero rebuilds — and the offset moves at
    // composite time (§15.4 "reused when only color/offset change").
    h.scene[0] = shadowed_path_at(4.0, 4.0, 0.8, [6.0, 2.0]);
    h.renderer.upload(&mut h.gpu, &h.scene);
    assert_eq!(
        h.renderer.frame_stats().clip_mask_builds,
        0,
        "changing only the shadow color/offset reuses the cached coverage mask"
    );

    // Move the path's geometry. The silhouette + fill keys both shift, so this
    // path rebuilds its two slots — and only its two, not the untouched sibling.
    h.scene[0] = shadowed_path_at(6.0, 6.0, 0.8, [6.0, 2.0]);
    h.renderer.upload(&mut h.gpu, &h.scene);
    assert_eq!(
        h.renderer.frame_stats().clip_mask_builds,
        2,
        "moving one path's geometry rebuilds exactly its two masks, not the scene"
    );
}

// ---------------------------------------------------------------------------
// E1.5 — the §31 blur / transient-resource gate. Three proofs, all read off
// `FrameStats` on a warmed renderer:
//
// * **Blur tiers** — small / medium / large sigma on one ROI-sized layer. The
//   ladder's pass count is a function of *tier*, never of sigma: a blur is
//   either skipped (sub-pixel), two full-resolution separable passes, or a
//   four-step downsample-then-blur ladder. Crucially the large tier addresses
//   *fewer* scratch bytes than the medium one despite twice the passes, because
//   it runs at reduced extent (§16.3).
// * **Many small-ROI blurs** — a flat run of independently blurred layers. Each
//   pays its own offscreen + ladder, but the transient pool aliases them down:
//   the pooled physical count and the live-set peak stay far below the bytes the
//   passes address (§16.4/§17.4).
// * **Idle blurred scene** — a static scene holding a gradient LUT, a
//   path-shadow coverage mask, analytic shadows and a blur ladder inside one
//   layer rebuilds *nothing* on a repeat frame: no mask, no tessellation, no
//   upload, no pooled allocation, no graph recompile, identical `FrameStats`.
//
// NOT observable here (flagged, not asserted): on-device shaded-pixel time per
// tier — the sigma at which downsampling beats a wider kernel is reasoned from
// the tap budget and asserted only as *pass and byte structure*, never as Metal
// time (§7.3/§36). "Does not continuously submit" is a present-loop property
// owned by `runtime/benches/frame_loop.rs::assert_idle_does_no_work`; from the
// render crate only "an idle upload does no work" is visible. `HeadlessRaster`
// is a CPU rasterizer with no bandwidth or overdraw counter, so the bandwidth
// saving a reduced-extent ladder buys is inferred from target bytes, not
// measured.
// ---------------------------------------------------------------------------

/// The blurred layer's ROI for the tier gate: a 64-pixel square, fully covered
/// by its content and landing exactly on a pool size class, so the tier
/// comparison is about the ladder and not about bucket slack.
const BLUR_ROI: f32 = 64.0;

/// One opaque layer blurred at `sigma`, its clip exactly covered by a single
/// quad — so the tight ROI is `BLUR_ROI` square and the frame is one offscreen
/// pass plus whatever ladder `sigma` selects.
fn blurred_layer_scene(sigma: f32) -> Vec<Primitive> {
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        w: BLUR_ROI,
        h: BLUR_ROI,
    };
    vec![
        Primitive::Layer(LayerClip {
            clip: rect,
            opacity: 1.0,
            blur_sigma: sigma,
            backdrop_sigma: 0.0,
        }),
        Primitive::Quad(Quad {
            rect,
            color: Rgba {
                r: 0.85,
                g: 0.45,
                b: 0.2,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        }),
        Primitive::LayerEnd,
    ]
}

/// A blur tier as the gate observes it: the authored sigma, whether the layer
/// needs an offscreen target at all, and the number of ladder passes it plans.
struct BlurTier {
    label: &'static str,
    sigma: f32,
    offscreen: usize,
    passes: u32,
}

/// E1.5 gate, part 1 (§16.3/§31): the ladder's cost is set by tier, not by
/// sigma. A sub-pixel blur is free — the ladder plans no rung, so an opaque
/// layer asking for one stays inline and pays no offscreen target either. Every
/// realized tier is one offscreen pass plus 2 or 4 ladder passes and never more,
/// and the compiled plan is exactly `offscreen + rungs + surface`. Across the
/// realized tiers the scratch footprint is *non-increasing* in sigma — the large
/// tiers downsample, so a 24-sigma blur addresses fewer bytes than a 10-sigma
/// one even though it runs twice the passes.
fn assert_blur_ladder_tiers_scale() {
    const TIERS: [BlurTier; 5] = [
        BlurTier {
            label: "sub-pixel",
            sigma: 0.5,
            offscreen: 0,
            passes: 0,
        },
        BlurTier {
            label: "small",
            sigma: 2.0,
            offscreen: 1,
            passes: 2,
        },
        BlurTier {
            label: "medium",
            sigma: 10.0,
            offscreen: 1,
            passes: 2,
        },
        BlurTier {
            label: "large",
            sigma: 24.0,
            offscreen: 1,
            passes: 4,
        },
        BlurTier {
            label: "huge",
            sigma: 64.0,
            offscreen: 1,
            passes: 4,
        },
    ];
    let roi_bytes = (BLUR_ROI as usize) * (BLUR_ROI as usize) * 4;
    let mut scratch = [0usize; TIERS.len()];

    for (i, tier) in TIERS.iter().enumerate() {
        let mut h = setup_scene(blurred_layer_scene(tier.sigma));
        frame(&mut h);
        frame(&mut h);

        let textures = h.gpu.texture_count();
        let baseline = h.renderer.frame_stats();
        let label = tier.label;

        assert_eq!(
            baseline.offscreen_passes, tier.offscreen,
            "{label}: a realized blur is exactly one offscreen pass, a skipped \
             one is none (§16.2)"
        );
        assert_eq!(
            baseline.blur_passes, tier.passes,
            "{label} (sigma {}): the ladder plans {} pass(es)",
            tier.sigma, tier.passes
        );
        assert_eq!(
            baseline.render_passes,
            1 + tier.offscreen + tier.passes as usize,
            "{label}: the compiled plan is the offscreen pass + each ladder rung \
             + the surface pass (§16.1)"
        );
        assert_eq!(
            baseline.transient_target_bytes,
            tier.offscreen * roi_bytes,
            "{label}: the layer's target is its tight ROI's size class, \
             independent of sigma (§16.4)"
        );
        assert!(
            baseline.transient_peak_bytes <= baseline.transient_pool_bytes,
            "{label}: the live-set peak never exceeds the pool it is drawn from \
             ({} vs {})",
            baseline.transient_peak_bytes,
            baseline.transient_pool_bytes
        );
        assert_eq!(
            baseline.transient_peak_bytes > 0,
            tier.offscreen == 1,
            "{label}: a realized blur holds a live target, a skipped one holds \
             nothing"
        );

        // Steady state: an unchanged blurred layer re-plans nothing — same
        // ladder, same pooled targets, no new texture, no graph recompile.
        for f in 0..2 {
            h.renderer.upload(&mut h.gpu, &h.scene);
            let steady = h.renderer.frame_stats();
            assert_eq!(
                steady, baseline,
                "{label} idle frame {f}: every counter must reproduce the warmed \
                 baseline"
            );
            assert_eq!(
                steady.transient_target_allocations, 0,
                "{label}: a steady blurred frame mints no pooled texture (§17.4)"
            );
            assert_eq!(
                steady.render_graph_compiles, 0,
                "{label}: the topology is unchanged, so the pass plan is reused \
                 (§16.1)"
            );
            assert_eq!(
                h.gpu.texture_count(),
                textures,
                "{label}: a steady blurred frame creates no backend texture"
            );
        }
        scratch[i] = baseline.blur_target_bytes;
    }

    // The tier structure, read as scratch bytes. This is the whole point of the
    // ladder: past the tap budget, cost stops growing with sigma and starts
    // *shrinking*, because the blur moves to a smaller extent.
    assert_eq!(
        scratch[0], 0,
        "a sub-pixel blur is a visual no-op: it addresses no scratch at all"
    );
    assert_eq!(
        scratch[1], scratch[2],
        "small and medium blurs share one realization — two full-resolution \
         separable passes — so their scratch footprint is identical"
    );
    assert_eq!(
        scratch[2],
        2 * roi_bytes,
        "two full-resolution rungs address two ROI-sized targets"
    );
    assert!(
        scratch[3] < scratch[2],
        "the large tier's four reduced-extent rungs address *fewer* scratch \
         bytes than the medium tier's two full-resolution ones ({} vs {}) — \
         downsampling is the point (§16.3)",
        scratch[3],
        scratch[2]
    );
    assert!(
        scratch[4] <= scratch[3],
        "a larger sigma downsamples further, so scratch never grows with sigma \
         ({} vs {})",
        scratch[4],
        scratch[3]
    );
}

/// The "many small-ROI blurs" fan-out: enough independently blurred layers that
/// naive per-layer allocation would be obvious, small enough to stay inside the
/// bench surface.
const BLUR_SIBLINGS: usize = 64;

/// The sigma for the fan-out rows: comfortably inside the tap budget, so every
/// layer plans the two-pass full-resolution ladder and the gate measures pooling
/// rather than tier selection.
const BLUR_SIBLING_SIGMA: f32 = 2.0;

/// E1.5 gate, part 2 (§16.4/§17.4/§31): [`BLUR_SIBLINGS`] small-ROI blurs each
/// pay their own offscreen pass and their own two-rung ladder, but the transient
/// planner aliases their targets: the pooled physical count is a small constant
/// above the number of targets that must survive to the surface pass, and the
/// live-set peak is a fraction of the bytes the passes address. Steady frames
/// then reproduce the whole `FrameStats` exactly with zero new allocations.
fn assert_many_small_roi_blurs_bound_transient_memory() {
    let mut h = setup_scene(blurred_sibling_layer_scene(
        BLUR_SIBLINGS,
        1.0,
        BLUR_SIBLING_SIGMA,
    ));
    frame(&mut h);
    frame(&mut h);

    let textures = h.gpu.texture_count();
    let baseline = h.renderer.frame_stats();

    assert_eq!(
        baseline.offscreen_passes, BLUR_SIBLINGS,
        "every blurred layer is its own offscreen pass"
    );
    assert_eq!(
        baseline.blur_passes as usize,
        2 * BLUR_SIBLINGS,
        "each layer plans a horizontal + vertical rung at this sigma"
    );
    assert_eq!(
        baseline.render_passes,
        3 * BLUR_SIBLINGS + 1,
        "the plan is one offscreen + two rungs per layer, plus the surface pass"
    );

    // Virtual targets the frame declares: a base plus one per rung, per layer.
    // The resource gate *records* what that costs, since the absolute numbers
    // are the interesting output, not just the inequalities below.
    let virtuals = 3 * BLUR_SIBLINGS;
    let addressed = baseline.transient_target_bytes + baseline.blur_target_bytes;
    println!(
        "E1.5 resource gate: {BLUR_SIBLINGS} small-ROI blurs -> \
         {} pooled targets for {virtuals} declared, peak {} B live, \
         pool {} B resident, {} B addressed by passes",
        baseline.transient_targets,
        baseline.transient_peak_bytes,
        baseline.transient_pool_bytes,
        addressed
    );

    assert!(
        baseline.transient_targets < virtuals,
        "the pool must alias: {} physical targets for {virtuals} declared \
         virtuals (§16.4)",
        baseline.transient_targets
    );
    assert!(
        baseline.transient_targets <= BLUR_SIBLINGS + 8,
        "only each layer's *final* rung has to survive to the surface pass; \
         bases and intermediate rungs die immediately and alias, so the pool is \
         ~one target per layer, not three ({} physicals)",
        baseline.transient_targets
    );
    assert!(
        baseline.transient_peak_bytes * 2 < addressed,
        "the concurrent live set is a fraction of the bytes the passes address \
         ({} B peak vs {addressed} B addressed) — lifetimes, not pass count, \
         set the memory bill (§16.4)",
        baseline.transient_peak_bytes
    );
    assert!(
        baseline.transient_peak_bytes <= baseline.transient_pool_bytes,
        "the peak live set never exceeds resident pool bytes ({} vs {})",
        baseline.transient_peak_bytes,
        baseline.transient_pool_bytes
    );

    for f in 0..2 {
        h.renderer.upload(&mut h.gpu, &h.scene);
        let steady = h.renderer.frame_stats();
        assert_eq!(
            steady, baseline,
            "idle frame {f}: {BLUR_SIBLINGS} blurred layers must reproduce every \
             counter of the warmed baseline"
        );
        assert_eq!(
            steady.uploaded_ranges, 0,
            "an unchanged blurred fan-out uploads zero ranges (§9.1)"
        );
        assert_eq!(
            steady.transient_target_allocations, 0,
            "a steady fan-out mints no pooled texture (§17.4)"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "the pool neither grows nor churns across steady frames"
        );
    }
}

/// The toolbar-shaped frosted row: enough panels that "one capture + one ladder
/// per panel" would be plainly visible in the pass counts.
const FROSTED_PANELS: usize = 6;

/// The sigma every panel of the row frosts at — the same one for all of them, so
/// a shared ladder is admissible, and small enough that the ladder is the
/// two-rung full-resolution tier.
const FROSTED_PANEL_SIGMA: f32 = 2.0;

/// Panel edge length and the pitch they sit on. The gap (`pitch - size`) is wider
/// than the blur reach `ceil(3 * sigma)`, so no panel's own composite lands inside
/// its neighbour's padded ROI and the whole row is sharable.
const FROSTED_PANEL_SIZE: f32 = 12.0;
const FROSTED_PANEL_PITCH: f32 = 20.0;

/// A row of `count` backdrop-blurred panels over one opaque background, each
/// carrying a small opaque tile of its own content — the §17.2 sharing case as a
/// real toolbar/sidebar would author it.
fn frosted_panel_row_scene(count: usize, sigma: f32) -> Vec<Primitive> {
    let mut scene = vec![Primitive::Quad(Quad {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            w: W as f32,
            h: H as f32,
        },
        color: Rgba {
            r: 0.2,
            g: 0.35,
            b: 0.5,
            a: 1.0,
        },
        radius: 0.0,
        border: Border::NONE,
    })];
    for i in 0..count {
        let panel = Rect {
            x: 4.0 + i as f32 * FROSTED_PANEL_PITCH,
            y: 20.0,
            w: FROSTED_PANEL_SIZE,
            h: FROSTED_PANEL_SIZE,
        };
        scene.push(Primitive::Layer(LayerClip {
            clip: panel,
            opacity: 1.0,
            blur_sigma: 0.0,
            backdrop_sigma: sigma,
        }));
        scene.push(Primitive::Quad(Quad {
            rect: Rect {
                x: panel.x + 3.0,
                y: panel.y + 3.0,
                w: panel.w - 6.0,
                h: panel.h - 6.0,
            },
            color: Rgba {
                r: 0.95,
                g: 0.95,
                b: 0.95,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        }));
        scene.push(Primitive::LayerEnd);
    }
    scene
}

/// E2.1 gate (§17.1/§17.2/§31): a frosted row's backdrop cost is set by the
/// *group*, not the panel count. [`FROSTED_PANELS`] panels over one background
/// take **one** capture pass over the union ROI and **one** blur ladder — the same
/// pass plan a single panel produces — and add only their own composites. The
/// forbidden default (N full-surface captures + N ladders) would scale every one
/// of these numbers with `count`.
///
/// Captured pixels are compared against `count ×` the single panel's capture, so
/// the union is proven cheaper than the N tight captures it replaces, not merely
/// cheaper than N full screens.
fn assert_frosted_panel_row_shares_one_capture() {
    let mut one = setup_scene(frosted_panel_row_scene(1, FROSTED_PANEL_SIGMA));
    frame(&mut one);
    frame(&mut one);
    let single = one.renderer.frame_stats();

    let mut h = setup_scene(frosted_panel_row_scene(FROSTED_PANELS, FROSTED_PANEL_SIGMA));
    frame(&mut h);
    frame(&mut h);
    let textures = h.gpu.texture_count();
    let baseline = h.renderer.frame_stats();

    println!(
        "E2.1 sharing gate: {FROSTED_PANELS} frosted panels -> \
         {} capture(s) over {} px, {} blur pass(es), {} render passes \
         (one panel alone: {} capture over {} px, {} blur, {} passes)",
        baseline.backdrop_captures,
        baseline.backdrop_capture_pixels,
        baseline.blur_passes,
        baseline.render_passes,
        single.backdrop_captures,
        single.backdrop_capture_pixels,
        single.blur_passes,
        single.render_passes
    );

    assert_eq!(
        single.backdrop_captures, 1,
        "one frosted panel is one capture"
    );
    assert_eq!(
        baseline.backdrop_captures, 1,
        "the whole row shares one capture, not {FROSTED_PANELS} (§17.2)"
    );
    assert_eq!(
        baseline.blur_passes, single.blur_passes,
        "a shared capture is blurred once: the row plans the same ladder as one \
         panel, not one ladder per panel"
    );
    assert_eq!(
        baseline.render_passes, single.render_passes,
        "sharing adds composites, never passes — capture + ladder + surface, the \
         same plan either way (§17.1)"
    );
    assert!(
        baseline.backdrop_capture_pixels < FROSTED_PANELS * single.backdrop_capture_pixels,
        "the union ROI must cost less than the {FROSTED_PANELS} tight captures it \
         replaces ({} vs {} px)",
        baseline.backdrop_capture_pixels,
        FROSTED_PANELS * single.backdrop_capture_pixels
    );
    assert!(
        baseline.backdrop_capture_pixels < (W as usize) * (H as usize),
        "and less than the surface: a shared group must not promote itself to a \
         full-screen capture ({} px)",
        baseline.backdrop_capture_pixels
    );

    for f in 0..2 {
        h.renderer.upload(&mut h.gpu, &h.scene);
        let steady = h.renderer.frame_stats();
        assert_eq!(
            steady, baseline,
            "idle frame {f}: an unchanged frosted row must reproduce every counter \
             of the warmed baseline"
        );
        assert_eq!(
            steady.transient_target_allocations, 0,
            "a steady capture comes back from the pool (§17.4)"
        );
        assert_eq!(
            steady.render_graph_compiles, 0,
            "the capture's topology is unchanged, so the cached plan is reused"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "the capture target neither grows nor churns across steady frames"
        );
    }
}

/// The graded-card row: enough cards that a per-effect render-target pass would
/// dominate the pass plan instead of hiding in the noise.
const GRADED_CARDS: usize = 8;

/// Card edge length and pitch. The gap keeps the cards disjoint, so each one is
/// its own layer with its own chain and nothing merges across them by accident.
const GRADED_CARD_SIZE: f32 = 12.0;
const GRADED_CARD_PITCH: f32 = 14.0;

/// The chain every card carries: five effects, every one of them affine on
/// straight linear RGBA, so the whole run is expressible as a single 4x5 matrix
/// (§17.3). Authored the way a theme would — a mild tone/color grade — not as a
/// synthetic worst case.
const GRADED_CHAIN: [ColorEffect; 5] = [
    ColorEffect::Brightness(1.08),
    ColorEffect::Contrast(1.12),
    ColorEffect::Saturation(0.85),
    ColorEffect::HueRotate(0.25),
    ColorEffect::Grayscale(0.15),
];

/// The same grade with one non-expressible stage wedged into the middle — the
/// stand-in for a custom filter, and the only thing that may cost a pass.
fn graded_chain_split() -> Vec<ColorEffect> {
    let mut chain = GRADED_CHAIN.to_vec();
    chain.insert(GRADED_CHAIN.len() / 2, ColorEffect::Gamma(2.2));
    chain
}

/// A row of `count` color-graded cards, each a layer carrying `effects` over its
/// own opaque tile — the §17.3 fusion case as a themed list/gallery authors it.
fn graded_card_scene(count: usize, effects: &[ColorEffect]) -> Vec<Primitive> {
    let mut scene = Vec::with_capacity(count * (effects.len() + 3));
    for i in 0..count {
        let card = Rect {
            x: 4.0 + i as f32 * GRADED_CARD_PITCH,
            y: 16.0,
            w: GRADED_CARD_SIZE,
            h: GRADED_CARD_SIZE,
        };
        scene.push(Primitive::Layer(LayerClip {
            clip: card,
            opacity: 1.0,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }));
        scene.extend(effects.iter().copied().map(Primitive::ColorEffect));
        scene.push(Primitive::Quad(Quad {
            rect: card,
            color: Rgba {
                r: 0.85,
                g: 0.45,
                b: 0.3,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        }));
        scene.push(Primitive::LayerEnd);
    }
    scene
}

/// E2.2 gate (§17.3/§31): a color chain's cost is set by how many *ops* it fuses
/// into, never by how many effects the author wrote. [`GRADED_CARDS`] cards each
/// carrying the five-effect [`GRADED_CHAIN`] plan exactly the pass count of the
/// same row carrying one effect — one op per card, riding the composite each
/// layer already draws, and zero render-target passes of their own. The forbidden
/// default (one pass per effect) would add `4 × GRADED_CARDS` passes here.
///
/// The split row is the other half of the contract: a single non-expressible
/// stage costs exactly one extra pass per card, not one per effect.
fn assert_color_chain_fuses_to_one_pass() {
    let mut one = setup_scene(graded_card_scene(GRADED_CARDS, &GRADED_CHAIN[..1]));
    frame(&mut one);
    frame(&mut one);
    let single = one.renderer.frame_stats();

    let mut h = setup_scene(graded_card_scene(GRADED_CARDS, &GRADED_CHAIN));
    frame(&mut h);
    frame(&mut h);
    let textures = h.gpu.texture_count();
    let baseline = h.renderer.frame_stats();

    let split_chain = graded_chain_split();
    let mut split_h = setup_scene(graded_card_scene(GRADED_CARDS, &split_chain));
    frame(&mut split_h);
    frame(&mut split_h);
    let split = split_h.renderer.frame_stats();

    let cards = GRADED_CARDS as u32;
    let effects = GRADED_CHAIN.len() as u32;
    // What the same row would cost if every effect took its own pass.
    let unfused_passes = baseline.render_passes + (GRADED_CHAIN.len() - 1) * GRADED_CARDS;

    println!(
        "E2.2 fusion gate: {GRADED_CARDS} cards x {effects} effects -> \
         {} op(s), {} color pass(es), {} render passes \
         (one effect each: {} op(s), {} color pass(es), {} passes; \
          split by one non-expressible stage: {} op(s), {} color pass(es), \
          {} passes; a pass per effect would be {unfused_passes})",
        baseline.color_effect_ops,
        baseline.color_transform_passes,
        baseline.render_passes,
        single.color_effect_ops,
        single.color_transform_passes,
        single.render_passes,
        split.color_effect_ops,
        split.color_transform_passes,
        split.render_passes
    );

    assert_eq!(
        baseline.color_effect_ops, cards,
        "each card's {effects} effects fuse into one op (§17.3)"
    );
    assert_eq!(
        baseline.color_transform_passes, 0,
        "a fused op rides the composite the layer already draws: no pass of its own"
    );
    assert_eq!(
        baseline.offscreen_passes, GRADED_CARDS,
        "one layer per card, no more: fusion must not split a layer"
    );
    assert_eq!(
        baseline.render_passes, single.render_passes,
        "the {effects}-effect row plans the same passes as the one-effect row — \
         chain length must not reach the pass plan"
    );
    assert_eq!(
        baseline.color_effect_ops, single.color_effect_ops,
        "and the same op count: the extra effects are matrix multiplications, \
         not work items"
    );
    assert!(
        baseline.render_passes < unfused_passes,
        "the fused plan must beat a pass per effect ({} vs {unfused_passes} passes)",
        baseline.render_passes
    );

    assert_eq!(
        split.color_effect_ops,
        2 * cards,
        "one non-expressible stage splits each card's run once"
    );
    assert_eq!(
        split.color_transform_passes, cards,
        "and costs exactly one pass per card — the trailing op still rides the \
         composite"
    );
    assert_eq!(
        split.render_passes,
        baseline.render_passes + GRADED_CARDS,
        "a split adds its own pass and nothing else"
    );
    assert!(
        split.render_passes < unfused_passes,
        "even the split plan stays below a pass per effect ({} vs \
         {unfused_passes} passes)",
        split.render_passes
    );

    for f in 0..2 {
        h.renderer.upload(&mut h.gpu, &h.scene);
        let steady = h.renderer.frame_stats();
        assert_eq!(
            steady, baseline,
            "idle frame {f}: an unchanged graded row must reproduce every counter \
             of the warmed baseline"
        );
        assert_eq!(
            steady.transient_target_allocations, 0,
            "a steady graded layer comes back from the pool (§17.4)"
        );
        assert_eq!(
            steady.render_graph_compiles, 0,
            "the chain's topology is unchanged, so the cached plan is reused"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "the layer targets neither grow nor churn across steady frames"
        );
    }

    for f in 0..2 {
        split_h.renderer.upload(&mut split_h.gpu, &split_h.scene);
        let steady = split_h.renderer.frame_stats();
        assert_eq!(
            steady, split,
            "idle frame {f}: the split row's scratch targets must be reused, not \
             replanned"
        );
        assert_eq!(steady.transient_target_allocations, 0);
        assert_eq!(steady.render_graph_compiles, 0);
    }
}

/// How many blended tiles the E2.3 row carries, and their geometry — a row of
/// small badges over a shared background, the shape a themed list paints when its
/// rows carry a non-`SrcOver` blend.
const BLENDED_CARDS: usize = 8;
const BLENDED_CARD_SIZE: f32 = 12.0;
const BLENDED_CARD_PITCH: f32 = 14.0;

/// The separable mode the gate plans and times: the cheapest advanced blend, so
/// the pass algebra it proves is the *floor* every other advanced mode also pays.
const BLENDED_MODE: Blend = Blend::Multiply;

/// A non-separable mode for the timing pair: `Luminosity` evaluates luminance and
/// a clipped color fit per pixel, the most expensive fragment in the family.
const BLENDED_MODE_HEAVY: Blend = Blend::Luminosity;

/// A row of `count` tiles over an opaque background, each tile in its own layer
/// carrying `mode`. With `mode == SrcOver` this is the same geometry on the common
/// fixed-function path — the control the isolation cost is measured against.
fn blended_card_scene(count: usize, mode: Blend) -> Vec<Primitive> {
    let mut scene = Vec::with_capacity(count * 4 + 1);
    // Something to blend against: the destination the snapshots read.
    scene.push(Primitive::Quad(Quad {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            w: W as f32,
            h: H as f32,
        },
        color: Rgba {
            r: 0.8,
            g: 0.4,
            b: 0.2,
            a: 1.0,
        },
        radius: 0.0,
        border: Border::NONE,
    }));
    for i in 0..count {
        let card = Rect {
            x: 4.0 + i as f32 * BLENDED_CARD_PITCH,
            y: 16.0,
            w: BLENDED_CARD_SIZE,
            h: BLENDED_CARD_SIZE,
        };
        scene.push(Primitive::Layer(LayerClip {
            clip: card,
            opacity: 1.0,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }));
        scene.push(Primitive::Blend(mode));
        scene.push(Primitive::Quad(Quad {
            rect: card,
            color: Rgba {
                r: 0.3,
                g: 0.6,
                b: 0.9,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        }));
        scene.push(Primitive::LayerEnd);
    }
    scene
}

/// E2.3 gate (§17.1/§31): an advanced blend is isolated through the planner and
/// pays only the layer target it already needs plus one *bounded* destination
/// snapshot — no third render-target pass per blend, and nothing on the common
/// path.
///
/// Two halves. The control row is the identical geometry under `SrcOver`: it must
/// open no offscreen, take no capture, and stay one surface pass — proof that the
/// fixed-function lane is untouched by the advanced family existing. The isolated
/// row then pays exactly `offscreen + capture` per card and composites in one draw
/// each, so `render_passes` is the sum of those two plus the surface. The forbidden
/// default — re-reading the whole destination per blend — would snapshot
/// `BLENDED_CARDS × W × H` pixels; the bound printed here is what isolation
/// actually reads.
fn assert_blend_isolation_costs_no_extra_pass() {
    let mut plain = setup_scene(blended_card_scene(BLENDED_CARDS, Blend::SrcOver));
    frame(&mut plain);
    frame(&mut plain);
    let common = plain.renderer.frame_stats();

    let mut h = setup_scene(blended_card_scene(BLENDED_CARDS, BLENDED_MODE));
    frame(&mut h);
    frame(&mut h);
    let textures = h.gpu.texture_count();
    let baseline = h.renderer.frame_stats();

    let cards = BLENDED_CARDS as u32;
    let full_surface_reads = BLENDED_CARDS * (W as usize) * (H as usize);

    println!(
        "E2.3 isolation gate: {BLENDED_CARDS} x {BLENDED_MODE:?} -> \
         {} isolation(s), {} offscreen pass(es), {} capture(s) reading {} px, \
         {} render passes, {} draw calls \
         (same row as SrcOver: {} isolation(s), {} offscreen pass(es), \
          {} capture(s), {} render passes; a full destination read per blend \
          would be {full_surface_reads} px)",
        baseline.blend_isolations,
        baseline.offscreen_passes,
        baseline.backdrop_captures,
        baseline.backdrop_capture_pixels,
        baseline.render_passes,
        baseline.draw_calls,
        common.blend_isolations,
        common.offscreen_passes,
        common.backdrop_captures,
        common.render_passes
    );

    // The common path is untouched: fixed-function blending is a pipeline state,
    // never a pass.
    assert_eq!(
        common.blend_isolations, 0,
        "`SrcOver` is fixed-function: it must never isolate (§C0.5)"
    );
    assert_eq!(
        common.offscreen_passes, 0,
        "an opaque `SrcOver` layer draws in-pass under a scissor"
    );
    assert_eq!(common.backdrop_captures, 0, "and reads no destination");
    assert_eq!(
        common.render_passes, 1,
        "so the whole control row is one surface pass"
    );

    // The isolated row: one isolation per card, each on its own layer target.
    assert_eq!(
        baseline.blend_isolations, cards,
        "every advanced-blend layer is isolated through the planner"
    );
    assert_eq!(
        baseline.offscreen_passes, BLENDED_CARDS,
        "an isolated blend renders into the layer target it already needed — \
         one per card, no scratch of its own"
    );
    assert_eq!(
        baseline.render_passes,
        baseline.offscreen_passes + baseline.backdrop_captures as usize + 1,
        "the pass plan is exactly layer targets + snapshots + the surface: an \
         advanced blend buys no third pass"
    );
    assert_eq!(
        baseline.color_transform_passes, 0,
        "no color op is authored here, so the blend must not open a color pass"
    );
    assert!(
        baseline.backdrop_capture_pixels < W as usize * H as usize,
        "the snapshots are bounded by the tiles, not the surface ({} px of {} )",
        baseline.backdrop_capture_pixels,
        W as usize * H as usize
    );
    assert!(
        baseline.backdrop_capture_pixels * (BLENDED_CARDS / 2) < full_surface_reads,
        "and stay far below a full destination read per blend ({} px vs \
         {full_surface_reads} px)",
        baseline.backdrop_capture_pixels
    );
    assert_eq!(
        baseline.backdrop_captures, 1,
        "disjoint badges share one snapshot pass over the union ROI — isolation \
         reuses the E2.1 capture machinery instead of capturing per blend"
    );
    assert_eq!(
        baseline.draw_calls,
        common.draw_calls + BLENDED_CARDS + 1,
        "each isolated layer composites in exactly one extra draw — the single \
         two-texture advanced-blend draw — plus the one draw the shared snapshot \
         re-renders the background with"
    );

    for f in 0..2 {
        h.renderer.upload(&mut h.gpu, &h.scene);
        let steady = h.renderer.frame_stats();
        assert_eq!(
            steady, baseline,
            "idle frame {f}: an unchanged blended row must reproduce every \
             counter of the warmed baseline"
        );
        assert_eq!(
            steady.transient_target_allocations, 0,
            "the layer target and the snapshot both come back from the pool (§17.4)"
        );
        assert_eq!(
            steady.render_graph_compiles, 0,
            "the isolation topology is unchanged, so the cached plan is reused"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "isolation targets neither grow nor churn across steady frames"
        );
    }
}

/// How many translucent cards the E2.4 row carries, and their geometry — a row of
/// faded tiles, the shape a themed list paints while its rows animate their group
/// opacity.
const FADED_CARDS: usize = 16;
const FADED_CARD_SIZE: f32 = 12.0;
const FADED_CARD_PITCH: f32 = 14.0;

/// The group opacity the row fades to. Any value in `(0, 1)` exercises the same
/// decision; this one is far enough from both ends that no clamp shortcut applies.
const FADED_OPACITY: f32 = 0.6;

/// A row of `count` translucent groups, each holding two children inside its own
/// clip. With `overlap == false` the children are separated by a two-pixel gap —
/// provably disjoint, so the group opacity is equivalent to per-child opacity and
/// the planner can push it down. With `overlap == true` the same two children
/// straddle the middle, where per-child opacity would double-blend, and the group
/// must isolate: the control this gate measures the fold against.
fn faded_card_scene(count: usize, overlap: bool) -> Vec<Primitive> {
    let mut scene = Vec::with_capacity(count * 4);
    for i in 0..count {
        let x = 4.0 + i as f32 * FADED_CARD_PITCH;
        let card = Rect {
            x,
            y: 16.0,
            w: FADED_CARD_SIZE,
            h: FADED_CARD_SIZE,
        };
        // Disjoint: [x+1, x+5) and [x+7, x+11) — a two-pixel gap, so the children
        // do not even share an antialiased edge pixel. Overlapping: two wide
        // halves that cross in the middle.
        let (first, second) = if overlap {
            (
                Rect {
                    x: x + 1.0,
                    y: 17.0,
                    w: 8.0,
                    h: 10.0,
                },
                Rect {
                    x: x + 3.0,
                    y: 17.0,
                    w: 8.0,
                    h: 10.0,
                },
            )
        } else {
            (
                Rect {
                    x: x + 1.0,
                    y: 17.0,
                    w: 4.0,
                    h: 10.0,
                },
                Rect {
                    x: x + 7.0,
                    y: 17.0,
                    w: 4.0,
                    h: 10.0,
                },
            )
        };
        scene.push(Primitive::Layer(LayerClip {
            clip: card,
            opacity: FADED_OPACITY,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }));
        for rect in [first, second] {
            scene.push(Primitive::Quad(Quad {
                rect,
                color: Rgba {
                    r: 0.3,
                    g: 0.6,
                    b: 0.9,
                    a: 1.0,
                },
                radius: 0.0,
                border: Border::NONE,
            }));
        }
        scene.push(Primitive::LayerEnd);
    }
    scene
}

/// E2.4 gate (§3102/§3120/§3145/§31): a group opacity is a multiply, not a render
/// target, whenever the planner can prove it.
///
/// Two rows of the same geometry and the same opacity, differing only in whether
/// the children overlap. The disjoint row must cost *nothing*: no offscreen pass,
/// no transient byte, one surface pass — the group factor rides into each child's
/// alpha. The overlapping row is the forbidden default made visible: it is what
/// every group opacity would cost if the planner did not try, one target and one
/// composite per card. The ratio between them is the whole point of §3145.
///
/// The gate also pins the §3202 half on the same frame: an unchanged row reports
/// no dirty backdrop ROI, so a static screen with frosted chrome does no repeat
/// capture work on the effect planner's account.
fn assert_group_opacity_folds_without_a_target() {
    let mut isolated = setup_scene(faded_card_scene(FADED_CARDS, true));
    frame(&mut isolated);
    frame(&mut isolated);
    let control = isolated.renderer.frame_stats();

    let mut h = setup_scene(faded_card_scene(FADED_CARDS, false));
    frame(&mut h);
    frame(&mut h);
    let textures = h.gpu.texture_count();
    let baseline = h.renderer.frame_stats();

    let cards = FADED_CARDS as u32;

    println!(
        "E2.4 planner gate: {FADED_CARDS} x opacity {FADED_OPACITY} -> \
         {} planned, {} eliminated, {} folded, {} offscreen pass(es), \
         {} transient byte(s), {} render passes, {} draw calls \
         (the same row with overlapping children: {} folded, \
          {} offscreen pass(es), {} transient byte(s), {} render passes, \
          {} draw calls)",
        baseline.layers_planned,
        baseline.layers_eliminated,
        baseline.opacity_folds,
        baseline.offscreen_passes,
        baseline.transient_target_bytes,
        baseline.render_passes,
        baseline.draw_calls,
        control.opacity_folds,
        control.offscreen_passes,
        control.transient_target_bytes,
        control.render_passes,
        control.draw_calls
    );

    // Every group was considered — the planner does not skip the question, it
    // answers it.
    assert_eq!(
        baseline.layers_planned, cards,
        "each translucent group raises its reason and is planned"
    );
    assert_eq!(
        baseline.opacity_folds, cards,
        "disjoint children take the fold, every card"
    );
    assert_eq!(
        baseline.layers_eliminated, cards,
        "and the layer that reason asked for is eliminated"
    );

    // What the fold costs: nothing.
    assert_eq!(
        baseline.offscreen_passes, 0,
        "a folded group allocates no render target (§3145)"
    );
    assert_eq!(
        baseline.transient_target_bytes, 0,
        "and therefore no transient bytes"
    );
    assert_eq!(
        baseline.transient_target_allocations, 0,
        "nothing is claimed from the pool either"
    );
    assert_eq!(
        baseline.render_passes, 1,
        "the whole row stays a single surface pass"
    );
    assert_eq!(
        baseline.backdrop_captures, 0,
        "group opacity reads no destination"
    );

    // What the same row costs when the proof fails — the forbidden default.
    assert_eq!(
        control.opacity_folds, 0,
        "overlapping children must not be folded: correctness first (§14.5)"
    );
    assert_eq!(
        control.offscreen_passes, FADED_CARDS,
        "the control row pays one target per card"
    );
    assert_eq!(
        control.render_passes,
        FADED_CARDS + 1,
        "and one pass per target plus the surface"
    );
    assert!(
        control.transient_target_bytes > 0,
        "the control row really does allocate"
    );
    assert!(
        baseline.draw_calls < control.draw_calls,
        "the folded row also composites less: {} draw(s) vs {}",
        baseline.draw_calls,
        control.draw_calls
    );

    for f in 0..2 {
        h.renderer.upload(&mut h.gpu, &h.scene);
        let steady = h.renderer.frame_stats();
        assert_eq!(
            steady, baseline,
            "idle frame {f}: an unchanged folded row must reproduce every counter \
             of the warmed baseline"
        );
        assert_eq!(
            steady.render_graph_compiles, 0,
            "the plan is a decision about unchanged content, so it is not recompiled"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "a folded row neither grows nor churns textures"
        );
    }

    // §3202: the frosted row's backdrop dependency is a fact about its ROI's
    // content, so an unchanged frame dirties nothing.
    let mut frosted = setup_scene(frosted_panel_row_scene(FROSTED_PANELS, FROSTED_PANEL_SIGMA));
    frame(&mut frosted);
    let first = frosted.renderer.frame_stats();
    assert!(
        first.backdrop_captures > 0,
        "the row really does capture a backdrop"
    );
    for f in 0..3 {
        frame(&mut frosted);
        let steady = frosted.renderer.frame_stats();
        assert_eq!(
            steady.backdrop_dirty_rois, 0,
            "idle frame {f}: nothing changed behind any panel, so no ROI is dirty \
             (§3202)"
        );
        assert!(
            frosted
                .renderer
                .backdrop_dependencies()
                .iter()
                .all(|d| !d.dirty),
            "idle frame {f}: every dependency reports clean"
        );
    }
}

/// A static "app screen" inside one blurred translucent layer: a bounded
/// gradient palette (LUT rows), a shadowed path (coverage masks + tessellation),
/// and a run of analytic shadows — every effect cache the renderer keeps, all
/// under a ladder. Warming this populates all of them at once, so a later idle
/// frame that rebuilds *any* of them shows up as a single counter regression.
fn idle_effect_scene(sigma: f32) -> Vec<Primitive> {
    let mut scene = vec![Primitive::Layer(LayerClip {
        clip: Rect {
            x: 0.0,
            y: 0.0,
            w: W as f32,
            h: H as f32,
        },
        opacity: 0.85,
        blur_sigma: sigma,
        backdrop_sigma: 0.0,
    })];
    scene.extend(gradient_grid_scene(LUT_PALETTE));
    scene.push(shadowed_path_at(8.0, 8.0, 0.5, [3.0, 4.0]));
    scene.extend(analytic_shadow_grid_scene(8));
    scene.push(Primitive::LayerEnd);
    scene
}

/// E1.5 gate, part 3 (§7.1/§9.1/§16.1/§31): a static idle scene rebuilds no
/// cache. With gradient LUT rows, a path-shadow coverage mask, analytic shadows
/// and a blur ladder all live, repeat uploads of the unchanged scene build zero
/// masks, tessellate zero paths, upload zero bytes, allocate zero pooled
/// targets, recompile zero pass plans, and report an identical [`FrameStats`].
fn assert_idle_blurred_scene_rebuilds_nothing() {
    let mut h = setup_scene(idle_effect_scene(6.0));
    frame(&mut h);
    frame(&mut h);

    let textures = h.gpu.texture_count();
    let buffers = h.gpu.buffer_count();
    let baseline = h.renderer.frame_stats();
    assert!(
        baseline.blur_passes > 0,
        "the idle scene must actually carry a blur ladder to be worth gating"
    );
    assert!(
        baseline.offscreen_passes > 0,
        "the idle scene must actually composite through an offscreen target"
    );

    for f in 0..4 {
        h.renderer.upload(&mut h.gpu, &h.scene);
        let idle = h.renderer.frame_stats();
        assert_eq!(
            idle, baseline,
            "idle frame {f}: an unchanged effect-heavy scene must reproduce every \
             counter exactly"
        );
        assert_eq!(
            idle.clip_mask_builds, 0,
            "idle frame {f}: no clip or shadow coverage mask is rebuilt \
             (§14.4/§15.4)"
        );
        assert_eq!(
            idle.path_tessellations, 0,
            "idle frame {f}: no path is re-tessellated (§16.3)"
        );
        assert_eq!(
            idle.uploaded_ranges, 0,
            "idle frame {f}: no instance range is re-uploaded (§9.1)"
        );
        assert_eq!(
            idle.gpu_upload_bytes, 0,
            "idle frame {f}: no gradient LUT row is re-baked and no instance byte \
             re-sent (§9.1)"
        );
        assert_eq!(
            idle.transient_target_allocations, 0,
            "idle frame {f}: the ladder's targets come back from the pool (§17.4)"
        );
        assert_eq!(
            idle.render_graph_compiles, 0,
            "idle frame {f}: the topology is unchanged, so the compiled plan is \
             reused (§16.1)"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "idle frame {f}: no backend texture is created"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "idle frame {f}: no backend buffer is created"
        );
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

    // D3.2 gate (§13.4): the retained vector-mesh lane. A path grid tessellates
    // once, then holds an unchanged frame to zero re-tessellation + zero upload,
    // a scroll to transform-only, and a recolor to paint-only.
    assert_path_grid_is_retained();

    // D3.5 gate (§31 `## D3`): curve/dash-heavy path scenes tessellate once and
    // then hold steady (0 re-tessellation, 0 upload), and path churn stays local
    // — re-tessellation scales with the changed set, not the scene.
    assert_curve_and_dash_scenes_are_retained();
    assert_path_churn_is_local();

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

    // C0.6 gate (§31 `## C0` compositing matrix): the clip/layer/opacity/blend
    // foundation pays only its tier's cost. A deep opacity==1 clip nest opens no
    // offscreen (in-pass scissor); nested and many-sibling translucent layers
    // each open one pooled offscreen pass and reuse it across steady frames with
    // no texture growth and identical stats.
    assert_deep_clip_nest_opens_no_offscreen();
    const NEST_DEPTH: usize = 16;
    const SIBLINGS: usize = 64;
    assert_translucent_layers_reuse_pooled_targets(
        || nested_layer_scene(NEST_DEPTH, 0.5),
        NEST_DEPTH,
    );
    assert_translucent_layers_reuse_pooled_targets(|| sibling_layer_scene(SIBLINGS, 0.5), SIBLINGS);

    // E0.2 gate (§15.3 / §20.1): the decorated-shape fusion decision. Pins the
    // separate-draw batch/switch structure and the shaded-quad overdraw proxy the
    // fusion turns on; the register-pressure / GPU-time half is a device
    // measurement this backend cannot express (see the fn doc).
    assert_decorated_fusion_gate();

    // E0.4 gate (§15/§15.4/§31): the analytic-shadow fast lane at 1k shadows
    // stays one mergeable batch on one pipeline with no offscreen and holds a
    // steady frame upload-free; the general-path shadow's coverage cache reuses
    // its mask across color/offset changes and rebuilds only the moved path.
    assert_analytic_shadow_lane_scales();
    assert_path_shadow_reuse_is_local();

    // E1.5 gate (§16.3/§16.4/§31): the blur ladder's cost is set by tier and its
    // scratch shrinks as sigma grows past the tap budget; a fan-out of small-ROI
    // blurs aliases down to ~one pooled target per layer; and a static
    // effect-heavy blurred scene rebuilds no cache when it idles.
    assert_blur_ladder_tiers_scale();
    assert_many_small_roi_blurs_bound_transient_memory();
    assert_idle_blurred_scene_rebuilds_nothing();

    // E2.1 gate (§17.1/§17.2/§31): a row of frosted panels shares one capture
    // pass and one ladder with the union ROI — the pass plan of a single panel —
    // instead of one full-surface capture and ladder per panel.
    assert_frosted_panel_row_shares_one_capture();

    // E2.2 gate (§17.3/§31): a row of color-graded cards plans the pass count of
    // a single effect no matter how long its mergeable chain is, and only a
    // non-expressible stage buys one extra pass per card.
    assert_color_chain_fuses_to_one_pass();

    // E2.3 gate (§17.1/§31): a row of advanced-blend badges isolates through the
    // planner for the price of the layer target it already needed plus one bounded
    // destination snapshot, while the same row under `SrcOver` stays a single
    // fixed-function surface pass.
    assert_blend_isolation_costs_no_extra_pass();

    // E2.4 gate (§3102/§3120/§3145/§3202/§31): a row of translucent cards whose
    // children provably do not overlap pushes its group opacity into them and pays
    // nothing, while the identical row with overlapping children pays a target per
    // card; and an idle frosted row dirties no backdrop ROI.
    assert_group_opacity_folds_without_a_target();

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

    // E0.4 timing (§15/§31): a steady `upload` of 1k analytic shadows is the
    // diff-and-coalesce cost of the shared shadow pool with zero GPU work — the
    // high-refresh sentinel for the analytic-shadow lane (all closed-form
    // coverage, no offscreen). Debug timing is not a perf result (§36); run
    // release. On-device shaded-pixel time for overlapping soft shadows is a
    // separate measurement this headless backend cannot express (§7.3).
    let mut shadows = setup_scene(analytic_shadow_grid_scene(SHADOW_GRID));
    frame(&mut shadows);
    frame(&mut shadows);
    c.bench_function("analytic_shadow_1k_upload_steady", |b| {
        b.iter(|| {
            shadows
                .renderer
                .upload(black_box(&mut shadows.gpu), black_box(&shadows.scene))
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

    // D3.5 timings (§31 `## D3`). A large static SVG-like path scene warms its
    // caches once; thereafter a steady `upload` is diff-and-coalesce with zero
    // GPU work, and a small-churn `upload` re-tessellates only the changed paths.
    // Both scale with the dirty set, not the scene.
    let mut svg = setup_scene(svg_like_path_scene(GRID_10K));
    frame(&mut svg);
    frame(&mut svg);
    c.bench_function("svg_path_grid_upload_steady", |b| {
        b.iter(|| {
            svg.renderer
                .upload(black_box(&mut svg.gpu), black_box(&svg.scene))
        });
    });

    // Deform one path's shape before each iteration (a relative-shape change,
    // not a uniform translate), so every measured upload re-tessellates exactly
    // one path against a 10k-path scene.
    let churn_target = GRID_10K / 2;
    c.bench_function("svg_path_grid_churn_one", |b| {
        b.iter(|| {
            if let Primitive::Path(path) = &mut svg.scene[churn_target]
                && let Some(PathCmd::CubicTo(c0, _, _)) = path.cmds.get_mut(1)
            {
                c0.x += 0.25;
            }
            svg.renderer
                .upload(black_box(&mut svg.gpu), black_box(&svg.scene));
        });
    });

    // Stroke/dash-heavy scene: a steady `upload` holds the stroke-geometry cache
    // (dash expansion + caps/joins tessellated once, then reused).
    let mut dash = setup_scene(dashed_stroke_scene(GRID_1K));
    frame(&mut dash);
    frame(&mut dash);
    c.bench_function("dashed_stroke_upload_steady", |b| {
        b.iter(|| {
            dash.renderer
                .upload(black_box(&mut dash.gpu), black_box(&dash.scene))
        });
    });

    // C0.6 timings (§31 `## C0`). A deep in-pass clip nest is the pure-scissor
    // compositing cost (no offscreen); the translucent nests/siblings add the
    // pooled-offscreen open/close cost. All are steady `upload`s of an unchanged
    // scene — the diff-and-coalesce path with the targets reused, not the frame
    // rate. High-refresh cadence (see the note in the C0.6 checklist) is a
    // present-loop property this microbench cannot observe.
    let mut clip = setup_scene(nested_layer_scene(32, 1.0));
    frame(&mut clip);
    frame(&mut clip);
    c.bench_function("deep_clip_nest_upload_steady", |b| {
        b.iter(|| {
            clip.renderer
                .upload(black_box(&mut clip.gpu), black_box(&clip.scene))
        });
    });

    let mut nested = setup_scene(nested_layer_scene(16, 0.5));
    frame(&mut nested);
    frame(&mut nested);
    c.bench_function("nested_opacity_upload_steady", |b| {
        b.iter(|| {
            nested
                .renderer
                .upload(black_box(&mut nested.gpu), black_box(&nested.scene))
        });
    });

    let mut layers = setup_scene(sibling_layer_scene(64, 0.5));
    frame(&mut layers);
    frame(&mut layers);
    c.bench_function("sibling_layers_upload_steady", |b| {
        b.iter(|| {
            layers
                .renderer
                .upload(black_box(&mut layers.gpu), black_box(&layers.scene))
        });
    });

    // E0.2 timing (§15.3 / §20.1): the separate-draw decorated-card baseline —
    // one shadow + one rrect per card, `2N` draws. This is the cost a fused
    // `DecoratedShape` family would be measured against on device; here it is the
    // steady diff-and-coalesce upload of the separate path.
    let mut cards = setup_scene(decorated_card_scene(DECORATED_CARDS));
    frame(&mut cards);
    frame(&mut cards);
    c.bench_function("decorated_cards_upload_steady", |b| {
        b.iter(|| {
            cards
                .renderer
                .upload(black_box(&mut cards.gpu), black_box(&cards.scene))
        });
    });

    // E1.5 timing (§16.3/§31): the blur tiers and the small-ROI fan-out. `upload`
    // is where the ladder is planned and its targets are claimed; `frame` also
    // encodes every rung, so the pair separates plan reuse from multi-pass encode
    // cost. Neither is device shaded-pixel time (§36).
    let mut blur_small = setup_scene(blurred_layer_scene(2.0));
    frame(&mut blur_small);
    frame(&mut blur_small);
    c.bench_function("blur_small_upload_steady", |b| {
        b.iter(|| {
            blur_small
                .renderer
                .upload(black_box(&mut blur_small.gpu), black_box(&blur_small.scene))
        });
    });

    let mut blur_large = setup_scene(blurred_layer_scene(24.0));
    frame(&mut blur_large);
    frame(&mut blur_large);
    c.bench_function("blur_large_upload_steady", |b| {
        b.iter(|| {
            blur_large
                .renderer
                .upload(black_box(&mut blur_large.gpu), black_box(&blur_large.scene))
        });
    });
    c.bench_function("blur_large_frame", |b| {
        b.iter(|| frame(black_box(&mut blur_large)));
    });

    let mut many_blurs = setup_scene(blurred_sibling_layer_scene(
        BLUR_SIBLINGS,
        1.0,
        BLUR_SIBLING_SIGMA,
    ));
    frame(&mut many_blurs);
    frame(&mut many_blurs);
    c.bench_function("many_small_blurs_upload_steady", |b| {
        b.iter(|| {
            many_blurs
                .renderer
                .upload(black_box(&mut many_blurs.gpu), black_box(&many_blurs.scene))
        });
    });
    c.bench_function("many_small_blurs_frame", |b| {
        b.iter(|| frame(black_box(&mut many_blurs)));
    });

    // E2.1 timing (§17.1/§17.2/§31): the frosted row. `upload` is where the
    // sharing decision runs and the capture's target/ladder are claimed, so it is
    // the cost of *planning* a shared backdrop; `frame` also encodes the capture
    // pass, its rungs, and one composite per panel. Neither is device shaded-pixel
    // time (§36).
    let mut frosted_row = setup_scene(frosted_panel_row_scene(FROSTED_PANELS, FROSTED_PANEL_SIGMA));
    frame(&mut frosted_row);
    frame(&mut frosted_row);
    c.bench_function("frosted_row_upload_steady", |b| {
        b.iter(|| {
            frosted_row.renderer.upload(
                black_box(&mut frosted_row.gpu),
                black_box(&frosted_row.scene),
            )
        });
    });
    c.bench_function("frosted_row_frame", |b| {
        b.iter(|| frame(black_box(&mut frosted_row)));
    });

    // E2.2 timing (§17.3/§31): the graded row. `upload` is where the chain fuses
    // and the fused matrix is folded into the composite instance, so it is the
    // cost of *planning* the grade; `frame` also encodes one composite per card
    // through the color-transform pipeline. The split row pays one extra scratch
    // pass per card, so the pair brackets the fused/split difference in CPU terms
    // — neither is device shaded-pixel time (§36).
    let mut graded = setup_scene(graded_card_scene(GRADED_CARDS, &GRADED_CHAIN));
    frame(&mut graded);
    frame(&mut graded);
    c.bench_function("graded_cards_upload_steady", |b| {
        b.iter(|| {
            graded
                .renderer
                .upload(black_box(&mut graded.gpu), black_box(&graded.scene))
        });
    });
    c.bench_function("graded_cards_frame", |b| {
        b.iter(|| frame(black_box(&mut graded)));
    });

    let mut graded_split = setup_scene(graded_card_scene(GRADED_CARDS, &graded_chain_split()));
    frame(&mut graded_split);
    frame(&mut graded_split);
    c.bench_function("graded_cards_split_frame", |b| {
        b.iter(|| frame(black_box(&mut graded_split)));
    });

    // E2.3 timing (§17.1/§31): the blended row. `upload` is where the blend is
    // classified, the layer is forced offscreen, and its bounded snapshot is
    // claimed — the cost of *planning* an isolation; `frame` also encodes the
    // snapshot pass and one two-texture composite per badge. The `SrcOver` control
    // brackets what isolation costs over the fixed-function lane, and the
    // non-separable row brackets the fragment's spread across the family. None of
    // these is device shaded-pixel time (§36).
    let mut blended = setup_scene(blended_card_scene(BLENDED_CARDS, BLENDED_MODE));
    frame(&mut blended);
    frame(&mut blended);
    c.bench_function("blended_cards_upload_steady", |b| {
        b.iter(|| {
            blended
                .renderer
                .upload(black_box(&mut blended.gpu), black_box(&blended.scene))
        });
    });
    c.bench_function("blended_cards_frame", |b| {
        b.iter(|| frame(black_box(&mut blended)));
    });

    let mut blended_heavy = setup_scene(blended_card_scene(BLENDED_CARDS, BLENDED_MODE_HEAVY));
    frame(&mut blended_heavy);
    frame(&mut blended_heavy);
    c.bench_function("blended_cards_nonseparable_frame", |b| {
        b.iter(|| frame(black_box(&mut blended_heavy)));
    });

    let mut src_over_cards = setup_scene(blended_card_scene(BLENDED_CARDS, Blend::SrcOver));
    frame(&mut src_over_cards);
    frame(&mut src_over_cards);
    c.bench_function("src_over_cards_frame", |b| {
        b.iter(|| frame(black_box(&mut src_over_cards)));
    });

    // E2.4 (§3145/§36): what the Effect Planner's cheapest rung is worth. Both
    // rows author the same geometry at the same group opacity; only the children's
    // overlap differs, so the pair brackets the fold against the target it avoids.
    // `upload` is where the subtree is scanned, the plan is made and — in the
    // folded row — the factor is multiplied into each child's alpha; `frame` adds
    // the pass encoding, which is where the isolated row pays for a target per
    // card. The scan is O(n²) in a group's foldable children by design (it is a
    // pairwise disjointness proof, capped at a small child count), so `faded_cards_
    // upload_steady` is also the guard that the proof does not grow into the cost
    // it exists to remove. None of this is device shaded-pixel time (§36).
    let mut faded = setup_scene(faded_card_scene(FADED_CARDS, false));
    frame(&mut faded);
    frame(&mut faded);
    c.bench_function("faded_cards_upload_steady", |b| {
        b.iter(|| {
            faded
                .renderer
                .upload(black_box(&mut faded.gpu), black_box(&faded.scene))
        });
    });
    c.bench_function("faded_cards_frame", |b| {
        b.iter(|| frame(black_box(&mut faded)));
    });

    let mut faded_isolated = setup_scene(faded_card_scene(FADED_CARDS, true));
    frame(&mut faded_isolated);
    frame(&mut faded_isolated);
    c.bench_function("faded_cards_isolated_frame", |b| {
        b.iter(|| frame(black_box(&mut faded_isolated)));
    });
}

criterion_group!(benches, bench_steady_state);
criterion_main!(benches);
