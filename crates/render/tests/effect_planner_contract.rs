//! Effect Planner contract (§3102, §3120, §3145, §3202).
//!
//! One sentence governs this file: *offscreen is an expensive mechanism, not a
//! convenient default* (§3145). Four properties make that sentence enforceable
//! rather than aspirational:
//!
//! 1. **Local and nonlocal are one threshold, not two lists.** An effect that is
//!    a pure function of the fragment it shades fuses into the draw that was
//!    going to run anyway (§3102); only an effect needing neighbor samples, the
//!    previous framebuffer, or group isolation may even *consider* a target
//!    (§3120). The frontier is a single rung on the [`EffectCost`] ladder, so the
//!    two facts cannot drift apart.
//! 2. **A layer states its reason and the planner tries to take it away.** Every
//!    potential layer raises a [`LayerReason`]; the planner walks the elimination
//!    ladder and a target is created only when a reason survives all of it. The
//!    assertions here pin both halves: the reason is *raised* (not hidden), and
//!    where it can be eliminated the frame pays zero offscreen passes.
//! 3. **Eliminating a layer does not change the pixels.** The fold is the
//!    planner's most valuable rung and also its most dangerous: it is only legal
//!    when no two children overlap. A headless readback compares the folded
//!    group against the same coverage authored directly, and the overlapping /
//!    nested / non-foldable cases assert the planner refuses to fold.
//! 4. **A backdrop's damage is scoped to its ROI (§3202).** A frosted panel does
//!    not re-capture because *something* changed; it re-captures because
//!    something changed *behind it*. Two panels over separate content must be
//!    independently dirty.
//!
//! Everything goes through the public surface: `Primitive` in, `FrameStats` /
//! `Renderer::layer_plans()` / `Renderer::backdrop_dependencies()` out, plus one
//! pixel readback. The planner's thresholds are private consts, so the scenes are
//! chosen to be unambiguous under any sane value of them.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureFormat};
use viso_render::{
    Blend, Border, ColorEffect, EffectCost, EffectLocality, ExtendMode, FrameStats, Gradient,
    GradientKind, GradientStop, InterpolationSpace, LayerClip, LayerElimination, LayerReason,
    Point, Primitive, Quad, Rect, Renderer, Rgba,
};

const W: u32 = 256;
const H: u32 = 256;

fn renderer(w: u32, h: u32) -> (HeadlessRaster, SurfaceId, Renderer) {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);
    let format: TextureFormat = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([w as f32, h as f32]);
    (gpu, surface, r)
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn tinted(rect: Rect, color: Rgba) -> Primitive {
    Primitive::Quad(Quad {
        rect,
        color,
        radius: 0.0,
        border: Border::NONE,
    })
}

fn quad(rect: Rect) -> Primitive {
    tinted(rect, Rgba::new(0.8, 0.3, 0.2, 1.0))
}

/// A group layer: `opacity` is the group factor, no filters.
fn layer(clip: Rect, opacity: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    })
}

/// A group that blurs its own content — a nonlocal image filter.
fn blurred(clip: Rect, sigma: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma: sigma,
        backdrop_sigma: 0.0,
    })
}

/// A frosted panel: opaque, unblurred content, blurring what is behind it.
fn frosted(clip: Rect, sigma: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma: 0.0,
        backdrop_sigma: sigma,
    })
}

/// A two-stop linear gradient — a drawable the fold deliberately refuses,
/// because its instance colors are stored premultiplied.
fn gradient(r: Rect) -> Primitive {
    Primitive::Gradient(Gradient {
        rect: r,
        kind: GradientKind::Linear,
        extend: ExtendMode::Clamp,
        p0: Point::new(r.x, r.y),
        p1: Point::new(r.x + r.w, r.y),
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: Rgba::new(0.9, 0.1, 0.1, 1.0),
            },
            GradientStop {
                offset: 1.0,
                color: Rgba::new(0.1, 0.2, 0.9, 1.0),
            },
        ],
        interp: InterpolationSpace::LinearRgb,
    })
}

fn frame(gpu: &mut HeadlessRaster, r: &mut Renderer, primitives: &[Primitive]) -> FrameStats {
    r.upload(gpu, primitives);
    r.frame_stats()
}

/// Upload a scene and read back the composited surface as BGRA8, top-left
/// origin. The clear is fully transparent so the comparison sees only what the
/// scene drew.
fn pixels(primitives: &[Primitive]) -> Vec<u8> {
    let (mut gpu, surface, mut r) = renderer(W, H);
    r.upload(&mut gpu, primitives);
    r.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 0.0],
        [W as f32, H as f32],
    );
    gpu.read_pixels_bgra8(surface)
}

