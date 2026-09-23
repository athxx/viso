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

pub mod backend;
pub mod headless;
pub mod instance;
/// The native Metal backend (compiled only on Apple targets; ADR-007 cfg select).
#[cfg(target_vendor = "apple")]
pub mod metal;
pub mod resource;
pub mod retire;
pub mod slots;
/// The native Vulkan backend (Linux/Android, or any target with the `vulkan`
/// feature).
#[cfg(any(feature = "vulkan", target_os = "linux", target_os = "android"))]
pub mod vulkan;

pub use backend::{
    DrawCommand, DrawList, Frame, Geometry, GpuBackend, IndexFormat, InlineUniforms, LoadOp,
    RenderPass, RenderTarget,
};
pub use headless::HeadlessRaster;
#[cfg(target_vendor = "apple")]
pub use metal::MetalBackend;
#[cfg(any(feature = "vulkan", target_os = "linux", target_os = "android"))]
pub use vulkan::VulkanBackend;

/// The concrete GPU backend for this target, selected at compile time.
///
/// ADR-007: there is one [`GpuBackend`] trait for source-level unification, but
/// the facade holds *this concrete type* monomorphized so the frame hot path has
/// no `dyn GpuBackend` dispatch. On macOS and iOS it is the native
/// [`MetalBackend`]; on Linux and Android the [`VulkanBackend`]; on targets
/// without a native backend it is the software [`HeadlessRaster`], which always
/// compiles and needs no GPU.
#[cfg(target_vendor = "apple")]
pub type Backend = MetalBackend;
/// The concrete GPU backend for this target.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub type Backend = VulkanBackend;
/// The concrete GPU backend for this target (software raster: no native
/// backend is selected here).
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
pub type Backend = HeadlessRaster;

/// Create the GPU device/backend for this target (ADR-007 cfg static select).
///
/// The facade calls this once at launch and stores the returned [`Backend`] by
/// value. Backends that need a live window (Metal) create their surface later
/// via [`GpuBackend::create_surface`]; construction here only opens the device.
pub fn create_device() -> Backend {
    Backend::new()
}
pub use instance::{
    AttrFormat, InstanceField, InstanceLayout, InstanceSchema, LayoutError, SchemaAttr,
};
pub use resource::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BufferUsage, BuiltinShader, Caps,
    ColorDomain, ColorSpace, FilterMode, PipelineDesc, SamplerDesc, ShaderCode, ShaderLang,
    TextureDesc, TextureFormat, f16_to_f32, f32_to_f16,
};
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

/// Typed, cheap-to-copy handles for GPU resources. Backends map these to
/// their own native objects; users and upper layers never see raw pointers.
///
/// Each handle is generation-safe: `index` selects a storage slot and
/// `generation` is bumped when that slot is reclaimed, so a handle left over
/// from a destroyed resource is detectable and never resolves to whatever new
/// resource later took its slot. The `{index, generation}` shape matches the
/// runtime `NodeId` (§8.2).
macro_rules! resource_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[repr(C)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name {
            /// Storage-slot index.
            pub index: u32,
            /// Slot generation; bumped on reclaim so stale handles miss.
            pub generation: u32,
        }

        impl $name {
            /// A handle to `index` in generation 0.
            #[inline]
            pub const fn new(index: u32) -> Self {
                Self {
                    index,
                    generation: 0,
                }
            }

            /// The storage-slot index this handle selects.
            #[inline]
            pub const fn index(self) -> u32 {
                self.index
            }
        }

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
    };
}

resource_id!(/// Handle to a GPU buffer.
    BufferId);
resource_id!(/// Handle to a GPU texture.
    TextureId);
resource_id!(/// Handle to a sampler.
    SamplerId);
resource_id!(/// Handle to a render/compute pipeline.
    PipelineId);
resource_id!(/// Handle to a bind group.
    BindGroupId);
resource_id!(/// Handle to a swapchain/surface.
    SurfaceId);

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
