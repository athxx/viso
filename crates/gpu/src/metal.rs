//! `MetalBackend` — the native macOS Metal implementation of [`GpuBackend`].
//!
//! It is built directly on `objc2-metal` 0.3 / `objc2-quartz-core` 0.3 and
//! compiles only on macOS; other targets use [`HeadlessRaster`].
//!
//! ## Design decisions
//!
//! - **Buffers use `StorageModeShared`** (`contents()` + memcpy). On
//!   Apple-Silicon UMA this avoids explicit-flush bookkeeping.
//! - **No `MTLVertexDescriptor`.** The Quad vertex shader generates the four
//!   corner positions from `vertex_id` and reads per-instance data from a raw
//!   buffer bound at index 1 (the same convention as the RHI's `DrawCommand`).
//! - **Uniforms are inline** via `setVertexBytes:`/`setFragmentBytes:` — the
//!   renderer passes the surface `[width, height]` so the shader maps pixel-space
//!   rects to NDC. (Headless ignores uniforms; it works in pixel space.)
//! - Every frame's acquire/encode/present runs inside an `autoreleasepool` so the
//!   per-frame Metal autoreleased objects (drawable, command buffer, encoder) are
//!   drained each frame rather than piling up.
//!
//! Blend is premultiplied source-over (`src One`, `dst OneMinusSourceAlpha`,
//! op `Add`), matching the headless raster.

use core::ffi::c_void;
use core::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_foundation::{CGRect, CGSize};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLBuffer, MTLClearColor, MTLCommandBuffer,
    MTLCommandEncoder, MTLCommandQueue, MTLCompileOptions, MTLCreateSystemDefaultDevice, MTLDevice,
    MTLIndexType, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType,
    MTLRegion, MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLResourceOptions, MTLSamplerAddressMode, MTLSamplerDescriptor,
    MTLSamplerMinMagFilter, MTLSamplerMipFilter, MTLSamplerState, MTLScissorRect, MTLSize,
    MTLStoreAction, MTLTexture, MTLTextureDescriptor, MTLTextureUsage, MTLViewport,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use viso_handle::RawWindowHandle;

use crate::backend::{
    DrawCommand, DrawList, Frame, Geometry, GpuBackend, IndexFormat, LoadOp, RenderPass,
    RenderTarget,
};
use crate::instance::InstanceLayout;
use crate::resource::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BuiltinShader, Caps, FilterMode,
    PipelineDesc, SamplerDesc, TextureDesc, TextureFormat,
};
use crate::retire::{Epoch, Fence, ResourceKind, RetireQueue, Retired};
use crate::slots::SlotMap;
use crate::{BindGroupId, BufferId, PipelineId, SamplerId, SurfaceId, TextureId};

/// A GPU buffer: a `StorageModeShared` `MTLBuffer` whose `contents()` we memcpy
/// into on `write_buffer`.
struct MetalBuffer {
    buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    /// Allocated size in bytes.
    len: usize,
}

/// A GPU texture: the `MTLTexture` plus its format (to compute the row stride
/// on `write_texture`).
struct MetalTexture {
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    format: TextureFormat,
}

/// A sampler state object.
struct MetalSampler {
    state: Retained<ProtocolObject<dyn MTLSamplerState>>,
}

/// A bind group: the resolved bindings (texture/sampler ids), consulted at
/// encode time to bind the fragment texture @0 and sampler @0.
struct MetalBindGroup {
    bindings: Vec<Binding>,
}

/// A registered pipeline: the compiled render pipeline state plus its built-in
/// tag (unused by Metal beyond debugging — Metal runs the compiled MSL).
struct MetalPipeline {
    state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    #[allow(dead_code)]
    builtin: BuiltinShader,
}

/// A surface: a `CAMetalLayer` attached to the window's content `NSView`, plus
/// its current drawable size in device pixels.
struct MetalSurface {
    layer: Retained<CAMetalLayer>,
    /// The live content view whose point-space bounds determine layer geometry.
    view: *mut c_void,
    width: u32,
    height: u32,
    format: TextureFormat,
    /// The drawable acquired by `begin_frame`, consumed by `present`. Metal's
    /// `nextDrawable` is per-frame; we hold it across encode.
    current: Option<Retained<ProtocolObject<dyn objc2_quartz_core::CAMetalDrawable>>>,
}

