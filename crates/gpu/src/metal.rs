//! `MetalBackend` — the native macOS Metal implementation of [`GpuBackend`].
//!
//! This is a *native rewrite* of makepad's Metal command sequence (behavior and
//! operation order ported from `platform/src/os/apple/metal.rs`), built directly
//! on `objc2-metal` 0.3 / `objc2-quartz-core` 0.3 — not a port of makepad's RHI
//! structure. It compiles only on macOS; other targets use [`HeadlessRaster`].
//!
//! ## Design decisions (diverging from makepad where it simplifies)
//!
//! - **Buffers use `StorageModeShared`** (`contents()` + memcpy), not makepad's
//!   Managed/`didModifyRange:` path. On Apple-Silicon UMA this is the simple
//!   modern default and avoids the explicit-flush bookkeeping.
//! - **No `MTLVertexDescriptor`.** The Quad vertex shader generates the four
//!   corner positions from `vertex_id` and reads per-instance data from a raw
//!   buffer bound at index 1 (the same convention as the RHI's `DrawCommand`).
//! - **Uniforms are inline** via `setVertexBytes:`/`setFragmentBytes:` — the
//!   renderer passes the surface `[width, height]` so the shader maps pixel-space
//!   rects to NDC. (Headless ignores uniforms; it works in pixel space.)
//! - Every frame's acquire/encode/present runs inside an `autoreleasepool` so the
//!   per-frame Metal autoreleased objects (drawable, command buffer, encoder) are
//!   drained each frame rather than piling up (Phase 1 lesson).
//!
//! Blend is premultiplied source-over (`src One`, `dst OneMinusSourceAlpha`,
//! op `Add`), matching the headless raster and the makepad recipe.

use core::ffi::c_void;
use core::ptr::NonNull;

use objc2::msg_send;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_foundation::CGSize;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLBuffer, MTLClearColor, MTLCommandBuffer,
    MTLCommandEncoder, MTLCommandQueue, MTLCompileOptions, MTLCreateSystemDefaultDevice, MTLDevice,
    MTLIndexType, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType,
    MTLRegion, MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLResourceOptions, MTLSamplerAddressMode, MTLSamplerDescriptor,
    MTLSamplerMinMagFilter, MTLSamplerState, MTLScissorRect, MTLSize, MTLStoreAction, MTLTexture,
    MTLTextureDescriptor, MTLTextureUsage, MTLViewport,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use viso_handle::RawWindowHandle;

use crate::backend::{
    DrawCommand, DrawList, Frame, Geometry, GpuBackend, LoadOp, RenderPass, RenderTarget,
};
use crate::instance::InstanceLayout;
use crate::resource::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BuiltinShader, Caps, FilterMode,
    PipelineDesc, SamplerDesc, TextureDesc, TextureFormat,
};
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
    buffers: Vec<MetalBuffer>,
    textures: Vec<MetalTexture>,
    samplers: Vec<MetalSampler>,
    bind_groups: Vec<MetalBindGroup>,
    pipelines: Vec<MetalPipeline>,
    surfaces: Vec<MetalSurface>,
    caps: Caps,
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
            buffers: Vec::new(),
            textures: Vec::new(),
            samplers: Vec::new(),
            bind_groups: Vec::new(),
            pipelines: Vec::new(),
            surfaces: Vec::new(),
            caps: Caps {
                max_texture_size,
                presents_to_display: true,
            },
        }
    }
}

impl GpuBackend for MetalBackend {
    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        // Shared storage: CPU-visible, coherent on UMA — no `didModifyRange:`.
        let len = desc.size.max(1);
        let buffer = self
            .device
            .newBufferWithLength_options(len, MTLResourceOptions::StorageModeShared)
            .expect("failed to allocate a Metal buffer");
        let id = BufferId(self.buffers.len() as u32);
        self.buffers.push(MetalBuffer { buffer, len });
        id
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
        let id = TextureId(self.textures.len() as u32);
        self.textures.push(MetalTexture {
            texture,
            format: desc.format,
        });
        id
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        let sd = MTLSamplerDescriptor::new();
        let filter = match desc.filter {
            FilterMode::Nearest => MTLSamplerMinMagFilter::Nearest,
            FilterMode::Linear => MTLSamplerMinMagFilter::Linear,
        };
        sd.setMinFilter(filter);
        sd.setMagFilter(filter);
        let address = match desc.address {
            AddressMode::ClampToEdge => MTLSamplerAddressMode::ClampToEdge,
            AddressMode::Repeat => MTLSamplerAddressMode::Repeat,
        };
        sd.setSAddressMode(address);
        sd.setTAddressMode(address);

        let state = self
            .device
            .newSamplerStateWithDescriptor(&sd)
            .expect("failed to create a Metal sampler state");
        let id = SamplerId(self.samplers.len() as u32);
        self.samplers.push(MetalSampler { state });
        id
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, crate::instance::LayoutError> {
        // Registration-time layout check (§32), identical to the headless path.
        layout.validate_against(&desc.instance_schema)?;

        let source = NSString::from_str(desc.shader_source);
        let options = MTLCompileOptions::new();
        let library = self
            .device
            .newLibraryWithSource_options_error(&source, Some(&options))
            .expect("MSL compilation failed");

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

        let id = PipelineId(self.pipelines.len() as u32);
        self.pipelines.push(MetalPipeline {
            state,
            builtin: desc.builtin,
        });
        Ok(id)
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        let id = BindGroupId(self.bind_groups.len() as u32);
        self.bind_groups.push(MetalBindGroup {
            bindings: desc.bindings.clone(),
        });
        id
    }

