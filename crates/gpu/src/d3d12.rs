//! `D3D12Backend` — the Direct3D 12 implementation of [`GpuBackend`], the
//! Windows path.
//!
//! ## Design decisions
//!
//! - **Programs are HLSL (shader model 5.1)** ([`ShaderCode::Hlsl`]), compiled
//!   once per stage with `D3DCompile` at pipeline creation and cached by source,
//!   so pipelines that share a program (blend and format variants) compile it
//!   once. Attributes carry the semantic `ATTR<i>`; the uniforms are
//!   `ConstantBuffer<Uniforms>` at `b0`; `tex`/`dst_tex` sit at `t0`/`t1` and
//!   the sampler at `s0`. D3D's clip space, viewport and texture rows are all
//!   top-left like Metal's, so no coordinate is flipped.
//! - **One root signature** serves every pipeline: [`InlineUniforms::MAX`] bytes
//!   of root constants at `b0`, a two-SRV descriptor table at `t0` and a
//!   one-sampler table at `s0` — a draw's uniforms are one
//!   `SetGraphicsRoot32BitConstants`, and a bind group is two table offsets.
//! - **A user program has its own root signature**, built with its pipeline
//!   state by [`D3D12Compiler`] on any thread: the same three parameters (the
//!   uniforms as a root constant-buffer view when the block outgrows the root
//!   constants) and a root view of the instance array at `t0, space1`, set at
//!   the draw's instance offset — the structured buffer replaces the input
//!   layout. Drawing one switches the root signature and rebinds the tables.
//! - **One shader-visible heap per descriptor type** for the device's lifetime.
//!   A bind group owns one [`PROGRAM_TEXTURES`]-descriptor unit of the SRV heap
//!   (its last texture repeated to fill it), and a sampler owns one slot of the
//!   sampler heap; a group binding two samplers also holds a pair of adjacent
//!   slots, shared by every group naming the same two. Unit 0 holds null SRVs
//!   and slots 0–1 the default sampler, bound when a draw has no bind group.
//!   The heaps are set once per command list, never switched.
//! - **Buffers are persistently mapped**: in the GPU-upload heap (device-local,
//!   CPU-visible through resizable BAR) where the device supports it, otherwise
//!   the upload heap — `write_buffer` is a memcpy exactly as on Metal.
//! - **Textures are default-heap committed resources** that rest in
//!   `ALL_SHADER_RESOURCE` between commands; every operation needing another
//!   state (upload, render, read-back) transitions away and back inside the
//!   command list it records. Committed resources start zeroed; render targets
//!   are cleared explicitly once.
//! - **Two frame slots**, each with an *upload* and a *draw* allocator and
//!   command list and a staging arena. Texture uploads record into the upload
//!   list and passes into the draw list; both execute together (uploads first)
//!   at present, or at the end of an `encode` that ran with no frame open. One
//!   queue fence stamps every submission; a slot is reopened only after its
//!   value completes, which bounds the CPU to two frames ahead of the GPU.
//! - **Completion is read from the fence**: the retire queue is drained up to
//!   the oldest epoch any unfinished submission — or the open recording — still
//!   holds.
//! - **Swapchains are flip-discard** with three buffers, a maximum frame latency
//!   of one and its waitable object, waited on in `begin_frame` so input is
//!   sampled as late as the display allows. A resize rebuilds the buffers lazily
//!   at the next `begin_frame`.

use core::ffi::{CStr, c_void};
use core::mem::ManuallyDrop;
use std::cell::Cell;
use std::ffi::CString;

use std::sync::Arc;

use viso_handle::{ExternalHold, ExternalHolds, ExternalImage, RawWindowHandle};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, RECT, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, WaitForSingleObject, WaitForSingleObjectEx,
};
use windows::core::{Interface, PCSTR, PCWSTR};

use crate::backend::{
    DrawCommand, DrawList, Frame, Geometry, GpuBackend, IndexFormat, InlineUniforms, LoadOp,
    RenderPass, RenderTarget,
};
use crate::instance::{AttrFormat, InstanceLayout, InstanceSchema};
use crate::program::{
    PROGRAM_SAMPLERS, PROGRAM_TEXTURES, ProgramCode, ProgramCompiler, ProgramDesc,
};
use crate::resource::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BuiltinShader, Caps,
    CompareFunction, CullMode, DepthState, FilterMode, PipelineDesc, SampledFormats, SamplerDesc,
    ShaderCode, ShaderLang, TextureDesc, TextureFormat,
};
use crate::retire::{Epoch, Fence, ResourceKind, RetireQueue, Retired};
use crate::scene::{self, SceneInstance};
use crate::slots::SlotMap;
use crate::{BindGroupId, BufferId, PipelineId, SamplerId, SurfaceId, TextureId};

/// Frames the CPU may record ahead of the GPU.
const FRAMES_IN_FLIGHT: usize = 2;
/// The smallest staging chunk a frame slot allocates for texture uploads.
const STAGING_CHUNK: u64 = 4 << 20;
/// Swapchain buffers: one on screen, one queued, one being drawn.
const SWAP_BUFFERS: u32 = 3;
/// Descriptor units in the shader-visible SRV heap (unit 0 is all null views).
const SRV_UNITS: u32 = 32768;
/// Views one unit holds: a bind group's textures, the last repeated to fill it.
const SRV_UNIT: u32 = PROGRAM_TEXTURES;
/// Slots in the shader-visible sampler heap (slot 0 is the default sampler);
/// 2048 is the D3D12 limit.
const SAMPLER_SLOTS: u32 = 2048;
/// Render-target views per CPU-only RTV heap; a full heap is followed by another.
const RTV_BLOCK: u32 = 256;
/// Root parameter indices; a user program's root signature appends the
/// instance array's view.
const ROOT_UNIFORMS: u32 = 0;
const ROOT_TEXTURES: u32 = 1;
const ROOT_SAMPLER: u32 = 2;
const ROOT_INSTANCES: u32 = 3;
/// Textures and samplers the built-ins' tables read.
const BUILTIN_TEXTURES: u32 = 2;
const BUILTIN_SAMPLERS: u32 = 1;
/// Compile targets and the attribute semantic of the HLSL the shader crate emits.
const VERTEX_PROFILE: &CStr = c"vs_5_1";
const PIXEL_PROFILE: &CStr = c"ps_5_1";
const ATTR_SEMANTIC: &CStr = c"ATTR";
/// Placement alignment of a texture copy's footprint in a buffer.
const PLACEMENT_ALIGN: u64 = D3D12_TEXTURE_DATA_PLACEMENT_ALIGNMENT as u64;
/// Row-pitch alignment of a texture copy's footprint in a buffer.
const PITCH_ALIGN: u32 = D3D12_TEXTURE_DATA_PITCH_ALIGNMENT;
/// The state every texture rests in between commands.
const TEXTURE_REST: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_ALL_SHADER_RESOURCE;
/// The swapchain's format and flags (kept identical across `ResizeBuffers`).
/// A flip-model swapchain takes no sRGB format; its buffers' views are
/// [`SURFACE_FORMAT`], which encodes what the pipelines write.
const SWAP_FORMAT: DXGI_FORMAT = DXGI_FORMAT_B8G8R8A8_UNORM;
/// The format a surface reports and its buffers' render-target views take.
const SURFACE_FORMAT: TextureFormat = TextureFormat::Bgra8UnormSrgb;
const SWAP_FLAGS: DXGI_SWAP_CHAIN_FLAG = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
/// How long `begin_frame` waits for the swapchain before skipping the frame.
const FRAME_WAIT_MS: u32 = 1000;
/// How many shared textures stay open: a few players' slots.
const OPENED_SHARED: usize = 16;

/// A buffer: a CPU-visible resource mapped for its lifetime.
struct D3D12Buffer {
    /// Owns the memory `ptr` maps; never read, released on drop.
    _resource: ID3D12Resource,
    /// The persistent mapping, `len` bytes.
    ptr: *mut u8,
    len: usize,
    gpu_va: u64,
}

/// A texture: a default-heap resource and, for render targets, its view: a
/// render-target view, or for a depth target a depth-stencil view.
struct D3D12Texture {
    resource: ID3D12Resource,
    format: TextureFormat,
    width: u32,
    height: u32,
    rtv: Option<D3D12_CPU_DESCRIPTOR_HANDLE>,
    dsv: Option<D3D12_CPU_DESCRIPTOR_HANDLE>,
    /// Made by `import_external`: written only by copies of external images.
    external: bool,
}

/// A shared texture another device writes, opened on this one, with the
/// fence it signals; named by the holds on it, which it keeps alive.
struct Opened {
    holds: Arc<ExternalHolds>,
    resource: ID3D12Resource,
    fence: ID3D12Fence,
}

/// A registered pipeline and the vertex stride its input layout reads.
struct D3D12Pipeline {
    pso: ID3D12PipelineState,
    stride: u32,
    /// The scene built-in, drawn under the scene root signature.
    scene: bool,
    /// A user program's own root signature; `None` for a built-in, drawn under
    /// the shared one.
    program: Option<ProgramRoot>,
}

/// The root signature a user program is drawn under.
struct ProgramRoot {
    signature: ID3D12RootSignature,
    /// Samplers the program reads: two read the bind group's sampler pair.
    samplers: u32,
    /// The uniforms are a root constant-buffer view on the bind group's uniform
    /// buffer rather than root constants.
    uniform_buffer: bool,
}

/// A sampler: its own heap slot and the description that filled it.
struct D3D12Sampler {
    slot: u32,
    desc: D3D12_SAMPLER_DESC,
}

/// Two samplers written side by side for a program's two-sampler table,
/// shared by every bind group naming the same pair.
struct SamplerPair {
    samplers: [SamplerId; 2],
    slot: u32,
    refs: u32,
}

/// A bind group: its SRV-heap unit and sampler-heap slots.
#[derive(Clone, Copy)]
struct D3D12BindGroup {
    /// 0 (the null pair) when the group binds no texture.
    srv_unit: u32,
    /// Owned by the group, returned to the free list when it is reclaimed.
    owns_unit: bool,
    /// The first sampler's own slot.
    sampler_slot: u32,
    /// The group's two samplers and the heap slots holding them side by side,
    /// for a program that reads both.
    sampler_pair: Option<([SamplerId; 2], u32)>,
    /// The uniform buffer a user program with a large uniform block reads.
    uniform: Option<BufferId>,
}

/// A window surface and its swapchain.
struct D3D12Surface {
    swapchain: IDXGISwapChain3,
    /// Signalled when the swapchain can accept another frame.
    waitable: HANDLE,
    /// A wait on `waitable` has been consumed and no present has followed.
    waited: bool,
    buffers: Vec<ID3D12Resource>,
    rtvs: [D3D12_CPU_DESCRIPTOR_HANDLE; SWAP_BUFFERS as usize],
    /// The size the platform last asked for, in physical pixels.
    width: u32,
    height: u32,
    /// The size the buffers were built at.
    extent: (u32, u32),
    /// The buffers must be rebuilt before the next frame.
    stale: bool,
    /// Presents so far, and the present count at which each buffer was last
    /// presented (`0`: never since it was built): a flip-sequential buffer
    /// keeps what it showed, so the frame drawn into it next is that many
    /// presents newer.
    presents: u64,
    presented: [u64; SWAP_BUFFERS as usize],
}

/// An upload-heap buffer texture uploads are copied out of.
struct StagingChunk {
    resource: ID3D12Resource,
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
    upload_alloc: ID3D12CommandAllocator,
    draw_alloc: ID3D12CommandAllocator,
    upload: ID3D12GraphicsCommandList,
    draw: ID3D12GraphicsCommandList,
    /// `[upload, draw]` as base command lists, for `ExecuteCommandLists`.
    lists: [Option<ID3D12CommandList>; 2],
    /// The fence value the slot's last submission signals (0: none yet).
    fence_value: u64,
    /// The oldest epoch the last submission's commands were recorded in.
    epoch: Epoch,
    staging: Staging,
    /// Whether the open recording stamped its start in the clock.
    clocking: bool,
    /// The frame epoch the last submission timed, until its time is read.
    timed: Option<u64>,
}

/// The queue's timestamp queries, two per frame slot — where the slot's
/// recording began and where its last pass ended — resolved into a
/// readback buffer the CPU reads once the slot's fence passes.
struct Clock {
    heap: ID3D12QueryHeap,
    readback: ID3D12Resource,
    /// `readback`, mapped for as long as it lives.
    stamps: *const u64,
    /// Ticks per second.
    frequency: u64,
    on: bool,
    /// Frames timed and read, waiting for the profiler: epoch and ns.
    read: Vec<(u64, u64)>,
}

