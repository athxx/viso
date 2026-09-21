//! The blend model and its realization classifier (§14.6): the set of blend
//! modes a drawable can composite with, and the cold-path decision of *how* each
//! one is realized — a plain hardware blend, a destination read, or an isolated
//! offscreen composite routed to the Effect Planner (E2).
//!
//! A blend mode says how a source color combines with the destination already in
//! the target. Three tiers matter to the renderer, and the whole point of the
//! classifier is to keep the common one cheap (§14.6: "复杂 blend 不允许污染最
//! 常用 `SrcOver` pipeline" — advanced blends must not pollute the SrcOver
//! pipeline):
//!
//! - **Fixed-function.** The Porter-Duff compositing operators plus `Plus` are
//!   expressible directly by the hardware blender's factor/equation state
//!   (`src * Fs (op) dst * Fd`). They cost nothing beyond an in-place draw —
//!   [`EffectCost::Local`] — and [`SrcOver`](Blend::SrcOver), the default, is the
//!   hot path every ordinary drawable takes.
//! - **Destination read.** The separable artistic modes (`Multiply`, `Screen`,
//!   `Overlay`, `Darken`, `Lighten`, and the rest of the SVG/CSS separable set)
//!   are per-channel functions of source and destination that the fixed-function
//!   blender cannot express. They need the destination sampled in-pass —
//!   [`EffectCost::DestinationRead`] — via a backend fast path
//!   (subpass / framebuffer-fetch) where one exists.
//! - **Isolation.** The non-separable HSL modes (`Hue`, `Saturation`, `Color`,
//!   `Luminosity`) mix channels across the whole pixel and, applied to a group,
//!   must composite an isolated layer. They carry [`EffectCost::NeedsOffscreen`]
//!   and tag their layer [`LayerReason::AdvancedBlend`].
//!
//! Per §14.6 the last two tiers "must explicitly enter the Effect Planner": this
//! module *classifies and records* the tier (the cost class and, for isolation,
//! the layer reason) at ingest and hands it to the planner. It does not itself
//! read the destination or allocate a target — that realization is E2. The
//! implementation strategy is the spec's ladder: fixed-function where available,
//! else a backend destination-read fast path, else a bounded offscreen composite.
//!
//! This is a cold-path decision (§7.2), the same shape as the
//! [`clip`](crate::clip) and [`opacity`](crate::opacity) planners: the plan is
//! computed once when a drawable's blend is set, beside its [`EffectCost`], and
//! carries what tier the mode falls in.

use crate::EffectCost;
use crate::opacity::LayerReason;

/// A blend mode a drawable composites with (§14.6). The standard Porter-Duff
/// compositing operators, the separable artistic modes, and the non-separable
/// HSL modes — the full SVG/CSS `mix-blend-mode` set.
///
/// The variants are grouped by realization tier ([`realization`](Blend::realization)),
/// not alphabetically: the Porter-Duff + `Plus` block is fixed-function, the
/// separable artistic block reads the destination, and the HSL block isolates.
///
/// The discriminants are the shader ABI. `#[repr(u8)]` pins them so
/// [`mode`](Blend::mode) is a plain cast and the AdvancedBlend fragment can
/// branch on the same numbering: `0..=12` Porter-Duff + `Plus`, `13..=23` the
/// separable artistic modes, `24..=27` the HSL ones. Reordering the variants is
/// an ABI break, which is why a frozen test pins every number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Blend {
    // --- Fixed-function: Porter-Duff compositing + Plus ---
    /// Both source and destination cleared to zero within the bounds.
    Clear,
    /// Source replaces destination (`src`).
    Src,
    /// Destination kept, source discarded (`dst`).
    Dst,
    /// Source over destination — the default. `src + dst * (1 - src.a)`.
    #[default]
    SrcOver,
    /// Destination over source.
    DstOver,
    /// Source clipped to destination's coverage (`src * dst.a`).
    SrcIn,
    /// Destination clipped to source's coverage.
    DstIn,
    /// Source outside destination's coverage (`src * (1 - dst.a)`).
    SrcOut,
    /// Destination outside source's coverage.
    DstOut,
    /// Source atop destination (source where they overlap, destination elsewhere).
    SrcATop,
    /// Destination atop source.
    DstATop,
    /// Non-overlapping parts of both (`src * (1 - dst.a) + dst * (1 - src.a)`).
    Xor,
    /// Additive (`src + dst`), clamped. Lighter-color / "plus-lighter".
    Plus,

    // --- Separable artistic: destination read ---
    /// `src * dst` — darkens.
    Multiply,
    /// `1 - (1 - src)(1 - dst)` — lightens.
    Screen,
    /// Multiply or screen per channel depending on destination — contrast.
    Overlay,
    /// Per-channel minimum.
    Darken,
    /// Per-channel maximum.
    Lighten,
    /// Brightens destination to reflect source.
    ColorDodge,
    /// Darkens destination to reflect source.
    ColorBurn,
    /// Overlay with source and destination swapped.
    HardLight,
    /// A softer `HardLight`.
    SoftLight,
    /// Absolute per-channel difference.
    Difference,
    /// Like `Difference` with lower contrast.
    Exclusion,

    // --- Non-separable HSL: isolation ---
    /// Source hue, destination saturation and luminosity.
    Hue,
    /// Source saturation, destination hue and luminosity.
    Saturation,
    /// Source hue and saturation, destination luminosity.
    Color,
    /// Source luminosity, destination hue and saturation.
    Luminosity,
}

