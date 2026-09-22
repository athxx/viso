//! Material lane contract (§19).
//!
//! A material surface has two realizations, and §19's rule is that choosing
//! between them is a *policy over trade-offs*, not a quality ranking:
//!
//! - The **GPU lane** is what M0 built: Viso captures the backdrop, blurs it, and
//!   composites the §18 chain in one draw. Composable, frame-synchronous, identical
//!   on every backend.
//! - The **native lane** hands the material to the platform, which composites it
//!   behind the Viso surface. It matches the system exactly and costs Viso nothing,
//!   but it cannot blur Viso content drawn below the surface.
//!
//! The contracts defended here:
//!
//! 1. **The native lane costs Viso nothing.** No capture, no blur rung, no
//!    composite, no draw — and it says so through `FrameStats`, not by inspection.
//! 2. **No platform-private API reaches the render IR.** The renderer reports a
//!    region with geometry plus the generic parameters the surface authored; the
//!    mapping to a platform material name lives above this crate.
//! 3. **Lane selection is correctness-before-fidelity.** Anything the native lane
//!    cannot do rules it out; only then does system integration pull toward it. A
//!    wrong answer degrades to "Viso drew the glass", never to "the panel composites
//!    wrongly".
//! 4. **The lanes coexist.** A screen mixing both pays for exactly the GPU-lane
//!    panels and reports exactly the native-lane ones.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureFormat};
use viso_render::{
    Border, ColorOp, Corners, FrameStats, FrostedMaterial, MaterialLane, MaterialLaneNeeds,
    Primitive, Quad, Rect, Renderer, Rgba,
};

const W: u32 = 128;
const H: u32 = 128;
const SIGMA: f32 = 4.0;

