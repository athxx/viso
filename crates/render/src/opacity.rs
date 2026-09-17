//! The opacity planner (§14.5): decide how a group's opacity is realized —
//! folded into its children for free, or paid for with an isolation layer — and
//! record *why* a layer was needed when one is.
//!
//! Two opacities are not the same thing:
//!
//! - **Primitive opacity** is a single drawable's own alpha. It multiplies
//!   straight into the drawable's premultiplied color and costs nothing extra:
//!   an [`EffectCost::Local`] draw stays local.
//! - **Group opacity** is a factor applied to a *subtree as a whole*. Applying it
//!   to each child independently is only correct when the children do not overlap
//!   — where two translucent children overlap, per-child opacity double-blends
//!   the overlap, but the group is meant to composite once and then fade. Only
//!   then does the group need to render to an isolation layer (an offscreen pass)
//!   and composite that layer back at the group opacity (§14.5, §3145).
//!
//! So the planner's job is to *avoid* the offscreen path whenever it is
//! semantically safe: a fully-opaque group is a no-op; a group whose children
//! provably do not overlap pushes its opacity into the children and stays local;
//! only an overlapping translucent group escalates to a layer. When overlap is
//! unknown, the planner keeps correctness and uses a layer (§14.5: "when unsure,
//! keep correctness and use a layer").
//!
//! This is a cold-path decision (§7.2), the same shape as the
//! [`clip`](crate::clip) planner: the plan is computed once when a group is
//! ingested — beside where its [`EffectCost`] is assigned — and carries *what to
//! do* plus, for a layer, the [`LayerReason`] the Effect Planner (§3145) records.
//! The renderer carries the plan out; this module does not itself allocate a
//! target.

use crate::EffectCost;

/// Why a potential offscreen layer exists (§3149). Every layer the renderer
/// creates is tagged with its reason so the Effect Planner can try to eliminate
/// it and the inspector can show *why* a group cost an offscreen pass (§62).
///
/// C0.4 introduces the enum and populates the [`GroupOpacity`](LayerReason::GroupOpacity)
/// case; the remaining reasons are the vocabulary the later effect lanes
/// (backdrop filters, advanced blend, snapshot cache) tag their layers with, so
/// the reason a layer carries is nameable from one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerReason {
    /// A group opacity whose children overlap, so per-child opacity would
    /// double-blend the overlap: the subtree renders to a layer composited back
    /// at the group's opacity. The only reason C0.4 produces.
    GroupOpacity,
    /// A group filter that samples neighbors (blur, displacement) — an image
    /// filter lane, not yet produced here.
    ImageFilter,
    /// A filter reading the backdrop behind the group (backdrop blur) — a
    /// backdrop lane, not yet produced here.
    BackdropFilter,
    /// A non-separable / destination-reading blend that the hardware blender
    /// cannot express in place — an advanced-blend lane, not yet produced here.
    AdvancedBlend,
    /// Explicit isolation requested for its own sake (an isolated blend group).
    Isolation,
    /// A clip whose coverage is complex enough to isolate — bridges to the
    /// [`mask`](crate::mask) lane.
    ComplexMask,
    /// A subtree snapshotted and reused across frames (a retained group cache).
    SnapshotCache,
    /// A boundary with a native material / platform layer beneath it.
    NativeMaterialBoundary,
}

impl LayerReason {
    /// The lowercase label for an inspector dump / overlay row.
    pub fn label(self) -> &'static str {
        match self {
            LayerReason::GroupOpacity => "group-opacity",
            LayerReason::ImageFilter => "image-filter",
            LayerReason::BackdropFilter => "backdrop-filter",
            LayerReason::AdvancedBlend => "advanced-blend",
            LayerReason::Isolation => "isolation",
            LayerReason::ComplexMask => "complex-mask",
            LayerReason::SnapshotCache => "snapshot-cache",
            LayerReason::NativeMaterialBoundary => "native-material-boundary",
        }
    }
}

/// What the planner knows about whether a group's children overlap — the fact
/// that decides between the free fold and an isolation layer (§14.5).
///
/// This is deliberately a three-state fact, not a bool: `Unknown` is distinct
/// from `Overlapping` because the safe response to both is a layer, but the
/// inspector and the later planner passes benefit from knowing an escalation was
/// a *proven* overlap versus a *conservative* fallback (§14.5's "when unsure,
/// keep correctness").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChildOverlap {
    /// The children provably do not overlap (disjoint bounds, or a single child):
    /// group opacity is equivalent to per-child opacity and folds in free.
    Disjoint,
    /// Two or more children overlap: per-child opacity would double-blend the
    /// overlap, so the group must isolate.
    Overlapping,
    /// Overlap could not be determined cheaply. Treated as `Overlapping` for
    /// correctness, but recorded distinctly.
    Unknown,
}

