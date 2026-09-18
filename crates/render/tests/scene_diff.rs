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
use viso_render::{
    Border, LineCap, Path, PathCmd, Point, Primitive, Quad, Rect, Renderer, Rgba, Stroke,
};

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

fn pt(x: f32, y: f32) -> Point {
    Point { x, y }
}

/// A closed triangle outline anchored at `origin`, filled `fill` and (optionally)
/// stroked `stroke`. Translating `origin` is a pure translation of the outline;
/// changing only `fill`/`stroke` is a pure recolor.
fn tri_scene(origin: (f32, f32), fill: Option<Rgba>, stroke: Option<Rgba>) -> Vec<Primitive> {
    let (x, y) = origin;
    vec![Primitive::Path(Path {
        cmds: vec![
            PathCmd::MoveTo(pt(x, y)),
            PathCmd::LineTo(pt(x + 30.0, y)),
            PathCmd::LineTo(pt(x + 15.0, y + 26.0)),
            PathCmd::Close,
        ],
        fill,
        shadow: None,
        stroke: stroke.map(|color| Stroke::new(2.0, color)),
    })]
}

/// Settle a path scene into the store (two identical frames), returning the
/// steady-state revisions and cumulative tessellation count.
fn settle(gpu: &mut HeadlessRaster, renderer: &mut Renderer, scene: &[Primitive]) {
    renderer.upload(gpu, scene);
    renderer.upload(gpu, scene);
}

#[test]
fn path_geometry_change_retessellates_and_bumps_geometry_alone() {
    let mut gpu = HeadlessRaster::new();
    let mut renderer = new_renderer(&mut gpu);

    let fill = Some(Rgba::new(0.2, 0.6, 0.3, 1.0));
    let small = tri_scene((10.0, 10.0), fill, None);
    settle(&mut gpu, &mut renderer, &small);
    let settled = renderer.scene_revisions();
    let base_tess = renderer.frame_stats().path_tessellations;

    // A different outline shape (extra vertex) — the structural fingerprint
    // changes, so the tessellator must run and only the geometry plane moves.
    let bigger = vec![Primitive::Path(Path {
        cmds: vec![
            PathCmd::MoveTo(pt(10.0, 10.0)),
            PathCmd::LineTo(pt(60.0, 10.0)),
            PathCmd::LineTo(pt(60.0, 50.0)),
            PathCmd::LineTo(pt(10.0, 50.0)),
            PathCmd::Close,
        ],
        fill,
        shadow: None,
        stroke: None,
    })];
    renderer.upload(&mut gpu, &bigger);
    let after = renderer.scene_revisions();
    let stats = renderer.frame_stats();

    assert_eq!(
        after.geometry,
        settled.geometry + 1,
        "a shape change must advance the geometry plane exactly once"
    );
    assert_eq!(
        after.paint, settled.paint,
        "geometry change must not bump paint"
    );
    assert_eq!(
        after.transform, settled.transform,
        "geometry change must not bump transform"
    );
    assert_eq!(
        stats.path_tessellations,
        base_tess + 1,
        "a shape change must re-tessellate exactly once"
    );
}

#[test]
fn path_translation_reuses_geometry_and_bumps_transform_alone() {
    let mut gpu = HeadlessRaster::new();
    let mut renderer = new_renderer(&mut gpu);

    let fill = Some(Rgba::new(0.2, 0.6, 0.3, 1.0));
    settle(
        &mut gpu,
        &mut renderer,
        &tri_scene((10.0, 10.0), fill, None),
    );
    let settled = renderer.scene_revisions();
    let base_tess = renderer.frame_stats().path_tessellations;

    // Same outline shifted by a constant vector: transform-only, no re-tessellate.
    renderer.upload(&mut gpu, &tri_scene((25.0, 18.0), fill, None));
    let after = renderer.scene_revisions();
    let stats = renderer.frame_stats();

    assert_eq!(
        after.transform,
        settled.transform + 1,
        "a pure translation must advance the transform plane exactly once"
    );
    assert_eq!(
        after.geometry, settled.geometry,
        "a translation must not touch the geometry plane"
    );
    assert_eq!(
        after.paint, settled.paint,
        "a translation must not touch paint"
    );
    assert_eq!(
        stats.path_tessellations, base_tess,
        "a pure translation must not re-tessellate"
    );
    assert_eq!(stats.dirty_primitives, 1);
}

#[test]
fn path_recolor_reuses_geometry_and_bumps_paint_alone() {
    let mut gpu = HeadlessRaster::new();
    let mut renderer = new_renderer(&mut gpu);

    let green = Some(Rgba::new(0.2, 0.6, 0.3, 1.0));
    let red = Some(Rgba::new(0.8, 0.1, 0.1, 1.0));
    settle(
        &mut gpu,
        &mut renderer,
        &tri_scene((10.0, 10.0), green, None),
    );
    let settled = renderer.scene_revisions();
    let base_tess = renderer.frame_stats().path_tessellations;

    // Only the fill color changes: paint-only, geometry reused.
    renderer.upload(&mut gpu, &tri_scene((10.0, 10.0), red, None));
    let after = renderer.scene_revisions();
    let stats = renderer.frame_stats();

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
        stats.path_tessellations, base_tess,
        "a recolor must not re-tessellate"
    );
}

