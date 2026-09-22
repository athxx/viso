//! Culling contract (§20, §24.1): who decides what is visible, and what it costs.
//!
//! §20 allows escalating from a per-primitive CPU bounds test to chunk-level CPU
//! rejection and then to a GPU cull feeding indirect draws, and states the rule that
//! matters more than any of them: **50 UI nodes must never produce a compute
//! dispatch for "GPU-driven"**. Culling is an optimization for scenes far larger
//! than their viewport, not an architecture every frame pays into.
//!
//! Pinned from both ends, like the §20.1 vector lane:
//!
//! 1. **The policy.** [`CullPlan::select`] is conjunctive at each escalation, so an
//!    incompletely described scene lands on the plain bounds test, and the GPU plan
//!    additionally needs a capability no backend here reports.
//! 2. **The frame.** Real scenes drawn through the real renderer report zero compute
//!    dispatches and zero indirect draws, while still culling correctly — the
//!    arithmetic form of "ordinary UI decides its own visibility on the CPU".
//!
//! The chunked plan's own correctness — that it returns exactly the per-primitive
//! walk's answer — is pinned in the module's unit tests, where the reference walk
//! lives. Here the concern is which plan a scene gets and what a frame reports.
//!
//! Thresholds are deliberately not asserted as values (§20 fixes no ABI); what is
//! pinned is which way each condition points.