impl Clock {
    /// The most read times kept for a profiler that stopped asking.
    const KEPT: usize = 64;

    /// Reads the time slot `index` took, if its finished submission timed a
    /// frame.
    fn read(&mut self, slot: &mut FrameSlot, index: usize) {
        let Some(epoch) = slot.timed.take() else {
            return;
        };
        // SAFETY: the slot's submission resolved its two stamps into
        // `readback` and has completed; the mapping covers every slot's pair.
        let (start, end) = unsafe {
            (
                self.stamps.add(2 * index).read_volatile(),
                self.stamps.add(2 * index + 1).read_volatile(),
            )
        };
        if end > start && self.read.len() < Self::KEPT {
            let nanos = u128::from(end - start) * 1_000_000_000 / u128::from(self.frequency);
            self.read.push((epoch, nanos as u64));
        }
    }
}

/// Which root signature a pipeline draws under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Root {
    /// The built-ins' shared one, uniforms as root constants.
    #[default]
    Shared,
    /// The scene built-in's, uniforms as a root constant-buffer view.
    Scene,
    /// A user program's own.
    Program(PipelineId),
}

/// What a pass's recording has bound so far.
#[derive(Default)]
struct Bound {
    pipeline: Option<PipelineId>,
    root: Root,
    /// The bind group and whether its sampler pair is bound.
    group: Option<(BindGroupId, bool)>,
}

/// The frame between `begin_frame` and `present`.
#[derive(Debug, Clone, Copy)]
struct Acquired {
    surface: SurfaceId,
    image: u32,
    /// Whether the draw list has moved the back buffer to `RENDER_TARGET`.
    in_rt: bool,
}

/// A compiled shader stage, cached by the program text it came from.
struct CompiledStage {
    source: *const u8,
    entry: *const u8,
    pixel: bool,
    blob: ID3DBlob,
}

/// A shader-visible descriptor heap with its handle arithmetic.
struct GpuHeap {
    heap: ID3D12DescriptorHeap,
    cpu: D3D12_CPU_DESCRIPTOR_HANDLE,
    gpu: D3D12_GPU_DESCRIPTOR_HANDLE,
    increment: u32,
}

impl GpuHeap {
    fn cpu(&self, index: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: self.cpu.ptr + index as usize * self.increment as usize,
        }
    }

    fn gpu(&self, index: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: self.gpu.ptr + index as u64 * self.increment as u64,
        }
    }
}

/// The Direct3D 12 backend.
pub struct D3D12Backend {
    factory: IDXGIFactory6,
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    info_queue: Option<ID3D12InfoQueue>,
    /// Errors the debug layer has reported, accumulated as they are drained.
    validation_errors: Cell<u32>,
    root_signature: ID3D12RootSignature,
    /// The scene built-in's root signature: the shared one with its uniforms
    /// a constant-buffer view on the bind group's uniform buffer.
    scene_root_signature: ID3D12RootSignature,
    srv_heap: GpuHeap,
    sampler_heap: GpuHeap,
    /// Both shader-visible heaps, as `SetDescriptorHeaps` takes them.
    heaps: [Option<ID3D12DescriptorHeap>; 2],
    srv_free: Vec<u32>,
    srv_next: u32,
    sampler_free: Vec<u32>,
    sampler_next: u32,
    rtv_heaps: Vec<ID3D12DescriptorHeap>,
    rtv_free: Vec<D3D12_CPU_DESCRIPTOR_HANDLE>,
    rtv_increment: u32,
    dsv_heaps: Vec<ID3D12DescriptorHeap>,
    dsv_free: Vec<D3D12_CPU_DESCRIPTOR_HANDLE>,
    dsv_increment: u32,
    /// Buffers live in the GPU-upload heap (resizable BAR) rather than the
    /// upload heap.
    gpu_upload_heap: bool,
    shader_cache: Vec<CompiledStage>,
    fence: ID3D12Fence,
    fence_event: HANDLE,
    /// The last value signalled on `fence`.
    fence_value: u64,
    slots: [FrameSlot; FRAMES_IN_FLIGHT],
    /// The slot being recorded or next to record.
    slot: usize,
    /// The epoch the open recording was begun in, if one is open.
    recording: Option<Epoch>,
    acquired: Option<Acquired>,
    buffers: SlotMap<D3D12Buffer>,
    textures: SlotMap<D3D12Texture>,
    /// Each sampler's slot in the sampler heap.
    samplers: SlotMap<D3D12Sampler>,
    sampler_pairs: Vec<SamplerPair>,
    /// Freed pair slots (the first of two adjacent slots).
    sampler_pair_free: Vec<u32>,
    bind_groups: SlotMap<D3D12BindGroup>,
    pipelines: SlotMap<D3D12Pipeline>,
    surfaces: SlotMap<D3D12Surface>,
    caps: Caps,
    retire_queue: RetireQueue,
    current_epoch: Epoch,
    reclaim_scratch: Vec<Retired>,
    /// The frame clock, on a queue that takes timestamps.
    clock: Option<Clock>,
    /// The shared textures opened lately, the latest last.
    opened: Vec<Opened>,
    /// Holds on shared textures, each let go once the epoch it was copied
    /// in completes; the texture and fence the copy reads are kept alive as
    /// long, opened or not.
    held: Vec<(Epoch, ExternalHold, ID3D12Resource, ID3D12Fence)>,
    /// The fence values the next submission waits for on the GPU: the
    /// pictures it copies are written by then.
    waits: Vec<(ID3D12Fence, u64)>,
}

impl Default for D3D12Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl D3D12Backend {
    /// Create the backend on the best available Direct3D 12 adapter.
    ///
    /// # Panics
    /// Panics if no adapter (not even WARP) supports Direct3D 12.
    pub fn new() -> Self {
        Self::try_new().expect("no usable Direct3D 12 device")
    }

    /// Create the backend, or `None` if Direct3D 12 is unavailable. Enables the
    /// debug layer when `VISO_D3D12_DEBUG` is set and the layer is installed.
    pub fn try_new() -> Option<Self> {
        Self::try_new_with(std::env::var_os("VISO_D3D12_DEBUG").is_some())
    }

