//! The `f32` functions `core` lacks: the platform's through `std` where the
//! build has it, else `libm`'s, so a `no_std` target computes the same
//! formulas.

#[cfg(feature = "std")]
mod imp {
    #[inline]
    pub fn sqrt(x: f32) -> f32 {
        x.sqrt()
    }
    #[inline]
    pub fn round(x: f32) -> f32 {
        x.round()
    }
    #[inline]
    pub fn sin(x: f32) -> f32 {
        x.sin()
    }
    #[inline]
    pub fn tan(x: f32) -> f32 {
        x.tan()
    }
    #[inline]
    pub fn acos(x: f32) -> f32 {
        x.acos()
    }
    #[inline]
    pub fn sin_cos(x: f32) -> (f32, f32) {
        x.sin_cos()
    }
    #[inline]
    pub fn powf(x: f32, y: f32) -> f32 {
        x.powf(y)
    }
    #[inline]
    pub fn exp(x: f32) -> f32 {
        x.exp()
    }
}

#[cfg(not(feature = "std"))]
mod imp {
    #[inline]
    pub fn sqrt(x: f32) -> f32 {
        libm::sqrtf(x)
    }
    #[inline]
    pub fn round(x: f32) -> f32 {
        libm::roundf(x)
    }
    #[inline]
    pub fn sin(x: f32) -> f32 {
        libm::sinf(x)
    }
    #[inline]
    pub fn tan(x: f32) -> f32 {
        libm::tanf(x)
    }
    #[inline]
    pub fn acos(x: f32) -> f32 {
        libm::acosf(x)
    }
    #[inline]
    pub fn sin_cos(x: f32) -> (f32, f32) {
        libm::sincosf(x)
    }
    #[inline]
    pub fn powf(x: f32, y: f32) -> f32 {
        libm::powf(x, y)
    }
    #[inline]
    pub fn exp(x: f32) -> f32 {
        libm::expf(x)
    }
}

pub(crate) use imp::{acos, exp, powf, round, sin, sin_cos, sqrt, tan};
