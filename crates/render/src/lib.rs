//! `viso-render` — paint primitives, batching, and the render graph (§16).
//!
//! The UI produces paint/primitive *data*; the renderer decides batching,
//! atlas usage, clip/layer strategy, GPU uploads, and pass ordering (§16.1).
//! Users never manually select a batch boundary for correctness (§16.2), and
//! there is no public `new_batch: true` escape hatch.
//!
//! Phase 2 status: the Quad primitive is a full vertical slice
//! (primitive → instance → headless raster → golden); the other primitives
//! land incrementally.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod primitive;
pub mod renderer;

pub use primitive::{
    Border, GlyphInstance, GlyphInstanceData, GlyphRunDraw, ImageDraw, ImageInstance, LayerClip,
    LineJoin, Mesh, MeshVertex, Path, PathCmd, Point, Primitive, Quad, QuadInstance, Rect, Rgba,
    Stroke, glyphrun_schema, image_schema, mesh_schema, quad_schema,
};
pub use renderer::{FrameStats, Renderer};
pub use viso_gpu::TextureId;
use viso_text::TextSystem;

/// A compact batch key. Batch keys use integer IDs, never strings (§16.2),
/// and must respect visual order and clipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BatchKey(pub u32);

/// The Image test texture: a 4×4 red/blue checkerboard, BGRA8, top-left origin,
/// premultiplied (opaque, so premultiplied == straight).
///
/// Returned as `(width, height, bgra8_bytes)` so the caller can create a texture
/// and [`GpuBackend::write_texture`] it. Kept small and procedural so the golden
/// stays deterministic without an image asset. Sampling this with a linear
/// filter over a larger destination rect produces visible color gradients at the
/// cell boundaries, which the golden captures.
///
/// [`GpuBackend::write_texture`]: viso_gpu::GpuBackend::write_texture
pub fn test_texture() -> (u32, u32, Vec<u8>) {
    const N: u32 = 4;
    // BGRA8: red = [0,0,255,255], blue = [255,0,0,255].
    let red = [0u8, 0, 255, 255];
    let blue = [255u8, 0, 0, 255];
    let mut bytes = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let cell = (x + y) % 2 == 0;
            bytes.extend_from_slice(if cell { &red } else { &blue });
        }
    }
    (N, N, bytes)
}

/// The prepared text for [`test_scene`]: the positioned glyph quads, the run
/// color, and the R8 SDF atlas backing them (raw pixels + edge length).
///
/// Returned separately from [`test_scene`] because the atlas is a real GPU
/// texture the caller must create and upload (like [`test_texture`]): the
/// caller creates an `R8Unorm` texture of `atlas_size²`, writes `atlas_pixels`
/// into it, and passes the resulting [`TextureId`] back into [`test_scene`].
#[derive(Debug, Clone)]
pub struct TestGlyphs {
    /// One positioned quad per visible glyph (screen rect + atlas UV + px_range).
    pub glyphs: Vec<GlyphInstanceData>,
    /// The single color applied to the whole run.
    pub color: Rgba,
    /// The full R8 atlas pixel buffer (`atlas_size²` bytes).
    pub atlas_pixels: Vec<u8>,
    /// Atlas edge length in texels.
    pub atlas_size: u32,
}

/// The embedded ASCII-subset test font: a small DejaVu Sans slice with enough
/// glyphs for the demo string. Kept in-tree so text golden output stays
/// deterministic without a system font dependency.
const TEST_FONT: &[u8] = include_bytes!("fixtures/DejaVuSans-subset.ttf");

