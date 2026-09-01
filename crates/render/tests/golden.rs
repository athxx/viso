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

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso_render::{GlyphRunDraw, Renderer, test_glyphs, test_scene, test_texture};

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

    // Create and upload the R8 glyph SDF atlas, then assemble the run.
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
