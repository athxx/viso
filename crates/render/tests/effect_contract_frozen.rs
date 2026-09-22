//! Frozen contract for the destination-dependent effect lane (§17, §3102-§3202).
//!
//! Six contracts are pinned here, because M0's frosted/glass material, M1 and A0
//! compose *entirely* out of them and must never have to redefine one:
//!
//! 1. **Backdrop dependency + shared capture.** What joins one capture group and
//!    what opens a second, and the promise that a shared group's plan is the plan
//!    of a single sharer.
//! 2. **The backdrop ladder tier is not the capture.** Crossing a blur tier
//!    changes how many rungs read the capture, never how many captures exist.
//! 3. **The color-effect fusion rule.** A maximal run of affine stages is one op
//!    riding a draw that already happens; a non-affine stage splits the run and
//!    costs exactly one pass; order is preserved because matrix products do not
//!    commute.
//! 4. **Advanced-blend isolation.** A blend the fixed-function stage cannot
//!    express reads its destination through a *bounded* snapshot of the layer it
//!    already needed — never the surface, and never at all for `SrcOver`.
//! 5. **Local / Nonlocal + the elimination ladder order.** The frontier is one
//!    rung on the cost ladder; the eight reasons and six rungs are a closed
//!    vocabulary; reason-removing rungs run before pass-saving ones and the
//!    scissor rung is the outcome of all of them.
//! 6. **Effect Damage.** A capture's revision is a function of the *content*
//!    under its ROI, so a move and a recolor both dirty it, and one panel's
//!    damage never reaches another's.
//!
//! The behavioural coverage lives in `backdrop_contract.rs`,
//! `color_effect_contract.rs`, `blend_contract.rs` and
//! `effect_planner_contract.rs`; this file exists to state the contracts in one
//! place, through the public surface only, so that a downstream slice that breaks
//! one trips a test whose name says which promise it broke.

use std::mem::size_of;

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureFormat};
use viso_render::{
    Blend, Border, ChildOverlap, ColorEffect, EffectCost, EffectLocality, EliminationSet,
    FrameStats, LayerClip, LayerElimination, LayerPlan, LayerReason, LayerRequest, Primitive, Quad,
    ReasonSet, Rect, Renderer, Rgba, plan_layer,
};

const W: u32 = 192;
const H: u32 = 128;

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
    tinted(rect, Rgba::new(0.75, 0.4, 0.25, 1.0))
}

/// A quad strictly inside `r`, so a group holding both has provably overlapping
/// children and cannot have its opacity folded away — the fixtures below pin what
/// a *layer* costs, so the planner must not be able to sidestep them by removing
/// the layer (§3145).
fn contained(r: Rect) -> Primitive {
    quad(rect(
        r.x + r.w * 0.25,
        r.y + r.h * 0.25,
        (r.w * 0.5).max(1.0),
        (r.h * 0.5).max(1.0),
    ))
}

/// A group that blurs what is behind it and nothing else.
fn frosted(clip: Rect, sigma: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma: 0.0,
        backdrop_sigma: sigma,
    })
}

/// An opaque, unfiltered group — a clip that only becomes a layer if something
/// inside it forces one.
fn plain(clip: Rect) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    })
}

fn frame(gpu: &mut HeadlessRaster, r: &mut Renderer, primitives: &[Primitive]) -> FrameStats {
    r.upload(gpu, primitives);
    r.frame_stats()
}

/// A full-surface opaque background, so every frosted panel has something to
/// sample.
fn background() -> Primitive {
    tinted(
        rect(0.0, 0.0, W as f32, H as f32),
        Rgba::new(0.2, 0.35, 0.5, 1.0),
    )
}

/// One frosted panel with its own opaque tile of content.
fn panel(clip: Rect, sigma: f32) -> Vec<Primitive> {
    vec![
        frosted(clip, sigma),
        tinted(
            rect(clip.x + 2.0, clip.y + 2.0, clip.w - 4.0, clip.h - 4.0),
            Rgba::new(0.95, 0.95, 0.95, 1.0),
        ),
        Primitive::LayerEnd,
    ]
}

