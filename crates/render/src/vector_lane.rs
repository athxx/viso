//! Which lane rasterizes vector geometry (§20.1).
//!
//! Viso has one vector lane today: the CPU flattens and tessellates an outline,
//! and the GPU draws the resulting mesh. §20.1 describes a second one — bin the
//! path segments into tiles on the GPU, prefix-sum the per-tile allocation,
//! accumulate coverage per tile, then fine-raster — and it is genuinely faster
//! for one shape of workload: a large, *churning* segment population where the
//! CPU tessellator, not the GPU, is the frame's limit.
//!
//! The load-bearing part of §20.1 is not that pipeline, though. It is the rule
//! that the compute lane is **workload specialization**: twenty ordinary
//! buttons, a panel, or a few dozen stable paths must never be routed through
//! compute dispatch for the sake of architectural uniformity. A dispatch is not
//! free — it is a barrier, an allocation pass, and a round trip whose fixed cost
//! a small scene can never amortize — so an unconditional "GPU-driven" vector
//! path would make the common case slower while looking more advanced.
//!
//! So this module is the decision, not the kernel: [`VectorLane::select`] answers
//! which lane a workload belongs in, and the answer for anything resembling
//! ordinary UI is [`VectorLane::CpuTessellate`]. Three properties make that a
//! rule rather than a hope:
//!
//! 1. **A measurement is required.** [`VectorWorkload::tessellation_share`] is a
//!    measured fraction of the frame budget, and it defaults to `0.0`. A caller
//!    that has not profiled cannot reach the compute lane at all — §7.3 as a
//!    type, not as a comment.
//! 2. **Scale is required.** Below [`LARGE_SEGMENT_COUNT`] segments the answer is
//!    the CPU lane even when the measurement says tessellation dominates, because
//!    at that size the fix is the tessellator, not a dispatch.
//! 3. **Churn is required.** A large but *stable* path population belongs in
//!    retained cached geometry (§33's "general stable path can be retained cached
//!    geometry"), which costs nothing per frame. Re-binning it on the GPU every
//!    frame would be work that the CPU lane had already stopped doing.
//!
//! The thresholds below are internal benchmark parameters, not public ABI
//! (§20.1): they are the crossover of one tessellator against one dispatch cost,
//! and both move. What is stable is the *shape* of the policy — measured
//! bottleneck AND scale AND churn, with capability as a veto.

/// Which lane turns vector outlines into pixels (§20.1).
///
/// Not a quality ranking: the two lanes produce the same coverage, and differ
/// only in where the work happens and what fixed cost it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorLane {
    /// The CPU flattens and tessellates the outline; the GPU draws the mesh.
    ///
    /// The default and the overwhelmingly common answer. Its cost is proportional
    /// to the segments that actually changed — a stable path is tessellated once
    /// and its mesh retained — and it adds no dispatch, no barrier, and no
    /// intermediate buffer to a frame.
    #[default]
    CpuTessellate,
    /// The GPU bins segments into tiles, allocates per-tile work with a parallel
    /// prefix sum, accumulates coverage, and fine-rasters (§20.1).
    ///
    /// Wins only where the CPU tessellator is the measured frame limit over a
    /// large, churning segment population: a vector editor dragging control
    /// points, a canvas whose contents are regenerated per frame. Its fixed cost
    /// (dispatches, barriers, an allocation pass) is exactly what makes it the
    /// wrong answer for ordinary UI.
    GpuCompute,
}

/// Total segment count below which the compute lane is never selected, however
/// slow tessellation is measured to be.
///
/// A scene this small that spends a quarter of its frame tessellating has a
/// tessellator problem, and moving the same work to a dispatch would hide it
/// behind a larger fixed cost. Internal benchmark parameter (§20.1).
const LARGE_SEGMENT_COUNT: u32 = 50_000;

/// Segments re-tessellated per frame above which a workload counts as churning.
///
/// Below this a large scene is *stable*, and stable geometry belongs in the
/// retained mesh cache, where its per-frame cost is already zero. Internal
/// benchmark parameter (§20.1).
const CHURNING_SEGMENTS_PER_FRAME: u32 = 10_000;

/// Clip/composite operations per frame that make a large workload count as
/// churning even when its segments are stable — the "frequent
/// clipping/compositing" entry condition in §20.1, where the per-tile coverage
/// the compute lane already computes is what the clips need anyway.
const HEAVY_CLIP_COMPOSITE_OPS: u32 = 1_000;

/// Measured share of the frame budget spent in CPU tessellation at which it
/// counts as the bottleneck.
///
/// A quarter of the frame is the point past which no other optimization in the
/// frame can pay for itself while tessellation stands still. Internal benchmark
/// parameter (§20.1); the *requirement* that it be measured is not (§7.3).
const TESSELLATION_BOTTLENECK_SHARE: f32 = 0.25;