    /// Create the backend, with the debug layer if `debug` and the layer is
    /// installed (the Graphics Tools optional feature). Errors it reports are
    /// counted by [`validation_errors`](Self::validation_errors).
    pub fn try_new_with(debug: bool) -> Option<Self> {
        let mut debug_active = false;
        if debug {
            let mut layer: Option<ID3D12Debug> = None;
            // SAFETY: querying the debug interface has no preconditions; enabling
            // it before the device is created is the documented order.
            unsafe {
                if D3D12GetDebugInterface(&mut layer).is_ok()
                    && let Some(layer) = &layer
                {
                    layer.EnableDebugLayer();
                    debug_active = true;
                }
            }
        }
        // SAFETY: factory creation has no preconditions; the debug flag needs
        // the debug DXGI runtime, so creation falls back without it.
        let factory: IDXGIFactory6 = unsafe {
            let flags = if debug_active {
                DXGI_CREATE_FACTORY_DEBUG
            } else {
                DXGI_CREATE_FACTORY_FLAGS(0)
            };
            CreateDXGIFactory2(flags)
                .or_else(|_| CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)))
                .ok()?
        };
        let device = create_device(&factory)?;

        let info_queue = if debug_active {
            device.cast::<ID3D12InfoQueue>().ok()
        } else {
            None
        };
        // SAFETY: plain object creation on a live device with descriptions that
        // borrow locals for the duration of each call.
        let (queue, fence, fence_event) = unsafe {
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    ..Default::default()
                })
                .ok()?;
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
            let event = CreateEventW(None, false, false, PCWSTR::null()).ok()?;
            (queue, fence, event)
        };
        let root_signature = create_root_signature(&device);
        let scene_root_signature =
            serialize_root_signature(&device, None, true).unwrap_or_else(|e| {
                panic!("failed to create the Direct3D 12 scene root signature: {e}")
            });
        let srv_heap = create_gpu_heap(
            &device,
            D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            SRV_UNITS * SRV_UNIT,
        );
        let sampler_heap =
            create_gpu_heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER, SAMPLER_SLOTS);
        let heaps = [Some(srv_heap.heap.clone()), Some(sampler_heap.heap.clone())];
        // SAFETY: the null SRVs and the default sampler pair are written into
        // the first unit and slots of heaps just created with room for them.
        unsafe {
            let null = null_srv_desc();
            for i in 0..SRV_UNIT {
                device.CreateShaderResourceView(None, Some(&null), srv_heap.cpu(i));
            }
            for i in 0..2 {
                device.CreateSampler(
                    &sampler_desc(&SamplerDesc::LINEAR_CLAMP),
                    sampler_heap.cpu(i),
                );
            }
        }
        let gpu_upload_heap = supports_gpu_upload_heap(&device);
        // SAFETY: querying a descriptor size has no preconditions.
        let (rtv_increment, dsv_increment) = unsafe {
            (
                device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV),
                device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_DSV),
            )
        };
        let slots = std::array::from_fn(|_| create_frame_slot(&device));
        let sampled = sampled_formats(&device);

        let mut backend = Self {
            factory,
            device,
            queue,
            info_queue,
            validation_errors: Cell::new(0),
            root_signature,
            scene_root_signature,
            srv_heap,
            sampler_heap,
            heaps,
            srv_free: Vec::new(),
            srv_next: 1,
            sampler_free: Vec::new(),
            sampler_next: 2,
            rtv_heaps: Vec::new(),
            rtv_free: Vec::new(),
            rtv_increment,
            dsv_heaps: Vec::new(),
            dsv_free: Vec::new(),
            dsv_increment,
            gpu_upload_heap,
            shader_cache: Vec::new(),
            fence,
            fence_event,
            fence_value: 0,
            slots,
            slot: 0,
            recording: None,
            acquired: None,
            buffers: SlotMap::new(),
            textures: SlotMap::new(),
            samplers: SlotMap::new(),
            sampler_pairs: Vec::new(),
            sampler_pair_free: Vec::new(),
            bind_groups: SlotMap::new(),
            pipelines: SlotMap::new(),
            surfaces: SlotMap::new(),
            caps: Caps {
                max_texture_size: D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION,
                presents_to_display: true,
                compute_dispatch: false,
                bindless_texture_slots: 0,
                indirect_draw: false,
                program_instance_align: 4,
                coverage_paths: false,
                encodes_packed: false,
                sampled,
            },
            retire_queue: RetireQueue::new(),
            current_epoch: Epoch::START,
            reclaim_scratch: Vec::new(),
            clock: None,
            opened: Vec::new(),
            held: Vec::new(),
            waits: Vec::new(),
        };
        backend.clock = backend.create_clock();
        Some(backend)
    }

    /// The frame clock, if the queue takes timestamps.
    fn create_clock(&self) -> Option<Clock> {
        // SAFETY: querying a live queue has no preconditions.
        let frequency = unsafe { self.queue.GetTimestampFrequency() }.ok()?;
        if frequency == 0 {
            return None;
        }
        let count = 2 * FRAMES_IN_FLIGHT as u32;
        let mut heap: Option<ID3D12QueryHeap> = None;
        // SAFETY: the description borrows a local for the call.
        unsafe {
            self.device
                .CreateQueryHeap(
                    &D3D12_QUERY_HEAP_DESC {
                        Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
                        Count: count,
                        NodeMask: 0,
                    },
                    &mut heap,
                )
                .ok()?;
        }
        let readback = self.create_buffer_resource(
            D3D12_HEAP_TYPE_READBACK,
            D3D12_RESOURCE_STATE_COPY_DEST,
            u64::from(count) * 8,
        );
        let mut ptr: *mut c_void = core::ptr::null_mut();
        // SAFETY: a readback buffer may stay mapped while the GPU writes it;
        // the CPU reads a slot's stamps only after its fence passes.
        unsafe { readback.Map(0, None, Some(&mut ptr)) }.ok()?;
        Some(Clock {
            heap: heap?,
            readback,
            stamps: ptr.cast(),
            frequency,
            on: false,
            read: Vec::new(),
        })
    }

    /// How many errors the debug layer has reported (always 0 without it).
    /// Drains the layer's message queue, printing every message it held.
    pub fn validation_errors(&self) -> u32 {
        let Some(queue) = &self.info_queue else {
            return 0;
        };
        // SAFETY: each message is read into a buffer of the length the queue
        // reported for it, 8-byte aligned for `D3D12_MESSAGE`; the description
        // pointer and length it holds lie inside that buffer.
        unsafe {
            for i in 0..queue.GetNumStoredMessages() {
                let mut len = 0usize;
                if queue.GetMessage(i, None, &mut len).is_err() || len == 0 {
                    continue;
                }
                let mut storage = vec![0u64; len.div_ceil(8)];
                let msg = storage.as_mut_ptr().cast::<D3D12_MESSAGE>();
                if queue.GetMessage(i, Some(msg), &mut len).is_err() {
                    continue;
                }
                let m = &*msg;
                let text = core::slice::from_raw_parts(
                    m.pDescription,
                    m.DescriptionByteLength.saturating_sub(1),
                );
                if m.Severity == D3D12_MESSAGE_SEVERITY_ERROR
                    || m.Severity == D3D12_MESSAGE_SEVERITY_CORRUPTION
                {
                    self.validation_errors.set(self.validation_errors.get() + 1);
                }
                eprintln!("d3d12 debug layer: {}", String::from_utf8_lossy(text));
            }
            queue.ClearStoredMessages();
        }
        self.validation_errors.get()
    }

    /// Whether the debug layer is active on this backend.
    pub fn validation_enabled(&self) -> bool {
        self.info_queue.is_some()
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
        let t = self.texture(id);
        let (width, height) = (t.width, t.height);
        let resource = t.resource.clone();
        let row = t.format.row_bytes(width) as u32;
        let height = t.format.block_rows(height);
        let pitch = row.next_multiple_of(PITCH_ALIGN);
        let size = pitch as u64 * height as u64;
        let readback = self.create_buffer_resource(
            D3D12_HEAP_TYPE_READBACK,
            D3D12_RESOURCE_STATE_COPY_DEST,
            size,
        );
        let footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
            Offset: 0,
            Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                Format: dxgi_format(self.texture(id).format),
                Width: width,
                Height: height,
                Depth: 1,
                RowPitch: pitch,
            },
        };
        let cmd = self.slots[self.slot].draw.clone();
        // SAFETY: `cmd` is recording; the texture rests in `TEXTURE_REST` and
        // returns to it; the read-back buffer holds the whole pitched footprint.
        unsafe {
            cmd.ResourceBarrier(&[transition(
                &resource,
                TEXTURE_REST,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            cmd.CopyTextureRegion(
                &placed_location(&readback, footprint),
                0,
                0,
                0,
                &subresource_location(&resource),
                None,
            );
            cmd.ResourceBarrier(&[transition(
                &resource,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                TEXTURE_REST,
            )]);
        }
        let slot = self.slot;
        self.submit();
        self.wait_for(self.slots[slot].fence_value);
        let mut out = vec![0u8; row as usize * height as usize];
        // SAFETY: the submission that wrote the buffer has completed; the
        // mapping covers `size` bytes, of which each of the `height` rows reads
        // `row` bytes at its pitched offset.
        unsafe {
            let mut ptr: *mut c_void = core::ptr::null_mut();
            let range = D3D12_RANGE {
                Begin: 0,
                End: size as usize,
            };
            readback
                .Map(0, Some(&range), Some(&mut ptr))
                .expect("map the read-back buffer");
            let src = ptr.cast::<u8>();
            for y in 0..height as usize {
                core::ptr::copy_nonoverlapping(
                    src.add(y * pitch as usize),
                    out.as_mut_ptr().add(y * row as usize),
                    row as usize,
                );
            }
            readback.Unmap(0, Some(&D3D12_RANGE::default()));
        }
        self.reclaim_completed();
        out
    }

    fn buffer(&self, id: BufferId) -> &D3D12Buffer {
        self.buffers
            .get(id.into())
            .expect("buffer handle does not resolve")
    }

    fn texture(&self, id: TextureId) -> &D3D12Texture {
        self.textures
            .get(id.into())
            .expect("texture handle does not resolve")
    }

    fn surface(&self, id: SurfaceId) -> &D3D12Surface {
        self.surfaces
            .get(id.into())
            .expect("surface handle does not resolve")
    }

    fn surface_mut(&mut self, id: SurfaceId) -> &mut D3D12Surface {
        self.surfaces
            .get_mut(id.into())
            .expect("surface handle does not resolve")
    }

    /// A committed buffer resource of `size` bytes in `heap`, created in `state`.
    fn create_buffer_resource(
        &self,
        heap: D3D12_HEAP_TYPE,
        state: D3D12_RESOURCE_STATES,
        size: u64,
    ) -> ID3D12Resource {
        let props = D3D12_HEAP_PROPERTIES {
            Type: heap,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size.max(1),
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: the descriptions borrow locals for the call; a buffer takes
        // no clear value.
        unsafe {
            self.device
                .CreateCommittedResource(
                    &props,
                    D3D12_HEAP_FLAG_NONE,
                    &desc,
                    state,
                    None,
                    &mut resource,
                )
                .expect("failed to create a Direct3D 12 buffer");
        }
        resource.expect("CreateCommittedResource returned no resource")
    }

    /// An upload-heap buffer of `size` bytes, persistently mapped.
    fn create_mapped(&self, heap: D3D12_HEAP_TYPE, size: u64) -> (ID3D12Resource, *mut u8) {
        let state = if heap == D3D12_HEAP_TYPE_UPLOAD {
            D3D12_RESOURCE_STATE_GENERIC_READ
        } else {
            D3D12_RESOURCE_STATE_COMMON
        };
        let resource = self.create_buffer_resource(heap, state, size);
        let mut ptr: *mut c_void = core::ptr::null_mut();
        // SAFETY: the resource is a CPU-visible buffer; the CPU never reads it,
        // so the read range is empty. It stays mapped until released.
        unsafe {
            resource
                .Map(0, Some(&D3D12_RANGE::default()), Some(&mut ptr))
                .expect("failed to map a Direct3D 12 buffer");
        }
        (resource, ptr.cast())
    }

    /// Block until the fence reaches `value`.
    fn wait_for(&self, value: u64) {
        // SAFETY: the fence and event are live; the event is auto-reset and
        // armed for `value` before the wait.
        unsafe {
            if self.fence.GetCompletedValue() >= value {
                return;
            }
            self.fence
                .SetEventOnCompletion(value, self.fence_event)
                .expect("arm the frame fence event");
            WaitForSingleObject(self.fence_event, INFINITE);
        }
    }

    /// Block until every submission so far has completed.
    fn wait_idle(&mut self) {
        self.fence_value += 1;
        // SAFETY: signalling a live fence from the queue has no preconditions.
        unsafe {
            let _ = self.queue.Signal(&self.fence, self.fence_value);
        }
        self.wait_for(self.fence_value);
    }

    /// Open the current slot's command lists if they are not recording: wait
    /// for the slot's previous submission, reset its allocators and staging, and
    /// bind the root signature, heaps and default tables on the draw list.
    fn ensure_recording(&mut self) {
        if self.recording.is_some() {
            return;
        }
        self.wait_for(self.slots[self.slot].fence_value);
        let slot = &mut self.slots[self.slot];
        if let Some(clock) = &mut self.clock {
            clock.read(slot, self.slot);
        }
        let zeros = [0u32; InlineUniforms::MAX / 4];
        // SAFETY: the slot's last submission has completed, so its allocators
        // and lists are no longer in use and may be reset; the draw list binds
        // objects that live as long as the backend.
        unsafe {
            slot.upload_alloc
                .Reset()
                .expect("reset an upload allocator");
            slot.draw_alloc.Reset().expect("reset a draw allocator");
            slot.upload
                .Reset(&slot.upload_alloc, None)
                .expect("reset an upload command list");
            slot.draw
                .Reset(&slot.draw_alloc, None)
                .expect("reset a draw command list");
            let draw = &slot.draw;
            draw.SetDescriptorHeaps(&self.heaps);
            draw.SetGraphicsRootSignature(&self.root_signature);
            draw.SetGraphicsRoot32BitConstants(
                ROOT_UNIFORMS,
                zeros.len() as u32,
                zeros.as_ptr().cast(),
                0,
            );
            draw.SetGraphicsRootDescriptorTable(ROOT_TEXTURES, self.srv_heap.gpu(0));
            draw.SetGraphicsRootDescriptorTable(ROOT_SAMPLER, self.sampler_heap.gpu(0));
            draw.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            slot.clocking = false;
            if let Some(clock) = self.clock.as_ref().filter(|clock| clock.on) {
                slot.upload.EndQuery(
                    &clock.heap,
                    D3D12_QUERY_TYPE_TIMESTAMP,
                    2 * self.slot as u32,
                );
                slot.clocking = true;
            }
        }
        slot.staging.current = 0;
        slot.staging.cursor = 0;
        self.recording = Some(self.current_epoch);
    }

    /// Execute the open recording — uploads, then draws — signal the fence for
    /// it, and advance to the next slot.
    fn submit(&mut self) {
        let Some(epoch) = self.recording.take() else {
            return;
        };
        self.fence_value += 1;
        let slot = &mut self.slots[self.slot];
        // SAFETY: both lists were reset by `ensure_recording` and are closed here
        // exactly once before execution; the waits are on live shared fences
        // their writers signal.
        unsafe {
            for (fence, value) in self.waits.drain(..) {
                let _ = self.queue.Wait(&fence, value);
            }
            slot.upload.Close().expect("close the upload command list");
            slot.draw.Close().expect("close the draw command list");
            self.queue.ExecuteCommandLists(&slot.lists);
            self.queue
                .Signal(&self.fence, self.fence_value)
                .expect("signal the frame fence");
        }
        slot.fence_value = self.fence_value;
        slot.epoch = epoch;
        self.slot = (self.slot + 1) % FRAMES_IN_FLIGHT;
    }

    /// The newest epoch no unfinished GPU work — nor the open recording — can
    /// still reference.
    fn completed_epoch(&self) -> Epoch {
        // SAFETY: reading a live fence's value has no preconditions.
        let completed = unsafe { self.fence.GetCompletedValue() };
        let mut done = self.current_epoch.0;
        if let Some(e) = self.recording {
            done = done.min(e.0.saturating_sub(1));
        }
        for slot in &self.slots {
            if slot.fence_value > completed {
                done = done.min(slot.epoch.0.saturating_sub(1));
            }
        }
        Epoch(done)
    }

    /// The shared texture `image` names, opened on this device: the one
    /// opened for it lately, else newly; its index in `opened`. `None` where
    /// it cannot be, as for a texture of another adapter.
    fn open_shared(&mut self, image: &ExternalImage) -> Option<usize> {
        let ExternalImage::DxgiShared {
            texture,
            fence,
            holds,
            ..
        } = *image
        else {
            return None;
        };
        if let Some(at) = self
            .opened
            .iter()
            .position(|o| std::ptr::eq(Arc::as_ptr(&o.holds), holds))
        {
            return Some(at);
        }
        // SAFETY: the image's producer keeps its holds alive while it hands
        // the image over.
        let holds = unsafe { ExternalHolds::retain(holds) };
        let (mut resource, mut opened_fence) = (None::<ID3D12Resource>, None::<ID3D12Fence>);
        // SAFETY: the handles are live NT handles the producer owns while it
        // hands the image over; each call fills the local it is handed.
        unsafe {
            self.device
                .OpenSharedHandle(HANDLE(texture), &mut resource)
                .ok()?;
            self.device
                .OpenSharedHandle(HANDLE(fence), &mut opened_fence)
                .ok()?;
        }
        // A few players' textures stay open; the oldest goes first.
        if self.opened.len() == OPENED_SHARED {
            self.opened.remove(0);
        }
        self.opened.push(Opened {
            holds,
            resource: resource?,
            fence: opened_fence?,
        });
        Some(self.opened.len() - 1)
    }

    /// Copies the shared texture `opened[at]` into texture `id` once its
    /// writer has signalled `image`'s fence value, holding it until the copy
    /// completes.
    fn copy_shared(&mut self, id: TextureId, at: usize, image: &ExternalImage) {
        let ExternalImage::DxgiShared { value, .. } = *image else {
            return;
        };
        self.let_go_completed();
        let dst = self.texture(id).resource.clone();
        let opened = &self.opened[at];
        let (src, fence, hold) = (
            opened.resource.clone(),
            opened.fence.clone(),
            opened.holds.hold(),
        );
        self.ensure_recording();
        let cmd = &self.slots[self.slot].upload;
        // SAFETY: `cmd` is recording; the texture rests in `TEXTURE_REST` and
        // returns to it; the shared texture, in the common state, is promoted
        // to a copy source by the copy and decays back once it completes;
        // both are BGRA of one size.
        unsafe {
            cmd.ResourceBarrier(&[transition(
                &dst,
                TEXTURE_REST,
                D3D12_RESOURCE_STATE_COPY_DEST,
            )]);
            cmd.CopyResource(&dst, &src);
            cmd.ResourceBarrier(&[transition(
                &dst,
                D3D12_RESOURCE_STATE_COPY_DEST,
                TEXTURE_REST,
            )]);
        }
        self.waits.push((fence.clone(), value));
        let epoch = self.recording.unwrap_or(self.current_epoch);
        self.held.push((epoch, hold, src, fence));
    }

    /// Lets go of the shared textures no copy in flight reads.
    fn let_go_completed(&mut self) {
        if self.held.is_empty() {
            return;
        }
        let done = self.completed_epoch();
        self.held.retain(|(epoch, ..)| epoch.0 > done.0);
    }

    /// Release every retired resource the GPU can no longer reference.
    fn reclaim_completed(&mut self) {
        self.let_go_completed();
        let fence = Fence::at(self.completed_epoch());
        self.reclaim_scratch.clear();
        self.retire_queue
            .drain_completed(fence, &mut self.reclaim_scratch);
        for i in 0..self.reclaim_scratch.len() {
            let entry = self.reclaim_scratch[i];
            // The completion fence has passed the epoch each resource was retired
            // in, so no pending or recorded command references it.
            match entry.kind {
                ResourceKind::Buffer => {
                    self.buffers.remove(entry.id);
                }
                ResourceKind::Texture => {
                    if let Some(t) = self.textures.remove(entry.id) {
                        self.rtv_free.extend(t.rtv);
                        self.dsv_free.extend(t.dsv);
                    }
                }
                ResourceKind::Sampler => {
                    if let Some(s) = self.samplers.remove(entry.id) {
                        self.sampler_free.push(s.slot);
                    }
                }
                ResourceKind::Pipeline => {
                    self.pipelines.remove(entry.id);
                }
                ResourceKind::BindGroup => {
                    if let Some(g) = self.bind_groups.remove(entry.id) {
                        if g.owns_unit {
                            self.srv_free.push(g.srv_unit);
                        }
                        if let Some((pair, _)) = g.sampler_pair {
                            self.release_sampler_pair(pair);
                        }
                    }
                }
            }
        }
    }

    /// Copy `rows` rows of `row` bytes from `bytes` into the current slot's
    /// staging arena at a copyable pitch, returning the chunk, the footprint's
    /// offset and its row pitch. The recording must be open.
    fn stage_rows(&mut self, bytes: &[u8], row: u32, rows: u32) -> (ID3D12Resource, u64, u32) {
        let pitch = row.next_multiple_of(PITCH_ALIGN);
        let size = pitch as u64 * rows as u64;
        loop {
            let staging = &mut self.slots[self.slot].staging;
            if let Some(chunk) = staging.chunks.get(staging.current) {
                let offset = staging.cursor.next_multiple_of(PLACEMENT_ALIGN);
                if offset + size <= chunk.size {
                    for y in 0..rows as usize {
                        // SAFETY: row `y` lands at `offset + y * pitch`, inside
                        // the chunk's mapping since `offset + size` fits, and
                        // reads `row` bytes of the caller's checked region; the
                        // slot's previous reader finished before it opened.
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                bytes.as_ptr().add(y * row as usize),
                                chunk.ptr.add(offset as usize + y * pitch as usize),
                                row as usize,
                            );
                        }
                    }
                    staging.cursor = offset + size;
                    return (chunk.resource.clone(), offset, pitch);
                }
                staging.current += 1;
                staging.cursor = 0;
                continue;
            }
            let chunk_size = size.max(STAGING_CHUNK).next_power_of_two();
            let (resource, ptr) = self.create_mapped(D3D12_HEAP_TYPE_UPLOAD, chunk_size);
            let staging = &mut self.slots[self.slot].staging;
            staging.chunks.push(StagingChunk {
                resource,
                ptr,
                size: chunk_size,
            });
            staging.current = staging.chunks.len() - 1;
            staging.cursor = 0;
        }
    }

    /// A free render-target view handle, opening a new RTV heap when none is.
    fn allocate_rtv(&mut self) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        allocate_view(
            &self.device,
            D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
            self.rtv_increment,
            &mut self.rtv_heaps,
            &mut self.rtv_free,
        )
    }

    /// A free depth-stencil view handle, opening a new DSV heap when none is.
    fn allocate_dsv(&mut self) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        allocate_view(
            &self.device,
            D3D12_DESCRIPTOR_HEAP_TYPE_DSV,
            self.dsv_increment,
            &mut self.dsv_heaps,
            &mut self.dsv_free,
        )
    }

    fn allocate_srv_unit(&mut self) -> u32 {
        if let Some(unit) = self.srv_free.pop() {
            return unit;
        }
        assert!(
            self.srv_next < SRV_UNITS,
            "more than {} live textured bind groups",
            SRV_UNITS - 1
        );
        self.srv_next += 1;
        self.srv_next - 1
    }

    fn allocate_sampler_slot(&mut self) -> u32 {
        if let Some(slot) = self.sampler_free.pop() {
            return slot;
        }
        assert!(
            self.sampler_next < SAMPLER_SLOTS,
            "more than {} live samplers",
            SAMPLER_SLOTS - 1
        );
        self.sampler_next += 1;
        self.sampler_next - 1
    }

    /// The heap slots holding `pair` side by side, written on first use.
    fn acquire_sampler_pair(&mut self, pair: [SamplerId; 2]) -> u32 {
        if let Some(p) = self.sampler_pairs.iter_mut().find(|p| p.samplers == pair) {
            p.refs += 1;
            return p.slot;
        }
        let slot = self.sampler_pair_free.pop().unwrap_or_else(|| {
            assert!(
                self.sampler_next + 2 <= SAMPLER_SLOTS,
                "more than {SAMPLER_SLOTS} live sampler slots"
            );
            self.sampler_next += 2;
            self.sampler_next - 2
        });
        for (i, id) in pair.iter().enumerate() {
            let desc = self
                .samplers
                .get((*id).into())
                .expect("sampler handle does not resolve")
                .desc;
            // SAFETY: `slot + i` lies in the pair's two free slots.
            unsafe {
                self.device
                    .CreateSampler(&desc, self.sampler_heap.cpu(slot + i as u32));
            }
        }
        self.sampler_pairs.push(SamplerPair {
            samplers: pair,
            slot,
            refs: 1,
        });
        slot
    }

    fn release_sampler_pair(&mut self, pair: [SamplerId; 2]) {
        if let Some(i) = self.sampler_pairs.iter().position(|p| p.samplers == pair) {
            let p = &mut self.sampler_pairs[i];
            p.refs -= 1;
            if p.refs == 0 {
                self.sampler_pair_free.push(p.slot);
                self.sampler_pairs.swap_remove(i);
            }
        }
    }

    /// `source`'s `entry` compiled for `profile`, compiled once per program.
    fn compile(&mut self, source: &'static str, entry: &'static str, pixel: bool) -> ID3DBlob {
        if let Some(c) = self
            .shader_cache
            .iter()
            .find(|c| c.source == source.as_ptr() && c.entry == entry.as_ptr() && c.pixel == pixel)
        {
            return c.blob.clone();
        }
        let blob = compile_stage(source, entry, pixel).unwrap_or_else(|e| panic!("{e}"));
        self.shader_cache.push(CompiledStage {
            source: source.as_ptr(),
            entry: entry.as_ptr(),
            pixel,
            blob: blob.clone(),
        });
        blob
    }

    /// Create a swapchain's buffers' views into `rtvs`.
    fn fetch_buffers(
        &self,
        swapchain: &IDXGISwapChain3,
        rtvs: &[D3D12_CPU_DESCRIPTOR_HANDLE; SWAP_BUFFERS as usize],
    ) -> Vec<ID3D12Resource> {
        (0..SWAP_BUFFERS)
            .map(|i| {
                let view = D3D12_RENDER_TARGET_VIEW_DESC {
                    Format: dxgi_format(SURFACE_FORMAT),
                    ViewDimension: D3D12_RTV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D12_RENDER_TARGET_VIEW_DESC_0 {
                        Texture2D: D3D12_TEX2D_RTV {
                            MipSlice: 0,
                            PlaneSlice: 0,
                        },
                    },
                };
                // SAFETY: `i` is below the swapchain's buffer count, each view
                // is written into a handle the surface owns, and `view` names
                // the sRGB twin of the buffers' format, which a flip-model
                // buffer's render-target view may take.
                unsafe {
                    let buffer: ID3D12Resource =
                        swapchain.GetBuffer(i).expect("query a swapchain buffer");
                    self.device
                        .CreateRenderTargetView(&buffer, Some(&view), rtvs[i as usize]);
                    buffer
                }
            })
            .collect()
    }

    /// Rebuild `id`'s buffers at its requested size. Returns `false` when the
    /// surface currently has no area (minimized), leaving it stale.
    fn rebuild_swapchain(&mut self, id: SurfaceId) -> bool {
        let s = self.surface(id);
        let (w, h) = (s.width, s.height);
        if w == 0 || h == 0 {
            return false;
        }
        // Every reference to the old buffers — ours and the GPU's — must be gone
        // before they can be resized.
        self.wait_idle();
        let s = self.surface_mut(id);
        s.buffers.clear();
        let swapchain = s.swapchain.clone();
        let rtvs = s.rtvs;
        // SAFETY: the device is idle and the surface held the only references to
        // the buffers; the format and flags match the swapchain's creation.
        unsafe {
            swapchain
                .ResizeBuffers(SWAP_BUFFERS, w, h, SWAP_FORMAT, SWAP_FLAGS)
                .expect("resize the swapchain buffers");
        }
        let buffers = self.fetch_buffers(&swapchain, &rtvs);
        let s = self.surface_mut(id);
        s.buffers = buffers;
        s.extent = (w, h);
        s.stale = false;
        s.presented = [0; SWAP_BUFFERS as usize];
        true
    }

    /// Record one render pass into the draw list.
    fn record_pass(&mut self, pass: &RenderPass, commands: &[DrawCommand]) {
        let cmd = self.slots[self.slot].draw.clone();
        let (resource, rtv, extent, texture) = match pass.target {
            RenderTarget::Surface(frame) => {
                let Some(acq) = self.acquired.filter(|a| a.surface == frame.surface) else {
                    return;
                };
                let s = self.surface(frame.surface);
                let out = (
                    s.buffers[acq.image as usize].clone(),
                    s.rtvs[acq.image as usize],
                    s.extent,
                    false,
                );
                if !acq.in_rt {
                    // SAFETY: `cmd` is recording and the back buffer is in
                    // PRESENT, where the swapchain hands it over.
                    unsafe {
                        cmd.ResourceBarrier(&[transition(
                            &out.0,
                            D3D12_RESOURCE_STATE_PRESENT,
                            D3D12_RESOURCE_STATE_RENDER_TARGET,
                        )]);
                    }
                    if let Some(a) = self.acquired.as_mut() {
                        a.in_rt = true;
                    }
                }
                out
            }
            RenderTarget::Texture(id) => {
                let t = self.texture(id);
                let rtv = t
                    .rtv
                    .expect("texture drawn into was not created as a render target");
                let out = (t.resource.clone(), rtv, (t.width, t.height), true);
                // SAFETY: `cmd` is recording; the texture rests in `TEXTURE_REST`.
                unsafe {
                    cmd.ResourceBarrier(&[transition(
                        &out.0,
                        TEXTURE_REST,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                    )]);
                }
                out
            }
        };
        let viewport = D3D12_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: extent.0 as f32,
            Height: extent.1 as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let depth = pass.depth.map(|d| {
            let t = self.texture(d.texture);
            let dsv = t
                .dsv
                .expect("a depth attachment is a Depth32Float render target");
            (dsv, d.clear, t.resource.clone())
        });
        // SAFETY: `cmd` is recording and the target is in RENDER_TARGET; `rtv`
        // is its live view. A depth target rests in DEPTH_WRITE and `dsv` is
        // its live view.
        unsafe {
            cmd.OMSetRenderTargets(
                1,
                Some(&rtv),
                false,
                depth.as_ref().map(|d| &d.0 as *const _),
            );
            if let LoadOp::Clear(color) = pass.load {
                cmd.ClearRenderTargetView(rtv, &color, None);
            }
            if let Some((dsv, clear, _)) = &depth {
                cmd.ClearDepthStencilView(*dsv, D3D12_CLEAR_FLAG_DEPTH, *clear, 0, None);
            }
            cmd.RSSetViewports(&[viewport]);
        }
        let mut bound = Bound::default();
        for c in commands {
            self.record_command(&cmd, c, extent, &mut bound);
        }
        if let Some((_, _, resource)) = &depth {
            // SAFETY: the depth is transient: nothing reads it past the pass.
            unsafe { cmd.DiscardResource(resource, None) };
        }
        if texture {
            // SAFETY: the texture returns to its resting state.
            unsafe {
                cmd.ResourceBarrier(&[transition(
                    &resource,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    TEXTURE_REST,
                )]);
            }
        }
    }

    /// Record one draw against the bound render target.
    fn record_command(
        &self,
        cmd: &ID3D12GraphicsCommandList,
        c: &DrawCommand,
        extent: (u32, u32),
        bound: &mut Bound,
    ) {
        let (sx, sy, sw, sh) = match c.scissor {
            Some((x, y, w, h)) => {
                let x = x.min(extent.0);
                let y = y.min(extent.1);
                (x, y, w.min(extent.0 - x), h.min(extent.1 - y))
            }
            None => (0, 0, extent.0, extent.1),
        };
        let scissor = RECT {
            left: sx as i32,
            top: sy as i32,
            right: (sx + sw) as i32,
            bottom: (sy + sh) as i32,
        };
        let p = self
            .pipelines
            .get(c.pipeline.into())
            .expect("pipeline handle does not resolve");
        if bound.pipeline != Some(c.pipeline) {
            let root = match (&p.program, p.scene) {
                (Some(_), _) => Root::Program(c.pipeline),
                (None, true) => Root::Scene,
                (None, false) => Root::Shared,
            };
            if bound.root != root {
                let signature = match (&p.program, root) {
                    (Some(r), _) => &r.signature,
                    (None, Root::Scene) => &self.scene_root_signature,
                    _ => &self.root_signature,
                };
                // SAFETY: `cmd` is recording; switching the root signature
                // drops every root binding, so the default tables are rebound
                // and the bind group with them.
                unsafe {
                    cmd.SetGraphicsRootSignature(signature);
                    if root == Root::Shared {
                        let zeros = [0u32; InlineUniforms::MAX / 4];
                        cmd.SetGraphicsRoot32BitConstants(
                            ROOT_UNIFORMS,
                            zeros.len() as u32,
                            zeros.as_ptr().cast(),
                            0,
                        );
                    }
                    cmd.SetGraphicsRootDescriptorTable(ROOT_TEXTURES, self.srv_heap.gpu(0));
                    cmd.SetGraphicsRootDescriptorTable(ROOT_SAMPLER, self.sampler_heap.gpu(0));
                }
                bound.root = root;
                bound.group = None;
            }
            // SAFETY: `cmd` is recording; the PSO was built against the root
            // signature just bound.
            unsafe { cmd.SetPipelineState(&p.pso) };
            bound.pipeline = Some(c.pipeline);
        }
        let stride = p.stride;
        let group = c.bind_group.map(|bg| {
            self.bind_groups
                .get(bg.into())
                .expect("bind group handle does not resolve")
        });
        // SAFETY: `cmd` is recording under the pipeline's root signature; the
        // tables point into the bound heaps, the views into live buffers, and
        // the renderer guarantees the instance/index ranges lie inside them.
        unsafe {
            cmd.RSSetScissorRects(&[scissor]);
            let pair = p.program.as_ref().is_some_and(|r| r.samplers > 1);
            if let (Some(bg), Some(g)) = (c.bind_group, group)
                && bound.group != Some((bg, pair))
            {
                let sampler = if pair {
                    g.sampler_pair
                        .expect("a program reading two samplers draws with a group binding two")
                        .1
                } else {
                    g.sampler_slot
                };
                cmd.SetGraphicsRootDescriptorTable(
                    ROOT_TEXTURES,
                    self.srv_heap.gpu(g.srv_unit * SRV_UNIT),
                );
                cmd.SetGraphicsRootDescriptorTable(ROOT_SAMPLER, self.sampler_heap.gpu(sampler));
                bound.group = Some((bg, pair));
            }
            let uniforms = c.uniforms.as_bytes();
            if p.scene || p.program.as_ref().is_some_and(|r| r.uniform_buffer) {
                let buffer = group
                    .and_then(|g| g.uniform)
                    .expect("a uniform-buffer pipeline draws with a group binding one");
                cmd.SetGraphicsRootConstantBufferView(ROOT_UNIFORMS, self.buffer(buffer).gpu_va);
            } else if !uniforms.is_empty() {
                let mut words = [0u32; InlineUniforms::MAX / 4];
                core::ptr::copy_nonoverlapping(
                    uniforms.as_ptr(),
                    words.as_mut_ptr().cast::<u8>(),
                    uniforms.len(),
                );
                cmd.SetGraphicsRoot32BitConstants(
                    ROOT_UNIFORMS,
                    uniforms.len().div_ceil(4) as u32,
                    words.as_ptr().cast(),
                    0,
                );
            }
            match c.geometry {
                Geometry::Generated { count } if p.program.is_some() => {
                    let inst = self.buffer(c.instance_buffer);
                    cmd.SetGraphicsRootShaderResourceView(
                        ROOT_INSTANCES,
                        inst.gpu_va + c.instance_offset as u64,
                    );
                    cmd.DrawInstanced(6, count, 0, 0);
                }
                Geometry::Generated { count } => {
                    let inst = self.buffer(c.instance_buffer);
                    if stride > 0 {
                        cmd.IASetVertexBuffers(
                            0,
                            Some(&[vertex_view(inst, c.instance_offset, stride)]),
                        );
                    }
                    cmd.DrawInstanced(6, count, 0, 0);
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
                    let start = index_offset as usize * index_format.size();
                    cmd.IASetVertexBuffers(0, Some(&[vertex_view(vtx, 0, stride)]));
                    cmd.IASetIndexBuffer(Some(&D3D12_INDEX_BUFFER_VIEW {
                        BufferLocation: idx.gpu_va + start as u64,
                        SizeInBytes: (idx.len - start) as u32,
                        Format: match index_format {
                            IndexFormat::U16 => DXGI_FORMAT_R16_UINT,
                            IndexFormat::U32 => DXGI_FORMAT_R32_UINT,
                        },
                    }));
                    cmd.DrawIndexedInstanced(index_count, 1, 0, 0, 0);
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
                    let start = index_offset as usize * index_format.size();
                    cmd.IASetVertexBuffers(
                        0,
                        Some(&[
                            vertex_view(vtx, 0, stride),
                            vertex_view(inst, c.instance_offset, SceneInstance::SIZE as u32),
                        ]),
                    );
                    cmd.IASetIndexBuffer(Some(&D3D12_INDEX_BUFFER_VIEW {
                        BufferLocation: idx.gpu_va + start as u64,
                        SizeInBytes: (idx.len - start) as u32,
                        Format: match index_format {
                            IndexFormat::U16 => DXGI_FORMAT_R16_UINT,
                            IndexFormat::U32 => DXGI_FORMAT_R32_UINT,
                        },
                    }));
                    cmd.DrawIndexedInstanced(index_count, count, 0, 0, 0);
                }
            }
        }
    }
}

