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

use core::ffi::{CStr, c_char, c_void};
use std::ffi::CString;
use std::io::Cursor;
use std::sync::atomic::{AtomicU32, Ordering};

use ash::vk;
use ash::{ext, khr};
use viso_handle::RawWindowHandle;

use crate::backend::{
    DrawCommand, DrawList, Frame, Geometry, GpuBackend, IndexFormat, InlineUniforms, LoadOp,
    RenderPass, RenderTarget,
};
use crate::instance::{AttrFormat, InstanceLayout};
use crate::resource::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BuiltinShader, Caps, FilterMode,
    PipelineDesc, SamplerDesc, ShaderCode, ShaderLang, TextureDesc, TextureFormat,
};
use crate::retire::{Epoch, Fence, ResourceKind, RetireQueue, Retired};
use crate::slots::SlotMap;
use crate::{BindGroupId, BufferId, PipelineId, SamplerId, SurfaceId, TextureId};

/// Frames the CPU may record ahead of the GPU.
const FRAMES_IN_FLIGHT: usize = 2;
/// The smallest staging chunk a frame slot allocates for texture uploads.
const STAGING_CHUNK: u64 = 4 << 20;
/// Descriptor sets per descriptor pool; a full pool is followed by a new one.
const SETS_PER_POOL: u32 = 256;
/// Descriptor bindings of set 0.
const BINDING_TEX: u32 = 0;
const BINDING_DST_TEX: u32 = 1;
const BINDING_SAMPLER: u32 = 2;
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
}

/// A registered pipeline.
struct VulkanPipeline {
    pipeline: vk::Pipeline,
}

/// A bind group: one descriptor set and the pool it came from.
struct VulkanBindGroup {
    set: vk::DescriptorSet,
    pool: vk::DescriptorPool,
}