// ---------------------------------------------------------------------------
// 1. Backdrop dependency + shared capture
// ---------------------------------------------------------------------------

/// Two panels blurring at the same sigma, close enough that one padded ROI covers
/// both, share **one** capture and **one** ladder: the pass plan of a single
/// panel. Sharing adds composite draws, never passes (§17.1/§17.2).
#[test]
fn panels_at_one_sigma_share_one_capture_and_one_ladder() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let sigma = 2.0;

    let mut one = vec![background()];
    one.extend(panel(rect(10.0, 40.0, 16.0, 16.0), sigma));
    let single = frame(&mut gpu, &mut r, &one);

    let mut two = vec![background()];
    two.extend(panel(rect(10.0, 40.0, 16.0, 16.0), sigma));
    two.extend(panel(rect(36.0, 40.0, 16.0, 16.0), sigma));
    let shared = frame(&mut gpu, &mut r, &two);

    assert_eq!(single.backdrop_captures, 1, "one panel is one capture");
    assert_eq!(
        shared.backdrop_captures, 1,
        "two panels at one sigma join one capture group (§17.2)"
    );
    assert_eq!(
        shared.blur_passes, single.blur_passes,
        "a shared capture is blurred once, not once per sharer"
    );
    assert_eq!(
        shared.render_passes, single.render_passes,
        "sharing adds composites, never passes (§17.1)"
    );
    assert!(
        shared.draw_calls > single.draw_calls,
        "the second panel must still draw its own composite"
    );
    assert_eq!(
        r.backdrop_dependencies().len(),
        1,
        "one capture group is one dependency"
    );
}

/// Different sigmas are different ladders, so they can never share a capture
/// however close the panels are. The capture is the *input* to one blur, not a
/// general-purpose copy of the framebuffer.
#[test]
fn a_different_sigma_opens_its_own_capture() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let mut scene = vec![background()];
    scene.extend(panel(rect(10.0, 40.0, 16.0, 16.0), 2.0));
    scene.extend(panel(rect(36.0, 40.0, 16.0, 16.0), 8.0));
    let stats = frame(&mut gpu, &mut r, &scene);

    assert_eq!(
        stats.backdrop_captures, 2,
        "two sigmas are two ladders and therefore two captures"
    );
    assert_eq!(
        r.backdrop_dependencies().len(),
        2,
        "each capture group carries its own dependency"
    );
}

/// A panel far from the group does not get folded into it: the union would cost
/// more than the two tight captures it replaced, which is the whole point of
/// sharing. The slack bound is an internal const, so this pins the *behaviour* —
/// opposite corners of the surface never join.
#[test]
fn a_distant_panel_opens_its_own_capture() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let mut scene = vec![background()];
    scene.extend(panel(rect(6.0, 6.0, 16.0, 16.0), 2.0));
    scene.extend(panel(
        rect(W as f32 - 22.0, H as f32 - 22.0, 16.0, 16.0),
        2.0,
    ));
    let stats = frame(&mut gpu, &mut r, &scene);

    assert_eq!(
        stats.backdrop_captures, 2,
        "a union spanning the surface is not a shared capture, it is a full-screen \
         one — two tight captures are cheaper"
    );
}

/// A capture is sized by the ROI it will be sampled through, never by the
/// surface: strictly bigger than the panel (the blur reaches outside it) and
/// strictly smaller than the window.
#[test]
fn a_capture_is_bounded_by_its_roi_not_the_surface() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let clip = rect(60.0, 50.0, 16.0, 16.0);
    let mut scene = vec![background()];
    scene.extend(panel(clip, 2.0));
    let stats = frame(&mut gpu, &mut r, &scene);

    let panel_px = (clip.w * clip.h) as usize;
    let surface_px = (W as usize) * (H as usize);
    assert!(
        stats.backdrop_capture_pixels > panel_px,
        "the capture must include the blur's reach outside the panel ({} vs {} px)",
        stats.backdrop_capture_pixels,
        panel_px
    );
    assert!(
        stats.backdrop_capture_pixels < surface_px,
        "a panel must never promote itself to a full-surface capture ({} vs {} px)",
        stats.backdrop_capture_pixels,
        surface_px
    );
}

