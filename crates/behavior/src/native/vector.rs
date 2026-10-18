//! `viso::math` vectors: `Vec2F32` and `Vec3F32`, native value types of `F32`
//! components, carried as the `viso-math` [`Vec2`] and [`Vec3`]. As plain values they compare by content, live in any state
//! and snapshot; their arithmetic is IEEE single precision with no fused
//! operations, so it reproduces bit for bit on every target.

use alloc::rc::Rc;

use super::{NativeFunction, NativeValue, SchemaTy};
use crate::value::{Aggregate, Value};

pub use viso_math::{Vec2, Vec3};

/// The full path of the `Vec2F32` native value type.
pub const VEC2_PATH: &str = "viso::math::Vec2F32";

/// The full path of the `Vec3F32` native value type.
pub const VEC3_PATH: &str = "viso::math::Vec3F32";

/// The `N` components of a vector value.
fn components<const N: usize>(value: &Value) -> Option<[f32; N]> {
    let Value::Agg(agg) = value else {
        return None;
    };
    let fields: &[Value; N] = agg.fields[..].try_into().ok()?;
    let mut out = [0.0; N];
    for (out, field) in out.iter_mut().zip(fields) {
        *out = field.as_float()? as f32;
    }
    Some(out)
}

fn aggregate(components: &[f32]) -> Value {
    Value::Agg(Rc::new(Aggregate {
        tag: 0,
        fields: components
            .iter()
            .map(|&c| Value::Float(f64::from(c)))
            .collect(),
    }))
}

impl NativeValue for Vec2 {
    const TY: SchemaTy = SchemaTy::Value(VEC2_PATH);

    fn from_value(value: &Value) -> Option<Vec2> {
        components(value).map(|[x, y]| Vec2::new(x, y))
    }

    fn into_value(self) -> Value {
        aggregate(&[self.x, self.y])
    }
}

impl NativeValue for Vec3 {
    const TY: SchemaTy = SchemaTy::Value(VEC3_PATH);

    fn from_value(value: &Value) -> Option<Vec3> {
        components(value).map(|[x, y, z]| Vec3::new(x, y, z))
    }

    fn into_value(self) -> Value {
        aggregate(&[self.x, self.y, self.z])
    }
}

pub(super) static VEC2_METHODS: [NativeFunction; 7] = [
    crate::native!(fn "new" |_cx, x: f32, y: f32| -> Vec2 { Ok(Vec2::new(x, y)) })
        .constant()
        .realtime_safe(),
    crate::native!(fn "x" |_cx, this: Vec2| -> f32 { Ok(this.x) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "y" |_cx, this: Vec2| -> f32 { Ok(this.y) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "add" |_cx, this: Vec2, other: Vec2| -> Vec2 {
        Ok(Vec2::new(this.x + other.x, this.y + other.y))
    })
    .deterministic(),
    crate::native!(fn "sub" |_cx, this: Vec2, other: Vec2| -> Vec2 {
        Ok(Vec2::new(this.x - other.x, this.y - other.y))
    })
    .deterministic(),
    crate::native!(fn "scale" |_cx, this: Vec2, factor: f32| -> Vec2 {
        Ok(Vec2::new(this.x * factor, this.y * factor))
    })
    .deterministic(),
    crate::native!(fn "length" |_cx, this: Vec2| -> f32 { Ok(this.length()) })
        .deterministic()
        .realtime_safe(),
];

pub(super) static VEC3_METHODS: [NativeFunction; 8] = [
    crate::native!(fn "new" |_cx, x: f32, y: f32, z: f32| -> Vec3 {
        Ok(Vec3::new(x, y, z))
    })
    .constant()
    .realtime_safe(),
    crate::native!(fn "x" |_cx, this: Vec3| -> f32 { Ok(this.x) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "y" |_cx, this: Vec3| -> f32 { Ok(this.y) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "z" |_cx, this: Vec3| -> f32 { Ok(this.z) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "add" |_cx, this: Vec3, other: Vec3| -> Vec3 {
        Ok(Vec3::new(this.x + other.x, this.y + other.y, this.z + other.z))
    })
    .deterministic(),
    crate::native!(fn "sub" |_cx, this: Vec3, other: Vec3| -> Vec3 {
        Ok(Vec3::new(this.x - other.x, this.y - other.y, this.z - other.z))
    })
    .deterministic(),
    crate::native!(fn "scale" |_cx, this: Vec3, factor: f32| -> Vec3 {
        Ok(Vec3::new(this.x * factor, this.y * factor, this.z * factor))
    })
    .deterministic(),
    crate::native!(fn "length" |_cx, this: Vec3| -> f32 { Ok(this.length()) })
        .deterministic()
        .realtime_safe(),
];
