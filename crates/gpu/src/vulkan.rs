//! `VulkanBackend` — the Vulkan implementation of [`GpuBackend`], the Linux and
//! Android path (and any target built with the `vulkan` feature).
//!
//! ## Design decisions
//!
//! - **Programs are frozen SPIR-V** ([`ShaderCode::SpirV`]): no shader compiler
//!   ships. Each module declares the uniforms as a push-constant block, the
//!   attributes at locations `0..n`, and `tex`/`dst_tex`/`samp` at bindings
//!   0/1/2 of set 0. Clip-space y is flipped in the vertex stage, so the viewport
//!   is the plain top-left one and every pixel-space convention (scissor,
//!   `position`, texture rows) matches Metal's.
//! - **One pipeline layout** serves every pipeline: set 0 with two sampled
//!   images and a sampler, plus [`InlineUniforms::MAX`] bytes of push constants
//!   visible to both stages — so a draw's uniforms are one `vkCmdPushConstants`.
//! - **Buffers are persistently mapped host-coherent memory** (device-local
//!   too where the heap allows it: UMA and resizable BAR), so `write_buffer` is a
//!   memcpy exactly as on Metal.
//! - **Textures are device-local optimal images** that rest in
//!   `SHADER_READ_ONLY_OPTIMAL` between commands. Every operation that needs
//!   another layout (upload, render, read-back) transitions away and back inside
//!   the command it records, so no per-image layout is tracked.
//! - **Two frame slots**, each with one command pool holding an *upload* and a
//!   *draw* command buffer, a fence, an acquire semaphore and a staging arena.
//!   Texture uploads record into the upload buffer and passes into the draw
//!   buffer; both are submitted together (uploads first) when the frame
//!   presents, or at the end of an `encode` that ran with no frame open. A slot
//!   is reopened only after its fence signals, which bounds the CPU to two
//!   frames ahead of the GPU.
//! - **Completion is read from the slot fences**: the retire queue is drained
//!   up to the oldest epoch any unfinished submission — or the open recording —
//!   still holds, so a retired resource is destroyed once nothing can use it.
//! - **Swapchains** are FIFO, recreated lazily (on resize, out-of-date or
//!   suboptimal) at the next `begin_frame`, with one render-finished semaphore
//!   per image.
//! - **User programs** ([`VulkanCompiler`]) are SPIR-V laid out as the shader
//!   program ABI the WGSL emitter declares: set 0 holds the uniform block
//!   (binding 0) and the instances as a read-only storage buffer (binding 1),
//!   set 1 the textures then the samplers. Each program has its own pipeline
//!   layout, built with its pipeline on any thread; its descriptor sets are
//!   written per draw from the bind group and the instance buffer, out of
//!   descriptor pools the frame slot rewinds when it is reopened. A draw's
//!   inline uniforms are staged into the slot's upload arena and bound as the
//!   uniform block. Like the built-ins, a program's SPIR-V flips clip-space y
//!   in its vertex stage.

use core::ffi::{CStr, c_char, c_void};
use std::ffi::CString;
use std::io::Cursor;
use std::sync::atomic::{AtomicU32, Ordering};

use ash::vk;
use ash::{ext, khr};
use viso_handle::{ExternalHold, ExternalImage, RawWindowHandle};

use crate::backend::{
    DrawCommand, DrawList, Frame, Geometry, GpuBackend, IndexFormat, InlineUniforms, LoadOp,
    RenderPass, RenderTarget,
};
use crate::instance::{AttrFormat, InstanceLayout, InstanceSchema};
use crate::program::{PROGRAM_TEXTURES, ProgramCode, ProgramCompiler, ProgramDesc};
use crate::resource::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BuiltinShader, Caps,
    CompareFunction, CullMode, DepthState, FilterMode, PipelineDesc, SampledFormats, SamplerDesc,
    ShaderCode, ShaderLang, TextureDesc, TextureFormat,
};
use crate::retire::{Epoch, Fence, ResourceKind, RetireQueue, Retired};
use crate::scene::{self, SceneInstance};
use crate::slots::SlotMap;
use crate::{BindGroupId, BufferId, Nv12, PipelineId, SamplerId, SurfaceId, TextureId};

/// Frames the CPU may record ahead of the GPU.
const FRAMES_IN_FLIGHT: usize = 2;

/// The most damage rects a present names; more name the whole image.
const PRESENT_RECTS: usize = 8;
/// The smallest staging chunk a frame slot allocates for texture uploads.
const STAGING_CHUNK: u64 = 4 << 20;
/// Descriptor sets per descriptor pool; a full pool is followed by a new one.
const SETS_PER_POOL: u32 = 256;
/// The binding of a 2D program's second texture, `dst_tex`, in set 0.
const BINDING_DST_TEX: u32 = scene::NATIVE_TEXTURE_BINDINGS[1];
/// Where a Vulkan loader lives on macOS when it is not on the default search
/// path; empty elsewhere.
const MACOS_LOADERS: &[&str] = if cfg!(target_os = "macos") {
    &[
        "/opt/homebrew/lib/libvulkan.1.dylib",
        "/usr/local/lib/libvulkan.1.dylib",
    ]
} else {
    &[]
};
/// The validation layer enabled by [`VulkanBackend::try_new_with`].
const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// A buffer: host-visible memory mapped for the buffer's lifetime.
struct VulkanBuffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    /// The persistent mapping of `memory`, `len` bytes.
    ptr: *mut u8,
    len: usize,
}

/// A texture: an optimal-tiling image, its view, and (for render targets) the
/// framebuffer passes draw into, built on first use.
struct VulkanTexture {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    format: TextureFormat,
    width: u32,
    height: u32,
    render_target: bool,
    framebuffer: vk::Framebuffer,
    /// The framebuffer of passes with a depth attachment, and the depth
    /// texture it was built with.
    depth_framebuffer: Option<(TextureId, vk::Framebuffer)>,
    /// A texture pictures are converted into.
    converted: Option<Converted>,
}

/// What a texture pictures are converted into keeps: its UNORM view, the
/// framebuffer over it a conversion draws into, and the images planes at
/// hand are uploaded into, made at the first.
struct Converted {
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    planes: Option<planes::Planes>,
}

/// A registered pipeline.
struct VulkanPipeline {
    pipeline: vk::Pipeline,
    /// A user program's own layout; `None` for the built-ins, which share one.
    program: Option<ProgramLayout>,
}

/// The pipeline layout of one user program: set 0 the uniform block and the
/// instance storage buffer, set 1 its textures then its samplers.
struct ProgramLayout {
    layout: vk::PipelineLayout,
    sets: [vk::DescriptorSetLayout; 2],
    textures: u32,
    samplers: u32,
    /// Bytes of the uniform block an inline payload is staged into.
    uniform_size: u32,
}

/// A bind group: one descriptor set of the shared layout, the pool it came
/// from, and the bindings themselves, which a program's per-draw sets read.
struct VulkanBindGroup {
    set: vk::DescriptorSet,
    pool: vk::DescriptorPool,
    bindings: Vec<Binding>,
}

/// What the pass being recorded has bound: the pipeline and the
/// shared-layout bind group.
#[derive(Default)]
struct Bound {
    pipeline: Option<PipelineId>,
    group: Option<BindGroupId>,
}

/// One swapchain image with its view and framebuffer.
struct SwapImage {
    image: vk::Image,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    /// The surface's present count when the image was last presented (`0`:
    /// not since the swapchain was built). A presented image keeps its pixels
    /// in `PRESENT_SRC_KHR`; otherwise its contents are undefined.
    presented: u64,
}

/// A window surface and its swapchain.
struct VulkanSurface {
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    images: Vec<SwapImage>,
    /// Render-finished semaphores, indexed like `images`. Grown when a rebuilt
    /// swapchain has more images, never shrunk while the surface lives: a present
    /// may still be waiting on one.
    finished: Vec<vk::Semaphore>,
    format: TextureFormat,
    vk_format: vk::Format,
    color_space: vk::ColorSpaceKHR,
    /// The size the platform last asked for, in physical pixels.
    width: u32,
    height: u32,
    /// The size the swapchain was built at.
    extent: vk::Extent2D,
    /// The swapchain must be rebuilt before the next acquire.
    stale: bool,
    /// Presents so far.
    presents: u64,
}

/// A host-visible buffer texture uploads are copied out of.
struct StagingChunk {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    ptr: *mut u8,
    size: u64,
}

/// A frame slot's upload arena: chunks filled front to back, rewound when the
/// slot is reopened (its previous submission has then finished reading them).
#[derive(Default)]
struct Staging {
    chunks: Vec<StagingChunk>,
    current: usize,
    cursor: u64,
}

/// Everything one in-flight frame owns.
struct FrameSlot {
    pool: vk::CommandPool,
    upload: vk::CommandBuffer,
    draw: vk::CommandBuffer,
    fence: vk::Fence,
    /// Signalled by the swapchain acquire, waited on by the draw submission.
    acquire: vk::Semaphore,
    /// A submission has been made and its fence not yet waited and reset.
    submitted: bool,
    /// The oldest epoch the last submission's commands were recorded in.
    epoch: Epoch,
    staging: Staging,
    /// The pools the slot's program draws allocate their descriptor sets
    /// from, rewound when the slot is reopened; `program_pool` is the one in
    /// use.
    program_pools: Vec<vk::DescriptorPool>,
    program_pool: usize,
    /// Whether the open recording stamped its start in the clock.
    clocking: bool,
    /// The frame epoch the last submission timed, until its time is read.
    timed: Option<u64>,
}

/// The device's timestamp queries, two per frame slot: where the slot's
/// recording began and where its last pass ended.
struct Clock {
    pool: vk::QueryPool,
    /// Nanoseconds per tick.
    period: f64,
    /// The bits a timestamp carries.
    mask: u64,
    on: bool,
    /// Frames timed and read, waiting for the profiler: epoch and ns.
    read: Vec<(u64, u64)>,
}

impl Clock {
    /// The most read times kept for a profiler that stopped asking.
    const KEPT: usize = 64;
}

/// The frame between `begin_frame` and `present`.
#[derive(Debug, Clone, Copy)]
struct Acquired {
    surface: SurfaceId,
    image: u32,
    /// The image's layout at the current end of the draw command buffer.
    layout: vk::ImageLayout,
}

/// One cached render pass.
struct RenderPassEntry {
    format: vk::Format,
    clear: bool,
    depth: Option<vk::Format>,
    pass: vk::RenderPass,
}

/// The Vulkan backend.
pub struct VulkanBackend {
    entry: ash::Entry,
    instance: ash::Instance,
    surface_fn: Option<khr::surface::Instance>,
    debug: Option<(ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    /// The validation-error counter the debug messenger increments; boxed so its
    /// address stays fixed while the messenger holds it.
    validation_errors: Box<AtomicU32>,
    physical: vk::PhysicalDevice,
    memory_props: vk::PhysicalDeviceMemoryProperties,
    device: ash::Device,
    queue: vk::Queue,
    queue_family: u32,
    swapchain_fn: Option<khr::swapchain::Device>,
    /// `VK_KHR_incremental_present` is on: a present names its damage.
    incremental_present: bool,
    set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline_cache: vk::PipelineCache,
    render_passes: Vec<RenderPassEntry>,
    /// Framebuffers replaced while a submitted frame may still use them, with
    /// the epoch they were replaced in.
    dead_framebuffers: Vec<(Epoch, vk::Framebuffer)>,
    descriptor_pools: Vec<vk::DescriptorPool>,
    slots: [FrameSlot; FRAMES_IN_FLIGHT],
    /// The slot being recorded or next to record.
    slot: usize,
    /// The epoch the open recording was begun in, if one is open.
    recording: Option<Epoch>,
    acquired: Option<Acquired>,
    buffers: SlotMap<VulkanBuffer>,
    textures: SlotMap<VulkanTexture>,
    samplers: SlotMap<vk::Sampler>,
    bind_groups: SlotMap<VulkanBindGroup>,
    pipelines: SlotMap<VulkanPipeline>,
    surfaces: SlotMap<VulkanSurface>,
    caps: Caps,
    retire_queue: RetireQueue,
    current_epoch: Epoch,
    reclaim_scratch: Vec<Retired>,
    /// The device's `minStorageBufferOffsetAlignment`: a program's instance
    /// offset is a multiple of it.
    storage_align: u64,
    /// The device's `minUniformBufferOffsetAlignment`, where a staged inline
    /// uniform block starts.
    uniform_align: u64,
    /// The frame clock, on a queue that takes timestamps.
    clock: Option<Clock>,
    /// The device converts Y′CbCr images (`samplerYcbcrConversion`).
    ycbcr: bool,
    /// The Y′CbCr conversions and sources, made at the first external
    /// picture.
    converter: Option<ycbcr::Converter>,
    /// The conversion of NV12 planes at hand, made at the first.
    planar: Option<planes::Planar>,
    /// The dma-bufs imported, read where they lie.
    imports: planes::Imports,
    /// Dma-buf import (`VK_KHR_external_memory_fd`,
    /// `VK_EXT_external_memory_dma_buf`).
    #[cfg(unix)]
    dma_bufs: Option<khr::external_memory_fd::Device>,
    /// Holds on external pictures, each let go once the epoch it was
    /// converted in completes.
    held: Vec<(Epoch, ExternalHold)>,
    /// Android hardware buffers import (`VK_ANDROID_external_memory_android_hardware_buffer`).
    #[cfg(target_os = "android")]
    hardware_buffers: Option<ash::android::external_memory_android_hardware_buffer::Device>,
}

mod planes;
mod ycbcr;

/// What `AHardwareBuffer_describe` tells of a buffer.
#[cfg(target_os = "android")]
#[repr(C)]
#[derive(Default)]
struct AHardwareBufferDesc {
    width: u32,
    height: u32,
    layers: u32,
    format: u32,
    usage: u64,
    stride: u32,
    rfu0: u32,
    rfu1: u64,
}

#[cfg(target_os = "android")]
#[link(name = "nativewindow")]
unsafe extern "C" {
    fn AHardwareBuffer_describe(buffer: *const c_void, desc: *mut AHardwareBufferDesc);
}

impl Default for VulkanBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Record every validation error into the counter `user` points at and print
/// every warning and error.
unsafe extern "system" fn on_validation_message(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user: *mut c_void,
) -> vk::Bool32 {
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        // SAFETY: `user` is the backend's boxed counter, which outlives the
        // messenger (it is destroyed before the box is dropped).
        unsafe { &*(user as *const AtomicU32) }.fetch_add(1, Ordering::Relaxed);
    }
    // SAFETY: the loader passes callback data valid for this call, whose
    // `p_message` is null or a NUL-terminated string.
    let message = unsafe {
        let p = (*data).p_message;
        if p.is_null() {
            "".into()
        } else {
            CStr::from_ptr(p).to_string_lossy()
        }
    };
    eprintln!("vulkan validation: {message}");
    vk::FALSE
}

impl VulkanBackend {
    /// Create the backend on the best available Vulkan device.
    ///
    /// # Panics
    /// Panics if there is no Vulkan loader or no device with a graphics queue,
    /// saying which step failed.
    pub fn new() -> Self {
        let validation = std::env::var_os("VISO_VULKAN_VALIDATION").is_some();
        Self::open(validation).unwrap_or_else(|why| panic!("no usable Vulkan device: {why}"))
    }

    /// Create the backend, or `None` if Vulkan is unavailable. Enables the
    /// Khronos validation layer when `VISO_VULKAN_VALIDATION` is set and the
    /// layer is installed.
    pub fn try_new() -> Option<Self> {
        Self::try_new_with(std::env::var_os("VISO_VULKAN_VALIDATION").is_some())
    }

    /// Create the backend, with the validation layer if `validation` and the
    /// layer is installed. Errors it reports are counted by
    /// [`validation_errors`](Self::validation_errors).
    pub fn try_new_with(validation: bool) -> Option<Self> {
        Self::open(validation).ok()
    }