/// What a frame's vector workload looks like, as the entry conditions in §20.1
/// name it.
///
/// Every field is a property of the *workload*, not of a backend or a platform,
/// except the one capability veto — so the decision is portable and a backend
/// without compute simply never offers the lane.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct VectorWorkload {
    /// The backend can dispatch compute kernels at all. `false` forces the CPU
    /// lane regardless of everything else, and it is `false` on every backend
    /// whose RHI has no dispatch entry point.
    pub compute_available: bool,
    /// Total path segments in the scene's vector geometry, after curves have been
    /// lowered to canonical segments.
    pub segments: u32,
    /// Segments re-tessellated this frame: the churn. Zero for a scene whose
    /// paths are all retained from an earlier frame.
    pub remeshed_segments_per_frame: u32,
    /// Clip and composite operations this frame — the §20.1 "frequent
    /// clipping/compositing" condition.
    pub clip_composite_ops: u32,
    /// Measured fraction of the frame budget CPU tessellation took, in `[0, 1]`.
    ///
    /// `0.0` means *not measured*, which is the default and which pins the answer
    /// to the CPU lane: §7.3 forbids acting on an unmeasured performance claim,
    /// and this is where that rule is enforced rather than reviewed.
    pub tessellation_share: f32,
}

impl VectorWorkload {
    /// Whether CPU tessellation has been *measured* to dominate the frame.
    ///
    /// Separate from the rest of the policy because it is the one input that
    /// cannot be inferred from the scene: a caller either profiled the frame or
    /// did not.
    pub fn tessellation_is_the_bottleneck(&self) -> bool {
        self.tessellation_share >= TESSELLATION_BOTTLENECK_SHARE
    }

    /// Whether the workload is large *and* moving — the two scene-shape
    /// conditions the compute lane needs to amortize its fixed cost.
    ///
    /// Large and stable is deliberately excluded: that is the retained-geometry
    /// case, whose per-frame cost is already lower than any dispatch.
    pub fn is_large_and_dynamic(&self) -> bool {
        self.segments >= LARGE_SEGMENT_COUNT
            && (self.remeshed_segments_per_frame >= CHURNING_SEGMENTS_PER_FRAME
                || self.clip_composite_ops >= HEAVY_CLIP_COMPOSITE_OPS)
    }
}