// ---------------------------------------------------------------------------
// 2. The ladder tier is not the capture
// ---------------------------------------------------------------------------

/// Crossing a blur tier changes how many rungs read the capture and nothing else:
/// still one capture, still one dependency. The tier belongs to the ladder (E1),
/// the capture to the group (E2.1), and this pins that they stay separable.
#[test]
fn a_backdrop_tier_change_adds_rungs_not_captures() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let clip = rect(60.0, 50.0, 16.0, 16.0);

    let mut small = vec![background()];
    small.extend(panel(clip, 2.0));
    let small_stats = frame(&mut gpu, &mut r, &small);

    let mut large = vec![background()];
    large.extend(panel(clip, 40.0));
    let large_stats = frame(&mut gpu, &mut r, &large);

    assert_eq!(small_stats.backdrop_captures, 1);
    assert_eq!(
        large_stats.backdrop_captures, 1,
        "a bigger sigma is a longer ladder, not a second capture"
    );
    assert!(
        large_stats.blur_passes > small_stats.blur_passes,
        "the large tier must actually plan more rungs ({} vs {})",
        large_stats.blur_passes,
        small_stats.blur_passes
    );
    assert_eq!(
        r.backdrop_dependencies().len(),
        1,
        "the dependency belongs to the capture, not to the ladder"
    );
}

// ---------------------------------------------------------------------------
// 3. The color-effect fusion rule
// ---------------------------------------------------------------------------

/// A card whose group carries `effects`, drawn through a layer.
fn graded(clip: Rect, effects: &[ColorEffect]) -> Vec<Primitive> {
    let mut scene = vec![plain(clip)];
    for &e in effects {
        scene.push(Primitive::ColorEffect(e));
    }
    scene.push(quad(rect(
        clip.x + 1.0,
        clip.y + 1.0,
        clip.w - 2.0,
        clip.h - 2.0,
    )));
    scene.push(contained(rect(
        clip.x + 1.0,
        clip.y + 1.0,
        clip.w - 2.0,
        clip.h - 2.0,
    )));
    scene.push(Primitive::LayerEnd);
    scene
}

/// A maximal run of affine stages fuses to **one** op that rides the composite
/// the layer already draws — zero passes of its own, no matter how long the run
/// (§17.3).
#[test]
fn an_affine_run_is_one_op_and_no_pass_of_its_own() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let clip = rect(20.0, 20.0, 40.0, 40.0);
    let chain = [
        ColorEffect::Brightness(1.2),
        ColorEffect::Contrast(1.1),
        ColorEffect::Saturation(0.8),
        ColorEffect::HueRotate(0.3),
        ColorEffect::Grayscale(0.25),
    ];

    let short = frame(&mut gpu, &mut r, &graded(clip, &chain[..1]));
    let long = frame(&mut gpu, &mut r, &graded(clip, &chain));

    assert_eq!(long.color_effect_ops, 1, "five affine stages are one op");
    assert_eq!(
        long.color_transform_passes, 0,
        "a fused op rides the composite: no pass of its own"
    );
    assert_eq!(
        long.render_passes, short.render_passes,
        "chain length must not reach the pass plan"
    );
    assert_eq!(
        long.offscreen_passes, short.offscreen_passes,
        "fusion must not split the layer it rides on"
    );
}

/// `Gamma` is the one non-affine stage, so it splits a run: two ops, and exactly
/// **one** extra pass — the leading half has to be evaluated somewhere before the
/// power is applied, while the trailing half still rides the composite.
#[test]
fn a_non_affine_stage_splits_the_run_for_exactly_one_pass() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let clip = rect(20.0, 20.0, 40.0, 40.0);

    let fused = frame(
        &mut gpu,
        &mut r,
        &graded(
            clip,
            &[ColorEffect::Brightness(1.2), ColorEffect::Saturation(0.8)],
        ),
    );
    let split = frame(
        &mut gpu,
        &mut r,
        &graded(
            clip,
            &[
                ColorEffect::Brightness(1.2),
                ColorEffect::Gamma(2.2),
                ColorEffect::Saturation(0.8),
            ],
        ),
    );

    assert_eq!(fused.color_effect_ops, 1);
    assert_eq!(fused.color_transform_passes, 0);
    assert_eq!(
        split.color_effect_ops, 2,
        "a non-affine stage splits the run exactly once"
    );
    assert_eq!(
        split.color_transform_passes, 1,
        "the leading op costs one pass; the trailing one still rides the composite"
    );
    assert_eq!(
        split.render_passes,
        fused.render_passes + 1,
        "a split adds its own pass and nothing else"
    );
}

