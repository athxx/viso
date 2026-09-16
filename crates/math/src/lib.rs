//! `viso-math` — the allocation-free numeric and geometry foundation (DAG leaf).
//!
//! This is not a draw helper and not a new `core`/`utils` bucket. It defines the
//! basic numeric and geometry vocabulary shared across the UI, Render, Shader
//! interface, Text geometry, Input, and Animation subsystems:
//!
//! - [`Vec2`], [`Vec3`], [`Vec4`] — `f32` vectors, the primary precision;
//! - [`DVec2`] — an `f64` vector for the accuracy-sensitive UI-layout path,
//!   where large-coordinate accumulation plus DPI snapping would otherwise cause
//!   visible sub-pixel shimmer;
//! - [`Mat2`], [`Mat3`], [`Mat4`] — column-major matrices;
//! - [`Quat`], [`Affine2`], [`Transform3`] — rotation and 2D/3D transforms;
//! - [`Point`], [`Size`], [`Rect`], [`Insets`] (+ [`DPoint`], [`DRect`]);
//! - [`Ray`], [`Plane`], [`Aabb`] — basic spatial geometry.
//!
//! Hot-path contract (architecture doc, Math hot-path section): no heap
//! allocation, no `String`/`HashMap`/`Rc`/`Arc`/trait-object dispatch, and a
//! public data layout that does not depend on pointer width (no `usize`/`isize`
//! in a public struct), so 32-bit / 64-bit / wasm32 stay bit-identical. SIMD is
//! an internal optimization only; the public ABI is always the scalar layout,
//! and any SIMD kernel must match it bit-for-bit. The Rust memory layout here is
//! deliberately NOT a GPU uniform/instance wire ABI: uploads go through the
//! validated explicit layout owned by `viso-shader` / `viso-gpu`, and the wire
//! representation is defined by `viso-ende`. It depends on no other viso crate.
//!
//! Where this diverges from the reference framework's math library it does so on
//! purpose: `dot`/`cross`/quaternion product are methods (not associated
//! functions), matrices use a uniform flat storage story, and it adds the 2D
//! [`Affine2`] transform and [`Insets`] the reference lacks.

#![forbid(unsafe_op_in_unsafe_fn)]

mod color;
mod coverage;
mod dvec;
mod geom;
mod legality;
mod mat;
mod quat;
mod rect;
mod simd;
mod snap;
mod transform;
mod vec;

pub use color::{
    DisplayP3, ExtendMode, ExtendedLinear, InterpolationSpace, LinearPremul, LinearSrgb,
    LinearStraight, Srgb, linear_to_srgb, srgb_to_linear,
};
pub use coverage::{Coverage, composite};
pub use dvec::{DVec2, dvec2};
pub use geom::{Aabb, Plane, Ray};
pub use legality::{GeometryLegality, classify_extent, classify_rect, classify_transform};
pub use mat::{Mat2, Mat3, Mat4};
pub use quat::Quat;
pub use rect::{DPoint, DRect, Insets, Point, Rect, Size};
pub use snap::{Hairline, PixelSnap, snap_bounds, snap_device, snap_position, snap_stroke_center};
pub use transform::{Affine2, Transform3};
pub use vec::{Vec2, Vec3, Vec4, vec2, vec3, vec4};