impl VectorLane {
    /// Pick the lane for a workload (§20.1).
    ///
    /// Conjunctive by design: capability, a measured bottleneck, and a
    /// large-and-dynamic scene shape must *all* hold. Any one of them missing
    /// yields [`VectorLane::CpuTessellate`], so every way of being wrong about a
    /// workload degrades to the lane that is correct everywhere — which is the
    /// §7.2 requirement that ordinary UI keep working with zero compute
    /// dependency, expressed as the default branch of the decision rather than as
    /// a reviewer's promise.
    pub fn select(workload: VectorWorkload) -> VectorLane {
        if workload.compute_available
            && workload.tessellation_is_the_bottleneck()
            && workload.is_large_and_dynamic()
        {
            VectorLane::GpuCompute
        } else {
            VectorLane::CpuTessellate
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vector editor mid-drag: a large segment population, re-tessellated every
    /// frame, with tessellation measured as the frame's limit on a backend that
    /// can dispatch. This is the one shape §20.1 describes the lane for.
    fn vector_editor_mid_drag() -> VectorWorkload {
        VectorWorkload {
            compute_available: true,
            segments: 400_000,
            remeshed_segments_per_frame: 120_000,
            clip_composite_ops: 12,
            tessellation_share: 0.62,
        }
    }

    #[test]
    fn the_default_lane_is_cpu_tessellation() {
        assert_eq!(VectorLane::default(), VectorLane::CpuTessellate);
        assert_eq!(
            VectorLane::select(VectorWorkload::default()),
            VectorLane::CpuTessellate,
            "a workload nobody described must not reach a dispatch"
        );
    }

    #[test]
    fn a_churning_vector_editor_enters_the_compute_lane() {
        assert_eq!(
            VectorLane::select(vector_editor_mid_drag()),
            VectorLane::GpuCompute
        );
    }

    #[test]
    fn an_unmeasured_workload_never_enters_the_compute_lane() {
        // Same scene, no profile. §7.3: the share is the measurement, and without
        // it the answer cannot be "compute is faster".
        let unmeasured = VectorWorkload {
            tessellation_share: 0.0,
            ..vector_editor_mid_drag()
        };
        assert_eq!(
            VectorLane::select(unmeasured),
            VectorLane::CpuTessellate,
            "an unmeasured bottleneck is not a bottleneck"
        );
    }

    #[test]
    fn a_backend_without_compute_always_tessellates() {
        let no_compute = VectorWorkload {
            compute_available: false,
            ..vector_editor_mid_drag()
        };
        assert_eq!(VectorLane::select(no_compute), VectorLane::CpuTessellate);
    }

    #[test]
    fn a_large_but_stable_scene_stays_on_retained_geometry() {
        // A map or a chart drawn once and then only scrolled: the mesh is already
        // built, so the CPU lane's per-frame cost is zero and a dispatch would be
        // pure addition.
        let stable = VectorWorkload {
            remeshed_segments_per_frame: 0,
            clip_composite_ops: 4,
            ..vector_editor_mid_drag()
        };
        assert_eq!(VectorLane::select(stable), VectorLane::CpuTessellate);
        assert!(!stable.is_large_and_dynamic());
        assert!(
            stable.tessellation_is_the_bottleneck(),
            "the measurement still holds; scene shape is what rules the lane out"
        );
    }

    #[test]
    fn a_small_scene_is_fixed_in_the_tessellator_not_in_a_dispatch() {
        let small_but_slow = VectorWorkload {
            segments: 900,
            remeshed_segments_per_frame: 900,
            tessellation_share: 0.9,
            ..vector_editor_mid_drag()
        };
        assert_eq!(
            VectorLane::select(small_but_slow),
            VectorLane::CpuTessellate
        );
    }

    #[test]
    fn heavy_clipping_counts_as_churn_at_scale() {
        // §20.1's other entry condition: the segments are stable but the frame is
        // dominated by clip/composite work, which per-tile coverage answers
        // directly.
        let clip_heavy = VectorWorkload {
            remeshed_segments_per_frame: 0,
            clip_composite_ops: 4_000,
            ..vector_editor_mid_drag()
        };
        assert_eq!(VectorLane::select(clip_heavy), VectorLane::GpuCompute);
        // And it is still gated on scale: the same clip load over a small scene
        // stays on the CPU.
        let small = VectorWorkload {
            segments: 300,
            ..clip_heavy
        };
        assert_eq!(VectorLane::select(small), VectorLane::CpuTessellate);
    }

    /// The §20.1 hard rule, stated as arithmetic: no plausible ordinary-UI scene
    /// reaches the compute lane, on a backend that *can* dispatch, even with
    /// tessellation measured at the bottleneck. Nothing about the answer depends
    /// on a reviewer noticing.
    #[test]
    fn ordinary_ui_never_reaches_the_compute_lane() {
        // Twenty buttons: each an analytic rrect or a handful of segments, all
        // stable after the first frame.
        for (label, scene) in [
            (
                "twenty buttons",
                VectorWorkload {
                    segments: 20 * 8,
                    remeshed_segments_per_frame: 20 * 8,
                    clip_composite_ops: 2,
                    ..Default::default()
                },
            ),
            (
                "a panel with a few dozen stable paths",
                VectorWorkload {
                    segments: 48 * 60,
                    remeshed_segments_per_frame: 0,
                    clip_composite_ops: 6,
                    ..Default::default()
                },
            ),
            (
                "a dense settings page, every icon redrawn",
                VectorWorkload {
                    segments: 4_000,
                    remeshed_segments_per_frame: 4_000,
                    clip_composite_ops: 40,
                    ..Default::default()
                },
            ),
        ] {
            let with_compute_and_a_profile = VectorWorkload {
                compute_available: true,
                tessellation_share: 0.8,
                ..scene
            };
            assert_eq!(
                VectorLane::select(with_compute_and_a_profile),
                VectorLane::CpuTessellate,
                "{label} must not be routed through compute dispatch"
            );
        }
    }

    /// The scale floor is a floor, not a range: one segment below it is the CPU
    /// lane and the threshold itself is the compute lane, so the boundary cannot
    /// drift without this failing.
    #[test]
    fn the_scale_floor_is_exact() {
        let at = VectorWorkload {
            segments: LARGE_SEGMENT_COUNT,
            ..vector_editor_mid_drag()
        };
        let below = VectorWorkload {
            segments: LARGE_SEGMENT_COUNT - 1,
            ..vector_editor_mid_drag()
        };
        assert_eq!(VectorLane::select(at), VectorLane::GpuCompute);
        assert_eq!(VectorLane::select(below), VectorLane::CpuTessellate);
    }

    /// Likewise the churn floor and the measured-share floor.
    #[test]
    fn the_churn_and_measurement_floors_are_exact() {
        let at_churn = VectorWorkload {
            remeshed_segments_per_frame: CHURNING_SEGMENTS_PER_FRAME,
            clip_composite_ops: 0,
            ..vector_editor_mid_drag()
        };
        let below_churn = VectorWorkload {
            remeshed_segments_per_frame: CHURNING_SEGMENTS_PER_FRAME - 1,
            ..at_churn
        };
        assert_eq!(VectorLane::select(at_churn), VectorLane::GpuCompute);
        assert_eq!(VectorLane::select(below_churn), VectorLane::CpuTessellate);

        let at_share = VectorWorkload {
            tessellation_share: TESSELLATION_BOTTLENECK_SHARE,
            ..vector_editor_mid_drag()
        };
        let below_share = VectorWorkload {
            tessellation_share: TESSELLATION_BOTTLENECK_SHARE - 0.01,
            ..vector_editor_mid_drag()
        };
        assert_eq!(VectorLane::select(at_share), VectorLane::GpuCompute);
        assert_eq!(VectorLane::select(below_share), VectorLane::CpuTessellate);
    }
}
