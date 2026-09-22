//! The 1.0 render-runtime acceptance gate (§33).
//!
//! Every other test file in this crate defends one mechanism against regression.
//! This one is different in kind: it is the shipping checklist, and each test states
//! one line of it as an assertion a release either satisfies or does not. The
//! statements are deliberately coarse — "a rounded container does not clip its
//! children", "three color effects cost no extra pass" — because a checklist item is
//! an outcome a user can observe, not an implementation the detailed contracts
//! already pin (`clip.rs`, `backdrop_contract.rs`, `color_effect_contract.rs`,
//! `effect_planner_contract.rs`, `scene_diff.rs`, `color_domain.rs`, `vector_lane.rs`,
//! `texture_binding.rs`, `culling.rs`, `gpu_specialization.rs`).
//!
//! The value of stating them together is that the checklist stops being a document
//! someone reads. A change that keeps every mechanism test green while quietly
//! reintroducing a forbidden default — a `saveLayer` for group opacity, a blur layer
//! for a drop shadow, a full-screen backdrop capture — fails here.
//!
//! Two items are not stateable from this crate and live where their subject does:
//! "default UI does not globally enable MSAA" is asserted over the shipped pipeline
//! manifest in `viso-shader`, and the brush model's explicit semantics — including
//! that the two unlowered brushes reject rather than no-op — are asserted in
//! `scene::store`, where `BrushStore` is reachable.

use viso_gpu::{
    ColorSpace, GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureDesc, TextureFormat,
    f16_to_f32,
};
use viso_render::{
    AnalyticCapsule, AnalyticEllipse, AnalyticLine, AnalyticRRect, AnalyticShadow, Border,
    ChildOverlap, ClipShape, ClipTier, ColorEffect, Corners, DashPattern, FrameStats,
    GlyphInstanceData, GlyphLane, GlyphRunDraw, ImageDraw, LayerClip, LayerReason, LineCap,
    LineJoin, Mesh, MeshVertex, OpacityPlan, Path, PathArena, PathCmd, Point, Primitive, Quad,
    ReasonSet, Rect, Renderer, Rgba, SamplerDesc, ShadowShape, Stroke, StrokeAlign, VectorLane,
    VectorWorkload, clips_children, plan_clip, plan_group_opacity,
};

const W: u32 = 256;
const H: u32 = 256;

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn pt(x: f32, y: f32) -> Point {
    Point { x, y }
}

fn quad(r: Rect) -> Primitive {
    Primitive::Quad(Quad {
        rect: r,
        color: Rgba::new(0.4, 0.5, 0.6, 1.0),
        radius: 0.0,
        border: Border::NONE,
    })
}

/// A surface of `w`×`h` with a renderer sized to it.
fn renderer(w: u32, h: u32) -> (HeadlessRaster, SurfaceId, Renderer) {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);
    let format: TextureFormat = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([w as f32, h as f32]);
    (gpu, surface, r)
}

fn upload(r: &mut Renderer, gpu: &mut HeadlessRaster, prims: &[Primitive]) -> FrameStats {
    r.upload(gpu, prims);
    r.frame_stats()
}

/// Draw `prims` all the way to the surface and hand back the presented pixels.
fn present(
    r: &mut Renderer,
    gpu: &mut HeadlessRaster,
    surface: SurfaceId,
    clear: [f32; 4],
    prims: &[Primitive],
) -> FrameStats {
    r.upload(gpu, prims);
    r.submit(gpu, surface, clear, [W as f32, H as f32]);
    r.frame_stats()
}

// ===========================================================================
// 1. Every shape has an explicit primitive path
// ===========================================================================

