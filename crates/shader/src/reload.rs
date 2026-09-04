//! Hot-reload last-good pipeline holder (architecture section 36 / AGENTS 19).
//!
//! AGENTS 19 requires that a shader compile error during hot reload preserve the
//! last-good pipeline when possible. This module is that guarantee for the shader
//! side: a [`ShaderPipeline`] holds one [`CompiledShader`] — the IR, its emitted
//! MSL, and its validated schema — and [`ShaderPipeline::reload`] only replaces it
//! **atomically on success**. A candidate that fails compilation or ABI validation
//! returns diagnostics and leaves `last_good` byte-for-byte untouched.
//!
//! This is structurally the same keep-last-good invariant the Viso DSL enforces
//! in `viso-dsl`'s `hotreload` (compile → validate → commit; on failure the live
//! state stays at last-good with no snapshot to restore), but **independent and
//! VM-free**: there is only plain data here — an [`ir::module::ShaderIr`], an owned
//! MSL `String`, and a schema — plus the cold-path validation. No script VM is
//! shared, satisfying the AGENTS 19 divergence (the shader and DSL compilers may
//! share diagnostic *shape* but never a runtime VM).
//!
//! Validation reuses the GPU-side [`InstanceLayout::validate_against`], which since
//! the section-36.1 change cross-checks attribute count, name, format, **byte
//! offset, and stride** against the CPU `#[repr(C)]` layout. So a reload whose IR
//! disagrees with the real instance struct — a shifted field, a wrong format, a
//! stride mismatch — is rejected here rather than silently corrupting GPU memory
//! at draw time (architecture section 30 / 53).

use viso_gpu::{InstanceLayout, LayoutError, SchemaAttr};

use crate::CompileStage;
use crate::diag::Diagnostic;
use crate::ir::codegen_msl::emit_msl;
use crate::ir::module::ShaderIr;

/// The stable diagnostic code for a reload rejected by ABI validation.
const CODE_ABI_MISMATCH: &str = "Shader0001";

/// One successfully compiled shader: the typed IR, the MSL it emits, and the
/// schema the CPU instance layout was validated against. All three project from
/// the one `ir` (see the [`ir`](crate::ir) module docs), so they cannot drift.
#[derive(Debug, Clone)]
pub struct CompiledShader {
    /// The typed IR this shader was compiled from — the single source of truth.
    pub ir: ShaderIr,
    /// The emitted MSL for the Metal backend.
    pub msl: String,
    /// The instance schema, projected from `ir`'s attribute list. Its attributes
    /// are owned here so a [`CompiledShader`] is self-contained (unlike the
    /// `msl.rs` accessors, which cache to `'static` for the pipeline-registration
    /// path); [`CompiledShader::schema`] lends a borrowed [`InstanceSchema`].
    pub schema_attrs: Vec<SchemaAttr>,
}

impl CompiledShader {
    /// Compile a candidate IR into MSL + schema attributes. This is the pure
    /// codegen half of a reload; it never fails (validation is a separate step in
    /// [`ShaderPipeline::reload`], which needs the CPU layout to check against).
    fn compile(ir: &ShaderIr) -> CompiledShader {
        CompiledShader {
            ir: ir.clone(),
            msl: emit_msl(ir),
            schema_attrs: ir.schema_attrs(),
        }
    }

    /// The shader's declared instance schema as a borrowed attribute slice.
    ///
    /// Returns the slice rather than an [`InstanceSchema`] because that type
    /// borrows its attributes for `'static` (for the pipeline-registration path),
    /// which a `&self` method cannot provide; the reload path validates through
    /// [`InstanceLayout::validate_attrs`], which takes a slice of any lifetime.
    pub fn schema_attrs(&self) -> &[SchemaAttr] {
        &self.schema_attrs
    }
}

/// A hot-reloadable shader pipeline that always holds a last-good compiled shader.
///
/// Constructed from an initial IR that has already been validated against its CPU
/// layout (via [`ShaderPipeline::new`]); thereafter [`ShaderPipeline::reload`]
/// swaps in a new IR only when it compiles and validates, so `last_good` is never
/// left in a broken state.
#[derive(Debug, Clone)]
pub struct ShaderPipeline {
    last_good: CompiledShader,
}

