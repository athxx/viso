//! The Effect Planner (§3145): decide whether a potential layer becomes an
//! offscreen target, by trying to eliminate every reason it exists.
//!
//! "Offscreen is an expensive mechanism, not a convenient default" (§3145). A
//! `saveLayer`-shaped API invites the opposite habit: the author asks for a
//! layer, the renderer allocates one. This module inverts that. A layer arrives
//! as a [`LayerRequest`] — *what was asked for* — which the planner turns into a
//! set of [`LayerReason`]s, then walks the §3161 elimination ladder in order:
//!
//! 1. push the opacity into the children?
//! 2. fuse the color matrix?
//! 3. use a scissor instead of a clip layer?
//! 4. use an analytic shadow?
//! 5. share a backdrop capture?
//! 6. collapse adjacent effects?
//!
//! Only when every reason survives its elimination is an offscreen target
//! created. The output [`LayerPlan`] records all three sets — requested,
//! eliminated, surviving — so the inspector can answer *why* a group cost a pass,
//! and so a test can assert the elimination that fired rather than only its
//! effect on a counter (§62).
//!
//! Two kinds of elimination live in the same ladder, and the distinction matters
//! when reading a plan:
//!
//! - An elimination that removes a **reason** removes the layer itself when it is
//!   the last one standing: [`OpacityPushedIntoChildren`], [`FusedColorMatrix`],
//!   and [`SharedBackdrop`] — a shared capture hands the group a texture it can
//!   sample in place, so the backdrop filter stops needing a target of its own.
//! - An elimination that removes **passes** leaves the layer but makes it
//!   cheaper: [`CollapsedAdjacentEffects`] (an N-effect chain costs one op, E2.2),
//!   [`AnalyticShadow`] (a shadow shaded in closed form never asked for a blur
//!   layer, E0), [`ScissorInsteadOfClipLayer`] (the clip rides the pass's
//!   scissor, C0).
//!
//! The planner is a pure function on facts the renderer has already gathered — a
//! cold-path decision taken once per layer per frame (§7.2), no allocation, no
//! target claimed here. The renderer carries the plan out.
//!
//! [`OpacityPushedIntoChildren`]: LayerElimination::OpacityPushedIntoChildren
//! [`FusedColorMatrix`]: LayerElimination::FusedColorMatrix
//! [`SharedBackdrop`]: LayerElimination::SharedBackdrop
//! [`CollapsedAdjacentEffects`]: LayerElimination::CollapsedAdjacentEffects
//! [`AnalyticShadow`]: LayerElimination::AnalyticShadow
//! [`ScissorInsteadOfClipLayer`]: LayerElimination::ScissorInsteadOfClipLayer

use crate::opacity::{ChildOverlap, LayerReason, OpacityPlan, plan_group_opacity};
use crate::{EffectCost, EffectLocality};

/// One rung of the §3161 elimination ladder — an attempt the planner makes to
/// avoid an offscreen target, in the order the specification lists them.
///
/// The discriminant order *is* the trial order, so an [`EliminationSet`] prints
/// and iterates in the order the planner tried its rungs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerElimination {
    /// The group's opacity was multiplied into each child instead of fading a
    /// composited layer — correct because the children provably do not overlap
    /// (§14.5). Removes [`LayerReason::GroupOpacity`].
    OpacityPushedIntoChildren = 0,
    /// The group's color chain merged into a single matrix (§3178). When the
    /// merged chain computes nothing, it removes [`LayerReason::ImageFilter`]
    /// outright; otherwise it is the pass-saving half of the same fusion.
    FusedColorMatrix = 1,
    /// The group's clip was realized as the pass's scissor rectangle rather than
    /// as a clip layer — the C0 default, recorded whenever a clipped group ends
    /// up staying in its parent's pass.
    ScissorInsteadOfClipLayer = 2,
    /// A shadow in the group is shaded in closed form (E0's analytic soft shadow)
    /// rather than by blurring a rendered copy of the shape, so it never asked for
    /// an image-filter layer.
    AnalyticShadow = 3,
    /// The group's backdrop capture joined an existing capture group instead of
    /// opening its own (§17.1, E2.1): one snapshot pass serves every member.
    SharedBackdrop = 4,
    /// Two or more adjacent effects on this group collapsed into one op (§3178,
    /// E2.2): an N-effect chain costs one op instead of N passes.
    CollapsedAdjacentEffects = 5,
}