/// Rect, RRect, Circle, Ellipse, Line, Arc, Path, Image, Text and Mesh, drawn in
/// one frame: ten shapes, ten primitives that reach the GPU, none of them a
/// fallback for another.
///
/// The checklist item is about *routes*, so the assertion counts them. A shape
/// silently lowered through a neighbour's path — a circle emitted as a 64-segment
/// polygon, a line as a thin quad — would still paint something, which is why
/// "it looks right" is not the test. Circle and Ellipse deliberately share
/// `AnalyticEllipse`: a circle is an ellipse with equal axes, and one shader
/// evaluating one closed form is the explicit path for both, not a missing one.
/// Arc shares `Path` for the same kind of reason — it lowers to canonical cubics
/// through [`PathArena::arc`], which is a documented route rather than a
/// tessellator guessing.
#[test]
fn every_shape_has_its_own_primitive() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);

    let atlas = gpu.create_texture(&TextureDesc {
        width: 64,
        height: 64,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "text",
    });
    let image = gpu.create_texture(&TextureDesc {
        width: 32,
        height: 32,
        format: TextureFormat::Rgba8Unorm,
        render_target: false,
        label: "image",
    });

    let mut arc = PathArena::new();
    arc.arc(pt(200.0, 60.0), 24.0, 0.0, std::f32::consts::FRAC_PI_2);

    let scene = vec![
        // Rect.
        quad(rect(8.0, 8.0, 40.0, 24.0)),
        // RRect, with genuinely per-corner radii.
        Primitive::AnalyticRRect(AnalyticRRect {
            rect: rect(56.0, 8.0, 40.0, 24.0),
            color: Rgba::new(0.8, 0.3, 0.3, 1.0),
            radius: Corners {
                left_top: 8.0,
                right_top: 2.0,
                right_bottom: 8.0,
                left_bottom: 2.0,
            },
            border: Border::NONE,
        }),
        // Circle: an ellipse with equal axes.
        Primitive::AnalyticEllipse(AnalyticEllipse {
            rect: rect(104.0, 8.0, 24.0, 24.0),
            color: Rgba::new(0.3, 0.8, 0.3, 1.0),
            border: Border::NONE,
        }),
        // Ellipse.
        Primitive::AnalyticEllipse(AnalyticEllipse {
            rect: rect(136.0, 8.0, 40.0, 20.0),
            color: Rgba::new(0.3, 0.6, 0.9, 1.0),
            border: Border::NONE,
        }),
        // Capsule — not on the checklist, but it has its own route too.
        Primitive::AnalyticCapsule(AnalyticCapsule {
            rect: rect(184.0, 8.0, 48.0, 16.0),
            color: Rgba::new(0.9, 0.7, 0.2, 1.0),
            border: Border::NONE,
        }),
        // Line.
        Primitive::AnalyticLine(AnalyticLine {
            p0: pt(8.0, 48.0),
            p1: pt(96.0, 72.0),
            width: 3.0,
            color: Rgba::new(0.1, 0.1, 0.1, 1.0),
            cap: LineCap::Round,
            join: LineJoin::Round,
            miter_limit: 4.0,
            border: Border::NONE,
        }),
        // Arc: lowered to cubics, stroked.
        Primitive::Path(Path {
            cmds: arc.to_cmds(),
            fill: None,
            stroke: Some(Stroke::new(2.0, Rgba::new(0.5, 0.2, 0.7, 1.0))),
            shadow: None,
        }),
        // A general path: curved and filled.
        Primitive::Path(Path {
            cmds: vec![
                PathCmd::MoveTo(pt(24.0, 120.0)),
                PathCmd::CubicTo(pt(48.0, 88.0), pt(80.0, 152.0), pt(104.0, 120.0)),
                PathCmd::LineTo(pt(104.0, 152.0)),
                PathCmd::LineTo(pt(24.0, 152.0)),
                PathCmd::Close,
            ],
            fill: Some(Rgba::new(0.2, 0.4, 0.8, 1.0)),
            stroke: None,
            shadow: None,
        }),
        // Image.
        Primitive::Image(ImageDraw {
            rect: rect(128.0, 96.0, 48.0, 48.0),
            uv: rect(0.0, 0.0, 1.0, 1.0),
            tint: Rgba::new(1.0, 1.0, 1.0, 1.0),
            texture: image,
            sampler: SamplerDesc::LINEAR_CLAMP,
        }),
        // Text.
        Primitive::GlyphRun(GlyphRunDraw {
            glyphs: (0..6)
                .map(|i| GlyphInstanceData {
                    rect: rect(16.0 + i as f32 * 12.0, 180.0, 10.0, 14.0),
                    uv: rect(0.0, 0.0, 0.1, 0.1),
                })
                .collect(),
            atlas,
            color: Rgba::new(0.05, 0.05, 0.05, 1.0),
            lane: GlyphLane::CoverageA8,
        }),
        // Mesh.
        Primitive::Mesh(Mesh {
            vertices: vec![
                MeshVertex {
                    pos: [160.0, 180.0],
                    color: [1.0, 0.0, 0.0, 1.0],
                    edge: 1.0,
                },
                MeshVertex {
                    pos: [220.0, 180.0],
                    color: [0.0, 1.0, 0.0, 1.0],
                    edge: 1.0,
                },
                MeshVertex {
                    pos: [190.0, 232.0],
                    color: [0.0, 0.0, 1.0, 1.0],
                    edge: 1.0,
                },
            ],
            indices: vec![0, 1, 2],
        }),
    ];

    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([W as f32, H as f32]);
    let s = upload(&mut r, &mut gpu, &scene);

    assert_eq!(
        s.visible_primitives, 11,
        "every shape reached the renderer as itself"
    );
    assert!(
        s.instances > 0 && s.draw_calls > 0,
        "and the frame is drawable"
    );
    // Only the two `Path` entries are geometry at all, and they take the two
    // routes a path can take: the stroked arc is tessellated, while the concave
    // curved fill is cheaper as its own cached coverage mask (§14.4) — a masked
    // draw, not a tessellation. Eight of the eleven shapes reach neither route.
    assert_eq!(
        s.path_tessellations, 1,
        "only the stroked path needed the tessellator"
    );
    assert!(
        s.clip_mask_builds > 0,
        "and the concave fill took the coverage-mask lane"
    );
}