    /// Create the backend, or say which step failed.
    fn open(validation: bool) -> Result<Self, String> {
        // SAFETY: loading the system Vulkan loader runs its initialisers, which
        // is the documented way to reach Vulkan; nothing else is loaded.
        let entry = unsafe { ash::Entry::load() }
            .ok()
            .or_else(|| {
                // On macOS the loader that reaches MoltenVK is installed outside
                // the default library search path (Homebrew, the Vulkan SDK).
                MACOS_LOADERS.iter().find_map(|path| {
                    // SAFETY: as above, a Vulkan loader at a fixed location.
                    unsafe { ash::Entry::load_from(path) }.ok()
                })
            })
            .ok_or("no Vulkan loader")?;

        // SAFETY: `entry` is a loaded Vulkan entry; enumeration has no
        // preconditions.
        let extensions = unsafe { entry.enumerate_instance_extension_properties(None) }
            .map_err(|e| format!("cannot enumerate the instance extensions: {e}"))?;
        let has_ext = |name: &CStr| {
            extensions
                .iter()
                .any(|e| e.extension_name_as_c_str() == Ok(name))
        };
        // SAFETY: as above.
        let layers = unsafe { entry.enumerate_instance_layer_properties() }.unwrap_or_default();
        let validation = validation
            && layers
                .iter()
                .any(|l| l.layer_name_as_c_str() == Ok(VALIDATION_LAYER));

        let mut enabled: Vec<*const c_char> = Vec::new();
        let has_surface = has_ext(khr::surface::NAME);
        if has_surface {
            enabled.push(khr::surface::NAME.as_ptr());
            for name in [
                khr::xcb_surface::NAME,
                khr::wayland_surface::NAME,
                khr::android_surface::NAME,
                khr::win32_surface::NAME,
            ] {
                if has_ext(name) {
                    enabled.push(name.as_ptr());
                }
            }
        }
        let mut flags = vk::InstanceCreateFlags::empty();
        if has_ext(khr::portability_enumeration::NAME) {
            enabled.push(khr::portability_enumeration::NAME.as_ptr());
            flags |= vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
        }
        let mut validation = validation && has_ext(ext::debug_utils::NAME);
        if validation {
            enabled.push(ext::debug_utils::NAME.as_ptr());
        }
        let layer_names = [VALIDATION_LAYER.as_ptr()];

        // SAFETY: as above.
        let loader_version = unsafe { entry.try_enumerate_instance_version() }
            .ok()
            .flatten()
            .unwrap_or(vk::API_VERSION_1_0);
        let api_version = if loader_version >= vk::API_VERSION_1_1 {
            vk::API_VERSION_1_1
        } else {
            vk::API_VERSION_1_0
        };
        let app = vk::ApplicationInfo::default()
            .application_name(c"viso")
            .engine_name(c"viso")
            .api_version(api_version);
        let create = |enabled: &[*const c_char], layers: &[*const c_char]| {
            let info = vk::InstanceCreateInfo::default()
                .flags(flags)
                .application_info(&app)
                .enabled_extension_names(enabled)
                .enabled_layer_names(layers);
            // SAFETY: every extension and layer name was enumerated as
            // available and is a 'static NUL-terminated string; `info` borrows
            // live locals.
            unsafe { entry.create_instance(&info, None) }
        };
        let instance = if validation {
            // A layer that is listed but whose library does not load (a
            // manifest off the library search path) fails the whole instance:
            // run without it.
            create(&enabled, &layer_names).or_else(|_| {
                validation = false;
                enabled.retain(|&name| name != ext::debug_utils::NAME.as_ptr());
                create(&enabled, &[])
            })
        } else {
            create(&enabled, &[])
        }
        .map_err(|e| format!("cannot create the instance: {e}"))?;

        let validation_errors = Box::new(AtomicU32::new(0));
        let debug = validation.then(|| {
            let loader = ext::debug_utils::Instance::new(&entry, &instance);
            let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                        | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(on_validation_message))
                .user_data(&*validation_errors as *const AtomicU32 as *mut c_void);
            // SAFETY: the debug-utils extension is enabled on `instance`; the
            // callback's user data is the boxed counter, which outlives the
            // messenger (see `Drop`).
            let messenger = unsafe { loader.create_debug_utils_messenger(&info, None) }
                .expect("failed to create the Vulkan debug messenger");
            (loader, messenger)
        });

        let (physical, queue_family) = match pick_physical_device(&instance) {
            Ok(picked) => picked,
            Err(why) => {
                // SAFETY: nothing was created from `instance` except the messenger,
                // which is destroyed first.
                unsafe {
                    if let Some((loader, messenger)) = &debug {
                        loader.destroy_debug_utils_messenger(*messenger, None);
                    }
                    instance.destroy_instance(None);
                }
                return Err(why);
            }
        };

        // SAFETY: `physical` was enumerated from `instance`.
        let device_exts = unsafe { instance.enumerate_device_extension_properties(physical) }
            .map_err(|e| format!("cannot enumerate the device extensions: {e}"))?;
        let has_device_ext = |name: &CStr| {
            device_exts
                .iter()
                .any(|e| e.extension_name_as_c_str() == Ok(name))
        };
        let mut device_enabled: Vec<*const c_char> = Vec::new();
        let has_swapchain = has_surface && has_device_ext(khr::swapchain::NAME);
        if has_swapchain {
            device_enabled.push(khr::swapchain::NAME.as_ptr());
        }
        if has_device_ext(khr::portability_subset::NAME) {
            device_enabled.push(khr::portability_subset::NAME.as_ptr());
        }
        let incremental_present = has_swapchain && has_device_ext(vk::KHR_INCREMENTAL_PRESENT_NAME);
        if incremental_present {
            device_enabled.push(vk::KHR_INCREMENTAL_PRESENT_NAME.as_ptr());
        }
        let priorities = [1.0f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        // The block-compressed families the device samples, enabled so their
        // images can be created.
        // SAFETY: `physical` belongs to `instance`.
        let supported = unsafe { instance.get_physical_device_features(physical) };
        let features = vk::PhysicalDeviceFeatures::default()
            .texture_compression_bc(supported.texture_compression_bc == vk::TRUE)
            .texture_compression_etc2(supported.texture_compression_etc2 == vk::TRUE)
            .texture_compression_astc_ldr(supported.texture_compression_astc_ldr == vk::TRUE);
        // Y′CbCr conversion, core since 1.1, where the device has it: what
        // external pictures in Y′CbCr are sampled through.
        // SAFETY: `physical` belongs to `instance`; the query fills the
        // chained local.
        let device_1_1 = api_version >= vk::API_VERSION_1_1
            && unsafe { instance.get_physical_device_properties(physical) }.api_version
                >= vk::API_VERSION_1_1;
        let ycbcr = device_1_1 && {
            let mut conversion = vk::PhysicalDeviceSamplerYcbcrConversionFeatures::default();
            let mut all = vk::PhysicalDeviceFeatures2::default().push_next(&mut conversion);
            unsafe { instance.get_physical_device_features2(physical, &mut all) };
            conversion.sampler_ycbcr_conversion == vk::TRUE
        };
        #[cfg(target_os = "android")]
        let hardware_buffers = ycbcr
            && has_device_ext(ash::android::external_memory_android_hardware_buffer::NAME)
            && has_device_ext(ext::queue_family_foreign::NAME);
        #[cfg(target_os = "android")]
        if hardware_buffers {
            device_enabled
                .push(ash::android::external_memory_android_hardware_buffer::NAME.as_ptr());
            device_enabled.push(ext::queue_family_foreign::NAME.as_ptr());
        }
        // Dma-bufs, core external memory since 1.1.
        #[cfg(unix)]
        let dma_bufs = device_1_1
            && has_device_ext(khr::external_memory_fd::NAME)
            && has_device_ext(ext::external_memory_dma_buf::NAME);
        #[cfg(unix)]
        if dma_bufs {
            device_enabled.push(khr::external_memory_fd::NAME.as_ptr());
            device_enabled.push(ext::external_memory_dma_buf::NAME.as_ptr());
        }
        let mut conversion = vk::PhysicalDeviceSamplerYcbcrConversionFeatures::default()
            .sampler_ycbcr_conversion(true);
        let mut device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&device_enabled)
            .enabled_features(&features);
        if ycbcr {
            device_info = device_info.push_next(&mut conversion);
        }
        // SAFETY: `queue_family` has a graphics queue on `physical`, and every
        // extension was enumerated as supported by it.
        let device = unsafe { instance.create_device(physical, &device_info, None) }
            .map_err(|e| format!("cannot create the device: {e}"))?;
        // SAFETY: queue 0 of `queue_family` was requested above.
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        // SAFETY: `physical` belongs to `instance`.
        let (memory_props, limits) = unsafe {
            (
                instance.get_physical_device_memory_properties(physical),
                instance.get_physical_device_properties(physical).limits,
            )
        };

        let sampled = sampled_formats(&features);

        let surface_fn = has_surface.then(|| khr::surface::Instance::new(&entry, &instance));
        let swapchain_fn = has_swapchain.then(|| khr::swapchain::Device::new(&instance, &device));