impl LayerElimination {
    /// Every rung, in §3161 trial order.
    pub const ALL: [LayerElimination; 6] = [
        LayerElimination::OpacityPushedIntoChildren,
        LayerElimination::FusedColorMatrix,
        LayerElimination::ScissorInsteadOfClipLayer,
        LayerElimination::AnalyticShadow,
        LayerElimination::SharedBackdrop,
        LayerElimination::CollapsedAdjacentEffects,
    ];

    /// This rung's bit position in an [`EliminationSet`].
    pub fn index(self) -> u32 {
        self as u32
    }

    /// The lowercase label for an inspector dump / overlay row.
    pub fn label(self) -> &'static str {
        match self {
            LayerElimination::OpacityPushedIntoChildren => "opacity-pushed-into-children",
            LayerElimination::FusedColorMatrix => "fused-color-matrix",
            LayerElimination::ScissorInsteadOfClipLayer => "scissor-instead-of-clip-layer",
            LayerElimination::AnalyticShadow => "analytic-shadow",
            LayerElimination::SharedBackdrop => "shared-backdrop",
            LayerElimination::CollapsedAdjacentEffects => "collapsed-adjacent-effects",
        }
    }
}

/// A set of [`LayerReason`]s as one byte — the eight reasons fit a `u8` exactly,
/// so a plan carries its three sets in three bytes with no allocation and no
/// per-layer `Vec` (§28).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ReasonSet(u8);

impl ReasonSet {
    /// The empty set: no reason to isolate — the layer disappears.
    pub const EMPTY: ReasonSet = ReasonSet(0);

    /// A set holding exactly one reason.
    pub fn of(reason: LayerReason) -> ReasonSet {
        ReasonSet(1 << reason.index())
    }

    /// Add a reason, keeping the rest.
    pub fn insert(&mut self, reason: LayerReason) {
        self.0 |= 1 << reason.index();
    }

    /// Remove a reason (an elimination succeeded).
    pub fn remove(&mut self, reason: LayerReason) {
        self.0 &= !(1 << reason.index());
    }

    /// Whether the set holds this reason.
    pub fn contains(self, reason: LayerReason) -> bool {
        self.0 & (1 << reason.index()) != 0
    }

    /// Whether the set holds nothing.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// How many reasons the set holds.
    pub fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// The reasons in the set, in [`LayerReason::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = LayerReason> {
        LayerReason::ALL
            .into_iter()
            .filter(move |r| self.contains(*r))
    }

    /// The raw bits — for a stable inspector dump / a test that pins a set.
    pub fn bits(self) -> u8 {
        self.0
    }
}

/// A set of [`LayerElimination`]s as one byte: which rungs of the §3161 ladder
/// actually fired for a layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EliminationSet(u8);

impl EliminationSet {
    /// The empty set: no rung fired.
    pub const EMPTY: EliminationSet = EliminationSet(0);

    /// Record a rung that fired.
    pub fn insert(&mut self, elimination: LayerElimination) {
        self.0 |= 1 << elimination.index();
    }

    /// Withdraw a rung that turned out not to eliminate anything.
    pub fn remove(&mut self, elimination: LayerElimination) {
        self.0 &= !(1 << elimination.index());
    }

    /// Whether this rung fired.
    pub fn contains(self, elimination: LayerElimination) -> bool {
        self.0 & (1 << elimination.index()) != 0
    }

    /// Whether no rung fired.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// How many rungs fired.
    pub fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// The rungs that fired, in §3161 trial order.
    pub fn iter(self) -> impl Iterator<Item = LayerElimination> {
        LayerElimination::ALL
            .into_iter()
            .filter(move |e| self.contains(*e))
    }

    /// The raw bits — for a stable inspector dump / a test that pins a set.
    pub fn bits(self) -> u8 {
        self.0
    }
}

