//! Frame phases (architecture §11.2).
//!
//! A frame proceeds through these phases in order. Implementations may merge
//! adjacent phases for performance, but the *semantics* of each phase must
//! remain distinguishable. Each phase is paired with a purpose-specific
//! context (§11.3) so that, e.g., a layout function cannot accidentally
//! launch a network request or submit arbitrary GPU work.

/// The ordered phases of a single frame.
///
/// The discriminant order is the execution order and is part of the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FramePhase {
    /// 1. Drain normalized input from the platform layer.
    CollectInput,
    /// 2. Route input to focus/hit-test targets (capture → target → bubble).
    DispatchInput,
    /// 3. Flush batched state writes from this input transaction exactly once.
    FlushStateTransactions,
    /// 4. Resolve style/theme deltas for dirty nodes only.
    ResolveStyle,
    /// 5. Measure content (using cached results where constraints are stable).
    Measure,
    /// 6. Position nodes.
    Layout,
    /// 7. Incrementally update accessibility semantics.
    UpdateSemantics,
    /// 8. Produce paint/primitive deltas for changed nodes.
    BuildPaintChanges,
    /// 9. Bucket primitives into batches (compact IDs, respects z-order).
    BuildRenderBatches,
    /// 10. Upload changed GPU data via ring/persistent buffers.
    UploadGpuChanges,
    /// 11. Submit to the backend.
    Submit,
    /// 12. Post-frame cleanup / recycle.
    PostFrameCleanup,
}

impl FramePhase {
    /// All phases in execution order.
    pub const ORDER: [FramePhase; 12] = [
        FramePhase::CollectInput,
        FramePhase::DispatchInput,
        FramePhase::FlushStateTransactions,
        FramePhase::ResolveStyle,
        FramePhase::Measure,
        FramePhase::Layout,
        FramePhase::UpdateSemantics,
        FramePhase::BuildPaintChanges,
        FramePhase::BuildRenderBatches,
        FramePhase::UploadGpuChanges,
        FramePhase::Submit,
        FramePhase::PostFrameCleanup,
    ];
}
