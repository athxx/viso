//! Vector lane contract (§20.1): compute is workload specialization.
//!
//! §20.1 describes a second vector lane — GPU binning, a parallel prefix
//! allocation, per-tile coverage, fine raster — and then states the rule that
//! matters more than the pipeline: it is entered **only** when a benchmark proves
//! a large dynamic workload benefits. Twenty ordinary buttons, a panel, or a few
//! dozen stable paths must never be routed through compute dispatch for the sake
//! of architectural uniformity (§7.2).
//!
//! That rule is easy to state and easy to erode, so it is pinned from both ends
//! here:
//!
//! 1. **The policy.** [`VectorLane::select`] is conjunctive — capability, a
//!    *measured* bottleneck, and a large-and-dynamic scene shape must all hold —
//!    so every incomplete description of a workload lands on the CPU lane. A
//!    caller that has not profiled cannot reach compute at all.
//! 2. **The frame.** Real scenes drawn through the real renderer report zero
//!    compute dispatches, including scenes with dozens of filled and stroked
//!    paths. This is the arithmetic form of "ordinary UI has zero compute
//!    dependency": a future lane that dispatched for a button would fail here
//!    without anyone having to review it.
//!
//! The thresholds themselves are deliberately not asserted as values — §20.1 says
//! the algorithm and its crossovers are not public ABI. What is pinned is the
//! *shape*: which way each condition points, and that all of them are required.

use viso_gpu::{Caps, GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureFormat};
use viso_render::{
    Border, LayerClip, PathCmd, Point, Primitive, Quad, Rect, Renderer, Rgba, Stroke, VectorLane,
    VectorWorkload,
};

const W: u32 = 256;
const H: u32 = 256;

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

/// One button: a rounded quad plus a small stroked vector glyph over it, which is
/// what an ordinary control actually costs the vector lane.
fn button(i: usize) -> Vec<Primitive> {
    let x = 8.0 + (i % 4) as f32 * 60.0;
    let y = 8.0 + (i / 4) as f32 * 40.0;
    vec![
        Primitive::Quad(Quad {
            rect: rect(x, y, 52.0, 32.0),
            color: Rgba::new(0.2, 0.4, 0.8, 1.0),
            radius: 6.0,
            border: Border::NONE,
        }),
        Primitive::Path(viso_render::Path {
            cmds: vec![
                PathCmd::MoveTo(Point {
                    x: x + 10.0,
                    y: y + 16.0,
                }),
                PathCmd::LineTo(Point {
                    x: x + 18.0,
                    y: y + 24.0,
                }),
                PathCmd::LineTo(Point {
                    x: x + 34.0,
                    y: y + 8.0,
                }),
            ],
            fill: None,
            stroke: Some(Stroke::new(2.0, Rgba::new(1.0, 1.0, 1.0, 1.0))),
            shadow: None,
        }),
    ]
}

/// A filled, curved blob — the general-path case, so the scene is not made only
/// of straight segments a tessellator trivially handles. A fill-only concave or
/// curved path takes the self-masked coverage lane (§14.4); adding a stroke sends
/// the same outline down the tessellated lane instead, so `outlined` selects which
/// of the two CPU lanes the caller wants to exercise.
fn blob(cx: f32, cy: f32, r: f32, outlined: bool) -> Primitive {
    Primitive::Path(viso_render::Path {
        cmds: vec![
            PathCmd::MoveTo(Point { x: cx - r, y: cy }),
            PathCmd::CubicTo(
                Point {
                    x: cx - r,
                    y: cy - r,
                },
                Point {
                    x: cx + r,
                    y: cy - r,
                },
                Point { x: cx + r, y: cy },
            ),
            PathCmd::CubicTo(
                Point {
                    x: cx + r,
                    y: cy + r,
                },
                Point {
                    x: cx - r,
                    y: cy + r,
                },
                Point { x: cx - r, y: cy },
            ),
            PathCmd::Close,
        ],
        fill: Some(Rgba::new(0.9, 0.3, 0.2, 1.0)),
        stroke: outlined.then(|| Stroke::new(1.5, Rgba::new(0.1, 0.1, 0.1, 1.0))),
        shadow: None,
    })
}