use viso_gpu::{Caps, GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureFormat};
use viso_render::{
    Border, ChunkedCull, CullPlan, CullWorkload, LayerClip, Primitive, Quad, Rect, Renderer, Rgba,
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

/// One UI node's worth of paint: a rounded rect, which is what most of a screen is.
fn node(i: usize) -> Primitive {
    Primitive::Quad(Quad {
        rect: rect(
            4.0 + (i % 10) as f32 * 25.0,
            4.0 + (i / 10) as f32 * 25.0,
            20.0,
            20.0,
        ),
        color: Rgba::new(0.3, 0.5, 0.7, 1.0),
        radius: 4.0,
        border: Border::NONE,
    })
}

/// A clip that forces the renderer's own per-primitive cull to run.
fn clip(r: Rect) -> Primitive {
    Primitive::Layer(LayerClip {
        clip: r,
        opacity: 0.5,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    })
}

// ---------------------------------------------------------------------------
// 1. The policy: escalation is by scene shape, capability last
// ---------------------------------------------------------------------------

/// The scene §24.1 names GPU culling for, on a backend with both halves of the
/// capability and with culling profiled as a real share of the frame.
fn a_zoomed_out_graph() -> CullWorkload {
    CullWorkload {
        indirect_draw: true,
        compute_dispatch: true,
        primitives: 800_000,
        offscreen_share: 0.98,
        cull_share: 0.35,
    }
}

#[test]
fn the_plan_ordinary_ui_uses_is_the_default_one() {
    assert_eq!(CullPlan::default(), CullPlan::PerPrimitive);
    assert_eq!(
        CullPlan::select(CullWorkload::default()),
        CullPlan::PerPrimitive
    );
    assert!(!CullPlan::default().dispatches_compute());
}

/// The escalation that actually ships needs no GPU feature at all: a large canvas
/// scrolled to show a corner of itself gets chunk-level CPU rejection on every
/// backend.
#[test]
fn a_large_canvas_escalates_on_the_cpu() {
    let scrolled_map = CullWorkload {
        primitives: 250_000,
        offscreen_share: 0.97,
        ..CullWorkload::default()
    };
    assert_eq!(CullPlan::select(scrolled_map), CullPlan::Chunked);
    assert!(!CullPlan::Chunked.dispatches_compute());
}

#[test]
fn a_huge_offscreen_scene_is_what_gpu_culling_is_for() {
    assert_eq!(
        CullPlan::select(a_zoomed_out_graph()),
        CullPlan::GpuIndirect
    );
}

/// Scale alone is never evidence: an enormous, almost entirely offscreen scene that
/// nobody profiled still culls on the CPU, because `cull_share` defaults to zero and
/// "not measured" and "not the bottleneck" are the same input (§7.3).
#[test]
fn an_unprofiled_frame_cannot_reach_the_gpu_plan() {
    let huge = CullWorkload {
        cull_share: 0.0,
        ..a_zoomed_out_graph()
    };
    assert!(!huge.culling_is_the_bottleneck());
    assert!(huge.scene_is_huge());
    assert_eq!(
        CullPlan::select(huge),
        CullPlan::Chunked,
        "an unmeasured frame gets the plan that needs no evidence"
    );
}

/// A scene that is large but entirely visible is fill-bound: every primitive has to
/// be submitted, so no plan has anything to reject and none is selected.
#[test]
fn a_fully_visible_scene_is_not_a_culling_problem() {
    let visible = CullWorkload {
        offscreen_share: 0.0,
        ..a_zoomed_out_graph()
    };
    assert_eq!(CullPlan::select(visible), CullPlan::PerPrimitive);
}

/// §20's hard rule over the scene sizes it names, under the most favorable
/// conditions a backend could report: everything off screen, culling measured as the
/// entire frame, both capabilities present. Scale is the only thing these lack, and
/// it is enough.
#[test]
fn fifty_ui_nodes_are_never_gpu_driven() {
    for (label, primitives) in [
        ("fifty UI nodes", 50),
        ("a window of controls", 400),
        ("a dense screen with text", 2_000),
    ] {
        let plan = CullPlan::select(CullWorkload {
            indirect_draw: true,
            compute_dispatch: true,
            primitives,
            offscreen_share: 1.0,
            cull_share: 1.0,
        });
        assert_eq!(
            plan,
            CullPlan::PerPrimitive,
            "{label} must stay on the plain bounds test"
        );
        assert!(!plan.dispatches_compute());
    }
}

// ---------------------------------------------------------------------------
// 2. The capability is reported honestly
// ---------------------------------------------------------------------------

/// Neither half of GPU-driven rendering exists here, so nothing above may plan it.
/// Both are vetoes, and the scene that would otherwise qualify is vetoed by exactly
/// this fact.
#[test]
fn no_backend_offers_indirect_draw_yet() {
    let gpu = HeadlessRaster::new();
    let caps: &Caps = gpu.caps();
    assert!(!caps.indirect_draw);
    assert!(!caps.compute_dispatch);

    let workload = CullWorkload {
        indirect_draw: caps.indirect_draw,
        compute_dispatch: caps.compute_dispatch,
        ..a_zoomed_out_graph()
    };
    assert!(!workload.gpu_driven_is_available());
    assert_eq!(
        CullPlan::select(workload),
        CullPlan::Chunked,
        "the scene still culls, just on the CPU"
    );
}

// ---------------------------------------------------------------------------
// 3. The frame: real scenes decide visibility on the CPU
// ---------------------------------------------------------------------------

/// Fifty nodes, drawn for real: no dispatch, no indirect draw. The scene is
/// unremarkable on purpose — this is the case the hard rule protects, and these two
/// counters are how a later plan's regression becomes visible without review.
#[test]
fn a_screen_of_fifty_nodes_is_not_gpu_driven() {
    let (mut gpu, surface, mut r) = renderer();
    let scene: Vec<Primitive> = (0..50).map(node).collect();

    r.upload(&mut gpu, &scene);
    r.submit(&mut gpu, surface, [0.0; 4], [W as f32, H as f32]);

    let s = r.frame_stats();
    assert_eq!(s.compute_dispatches, 0, "fifty nodes must not dispatch");
    assert_eq!(s.indirect_draws, 0, "and must not draw indirectly");
    assert!(s.draw_calls > 0, "the scene really was drawn");
}

/// A frame that really does cull — a tight clip with most of its subtree outside —
/// still reports neither counter: the rejection happened in the CPU walk, which is
/// the plan every ordinary frame is on.
#[test]
fn a_frame_that_culls_still_culls_on_the_cpu() {
    let (mut gpu, surface, mut r) = renderer();
    let mut scene = vec![clip(rect(0.0, 0.0, 30.0, 30.0))];
    scene.extend((0..40).map(node));
    scene.push(Primitive::LayerEnd);

    r.upload(&mut gpu, &scene);
    r.submit(&mut gpu, surface, [0.0; 4], [W as f32, H as f32]);

    let s = r.frame_stats();
    assert!(
        s.culled_primitives > 0,
        "the clip really did reject content"
    );
    assert_eq!(s.compute_dispatches, 0);
    assert_eq!(s.indirect_draws, 0);
}

/// Three consecutive frames with a local change: no plan warms up into GPU-driven
/// mode. A dispatch that appeared only on a later frame would be the same violation,
/// just harder to see.
#[test]
fn a_steady_frame_never_warms_up_into_a_dispatch() {
    let (mut gpu, surface, mut r) = renderer();
    let mut scene: Vec<Primitive> = (0..50).map(node).collect();

    for i in 0..3 {
        if let Primitive::Quad(q) = &mut scene[7] {
            q.color = Rgba::new(0.1 * i as f32, 0.2, 0.9, 1.0);
        }
        r.upload(&mut gpu, &scene);
        r.submit(&mut gpu, surface, [0.0; 4], [W as f32, H as f32]);
        let s = r.frame_stats();
        assert_eq!(s.compute_dispatches, 0, "frame {i}");
        assert_eq!(s.indirect_draws, 0, "frame {i}");
    }
}

// ---------------------------------------------------------------------------
// 4. The chunked plan is usable from outside the crate
// ---------------------------------------------------------------------------

/// The chunk index is public because a host drawing a large canvas is the caller who
/// knows its scene's bounds. Used the way such a host would — build once over the
/// scene, query per viewport — it returns paint-ordered indices and tests far fewer
/// primitives than the scene holds.
#[test]
fn a_host_can_cull_its_own_large_scene() {
    let bounds: Vec<Rect> = (0..40_000)
        .map(|i| rect((i % 200) as f32 * 30.0, (i / 200) as f32 * 30.0, 24.0, 24.0))
        .collect();
    let mut index = ChunkedCull::default();
    index.build(&bounds);

    let mut visible = Vec::new();
    let outcome = index.cull(&bounds, rect(0.0, 0.0, W as f32, H as f32), &mut visible);

    assert!(!visible.is_empty(), "the viewport does show some of it");
    assert!(
        visible.windows(2).all(|w| w[0] < w[1]),
        "the visible set stays in paint order, so z-order survives culling"
    );
    assert!(
        outcome.chunks_rejected > 0 && outcome.primitives_tested * 4 < bounds.len() as u32,
        "a viewport this small must not cost a walk of the whole scene: tested {} of {}",
        outcome.primitives_tested,
        bounds.len()
    );
}