impl ShaderPipeline {
    /// Build a pipeline from an IR and the CPU instance layout it must match,
    /// validating them up front. Returns diagnostics (leaving nothing behind) if
    /// the initial IR does not agree with the layout — there is no prior good
    /// state to fall back to, so an invalid initial IR is a hard error.
    pub fn new(ir: &ShaderIr, cpu_layout: &InstanceLayout) -> Result<Self, Vec<Diagnostic>> {
        let compiled = CompiledShader::compile(ir);
        validate(&compiled, cpu_layout)?;
        Ok(Self {
            last_good: compiled,
        })
    }

    /// The current last-good compiled shader.
    pub fn last_good(&self) -> &CompiledShader {
        &self.last_good
    }

    /// Attempt to reload with a candidate IR, validated against `cpu_layout`.
    ///
    /// On success the candidate is compiled and **atomically** replaces
    /// `last_good`, and `Ok(())` is returned. On failure (ABI/layout validation)
    /// the diagnostics are returned and `last_good` is left **byte-for-byte
    /// unchanged** — the keep-last-good invariant. This mirrors the DSL's
    /// `reload(last_good, candidate)` without sharing its VM.
    pub fn reload(
        &mut self,
        candidate_ir: &ShaderIr,
        cpu_layout: &InstanceLayout,
    ) -> Result<(), Vec<Diagnostic>> {
        let candidate = CompiledShader::compile(candidate_ir);
        validate(&candidate, cpu_layout)?;
        // Only reached when validation passed: commit atomically.
        self.last_good = candidate;
        Ok(())
    }
}

/// Run the section-36.1 ABI validation of a compiled shader's schema against a CPU
/// instance layout, mapping any [`LayoutError`] to a self-contained
/// [`Diagnostic`]. Cold path (reload / registration), so allocating the message is
/// fine.
fn validate(compiled: &CompiledShader, cpu_layout: &InstanceLayout) -> Result<(), Vec<Diagnostic>> {
    match cpu_layout.validate_attrs(compiled.schema_attrs()) {
        Ok(()) => Ok(()),
        Err(err) => Err(vec![layout_error_diagnostic(&err)]),
    }
}