/// The native Metal backend.
pub struct MetalBackend {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    buffers: SlotMap<MetalBuffer>,
    textures: SlotMap<MetalTexture>,
    samplers: SlotMap<MetalSampler>,
    bind_groups: SlotMap<MetalBindGroup>,
    pipelines: SlotMap<MetalPipeline>,
    surfaces: SlotMap<MetalSurface>,
    caps: Caps,
    /// Resources awaiting GPU completion before their storage slot is freed.
    retire_queue: RetireQueue,
    /// The epoch of the frame currently being built (the one a `destroy_*` call
    /// parks against). Advanced by `begin_frame`.
    current_epoch: Epoch,
    /// Highest epoch the GPU has finished, shared with the command-buffer
    /// completion handler. The handler runs on a background thread and raises
    /// this via `fetch_max`; `begin_frame` reads it to build a [`Fence`] and
    /// reclaim everything parked at or below it.
    completed: Arc<AtomicU64>,
    /// Reused across `begin_frame` calls to drain the retire queue without a
    /// per-frame heap allocation.
    reclaim_scratch: Vec<Retired>,
    /// How many times MSL has been compiled into a `MTLLibrary` (§7.1). Every
    /// standard pipeline is compiled once at device init; a well-formed frame
    /// never adds to this. A test snapshots it after prewarm and asserts a paint
    /// leaves it unchanged — the "no runtime shader compilation" contract.
    library_compiles: u64,
}

impl Default for MetalBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MetalBackend {
    /// Create the backend on the system default Metal device.
    ///
    /// # Panics
    /// Panics if no Metal device is available (Metal is required on the macOS
    /// path; there is no software fallback here — that is [`HeadlessRaster`]).
    pub fn new() -> Self {
        let device = MTLCreateSystemDefaultDevice().expect("no system default Metal device");
        let queue = device
            .newCommandQueue()
            .expect("failed to create a Metal command queue");
        let max_texture_size = 16384;
        Self {
            device,
            queue,
            buffers: SlotMap::new(),
            textures: SlotMap::new(),
            samplers: SlotMap::new(),
            bind_groups: SlotMap::new(),
            pipelines: SlotMap::new(),
            surfaces: SlotMap::new(),
            caps: Caps {
                max_texture_size,
                presents_to_display: true,
                // The device has compute; this backend exposes no dispatch
                // encoder, so nothing above it may plan one (§20.1).
                compute_dispatch: false,
                // Metal has argument tables; this backend's bind groups bind one
                // texture per slot at creation, so there is no table to index
                // (§20.2). Atlas + bind-group batching is the path that exists.
                bindless_texture_slots: 0,
            },
            retire_queue: RetireQueue::new(),
            current_epoch: Epoch::START,
            completed: Arc::new(AtomicU64::new(0)),
            reclaim_scratch: Vec::new(),
            library_compiles: 0,
        }
    }

    /// Resolve a buffer handle, panicking on a stale/unknown one (an internal
    /// invariant break — a live handle always resolves in this backend).
    fn buffer(&self, id: BufferId) -> &MetalBuffer {
        self.buffers
            .get(id.into())
            .expect("buffer handle does not resolve")
    }

    /// Resolve a texture handle (see [`buffer`](Self::buffer) for the panic).
    fn texture(&self, id: TextureId) -> &MetalTexture {
        self.textures
            .get(id.into())
            .expect("texture handle does not resolve")
    }

    /// Resolve a sampler handle (see [`buffer`](Self::buffer)).
    fn sampler(&self, id: SamplerId) -> &MetalSampler {
        self.samplers
            .get(id.into())
            .expect("sampler handle does not resolve")
    }

    /// Resolve a pipeline handle (see [`buffer`](Self::buffer)).
    fn pipeline(&self, id: PipelineId) -> &MetalPipeline {
        self.pipelines
            .get(id.into())
            .expect("pipeline handle does not resolve")
    }

    /// Resolve a bind-group handle (see [`buffer`](Self::buffer)).
    fn bind_group(&self, id: BindGroupId) -> &MetalBindGroup {
        self.bind_groups
            .get(id.into())
            .expect("bind group handle does not resolve")
    }

    /// Resolve a surface handle for shared access (see [`buffer`](Self::buffer)).
    fn surface(&self, id: SurfaceId) -> &MetalSurface {
        self.surfaces
            .get(id.into())
            .expect("surface handle does not resolve")
    }

