//! Effect cost metadata (§7.5, §30): how a primitive or effect must be realized
//! on the GPU, and what that realization costs the frame.
//!
//! Every drawable carries one [`EffectCost`] class. The class is *what the
//! renderer must do to composite it correctly* — draw it in place, evaluate it
//! analytically, build a coverage mask, render it through an offscreen pass,
//! capture the backdrop behind it, read the destination, or route it to a
//! compute pass. The class drives the frame's resource decisions (mask
//! allocation, offscreen passes, backdrop capture) and the matching §30
//! counters, and it lets the inspector show *why* a primitive costs what it
//! costs without unsafe memory poking (§62).
//!
//! This is a cold-path contract: the class is computed once when a primitive is
//! ingested (or an effect chain is built), stored beside it, and read by the
//! planner and the inspector — never recomputed per frame on the hot path
//! (§7.2). The classes are ordered by increasing realization cost, so a chain's
//! dominating cost is the maximum over its links.

/// How a drawable must be realized on the GPU, ordered by increasing cost
/// (§7.5). A primitive or effect chain carries the single class that dominates
/// its realization; the renderer reads it to decide which resources the frame
/// needs and which §30 counters advance.
///
/// The variants are ordered cheapest-first so `max` over a chain yields the
/// dominating cost, and the discriminant doubles as a coarse cost rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum EffectCost {
    /// Drawn in place in the current pass with no extra target, mask, or read:
    /// a solid rect, an opaque image, a glyph run. The D0 steady-state case —
    /// nothing but instances and one draw. Cheapest.
    #[default]
    Local = 0,
    /// Evaluated analytically in the fragment shader (a rounded-rect / ellipse /
    /// line coverage function) — still one in-place draw, no offscreen target,
    /// but a heavier shader than `Local`.
    Analytic = 1,
    /// Needs a coverage mask built first (an arbitrary clip path, a soft mask):
    /// one `clip_mask_builds` before the masked draw. No offscreen color pass.
    NeedsMask = 2,
    /// Needs an offscreen color pass rendered then composited back — a
    /// translucent group layer, an isolated blend group: one `offscreen_passes`
    /// and its `transient_target_bytes`.
    NeedsOffscreen = 3,
    /// Needs the backdrop behind the drawable captured before it draws — a
    /// backdrop blur / backdrop filter: one `backdrop_capture_pixels` region
    /// plus (usually) an offscreen pass.
    NeedsBackdrop = 4,
    /// Needs to read the destination it draws over within the same pass — a
    /// non-separable blend mode (multiply / screen / overlay) where the hardware
    /// blender is insufficient. Forces a barrier or destination copy.
    DestinationRead = 5,
    /// Better realized on a compute pass than the raster pipeline — a large
    /// separable blur, a reduction: routed to compute when the backend supports
    /// it. Most specialized.
    ComputePreferred = 6,
}

/// Whether an effect can be evaluated from the fragment it is shading alone
/// (§3102) or needs pixels it does not own (§3120).
///
/// This is the single most consequential fact about an effect, because it decides
/// whether the effect is *free* — folded into the draw shader that was going to
/// run anyway — or whether it buys a render target:
///
/// - **Local** (§3102): the output pixel is a pure function of the source pixel.
///   Opacity, tint, a color matrix, brightness/contrast/saturation, a simple
///   gradient, a simple mask, a blend the fixed-function stage expresses. These
///   fuse into the existing draw shader; the frame allocates nothing for them.
/// - **Nonlocal** (§3120): the output pixel depends on neighbor samples, on the
///   previous framebuffer, or on the group being composited in isolation first.
///   Blur, backdrop blur, a large/general shadow, a destination-dependent
///   advanced blend, displacement, group opacity over overlapping children. Only
///   these may consider an offscreen target.
///
/// The frontier is exactly one threshold on the [`EffectCost`] ladder — the
/// ladder is already ordered by realization cost, and the first rung that stops
/// being expressible in place is [`NeedsOffscreen`](EffectCost::NeedsOffscreen).
/// Deriving locality from the ladder rather than storing it separately is what
/// keeps the two facts from drifting apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum EffectLocality {
    /// Evaluated from the shaded fragment alone — fuses into the draw shader
    /// (§3102).
    #[default]
    Local = 0,
    /// Needs neighbor samples, the previous framebuffer, or group isolation —
    /// only these effects may consider an offscreen target (§3120).
    Nonlocal = 1,
}