/// Fusion is a matrix product, and matrix products do not commute: swapping two
/// stages must produce a different fused op, not the same one. This is the
/// contract that makes fusion safe to apply silently — it is an optimization of
/// evaluation, not of ordering.
#[test]
fn fusion_preserves_the_authored_order() {
    let clip = rect(20.0, 20.0, 40.0, 40.0);
    let a = [ColorEffect::Saturation(0.0), ColorEffect::Sepia(1.0)];
    let b = [ColorEffect::Sepia(1.0), ColorEffect::Saturation(0.0)];

    let forward = surface_pixels(&graded(clip, &a));
    let reverse = surface_pixels(&graded(clip, &b));

    assert_ne!(
        forward, reverse,
        "desaturate-then-sepia and sepia-then-desaturate are different functions; \
         a fused chain that produced the same pixels would have reordered the \
         authored chain"
    );
}

/// Upload a scene and read the composited surface back as BGRA8.
fn surface_pixels(primitives: &[Primitive]) -> Vec<u8> {
    let (mut gpu, surface, mut r) = renderer(W, H);
    r.upload(&mut gpu, primitives);
    r.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    gpu.read_pixels_bgra8(surface)
}

// ---------------------------------------------------------------------------
// 4. Advanced-blend isolation
// ---------------------------------------------------------------------------

/// A badge drawn with `mode` inside its own group.
fn blended(clip: Rect, mode: Blend) -> Vec<Primitive> {
    vec![
        plain(clip),
        Primitive::Blend(mode),
        quad(rect(clip.x + 1.0, clip.y + 1.0, clip.w - 2.0, clip.h - 2.0)),
        contained(rect(clip.x + 1.0, clip.y + 1.0, clip.w - 2.0, clip.h - 2.0)),
        Primitive::LayerEnd,
    ]
}

/// A blend the fixed-function stage cannot express reads its destination through
/// a snapshot bounded by the group's ROI — never the surface — and pays for it
/// inside the layer target it already needed (§14.6).
#[test]
fn an_advanced_blend_reads_a_bounded_destination() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let clip = rect(40.0, 30.0, 24.0, 24.0);
    let mut scene = vec![background()];
    scene.extend(blended(clip, Blend::Multiply));
    let stats = frame(&mut gpu, &mut r, &scene);

    assert_eq!(
        stats.blend_isolations, 1,
        "a non-fixed-function blend isolates exactly once"
    );
    assert_eq!(
        stats.offscreen_passes, 1,
        "and does so in the group's own target, not an extra one"
    );
    assert_eq!(
        stats.backdrop_captures, 1,
        "the destination is read through one capture"
    );
    assert!(
        stats.backdrop_capture_pixels < (W as usize) * (H as usize),
        "a destination read must be bounded by the group's ROI, not the surface \
         ({} px)",
        stats.backdrop_capture_pixels
    );
}

/// `SrcOver` is the fixed-function lane and must stay free: no isolation, no
/// target, no capture, one surface pass. The forbidden shape is a renderer that
/// isolates whenever a blend is *named*.
#[test]
fn src_over_never_isolates() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let clip = rect(40.0, 30.0, 24.0, 24.0);
    let mut scene = vec![background()];
    scene.extend(blended(clip, Blend::SrcOver));
    let stats = frame(&mut gpu, &mut r, &scene);

    assert_eq!(stats.blend_isolations, 0);
    assert_eq!(stats.offscreen_passes, 0);
    assert_eq!(stats.backdrop_captures, 0);
    assert_eq!(
        stats.render_passes, 1,
        "the fixed-function lane is one surface pass"
    );
}