/// The Direct3D 12 program compiler: `D3DCompile`, the program's root
/// signature and its pipeline state, on any thread.
#[derive(Clone)]
pub struct D3D12Compiler {
    device: ID3D12Device,
}

/// A compiled user program, ready to install.
pub struct D3D12Program {
    pso: ID3D12PipelineState,
    root: ProgramRoot,
}

impl ProgramCompiler for D3D12Compiler {
    type Program = D3D12Program;

    fn compile(&self, desc: &ProgramDesc<'_>) -> Result<D3D12Program, String> {
        let ProgramCode::Hlsl(source) = desc.code else {
            return Err(format!(
                "the Direct3D 12 backend consumes HLSL, got {:?}",
                desc.code.lang()
            ));
        };
        desc.check_abi()?;
        let uniform_buffer = desc.abi.uniform_size as usize > InlineUniforms::MAX;
        let tables = (desc.abi.textures.max(1), desc.abi.samplers.max(1));
        let signature = serialize_root_signature(&self.device, Some(tables), uniform_buffer)?;
        let vs = compile_stage(source, desc.vertex_entry, false)?;
        let ps = compile_stage(source, desc.fragment_entry, true)?;
        let pso = create_pso(
            &self.device,
            &signature,
            &vs,
            &ps,
            &[],
            Fixed {
                color_format: desc.color_format,
                blend: desc.blend,
                depth: None,
                cull: CullMode::None,
            },
        )?;
        Ok(D3D12Program {
            pso,
            root: ProgramRoot {
                signature,
                samplers: desc.abi.samplers,
                uniform_buffer,
            },
        })
    }
}