        let (set_layout, pipeline_layout, pipeline_cache) = create_layouts(&device);
        let slots = std::array::from_fn(|_| create_frame_slot(&device, queue_family));
        // SAFETY: `physical` belongs to `instance`.
        let bits = unsafe { instance.get_physical_device_queue_family_properties(physical) }
            .get(queue_family as usize)
            .map_or(0, |family| family.timestamp_valid_bits);
        let clock = (bits > 0 && limits.timestamp_period > 0.0).then(|| {
            let info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::TIMESTAMP)
                .query_count(2 * FRAMES_IN_FLIGHT as u32);
            // SAFETY: plain object creation on a live device.
            let pool = unsafe { device.create_query_pool(&info, None) }
                .expect("failed to create a Vulkan query pool");
            Clock {
                pool,
                period: f64::from(limits.timestamp_period),
                mask: if bits >= 64 {
                    u64::MAX
                } else {
                    (1 << bits) - 1
                },
                on: false,
                read: Vec::new(),
            }
        });

        #[cfg(unix)]
        let dma_bufs = dma_bufs.then(|| khr::external_memory_fd::Device::new(&instance, &device));
        #[cfg(target_os = "android")]
        let hardware_buffers = hardware_buffers.then(|| {
            ash::android::external_memory_android_hardware_buffer::Device::new(&instance, &device)
        });
        Ok(Self {
            entry,
            instance,
            surface_fn,
            debug,
            validation_errors,
            physical,
            memory_props,
            device,
            queue,
            queue_family,
            swapchain_fn,
            incremental_present,
            set_layout,
            pipeline_layout,
            pipeline_cache,
            render_passes: Vec::new(),
            dead_framebuffers: Vec::new(),
            descriptor_pools: Vec::new(),
            slots,
            slot: 0,
            recording: None,
            acquired: None,
            buffers: SlotMap::new(),
            textures: SlotMap::new(),
            samplers: SlotMap::new(),
            bind_groups: SlotMap::new(),
            pipelines: SlotMap::new(),
            surfaces: SlotMap::new(),
            caps: Caps {
                max_texture_size: limits.max_image_dimension2_d,
                presents_to_display: has_swapchain,
                compute_dispatch: false,
                bindless_texture_slots: 0,
                indirect_draw: false,
                program_instance_align: (limits.min_storage_buffer_offset_alignment.max(4) as u32)
                    .next_power_of_two(),
                coverage_paths: false,
                encodes_packed: false,
                sampled,
            },
            retire_queue: RetireQueue::new(),
            current_epoch: Epoch::START,
            reclaim_scratch: Vec::new(),
            storage_align: limits.min_storage_buffer_offset_alignment.max(1),
            uniform_align: limits.min_uniform_buffer_offset_alignment.max(16),
            clock,
            ycbcr,
            converter: None,
            planar: None,
            imports: planes::Imports::default(),
            #[cfg(unix)]
            dma_bufs,
            held: Vec::new(),
            #[cfg(target_os = "android")]
            hardware_buffers,
        })
    }

    /// How many errors the validation layer has reported (always 0 without it).
    pub fn validation_errors(&self) -> u32 {
        self.validation_errors.load(Ordering::Relaxed)
    }

    /// Whether the validation layer is active on this backend.
    pub fn validation_enabled(&self) -> bool {
        self.debug.is_some()
    }

    /// Number of resources parked in the retire queue awaiting GPU completion.
    pub fn retired_count(&self) -> usize {
        self.retire_queue.len()
    }

    /// Read back a texture's full contents (row-major, tightly packed at the
    /// format's native texel size), blocking until the GPU has finished every
    /// earlier command. A verification path, never a frame path.
    ///
    /// # Panics
    /// Panics while a surface frame is open (its commands cannot be submitted
    /// before `present`).
    pub fn read_texture(&mut self, id: TextureId) -> Vec<u8> {
        assert!(
            self.acquired.is_none(),
            "read_texture between begin_frame and present"
        );
        self.ensure_recording();
        let tex = self.texture(id);
        let (image, width, height) = (tex.image, tex.width, tex.height);
        let size = tex.format.data_size(width, height) as u64;
        let aspect = aspect_of(tex.format);
        let (buffer, memory, ptr) =
            self.create_host_buffer(size.max(1), vk::BufferUsageFlags::TRANSFER_DST);
        let cmd = self.slots[self.slot].draw;
        let region = vk::BufferImageCopy::default()
            .image_subresource(subresource_layers(aspect))
            .image_extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            });
        // SAFETY: `cmd` is in the recording state; `image` is a live texture
        // resting in SHADER_READ_ONLY_OPTIMAL with TRANSFER_SRC usage, and
        // `buffer` holds `size` bytes, the whole mip 0.
        unsafe {
            transition(
                &self.device,
                cmd,
                image,
                aspect,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            );
            self.device.cmd_copy_image_to_buffer(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &[region],
            );
            transition(
                &self.device,
                cmd,
                image,
                aspect,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );
            let to_host = vk::MemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::HOST_READ);
            self.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[to_host],
                &[],
                &[],
            );
        }
        let slot = self.slot;
        self.submit(None, None);
        let fence = self.slots[slot].fence;
        let mut out = vec![0u8; size as usize];
        // SAFETY: `fence` guards the submission that wrote `buffer`; after it
        // signals, the host-read barrier makes the `size` mapped bytes at `ptr`
        // visible. The buffer is destroyed only after the copy.
        unsafe {
            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .expect("wait for the read-back submission");
            core::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), out.len());
            self.device.destroy_buffer(buffer, None);
            self.device.free_memory(memory, None);
        }
        self.reclaim_completed();
        out
    }

    /// A texture as `desc` describes; `mutable` lets it be viewed in the
    /// other encoding of its format too (an sRGB texture as UNORM).
    /// Copy `bytes`, the tightly packed region `(x, y, w, h)`, into mip level
    /// `level` of texture `id`.
    fn write_level(
        &mut self,
        id: TextureId,
        level: u32,
        (x, y, w, h): (u32, u32, u32, u32),
        bytes: &[u8],
    ) {
        let t = self.texture(id);
        let (image, aspect) = (t.image, aspect_of(t.format));
        self.ensure_recording();
        let (buffer, offset) = self.stage(bytes);
        let cmd = self.slots[self.slot].upload;
        let region = vk::BufferImageCopy::default()
            .buffer_offset(offset)
            .image_subresource(vk::ImageSubresourceLayers {
                mip_level: level,
                ..subresource_layers(aspect)
            })
            .image_offset(vk::Offset3D {
                x: x as i32,
                y: y as i32,
                z: 0,
            })
            .image_extent(vk::Extent3D {
                width: w,
                height: h,
                depth: 1,
            });
        // SAFETY: `cmd` is recording; the texture rests in
        // SHADER_READ_ONLY_OPTIMAL and returns to it; the staged bytes cover the
        // tightly packed `w`×`h` region.
        unsafe {
            transition(
                &self.device,
                cmd,
                image,
                aspect,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            self.device.cmd_copy_buffer_to_image(
                cmd,
                buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
            transition(
                &self.device,
                cmd,
                image,
                aspect,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );
        }
    }

    fn new_texture(&mut self, desc: &TextureDesc, mutable: bool) -> TextureId {
        self.new_texture_levels(desc, mutable, 1)
    }

    /// A texture of `levels` mip levels, every one cleared.
    fn new_texture_levels(&mut self, desc: &TextureDesc, mutable: bool, levels: u32) -> TextureId {
        let format = vk_format(desc.format);

        let aspect = aspect_of(desc.format);
        let depth = aspect == vk::ImageAspectFlags::DEPTH;
        let mut usage = vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::TRANSFER_DST
            | vk::ImageUsageFlags::TRANSFER_SRC;
        if desc.render_target {
            usage |= if depth {
                vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
            } else {
                vk::ImageUsageFlags::COLOR_ATTACHMENT
            };
        }
        let info = vk::ImageCreateInfo::default()
            .flags(if mutable {
                vk::ImageCreateFlags::MUTABLE_FORMAT
            } else {
                vk::ImageCreateFlags::empty()
            })
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: desc.width.max(1),
                height: desc.height.max(1),
                depth: 1,
            })
            .mip_levels(levels.max(1))
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: `info` describes a 2D image with a positive extent and no more
        // levels than its chain; its memory comes from a type its requirements
        // allow and is bound once.
        let (image, memory) = unsafe {
            let image = self
                .device
                .create_image(&info, None)
                .expect("failed to create a Vulkan image");
            let req = self.device.get_image_memory_requirements(image);
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(self.memory_type(
                    req.memory_type_bits,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    vk::MemoryPropertyFlags::empty(),
                ));
            let memory = self
                .device
                .allocate_memory(&alloc, None)
                .expect("failed to allocate Vulkan image memory");
            self.device
                .bind_image_memory(image, memory, 0)
                .expect("failed to bind Vulkan image memory");
            (image, memory)
        };
        let view = self.create_view(image, format, aspect);
        // A depth render target only ever serves as a pass's attachment.
        let rest = if depth && desc.render_target {
            vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL
        } else {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        };

        // Zero the image and bring it to its resting layout, so a texture that is
        // sampled before it is written reads transparent black, not garbage.
        self.ensure_recording();
        let cmd = self.slots[self.slot].upload;
        // SAFETY: `cmd` is recording; the image is fresh (UNDEFINED) and has
        // TRANSFER_DST usage.
        unsafe {
            transition(
                &self.device,
                cmd,
                image,
                aspect,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            let range = [subresource_range(aspect)];
            if depth {
                self.device.cmd_clear_depth_stencil_image(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearDepthStencilValue::default(),
                    &range,
                );
            } else {
                self.device.cmd_clear_color_image(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue::default(),
                    &range,
                );
            }
            transition(
                &self.device,
                cmd,
                image,
                aspect,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                rest,
            );
        }
        self.textures
            .insert(VulkanTexture {
                image,
                memory,
                view,
                format: desc.format,
                width: desc.width.max(1),
                height: desc.height.max(1),
                render_target: desc.render_target,
                framebuffer: vk::Framebuffer::null(),
                depth_framebuffer: None,
                converted: None,
            })
            .into()
    }

    /// A texture of `width` × `height` external pictures are converted into:
    /// sRGB, drawn into through its UNORM view.
    fn converted_texture(&mut self, width: u32, height: u32) -> TextureId {
        let id = self.new_texture(
            &TextureDesc {
                width,
                height,
                format: TextureFormat::Rgba8UnormSrgb,
                render_target: true,
                label: "external picture",
            },
            true,
        );
        let image = self.texture(id).image;
        let view = self.create_view(
            image,
            vk::Format::R8G8B8A8_UNORM,
            vk::ImageAspectFlags::COLOR,
        );
        let pass = self.render_pass(vk::Format::R8G8B8A8_UNORM, true, None);
        let framebuffer = self.create_framebuffer(pass, &[view], width.max(1), height.max(1));
        if let Some(t) = self.textures.get_mut(id.into()) {
            t.converted = Some(Converted {
                view,
                framebuffer,
                planes: None,
            });
        }
        id
    }

    /// The source `image` names — made of it if it is new — the region of
    /// it the picture is, as an offset and a scale in its texture
    /// coordinates, and the quarter turns clockwise that make it upright.
    /// `None` where the device cannot convert it.
    fn external_source(&mut self, image: &ExternalImage) -> Option<(usize, [f32; 4], u8)> {
        if !self.ycbcr {
            return None;
        }
        match *image {
            #[cfg(target_os = "android")]
            ExternalImage::AndroidHardwareBuffer {
                buffer,
                holds,
                x,
                y,
                width,
                height,
                turns,
            } => {
                let converter = self
                    .converter
                    .get_or_insert_with(|| ycbcr::Converter::new(&self.device));
                let at = match converter.source_of(holds) {
                    Some(at) => at,
                    None => {
                        // SAFETY: the producer keeps the buffer and its
                        // holds alive while it hands the image over.
                        let (made, encoding) = unsafe { self.import_hardware_buffer(buffer)? };
                        // SAFETY: as above.
                        let holds = unsafe { viso_handle::ExternalHolds::retain(holds) };
                        let pass = self.render_pass(vk::Format::R8G8B8A8_UNORM, true, None);
                        let epoch = self.recording.unwrap_or(self.current_epoch);
                        let converter = self.converter.as_mut()?;
                        // SAFETY: the image was just made and bound, sampled,
                        // in the encoding's format; the pass has one UNORM
                        // color attachment.
                        unsafe {
                            converter.make_source(
                                &self.device,
                                self.pipeline_cache,
                                pass,
                                made,
                                encoding,
                                Some(holds),
                                epoch,
                            )?
                        }
                    }
                };
                let (bw, bh) = self.converter.as_ref()?.size(at);
                let (bw, bh) = (bw.max(1) as f32, bh.max(1) as f32);
                Some((
                    at,
                    [
                        x as f32 / bw,
                        y as f32 / bh,
                        width as f32 / bw,
                        height as f32 / bh,
                    ],
                    turns,
                ))
            }
            _ => None,
        }
    }

    /// Records the conversion of source `at`'s `region` into texture `id`,
    /// turned `turns` quarter turns clockwise, holding the source's buffer
    /// until the frame it is recorded in completes.
    fn convert(&mut self, id: TextureId, at: usize, region: [f32; 4], turns: u8) {
        self.ensure_recording();
        let pass = self.render_pass(vk::Format::R8G8B8A8_UNORM, true, None);
        let t = self.texture(id);
        let Some(Converted { framebuffer, .. }) = t.converted else {
            return;
        };
        let target = ycbcr::Target {
            image: t.image,
            framebuffer,
            width: t.width,
            height: t.height,
        };
        let cmd = self.slots[self.slot].upload;
        let Some(converter) = &self.converter else {
            return;
        };
        // SAFETY: `cmd` is recording outside a render pass on the backend's
        // queue family; the target is a live converted texture whose
        // framebuffer was made against the UNORM pass.
        unsafe {
            converter.record(
                &self.device,
                cmd,
                self.queue_family,
                pass,
                at,
                region,
                turns,
                &target,
            );
        }
        if let Some(holds) = converter.holds(at) {
            let epoch = self.recording.unwrap_or(self.current_epoch);
            self.held.push((epoch, holds.hold()));
        }
    }

    /// Texture `id` where it is one planes are converted into, of `width` ×
    /// `height`, else a new one; its plane images and the planar pipeline
    /// made if they are not yet.
    fn plane_target(&mut self, id: Option<TextureId>, width: u32, height: u32) -> TextureId {
        let id = match id {
            Some(id)
                if self.textures.get(id.into()).is_some_and(|t| {
                    t.converted.is_some() && (t.width, t.height) == (width, height)
                }) =>
            {
                id
            }
            _ => self.converted_texture(width, height),
        };
        if self.planar.is_none() {
            let pass = self.render_pass(vk::Format::R8G8B8A8_UNORM, true, None);
            // SAFETY: the pass has one UNORM color attachment.
            self.planar =
                Some(unsafe { planes::Planar::new(&self.device, self.pipeline_cache, pass) });
        }
        let made = self
            .textures
            .get(id.into())
            .and_then(|t| t.converted.as_ref())
            .is_some_and(|c| c.planes.is_some());
        if !made && let Some(planar) = &self.planar {
            let memory_type = |bits| {
                self.memory_type(
                    bits,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    vk::MemoryPropertyFlags::empty(),
                )
            };
            let made = planar.planes(&self.device, memory_type, width, height);
            if let Some(c) = self
                .textures
                .get_mut(id.into())
                .and_then(|t| t.converted.as_mut())
            {
                c.planes = Some(made);
            }
        }
        id
    }

    /// Records the upload of the planes at `staged` into texture `id`'s
    /// plane images and their conversion into it, as `picture` is encoded;
    /// `foreign` is a buffer owned outside Vulkan they are read from,
    /// acquired for the upload and released after.
    fn convert_planes(
        &mut self,
        id: TextureId,
        picture: &Nv12<'_>,
        staged: [planes::Staged; 2],
        foreign: Option<vk::Buffer>,
    ) {
        self.ensure_recording();
        let pass = self.render_pass(vk::Format::R8G8B8A8_UNORM, true, None);
        let cmd = self.slots[self.slot].upload;
        let family = self.queue_family;
        let Some(t) = self.textures.get_mut(id.into()) else {
            return;
        };
        let Some(c) = t.converted.as_mut() else {
            return;
        };
        let target = ycbcr::Target {
            image: t.image,
            framebuffer: c.framebuffer,
            width: t.width,
            height: t.height,
        };
        let (Some(planes), Some(planar)) = (c.planes.as_mut(), &self.planar) else {
            return;
        };
        let barrier = |acquire: bool, buffer: vk::Buffer| {
            let b = vk::BufferMemoryBarrier::default()
                .buffer(buffer)
                .size(vk::WHOLE_SIZE);
            if acquire {
                b.src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                    .dst_queue_family_index(family)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            } else {
                b.src_queue_family_index(family)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                    .src_access_mask(vk::AccessFlags::TRANSFER_READ)
            }
        };
        // SAFETY: `cmd` is the open upload recording, outside a render pass,
        // which orders every use of the planes and the target on the one
        // queue; the planes were made for the target's size, the staged rows
        // cover them, and the target's framebuffer was made against the
        // pass; a foreign buffer is owned by this queue for the copies alone.
        unsafe {
            if let Some(buffer) = foreign {
                self.device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[barrier(true, buffer)],
                    &[],
                );
            }
            planar.record(&self.device, cmd, pass, picture, planes, staged, &target);
            if let Some(buffer) = foreign {
                self.device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[barrier(false, buffer)],
                    &[],
                );
            }
        }
    }

    /// Writes the NV12 picture in the dma-buf `image` names into texture
    /// `id`, as [`write_nv12`](GpuBackend::write_nv12) does, the buffer
    /// imported once and read where it lies; `None` where the device does
    /// not import dma-bufs or the planes do not fit.
    #[cfg(unix)]
    fn write_dma_buf(&mut self, id: Option<TextureId>, image: &ExternalImage) -> Option<TextureId> {
        let ExternalImage::DmaBuf {
            fd,
            len,
            offsets,
            strides,
            holds,
            matrix,
            full_range,
            left_chroma,
            width,
            height,
        } = *image
        else {
            return None;
        };
        let fds = self.dma_bufs.as_ref()?;
        let (cw, ch) = (u64::from(width.div_ceil(2)), u64::from(height.div_ceil(2)));
        let fits = |offset: u64, stride: u32, row: u64, rows: u64| {
            stride > 0
                && u64::from(stride) >= row
                && offset
                    .checked_add(u64::from(stride) * (rows.max(1) - 1) + row)
                    .is_some_and(|end| end <= len)
        };
        if width == 0
            || height == 0
            || !strides[1].is_multiple_of(2)
            || !offsets[0].is_multiple_of(4)
            || !offsets[1].is_multiple_of(4)
            || !fits(offsets[0], strides[0], u64::from(width), u64::from(height))
            || !fits(offsets[1], strides[1], cw * 2, ch)
        {
            return None;
        }
        let epoch = self.recording.unwrap_or(self.current_epoch);
        let at = match self.imports.source_of(holds) {
            Some(at) => at,
            None => {
                // SAFETY: the producer keeps the dma-buf and its holds alive
                // while it hands the image over; the duplicate is Vulkan's
                // once imported.
                let Some((buffer, memory)) = (unsafe { self.import_dma_buf(fds, fd, len) }) else {
                    self.dma_bufs = None;
                    return None;
                };
                // SAFETY: as above.
                let holds = unsafe { viso_handle::ExternalHolds::retain(holds) };
                self.imports.keep(holds, buffer, memory, epoch)
            }
        };
        let (buffer, holds) = self.imports.get(at);
        let hold = holds.hold();
        let id = self.plane_target(id, width, height);
        let picture = Nv12 {
            width,
            height,
            y: &[],
            y_stride: strides[0] as usize,
            uv: &[],
            uv_stride: strides[1] as usize,
            matrix,
            full_range,
            left_chroma,
        };
        let staged = |offset: u64, row_length: u32| planes::Staged {
            buffer,
            offset,
            row_length,
        };
        self.convert_planes(
            id,
            &picture,
            [
                staged(offsets[0], strides[0]),
                staged(offsets[1], strides[1] / 2),
            ],
            Some(buffer),
        );
        let epoch = self.recording.unwrap_or(self.current_epoch);
        self.held.push((epoch, hold));
        Some(id)
    }

    /// A transfer-source buffer over the dma-buf `fd` of `len` bytes, bound
    /// to memory imported from a duplicate of it; `None` where the device
    /// refuses it, the duplicate then left open.
    ///
    /// # Safety
    ///
    /// `fd` is a live dma-buf of at least `len` bytes.
    #[cfg(unix)]
    unsafe fn import_dma_buf(
        &self,
        fds: &khr::external_memory_fd::Device,
        fd: i32,
        len: u64,
    ) -> Option<(vk::Buffer, vk::DeviceMemory)> {
        use std::os::fd::{AsRawFd, BorrowedFd, IntoRawFd};
        let kind = vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;
        // SAFETY: the caller's contract: the descriptor is live for the call.
        let owned = unsafe { BorrowedFd::borrow_raw(fd) }
            .try_clone_to_owned()
            .ok()?;
        let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(kind);
        let info = vk::BufferCreateInfo::default()
            .size(len)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external);
        // SAFETY: plain object creation and import on a live device; the
        // duplicate is handed to Vulkan only by a successful allocation, and
        // closed by `owned` otherwise; the buffer is bound once.
        unsafe {
            let mut props = vk::MemoryFdPropertiesKHR::default();
            fds.get_memory_fd_properties(kind, owned.as_raw_fd(), &mut props)
                .ok()?;
            let buffer = self.device.create_buffer(&info, None).ok()?;
            let req = self.device.get_buffer_memory_requirements(buffer);
            let bits = req.memory_type_bits & props.memory_type_bits;
            if bits == 0 || req.size > len {
                self.device.destroy_buffer(buffer, None);
                return None;
            }
            let raw = owned.into_raw_fd();
            let mut import = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(kind)
                .fd(raw);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(len)
                .memory_type_index(bits.trailing_zeros())
                .push_next(&mut import)
                .push_next(&mut dedicated);
            let Ok(memory) = self.device.allocate_memory(&alloc, None) else {
                // Some drivers close the descriptor of a failed import too:
                // it is left, and the device asked for no more.
                self.device.destroy_buffer(buffer, None);
                return None;
            };
            if self.device.bind_buffer_memory(buffer, memory, 0).is_err() {
                self.device.free_memory(memory, None);
                self.device.destroy_buffer(buffer, None);
                return None;
            }
            Some((buffer, memory))
        }
    }

    /// A source of the `width` × `height` picture in NV12 planes `y` and
    /// `uv`, studio range in BT.601; `None` where the device does not
    /// convert NV12.
    #[cfg(test)]
    fn nv12_source(&mut self, width: u32, height: u32, y: &[u8], uv: &[u8]) -> Option<usize> {
        let format = vk::Format::G8_B8R8_2PLANE_420_UNORM;
        // SAFETY: `physical` belongs to `instance`.
        let features = unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, format)
        }
        .optimal_tiling_features;
        let needed = vk::FormatFeatureFlags::SAMPLED_IMAGE
            | vk::FormatFeatureFlags::TRANSFER_DST
            | vk::FormatFeatureFlags::MIDPOINT_CHROMA_SAMPLES;
        if !self.ycbcr || !features.contains(needed) {
            return None;
        }
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: the image's memory comes from a type its requirements
        // allow and is bound once; the planes are copied in from staging
        // while it is in the transfer layout, then it rests sampled.
        unsafe {
            let image = self.device.create_image(&info, None).ok()?;
            let req = self.device.get_image_memory_requirements(image);
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(self.memory_type(
                    req.memory_type_bits,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    vk::MemoryPropertyFlags::empty(),
                ));
            let memory = self.device.allocate_memory(&alloc, None).ok()?;
            self.device.bind_image_memory(image, memory, 0).ok()?;
            self.ensure_recording();
            let (y_buf, y_at) = self.stage_aligned(y, 4);
            let (uv_buf, uv_at) = self.stage_aligned(uv, 4);
            let cmd = self.slots[self.slot].upload;
            transition(
                &self.device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            for (buffer, offset, aspect, w, h) in [
                (y_buf, y_at, vk::ImageAspectFlags::PLANE_0, width, height),
                (
                    uv_buf,
                    uv_at,
                    vk::ImageAspectFlags::PLANE_1,
                    width / 2,
                    height / 2,
                ),
            ] {
                let region = vk::BufferImageCopy::default()
                    .buffer_offset(offset)
                    .image_subresource(subresource_layers(aspect))
                    .image_extent(vk::Extent3D {
                        width: w,
                        height: h,
                        depth: 1,
                    });
                self.device.cmd_copy_buffer_to_image(
                    cmd,
                    buffer,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
            }
            transition(
                &self.device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );
            let encoding = ycbcr::Encoding {
                format,
                external: 0,
                model: vk::SamplerYcbcrModelConversion::YCBCR_601,
                range: vk::SamplerYcbcrRange::ITU_NARROW,
                components: [vk::ComponentSwizzle::IDENTITY; 4],
                x_chroma: vk::ChromaLocation::MIDPOINT,
                y_chroma: vk::ChromaLocation::MIDPOINT,
                // Nearest: each texel converts from its own samples alone.
                linear: false,
            };
            let pass = self.render_pass(vk::Format::R8G8B8A8_UNORM, true, None);
            let epoch = self.recording.unwrap_or(self.current_epoch);
            let converter = self
                .converter
                .get_or_insert_with(|| ycbcr::Converter::new(&self.device));
            converter.make_source(
                &self.device,
                self.pipeline_cache,
                pass,
                ycbcr::Image {
                    image,
                    memory,
                    width,
                    height,
                    foreign: false,
                },
                encoding,
                None,
                epoch,
            )
        }
    }

    /// An image over the Android hardware buffer `buffer`, its memory
    /// imported, and the encoding the device samples it in.
    ///
    /// # Safety
    ///
    /// `buffer` is a live `AHardwareBuffer`.
    #[cfg(target_os = "android")]
    unsafe fn import_hardware_buffer(
        &self,
        buffer: *mut c_void,
    ) -> Option<(ycbcr::Image, ycbcr::Encoding)> {
        let loader = self.hardware_buffers.as_ref()?;
        let mut format = vk::AndroidHardwareBufferFormatPropertiesANDROID::default();
        let mut props =
            vk::AndroidHardwareBufferPropertiesANDROID::default().push_next(&mut format);
        // SAFETY: the caller's contract; the query fills the chained locals.
        unsafe {
            loader
                .get_android_hardware_buffer_properties(buffer.cast_const(), &mut props)
                .ok()?;
        }
        let (size, bits) = (props.allocation_size, props.memory_type_bits);
        let mut desc = AHardwareBufferDesc::default();
        // SAFETY: as above; the call fills the local.
        unsafe { AHardwareBuffer_describe(buffer.cast_const(), &mut desc) };
        let external = if format.format == vk::Format::UNDEFINED {
            format.external_format
        } else {
            0
        };
        let encoding = ycbcr::Encoding {
            format: format.format,
            external,
            model: format.suggested_ycbcr_model,
            range: format.suggested_ycbcr_range,
            components: {
                let c = format.sampler_ycbcr_conversion_components;
                [c.r, c.g, c.b, c.a]
            },
            x_chroma: format.suggested_x_chroma_offset,
            y_chroma: format.suggested_y_chroma_offset,
            linear: format
                .format_features
                .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE_YCBCR_CONVERSION_LINEAR_FILTER),
        };
        let mut handle = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::ANDROID_HARDWARE_BUFFER_ANDROID);
        let mut external_format = vk::ExternalFormatANDROID::default().external_format(external);
        let mut info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format.format)
            .extent(vk::Extent3D {
                width: desc.width,
                height: desc.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut handle);
        if external != 0 {
            info = info.push_next(&mut external_format);
        }
        // SAFETY: the image is made to import the buffer's memory, which is
        // allocated dedicated to it from a type the buffer allows and bound
        // once; the import takes a reference to the buffer.
        unsafe {
            let image = self.device.create_image(&info, None).ok()?;
            let mut import = vk::ImportAndroidHardwareBufferInfoANDROID::default().buffer(buffer);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(size)
                .memory_type_index(bits.trailing_zeros())
                .push_next(&mut import)
                .push_next(&mut dedicated);
            let Ok(memory) = self.device.allocate_memory(&alloc, None) else {
                self.device.destroy_image(image, None);
                return None;
            };
            if self.device.bind_image_memory(image, memory, 0).is_err() {
                self.device.destroy_image(image, None);
                self.device.free_memory(memory, None);
                return None;
            }
            Some((
                ycbcr::Image {
                    image,
                    memory,
                    width: desc.width,
                    height: desc.height,
                    foreign: true,
                },
                encoding,
            ))
        }
    }

    fn buffer(&self, id: BufferId) -> &VulkanBuffer {
        self.buffers
            .get(id.into())
            .expect("buffer handle does not resolve")
    }

    fn texture(&self, id: TextureId) -> &VulkanTexture {
        self.textures
            .get(id.into())
            .expect("texture handle does not resolve")
    }

    fn surface(&self, id: SurfaceId) -> &VulkanSurface {
        self.surfaces
            .get(id.into())
            .expect("surface handle does not resolve")
    }

    fn surface_mut(&mut self, id: SurfaceId) -> &mut VulkanSurface {
        self.surfaces
            .get_mut(id.into())
            .expect("surface handle does not resolve")
    }

    fn swapchain_fn(&self) -> &khr::swapchain::Device {
        self.swapchain_fn
            .as_ref()
            .expect("this Vulkan device has no swapchain support")
    }

    /// A memory type index allowed by `bits` with `required` properties,
    /// preferring one that also has `preferred`.
    fn memory_type(
        &self,
        bits: u32,
        required: vk::MemoryPropertyFlags,
        preferred: vk::MemoryPropertyFlags,
    ) -> u32 {
        let props = &self.memory_props;
        let find = |flags: vk::MemoryPropertyFlags| {
            (0..props.memory_type_count).find(|&i| {
                bits & (1 << i) != 0
                    && props.memory_types[i as usize]
                        .property_flags
                        .contains(flags)
            })
        };
        find(required | preferred)
            .or_else(|| find(required))
            .expect("no Vulkan memory type satisfies the request")
    }

    /// A host-visible, host-coherent buffer of `size` bytes, persistently mapped.
    fn create_host_buffer(
        &self,
        size: u64,
        usage: vk::BufferUsageFlags,
    ) -> (vk::Buffer, vk::DeviceMemory, *mut u8) {
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: `info` describes a non-zero-sized exclusive buffer; the memory
        // is allocated from a type the buffer's requirements allow, bound once at
        // offset 0, and mapped whole — it is host-visible by construction.
        unsafe {
            let buffer = self
                .device
                .create_buffer(&info, None)
                .expect("failed to create a Vulkan buffer");
            let req = self.device.get_buffer_memory_requirements(buffer);
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(self.memory_type(
                    req.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                ));
            let memory = self
                .device
                .allocate_memory(&alloc, None)
                .expect("failed to allocate Vulkan buffer memory");
            self.device
                .bind_buffer_memory(buffer, memory, 0)
                .expect("failed to bind Vulkan buffer memory");
            let ptr = self
                .device
                .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
                .expect("failed to map Vulkan buffer memory") as *mut u8;
            (buffer, memory, ptr)
        }
    }

    /// The render pass for `format` with a clearing or loading color
    /// attachment, and a transient `depth` attachment if one is named. Passes
    /// differing only in load op are compatible, so pipelines and framebuffers
    /// are built against the clearing one.
    fn render_pass(
        &mut self,
        format: vk::Format,
        clear: bool,
        depth: Option<vk::Format>,
    ) -> vk::RenderPass {
        if let Some(e) = self
            .render_passes
            .iter()
            .find(|e| e.format == format && e.clear == clear && e.depth == depth)
        {
            return e.pass;
        }
        // SAFETY: the device is live.
        let pass = unsafe { create_render_pass(&self.device, format, clear, depth) };
        self.render_passes.push(RenderPassEntry {
            format,
            clear,
            depth,
            pass,
        });
        pass
    }

    /// Open the current slot's command buffers if they are not recording:
    /// wait for the slot's previous submission, rewind its pool and staging.
    fn ensure_recording(&mut self) {
        if self.recording.is_some() {
            return;
        }
        let slot = &mut self.slots[self.slot];
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the fence guards the slot's last submission; once it signals,
        // nothing on the GPU uses the pool's command buffers or the staging
        // chunks, so both may be reset and re-recorded.
        unsafe {
            if slot.submitted {
                self.device
                    .wait_for_fences(&[slot.fence], true, u64::MAX)
                    .expect("wait for a frame slot");
                self.device
                    .reset_fences(&[slot.fence])
                    .expect("reset a frame fence");
                slot.submitted = false;
            }
            if let Some(clock) = &mut self.clock {
                read_clock(&self.device, clock, slot, self.slot);
            }
            self.device
                .reset_command_pool(slot.pool, vk::CommandPoolResetFlags::empty())
                .expect("reset a frame command pool");
            for &pool in &slot.program_pools {
                self.device
                    .reset_descriptor_pool(pool, vk::DescriptorPoolResetFlags::empty())
                    .expect("reset a program descriptor pool");
            }
            slot.program_pool = 0;
            self.device
                .begin_command_buffer(slot.upload, &begin)
                .expect("begin the upload command buffer");
            self.device
                .begin_command_buffer(slot.draw, &begin)
                .expect("begin the draw command buffer");
            slot.clocking = false;
            if let Some(clock) = self.clock.as_ref().filter(|clock| clock.on) {
                let first = 2 * self.slot as u32;
                self.device
                    .cmd_reset_query_pool(slot.upload, clock.pool, first, 2);
                self.device.cmd_write_timestamp(
                    slot.upload,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    clock.pool,
                    first,
                );
                slot.clocking = true;
            }
        }
        slot.staging.current = 0;
        slot.staging.cursor = 0;
        self.recording = Some(self.current_epoch);
    }

    /// Submit the open recording: uploads first, then the draw buffer — which
    /// waits on `wait` (a swapchain acquire) and signals `signal` — under the
    /// slot's fence. Advances to the next slot.
    fn submit(&mut self, wait: Option<vk::Semaphore>, signal: Option<vk::Semaphore>) {
        let Some(epoch) = self.recording.take() else {
            return;
        };
        let slot = &mut self.slots[self.slot];
        let upload = [slot.upload];
        let draw = [slot.draw];
        let wait_sems: Vec<vk::Semaphore> = wait.into_iter().collect();
        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let signal_sems: Vec<vk::Semaphore> = signal.into_iter().collect();
        let infos = [
            vk::SubmitInfo::default().command_buffers(&upload),
            vk::SubmitInfo::default()
                .wait_semaphores(&wait_sems)
                .wait_dst_stage_mask(&wait_stages[..wait_sems.len()])
                .command_buffers(&draw)
                .signal_semaphores(&signal_sems),
        ];
        // SAFETY: both command buffers were begun by `ensure_recording` and are
        // ended here exactly once; the fence was reset when the slot opened.
        // The end stamp follows the start the upload buffer wrote, which the
        // same submission runs first.
        unsafe {
            slot.timed = None;
            if let Some(clock) = &self.clock
                && slot.clocking
                && signal.is_some()
            {
                self.device.cmd_write_timestamp(
                    slot.draw,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    clock.pool,
                    2 * self.slot as u32 + 1,
                );
                slot.timed = Some(self.current_epoch.0);
            }
            self.device
                .end_command_buffer(slot.upload)
                .expect("end the upload command buffer");
            self.device
                .end_command_buffer(slot.draw)
                .expect("end the draw command buffer");
            self.device
                .queue_submit(self.queue, &infos, slot.fence)
                .expect("Vulkan queue submit failed");
        }
        slot.submitted = true;
        slot.epoch = epoch;
        self.slot = (self.slot + 1) % FRAMES_IN_FLIGHT;
    }

    /// The newest epoch no unfinished GPU work — nor the open recording — can
    /// still reference.
    fn completed_epoch(&self) -> Epoch {
        let mut done = self.current_epoch.0;
        if let Some(e) = self.recording {
            done = done.min(e.0.saturating_sub(1));
        }
        for slot in &self.slots {
            // SAFETY: `slot.fence` is a live fence of this device.
            let finished = !slot.submitted
                || unsafe { self.device.get_fence_status(slot.fence) }.unwrap_or(false);
            if !finished {
                done = done.min(slot.epoch.0.saturating_sub(1));
            }
        }
        Epoch(done)
    }

    /// Destroy every retired resource the GPU can no longer reference.
    fn reclaim_completed(&mut self) {
        let completed = self.completed_epoch();
        let fence = Fence::at(completed);
        self.held.retain(|(epoch, _)| *epoch > completed);
        self.imports.reclaim(&self.device, completed);
        if let Some(converter) = &mut self.converter {
            converter.reclaim(&self.device, completed);
        }
        let device = &self.device;
        self.dead_framebuffers.retain(|&(epoch, fb)| {
            let done = epoch <= completed;
            if done {
                // SAFETY: every frame up to the one it was replaced in has
                // finished, so no command references the framebuffer.
                unsafe { device.destroy_framebuffer(fb, None) };
            }
            !done
        });
        self.reclaim_scratch.clear();
        self.retire_queue
            .drain_completed(fence, &mut self.reclaim_scratch);
        for i in 0..self.reclaim_scratch.len() {
            let entry = self.reclaim_scratch[i];
            // SAFETY: the completion fence has passed the epoch the resource
            // was retired in, so no pending or recorded command references it.
            unsafe {
                match entry.kind {
                    ResourceKind::Buffer => {
                        if let Some(b) = self.buffers.remove(entry.id) {
                            destroy_buffer(&self.device, b);
                        }
                    }
                    ResourceKind::Texture => {
                        if let Some(t) = self.textures.remove(entry.id) {
                            destroy_texture(&self.device, t);
                        }
                    }
                    ResourceKind::Sampler => {
                        if let Some(s) = self.samplers.remove(entry.id) {
                            self.device.destroy_sampler(s, None);
                        }
                    }
                    ResourceKind::Pipeline => {
                        if let Some(p) = self.pipelines.remove(entry.id) {
                            destroy_pipeline(&self.device, p);
                        }
                    }
                    ResourceKind::BindGroup => {
                        if let Some(g) = self.bind_groups.remove(entry.id) {
                            let _ = self.device.free_descriptor_sets(g.pool, &[g.set]);
                        }
                    }
                }
            }
        }
    }

    /// Copy `bytes` into the current slot's staging arena, returning the chunk
    /// buffer and byte offset. The recording must be open.
    fn stage(&mut self, bytes: &[u8]) -> (vk::Buffer, u64) {
        // Texel-copy offsets must be a multiple of 4 and of the texel size; 16
        // covers every format.
        self.stage_aligned(bytes, 16)
    }

    /// [`stage`](Self::stage) at an offset that is a multiple of `align`.
    fn stage_aligned(&mut self, bytes: &[u8], align: u64) -> (vk::Buffer, u64) {
        let size = bytes.len() as u64;
        loop {
            let staging = &mut self.slots[self.slot].staging;
            if let Some(chunk) = staging.chunks.get(staging.current) {
                let offset = staging.cursor.next_multiple_of(align);
                if offset + size <= chunk.size {
                    // SAFETY: `offset + size` lies inside the chunk's mapping,
                    // and the slot's previous reader finished before it opened.
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            bytes.as_ptr(),
                            chunk.ptr.add(offset as usize),
                            bytes.len(),
                        );
                    }
                    staging.cursor = offset + size;
                    return (chunk.buffer, offset);
                }
                staging.current += 1;
                staging.cursor = 0;
                continue;
            }
            let chunk_size = size.max(STAGING_CHUNK).next_power_of_two();
            let (buffer, memory, ptr) = self.create_host_buffer(
                chunk_size,
                vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::UNIFORM_BUFFER,
            );
            let staging = &mut self.slots[self.slot].staging;
            staging.chunks.push(StagingChunk {
                buffer,
                memory,
                ptr,
                size: chunk_size,
            });
            staging.current = staging.chunks.len() - 1;
            staging.cursor = 0;
        }
    }

    /// Allocate one descriptor set, opening a new pool when the last is full.
    fn allocate_set(&mut self) -> (vk::DescriptorSet, vk::DescriptorPool) {
        if let Some(&pool) = self.descriptor_pools.last() {
            let layouts = [self.set_layout];
            let info = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pool)
                .set_layouts(&layouts);
            // SAFETY: `pool` and `set_layout` are live objects of this device.
            if let Ok(sets) = unsafe { self.device.allocate_descriptor_sets(&info) } {
                return (sets[0], pool);
            }
        }
        // Every set of the shared layout holds each of its slots.
        let sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: SETS_PER_POOL * scene::TEXTURE_SLOTS as u32,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLER,
                descriptor_count: SETS_PER_POOL * scene::SAMPLER_SLOTS as u32,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: SETS_PER_POOL * scene::UNIFORM_SLOTS as u32,
            },
        ];
        let info = vk::DescriptorPoolCreateInfo::default()
            .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
            .max_sets(SETS_PER_POOL)
            .pool_sizes(&sizes);
        // SAFETY: `info` describes a pool sized for `SETS_PER_POOL` sets of the
        // shared layout.
        let pool = unsafe { self.device.create_descriptor_pool(&info, None) }
            .expect("failed to create a Vulkan descriptor pool");
        self.descriptor_pools.push(pool);
        self.allocate_set()
    }

    /// A triangle-list graphics pipeline over `module`'s two entries, drawing
    /// as `desc` says through `layout`.
    ///
    /// # Safety
    ///
    /// `module` and `layout` are live objects of this device, and the entries
    /// read only what `layout` and `vertex_input` declare.
    unsafe fn graphics_pipeline(
        &mut self,
        module: vk::ShaderModule,
        desc: &PipelineDesc,
        vertex_input: &vk::PipelineVertexInputStateCreateInfo<'_>,
        layout: vk::PipelineLayout,
    ) -> Result<vk::Pipeline, vk::Result> {
        let render_pass = self.render_pass(
            vk_format(desc.color_format),
            true,
            desc.depth.map(|d| vk_format(d.format)),
        );
        // SAFETY: forwarded from the caller; the render pass is live.
        unsafe {
            graphics_pipeline(
                &self.device,
                self.pipeline_cache,
                render_pass,
                module,
                Stages {
                    vertex_entry: desc.vertex_entry,
                    fragment_entry: desc.fragment_entry,
                    vertex_input,
                    blend: desc.blend,
                    depth: desc.depth,
                    cull: desc.cull,
                },
                layout,
            )
        }
    }

    /// The descriptor sets of a program draw, written from its bind group and
    /// instance buffer; `None` for a built-in pipeline.
    fn program_sets(
        &mut self,
        c: &DrawCommand,
    ) -> Option<(vk::PipelineLayout, [vk::DescriptorSet; 2])> {
        let p = self
            .pipelines
            .get(c.pipeline.into())
            .expect("pipeline handle does not resolve");
        let program = p.program.as_ref()?;
        let (layout, layouts, textures, samplers, uniform_size) = (
            program.layout,
            program.sets,
            program.textures,
            program.samplers,
            program.uniform_size,
        );
        assert!(
            (c.instance_offset as u64).is_multiple_of(self.storage_align),
            "a program's instance offset {} is not a multiple of the storage alignment {}",
            c.instance_offset,
            self.storage_align
        );
        let bindings = c.bind_group.map_or_else(Vec::new, |bg| {
            self.bind_groups
                .get(bg.into())
                .expect("bind group handle does not resolve")
                .bindings
                .clone()
        });
        let sets = self.allocate_program_sets(&layouts);
        let uniform = bindings.iter().find_map(|b| match b {
            Binding::Uniform(id) => Some(*id),
            _ => None,
        });
        let views: Vec<vk::ImageView> = bindings
            .iter()
            .filter_map(|b| match b {
                Binding::Texture(id) => Some(self.texture(*id).view),
                _ => None,
            })
            .collect();
        let sampler_states: Vec<vk::Sampler> = bindings
            .iter()
            .filter_map(|b| match b {
                Binding::Sampler(id) => Some(
                    *self
                        .samplers
                        .get((*id).into())
                        .expect("sampler handle does not resolve"),
                ),
                _ => None,
            })
            .collect();
        assert!(
            views.len() >= textures as usize && sampler_states.len() >= samplers as usize,
            "the bind group binds {} textures and {} samplers; the program declares {textures} and {samplers}",
            views.len(),
            sampler_states.len()
        );
        let uniform_info = match uniform {
            Some(id) => Some([vk::DescriptorBufferInfo::default()
                .buffer(self.buffer(id).buffer)
                .range(vk::WHOLE_SIZE)]),
            // An inline block, padded to the size the program declares.
            None if !c.uniforms.is_empty() => {
                let mut block = [0u8; InlineUniforms::MAX];
                let inline = c.uniforms.as_bytes();
                block[..inline.len()].copy_from_slice(inline);
                let len = (uniform_size as usize).clamp(inline.len(), InlineUniforms::MAX);
                let (buffer, offset) = self.stage_aligned(&block[..len], self.uniform_align);
                Some([vk::DescriptorBufferInfo::default()
                    .buffer(buffer)
                    .offset(offset)
                    .range(len as u64)])
            }
            None => None,
        };
        let instance = self.buffer(c.instance_buffer);
        let instance_info = [vk::DescriptorBufferInfo::default()
            .buffer(instance.buffer)
            .offset(c.instance_offset as u64)
            .range(vk::WHOLE_SIZE)];
        let image_infos: Vec<[vk::DescriptorImageInfo; 1]> = views[..textures as usize]
            .iter()
            .map(|&view| {
                [vk::DescriptorImageInfo::default()
                    .image_view(view)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]
            })
            .collect();
        let sampler_infos: Vec<[vk::DescriptorImageInfo; 1]> = sampler_states[..samplers as usize]
            .iter()
            .map(|&s| [vk::DescriptorImageInfo::default().sampler(s)])
            .collect();
        let mut writes = Vec::with_capacity(2 + image_infos.len() + sampler_infos.len());
        if let Some(info) = &uniform_info {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(sets[0])
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                    .buffer_info(info),
            );
        }
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(sets[0])
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&instance_info),
        );
        for (i, info) in image_infos.iter().enumerate() {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(sets[1])
                    .dst_binding(i as u32)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(info),
            );
        }
        for (i, info) in sampler_infos.iter().enumerate() {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(sets[1])
                    .dst_binding(PROGRAM_TEXTURES + i as u32)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .image_info(info),
            );
        }
        // SAFETY: the sets were just allocated with the program's layouts and
        // are not yet in use; every write targets a binding of the matching
        // type with live buffers, views and samplers.
        unsafe { self.device.update_descriptor_sets(&writes, &[]) };
        Some((layout, sets))
    }

    /// Two sets of `layouts` from the current slot's program pools, opening
    /// a new pool when the ones it has are full.
    fn allocate_program_sets(
        &mut self,
        layouts: &[vk::DescriptorSetLayout; 2],
    ) -> [vk::DescriptorSet; 2] {
        loop {
            let slot = &mut self.slots[self.slot];
            if let Some(&pool) = slot.program_pools.get(slot.program_pool) {
                let info = vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(layouts);
                // SAFETY: `pool` and the layouts are live objects of this device.
                match unsafe { self.device.allocate_descriptor_sets(&info) } {
                    Ok(sets) => return [sets[0], sets[1]],
                    Err(_) => {
                        slot.program_pool += 1;
                        continue;
                    }
                }
            }
            let sizes = [
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::UNIFORM_BUFFER,
                    descriptor_count: SETS_PER_POOL,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::STORAGE_BUFFER,
                    descriptor_count: SETS_PER_POOL,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLED_IMAGE,
                    descriptor_count: SETS_PER_POOL * 2,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLER,
                    descriptor_count: SETS_PER_POOL,
                },
            ];
            let info = vk::DescriptorPoolCreateInfo::default()
                .max_sets(SETS_PER_POOL * 2)
                .pool_sizes(&sizes);
            // SAFETY: `info` describes a plain pool on a live device.
            let pool = unsafe { self.device.create_descriptor_pool(&info, None) }
                .expect("failed to create a program descriptor pool");
            slot.program_pools.push(pool);
        }
    }

    /// Rebuild `id`'s swapchain at its requested size. Returns `false` when the
    /// surface currently has no area (minimized), leaving it stale.
    fn rebuild_swapchain(&mut self, id: SurfaceId) -> bool {
        let surface_fn = self.surface_fn.as_ref().expect("surface support");
        let s = self.surfaces.get(id.into()).expect("surface handle");
        // SAFETY: `s.surface` is a live surface created on `physical`.
        let caps = unsafe {
            surface_fn.get_physical_device_surface_capabilities(self.physical, s.surface)
        }
        .expect("query Vulkan surface capabilities");
        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: s
                    .width
                    .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: s
                    .height
                    .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        if extent.width == 0 || extent.height == 0 {
            return false;
        }
        let mut image_count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            image_count = image_count.min(caps.max_image_count);
        }
        let composite = [
            vk::CompositeAlphaFlagsKHR::OPAQUE,
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::INHERIT,
            vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
        ]
        .into_iter()
        .find(|&f| caps.supported_composite_alpha.contains(f))
        .unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE);
        let mut usage = vk::ImageUsageFlags::COLOR_ATTACHMENT;
        if caps
            .supported_usage_flags
            .contains(vk::ImageUsageFlags::TRANSFER_SRC)
        {
            usage |= vk::ImageUsageFlags::TRANSFER_SRC;
        }
        let old = s.swapchain;
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(s.surface)
            .min_image_count(image_count)
            .image_format(s.vk_format)
            .image_color_space(s.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(usage)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(composite)
            .present_mode(vk::PresentModeKHR::FIFO)
            // Unclipped, so a presented image keeps every pixel, including
            // ones another window covered: a frame redrawn in part relies on
            // them when the window is uncovered.
            .clipped(false)
            .old_swapchain(old);
        let vk_format = s.vk_format;

        // SAFETY: the device is idle, so no command references the old images,
        // views or framebuffers; the new swapchain is created before the old is
        // destroyed, as `old_swapchain` requires.
        let images = unsafe {
            let _ = self.device.device_wait_idle();
            let swapchain = self
                .swapchain_fn()
                .create_swapchain(&info, None)
                .expect("failed to create a Vulkan swapchain");
            let old_images = std::mem::take(&mut self.surface_mut(id).images);
            for img in old_images {
                self.device.destroy_framebuffer(img.framebuffer, None);
                self.device.destroy_image_view(img.view, None);
            }
            if old != vk::SwapchainKHR::null() {
                self.swapchain_fn().destroy_swapchain(old, None);
            }
            self.surface_mut(id).swapchain = swapchain;
            self.swapchain_fn()
                .get_swapchain_images(swapchain)
                .expect("query swapchain images")
        };
        let pass = self.render_pass(vk_format, true, None);
        let mut built = Vec::with_capacity(images.len());
        for image in images {
            let view = self.create_view(image, vk_format, vk::ImageAspectFlags::COLOR);
            let framebuffer = self.create_framebuffer(pass, &[view], extent.width, extent.height);
            built.push(SwapImage {
                image,
                view,
                framebuffer,
                presented: 0,
            });
        }
        while self.surface(id).finished.len() < built.len() {
            // SAFETY: creating a binary semaphore has no preconditions.
            let sem = unsafe {
                self.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            }
            .expect("failed to create a Vulkan semaphore");
            self.surface_mut(id).finished.push(sem);
        }
        let s = self.surface_mut(id);
        s.images = built;
        s.extent = extent;
        s.stale = false;
        true
    }

    fn create_view(
        &self,
        image: vk::Image,
        format: vk::Format,
        aspect: vk::ImageAspectFlags,
    ) -> vk::ImageView {
        let info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(subresource_range(aspect));
        // SAFETY: `image` is a live single-layer 2D image of `format`; the view
        // spans its every level.
        unsafe { self.device.create_image_view(&info, None) }
            .expect("failed to create a Vulkan image view")
    }

    fn create_framebuffer(
        &self,
        pass: vk::RenderPass,
        attachments: &[vk::ImageView],
        width: u32,
        height: u32,
    ) -> vk::Framebuffer {
        let info = vk::FramebufferCreateInfo::default()
            .render_pass(pass)
            .attachments(attachments)
            .width(width)
            .height(height)
            .layers(1);
        // SAFETY: `attachments` are views of `width`×`height` images whose
        // formats match `pass`'s attachments, in order.
        unsafe { self.device.create_framebuffer(&info, None) }
            .expect("failed to create a Vulkan framebuffer")
    }

    /// Record one render pass into the draw command buffer.
    fn record_pass(&mut self, pass: &RenderPass, commands: &[DrawCommand]) {
        let cmd = self.slots[self.slot].draw;
        let clear = matches!(pass.load, LoadOp::Clear(_));
        let depth = pass.depth.map(|d| {
            let t = self.texture(d.texture);
            (d, t.image, t.view, vk_format(t.format))
        });
        let (image, aspect, framebuffer, extent, format, texture) = match pass.target {
            RenderTarget::Surface(_) if depth.is_some() => {
                panic!("a pass with a depth attachment draws into a texture")
            }
            RenderTarget::Surface(frame) => {
                let Some(acq) = self.acquired.filter(|a| a.surface == frame.surface) else {
                    return;
                };
                let s = self.surface(frame.surface);
                let img = &s.images[acq.image as usize];
                let out = (
                    img.image,
                    vk::ImageAspectFlags::COLOR,
                    img.framebuffer,
                    s.extent,
                    s.vk_format,
                    None,
                );
                let old = if clear {
                    vk::ImageLayout::UNDEFINED
                } else {
                    acq.layout
                };
                // SAFETY: `cmd` is recording and the image is the acquired
                // swapchain image, currently in `old` (or discarded by a clear).
                unsafe {
                    transition(
                        &self.device,
                        cmd,
                        out.0,
                        out.1,
                        old,
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    );
                }
                if let Some(a) = self.acquired.as_mut() {
                    a.layout = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;
                }
                out
            }
            RenderTarget::Texture(id) => {
                let t = self.texture(id);
                assert!(
                    t.render_target,
                    "texture drawn into was not created as a render target"
                );
                let (fmt, w, h, view, fb) = (
                    vk_format(t.format),
                    t.width,
                    t.height,
                    t.view,
                    t.framebuffer,
                );
                let fb = match depth {
                    None if fb == vk::Framebuffer::null() => {
                        let rp = self.render_pass(fmt, true, None);
                        let fb = self.create_framebuffer(rp, &[view], w, h);
                        self.textures
                            .get_mut(id.into())
                            .expect("texture handle")
                            .framebuffer = fb;
                        fb
                    }
                    None => fb,
                    Some((d, _, depth_view, depth_fmt)) => match t.depth_framebuffer {
                        Some((with, fb)) if with == d.texture => fb,
                        old => {
                            if let Some((_, fb)) = old {
                                self.dead_framebuffers.push((self.current_epoch, fb));
                            }
                            let rp = self.render_pass(fmt, true, Some(depth_fmt));
                            let fb = self.create_framebuffer(rp, &[view, depth_view], w, h);
                            self.textures
                                .get_mut(id.into())
                                .expect("texture handle")
                                .depth_framebuffer = Some((d.texture, fb));
                            fb
                        }
                    },
                };
                let t = self.texture(id);
                let old = if clear {
                    vk::ImageLayout::UNDEFINED
                } else {
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                };
                // SAFETY: `cmd` is recording; the texture rests in
                // SHADER_READ_ONLY_OPTIMAL between commands.
                unsafe {
                    transition(
                        &self.device,
                        cmd,
                        t.image,
                        vk::ImageAspectFlags::COLOR,
                        old,
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    );
                }
                (
                    t.image,
                    vk::ImageAspectFlags::COLOR,
                    fb,
                    vk::Extent2D {
                        width: w,
                        height: h,
                    },
                    fmt,
                    Some(id),
                )
            }
        };
        if let Some((_, depth_image, _, _)) = depth {
            // SAFETY: `cmd` is recording outside a render pass; a depth
            // render target rests in DEPTH_STENCIL_ATTACHMENT_OPTIMAL. The
            // barrier orders the previous pass's depth writes before this
            // pass's clear.
            unsafe {
                transition(
                    &self.device,
                    cmd,
                    depth_image,
                    vk::ImageAspectFlags::DEPTH,
                    vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                    vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                );
            }
        }
        let render_pass = self.render_pass(format, clear, depth.map(|d| d.3));
        let clear_values = [
            vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: match pass.load {
                        LoadOp::Clear(c) => c,
                        LoadOp::Load => [0.0; 4],
                    },
                },
            },
            vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: pass.depth.map_or(0.0, |d| d.clear),
                    stencil: 0,
                },
            },
        ];
        let clear_values = &clear_values[..1 + usize::from(depth.is_some())];
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        };
        let begin = vk::RenderPassBeginInfo::default()
            .render_pass(render_pass)
            .framebuffer(framebuffer)
            .render_area(area)
            .clear_values(clear_values);
        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: extent.width as f32,
            height: extent.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        // SAFETY: `cmd` is recording outside a render pass; `framebuffer` was
        // built for a pass compatible with `render_pass` and the target is in
        // COLOR_ATTACHMENT_OPTIMAL, the pass's initial layout.
        unsafe {
            self.device
                .cmd_begin_render_pass(cmd, &begin, vk::SubpassContents::INLINE);
            self.device.cmd_set_viewport(cmd, 0, &[viewport]);
        }
        let mut bound = Bound::default();
        for c in commands {
            let program = self.program_sets(c);
            self.record_command(cmd, c, extent, program, &mut bound);
        }
        // SAFETY: the render pass begun above is ended in the same command
        // buffer; a texture target returns to its resting layout.
        unsafe {
            self.device.cmd_end_render_pass(cmd);
            if texture.is_some() {
                transition(
                    &self.device,
                    cmd,
                    image,
                    aspect,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                );
            }
        }
    }

    /// Record one draw inside the open render pass; `program` carries a
    /// program draw's layout and descriptor sets.
    fn record_command(
        &self,
        cmd: vk::CommandBuffer,
        c: &DrawCommand,
        extent: vk::Extent2D,
        program: Option<(vk::PipelineLayout, [vk::DescriptorSet; 2])>,
        bound: &mut Bound,
    ) {
        let (sx, sy, sw, sh) = match c.scissor {
            Some((x, y, w, h)) => {
                let x = x.min(extent.width);
                let y = y.min(extent.height);
                (x, y, w.min(extent.width - x), h.min(extent.height - y))
            }
            None => (0, 0, extent.width, extent.height),
        };
        let scissor = vk::Rect2D {
            offset: vk::Offset2D {
                x: sx as i32,
                y: sy as i32,
            },
            extent: vk::Extent2D {
                width: sw,
                height: sh,
            },
        };
        // SAFETY: `cmd` is recording inside a render pass; the pipeline,
        // descriptor set and buffers are live objects of this device, bound
        // through the shared pipeline layout they were created against. The
        // renderer guarantees the instance/index ranges lie inside their buffers.
        unsafe {
            self.device.cmd_set_scissor(cmd, 0, &[scissor]);
            if bound.pipeline != Some(c.pipeline) {
                let p = self
                    .pipelines
                    .get(c.pipeline.into())
                    .expect("pipeline handle does not resolve");
                self.device
                    .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, p.pipeline);
                bound.pipeline = Some(c.pipeline);
            }
            if let Some((layout, sets)) = program {
                self.device.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    layout,
                    0,
                    &sets,
                    &[],
                );
                // The program's sets displace the shared layout's.
                bound.group = None;
                let Geometry::Generated { count } = c.geometry else {
                    panic!("a program draws generated quads");
                };
                self.device.cmd_draw(cmd, 6, count, 0, 0);
                return;
            }
            if let Some(bg) = c.bind_group
                && bound.group != Some(bg)
            {
                let g = self
                    .bind_groups
                    .get(bg.into())
                    .expect("bind group handle does not resolve");
                self.device.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipeline_layout,
                    0,
                    &[g.set],
                    &[],
                );
                bound.group = Some(bg);
            }
            let uniforms = c.uniforms.as_bytes();
            if !uniforms.is_empty() {
                let mut words = [0u8; InlineUniforms::MAX];
                words[..uniforms.len()].copy_from_slice(uniforms);
                self.device.cmd_push_constants(
                    cmd,
                    self.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    &words[..uniforms.len().next_multiple_of(4)],
                );
            }
            match c.geometry {
                Geometry::Generated { count } => {
                    let inst = self.buffer(c.instance_buffer);
                    self.device.cmd_bind_vertex_buffers(
                        cmd,
                        0,
                        &[inst.buffer],
                        &[c.instance_offset as u64],
                    );
                    self.device.cmd_draw(cmd, 6, count, 0, 0);
                }
                Geometry::IndexedMesh {
                    vertex_buffer,
                    index_buffer,
                    index_format,
                    index_offset,
                    index_count,
                } => {
                    let vtx = self.buffer(vertex_buffer);
                    let idx = self.buffer(index_buffer);
                    let index_type = match index_format {
                        IndexFormat::U16 => vk::IndexType::UINT16,
                        IndexFormat::U32 => vk::IndexType::UINT32,
                    };
                    self.device
                        .cmd_bind_vertex_buffers(cmd, 0, &[vtx.buffer], &[0]);
                    self.device.cmd_bind_index_buffer(
                        cmd,
                        idx.buffer,
                        (index_offset as usize * index_format.size()) as u64,
                        index_type,
                    );
                    self.device.cmd_draw_indexed(cmd, index_count, 1, 0, 0, 0);
                }
                Geometry::InstancedMesh {
                    vertex_buffer,
                    index_buffer,
                    index_format,
                    index_offset,
                    index_count,
                    count,
                } => {
                    let vtx = self.buffer(vertex_buffer);
                    let inst = self.buffer(c.instance_buffer);
                    let idx = self.buffer(index_buffer);
                    let index_type = match index_format {
                        IndexFormat::U16 => vk::IndexType::UINT16,
                        IndexFormat::U32 => vk::IndexType::UINT32,
                    };
                    self.device.cmd_bind_vertex_buffers(
                        cmd,
                        0,
                        &[vtx.buffer, inst.buffer],
                        &[0, c.instance_offset as u64],
                    );
                    self.device.cmd_bind_index_buffer(
                        cmd,
                        idx.buffer,
                        (index_offset as usize * index_format.size()) as u64,
                        index_type,
                    );
                    self.device
                        .cmd_draw_indexed(cmd, index_count, count, 0, 0, 0);
                }
            }
        }
    }
}

