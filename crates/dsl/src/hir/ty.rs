//! The static type lattice and the numeric conversion rules.
//!
//! [`Ty`] is the closed list of core scalar types plus the UI dimensional types,
//! the nominal types (a component/record/enum/type-alias named by its [`SymbolId`]),
//! the structural types (tuple / function / list / option), and two inference
//! placeholders for as-yet-undetermined numeric literals. `Float` is deliberately
//! absent (there is no width-ambiguous float type); naming it is a hard error.
//!
//! The conversion rules are the doc's: a small implicit safe-widening ladder and an
//! everything-else-is-explicit policy. Widening never crosses the signed/unsigned
//! boundary, never int↔float, and never narrows (including `F64 -> F32`).

use crate::ast::TypePath;
use crate::resolve::SymbolId;

/// A resolved static type.
///
/// The scalar and UI-dimensional variants are the doc's unique primitive list; the
/// nominal variant carries the declaration's `SymbolId`; the structural variants are
/// tuple/function/list/option. [`Ty::InferInt`] and [`Ty::InferFloat`] are the
/// undetermined numeric-literal placeholders that inference must resolve before HIR
/// is complete; [`Ty::Unknown`] marks a slot inference could not fill (always paired
/// with a diagnostic) and [`Ty::Never`] is the bottom type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    // --- core scalars (doc "unique primitive list") -------------------------
    Bool,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Char,
    String,
    Bytes,
    Unit,
    Never,
    Color,

    // --- UI dimensional types -----------------------------------------------
    Dp,
    Px,
    Sp,
    Percent,
    Duration,
    Angle,
    Frequency,

    // --- nominal & structural -----------------------------------------------
    /// A nominal type: a record/enum/component/type-alias, identified by its symbol.
    Named(SymbolId),
    /// A tuple `(A, B, ...)` — structural.
    Tuple(Vec<Ty>),
    /// A function type `(params) -> ret` — structural.
    Fn(Vec<Ty>, Box<Ty>),
    /// `List<T>`.
    List(Box<Ty>),
    /// `Option<T>`.
    Option(Box<Ty>),

    // --- inference placeholders ---------------------------------------------
    /// An undetermined integer literal (host default `I64` if no context pins it).
    InferInt,
    /// An undetermined float literal (host default `F64` if no context pins it).
    InferFloat,
    /// Inference failed here; always accompanied by a diagnostic.
    Unknown,
}

/// A type-annotation resolution error (mapped to a stable diagnostic code by the
/// caller that has the annotation's span).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeError {
    /// The removed `Float` type was named. Diagnostic code `E2101`.
    FloatRemoved,
    /// The annotation named no known type. (Nominal types resolve through the
    /// resolver's refs; a bare unknown scalar name lands here.)
    UnknownType,
}

impl TypeError {
    /// The stable diagnostic code for this error.
    pub fn code(self) -> &'static str {
        match self {
            TypeError::FloatRemoved => "E2101",
            TypeError::UnknownType => "E2103",
        }
    }

    /// A one-line message for this error.
    pub fn message(self) -> &'static str {
        match self {
            TypeError::FloatRemoved => {
                "the `Float` type has been removed; use `F32` or `F64` with an explicit width"
            }
            TypeError::UnknownType => "unknown type",
        }
    }
}

/// Why an implicit conversion between two numeric types is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidenError {
    /// The conversion is not an allowed implicit safe widening; an explicit `as` /
    /// `checked_cast` is required. Diagnostic code `E2102`.
    IllegalImplicit,
}

impl Ty {
    /// Resolves a scalar or UI-dimensional type *name* to its [`Ty`]. Nominal types
    /// (records/enums/components) are not decided here — those resolve through the
    /// resolver's symbol refs — so an unrecognized name returns `None` and the caller
    /// decides whether a nominal binding exists. The one hard error surfaced here is
    /// the removed `Float` type.
    pub fn from_builtin_name(name: &str) -> Result<Option<Ty>, TypeError> {
        Ok(Some(match name {
            "Bool" => Ty::Bool,
            "I8" => Ty::I8,
            "I16" => Ty::I16,
            "I32" => Ty::I32,
            "I64" => Ty::I64,
            "U8" => Ty::U8,
            "U16" => Ty::U16,
            "U32" => Ty::U32,
            "U64" => Ty::U64,
            "F32" => Ty::F32,
            "F64" => Ty::F64,
            "Char" => Ty::Char,
            "String" => Ty::String,
            "Bytes" => Ty::Bytes,
            "Unit" => Ty::Unit,
            "Never" => Ty::Never,
            "Color" => Ty::Color,
            "Dp" => Ty::Dp,
            "Px" => Ty::Px,
            "Sp" => Ty::Sp,
            "Percent" => Ty::Percent,
            "Duration" => Ty::Duration,
            "Angle" => Ty::Angle,
            "Frequency" => Ty::Frequency,
            "Float" => return Err(TypeError::FloatRemoved),
            _ => return Ok(None),
        }))
    }

