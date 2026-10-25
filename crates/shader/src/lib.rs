//! `viso-shader` — the shader compilation pipeline.
//!
//! Flow: source → parsed syntax → typed IR → validation → backend codegen.
//! On hot reload, a shader compile error must preserve the last-good pipeline
//! where possible.
//!
//! The pipeline is real: a typed shader [`ir`] describes each built-in
//! primitive's GPU interface once, and both the emitted MSL and the validated
//! [`InstanceSchema`](viso_gpu_abi::InstanceSchema) project from that single value
//! (see the [`ir`] module docs). Packing rules and per-primitive math are
//! represented directly in a typed IR
//! without sharing a script VM.
//!
//! Diagnostics are self-contained: `viso-shader` sits below `viso-dsl` in the
//! crate DAG, so it cannot import the DSL's `Diagnostic`; sharing diagnostic
//! infrastructure would be permitted, but the dependency direction forbids that
//! specific edge, so this crate carries its own.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod code;
pub mod diag;
pub mod ir;
pub mod manifest;
pub mod msl;
pub mod program;
pub mod reload;

pub use code::shader_code;
pub use diag::{Diagnostic, Severity};
pub use manifest::{
    ColorTargetClass, PipelineEntry, PipelineFamily, PipelineManifest, VariantKey,
    standard_manifest,
};
pub use msl::{
    ADVANCED_BLEND_MSL, ANALYTIC_CAPSULE_MSL, ANALYTIC_ELLIPSE_MSL, ANALYTIC_LINE_MSL,
    ANALYTIC_RRECT_MSL, ANALYTIC_SHADOW_MSL, COLOR_TRANSFORM_MSL, GLYPHRUN_MSL, GRADIENT_MSL,
    IMAGE_MSL, MESH_MSL, OUTPUT_ENCODE_MSL, PrimitiveKind, QUAD_MSL, SCENE_MSL,
    advanced_blend_schema, analytic_capsule_schema, analytic_ellipse_schema, analytic_line_schema,
    analytic_rrect_schema, analytic_shadow_schema, color_transform_schema, glyphrun_schema,
    gradient_schema, image_schema, instance_schema, mesh_schema, output_encode_schema, quad_schema,
    scene_schema, shader_source,
};
pub use reload::{CompiledShader, ShaderPipeline};

/// The stages a shader source passes through, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileStage {
    Source,
    ParsedSyntax,
    TypedIr,
    Validation,
    BackendCodegen,
}