/// How a blend mode must be realized — the tier the classifier assigns (§14.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendRealization {
    /// Expressible by the hardware blender's factor/equation state: an in-place
    /// draw, [`EffectCost::Local`]. The Porter-Duff operators and `Plus`.
    FixedFunction,
    /// A separable function of source and destination the fixed-function blender
    /// cannot express: needs the destination sampled in-pass,
    /// [`EffectCost::DestinationRead`], via a backend subpass / framebuffer-fetch
    /// fast path. The separable artistic modes.
    DestinationRead,
    /// A non-separable mode that, applied to a group, composites an isolated
    /// offscreen layer: [`EffectCost::NeedsOffscreen`], tagged
    /// [`LayerReason::AdvancedBlend`]. The HSL modes.
    Isolation,
}

impl Blend {
    /// The realization tier of this mode (§14.6).
    pub fn realization(self) -> BlendRealization {
        use Blend::*;
        match self {
            Clear | Src | Dst | SrcOver | DstOver | SrcIn | DstIn | SrcOut | DstOut | SrcATop
            | DstATop | Xor | Plus => BlendRealization::FixedFunction,
            Multiply | Screen | Overlay | Darken | Lighten | ColorDodge | ColorBurn | HardLight
            | SoftLight | Difference | Exclusion => BlendRealization::DestinationRead,
            Hue | Saturation | Color | Luminosity => BlendRealization::Isolation,
        }
    }

    /// Whether this mode is the fixed-function hot path (Porter-Duff + `Plus`) —
    /// composited in place with no destination read or offscreen target. Every
    /// ordinary drawable is here.
    pub fn is_fixed_function(self) -> bool {
        matches!(self.realization(), BlendRealization::FixedFunction)
    }

    /// Whether this mode is *advanced* — a destination read or an isolation blend
    /// that must enter the Effect Planner (E2) rather than the `SrcOver` pipeline
    /// (§14.6). The complement of [`is_fixed_function`](Blend::is_fixed_function).
    pub fn is_advanced(self) -> bool {
        !self.is_fixed_function()
    }

    /// The blend discriminant handed to the AdvancedBlend shader — the ABI the
    /// fragment branches on. `0..=12` are the Porter-Duff coverage modes (linear
    /// in the premultiplied operands), `13..=23` the W3C separable artistic ones,
    /// `24..=27` the non-separable HSL ones, so the shader needs one comparison to
    /// pick a family and one switch inside it.
    ///
    /// A cast, not a table: `#[repr(u8)]` on [`Blend`] makes the declaration order
    /// the numbering, and `blend_mode_abi_is_frozen` pins every value.
    pub const fn mode(self) -> u32 {
        self as u32
    }
}

