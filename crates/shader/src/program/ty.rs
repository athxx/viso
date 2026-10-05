//! The closed type set of a user shader: `Bool`, `I32`, `U32`, `F32`, their
//! two- to four-lane vectors, square `F32` matrices, `ColorLinear`, the
//! records a program declares, the profile's `VertexOutput`, and the texture
//! and sampler bindings.

/// A scalar lane type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scalar {
    Bool,
    I32,
    U32,
    F32,
}

impl Scalar {
    /// Whether arithmetic is defined on it.
    pub const fn is_numeric(self) -> bool {
        !matches!(self, Scalar::Bool)
    }

    /// Whether it is `I32` or `U32`.
    pub const fn is_integer(self) -> bool {
        matches!(self, Scalar::I32 | Scalar::U32)
    }

    /// Its source spelling.
    pub const fn name(self) -> &'static str {
        match self {
            Scalar::Bool => "Bool",
            Scalar::I32 => "I32",
            Scalar::U32 => "U32",
            Scalar::F32 => "F32",
        }
    }
}

/// What one texel of a texture reads as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Texel {
    F32,
    Vec4F32,
}

impl Texel {
    /// The value a sample returns.
    pub const fn ty(self) -> Ty {
        match self {
            Texel::F32 => Ty::Scalar(Scalar::F32),
            Texel::Vec4F32 => Ty::Vector(Scalar::F32, 4),
        }
    }
}

/// A shader value or binding type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    /// What a function without a value returns.
    Unit,
    Scalar(Scalar),
    /// `lanes` in `2..=4`.
    Vector(Scalar, u8),
    /// An `n`×`n` `F32` matrix, `n` in `2..=4`, stored by column.
    Matrix(u8),
    /// A linear-light RGBA color.
    Color,
    /// The record the program declares at this index.
    Record(u32),
    /// The record a vertex entry returns: its clip-space position.
    VertexOutput,
    Texture(Texel),
    Sampler,
}

impl Ty {
    pub const BOOL: Ty = Ty::Scalar(Scalar::Bool);
    pub const I32: Ty = Ty::Scalar(Scalar::I32);
    pub const U32: Ty = Ty::Scalar(Scalar::U32);
    pub const F32: Ty = Ty::Scalar(Scalar::F32);
    pub const VEC2: Ty = Ty::Vector(Scalar::F32, 2);
    pub const VEC3: Ty = Ty::Vector(Scalar::F32, 3);
    pub const VEC4: Ty = Ty::Vector(Scalar::F32, 4);

    /// The built-in type a single-segment name spells, the texture types
    /// aside (they take an argument).
    pub fn from_name(name: &str) -> Option<Ty> {
        let vector = |scalar, rest: &str| match rest {
            "2" => Some(Ty::Vector(scalar, 2)),
            "3" => Some(Ty::Vector(scalar, 3)),
            "4" => Some(Ty::Vector(scalar, 4)),
            _ => None,
        };
        Some(match name {
            "Bool" => Ty::BOOL,
            "I32" => Ty::I32,
            "U32" => Ty::U32,
            "F32" => Ty::F32,
            "Mat2F32" => Ty::Matrix(2),
            "Mat3F32" => Ty::Matrix(3),
            "Mat4F32" => Ty::Matrix(4),
            "ColorLinear" => Ty::Color,
            "VertexOutput" => Ty::VertexOutput,
            "Sampler" => Ty::Sampler,
            _ => {
                let rest = name.strip_prefix("Vec")?;
                let (lanes, scalar) = rest.split_at(1.min(rest.len()));
                return match scalar {
                    "F32" => vector(Scalar::F32, lanes),
                    "I32" => vector(Scalar::I32, lanes),
                    "U32" => vector(Scalar::U32, lanes),
                    _ => None,
                };
            }
        })
    }

    /// Whether `name` is a built-in shader type name, `Texture2D` included.
    pub fn is_builtin_name(name: &str) -> bool {
        name == "Texture2D" || Ty::from_name(name).is_some()
    }

    /// Its scalar and lane count, for a scalar or a vector.
    pub const fn lanes(self) -> Option<(Scalar, u8)> {
        match self {
            Ty::Scalar(s) => Some((s, 1)),
            Ty::Vector(s, n) => Some((s, n)),
            _ => None,
        }
    }

    /// The scalar or vector of `lanes` lanes of `scalar`.
    pub const fn of_lanes(scalar: Scalar, lanes: u8) -> Ty {
        if lanes == 1 {
            Ty::Scalar(scalar)
        } else {
            Ty::Vector(scalar, lanes)
        }
    }

    /// Its scalar, for a scalar or a vector.
    pub const fn scalar(self) -> Option<Scalar> {
        match self.lanes() {
            Some((s, _)) => Some(s),
            None => None,
        }
    }

    /// Whether it is an `F32` scalar or vector.
    pub const fn is_float(self) -> bool {
        matches!(self.scalar(), Some(Scalar::F32))
    }

    /// Whether values of it are plain data a local, parameter or return can
    /// hold: everything but `Unit` and the bindings.
    pub const fn is_value(self) -> bool {
        !matches!(self, Ty::Unit | Ty::Texture(_) | Ty::Sampler)
    }

    /// Its source spelling, a record named by `records`.
    pub fn spelling(self, records: &[impl AsRef<str>]) -> String {
        match self {
            Ty::Unit => "Unit".into(),
            Ty::Scalar(s) => s.name().into(),
            Ty::Vector(s, n) => format!("Vec{n}{}", s.name()),
            Ty::Matrix(n) => format!("Mat{n}F32"),
            Ty::Color => "ColorLinear".into(),
            Ty::Record(i) => records
                .get(i as usize)
                .map_or_else(|| format!("record #{i}"), |r| r.as_ref().to_owned()),
            Ty::VertexOutput => "VertexOutput".into(),
            Ty::Texture(Texel::F32) => "Texture2D<F32>".into(),
            Ty::Texture(Texel::Vec4F32) => "Texture2D<Vec4F32>".into(),
            Ty::Sampler => "Sampler".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_name_spells_back() {
        for name in [
            "Bool",
            "I32",
            "U32",
            "F32",
            "Vec2F32",
            "Vec3F32",
            "Vec4F32",
            "Vec2I32",
            "Vec3I32",
            "Vec4I32",
            "Vec2U32",
            "Vec3U32",
            "Vec4U32",
            "Mat2F32",
            "Mat3F32",
            "Mat4F32",
            "ColorLinear",
            "VertexOutput",
            "Sampler",
        ] {
            let ty = Ty::from_name(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(ty.spelling(&[] as &[&str]), name);
        }
        for other in ["F64", "Vec5F32", "Vec2", "Vec2F64", "Mat2", "String", "Vec"] {
            assert_eq!(Ty::from_name(other), None, "{other}");
        }
        assert!(Ty::is_builtin_name("Texture2D"));
    }
}
