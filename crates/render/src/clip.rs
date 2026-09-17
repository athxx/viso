//! The clip planner (§14.1): classify a requested clip into the cheapest tier
//! that realizes it correctly, and encode the "what actually clips children"
//! policy that keeps ordinary rounded containers off the offscreen path.
//!
//! Clipping is a *ladder*, not a single mechanism. An axis-aligned rectangle is
//! a hardware scissor and costs nothing extra; a simple rounded rect is an
//! analytic coverage function evaluated in the fragment shader (still one
//! in-place draw); an arbitrary path needs a coverage mask built first; a stable
//! repeated complex clip earns a cached (retained R8) realization so it is built
//! once and reused. Defaulting every non-rect clip to an offscreen layer is the
//! anti-pattern this planner exists to prevent (§14): cost is paid by tier.
//!
//! This is a cold-path decision (§7.2): the tier is chosen once when a clip is
//! ingested — the same place [`EffectCost`] is assigned to a primitive — and
//! stored beside the resolved clip, never recomputed per frame on the hot path.
//! The planner returns *what to do*; the renderer and the retained
//! [`ClipChain`](crate::scene) machinery (C0.2) carry out and cache the result.

use crate::EffectCost;
use crate::primitive::Corners;
use crate::primitive::Rect;

/// The geometry of a requested clip, as authored — before the planner decides
/// how to realize it.
///
/// A [`RoundRect`](ClipShape::RoundRect) whose radii all normalize to `0` is
/// *not* a special case the caller must strip: the planner treats it as a plain
/// rect (a scissor), so a widget can pass its authored radii straight through
/// and still get the cheap path when they are sharp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipShape {
    /// An axis-aligned rectangle: the cheapest clip, a hardware scissor.
    Rect(Rect),
    /// A rounded rectangle with per-corner radii. Realized analytically when its
    /// radii are non-trivial, or as a plain rect scissor when they normalize to
    /// sharp corners.
    RoundRect {
        /// The clip box in physical pixels.
        rect: Rect,
        /// Per-corner radii, normalized against `rect` by the planner (§11.2).
        radii: Corners,
    },
    /// An arbitrary path clip, described here only by its axis-aligned bounds
    /// (the geometry itself lives in the path arena). Needs a coverage mask.
    Path {
        /// The path's axis-aligned bounding box in physical pixels — the tight
        /// ROI a coverage mask would be built over.
        bounds: Rect,
    },
}

/// How the planner decided a [`ClipShape`] must be realized, cheapest first.
///
/// The tier maps onto an [`EffectCost`] class ([`ClipTier::cost`]) so a clipped
/// drawable's dominating cost folds the clip in alongside its own realization,
/// and the renderer reads the same ladder it uses for every other effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipTier {
    /// A merged hardware scissor — an axis-aligned rect, or a rounded rect whose
    /// radii vanished. No shader work, no mask, no extra pass. [`EffectCost::Local`].
    Scissor,
    /// An analytic coverage clip evaluated in the fragment shader — a simple
    /// rounded rect / simple analytic shape, drawn in place. [`EffectCost::Analytic`].
    Analytic,
    /// A coverage mask built once for this frame, then sampled by the clipped
    /// draw — an arbitrary path clip that is not stable enough to cache.
    /// [`EffectCost::NeedsMask`], advances `clip_mask_builds`.
    Mask,
    /// A retained coverage mask (R8 `ClipMaskAtlas`) built once and reused across
    /// frames — a stable repeated complex clip. Same per-frame cost class as
    /// [`Mask`](ClipTier::Mask) the frame it is first built; free afterward while
    /// the geometry is unchanged (C0.2).
    CachedMask,
}

impl ClipTier {
    /// The [`EffectCost`] class this tier contributes to a clipped drawable.
    pub fn cost(self) -> EffectCost {
        match self {
            ClipTier::Scissor => EffectCost::Local,
            ClipTier::Analytic => EffectCost::Analytic,
            // A cached mask still costs a mask build the frame it is realized;
            // C0.2's retained ClipChain is what makes later frames free, not a
            // cheaper cost class here.
            ClipTier::Mask | ClipTier::CachedMask => EffectCost::NeedsMask,
        }
    }

