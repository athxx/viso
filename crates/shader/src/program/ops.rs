//! The operators and intrinsic functions of a user shader and the types they
//! take and give: one table the front end types calls against, the validator
//! rechecks, and every backend and the reference interpreter implement.

use super::ty::{Scalar, Ty};

/// A prefix operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `-x` on a signed or float scalar, vector or matrix.
    Neg,
    /// `!b` on a `Bool`.
    Not,
    /// `~n` on an integer scalar or vector.
    BitNot,
}

impl UnaryOp {
    /// The type of the operator applied to `operand`, `None` when it does not
    /// apply.
    pub fn result(self, operand: Ty) -> Option<Ty> {
        let ok = match self {
            UnaryOp::Neg => {
                matches!(operand, Ty::Matrix(_))
                    || matches!(operand.scalar(), Some(Scalar::I32 | Scalar::F32))
            }
            UnaryOp::Not => operand == Ty::BOOL,
            UnaryOp::BitNot => operand.scalar().is_some_and(Scalar::is_integer),
        };
        ok.then_some(operand)
    }

    /// Its source spelling.
    pub const fn symbol(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "!",
            UnaryOp::BitNot => "~",
        }
    }
}

/// An infix operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Short-circuit `&&`.
    And,
    /// Short-circuit `||`.
    Or,
}

impl BinaryOp {
    /// The operator a source symbol spells.
    pub fn from_symbol(symbol: &str) -> Option<BinaryOp> {
        Some(match symbol {
            "+" => BinaryOp::Add,
            "-" => BinaryOp::Sub,
            "*" => BinaryOp::Mul,
            "/" => BinaryOp::Div,
            "%" => BinaryOp::Rem,
            "&" => BinaryOp::BitAnd,
            "|" => BinaryOp::BitOr,
            "^" => BinaryOp::BitXor,
            "<<" => BinaryOp::Shl,
            ">>" => BinaryOp::Shr,
            "==" => BinaryOp::Eq,
            "!=" => BinaryOp::Ne,
            "<" => BinaryOp::Lt,
            "<=" => BinaryOp::Le,
            ">" => BinaryOp::Gt,
            ">=" => BinaryOp::Ge,
            "&&" => BinaryOp::And,
            "||" => BinaryOp::Or,
            _ => return None,
        })
    }

    /// Its source spelling.
    pub const fn symbol(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitOr => "|",
            BinaryOp::BitXor => "^",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
        }
    }

    /// Whether it is `+ - * / %`.
    pub const fn is_arithmetic(self) -> bool {
        matches!(
            self,
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem
        )
    }

    /// The type of `lhs op rhs`, `None` when the operator does not apply.
    ///
    /// Arithmetic takes two operands of one numeric scalar or vector type, or
    /// a vector and a scalar of its lane type either way round; a matrix adds
    /// and subtracts with its own type, and multiplies a matrix, a vector of
    /// its size or an `F32`. Bitwise operators take one integer type; a shift
    /// amount may also be `U32` lanes. Comparisons take two scalars of one
    /// type, order comparisons numeric ones; `&&` and `||` take `Bool`s.
    pub fn result(self, lhs: Ty, rhs: Ty) -> Option<Ty> {
        use BinaryOp as B;
        match self {
            B::Add | B::Sub | B::Mul | B::Div | B::Rem => {
                if let (Ty::Matrix(n), Ty::Matrix(m)) = (lhs, rhs) {
                    return (n == m && matches!(self, B::Add | B::Sub | B::Mul)).then_some(lhs);
                }
                if self == B::Mul {
                    match (lhs, rhs) {
                        (Ty::Matrix(n), Ty::Vector(Scalar::F32, m))
                        | (Ty::Vector(Scalar::F32, m), Ty::Matrix(n)) => {
                            return (n == m).then_some(Ty::Vector(Scalar::F32, n));
                        }
                        (Ty::Matrix(_), Ty::Scalar(Scalar::F32)) => return Some(lhs),
                        (Ty::Scalar(Scalar::F32), Ty::Matrix(_)) => return Some(rhs),
                        _ => {}
                    }
                }
                let ((ls, ln), (rs, rn)) = (lhs.lanes()?, rhs.lanes()?);
                (ls == rs && ls.is_numeric() && (ln == rn || ln == 1 || rn == 1))
                    .then_some(Ty::of_lanes(ls, ln.max(rn)))
            }
            B::BitAnd | B::BitOr | B::BitXor => {
                (lhs == rhs && lhs.scalar().is_some_and(Scalar::is_integer)).then_some(lhs)
            }
            B::Shl | B::Shr => {
                let ((ls, ln), (rs, rn)) = (lhs.lanes()?, rhs.lanes()?);
                (ls.is_integer() && ln == rn && (rs == ls || rs == Scalar::U32)).then_some(lhs)
            }
            B::Eq | B::Ne => (lhs == rhs && matches!(lhs, Ty::Scalar(_))).then_some(Ty::BOOL),
            B::Lt | B::Le | B::Gt | B::Ge => {
                (lhs == rhs && matches!(lhs, Ty::Scalar(s) if s.is_numeric())).then_some(Ty::BOOL)
            }
            B::And | B::Or => (lhs == Ty::BOOL && rhs == Ty::BOOL).then_some(Ty::BOOL),
        }
    }
}