impl GpuBackend for D3D12Backend {
    const SHADER_LANG: ShaderLang = ShaderLang::Hlsl;

    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        let len = desc.size.max(1);
        let heap = if self.gpu_upload_heap {
            D3D12_HEAP_TYPE_GPU_UPLOAD
        } else {
            D3D12_HEAP_TYPE_UPLOAD
        };
        let (resource, ptr) = self.create_mapped(heap, len as u64);
        // SAFETY: `resource` is a live buffer.
        let gpu_va = unsafe { resource.GetGPUVirtualAddress() };
        self.buffers
            .insert(D3D12Buffer {
                _resource: resource,
                ptr,
                len,
                gpu_va,
            })
            .into()
    }

    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId {
        let (width, height) = (desc.width.max(1), desc.height.max(1));
        // A depth render target is a depth-stencil resource; any other depth
        // texture is a single-channel float one, sampled or copied.
        let depth = desc.render_target && desc.format == TextureFormat::Depth32Float;
        let format = if depth {
            DXGI_FORMAT_D32_FLOAT
        } else {
            dxgi_format(desc.format)
        };
        let props = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };
        let rdesc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: width as u64,
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: if depth {
                D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL | D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE
            } else if desc.render_target {
                D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
            } else {
                D3D12_RESOURCE_FLAG_NONE
            },
            ..Default::default()
        };
        let clear = D3D12_CLEAR_VALUE {
            Format: format,
            Anonymous: if depth {
                D3D12_CLEAR_VALUE_0 {
                    DepthStencil: D3D12_DEPTH_STENCIL_VALUE {
                        Depth: 0.0,
                        Stencil: 0,
                    },
                }
            } else {
                D3D12_CLEAR_VALUE_0 { Color: [0.0; 4] }
            },
        };
        // A depth target rests as one: it only ever serves a pass.
        let state = if depth {
            D3D12_RESOURCE_STATE_DEPTH_WRITE
        } else if desc.render_target {
            D3D12_RESOURCE_STATE_RENDER_TARGET
        } else {
            TEXTURE_REST
        };
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: the descriptions borrow locals for the call; the optimized
        // clear value is given only to a render target, in its own format.
        unsafe {
            self.device
                .CreateCommittedResource(
                    &props,
                    D3D12_HEAP_FLAG_NONE,
                    &rdesc,
                    state,
                    desc.render_target.then_some(&clear as *const _),
                    &mut resource,
                )
                .expect("failed to create a Direct3D 12 texture");
        }
        let resource = resource.expect("CreateCommittedResource returned no resource");
        let dsv = depth.then(|| {
            let dsv = self.allocate_dsv();
            // SAFETY: `resource` allows depth-stencil views; `dsv` is a free
            // handle.
            unsafe { self.device.CreateDepthStencilView(&resource, None, dsv) };
            dsv
        });
        let rtv = (desc.render_target && !depth).then(|| {
            let rtv = self.allocate_rtv();
            // SAFETY: `resource` allows render targets; `rtv` is a free handle.
            unsafe { self.device.CreateRenderTargetView(&resource, None, rtv) };
            // Clear once so a target sampled before it is drawn reads
            // transparent black, then move it to its resting state.
            self.ensure_recording();
            let cmd = &self.slots[self.slot].upload;
            // SAFETY: `cmd` is recording and the texture is in RENDER_TARGET.
            unsafe {
                cmd.ClearRenderTargetView(rtv, &[0.0; 4], None);
                cmd.ResourceBarrier(&[transition(
                    &resource,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    TEXTURE_REST,
                )]);
            }
            rtv
        });
        self.textures
            .insert(D3D12Texture {
                resource,
                format: desc.format,
                width,
                height,
                rtv,
                dsv,
                external: false,
            })
            .into()
    }

    fn import_external(&mut self, image: &ExternalImage) -> Option<TextureId> {
        let ExternalImage::DxgiShared { width, height, .. } = *image else {
            return None;
        };
        let at = self.open_shared(image)?;
        let id = self.create_texture(&TextureDesc {
            width,
            height,
            format: TextureFormat::Bgra8UnormSrgb,
            render_target: false,
            label: "external image",
        });
        if let Some(t) = self.textures.get_mut(id.into()) {
            t.external = true;
        }
        self.copy_shared(id, at, image);
        Some(id)
    }

    fn adopt_external(&mut self, id: TextureId, image: &ExternalImage) -> bool {
        let Some(t) = self.textures.get(id.into()) else {
            return false;
        };
        if !t.external || (t.width, t.height) != image.size() {
            return false;
        }
        let Some(at) = self.open_shared(image) else {
            return false;
        };
        self.copy_shared(id, at, image);
        true
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        let slot = self.allocate_sampler_slot();
        let desc = sampler_desc(desc);
        // SAFETY: `slot` is a free slot of the sampler heap.
        unsafe {
            self.device
                .CreateSampler(&desc, self.sampler_heap.cpu(slot));
        }
        self.samplers.insert(D3D12Sampler { slot, desc }).into()
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, crate::instance::LayoutError> {
        layout.validate_against(&desc.instance_schema)?;
        let ShaderCode::Hlsl(source) = desc.code else {
            panic!(
                "the Direct3D 12 backend consumes HLSL, got {:?}",
                desc.code.lang()
            );
        };
        let vs = self.compile(source, desc.vertex_entry, false);
        let ps = self.compile(source, desc.fragment_entry, true);

        let scene = desc.builtin == BuiltinShader::Scene;
        let per_vertex = (D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA, 0);
        let per_instance = (D3D12_INPUT_CLASSIFICATION_PER_INSTANCE_DATA, 1);
        // A scene draw adds its instances as slot 1, at the semantic indices
        // after the vertex attributes.
        let streams: &[(InstanceSchema, _)] = match desc.builtin {
            BuiltinShader::Scene => &[
                (desc.instance_schema, per_vertex),
                (scene::INSTANCE_SCHEMA, per_instance),
            ],
            BuiltinShader::Path | BuiltinShader::Mesh => &[(desc.instance_schema, per_vertex)],
            _ => &[(desc.instance_schema, per_instance)],
        };
        let mut elements = Vec::new();
        for (slot, (schema, (class, step))) in streams.iter().enumerate() {
            let mut offset = 0u32;
            for a in schema.attributes {
                elements.push(D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR(ATTR_SEMANTIC.as_ptr().cast()),
                    SemanticIndex: elements.len() as u32,
                    Format: attr_format(a.format),
                    InputSlot: slot as u32,
                    AlignedByteOffset: offset,
                    InputSlotClass: *class,
                    InstanceDataStepRate: *step,
                });
                offset += a.format.size() as u32;
            }
        }

        let root = if scene {
            &self.scene_root_signature
        } else {
            &self.root_signature
        };
        let pso = create_pso(&self.device, root, &vs, &ps, &elements, desc)
            .unwrap_or_else(|e| panic!("failed to create a Direct3D 12 pipeline: {e}"));
        Ok(self
            .pipelines
            .insert(D3D12Pipeline {
                pso,
                stride: layout.stride as u32,
                scene,
                program: None,
            })
            .into())
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        let mut textures: [Option<ID3D12Resource>; SRV_UNIT as usize] = Default::default();
        let mut formats = [TextureFormat::Rgba8Unorm; SRV_UNIT as usize];
        let mut texture_count = 0;
        let mut samplers = [None; PROGRAM_SAMPLERS as usize];
        let mut sampler_count = 0;
        let mut uniform = None;
        for binding in &desc.bindings {
            match *binding {
                Binding::Texture(id) => {
                    if texture_count < textures.len() {
                        let t = self.texture(id);
                        textures[texture_count] = Some(t.resource.clone());
                        formats[texture_count] = t.format;
                        texture_count += 1;
                    }
                }
                Binding::Sampler(id) => {
                    if sampler_count < samplers.len() {
                        samplers[sampler_count] = Some(id);
                        sampler_count += 1;
                    }
                }
                // The built-ins' uniforms are root constants; a user program
                // with a large block reads this buffer.
                Binding::Uniform(id) => uniform = Some(id),
            }
        }
        // A program reading fewer textures leaves the rest of the unit unread;
        // repeat the last one so the unit is complete for any program.
        for i in texture_count.max(1)..textures.len() {
            textures[i] = textures[i - 1].clone();
            formats[i] = formats[i - 1];
        }
        let srv_unit = if texture_count == 0 {
            0
        } else {
            let unit = self.allocate_srv_unit();
            for (i, (t, f)) in textures.iter().zip(formats).enumerate() {
                let srv = srv_desc(dxgi_format(f));
                // SAFETY: the view describes a live single-mip 2D texture and is
                // written into the group's own unit.
                unsafe {
                    self.device.CreateShaderResourceView(
                        t.as_ref(),
                        Some(&srv),
                        self.srv_heap.cpu(unit * SRV_UNIT + i as u32),
                    );
                }
            }
            unit
        };
        let sampler_slot = samplers[0].map_or(0, |id| {
            self.samplers
                .get(id.into())
                .expect("sampler handle does not resolve")
                .slot
        });
        let sampler_pair = match samplers {
            [Some(a), Some(b)] => Some(([a, b], self.acquire_sampler_pair([a, b]))),
            _ => None,
        };
        self.bind_groups
            .insert(D3D12BindGroup {
                srv_unit,
                owns_unit: texture_count > 0,
                sampler_slot,
                sampler_pair,
                uniform,
            })
            .into()
    }

    type Compiler = D3D12Compiler;

    fn program_compiler(&self) -> D3D12Compiler {
        D3D12Compiler {
            device: self.device.clone(),
        }
    }

    fn install_program(&mut self, program: D3D12Program) -> PipelineId {
        self.pipelines
            .insert(D3D12Pipeline {
                pso: program.pso,
                stride: 0,
                scene: false,
                program: Some(program.root),
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
        // heap is write-combined and coherent, so no flush is needed.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.ptr.add(offset), bytes.len());
        }
    }

    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]) {
        let t = self.texture(id);
        // A block format's rows are rows of blocks, its footprint whole blocks.
        let block = t.format.block();
        let row = t.format.row_bytes(w) as u32;
        let rows = t.format.block_rows(h);
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
        let (resource, format) = (t.resource.clone(), dxgi_format(t.format));
        self.ensure_recording();
        let (chunk, offset, pitch) = self.stage_rows(&bytes[..len], row, rows);
        let footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
            Offset: offset,
            Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                Format: format,
                Width: w.next_multiple_of(block.width),
                Height: h.next_multiple_of(block.height),
                Depth: 1,
                RowPitch: pitch,
            },
        };
        let cmd = &self.slots[self.slot].upload;
        // SAFETY: `cmd` is recording; the texture rests in `TEXTURE_REST` and
        // returns to it; the staged footprint covers the `w`×`h` region.
        unsafe {
            cmd.ResourceBarrier(&[transition(
                &resource,
                TEXTURE_REST,
                D3D12_RESOURCE_STATE_COPY_DEST,
            )]);
            cmd.CopyTextureRegion(
                &subresource_location(&resource),
                x,
                y,
                0,
                &placed_location(&chunk, footprint),
                None,
            );
            cmd.ResourceBarrier(&[transition(
                &resource,
                D3D12_RESOURCE_STATE_COPY_DEST,
                TEXTURE_REST,
            )]);
        }
    }

    fn create_surface(&mut self, raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId {
        let RawWindowHandle::Win32 { hwnd, .. } = raw else {
            panic!("D3D12Backend cannot present to {raw:?}");
        };
        let hwnd = HWND(hwnd);
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(1),
            Height: height.max(1),
            Format: SWAP_FORMAT,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: SWAP_BUFFERS,
            Scaling: DXGI_SCALING_STRETCH,
            // Sequential, not discard: a back buffer keeps the frame it last
            // showed, so a frame redraws only what changed since.
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: SWAP_FLAGS.0 as u32,
        };
        // SAFETY: the platform layer hands over a live window that outlives the
        // surface; the swapchain presents from this backend's queue.
        let (swapchain, waitable) = unsafe {
            let swapchain: IDXGISwapChain3 = self
                .factory
                .CreateSwapChainForHwnd(&self.queue, hwnd, &desc, None, None)
                .and_then(|s| s.cast())
                .expect("failed to create a DXGI swapchain");
            let _ = self
                .factory
                .MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            swapchain
                .SetMaximumFrameLatency(1)
                .expect("set the swapchain frame latency");
            let waitable = swapchain.GetFrameLatencyWaitableObject();
            (swapchain, waitable)
        };
        let rtvs = std::array::from_fn(|_| self.allocate_rtv());
        let buffers = self.fetch_buffers(&swapchain, &rtvs);
        let extent = (desc.Width, desc.Height);
        self.surfaces
            .insert(D3D12Surface {
                swapchain,
                waitable,
                waited: false,
                buffers,
                rtvs,
                width,
                height,
                extent,
                stale: extent != (width, height),
                presents: 0,
                presented: [0; SWAP_BUFFERS as usize],
            })
            .into()
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        let s = self.surface_mut(id);
        s.width = width;
        s.height = height;
        if s.extent != (width, height) {
            s.stale = true;
        }
    }

    fn destroy_surface(&mut self, id: SurfaceId) {
        if self.surfaces.get(id.into()).is_none() {
            return;
        }
        if self.acquired.is_some() {
            self.device_lost(id);
        } else {
            self.submit();
            self.wait_idle();
            self.reclaim_completed();
        }
        let s = self.surfaces.remove(id.into()).expect("checked");
        self.rtv_free.extend(s.rtvs);
        // SAFETY: the queue is idle, so nothing waits on or presents to the
        // swapchain; its latency handle is closed exactly once. The buffers
        // and the swapchain release on drop.
        unsafe {
            let _ = CloseHandle(s.waitable);
        }
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame> {
        self.reclaim_completed();
        if let Some(open) = self.acquired.map(|a| a.surface) {
            // A frame begun and never presented: drop it as a lost frame.
            self.device_lost(open);
        }
        if self.surface(surface).stale && !self.rebuild_swapchain(surface) {
            return None;
        }
        let s = self.surface_mut(surface);
        if !s.waited {
            // SAFETY: `waitable` is the swapchain's live latency object.
            let ready = unsafe { WaitForSingleObjectEx(s.waitable, FRAME_WAIT_MS, true) };
            if ready != WAIT_OBJECT_0 {
                return None;
            }
            s.waited = true;
        }
        // SAFETY: querying a live swapchain has no preconditions.
        let image = unsafe { s.swapchain.GetCurrentBackBufferIndex() };
        self.ensure_recording();
        self.acquired = Some(Acquired {
            surface,
            image,
            in_rt: false,
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
            self.submit();
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
        match s.presented.get(frame.drawable as usize) {
            Some(&at) if at > 0 => u32::try_from(s.presents + 1 - at).unwrap_or(0),
            _ => 0,
        }
    }

    fn present_with_damage(&mut self, frame: Frame, damage: &[(u32, u32, u32, u32)]) {
        let Some(acq) = self.acquired.filter(|a| a.surface == frame.surface) else {
            return;
        };
        self.acquired = None;
        if acq.in_rt {
            let buffer = self.surface(frame.surface).buffers[acq.image as usize].clone();
            // SAFETY: the recording is open (begin_frame opened it) and the back
            // buffer is in RENDER_TARGET.
            unsafe {
                self.slots[self.slot].draw.ResourceBarrier(&[transition(
                    &buffer,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    D3D12_RESOURCE_STATE_PRESENT,
                )]);
            }
        }
        let slot = &mut self.slots[self.slot];
        slot.timed = None;
        if let Some(clock) = &self.clock
            && slot.clocking
        {
            let first = 2 * self.slot as u32;
            // SAFETY: the recording is open; the end stamp follows the start
            // the upload list wrote, which the same submission runs first,
            // and both resolve into this slot's pair of the readback buffer.
            unsafe {
                slot.draw
                    .EndQuery(&clock.heap, D3D12_QUERY_TYPE_TIMESTAMP, first + 1);
                slot.draw.ResolveQueryData(
                    &clock.heap,
                    D3D12_QUERY_TYPE_TIMESTAMP,
                    first,
                    2,
                    &clock.readback,
                    u64::from(first) * 8,
                );
            }
            slot.timed = Some(self.current_epoch.0);
        }
        self.submit();
        let s = self.surface_mut(frame.surface);
        s.waited = false;
        let mut rects: Vec<RECT> = damage
            .iter()
            .map(|&(x, y, w, h)| RECT {
                left: x as i32,
                top: y as i32,
                right: (x + w) as i32,
                bottom: (y + h) as i32,
            })
            .collect();
        let params = DXGI_PRESENT_PARAMETERS {
            DirtyRectsCount: rects.len() as u32,
            pDirtyRects: if rects.is_empty() {
                std::ptr::null_mut()
            } else {
                rects.as_mut_ptr()
            },
            pScrollRect: std::ptr::null_mut(),
            pScrollOffset: std::ptr::null_mut(),
        };
        // SAFETY: the back buffer's commands were just submitted on the
        // swapchain's queue, ending in PRESENT; `rects` outlives the call.
        let hr = unsafe { s.swapchain.Present1(1, DXGI_PRESENT(0), &params) };
        s.presents += 1;
        let presents = s.presents;
        if let Some(at) = s.presented.get_mut(acq.image as usize) {
            *at = presents;
        }
        if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
            // SAFETY: querying the removal reason has no preconditions.
            let reason = unsafe { self.device.GetDeviceRemovedReason() };
            panic!("Direct3D 12 device removed: {reason:?}");
        }
        if hr.is_err() {
            s.stale = true;
        }
    }

    fn device_lost(&mut self, _surface: SurfaceId) {
        if let Some(acq) = self.acquired.take() {
            // The frame is abandoned: execute only the uploads and discard the
            // draw list (its allocator is reset when the slot reopens), leaving
            // the back buffer in PRESENT for the next frame to take again.
            if let Some(epoch) = self.recording.take() {
                self.fence_value += 1;
                let slot = &mut self.slots[self.slot];
                // SAFETY: both lists are recording and closed exactly once.
                unsafe {
                    let _ = slot.upload.Close();
                    let _ = slot.draw.Close();
                    self.queue.ExecuteCommandLists(&slot.lists[..1]);
                    let _ = self.queue.Signal(&self.fence, self.fence_value);
                }
                slot.fence_value = self.fence_value;
                slot.epoch = epoch;
                self.slot = (self.slot + 1) % FRAMES_IN_FLIGHT;
            }
            self.surface_mut(acq.surface).stale = true;
        } else {
            self.submit();
        }
        self.wait_idle();
        self.reclaim_completed();
    }

    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn surface_format(&self, _surface: SurfaceId) -> TextureFormat {
        SURFACE_FORMAT
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
        // SAFETY: reading a live fence's value has no preconditions.
        let completed = unsafe { self.fence.GetCompletedValue() };
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.fence_value <= completed {
                clock.read(slot, index);
            }
        }
        clock.read.sort_unstable_by_key(|&(epoch, _)| epoch);
        out.append(&mut clock.read);
    }
}

