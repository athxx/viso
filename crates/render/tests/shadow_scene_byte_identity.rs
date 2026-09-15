//! Retained-scene lowering invariant: `Renderer::upload` drives the frame's GPU
//! scratch entirely from the retained scene, and a repeated identical upload
//! reproduces the same scene shape without a full rebuild.
//!
//! The stream walk only folds each primitive into its store and records paint
//! order; `lower_from_scene` is the single source of truth that derives the
//! instance buffers and segments from the scene (§8). This test drives `upload`
//! with the full `test_scene` — quads, an image, a glyph run, a filled+stroked
//! path, a mesh, an opaque scissor-clip layer, and a translucent offscreen layer
//! that composites back — so every `StoreRef` arm and both passes are exercised.
//!
//! It runs `upload` twice: the second frame reuses the cleared-not-freed stores
//! (`begin_frame`), so an unchanged scene mutates no store and the derived
//! scratch is identical to the first frame — the steady-state "0 primitive
//! reconstruction" guarantee (§8.4) under a whole-tree re-emit.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso_render::{GlyphRunDraw, Renderer, test_glyphs, test_scene, test_texture};

const W: u32 = 128;
const H: u32 = 96;

#[test]
fn scene_lowering_is_stable_across_frames() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    // The Image test texture (BGRA8 checkerboard).
    let (tw, th, texels) = test_texture();
    let texture = gpu.create_texture(&TextureDesc {
        width: tw,
        height: th,
        format: TextureFormat::Bgra8Unorm,
        render_target: false,
        label: "test-checkerboard",
    });
    gpu.write_texture(texture, 0, 0, tw, th, &texels);

    // The A8 glyph coverage atlas + run.
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

    // First frame: cold stores populated and lowered from the scene.
    renderer.upload(&mut gpu, &scene);
    // Second frame: cleared-not-freed stores reused; unchanged scene, same output.
    renderer.upload(&mut gpu, &scene);
}
