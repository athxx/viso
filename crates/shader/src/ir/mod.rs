//! The typed shader IR: one strongly-typed value per built-in primitive that is
//! the single source of truth for both the emitted MSL and the validated
//! [`InstanceSchema`](viso_gpu_abi::InstanceSchema).
//!
//! - [`types`] — the IR type system (scalar/vector, MSL spelling, `AttrFormat`
//!   projection, packed size/alignment for the offset cross-check).
//! - [`module`] — one [`ShaderIr`](module::ShaderIr) description plus the four
//!   built-in constructors (`quad_ir`/`image_ir`/`glyphrun_ir`/`mesh_ir`), the
//!   only hand-written per-primitive field contracts.
//! - [`body`] — the vertex/helper/fragment bodies as a parsed, typed tree that
//!   prints to MSL, WGSL, HLSL and GLSL.
//! - [`codegen_msl`] — IR → MSL (`emit_msl`) and IR → schema attributes.
//! - [`codegen_wgsl`] — IR → WGSL (`emit_wgsl`) for WebGPU and SPIR-V.
//! - [`codegen_hlsl`] — IR → HLSL shader model 5.1 (`emit_hlsl`) for D3D12.
//! - [`codegen_glsl`] — IR → GLSL ES 3.00 (`emit_glsl`) for OpenGL ES 3.

pub mod body;
pub mod codegen_glsl;
pub mod codegen_hlsl;
pub mod codegen_msl;
pub mod codegen_wgsl;
pub mod module;
pub mod types;

#[cfg(test)]
pub mod testdata;
