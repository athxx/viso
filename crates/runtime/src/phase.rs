//! Frame phases.
//!
//! A frame proceeds through these phases in order. Implementations may merge
//! adjacent phases for performance, but the *semantics* of each phase must
//! remain distinguishable. Each phase is paired with a purpose-specific
//! context so that, e.g., a layout function cannot accidentally
//! launch a network request or submit arbitrary GPU work.

/// The ordered phases of a single frame.
///
/// The discriminant order is the execution order and is part of the contract.
/// Rules:
///
/// - every write of phases 2–5 joins one transaction, flushed once in 6;
/// - an empty phase costs O(1), decided by a dirty worklist, never a walk;
/// - an effect runs in 15, after `Submit`, and its writes are the next frame's;
/// - semantics publish in 16, off the input-to-photon path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FramePhase {
    /// 1. Drain normalized input from the platform layer.
    CollectInput,
    /// 2. Route input to focus/hit-test targets (capture → target → bubble).
    DispatchInput,
    /// 3. Run continuations: finished tasks, due timers, async completions.
    RunContinuations,
    /// 4. Commit the staged dev patch bundle at the frame boundary. Dev builds
    ///    only; a Release driver never sees work here.
    ApplyDevPatches,
    /// 5. Advance animation tracks, transitions and frame clients.
    AdvanceAnimations,
    /// 6. Flush the frame's one transaction: derivations, bindings, structure.
    FlushStateTransactions,
    /// 7. Resolve style/theme deltas for dirty nodes only.
    ResolveStyle,
    /// 8. Measure content (using cached results where constraints are stable).
    Measure,
    /// 9. Position nodes.
    Layout,
    /// 10. Propagate world transforms, update the transform buffer and the
    ///     visible set.
    UpdateTransforms,
    /// 11. Produce paint/primitive deltas for changed nodes.
    BuildPaintChanges,
    /// 12. Bucket primitives into batches and compute the damage region.
    BuildRenderBatches,
    /// 13. Upload changed GPU data (or rasterize damaged tiles on the CPU).
    UploadGpuChanges,
    /// 14. Present, with damage.
    Submit,
    /// 15. Run the effects the flush queued; their writes are the next frame's.
    RunEffects,
    /// 16. Publish accessibility semantics.
    UpdateSemantics,
    /// 17. Post-frame cleanup / recycle.
    PostFrameCleanup,
}

impl FramePhase {
    /// All phases in execution order.
    pub const ORDER: [FramePhase; 17] = [
        FramePhase::CollectInput,
        FramePhase::DispatchInput,
        FramePhase::RunContinuations,
        FramePhase::ApplyDevPatches,
        FramePhase::AdvanceAnimations,
        FramePhase::FlushStateTransactions,
        FramePhase::ResolveStyle,
        FramePhase::Measure,
        FramePhase::Layout,
        FramePhase::UpdateTransforms,
        FramePhase::BuildPaintChanges,
        FramePhase::BuildRenderBatches,
        FramePhase::UploadGpuChanges,
        FramePhase::Submit,
        FramePhase::RunEffects,
        FramePhase::UpdateSemantics,
        FramePhase::PostFrameCleanup,
    ];
}

#[cfg(test)]
mod tests {
    use super::FramePhase;

    #[test]
    fn order_is_discriminant_order() {
        for (i, phase) in FramePhase::ORDER.iter().enumerate() {
            assert_eq!(*phase as usize, i);
        }
    }

    #[test]
    fn effects_and_semantics_follow_submit() {
        let at = |p| FramePhase::ORDER.iter().position(|&q| q == p).unwrap();
        assert!(at(FramePhase::Submit) < at(FramePhase::RunEffects));
        assert!(at(FramePhase::RunEffects) < at(FramePhase::UpdateSemantics));
        assert!(at(FramePhase::AdvanceAnimations) < at(FramePhase::FlushStateTransactions));
    }
}
