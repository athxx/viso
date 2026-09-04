//! The shader IR type system — the single source of truth for how a GPU
//! scalar/vector field is spelled in Metal, projected to a validation
//! [`AttrFormat`], and sized/aligned for the CPU↔GPU offset cross-check
//! (architecture section 36 / AGENTS 19).
//!
//! This takes makepad's *semantics* — every `[f32; N>=2]` field is `packed_floatN`
//! so the emitted `struct` matches the CPU `#[repr(C)]` layout with no inter-field
//! padding, scalars stay a bare 4-byte-aligned `float`/`uint`, and a `mat4x4` is
//! four `packed_float4` columns — but expresses them as a real typed tree the
//! codegen prints, rather than a bytecode walk over a shared script VM (see the
//! `viso-diverge-from-makepad` note; makepad's `metal_create_instance_struct`
//! decides packing inline in its transliterator, we decide it here once).
//!
//! Reserved-word caveat: `half` is an MSL type (16-bit float) and must never be
//! produced as an identifier — the type printer here emits only `float`/`uint`
//! spellings, so no `half` token can leak into codegen (see `viso-msl-reserved-half`).

use viso_gpu::AttrFormat;

/// A GPU scalar element type. These are the only element types a built-in
/// instance/vertex field is allowed to carry — 4-byte-aligned, directly
/// uploadable, and byte-for-byte matching the CPU `#[repr(C)]` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarType {
    /// 32-bit float (`f32` on the CPU, `float` in MSL).
    F32,
    /// 32-bit unsigned integer (`u32` on the CPU, `uint` in MSL).
    U32,
}

impl ScalarType {
    /// The MSL scalar spelling. Never `half` (see the module note).
    const fn msl_scalar(self) -> &'static str {
        match self {
            ScalarType::F32 => "float",
            ScalarType::U32 => "uint",
        }
    }

    /// The byte size of one scalar. Both GPU scalars we allow are 4 bytes.
    const fn size(self) -> usize {
        4
    }
}

/// A shader IR type: a scalar, or a vector of 2/3/4 lanes of one scalar.
///
/// This is the interface-layer (instance/uniform/varying field) type.
/// Body-level expression types are out of scope this slice — built-in
/// primitives carry their algorithm body as structured MSL fragments, not an
/// expression AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrType {
    /// A single scalar (`float` / `uint`).
    Scalar(ScalarType),
    /// A vector of `lanes` scalars, `lanes` in `2..=4`.
    Vector { scalar: ScalarType, lanes: u8 },
}

impl IrType {
    /// A 2-lane float vector (`[f32; 2]` ⇒ `packed_float2`).
    pub const F32X2: IrType = IrType::Vector {
        scalar: ScalarType::F32,
        lanes: 2,
    };
    /// A 3-lane float vector (`[f32; 3]` ⇒ `packed_float3`).
    pub const F32X3: IrType = IrType::Vector {
        scalar: ScalarType::F32,
        lanes: 3,
    };
    /// A 4-lane float vector (`[f32; 4]` ⇒ `packed_float4`).
    pub const F32X4: IrType = IrType::Vector {
        scalar: ScalarType::F32,
        lanes: 4,
    };
    /// A single float (`f32` ⇒ `float`).
    pub const F32: IrType = IrType::Scalar(ScalarType::F32);
    /// A single unsigned int (`u32` ⇒ `uint`).
    pub const U32: IrType = IrType::Scalar(ScalarType::U32);

    /// The MSL type spelling for a field of this type inside an instance/vertex
    /// `struct`. This encodes makepad's packing rule: a vector is
    /// `packed_floatN`/`packed_uintN` so the struct has no inter-field padding
    /// versus the CPU `#[repr(C)]` layout; a scalar stays bare (already
    /// 4-byte-aligned). Never emits `half` (see the module note).
    pub fn msl_field_type(self) -> String {
        match self {
            IrType::Scalar(s) => s.msl_scalar().to_string(),
            IrType::Vector { scalar, lanes } => {
                format!("packed_{}{}", scalar.msl_scalar(), lanes)
            }
        }
    }

    /// The MSL type spelling for a *value* (a local, a `VOut` field, a cast
    /// target): the unpacked vector `floatN`/`uintN`, or the bare scalar. Packed
    /// types are a storage-only spelling in MSL; arithmetic and stage-in/out use
    /// the unpacked form.
    pub fn msl_value_type(self) -> String {
        match self {
            IrType::Scalar(s) => s.msl_scalar().to_string(),
            IrType::Vector { scalar, lanes } => {
                format!("{}{}", scalar.msl_scalar(), lanes)
            }
        }
    }

