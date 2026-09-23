//! The golden scenes, shared by the headless golden test and every device
//! backend's test: each builds its primitives (and uploads the textures they
//! sample) against any [`GpuBackend`]. The headless test pins the reference
//! rasterizer to its baselines; a device test compares its frame with the
//! reference rendered in-process, so it needs no baseline files.

#![allow(dead_code)]

use std::path::PathBuf;

use viso_gpu::{
    AddressMode, FilterMode, GpuBackend, HeadlessRaster, RawWindowHandle, SamplerDesc, TextureDesc,
    TextureFormat, TextureId,
};
use viso_render::{
    Align, Align2, DashPattern, Fit, GlyphLane, GlyphRunDraw, ImageRect, LineCap, LineJoin,
    NineSlice, Path, PathCmd, Point, Primitive, Rect, Renderer, Rgba, Stroke, TiledImage,
    test_glyphs, test_scene, test_texture,
};

pub const W: u32 = 128;
pub const H: u32 = 96;
/// Opaque dark gray, premultiplied.
pub const CLEAR: [f32; 4] = [0.1, 0.1, 0.1, 1.0];

/// One committed golden scene.
#[derive(Clone, Copy, Debug)]
pub enum GoldenScene {
    Quad,
    ImageFamily,
    Path,
    Stroke,
}

impl GoldenScene {
    pub const ALL: [GoldenScene; 4] = [Self::Quad, Self::ImageFamily, Self::Path, Self::Stroke];

    /// The baseline: a raw BGRA8 dump, top-left origin.
    pub fn golden_path(self) -> PathBuf {
        let file = match self {
            Self::Quad => "quad_scene",
            Self::ImageFamily => "image_family",
            Self::Path => "path_scene",
            Self::Stroke => "stroke_scene",
        };
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/{file}.bgra8"))
    }

    /// The committed baseline bytes.
    pub fn golden(self) -> Vec<u8> {
        let path = self.golden_path();
        std::fs::read(&path).unwrap_or_else(|_| {
            panic!(
                "missing golden {}; run the headless golden test with BLESS=1",
                path.display()
            )
        })
    }

    /// Build the scene's primitives, creating and uploading what they sample.
    pub fn build<B: GpuBackend>(self, gpu: &mut B) -> Vec<Primitive> {
        match self {
            Self::Quad => quad(gpu),
            Self::ImageFamily => image_family(gpu),
            Self::Path => path(),
            Self::Stroke => stroke(),
        }
    }
}

/// Render `scene` through the full renderer into a fresh BGRA8 render target
/// on `gpu`, returning the target for read-back.
pub fn render_to_target<B: GpuBackend>(gpu: &mut B, scene: GoldenScene) -> TextureId {
    let mut renderer = Renderer::new(gpu, TextureFormat::Bgra8Unorm);
    let primitives = scene.build(gpu);
    renderer.upload(gpu, &primitives);
    let target = gpu.create_texture(&TextureDesc {
        width: W,
        height: H,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "golden-target",
    });
    renderer.render_to_texture(gpu, target, CLEAR, [W as f32, H as f32]);
    target
}

/// Render `scene` through the headless reference rasterizer; BGRA8, top-left.
pub fn reference(scene: GoldenScene) -> Vec<u8> {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
    let primitives = scene.build(&mut gpu);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [W as f32, H as f32]);
    gpu.read_pixels_bgra8(surface)
}

/// How far a device rendering is from its golden.
#[derive(Debug)]
pub struct GoldenDiff {
    /// Largest per-channel difference anywhere.
    pub worst: u8,
    /// Pixels with any channel differing by more than `tol`.
    pub pixels_over: usize,
    /// The first such pixel, `(x, y)`.
    pub first_over: Option<(u32, u32)>,
}

/// Compare BGRA8 `actual` with `expected` at per-channel tolerance `tol`.
pub fn diff(actual: &[u8], expected: &[u8], tol: u8) -> GoldenDiff {
    assert_eq!(actual.len(), expected.len(), "golden size mismatch");
    let mut out = GoldenDiff {
        worst: 0,
        pixels_over: 0,
        first_over: None,
    };
    for (i, (a, e)) in actual
        .as_chunks::<4>()
        .0
        .iter()
        .zip(expected.as_chunks::<4>().0)
        .enumerate()
    {
        let d = a
            .iter()
            .zip(e)
            .map(|(a, e)| a.abs_diff(*e))
            .max()
            .unwrap_or(0);
        out.worst = out.worst.max(d);
        if d > tol {
            out.pixels_over += 1;
            out.first_over
                .get_or_insert(((i as u32) % W, (i as u32) / W));
        }
    }
    out
}