// ===========================================================================
// 2. Stroke style is explicit in every dimension
// ===========================================================================

/// Cap, join, miter limit, dash and alignment are all authored fields, and each
/// is honored rather than accepted and dropped.
///
/// Honoring is checked the only way it can be: two strokes differing in exactly
/// one field must not rasterize identically. A field that was parsed and ignored
/// would make these pairs equal, which is precisely the failure "has explicit
/// semantics" is meant to exclude.
#[test]
fn stroke_style_is_honored_in_every_dimension() {
    /// The same stroke over a closed outline (corners, and an inside) or an open
    /// polyline (two free ends). Which one a field can be seen in is part of its
    /// semantics: a cap exists only where a contour stops.
    fn stroked(closed: bool, mutate: impl FnOnce(&mut Stroke)) -> Vec<u8> {
        let (mut gpu, surface, mut r) = renderer(W, H);
        let mut stroke = Stroke::new(6.0, Rgba::new(0.0, 0.0, 0.0, 1.0));
        mutate(&mut stroke);
        let mut cmds = vec![
            PathCmd::MoveTo(pt(64.0, 64.0)),
            PathCmd::LineTo(pt(192.0, 96.0)),
            PathCmd::LineTo(pt(96.0, 192.0)),
        ];
        if closed {
            cmds.push(PathCmd::Close);
        }
        let path = Primitive::Path(Path {
            cmds,
            fill: None,
            stroke: Some(stroke),
            shadow: None,
        });
        present(&mut r, &mut gpu, surface, [1.0, 1.0, 1.0, 1.0], &[path]);
        gpu.read_pixels_bgra8(surface)
    }

    let closed = stroked(true, |_| {});
    for (field, mutated) in [
        ("join", stroked(true, |s| s.join = LineJoin::Round)),
        ("miter limit", stroked(true, |s| s.miter_limit = 1.0)),
        ("alignment", stroked(true, |s| s.align = StrokeAlign::Inner)),
        (
            "dash",
            stroked(true, |s| s.dash = Some(DashPattern::new(&[8.0, 8.0], 0.0))),
        ),
        ("hairline", stroked(true, |s| s.hairline = true)),
    ] {
        assert_ne!(
            closed, mutated,
            "{field} was accepted and then ignored: the raster is unchanged"
        );
    }

    // A cap is only visible where a contour has an end, so it is asked of the
    // open polyline — and a closed contour has none, which is why the loop above
    // cannot state this one.
    let open = stroked(false, |_| {});
    for cap in [LineCap::Square, LineCap::Round] {
        assert_ne!(
            open,
            stroked(false, |s| s.cap = cap),
            "{cap:?} was accepted and then ignored"
        );
    }
    assert_eq!(
        closed,
        stroked(true, |s| s.cap = LineCap::Square),
        "a closed contour has no ends, so a cap has nothing to change"
    );

    // Symmetrically, an open contour has no inside, so alignment is centered
    // whatever is asked — a documented semantic, not an omission.
    assert_eq!(
        open,
        stroked(false, |s| s.align = StrokeAlign::Outer),
        "an open contour has no inside, so alignment cannot move the stroke"
    );
}