/// What a potential layer asked for, in terms the planner can reason about.
///
/// Every field is a *fact already established* by an earlier lane — the planner
/// classifies and eliminates, it does not measure. The renderer fills this in at
/// the layer-open site from the authored [`LayerClip`](crate::LayerClip), the
/// fused color chain (E2.2), the blend marker (E2.3), the backdrop group it
/// joined (E2.1), and a look-ahead over the layer's own subtree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerRequest {
    /// The group opacity in `[0, 1]`. `1.0` asks for nothing.
    pub opacity: f32,
    /// What is known about whether the group's children overlap — the fact that
    /// decides whether the opacity can be pushed down (§14.5).
    pub overlap: ChildOverlap,
    /// The group's content is blurred by an effective sigma the ladder will
    /// actually realize (a sub-pixel sigma is not a blur, §16.2).
    pub blurs_content: bool,
    /// The group filters the pixels behind it (backdrop blur, §17.1).
    pub filters_backdrop: bool,
    /// The group composites with a blend the fixed-function stage cannot express,
    /// so it must read what it draws over (§14.6).
    pub advanced_blend: bool,
    /// Color effects authored on this group, before fusion (§17.3).
    pub color_effects: u32,
    /// Color ops left *after* fusion. `0` means the chain computes nothing.
    pub fused_color_ops: u32,
    /// The group's clip needs coverage a scissor rectangle cannot express.
    pub complex_mask: bool,
    /// Isolation was requested for its own sake (an isolated blend group).
    pub isolated: bool,
    /// The group is snapshotted and reused across frames (a retained cache).
    pub snapshot_cached: bool,
    /// The group sits on a boundary with a native material / platform layer.
    pub native_material_boundary: bool,
    /// The group's backdrop capture joined an existing capture group rather than
    /// opening its own (E2.1).
    pub shared_backdrop: bool,
    /// The group contains a shadow shaded in closed form (E0), so no blur layer
    /// was requested for it.
    pub analytic_shadow: bool,
}

impl Default for LayerRequest {
    fn default() -> LayerRequest {
        LayerRequest {
            opacity: 1.0,
            overlap: ChildOverlap::Unknown,
            blurs_content: false,
            filters_backdrop: false,
            advanced_blend: false,
            color_effects: 0,
            fused_color_ops: 0,
            complex_mask: false,
            isolated: false,
            snapshot_cached: false,
            native_material_boundary: false,
            shared_backdrop: false,
            analytic_shadow: false,
        }
    }
}

/// The planner's verdict for one potential layer (§3145).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerPlan {
    /// Every reason this layer could exist for, before any elimination.
    pub requested: ReasonSet,
    /// Which rungs of the §3161 ladder fired.
    pub eliminated: EliminationSet,
    /// The reasons that survived. Empty means no offscreen target: the group
    /// stays in its parent's pass.
    pub surviving: ReasonSet,
    /// The opacity to multiply into each child, when the opacity was pushed down.
    /// `1.0` when it was not (nothing to fold).
    pub fold_opacity: f32,
}

impl LayerPlan {
    /// Whether this layer must render to an offscreen target: true exactly when a
    /// reason survived every elimination (§3145's "offscreen is created only when
    /// all fail").
    pub fn needs_offscreen(&self) -> bool {
        !self.surviving.is_empty()
    }

    /// Whether the group's opacity was pushed into its children.
    pub fn folds_opacity(&self) -> bool {
        self.eliminated
            .contains(LayerElimination::OpacityPushedIntoChildren)
    }

    /// The dominating realization cost of the surviving reasons (§7.5). A fully
    /// eliminated layer is [`EffectCost::Local`].
    pub fn cost(&self) -> EffectCost {
        EffectCost::dominating(self.surviving.iter().map(LayerReason::cost))
    }

    /// Whether this layer ended up local (§3102) or nonlocal (§3120). A layer is
    /// nonlocal exactly when it needs a target — the frontier and the offscreen
    /// decision are the same decision.
    pub fn locality(&self) -> EffectLocality {
        self.cost().locality()
    }

    /// The single reason to report when a layer survives — the most expensive one,
    /// which is the one that would have to be eliminated next to save the pass.
    pub fn dominating_reason(&self) -> Option<LayerReason> {
        self.surviving.iter().max_by_key(|r| r.cost())
    }
}