impl GpuBackend for VulkanBackend {
    const SHADER_LANG: ShaderLang = ShaderLang::SpirV;

    type Compiler = VulkanCompiler;

    fn program_compiler(&self) -> VulkanCompiler {
        VulkanCompiler {
            device: self.device.clone(),
            cache: self.pipeline_cache,
        }
    }

    fn install_program(&mut self, mut program: VulkanProgram) -> PipelineId {
        let (pipeline, layout) = program
            .built
            .take()
            .expect("a compiled program is installed once");
        self.pipelines
            .insert(VulkanPipeline {
                pipeline,
                program: Some(layout),
            })
            .into()
    }

    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        let len = desc.size.max(1);
        let (buffer, memory, ptr) = self.create_host_buffer(
            len as u64,
            vk::BufferUsageFlags::VERTEX_BUFFER
                | vk::BufferUsageFlags::INDEX_BUFFER
                | vk::BufferUsageFlags::UNIFORM_BUFFER
                | vk::BufferUsageFlags::STORAGE_BUFFER
                | vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST,
        );
        self.buffers
            .insert(VulkanBuffer {
                buffer,
                memory,
                ptr,
                len,
            })
            .into()
    }

    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId {
        self.new_texture(desc, false)
    }

    fn create_texture_mipped(&mut self, desc: &TextureDesc, levels: &[&[u8]]) -> TextureId {
        let id = self.new_texture_levels(desc, false, levels.len().max(1) as u32);
        let (mut w, mut h) = (desc.width.max(1), desc.height.max(1));
        for (k, bytes) in levels.iter().enumerate() {
            if k > 0 {
                (w, h) = ((w / 2).max(1), (h / 2).max(1));
            }
            let len = desc.format.data_size(w, h);
            if bytes.len() < len {
                break;
            }
            self.write_level(id, k as u32, (0, 0, w, h), &bytes[..len]);
        }
        id
    }

    fn import_external(&mut self, image: &ExternalImage) -> Option<TextureId> {
        #[cfg(unix)]
        if let ExternalImage::DmaBuf { .. } = image {
            return self.write_dma_buf(None, image);
        }
        let (at, region, turns) = self.external_source(image)?;
        let (width, height) = image.size();
        let id = self.converted_texture(width, height);
        self.convert(id, at, region, turns);
        Some(id)
    }

    fn adopt_external(&mut self, id: TextureId, image: &ExternalImage) -> bool {
        let Some(t) = self.textures.get(id.into()) else {
            return false;
        };
        if t.converted.is_none() || (t.width, t.height) != image.size() {
            return false;
        }
        #[cfg(unix)]
        if let ExternalImage::DmaBuf { .. } = image {
            return self.write_dma_buf(Some(id), image) == Some(id);
        }
        let Some((at, region, turns)) = self.external_source(image) else {
            return false;
        };
        self.convert(id, at, region, turns);
        true
    }

    fn write_nv12(&mut self, id: Option<TextureId>, picture: &Nv12<'_>) -> Option<TextureId> {
        let (w, h) = (picture.width, picture.height);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (luma, chroma) = (
            rows_of(picture.y, picture.y_stride, w as usize, h as usize)?,
            rows_of(picture.uv, picture.uv_stride, cw as usize * 2, ch as usize)?,
        );
        if w == 0 || h == 0 || !picture.uv_stride.is_multiple_of(2) {
            return None;
        }
        let id = self.plane_target(id, w, h);
        self.ensure_recording();
        let (buffer, offset) = self.stage_aligned(luma, 4);
        let luma = planes::Staged {
            buffer,
            offset,
            row_length: picture.y_stride as u32,
        };
        let (buffer, offset) = self.stage_aligned(chroma, 4);
        let chroma = planes::Staged {
            buffer,
            offset,
            row_length: (picture.uv_stride / 2) as u32,
        };
        self.convert_planes(id, picture, [luma, chroma], None);
        Some(id)
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        let (filter, mip) = match desc.filter {
            FilterMode::Nearest => (vk::Filter::NEAREST, vk::SamplerMipmapMode::NEAREST),
            FilterMode::Linear => (vk::Filter::LINEAR, vk::SamplerMipmapMode::NEAREST),
            FilterMode::MipmapLinear => (vk::Filter::LINEAR, vk::SamplerMipmapMode::LINEAR),
        };
        let address = match desc.address {
            AddressMode::ClampToEdge => vk::SamplerAddressMode::CLAMP_TO_EDGE,
            AddressMode::Repeat => vk::SamplerAddressMode::REPEAT,
            AddressMode::Mirror => vk::SamplerAddressMode::MIRRORED_REPEAT,
        };
        let info = vk::SamplerCreateInfo::default()
            .mag_filter(filter)
            .min_filter(filter)
            .mipmap_mode(mip)
            .address_mode_u(address)
            .address_mode_v(address)
            .address_mode_w(address)
            .max_lod(vk::LOD_CLAMP_NONE);
        // SAFETY: `info` is a plain sampler description with no extensions.
        let sampler = unsafe { self.device.create_sampler(&info, None) }
            .expect("failed to create a Vulkan sampler");
        self.samplers.insert(sampler).into()
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, crate::instance::LayoutError> {
        layout.validate_against(&desc.instance_schema)?;
        let ShaderCode::SpirV(bytes) = desc.code else {
            panic!(
                "the Vulkan backend consumes SPIR-V, got {:?}",
                desc.code.lang()
            );
        };
        let words = ash::util::read_spv(&mut Cursor::new(bytes)).expect("malformed SPIR-V module");
        let module_info = vk::ShaderModuleCreateInfo::default().code(&words);
        // SAFETY: `words` is a whole SPIR-V module (checked by the validator when
        // the artifacts were frozen).
        let module = unsafe { self.device.create_shader_module(&module_info, None) }
            .expect("failed to create a Vulkan shader module");
        let input_rate = match desc.builtin {
            BuiltinShader::Path | BuiltinShader::Mesh | BuiltinShader::Scene => {
                vk::VertexInputRate::VERTEX
            }
            _ => vk::VertexInputRate::INSTANCE,
        };
        // A scene draw adds its instances as a second, instance-rate stream at
        // the locations after the vertex attributes.
        let streams: &[(InstanceSchema, u32, vk::VertexInputRate)] =
            if desc.builtin == BuiltinShader::Scene {
                &[
                    (desc.instance_schema, layout.stride as u32, input_rate),
                    (
                        scene::INSTANCE_SCHEMA,
                        SceneInstance::SIZE as u32,
                        vk::VertexInputRate::INSTANCE,
                    ),
                ]
            } else {
                &[(desc.instance_schema, layout.stride as u32, input_rate)]
            };
        let mut attributes = Vec::new();
        let mut bindings = Vec::with_capacity(streams.len());
        for (binding, (schema, stride, input_rate)) in streams.iter().enumerate() {
            let mut offset = 0u32;
            for a in schema.attributes {
                attributes.push(vk::VertexInputAttributeDescription {
                    location: attributes.len() as u32,
                    binding: binding as u32,
                    format: attr_format(a.format),
                    offset,
                });
                offset += a.format.size() as u32;
            }
            bindings.push(vk::VertexInputBindingDescription {
                binding: binding as u32,
                stride: *stride,
                input_rate: *input_rate,
            });
        }
        let vertex_input = if attributes.is_empty() {
            vk::PipelineVertexInputStateCreateInfo::default()
        } else {
            vk::PipelineVertexInputStateCreateInfo::default()
                .vertex_binding_descriptions(&bindings)
                .vertex_attribute_descriptions(&attributes)
        };
        let pipeline_layout = self.pipeline_layout;
        // SAFETY: the module's entry points read the attributes, push constants
        // and set-0 bindings the shared layout declares.
        let pipeline = unsafe {
            let result = self.graphics_pipeline(module, desc, &vertex_input, pipeline_layout);
            self.device.destroy_shader_module(module, None);
            result.expect("failed to create a Vulkan pipeline")
        };
        Ok(self
            .pipelines
            .insert(VulkanPipeline {
                pipeline,
                program: None,
            })
            .into())
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        let (set, pool) = self.allocate_set();
        // The k-th binding of each kind takes that kind's k-th slot of the
        // shared layout: a 2D program's `tex`, `dst_tex`, sampler and uniform
        // block are each kind's first slots.
        let mut images: Vec<(u32, [vk::DescriptorImageInfo; 1])> = Vec::with_capacity(4);
        let mut samplers: Vec<(u32, [vk::DescriptorImageInfo; 1])> = Vec::with_capacity(1);
        let mut uniforms: Vec<(u32, [vk::DescriptorBufferInfo; 1])> = Vec::with_capacity(1);
        for binding in &desc.bindings {
            match *binding {
                Binding::Texture(id) => {
                    if let Some(&at) = scene::NATIVE_TEXTURE_BINDINGS.get(images.len()) {
                        let info = vk::DescriptorImageInfo::default()
                            .image_view(self.texture(id).view)
                            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                        images.push((at, [info]));
                    }
                }
                Binding::Sampler(id) => {
                    let sampler = *self
                        .samplers
                        .get(id.into())
                        .expect("sampler handle does not resolve");
                    if let Some(&at) = scene::NATIVE_SAMPLER_BINDINGS.get(samplers.len()) {
                        samplers.push((at, [vk::DescriptorImageInfo::default().sampler(sampler)]));
                    }
                }
                Binding::Uniform(id) => {
                    if let Some(&at) = scene::NATIVE_UNIFORM_BINDINGS.get(uniforms.len()) {
                        let info = vk::DescriptorBufferInfo::default()
                            .buffer(self.buffer(id).buffer)
                            .offset(0)
                            .range(vk::WHOLE_SIZE);
                        uniforms.push((at, [info]));
                    }
                }
            }
        }
        // A single-texture program leaves `dst_tex` unread; fill it anyway so the
        // set is complete for any program it is bound with.
        if images.len() == 1 {
            let info = images[0].1;
            images.push((BINDING_DST_TEX, info));
        }
        let mut writes = Vec::with_capacity(images.len() + samplers.len() + uniforms.len());
        for (at, info) in &images {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(*at)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(info),
            );
        }
        for (at, info) in &samplers {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(*at)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .image_info(info),
            );
        }
        for (at, info) in &uniforms {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(*at)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                    .buffer_info(info),
            );
        }
        // SAFETY: `set` was just allocated with the shared layout; every write
        // targets a binding of the matching type with live views and samplers.
        unsafe { self.device.update_descriptor_sets(&writes, &[]) };
        self.bind_groups
            .insert(VulkanBindGroup {
                set,
                pool,
                bindings: desc.bindings.clone(),
            })
            .into()
    }

    fn destroy_buffer(&mut self, id: BufferId) {
        if self.buffers.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Buffer, id.into(), self.current_epoch);
        }
    }

    fn destroy_texture(&mut self, id: TextureId) {
        if self.textures.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Texture, id.into(), self.current_epoch);
        }
    }

    fn destroy_sampler(&mut self, id: SamplerId) {
        if self.samplers.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Sampler, id.into(), self.current_epoch);
        }
    }

    fn destroy_pipeline(&mut self, id: PipelineId) {
        if self.pipelines.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Pipeline, id.into(), self.current_epoch);
        }
    }

    fn destroy_bind_group(&mut self, id: BindGroupId) {
        if self.bind_groups.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::BindGroup, id.into(), self.current_epoch);
        }
    }

    fn write_buffer(&mut self, id: BufferId, offset: usize, bytes: &[u8]) {
        let buf = self.buffer(id);
        assert!(
            offset + bytes.len() <= buf.len,
            "write_buffer out of range: {} + {} > {}",
            offset,
            bytes.len(),
            buf.len
        );
        // SAFETY: the mapping covers `len` bytes and the range was checked; the
        // memory is host-coherent, so no flush is needed.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.ptr.add(offset), bytes.len());
        }
    }

    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]) {
        let t = self.texture(id);
        // A block format's region is rows of whole blocks, tightly packed.
        let len = t.format.data_size(w, h);
        assert!(
            bytes.len() >= len,
            "write_texture: {} bytes < a {}x{} region of {:?}",
            bytes.len(),
            w,
            h,
            t.format
        );
        if len == 0 {
            return;
        }
        self.write_level(id, 0, (x, y, w, h), &bytes[..len]);
    }

    fn create_surface(&mut self, raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId {
        // SAFETY: the platform layer hands over live native window handles that
        // outlive the GPU surface; each create call is made only when its
        // extension was enabled on the instance (the loader panics otherwise).
        let surface = unsafe {
            match raw {
                RawWindowHandle::Xcb { connection, window } => {
                    let info = vk::XcbSurfaceCreateInfoKHR::default()
                        .connection(connection)
                        .window(window);
                    khr::xcb_surface::Instance::new(&self.entry, &self.instance)
                        .create_xcb_surface(&info, None)
                }
                RawWindowHandle::Wayland { display, surface } => {
                    let info = vk::WaylandSurfaceCreateInfoKHR::default()
                        .display(display.cast())
                        .surface(surface.cast());
                    khr::wayland_surface::Instance::new(&self.entry, &self.instance)
                        .create_wayland_surface(&info, None)
                }
                RawWindowHandle::AndroidNdk { a_native_window } => {
                    let info =
                        vk::AndroidSurfaceCreateInfoKHR::default().window(a_native_window.cast());
                    khr::android_surface::Instance::new(&self.entry, &self.instance)
                        .create_android_surface(&info, None)
                }
                RawWindowHandle::Win32 { hwnd, hinstance } => {
                    let info = vk::Win32SurfaceCreateInfoKHR::default()
                        .hinstance(hinstance as vk::HINSTANCE)
                        .hwnd(hwnd as vk::HWND);
                    khr::win32_surface::Instance::new(&self.entry, &self.instance)
                        .create_win32_surface(&info, None)
                }
                other => panic!("VulkanBackend cannot present to {other:?}"),
            }
        }
        .expect("failed to create a Vulkan surface");
        let surface_fn = self.surface_fn.as_ref().expect("surface support");
        // SAFETY: `surface` was just created on this instance; `physical` and
        // `queue_family` belong to it.
        let (supported, formats) = unsafe {
            (
                surface_fn
                    .get_physical_device_surface_support(self.physical, self.queue_family, surface)
                    .unwrap_or(false),
                surface_fn
                    .get_physical_device_surface_formats(self.physical, surface)
                    .unwrap_or_default(),
            )
        };
        assert!(
            supported,
            "the Vulkan graphics queue cannot present to this surface"
        );
        let pick = |f: vk::Format| {
            formats
                .iter()
                .find(|s| s.format == f && s.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
                .or_else(|| formats.iter().find(|s| s.format == f))
                .copied()
        };
        // An sRGB format encodes what the pipelines write; a plain
        // unorm one is the last resort of a surface that offers no other.
        let (format, chosen) = [
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Bgra8Unorm,
            TextureFormat::Rgba8Unorm,
        ]
        .into_iter()
        .find_map(|f| pick(vk_format(f)).map(|s| (f, s)))
        // A lone UNDEFINED entry means "any format".
        .unwrap_or((
            TextureFormat::Bgra8UnormSrgb,
            vk::SurfaceFormatKHR {
                format: vk::Format::B8G8R8A8_SRGB,
                color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
            },
        ));
        self.surfaces
            .insert(VulkanSurface {
                surface,
                swapchain: vk::SwapchainKHR::null(),
                images: Vec::new(),
                finished: Vec::new(),
                format,
                vk_format: chosen.format,
                color_space: chosen.color_space,
                width,
                height,
                extent: vk::Extent2D::default(),
                stale: true,
                presents: 0,
            })
            .into()
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        let s = self.surface_mut(id);
        s.width = width;
        s.height = height;
        if s.extent.width != width || s.extent.height != height {
            s.stale = true;
        }
    }

    fn destroy_surface(&mut self, id: SurfaceId) {
        if self.surfaces.get(id.into()).is_none() {
            return;
        }
        // Abandoning an open frame also idles the device.
        if self.acquired.is_some() {
            self.device_lost(id);
        } else {
            self.submit(None, None);
            // SAFETY: waiting for the device to idle has no preconditions.
            unsafe {
                let _ = self.device.device_wait_idle();
            }
            self.reclaim_completed();
        }
        let s = self.surfaces.remove(id.into()).expect("checked");
        // SAFETY: the device is idle, so no submission references the
        // swapchain's images, views, framebuffers or semaphores; each is
        // destroyed once, the swapchain before its surface.
        unsafe {
            for img in s.images {
                self.device.destroy_framebuffer(img.framebuffer, None);
                self.device.destroy_image_view(img.view, None);
            }
            for sem in s.finished {
                self.device.destroy_semaphore(sem, None);
            }
            if s.swapchain != vk::SwapchainKHR::null() {
                self.swapchain_fn().destroy_swapchain(s.swapchain, None);
            }
            if let Some(f) = &self.surface_fn {
                f.destroy_surface(s.surface, None);
            }
        }
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame> {
        self.reclaim_completed();
        if self.acquired.is_some() {
            // A frame begun and never presented: drop it as a lost frame.
            let open = self.acquired.map(|a| a.surface).expect("checked");
            self.device_lost(open);
        }
        if self.surface(surface).stale && !self.rebuild_swapchain(surface) {
            return None;
        }
        self.ensure_recording();
        let acquire = self.slots[self.slot].acquire;
        let swapchain = self.surface(surface).swapchain;
        // SAFETY: the slot's acquire semaphore is unsignalled — its last wait
        // completed before the slot was reopened — and the swapchain is live.
        let result = unsafe {
            self.swapchain_fn()
                .acquire_next_image(swapchain, u64::MAX, acquire, vk::Fence::null())
        };
        let image = match result {
            Ok((image, suboptimal)) => {
                if suboptimal {
                    self.surface_mut(surface).stale = true;
                }
                image
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                self.surface_mut(surface).stale = true;
                return None;
            }
            Err(e) => panic!("Vulkan swapchain acquire failed: {e}"),
        };
        let presented = self.surface(surface).images[image as usize].presented > 0;
        self.acquired = Some(Acquired {
            surface,
            image,
            layout: if presented {
                vk::ImageLayout::PRESENT_SRC_KHR
            } else {
                vk::ImageLayout::UNDEFINED
            },
        });
        self.current_epoch = self.current_epoch.next();
        Some(Frame {
            surface,
            drawable: image,
        })
    }

    fn encode(&mut self, list: &DrawList<'_>) {
        self.ensure_recording();
        for pass in list.passes {
            self.record_pass(pass, &list.commands[pass.command_range()]);
        }
        if self.acquired.is_none() {
            // Offscreen-only work never reaches `begin_frame`, so reclaim here too.
            self.submit(None, None);
            self.reclaim_completed();
        }
    }

    fn present(&mut self, frame: Frame) {
        self.present_with_damage(frame, &[]);
    }

    fn frame_age(&self, frame: Frame) -> u32 {
        let Some(s) = self.surfaces.get(frame.surface.into()) else {
            return 0;
        };
        match s.images.get(frame.drawable as usize) {
            Some(img) if img.presented > 0 => {
                u32::try_from(s.presents + 1 - img.presented).unwrap_or(0)
            }
            _ => 0,
        }
    }

    fn present_with_damage(&mut self, frame: Frame, damage: &[(u32, u32, u32, u32)]) {
        let Some(acq) = self.acquired.filter(|a| a.surface == frame.surface) else {
            return;
        };
        self.acquired = None;
        let cmd = self.slots[self.slot].draw;
        let acquire = self.slots[self.slot].acquire;
        let s = self.surface(frame.surface);
        let (image, swapchain, finished) = (
            s.images[acq.image as usize].image,
            s.swapchain,
            s.finished[acq.image as usize],
        );
        // SAFETY: the recording is open (begin_frame opened it); the image is
        // the acquired one in `acq.layout`.
        unsafe {
            transition(
                &self.device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                acq.layout,
                vk::ImageLayout::PRESENT_SRC_KHR,
            );
        }
        let s = self.surface_mut(frame.surface);
        s.presents += 1;
        s.images[acq.image as usize].presented = s.presents;
        self.submit(Some(acquire), Some(finished));
        let wait = [finished];
        let swapchains = [swapchain];
        let indices = [acq.image];
        // More rects than fit name no damage: the whole image.
        let mut rects = [vk::RectLayerKHR::default(); PRESENT_RECTS];
        let named = if damage.len() <= PRESENT_RECTS {
            damage.len()
        } else {
            0
        };
        for (out, &(x, y, w, h)) in rects.iter_mut().zip(&damage[..named]) {
            *out = vk::RectLayerKHR::default()
                .offset(vk::Offset2D {
                    x: x as i32,
                    y: y as i32,
                })
                .extent(vk::Extent2D {
                    width: w,
                    height: h,
                });
        }
        let rects = &rects[..named];
        let region = [vk::PresentRegionKHR::default().rectangles(rects)];
        let mut regions = vk::PresentRegionsKHR::default().regions(&region);
        let mut info = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait)
            .swapchains(&swapchains)
            .image_indices(&indices);
        if self.incremental_present && !rects.is_empty() {
            info = info.push_next(&mut regions);
        }
        // SAFETY: the image was acquired from `swapchain` and the submission
        // just made signals `finished` once it is in PRESENT_SRC_KHR.
        match unsafe { self.swapchain_fn().queue_present(self.queue, &info) } {
            Ok(false) => {}
            Ok(true)
            | Err(vk::Result::ERROR_OUT_OF_DATE_KHR)
            | Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                self.surface_mut(frame.surface).stale = true;
            }
            Err(e) => panic!("Vulkan present failed: {e}"),
        }
    }

    fn device_lost(&mut self, _surface: SurfaceId) {
        if let Some(acq) = self.acquired.take() {
            // The acquired image is abandoned: submit only the uploads, waiting on
            // the acquire so its semaphore is consumed, and rebuild the swapchain
            // before the next frame — which also releases the image.
            if self.recording.is_some() {
                let slot = &mut self.slots[self.slot];
                let upload = [slot.upload];
                let wait = [slot.acquire];
                let stages = [vk::PipelineStageFlags::ALL_COMMANDS];
                let info = [vk::SubmitInfo::default()
                    .wait_semaphores(&wait)
                    .wait_dst_stage_mask(&stages)
                    .command_buffers(&upload)];
                // SAFETY: both buffers are recording and ended exactly once; the
                // draw buffer is discarded (its pool is reset when the slot
                // reopens) and the fence was reset when the slot opened.
                unsafe {
                    let _ = self.device.end_command_buffer(slot.upload);
                    let _ = self.device.end_command_buffer(slot.draw);
                    let _ = self.device.queue_submit(self.queue, &info, slot.fence);
                }
                slot.submitted = true;
                slot.epoch = self.recording.take().expect("checked");
                self.slot = (self.slot + 1) % FRAMES_IN_FLIGHT;
            }
            self.surface_mut(acq.surface).stale = true;
        } else {
            self.submit(None, None);
        }
        // SAFETY: waiting for the device to idle has no preconditions; after it
        // every submission has finished (or the device is gone, and no further
        // work will run).
        unsafe {
            let _ = self.device.device_wait_idle();
        }
        self.reclaim_completed();
    }

    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn surface_format(&self, surface: SurfaceId) -> TextureFormat {
        self.surface(surface).format
    }

    fn time_frames(&mut self, on: bool) {
        if let Some(clock) = &mut self.clock {
            if on && !clock.on {
                clock.read.clear();
            }
            clock.on = on;
        }
    }

    fn frame_epoch(&self) -> u64 {
        self.current_epoch.0
    }

    fn finished_frame_times(&mut self, out: &mut Vec<(u64, u64)>) {
        let Some(clock) = &mut self.clock else {
            return;
        };
        for (index, slot) in self.slots.iter_mut().enumerate() {
            // SAFETY: `slot.fence` is a live fence of this device.
            if slot.submitted && unsafe { self.device.get_fence_status(slot.fence) } == Ok(true) {
                read_clock(&self.device, clock, slot, index);
            }
        }
        clock.read.sort_unstable_by_key(|&(epoch, _)| epoch);
        out.append(&mut clock.read);
    }
}

