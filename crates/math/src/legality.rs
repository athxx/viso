//! Geometry legality: classify degenerate and non-finite inputs at the
//! commit / cold boundary.
//!
//! Illegal geometry — a NaN coordinate, an infinite extent, a negative size, a
//! singular transform, a zero-area shape — must be rejected before it reaches
//! the renderer, but the rejection happens **once**, where geometry is committed
//! (a cold path), not with a per-operation `is_finite()` sprinkled through the
//! hot rasterization loop. These classifiers return a verdict; the caller
//! fast-rejects the primitive (skips it) rather than panicking. A degenerate
//! shape is legal-but-empty (nothing to draw), which is distinct from an illegal
//! (corrupt) input.

use crate::transform::Affine2;

/// The verdict for a piece of committed geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryLegality {
    /// Finite, non-degenerate — safe to rasterize.
    Ok,
    /// A coordinate is NaN.
    NaN,
    /// A coordinate is ±infinity.
    NonFinite,
    /// An extent is negative, or an origin+extent overflows to non-finite.
    IllegalExtent,
    /// A transform's linear part is singular (determinant ≈ 0) — it collapses
    /// area to a line or point and cannot be inverted.
    SingularTransform,
    /// Finite and legal, but empty (zero area / zero-length) — nothing to draw.
    Degenerate,
}

impl GeometryLegality {
    /// Whether this verdict means the primitive should be rasterized.
    /// `Degenerate` and every illegal verdict are fast-rejected.
    #[inline]
    pub fn is_drawable(self) -> bool {
        matches!(self, GeometryLegality::Ok)
    }

    /// Whether the input was corrupt (illegal), as opposed to legal-but-empty.
    #[inline]
    pub fn is_illegal(self) -> bool {
        matches!(
            self,
            GeometryLegality::NaN
                | GeometryLegality::NonFinite
                | GeometryLegality::IllegalExtent
                | GeometryLegality::SingularTransform
        )
    }
}

/// Classify an axis-aligned box given as origin + extent components. Field
/// layout is intentionally not a specific `Rect` type — both the layout-side
/// (`origin`/`size`) and render-side (`x`/`y`/`w`/`h`) rectangles feed the same
/// classifier.
#[inline]
pub fn classify_rect(x: f32, y: f32, w: f32, h: f32) -> GeometryLegality {
    if x.is_nan() || y.is_nan() || w.is_nan() || h.is_nan() {
        return GeometryLegality::NaN;
    }
    if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() {
        return GeometryLegality::NonFinite;
    }
    if w < 0.0 || h < 0.0 || !(x + w).is_finite() || !(y + h).is_finite() {
        return GeometryLegality::IllegalExtent;
    }
    if w == 0.0 || h == 0.0 {
        return GeometryLegality::Degenerate;
    }
    GeometryLegality::Ok
}

/// Classify a scalar extent (a width, height, radius, or stroke width) that must
/// be finite and non-negative.
#[inline]
pub fn classify_extent(v: f32) -> GeometryLegality {
    if v.is_nan() {
        GeometryLegality::NaN
    } else if !v.is_finite() {
        GeometryLegality::NonFinite
    } else if v < 0.0 {
        GeometryLegality::IllegalExtent
    } else if v == 0.0 {
        GeometryLegality::Degenerate
    } else {
        GeometryLegality::Ok
    }
}

/// Classify a 2D affine transform: reject non-finite entries and a singular
/// linear part (which would flatten geometry and has no inverse).
#[inline]
pub fn classify_transform(t: &Affine2) -> GeometryLegality {
    let m = &t.matrix;
    let tx = t.translation;
    let all = [m[0], m[1], m[2], m[3], tx.x, tx.y];
    if all.iter().any(|v| v.is_nan()) {
        return GeometryLegality::NaN;
    }
    if all.iter().any(|v| !v.is_finite()) {
        return GeometryLegality::NonFinite;
    }
    let det = m[0] * m[3] - m[2] * m[1];
    if det == 0.0 {
        return GeometryLegality::SingularTransform;
    }
    GeometryLegality::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::Affine2;
    use crate::vec::Vec2;

    #[test]
    fn ok_rect_is_drawable() {
        assert_eq!(classify_rect(0.0, 0.0, 10.0, 4.0), GeometryLegality::Ok);
        assert!(classify_rect(0.0, 0.0, 10.0, 4.0).is_drawable());
    }

    #[test]
    fn nan_rect() {
        assert_eq!(
            classify_rect(f32::NAN, 0.0, 1.0, 1.0),
            GeometryLegality::NaN
        );
    }

    #[test]
    fn infinite_rect() {
        assert_eq!(
            classify_rect(0.0, 0.0, f32::INFINITY, 1.0),
            GeometryLegality::NonFinite
        );
    }

    #[test]
    fn negative_extent_is_illegal() {
        assert_eq!(
            classify_rect(0.0, 0.0, -1.0, 1.0),
            GeometryLegality::IllegalExtent
        );
    }

    #[test]
    fn zero_area_is_degenerate_not_illegal() {
        let v = classify_rect(0.0, 0.0, 0.0, 5.0);
        assert_eq!(v, GeometryLegality::Degenerate);
        assert!(!v.is_drawable() && !v.is_illegal());
    }

    #[test]
    fn extent_classification() {
        assert_eq!(classify_extent(2.0), GeometryLegality::Ok);
        assert_eq!(classify_extent(0.0), GeometryLegality::Degenerate);
        assert_eq!(classify_extent(-1.0), GeometryLegality::IllegalExtent);
        assert_eq!(classify_extent(f32::NAN), GeometryLegality::NaN);
        assert_eq!(classify_extent(f32::INFINITY), GeometryLegality::NonFinite);
    }

    #[test]
    fn identity_transform_ok() {
        assert_eq!(classify_transform(&Affine2::IDENTITY), GeometryLegality::Ok);
    }

    #[test]
    fn singular_transform_rejected() {
        let degenerate = Affine2 {
            matrix: [0.0, 0.0, 0.0, 0.0],
            translation: Vec2::ZERO,
        };
        assert_eq!(
            classify_transform(&degenerate),
            GeometryLegality::SingularTransform
        );
    }

    #[test]
    fn nan_transform_rejected() {
        let bad = Affine2 {
            matrix: [f32::NAN, 0.0, 0.0, 1.0],
            translation: Vec2::ZERO,
        };
        assert_eq!(classify_transform(&bad), GeometryLegality::NaN);
    }
}