fn renderer() -> (HeadlessRaster, SurfaceId, Renderer) {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format: TextureFormat = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([W as f32, H as f32]);
    (gpu, surface, r)
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn background() -> Primitive {
    Primitive::Quad(Quad {
        rect: rect(0.0, 0.0, W as f32, H as f32),
        color: Rgba::new(0.2, 0.5, 0.9, 1.0),
        radius: 0.0,
        border: Border::NONE,
    })
}

fn panel(rect: Rect, lane: MaterialLane) -> Primitive {
    let mut m = FrostedMaterial::new(rect, Corners::uniform(12.0), SIGMA);
    m.lane = lane;
    Primitive::Frosted(m)
}

// ---------------------------------------------------------------------------
// 1. The native lane costs Viso nothing
// ---------------------------------------------------------------------------

/// A native-lane surface reserves its region and draws nothing: the platform
/// composites its own material behind the Viso surface, so capturing and blurring a
/// backdrop would render the glass twice and then hide one copy. Against the same
/// panel on the GPU lane, every cost counter drops to zero.
#[test]
fn the_native_lane_costs_no_capture_blur_or_composite() {
    let r = rect(32.0, 32.0, 48.0, 48.0);

    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(&mut gpu, &[background(), panel(r, MaterialLane::Gpu)]);
    let gpu_lane: FrameStats = rend.frame_stats();

    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(&mut gpu, &[background(), panel(r, MaterialLane::Native)]);
    let native: FrameStats = rend.frame_stats();

    // The GPU lane is the thing being compared against: it must actually cost.
    assert_eq!(gpu_lane.material_composites, 1);
    assert_eq!(gpu_lane.backdrop_captures, 1);
    assert!(gpu_lane.blur_passes > 0);
    assert_eq!(gpu_lane.native_material_regions, 0);

    assert_eq!(native.native_material_regions, 1);
    assert_eq!(
        native.material_composites, 0,
        "the native lane composites nothing of its own"
    );
    assert_eq!(
        native.backdrop_captures, 0,
        "and captures no backdrop — the platform reads what is behind the window"
    );
    assert_eq!(native.blur_passes, 0, "and runs no blur ladder");
    assert_eq!(native.transient_targets, 0, "and allocates no target");
    assert_eq!(
        native.render_passes, 1,
        "surface pass only: nothing offscreen happens"
    );
    assert!(
        native.draw_calls < gpu_lane.draw_calls,
        "the composite draw is gone: {} vs {}",
        native.draw_calls,
        gpu_lane.draw_calls
    );
}

/// The border/highlight ring stays Viso's on both lanes. A native material draws its
/// own body but not the app's edge treatment, so the panel's border must still come
/// from the frame — otherwise switching lanes would silently change the design.
#[test]
fn the_border_ring_is_viso_s_on_both_lanes() {
    let r = rect(32.0, 32.0, 48.0, 48.0);
    let bordered = |lane: MaterialLane| {
        let mut m = FrostedMaterial::new(r, Corners::uniform(12.0), SIGMA);
        m.lane = lane;
        m.border = Border {
            width: 2.0,
            color: Rgba::new(1.0, 1.0, 1.0, 0.8),
        };
        Primitive::Frosted(m)
    };

    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(&mut gpu, &[background(), bordered(MaterialLane::Native)]);
    let with_border = rend.frame_stats();

    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(&mut gpu, &[background(), panel(r, MaterialLane::Native)]);
    let without = rend.frame_stats();

    assert_eq!(with_border.native_material_regions, 1);
    assert_eq!(
        with_border.draw_calls,
        without.draw_calls + 1,
        "the ring is one more draw (an analytic rrect, which does not merge with quads)"
    );
}

// ---------------------------------------------------------------------------
// 2. The reported region is generic, not platform-private
// ---------------------------------------------------------------------------

/// The region the renderer hands the platform carries geometry plus the generic
/// parameters the surface authored, and nothing else. There is no platform material
/// name anywhere in it — that mapping is the platform layer's job, which is what
/// keeps platform-private material APIs out of the render IR (§19).
#[test]
fn a_reserved_region_carries_geometry_and_generic_parameters() {
    let r = rect(24.0, 16.0, 64.0, 40.0);
    let mut m = FrostedMaterial::new(r, Corners::uniform(20.0), 7.5);
    m.lane = MaterialLane::Native;
    m.noise = 0.25;
    m.opacity = 0.75;

    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(&mut gpu, &[background(), Primitive::Frosted(m)]);

    let regions = rend.native_material_regions();
    assert_eq!(regions.len(), 1);
    let region = regions[0];
    assert_eq!(region.rect, r);
    assert_eq!(
        region.radius,
        m.radii(),
        "both lanes round through the same normalized radii, so a lane switch \
         cannot change the corner"
    );
    assert_eq!(
        region.sigma, 7.5,
        "the sigma is a hint, not a kernel to run"
    );
    assert_eq!(region.color, ColorOp::IDENTITY);
    assert_eq!(region.noise, 0.25);
    assert_eq!(region.opacity, 0.75);
}

/// A clipped native surface reports the *visible* region, not the authored one: the
/// platform sizes a real view from this, so handing it geometry the frame clipped
/// away would place a material where nothing is drawn.
#[test]
fn a_clipped_region_reports_what_survived_the_clip() {
    let r = rect(0.0, 0.0, 80.0, 80.0);
    let clip = rect(32.0, 32.0, 96.0, 96.0);

    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(
        &mut gpu,
        &[
            background(),
            Primitive::Layer(viso_render::LayerClip {
                clip,
                opacity: 1.0,
                blur_sigma: 0.0,
                backdrop_sigma: 0.0,
            }),
            panel(r, MaterialLane::Native),
            Primitive::LayerEnd,
        ],
    );

    let regions = rend.native_material_regions();
    assert_eq!(regions.len(), 1);
    assert_eq!(
        regions[0].rect,
        rect(32.0, 32.0, 48.0, 48.0),
        "the reserved region is the surface intersected with the live clip"
    );
}

/// Inside a group-opacity layer the native lane gives up. That layer composites its
/// subtree through an offscreen texture, and a material the *platform* composites
/// behind the Viso surface is not in that texture — the layer's opacity could never
/// reach it. Reserving a region there would hand the platform a material the frame
/// then fails to honour, so the surface falls back to what the GPU lane can do at
/// that position, matching it exactly.
#[test]
fn a_native_surface_inside_a_group_opacity_layer_falls_back() {
    let r = rect(32.0, 32.0, 48.0, 48.0);
    let scene = |lane: MaterialLane| {
        [
            background(),
            Primitive::Layer(viso_render::LayerClip {
                clip: rect(0.0, 0.0, W as f32, H as f32),
                opacity: 0.5,
                blur_sigma: 0.0,
                backdrop_sigma: 0.0,
            }),
            panel(r, lane),
            Primitive::LayerEnd,
        ]
    };

    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(&mut gpu, &scene(MaterialLane::Native));
    let native = rend.frame_stats();
    assert_eq!(
        rend.native_material_regions().len(),
        0,
        "no region is promised to the platform where the frame could not honour it"
    );

    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(&mut gpu, &scene(MaterialLane::Gpu));
    let as_gpu = rend.frame_stats();

    assert_eq!(native.native_material_regions, 0);
    assert_eq!(native.material_composites, as_gpu.material_composites);
    assert_eq!(native.draw_calls, as_gpu.draw_calls);
    assert_eq!(native.render_passes, as_gpu.render_passes);
    assert_eq!(native.blur_passes, as_gpu.blur_passes);
}

/// The reported opacity and sigma are the surface's own, unmodified: the platform
/// receives what the author asked for, not a value the renderer substituted, so the
/// mapping into a platform material vocabulary has something honest to map.
#[test]
fn the_reported_parameters_are_the_authored_ones() {
    let r = rect(32.0, 32.0, 48.0, 48.0);
    let mut m = FrostedMaterial::new(r, Corners::SHARP, 0.0);
    m.lane = MaterialLane::Native;
    m.opacity = 0.8;

    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(&mut gpu, &[background(), Primitive::Frosted(m)]);
    let regions = rend.native_material_regions();
    assert_eq!(regions.len(), 1);
    assert!(
        (regions[0].opacity - 0.8).abs() < 1e-6,
        "the surface's own opacity, got {}",
        regions[0].opacity
    );
    assert_eq!(
        regions[0].sigma, 0.0,
        "a zero sigma is reported as authored: the platform decides whether its \
         material blurs, the renderer does not quietly substitute a default"
    );
}

/// The region list is per-frame, like every other derived draw: a frame with no
/// native surfaces reports none, and last frame's regions do not linger.
#[test]
fn the_region_list_is_rebuilt_every_frame() {
    let r = rect(32.0, 32.0, 48.0, 48.0);
    let (mut gpu, _s, mut rend) = renderer();

    rend.upload(&mut gpu, &[background(), panel(r, MaterialLane::Native)]);
    assert_eq!(rend.native_material_regions().len(), 1);

    rend.upload(&mut gpu, &[background()]);
    assert_eq!(rend.native_material_regions().len(), 0);
    assert_eq!(rend.frame_stats().native_material_regions, 0);

    rend.upload(&mut gpu, &[background(), panel(r, MaterialLane::Native)]);
    assert_eq!(rend.native_material_regions().len(), 1);
}

// ---------------------------------------------------------------------------
// 3. Selection is correctness before fidelity
// ---------------------------------------------------------------------------

/// The default lane is the portable one. A surface that says nothing about its needs
/// gets the GPU lane, which works identically on every backend.
#[test]
fn the_default_lane_is_the_portable_one() {
    let m = FrostedMaterial::new(rect(0.0, 0.0, 10.0, 10.0), Corners::SHARP, SIGMA);
    assert_eq!(m.lane, MaterialLane::Gpu);
    assert_eq!(MaterialLane::default(), MaterialLane::Gpu);
    assert_eq!(
        MaterialLane::select(MaterialLaneNeeds::default()),
        MaterialLane::Gpu,
        "an empty set of needs must not opt into a platform-specific realization"
    );
}

/// System integration is what pulls toward the native lane — and it is the *only*
/// thing that does. Without it there is no reason to give up composability.
#[test]
fn system_integration_is_what_chooses_the_native_lane() {
    let needs = MaterialLaneNeeds {
        native_available: true,
        system_integration: true,
        ..MaterialLaneNeeds::default()
    };
    assert_eq!(MaterialLane::select(needs), MaterialLane::Native);

    assert_eq!(
        MaterialLane::select(MaterialLaneNeeds {
            system_integration: false,
            ..needs
        }),
        MaterialLane::Gpu,
        "a panel with no system-integration requirement keeps the composable lane"
    );
    assert_eq!(
        MaterialLane::select(MaterialLaneNeeds {
            native_available: false,
            ..needs
        }),
        MaterialLane::Gpu,
        "a backend with no native material must still render the panel"
    );
}

/// Each disqualifier alone overrides system integration. This is the
/// correctness-before-fidelity precedence: the native lane composites behind the
/// whole Viso surface and animates on the system's schedule, so a surface that needs
/// Viso content to show through, or that animates per frame, or that must compose
/// with surrounding Viso effects, cannot use it — no matter how much it would like
/// to match the system.
#[test]
fn any_requirement_the_native_lane_cannot_meet_overrides_fidelity() {
    let eligible = MaterialLaneNeeds {
        native_available: true,
        system_integration: true,
        ..MaterialLaneNeeds::default()
    };
    assert_eq!(MaterialLane::select(eligible), MaterialLane::Native);

    for (label, needs) in [
        (
            "Viso content below must show through",
            MaterialLaneNeeds {
                viso_content_below: true,
                ..eligible
            },
        ),
        (
            "the surface animates per frame",
            MaterialLaneNeeds {
                animated: true,
                ..eligible
            },
        ),
        (
            "the surface must compose with Viso effects",
            MaterialLaneNeeds {
                composability: true,
                ..eligible
            },
        ),
    ] {
        assert_eq!(
            MaterialLane::select(needs),
            MaterialLane::Gpu,
            "{label}: must fall back to the lane that can actually do it"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. The lanes coexist
// ---------------------------------------------------------------------------

/// A screen mixing both lanes pays for exactly its GPU-lane panels. Two native
/// panels alongside two GPU ones still share one capture and one blur ladder — the
/// native panels neither join the capture group nor break its sharing.
#[test]
fn a_mixed_screen_pays_only_for_its_gpu_lane_panels() {
    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(
        &mut gpu,
        &[
            background(),
            panel(rect(8.0, 8.0, 40.0, 24.0), MaterialLane::Gpu),
            panel(rect(8.0, 40.0, 40.0, 24.0), MaterialLane::Native),
            panel(rect(8.0, 72.0, 40.0, 24.0), MaterialLane::Gpu),
            panel(rect(8.0, 104.0, 40.0, 20.0), MaterialLane::Native),
        ],
    );
    let mixed = rend.frame_stats();

    assert_eq!(mixed.material_composites, 2, "the two GPU-lane panels");
    assert_eq!(mixed.native_material_regions, 2, "the two native ones");
    assert_eq!(
        mixed.backdrop_captures, 1,
        "the GPU-lane panels still share one capture across the native ones \
         interleaved between them"
    );

    // The same two GPU panels with the native ones removed: identical GPU cost, so
    // a native panel is not silently widening the capture ROI it sits inside.
    let (mut gpu, _s, mut rend) = renderer();
    rend.upload(
        &mut gpu,
        &[
            background(),
            panel(rect(8.0, 8.0, 40.0, 24.0), MaterialLane::Gpu),
            panel(rect(8.0, 72.0, 40.0, 24.0), MaterialLane::Gpu),
        ],
    );
    let only_gpu = rend.frame_stats();
    assert_eq!(mixed.backdrop_captures, only_gpu.backdrop_captures);
    assert_eq!(mixed.blur_passes, only_gpu.blur_passes);
    assert_eq!(
        mixed.backdrop_capture_pixels, only_gpu.backdrop_capture_pixels,
        "a reserved region contributes nothing to the captured ROI"
    );
}

/// Switching a panel's lane changes nothing else about the frame's structure: the
/// same primitive stream, the same paint order, the same border. Only the material's
/// realization moves, which is what makes the lane a safe runtime choice.
#[test]
fn switching_lanes_is_the_only_difference_between_two_frames() {
    let r = rect(32.0, 32.0, 48.0, 48.0);
    let (mut gpu, _s, mut rend) = renderer();

    rend.upload(&mut gpu, &[background(), panel(r, MaterialLane::Gpu)]);
    let as_gpu = rend.frame_stats();
    rend.upload(&mut gpu, &[background(), panel(r, MaterialLane::Native)]);
    let as_native = rend.frame_stats();
    rend.upload(&mut gpu, &[background(), panel(r, MaterialLane::Gpu)]);
    let back_to_gpu = rend.frame_stats();

    assert_eq!(as_gpu.material_composites, 1);
    assert_eq!(as_native.material_composites, 0);
    assert_eq!(
        back_to_gpu.material_composites, 1,
        "switching back restores the GPU lane with no residue from the native frame"
    );
    assert_eq!(back_to_gpu.backdrop_captures, as_gpu.backdrop_captures);
    assert_eq!(back_to_gpu.blur_passes, as_gpu.blur_passes);
    assert_eq!(back_to_gpu.draw_calls, as_gpu.draw_calls);
}