impl ChildOverlap {
    /// Whether the group can be composited correctly by folding its opacity into
    /// each child independently — true only when the children are provably
    /// disjoint. `Unknown` conservatively returns `false`.
    pub fn allows_fold(self) -> bool {
        matches!(self, ChildOverlap::Disjoint)
    }
}

/// How a group's opacity should be realized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OpacityPlan {
    /// The group is fully opaque (opacity `>= 1`): a no-op. Children draw as-is;
    /// no factor to push, no layer. [`EffectCost::Local`].
    Opaque,
    /// The group's opacity folds into its children — each child multiplies this
    /// factor into its own premultiplied color. Correct because the children do
    /// not overlap (or there is only one). Stays [`EffectCost::Local`]; carries
    /// the factor to push down.
    FoldIntoChildren {
        /// The group opacity in `[0, 1)` to multiply into each child.
        factor: f32,
    },
    /// The group must render to an isolation layer composited back at `opacity`:
    /// the children overlap (or overlap is unknown) and are translucent, so
    /// per-child opacity would double-blend. [`EffectCost::NeedsOffscreen`],
    /// tagged [`LayerReason::GroupOpacity`].
    IsolateLayer {
        /// The group opacity in `[0, 1)` applied when the layer is composited.
        opacity: f32,
        /// Why the layer exists — always [`LayerReason::GroupOpacity`] here.
        reason: LayerReason,
    },
}

impl OpacityPlan {
    /// The realization cost of this plan (§7.5): folding or a no-op stays
    /// [`Local`](EffectCost::Local); an isolation layer is
    /// [`NeedsOffscreen`](EffectCost::NeedsOffscreen).
    pub fn cost(&self) -> EffectCost {
        match self {
            OpacityPlan::Opaque | OpacityPlan::FoldIntoChildren { .. } => EffectCost::Local,
            OpacityPlan::IsolateLayer { .. } => EffectCost::NeedsOffscreen,
        }
    }

    /// Whether this plan needs an offscreen isolation pass.
    pub fn needs_offscreen(&self) -> bool {
        matches!(self, OpacityPlan::IsolateLayer { .. })
    }

    /// The layer reason, if this plan creates a layer.
    pub fn layer_reason(&self) -> Option<LayerReason> {
        match self {
            OpacityPlan::IsolateLayer { reason, .. } => Some(*reason),
            _ => None,
        }
    }
}

/// Plan how a group opacity is realized (§14.5).
///
/// `opacity` is the group factor; `overlap` is what is known about the children.
/// The ladder, cheapest first:
///
/// 1. A fully-opaque group (`opacity >= 1`) is a no-op — [`OpacityPlan::Opaque`].
/// 2. A group whose children are provably [`Disjoint`](ChildOverlap::Disjoint)
///    folds its opacity into them — [`OpacityPlan::FoldIntoChildren`], local.
/// 3. Otherwise (overlapping, or overlap [`Unknown`](ChildOverlap::Unknown)) the
///    group isolates — [`OpacityPlan::IsolateLayer`] tagged
///    [`LayerReason::GroupOpacity`]. Choosing the layer for `Unknown` is the
///    "when unsure, keep correctness" rule (§14.5).
///
/// `opacity` is clamped to `[0, 1]`; a value `<= 0` is treated as a fully
/// transparent fold factor (the group contributes nothing) rather than a layer,
/// since an empty contribution never double-blends.
pub fn plan_group_opacity(opacity: f32, overlap: ChildOverlap) -> OpacityPlan {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity >= 1.0 {
        return OpacityPlan::Opaque;
    }
    // A fully-transparent group draws nothing; folding a zero factor into the
    // children is correct and cheaper than isolating an invisible layer.
    if opacity <= 0.0 || overlap.allows_fold() {
        return OpacityPlan::FoldIntoChildren { factor: opacity };
    }
    OpacityPlan::IsolateLayer {
        opacity,
        reason: LayerReason::GroupOpacity,
    }
}