// ---------------------------------------------------------------------------
// 1. The policy: all three conditions are required
// ---------------------------------------------------------------------------

/// The workload §20.1 names the lane for: a large churning segment population on
/// a compute-capable backend where tessellation is the *measured* limit.
fn a_vector_editor_mid_drag() -> VectorWorkload {
    VectorWorkload {
        compute_available: true,
        segments: 250_000,
        remeshed_segments_per_frame: 80_000,
        clip_composite_ops: 10,
        tessellation_share: 0.55,
    }
}

#[test]
fn the_lane_ordinary_ui_uses_is_the_default_one() {
    assert_eq!(VectorLane::default(), VectorLane::CpuTessellate);
    assert_eq!(
        VectorLane::select(VectorWorkload::default()),
        VectorLane::CpuTessellate
    );
}

#[test]
fn a_large_churning_workload_is_what_the_compute_lane_is_for() {
    assert_eq!(
        VectorLane::select(a_vector_editor_mid_drag()),
        VectorLane::GpuCompute
    );
}

/// Removing any single condition returns the answer to the CPU lane. Stated as
/// one table so the conjunction cannot decay into a disjunction: each row is the
/// same qualifying workload minus one requirement.
#[test]
fn every_condition_is_individually_necessary() {
    let base = a_vector_editor_mid_drag();
    for (missing, workload) in [
        (
            "the backend cannot dispatch",
            VectorWorkload {
                compute_available: false,
                ..base
            },
        ),
        (
            "nobody measured the frame",
            VectorWorkload {
                tessellation_share: 0.0,
                ..base
            },
        ),
        (
            "the scene is small",
            VectorWorkload {
                segments: 600,
                remeshed_segments_per_frame: 600,
                ..base
            },
        ),
        (
            "the geometry is stable",
            VectorWorkload {
                remeshed_segments_per_frame: 0,
                clip_composite_ops: 3,
                ..base
            },
        ),
    ] {
        assert_eq!(
            VectorLane::select(workload),
            VectorLane::CpuTessellate,
            "{missing}: the compute lane must not be entered"
        );
    }
}

/// The measurement is the gate §7.3 asks for, and it is a *type*, not a comment:
/// the share defaults to zero, so "we did not profile" and "tessellation is not
/// the bottleneck" are the same input and give the same answer.
#[test]
fn an_unprofiled_frame_cannot_reach_the_compute_lane() {
    let huge_and_churning = VectorWorkload {
        compute_available: true,
        segments: 5_000_000,
        remeshed_segments_per_frame: 5_000_000,
        clip_composite_ops: 50_000,
        ..Default::default()
    };
    assert!(!huge_and_churning.tessellation_is_the_bottleneck());
    assert!(huge_and_churning.is_large_and_dynamic());
    assert_eq!(
        VectorLane::select(huge_and_churning),
        VectorLane::CpuTessellate,
        "scale alone is not evidence of a benefit"
    );
}

/// A big *stable* scene belongs in retained cached geometry, whose per-frame cost
/// is already zero — re-binning it every frame would add work the CPU lane had
/// stopped doing.
#[test]
fn a_large_stable_scene_stays_with_retained_geometry() {
    let map_being_scrolled = VectorWorkload {
        remeshed_segments_per_frame: 0,
        clip_composite_ops: 12,
        ..a_vector_editor_mid_drag()
    };
    assert_eq!(
        VectorLane::select(map_being_scrolled),
        VectorLane::CpuTessellate
    );
}

