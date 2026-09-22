//! Texture binding contract (§20.2): what a frame of images and text costs when
//! a draw can bind only one texture at a time.
//!
//! §20.2 offers a fast path — a resource table the instance indexes — and then
//! names the fallback that has to carry every backend without one: **atlas, a
//! small texture set, and bind-group batching**. This file is the fallback's
//! contract, measured through the public paint stream and `FrameStats`:
//!
//! 1. **Sharing a resource costs an instance, not a draw.** A hundred images from
//!    one atlas, a page of glyph runs from one coverage atlas, a screen of
//!    gradients from one LUT page — each collapses to a single draw.
//! 2. **A texture change is the only thing that splits them**, so the cost of the
//!    per-draw model is the number of *switches* in paint order, not the number of
//!    images. Grouping a frame by resource is what makes the fallback reach the
//!    draw count a resource table would.
//! 3. **The public paint API does not change with the answer.** Nothing here names
//!    a binding model, and the model an honest description of these frames selects
//!    is the portable one — on a backend reporting a huge table, too.
//!
//! The one thing not asserted here is the fast path itself: no backend in this
//! repository reports a resource table, so there is no dispatch to measure. That
//! bound is pinned instead — `bindless_texture_slots == 0` — so the day a backend
//! grows one, this file is where the new cost has to be proven.

use viso_gpu::{
    GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat, TextureId,
};
use viso_render::{
    BindingModel, Border, ExtendMode, FrameStats, GlyphInstanceData, GlyphRunDraw, Gradient,
    GradientKind, GradientStop, ImageDraw, InterpolationSpace, Point, Primitive, Quad, Rect,
    Renderer, Rgba, TextureWorkload,
};

const W: u32 = 256;
const H: u32 = 256;

fn rect(x: f32, y: f32) -> Rect {
    Rect {
        x,
        y,
        w: 8.0,
        h: 8.0,
    }
}

/// A distinct 2x2 BGRA texture — one unatlasable resource.
fn texture(gpu: &mut HeadlessRaster, label: &'static str) -> TextureId {
    gpu.create_texture(&TextureDesc {
        width: 2,
        height: 2,
        format: TextureFormat::Bgra8Unorm,
        render_target: false,
        label,
    })
}

/// An A8 coverage atlas, the resource a page of text shares.
fn atlas(gpu: &mut HeadlessRaster) -> TextureId {
    gpu.create_texture(&TextureDesc {
        width: 64,
        height: 64,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "coverage-atlas",
    })
}

fn image(texture: TextureId, x: f32) -> Primitive {
    Primitive::Image(ImageDraw::new(rect(x, 0.0), texture))
}

fn quad(x: f32) -> Primitive {
    Primitive::Quad(Quad {
        rect: rect(x, 32.0),
        color: Rgba::new(0.2, 0.4, 0.6, 1.0),
        radius: 0.0,
        border: Border::NONE,
    })
}

/// One shaped run of `glyphs` glyphs sampling `atlas`.
fn glyph_run(atlas: TextureId, glyphs: u32, y: f32) -> Primitive {
    Primitive::GlyphRun(GlyphRunDraw {
        glyphs: (0..glyphs)
            .map(|i| GlyphInstanceData {
                rect: Rect {
                    x: i as f32 * 9.0,
                    y,
                    w: 8.0,
                    h: 12.0,
                },
                uv: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.125,
                    h: 0.125,
                },
            })
            .collect(),
        atlas,
        color: Rgba::new(0.9, 0.9, 0.9, 1.0),
    })
}

/// A three-stop gradient, so the ramp bakes a LUT row rather than riding inline in
/// the instance: this is the case that actually samples a texture.
fn gradient(x: f32) -> Primitive {
    Primitive::Gradient(Gradient {
        rect: rect(x, 64.0),
        kind: GradientKind::Linear,
        p0: Point { x: 0.0, y: 0.0 },
        p1: Point { x: 8.0, y: 0.0 },
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            },
            GradientStop {
                offset: 0.5,
                color: Rgba::new(0.0, 1.0, 0.0, 1.0),
            },
            GradientStop {
                offset: 1.0,
                color: Rgba::new(0.0, 0.0, 1.0, 1.0),
            },
        ],
        extend: ExtendMode::Clamp,
        interp: InterpolationSpace::LinearRgb,
    })
}

