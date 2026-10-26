//! Transform roots: moving a root re-encodes its draws and rewrites no
//! instance.
//!
//! Each case paints a scene once, moves its roots with `set_transforms`, and
//! requires the frame to be pixel-identical to the same scene painted from
//! scratch at the moved positions — while uploading nothing.

use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, SurfaceId};
use viso_render::{
    Affine2, Border, LayerClip, Primitive, Quad, Rect, Renderer, Rgba, TransformSlot,
    TransformUpdate,
};

const W: u32 = 64;
const H: u32 = 64;

fn renderer() -> (CpuRaster, SurfaceId, Renderer) {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([W as f32, H as f32], 1.0);
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
    moved.upload_with_transforms(
        &mut gpu,
        &scrolled(0.0, true),
        &[Affine2::translate(0.0, 0.0)],
    );
    let still = draw(&mut gpu, surface, &mut moved);
    assert_eq!(
        moved.set_transforms(&[Affine2::translate(0.0, -20.0)]),
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
        moved.set_transforms(&[Affine2::translate(0.0, 0.0)]),
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
    r.upload_with_transforms(&mut gpu, &stream, &[Affine2::translate(0.0, 0.0)]);
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
    r.upload_with_transforms(
        &mut gpu,
        &stream,
        &[Affine2::translate(0.0, 0.0), Affine2::translate(0.0, 0.0)],
    );
    assert_eq!(
        r.set_transforms(&[Affine2::translate(0.0, 0.0), Affine2::translate(5.0, 0.0)]),
        TransformUpdate::Moved
    );
    assert_eq!(
        r.set_transforms(&[Affine2::translate(5.0, 0.0), Affine2::translate(5.0, 0.0)]),
        TransformUpdate::NeedsUpload
    );
}

#[test]
fn a_plain_upload_never_moves() {
    let (mut gpu, surface, mut r) = renderer();
    r.upload(&mut gpu, &scrolled(0.0, true));
    let before = draw(&mut gpu, surface, &mut r);
    assert_eq!(
        r.set_transforms(&[Affine2::translate(0.0, -20.0)]),
        TransformUpdate::Unchanged
    );
    assert_eq!(draw(&mut gpu, surface, &mut r), before);
}

/// Whether pixel `(x, y)` of a BGRA frame is mostly red.
fn red(frame: &[u8], x: u32, y: u32) -> bool {
    let at = ((y * W + x) * 4) as usize;
    frame[at + 2] > 200 && frame[at + 1] < 60 && frame[at] < 60
}

/// A quarter turn clockwise about `(32, 32)`.
fn quarter_turn() -> Affine2 {
    Affine2::translate(-32.0, -32.0)
        .then(&Affine2::from_rotation(std::f32::consts::FRAC_PI_2))
        .then(&Affine2::translate(32.0, 32.0))
}

#[test]
fn a_turned_root_draws_its_content_turned_and_uploads_nothing() {
    let (mut gpu, surface, mut r) = renderer();
    // A tall bar, 20 wide and 40 high, centered on (32, 32).
    let stream = vec![
        Primitive::Transform(TransformSlot(0)),
        quad(rect(22.0, 12.0, 20.0, 40.0), Rgba::new(1.0, 0.0, 0.0, 1.0)),
        Primitive::TransformEnd,
    ];
    r.upload_with_transforms(&mut gpu, &stream, &[Affine2::IDENTITY]);
    let upright = draw(&mut gpu, surface, &mut r);
    assert!(red(&upright, 32, 14) && !red(&upright, 14, 32));
    assert_eq!(r.set_transforms(&[quarter_turn()]), TransformUpdate::Moved);
    let turned = draw(&mut gpu, surface, &mut r);
    assert!(red(&turned, 14, 32), "the bar lies across");
    assert!(red(&turned, 50, 32));
    assert!(!red(&turned, 32, 14), "and no longer stands");
    assert!(!red(&turned, 32, 50));
}

#[test]
fn a_clip_inside_a_turned_root_turns_with_it() {
    let (mut gpu, surface, mut r) = renderer();
    // The bar clipped to its top half, then the whole root turned: the half
    // that stays lies on the right.
    let stream = vec![
        Primitive::Transform(TransformSlot(0)),
        clip(rect(0.0, 0.0, 64.0, 32.0)),
        quad(rect(22.0, 12.0, 20.0, 40.0), Rgba::new(1.0, 0.0, 0.0, 1.0)),
        Primitive::LayerEnd,
        Primitive::TransformEnd,
    ];
    r.upload_with_transforms(&mut gpu, &stream, &[quarter_turn()]);
    let frame = draw(&mut gpu, surface, &mut r);
    assert!(red(&frame, 48, 32), "the kept half turned onto the right");
    assert!(!red(&frame, 16, 32), "the clipped half stays away");

    // A scissored clip in a root that turns later cannot follow it.
    let (mut gpu, _surface, mut r) = renderer();
    r.upload_with_transforms(&mut gpu, &stream, &[Affine2::IDENTITY]);
    assert_eq!(
        r.set_transforms(&[quarter_turn()]),
        TransformUpdate::NeedsUpload
    );
}

/// A red line from `(8, 16)` to `(24, 16)` built as its centerline, each
/// vertex extruded half its 4-point width, in transform root 0.
fn extruded_line() -> Vec<Primitive> {
    let red = [1.0, 0.0, 0.0, 1.0];
    let v = |x: f32, side: f32| viso_render::MeshVertex {
        extrude: [0.0, 2.0 * side],
        ..viso_render::MeshVertex::new([x, 16.0], red, 1.0)
    };
    vec![
        Primitive::Transform(TransformSlot(0)),
        Primitive::Mesh(viso_render::Mesh::new(
            vec![v(8.0, -1.0), v(24.0, -1.0), v(24.0, 1.0), v(8.0, 1.0)],
            vec![0, 1, 2, 0, 2, 3],
        )),
        Primitive::TransformEnd,
    ]
}

/// The red pixels of `frame` in column `x` and in row `y`.
fn red_runs(frame: &[u8], x: usize, y: usize) -> (usize, usize) {
    let red = |i: usize| frame[i * 4 + 2] > 128 && frame[i * 4 + 1] < 64;
    let column = (0..H as usize).filter(|&r| red(r * W as usize + x)).count();
    let row = (0..W as usize).filter(|&c| red(y * W as usize + c)).count();
    (column, row)
}

#[test]
fn an_extruded_line_keeps_its_width_as_its_root_zooms_and_turns() {
    let (mut gpu, surface, mut r) = renderer();
    r.upload_with_transforms(&mut gpu, &extruded_line(), &[Affine2::IDENTITY]);
    let flat = draw(&mut gpu, surface, &mut r);
    assert_eq!(red_runs(&flat, 16, 16), (4, 16), "4 points wide, 16 long");
    // A flat line moved: its extent has no height, its extrusion does.
    assert_eq!(
        r.set_transforms(&[Affine2::translate(0.0, 10.0)]),
        TransformUpdate::Moved
    );
    let frame = draw(&mut gpu, surface, &mut r);
    assert_eq!(red_runs(&frame, 16, 26), (4, 16), "moved");

    // Twice the size about the line's start: twice as long, as wide.
    let zoomed = Affine2::translate(-8.0, -16.0)
        .then(&Affine2::scale(2.0, 2.0))
        .then(&Affine2::translate(8.0, 32.0));
    assert_eq!(r.set_transforms(&[zoomed]), TransformUpdate::Moved);
    let frame = draw(&mut gpu, surface, &mut r);
    assert_eq!(red_runs(&frame, 24, 32), (4, 32));

    // A quarter turn as well: the line runs down the view, still 4 wide.
    let turned = Affine2::translate(-8.0, -16.0)
        .then(&Affine2::scale(2.0, 2.0))
        .then(&Affine2::from_rotation(std::f32::consts::FRAC_PI_2))
        .then(&Affine2::translate(32.0, 8.0));
    assert_eq!(r.set_transforms(&[turned]), TransformUpdate::Moved);
    let frame = draw(&mut gpu, surface, &mut r);
    assert_eq!(red_runs(&frame, 32, 24), (32, 4));
}
