//! The `f32`/`f64` functions `core` lacks, for a build without `std`: the
//! same method names as `std`'s inherent ones, computed by [`libm`]. With
//! `std` the inherent methods are used and this module is not built.

/// `std`'s float methods `core` has no inherent version of.
pub(crate) trait FloatMath {
    fn sqrt(self) -> Self;
    fn floor(self) -> Self;
    fn ceil(self) -> Self;
    fn round(self) -> Self;
    fn fract(self) -> Self;
    fn sin(self) -> Self;
    fn cos(self) -> Self;
    fn exp(self) -> Self;
    fn exp2(self) -> Self;
    fn log2(self) -> Self;
    fn atan2(self, x: Self) -> Self;
    fn rem_euclid(self, rhs: Self) -> Self;
}

impl FloatMath for f32 {
    fn sqrt(self) -> f32 {
        libm::sqrtf(self)
    }
    fn floor(self) -> f32 {
        libm::floorf(self)
    }
    fn ceil(self) -> f32 {
        libm::ceilf(self)
    }
    fn round(self) -> f32 {
        libm::roundf(self)
    }
    fn fract(self) -> f32 {
        self - libm::truncf(self)
    }
    fn sin(self) -> f32 {
        libm::sinf(self)
    }
    fn cos(self) -> f32 {
        libm::cosf(self)
    }
    fn exp(self) -> f32 {
        libm::expf(self)
    }
    fn exp2(self) -> f32 {
        libm::exp2f(self)
    }
    fn log2(self) -> f32 {
        libm::log2f(self)
    }
    fn atan2(self, x: f32) -> f32 {
        libm::atan2f(self, x)
    }
    fn rem_euclid(self, rhs: f32) -> f32 {
        let r = self % rhs;
        if r < 0.0 { r + rhs.abs() } else { r }
    }
}

impl FloatMath for f64 {
    fn sqrt(self) -> f64 {
        libm::sqrt(self)
    }
    fn floor(self) -> f64 {
        libm::floor(self)
    }
    fn ceil(self) -> f64 {
        libm::ceil(self)
    }
    fn round(self) -> f64 {
        libm::round(self)
    }
    fn fract(self) -> f64 {
        self - libm::trunc(self)
    }
    fn sin(self) -> f64 {
        libm::sin(self)
    }
    fn cos(self) -> f64 {
        libm::cos(self)
    }
    fn exp(self) -> f64 {
        libm::exp(self)
    }
    fn exp2(self) -> f64 {
        libm::exp2(self)
    }
    fn log2(self) -> f64 {
        libm::log2(self)
    }
    fn atan2(self, x: f64) -> f64 {
        libm::atan2(self, x)
    }
    fn rem_euclid(self, rhs: f64) -> f64 {
        let r = self % rhs;
        if r < 0.0 { r + rhs.abs() } else { r }
    }
}