impl Drop for D3D12Backend {
    fn drop(&mut self) {
        self.wait_idle();
        // SAFETY: the queue is idle, so no command references anything below;
        // each handle is closed exactly once. COM objects release on drop.
        unsafe {
            for s in self.surfaces.drain() {
                let _ = CloseHandle(s.waitable);
            }
            let _ = CloseHandle(self.fence_event);
        }
    }
}

/// The first hardware adapter, by GPU preference, that creates a Direct3D 12
/// device at feature level 11.0 — or WARP when none does.
fn create_device(factory: &IDXGIFactory6) -> Option<ID3D12Device> {
    let mut device: Option<ID3D12Device> = None;
    // SAFETY: adapter enumeration and device creation have no preconditions
    // beyond live arguments.
    unsafe {
        let mut i = 0;
        while let Ok(adapter) = factory
            .EnumAdapterByGpuPreference::<IDXGIAdapter1>(i, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)
        {
            i += 1;
            let software = adapter
                .GetDesc1()
                .is_ok_and(|d| d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0);
            if !software
                && D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device).is_ok()
                && device.is_some()
            {
                return device;
            }
        }
        let warp: IDXGIAdapter = factory.EnumWarpAdapter().ok()?;
        D3D12CreateDevice(&warp, D3D_FEATURE_LEVEL_11_0, &mut device).ok()?;
    }
    device
}