    /// Whether realizing this tier builds a coverage mask (advances
    /// `clip_mask_builds` for the frame it is built).
    pub fn builds_mask(self) -> bool {
        matches!(self, ClipTier::Mask | ClipTier::CachedMask)
    }
}

/// The planner's decision for one clip: the tier that realizes it, that tier's
/// cost class, and the tight bounds the clip constrains drawing to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipPlan {
    /// The chosen realization tier.
    pub tier: ClipTier,
    /// The tier's [`EffectCost`] contribution (`tier.cost()`), stored so callers
    /// fold it without re-deriving.
    pub cost: EffectCost,
    /// The clip's axis-aligned bounds in physical pixels: a scissor rect, or the
    /// ROI a mask is built over. An analytic rounded-rect clip still scissors to
    /// this box first, so nothing outside the corners is even shaded.
    pub bounds: Rect,
}

/// Classify a clip [`ClipShape`] into the cheapest tier that realizes it (§14.1).
///
/// `stable` marks a clip whose geometry repeats unchanged across frames (a
/// retained container's clip, not a per-frame-rebuilt one); a stable *complex*
/// clip earns [`ClipTier::CachedMask`] so its coverage is built once and reused,
/// while an unstable one stays a per-frame [`ClipTier::Mask`]. `stable` has no
/// effect on rect / analytic clips — they never build a mask to cache.
///
/// The rounded-rect decision is where "when profitable" lives: a `RoundRect`
/// whose radii normalize to all-sharp is a plain scissor (free), and only a
/// genuinely rounded one pays the analytic shader.
pub fn plan_clip(shape: ClipShape, stable: bool) -> ClipPlan {
    match shape {
        ClipShape::Rect(rect) => ClipPlan {
            tier: ClipTier::Scissor,
            cost: ClipTier::Scissor.cost(),
            bounds: rect,
        },
        ClipShape::RoundRect { rect, radii } => {
            // Normalize against the box, then ask whether any corner survives.
            // All-sharp → a rect scissor; otherwise an analytic coverage clip
            // (which still scissors to `rect` first).
            let n = radii.normalized(rect.w, rect.h);
            let rounded = n.left_top > 0.0
                || n.right_top > 0.0
                || n.right_bottom > 0.0
                || n.left_bottom > 0.0;
            let tier = if rounded {
                ClipTier::Analytic
            } else {
                ClipTier::Scissor
            };
            ClipPlan {
                tier,
                cost: tier.cost(),
                bounds: rect,
            }
        }
        ClipShape::Path { bounds } => {
            let tier = if stable {
                ClipTier::CachedMask
            } else {
                ClipTier::Mask
            };
            ClipPlan {
                tier,
                cost: tier.cost(),
                bounds,
            }
        }
    }
}

