//! `viso-gpu` — a deliberately small GPU RHI (§17).
//!
//! Concepts stay close to: Device, Queue, Buffer, Texture, Sampler, Pipeline,
//! BindGroup, CommandEncoder, Surface, Fence. The backend is selected at
//! compile time; there is no per-primitive `dyn GpuBackend` dispatch (§17.2).
//!
//! This crate must never see widgets, layout, or state (§17.1).
//!
//! Resource handles are generation-safe (`{index, generation}`): a stale handle
//! is detectable and never resolves to a different live resource.

#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(all(
    feature = "vulkan",
    not(any(target_os = "linux", target_os = "android"))
))]
extern crate ash_portable as ash;

pub mod backend;
pub mod cpu;
/// The native Direct3D 12 backend (compiled only on Windows).
#[cfg(target_os = "windows")]
pub mod d3d12;
/// The OpenGL ES 3 backend (Linux/Android, with the `gles` feature).
#[cfg(all(feature = "gles", any(target_os = "linux", target_os = "android")))]
pub mod gles;
/// The native Metal backend (compiled only on Apple targets; ADR 0029 cfg select).
#[cfg(target_vendor = "apple")]
pub mod metal;
pub mod retire;
pub mod slots;
/// The Vulkan backend (Linux/Android with `native-vulkan`, or any target with
/// `vulkan`).
#[cfg(any(
    feature = "vulkan",
    all(
        any(target_os = "linux", target_os = "android"),
        feature = "native-vulkan"
    )
))]
pub mod vulkan;
/// The WebGPU backend (compiled only for `wasm32-unknown-unknown`).
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod webgpu;

pub use backend::{
    DepthAttachment, DrawCommand, DrawList, Frame, Geometry, GpuBackend, IndexFormat,
    InlineUniforms, LoadOp, ProgramOf, RenderPass, RenderTarget,
};
pub use cpu::{CpuCompiler, CpuRaster, CpuRasterProgram};
#[cfg(target_os = "windows")]
pub use d3d12::{D3D12Backend, D3D12Compiler, D3D12Program};
#[cfg(all(feature = "gles", any(target_os = "linux", target_os = "android")))]
pub use gles::{Gles3Backend, Gles3Compiler, Gles3Program};
#[cfg(target_vendor = "apple")]
pub use metal::{MetalBackend, MetalCompiler, MetalProgram};
#[cfg(any(
    feature = "vulkan",
    all(
        any(target_os = "linux", target_os = "android"),
        feature = "native-vulkan"
    )
))]
pub use vulkan::{VulkanBackend, VulkanCompiler, VulkanProgram};
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use webgpu::{WebGpuBackend, WebGpuCompiler, WebGpuProgram};

/// The concrete GPU backend for this target, selected at compile time.
///
/// ADR 0029: there is one [`GpuBackend`] trait for source-level unification, but
/// the facade holds *this concrete type* monomorphized so the frame hot path has
/// no `dyn GpuBackend` dispatch. On macOS and iOS it is the native
/// [`MetalBackend`]; on Windows the [`D3D12Backend`]; on Linux and Android the
/// OpenGL ES 3 `Gles3Backend` with the `gles` feature, else the
/// `VulkanBackend` with `native-vulkan` (on by default); on the web the
/// [`WebGpuBackend`]; on targets without a native backend, on Linux and
/// Android with neither, and in a build with the `headless` or `cpu` feature,
/// it is the software [`CpuRaster`], which always compiles and needs no GPU.
#[cfg(all(
    target_vendor = "apple",
    not(any(feature = "headless", feature = "cpu"))
))]
pub type Backend = MetalBackend;
/// The concrete GPU backend for this target.
#[cfg(all(target_os = "windows", not(any(feature = "headless", feature = "cpu"))))]
pub type Backend = D3D12Backend;
/// The concrete GPU backend for this target.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    not(any(feature = "headless", feature = "cpu")),
    not(feature = "gles"),
    any(feature = "native-vulkan", feature = "vulkan")
))]
pub type Backend = VulkanBackend;
/// The concrete GPU backend for this target.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    not(any(feature = "headless", feature = "cpu")),
    feature = "gles"
))]
pub type Backend = Gles3Backend;
/// The concrete GPU backend for this target.
#[cfg(all(
    target_arch = "wasm32",
    target_os = "unknown",
    not(any(feature = "headless", feature = "cpu"))
))]
pub type Backend = WebGpuBackend;
/// The concrete GPU backend for this target (software raster: the build is
/// headless or CPU-drawn, or no native backend is selected here).
#[cfg(any(
    feature = "headless",
    feature = "cpu",
    not(any(
        target_vendor = "apple",
        target_os = "windows",
        target_os = "linux",
        target_os = "android",
        all(target_arch = "wasm32", target_os = "unknown")
    )),
    all(
        any(target_os = "linux", target_os = "android"),
        not(any(feature = "gles", feature = "native-vulkan", feature = "vulkan"))
    )
))]
pub type Backend = CpuRaster;

/// The one of its expressions for this target's [`Backend`], the others
/// compiled out: how code built ahead of time for every backend embeds only
/// what this target's reads. `none` is the software raster's.
///
/// The choice is made where this crate compiles, so a crate expanding it
/// follows this crate's `headless`, `cpu` and `gles` features.
#[cfg(not(any(feature = "headless", feature = "cpu")))]
#[doc(hidden)]
#[macro_export]
macro_rules! __per_backend {
    (msl: $msl:expr, hlsl: $hlsl:expr, spirv: $spirv:expr, glsl: $glsl:expr, wgsl: $wgsl:expr, none: $none:expr $(,)?) => {{
        #[cfg(target_vendor = "apple")]
        let chosen = $msl;
        #[cfg(target_os = "windows")]
        let chosen = $hlsl;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let chosen = $crate::__linux_backend!($spirv, $glsl, $none);
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        let chosen = $wgsl;
        #[cfg(not(any(
            target_vendor = "apple",
            target_os = "windows",
            target_os = "linux",
            target_os = "android",
            all(target_arch = "wasm32", target_os = "unknown")
        )))]
        let chosen = $none;
        chosen
    }};
}