    /// Resolve a surface handle for mutation (see [`buffer`](Self::buffer)).
    fn surface_mut(&mut self, id: SurfaceId) -> &mut MetalSurface {
        self.surfaces
            .get_mut(id.into())
            .expect("surface handle does not resolve")
    }

    /// Number of resources parked in the retire queue awaiting GPU completion.
    /// The queue drains as frames finish; a bounded value across steady-state
    /// frames is the memory contract deferred destruction exists to keep.
    pub fn retired_count(&self) -> usize {
        self.retire_queue.len()
    }

    /// How many times MSL has been compiled into a `MTLLibrary` since this
    /// backend was created. The standard pipelines account for one compile each
    /// at device init (§7.1); a steady-state frame adds none. Instrumentation for
    /// the no-runtime-compile test — not a hot-path value.
    pub fn library_compiles(&self) -> u64 {
        self.library_compiles
    }

    /// Free the storage slots of resources whose parking epoch the GPU has
    /// finished. Reads the shared completion counter into a [`Fence`], drains
    /// every entry at or below it, and removes each from its `SlotMap` — which
    /// bumps the slot's generation so any surviving handle now resolves to
    /// nothing rather than to whatever later reuses the slot.
    fn reclaim_completed(&mut self) {
        let fence = Fence::at(Epoch(self.completed.load(Ordering::Acquire)));
        self.reclaim_scratch.clear();
        self.retire_queue
            .drain_completed(fence, &mut self.reclaim_scratch);
        for entry in self.reclaim_scratch.drain(..) {
            match entry.kind {
                ResourceKind::Buffer => {
                    self.buffers.remove(entry.id);
                }
                ResourceKind::Texture => {
                    self.textures.remove(entry.id);
                }
                ResourceKind::Sampler => {
                    self.samplers.remove(entry.id);
                }
                ResourceKind::Pipeline => {
                    self.pipelines.remove(entry.id);
                }
                ResourceKind::BindGroup => {
                    self.bind_groups.remove(entry.id);
                }
            }
        }
    }
}

/// Keep the layer's point-space frame and pixel-space drawable synchronized.
///
/// `NSView::bounds` is expressed in logical points while the drawable uses
/// physical pixels. The view's backing conversion supplies the scale Core
/// Animation needs when it composites the drawable into the content area.
///
/// `width`/`height` are the target drawable size in **physical pixels**. Returns
/// `true` if the drawable size or the backing scale actually changed from what
/// the layer already carries — the caller uses that to skip redundant work
/// (resetting an unchanged `drawableSize` restarts the drawable pool for no
/// reason). The layer frame follows the view bounds on every call, since the
/// view may have moved without its pixel size changing.
///
/// # Safety
///
/// `view` must be the live NSView that owns `layer`, and this must run on its
/// AppKit thread.
unsafe fn configure_layer_geometry(
    layer: &CAMetalLayer,
    view: &AnyObject,
    width: u32,
    height: u32,
) -> bool {
    // SAFETY: the caller guarantees `view` is a live NSView.
    let bounds: CGRect = unsafe { msg_send![view, bounds] };
    // SAFETY: the caller guarantees `view` is a live NSView.
    let backing_unit: CGSize = unsafe {
        msg_send![
            view,
            convertSizeToBacking: CGSize {
                width: 1.0,
                height: 1.0,
            }
        ]
    };
    layer.setFrame(bounds);

    let scale = backing_unit.width.max(backing_unit.height).max(1.0);
    let target = CGSize {
        width: width as f64,
        height: height as f64,
    };

    // Only touch the pixel-space geometry when it genuinely differs: each
    // `setDrawableSize`/`setContentsScale` invalidates and rebuilds the drawable
    // pool, so re-setting an unchanged value costs a needless reallocation.
    let current = layer.drawableSize();
    let size_changed = current.width != target.width || current.height != target.height;
    let scale_changed = layer.contentsScale() != scale;
    if scale_changed {
        layer.setContentsScale(scale);
    }
    if size_changed {
        layer.setDrawableSize(target);
    }
    size_changed || scale_changed
}