/// Reads the time slot `index` took, if its finished submission timed a frame.
fn read_clock(device: &ash::Device, clock: &mut Clock, slot: &mut FrameSlot, index: usize) {
    let Some(epoch) = slot.timed.take() else {
        return;
    };
    let mut stamps = [0u64; 2];
    // SAFETY: the slot's submission finished, so both of its queries are
    // available; `stamps` holds exactly the two read.
    let read = unsafe {
        device.get_query_pool_results(
            clock.pool,
            2 * index as u32,
            &mut stamps,
            vk::QueryResultFlags::TYPE_64,
        )
    };
    if read.is_ok() && clock.read.len() < Clock::KEPT {
        let ticks = stamps[1].wrapping_sub(stamps[0]) & clock.mask;
        clock
            .read
            .push((epoch, (ticks as f64 * clock.period) as u64));
    }
}

impl Drop for VulkanBackend {
    fn drop(&mut self) {
        // SAFETY: after the device idles nothing references any object below;
        // each is destroyed exactly once, children before their parents, and the
        // messenger before the counter it points at.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.held.clear();
            if let Some(mut converter) = self.converter.take() {
                converter.destroy(&self.device);
            }
            if let Some(mut planar) = self.planar.take() {
                planar.destroy(&self.device);
            }
            self.imports.destroy(&self.device);
            for b in self.buffers.drain() {
                destroy_buffer(&self.device, b);
            }
            for t in self.textures.drain() {
                destroy_texture(&self.device, t);
            }
            for s in self.samplers.drain() {
                self.device.destroy_sampler(s, None);
            }
            for p in self.pipelines.drain() {
                destroy_pipeline(&self.device, p);
            }
            self.bind_groups.drain().for_each(drop);
            for s in self.surfaces.drain() {
                for img in s.images {
                    self.device.destroy_framebuffer(img.framebuffer, None);
                    self.device.destroy_image_view(img.view, None);
                }
                for sem in s.finished {
                    self.device.destroy_semaphore(sem, None);
                }
                if s.swapchain != vk::SwapchainKHR::null() {
                    self.swapchain_fn
                        .as_ref()
                        .expect("a swapchain implies swapchain support")
                        .destroy_swapchain(s.swapchain, None);
                }
                if let Some(f) = &self.surface_fn {
                    f.destroy_surface(s.surface, None);
                }
            }
            for slot in &mut self.slots {
                for chunk in slot.staging.chunks.drain(..) {
                    self.device.destroy_buffer(chunk.buffer, None);
                    self.device.free_memory(chunk.memory, None);
                }
                for pool in slot.program_pools.drain(..) {
                    self.device.destroy_descriptor_pool(pool, None);
                }
                self.device.destroy_command_pool(slot.pool, None);
                self.device.destroy_fence(slot.fence, None);
                self.device.destroy_semaphore(slot.acquire, None);
            }
            if let Some(clock) = self.clock.take() {
                self.device.destroy_query_pool(clock.pool, None);
            }
            for e in self.render_passes.drain(..) {
                self.device.destroy_render_pass(e.pass, None);
            }
            for pool in self.descriptor_pools.drain(..) {
                self.device.destroy_descriptor_pool(pool, None);
            }
            self.device
                .destroy_pipeline_cache(self.pipeline_cache, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .destroy_descriptor_set_layout(self.set_layout, None);
            self.device.destroy_device(None);
            if let Some((loader, messenger)) = self.debug.take() {
                loader.destroy_debug_utils_messenger(messenger, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

/// The best physical device with a graphics queue: discrete over integrated
/// over anything else.
fn pick_physical_device(instance: &ash::Instance) -> Result<(vk::PhysicalDevice, u32), String> {
    // SAFETY: `instance` is live; the queries have no other preconditions.
    let devices = unsafe { instance.enumerate_physical_devices() }
        .map_err(|e| format!("cannot enumerate the devices: {e}"))?;
    let count = devices.len();
    devices
        .into_iter()
        .filter_map(|pd| {
            // SAFETY: `pd` was enumerated from `instance`.
            let (props, families) = unsafe {
                (
                    instance.get_physical_device_properties(pd),
                    instance.get_physical_device_queue_family_properties(pd),
                )
            };
            let family = families
                .iter()
                .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))?;
            let rank = match props.device_type {
                vk::PhysicalDeviceType::DISCRETE_GPU => 3,
                vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
                vk::PhysicalDeviceType::VIRTUAL_GPU => 1,
                _ => 0,
            };
            Some((rank, pd, family as u32))
        })
        .max_by_key(|&(rank, ..)| rank)
        .map(|(_, pd, family)| (pd, family))
        .ok_or_else(|| format!("none of the {count} devices has a graphics queue"))
}

/// The shared descriptor-set layout, pipeline layout and pipeline cache.
fn create_layouts(
    device: &ash::Device,
) -> (
    vk::DescriptorSetLayout,
    vk::PipelineLayout,
    vk::PipelineCache,
) {
    let stages = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT;
    // The 2D programs' `tex`, `dst_tex`, sampler and uniform block are the
    // first slots of each kind; a scene program's further slots follow.
    let slot = |at: u32, ty| {
        vk::DescriptorSetLayoutBinding::default()
            .binding(at)
            .descriptor_type(ty)
            .descriptor_count(1)
            .stage_flags(stages)
    };
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = scene::NATIVE_TEXTURE_BINDINGS
        .iter()
        .map(|&at| slot(at, vk::DescriptorType::SAMPLED_IMAGE))
        .chain(
            scene::NATIVE_SAMPLER_BINDINGS
                .iter()
                .map(|&at| slot(at, vk::DescriptorType::SAMPLER)),
        )
        .chain(
            scene::NATIVE_UNIFORM_BINDINGS
                .iter()
                .map(|&at| slot(at, vk::DescriptorType::UNIFORM_BUFFER)),
        )
        .collect();
    let push = [vk::PushConstantRange {
        stage_flags: stages,
        offset: 0,
        size: InlineUniforms::MAX as u32,
    }];
    // SAFETY: plain layout descriptions borrowing live locals.
    unsafe {
        let set_layout = device
            .create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )
            .expect("failed to create the Vulkan descriptor set layout");
        let set_layouts = [set_layout];
        let pipeline_layout = device
            .create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&set_layouts)
                    .push_constant_ranges(&push),
                None,
            )
            .expect("failed to create the Vulkan pipeline layout");
        let cache = device
            .create_pipeline_cache(&vk::PipelineCacheCreateInfo::default(), None)
            .expect("failed to create the Vulkan pipeline cache");
        (set_layout, pipeline_layout, cache)
    }
}