// ---------------------------------------------------------------------------
// 1. Local vs nonlocal is one threshold (§3102 / §3120)
// ---------------------------------------------------------------------------

/// The §3102 effects — opacity, tint, a color matrix, brightness/contrast/
/// saturation, a simple gradient, a simple mask, a blend the fixed-function
/// stage expresses — all land below the frontier and therefore fuse into the
/// draw shader; the §3120 effects all land at or above it. "May consider an
/// offscreen target" and "is nonlocal" are the same predicate, not two.
#[test]
fn locality_is_the_offscreen_frontier() {
    for local in [
        EffectCost::Local,
        EffectCost::Analytic,
        EffectCost::NeedsMask,
    ] {
        assert!(local.is_local(), "{local:?} shades in place (§3102)");
        assert!(
            !local.needs_offscreen(),
            "{local:?} must not buy a target (§3102)"
        );
    }
    for nonlocal in [
        EffectCost::NeedsOffscreen,
        EffectCost::NeedsBackdrop,
        EffectCost::DestinationRead,
        EffectCost::ComputePreferred,
    ] {
        assert_eq!(
            nonlocal.locality(),
            EffectLocality::Nonlocal,
            "{nonlocal:?}"
        );
    }
    // A chain is as nonlocal as its worst link, and no more.
    assert!(
        EffectCost::dominating([
            EffectCost::Local,
            EffectCost::Analytic,
            EffectCost::NeedsMask
        ])
        .is_local(),
        "a chain of local links fuses into one draw"
    );
    assert!(
        !EffectCost::dominating([EffectCost::Local, EffectCost::NeedsOffscreen]).is_local(),
        "one nonlocal link makes the chain nonlocal"
    );
}

/// A reason that *survives* planning is by definition an effect that could not
/// be shaded in place, so every [`LayerReason`] is nonlocal. This is what makes
/// "a surviving reason means a target" a theorem rather than a convention.
#[test]
fn every_surviving_reason_is_nonlocal() {
    for reason in LayerReason::ALL {
        assert!(
            !reason.cost().is_local(),
            "{reason:?} needs pixels it does not own"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Group opacity: the planner's cheapest rung
// ---------------------------------------------------------------------------

/// Two disjoint children under a translucent group: the group opacity is pushed
/// into the children and the frame pays *no* offscreen pass. The forbidden
/// default — saveLayer for every group opacity — would allocate a target, render
/// the subtree into it, and composite it back, for a result a multiply produces.
#[test]
fn a_translucent_group_over_disjoint_children_costs_no_target() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            layer(rect(10.0, 10.0, 140.0, 60.0), 0.5),
            quad(rect(20.0, 20.0, 40.0, 40.0)),
            quad(rect(90.0, 20.0, 40.0, 40.0)),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(stats.offscreen_passes, 0, "the fold allocates nothing");
    assert_eq!(stats.transient_target_bytes, 0);
    assert_eq!(stats.layers_planned, 1, "the layer was still considered");
    assert_eq!(stats.layers_eliminated, 1);
    assert_eq!(stats.opacity_folds, 1);

    let plan = r.layer_plans()[0];
    assert_eq!(
        plan.requested,
        viso_render::ReasonSet::of(LayerReason::GroupOpacity),
        "the reason is raised, not hidden"
    );
    assert!(plan.surviving.is_empty(), "and then taken away");
    assert!(
        plan.eliminated
            .contains(LayerElimination::OpacityPushedIntoChildren)
    );
    assert!(
        plan.eliminated
            .contains(LayerElimination::ScissorInsteadOfClipLayer),
        "with no reason left, the clip is just the pass scissor"
    );
    assert_eq!(plan.fold_opacity, 0.5);
    assert!(plan.cost().is_local(), "a folded group is a local effect");
}

/// The fold must be invisible. A group faded to 0.5 over non-overlapping
/// children is compared pixel-for-pixel against the same children authored with
/// the faded alpha directly — the definition of "pushing the opacity into the
/// children". If the planner ever folds something it should not, this is the
/// test that notices.
#[test]
fn the_fold_is_pixel_identical_to_the_faded_children() {
    let a = rect(20.0, 20.0, 40.0, 40.0);
    let b = rect(90.0, 20.0, 40.0, 40.0);
    let c = rect(20.0, 90.0, 40.0, 40.0);
    let opaque = Rgba::new(0.8, 0.3, 0.2, 1.0);
    let faded = Rgba::new(0.8, 0.3, 0.2, 0.5);

    let folded = pixels(&[
        layer(rect(0.0, 0.0, 200.0, 200.0), 0.5),
        tinted(a, opaque),
        tinted(b, opaque),
        tinted(c, opaque),
        Primitive::LayerEnd,
    ]);
    let direct = pixels(&[tinted(a, faded), tinted(b, faded), tinted(c, faded)]);

    assert_eq!(folded.len(), direct.len());
    assert_eq!(
        folded, direct,
        "the fold changed the image it was supposed to preserve"
    );
    // And the scene really did take the folded path.
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            layer(rect(0.0, 0.0, 200.0, 200.0), 0.5),
            tinted(a, opaque),
            tinted(b, opaque),
            tinted(c, opaque),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.offscreen_passes, 0);
    assert_eq!(stats.opacity_folds, 1);
}

/// Overlapping children are exactly where the fold becomes wrong: per-child
/// opacity blends the overlap twice, while the group means "composite once, then
/// fade". The planner must give up its cheapest rung here and isolate.
#[test]
fn overlapping_children_keep_the_layer() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            layer(rect(10.0, 10.0, 140.0, 140.0), 0.5),
            quad(rect(20.0, 20.0, 60.0, 60.0)),
            quad(rect(50.0, 50.0, 60.0, 60.0)),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(stats.offscreen_passes, 1, "correctness outranks the saving");
    assert_eq!(stats.opacity_folds, 0);
    assert_eq!(stats.layers_planned, 1);

    let plan = r.layer_plans()[0];
    assert!(plan.needs_offscreen());
    assert!(plan.surviving.contains(LayerReason::GroupOpacity));
    assert!(
        !plan
            .eliminated
            .contains(LayerElimination::OpacityPushedIntoChildren),
        "an elimination that does not eliminate is not recorded"
    );
    assert_eq!(plan.dominating_reason(), Some(LayerReason::GroupOpacity));
    assert_eq!(plan.cost(), EffectCost::NeedsOffscreen);
}