/// Which stages may call an intrinsic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageUse {
    Any,
    /// It needs screen-space derivatives.
    FragmentOnly,
    /// It synthesizes vertex positions.
    VertexOnly,
}

macro_rules! intrinsics {
    ($($(#[$doc:meta])* $variant:ident = $name:literal,)*) => {
        /// A built-in function a shader calls by name.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Intrinsic {
            $($(#[$doc])* $variant,)*
        }

        impl Intrinsic {
            /// Every intrinsic.
            pub const ALL: &'static [Intrinsic] = &[$(Intrinsic::$variant,)*];

            /// The intrinsic a source name calls.
            pub fn from_name(name: &str) -> Option<Intrinsic> {
                match name {
                    $($name => Some(Intrinsic::$variant),)*
                    _ => None,
                }
            }

            /// Its source name.
            pub const fn name(self) -> &'static str {
                match self {
                    $(Intrinsic::$variant => $name,)*
                }
            }
        }
    };
}

intrinsics! {
    Abs = "abs",
    Sign = "sign",
    Floor = "floor",
    Ceil = "ceil",
    /// Rounds half to even.
    Round = "round",
    Trunc = "trunc",
    /// `x - floor(x)`.
    Fract = "fract",
    Sqrt = "sqrt",
    InverseSqrt = "inverse_sqrt",
    Exp = "exp",
    Exp2 = "exp2",
    Log = "log",
    Log2 = "log2",
    Sin = "sin",
    Cos = "cos",
    Tan = "tan",
    Asin = "asin",
    Acos = "acos",
    Atan = "atan",
    Sinh = "sinh",
    Cosh = "cosh",
    Tanh = "tanh",
    Radians = "radians",
    Degrees = "degrees",
    /// `atan2(y, x)`.
    Atan2 = "atan2",
    Pow = "pow",
    /// `step(edge, x)`: `0` below the edge, `1` from it.
    Step = "step",
    Min = "min",
    Max = "max",
    Clamp = "clamp",
    /// `mix(a, b, t)`: `a + (b - a) * t`.
    Mix = "mix",
    /// `smoothstep(e0, e1, x)`: Hermite `t * t * (3 - 2t)` of
    /// `t = clamp((x - e0) / (e1 - e0), 0, 1)`.
    Smoothstep = "smoothstep",
    Length = "length",
    Distance = "distance",
    Dot = "dot",
    Normalize = "normalize",
    Cross = "cross",
    Transpose = "transpose",
    Dpdx = "dpdx",
    Dpdy = "dpdy",
    Fwidth = "fwidth",
    /// `sample(texture, sampler, uv)`, with derivatives for the mip level.
    Sample = "sample",
    /// `sample_level(texture, sampler, uv, lod)`.
    SampleLevel = "sample_level",
    /// The four channels of a `ColorLinear`.
    ToVec4 = "to_vec4",
    /// The unit-square corner of a vertex of a six-vertex instanced quad:
    /// `(0,0) (1,0) (0,1) (1,0) (1,1) (0,1)`.
    QuadVertex = "quad_vertex",
    /// A top-left-origin pixel position as a clip-space position of a
    /// `viewport`-sized target.
    ToClip = "to_clip",
    /// The signed distance from a point of a `size` rectangle's local space,
    /// origin at its top left, to the rectangle with corners of `radius`:
    /// negative inside.
    RoundedRectSdf = "rounded_rect_sdf",
}

impl Intrinsic {
    /// The stages that may call it.
    pub const fn stages(self) -> StageUse {
        match self {
            Intrinsic::Dpdx | Intrinsic::Dpdy | Intrinsic::Fwidth | Intrinsic::Sample => {
                StageUse::FragmentOnly
            }
            Intrinsic::QuadVertex => StageUse::VertexOnly,
            _ => StageUse::Any,
        }
    }

    /// The type of a call on `args`, `None` when it does not take them.
    pub fn result(self, args: &[Ty]) -> Option<Ty> {
        use Intrinsic as I;
        let float = |t: Ty| t.is_float();
        let numeric = |t: Ty| t.scalar().is_some_and(Scalar::is_numeric);
        let fvec = |t: Ty| matches!(t, Ty::Vector(Scalar::F32, _));
        // A lane-wise parameter that may also be the `F32` scalar.
        let like = |t: Ty, of: Ty| t == of || t == Ty::F32;
        match (self, args) {
            (I::Abs, &[x]) => numeric(x).then_some(x),
            (I::Sign, &[x]) => float(x).then_some(x),
            (
                I::Floor
                | I::Ceil
                | I::Round
                | I::Trunc
                | I::Fract
                | I::Sqrt
                | I::InverseSqrt
                | I::Exp
                | I::Exp2
                | I::Log
                | I::Log2
                | I::Sin
                | I::Cos
                | I::Tan
                | I::Asin
                | I::Acos
                | I::Atan
                | I::Sinh
                | I::Cosh
                | I::Tanh
                | I::Radians
                | I::Degrees
                | I::Dpdx
                | I::Dpdy
                | I::Fwidth,
                &[x],
            ) => float(x).then_some(x),
            (I::Atan2 | I::Pow, &[a, b]) => (float(a) && a == b).then_some(a),
            (I::Step, &[edge, x]) => (float(x) && like(edge, x)).then_some(x),
            (I::Min | I::Max, &[a, b]) => (numeric(a) && a == b).then_some(a),
            (I::Clamp, &[x, lo, hi]) => {
                let ok = if float(x) {
                    like(lo, x) && like(hi, x)
                } else {
                    numeric(x) && lo == x && hi == x
                };
                ok.then_some(x)
            }
            (I::Mix, &[a, b, t]) => (float(a) && a == b && like(t, a)).then_some(a),
            (I::Smoothstep, &[e0, e1, x]) => (float(x) && like(e0, x) && like(e1, x)).then_some(x),
            (I::Length, &[x]) => float(x).then_some(Ty::F32),
            (I::Distance, &[a, b]) => (float(a) && a == b).then_some(Ty::F32),
            (I::Dot, &[a, b]) => (fvec(a) && a == b).then_some(Ty::F32),
            (I::Normalize, &[x]) => fvec(x).then_some(x),
            (I::Cross, &[a, b]) => (a == Ty::VEC3 && b == Ty::VEC3).then_some(Ty::VEC3),
            (I::Transpose, &[m]) => matches!(m, Ty::Matrix(_)).then_some(m),
            (I::Sample, &[Ty::Texture(texel), Ty::Sampler, uv]) => {
                (uv == Ty::VEC2).then_some(texel.ty())
            }
            (I::SampleLevel, &[Ty::Texture(texel), Ty::Sampler, uv, lod]) => {
                (uv == Ty::VEC2 && lod == Ty::F32).then_some(texel.ty())
            }
            (I::ToVec4, &[Ty::Color]) => Some(Ty::VEC4),
            (I::QuadVertex, &[Ty::Scalar(Scalar::U32)]) => Some(Ty::VEC2),
            (I::ToClip, &[pos, viewport]) => {
                (pos == Ty::VEC2 && viewport == Ty::VEC2).then_some(Ty::VEC4)
            }
            (I::RoundedRectSdf, &[p, size, radius]) => {
                (p == Ty::VEC2 && size == Ty::VEC2 && radius == Ty::F32).then_some(Ty::F32)
            }
            _ => None,
        }
    }

    /// How many arguments it takes.
    pub const fn arity(self) -> usize {
        use Intrinsic as I;
        match self {
            I::Atan2
            | I::Pow
            | I::Step
            | I::Min
            | I::Max
            | I::Distance
            | I::Dot
            | I::Cross
            | I::ToClip => 2,
            I::Clamp | I::Mix | I::Smoothstep | I::Sample | I::RoundedRectSdf => 3,
            I::SampleLevel => 4,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_broadcasts_a_scalar_and_multiplies_matrices() {
        let v2 = Ty::VEC2;
        assert_eq!(BinaryOp::Mul.result(v2, Ty::F32), Some(v2));
        assert_eq!(BinaryOp::Sub.result(Ty::F32, v2), Some(v2));
        assert_eq!(BinaryOp::Add.result(v2, Ty::VEC3), None);
        assert_eq!(BinaryOp::Add.result(Ty::I32, Ty::F32), None);
        assert_eq!(
            BinaryOp::Mul.result(Ty::Matrix(4), Ty::VEC4),
            Some(Ty::VEC4)
        );
        assert_eq!(BinaryOp::Mul.result(Ty::Matrix(3), Ty::VEC4), None);
        assert_eq!(BinaryOp::Div.result(Ty::Matrix(2), Ty::Matrix(2)), None);
        assert_eq!(BinaryOp::Lt.result(Ty::F32, Ty::F32), Some(Ty::BOOL));
        assert_eq!(BinaryOp::Lt.result(v2, v2), None);
        assert_eq!(BinaryOp::Shl.result(Ty::I32, Ty::U32), Some(Ty::I32));
        assert_eq!(BinaryOp::BitAnd.result(Ty::F32, Ty::F32), None);
        assert_eq!(UnaryOp::Neg.result(Ty::U32), None);
        assert_eq!(UnaryOp::Not.result(Ty::BOOL), Some(Ty::BOOL));
    }

    #[test]
    fn intrinsics_type_their_arguments() {
        use Intrinsic as I;
        assert_eq!(I::from_name("smoothstep"), Some(I::Smoothstep));
        assert_eq!(
            I::Smoothstep.result(&[Ty::F32, Ty::F32, Ty::VEC2]),
            Some(Ty::VEC2)
        );
        assert_eq!(
            I::Mix.result(&[Ty::VEC4, Ty::VEC4, Ty::F32]),
            Some(Ty::VEC4)
        );
        assert_eq!(I::Length.result(&[Ty::VEC3]), Some(Ty::F32));
        assert_eq!(I::Sqrt.result(&[Ty::I32]), None);
        assert_eq!(I::Abs.result(&[Ty::U32]), Some(Ty::U32));
        assert_eq!(I::Clamp.result(&[Ty::I32, Ty::I32, Ty::I32]), Some(Ty::I32));
        assert_eq!(I::Cross.result(&[Ty::VEC2, Ty::VEC2]), None);
        let tex = Ty::Texture(super::super::ty::Texel::Vec4F32);
        assert_eq!(
            I::Sample.result(&[tex, Ty::Sampler, Ty::VEC2]),
            Some(Ty::VEC4)
        );
        assert_eq!(I::Sample.stages(), StageUse::FragmentOnly);
        for &i in I::ALL {
            assert_eq!(I::from_name(i.name()), Some(i));
        }
    }
}
