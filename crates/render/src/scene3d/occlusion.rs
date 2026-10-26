//! Screen-space ambient occlusion: how the creases and contacts of a
//! scene's surfaces keep the environment's light out.
//!
//! The view's depth is drawn at half the target's size by the depth
//! program; the occlusion program takes a dozen hemisphere taps about every
//! texel's surface against it, and a depth-aware blur along each axis
//! smooths the taps' noise without crossing a silhouette. The mesh program
//! scales the environment's light — never the direct light — by the
//! result at its pixel.

/// How strongly, and over what reach, a scene's surfaces occlude the
/// environment's light from each other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmbientOcclusion {
    /// How much of the environment's light full occlusion takes away: `1.0`
    /// all of it, `0.0` none — and then no occlusion is drawn at all.
    pub strength: f32,
    /// How far, in world units, a surface reaches to occlude another: the
    /// size of a contact's shading. It is a size in the world, so it keeps
    /// as the camera moves.
    pub radius: f32,
    /// How far, in world units, a surface must stand in front of another
    /// to occlude it at least: surfaces closer together than this (a decal,
    /// layers of one wall) never shade each other.
    pub bias: f32,
}

impl Default for AmbientOcclusion {
    fn default() -> AmbientOcclusion {
        AmbientOcclusion {
            strength: 0.7,
            radius: 0.5,
            bias: 0.02,
        }
    }
}

impl AmbientOcclusion {
    /// No ambient occlusion.
    pub const NONE: AmbientOcclusion = AmbientOcclusion {
        strength: 0.0,
        radius: 0.0,
        bias: 0.0,
    };
}

/// How much farther in front a surface must stand to occlude, per unit of
/// its distance from the camera, so grazing surfaces far away stay clean.
pub(crate) const BIAS_PER_DISTANCE: f32 = 0.002;
