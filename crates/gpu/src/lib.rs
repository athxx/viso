//! `viso-gpu` — a deliberately small GPU RHI (§17).
//!
//! Concepts stay close to: Device, Queue, Buffer, Texture, Sampler, Pipeline,
//! BindGroup, CommandEncoder, Surface, Fence. The backend is selected at
//! compile time; there is no per-primitive `dyn GpuBackend` dispatch (§17.2).
//!
//! This crate must never see widgets, layout, or state (§17.1).
//!
//! Phase 0 status: typed resource-handle contract + the `GpuInstance` marker.
//! No backend implementation.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod headless;
pub mod instance;
/// The native macOS Metal backend (compiled only on macOS; ADR-007 cfg select).
#[cfg(target_os = "macos")]
pub mod metal;
pub mod resource;

pub use backend::{DrawCommand, DrawList, Frame, GpuBackend, LoadOp, RenderPass, RenderTarget};
pub use headless::HeadlessRaster;
#[cfg(target_os = "macos")]
pub use metal::MetalBackend;

/// The concrete GPU backend for this target, selected at compile time.
///
/// ADR-007: there is one [`GpuBackend`] trait for source-level unification, but
/// the facade holds *this concrete type* monomorphized so the frame hot path has
/// no `dyn GpuBackend` dispatch. On macOS it is the native [`MetalBackend`];
/// everywhere else (CI, unported targets) it is the software [`HeadlessRaster`],
/// which always compiles and needs no GPU.
#[cfg(target_os = "macos")]
pub type Backend = MetalBackend;
/// The concrete GPU backend for this target (non-macOS: software raster).
#[cfg(not(target_os = "macos"))]
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
    FilterMode, PipelineDesc, SamplerDesc, TextureDesc, TextureFormat,
};
/// Re-exported so consumers of [`GpuBackend::create_surface`] can name the
/// handle type without depending on `viso-handle` directly.
pub use viso_handle::RawWindowHandle;

/// Derive an explicit, validated GPU instance layout for a `#[repr(C)]` struct.
///
/// Re-exported from `viso-macros` so users import both the [`GpuInstance`] trait
/// and its derive from `viso_gpu`. The derive generates `unsafe impl
/// GpuInstance` plus an inherent `const LAYOUT: InstanceLayout` and
/// `validate_against` — see the trait docs and `viso-macros`.
pub use viso_macros::GpuInstance;

/// Typed, cheap-to-copy handles for GPU resources. Backends map these to
/// their own native objects; users and upper layers never see raw pointers.
macro_rules! resource_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(pub u32);
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
/// intended to be implemented only by the `#[derive(GpuInstance)]` macro,
/// which validates field offsets, alignment, types, and the matching shader
/// declaration — so the framework never relies on the implicit
/// "everything after field X is GPU memory" assumption that legacy makepad's
/// `DrawVars` leaned on.
///
/// # Safety
/// Implementors must be `#[repr(C)]` and contain only GPU-uploadable fields
/// with layout matching the declared instance schema. Hand-implementing this
/// requires the same safety documentation and tests the derive generates.
pub unsafe trait GpuInstance: Copy + 'static {
    /// Size in bytes of one instance, as seen by the GPU.
    const STRIDE: usize;
}