/// Whether a container clips its children, and if so how, given only that it has
/// a border radius and whether it is a scroll viewport (§14).
///
/// This encodes the policy that keeps the common case cheap: **a border radius
/// does not imply clip-children**. An ordinary rounded container is
/// overflow-visible — its children may draw past the rounded corners, so it
/// needs no clip at all and never forces an offscreen layer just for being
/// rounded. Only a scroll viewport actually clips its content, and it does so
/// with a scissor (the analytic corner rounding, if any, is a separate paint of
/// the container's own background — not a reason to offscreen the subtree).
///
/// Returns `None` when the container imposes no clip on its children.
pub fn clips_children(has_radius: bool, is_scroll_viewport: bool) -> Option<ClipTier> {
    // A scroll viewport clips its overflow to its box — a scissor, regardless of
    // whether its background is rounded. Rounding the *clip* of a scroller is a
    // later, opt-in refinement; the default viewport clip is the cheap rect.
    if is_scroll_viewport {
        return Some(ClipTier::Scissor);
    }
    // Border radius alone: overflow-visible, no clip. `has_radius` is accepted so
    // the policy reads at the call site as "radius considered, deliberately does
    // not clip" rather than being silently dropped.
    let _ = has_radius;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 40.0,
        }
    }

    /// An axis-aligned rect clip is the cheapest tier: a scissor, `Local` cost,
    /// bounds passed straight through.
    #[test]
    fn rect_clip_is_a_scissor() {
        let plan = plan_clip(ClipShape::Rect(rect()), false);
        assert_eq!(plan.tier, ClipTier::Scissor);
        assert_eq!(plan.cost, EffectCost::Local);
        assert_eq!(plan.bounds, rect());
        assert!(!plan.tier.builds_mask());
    }

    /// A rounded-rect clip with real radii is an analytic coverage clip — one
    /// in-place draw, `Analytic` cost, no mask build.
    #[test]
    fn rounded_clip_is_analytic() {
        let plan = plan_clip(
            ClipShape::RoundRect {
                rect: rect(),
                radii: Corners::uniform(8.0),
            },
            false,
        );
        assert_eq!(plan.tier, ClipTier::Analytic);
        assert_eq!(plan.cost, EffectCost::Analytic);
        assert!(!plan.tier.builds_mask());
    }

    /// A rounded-rect whose radii are all sharp collapses to a scissor — the
    /// caller need not strip zero radii itself ("when profitable").
    #[test]
    fn sharp_rounded_clip_collapses_to_scissor() {
        let plan = plan_clip(
            ClipShape::RoundRect {
                rect: rect(),
                radii: Corners::SHARP,
            },
            true, // stability is irrelevant for a rect/analytic clip
        );
        assert_eq!(plan.tier, ClipTier::Scissor);
        assert_eq!(plan.cost, EffectCost::Local);
    }

    /// An unstable path clip builds a per-frame coverage mask; a stable one is
    /// promoted to a cached (retained) mask. Both are `NeedsMask` cost and both
    /// build a mask the frame they are realized.
    #[test]
    fn path_clip_masks_and_caches_by_stability() {
        let unstable = plan_clip(ClipShape::Path { bounds: rect() }, false);
        assert_eq!(unstable.tier, ClipTier::Mask);
        assert_eq!(unstable.cost, EffectCost::NeedsMask);
        assert!(unstable.tier.builds_mask());

        let stable = plan_clip(ClipShape::Path { bounds: rect() }, true);
        assert_eq!(stable.tier, ClipTier::CachedMask);
        assert_eq!(stable.cost, EffectCost::NeedsMask);
        assert!(stable.tier.builds_mask());
    }

    /// A path clip's bounds are its tight ROI (the mask extent), not the whole
    /// surface.
    #[test]
    fn path_clip_bounds_are_the_roi() {
        let roi = Rect {
            x: 12.0,
            y: 5.0,
            w: 30.0,
            h: 20.0,
        };
        let plan = plan_clip(ClipShape::Path { bounds: roi }, false);
        assert_eq!(plan.bounds, roi);
    }

    /// Border radius alone does not clip children: an ordinary rounded container
    /// is overflow-visible and imposes no clip (never an offscreen layer just for
    /// being rounded).
    #[test]
    fn border_radius_does_not_clip_children() {
        assert_eq!(clips_children(true, false), None);
        assert_eq!(clips_children(false, false), None);
    }

    /// A scroll viewport clips its overflow with a scissor, rounded parent or
    /// not — a rect clip, not an offscreen layer.
    #[test]
    fn scroll_viewport_gets_a_scissor() {
        assert_eq!(clips_children(false, true), Some(ClipTier::Scissor));
        // Even with a rounded background, the viewport clip stays a scissor.
        assert_eq!(clips_children(true, true), Some(ClipTier::Scissor));
    }

    /// Each tier maps to its stated cost class, ordered cheapest-first, so a
    /// clipped drawable folds the clip cost through the same ladder as any
    /// effect.
    #[test]
    fn tier_costs_are_ordered() {
        assert!(ClipTier::Scissor.cost() < ClipTier::Analytic.cost());
        assert!(ClipTier::Analytic.cost() < ClipTier::Mask.cost());
        assert_eq!(ClipTier::CachedMask.cost(), ClipTier::Mask.cost());
    }
}