/// Build a scene against a live backend (so it can create the textures the scene
/// samples) and report the frame's counters.
fn scene(build: impl FnOnce(&mut HeadlessRaster) -> Vec<Primitive>) -> FrameStats {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let prims = build(&mut gpu);
    let mut r = Renderer::new(&mut gpu, format);
    r.upload(&mut gpu, &prims);
    r.frame_stats()
}

// ---------------------------------------------------------------------------
// 1. Sharing a resource costs an instance, not a draw
// ---------------------------------------------------------------------------

/// The atlas case, which is the whole fallback: images sharing one texture join
/// one instanced draw, so the draw count is flat in the image count. Drawn a
/// hundred at a time, the forbidden model would submit a hundred draws.
#[test]
fn images_sharing_one_texture_are_one_draw() {
    for count in [1usize, 2, 8, 100] {
        let stats = scene(|gpu| {
            let tex = texture(gpu, "shared");
            (0..count).map(|i| image(tex, i as f32 * 2.0)).collect()
        });
        assert_eq!(
            stats.draw_calls, 1,
            "{count} images on one texture must cost one draw"
        );
        assert_eq!(
            stats.instances, count,
            "each image still costs its own instance"
        );
    }
}

/// The same property for text, the resource-sharing case every real frame has:
/// every run on one coverage atlas joins one draw, whatever the line count.
#[test]
fn a_page_of_text_is_one_draw() {
    let lines = 40;
    let stats = scene(|gpu| {
        let a = atlas(gpu);
        (0..lines)
            .map(|i| glyph_run(a, 30, i as f32 * 14.0))
            .collect()
    });
    assert_eq!(stats.draw_calls, 1, "one atlas, one draw");
    assert_eq!(stats.glyph_instances, lines * 30);
}

/// And for gradients, which share a baked LUT page: a screen of gradient fills
/// costs a draw per page, not a draw per fill.
#[test]
fn gradients_sharing_a_lut_page_are_one_draw() {
    let stats = scene(|_| (0..24).map(|i| gradient(i as f32 * 4.0)).collect());
    assert_eq!(stats.draw_calls, 1);
    assert_eq!(stats.instances, 24);
}

// ---------------------------------------------------------------------------
// 2. A texture change is what splits a batch
// ---------------------------------------------------------------------------

/// Distinct textures cannot share a bind group, so they cannot share a draw. This
/// is the cost bindless removes, and the reason atlasing comes first.
#[test]
fn distinct_textures_cost_a_draw_each() {
    let stats = scene(|gpu| {
        let a = texture(gpu, "a");
        let b = texture(gpu, "b");
        let c = texture(gpu, "c");
        vec![image(a, 0.0), image(b, 10.0), image(c, 20.0)]
    });
    assert_eq!(stats.draw_calls, 3, "one draw per distinct texture");
}

/// The count that matters is switches, not textures: the same eight images over
/// two textures cost two draws grouped and eight interleaved. Paint order is the
/// lever the fallback has, and it is worth four times the draws here.
#[test]
fn the_cost_is_switches_in_paint_order_not_texture_count() {
    let grouped = scene(|gpu| {
        let (a, b) = (texture(gpu, "a"), texture(gpu, "b"));
        let mut prims: Vec<_> = (0..4).map(|i| image(a, i as f32 * 2.0)).collect();
        prims.extend((0..4).map(|i| image(b, 10.0 + i as f32 * 2.0)));
        prims
    });
    let interleaved = scene(|gpu| {
        let (a, b) = (texture(gpu, "a"), texture(gpu, "b"));
        (0..8)
            .map(|i| image(if i % 2 == 0 { a } else { b }, i as f32 * 2.0))
            .collect()
    });

    assert_eq!(
        grouped.instances, interleaved.instances,
        "same eight images"
    );
    assert_eq!(grouped.draw_calls, 2, "grouped: one draw per texture");
    assert_eq!(interleaved.draw_calls, 8, "interleaved: one draw per image");
    assert_eq!(
        grouped.texture_binding_switches, 2,
        "a grouped frame binds each texture once"
    );
}

