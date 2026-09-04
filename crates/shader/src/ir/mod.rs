//! The typed shader IR: one strongly-typed value per built-in primitive that is
//! the single source of truth for both the emitted MSL and the validated
//! [`InstanceSchema`](viso_gpu::InstanceSchema) (architecture section 36 / AGENTS 19).
//!
//! - [`types`] — the IR type system (scalar/vector, MSL spelling, `AttrFormat`
//!   projection, packed size/alignment for the offset cross-check).
//! - [`module`] — one [`ShaderIr`](module::ShaderIr) description plus the four
//!   built-in constructors (`quad_ir`/`image_ir`/`glyphrun_ir`/`mesh_ir`), the
//!   only hand-written per-primitive field contracts.
//! - [`codegen_msl`] — IR → MSL (`emit_msl`) and IR → schema attributes.

pub mod codegen_msl;
pub mod module;
pub mod types;

#[cfg(test)]
pub mod testdata;
