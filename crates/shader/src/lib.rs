//! `viso-shader` — the shader compilation pipeline (architecture section 36 /
//! AGENTS 19).
//!
//! Flow: source → parsed syntax → typed IR → validation → backend codegen.
//! On hot reload, a shader compile error must preserve the last-good pipeline
//! where possible.
//!
//! The pipeline is real: a typed shader [`ir`] describes each built-in
//! primitive's GPU interface once, and both the emitted MSL and the validated
//! [`InstanceSchema`](viso_gpu::InstanceSchema) project from that single value
//! (see the [`ir`] module docs). This takes makepad's shader *semantics* — its
//! packing rule and per-primitive math — and rebuilds them as a genuine IR
//! without sharing a script VM, the divergence AGENTS 19 mandates.
//!
//! Diagnostics are self-contained: `viso-shader` sits below `viso-dsl` in the
//! crate DAG, so it cannot import the DSL's `Diagnostic`; architecture section 36
//! permits sharing diagnostic infrastructure but the dependency direction
//! forbids that specific edge, so this crate carries its own.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod ir;
pub mod msl;

pub use msl::{
    GLYPHRUN_MSL, IMAGE_MSL, MESH_MSL, PrimitiveKind, QUAD_MSL, glyphrun_schema, image_schema,
    instance_schema, mesh_schema, quad_schema, shader_source,
};

/// The stages a shader source passes through, in order (architecture section 36
/// / AGENTS 19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileStage {
    Source,
    ParsedSyntax,
    TypedIr,
    Validation,
    BackendCodegen,
}