// ===========================================================================
// 3. Simple UI shapes are analytic, not tessellated
// ===========================================================================

/// A screenful of ordinary UI — rounded panels, dots, dividers — costs **zero**
/// tessellations, zero coverage masks, and a handful of draws. This is the most
/// load-bearing item on the checklist: if simple shapes went through a
/// tessellator, every other cost on the list would be measured on top of a base
/// the framework did not need to pay.
///
/// The shapes are grouped by kind, which is what makes the draw-call bound a
/// statement about instancing rather than about reordering. Batching may not
/// reorder across an overlap it cannot prove absent (§16.2), so an interleaved
/// scene legitimately switches pipelines; what must hold either way is that 40
/// rounded rects are one draw, not 40.
#[test]
fn a_screen_of_simple_shapes_never_tessellates() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let cell = |i: usize| (6.0 + (i % 5) as f32 * 50.0, 6.0 + (i / 5) as f32 * 31.0);
    let mut scene = Vec::new();
    for i in 0..40 {
        let (x, y) = cell(i);
        scene.push(Primitive::AnalyticRRect(AnalyticRRect {
            rect: rect(x, y, 44.0, 20.0),
            color: Rgba::new(0.3, 0.5, 0.8, 1.0),
            radius: Corners::uniform(5.0),
            border: Border::NONE,
        }));
    }
    for i in 0..40 {
        let (x, y) = cell(i);
        scene.push(Primitive::AnalyticEllipse(AnalyticEllipse {
            rect: rect(x + 2.0, y + 22.0, 6.0, 6.0),
            color: Rgba::new(0.9, 0.4, 0.2, 1.0),
            border: Border::NONE,
        }));
    }
    for i in 0..40 {
        let (x, y) = cell(i);
        scene.push(Primitive::AnalyticLine(AnalyticLine {
            p0: pt(x + 12.0, y + 25.0),
            p1: pt(x + 42.0, y + 25.0),
            width: 1.0,
            color: Rgba::new(0.7, 0.7, 0.7, 1.0),
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 4.0,
            border: Border::NONE,
        }));
    }

    let s = upload(&mut r, &mut gpu, &scene);
    assert_eq!(s.visible_primitives, 120);
    assert_eq!(
        s.path_tessellations, 0,
        "an analytic shape must never reach the tessellator"
    );
    assert!(
        s.draw_calls <= 8,
        "120 shapes instanced into {} draws is not instancing",
        s.draw_calls
    );
    assert_eq!(s.clip_mask_builds, 0, "and nothing needed a coverage mask");
}

/// A general path drawn twice unchanged tessellates once: the second frame reuses
/// the retained geometry. "Stable path can be retained cached geometry" is a
/// per-frame-cost claim, so it is read off the second frame's counters.
#[test]
fn a_stable_path_is_tessellated_once_not_once_per_frame() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let scene = vec![Primitive::Path(Path {
        cmds: vec![
            PathCmd::MoveTo(pt(32.0, 32.0)),
            PathCmd::CubicTo(pt(96.0, 8.0), pt(160.0, 120.0), pt(224.0, 64.0)),
            PathCmd::LineTo(pt(224.0, 200.0)),
            PathCmd::LineTo(pt(32.0, 200.0)),
            PathCmd::Close,
        ],
        fill: Some(Rgba::new(0.4, 0.2, 0.6, 1.0)),
        stroke: Some(Stroke::new(2.0, Rgba::new(0.1, 0.1, 0.1, 1.0))),
        shadow: None,
    })];

    let cold = upload(&mut r, &mut gpu, &scene);
    assert!(
        cold.path_tessellations > 0,
        "the cold frame builds geometry"
    );

    upload(&mut r, &mut gpu, &scene);
    let steady = upload(&mut r, &mut gpu, &scene);
    assert_eq!(
        steady.path_tessellations, 0,
        "an unchanged path must not be re-tessellated"
    );
    assert_eq!(steady.dirty_primitives, 0);
    assert_eq!(steady.visible_primitives, 1, "and it is still drawn");
}