/// One swapchain image with its view and framebuffer.
struct SwapImage {
    image: vk::Image,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    /// Whether the image has been presented since the swapchain was built — its
    /// contents are then in `PRESENT_SRC_KHR`, otherwise undefined.
    presented: bool,
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
    set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline_cache: vk::PipelineCache,
    render_passes: Vec<RenderPassEntry>,
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
    /// Panics if there is no Vulkan loader or no device with a graphics queue.
    pub fn new() -> Self {
        Self::try_new().expect("no usable Vulkan device")
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
        // SAFETY: loading the system Vulkan loader runs its initialisers, which
        // is the documented way to reach Vulkan; nothing else is loaded.
        let entry = unsafe { ash::Entry::load() }.ok()?;

        // SAFETY: `entry` is a loaded Vulkan entry; enumeration has no
        // preconditions.
        let extensions = unsafe { entry.enumerate_instance_extension_properties(None) }.ok()?;
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
        let validation = validation && has_ext(ext::debug_utils::NAME);
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
        let mut info = vk::InstanceCreateInfo::default()
            .flags(flags)
            .application_info(&app)
            .enabled_extension_names(&enabled);
        if validation {
            info = info.enabled_layer_names(&layer_names);
        }
        // SAFETY: every extension and layer name was enumerated as available and
        // is a 'static NUL-terminated string; `info` borrows live locals.
        let instance = unsafe { entry.create_instance(&info, None) }.ok()?;

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

        let Some((physical, queue_family)) = pick_physical_device(&instance) else {
            // SAFETY: nothing was created from `instance` except the messenger,
            // which is destroyed first.
            unsafe {
                if let Some((loader, messenger)) = &debug {
                    loader.destroy_debug_utils_messenger(*messenger, None);
                }
                instance.destroy_instance(None);
            }
            return None;
        };

        // SAFETY: `physical` was enumerated from `instance`.
        let device_exts =
            unsafe { instance.enumerate_device_extension_properties(physical) }.ok()?;
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
        let priorities = [1.0f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&device_enabled);
        // SAFETY: `queue_family` has a graphics queue on `physical`, and every
        // extension was enumerated as supported by it.
        let device = unsafe { instance.create_device(physical, &device_info, None) }.ok()?;
        // SAFETY: queue 0 of `queue_family` was requested above.
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        // SAFETY: `physical` belongs to `instance`.
        let (memory_props, limits) = unsafe {
            (
                instance.get_physical_device_memory_properties(physical),
                instance.get_physical_device_properties(physical).limits,
            )
        };

        let surface_fn = has_surface.then(|| khr::surface::Instance::new(&entry, &instance));
        let swapchain_fn = has_swapchain.then(|| khr::swapchain::Device::new(&instance, &device));

        let (set_layout, pipeline_layout, pipeline_cache) = create_layouts(&device);
        let slots = std::array::from_fn(|_| create_frame_slot(&device, queue_family));

        Some(Self {
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
            set_layout,
            pipeline_layout,
            pipeline_cache,
            render_passes: Vec::new(),
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
            },
            retire_queue: RetireQueue::new(),
            current_epoch: Epoch::START,
            reclaim_scratch: Vec::new(),
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
        let size = width as u64 * height as u64 * tex.format.bytes_per_texel() as u64;
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

    /// The render pass for `format` with a clearing or loading color attachment.
    /// Passes differing only in load op are compatible, so pipelines and
    /// framebuffers are built against the clearing one.
    fn render_pass(&mut self, format: vk::Format, clear: bool) -> vk::RenderPass {
        if let Some(e) = self
            .render_passes
            .iter()
            .find(|e| e.format == format && e.clear == clear)
        {
            return e.pass;
        }
        let attachment = [vk::AttachmentDescription::default()
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
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let color_ref = [vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let subpass = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref)];
        let info = vk::RenderPassCreateInfo::default()
            .attachments(&attachment)
            .subpasses(&subpass);
        // SAFETY: `info` describes one color attachment used by one subpass; the
        // layouts around it are established by explicit barriers.
        let pass = unsafe { self.device.create_render_pass(&info, None) }
            .expect("failed to create a Vulkan render pass");
        self.render_passes.push(RenderPassEntry {
            format,
            clear,
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
            self.device
                .reset_command_pool(slot.pool, vk::CommandPoolResetFlags::empty())
                .expect("reset a frame command pool");
            self.device
                .begin_command_buffer(slot.upload, &begin)
                .expect("begin the upload command buffer");
            self.device
                .begin_command_buffer(slot.draw, &begin)
                .expect("begin the draw command buffer");
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
        unsafe {
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
        let fence = Fence::at(self.completed_epoch());
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
                            self.device.destroy_pipeline(p.pipeline, None);
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
        let size = bytes.len() as u64;
        loop {
            let staging = &mut self.slots[self.slot].staging;
            if let Some(chunk) = staging.chunks.get(staging.current) {
                // Texel-copy offsets must be a multiple of 4 and of the texel
                // size; 16 covers every format.
                let offset = staging.cursor.next_multiple_of(16);
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
            let (buffer, memory, ptr) =
                self.create_host_buffer(chunk_size, vk::BufferUsageFlags::TRANSFER_SRC);
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
        let sizes = [
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
            .clipped(true)
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
        let pass = self.render_pass(vk_format, true);
        let mut built = Vec::with_capacity(images.len());
        for image in images {
            let view = self.create_view(image, vk_format, vk::ImageAspectFlags::COLOR);
            let framebuffer = self.create_framebuffer(pass, view, extent.width, extent.height);
            built.push(SwapImage {
                image,
                view,
                framebuffer,
                presented: false,
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
        // SAFETY: `image` is a live single-mip, single-layer 2D image of `format`.
        unsafe { self.device.create_image_view(&info, None) }
            .expect("failed to create a Vulkan image view")
    }

    fn create_framebuffer(
        &self,
        pass: vk::RenderPass,
        view: vk::ImageView,
        width: u32,
        height: u32,
    ) -> vk::Framebuffer {
        let attachments = [view];
        let info = vk::FramebufferCreateInfo::default()
            .render_pass(pass)
            .attachments(&attachments)
            .width(width)
            .height(height)
            .layers(1);
        // SAFETY: `view` is a color-attachment view of a `width`×`height` image
        // whose format matches `pass`'s attachment.
        unsafe { self.device.create_framebuffer(&info, None) }
            .expect("failed to create a Vulkan framebuffer")
    }

    /// Record one render pass into the draw command buffer.
    fn record_pass(&mut self, pass: &RenderPass, commands: &[DrawCommand]) {
        let cmd = self.slots[self.slot].draw;
        let clear = matches!(pass.load, LoadOp::Clear(_));
        let (image, aspect, framebuffer, extent, format, texture) = match pass.target {
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
                let fb = if fb == vk::Framebuffer::null() {
                    let rp = self.render_pass(fmt, true);
                    let fb = self.create_framebuffer(rp, view, w, h);
                    self.textures
                        .get_mut(id.into())
                        .expect("texture handle")
                        .framebuffer = fb;
                    fb
                } else {
                    fb
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
        let render_pass = self.render_pass(format, clear);
        let clear_values = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: match pass.load {
                    LoadOp::Clear(c) => c,
                    LoadOp::Load => [0.0; 4],
                },
            },
        }];
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        };
        let begin = vk::RenderPassBeginInfo::default()
            .render_pass(render_pass)
            .framebuffer(framebuffer)
            .render_area(area)
            .clear_values(&clear_values);
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
        let mut bound_pipeline = None;
        let mut bound_group = None;
        for c in commands {
            self.record_command(cmd, c, extent, &mut bound_pipeline, &mut bound_group);
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

    /// Record one draw inside the open render pass.
    fn record_command(
        &self,
        cmd: vk::CommandBuffer,
        c: &DrawCommand,
        extent: vk::Extent2D,
        bound_pipeline: &mut Option<PipelineId>,
        bound_group: &mut Option<BindGroupId>,
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
            if *bound_pipeline != Some(c.pipeline) {
                let p = self
                    .pipelines
                    .get(c.pipeline.into())
                    .expect("pipeline handle does not resolve");
                self.device
                    .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, p.pipeline);
                *bound_pipeline = Some(c.pipeline);
            }
            if let Some(bg) = c.bind_group
                && *bound_group != Some(bg)
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
                *bound_group = Some(bg);
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
            }
        }
    }
}

impl GpuBackend for VulkanBackend {
    const SHADER_LANG: ShaderLang = ShaderLang::SpirV;

    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        let len = desc.size.max(1);
        let (buffer, memory, ptr) = self.create_host_buffer(
            len as u64,
            vk::BufferUsageFlags::VERTEX_BUFFER
                | vk::BufferUsageFlags::INDEX_BUFFER
                | vk::BufferUsageFlags::UNIFORM_BUFFER
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
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: desc.width.max(1),
                height: desc.height.max(1),
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: `info` describes a single-mip 2D image with a positive extent;
        // its memory comes from a type its requirements allow and is bound once.
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
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
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
            })
            .into()
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
        let vertex_entry = CString::new(desc.vertex_entry).expect("entry name");
        let fragment_entry = CString::new(desc.fragment_entry).expect("entry name");
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

        let input_rate = match desc.builtin {
            BuiltinShader::Path | BuiltinShader::Mesh => vk::VertexInputRate::VERTEX,
            _ => vk::VertexInputRate::INSTANCE,
        };
        let mut attributes = Vec::with_capacity(desc.instance_schema.attributes.len());
        let mut offset = 0u32;
        for (location, a) in desc.instance_schema.attributes.iter().enumerate() {
            attributes.push(vk::VertexInputAttributeDescription {
                location: location as u32,
                binding: 0,
                format: attr_format(a.format),
                offset,
            });
            offset += a.format.size() as u32;
        }
        let bindings = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: layout.stride as u32,
            input_rate,
        }];
        let vertex_input = if attributes.is_empty() {
            vk::PipelineVertexInputStateCreateInfo::default()
        } else {
            vk::PipelineVertexInputStateCreateInfo::default()
                .vertex_binding_descriptions(&bindings)
                .vertex_attribute_descriptions(&attributes)
        };
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let blend_attachment = [match desc.blend {
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
        let render_pass = self.render_pass(vk_format(desc.color_format), true);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(self.pipeline_layout)
            .render_pass(render_pass)
            .subpass(0);
        // SAFETY: every state block borrows live locals; the module's entry
        // points read the attributes, push constants and set-0 bindings the
        // shared layout declares. The module is no longer needed once the
        // pipeline exists.
        let pipeline = unsafe {
            let result = self
                .device
                .create_graphics_pipelines(self.pipeline_cache, &[info], None);
            self.device.destroy_shader_module(module, None);
            result
                .map_err(|(_, e)| e)
                .expect("failed to create a Vulkan pipeline")[0]
        };
        Ok(self.pipelines.insert(VulkanPipeline { pipeline }).into())
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        let (set, pool) = self.allocate_set();
        let mut views = [vk::ImageView::null(); 2];
        let mut texture_count = 0;
        let mut sampler = None;
        for binding in &desc.bindings {
            match *binding {
                Binding::Texture(id) => {
                    if texture_count < views.len() {
                        views[texture_count] = self.texture(id).view;
                        texture_count += 1;
                    }
                }
                Binding::Sampler(id) => {
                    sampler = Some(
                        *self
                            .samplers
                            .get(id.into())
                            .expect("sampler handle does not resolve"),
                    );
                }
                // The built-ins' uniforms are push constants.
                Binding::Uniform(_) => {}
            }
        }
        // A single-texture program leaves `dst_tex` unread; fill it anyway so the
        // set is complete for any program it is bound with.
        if texture_count == 1 {
            views[1] = views[0];
        }
        let image_infos: Vec<[vk::DescriptorImageInfo; 1]> = if texture_count == 0 {
            Vec::new()
        } else {
            views
                .iter()
                .map(|&view| {
                    [vk::DescriptorImageInfo::default()
                        .image_view(view)
                        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]
                })
                .collect()
        };
        let sampler_info = sampler.map(|s| [vk::DescriptorImageInfo::default().sampler(s)]);
        let mut writes = Vec::with_capacity(3);
        for (i, info) in image_infos.iter().enumerate() {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(if i == 0 { BINDING_TEX } else { BINDING_DST_TEX })
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(info),
            );
        }
        if let Some(info) = &sampler_info {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(BINDING_SAMPLER)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .image_info(info),
            );
        }
        // SAFETY: `set` was just allocated with the shared layout; every write
        // targets a binding of the matching type with live views and samplers.
        unsafe { self.device.update_descriptor_sets(&writes, &[]) };
        self.bind_groups
            .insert(VulkanBindGroup { set, pool })
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
        let bytes_per_row = w as usize * t.format.bytes_per_texel();
        let len = bytes_per_row * h as usize;
        assert!(
            bytes.len() >= len,
            "write_texture: {} bytes < {}x{} region of {}-byte texels",
            bytes.len(),
            w,
            h,
            t.format.bytes_per_texel()
        );
        if len == 0 {
            return;
        }
        let (image, aspect) = (t.image, aspect_of(t.format));
        self.ensure_recording();
        let (buffer, offset) = self.stage(&bytes[..len]);
        let cmd = self.slots[self.slot].upload;
        let region = vk::BufferImageCopy::default()
            .buffer_offset(offset)
            .image_subresource(subresource_layers(aspect))
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
        let (format, chosen) = match pick(vk::Format::B8G8R8A8_UNORM) {
            Some(s) => (TextureFormat::Bgra8Unorm, s),
            None => match pick(vk::Format::R8G8B8A8_UNORM) {
                Some(s) => (TextureFormat::Rgba8Unorm, s),
                // A lone UNDEFINED entry means "any format".
                None => (
                    TextureFormat::Bgra8Unorm,
                    vk::SurfaceFormatKHR {
                        format: vk::Format::B8G8R8A8_UNORM,
                        color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
                    },
                ),
            },
        };
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
        let presented = self.surface(surface).images[image as usize].presented;
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
        self.surface_mut(frame.surface).images[acq.image as usize].presented = true;
        self.submit(Some(acquire), Some(finished));
        let wait = [finished];
        let swapchains = [swapchain];
        let indices = [acq.image];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait)
            .swapchains(&swapchains)
            .image_indices(&indices);
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
}

impl Drop for VulkanBackend {
    fn drop(&mut self) {
        // SAFETY: after the device idles nothing references any object below;
        // each is destroyed exactly once, children before their parents, and the
        // messenger before the counter it points at.
        unsafe {
            let _ = self.device.device_wait_idle();
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
                self.device.destroy_pipeline(p.pipeline, None);
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
                self.device.destroy_command_pool(slot.pool, None);
                self.device.destroy_fence(slot.fence, None);
                self.device.destroy_semaphore(slot.acquire, None);
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
fn pick_physical_device(instance: &ash::Instance) -> Option<(vk::PhysicalDevice, u32)> {
    // SAFETY: `instance` is live; the queries have no other preconditions.
    let devices = unsafe { instance.enumerate_physical_devices() }.ok()?;
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
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(BINDING_TEX)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .descriptor_count(1)
            .stage_flags(stages),
        vk::DescriptorSetLayoutBinding::default()
            .binding(BINDING_DST_TEX)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .descriptor_count(1)
            .stage_flags(stages),
        vk::DescriptorSetLayoutBinding::default()
            .binding(BINDING_SAMPLER)
            .descriptor_type(vk::DescriptorType::SAMPLER)
            .descriptor_count(1)
            .stage_flags(stages),
    ];
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

fn subresource_range(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: 0,
        level_count: 1,
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
        TextureFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        TextureFormat::Depth32Float => vk::Format::D32_SFLOAT,
    }
}

/// Map an attribute format to its Vulkan vertex format.
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
