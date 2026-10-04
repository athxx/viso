//! `viso::math` vectors: `Vec2F32` and `Vec3F32`, native value types of `F32`
//! components. As plain values they compare by content, live in any state
//! and snapshot; their arithmetic is IEEE single precision with no fused
//! operations, so it reproduces bit for bit on every target.

use std::rc::Rc;

use super::{NativeFunction, NativeValue, SchemaTy};
use crate::value::{Aggregate, Value};

/// A 2D vector.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2F32 {
    /// Its x component.
    pub x: f32,
    /// Its y component.
    pub y: f32,
}

/// A 3D vector.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3F32 {
    /// Its x component.
    pub x: f32,
    /// Its y component, up.
    pub y: f32,
    /// Its z component.
    pub z: f32,
}

impl Vec2F32 {
    /// The full path of its native value type.
    pub const PATH: &'static str = "viso::math::Vec2F32";

    /// The vector `(x, y)`.
    pub const fn new(x: f32, y: f32) -> Vec2F32 {
        Vec2F32 { x, y }
    }

    /// Its length.
    pub fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
}

impl Vec3F32 {
    /// The full path of its native value type.
    pub const PATH: &'static str = "viso::math::Vec3F32";

    /// The vector `(x, y, z)`.
    pub const fn new(x: f32, y: f32, z: f32) -> Vec3F32 {
        Vec3F32 { x, y, z }
    }

    /// Its components.
    pub const fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }

    /// The vector of `components`.
    pub const fn from_array([x, y, z]: [f32; 3]) -> Vec3F32 {
        Vec3F32 { x, y, z }
    }

    /// Its length.
    pub fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    /// The point `t` of the way from `self` to `to`.
    pub fn lerp(self, to: Vec3F32, t: f32) -> Vec3F32 {
        Vec3F32 {
            x: self.x + (to.x - self.x) * t,
            y: self.y + (to.y - self.y) * t,
            z: self.z + (to.z - self.z) * t,
        }
    }
}

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

impl NativeValue for Vec2F32 {
    const TY: SchemaTy = SchemaTy::Value(Vec2F32::PATH);

    fn from_value(value: &Value) -> Option<Vec2F32> {
        components(value).map(|[x, y]| Vec2F32 { x, y })
    }

    fn into_value(self) -> Value {
        aggregate(&[self.x, self.y])
    }
}

impl NativeValue for Vec3F32 {
    const TY: SchemaTy = SchemaTy::Value(Vec3F32::PATH);

    fn from_value(value: &Value) -> Option<Vec3F32> {
        components(value).map(Vec3F32::from_array)
    }

    fn into_value(self) -> Value {
        aggregate(&self.to_array())
    }
}

pub(super) static VEC2_METHODS: [NativeFunction; 7] = [
    crate::native!(fn "new" |_cx, x: f32, y: f32| -> Vec2F32 { Ok(Vec2F32::new(x, y)) })
        .constant()
        .realtime_safe(),
    crate::native!(fn "x" |_cx, this: Vec2F32| -> f32 { Ok(this.x) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "y" |_cx, this: Vec2F32| -> f32 { Ok(this.y) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "add" |_cx, this: Vec2F32, other: Vec2F32| -> Vec2F32 {
        Ok(Vec2F32::new(this.x + other.x, this.y + other.y))
    })
    .deterministic(),
    crate::native!(fn "sub" |_cx, this: Vec2F32, other: Vec2F32| -> Vec2F32 {
        Ok(Vec2F32::new(this.x - other.x, this.y - other.y))
    })
    .deterministic(),
    crate::native!(fn "scale" |_cx, this: Vec2F32, factor: f32| -> Vec2F32 {
        Ok(Vec2F32::new(this.x * factor, this.y * factor))
    })
    .deterministic(),
    crate::native!(fn "length" |_cx, this: Vec2F32| -> f32 { Ok(this.length()) })
        .deterministic()
        .realtime_safe(),
];

pub(super) static VEC3_METHODS: [NativeFunction; 8] = [
    crate::native!(fn "new" |_cx, x: f32, y: f32, z: f32| -> Vec3F32 {
        Ok(Vec3F32::new(x, y, z))
    })
    .constant()
    .realtime_safe(),
    crate::native!(fn "x" |_cx, this: Vec3F32| -> f32 { Ok(this.x) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "y" |_cx, this: Vec3F32| -> f32 { Ok(this.y) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "z" |_cx, this: Vec3F32| -> f32 { Ok(this.z) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "add" |_cx, this: Vec3F32, other: Vec3F32| -> Vec3F32 {
        Ok(Vec3F32::new(this.x + other.x, this.y + other.y, this.z + other.z))
    })
    .deterministic(),
    crate::native!(fn "sub" |_cx, this: Vec3F32, other: Vec3F32| -> Vec3F32 {
        Ok(Vec3F32::new(this.x - other.x, this.y - other.y, this.z - other.z))
    })
    .deterministic(),
    crate::native!(fn "scale" |_cx, this: Vec3F32, factor: f32| -> Vec3F32 {
        Ok(Vec3F32::new(this.x * factor, this.y * factor, this.z * factor))
    })
    .deterministic(),
    crate::native!(fn "length" |_cx, this: Vec3F32| -> f32 { Ok(this.length()) })
        .deterministic()
        .realtime_safe(),
];