fn create_frame_slot(device: &ash::Device, queue_family: u32) -> FrameSlot {
    // SAFETY: plain object creation on a live device; the fence starts
    // unsignalled and unsubmitted.
    unsafe {
        let pool = device
            .create_command_pool(
                &vk::CommandPoolCreateInfo::default().queue_family_index(queue_family),
                None,
            )
            .expect("failed to create a Vulkan command pool");
        let buffers = device
            .allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(2),
            )
            .expect("failed to allocate Vulkan command buffers");
        FrameSlot {
            pool,
            upload: buffers[0],
            draw: buffers[1],
            fence: device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .expect("failed to create a Vulkan fence"),
            acquire: device
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                .expect("failed to create a Vulkan semaphore"),
            submitted: false,
            epoch: Epoch::START,
            staging: Staging::default(),
            program_pools: Vec::new(),
            program_pool: 0,
            clocking: false,
            timed: None,
        }
    }
}

/// A render pass for `format` with a clearing or loading color attachment.
///
/// # Safety
///
/// `device` is live.
unsafe fn create_render_pass(
    device: &ash::Device,
    format: vk::Format,
    clear: bool,
    depth: Option<vk::Format>,
) -> vk::RenderPass {
    let attachments = [
        vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(if clear {
                vk::AttachmentLoadOp::CLEAR
            } else {
                vk::AttachmentLoadOp::LOAD
            })
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL),
        // Transient: cleared on load, discarded on store.
        vk::AttachmentDescription::default()
            .format(depth.unwrap_or(vk::Format::D32_SFLOAT))
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
            .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL),
    ];
    let color_ref = [vk::AttachmentReference::default()
        .attachment(0)
        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let depth_ref = vk::AttachmentReference::default()
        .attachment(1)
        .layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
    let mut subpass = vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color_ref);
    if depth.is_some() {
        subpass = subpass.depth_stencil_attachment(&depth_ref);
    }
    let subpasses = [subpass];
    let info = vk::RenderPassCreateInfo::default()
        .attachments(&attachments[..1 + usize::from(depth.is_some())])
        .subpasses(&subpasses);
    // SAFETY: `info` describes a color attachment, and a depth one if named,
    // used by one subpass; the layouts around them are established by explicit
    // barriers.
    unsafe { device.create_render_pass(&info, None) }
        .expect("failed to create a Vulkan render pass")
}