/// Render a [`LayoutError`] as a validation-stage [`Diagnostic`]. Each variant
/// gets a concrete message naming the offending attribute and the two disagreeing
/// values, so the user sees *what* the CPU struct and the shader disagreed on.
fn layout_error_diagnostic(err: &LayoutError) -> Diagnostic {
    let message = match err {
        LayoutError::CountMismatch { layout, schema } => {
            format!("instance layout has {layout} attributes but the shader declares {schema}")
        }
        LayoutError::NameMismatch {
            index,
            layout,
            schema,
        } => format!(
            "attribute #{index} is `{layout}` in the instance layout but `{schema}` in the shader"
        ),
        LayoutError::FormatMismatch {
            name,
            layout,
            schema,
        } => format!(
            "attribute `{name}` has format {layout:?} in the instance layout but {schema:?} in the shader"
        ),
        LayoutError::OffsetMismatch {
            name,
            cpu_offset,
            shader_offset,
        } => format!(
            "attribute `{name}` is at byte offset {cpu_offset} in the instance struct but the shader reads it at {shader_offset}"
        ),
        LayoutError::StrideMismatch {
            cpu_stride,
            shader_stride,
        } => format!(
            "instance stride is {cpu_stride} bytes but the shader packs one instance as {shader_stride}"
        ),
    };
    Diagnostic::error(CODE_ABI_MISMATCH, CompileStage::Validation, message)
        .with_note("the CPU instance struct and the shader's declared layout must agree byte-for-byte (architecture section 36.1)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Severity;
    use crate::ir::module::{image_ir, quad_ir};
    use viso_gpu::{AttrFormat, InstanceField};

    // A CPU layout matching `quad_ir`, tightly packed exactly as the shader
    // expects. Offsets are the packed prefix-sums of the quad attribute sizes:
    // rect_pos(8) rect_size(8) color(16) radius(4) border_width(4) border_color(16).
    const QUAD_FIELDS: &[InstanceField] = &[
        InstanceField {
            name: "rect_pos",
            offset: 0,
            format: AttrFormat::Float2,
        },
        InstanceField {
            name: "rect_size",
            offset: 8,
            format: AttrFormat::Float2,
        },
        InstanceField {
            name: "color",
            offset: 16,
            format: AttrFormat::Float4,
        },
        InstanceField {
            name: "radius",
            offset: 32,
            format: AttrFormat::Float1,
        },
        InstanceField {
            name: "border_width",
            offset: 36,
            format: AttrFormat::Float1,
        },
        InstanceField {
            name: "border_color",
            offset: 40,
            format: AttrFormat::Float4,
        },
    ];

    fn quad_layout() -> InstanceLayout {
        InstanceLayout {
            stride: 56,
            fields: QUAD_FIELDS,
        }
    }

    #[test]
    fn new_accepts_a_matching_ir_and_layout() {
        let pipeline = ShaderPipeline::new(&quad_ir(), &quad_layout()).expect("quad validates");
        assert_eq!(pipeline.last_good().ir.kind, quad_ir().kind);
        assert!(pipeline.last_good().msl.contains("struct InstanceIn {"));
    }

    #[test]
    fn valid_reload_replaces_last_good() {
        let mut pipeline = ShaderPipeline::new(&quad_ir(), &quad_layout()).expect("quad validates");
        // Reloading the same IR is trivially valid and updates last_good to the
        // freshly compiled copy.
        let before = pipeline.last_good().msl.clone();
        pipeline
            .reload(&quad_ir(), &quad_layout())
            .expect("reloading the same IR validates");
        assert_eq!(pipeline.last_good().msl, before);
    }

    #[test]
    fn invalid_reload_keeps_last_good_field_for_field() {
        let mut pipeline = ShaderPipeline::new(&quad_ir(), &quad_layout()).expect("quad validates");
        let good = pipeline.last_good().clone();

        // `image_ir` declares a different attribute set (rect_pos, rect_size,
        // uv_pos, uv_size, color) than the quad CPU layout, so validating it
        // against the quad layout must fail — and leave last_good intact.
        let result = pipeline.reload(&image_ir(), &quad_layout());
        let diags = result.expect_err("image IR does not match the quad layout");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].stage, CompileStage::Validation);

        // last_good is byte-for-byte the previous good compile.
        assert_eq!(pipeline.last_good().ir.kind, good.ir.kind);
        assert_eq!(pipeline.last_good().msl, good.msl);
        assert_eq!(pipeline.last_good().schema_attrs, good.schema_attrs);
    }

    #[test]
    fn offset_mismatch_is_rejected_and_reported() {
        // A CPU layout that shifts `rect_size` to offset 12 (as if 4 bytes of
        // padding preceded it) while the shader reads it at the packed offset 8.
        const SHIFTED: &[InstanceField] = &[
            InstanceField {
                name: "rect_pos",
                offset: 0,
                format: AttrFormat::Float2,
            },
            InstanceField {
                name: "rect_size",
                offset: 12,
                format: AttrFormat::Float2,
            },
            InstanceField {
                name: "color",
                offset: 20,
                format: AttrFormat::Float4,
            },
            InstanceField {
                name: "radius",
                offset: 36,
                format: AttrFormat::Float1,
            },
            InstanceField {
                name: "border_width",
                offset: 40,
                format: AttrFormat::Float1,
            },
            InstanceField {
                name: "border_color",
                offset: 44,
                format: AttrFormat::Float4,
            },
        ];
        let shifted = InstanceLayout {
            stride: 60,
            fields: SHIFTED,
        };
        let diags = ShaderPipeline::new(&quad_ir(), &shifted)
            .expect_err("a shifted field must fail validation");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, CODE_ABI_MISMATCH);
        // The message names the offending attribute and both offsets.
        assert!(diags[0].message.contains("rect_size"));
        assert!(!diags[0].notes.is_empty());
    }
}