impl EffectLocality {
    /// The lowercase label for an inspector dump / overlay row.
    pub fn label(self) -> &'static str {
        match self {
            EffectLocality::Local => "local",
            EffectLocality::Nonlocal => "nonlocal",
        }
    }
}

impl EffectCost {
    /// The lowercase label for an inspector dump / overlay row.
    pub fn label(self) -> &'static str {
        match self {
            EffectCost::Local => "local",
            EffectCost::Analytic => "analytic",
            EffectCost::NeedsMask => "needs-mask",
            EffectCost::NeedsOffscreen => "needs-offscreen",
            EffectCost::NeedsBackdrop => "needs-backdrop",
            EffectCost::DestinationRead => "destination-read",
            EffectCost::ComputePreferred => "compute-preferred",
        }
    }

    /// Whether this class needs a coverage mask built before it draws
    /// (advances `clip_mask_builds`).
    pub fn needs_mask(self) -> bool {
        matches!(self, EffectCost::NeedsMask)
    }

    /// Whether this class needs an offscreen color pass (advances
    /// `offscreen_passes` / `transient_target_bytes`). A backdrop effect also
    /// composites through an offscreen target.
    pub fn needs_offscreen(self) -> bool {
        matches!(self, EffectCost::NeedsOffscreen | EffectCost::NeedsBackdrop)
    }

    /// Whether this class needs the backdrop captured before it draws (advances
    /// `backdrop_capture_pixels`).
    pub fn needs_backdrop(self) -> bool {
        matches!(self, EffectCost::NeedsBackdrop)
    }

    /// Whether this class must read the destination within its pass (forces a
    /// barrier / destination copy — no plain hardware blend suffices).
    pub fn reads_destination(self) -> bool {
        matches!(self, EffectCost::DestinationRead)
    }

    /// Whether this class is local (§3102) or nonlocal (§3120): the one threshold
    /// on the ladder at [`NeedsOffscreen`](EffectCost::NeedsOffscreen) — a class
    /// below it shades in place, a class at or above it needs pixels the fragment
    /// does not own. [`NeedsMask`](EffectCost::NeedsMask) stays local: the mask is
    /// a separate coverage build, but the masked draw itself still shades in place
    /// from its own fragment (§3102's "simple mask").
    pub fn locality(self) -> EffectLocality {
        if self >= EffectCost::NeedsOffscreen {
            EffectLocality::Nonlocal
        } else {
            EffectLocality::Local
        }
    }

    /// Whether this class fuses into the existing draw shader (§3102).
    pub fn is_local(self) -> bool {
        self.locality() == EffectLocality::Local
    }

    /// The dominating cost of a chain of links: the maximum class, since the
    /// variants are ordered cheapest-first. An empty chain is [`Local`].
    ///
    /// [`Local`]: EffectCost::Local
    pub fn dominating(chain: impl IntoIterator<Item = EffectCost>) -> EffectCost {
        chain.into_iter().max().unwrap_or(EffectCost::Local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The variants are ordered cheapest-first, so the discriminant is a
    /// monotonic cost rank and `Ord` agrees with realization cost.
    #[test]
    fn classes_are_ordered_cheapest_first() {
        let order = [
            EffectCost::Local,
            EffectCost::Analytic,
            EffectCost::NeedsMask,
            EffectCost::NeedsOffscreen,
            EffectCost::NeedsBackdrop,
            EffectCost::DestinationRead,
            EffectCost::ComputePreferred,
        ];
        for pair in order.windows(2) {
            assert!(pair[0] < pair[1], "{:?} ranks below {:?}", pair[0], pair[1]);
        }
        assert_eq!(
            EffectCost::default(),
            EffectCost::Local,
            "default is cheapest"
        );
    }

    /// The resource predicates match each class's stated realization.
    #[test]
    fn predicates_match_realization() {
        assert!(EffectCost::NeedsMask.needs_mask());
        assert!(!EffectCost::Local.needs_mask());

        // Both an isolated layer and a backdrop composite through an offscreen
        // target.
        assert!(EffectCost::NeedsOffscreen.needs_offscreen());
        assert!(EffectCost::NeedsBackdrop.needs_offscreen());
        assert!(!EffectCost::NeedsMask.needs_offscreen());

        assert!(EffectCost::NeedsBackdrop.needs_backdrop());
        assert!(!EffectCost::NeedsOffscreen.needs_backdrop());

        assert!(EffectCost::DestinationRead.reads_destination());
        assert!(!EffectCost::Local.reads_destination());
    }

    /// A chain's dominating cost is the maximum link; an empty chain is `Local`.
    #[test]
    fn dominating_is_the_max_link() {
        assert_eq!(EffectCost::dominating([]), EffectCost::Local);
        assert_eq!(
            EffectCost::dominating([EffectCost::Local, EffectCost::Analytic]),
            EffectCost::Analytic,
        );
        assert_eq!(
            EffectCost::dominating([
                EffectCost::NeedsMask,
                EffectCost::NeedsBackdrop,
                EffectCost::Analytic,
            ]),
            EffectCost::NeedsBackdrop,
        );
    }

    /// The Local/Nonlocal frontier (§3102 vs §3120) is one threshold on the
    /// ladder, and it coincides exactly with "may consider an offscreen target".
    #[test]
    fn locality_is_one_threshold_on_the_ladder() {
        for local in [
            EffectCost::Local,
            EffectCost::Analytic,
            EffectCost::NeedsMask,
        ] {
            assert_eq!(local.locality(), EffectLocality::Local, "{local:?}");
            assert!(local.is_local());
            assert!(!local.needs_offscreen(), "a local class needs no target");
        }
        for nonlocal in [
            EffectCost::NeedsOffscreen,
            EffectCost::NeedsBackdrop,
            EffectCost::DestinationRead,
            EffectCost::ComputePreferred,
        ] {
            assert_eq!(
                nonlocal.locality(),
                EffectLocality::Nonlocal,
                "{nonlocal:?}"
            );
            assert!(!nonlocal.is_local());
        }
        // The threshold is monotone: locality never drops as cost rises.
        let ladder = [
            EffectCost::Local,
            EffectCost::Analytic,
            EffectCost::NeedsMask,
            EffectCost::NeedsOffscreen,
            EffectCost::NeedsBackdrop,
            EffectCost::DestinationRead,
            EffectCost::ComputePreferred,
        ];
        for pair in ladder.windows(2) {
            assert!(pair[0].locality() <= pair[1].locality());
        }
        assert_eq!(EffectLocality::default(), EffectLocality::Local);
        assert_ne!(
            EffectLocality::Local.label(),
            EffectLocality::Nonlocal.label()
        );
    }

    /// Every class has a distinct, stable label.
    #[test]
    fn labels_are_distinct() {
        let all = [
            EffectCost::Local,
            EffectCost::Analytic,
            EffectCost::NeedsMask,
            EffectCost::NeedsOffscreen,
            EffectCost::NeedsBackdrop,
            EffectCost::DestinationRead,
            EffectCost::ComputePreferred,
        ];
        let labels: Vec<&str> = all.iter().map(|c| c.label()).collect();
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b, "labels are unique");
            }
        }
    }
}