/// The programmable and fixed-function state of a graphics pipeline.
struct Stages<'a> {
    vertex_entry: &'a str,
    fragment_entry: &'a str,
    vertex_input: &'a vk::PipelineVertexInputStateCreateInfo<'a>,
    blend: BlendMode,
    depth: Option<DepthState>,
    cull: CullMode,
}

/// A triangle-list graphics pipeline over `module`'s two entries, drawing as
/// `stages` says through `layout`, built against `render_pass` (and usable
/// with any pass compatible with it).
///
/// # Safety
///
/// Every handle is a live object of `device`, the render pass has a depth
/// attachment exactly when `stages.depth` is set, and the entries read only
/// what `layout` and `stages.vertex_input` declare.
unsafe fn graphics_pipeline(
    device: &ash::Device,
    cache: vk::PipelineCache,
    render_pass: vk::RenderPass,
    module: vk::ShaderModule,
    stages: Stages<'_>,
    layout: vk::PipelineLayout,
) -> Result<vk::Pipeline, vk::Result> {
    let Stages {
        vertex_entry,
        fragment_entry,
        vertex_input,
        blend,
        depth,
        cull,
    } = stages;
    let vertex_entry = CString::new(vertex_entry).expect("entry name");
    let fragment_entry = CString::new(fragment_entry).expect("entry name");
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(module)
            .name(&vertex_entry),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(module)
            .name(&fragment_entry),
    ];
    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(match cull {
            CullMode::None => vk::CullModeFlags::NONE,
            CullMode::Back => vk::CullModeFlags::BACK,
            CullMode::Front => vk::CullModeFlags::FRONT,
        })
        // The vertex stage flips y for Vulkan's downward framebuffer, so a
        // face counter-clockwise in clip space is counter-clockwise on screen.
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let depth_stencil = depth.map(|d| {
        vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(d.write)
            .depth_compare_op(compare_op(d.compare))
    });
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let blend_attachment = [match blend {
        BlendMode::Replace => vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA),
        BlendMode::PremultipliedOver => vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA),
    }];
    let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(vertex_input)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(layout)
        .render_pass(render_pass)
        .subpass(0);
    let info = match &depth_stencil {
        Some(d) => info.depth_stencil_state(d),
        None => info,
    };
    // SAFETY: every state block borrows live locals; the caller vouches for
    // the handles.
    unsafe {
        device
            .create_graphics_pipelines(cache, &[info], None)
            .map(|p| p[0])
            .map_err(|(_, e)| e)
    }
}

