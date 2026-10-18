//! Transform roots (§21): moving a root re-encodes its draws and rewrites no
//! instance.
//!
//! Each case paints a scene once, moves its roots with `set_transforms`, and
//! requires the frame to be pixel-identical to the same scene painted from
//! scratch at the moved positions — while uploading nothing.

use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, SurfaceId};
use viso_render::{
    Border, LayerClip, Primitive, Quad, Rect, Renderer, Rgba, TransformSlot, TransformUpdate,
};

const W: u32 = 64;
const H: u32 = 64;

fn renderer() -> (CpuRaster, SurfaceId, Renderer) {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([W as f32, H as f32]);
    (gpu, surface, r)
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn quad(r: Rect, color: Rgba) -> Primitive {
    Primitive::Quad(Quad {
        rect: r,
        color,
        radius: 0.0,
        border: Border::NONE,
    })
}

fn clip(r: Rect) -> Primitive {
    Primitive::Layer(LayerClip {
        clip: r,
        opacity: 1.0,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    })
}

fn draw(gpu: &mut CpuRaster, surface: SurfaceId, r: &mut Renderer) -> Vec<u8> {
    r.submit(gpu, surface, [0.0, 0.0, 0.0, 1.0], [W as f32, H as f32]);
    gpu.read_pixels_bgra8(surface)
}

/// A viewport clipped to the top half, scrolled by `dy`: three rows, the last
/// under a clip of its own that scrolls with the content. With `root`, the rows
/// sit in transform root 0.
fn scrolled(dy: f32, root: bool) -> Vec<Primitive> {
    let mut out = vec![quad(
        rect(0.0, 0.0, 64.0, 64.0),
        Rgba::new(0.2, 0.2, 0.2, 1.0),
    )];
    out.push(clip(rect(0.0, 0.0, 64.0, 32.0)));
    if root {
        out.push(Primitive::Transform(TransformSlot(0)));
    }
    out.push(quad(
        rect(0.0, dy, 64.0, 16.0),
        Rgba::new(1.0, 0.0, 0.0, 1.0),
    ));
    out.push(quad(
        rect(4.0, 16.0 + dy, 56.0, 16.0),
        Rgba::new(0.0, 1.0, 0.0, 1.0),
    ));
    out.push(clip(rect(0.0, 36.0 + dy, 32.0, 12.0)));
    out.push(quad(
        rect(0.0, 32.0 + dy, 64.0, 16.0),
        Rgba::new(0.0, 0.0, 1.0, 1.0),
    ));
    out.push(Primitive::LayerEnd);
    if root {
        out.push(Primitive::TransformEnd);
    }
    out.push(Primitive::LayerEnd);
    out
}

#[test]
fn a_moved_root_draws_what_painting_it_there_draws_and_uploads_nothing() {
    let (mut gpu, surface, mut moved) = renderer();
    moved.upload_with_transforms(&mut gpu, &scrolled(0.0, true), &[[0.0, 0.0]]);
    let still = draw(&mut gpu, surface, &mut moved);
    assert_eq!(
        moved.set_transforms(&[[0.0, -20.0]]),
        TransformUpdate::Moved
    );
    let shifted = draw(&mut gpu, surface, &mut moved);

    let (mut gpu2, surface2, mut fresh) = renderer();
    fresh.upload(&mut gpu2, &scrolled(-20.0, false));
    let expected = draw(&mut gpu2, surface2, &mut fresh);

    assert_ne!(still, shifted, "the scroll is visible");
    assert_eq!(
        shifted, expected,
        "a shift equals painting at the new place"
    );

    // Moving back restores the painted frame.
    assert_eq!(
        moved.set_transforms(&[[0.0, 0.0]]),
        TransformUpdate::Unchanged
    );
    assert_eq!(draw(&mut gpu, surface, &mut moved), still);
}

#[test]
fn rows_in_one_root_still_batch_and_a_static_neighbour_does_not_join_them() {
    let (mut gpu, _surface, mut r) = renderer();
    let mut stream = vec![Primitive::Transform(TransformSlot(0))];
    for i in 0..8 {
        stream.push(quad(
            rect(0.0, i as f32 * 8.0, 64.0, 8.0),
            Rgba::new(1.0, 1.0, 1.0, 1.0),
        ));
    }
    stream.push(Primitive::TransformEnd);
    stream.push(quad(
        rect(0.0, 0.0, 8.0, 8.0),
        Rgba::new(1.0, 1.0, 1.0, 1.0),
    ));
    r.upload_with_transforms(&mut gpu, &stream, &[[0.0, 0.0]]);
    assert_eq!(r.frame_stats().draw_calls, 2);
}

#[test]
fn a_root_holding_a_translucent_layer_asks_for_an_upload() {
    let (mut gpu, _surface, mut r) = renderer();
    let stream = vec![
        Primitive::Transform(TransformSlot(0)),
        Primitive::Layer(LayerClip {
            clip: rect(0.0, 0.0, 32.0, 32.0),
            opacity: 0.5,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }),
        quad(rect(0.0, 0.0, 16.0, 16.0), Rgba::new(1.0, 0.0, 0.0, 1.0)),
        quad(rect(8.0, 8.0, 16.0, 16.0), Rgba::new(0.0, 1.0, 0.0, 1.0)),
        Primitive::LayerEnd,
        Primitive::TransformEnd,
        Primitive::Transform(TransformSlot(1)),
        quad(rect(32.0, 32.0, 16.0, 16.0), Rgba::new(0.0, 0.0, 1.0, 1.0)),
        Primitive::TransformEnd,
    ];
    r.upload_with_transforms(&mut gpu, &stream, &[[0.0, 0.0], [0.0, 0.0]]);
    assert_eq!(
        r.set_transforms(&[[0.0, 0.0], [5.0, 0.0]]),
        TransformUpdate::Moved
    );
    assert_eq!(
        r.set_transforms(&[[5.0, 0.0], [5.0, 0.0]]),
        TransformUpdate::NeedsUpload
    );
}

#[test]
fn a_plain_upload_never_moves() {
    let (mut gpu, surface, mut r) = renderer();
    r.upload(&mut gpu, &scrolled(0.0, true));
    let before = draw(&mut gpu, surface, &mut r);
    assert_eq!(
        r.set_transforms(&[[0.0, -20.0]]),
        TransformUpdate::Unchanged
    );
    assert_eq!(draw(&mut gpu, surface, &mut r), before);
}
