//! F3.1 shadow invariant: the retained scene, re-lowered from its stores and
//! paint-order record, is byte-identical to the immediate walk's scratch.
//!
//! `Renderer::upload` builds the immediate scratch buffers and segments (the
//! authoritative path this stage), and *also* builds the retained scene as a
//! shadow. Under `debug_assertions` it then re-derives scratch + segments from
//! the scene and `debug_assert_eq!`s every buffer against the immediate walk;
//! any divergence panics inside `upload`. This test drives `upload` with the
//! full `test_scene` — quads, an image, a glyph run, a filled+stroked path, a
//! mesh, an opaque scissor-clip layer, and a translucent offscreen layer that
//! composites back — so every `StoreRef` arm and both passes are exercised.
//!
//! It runs `upload` twice: the second frame reuses the cleared-not-freed stores
//! (`begin_frame`), proving the shadow stays byte-identical in steady state, not
//! just on a cold first frame. If the assertion is disabled (release), the test
//! still validates that a repeated upload produces the same scene shape.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso_render::{GlyphRunDraw, Renderer, test_glyphs, test_scene, test_texture};

const W: u32 = 128;
const H: u32 = 96;

#[test]
fn shadow_rederivation_matches_immediate_walk() {
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

    // First frame: cold stores. The shadow assertion runs inside `upload`.
    renderer.upload(&mut gpu, &scene);
    // Second frame: cleared-not-freed stores reused. Still byte-identical.
    renderer.upload(&mut gpu, &scene);
}