/// Run the Effect Planner over one potential layer (§3145).
///
/// Collects the [`LayerReason`]s the request raises, then walks the §3161
/// elimination ladder in order. Two rungs can remove a reason:
///
/// - **push opacity into children**: a translucent group whose children provably
///   do not overlap fades each child instead of a composited layer (§14.5), which
///   is decided by [`plan_group_opacity`] — the same pure planner C0.4 defined, so
///   the fold rule lives in exactly one place.
/// - **fuse the color matrix**: a chain that fuses to zero ops computes nothing,
///   so the target it would have been evaluated in is not needed (§3178).
///
/// The remaining four rungs are recorded facts about *how* the surviving work was
/// made cheap — they are what the planner "accretes" from C0 (scissor), E0
/// (analytic shadow), E2.1 (shared backdrop) and E2.2 (collapsed effects). A
/// layer with an empty surviving set stays in its parent's pass; anything else
/// gets a target, and its cost is the dominating surviving reason.
pub fn plan_layer(request: &LayerRequest) -> LayerPlan {
    let mut requested = ReasonSet::EMPTY;
    if request.opacity < 1.0 {
        requested.insert(LayerReason::GroupOpacity);
    }
    // A blur and a color chain are both "a filter over the group's own pixels":
    // they need the group rendered before the filter can read it.
    if request.blurs_content || request.fused_color_ops > 0 {
        requested.insert(LayerReason::ImageFilter);
    }
    if request.filters_backdrop {
        requested.insert(LayerReason::BackdropFilter);
    }
    if request.advanced_blend {
        requested.insert(LayerReason::AdvancedBlend);
    }
    if request.isolated {
        requested.insert(LayerReason::Isolation);
    }
    if request.complex_mask {
        requested.insert(LayerReason::ComplexMask);
    }
    if request.snapshot_cached {
        requested.insert(LayerReason::SnapshotCache);
    }
    if request.native_material_boundary {
        requested.insert(LayerReason::NativeMaterialBoundary);
    }

    let mut surviving = requested;
    let mut eliminated = EliminationSet::EMPTY;
    let mut fold_opacity = 1.0;

    // 1. Push the opacity into the children?
    if requested.contains(LayerReason::GroupOpacity)
        && let OpacityPlan::FoldIntoChildren { factor } =
            plan_group_opacity(request.opacity, request.overlap)
    {
        surviving.remove(LayerReason::GroupOpacity);
        eliminated.insert(LayerElimination::OpacityPushedIntoChildren);
        fold_opacity = factor;
    }

    // 2. Fuse the color matrix? A chain that fused away entirely needs no target
    //    to be evaluated in; a chain that fused to fewer ops than it was authored
    //    with still saved a pass per op it shed.
    if request.color_effects > 0 && request.fused_color_ops < request.color_effects {
        eliminated.insert(LayerElimination::FusedColorMatrix);
        if request.fused_color_ops == 0 && !request.blurs_content {
            surviving.remove(LayerReason::ImageFilter);
        }
    }

    // 5a. Share the backdrop? A backdrop filter whose capture group already exists
    //     samples pixels somebody else re-rendered, and the blurred result is a
    //     texture the group's own draw can read in place — so the *reason* is gone,
    //     not merely made cheaper. A backdrop request that could not be captured
    //     never reaches here with this flag set.
    if request.shared_backdrop {
        surviving.remove(LayerReason::BackdropFilter);
    }

    // An elimination that does not eliminate is not applied: if some other reason
    // kept the target alive, the composite draw that reads it back multiplies the
    // group opacity for free, so pushing the same factor into every child would be
    // work with no saving (and a second place for the factor to be wrong).
    if !surviving.is_empty() && eliminated.contains(LayerElimination::OpacityPushedIntoChildren) {
        eliminated.remove(LayerElimination::OpacityPushedIntoChildren);
        surviving.insert(LayerReason::GroupOpacity);
        fold_opacity = 1.0;
    }

    // 4-6. Facts the earlier lanes established for this group. These rungs only
    //    make a layer cheaper, so they are recorded after the reason-removing ones
    //    have settled `surviving`.
    if request.analytic_shadow {
        eliminated.insert(LayerElimination::AnalyticShadow);
    }
    if request.shared_backdrop {
        eliminated.insert(LayerElimination::SharedBackdrop);
    }
    if request.color_effects >= 2 && request.fused_color_ops < request.color_effects {
        eliminated.insert(LayerElimination::CollapsedAdjacentEffects);
    }

    // 3. A scissor instead of a clip layer: true whenever the group ends up staying
    //    in its parent's pass, where its clip *is* the pass scissor. Evaluated last
    //    because it is a statement about the outcome of every other rung.
    if surviving.is_empty() {
        eliminated.insert(LayerElimination::ScissorInsteadOfClipLayer);
    }

    LayerPlan {
        requested,
        eliminated,
        surviving,
        fold_opacity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layer that asked for nothing is not a layer: no reason, no target, and
    /// its clip rides the pass scissor.
    #[test]
    fn an_empty_request_is_not_a_layer() {
        let plan = plan_layer(&LayerRequest::default());
        assert_eq!(plan.requested, ReasonSet::EMPTY);
        assert_eq!(plan.surviving, ReasonSet::EMPTY);
        assert!(!plan.needs_offscreen());
        assert_eq!(plan.cost(), EffectCost::Local);
        assert_eq!(plan.locality(), EffectLocality::Local);
        assert_eq!(plan.dominating_reason(), None);
        assert!(
            plan.eliminated
                .contains(LayerElimination::ScissorInsteadOfClipLayer)
        );
        assert_eq!(plan.fold_opacity, 1.0);
    }

    /// The headline elimination: a translucent group over disjoint children pushes
    /// its opacity down and allocates nothing.
    #[test]
    fn disjoint_group_opacity_is_pushed_into_the_children() {
        let plan = plan_layer(&LayerRequest {
            opacity: 0.5,
            overlap: ChildOverlap::Disjoint,
            ..LayerRequest::default()
        });
        assert!(plan.requested.contains(LayerReason::GroupOpacity));
        assert!(plan.folds_opacity());
        assert_eq!(plan.fold_opacity, 0.5);
        assert!(!plan.needs_offscreen());
        assert_eq!(plan.locality(), EffectLocality::Local);
    }

    /// Overlapping children keep the layer, and `Unknown` overlap behaves exactly
    /// like proven overlap — the "when unsure, keep correctness" rule (§14.5).
    #[test]
    fn overlapping_group_opacity_survives() {
        for overlap in [ChildOverlap::Overlapping, ChildOverlap::Unknown] {
            let plan = plan_layer(&LayerRequest {
                opacity: 0.5,
                overlap,
                ..LayerRequest::default()
            });
            assert!(plan.needs_offscreen(), "{overlap:?} keeps the layer");
            assert!(!plan.folds_opacity());
            assert_eq!(plan.fold_opacity, 1.0);
            assert_eq!(plan.surviving, ReasonSet::of(LayerReason::GroupOpacity));
            assert_eq!(plan.cost(), EffectCost::NeedsOffscreen);
            assert_eq!(plan.locality(), EffectLocality::Nonlocal);
            assert_eq!(plan.dominating_reason(), Some(LayerReason::GroupOpacity));
            assert!(
                !plan
                    .eliminated
                    .contains(LayerElimination::ScissorInsteadOfClipLayer),
                "a surviving layer did not become a scissor"
            );
        }
    }

    /// A color chain that fuses to nothing eliminates the filter reason; one that
    /// fuses to an op keeps it, and records the collapse either way.
    #[test]
    fn a_fused_away_color_chain_eliminates_the_layer() {
        let gone = plan_layer(&LayerRequest {
            color_effects: 3,
            fused_color_ops: 0,
            ..LayerRequest::default()
        });
        assert!(gone.requested.is_empty(), "no op means no filter requested");
        assert!(!gone.needs_offscreen());
        assert!(gone.eliminated.contains(LayerElimination::FusedColorMatrix));
        assert!(
            gone.eliminated
                .contains(LayerElimination::CollapsedAdjacentEffects)
        );

        let kept = plan_layer(&LayerRequest {
            color_effects: 3,
            fused_color_ops: 1,
            ..LayerRequest::default()
        });
        assert_eq!(kept.surviving, ReasonSet::of(LayerReason::ImageFilter));
        assert!(kept.needs_offscreen());
        assert!(kept.eliminated.contains(LayerElimination::FusedColorMatrix));
        assert!(
            kept.eliminated
                .contains(LayerElimination::CollapsedAdjacentEffects)
        );
    }

    /// A single authored effect that survives fusion collapsed nothing: the
    /// pass-saving rungs must not claim credit they did not earn.
    #[test]
    fn one_unfused_effect_collapses_nothing() {
        let plan = plan_layer(&LayerRequest {
            color_effects: 1,
            fused_color_ops: 1,
            ..LayerRequest::default()
        });
        assert!(!plan.eliminated.contains(LayerElimination::FusedColorMatrix));
        assert!(
            !plan
                .eliminated
                .contains(LayerElimination::CollapsedAdjacentEffects)
        );
        assert!(plan.needs_offscreen());
    }

    /// A blur is nonlocal by definition (§3120): no rung of the ladder can push
    /// neighbor sampling into a child's shader. The target survives, so the
    /// opacity fold is withdrawn — the composite draw carries the factor for free.
    #[test]
    fn a_blur_survives_every_elimination() {
        let plan = plan_layer(&LayerRequest {
            opacity: 0.5,
            overlap: ChildOverlap::Disjoint,
            blurs_content: true,
            ..LayerRequest::default()
        });
        assert!(plan.needs_offscreen());
        assert!(
            !plan.folds_opacity(),
            "a fold that saves nothing is not applied"
        );
        assert_eq!(plan.fold_opacity, 1.0);
        assert_eq!(plan.surviving.len(), 2);
        assert!(plan.surviving.contains(LayerReason::ImageFilter));
        assert!(plan.surviving.contains(LayerReason::GroupOpacity));
        assert_eq!(plan.locality(), EffectLocality::Nonlocal);
    }

    /// A fused-away color chain over a blurred group must not eliminate the blur's
    /// filter reason — both raise `ImageFilter`, and only one of them went away.
    #[test]
    fn fusing_a_chain_does_not_eliminate_a_blur() {
        let plan = plan_layer(&LayerRequest {
            blurs_content: true,
            color_effects: 2,
            fused_color_ops: 0,
            ..LayerRequest::default()
        });
        assert_eq!(plan.surviving, ReasonSet::of(LayerReason::ImageFilter));
        assert!(plan.needs_offscreen());
    }

    /// The costliest surviving reason dominates, and it is the one to report.
    #[test]
    fn the_costliest_surviving_reason_dominates() {
        let plan = plan_layer(&LayerRequest {
            opacity: 0.5,
            filters_backdrop: true,
            advanced_blend: true,
            ..LayerRequest::default()
        });
        assert_eq!(plan.surviving.len(), 3);
        assert_eq!(plan.cost(), EffectCost::DestinationRead);
        assert_eq!(plan.dominating_reason(), Some(LayerReason::AdvancedBlend));
        assert!(plan.cost().reads_destination());
    }

    /// Every reason raises, survives, and is reported independently — no reason is
    /// silently dropped or conflated with another.
    #[test]
    fn every_reason_can_be_raised_alone() {
        let requests = [
            (
                LayerReason::GroupOpacity,
                LayerRequest {
                    opacity: 0.5,
                    ..LayerRequest::default()
                },
            ),
            (
                LayerReason::ImageFilter,
                LayerRequest {
                    blurs_content: true,
                    ..LayerRequest::default()
                },
            ),
            (
                LayerReason::BackdropFilter,
                LayerRequest {
                    filters_backdrop: true,
                    ..LayerRequest::default()
                },
            ),
            (
                LayerReason::AdvancedBlend,
                LayerRequest {
                    advanced_blend: true,
                    ..LayerRequest::default()
                },
            ),
            (
                LayerReason::Isolation,
                LayerRequest {
                    isolated: true,
                    ..LayerRequest::default()
                },
            ),
            (
                LayerReason::ComplexMask,
                LayerRequest {
                    complex_mask: true,
                    ..LayerRequest::default()
                },
            ),
            (
                LayerReason::SnapshotCache,
                LayerRequest {
                    snapshot_cached: true,
                    ..LayerRequest::default()
                },
            ),
            (
                LayerReason::NativeMaterialBoundary,
                LayerRequest {
                    native_material_boundary: true,
                    ..LayerRequest::default()
                },
            ),
        ];
        for (reason, request) in requests {
            let plan = plan_layer(&request);
            assert_eq!(plan.requested, ReasonSet::of(reason), "{reason:?}");
            assert_eq!(plan.surviving, ReasonSet::of(reason), "{reason:?}");
            assert_eq!(plan.dominating_reason(), Some(reason));
            assert_eq!(plan.cost(), reason.cost());
        }
    }

    /// A fully transparent group folds rather than isolating, whatever the
    /// children do — nothing it draws can double-blend.
    #[test]
    fn a_transparent_group_folds_even_when_overlapping() {
        let plan = plan_layer(&LayerRequest {
            opacity: 0.0,
            overlap: ChildOverlap::Overlapping,
            ..LayerRequest::default()
        });
        assert!(plan.folds_opacity());
        assert_eq!(plan.fold_opacity, 0.0);
        assert!(!plan.needs_offscreen());
    }

    /// The pass-saving rungs are recorded even when the layer survives — they
    /// describe how the surviving work was made cheap, not whether it exists.
    #[test]
    fn pass_saving_rungs_are_recorded_on_a_surviving_layer() {
        let plan = plan_layer(&LayerRequest {
            blurs_content: true,
            filters_backdrop: true,
            shared_backdrop: true,
            analytic_shadow: true,
            ..LayerRequest::default()
        });
        assert!(plan.needs_offscreen(), "the content blur keeps it alive");
        assert!(plan.eliminated.contains(LayerElimination::SharedBackdrop));
        assert!(plan.eliminated.contains(LayerElimination::AnalyticShadow));
        assert_eq!(plan.eliminated.len(), 2);
        assert_eq!(plan.surviving, ReasonSet::of(LayerReason::ImageFilter));
    }

    /// A frosted panel over an existing capture group raises its backdrop reason
    /// and then retires it: the shared capture hands it a texture to sample in
    /// place, so the panel stays in its parent's pass.
    #[test]
    fn a_shared_capture_retires_the_backdrop_reason() {
        let plan = plan_layer(&LayerRequest {
            filters_backdrop: true,
            shared_backdrop: true,
            ..LayerRequest::default()
        });
        assert_eq!(
            plan.requested,
            ReasonSet::of(LayerReason::BackdropFilter),
            "the reason is raised, not hidden"
        );
        assert!(!plan.needs_offscreen());
        assert!(plan.eliminated.contains(LayerElimination::SharedBackdrop));
        assert!(
            plan.eliminated
                .contains(LayerElimination::ScissorInsteadOfClipLayer),
            "retiring the last reason leaves the clip on the pass scissor"
        );
    }

    /// A translucent frosted panel over disjoint children folds *and* retires its
    /// backdrop reason: the fold is not revoked by a reason that itself goes away.
    #[test]
    fn a_retired_backdrop_does_not_revoke_the_fold() {
        let plan = plan_layer(&LayerRequest {
            opacity: 0.4,
            overlap: ChildOverlap::Disjoint,
            filters_backdrop: true,
            shared_backdrop: true,
            ..LayerRequest::default()
        });
        assert!(plan.folds_opacity());
        assert_eq!(plan.fold_opacity, 0.4);
        assert!(!plan.needs_offscreen());
    }

    /// The two bitsets hold their whole domain, round-trip every member, and
    /// iterate in the declared order.
    #[test]
    fn the_bitsets_hold_their_whole_domain() {
        let mut reasons = ReasonSet::EMPTY;
        assert!(reasons.is_empty());
        for reason in LayerReason::ALL {
            reasons.insert(reason);
            assert!(reasons.contains(reason));
        }
        assert_eq!(reasons.len(), 8);
        assert_eq!(reasons.bits(), u8::MAX, "eight reasons fill the byte");
        assert!(reasons.iter().eq(LayerReason::ALL));
        for reason in LayerReason::ALL {
            reasons.remove(reason);
        }
        assert!(reasons.is_empty());

        let mut rungs = EliminationSet::EMPTY;
        assert!(rungs.is_empty());
        for rung in LayerElimination::ALL {
            rungs.insert(rung);
        }
        assert_eq!(rungs.len(), 6);
        assert!(rungs.iter().eq(LayerElimination::ALL));
        // Idempotent: recording the same rung twice is still one rung.
        rungs.insert(LayerElimination::SharedBackdrop);
        assert_eq!(rungs.len(), 6);
    }

    /// Every rung has a distinct, stable label and a dense index.
    #[test]
    fn elimination_labels_and_indices_are_stable() {
        let labels: Vec<&str> = LayerElimination::ALL.iter().map(|e| e.label()).collect();
        for (i, a) in labels.iter().enumerate() {
            assert_eq!(LayerElimination::ALL[i].index() as usize, i);
            for b in &labels[i + 1..] {
                assert_ne!(a, b, "labels are unique");
            }
        }
    }
}