/// The compute lane is entered on measured benefit over a large dynamic workload,
/// never by default — and an ordinary frame is not that workload.
#[test]
fn the_compute_vector_lane_is_not_the_default() {
    assert_eq!(
        VectorLane::select(VectorWorkload::default()),
        VectorLane::CpuTessellate
    );
    let (mut gpu, surface, mut r) = renderer(W, H);
    let scene: Vec<Primitive> = (0..30)
        .map(|i| {
            quad(rect(
                4.0 + (i % 6) as f32 * 41.0,
                4.0 + (i / 6) as f32 * 50.0,
                36.0,
                44.0,
            ))
        })
        .collect();
    let s = present(&mut r, &mut gpu, surface, [0.0; 4], &scene);
    assert_eq!(s.compute_dispatches, 0);
    assert_eq!(s.indirect_draws, 0);
}

// ===========================================================================
// 4. Clipping is paid for by tier
// ===========================================================================

/// A rect clip is a scissor, which is free: no mask, the cheapest tier, and the
/// clip's own bounds.
#[test]
fn a_rect_clip_is_a_scissor() {
    let plan = plan_clip(ClipShape::Rect(rect(4.0, 4.0, 80.0, 40.0)), true);
    assert_eq!(plan.tier, ClipTier::Scissor);
    assert!(!plan.tier.builds_mask());
    assert_eq!(plan.bounds, rect(4.0, 4.0, 80.0, 40.0));

    // And a "rounded" rect whose radii normalize away is the same free tier —
    // the shape asked for, not the type it was authored as, decides the cost.
    let sharp = plan_clip(
        ClipShape::RoundRect {
            rect: rect(0.0, 0.0, 80.0, 40.0),
            radii: Corners::SHARP,
        },
        true,
    );
    assert_eq!(sharp.tier, ClipTier::Scissor);
}

/// A border radius does not clip children. An ordinary rounded container is
/// overflow-visible: it needs no clip, so it never forces a mask or an offscreen
/// layer for being rounded. Only a scroll viewport clips, and with a scissor.
#[test]
fn a_border_radius_does_not_clip_children() {
    assert_eq!(
        clips_children(true, false),
        None,
        "a rounded container must not clip its children"
    );
    assert_eq!(clips_children(false, false), None);
    assert_eq!(clips_children(true, true), Some(ClipTier::Scissor));
    assert_eq!(clips_children(false, true), Some(ClipTier::Scissor));

    // End to end: a rounded panel with a child hanging past its corner pays for
    // no mask and no offscreen target.
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let s = upload(
        &mut r,
        &mut gpu,
        &[
            Primitive::AnalyticRRect(AnalyticRRect {
                rect: rect(40.0, 40.0, 120.0, 80.0),
                color: Rgba::new(0.9, 0.9, 0.9, 1.0),
                radius: Corners::uniform(24.0),
                border: Border::NONE,
            }),
            quad(rect(30.0, 100.0, 60.0, 60.0)),
        ],
    );
    assert_eq!(s.clip_mask_builds, 0);
    assert_eq!(s.offscreen_passes, 0);
    assert_eq!(s.transient_target_bytes, 0);
}

/// A complex clip needs a coverage mask, and a *stable* one earns the retained
/// realization so it is built once rather than every frame. Stability is the only
/// difference between the two tiers here.
#[test]
fn a_stable_complex_clip_enters_the_mask_cache() {
    let shape = ClipShape::Path {
        bounds: rect(10.0, 10.0, 90.0, 70.0),
    };
    assert_eq!(plan_clip(shape, false).tier, ClipTier::Mask);
    assert_eq!(plan_clip(shape, true).tier, ClipTier::CachedMask);
    assert!(ClipTier::Mask.builds_mask() && ClipTier::CachedMask.builds_mask());
}

// ===========================================================================
// 5. Effects cost what they are, not what a generic path would cost
// ===========================================================================