// ---------------------------------------------------------------------------
// 5. Local / Nonlocal and the elimination ladder
// ---------------------------------------------------------------------------

/// The frontier: exactly which rungs of the cost ladder are local (§3102) and
/// which are nonlocal (§3120), plus what each rung actually demands. A local
/// class shades in place and therefore demands *nothing* — no target, no capture,
/// no destination read; every nonlocal class demands at least one of them. That
/// implication is the frontier, and `is_local` / `locality` must agree with it.
#[test]
fn the_locality_frontier_is_one_threshold() {
    // (class, locality, mask, offscreen, backdrop, destination read)
    let table = [
        (
            EffectCost::Local,
            EffectLocality::Local,
            false,
            false,
            false,
            false,
        ),
        (
            EffectCost::Analytic,
            EffectLocality::Local,
            false,
            false,
            false,
            false,
        ),
        (
            EffectCost::NeedsMask,
            EffectLocality::Local,
            true,
            false,
            false,
            false,
        ),
        (
            EffectCost::NeedsOffscreen,
            EffectLocality::Nonlocal,
            false,
            true,
            false,
            false,
        ),
        (
            EffectCost::NeedsBackdrop,
            EffectLocality::Nonlocal,
            false,
            true,
            true,
            false,
        ),
        (
            EffectCost::DestinationRead,
            EffectLocality::Nonlocal,
            false,
            false,
            false,
            true,
        ),
        (
            EffectCost::ComputePreferred,
            EffectLocality::Nonlocal,
            false,
            false,
            false,
            false,
        ),
    ];
    for (cost, locality, mask, offscreen, backdrop, destination) in table {
        assert_eq!(cost.locality(), locality, "{}", cost.label());
        assert_eq!(
            cost.is_local(),
            locality == EffectLocality::Local,
            "{}: is_local must agree with locality",
            cost.label()
        );
        assert_eq!(cost.needs_mask(), mask, "{}: needs_mask", cost.label());
        assert_eq!(
            cost.needs_offscreen(),
            offscreen,
            "{}: needs_offscreen",
            cost.label()
        );
        assert_eq!(
            cost.needs_backdrop(),
            backdrop,
            "{}: needs_backdrop",
            cost.label()
        );
        assert_eq!(
            cost.reads_destination(),
            destination,
            "{}: reads_destination",
            cost.label()
        );
        if locality == EffectLocality::Local {
            assert!(
                !offscreen && !backdrop && !destination,
                "{}: a local class shades in place, so it must demand no pixels it \
                 does not own — a mask is a separate coverage build, not a read of \
                 someone else's color (§3102)",
                cost.label()
            );
        } else {
            assert!(
                offscreen || backdrop || destination || cost == EffectCost::ComputePreferred,
                "{}: a nonlocal class must name what it needs beyond its fragment",
                cost.label()
            );
        }
    }
}

/// A chain is as nonlocal as its worst link, and an empty chain is local — one
/// local link never rescues a nonlocal one, and a run of local links never
/// escalates.
#[test]
fn a_chain_is_as_nonlocal_as_its_worst_link() {
    assert_eq!(EffectCost::dominating([]), EffectCost::Local);
    assert_eq!(
        EffectCost::dominating([EffectCost::Local, EffectCost::Analytic]),
        EffectCost::Analytic
    );
    assert!(
        EffectCost::dominating([EffectCost::Local, EffectCost::NeedsBackdrop]).needs_offscreen(),
        "one nonlocal link taints the chain"
    );
    assert!(
        EffectCost::dominating([EffectCost::Analytic, EffectCost::NeedsMask]).is_local(),
        "a run of local links stays fusable"
    );
}

