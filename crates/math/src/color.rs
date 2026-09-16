//! Color: the canonical blend representation and the input color spaces that
//! feed it.
//!
//! # Canonical blend representation
//!
//! All compositing in Viso happens in **linear-light, premultiplied-alpha**
//! space ([`LinearPremul`]). This is the only representation the renderer,
//! rasterizer, and blend hardware agree on: blur, gradients, coverage
//! anti-aliasing, and source-over compositing are only correct in linear light,
//! and premultiplication is what makes `src + dst * (1 - src.a)` associative
//! across nested layers. There is deliberately no sRGB-space blend path — a
//! gamma-encoded value cannot be handed to [`composite`](crate::composite) or a
//! blend, because it is not a [`LinearPremul`].
//!
//! # Wire representation
//!
//! GPU instance data and the primitive value types carry **straight**
//! (non-premultiplied) linear RGBA ([`LinearStraight`]); the fragment shader (or
//! the software rasterizer) premultiplies at the point of blend. Straight linear
//! is also how authors reason about a color independent of its opacity. The two
//! are one multiply apart ([`LinearStraight::premultiply`] /
//! [`LinearPremul::unpremultiply`]).
//!
//! # Input spaces
//!
//! Author- and asset-facing colors arrive in one of several encodings —
//! gamma-encoded sRGB, Display-P3, already-linear sRGB, or extended-range linear
//! — each a distinct type ([`Srgb`], [`DisplayP3`], [`LinearSrgb`],
//! [`ExtendedLinear`]). Every one exposes `into_linear_straight()` and
//! `into_linear_premul()`; there is no implicit conversion and no ambiguous
//! "just a float4" that could skip the transfer function.

/// Linear-light, **premultiplied**-alpha RGBA — the canonical blend
/// representation. RGB channels are already scaled by alpha, so channel values
/// satisfy `0 <= rgb <= a` for in-gamut opaque-or-translucent colors (extended
/// range may exceed `a`). This is the only type [`composite`](crate::composite)
/// and blend math operate on.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LinearPremul {
    /// Red, premultiplied by alpha.
    pub r: f32,
    /// Green, premultiplied by alpha.
    pub g: f32,
    /// Blue, premultiplied by alpha.
    pub b: f32,
    /// Alpha `[0, 1]`.
    pub a: f32,
}

/// Straight (non-premultiplied) linear RGBA — the wire shape carried by GPU
/// instance data and primitive value types. RGB is independent of `a`; the
/// fragment stage premultiplies before blending.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LinearStraight {
    /// Red, linear `[0, 1]` (extended range may exceed 1).
    pub r: f32,
    /// Green, linear.
    pub g: f32,
    /// Blue, linear.
    pub b: f32,
    /// Alpha `[0, 1]`.
    pub a: f32,
}