    /// Resolves a single-segment builtin annotation from a [`TypePath`]. Multi-segment
    /// paths (`a::B`) and generic heads (`List<T>`) are nominal/structural and resolve
    /// elsewhere; this returns `Ok(None)` for anything but a single recognized builtin
    /// segment, and the `Float` error for a `Float` head.
    pub fn from_type_path(path: &TypePath) -> Result<Option<Ty>, TypeError> {
        let mut segs = path.segments();
        let Some(head) = segs.next() else {
            return Ok(None);
        };
        // A builtin scalar is a single bare segment. A qualified path is nominal.
        if segs.next().is_some() {
            return Ok(None);
        }
        Ty::from_builtin_name(&head.text())
    }

    /// Whether `self` implicitly and safely widens to `target` (doc's allowed set):
    /// the signed ladder `I8→I16→I32→I64`, the unsigned ladder `U8→…→U64`, and
    /// `F32→F64`. Equal types trivially widen. Nothing crosses signedness, nothing
    /// goes int↔float, nothing narrows. An undetermined literal placeholder is
    /// handled by literal typing, not here.
    pub fn widens_to(&self, target: &Ty) -> bool {
        if self == target {
            return true;
        }
        widen_rank(self)
            .zip(widen_rank(target))
            .is_some_and(|(from, to)| from.family == to.family && from.rank <= to.rank)
    }

    /// The implicit-conversion check used when a value of type `self` is supplied
    /// where `target` is expected. Returns `Ok(())` if identical or a legal safe
    /// widening, else [`WidenError::IllegalImplicit`] (the caller renders `E2102`).
    /// This only governs numeric widening; a non-numeric type mismatch is a plain
    /// type mismatch (`E2103`), decided by the caller comparing types.
    pub fn check_implicit_widen(&self, target: &Ty) -> Result<(), WidenError> {
        if self.widens_to(target) {
            Ok(())
        } else {
            Err(WidenError::IllegalImplicit)
        }
    }
}

/// The two numeric widening families; widening only moves up within one family.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WidenFamily {
    Signed,
    Unsigned,
    Float,
}

struct WidenPos {
    family: WidenFamily,
    rank: u8,
}

fn widen_rank(ty: &Ty) -> Option<WidenPos> {
    let (family, rank) = match ty {
        Ty::I8 => (WidenFamily::Signed, 0),
        Ty::I16 => (WidenFamily::Signed, 1),
        Ty::I32 => (WidenFamily::Signed, 2),
        Ty::I64 => (WidenFamily::Signed, 3),
        Ty::U8 => (WidenFamily::Unsigned, 0),
        Ty::U16 => (WidenFamily::Unsigned, 1),
        Ty::U32 => (WidenFamily::Unsigned, 2),
        Ty::U64 => (WidenFamily::Unsigned, 3),
        Ty::F32 => (WidenFamily::Float, 0),
        Ty::F64 => (WidenFamily::Float, 1),
        _ => return None,
    };
    Some(WidenPos { family, rank })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_scalar_names_resolve() {
        assert_eq!(Ty::from_builtin_name("Bool"), Ok(Some(Ty::Bool)));
        assert_eq!(Ty::from_builtin_name("I64"), Ok(Some(Ty::I64)));
        assert_eq!(Ty::from_builtin_name("F32"), Ok(Some(Ty::F32)));
        assert_eq!(Ty::from_builtin_name("Color"), Ok(Some(Ty::Color)));
        assert_eq!(Ty::from_builtin_name("Dp"), Ok(Some(Ty::Dp)));
    }

    #[test]
    fn a_nominal_name_is_not_a_builtin() {
        assert_eq!(Ty::from_builtin_name("Counter"), Ok(None));
        assert_eq!(Ty::from_builtin_name("Money"), Ok(None));
    }

    #[test]
    fn float_is_a_removed_type() {
        assert_eq!(Ty::from_builtin_name("Float"), Err(TypeError::FloatRemoved));
        assert_eq!(TypeError::FloatRemoved.code(), "E2101");
    }

    #[test]
    fn signed_ladder_widens_upward_only() {
        assert!(Ty::I8.widens_to(&Ty::I64));
        assert!(Ty::I32.widens_to(&Ty::I64));
        assert!(Ty::I32.widens_to(&Ty::I32));
        assert!(!Ty::I64.widens_to(&Ty::I32));
    }

    #[test]
    fn float_ladder_widens_f32_to_f64_only() {
        assert!(Ty::F32.widens_to(&Ty::F64));
        // The doc forbids the narrowing direction implicitly.
        assert!(!Ty::F64.widens_to(&Ty::F32));
    }

    #[test]
    fn widening_never_crosses_family() {
        assert!(!Ty::I32.widens_to(&Ty::U32));
        assert!(!Ty::U32.widens_to(&Ty::I64));
        assert!(!Ty::I32.widens_to(&Ty::F32));
        assert!(!Ty::F32.widens_to(&Ty::I64));
    }

    #[test]
    fn check_implicit_widen_reports_illegal() {
        assert_eq!(Ty::I8.check_implicit_widen(&Ty::I64), Ok(()));
        assert_eq!(
            Ty::F64.check_implicit_widen(&Ty::F32),
            Err(WidenError::IllegalImplicit)
        );
    }
}
