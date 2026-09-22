//! Golden test: render a Quad test scene through the full renderer → headless
//! rasterizer pipeline and compare the pixels against a committed baseline.
//!
//! The baseline is a raw BGRA8 byte dump (top-left origin) stored next to this
//! file. Set `BLESS=1` to (re)generate it. Comparison is per-channel with a
//! small tolerance so it survives trivial rounding differences and, later, the
//! Metal backend's readback.
//!
//! This closes the Phase 2 vertical slice for Quad: primitive → instance →
//! batch → headless raster → readback → golden, with no GPU required.

use std::path::PathBuf;

use viso_gpu::{
    AddressMode, FilterMode, GpuBackend, HeadlessRaster, RawWindowHandle, SamplerDesc, TextureDesc,
    TextureFormat,
};
use viso_render::{
    Align, Align2, DashPattern, Fit, GlyphLane, GlyphRunDraw, ImageRect, LineCap, LineJoin,
    NineSlice, Path, PathCmd, Point, Primitive, Rect, Renderer, Rgba, Stroke, TiledImage,
    test_glyphs, test_scene, test_texture,
};

const W: u32 = 128;
const H: u32 = 96;
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// Render the scene through the headless backend and return BGRA8 bytes.
fn render_scene() -> Vec<u8> {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

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

    let scene = test_scene(texture, glyphs);
    renderer.upload(&mut gpu, &scene);
    // Clear to opaque dark gray.
    renderer.submit(
        &mut gpu,
        surface,
        [0.1, 0.1, 0.1, 1.0],
        [W as f32, H as f32],
    );

    gpu.read_pixels_bgra8(surface)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/quad_scene.bgra8")
}

#[test]
fn quad_scene_matches_golden() {
    let actual = render_scene();
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

/// Render the image-family scene: the high-level [`ImageRect`]/[`NineSlice`]/
/// [`TiledImage`] convenience types all lower into the low-level Image family on
/// the CPU, so this exercises fit/align, nine-patch expansion, the tiled
/// single-instance Repeat fast path, and Nearest vs Linear sampling — end to end
/// through the headless rasterizer.
fn render_image_scene() -> Vec<u8> {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

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

    renderer.upload(&mut gpu, &scene);
    renderer.submit(
        &mut gpu,
        surface,
        [0.1, 0.1, 0.1, 1.0],
        [W as f32, H as f32],
    );
    gpu.read_pixels_bgra8(surface)
}

/// Render the vector-path scene: a filled+stroked convex outline (a pentagon) and
/// a filled+stroked concave outline (a five-pointed star), tessellated through the
/// retained vector-mesh lane (§13.4) and rasterized headless. Covers fill + stroke
/// over both convex and concave geometry.
fn render_path_scene() -> Vec<u8> {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

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

    let scene = vec![Primitive::Path(pentagon), Primitive::Path(star)];
    renderer.upload(&mut gpu, &scene);
    renderer.submit(
        &mut gpu,
        surface,
        [0.1, 0.1, 0.1, 1.0],
        [W as f32, H as f32],
    );
    gpu.read_pixels_bgra8(surface)
}

#[test]
fn path_scene_matches_golden() {
    let actual = render_path_scene();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/path_scene.bgra8");

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

/// Render open-subpath stroke features exercised by D3.3: square and round
/// caps, a real round join, and a dashed stroke — all through the retained
/// vector-mesh lane, rasterized headless.
fn render_stroke_scene() -> Vec<u8> {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

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

    let scene = vec![
        Primitive::Path(squared),
        Primitive::Path(rounded),
        Primitive::Path(dashed),
    ];
    renderer.upload(&mut gpu, &scene);
    renderer.submit(
        &mut gpu,
        surface,
        [0.1, 0.1, 0.1, 1.0],
        [W as f32, H as f32],
    );
    gpu.read_pixels_bgra8(surface)
}

#[test]
fn stroke_scene_matches_golden() {
    let actual = render_stroke_scene();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/stroke_scene.bgra8");

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

#[test]
fn image_family_scene_matches_golden() {
    let actual = render_image_scene();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/image_family.bgra8");

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