/// A nested group is where "do the children overlap?" stops being a cheap
/// question: the inner group's own compositing sits between the outer fade and
/// the leaves. Rather than answer it expensively or wrongly, the scan reports
/// unknown overlap and the planner keeps the layer (§14.5, "when unsure, keep
/// correctness").
#[test]
fn a_nested_group_keeps_the_outer_layer() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            layer(rect(10.0, 10.0, 140.0, 140.0), 0.5),
            quad(rect(20.0, 20.0, 30.0, 30.0)),
            layer(rect(70.0, 70.0, 60.0, 60.0), 0.5),
            quad(rect(80.0, 80.0, 30.0, 30.0)),
            Primitive::LayerEnd,
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(stats.layers_planned, 2, "both groups were considered");
    assert!(
        stats.offscreen_passes >= 1,
        "the outer group did not fold away"
    );
    assert!(
        r.layer_plans().iter().any(|p| p.needs_offscreen()),
        "at least one plan kept its target"
    );
}

/// The fold is a whitelist, not a fallback: it multiplies a factor into a
/// drawable's straight alpha, which is only meaningful for drawables that carry
/// one. A gradient's instance colors are stored premultiplied, so it is not on
/// the list and its group isolates even though a single child trivially cannot
/// overlap itself.
#[test]
fn a_non_foldable_child_keeps_the_layer() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            layer(rect(10.0, 10.0, 140.0, 60.0), 0.5),
            gradient(rect(20.0, 20.0, 100.0, 40.0)),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(
        stats.opacity_folds, 0,
        "no factor is pushed into a gradient"
    );
    assert_eq!(stats.offscreen_passes, 1);
    assert!(
        r.layer_plans()[0]
            .surviving
            .contains(LayerReason::GroupOpacity)
    );
}

/// A fully opaque group raises nothing at all: there is no reason to eliminate,
/// so the planner records a plan with an empty requested set and the frame does
/// not even consider a target.
#[test]
fn an_opaque_group_raises_no_reason() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            layer(rect(10.0, 10.0, 140.0, 60.0), 1.0),
            quad(rect(20.0, 20.0, 60.0, 60.0)),
            quad(rect(40.0, 30.0, 60.0, 60.0)),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(stats.offscreen_passes, 0);
    assert_eq!(stats.opacity_folds, 0, "nothing to fold");
    let plan = r.layer_plans()[0];
    assert!(plan.requested.is_empty());
    assert!(!plan.needs_offscreen());
}