/// The one of its expressions for this target's [`Backend`]: in a headless
/// or CPU-drawn build, the software raster's.
#[cfg(any(feature = "headless", feature = "cpu"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __per_backend {
    (msl: $msl:expr, hlsl: $hlsl:expr, spirv: $spirv:expr, glsl: $glsl:expr, wgsl: $wgsl:expr, none: $none:expr $(,)?) => {
        $none
    };
}

/// Linux and Android's expression: GLSL with the `gles` feature, else SPIR-V
/// with Vulkan, else the software raster's.
#[cfg(feature = "gles")]
#[doc(hidden)]
#[macro_export]
macro_rules! __linux_backend {
    ($spirv:expr, $glsl:expr, $none:expr) => {
        $glsl
    };
}

/// Linux and Android's expression: GLSL with the `gles` feature, else SPIR-V
/// with Vulkan, else the software raster's.
#[cfg(all(
    not(feature = "gles"),
    any(feature = "native-vulkan", feature = "vulkan")
))]
#[doc(hidden)]
#[macro_export]
macro_rules! __linux_backend {
    ($spirv:expr, $glsl:expr, $none:expr) => {
        $spirv
    };
}

/// Linux and Android's expression: GLSL with the `gles` feature, else SPIR-V
/// with Vulkan, else the software raster's.
#[cfg(not(any(feature = "gles", feature = "native-vulkan", feature = "vulkan")))]
#[doc(hidden)]
#[macro_export]
macro_rules! __linux_backend {
    ($spirv:expr, $glsl:expr, $none:expr) => {
        $none
    };
}

/// Create the GPU device/backend for this target (ADR 0029 cfg static select).
///
/// The facade calls this once at launch and stores the returned [`Backend`] by
/// value. Backends that need a live window (Metal) create their surface later
/// via [`GpuBackend::create_surface`]; construction here only opens the device.
pub fn create_device() -> Backend {
    Backend::new()
}
pub use viso_gpu_abi::{
    AddressMode, AttrFormat, BindGroupDesc, BindGroupId, Binding, BlendMode, BufferDesc, BufferId,
    BufferUsage, BuiltinShader, Caps, ColorDomain, ColorSpace, CompareFunction, CpuDraw,
    CpuProgram, CpuTarget, CpuTexels, CpuTexture, CullMode, DepthState, FilterMode, FormatBlock,
    InstanceField, InstanceLayout, InstanceSchema, LayoutError, PROGRAM_SAMPLERS, PROGRAM_TEXTURES,
    Packed16, PipelineDesc, PipelineId, ProgramAbi, ProgramCode, ProgramCompiler, ProgramDesc,
    SampledFormats, SamplerDesc, SamplerId, SchemaAttr, ShaderCode, ShaderLang, SurfaceId,
    TextureDesc, TextureFormat, TextureId, f16_to_f32, f32_to_f16, rgb565_to_8,
};
pub use viso_gpu_abi::{instance, program, resource, scene, srgb};
/// Re-exported so consumers of [`GpuBackend::create_surface`] can name the
/// handle type without depending on `viso-handle` directly.
pub use viso_handle::RawWindowHandle;

/// Derive an explicit, validated GPU instance layout for a `#[repr(C)]` struct.
///
/// Re-exported from `viso-macros` so users import both the [`GpuPod`] trait
/// and its derive from `viso_gpu`. The derive generates `unsafe impl
/// GpuPod` plus an inherent `const LAYOUT: InstanceLayout` and
/// `validate_against` — see the trait docs and `viso-macros`.
pub use viso_macros::GpuPod;

/// A typed handle as a raw `SlotMap` slot id and back, for the backends'
/// storage.
macro_rules! raw_id {
    ($($name:ident),+) => {$(
    impl From<$crate::slots::RawId> for $name {
        /// Stamp this handle's type onto a `SlotMap` slot id — the
        /// `{index, generation}` a backend gets back from `insert`.
        #[inline]
        fn from(raw: $crate::slots::RawId) -> Self {
            Self {
                index: raw.index,
                generation: raw.generation,
            }
        }
    }

    impl From<$name> for $crate::slots::RawId {
        /// Strip a typed handle back to a raw `SlotMap` slot id for a
        /// `get`/`remove` lookup.
        #[inline]
        fn from(id: $name) -> Self {
            $crate::slots::RawId {
                index: id.index,
                generation: id.generation,
            }
        }
    }
    )+};
}

raw_id!(
    BufferId,
    TextureId,
    SamplerId,
    PipelineId,
    BindGroupId,
    SurfaceId
);

/// Marker for a `#[repr(C)]` type that is safe to upload as per-instance GPU
/// data (§18).
///
/// Host structs and GPU instance data are separate concerns. This trait is
/// intended to be implemented only by the `#[derive(GpuPod)]` macro,
/// which validates field offsets, alignment, types, and the matching shader
/// declaration — so the framework never relies on an implicit
/// "everything after field X is GPU memory" assumption.
///
/// # Safety
/// Implementors must be `#[repr(C)]` and contain only GPU-uploadable fields
/// with layout matching the declared instance schema. Hand-implementing this
/// requires the same safety documentation and tests the derive generates.
pub unsafe trait GpuPod: Copy + 'static {
    /// Size in bytes of one instance, as seen by the GPU.
    const STRIDE: usize;
}