impl GpuBackend for MetalBackend {
    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        // Shared storage: CPU-visible, coherent on UMA — no `didModifyRange:`.
        let len = desc.size.max(1);
        let buffer = self
            .device
            .newBufferWithLength_options(len, MTLResourceOptions::StorageModeShared)
            .expect("failed to allocate a Metal buffer");
        self.buffers.insert(MetalBuffer { buffer, len }).into()
    }

    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId {
        // 2D, shared storage (CPU-writable on UMA via `replaceRegion`), sampled
        // in the fragment shader; render-target textures also get render usage.
        let td = MTLTextureDescriptor::new();
        td.setTextureType(objc2_metal::MTLTextureType::Type2D);
        td.setPixelFormat(pixel_format(desc.format));
        td.setStorageMode(objc2_metal::MTLStorageMode::Shared);
        let mut usage = MTLTextureUsage::ShaderRead;
        if desc.render_target {
            usage |= MTLTextureUsage::RenderTarget;
        }
        td.setUsage(usage);
        // SAFETY: setting positive, in-range dimensions and a single mip level on
        // a freshly-created descriptor upholds `MTLTextureDescriptor`'s invariants.
        unsafe {
            td.setWidth(desc.width as usize);
            td.setHeight(desc.height as usize);
            td.setMipmapLevelCount(1);
        }

        let texture = self
            .device
            .newTextureWithDescriptor(&td)
            .expect("failed to create a Metal texture");
        self.textures
            .insert(MetalTexture {
                texture,
                format: desc.format,
            })
            .into()
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        let sd = MTLSamplerDescriptor::new();
        // Min/mag texel filter, plus the mip filter for the trilinear variant.
        let (filter, mip) = match desc.filter {
            FilterMode::Nearest => (
                MTLSamplerMinMagFilter::Nearest,
                MTLSamplerMipFilter::NotMipmapped,
            ),
            FilterMode::Linear => (
                MTLSamplerMinMagFilter::Linear,
                MTLSamplerMipFilter::NotMipmapped,
            ),
            FilterMode::MipmapLinear => {
                (MTLSamplerMinMagFilter::Linear, MTLSamplerMipFilter::Linear)
            }
        };
        sd.setMinFilter(filter);
        sd.setMagFilter(filter);
        sd.setMipFilter(mip);
        let address = match desc.address {
            AddressMode::ClampToEdge => MTLSamplerAddressMode::ClampToEdge,
            AddressMode::Repeat => MTLSamplerAddressMode::Repeat,
            AddressMode::Mirror => MTLSamplerAddressMode::MirrorRepeat,
        };
        sd.setSAddressMode(address);
        sd.setTAddressMode(address);

        let state = self
            .device
            .newSamplerStateWithDescriptor(&sd)
            .expect("failed to create a Metal sampler state");
        self.samplers.insert(MetalSampler { state }).into()
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, crate::instance::LayoutError> {
        // Registration-time layout check, identical to the headless path.
        layout.validate_against(&desc.instance_schema)?;

        let source = NSString::from_str(desc.msl);
        let options = MTLCompileOptions::new();
        let library = self
            .device
            .newLibraryWithSource_options_error(&source, Some(&options))
            .expect("MSL compilation failed");
        self.library_compiles += 1;

        let vfn = NSString::from_str(desc.vertex_entry);
        let ffn = NSString::from_str(desc.fragment_entry);
        let vertex_fn = library
            .newFunctionWithName(&vfn)
            .expect("vertex entry point not found in MSL");
        let fragment_fn = library
            .newFunctionWithName(&ffn)
            .expect("fragment entry point not found in MSL");

        let pd = MTLRenderPipelineDescriptor::new();
        pd.setVertexFunction(Some(&vertex_fn));
        pd.setFragmentFunction(Some(&fragment_fn));

        // colorAttachments[0]: swapchain format + premultiplied over-blend.
        let color = pd.colorAttachments();
        // SAFETY: index 0 is a valid color-attachment slot.
        let attach = unsafe { color.objectAtIndexedSubscript(0) };
        attach.setPixelFormat(pixel_format(desc.color_format));
        match desc.blend {
            BlendMode::Replace => attach.setBlendingEnabled(false),
            BlendMode::PremultipliedOver => {
                attach.setBlendingEnabled(true);
                attach.setSourceRGBBlendFactor(MTLBlendFactor::One);
                attach.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
                attach.setRgbBlendOperation(MTLBlendOperation::Add);
                attach.setSourceAlphaBlendFactor(MTLBlendFactor::One);
                attach.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
                attach.setAlphaBlendOperation(MTLBlendOperation::Add);
            }
        }

        let state = self
            .device
            .newRenderPipelineStateWithDescriptor_error(&pd)
            .expect("failed to create Metal render pipeline state");

        Ok(self
            .pipelines
            .insert(MetalPipeline {
                state,
                builtin: desc.builtin,
            })
            .into())
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        self.bind_groups
            .insert(MetalBindGroup {
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
        // SAFETY: Shared-storage buffers expose their bytes via `contents()`; we
        // write within the validated range. No `didModifyRange:` needed.
        unsafe {
            let base = buf.buffer.contents().as_ptr() as *mut u8;
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(offset), bytes.len());
        }
    }

    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]) {
        let tex = self.texture(id);
        let bytes_per_row = w as usize * tex.format.bytes_per_texel();
        assert!(
            bytes.len() >= bytes_per_row * h as usize,
            "write_texture: {} bytes < {}x{} region of {}-byte texels",
            bytes.len(),
            w,
            h,
            tex.format.bytes_per_texel()
        );
        let region = MTLRegion {
            origin: MTLOrigin {
                x: x as usize,
                y: y as usize,
                z: 0,
            },
            size: MTLSize {
                width: w as usize,
                height: h as usize,
                depth: 1,
            },
        };
        let ptr = NonNull::new(bytes.as_ptr() as *mut c_void).unwrap();
        // SAFETY: `ptr` points at ≥ `bytes_per_row * h` valid bytes (asserted);
        // Metal copies them synchronously into the shared-storage texture.
        unsafe {
            tex.texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                ptr,
                bytes_per_row,
            );
        }
    }

    fn create_surface(&mut self, raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId {
        let ns_view = match raw {
            RawWindowHandle::AppKit { ns_view } => ns_view,
            other => panic!("MetalBackend requires an AppKit window handle, got {other:?}"),
        };

        let layer = CAMetalLayer::new();
        layer.setDevice(Some(&self.device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setPresentsWithTransaction(false);
        layer.setMaximumDrawableCount(3);
        layer.setDisplaySyncEnabled(true);

        // Attach the layer to the content NSView. viso-gpu does not depend on
        // objc2-app-kit, so we message the opaque view pointer directly.
        // SAFETY: `ns_view` is the live content `NSView` the platform layer
        // handed us via `RawWindowHandle::AppKit`; these selectors exist on
        // NSView.
        unsafe {
            let view = &*(ns_view as *const AnyObject);
            let _: () = msg_send![view, setWantsLayer: true];
            let _: () = msg_send![view, setLayer: &*layer];
            // Initial geometry: the return (whether it changed) is irrelevant on a
            // freshly created layer, which starts with no drawable pool.
            let _ = configure_layer_geometry(&layer, view, width, height);
        }

        self.surfaces
            .insert(MetalSurface {
                layer,
                view: ns_view,
                width,
                height,
                format: TextureFormat::Bgra8Unorm,
                current: None,
            })
            .into()
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        let s = self.surface_mut(id);
        // A resize or DPI change may arrive while a drawable is still held (the
        // window can resize mid-frame). Drop it: it was sized for the old
        // geometry, and reconfiguring the layer rebuilds the drawable pool under
        // it. The next `begin_frame` acquires a correctly sized drawable.
        s.current = None;
        s.width = width;
        s.height = height;
        // SAFETY: the originating platform window outlives its GPU surface.
        let view = unsafe { &*(s.view as *const AnyObject) };
        // SAFETY: `view` is a live NSView and layer geometry is updated on the
        // platform thread that owns it. `configure_layer_geometry` no-ops the
        // pixel-space writes when the physical size and scale are unchanged.
        unsafe {
            configure_layer_geometry(&s.layer, view, width, height);
        }
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame> {
        // Free slots parked by frames the GPU has since finished. (Reclamation is
        // gated by the completion fence, not the epoch, so it is safe to do before
        // knowing whether this acquire succeeds.)
        self.reclaim_completed();

        let s = self.surface_mut(surface);
        // Acquire the next drawable; hold it for encode + present. A nil drawable
        // means the layer is momentarily out of date (mid-resize) or its pool is
        // exhausted — not an error. Return `None` and, crucially, leave the epoch
        // where it is: advancing it here would park a frame that `present` never
        // completes, stalling the fence and the retire queue behind it.
        let drawable = s.layer.nextDrawable()?;
        s.current = Some(drawable);

        // A drawable is in hand: this is a real frame, so open its epoch. Any
        // `destroy_*` during it parks against this epoch and reclaims once the
        // matching `present` completes.
        self.current_epoch = self.current_epoch.next();
        Some(Frame {
            surface,
            drawable: 0,
        })
    }

    fn encode(&mut self, list: &DrawList<'_>) {
        autoreleasepool(|_| {
            for pass in list.passes {
                self.encode_pass(pass, &list.commands[pass.command_range()]);
            }
        });
    }

    fn present(&mut self, frame: Frame) {
        let epoch = self.current_epoch.0;
        let completed = Arc::clone(&self.completed);
        let s = self.surface_mut(frame.surface);
        if let Some(drawable) = s.current.take() {
            autoreleasepool(|_| {
                let cmd = self
                    .queue
                    .commandBuffer()
                    .expect("failed to create a command buffer for present");
                // Raise the shared completion counter to this frame's epoch when
                // the GPU finishes it. The block runs on a Metal-owned background
                // thread, so it may only touch the atomic — `fetch_max` keeps the
                // counter monotonic under out-of-order handler invocation. The
                // next `begin_frame` reads it to reclaim parked slots.
                let handler =
                    RcBlock::new(move |_cb: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                        completed.fetch_max(epoch, Ordering::Release);
                    });
                // SAFETY: `handler` is a valid block matching `MTLCommandBufferHandler`
                // (`Fn(NonNull<ProtocolObject<dyn MTLCommandBuffer>>)`); Metal copies
                // it and invokes it at most once, after which the `RcBlock` here is
                // dropped. It captures only an `Arc<AtomicU64>`, which is `Send`.
                unsafe {
                    cmd.addCompletedHandler(RcBlock::as_ptr(&handler));
                }
                cmd.presentDrawable(ProtocolObject::from_ref(&*drawable));
                cmd.commit();
            });
        }
    }

    fn device_lost(&mut self, surface: SurfaceId) {
        // Drop any drawable held between `begin_frame` and `present`: the present
        // that would have signalled its epoch will never run, so it must not be
        // returned to the compositor.
        self.surface_mut(surface).current = None;
        // Advance the shared completion counter to the current epoch so the frame
        // whose fence just went unsignalled — and every earlier one — is treated
        // as finished. Without this the retire queue would wait forever on a fence
        // no command buffer will ever raise, and parked slots would never reclaim.
        // `fetch_max` keeps it monotonic against any real completion handlers still
        // in flight. The next `begin_frame` reclaims cleanly and re-acquires a
        // fresh drawable; persistent resources are untouched by the loss.
        self.completed
            .fetch_max(self.current_epoch.0, Ordering::Release);
        self.reclaim_completed();
    }

    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn surface_format(&self, surface: SurfaceId) -> TextureFormat {
        self.surface(surface).format
    }
}

impl MetalBackend {
    /// Read back a texture's full contents as raw bytes (row-major, tightly
    /// packed at the texture's native `bytes_per_texel`), blocking until the GPU
    /// has finished any prior work on this queue.
    ///
    /// This is a synchronous, allocating read used by offscreen verification and
    /// golden tests — never a steady-state frame path. It commits an empty
    /// command buffer as a serial-queue barrier and waits for it, which
    /// guarantees a preceding `encode` (whose command buffer committed on the
    /// same queue) has completed before `getBytes` copies out of the
    /// shared-storage texture.
    pub fn read_texture(&self, id: TextureId) -> Vec<u8> {
        let tex = self.texture(id);
        let w = tex.texture.width();
        let h = tex.texture.height();
        let bpr = w * tex.format.bytes_per_texel();

        // Serial-queue barrier: an empty command buffer completes only after all
        // earlier command buffers on this queue have, so the offscreen render is
        // guaranteed done when this returns.
        let barrier = self
            .queue
            .commandBuffer()
            .expect("failed to create a read-back barrier command buffer");
        barrier.commit();
        barrier.waitUntilCompleted();

        let mut out = vec![0u8; bpr * h];
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: w,
                height: h,
                depth: 1,
            },
        };
        let ptr = NonNull::new(out.as_mut_ptr() as *mut c_void).unwrap();
        // SAFETY: `ptr` addresses `bpr * h` writable bytes (asserted by the
        // Vec allocation); the region is the whole texture at mip 0.
        unsafe {
            tex.texture
                .getBytes_bytesPerRow_fromRegion_mipmapLevel(ptr, bpr, region, 0);
        }
        out
    }

    /// Encode one render pass into a command buffer and commit it. The color
    /// attachment is either the surface's drawable (main pass) or an offscreen
    /// texture (a translucent Layer's render-to-texture target); the viewport is
    /// sized to whichever is bound. A drawable target is kept alive by
    /// `MetalSurface::current` until `present`.
    fn encode_pass(&mut self, pass: &RenderPass, commands: &[DrawCommand]) {
        // Resolve the color attachment texture and its size. The drawable is only
        // ready after `begin_frame`; bail if the surface has none yet.
        let (color_tex, width, height) = match pass.target {
            RenderTarget::Surface(frame) => {
                let s = self.surface(frame.surface);
                match &s.current {
                    Some(d) => (d.texture(), s.width, s.height),
                    None => return,
                }
            }
            RenderTarget::Texture(id) => {
                let tex = self.texture(id).texture.clone();
                let (w, h) = (tex.width() as u32, tex.height() as u32);
                (tex, w, h)
            }
        };

        let rpd = MTLRenderPassDescriptor::renderPassDescriptor();
        let attach = unsafe { rpd.colorAttachments().objectAtIndexedSubscript(0) };
        attach.setTexture(Some(&color_tex));
        match pass.load {
            LoadOp::Clear([r, g, b, a]) => {
                attach.setLoadAction(MTLLoadAction::Clear);
                attach.setClearColor(MTLClearColor {
                    red: r as f64,
                    green: g as f64,
                    blue: b as f64,
                    alpha: a as f64,
                });
            }
            LoadOp::Load => attach.setLoadAction(MTLLoadAction::Load),
        }
        attach.setStoreAction(MTLStoreAction::Store);

        let cmd = self
            .queue
            .commandBuffer()
            .expect("failed to create a command buffer");
        let encoder = cmd
            .renderCommandEncoderWithDescriptor(&rpd)
            .expect("failed to create a render command encoder");

        encoder.setViewport(MTLViewport {
            originX: 0.0,
            originY: 0.0,
            width: width as f64,
            height: height as f64,
            znear: 0.0,
            zfar: 1.0,
        });

        for c in commands {
            self.encode_command(&encoder, c, width, height);
        }

        encoder.endEncoding();
        cmd.commit();
    }

    /// Encode one draw command: set the optional scissor rect, bind pipeline,
    /// inline uniforms, then dispatch on [`Geometry`]. A [`Geometry::Generated`]
    /// draw binds the instance buffer @1 and issues an instanced non-indexed
    /// triangle draw whose vertex shader generates six corners from `vertex_id`;
    /// a [`Geometry::IndexedMesh`] draw binds a real vertex buffer @0 and issues
    /// one `drawIndexedPrimitives`. `surface_w`/`surface_h` bound the scissor
    /// rect (Metal errors on an out-of-bounds `setScissorRect`).
    fn encode_command(
        &self,
        encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        c: &DrawCommand,
        surface_w: u32,
        surface_h: u32,
    ) {
        // A scissor of `None` means "full surface"; set the whole surface so a
        // prior command's scissor never leaks into this one within the pass.
        let (sx, sy, sw, sh) = match c.scissor {
            Some((x, y, w, h)) => {
                // Clamp into the surface: origin ≤ extent, width/height fit.
                let x = x.min(surface_w);
                let y = y.min(surface_h);
                let w = w.min(surface_w - x);
                let h = h.min(surface_h - y);
                (x, y, w, h)
            }
            None => (0, 0, surface_w, surface_h),
        };
        encoder.setScissorRect(MTLScissorRect {
            x: sx as usize,
            y: sy as usize,
            width: sw as usize,
            height: sh as usize,
        });

        encoder.setRenderPipelineState(&self.pipeline(c.pipeline).state);

        // Bind the fragment texture @0 and sampler @0 from the bind group, if
        // any (image/glyph draws). The MSL declares `texture(0)`/`sampler(0)`.
        if let Some(bg) = c.bind_group {
            for binding in &self.bind_group(bg).bindings {
                match binding {
                    Binding::Texture(tid) => {
                        let t = self.texture(*tid);
                        unsafe {
                            encoder.setFragmentTexture_atIndex(Some(&t.texture), 0);
                        }
                    }
                    Binding::Sampler(sid) => {
                        let s = self.sampler(*sid);
                        unsafe {
                            encoder.setFragmentSamplerState_atIndex(Some(&s.state), 0);
                        }
                    }
                    // Uniform buffers in a bind group carry no data for the
                    // inline-uniform built-ins; ignore.
                    Binding::Uniform(_) => {}
                }
            }
        }

        match c.geometry {
            Geometry::Generated { count } => {
                // Inline uniforms at buffer index 0 (both stages): the quad/image
                // built-ins put the instance buffer at index 1 and uniforms at 0.
                let uniforms = c.uniforms.as_bytes();
                if !uniforms.is_empty() {
                    let ptr = NonNull::new(uniforms.as_ptr() as *mut c_void).unwrap();
                    // SAFETY: `ptr` points at `uniforms.len()` valid bytes for
                    // the duration of the call; Metal copies them immediately.
                    unsafe {
                        encoder.setVertexBytes_length_atIndex(ptr, uniforms.len(), 0);
                        encoder.setFragmentBytes_length_atIndex(ptr, uniforms.len(), 0);
                    }
                }

                // Per-instance data at buffer index 1 (both stages), offset into
                // the persistent instance buffer.
                let inst = self.buffer(c.instance_buffer);
                // SAFETY: the offset is within the allocated buffer (renderer
                // guarantees `instance_offset + count*stride <= len`); index 1
                // matches the MSL.
                unsafe {
                    encoder.setVertexBuffer_offset_atIndex(
                        Some(&inst.buffer),
                        c.instance_offset,
                        1,
                    );
                    encoder.setFragmentBuffer_offset_atIndex(
                        Some(&inst.buffer),
                        c.instance_offset,
                        1,
                    );
                }

                // Six vertices (two triangles) per instance; corners from
                // `vertex_id`.
                // SAFETY: valid vertex range and a positive instance count.
                unsafe {
                    encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                        MTLPrimitiveType::Triangle,
                        0,
                        6,
                        count as usize,
                    );
                }
            }
            Geometry::IndexedMesh {
                vertex_buffer,
                index_buffer,
                index_format,
                index_offset,
                index_count,
            } => {
                // The mesh vertex buffer occupies vertex buffer index 0, so the
                // inline uniforms move to index 1 (the mesh MSL binds uniforms at
                // `[[buffer(1)]]`). Only the vertex stage reads the mesh vertices;
                // the fragment stage needs no per-vertex buffer.
                let uniforms = c.uniforms.as_bytes();
                if !uniforms.is_empty() {
                    let ptr = NonNull::new(uniforms.as_ptr() as *mut c_void).unwrap();
                    // SAFETY: `ptr` points at `uniforms.len()` valid bytes for
                    // the duration of the call; Metal copies them immediately.
                    unsafe {
                        encoder.setVertexBytes_length_atIndex(ptr, uniforms.len(), 1);
                    }
                }

                // Real per-vertex geometry at vertex buffer index 0; the MSL
                // reads `verts[vertex_id]` from `[[buffer(0)]]`.
                let vtx = self.buffer(vertex_buffer);
                // SAFETY: buffer id is valid; index 0 matches the mesh MSL.
                unsafe {
                    encoder.setVertexBuffer_offset_atIndex(Some(&vtx.buffer), 0, 0);
                }

                // One indexed triangle-list draw over this segment's index range.
                // `indexBufferOffset` is in bytes; each index is `index_format`
                // wide (2 for U16, 4 for U32).
                let idx = self.buffer(index_buffer);
                let (index_type, elem) = match index_format {
                    IndexFormat::U16 => (MTLIndexType::UInt16, 2usize),
                    IndexFormat::U32 => (MTLIndexType::UInt32, 4usize),
                };
                let byte_offset = index_offset as usize * elem;
                // SAFETY: `index_offset + index_count` indices of `index_format`
                // width fit within the index buffer (renderer guarantees the
                // buffer size); `index_type` matches the buffer's element width.
                unsafe {
                    encoder
                        .drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset(
                            MTLPrimitiveType::Triangle,
                            index_count as usize,
                            index_type,
                            &idx.buffer,
                            byte_offset,
                        );
                }
            }
        }
    }
}

/// Map a Viso [`TextureFormat`] to its Metal pixel format.
fn pixel_format(f: TextureFormat) -> MTLPixelFormat {
    match f {
        TextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        TextureFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        TextureFormat::R8Unorm => MTLPixelFormat::R8Unorm,
        TextureFormat::Rgba16Float => MTLPixelFormat::RGBA16Float,
        TextureFormat::Depth32Float => MTLPixelFormat::Depth32Float,
    }
}