/// Builds user programs on any thread: object creation on a Vulkan device is
/// free-threaded, and the pipeline cache synchronizes itself.
#[derive(Clone)]
pub struct VulkanCompiler {
    device: ash::Device,
    cache: vk::PipelineCache,
}

/// A user program built by [`VulkanCompiler`], destroyed with its layouts if
/// it is dropped before it is installed.
pub struct VulkanProgram {
    device: ash::Device,
    built: Option<(vk::Pipeline, ProgramLayout)>,
}

impl Drop for VulkanProgram {
    fn drop(&mut self) {
        if let Some((pipeline, program)) = self.built.take() {
            // SAFETY: an uninstalled program was never recorded into a command.
            unsafe {
                destroy_pipeline(
                    &self.device,
                    VulkanPipeline {
                        pipeline,
                        program: Some(program),
                    },
                );
            }
        }
    }
}

impl ProgramCompiler for VulkanCompiler {
    type Program = VulkanProgram;

    /// Builds the program's SPIR-V, laid out as the program ABI (see the
    /// module docs), against a render pass of its own for `color_format` that
    /// is destroyed once the pipeline exists: any pass of that format is
    /// compatible with it.
    fn compile(&self, desc: &ProgramDesc<'_>) -> Result<VulkanProgram, String> {
        let label = desc.label;
        let ProgramCode::SpirV(spirv) = desc.code else {
            return Err(format!(
                "{label}: Vulkan builds SPIR-V, not {:?}",
                desc.code.lang()
            ));
        };
        desc.check_abi()?;
        let words = ash::util::read_spv(&mut Cursor::new(spirv))
            .map_err(|e| format!("{label}: malformed SPIR-V: {e}"))?;
        let (textures, samplers) = (desc.abi.textures, desc.abi.samplers);
        let stages = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT;
        let buffers = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .descriptor_count(1)
                .stage_flags(stages),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(stages),
        ];
        let images: Vec<vk::DescriptorSetLayoutBinding<'_>> = (0..textures)
            .map(|i| (i, vk::DescriptorType::SAMPLED_IMAGE))
            .chain((0..samplers).map(|i| (PROGRAM_TEXTURES + i, vk::DescriptorType::SAMPLER)))
            .map(|(binding, ty)| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(binding)
                    .descriptor_type(ty)
                    .descriptor_count(1)
                    .stage_flags(stages)
            })
            .collect();
        let device = &self.device;
        // SAFETY: plain object creation on a live device; every object made
        // here is destroyed on each failure path, or owned by the program.
        unsafe {
            let module = device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
                .map_err(|e| format!("{label}: the shader module was refused: {e}"))?;
            let set_layout = |bindings: &[vk::DescriptorSetLayoutBinding<'_>]| {
                device
                    .create_descriptor_set_layout(
                        &vk::DescriptorSetLayoutCreateInfo::default().bindings(bindings),
                        None,
                    )
                    .expect("failed to create a program descriptor set layout")
            };
            let sets = [set_layout(&buffers), set_layout(&images)];
            let layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&sets),
                    None,
                )
                .expect("failed to create a program pipeline layout");
            let pass = create_render_pass(device, vk_format(desc.color_format), true, None);
            let result = graphics_pipeline(
                device,
                self.cache,
                pass,
                module,
                Stages {
                    vertex_entry: desc.vertex_entry,
                    fragment_entry: desc.fragment_entry,
                    vertex_input: &vk::PipelineVertexInputStateCreateInfo::default(),
                    blend: desc.blend,
                    depth: None,
                    cull: CullMode::None,
                },
                layout,
            );
            device.destroy_render_pass(pass, None);
            device.destroy_shader_module(module, None);
            let pipeline = match result {
                Ok(pipeline) => pipeline,
                Err(e) => {
                    device.destroy_pipeline_layout(layout, None);
                    for set in sets {
                        device.destroy_descriptor_set_layout(set, None);
                    }
                    return Err(format!("{label}: the pipeline was refused: {e}"));
                }
            };
            Ok(VulkanProgram {
                device: device.clone(),
                built: Some((
                    pipeline,
                    ProgramLayout {
                        layout,
                        sets,
                        textures,
                        samplers,
                        uniform_size: desc.abi.uniform_size,
                    },
                )),
            })
        }
    }
}

/// Destroy a pipeline and, for a program, the layout it owns.
///
/// # Safety
///
/// Nothing pending or recorded may use the pipeline.
unsafe fn destroy_pipeline(device: &ash::Device, p: VulkanPipeline) {
    // SAFETY: the caller guarantees the pipeline is unused; its layout and set
    // layouts belong to it alone.
    unsafe {
        device.destroy_pipeline(p.pipeline, None);
        if let Some(program) = p.program {
            device.destroy_pipeline_layout(program.layout, None);
            for set in program.sets {
                device.destroy_descriptor_set_layout(set, None);
            }
        }
    }
}

/// Destroy a buffer and its memory.
///
/// # Safety
/// No pending or recorded command may reference the buffer.
unsafe fn destroy_buffer(device: &ash::Device, b: VulkanBuffer) {
    // SAFETY: the caller guarantees the buffer is unused; freeing the memory
    // also unmaps it.
    unsafe {
        device.destroy_buffer(b.buffer, None);
        device.free_memory(b.memory, None);
    }
}

/// Destroy a texture's framebuffer, view, image and memory.
///
/// # Safety
/// No pending or recorded command may reference the texture.
unsafe fn destroy_texture(device: &ash::Device, t: VulkanTexture) {
    // SAFETY: the caller guarantees the texture is unused; children go first.
    unsafe {
        if t.framebuffer != vk::Framebuffer::null() {
            device.destroy_framebuffer(t.framebuffer, None);
        }
        if let Some((_, fb)) = t.depth_framebuffer {
            device.destroy_framebuffer(fb, None);
        }
        if let Some(c) = t.converted {
            device.destroy_framebuffer(c.framebuffer, None);
            device.destroy_image_view(c.view, None);
            if let Some(p) = c.planes {
                planes::destroy_planes(device, p);
            }
        }
        device.destroy_image_view(t.view, None);
        device.destroy_image(t.image, None);
        device.free_memory(t.memory, None);
    }
}

/// The access mask and pipeline stage that use an image in `layout`, as the
/// source (`src`) or destination side of a barrier.
fn layout_scope(layout: vk::ImageLayout, src: bool) -> (vk::AccessFlags, vk::PipelineStageFlags) {
    match layout {
        vk::ImageLayout::UNDEFINED => (
            vk::AccessFlags::empty(),
            // Chains with the acquire semaphore, which waits at this stage.
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        ),
        vk::ImageLayout::PRESENT_SRC_KHR => (
            vk::AccessFlags::empty(),
            if src {
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
            } else {
                vk::PipelineStageFlags::BOTTOM_OF_PIPE
            },
        ),
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
            vk::AccessFlags::SHADER_READ,
            vk::PipelineStageFlags::VERTEX_SHADER | vk::PipelineStageFlags::FRAGMENT_SHADER,
        ),
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
            vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        ),
        vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL => (
            vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
        ),
        vk::ImageLayout::TRANSFER_DST_OPTIMAL => (
            vk::AccessFlags::TRANSFER_WRITE,
            vk::PipelineStageFlags::TRANSFER,
        ),
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL => (
            vk::AccessFlags::TRANSFER_READ,
            vk::PipelineStageFlags::TRANSFER,
        ),
        other => panic!("no barrier scope for image layout {other:?}"),
    }
}

/// Record a layout transition of a whole single-mip image.
///
/// # Safety
/// `cmd` must be recording outside a render pass, and `image` must be a live
/// image of the device currently in `old` (or `old` is `UNDEFINED`).
unsafe fn transition(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
) {
    let (src_access, src_stage) = layout_scope(old, true);
    let (dst_access, dst_stage) = layout_scope(new, false);
    let barrier = vk::ImageMemoryBarrier::default()
        .src_access_mask(src_access)
        .dst_access_mask(dst_access)
        .old_layout(old)
        .new_layout(new)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(subresource_range(aspect));
    // SAFETY: guaranteed by the caller.
    unsafe {
        device.cmd_pipeline_barrier(
            cmd,
            src_stage,
            dst_stage,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
    }
}

/// The bytes of `rows` rows of `width` bytes each, `stride` apart, from the
/// top of `bytes`; `None` where they do not fit.
fn rows_of(bytes: &[u8], stride: usize, width: usize, rows: usize) -> Option<&[u8]> {
    if stride < width {
        return None;
    }
    let len = stride
        .checked_mul(rows.checked_sub(1)?)?
        .checked_add(width)?;
    bytes.get(..len)
}

fn subresource_range(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: 0,
        level_count: vk::REMAINING_MIP_LEVELS,
        base_array_layer: 0,
        layer_count: 1,
    }
}

fn subresource_layers(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: aspect,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    }
}

fn aspect_of(format: TextureFormat) -> vk::ImageAspectFlags {
    match format {
        TextureFormat::Depth32Float => vk::ImageAspectFlags::DEPTH,
        _ => vk::ImageAspectFlags::COLOR,
    }
}

/// Map a Viso [`TextureFormat`] to its Vulkan format.
fn vk_format(format: TextureFormat) -> vk::Format {
    match format {
        TextureFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        // Premultiplication is a content convention, not a storage property, so
        // a data plane and a color plane of the same width share one format.
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Data => vk::Format::R8G8B8A8_UNORM,
        TextureFormat::R8Unorm => vk::Format::R8_UNORM,
        // Vulkan names packed formats from the high bits down, as Viso stores
        // them.
        TextureFormat::Rgb565Unorm => vk::Format::R5G6B5_UNORM_PACK16,
        TextureFormat::Rgba4444Unorm => vk::Format::R4G4B4A4_UNORM_PACK16,
        TextureFormat::Rgba5551Unorm => vk::Format::R5G5B5A1_UNORM_PACK16,
        TextureFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        TextureFormat::Depth32Float => vk::Format::D32_SFLOAT,
        TextureFormat::Bc1RgbaUnorm => vk::Format::BC1_RGBA_UNORM_BLOCK,
        TextureFormat::Bc3RgbaUnorm => vk::Format::BC3_UNORM_BLOCK,
        TextureFormat::Bc7RgbaUnorm => vk::Format::BC7_UNORM_BLOCK,
        TextureFormat::Etc2Rgb8Unorm => vk::Format::ETC2_R8G8B8_UNORM_BLOCK,
        TextureFormat::Etc2Rgb8A1Unorm => vk::Format::ETC2_R8G8B8A1_UNORM_BLOCK,
        TextureFormat::Etc2Rgba8Unorm => vk::Format::ETC2_R8G8B8A8_UNORM_BLOCK,
        TextureFormat::Astc4x4Unorm => vk::Format::ASTC_4X4_UNORM_BLOCK,
        TextureFormat::Astc6x6Unorm => vk::Format::ASTC_6X6_UNORM_BLOCK,
        TextureFormat::Astc8x8Unorm => vk::Format::ASTC_8X8_UNORM_BLOCK,
        TextureFormat::Bgra8UnormSrgb => vk::Format::B8G8R8A8_SRGB,
        TextureFormat::Rgba8UnormSrgb => vk::Format::R8G8B8A8_SRGB,
        TextureFormat::Bc1RgbaUnormSrgb => vk::Format::BC1_RGBA_SRGB_BLOCK,
        TextureFormat::Bc3RgbaUnormSrgb => vk::Format::BC3_SRGB_BLOCK,
        TextureFormat::Bc7RgbaUnormSrgb => vk::Format::BC7_SRGB_BLOCK,
        TextureFormat::Etc2Rgb8UnormSrgb => vk::Format::ETC2_R8G8B8_SRGB_BLOCK,
        TextureFormat::Etc2Rgb8A1UnormSrgb => vk::Format::ETC2_R8G8B8A1_SRGB_BLOCK,
        TextureFormat::Etc2Rgba8UnormSrgb => vk::Format::ETC2_R8G8B8A8_SRGB_BLOCK,
        TextureFormat::Astc4x4UnormSrgb => vk::Format::ASTC_4X4_SRGB_BLOCK,
        TextureFormat::Astc6x6UnormSrgb => vk::Format::ASTC_6X6_SRGB_BLOCK,
        TextureFormat::Astc8x8UnormSrgb => vk::Format::ASTC_8X8_SRGB_BLOCK,
    }
}

/// The formats beyond the baseline the device samples: a block-compressed
/// family when its feature is enabled (`features`). None of the packed 16-bit
/// ones: they have no sRGB variant to decode a sample.
fn sampled_formats(features: &vk::PhysicalDeviceFeatures) -> SampledFormats {
    let mut sampled = SampledFormats::empty();
    for (enabled, class) in [
        (features.texture_compression_bc, SampledFormats::BC),
        (features.texture_compression_etc2, SampledFormats::ETC2),
        (features.texture_compression_astc_ldr, SampledFormats::ASTC),
    ] {
        if enabled == vk::TRUE {
            sampled |= class;
        }
    }
    sampled
}

/// Map an attribute format to its Vulkan vertex format.
fn compare_op(f: CompareFunction) -> vk::CompareOp {
    match f {
        CompareFunction::Always => vk::CompareOp::ALWAYS,
        CompareFunction::Less => vk::CompareOp::LESS,
        CompareFunction::LessEqual => vk::CompareOp::LESS_OR_EQUAL,
        CompareFunction::Greater => vk::CompareOp::GREATER,
        CompareFunction::GreaterEqual => vk::CompareOp::GREATER_OR_EQUAL,
    }
}

fn attr_format(format: AttrFormat) -> vk::Format {
    match format {
        AttrFormat::Float1 => vk::Format::R32_SFLOAT,
        AttrFormat::Float2 => vk::Format::R32G32_SFLOAT,
        AttrFormat::Float3 => vk::Format::R32G32B32_SFLOAT,
        AttrFormat::Float4 => vk::Format::R32G32B32A32_SFLOAT,
        AttrFormat::Uint1 => vk::Format::R32_UINT,
        AttrFormat::Uint2 => vk::Format::R32G32_UINT,
        AttrFormat::Uint4 => vk::Format::R32G32B32A32_UINT,
    }
}
