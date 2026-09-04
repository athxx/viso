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
    /// An attribute's real byte offset in the `#[repr(C)]` struct (from
    /// `offset_of!`) does not match the offset the shader reads it at (the tight
    /// prefix-sum of preceding attribute sizes). This is the section-36.1 CPU↔GPU
    /// cross-check: a name/format match is not enough if padding, field reordering,
    /// or a wrong `repr` shifts a field, so the two sides would read different bytes
    /// for the same attribute.
    OffsetMismatch {
        /// The attribute name (shared).
        name: &'static str,
        /// Real byte offset in the derived instance layout (`offset_of!`).
        cpu_offset: usize,
        /// Byte offset the shader reads the attribute at (packed prefix-sum).
        shader_offset: usize,
    },
    /// The struct's byte stride (`size_of`) does not match the size the shader
    /// reads one instance as (the tight sum of all attribute sizes). Trailing
    /// padding or a stride mismatch would make the backend advance the wrong
    /// number of bytes between instances.
    StrideMismatch {
        /// Real byte stride of the derived instance layout (`size_of`).
        cpu_stride: usize,
        /// Byte stride the shader assumes (packed total of attribute sizes).
        shader_stride: usize,
    },
}

impl InstanceLayout {
    /// Check that this derived layout matches a shader's declared schema.
    ///
    /// Compares attribute count, names, formats, **byte offsets, and stride** in
    /// declaration order. Called at pipeline-registration time (cold path),
    /// satisfying the exit criterion "GPU instance layout has
    /// compile-time/registration validation".
    ///
    /// The offset/stride cross-check is the section-36.1 CPU↔GPU ABI guard: the
    /// shader reads each attribute at the tight prefix-sum of preceding attribute
    /// sizes (all GPU attributes are 4-byte-aligned, so a shader packs them with no
    /// gaps), while the CPU offset is the real `#[repr(C)]` `offset_of!` value. If
    /// padding, field reordering, or a wrong `repr` shifts any field — or leaves
    /// trailing padding in the stride — the two sides read different bytes for the
    /// same attribute; catching that here turns silent memory corruption into a
    /// registration-time error (architecture section 30 / 53). Makepad has no such
    /// explicit cross-check; it relies on both sides happening to apply the same
    /// packing rule.
    pub fn validate_against(&self, schema: &InstanceSchema) -> Result<(), LayoutError> {
        self.validate_attrs(schema.attributes)
    }

    /// Like [`validate_against`](Self::validate_against) but takes a borrowed
    /// attribute slice of any lifetime rather than the `'static`-bound
    /// [`InstanceSchema`]. The hot-reload holder in `viso-shader` owns its
    /// candidate schema attributes (so a rejected reload can be dropped without
    /// leaking to `'static`) and validates them through here; the pipeline
    /// registration path calls [`validate_against`](Self::validate_against) with a
    /// `'static` schema. Both share this one implementation.
    pub fn validate_attrs(&self, attrs: &[SchemaAttr]) -> Result<(), LayoutError> {
        if self.fields.len() != attrs.len() {
            return Err(LayoutError::CountMismatch {
                layout: self.fields.len(),
                schema: attrs.len(),
            });
        }
        // The offset the shader reads the next attribute at: the running sum of the
        // sizes of the attributes before it, with no padding.
        let mut shader_offset = 0usize;
        for (index, (field, attr)) in self.fields.iter().zip(attrs).enumerate() {
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
            if field.offset != shader_offset {
                return Err(LayoutError::OffsetMismatch {
                    name: field.name,
                    cpu_offset: field.offset,
                    shader_offset,
                });
            }
            // Formats match here, so either side's size is the attribute's size.
            shader_offset += attr.format.size();
        }
        // After the loop `shader_offset` is the packed total of all attributes — the
        // stride the shader assumes. It must equal the real `#[repr(C)]` stride.
        if self.stride != shader_offset {
            return Err(LayoutError::StrideMismatch {
                cpu_stride: self.stride,
                shader_stride: shader_offset,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A two-field schema the layouts below are validated against: a `float2` at
    // the tight offset 0 and a `float4` at the tight offset 8.
    const SCHEMA: InstanceSchema = InstanceSchema {
        attributes: &[
            SchemaAttr {
                name: "a",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "b",
                format: AttrFormat::Float4,
            },
        ],
    };

    #[test]
    fn tightly_packed_layout_validates() {
        // Offsets 0/8 and stride 24 are exactly the packed prefix-sums, so the
        // CPU and shader sides read identical bytes.
        let layout = InstanceLayout {
            stride: 24,
            fields: &[
                InstanceField {
                    name: "a",
                    offset: 0,
                    format: AttrFormat::Float2,
                },
                InstanceField {
                    name: "b",
                    offset: 8,
                    format: AttrFormat::Float4,
                },
            ],
        };
        assert_eq!(layout.validate_against(&SCHEMA), Ok(()));
    }

    #[test]
    fn shifted_field_is_an_offset_mismatch() {
        // `b` sits at 16 (as if 8 bytes of padding preceded it) while the shader
        // reads it at the packed offset 8: a name/format match is not enough.
        let layout = InstanceLayout {
            stride: 32,
            fields: &[
                InstanceField {
                    name: "a",
                    offset: 0,
                    format: AttrFormat::Float2,
                },
                InstanceField {
                    name: "b",
                    offset: 16,
                    format: AttrFormat::Float4,
                },
            ],
        };
        assert_eq!(
            layout.validate_against(&SCHEMA),
            Err(LayoutError::OffsetMismatch {
                name: "b",
                cpu_offset: 16,
                shader_offset: 8,
            })
        );
    }

    #[test]
    fn trailing_padding_is_a_stride_mismatch() {
        // Every field offset is tight, but the struct's stride carries 8 bytes of
        // trailing padding the shader does not account for.
        let layout = InstanceLayout {
            stride: 32,
            fields: &[
                InstanceField {
                    name: "a",
                    offset: 0,
                    format: AttrFormat::Float2,
                },
                InstanceField {
                    name: "b",
                    offset: 8,
                    format: AttrFormat::Float4,
                },
            ],
        };
        assert_eq!(
            layout.validate_against(&SCHEMA),
            Err(LayoutError::StrideMismatch {
                cpu_stride: 32,
                shader_stride: 24,
            })
        );
    }
}