/// Shape and lay out a short two-line string with the embedded [`TEST_FONT`],
/// producing the glyph quads and R8 SDF atlas for [`test_scene`]'s
/// [`Primitive::GlyphRun`].
///
/// The `origin` is the top-left the text block is offset to (the layout's own
/// baseline math places lines relative to it). Multi-line is exercised by a
/// hard `\n`. The atlas is rasterized once here; the caller uploads it.
pub fn test_glyphs(origin: [f32; 2], font_size: f32) -> TestGlyphs {
    let mut text = TextSystem::new();
    let font = text
        .load_font(TEST_FONT, 0)
        .expect("embedded test font parses");
    // Two lines to exercise the layout's `\n` handling and baseline advance.
    let quads = text.prepare(font, "Viso\ngpu", font_size, 1.0);
    let glyphs = quads
        .iter()
        .map(|q| GlyphInstanceData {
            rect: Rect {
                x: origin[0] + q.rect_px[0],
                y: origin[1] + q.rect_px[1],
                w: q.rect_px[2],
                h: q.rect_px[3],
            },
            uv: Rect {
                x: q.uv[0],
                y: q.uv[1],
                w: q.uv[2] - q.uv[0],
                h: q.uv[3] - q.uv[1],
            },
            px_range: q.px_range,
        })
        .collect();
    TestGlyphs {
        glyphs,
        color: Rgba {
            r: 0.95,
            g: 0.95,
            b: 0.98,
            a: 1.0,
        },
        atlas_pixels: text.atlas_pixels().to_vec(),
        atlas_size: text.atlas_size(),
    }
}