impl LinearStraight {
    /// Fully transparent.
    pub const TRANSPARENT: LinearStraight = LinearStraight {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// Construct from straight linear components.
    #[inline]
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> LinearStraight {
        LinearStraight { r, g, b, a }
    }

    /// To the canonical premultiplied representation (`rgb * a`).
    #[inline]
    pub fn premultiply(self) -> LinearPremul {
        LinearPremul {
            r: self.r * self.a,
            g: self.g * self.a,
            b: self.b * self.a,
            a: self.a,
        }
    }
}

impl LinearPremul {
    /// Fully transparent.
    pub const TRANSPARENT: LinearPremul = LinearPremul {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// Construct from premultiplied linear components.
    #[inline]
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> LinearPremul {
        LinearPremul { r, g, b, a }
    }

    /// Recover straight linear (`rgb / a`), returning transparent black when
    /// `a == 0` (the only case where straight color is undefined).
    #[inline]
    pub fn unpremultiply(self) -> LinearStraight {
        if self.a == 0.0 {
            LinearStraight::TRANSPARENT
        } else {
            let inv = 1.0 / self.a;
            LinearStraight {
                r: self.r * inv,
                g: self.g * inv,
                b: self.b * inv,
                a: self.a,
            }
        }
    }

    /// Source-over composite of `self` over `dst`, both premultiplied linear:
    /// `out = src + dst * (1 - src.a)`.
    #[inline]
    pub fn over(self, dst: LinearPremul) -> LinearPremul {
        let inv = 1.0 - self.a;
        LinearPremul {
            r: self.r + dst.r * inv,
            g: self.g + dst.g * inv,
            b: self.b + dst.b * inv,
            a: self.a + dst.a * inv,
        }
    }

    /// Scale all channels (including alpha) by a coverage/opacity scalar. This
    /// is the operation behind [`composite`](crate::composite) — scaling a
    /// premultiplied color keeps it premultiplied.
    #[inline]
    pub fn scale(self, s: f32) -> LinearPremul {
        LinearPremul {
            r: self.r * s,
            g: self.g * s,
            b: self.b * s,
            a: self.a * s,
        }
    }
}

/// One sRGB gamma-encoded channel value `[0, 1]` to linear light.
///
/// IEC 61966-2-1 piecewise transfer: a linear toe below the breakpoint, a
/// gamma-2.4 curve above.
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The inverse of [`srgb_to_linear`]: one linear-light channel `[0, 1]` back to
/// sRGB gamma-encoded.
#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// A gamma-encoded sRGB color (the default author/asset space). Components are
/// `[0, 1]`; alpha is linear (not gamma-encoded), per the sRGB convention.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Srgb {
    /// Red, gamma-encoded `[0, 1]`.
    pub r: f32,
    /// Green, gamma-encoded.
    pub g: f32,
    /// Blue, gamma-encoded.
    pub b: f32,
    /// Alpha, linear `[0, 1]`.
    pub a: f32,
}

impl Srgb {
    /// From 8-bit sRGB channels (`0..=255`) with a separate alpha byte.
    #[inline]
    pub fn from_u8(r: u8, g: u8, b: u8, a: u8) -> Srgb {
        Srgb {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    /// From a packed `0xRRGGBBAA` value.
    #[inline]
    pub fn from_rgba32(v: u32) -> Srgb {
        Srgb::from_u8((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8)
    }

    /// To straight linear RGBA (transfer function on RGB, alpha unchanged).
    #[inline]
    pub fn into_linear_straight(self) -> LinearStraight {
        LinearStraight {
            r: srgb_to_linear(self.r),
            g: srgb_to_linear(self.g),
            b: srgb_to_linear(self.b),
            a: self.a,
        }
    }

    /// To the canonical premultiplied linear representation.
    #[inline]
    pub fn into_linear_premul(self) -> LinearPremul {
        self.into_linear_straight().premultiply()
    }
}

/// A Display-P3 color (gamma-encoded with the sRGB transfer, wide-gamut
/// primaries). Converts to linear-sRGB primaries so the whole pipeline shares
/// one working space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayP3 {
    /// Red, gamma-encoded `[0, 1]`.
    pub r: f32,
    /// Green, gamma-encoded.
    pub g: f32,
    /// Blue, gamma-encoded.
    pub b: f32,
    /// Alpha, linear `[0, 1]`.
    pub a: f32,
}

impl DisplayP3 {
    /// To straight linear-sRGB RGBA: sRGB transfer per channel, then the
    /// P3→sRGB primary matrix. Values may fall outside `[0, 1]` for colors
    /// outside the sRGB gamut (extended-range linear).
    #[inline]
    pub fn into_linear_straight(self) -> LinearStraight {
        let r = srgb_to_linear(self.r);
        let g = srgb_to_linear(self.g);
        let b = srgb_to_linear(self.b);
        // Display-P3 linear -> linear sRGB (D65), Bradford-adapted.
        LinearStraight {
            r: 1.224_940_2 * r - 0.224_940_18 * g + 0.0 * b,
            g: -0.042_056_955 * r + 1.042_057 * g + 0.0 * b,
            b: -0.019_637_555 * r - 0.078_636_04 * g + 1.098_273_6 * b,
            a: self.a,
        }
    }

    /// To the canonical premultiplied linear representation.
    #[inline]
    pub fn into_linear_premul(self) -> LinearPremul {
        self.into_linear_straight().premultiply()
    }
}

/// Already-linear sRGB-primaries RGBA in `[0, 1]` (e.g. a color computed in
/// linear space or decoded from a linear asset).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearSrgb {
    /// Red, linear `[0, 1]`.
    pub r: f32,
    /// Green, linear.
    pub g: f32,
    /// Blue, linear.
    pub b: f32,
    /// Alpha `[0, 1]`.
    pub a: f32,
}

impl LinearSrgb {
    /// To straight linear (identity on RGB; this space *is* the working space).
    #[inline]
    pub fn into_linear_straight(self) -> LinearStraight {
        LinearStraight {
            r: self.r,
            g: self.g,
            b: self.b,
            a: self.a,
        }
    }

    /// To the canonical premultiplied linear representation.
    #[inline]
    pub fn into_linear_premul(self) -> LinearPremul {
        self.into_linear_straight().premultiply()
    }
}

/// Extended-range linear sRGB-primaries RGBA: linear light with channel values
/// permitted outside `[0, 1]` (HDR, wide-gamut, or intermediate results).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExtendedLinear {
    /// Red, linear (unbounded).
    pub r: f32,
    /// Green, linear (unbounded).
    pub g: f32,
    /// Blue, linear (unbounded).
    pub b: f32,
    /// Alpha `[0, 1]`.
    pub a: f32,
}

impl ExtendedLinear {
    /// To straight linear (identity on RGB; range is preserved, not clamped).
    #[inline]
    pub fn into_linear_straight(self) -> LinearStraight {
        LinearStraight {
            r: self.r,
            g: self.g,
            b: self.b,
            a: self.a,
        }
    }