/// Whether buffers can live in the GPU-upload heap (resizable BAR).
fn supports_gpu_upload_heap(device: &ID3D12Device) -> bool {
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS16::default();
    // SAFETY: the pointer and size describe `options`, the structure the
    // feature expects; older runtimes reject the query, which reads as no.
    unsafe {
        device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS16,
                (&mut options as *mut D3D12_FEATURE_DATA_D3D12_OPTIONS16).cast(),
                size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS16>() as u32,
            )
            .is_ok()
            && options.GPUUploadHeapSupported.as_bool()
    }
}

/// The shared root signature: root constants at `b0`, an SRV table at `t0..t1`
/// and a sampler table at `s0`, visible to every stage.
/// The shared root signature every built-in pipeline is drawn under.
fn create_root_signature(device: &ID3D12Device) -> ID3D12RootSignature {
    serialize_root_signature(device, None, false)
        .unwrap_or_else(|e| panic!("failed to create the Direct3D 12 root signature: {e}"))
}

/// A root signature: the uniforms at `b0` (root constants, or a root
/// constant-buffer view when `uniform_buffer`), the texture table at `t0` and
/// the sampler table at `s0`. A program's — `program` holds its table sizes —
/// appends the instance array's root view at `t0, space1` and reads no input
/// layout.
fn serialize_root_signature(
    device: &ID3D12Device,
    program: Option<(u32, u32)>,
    uniform_buffer: bool,
) -> Result<ID3D12RootSignature, String> {
    let (textures, samplers) = program.unwrap_or((BUILTIN_TEXTURES, BUILTIN_SAMPLERS));
    let srv_range = [D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
        NumDescriptors: textures,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: 0,
    }];
    let sampler_range = [D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER,
        NumDescriptors: samplers,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: 0,
    }];
    let table = |ranges: &[D3D12_DESCRIPTOR_RANGE]| D3D12_ROOT_PARAMETER {
        ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
        Anonymous: D3D12_ROOT_PARAMETER_0 {
            DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                NumDescriptorRanges: ranges.len() as u32,
                pDescriptorRanges: ranges.as_ptr(),
            },
        },
        ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
    };
    let root_view = |ty, space| D3D12_ROOT_PARAMETER {
        ParameterType: ty,
        Anonymous: D3D12_ROOT_PARAMETER_0 {
            Descriptor: D3D12_ROOT_DESCRIPTOR {
                ShaderRegister: 0,
                RegisterSpace: space,
            },
        },
        ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
    };
    let uniforms = if uniform_buffer {
        root_view(D3D12_ROOT_PARAMETER_TYPE_CBV, 0)
    } else {
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Constants: D3D12_ROOT_CONSTANTS {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                    Num32BitValues: (InlineUniforms::MAX / 4) as u32,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        }
    };
    let params = [
        uniforms,
        table(&srv_range),
        table(&sampler_range),
        root_view(D3D12_ROOT_PARAMETER_TYPE_SRV, 1),
    ];
    let count = if program.is_some() { 4 } else { 3 };
    let desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: count,
        pParameters: params.as_ptr(),
        NumStaticSamplers: 0,
        pStaticSamplers: core::ptr::null(),
        Flags: if program.is_some() {
            D3D12_ROOT_SIGNATURE_FLAG_NONE
        } else {
            D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT
        },
    };
    let mut blob: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the description and the ranges it points at are live locals; the
    // serialized blob is read within its own size.
    unsafe {
        if let Err(e) = D3D12SerializeRootSignature(
            &desc,
            D3D_ROOT_SIGNATURE_VERSION_1,
            &mut blob,
            Some(&mut errors),
        ) {
            let log = errors.map(|b| blob_bytes(&b).to_vec()).unwrap_or_default();
            return Err(format!(
                "root signature serialization failed: {e}\n{}",
                String::from_utf8_lossy(&log)
            ));
        }
        let blob = blob.ok_or("root signature serialization returned no blob")?;
        device
            .CreateRootSignature(0, blob_bytes(&blob))
            .map_err(|e| e.to_string())
    }
}

/// `source`'s `entry` compiled for the vertex or the `pixel` stage, or the
/// compiler's log.
fn compile_stage(source: &str, entry: &str, pixel: bool) -> Result<ID3DBlob, String> {
    let entry_c = CString::new(entry).map_err(|_| format!("entry name `{entry}` holds a NUL"))?;
    let profile = if pixel { PIXEL_PROFILE } else { VERTEX_PROFILE };
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source pointer and length describe `source`; the entry and
    // profile are NUL-terminated and outlive the call.
    let result = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry_c.as_ptr().cast()),
            PCSTR(profile.as_ptr().cast()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = result {
        let log = errors.map(|b| blob_bytes(&b).to_vec()).unwrap_or_default();
        return Err(format!(
            "HLSL compile of `{entry}` ({}) failed: {e}\n{}",
            profile.to_string_lossy(),
            String::from_utf8_lossy(&log)
        ));
    }
    code.ok_or_else(|| "D3DCompile returned no bytecode".to_owned())
}

/// A render-target blend description for `blend`.
fn blend_desc(blend: BlendMode) -> D3D12_RENDER_TARGET_BLEND_DESC {
    let (enable, dest) = match blend {
        BlendMode::Replace => (false, D3D12_BLEND_ZERO),
        BlendMode::PremultipliedOver => (true, D3D12_BLEND_INV_SRC_ALPHA),
    };
    D3D12_RENDER_TARGET_BLEND_DESC {
        BlendEnable: enable.into(),
        LogicOpEnable: false.into(),
        SrcBlend: D3D12_BLEND_ONE,
        DestBlend: dest,
        BlendOp: D3D12_BLEND_OP_ADD,
        SrcBlendAlpha: D3D12_BLEND_ONE,
        DestBlendAlpha: dest,
        BlendOpAlpha: D3D12_BLEND_OP_ADD,
        LogicOp: D3D12_LOGIC_OP_NOOP,
        RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
    }
}

/// A triangle-list pipeline state drawing into one `color_format` target
/// under `root`, reading `elements` as its input layout.
/// The fixed-function state of a pipeline: what `desc` says of blending,
/// depth and culling.
struct Fixed {
    color_format: TextureFormat,
    blend: BlendMode,
    depth: Option<DepthState>,
    cull: CullMode,
}

impl From<&PipelineDesc> for Fixed {
    fn from(desc: &PipelineDesc) -> Self {
        Fixed {
            color_format: desc.color_format,
            blend: desc.blend,
            depth: desc.depth,
            cull: desc.cull,
        }
    }
}

fn create_pso(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    vs: &ID3DBlob,
    ps: &ID3DBlob,
    elements: &[D3D12_INPUT_ELEMENT_DESC],
    fixed: impl Into<Fixed>,
) -> Result<ID3D12PipelineState, String> {
    let Fixed {
        color_format,
        blend,
        depth,
        cull,
    } = fixed.into();
    let mut blend_state = D3D12_BLEND_DESC::default();
    blend_state.RenderTarget[0] = blend_desc(blend);
    let stencil_op = D3D12_DEPTH_STENCILOP_DESC {
        StencilFailOp: D3D12_STENCIL_OP_KEEP,
        StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
        StencilPassOp: D3D12_STENCIL_OP_KEEP,
        StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
    };
    let mut rtv_formats = [DXGI_FORMAT_UNKNOWN; 8];
    rtv_formats[0] = dxgi_format(color_format);
    let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
        // SAFETY: a borrowed copy of the root signature pointer, wrapped in
        // `ManuallyDrop` so the description never releases it; the
        // signature outlives the call.
        pRootSignature: unsafe { core::mem::transmute_copy(root) },
        VS: bytecode(vs),
        PS: bytecode(ps),
        BlendState: blend_state,
        SampleMask: u32::MAX,
        RasterizerState: D3D12_RASTERIZER_DESC {
            FillMode: D3D12_FILL_MODE_SOLID,
            CullMode: match cull {
                CullMode::None => D3D12_CULL_MODE_NONE,
                CullMode::Back => D3D12_CULL_MODE_BACK,
            },
            // A face counter-clockwise in clip space is counter-clockwise on
            // the target, whose y the viewport flips.
            FrontCounterClockwise: true.into(),
            DepthClipEnable: true.into(),
            ..Default::default()
        },
        DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
            DepthEnable: depth.is_some().into(),
            DepthWriteMask: if depth.is_some_and(|d| d.write) {
                D3D12_DEPTH_WRITE_MASK_ALL
            } else {
                D3D12_DEPTH_WRITE_MASK_ZERO
            },
            DepthFunc: depth.map_or(D3D12_COMPARISON_FUNC_ALWAYS, |d| comparison(d.compare)),
            StencilEnable: false.into(),
            StencilReadMask: 0xff,
            StencilWriteMask: 0xff,
            FrontFace: stencil_op,
            BackFace: stencil_op,
        },
        InputLayout: D3D12_INPUT_LAYOUT_DESC {
            pInputElementDescs: if elements.is_empty() {
                core::ptr::null()
            } else {
                elements.as_ptr()
            },
            NumElements: elements.len() as u32,
        },
        PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
        NumRenderTargets: 1,
        RTVFormats: rtv_formats,
        DSVFormat: if depth.is_some() {
            DXGI_FORMAT_D32_FLOAT
        } else {
            DXGI_FORMAT_UNKNOWN
        },
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        ..Default::default()
    };
    // SAFETY: every pointer in the description borrows a live local or blob for
    // the call; the bytecode reads what the root signature declares.
    unsafe { device.CreateGraphicsPipelineState(&desc) }.map_err(|e| e.to_string())
}

