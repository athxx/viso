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
    Border, Corners, DashPattern, ExtendMode, FrameStats, GlyphRunDraw, Gradient, GradientKind,
    GradientStop, ImageDraw, InterpolationSpace, LayerClip, LineCap, LineJoin, Path, PathCmd,
    Point, Primitive, Quad, Rect, Renderer, Rgba, ShadowShape, SpriteRegion, Stroke, test_glyphs,
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
        }));
        scene.push(layer_tile(level, inset + 1.0, inset + 1.0));
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
            blur_sigma: 0.0,
        }));
        scene.push(layer_tile(i, col * 4.0 + 0.5, row * 4.0 + 0.5));
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
}

criterion_group!(benches, bench_steady_state);
criterion_main!(benches);