/// A drop shadow is a closed-form coverage ramp in the fragment, so a shadowed
/// button allocates nothing: no blur pass, no offscreen target, no transient
/// bytes. The forbidden default — render the shape into a layer and blur it — is
/// what the whole analytic-shadow primitive exists to avoid.
#[test]
fn a_simple_shadow_creates_no_blur_layer() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let mut scene = Vec::new();
    for i in 0..12 {
        let x = 10.0 + (i % 3) as f32 * 80.0;
        let y = 10.0 + (i / 3) as f32 * 60.0;
        scene.push(Primitive::AnalyticShadow(AnalyticShadow {
            rect: rect(x, y, 64.0, 32.0),
            color: Rgba::new(0.0, 0.0, 0.0, 0.35),
            radius: Corners::uniform(8.0),
            offset: [0.0, 3.0],
            sigma: 6.0,
            spread: 0.0,
            shape: ShadowShape::RoundedBox,
            inner: false,
        }));
        scene.push(Primitive::AnalyticRRect(AnalyticRRect {
            rect: rect(x, y, 64.0, 32.0),
            color: Rgba::new(0.95, 0.95, 0.97, 1.0),
            radius: Corners::uniform(8.0),
            border: Border::NONE,
        }));
    }

    let s = upload(&mut r, &mut gpu, &scene);
    assert_eq!(
        s.blur_passes, 0,
        "a simple shadow must not run a blur ladder"
    );
    assert_eq!(s.offscreen_passes, 0, "nor open a layer");
    assert_eq!(s.transient_target_bytes, 0, "nor allocate a target");
    assert_eq!(s.blur_target_bytes, 0);
    assert!(s.draw_calls > 0 && s.instances >= 24);
}