/// A different *family* between two images is a barrier even when they share a
/// texture: batching never reorders paint order to save a draw (§16.2).
#[test]
fn an_opaque_quad_between_two_images_is_still_a_barrier() {
    let stats = scene(|gpu| {
        let tex = texture(gpu, "shared");
        vec![image(tex, 0.0), quad(0.0), image(tex, 20.0)]
    });
    assert_eq!(
        stats.draw_calls, 3,
        "the trailing image must not reach back across the quad"
    );
}

// ---------------------------------------------------------------------------
// 3. The paint API does not change with the binding model
// ---------------------------------------------------------------------------

/// An honest description of an ordinary frame — a few textures, a few switches —
/// selects the portable model even against a backend claiming a huge table, which
/// is §20.2's hard rule: bindless is for a resource set atlasing could not shrink,
/// not for images in general.
#[test]
fn an_ordinary_textured_frame_stays_on_the_portable_model() {
    let stats = scene(|gpu| {
        let (icons, photo) = (texture(gpu, "icons"), texture(gpu, "photo"));
        let a = atlas(gpu);
        let mut prims: Vec<_> = (0..20).map(|i| image(icons, i as f32 * 2.0)).collect();
        prims.push(image(photo, 64.0));
        prims.push(glyph_run(a, 40, 100.0));
        prims.extend((0..6).map(|i| gradient(i as f32 * 8.0)));
        prims
    });
    // Three sampled resources and one switch each: what the frame actually costs.
    assert_eq!(stats.draw_calls, 4, "icons, photo, text, gradients");

    let described = TextureWorkload {
        bindless_slots: 500_000,
        distinct_textures: stats.texture_binding_switches,
        texture_batch_breaks: stats.texture_binding_switches,
    };
    assert_eq!(
        BindingModel::select(described),
        BindingModel::PerDraw,
        "atlasing already did what a resource table would"
    );
}

/// The capability is reported honestly, so the fast path is never selected by
/// accident: this renderer's backends bind one texture per slot at bind-group
/// creation and have no table to index.
#[test]
fn no_backend_here_offers_a_resource_table() {
    let gpu = HeadlessRaster::new();
    assert_eq!(gpu.caps().bindless_texture_slots, 0);
    assert_eq!(
        BindingModel::select(TextureWorkload {
            bindless_slots: gpu.caps().bindless_texture_slots,
            distinct_textures: 4_000,
            texture_batch_breaks: 4_000,
        }),
        BindingModel::PerDraw,
        "a frame that would want a table still gets the model the backend has"
    );
}

/// Steady state: the same textured scene re-uploaded keeps the same draw count.
/// Bind-group batching is a property of the frame, not a cache that warms up.
#[test]
fn a_steady_textured_scene_keeps_its_draw_count() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let tex = texture(&mut gpu, "shared");
    let a = atlas(&mut gpu);
    let mut prims: Vec<_> = (0..12).map(|i| image(tex, i as f32 * 2.0)).collect();
    prims.push(glyph_run(a, 24, 80.0));

    let mut r = Renderer::new(&mut gpu, format);
    r.upload(&mut gpu, &prims);
    let first = r.frame_stats();
    assert_eq!(first.draw_calls, 2);
    for _ in 0..4 {
        r.upload(&mut gpu, &prims);
        let next = r.frame_stats();
        assert_eq!(next.draw_calls, first.draw_calls);
        assert_eq!(next.instances, first.instances);
        assert_eq!(
            next.texture_binding_switches,
            first.texture_binding_switches
        );
    }
}
