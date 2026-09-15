//! Coverage: the single `output = premul * coverage` identity.
//!
//! Coverage is a scalar `[0, 1]` describing how much of a pixel a primitive
//! occupies — analytic anti-aliasing weight, a path AA fringe, or a glyph's A8
//! mask value. Every one of those routes through [`composite`], which scales a
//! premultiplied color by coverage. Because the color is premultiplied, scaling
//! all four channels by the same coverage keeps it a valid premultiplied color
//! and makes partial coverage compose correctly under source-over. Coverage is
//! never applied to a straight (non-premultiplied) color — that would darken
//! toward black at the edges instead of fading to transparent.

use crate::color::LinearPremul;

/// A pixel-coverage / opacity scalar, clamped to `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Coverage(f32);

impl Coverage {
    /// No coverage.
    pub const ZERO: Coverage = Coverage(0.0);
    /// Full coverage.
    pub const FULL: Coverage = Coverage(1.0);

    /// Construct from a scalar, clamping to `[0, 1]` (NaN maps to `0`).
    #[inline]
    pub fn new(v: f32) -> Coverage {
        // Not `clamp`: `f32::clamp` propagates NaN, but a NaN coverage must
        // collapse to no coverage. `NaN.max(0.0)` is `0.0` on the std impl, so
        // `max` then `min` maps NaN to `0` instead of letting it through.
        #[allow(clippy::manual_clamp)]
        Coverage(v.max(0.0).min(1.0))
    }

    /// The clamped scalar value.
    #[inline]
    pub fn get(self) -> f32 {
        self.0
    }
}

/// The canonical coverage composite: scale a premultiplied color by coverage.
/// `output = premul * coverage`, applied uniformly to RGB **and** alpha.
#[inline]
pub fn composite(premul: LinearPremul, coverage: Coverage) -> LinearPremul {
    premul.scale(coverage.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_range() {
        assert_eq!(Coverage::new(-1.0).get(), 0.0);
        assert_eq!(Coverage::new(2.0).get(), 1.0);
        assert_eq!(Coverage::new(0.5).get(), 0.5);
    }

    #[test]
    fn nan_maps_to_zero() {
        assert_eq!(Coverage::new(f32::NAN).get(), 0.0);
    }

    #[test]
    fn composite_full_is_identity() {
        let p = LinearPremul::new(0.4, 0.3, 0.2, 0.5);
        assert_eq!(composite(p, Coverage::FULL), p);
    }

    #[test]
    fn composite_zero_is_transparent() {
        let p = LinearPremul::new(0.4, 0.3, 0.2, 0.5);
        assert_eq!(composite(p, Coverage::ZERO), LinearPremul::TRANSPARENT);
    }

    #[test]
    fn composite_scales_all_channels() {
        let p = LinearPremul::new(0.4, 0.2, 0.1, 0.8);
        let out = composite(p, Coverage::new(0.5));
        assert_eq!(out, LinearPremul::new(0.2, 0.1, 0.05, 0.4));
    }
}