/// A free handle of a CPU-only view heap of type `ty`, opening a new heap of
/// [`RTV_BLOCK`] views when `free` is empty.
fn allocate_view(
    device: &ID3D12Device,
    ty: D3D12_DESCRIPTOR_HEAP_TYPE,
    increment: u32,
    heaps: &mut Vec<ID3D12DescriptorHeap>,
    free: &mut Vec<D3D12_CPU_DESCRIPTOR_HANDLE>,
) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    if let Some(h) = free.pop() {
        return h;
    }
    // SAFETY: a CPU-only view heap description with no other preconditions.
    let heap: ID3D12DescriptorHeap = unsafe {
        device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
            Type: ty,
            NumDescriptors: RTV_BLOCK,
            ..Default::default()
        })
    }
    .expect("failed to create a Direct3D 12 view heap");
    // SAFETY: `heap` is live.
    let start = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    free.extend((0..RTV_BLOCK).rev().map(|i| D3D12_CPU_DESCRIPTOR_HANDLE {
        ptr: start.ptr + i as usize * increment as usize,
    }));
    heaps.push(heap);
    free.pop().expect("a fresh view heap has free handles")
}

fn create_gpu_heap(device: &ID3D12Device, ty: D3D12_DESCRIPTOR_HEAP_TYPE, count: u32) -> GpuHeap {
    // SAFETY: a shader-visible heap description within the type's limits.
    unsafe {
        let heap: ID3D12DescriptorHeap = device
            .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: ty,
                NumDescriptors: count,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })
            .expect("failed to create a Direct3D 12 descriptor heap");
        GpuHeap {
            cpu: heap.GetCPUDescriptorHandleForHeapStart(),
            gpu: heap.GetGPUDescriptorHandleForHeapStart(),
            increment: device.GetDescriptorHandleIncrementSize(ty),
            heap,
        }
    }
}

fn create_frame_slot(device: &ID3D12Device) -> FrameSlot {
    // SAFETY: plain object creation on a live device; each list is closed at
    // once so `ensure_recording` can reset it like any submitted list.
    unsafe {
        let upload_alloc: ID3D12CommandAllocator = device
            .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
            .expect("failed to create a Direct3D 12 command allocator");
        let draw_alloc: ID3D12CommandAllocator = device
            .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
            .expect("failed to create a Direct3D 12 command allocator");
        let upload: ID3D12GraphicsCommandList = device
            .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &upload_alloc, None)
            .expect("failed to create a Direct3D 12 command list");
        let draw: ID3D12GraphicsCommandList = device
            .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &draw_alloc, None)
            .expect("failed to create a Direct3D 12 command list");
        upload.Close().expect("close a fresh command list");
        draw.Close().expect("close a fresh command list");
        let lists = [
            Some(upload.cast::<ID3D12CommandList>().expect("command list")),
            Some(draw.cast::<ID3D12CommandList>().expect("command list")),
        ];
        FrameSlot {
            upload_alloc,
            draw_alloc,
            upload,
            draw,
            lists,
            fence_value: 0,
            epoch: Epoch::START,
            staging: Staging::default(),
            clocking: false,
            timed: None,
        }
    }
}

/// A blob's bytes, borrowed for the blob's lifetime.
fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    // SAFETY: a blob's buffer pointer is valid for its reported size for the
    // blob's lifetime.
    unsafe {
        core::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
    }
}

fn bytecode(blob: &ID3DBlob) -> D3D12_SHADER_BYTECODE {
    // SAFETY: reading a live blob's pointer and size has no preconditions.
    unsafe {
        D3D12_SHADER_BYTECODE {
            pShaderBytecode: blob.GetBufferPointer(),
            BytecodeLength: blob.GetBufferSize(),
        }
    }
}

fn vertex_view(buffer: &D3D12Buffer, offset: usize, stride: u32) -> D3D12_VERTEX_BUFFER_VIEW {
    D3D12_VERTEX_BUFFER_VIEW {
        BufferLocation: buffer.gpu_va + offset as u64,
        SizeInBytes: (buffer.len - offset) as u32,
        StrideInBytes: stride,
    }
}

/// A whole-resource state transition.
fn transition(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                // SAFETY: a borrowed copy of the resource pointer that the
                // barrier never releases; the resource outlives the recording.
                pResource: unsafe { core::mem::transmute_copy(resource) },
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

fn subresource_location(resource: &ID3D12Resource) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: a borrowed, never-released copy of a live resource pointer.
        pResource: unsafe { core::mem::transmute_copy(resource) },
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: 0,
        },
    }
}

fn placed_location(
    resource: &ID3D12Resource,
    footprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: a borrowed, never-released copy of a live resource pointer.
        pResource: unsafe { core::mem::transmute_copy(resource) },
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            PlacedFootprint: footprint,
        },
    }
}

fn srv_desc(format: DXGI_FORMAT) -> D3D12_SHADER_RESOURCE_VIEW_DESC {
    D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D12_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
                PlaneSlice: 0,
                ResourceMinLODClamp: 0.0,
            },
        },
    }
}

/// The view written for an unbound texture slot: it reads zero.
fn null_srv_desc() -> D3D12_SHADER_RESOURCE_VIEW_DESC {
    srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM)
}

fn sampler_desc(desc: &SamplerDesc) -> D3D12_SAMPLER_DESC {
    let address = match desc.address {
        AddressMode::ClampToEdge => D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressMode::Repeat => D3D12_TEXTURE_ADDRESS_MODE_WRAP,
        AddressMode::Mirror => D3D12_TEXTURE_ADDRESS_MODE_MIRROR,
    };
    D3D12_SAMPLER_DESC {
        Filter: match desc.filter {
            FilterMode::Nearest => D3D12_FILTER_MIN_MAG_MIP_POINT,
            FilterMode::Linear => D3D12_FILTER_MIN_MAG_LINEAR_MIP_POINT,
            FilterMode::MipmapLinear => D3D12_FILTER_MIN_MAG_MIP_LINEAR,
        },
        AddressU: address,
        AddressV: address,
        AddressW: address,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: D3D12_FLOAT32_MAX,
    }
}

/// Map a Viso [`TextureFormat`] to its DXGI format.
fn comparison(f: CompareFunction) -> D3D12_COMPARISON_FUNC {
    match f {
        CompareFunction::Always => D3D12_COMPARISON_FUNC_ALWAYS,
        CompareFunction::Less => D3D12_COMPARISON_FUNC_LESS,
        CompareFunction::LessEqual => D3D12_COMPARISON_FUNC_LESS_EQUAL,
        CompareFunction::Greater => D3D12_COMPARISON_FUNC_GREATER,
        CompareFunction::GreaterEqual => D3D12_COMPARISON_FUNC_GREATER_EQUAL,
    }
}

fn dxgi_format(format: TextureFormat) -> DXGI_FORMAT {
    match format {
        TextureFormat::Bgra8Unorm => DXGI_FORMAT_B8G8R8A8_UNORM,
        // Premultiplication is a content convention, not a storage property, so
        // a data plane and a color plane of the same width share one format.
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Data => DXGI_FORMAT_R8G8B8A8_UNORM,
        TextureFormat::R8Unorm => DXGI_FORMAT_R8_UNORM,
        // DXGI names packed formats from the low bits up: red sits high, as
        // Viso stores it.
        TextureFormat::Rgb565Unorm => DXGI_FORMAT_B5G6R5_UNORM,
        TextureFormat::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
        // As a color texture; a depth render target is `D32_FLOAT`.
        TextureFormat::Depth32Float => DXGI_FORMAT_R32_FLOAT,
        TextureFormat::Bc1RgbaUnorm => DXGI_FORMAT_BC1_UNORM,
        TextureFormat::Bc3RgbaUnorm => DXGI_FORMAT_BC3_UNORM,
        TextureFormat::Bc7RgbaUnorm => DXGI_FORMAT_BC7_UNORM,
        TextureFormat::Bgra8UnormSrgb => DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
        TextureFormat::Rgba8UnormSrgb => DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
        TextureFormat::Bc1RgbaUnormSrgb => DXGI_FORMAT_BC1_UNORM_SRGB,
        TextureFormat::Bc3RgbaUnormSrgb => DXGI_FORMAT_BC3_UNORM_SRGB,
        TextureFormat::Bc7RgbaUnormSrgb => DXGI_FORMAT_BC7_UNORM_SRGB,
        // DXGI's 4:4:4:4 and 5:5:5:1 put alpha high and red in the middle,
        // and it has no ETC2 or ASTC: `Caps::sampled` leaves them out, so an
        // image in one is decoded before it is uploaded.
        unsampled @ (TextureFormat::Rgba4444Unorm
        | TextureFormat::Rgba5551Unorm
        | TextureFormat::Etc2Rgb8Unorm
        | TextureFormat::Etc2Rgb8A1Unorm
        | TextureFormat::Etc2Rgba8Unorm
        | TextureFormat::Astc4x4Unorm
        | TextureFormat::Astc6x6Unorm
        | TextureFormat::Astc8x8Unorm
        | TextureFormat::Etc2Rgb8UnormSrgb
        | TextureFormat::Etc2Rgb8A1UnormSrgb
        | TextureFormat::Etc2Rgba8UnormSrgb
        | TextureFormat::Astc4x4UnormSrgb
        | TextureFormat::Astc6x6UnormSrgb
        | TextureFormat::Astc8x8UnormSrgb) => {
            panic!("Direct3D 12 samples no {unsampled:?} texture (Caps::sampled)")
        }
    }
}

/// The formats beyond the baseline `device` samples with filtering: BC on
/// every feature level 11 device. Not B5G6R5: it has no sRGB variant to
/// decode a sample.
fn sampled_formats(device: &ID3D12Device) -> SampledFormats {
    let samples = |format: DXGI_FORMAT| {
        let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
            Format: format,
            ..Default::default()
        };
        // SAFETY: `support` is the structure the format-support query fills,
        // passed with its own size.
        let queried = unsafe {
            device.CheckFeatureSupport(
                D3D12_FEATURE_FORMAT_SUPPORT,
                (&mut support as *mut D3D12_FEATURE_DATA_FORMAT_SUPPORT).cast(),
                core::mem::size_of::<D3D12_FEATURE_DATA_FORMAT_SUPPORT>() as u32,
            )
        };
        queried.is_ok()
            && (support.Support1.0 & D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE.0) != 0
            && (support.Support1.0 & D3D12_FORMAT_SUPPORT1_TEXTURE2D.0) != 0
    };
    let mut sampled = SampledFormats::empty();
    if [
        DXGI_FORMAT_BC1_UNORM,
        DXGI_FORMAT_BC3_UNORM,
        DXGI_FORMAT_BC7_UNORM,
    ]
    .into_iter()
    .all(samples)
    {
        sampled |= SampledFormats::BC;
    }
    sampled
}

/// Map an attribute format to its DXGI vertex format.
fn attr_format(format: AttrFormat) -> DXGI_FORMAT {
    match format {
        AttrFormat::Float1 => DXGI_FORMAT_R32_FLOAT,
        AttrFormat::Float2 => DXGI_FORMAT_R32G32_FLOAT,
        AttrFormat::Float3 => DXGI_FORMAT_R32G32B32_FLOAT,
        AttrFormat::Float4 => DXGI_FORMAT_R32G32B32A32_FLOAT,
        AttrFormat::Uint1 => DXGI_FORMAT_R32_UINT,
        AttrFormat::Uint2 => DXGI_FORMAT_R32G32_UINT,
        AttrFormat::Uint4 => DXGI_FORMAT_R32G32B32A32_UINT,
    }
}