#[test]
fn path_unchanged_reuses_geometry_and_bumps_nothing() {
    let mut gpu = HeadlessRaster::new();
    let mut renderer = new_renderer(&mut gpu);

    let scene = tri_scene(
        (10.0, 10.0),
        Some(Rgba::new(0.2, 0.6, 0.3, 1.0)),
        Some(Rgba::new(0.1, 0.1, 0.1, 1.0)),
    );
    // Cold frame appends + tessellates once (`path_tessellations` is per-frame).
    renderer.upload(&mut gpu, &scene);
    assert_eq!(
        renderer.frame_stats().path_tessellations,
        1,
        "the cold append must tessellate once"
    );

    // Steady frame: identical fill+stroke path — no plane, no re-tessellate.
    renderer.upload(&mut gpu, &scene);
    let settled = renderer.scene_revisions();
    renderer.upload(&mut gpu, &scene);
    let after = renderer.scene_revisions();
    let stats = renderer.frame_stats();

    assert_eq!(
        after, settled,
        "an unchanged path must not bump any revision plane"
    );
    assert_eq!(
        stats.path_tessellations, 0,
        "an unchanged path must not re-tessellate"
    );
    assert_eq!(stats.dirty_primitives, 0);
    assert_eq!(stats.visible_primitives, 1);
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

/// A stroked triangle whose stroke carries a full style (not just color).
fn tri_stroked(origin: (f32, f32), stroke: Stroke) -> Vec<Primitive> {
    let (x, y) = origin;
    vec![Primitive::Path(Path {
        cmds: vec![
            PathCmd::MoveTo(pt(x, y)),
            PathCmd::LineTo(pt(x + 30.0, y)),
            PathCmd::LineTo(pt(x + 15.0, y + 26.0)),
            PathCmd::Close,
        ],
        fill: None,
        shadow: None,
        stroke: Some(stroke),
    })]
}

#[test]
fn stroke_style_change_retessellates_and_bumps_geometry_alone() {
    let mut gpu = HeadlessRaster::new();
    let mut renderer = new_renderer(&mut gpu);

    let color = Rgba::new(0.1, 0.1, 0.1, 1.0);
    let base = Stroke::new(2.0, color);
    settle(&mut gpu, &mut renderer, &tri_stroked((10.0, 10.0), base));
    let settled = renderer.scene_revisions();
    let base_tess = renderer.frame_stats().path_tessellations;

    // Widen the stroke and switch its cap: pure stroke *geometry*, so the
    // fingerprint moves and only the geometry plane advances.
    let restyled = Stroke {
        width: 5.0,
        cap: LineCap::Round,
        ..base
    };
    renderer.upload(&mut gpu, &tri_stroked((10.0, 10.0), restyled));
    let after = renderer.scene_revisions();
    let stats = renderer.frame_stats();

    assert_eq!(
        after.geometry,
        settled.geometry + 1,
        "a stroke-style change must advance the geometry plane exactly once"
    );
    assert_eq!(
        after.paint, settled.paint,
        "a stroke-style change must not bump paint"
    );
    assert_eq!(
        after.transform, settled.transform,
        "a stroke-style change must not bump transform"
    );
    assert_eq!(
        stats.path_tessellations,
        base_tess + 1,
        "a stroke-style change must re-tessellate exactly once"
    );
}

#[test]
fn stroke_color_change_reuses_geometry_and_bumps_paint_alone() {
    let mut gpu = HeadlessRaster::new();
    let mut renderer = new_renderer(&mut gpu);

    let dark = Rgba::new(0.1, 0.1, 0.1, 1.0);
    let bright = Rgba::new(0.9, 0.3, 0.2, 1.0);
    // Keep every shape field fixed; only the stroke color differs.
    let style = |c| Stroke {
        width: 4.0,
        cap: LineCap::Square,
        ..Stroke::new(4.0, c)
    };
    settle(
        &mut gpu,
        &mut renderer,
        &tri_stroked((10.0, 10.0), style(dark)),
    );
    let settled = renderer.scene_revisions();
    let base_tess = renderer.frame_stats().path_tessellations;

    renderer.upload(&mut gpu, &tri_stroked((10.0, 10.0), style(bright)));
    let after = renderer.scene_revisions();
    let stats = renderer.frame_stats();

    assert_eq!(
        after.paint,
        settled.paint + 1,
        "a stroke recolor must advance the paint plane exactly once"
    );
    assert_eq!(
        after.geometry, settled.geometry,
        "a stroke recolor must not touch the geometry plane"
    );
    assert_eq!(
        stats.path_tessellations, base_tess,
        "a stroke recolor must not re-tessellate"
    );
}