/// How a drawable's blend is realized: the mode, its cost class, and — when it
/// isolates — the layer reason recorded for the Effect Planner. Mirrors
/// [`OpacityPlan`](crate::opacity::OpacityPlan)'s shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlendPlan {
    /// The requested blend mode.
    pub mode: Blend,
    /// The realization cost class (§7.5).
    pub cost: EffectCost,
    /// The layer reason, present only when the mode isolates
    /// ([`LayerReason::AdvancedBlend`]).
    pub reason: Option<LayerReason>,
}

impl BlendPlan {
    /// Whether this plan needs an offscreen isolation pass.
    pub fn needs_offscreen(&self) -> bool {
        self.cost.needs_offscreen()
    }

    /// Whether this plan must read the destination within its pass.
    pub fn reads_destination(&self) -> bool {
        self.cost.reads_destination()
    }
}

/// Classify how a blend mode is realized (§14.6), mapping its tier onto a cost
/// class and, for an isolation blend, a [`LayerReason::AdvancedBlend`]:
///
/// - fixed-function → [`EffectCost::Local`], no layer;
/// - destination read → [`EffectCost::DestinationRead`], no layer (an in-pass
///   read, not an offscreen target);
/// - isolation → [`EffectCost::NeedsOffscreen`] + [`LayerReason::AdvancedBlend`].
///
/// The advanced tiers are recorded here and deferred to the Effect Planner; the
/// common [`SrcOver`](Blend::SrcOver) path stays [`Local`](EffectCost::Local).
pub fn plan_blend(mode: Blend) -> BlendPlan {
    match mode.realization() {
        BlendRealization::FixedFunction => BlendPlan {
            mode,
            cost: EffectCost::Local,
            reason: None,
        },
        BlendRealization::DestinationRead => BlendPlan {
            mode,
            cost: EffectCost::DestinationRead,
            reason: None,
        },
        BlendRealization::Isolation => BlendPlan {
            mode,
            cost: EffectCost::NeedsOffscreen,
            reason: Some(LayerReason::AdvancedBlend),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default blend is `SrcOver`, and it is the fixed-function hot path:
    /// local cost, no destination read, no layer — the common `SrcOver` pipeline
    /// is not polluted by the advanced machinery (§14.6).
    #[test]
    fn src_over_is_the_fixed_function_default() {
        assert_eq!(Blend::default(), Blend::SrcOver);
        let plan = plan_blend(Blend::SrcOver);
        assert_eq!(plan.cost, EffectCost::Local);
        assert!(Blend::SrcOver.is_fixed_function());
        assert!(!Blend::SrcOver.is_advanced());
        assert!(!plan.needs_offscreen());
        assert!(!plan.reads_destination());
        assert_eq!(plan.reason, None);
    }

    /// Every Porter-Duff operator plus `Plus` is fixed-function and local.
    #[test]
    fn porter_duff_and_plus_are_fixed_function() {
        let fixed = [
            Blend::Clear,
            Blend::Src,
            Blend::Dst,
            Blend::SrcOver,
            Blend::DstOver,
            Blend::SrcIn,
            Blend::DstIn,
            Blend::SrcOut,
            Blend::DstOut,
            Blend::SrcATop,
            Blend::DstATop,
            Blend::Xor,
            Blend::Plus,
        ];
        for mode in fixed {
            assert_eq!(
                mode.realization(),
                BlendRealization::FixedFunction,
                "{mode:?}"
            );
            assert_eq!(plan_blend(mode).cost, EffectCost::Local, "{mode:?}");
            assert!(mode.is_fixed_function(), "{mode:?}");
        }
    }

    /// The separable artistic modes read the destination in-pass: a destination
    /// read, not an offscreen target, no layer reason.
    #[test]
    fn separable_artistic_modes_read_destination() {
        let dst_read = [
            Blend::Multiply,
            Blend::Screen,
            Blend::Overlay,
            Blend::Darken,
            Blend::Lighten,
            Blend::ColorDodge,
            Blend::ColorBurn,
            Blend::HardLight,
            Blend::SoftLight,
            Blend::Difference,
            Blend::Exclusion,
        ];
        for mode in dst_read {
            assert_eq!(
                mode.realization(),
                BlendRealization::DestinationRead,
                "{mode:?}"
            );
            let plan = plan_blend(mode);
            assert_eq!(plan.cost, EffectCost::DestinationRead, "{mode:?}");
            assert!(plan.reads_destination(), "{mode:?}");
            assert!(!plan.needs_offscreen(), "{mode:?}");
            assert_eq!(plan.reason, None, "{mode:?}");
            assert!(mode.is_advanced(), "{mode:?}");
        }
    }

    /// The non-separable HSL modes isolate: an offscreen pass tagged with the
    /// advanced-blend layer reason for the Effect Planner.
    #[test]
    fn hsl_modes_isolate_with_advanced_blend_reason() {
        for mode in [
            Blend::Hue,
            Blend::Saturation,
            Blend::Color,
            Blend::Luminosity,
        ] {
            assert_eq!(mode.realization(), BlendRealization::Isolation, "{mode:?}");
            let plan = plan_blend(mode);
            assert_eq!(plan.cost, EffectCost::NeedsOffscreen, "{mode:?}");
            assert!(plan.needs_offscreen(), "{mode:?}");
            assert_eq!(plan.reason, Some(LayerReason::AdvancedBlend), "{mode:?}");
            assert!(mode.is_advanced(), "{mode:?}");
        }
    }

    /// The blend discriminant is the AdvancedBlend shader ABI, so every number is
    /// frozen: the shader dispatches on `mode <= 12` / `mode <= 23` / else, and the
    /// three ranges must stay contiguous and in tier order. Reordering a variant
    /// silently repaints, which is why this pins all 28 rather than samples.
    #[test]
    fn blend_mode_abi_is_frozen() {
        let frozen = [
            (Blend::Clear, 0),
            (Blend::Src, 1),
            (Blend::Dst, 2),
            (Blend::SrcOver, 3),
            (Blend::DstOver, 4),
            (Blend::SrcIn, 5),
            (Blend::DstIn, 6),
            (Blend::SrcOut, 7),
            (Blend::DstOut, 8),
            (Blend::SrcATop, 9),
            (Blend::DstATop, 10),
            (Blend::Xor, 11),
            (Blend::Plus, 12),
            (Blend::Multiply, 13),
            (Blend::Screen, 14),
            (Blend::Overlay, 15),
            (Blend::Darken, 16),
            (Blend::Lighten, 17),
            (Blend::ColorDodge, 18),
            (Blend::ColorBurn, 19),
            (Blend::HardLight, 20),
            (Blend::SoftLight, 21),
            (Blend::Difference, 22),
            (Blend::Exclusion, 23),
            (Blend::Hue, 24),
            (Blend::Saturation, 25),
            (Blend::Color, 26),
            (Blend::Luminosity, 27),
        ];
        for (mode, want) in frozen {
            assert_eq!(mode.mode(), want, "{mode:?}");
        }

        // The tier boundaries the shader's two comparisons rely on.
        for (mode, want) in frozen {
            let tier = match want {
                0..=12 => BlendRealization::FixedFunction,
                13..=23 => BlendRealization::DestinationRead,
                _ => BlendRealization::Isolation,
            };
            assert_eq!(mode.realization(), tier, "{mode:?}");
        }
    }

    /// Advanced modes dominate a chain shared with an ordinary local draw — the
    /// cost class composes through `EffectCost::dominating`.
    #[test]
    fn advanced_blend_dominates_a_local_chain() {
        let multiply = plan_blend(Blend::Multiply).cost;
        let hsl = plan_blend(Blend::Color).cost;
        assert_eq!(
            EffectCost::dominating([EffectCost::Local, multiply]),
            EffectCost::DestinationRead,
        );
        assert_eq!(
            EffectCost::dominating([EffectCost::Local, hsl]),
            EffectCost::NeedsOffscreen,
        );
    }
}
