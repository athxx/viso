//! `viso-gpu-abi` — what a GPU backend reads, described without one
//! (ADR 0042).
//!
//! An instance struct's layout and the shader's declared [`InstanceSchema`]
//! ([`instance`]); a user program's code, its ABI and the CPU program the
//! software raster runs ([`program`]); the descriptors and formats of buffers,
//! textures, samplers and pipelines ([`resource`]); the 3D scene built-in's
//! vertex, instance and uniform layouts ([`scene`]); and the typed handles a
//! backend returns for them. `viso-gpu` re-exports all of it next to the
//! backends.

mod ids;
pub mod instance;
pub mod program;
pub mod resource;
pub mod scene;

pub use ids::{BindGroupId, BufferId, PipelineId, SamplerId, SurfaceId, TextureId};
pub use instance::{
    AttrFormat, InstanceField, InstanceLayout, InstanceSchema, LayoutError, SchemaAttr,
};
pub use program::{
    CpuDraw, CpuProgram, CpuTarget, CpuTexels, CpuTexture, PROGRAM_SAMPLERS, PROGRAM_TEXTURES,
    Packed16, ProgramAbi, ProgramCode, ProgramCompiler, ProgramDesc, rgb565_to_8,
};
pub use resource::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BufferUsage, BuiltinShader, Caps,
    ColorDomain, ColorSpace, CompareFunction, CullMode, DepthState, FilterMode, FormatBlock,
    PipelineDesc, SampledFormats, SamplerDesc, ShaderCode, ShaderLang, TextureDesc, TextureFormat,
    f16_to_f32, f32_to_f16,
};