/// The eight reasons are a closed, ordered vocabulary with distinct labels, and
/// every one of them is nonlocal at a pinned rung — "a reason survived planning"
/// and "this effect needs a target" are the same statement (§3149).
#[test]
fn the_layer_reason_vocabulary_is_closed_and_nonlocal() {
    assert_eq!(LayerReason::ALL.len(), 8);
    for (i, reason) in LayerReason::ALL.iter().enumerate() {
        assert_eq!(reason.index(), i as u32, "{}", reason.label());
        assert!(!reason.label().is_empty());
        assert!(
            !reason.cost().is_local(),
            "{} raised a layer, so it cannot be local (§3120)",
            reason.label()
        );
    }
    assert_eq!(
        LayerReason::BackdropFilter.cost(),
        EffectCost::NeedsBackdrop,
        "a backdrop filter reads pixels behind the group"
    );
    assert_eq!(
        LayerReason::AdvancedBlend.cost(),
        EffectCost::DestinationRead,
        "an advanced blend reads what it draws over"
    );
    for reason in LayerReason::ALL {
        if !matches!(
            reason,
            LayerReason::BackdropFilter | LayerReason::AdvancedBlend
        ) {
            assert_eq!(
                reason.cost(),
                EffectCost::NeedsOffscreen,
                "{} needs a target of its own and nothing more",
                reason.label()
            );
        }
    }
}

/// The six rungs are a closed, ordered vocabulary too, and both sets are single
/// bytes — a plan is a value, not an allocation (§28).
#[test]
fn the_elimination_ladder_is_closed_and_the_sets_are_bytes() {
    assert_eq!(LayerElimination::ALL.len(), 6);
    for (i, rung) in LayerElimination::ALL.iter().enumerate() {
        assert_eq!(rung.index(), i as u32, "{}", rung.label());
        assert!(!rung.label().is_empty());
    }
    assert_eq!(size_of::<ReasonSet>(), 1);
    assert_eq!(size_of::<EliminationSet>(), 1);
    assert!(
        size_of::<LayerPlan>() <= 8,
        "a whole plan must stay pointer-sized-ish, not grow a Vec ({} B)",
        size_of::<LayerPlan>()
    );

    let mut set = ReasonSet::of(LayerReason::GroupOpacity);
    set.insert(LayerReason::AdvancedBlend);
    assert_eq!(set.len(), 2);
    assert_eq!(
        set.iter().collect::<Vec<_>>(),
        vec![LayerReason::GroupOpacity, LayerReason::AdvancedBlend],
        "iteration follows LayerReason::ALL, so an inspector dump is stable (§62)"
    );
    set.remove(LayerReason::GroupOpacity);
    assert_eq!(set, ReasonSet::of(LayerReason::AdvancedBlend));
    assert_eq!(ReasonSet::EMPTY.bits(), 0);
}

/// The ladder's order, which is the part most easily broken by adding a rung:
/// the rungs that remove a *reason* must all run before the guard that revokes a
/// non-eliminating elimination. A translucent frosted group over disjoint children
/// both folds its opacity **and** retires its backdrop reason; evaluating the
/// share after the guard would have revoked the fold for a reason that was about
/// to disappear.
#[test]
fn reason_removing_rungs_run_before_the_revocation_guard() {
    let plan = plan_layer(&LayerRequest {
        opacity: 0.5,
        overlap: ChildOverlap::Disjoint,
        filters_backdrop: true,
        shared_backdrop: true,
        ..LayerRequest::default()
    });

    assert_eq!(
        plan.requested,
        {
            let mut s = ReasonSet::of(LayerReason::GroupOpacity);
            s.insert(LayerReason::BackdropFilter);
            s
        },
        "both reasons must be raised before either is eliminated"
    );
    assert!(
        plan.surviving.is_empty(),
        "and both must be gone afterwards: {:?}",
        plan.surviving
    );
    assert!(plan.folds_opacity(), "the fold must survive the guard");
    assert_eq!(plan.fold_opacity, 0.5);
    assert!(plan.eliminated.contains(LayerElimination::SharedBackdrop));
    assert!(!plan.needs_offscreen());
    assert_eq!(plan.locality(), EffectLocality::Local);
}