/// Device rasterizers differ from the headless reference only in arithmetic
/// precision (fixed-point sample snapping, derivative-based AA, filter
/// weights): a device frame matches when every channel is within `DEVICE_TOL`
/// but for at most `DEVICE_EDGE_PIXELS` edge pixels.
pub const DEVICE_TOL: u8 = 4;
pub const DEVICE_EDGE_PIXELS: usize = 8;

/// Assert a device frame of `scene` matches the headless reference.
pub fn assert_device_matches(scene: GoldenScene, actual: &[u8]) {
    let d = diff(actual, &reference(scene), DEVICE_TOL);
    eprintln!("{scene:?}: {d:?}");
    assert!(
        d.pixels_over <= DEVICE_EDGE_PIXELS,
        "{scene:?}: {} pixels differ by more than {DEVICE_TOL} (limit {DEVICE_EDGE_PIXELS}), \
         worst {}, first at {:?}",
        d.pixels_over,
        d.worst,
        d.first_over
    );
}

fn quad<B: GpuBackend>(gpu: &mut B) -> Vec<Primitive> {
    // Create and upload the Image test texture (BGRA8 checkerboard).
    let (tw, th, texels) = test_texture();
    let texture = gpu.create_texture(&TextureDesc {
        width: tw,
        height: th,
        format: TextureFormat::Bgra8Unorm,
        render_target: false,
        label: "test-checkerboard",
    });
    gpu.write_texture(texture, 0, 0, tw, th, &texels);

    // Create and upload the A8 glyph coverage atlas, then assemble the run.
    let tg = test_glyphs([6.0, 4.0], 22.0);
    let atlas = gpu.create_texture(&TextureDesc {
        width: tg.atlas_size,
        height: tg.atlas_size,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "test-glyph-atlas",
    });
    gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);
    let glyphs = GlyphRunDraw {
        glyphs: tg.glyphs,
        atlas,
        color: tg.color,
        lane: GlyphLane::CoverageA8,
    };

    test_scene(texture, glyphs)
}

/// Render the image-family scene: the high-level [`ImageRect`]/[`NineSlice`]/
/// [`TiledImage`] convenience types all lower into the low-level Image family on
/// the CPU, so this exercises fit/align, nine-patch expansion, the tiled
/// single-instance Repeat fast path, and Nearest vs Linear sampling — end to end
/// through the headless rasterizer.
fn image_family<B: GpuBackend>(gpu: &mut B) -> Vec<Primitive> {
    let (tw, th, texels) = test_texture();
    let texture = gpu.create_texture(&TextureDesc {
        width: tw,
        height: th,
        format: TextureFormat::Bgra8Unorm,
        render_target: false,
        label: "image-family-checkerboard",
    });
    gpu.write_texture(texture, 0, 0, tw, th, &texels);
    let tex_size = [tw, th];

    let mut scene: Vec<Primitive> = Vec::new();

    // Top-left: Contain into a wide box, centered — the 4×4 stays square and is
    // letterboxed with the clear color on both sides. Nearest sampling keeps the
    // cells crisp (no seam blending).
    let contain = ImageRect {
        fit: Fit::Contain,
        align: Align2::CENTER,
        sampler: SamplerDesc {
            filter: FilterMode::Nearest,
            address: AddressMode::ClampToEdge,
        },
        ..ImageRect::new(
            Rect {
                x: 4.0,
                y: 4.0,
                w: 56.0,
                h: 28.0,
            },
            texture,
            tex_size,
        )
    };
    scene.push(Primitive::Image(contain.to_image_draw()));

    // Top-right: Cover into a tall box, top-aligned — fills the box, crops the
    // bottom of the source. Linear sampling blends the cell seams.
    let cover = ImageRect {
        fit: Fit::Cover,
        align: Align2 {
            x: Align::Center,
            y: Align::Start,
        },
        ..ImageRect::new(
            Rect {
                x: 68.0,
                y: 4.0,
                w: 28.0,
                h: 44.0,
            },
            texture,
            tex_size,
        )
    };
    scene.push(Primitive::Image(cover.to_image_draw()));

    // Bottom-left: a nine-slice with a 1px inset — corners stay 1×1 source
    // texels (unscaled), edges/center stretch across a 40×36 box.
    let nine = NineSlice::new(
        texture,
        tex_size,
        Rect {
            x: 4.0,
            y: 52.0,
            w: 40.0,
            h: 36.0,
        },
        1.0,
    );
    for d in nine.to_image_draws() {
        scene.push(Primitive::Image(d));
    }

    // Bottom-right: tile the whole 4×4 texture across a 44×36 box via the
    // single-instance Repeat fast path (uv rect exceeds 0..1).
    let tiled = TiledImage::new(
        texture,
        tex_size,
        Rect {
            x: 52.0,
            y: 52.0,
            w: 44.0,
            h: 36.0,
        },
    );
    for d in tiled.to_image_draws() {
        scene.push(Primitive::Image(d));
    }
    scene
}