/// Fold a group opacity factor into a child's own opacity. Both are ordinary
/// alpha in `[0, 1]`; the combined opacity is their product. This is what a
/// [`FoldIntoChildren`](OpacityPlan::FoldIntoChildren) plan applies to each
/// child, and it is the whole cost of the common (non-overlapping) group case —
/// a multiply, no target.
pub fn fold_child_opacity(group_factor: f32, child_opacity: f32) -> f32 {
    (group_factor.clamp(0.0, 1.0) * child_opacity.clamp(0.0, 1.0)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fully-opaque group is a no-op regardless of overlap: nothing to fold, no
    /// layer, local cost.
    #[test]
    fn opaque_group_is_a_noop() {
        for overlap in [
            ChildOverlap::Disjoint,
            ChildOverlap::Overlapping,
            ChildOverlap::Unknown,
        ] {
            let plan = plan_group_opacity(1.0, overlap);
            assert_eq!(plan, OpacityPlan::Opaque);
            assert_eq!(plan.cost(), EffectCost::Local);
            assert!(!plan.needs_offscreen());
            assert_eq!(plan.layer_reason(), None);
        }
        // Over-unity clamps to opaque.
        assert_eq!(
            plan_group_opacity(2.0, ChildOverlap::Overlapping),
            OpacityPlan::Opaque
        );
    }

    /// A translucent group with disjoint children folds into the children and
    /// stays local — no offscreen pass.
    #[test]
    fn disjoint_children_fold_and_stay_local() {
        let plan = plan_group_opacity(0.5, ChildOverlap::Disjoint);
        assert_eq!(plan, OpacityPlan::FoldIntoChildren { factor: 0.5 });
        assert_eq!(plan.cost(), EffectCost::Local);
        assert!(!plan.needs_offscreen());
        assert_eq!(plan.layer_reason(), None);
    }

    /// Overlapping translucent children escalate to an isolation layer tagged
    /// with the group-opacity reason.
    #[test]
    fn overlapping_children_isolate() {
        let plan = plan_group_opacity(0.5, ChildOverlap::Overlapping);
        assert_eq!(
            plan,
            OpacityPlan::IsolateLayer {
                opacity: 0.5,
                reason: LayerReason::GroupOpacity,
            }
        );
        assert_eq!(plan.cost(), EffectCost::NeedsOffscreen);
        assert!(plan.needs_offscreen());
        assert_eq!(plan.layer_reason(), Some(LayerReason::GroupOpacity));
    }

    /// Unknown overlap keeps correctness by isolating, exactly as overlapping
    /// does (§14.5: "when unsure, keep correctness and use a layer").
    #[test]
    fn unknown_overlap_isolates_for_correctness() {
        let plan = plan_group_opacity(0.3, ChildOverlap::Unknown);
        assert!(plan.needs_offscreen());
        assert_eq!(plan.layer_reason(), Some(LayerReason::GroupOpacity));
        assert!(!ChildOverlap::Unknown.allows_fold());
    }

    /// A fully-transparent group folds a zero factor rather than isolating an
    /// invisible layer, even when overlap is unknown — nothing can double-blend.
    #[test]
    fn transparent_group_folds_not_isolates() {
        let plan = plan_group_opacity(0.0, ChildOverlap::Overlapping);
        assert_eq!(plan, OpacityPlan::FoldIntoChildren { factor: 0.0 });
        assert!(!plan.needs_offscreen());
        // Negative clamps to the same transparent fold.
        assert_eq!(
            plan_group_opacity(-1.0, ChildOverlap::Unknown),
            OpacityPlan::FoldIntoChildren { factor: 0.0 }
        );
    }

    /// Folding a group factor into a child is a clamped product — the common
    /// non-overlapping case's whole cost.
    #[test]
    fn fold_is_a_clamped_product() {
        assert_eq!(fold_child_opacity(0.5, 0.5), 0.25);
        assert_eq!(fold_child_opacity(1.0, 0.4), 0.4);
        assert_eq!(fold_child_opacity(0.0, 0.9), 0.0);
        // Out-of-range inputs clamp before multiplying.
        assert_eq!(fold_child_opacity(2.0, 0.5), 0.5);
        assert_eq!(fold_child_opacity(0.5, -1.0), 0.0);
    }

    /// Only `Disjoint` allows the fold; the two escalating states do not.
    #[test]
    fn only_disjoint_allows_fold() {
        assert!(ChildOverlap::Disjoint.allows_fold());
        assert!(!ChildOverlap::Overlapping.allows_fold());
        assert!(!ChildOverlap::Unknown.allows_fold());
    }

    /// Every layer reason has a distinct, stable label.
    #[test]
    fn layer_reason_labels_are_distinct() {
        let all = [
            LayerReason::GroupOpacity,
            LayerReason::ImageFilter,
            LayerReason::BackdropFilter,
            LayerReason::AdvancedBlend,
            LayerReason::Isolation,
            LayerReason::ComplexMask,
            LayerReason::SnapshotCache,
            LayerReason::NativeMaterialBoundary,
        ];
        let labels: Vec<&str> = all.iter().map(|r| r.label()).collect();
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b, "labels are unique");
            }
        }
    }
}