/// The guard itself: an elimination that does not eliminate is not recorded. A
/// group kept alive by a complex mask composites through a target anyway, where
/// the group opacity is free — so folding the same factor into every child would
/// be work with no saving, and a second place for it to be wrong.
#[test]
fn an_elimination_that_does_not_eliminate_is_not_recorded() {
    let plan = plan_layer(&LayerRequest {
        opacity: 0.5,
        overlap: ChildOverlap::Disjoint,
        complex_mask: true,
        ..LayerRequest::default()
    });

    assert!(plan.needs_offscreen(), "the mask keeps the target");
    assert!(
        !plan.folds_opacity(),
        "so the fold must be revoked, not reported"
    );
    assert_eq!(plan.fold_opacity, 1.0);
    assert!(plan.surviving.contains(LayerReason::GroupOpacity));
    assert!(plan.surviving.contains(LayerReason::ComplexMask));
}

/// The scissor rung is a statement about the outcome of every other rung, so it
/// is recorded exactly when the layer ended up staying in its parent's pass —
/// which is exactly when no reason survived, which is exactly when the plan is
/// local. Three spellings, one fact, checked across the whole request space that
/// a single flag can describe.
#[test]
fn the_scissor_rung_is_the_outcome_of_the_ladder() {
    let requests = [
        LayerRequest::default(),
        LayerRequest {
            opacity: 0.5,
            overlap: ChildOverlap::Disjoint,
            ..LayerRequest::default()
        },
        LayerRequest {
            opacity: 0.5,
            overlap: ChildOverlap::Overlapping,
            ..LayerRequest::default()
        },
        LayerRequest {
            opacity: 0.5,
            overlap: ChildOverlap::Unknown,
            ..LayerRequest::default()
        },
        LayerRequest {
            blurs_content: true,
            ..LayerRequest::default()
        },
        LayerRequest {
            filters_backdrop: true,
            ..LayerRequest::default()
        },
        LayerRequest {
            filters_backdrop: true,
            shared_backdrop: true,
            ..LayerRequest::default()
        },
        LayerRequest {
            advanced_blend: true,
            ..LayerRequest::default()
        },
        LayerRequest {
            color_effects: 4,
            fused_color_ops: 0,
            ..LayerRequest::default()
        },
        LayerRequest {
            color_effects: 4,
            fused_color_ops: 1,
            ..LayerRequest::default()
        },
        LayerRequest {
            isolated: true,
            ..LayerRequest::default()
        },
        LayerRequest {
            snapshot_cached: true,
            ..LayerRequest::default()
        },
        LayerRequest {
            native_material_boundary: true,
            ..LayerRequest::default()
        },
    ];

    for request in requests {
        let plan = plan_layer(&request);
        let stays_in_pass = plan.surviving.is_empty();
        assert_eq!(
            plan.eliminated
                .contains(LayerElimination::ScissorInsteadOfClipLayer),
            stays_in_pass,
            "the scissor rung must track the outcome, not a request flag: {request:?}"
        );
        assert_eq!(
            plan.needs_offscreen(),
            !stays_in_pass,
            "a target is created exactly when a reason survived: {request:?}"
        );
        assert_eq!(
            plan.locality() == EffectLocality::Local,
            stays_in_pass,
            "and locality is that same decision: {request:?}"
        );
        assert_eq!(
            plan.dominating_reason().is_some(),
            !stays_in_pass,
            "a surviving layer must be able to name why: {request:?}"
        );
        assert!(
            plan.eliminated.len() <= LayerElimination::ALL.len() as u32,
            "no rung may be recorded twice: {request:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 6. Effect Damage
// ---------------------------------------------------------------------------

/// Two panels over two separate quads. Recoloring what is under the first dirties
/// **its** ROI and leaves the second's revision untouched — a backdrop is damaged
/// by what changed behind it, not by the frame having changed (§3202).
#[test]
fn damage_is_scoped_to_the_roi_that_changed() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let under_a = rect(14.0, 14.0, 40.0, 40.0);
    let under_b = rect(130.0, 74.0, 40.0, 40.0);
    let mut scene = vec![quad(under_a), quad(under_b)];
    scene.extend(panel(rect(22.0, 22.0, 24.0, 24.0), 2.0));
    scene.extend(panel(rect(138.0, 82.0, 24.0, 24.0), 2.0));

    let first = frame(&mut gpu, &mut r, &scene);
    assert_eq!(
        first.backdrop_captures, 2,
        "the two panels are far apart, so they do not share"
    );
    let before: Vec<_> = r.backdrop_dependencies().to_vec();
    assert_eq!(before.len(), 2);

    scene[0] = tinted(under_a, Rgba::new(0.05, 0.9, 0.35, 1.0));
    let second = frame(&mut gpu, &mut r, &scene);
    let after = r.backdrop_dependencies();

    assert_eq!(
        second.backdrop_dirty_rois, 1,
        "exactly one backdrop saw its content change"
    );
    assert!(after[0].dirty, "the panel over the recolored quad is dirty");
    assert!(!after[1].dirty, "the other panel is not");
    assert_ne!(
        after[0].revision, before[0].revision,
        "a dirty backdrop's revision must advance"
    );
    assert_eq!(
        after[1].revision, before[1].revision,
        "a clean backdrop's revision must hold"
    );
}

/// The dependency is on *content*, not on one revision plane: a quad that only
/// moved dirties the backdrop above it exactly as a recolored one does. A
/// backdrop samples pixels, and a moved quad changes pixels.
#[test]
fn a_move_dirties_a_backdrop_as_much_as_a_recolor() {
    let under = rect(14.0, 14.0, 40.0, 40.0);
    let panel_clip = rect(22.0, 22.0, 24.0, 24.0);

    for (label, changed) in [
        ("recolor", tinted(under, Rgba::new(0.05, 0.9, 0.35, 1.0))),
        (
            "move",
            quad(rect(under.x + 3.0, under.y + 3.0, under.w, under.h)),
        ),
    ] {
        let (mut gpu, _s, mut r) = renderer(W, H);
        let mut scene = vec![quad(under)];
        scene.extend(panel(panel_clip, 2.0));

        frame(&mut gpu, &mut r, &scene);
        let before = r.backdrop_dependencies()[0];

        scene[0] = changed;
        let stats = frame(&mut gpu, &mut r, &scene);

        assert_eq!(
            stats.backdrop_dirty_rois, 1,
            "{label}: the backdrop above changed content is dirty"
        );
        assert!(r.backdrop_dependencies()[0].dirty, "{label}");
        assert_ne!(
            r.backdrop_dependencies()[0].revision,
            before.revision,
            "{label}: the revision must advance"
        );
    }
}

/// And the other half: a frame in which nothing moved dirties nothing, however
/// many times it is uploaded. This is the property the static-UI target rests on
/// — a glass panel that re-dirtied itself would redraw forever.
#[test]
fn an_unchanged_frame_dirties_no_backdrop() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let mut scene = vec![background()];
    scene.extend(panel(rect(22.0, 22.0, 24.0, 24.0), 2.0));
    scene.extend(panel(rect(138.0, 82.0, 24.0, 24.0), 2.0));

    // The first frame is a first draw, so every backdrop is legitimately dirty;
    // the second settles it. Steadiness is a property of the frames after that.
    let first = frame(&mut gpu, &mut r, &scene);
    assert_eq!(
        first.backdrop_dirty_rois, 2,
        "a first draw dirties every backdrop it plans"
    );
    frame(&mut gpu, &mut r, &scene);
    let before: Vec<_> = r.backdrop_dependencies().to_vec();
    assert!(
        before.iter().all(|d| !d.dirty),
        "the second upload of an unchanged scene must settle: {before:?}"
    );

    for f in 0..4 {
        let stats = frame(&mut gpu, &mut r, &scene);
        assert_eq!(
            stats.backdrop_dirty_rois, 0,
            "idle frame {f}: nothing changed, so no ROI is dirty"
        );
        assert_eq!(
            r.backdrop_dependencies(),
            before.as_slice(),
            "idle frame {f}: every dependency holds its revision and its ROI"
        );
    }
}

/// A scene with no backdrop reports no dependency at all: the mechanism costs
/// nothing when it is not used.
#[test]
fn no_backdrop_means_no_dependency() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let scene = vec![background(), quad(rect(20.0, 20.0, 30.0, 30.0))];
    let stats = frame(&mut gpu, &mut r, &scene);

    assert_eq!(stats.backdrop_captures, 0);
    assert_eq!(stats.backdrop_dirty_rois, 0);
    assert!(r.backdrop_dependencies().is_empty());
}