// ---------------------------------------------------------------------------
// 3. The reasons the planner cannot take away
// ---------------------------------------------------------------------------

/// A content blur reads neighbors, which is the definition of nonlocal: no rung
/// on the ladder can express it in place, so `ImageFilter` survives and the plan
/// reports the target it actually bought.
#[test]
fn a_content_blur_survives_as_an_image_filter() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            blurred(rect(10.0, 10.0, 140.0, 140.0), 4.0),
            quad(rect(30.0, 30.0, 60.0, 60.0)),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(stats.offscreen_passes, 1);
    assert_eq!(stats.layers_planned, 1);
    let plan = r.layer_plans()[0];
    assert!(plan.surviving.contains(LayerReason::ImageFilter));
    assert_eq!(plan.dominating_reason(), Some(LayerReason::ImageFilter));
    assert_eq!(plan.locality(), EffectLocality::Nonlocal);
}

/// A non-separable blend reads the destination it composites over. That is a
/// strictly worse rung than a plain color pass, and the plan says so: the
/// dominating reason is `AdvancedBlend` at `DestinationRead`, above every other
/// surviving reason in the same group.
#[test]
fn an_advanced_blend_survives_as_a_destination_read() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let content = rect(10.0, 10.0, 140.0, 140.0);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            quad(rect(0.0, 0.0, 200.0, 200.0)),
            layer(content, 0.5),
            Primitive::Blend(Blend::Multiply),
            quad(rect(20.0, 20.0, 60.0, 60.0)),
            quad(rect(50.0, 50.0, 60.0, 60.0)),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(stats.blend_isolations, 1);
    let plan = r.layer_plans()[0];
    assert!(plan.surviving.contains(LayerReason::AdvancedBlend));
    assert_eq!(plan.dominating_reason(), Some(LayerReason::AdvancedBlend));
    assert_eq!(
        plan.cost(),
        EffectCost::DestinationRead,
        "the dominating cost is the worst surviving reason, not the first"
    );
}

/// Adjacent color effects are §3102 local: the whole chain collapses into one
/// fused op inside the draw shader. The planner records that as a pass-saving
/// elimination — it does not remove the reason the layer exists, it removes the
/// passes the chain would otherwise have cost.
#[test]
fn a_color_effect_chain_collapses_without_a_pass() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let clip = rect(10.0, 10.0, 140.0, 140.0);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            blurred(clip, 4.0),
            Primitive::ColorEffect(ColorEffect::Brightness(1.2)),
            Primitive::ColorEffect(ColorEffect::Contrast(0.8)),
            Primitive::ColorEffect(ColorEffect::Saturation(0.5)),
            quad(rect(30.0, 30.0, 60.0, 60.0)),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(stats.color_effect_ops, 1, "three effects, one fused op");
    assert_eq!(stats.color_transform_passes, 0, "and no pass of their own");
    let plan = r.layer_plans()[0];
    assert!(
        plan.eliminated
            .contains(LayerElimination::CollapsedAdjacentEffects)
    );
    assert!(
        plan.surviving.contains(LayerReason::ImageFilter),
        "the blur still needs the layer the chain rides on"
    );
}

/// A frosted panel raises `BackdropFilter` and then hands it back: the capture
/// group already re-rendered the pixels behind the panel into a texture, and a
/// texture is something the panel's own draw can sample in place. So the reason
/// is retired — not merely made cheaper — and the panel stays in the pass.
#[test]
fn a_shared_capture_retires_the_backdrop_reason() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            quad(rect(0.0, 0.0, 256.0, 256.0)),
            frosted(rect(40.0, 40.0, 80.0, 40.0), 6.0),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(stats.backdrop_captures, 1, "the capture is still paid for");
    assert_eq!(stats.layers_planned, 1);
    let plan = r.layer_plans()[0];
    assert!(
        plan.requested.contains(LayerReason::BackdropFilter),
        "the reason is raised"
    );
    assert!(
        plan.eliminated.contains(LayerElimination::SharedBackdrop),
        "and then retired by the capture"
    );
    assert!(plan.surviving.is_empty());
    assert!(!plan.needs_offscreen());
}

// ---------------------------------------------------------------------------
// 4. Effect damage is scoped to the ROI (§3202)
// ---------------------------------------------------------------------------