/// A backdrop blur captures its own padded region and nothing more. On a 256²
/// surface a small panel's capture is a small fraction of the frame; capturing
/// the surface would be the forbidden default and is orders of magnitude more
/// pixels.
#[test]
fn a_backdrop_captures_a_tight_roi() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let panel = rect(80.0, 80.0, 48.0, 32.0);
    let s = upload(
        &mut r,
        &mut gpu,
        &[
            quad(rect(0.0, 0.0, W as f32, H as f32)),
            Primitive::Layer(LayerClip {
                clip: panel,
                opacity: 1.0,
                blur_sigma: 0.0,
                backdrop_sigma: 4.0,
            }),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(s.backdrop_captures, 1);
    assert!(s.blur_passes > 0, "the backdrop really was blurred");
    assert!(
        s.backdrop_capture_pixels * 8 < (W * H) as usize,
        "the capture is not tight: {} of {} pixels",
        s.backdrop_capture_pixels,
        W * H
    );
}

/// Two panels over the same background at the same sigma share one capture and
/// one blur ladder, and get one composite each. N panels are not N captures.
#[test]
fn panels_over_one_background_share_a_capture() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let s = upload(
        &mut r,
        &mut gpu,
        &[
            quad(rect(0.0, 0.0, W as f32, H as f32)),
            Primitive::Layer(LayerClip {
                clip: rect(20.0, 40.0, 60.0, 40.0),
                opacity: 1.0,
                blur_sigma: 0.0,
                backdrop_sigma: 6.0,
            }),
            Primitive::LayerEnd,
            Primitive::Layer(LayerClip {
                clip: rect(100.0, 40.0, 60.0, 40.0),
                opacity: 1.0,
                blur_sigma: 0.0,
                backdrop_sigma: 6.0,
            }),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(
        s.backdrop_captures, 1,
        "two panels, one shared capture — not one each"
    );
}

/// Three local color effects are one per-pixel computation, carried on the
/// composite the layer was already going to draw: no extra render-target pass.
#[test]
fn local_color_effects_fuse_into_one_op() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let clip = rect(32.0, 32.0, 96.0, 64.0);
    let s = upload(
        &mut r,
        &mut gpu,
        &[
            Primitive::Layer(LayerClip {
                clip,
                opacity: 1.0,
                blur_sigma: 0.0,
                backdrop_sigma: 0.0,
            }),
            Primitive::ColorEffect(ColorEffect::Brightness(1.2)),
            Primitive::ColorEffect(ColorEffect::Contrast(0.85)),
            Primitive::ColorEffect(ColorEffect::Saturation(1.4)),
            quad(clip),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(s.color_effect_ops, 1, "three effects, one fused op");
    assert_eq!(
        s.color_transform_passes, 0,
        "and no pass of its own: the composite carries the matrix"
    );
}

// ===========================================================================
// 6. An offscreen layer is always explained, and only taken when needed
// ===========================================================================

/// Every layer the renderer plans carries the reasons that asked for it, so an
/// offscreen target is never anonymous. The planner's own answer is checked
/// alongside the frame's, because "has an explicit reason" has to be true of the
/// decision, not only of a log line.
#[test]
fn an_offscreen_layer_names_its_reason() {
    assert_eq!(
        plan_group_opacity(0.5, ChildOverlap::Overlapping).layer_reason(),
        Some(LayerReason::GroupOpacity)
    );

    let (mut gpu, _surface, mut r) = renderer(W, H);
    let clip = rect(20.0, 20.0, 120.0, 90.0);
    let s = upload(
        &mut r,
        &mut gpu,
        &[
            Primitive::Layer(LayerClip {
                clip,
                opacity: 0.5,
                blur_sigma: 0.0,
                backdrop_sigma: 0.0,
            }),
            // Overlapping children, so the opacity cannot be folded away.
            quad(rect(30.0, 30.0, 70.0, 60.0)),
            quad(rect(60.0, 50.0, 70.0, 50.0)),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(s.offscreen_passes, 1, "this layer is genuinely required");
    let plans = r.layer_plans();
    assert_eq!(plans.len(), 1);
    assert_eq!(
        plans[0].requested,
        ReasonSet::of(LayerReason::GroupOpacity),
        "the target exists for a named reason"
    );
    assert!(
        !plans[0].surviving.is_empty(),
        "and that reason survived the planner"
    );
}

/// Group opacity isolates only when it must. Fully opaque: nothing to do.
/// Translucent over disjoint children: the factor multiplies into the children
/// and the frame allocates nothing. Translucent over overlapping children: the
/// layer is real, because folding would double-darken the overlap.
#[test]
fn group_opacity_isolates_only_when_required() {
    assert_eq!(
        plan_group_opacity(1.0, ChildOverlap::Overlapping),
        OpacityPlan::Opaque
    );
    assert_eq!(
        plan_group_opacity(0.5, ChildOverlap::Disjoint),
        OpacityPlan::FoldIntoChildren { factor: 0.5 }
    );
    assert_eq!(
        plan_group_opacity(0.5, ChildOverlap::Overlapping),
        OpacityPlan::IsolateLayer {
            opacity: 0.5,
            reason: LayerReason::GroupOpacity,
        }
    );
    assert!(
        matches!(
            plan_group_opacity(0.5, ChildOverlap::Unknown),
            OpacityPlan::IsolateLayer { .. }
        ),
        "not knowing is not permission to fold"
    );
    assert!(!plan_group_opacity(0.5, ChildOverlap::Disjoint).needs_offscreen());
    assert!(plan_group_opacity(0.5, ChildOverlap::Overlapping).needs_offscreen());

    let (mut gpu, _surface, mut r) = renderer(W, H);
    let s = upload(
        &mut r,
        &mut gpu,
        &[
            Primitive::Layer(LayerClip {
                clip: rect(10.0, 10.0, 200.0, 80.0),
                opacity: 0.5,
                blur_sigma: 0.0,
                backdrop_sigma: 0.0,
            }),
            quad(rect(20.0, 20.0, 60.0, 60.0)),
            quad(rect(130.0, 20.0, 60.0, 60.0)),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(s.offscreen_passes, 0, "disjoint children need no target");
    assert_eq!(s.transient_target_bytes, 0);
    assert_eq!(s.opacity_folds, 1);
    assert_eq!(
        s.layers_eliminated, 1,
        "the layer was considered and dropped"
    );
}

// ===========================================================================
// 7. Color: premultiplied everywhere, and never narrowed
// ===========================================================================

/// The canonical alpha in GPU memory is premultiplied. A half-transparent white
/// quad over transparent black therefore *stores* ~0.5 in each color channel,
/// which is what makes source-over a single multiply-add and what any later
/// sample of that texel reads.
///
/// Authoring stays straight — `Rgba::new(1, 1, 1, 0.5)` is white at half alpha —
/// and the 8-bit capture path un-premultiplies on the way out, so a golden image
/// shows white too. Both are asserted here because the pair is the actual
/// contract: premultiplied where it is composited, straight where it is written
/// and where it is read back for a human.
#[test]
fn gpu_alpha_is_premultiplied() {
    let a = 0.5_f32;
    let translucent_white = Primitive::Quad(Quad {
        rect: rect(0.0, 0.0, W as f32, H as f32),
        color: Rgba::new(1.0, 1.0, 1.0, a),
        radius: 0.0,
        border: Border::NONE,
    });
    let center = ((H / 2) * W + W / 2) as usize;

    let (mut gpu, surface, mut r) = renderer(W, H);
    present(
        &mut r,
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 0.0],
        std::slice::from_ref(&translucent_white),
    );

    // What is in memory.
    let px = gpu.read_pixels_rgba16f(surface);
    let chan = |c: usize| {
        f16_to_f32(u16::from_le_bytes([
            px[center * 8 + c * 2],
            px[center * 8 + c * 2 + 1],
        ]))
    };
    for (label, c) in [("r", 0), ("g", 1), ("b", 2)] {
        assert!(
            (chan(c) - a).abs() < 0.01,
            "channel {label} stores {}, premultiplied is {a} \
             (straight alpha would store 1.0)",
            chan(c)
        );
    }
    assert!((chan(3) - a).abs() < 0.01, "and alpha is untouched");

    // What a capture shows: the same texel, un-premultiplied back to white.
    let bgra = gpu.read_pixels_bgra8(surface);
    let i = center * 4;
    for (label, got) in [("b", bgra[i]), ("g", bgra[i + 1]), ("r", bgra[i + 2])] {
        assert_eq!(
            got, 255,
            "the capture path must undo the premultiply: {label} came back {got}"
        );
    }
    assert!((bgra[i + 3] as i32 - (a * 255.0).round() as i32).abs() <= 2);
}

/// An extended-range surface carries an out-of-range highlight through the whole
/// chain. The same value on an SDR surface clips — which is correct, and is why
/// the rule is "not *wrongly* degraded": the narrowing must happen at the display,
/// not at some intermediate nobody can see.
#[test]
fn an_hdr_highlight_is_not_narrowed_mid_pipeline() {
    fn center_red(format: TextureFormat, space: ColorSpace) -> f32 {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        gpu.set_surface_color_target(surface, format, space);
        let mut r = Renderer::for_surface(&mut gpu, surface);
        r.set_surface_size([W as f32, H as f32]);
        // Through a translucent blurred layer, so the value crosses every kind
        // of intermediate the frame has.
        let scene = vec![
            Primitive::Layer(LayerClip {
                clip: rect(0.0, 0.0, W as f32, H as f32),
                opacity: 1.0,
                blur_sigma: 2.0,
                backdrop_sigma: 0.0,
            }),
            Primitive::Quad(Quad {
                rect: rect(0.0, 0.0, W as f32, H as f32),
                color: Rgba::new(4.0, 1.0, 1.0, 1.0),
                radius: 0.0,
                border: Border::NONE,
            }),
            Primitive::LayerEnd,
        ];
        r.upload(&mut gpu, &scene);
        r.submit(&mut gpu, surface, [0.0; 4], [W as f32, H as f32]);
        let px = gpu.read_pixels_rgba16f(surface);
        let i = ((H / 2) * W + W / 2) as usize * 8;
        f16_to_f32(u16::from_le_bytes([px[i], px[i + 1]]))
    }

    let hdr = center_red(TextureFormat::Rgba16Float, ColorSpace::ExtendedLinearSrgb);
    assert!(
        hdr > 1.5,
        "an extended-range chain lost the highlight: red came back {hdr}"
    );

    let sdr = center_red(TextureFormat::Bgra8Unorm, ColorSpace::Srgb);
    assert!(
        sdr <= 1.01,
        "an SDR surface cannot show {sdr}: the clip belongs at the display"
    );
}
