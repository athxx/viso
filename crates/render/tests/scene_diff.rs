//! F3.2 ingest-diff invariants: the retained scene bumps only the revision
//! plane a change actually moved, and an unchanged scene mutates nothing.
//!
//! The renderer ingests the same flat `&[Primitive]` stream every frame, but
//! the retained stores persist across frames (`begin_frame` resets a cursor, it
//! does not clear entries). Each `ingest_*` diffs the incoming primitive against
//! the retained entry at its positional slot and reports which fields moved; the
//! scene bumps only the matching planes (§8.4). These tests drive `upload`
//! across frames and read back [`Renderer::scene_revisions`] and
//! [`Renderer::frame_stats`] to prove:
//!
//! - a paint-only edit (a quad recolor) advances the `paint` plane alone and
//!   leaves `geometry`/`transform`/`clip`/`resource` exactly where they were;
//! - re-uploading an identical scene mutates no store and dirties no primitive
//!   (`dirty_primitives == 0`), the "0 primitive reconstruction" guarantee under
//!   a whole-tree re-emit.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso_render::{Border, Primitive, Quad, Rect, Renderer, Rgba};

const W: u32 = 128;
const H: u32 = 96;

/// A minimal one-quad scene at a fixed rect, painted `color`.
fn quad_scene(color: Rgba) -> Vec<Primitive> {
    vec![Primitive::Quad(Quad {
        rect: Rect {
            x: 10.0,
            y: 12.0,
            w: 40.0,
            h: 24.0,
        },
        color,
        radius: 4.0,
        border: Border::NONE,
    })]
}

fn new_renderer(gpu: &mut HeadlessRaster) -> Renderer {
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    Renderer::new(gpu, format)
}

#[test]
fn paint_only_change_bumps_paint_plane_alone() {
    let mut gpu = HeadlessRaster::new();
    let mut renderer = new_renderer(&mut gpu);

    let red = Rgba::new(0.8, 0.1, 0.1, 1.0);
    let blue = Rgba::new(0.1, 0.2, 0.9, 1.0);

    // Two identical frames settle the store: the second frame re-visits the
    // same slot with the same value, so nothing moves.
    renderer.upload(&mut gpu, &quad_scene(red));
    renderer.upload(&mut gpu, &quad_scene(red));
    let settled = renderer.scene_revisions();

    // Third frame: only the fill color changed. The quad's rect/radius/border
    // are untouched, so this is a pure recolor.
    renderer.upload(&mut gpu, &quad_scene(blue));
    let after = renderer.scene_revisions();

    // The paint plane advanced, and only it.
    assert_eq!(
        after.paint,
        settled.paint + 1,
        "a recolor must advance the paint plane exactly once"
    );
    assert_eq!(
        after.geometry, settled.geometry,
        "a recolor must not touch the geometry plane"
    );
    assert_eq!(
        after.transform, settled.transform,
        "a recolor must not touch the transform plane"
    );
    assert_eq!(
        after.clip, settled.clip,
        "a recolor must not touch the clip plane"
    );
    assert_eq!(
        after.resource, settled.resource,
        "a recolor must not touch the resource plane"
    );

    // The recolored quad is counted as dirty; it is the only visible primitive.
    let stats = renderer.frame_stats();
    assert_eq!(stats.visible_primitives, 1);
    assert_eq!(stats.dirty_primitives, 1);
}

#[test]
fn identical_reupload_dirties_nothing() {
    let mut gpu = HeadlessRaster::new();
    let mut renderer = new_renderer(&mut gpu);

    let scene = quad_scene(Rgba::new(0.3, 0.6, 0.4, 1.0));

    // First frame is cold: the slot is appended, so it reads as dirty.
    renderer.upload(&mut gpu, &scene);
    let cold = renderer.scene_revisions();
    assert_eq!(renderer.frame_stats().dirty_primitives, 1);

    // Second frame re-visits the same slot with the same value: no store
    // mutation, no plane bump, no dirty primitive — but still one visible.
    renderer.upload(&mut gpu, &scene);
    let steady = renderer.scene_revisions();
    let stats = renderer.frame_stats();

    assert_eq!(
        steady, cold,
        "re-uploading an identical scene must not bump any revision plane"
    );
    assert_eq!(
        stats.dirty_primitives, 0,
        "an unchanged primitive must not be counted dirty"
    );
    assert_eq!(
        stats.visible_primitives, 1,
        "the primitive is still visible even though it did not change"
    );
    assert_eq!(stats.quad_instances, 1);
}
