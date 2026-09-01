//! `viso-shader` — the shader compilation pipeline (§19).
//!
//! Flow: source → parsed syntax → typed IR → validation → backend codegen.
//! On hot reload, a shader compile error must preserve the last-good pipeline
//! where possible.
//!
//! Phase 0 status: contract-only skeleton.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod msl;

pub use msl::{
    GLYPHRUN_MSL, IMAGE_MSL, MESH_MSL, PrimitiveKind, QUAD_MSL, glyphrun_schema, image_schema,
    instance_schema, mesh_schema, quad_schema, shader_source,
};

/// The stages a shader source passes through, in order (§19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileStage {
    Source,
    ParsedSyntax,
    TypedIr,
    Validation,
    BackendCodegen,
}