    fn write_buffer(&mut self, id: BufferId, offset: usize, bytes: &[u8]) {
        let buf = &self.buffers[id.0 as usize];
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
        let tex = &self.textures[id.0 as usize];
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
        layer.setDrawableSize(CGSize {
            width: width as f64,
            height: height as f64,
        });

        // Attach the layer to the content NSView. viso-gpu does not depend on
        // objc2-app-kit, so we message the opaque view pointer directly.
        // SAFETY: `ns_view` is the live content `NSView` the platform layer
        // handed us via `RawWindowHandle::AppKit`; these selectors exist on
        // NSView. `setLayerContentsPlacement: 11` = NSViewLayerContentsPlacementTopLeft.
        unsafe {
            let view = &*(ns_view as *const AnyObject);
            let _: () = msg_send![view, setWantsLayer: true];
            let _: () = msg_send![view, setLayer: &*layer];
            let _: () = msg_send![view, setLayerContentsPlacement: 11isize];
        }

        let id = SurfaceId(self.surfaces.len() as u32);
        self.surfaces.push(MetalSurface {
            layer,
            width,
            height,
            format: TextureFormat::Bgra8Unorm,
            current: None,
        });
        id
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        let s = &mut self.surfaces[id.0 as usize];
        s.width = width;
        s.height = height;
        s.layer.setDrawableSize(CGSize {
            width: width as f64,
            height: height as f64,
        });
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Frame {
        let s = &mut self.surfaces[surface.0 as usize];
        // Acquire the next drawable; hold it for encode + present.
        let drawable = s.layer.nextDrawable();
        s.current = drawable;
        Frame {
            surface,
            drawable: 0,
        }
    }

    fn encode(&mut self, list: &DrawList<'_>) {
        autoreleasepool(|_| {
            for pass in list.passes {
                self.encode_pass(pass, &list.commands[pass.command_range()]);
            }
        });
    }

    fn present(&mut self, frame: Frame) {
        let s = &mut self.surfaces[frame.surface.0 as usize];
        if let Some(drawable) = s.current.take() {
            autoreleasepool(|_| {
                let cmd = self
                    .queue
                    .commandBuffer()
                    .expect("failed to create a command buffer for present");
                cmd.presentDrawable(ProtocolObject::from_ref(&*drawable));
                cmd.commit();
            });
        }
    }

    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn surface_format(&self, surface: SurfaceId) -> TextureFormat {
        self.surfaces[surface.0 as usize].format
    }
}

impl MetalBackend {
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
                let s = &self.surfaces[frame.surface.0 as usize];
                match &s.current {
                    Some(d) => (d.texture(), s.width, s.height),
                    None => return,
                }
            }
            RenderTarget::Texture(id) => {
                let tex = self.textures[id.0 as usize].texture.clone();
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

        let pipeline = &self.pipelines[c.pipeline.0 as usize];
        encoder.setRenderPipelineState(&pipeline.state);

        // Bind the fragment texture @0 and sampler @0 from the bind group, if
        // any (image/glyph draws). The MSL declares `texture(0)`/`sampler(0)`.
        if let Some(bg) = c.bind_group {
            let group = &self.bind_groups[bg.0 as usize];
            for binding in &group.bindings {
                match binding {
                    Binding::Texture(tid) => {
                        let t = &self.textures[tid.0 as usize];
                        unsafe {
                            encoder.setFragmentTexture_atIndex(Some(&t.texture), 0);
                        }
                    }
                    Binding::Sampler(sid) => {
                        let s = &self.samplers[sid.0 as usize];
                        unsafe {
                            encoder.setFragmentSamplerState_atIndex(Some(&s.state), 0);
                        }
                    }
                    // Uniform buffers in a bind group are not used by the Phase 2
                    // built-ins (uniforms are inline); ignore for now.
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
                let inst = &self.buffers[c.instance_buffer.0 as usize];
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
                let vtx = &self.buffers[vertex_buffer.0 as usize];
                // SAFETY: buffer id is valid; index 0 matches the mesh MSL.
                unsafe {
                    encoder.setVertexBuffer_offset_atIndex(Some(&vtx.buffer), 0, 0);
                }

                // One indexed triangle-list draw over this segment's index range.
                // `indexBufferOffset` is in bytes; each index is a `u32`.
                let idx = &self.buffers[index_buffer.0 as usize];
                let byte_offset = index_offset as usize * core::mem::size_of::<u32>();
                // SAFETY: `index_offset + index_count` u32 indices fit within the
                // index buffer (renderer guarantees the buffer size); UInt32
                // matches the renderer's index type.
                unsafe {
                    encoder
                        .drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset(
                            MTLPrimitiveType::Triangle,
                            index_count as usize,
                            MTLIndexType::UInt32,
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
        TextureFormat::Depth32Float => MTLPixelFormat::Depth32Float,
    }
}