    /// Project this IR type onto the GPU instance-field [`AttrFormat`] the
    /// schema/layout validation speaks. This is the single mapping that replaces
    /// the hand-written `packed_floatN`/`float` + `Float2`/… decisions that were
    /// duplicated across `msl.rs`'s struct and its `*_schema()` fns.
    pub fn to_attr_format(self) -> AttrFormat {
        match self {
            IrType::Scalar(ScalarType::F32) => AttrFormat::Float1,
            IrType::Scalar(ScalarType::U32) => AttrFormat::Uint1,
            IrType::Vector {
                scalar: ScalarType::F32,
                lanes,
            } => match lanes {
                2 => AttrFormat::Float2,
                3 => AttrFormat::Float3,
                _ => AttrFormat::Float4,
            },
            IrType::Vector {
                scalar: ScalarType::U32,
                lanes,
            } => match lanes {
                2 => AttrFormat::Uint2,
                _ => AttrFormat::Uint4,
            },
        }
    }

    /// The byte size this field occupies in a tightly-packed `#[repr(C)]` /
    /// `packed_*` layout: `lanes * scalar_size` (a `packed_floatN` has no
    /// trailing pad). This is the arithmetic the section-36.1 offset cross-check
    /// runs to derive each field's expected byte offset from the field list.
    pub const fn packed_size(self) -> usize {
        match self {
            IrType::Scalar(s) => s.size(),
            IrType::Vector { scalar, lanes } => scalar.size() * lanes as usize,
        }
    }

    /// The CPU `#[repr(C)]` alignment of the matching Rust field type. A scalar
    /// (`f32`/`u32`) and an array `[f32; N]`/`[u32; N]` are all aligned to the
    /// 4-byte element — arrays do not raise alignment — so every allowed field
    /// is 4-byte-aligned. `#[repr(C)]` inserts padding before a field to reach
    /// its alignment; with a uniform 4-byte alignment the packed prefix sum is
    /// already the real offset, which is exactly why the built-in structs need
    /// no inter-field padding.
    pub const fn align(self) -> usize {
        // Every element is 4 bytes and arrays inherit element alignment.
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msl_field_spellings_are_packed_for_vectors_bare_for_scalars() {
        assert_eq!(IrType::F32.msl_field_type(), "float");
        assert_eq!(IrType::U32.msl_field_type(), "uint");
        assert_eq!(IrType::F32X2.msl_field_type(), "packed_float2");
        assert_eq!(IrType::F32X3.msl_field_type(), "packed_float3");
        assert_eq!(IrType::F32X4.msl_field_type(), "packed_float4");
    }

    #[test]
    fn msl_value_spellings_are_unpacked() {
        assert_eq!(IrType::F32X2.msl_value_type(), "float2");
        assert_eq!(IrType::F32X4.msl_value_type(), "float4");
        assert_eq!(IrType::F32.msl_value_type(), "float");
    }

    #[test]
    fn no_field_or_value_spelling_is_ever_half() {
        for ty in [
            IrType::F32,
            IrType::U32,
            IrType::F32X2,
            IrType::F32X3,
            IrType::F32X4,
        ] {
            assert!(!ty.msl_field_type().contains("half"));
            assert!(!ty.msl_value_type().contains("half"));
        }
    }

    #[test]
    fn attr_format_projection_matches_the_derive_macro_mapping() {
        assert_eq!(IrType::F32.to_attr_format(), AttrFormat::Float1);
        assert_eq!(IrType::F32X2.to_attr_format(), AttrFormat::Float2);
        assert_eq!(IrType::F32X3.to_attr_format(), AttrFormat::Float3);
        assert_eq!(IrType::F32X4.to_attr_format(), AttrFormat::Float4);
        assert_eq!(IrType::U32.to_attr_format(), AttrFormat::Uint1);
    }

    #[test]
    fn packed_size_is_tight() {
        assert_eq!(IrType::F32.packed_size(), 4);
        assert_eq!(IrType::F32X2.packed_size(), 8);
        assert_eq!(IrType::F32X3.packed_size(), 12);
        assert_eq!(IrType::F32X4.packed_size(), 16);
    }
}