/// A small pure-Viso demo scene used by the facade's example and the golden
/// test (§67 exit criterion "test scene 可绘制").
///
/// Phase 2 slice: two overlapping rounded quads — an opaque fill and a
/// bordered one — exercising fill, corner radius, and border stroke; a
/// [`Primitive::Layer`] clip wrapping an oversized quad to exercise rectangular
/// clipping (the quad extends past the clip on all sides, so the golden captures
/// a hard clip boundary); an [`Primitive::Image`] sampling `texture` (the
/// [`test_texture`] checkerboard) into a rect with a tint, exercising the
/// textured path; a filled-and-stroked [`Primitive::Path`] (a Bézier outline with
/// a miter corner, exercising curve flattening, fill coverage-AA, and stroke
/// joins); a caller-supplied [`Primitive::Mesh`] triangle (the direct-geometry
/// escape hatch, sharing the path pipeline); a multi-line
/// [`Primitive::GlyphRun`] (`glyphs`, from [`test_glyphs`]) sampling an R8 SDF
/// atlas, exercising the text vertical slice; and a translucent
/// [`Primitive::Layer`] (`opacity < 1`) wrapping a solid quad, exercising the
/// offscreen render-to-texture path and opacity compositing (the subtree is
/// rendered into a texture and blended back at the layer rect). Coordinates
/// are in physical pixels with a top-left origin; a caller renders it against a
/// surface sized to hold it, having first created `texture` and uploaded
/// [`test_texture`] into it, and created/uploaded the glyph atlas whose
/// [`TextureId`] is carried by `glyphs`.
pub fn test_scene(texture: TextureId, glyphs: GlyphRunDraw) -> Vec<Primitive> {
    vec![
        // A multi-line text run in the top-left, sampling the R8 SDF atlas. The
        // glyphs were shaped/laid-out by the text subsystem; the run carries a
        // single color and the atlas it samples. Drawn first so later quads can
        // overlap it if positioned to.
        Primitive::GlyphRun(glyphs),
        // Opaque red rounded rect.
        Primitive::Quad(Quad {
            rect: Rect {
                x: 12.0,
                y: 12.0,
                w: 48.0,
                h: 40.0,
            },
            color: Rgba {
                r: 0.9,
                g: 0.1,
                b: 0.1,
                a: 1.0,
            },
            radius: 8.0,
            border: Border::NONE,
        }),
        // Green rect with a blue border, overlapping the first.
        Primitive::Quad(Quad {
            rect: Rect {
                x: 44.0,
                y: 36.0,
                w: 56.0,
                h: 44.0,
            },
            color: Rgba {
                r: 0.1,
                g: 0.8,
                b: 0.2,
                a: 1.0,
            },
            radius: 4.0,
            border: Border {
                width: 3.0,
                color: Rgba {
                    r: 0.1,
                    g: 0.2,
                    b: 0.9,
                    a: 1.0,
                },
            },
        }),
        // Clip container: everything until LayerEnd is constrained to this rect.
        // Opaque (opacity == 1.0), so it clips in-pass with a hardware scissor.
        Primitive::Layer(LayerClip {
            clip: Rect {
                x: 76.0,
                y: 8.0,
                w: 40.0,
                h: 40.0,
            },
            opacity: 1.0,
        }),
        // A sharp-cornered orange quad larger than the clip on every side; only
        // the clip window shows, proving the scissor bounds the fill.
        Primitive::Quad(Quad {
            rect: Rect {
                x: 64.0,
                y: -4.0,
                w: 64.0,
                h: 64.0,
            },
            color: Rgba {
                r: 1.0,
                g: 0.6,
                b: 0.0,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        }),
        Primitive::LayerEnd,
        // A textured image: sample the whole checkerboard into a rect in the
        // lower-left, tinted with a slightly transparent warm white. Linear
        // sampling over a 40×32 destination makes the 4×4 cells clearly visible
        // and blended at their seams.
        Primitive::Image(ImageDraw {
            rect: Rect {
                x: 12.0,
                y: 58.0,
                w: 40.0,
                h: 32.0,
            },
            uv: Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            tint: Rgba {
                r: 1.0,
                g: 0.95,
                b: 0.9,
                a: 0.9,
            },
            texture,
        }),
        // A filled-and-stroked vector path in the lower-right: a closed
        // teardrop-ish outline that mixes a cubic and a quadratic Bézier (so the
        // tessellator flattens curves) with two straight segments meeting at a
        // sharp corner (exercising a miter join). The purple fill carries the 1px
        // coverage-AA fringe; the darker stroke is drawn over it.
        Primitive::Path(Path {
            cmds: vec![
                PathCmd::MoveTo(Point::new(64.0, 84.0)),
                PathCmd::CubicTo(
                    Point::new(64.0, 66.0),
                    Point::new(96.0, 66.0),
                    Point::new(96.0, 84.0),
                ),
                PathCmd::QuadTo(Point::new(96.0, 92.0), Point::new(80.0, 92.0)),
                PathCmd::LineTo(Point::new(70.0, 92.0)),
                PathCmd::Close,
            ],
            fill: Some(Rgba {
                r: 0.55,
                g: 0.2,
                b: 0.8,
                a: 1.0,
            }),
            stroke: Some(Stroke {
                width: 2.0,
                color: Rgba {
                    r: 0.2,
                    g: 0.05,
                    b: 0.35,
                    a: 1.0,
                },
                join: LineJoin::Miter,
            }),
        }),
        // A caller-supplied triangle mesh (the escape hatch for geometry the
        // higher-level primitives don't cover): a single opaque teal triangle in
        // the top-right, interior `edge = 1` so it fills solid. It shares the
        // exact buffers/pipeline that `Path` lowers into.
        Primitive::Mesh(Mesh {
            vertices: vec![
                MeshVertex {
                    pos: [104.0, 8.0],
                    color: [0.0, 0.7, 0.7, 1.0],
                    edge: 1.0,
                },
                MeshVertex {
                    pos: [124.0, 8.0],
                    color: [0.0, 0.7, 0.7, 1.0],
                    edge: 1.0,
                },
                MeshVertex {
                    pos: [114.0, 28.0],
                    color: [0.0, 0.7, 0.7, 1.0],
                    edge: 1.0,
                },
            ],
            indices: vec![0, 1, 2],
        }),
        // Translucent container: `opacity < 1.0` renders this subtree into an
        // offscreen texture, then composites it back over the scene at the layer
        // rect multiplied by `opacity`. The child is an opaque solid quad, so the
        // composited result is a clean blend of the layer color and whatever it
        // covers — the golden captures the half-transparent boundary against both
        // the background and the underlying primitives.
        Primitive::Layer(LayerClip {
            clip: Rect {
                x: 100.0,
                y: 52.0,
                w: 24.0,
                h: 32.0,
            },
            opacity: 0.5,
        }),
        // An opaque magenta quad exactly filling the layer clip; after compositing
        // at opacity 0.5 it appears as a half-strength wash over the scene.
        Primitive::Quad(Quad {
            rect: Rect {
                x: 100.0,
                y: 52.0,
                w: 24.0,
                h: 32.0,
            },
            color: Rgba {
                r: 0.9,
                g: 0.1,
                b: 0.7,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        }),
        Primitive::LayerEnd,
    ]
}