    /// To the canonical premultiplied linear representation.
    #[inline]
    pub fn into_linear_premul(self) -> LinearPremul {
        self.into_linear_straight().premultiply()
    }
}

/// The color space a gradient's stops are interpolated *in* — an explicit
/// author choice, never inferred from the target texture format or color-target
/// class (§12.3). Interpolating the same two stops in different spaces yields
/// visibly different midtones (linear-RGB darkens the midpoint of a
/// red→green ramp; gamma sRGB keeps it brighter), so the space is part of the
/// gradient's identity and its baked LUT key.
///
/// The renderer bakes stops into a linear-light premultiplied LUT regardless of
/// the interpolation space; the space only chooses *where* the per-texel lerp
/// happens before the result is stored linear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InterpolationSpace {
    /// Interpolate directly in linear light (the working space): lerp the
    /// already-linear stop colors, store linear. The native, cheapest path and
    /// the default.
    #[default]
    LinearRgb,
    /// Interpolate in gamma-encoded sRGB: encode each stop to sRGB, lerp there,
    /// then decode each interpolated texel back to linear for storage. Matches
    /// the "web" gradient look.
    Srgb,
    /// Interpolate in the perceptual OkLab space. Reserved; not implemented in
    /// D2.1 — constructing a gradient with this space is rejected at bake time.
    OkLab,
}

/// How a gradient samples parameter values outside the `[0, 1]` stop range
/// (§12.2). Applied to the gradient parameter `t` before the LUT/stop lookup;
/// distinct from a texture address mode, which wraps texel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExtendMode {
    /// Clamp `t` to `[0, 1]` — the end stops extend outward.
    #[default]
    Clamp,
    /// Repeat the `[0, 1]` ramp (`fract(t)`), tiling the gradient.
    Repeat,
    /// Mirror the ramp on each repeat (triangle wave), so adjacent tiles share
    /// an edge color with no seam.
    Mirror,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[inline]
    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn srgb_linear_round_trip() {
        for i in 0..=255u32 {
            let c = i as f32 / 255.0;
            let round = linear_to_srgb(srgb_to_linear(c));
            assert!(close(c, round), "c={c} round={round}");
        }
    }

    #[test]
    fn srgb_transfer_endpoints_and_breakpoint() {
        assert!(close(srgb_to_linear(0.0), 0.0));
        assert!(close(srgb_to_linear(1.0), 1.0));
        // Continuity across the piecewise breakpoint.
        let toe = srgb_to_linear(0.040_45);
        let curve = ((0.040_45 + 0.055) / 1.055f32).powf(2.4);
        assert!((toe - curve).abs() < 1e-4);
    }

    #[test]
    fn premultiply_unpremultiply_identity() {
        let s = LinearStraight::new(0.8, 0.4, 0.2, 0.5);
        let round = s.premultiply().unpremultiply();
        assert!(close(s.r, round.r) && close(s.g, round.g) && close(s.b, round.b));
        assert!(close(s.a, round.a));
    }

    #[test]
    fn unpremultiply_zero_alpha_is_transparent() {
        let p = LinearPremul::new(0.0, 0.0, 0.0, 0.0);
        assert_eq!(p.unpremultiply(), LinearStraight::TRANSPARENT);
    }

    #[test]
    fn premultiply_scales_rgb_by_alpha() {
        let p = LinearStraight::new(1.0, 0.5, 0.25, 0.5).premultiply();
        assert!(close(p.r, 0.5) && close(p.g, 0.25) && close(p.b, 0.125) && close(p.a, 0.5));
    }

    #[test]
    fn over_transparent_is_dst() {
        let dst = LinearPremul::new(0.3, 0.2, 0.1, 0.7);
        let out = LinearPremul::TRANSPARENT.over(dst);
        assert_eq!(out, dst);
    }

    #[test]
    fn over_opaque_is_src() {
        let src = LinearPremul::new(0.3, 0.2, 0.1, 1.0);
        let dst = LinearPremul::new(0.9, 0.9, 0.9, 1.0);
        let out = src.over(dst);
        assert_eq!(out, src);
    }

    #[test]
    fn srgb_white_is_linear_one() {
        let l = Srgb::from_u8(255, 255, 255, 255).into_linear_straight();
        assert!(close(l.r, 1.0) && close(l.g, 1.0) && close(l.b, 1.0) && close(l.a, 1.0));
    }

    #[test]
    fn p3_maps_neutral_gray_to_same_gray() {
        // A neutral (equal-channel) P3 color stays neutral in linear sRGB.
        let l = DisplayP3 {
            r: 0.5,
            g: 0.5,
            b: 0.5,
            a: 1.0,
        }
        .into_linear_straight();
        assert!((l.r - l.g).abs() < 1e-4 && (l.g - l.b).abs() < 1e-4);
    }

    #[test]
    fn interpolation_space_defaults_to_linear() {
        assert_eq!(InterpolationSpace::default(), InterpolationSpace::LinearRgb);
    }

    #[test]
    fn extend_mode_defaults_to_clamp() {
        assert_eq!(ExtendMode::default(), ExtendMode::Clamp);
    }

    #[test]
    fn linear_srgb_is_identity_on_rgb() {
        let l = LinearSrgb {
            r: 0.3,
            g: 0.6,
            b: 0.9,
            a: 0.5,
        }
        .into_linear_straight();
        assert_eq!(l, LinearStraight::new(0.3, 0.6, 0.9, 0.5));
    }
}