/// Two frosted panels, each over its own content, far enough apart that they
/// cannot share a capture. Changing the content behind one panel dirties exactly
/// that panel's ROI; the other's dependency revision does not move. The
/// forbidden behaviour is the easy one: any change anywhere re-captures and
/// re-blurs every backdrop in the frame.
#[test]
fn a_content_change_dirties_only_the_backdrop_above_it() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let under_a = rect(20.0, 20.0, 60.0, 60.0);
    let under_b = rect(170.0, 170.0, 60.0, 60.0);
    let panel_a = rect(30.0, 30.0, 40.0, 40.0);
    let panel_b = rect(180.0, 180.0, 40.0, 40.0);
    let scene = |color_a: Rgba| {
        vec![
            tinted(under_a, color_a),
            tinted(under_b, Rgba::new(0.2, 0.7, 0.4, 1.0)),
            frosted(panel_a, 5.0),
            Primitive::LayerEnd,
            frosted(panel_b, 5.0),
            Primitive::LayerEnd,
        ]
    };

    let first = frame(&mut gpu, &mut r, &scene(Rgba::new(0.8, 0.3, 0.2, 1.0)));
    assert_eq!(first.backdrop_captures, 2, "two panels, two captures");
    assert_eq!(
        first.backdrop_dirty_rois, 2,
        "a first frame has no previous revision to match"
    );
    let before: Vec<u64> = r
        .backdrop_dependencies()
        .iter()
        .map(|d| d.revision)
        .collect();
    assert_eq!(before.len(), 2);

    // Recolor only the content behind panel A.
    let stats = frame(&mut gpu, &mut r, &scene(Rgba::new(0.1, 0.4, 0.9, 1.0)));
    assert_eq!(stats.backdrop_captures, 2);
    assert_eq!(
        stats.backdrop_dirty_rois, 1,
        "one backdrop's input changed, so one ROI is dirty"
    );

    let deps = r.backdrop_dependencies();
    assert!(deps[0].dirty, "panel A samples the recolored quad");
    assert!(!deps[1].dirty, "panel B is unaffected");
    assert_ne!(deps[0].revision, before[0]);
    assert_eq!(
        deps[1].revision, before[1],
        "an untouched ROI does not move"
    );
    // Each dependency is scoped to its own ROI, which contains its panel and
    // nothing of the other one.
    let a_hit = deps[0].roi.intersect(panel_a);
    assert!(a_hit.w > 0.0 && a_hit.h > 0.0);
    let cross = deps[0].roi.intersect(panel_b);
    assert!(
        cross.w <= 0.0 || cross.h <= 0.0,
        "panel A's ROI must not reach panel B"
    );
}

/// A frame that changes nothing dirties nothing: every backdrop's dependency
/// revision is stable, so a static UI with frosted chrome does no repeat
/// capture work on the planner's account.
#[test]
fn a_static_frame_dirties_no_backdrop() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let scene = [
        quad(rect(0.0, 0.0, 256.0, 256.0)),
        frosted(rect(30.0, 30.0, 40.0, 40.0), 5.0),
        Primitive::LayerEnd,
        frosted(rect(180.0, 180.0, 40.0, 40.0), 5.0),
        Primitive::LayerEnd,
    ];

    let first = frame(&mut gpu, &mut r, &scene);
    assert_eq!(first.backdrop_dirty_rois, first.backdrop_captures);
    let before: Vec<u64> = r
        .backdrop_dependencies()
        .iter()
        .map(|d| d.revision)
        .collect();

    for _ in 0..3 {
        let next = frame(&mut gpu, &mut r, &scene);
        assert_eq!(next.backdrop_dirty_rois, 0, "an identical frame is clean");
        let after: Vec<u64> = r
            .backdrop_dependencies()
            .iter()
            .map(|d| d.revision)
            .collect();
        assert_eq!(after, before, "the revision is steady, not drifting");
        assert!(r.backdrop_dependencies().iter().all(|d| !d.dirty));
    }
}

/// A scene with no backdrop reports no dependencies at all — the §3202 bookkeeping
/// is not a per-frame cost every scene pays.
#[test]
fn no_backdrop_means_no_dependency() {
    let (mut gpu, _surface, mut r) = renderer(W, H);
    let stats = frame(
        &mut gpu,
        &mut r,
        &[
            blurred(rect(10.0, 10.0, 100.0, 100.0), 4.0),
            quad(rect(20.0, 20.0, 60.0, 60.0)),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.backdrop_captures, 0);
    assert_eq!(stats.backdrop_dirty_rois, 0);
    assert!(r.backdrop_dependencies().is_empty());
}