/// The §20.1 hard rule over the scenes it actually names, on a backend that can
/// dispatch and with tessellation measured as the frame's limit — the most
/// favorable case the rule has to survive.
#[test]
fn twenty_buttons_and_a_panel_are_never_routed_through_compute() {
    for (label, segments, remeshed) in [
        ("twenty buttons", 20 * 8, 20 * 8),
        ("a panel of a few dozen stable paths", 48 * 40, 0),
        ("a dense icon grid, every icon redrawn", 200 * 24, 200 * 24),
    ] {
        let workload = VectorWorkload {
            compute_available: true,
            segments,
            remeshed_segments_per_frame: remeshed,
            clip_composite_ops: 24,
            tessellation_share: 0.9,
        };
        assert_eq!(
            VectorLane::select(workload),
            VectorLane::CpuTessellate,
            "{label} must not be routed through compute dispatch"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The frame: real scenes dispatch nothing
// ---------------------------------------------------------------------------

/// No backend offers a dispatch encoder, so nothing above one may plan a compute
/// pass. The capability is a veto and it is reported honestly: a device with
/// compute units still reports `false` while the RHI has no way to call them.
#[test]
fn no_backend_offers_a_dispatch_encoder_yet() {
    let gpu = HeadlessRaster::new();
    let caps: &Caps = gpu.caps();
    assert!(!caps.compute_dispatch);

    // And a workload that would otherwise qualify is vetoed by exactly that fact.
    let workload = VectorWorkload {
        compute_available: caps.compute_dispatch,
        ..a_vector_editor_mid_drag()
    };
    assert_eq!(VectorLane::select(workload), VectorLane::CpuTessellate);
}

/// Twenty buttons, drawn for real: zero compute dispatches. The scene is
/// unremarkable on purpose — this is the case the hard rule exists to protect,
/// and the counter is how a later lane's regression becomes visible.
#[test]
fn a_screen_of_buttons_dispatches_nothing() {
    let (mut gpu, surface, mut r) = renderer();
    let mut scene = Vec::new();
    for i in 0..20 {
        scene.extend(button(i));
    }

    r.upload(&mut gpu, &scene);
    r.submit(&mut gpu, surface, [0.0; 4], [W as f32, H as f32]);

    let s = r.frame_stats();
    assert_eq!(s.compute_dispatches, 0, "twenty buttons must not dispatch");
    assert!(s.draw_calls > 0, "the scene really was drawn");
    assert!(
        s.path_tessellations > 0,
        "and its vector geometry really was tessellated on the CPU"
    );
}

/// Dozens of curved, filled, clipped paths — a chart or a map panel — still
/// dispatch nothing. Nesting them in a translucent layer adds the offscreen and
/// composite machinery, so the assertion covers a frame with real pass structure
/// rather than a single flat draw list. Half the blobs are outlined so *both* CPU
/// vector lanes are in the frame: the self-masked coverage lane a fill-only curve
/// takes (§14.4) and the tessellated lane a stroke forces.
#[test]
fn dozens_of_curved_paths_under_a_layer_dispatch_nothing() {
    let (mut gpu, surface, mut r) = renderer();
    let mut scene = vec![Primitive::Layer(LayerClip {
        clip: rect(0.0, 0.0, W as f32, H as f32),
        opacity: 0.6,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    })];
    for i in 0..48 {
        let cx = 16.0 + (i % 8) as f32 * 30.0;
        let cy = 16.0 + (i / 8) as f32 * 40.0;
        scene.push(blob(cx, cy, 12.0, i % 2 == 0));
    }
    scene.push(Primitive::LayerEnd);

    r.upload(&mut gpu, &scene);
    r.submit(&mut gpu, surface, [0.0; 4], [W as f32, H as f32]);

    let s = r.frame_stats();
    assert_eq!(s.compute_dispatches, 0);
    assert!(s.offscreen_passes > 0, "the layer really opened a pass");
    assert!(
        s.path_tessellations > 0,
        "the outlined curves took the tessellated lane"
    );
    assert!(
        s.clip_mask_builds > 0,
        "and the fill-only curves took the coverage-mask lane"
    );
}

/// Repeated frames do not accumulate a dispatch either: the counter is zero on
/// the cold frame, on a frame that changed one path, and on a frame that changed
/// nothing, so there is no "warm-up dispatch" hiding behind a steady state.
#[test]
fn the_dispatch_count_stays_zero_across_frames() {
    let (mut gpu, surface, mut r) = renderer();
    let mut scene: Vec<Primitive> = (0..8).flat_map(button).collect();

    for pass in 0..3 {
        if pass == 1 {
            // A local change: one path moves.
            scene.push(blob(200.0, 200.0, 20.0, false));
        }
        r.upload(&mut gpu, &scene);
        r.submit(&mut gpu, surface, [0.0; 4], [W as f32, H as f32]);
        assert_eq!(
            r.frame_stats().compute_dispatches,
            0,
            "frame {pass} must not dispatch"
        );
    }
}
