//! Per-instance GPU vertex layout: the explicit descriptor that replaces
//! makepad's implicit "everything after field X is GPU memory" `DrawVars` trick.
//!
//! `#[derive(GpuInstance)]` (in `viso-macros`) emits a `const LAYOUT:
//! InstanceLayout` for the annotated `#[repr(C)]` struct: one [`InstanceField`]
//! per field, each carrying the field's real byte offset (via `offset_of!`) and
//! its GPU attribute [`AttrFormat`]. A shader declares the layout it expects as
//! an [`InstanceSchema`]; [`InstanceLayout::validate_against`] checks the two
//! agree at pipeline-registration time.
//!
//! This mirrors makepad's `DrawShaderInputs` (`platform/src/draw_shader.rs`),
//! but keyed on explicit byte offsets rather than reconstructed f32 "slots":
//! our instance struct *is* the `#[repr(C)]` layout, so Rust already fixes the
//! offsets and we only need to describe and cross-check them.

/// The GPU attribute format of one instance field.
///
/// The scalar base type and component count together determine how the field is
/// declared in the shader's vertex-input struct and how the backend describes
/// the vertex attribute. Only 4-byte-aligned scalars are allowed (§32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrFormat {
    /// `f32` scalar / `float`.
    Float1,
    /// `[f32; 2]` / `float2`.
    Float2,
    /// `[f32; 3]` / `float3`.
    Float3,
    /// `[f32; 4]` / `float4`.
    Float4,
    /// `u32` scalar / `uint`.
    Uint1,
    /// `[u32; 2]` / `uint2`.
    Uint2,
    /// `[u32; 4]` / `uint4`.
    Uint4,
}

impl AttrFormat {
    /// Size of this attribute in bytes.
    pub const fn size(self) -> usize {
        match self {
            AttrFormat::Float1 | AttrFormat::Uint1 => 4,
            AttrFormat::Float2 | AttrFormat::Uint2 => 8,
            AttrFormat::Float3 => 12,
            AttrFormat::Float4 | AttrFormat::Uint4 => 16,
        }
    }

    /// Number of scalar components (1..=4).
    pub const fn components(self) -> u32 {
        match self {
            AttrFormat::Float1 | AttrFormat::Uint1 => 1,
            AttrFormat::Float2 | AttrFormat::Uint2 => 2,
            AttrFormat::Float3 => 3,
            AttrFormat::Float4 | AttrFormat::Uint4 => 4,
        }
    }

    /// Whether the scalar base type is integer (`uint`) rather than `float`.
    pub const fn is_integer(self) -> bool {
        matches!(
            self,
            AttrFormat::Uint1 | AttrFormat::Uint2 | AttrFormat::Uint4
        )
    }
}

/// One field of a `#[derive(GpuInstance)]` struct, as the GPU sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceField {
    /// The struct field's name (matched against the shader's attribute name).
    pub name: &'static str,
    /// Byte offset of the field within the instance struct (from `offset_of!`).
    pub offset: usize,
    /// The field's GPU attribute format.
    pub format: AttrFormat,
}

/// The full instance layout emitted by `#[derive(GpuInstance)]`.
///
/// `stride` is `size_of::<Instance>()`; `fields` are in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceLayout {
    /// Byte stride between consecutive instances (`size_of::<Self>()`).
    pub stride: usize,
    /// The fields, in struct declaration order.
    pub fields: &'static [InstanceField],
}

/// A shader's declaration of the instance layout it expects.
///
/// Written by hand alongside each shader (`viso-shader`), then checked against
/// the derived [`InstanceLayout`] via [`InstanceLayout::validate_against`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceSchema {
    /// The attributes the shader's vertex-input struct declares, in order.
    pub attributes: &'static [SchemaAttr],
}

/// One attribute expected by a shader: name + format (offset is derived from
/// the matching struct field, not re-declared here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaAttr {
    /// Attribute name; must match a struct field name.
    pub name: &'static str,
    /// Expected attribute format; must match the struct field's format.
    pub format: AttrFormat,
}

/// Why an [`InstanceLayout`] failed to match an [`InstanceSchema`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// The struct and schema declare a different number of attributes.
    CountMismatch {
        /// Attribute count in the derived instance layout.
        layout: usize,
        /// Attribute count declared by the shader schema.
        schema: usize,
    },
    /// An attribute name differs between struct and schema at `index`.
    NameMismatch {
        /// Position in declaration order.
        index: usize,
        /// Name from the derived instance layout.
        layout: &'static str,
        /// Name from the shader schema.
        schema: &'static str,
    },
    /// An attribute's format differs between struct and schema.
    FormatMismatch {
        /// The attribute name (shared).
        name: &'static str,
        /// Format from the derived instance layout.
        layout: AttrFormat,
        /// Format from the shader schema.
        schema: AttrFormat,
    },
}

impl InstanceLayout {
    /// Check that this derived layout matches a shader's declared schema.
    ///
    /// Compares attribute count, names, and formats in declaration order. Called
    /// at pipeline-registration time (cold path), satisfying the exit criterion
    /// "GPU instance layout has compile-time/registration validation".
    pub fn validate_against(&self, schema: &InstanceSchema) -> Result<(), LayoutError> {
        if self.fields.len() != schema.attributes.len() {
            return Err(LayoutError::CountMismatch {
                layout: self.fields.len(),
                schema: schema.attributes.len(),
            });
        }
        for (index, (field, attr)) in self.fields.iter().zip(schema.attributes).enumerate() {
            if field.name != attr.name {
                return Err(LayoutError::NameMismatch {
                    index,
                    layout: field.name,
                    schema: attr.name,
                });
            }
            if field.format != attr.format {
                return Err(LayoutError::FormatMismatch {
                    name: field.name,
                    layout: field.format,
                    schema: attr.format,
                });
            }
        }
        Ok(())
    }
}