/// Render the vector-path scene: a filled+stroked convex outline (a pentagon) and
/// a filled+stroked concave outline (a five-pointed star), tessellated through the
/// retained vector-mesh lane (§13.4) and rasterized headless. Covers fill + stroke
/// over both convex and concave geometry.
fn path() -> Vec<Primitive> {
    let stroke = Stroke::new(2.0, Rgba::new(0.05, 0.1, 0.2, 1.0));

    // Convex: an upright pentagon on the left.
    let pentagon = Path {
        cmds: vec![
            PathCmd::MoveTo(Point::new(32.0, 12.0)),
            PathCmd::LineTo(Point::new(54.0, 30.0)),
            PathCmd::LineTo(Point::new(45.0, 58.0)),
            PathCmd::LineTo(Point::new(19.0, 58.0)),
            PathCmd::LineTo(Point::new(10.0, 30.0)),
            PathCmd::Close,
        ],
        fill: Some(Rgba::new(0.2, 0.6, 0.35, 1.0)),
        shadow: None,
        stroke: Some(stroke),
    };

    // Concave: a five-pointed star on the right (alternating outer/inner radii).
    let mut cmds = Vec::with_capacity(11);
    let (cx, cy, outer, inner) = (94.0f32, 40.0f32, 26.0f32, 11.0f32);
    for k in 0..10 {
        let r = if k % 2 == 0 { outer } else { inner };
        let a = std::f32::consts::FRAC_PI_2 - (k as f32) * std::f32::consts::PI / 5.0;
        let p = Point::new(cx + r * a.cos(), cy - r * a.sin());
        cmds.push(if k == 0 {
            PathCmd::MoveTo(p)
        } else {
            PathCmd::LineTo(p)
        });
    }
    cmds.push(PathCmd::Close);
    let star = Path {
        cmds,
        fill: Some(Rgba::new(0.85, 0.55, 0.15, 1.0)),
        shadow: None,
        stroke: Some(stroke),
    };

    vec![Primitive::Path(pentagon), Primitive::Path(star)]
}

/// Render open-subpath stroke features exercised by D3.3: square and round
/// caps, a real round join, and a dashed stroke — all through the retained
/// vector-mesh lane, rasterized headless.
fn stroke() -> Vec<Primitive> {
    let ink = Rgba::new(0.1, 0.15, 0.25, 1.0);

    // An open L-polyline with square caps and a round join.
    let squared = Path {
        cmds: vec![
            PathCmd::MoveTo(Point::new(16.0, 20.0)),
            PathCmd::LineTo(Point::new(56.0, 20.0)),
            PathCmd::LineTo(Point::new(56.0, 56.0)),
        ],
        fill: None,
        shadow: None,
        stroke: Some(Stroke {
            width: 7.0,
            cap: LineCap::Square,
            join: LineJoin::Round,
            ..Stroke::new(7.0, ink)
        }),
    };

    // An open polyline with round caps and a round join.
    let rounded = Path {
        cmds: vec![
            PathCmd::MoveTo(Point::new(74.0, 56.0)),
            PathCmd::LineTo(Point::new(74.0, 20.0)),
            PathCmd::LineTo(Point::new(112.0, 20.0)),
        ],
        fill: None,
        shadow: None,
        stroke: Some(Stroke {
            width: 7.0,
            cap: LineCap::Round,
            join: LineJoin::Round,
            ..Stroke::new(7.0, ink)
        }),
    };

    // A dashed horizontal stroke across the bottom with round caps per dash.
    let dashed = Path {
        cmds: vec![
            PathCmd::MoveTo(Point::new(16.0, 78.0)),
            PathCmd::LineTo(Point::new(112.0, 78.0)),
        ],
        fill: None,
        shadow: None,
        stroke: Some(Stroke {
            width: 5.0,
            cap: LineCap::Round,
            dash: Some(DashPattern::new(&[10.0, 6.0], 0.0)),
            ..Stroke::new(5.0, Rgba::new(0.7, 0.25, 0.2, 1.0))
        }),
    };

    vec![
        Primitive::Path(squared),
        Primitive::Path(rounded),
        Primitive::Path(dashed),
    ]
}
