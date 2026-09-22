//! `HeadlessRaster` — a CPU software rasterizer implementing [`GpuBackend`].
//!
//! This backend exists so golden-image tests (and CI machines without a GPU)
//! can render a full frame and read the pixels back. It has no shader compiler:
//! it dispatches on each pipeline's [`BuiltinShader`] tag to a hand-written CPU
//! fill routine that reproduces the corresponding Metal shader's SDF / AA /
//! blend math. The output is intended to match the Metal backend to within a
//! small per-channel tolerance.
//!
//! ## Fidelity model
//!
//! The fill routines re-derive each shader's math in plain Rust so the output
//! matches the GPU backend's observable behavior to within tolerance:
//!
//! - **Framebuffer:** RGBA `f32`, **premultiplied**, linear (not sRGB-encoded).
//! - **Rounded-rect SDF** (IQ box): `k = min(2*r, min(halfw, halfh))` — the
//!   radius is *doubled* then clamped to the half-size; `d = length(max(|p - c|
//!   - (half - k), 0)) - k`, negative inside.
//! - **Anti-aliasing:** `coverage = clamp(-d * aa, 0, 1)` with `aa ≈ 1` at 1:1
//!   scale (linear ramp over ~1px; the `d == 0` iso-line is the zero-coverage
//!   boundary — *not* smoothstep, *not* a symmetric 0.5 edge).
//! - **Border/stroke:** `coverage = clamp(-(|d| - halfwidth) * aa, 0, 1)`.
//! - **Blend:** premultiplied source-over `out = src + dst * (1 - src.a)`,
//!   applied per channel, after quantizing the source to 8-bit.
//! - **8-bit quantize before blend:** `round(clamp(v,0,1) * 255) / 255`, so the
//!   result is byte-exact against a real Bgra8 target.
//! - **Readback:** un-premultiply (`rgb / a`), clamp, `round(v * 255)`, pack
//!   BGRA8 top-left — matching a Metal texture readback.

use viso_handle::RawWindowHandle;

use crate::backend::{
    DrawCommand, DrawList, Frame, Geometry, GpuBackend, IndexFormat, LoadOp, RenderPass,
    RenderTarget,
};
use crate::instance::{AttrFormat, InstanceLayout};
use crate::resource::{
    BindGroupDesc, BufferDesc, BuiltinShader, Caps, ColorSpace, PipelineDesc, SamplerDesc,
    TextureDesc, TextureFormat, f16_to_f32, f32_to_f16,
};
use crate::retire::{Epoch, Fence, ResourceKind, RetireQueue, Retired};
use crate::slots::SlotMap;
use crate::{BindGroupId, BufferId, PipelineId, SamplerId, SurfaceId, TextureId};

/// A CPU-resident buffer: just its bytes.
struct HeadlessBuffer {
    bytes: Vec<u8>,
}

/// A CPU-resident texture: RGBA-f32 premultiplied linear texels, or a single
/// coverage channel expanded to RGBA on write for uniform sampling.
struct HeadlessTexture {
    width: u32,
    height: u32,
    format: TextureFormat,
    /// Premultiplied linear RGBA, one `[f32; 4]` per texel, row-major top-left.
    texels: Vec<[f32; 4]>,
}

/// A registered pipeline: the built-in program tag plus the validated instance
/// layout, which the fill routines use to locate fields inside instance bytes.
struct HeadlessPipeline {
    builtin: BuiltinShader,
    layout: InstanceLayout,
}

/// A registered bind group (its bindings, in slot order).
struct HeadlessBindGroup {
    /// The bindings (texture/sampler/uniform) in slot order, resolved by the
    /// Image fill to sample the bound texture with the bound sampler.
    bindings: Vec<crate::resource::Binding>,
}

/// A surface backed by a CPU framebuffer (RGBA-f32 premultiplied linear).
struct HeadlessSurface {
    width: u32,
    height: u32,
    format: TextureFormat,
    /// The space the compositor would read this framebuffer through. The raster
    /// itself is space-agnostic — every framebuffer is premultiplied linear
    /// `f32` — so this exists to let a test stand in for a wide-gamut or HDR
    /// swapchain and observe what the renderer plans for one.
    color_space: ColorSpace,
    /// Premultiplied linear RGBA, row-major top-left.
    color: Vec<[f32; 4]>,
}

/// A CPU software-rasterizer backend.
pub struct HeadlessRaster {
    buffers: SlotMap<HeadlessBuffer>,
    textures: SlotMap<HeadlessTexture>,
    samplers: SlotMap<SamplerDesc>,
    pipelines: SlotMap<HeadlessPipeline>,
    bind_groups: SlotMap<HeadlessBindGroup>,
    surfaces: SlotMap<HeadlessSurface>,
    caps: Caps,
    /// Resources awaiting reclamation, parked with the epoch they were retired in.
    retire_queue: RetireQueue,
    /// Highest epoch the GPU has finished. Headless completes each frame the moment
    /// it is presented, so this trails `current_epoch` by exactly the in-flight frame.
    fence: Fence,
    /// The epoch of the frame currently being built. Advanced by `begin_frame`.
    current_epoch: Epoch,
    /// Scratch reused across `begin_frame` drains so reclamation allocates nothing.
    reclaim_scratch: Vec<Retired>,
}

impl Default for HeadlessRaster {
    fn default() -> Self {
        Self::new()
    }
}

impl HeadlessRaster {
    /// Create an empty headless backend.
    pub fn new() -> Self {
        Self {
            buffers: SlotMap::new(),
            textures: SlotMap::new(),
            samplers: SlotMap::new(),
            pipelines: SlotMap::new(),
            bind_groups: SlotMap::new(),
            surfaces: SlotMap::new(),
            caps: Caps {
                max_texture_size: 16384,
                presents_to_display: false,
                compute_dispatch: false,
                bindless_texture_slots: 0,
            },
            retire_queue: RetireQueue::new(),
            fence: Fence::new(),
            current_epoch: Epoch::START,
            reclaim_scratch: Vec::new(),
        }
    }

    /// The number of resources still parked awaiting reclamation.
    ///
    /// A steady-state frame that grows a buffer parks one slot and reclaims it a
    /// frame later, so this stays bounded and drains to zero — the renderer bench
    /// asserts the queue does not grow without bound.
    pub fn retired_count(&self) -> usize {
        self.retire_queue.len()
    }

    /// Reclaim every parked slot whose retire epoch the fence has now passed,
    /// returning each freed slot to its store's free-list (which bumps the slot
    /// generation, so the retired handle goes stale). Called from `begin_frame`.
    fn reclaim_completed(&mut self) {
        self.reclaim_scratch.clear();
        self.retire_queue
            .drain_completed(self.fence, &mut self.reclaim_scratch);
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

    /// Read the last-presented framebuffer of `surface` as tightly-packed
    /// **BGRA8** bytes, top-left origin (un-premultiplied, `round(v * 255)`).
    ///
    /// This is the golden-test capture path; it mirrors a Metal texture
    /// readback of a `Bgra8Unorm` swapchain.
    pub fn read_pixels_bgra8(&self, surface: SurfaceId) -> Vec<u8> {
        let s = self.surface(surface);
        let mut out = Vec::with_capacity(s.color.len() * 4);
        for &[r, g, b, a] in &s.color {
            let (ur, ug, ub) = unpremultiply(r, g, b, a);
            out.push(to_unorm8(ub)); // B
            out.push(to_unorm8(ug)); // G
            out.push(to_unorm8(ur)); // R
            out.push(to_unorm8(a)); // A
        }
        out
    }

    /// Read the last-presented framebuffer of `surface` as tightly-packed
    /// **RGBA16F** bytes, top-left origin, premultiplied — the readback an
    /// extended-range swapchain gives.
    ///
    /// Unlike [`read_pixels_bgra8`](Self::read_pixels_bgra8) this preserves values
    /// above `1.0`, so a test can tell an HDR highlight that survived the frame
    /// from one an 8-bit stage clipped on the way through.
    pub fn read_pixels_rgba16f(&self, surface: SurfaceId) -> Vec<u8> {
        let s = self.surface(surface);
        let mut out = Vec::with_capacity(s.color.len() * 8);
        for texel in &s.color {
            for &c in texel {
                out.extend_from_slice(&f32_to_f16(c).to_le_bytes());
            }
        }
        out
    }

    /// Stand this surface in for a wide-gamut or HDR swapchain.
    ///
    /// The raster is format-agnostic internally — every framebuffer is
    /// premultiplied linear `f32` — so this changes what the surface *reports*,
    /// which is exactly what the renderer plans its intermediate formats from. It
    /// lets a headless test exercise the HDR plan on a machine with no HDR display
    /// attached.
    pub fn set_surface_color_target(
        &mut self,
        surface: SurfaceId,
        format: TextureFormat,
        color_space: ColorSpace,
    ) {
        let s = self
            .surfaces
            .get_mut(surface.into())
            .expect("surface handle does not resolve");
        s.format = format;
        s.color_space = color_space;
    }

    /// Sample one texel `[f32; 4]` (premultiplied linear) from `surface` at
    /// pixel `(x, y)`, top-left origin. Convenience for single-pixel assertions.
    pub fn surface_texel(&self, surface: SurfaceId, x: u32, y: u32) -> [f32; 4] {
        let s = self.surface(surface);
        s.color[(y * s.width + x) as usize]
    }

    /// Resolve a surface handle, panicking on a stale/unknown one (an internal
    /// invariant break — a live handle always resolves in these backends).
    fn surface(&self, id: SurfaceId) -> &HeadlessSurface {
        self.surfaces
            .get(id.into())
            .expect("surface handle does not resolve")
    }

    /// The number of buffers created over this backend's lifetime.
    ///
    /// This is the store's cumulative create count (`SlotMap::created`), which
    /// only grows on a genuine create — never on a lookup or a slot reuse. A
    /// steady-state frame that reuses persistent buffers leaves it unchanged,
    /// which the renderer bench asserts to guard the hot-path allocation
    /// contract.
    pub fn buffer_count(&self) -> usize {
        self.buffers.created()
    }

    /// The number of textures created over this backend's lifetime.
    ///
    /// Like [`buffer_count`](Self::buffer_count), this only grows on a genuine
    /// create; a steady-state frame whose translucent layers reuse pooled
    /// offscreen textures leaves it unchanged.
    pub fn texture_count(&self) -> usize {
        self.textures.created()
    }

    /// The number of bind groups created over this backend's lifetime.
    ///
    /// Like [`buffer_count`](Self::buffer_count), this only grows on a genuine
    /// create; cached per-texture bind groups keep it stable across steady
    /// frames.
    pub fn bind_group_count(&self) -> usize {
        self.bind_groups.created()
    }
}

impl GpuBackend for HeadlessRaster {
    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        self.buffers
            .insert(HeadlessBuffer {
                bytes: vec![0u8; desc.size],
            })
            .into()
    }

    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId {
        let count = (desc.width * desc.height) as usize;
        self.textures
            .insert(HeadlessTexture {
                width: desc.width,
                height: desc.height,
                format: desc.format,
                texels: vec![[0.0; 4]; count],
            })
            .into()
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        self.samplers.insert(*desc).into()
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, crate::instance::LayoutError> {
        // Registration-time layout check: the derived instance layout must
        // match the shader's declared schema before the pipeline is usable.
        layout.validate_against(&desc.instance_schema)?;
        Ok(self
            .pipelines
            .insert(HeadlessPipeline {
                builtin: desc.builtin,
                layout: *layout,
            })
            .into())
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        self.bind_groups
            .insert(HeadlessBindGroup {
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
        let buf = self
            .buffers
            .get_mut(id.into())
            .expect("buffer handle does not resolve");
        buf.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]) {
        let tex = self
            .textures
            .get_mut(id.into())
            .expect("texture handle does not resolve");
        let bpt = tex.format.bytes_per_texel();
        for row in 0..h {
            for col in 0..w {
                let src = ((row * w + col) as usize) * bpt;
                let dst = ((y + row) * tex.width + (x + col)) as usize;
                tex.texels[dst] = decode_texel(tex.format, &bytes[src..src + bpt]);
            }
        }
    }

    fn create_surface(&mut self, _raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId {
        // The headless backend ignores the (Headless) handle; it just allocates
        // a CPU framebuffer of the requested size.
        self.surfaces
            .insert(HeadlessSurface {
                width,
                height,
                format: TextureFormat::Bgra8Unorm,
                color_space: ColorSpace::Srgb,
                color: vec![[0.0; 4]; (width * height) as usize],
            })
            .into()
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        let s = self
            .surfaces
            .get_mut(id.into())
            .expect("surface handle does not resolve");
        s.width = width;
        s.height = height;
        s.color = vec![[0.0; 4]; (width * height) as usize];
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame> {
        // Reclaim slots retired in epochs the GPU has finished, then open the next
        // frame. A resource destroyed in epoch N is thus reclaimed no earlier than
        // the begin_frame after N was presented — a genuine one-frame deferral. A
        // CPU framebuffer is never out of date, so acquisition always succeeds.
        self.reclaim_completed();
        self.current_epoch = self.current_epoch.next();
        Some(Frame {
            surface,
            drawable: 0,
        })
    }

    fn encode(&mut self, list: &DrawList<'_>) {
        for pass in list.passes {
            self.encode_pass(pass, &list.commands[pass.command_range()]);
        }
    }

    fn present(&mut self, _frame: Frame) {
        // No swapchain: the framebuffer already holds the final image, ready for
        // `read_pixels_bgra8`. With no asynchronous GPU, the presented frame is
        // finished the instant it is presented, so signal its epoch complete; the
        // next begin_frame will then reclaim anything retired in it.
        self.fence.signal(self.current_epoch);
    }

    fn device_lost(&mut self, _surface: SurfaceId) {
        // No GPU, no drawable, no async completion: there is nothing to rebuild.
        // The one obligation is the same as a real backend's — a frame that was
        // opened but never presented must not leave the fence behind the current
        // epoch, or parked slots would never reclaim. Signal the current epoch to
        // complete any such in-flight frame, then drain so the stalled queue is
        // freed here rather than waiting on a begin_frame that may never come.
        self.fence.signal(self.current_epoch);
        self.reclaim_completed();
    }

    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn surface_format(&self, surface: SurfaceId) -> TextureFormat {
        self.surface(surface).format
    }

    fn surface_color_space(&self, surface: SurfaceId) -> ColorSpace {
        self.surface(surface).color_space
    }
}

/// Where a pass's rasterized pixels land: the swapchain surface, or an
/// offscreen texture (a translucent Layer's render-to-texture target). Both
/// framebuffers are `Vec<[f32; 4]>` premultiplied linear RGBA, so a single set
/// of fill routines writes into either by resolving the backing slice per pixel.
#[derive(Clone, Copy)]
enum FbTarget {
    Surface(SurfaceId),
    Texture(TextureId),
}

impl HeadlessRaster {
    /// Rasterize one render pass into its target framebuffer — the swapchain
    /// surface, or an offscreen texture for a translucent Layer.
    fn encode_pass(&mut self, pass: &RenderPass, commands: &[DrawCommand]) {
        let (target, width, height) = match pass.target {
            RenderTarget::Surface(frame) => {
                let s = self.surface(frame.surface);
                (FbTarget::Surface(frame.surface), s.width, s.height)
            }
            RenderTarget::Texture(id) => {
                let t = self.texture(id);
                (FbTarget::Texture(id), t.width, t.height)
            }
        };

        if let LoadOp::Clear(rgba) = pass.load {
            self.framebuffer(target).fill(rgba);
        }

        for cmd in commands {
            self.encode_command(target, width, height, cmd);
        }
    }

    /// The framebuffer slice for `target`, for a single blend write. Scoped per
    /// call to keep the `&mut self` borrow narrow (the fill routines snapshot any
    /// sampled source texture out first, so a texture target never aliases here).
    fn framebuffer(&mut self, target: FbTarget) -> &mut [[f32; 4]] {
        match target {
            FbTarget::Surface(id) => {
                &mut self
                    .surfaces
                    .get_mut(id.into())
                    .expect("surface handle does not resolve")
                    .color
            }
            FbTarget::Texture(id) => {
                &mut self
                    .textures
                    .get_mut(id.into())
                    .expect("texture handle does not resolve")
                    .texels
            }
        }
    }

    /// The declared pixel format of `target` — what the rasterizer quantizes its
    /// writes to, so the readback matches a real attachment of that format.
    fn target_format(&self, target: FbTarget) -> TextureFormat {
        match target {
            FbTarget::Surface(id) => self.surface(id).format,
            FbTarget::Texture(id) => self.texture(id).format,
        }
    }

    /// Premultiplied source-over into one pixel of `target`, quantizing the
    /// source to the attachment's format first so the result matches a real
    /// target of that format.
    ///
    /// On an extended-range attachment there is no quantization step at all: a
    /// value above `1.0` is exactly what such a target stores, and rounding it
    /// into 8 bits here would clip an HDR intermediate in the one place a test
    /// could never see it (§19).
    fn blend_pixel(&mut self, target: FbTarget, width: u32, px: u32, py: u32, src: [f32; 4]) {
        let src = quantize_for(self.target_format(target), src);
        let fb = self.framebuffer(target);
        let idx = (py * width + px) as usize;
        let dst = fb[idx];
        let inv = 1.0 - src[3];
        fb[idx] = [
            src[0] + dst[0] * inv,
            src[1] + dst[1] * inv,
            src[2] + dst[2] * inv,
            src[3] + dst[3] * inv,
        ];
    }

    /// Overwrite one pixel of `target` — the raster equivalent of
    /// [`BlendMode::Replace`](crate::BlendMode::Replace).
    ///
    /// This backend's other fills all end in [`blend_pixel`](Self::blend_pixel)
    /// because every other pipeline composites premultiplied source-over. The
    /// advanced-blend fragment already folded the destination into its result, so
    /// blending it a second time would double-count it: the value is written
    /// as-is, quantized like every other path.
    fn write_pixel(&mut self, target: FbTarget, width: u32, px: u32, py: u32, src: [f32; 4]) {
        let src = quantize_for(self.target_format(target), src);
        self.framebuffer(target)[(py * width + px) as usize] = src;
    }

    /// Resolve a texture handle, panicking on a stale/unknown one (an internal
    /// invariant break — a live handle always resolves in these backends).
    fn texture(&self, id: TextureId) -> &HeadlessTexture {
        self.textures
            .get(id.into())
            .expect("texture handle does not resolve")
    }

    /// Resolve a buffer handle (see [`texture`](Self::texture) for the panic).
    fn buffer(&self, id: BufferId) -> &HeadlessBuffer {
        self.buffers
            .get(id.into())
            .expect("buffer handle does not resolve")
    }

    /// Resolve a bind-group handle (see [`texture`](Self::texture)).
    fn bind_group(&self, id: BindGroupId) -> &HeadlessBindGroup {
        self.bind_groups
            .get(id.into())
            .expect("bind group handle does not resolve")
    }

    /// Resolve a sampler handle (see [`texture`](Self::texture)).
    fn sampler(&self, id: SamplerId) -> SamplerDesc {
        *self
            .samplers
            .get(id.into())
            .expect("sampler handle does not resolve")
    }

    /// Rasterize one draw command's instances into the target framebuffer.
    fn encode_command(&mut self, target: FbTarget, width: u32, height: u32, cmd: &DrawCommand) {
        // Copy the pipeline metadata (both `Copy`) so the per-pixel fill can take
        // `&mut self` for blending without aliasing the pipeline/buffer tables.
        let pipeline = self
            .pipelines
            .get(cmd.pipeline.into())
            .expect("pipeline handle does not resolve");
        let builtin = pipeline.builtin;
        let layout = pipeline.layout;

        match cmd.geometry {
            Geometry::Generated { count } => {
                // Copy this command's instance bytes out for the same aliasing
                // reason.
                let span = count as usize * layout.stride;
                let instances = self.buffer(cmd.instance_buffer).bytes
                    [cmd.instance_offset..cmd.instance_offset + span]
                    .to_vec();

                for i in 0..count as usize {
                    let base = i * layout.stride;
                    let inst = &instances[base..base + layout.stride];
                    match builtin {
                        BuiltinShader::Quad => {
                            self.fill_quad(target, width, height, &layout, inst, cmd.scissor);
                        }
                        BuiltinShader::Image => {
                            self.fill_image(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.bind_group,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::GlyphRun => {
                            self.fill_glyph(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.bind_group,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::AnalyticRRect => {
                            self.fill_analytic_rrect(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::AnalyticEllipse => {
                            self.fill_analytic_ellipse(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::AnalyticCapsule => {
                            self.fill_analytic_capsule(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::AnalyticLine => {
                            self.fill_analytic_line(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::Gradient => {
                            self.fill_gradient(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.bind_group,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::AnalyticShadow => {
                            self.fill_analytic_shadow(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::Blur => {
                            self.fill_blur(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.bind_group,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::ColorTransform => {
                            self.fill_color_transform(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.bind_group,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::Material => {
                            self.fill_material(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.bind_group,
                                cmd.scissor,
                            );
                        }
                        BuiltinShader::AdvancedBlend => {
                            self.fill_advanced_blend(
                                target,
                                width,
                                height,
                                &layout,
                                inst,
                                cmd.bind_group,
                                cmd.scissor,
                            );
                        }
                        // Path/Mesh never arrive as generated geometry.
                        BuiltinShader::Path | BuiltinShader::Mesh | BuiltinShader::Layer => {}
                    }
                }
            }
            Geometry::IndexedMesh {
                vertex_buffer,
                index_buffer,
                index_format,
                index_offset,
                index_count,
            } => {
                // Snapshot the vertex + index bytes for the same aliasing reason.
                let verts = self.buffer(vertex_buffer).bytes.clone();
                let idx_bytes = self.buffer(index_buffer).bytes.clone();
                self.fill_mesh(
                    target,
                    width,
                    height,
                    &layout,
                    &verts,
                    &idx_bytes,
                    index_format,
                    index_offset,
                    index_count,
                    cmd.scissor,
                );
            }
        }
    }

    /// Rasterize an indexed triangle mesh (Path/Mesh) into the framebuffer.
    ///
    /// Reads per-vertex attributes by name from `layout` (the mesh vertex
    /// schema): `pos` (`Float2`, physical pixels), `color` (`Float4`, straight
    /// linear RGBA), and `edge` (`Float1`, coverage AA weight — 1 in the
    /// interior, ramping to 0 at antialiased fringe vertices). Each triangle is
    /// scan-filled with pixel-center sampling; color and `edge` are interpolated
    /// barycentrically, and `edge` scales the alpha for coverage AA. Colors are
    /// premultiplied on the fly and blended source-over.
    #[allow(clippy::too_many_arguments)]
    fn fill_mesh(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        verts: &[u8],
        idx_bytes: &[u8],
        index_format: IndexFormat,
        index_offset: u32,
        index_count: u32,
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let stride = layout.stride;
        // Read vertex `i`'s (pos, color, edge) from the packed vertex buffer.
        let vertex = |i: u32| -> ([f32; 2], [f32; 4], f32) {
            let base = i as usize * stride;
            let v = &verts[base..base + stride];
            (
                read_f2(layout, v, "pos"),
                read_f4(layout, v, "color"),
                read_f1(layout, v, "edge"),
            )
        };

        let tri_count = index_count / 3;
        for t in 0..tri_count {
            let base = index_offset + t * 3;
            let i0 = read_index(idx_bytes, index_format, base);
            let i1 = read_index(idx_bytes, index_format, base + 1);
            let i2 = read_index(idx_bytes, index_format, base + 2);
            let (p0, c0, e0) = vertex(i0);
            let (p1, c1, e1) = vertex(i1);
            let (p2, c2, e2) = vertex(i2);

            // Triangle bounding box (pad 1px for edge sampling), clipped to the
            // surface and the optional scissor.
            let minx = p0[0].min(p1[0]).min(p2[0]);
            let miny = p0[1].min(p1[1]).min(p2[1]);
            let maxx = p0[0].max(p1[0]).max(p2[0]);
            let maxy = p0[1].max(p1[1]).max(p2[1]);
            let (mut x0, mut y0, mut x1, mut y1) = (
                minx.floor().max(0.0) as u32,
                miny.floor().max(0.0) as u32,
                (maxx).ceil().min(width as f32) as u32,
                (maxy).ceil().min(height as f32) as u32,
            );
            if let Some((sx, sy, sw, sh)) = scissor {
                x0 = x0.max(sx);
                y0 = y0.max(sy);
                x1 = x1.min(sx + sw);
                y1 = y1.min(sy + sh);
            }

            // Twice the signed area of the triangle (edge-function denominator).
            let area = edge_fn(p0, p1, p2);
            if area == 0.0 {
                continue;
            }
            let inv_area = 1.0 / area;

            for py in y0..y1 {
                for px in x0..x1 {
                    let p = [px as f32 + 0.5, py as f32 + 0.5];
                    // Barycentric weights via edge functions.
                    let w0 = edge_fn(p1, p2, p) * inv_area;
                    let w1 = edge_fn(p2, p0, p) * inv_area;
                    let w2 = edge_fn(p0, p1, p) * inv_area;
                    // Inside test tolerant of either winding.
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }

                    // Interpolate straight color + coverage edge.
                    let cov = (w0 * e0 + w1 * e1 + w2 * e2).clamp(0.0, 1.0);
                    if cov <= 0.0 {
                        continue;
                    }
                    let mut col = [0.0f32; 4];
                    for k in 0..4 {
                        col[k] = w0 * c0[k] + w1 * c1[k] + w2 * c2[k];
                    }
                    let a = col[3] * cov;
                    if a <= 0.0 {
                        continue;
                    }
                    // Premultiplied source-over.
                    let src = [col[0] * a, col[1] * a, col[2] * a, a];
                    self.blend_pixel(target, width, px, py, src);
                }
            }
        }
    }

    /// Fill one Quad instance: a rounded, optionally bordered rectangle, with
    /// linear-coverage AA and premultiplied source-over blend.
    ///
    /// Reads these fields (by name) from the instance bytes, per the Quad
    /// built-in's schema:
    /// - `rect_pos`  : `Float2` top-left in physical pixels
    /// - `rect_size` : `Float2` width/height in physical pixels
    /// - `color`     : `Float4` **straight** (non-premultiplied) linear RGBA
    /// - `radius`    : `Float1` corner radius (pixels)
    /// - `border_width` : `Float1` (0 = no border)
    /// - `border_color` : `Float4` straight linear RGBA
    fn fill_quad(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let fill = read_f4(layout, inst, "color");
        let radius = read_f1(layout, inst, "radius");
        let border_w = read_f1(layout, inst, "border_width");
        let border_c = read_f4(layout, inst, "border_color");

        // Half-extents and center of the rect in pixel space.
        let half = [size[0] * 0.5, size[1] * 0.5];
        let center = [pos[0] + half[0], pos[1] + half[1]];
        // IQ box SDF radius: doubled then clamped to the smaller half-extent.
        let k = (2.0 * radius).min(half[0].min(half[1]));

        // Bounding box of affected pixels (pad by 1px for the AA ramp), clipped
        // to the surface and the optional scissor rect.
        let (mut x0, mut y0, mut x1, mut y1) = (
            (pos[0] - 1.0).floor().max(0.0) as u32,
            (pos[1] - 1.0).floor().max(0.0) as u32,
            (pos[0] + size[0] + 1.0).ceil().min(width as f32) as u32,
            (pos[1] + size[1] + 1.0).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        // Device-pixel coverage factor. The framebuffer grid is 1:1 with the
        // sampling position (one pixel step = one unit of `local`), so the
        // screen-space derivatives of `local` are the unit basis vectors and
        // `aa = 1 / length((1,0),(0,1)) = 1/sqrt(2)`. This mirrors the shader's
        // `aa_factor(in.local)` exactly (see `QUAD_HELPERS`).
        let aa = 1.0 / (2.0_f32).sqrt();

        for py in y0..y1 {
            for px in x0..x1 {
                // Sample at the pixel center.
                let p = [px as f32 + 0.5, py as f32 + 0.5];
                let d = box_sdf(p, center, half, k);

                // Device-pixel-aware coverage: linear ramp over ~1 physical pixel.
                let fill_cov = (-d * aa).clamp(0.0, 1.0);
                if fill_cov <= 0.0 && border_w <= 0.0 {
                    continue;
                }

                // Composite border over fill in straight-alpha space, then
                // convert to premultiplied for the framebuffer blend.
                let mut src = [
                    fill[0] * fill[3] * fill_cov,
                    fill[1] * fill[3] * fill_cov,
                    fill[2] * fill[3] * fill_cov,
                    fill[3] * fill_cov,
                ];
                if border_w > 0.0 {
                    let bcov = (-(d.abs() - border_w * 0.5) * aa).clamp(0.0, 1.0);
                    if bcov > 0.0 {
                        let ba = border_c[3] * bcov;
                        // border over fill (both premultiplied source-over).
                        let bsrc = [border_c[0] * ba, border_c[1] * ba, border_c[2] * ba, ba];
                        src = [
                            bsrc[0] + src[0] * (1.0 - ba),
                            bsrc[1] + src[1] * (1.0 - ba),
                            bsrc[2] + src[2] * (1.0 - ba),
                            bsrc[3] + src[3] * (1.0 - ba),
                        ];
                    }
                }
                if src[3] <= 0.0 {
                    continue;
                }

                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one AnalyticRRect instance: a rounded rectangle with an independent
    /// radius per corner, linear-coverage AA, premultiplied source-over blend.
    ///
    /// Reads these fields (by name) from the instance bytes, per the
    /// AnalyticRRect built-in's schema:
    /// - `rect_pos`  : `Float2` top-left in physical pixels
    /// - `rect_size` : `Float2` width/height in physical pixels
    /// - `color`     : `Float4` **straight** (non-premultiplied) linear RGBA
    /// - `radius`    : `Float4` per-corner radius (pixels): lt, rt, rb, lb
    /// - `border_width` : `Float1` (0 = no border)
    /// - `border_color` : `Float4` straight linear RGBA
    ///
    /// The per-pixel math mirrors [`ANALYTIC_RRECT_MSL`](../../shader)'s fragment
    /// (`rrect_sdf` + border-over-fill) exactly.
    fn fill_analytic_rrect(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let fill = read_f4(layout, inst, "color");
        let radii = read_f4(layout, inst, "radius");
        let border_w = read_f1(layout, inst, "border_width");
        let border_c = read_f4(layout, inst, "border_color");

        let half = [size[0] * 0.5, size[1] * 0.5];
        let center = [pos[0] + half[0], pos[1] + half[1]];

        let (mut x0, mut y0, mut x1, mut y1) = (
            (pos[0] - 1.0).floor().max(0.0) as u32,
            (pos[1] - 1.0).floor().max(0.0) as u32,
            (pos[0] + size[0] + 1.0).ceil().min(width as f32) as u32,
            (pos[1] + size[1] + 1.0).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        // See `fill_quad`: the headless grid is 1:1 with `local`, so `aa` is the
        // constant `1/sqrt(2)`, matching the shader's `aa_factor(in.local)`.
        let aa = 1.0 / (2.0_f32).sqrt();

        for py in y0..y1 {
            for px in x0..x1 {
                let p = [px as f32 + 0.5, py as f32 + 0.5];
                let d = rrect_sdf(p, center, half, radii);

                let fill_cov = (-d * aa).clamp(0.0, 1.0);
                if fill_cov <= 0.0 && border_w <= 0.0 {
                    continue;
                }

                let mut src = [
                    fill[0] * fill[3] * fill_cov,
                    fill[1] * fill[3] * fill_cov,
                    fill[2] * fill[3] * fill_cov,
                    fill[3] * fill_cov,
                ];
                if border_w > 0.0 {
                    let bcov = (-(d.abs() - border_w * 0.5) * aa).clamp(0.0, 1.0);
                    if bcov > 0.0 {
                        let ba = border_c[3] * bcov;
                        let bsrc = [border_c[0] * ba, border_c[1] * ba, border_c[2] * ba, ba];
                        src = [
                            bsrc[0] + src[0] * (1.0 - ba),
                            bsrc[1] + src[1] * (1.0 - ba),
                            bsrc[2] + src[2] * (1.0 - ba),
                            bsrc[3] + src[3] * (1.0 - ba),
                        ];
                    }
                }
                if src[3] <= 0.0 {
                    continue;
                }

                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one AnalyticEllipse instance: an axis-aligned ellipse (a circle when
    /// its axes are equal), linear-coverage AA, premultiplied source-over blend.
    ///
    /// Reads these fields (by name) from the instance bytes, per the
    /// AnalyticEllipse built-in's schema:
    /// - `rect_pos`  : `Float2` top-left in physical pixels
    /// - `rect_size` : `Float2` width/height in physical pixels
    /// - `color`     : `Float4` **straight** (non-premultiplied) linear RGBA
    /// - `border_width` : `Float1` (0 = no border)
    /// - `border_color` : `Float4` straight linear RGBA
    ///
    /// The ellipse radii are the rect's half-extents. The per-pixel math mirrors
    /// [`ANALYTIC_ELLIPSE_MSL`](../../shader)'s fragment (`ellipse_sdf` +
    /// border-over-fill) exactly.
    fn fill_analytic_ellipse(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let fill = read_f4(layout, inst, "color");
        let border_w = read_f1(layout, inst, "border_width");
        let border_c = read_f4(layout, inst, "border_color");

        let radii = [size[0] * 0.5, size[1] * 0.5];
        let center = [pos[0] + radii[0], pos[1] + radii[1]];

        let (mut x0, mut y0, mut x1, mut y1) = (
            (pos[0] - 1.0).floor().max(0.0) as u32,
            (pos[1] - 1.0).floor().max(0.0) as u32,
            (pos[0] + size[0] + 1.0).ceil().min(width as f32) as u32,
            (pos[1] + size[1] + 1.0).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        let aa = 1.0 / (2.0_f32).sqrt();

        for py in y0..y1 {
            for px in x0..x1 {
                let p = [px as f32 + 0.5, py as f32 + 0.5];
                let d = ellipse_sdf(p, center, radii);

                let fill_cov = (-d * aa).clamp(0.0, 1.0);
                if fill_cov <= 0.0 && border_w <= 0.0 {
                    continue;
                }

                let mut src = [
                    fill[0] * fill[3] * fill_cov,
                    fill[1] * fill[3] * fill_cov,
                    fill[2] * fill[3] * fill_cov,
                    fill[3] * fill_cov,
                ];
                if border_w > 0.0 {
                    let bcov = (-(d.abs() - border_w * 0.5) * aa).clamp(0.0, 1.0);
                    if bcov > 0.0 {
                        let ba = border_c[3] * bcov;
                        let bsrc = [border_c[0] * ba, border_c[1] * ba, border_c[2] * ba, ba];
                        src = [
                            bsrc[0] + src[0] * (1.0 - ba),
                            bsrc[1] + src[1] * (1.0 - ba),
                            bsrc[2] + src[2] * (1.0 - ba),
                            bsrc[3] + src[3] * (1.0 - ba),
                        ];
                    }
                }
                if src[3] <= 0.0 {
                    continue;
                }

                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one AnalyticCapsule instance: a capsule/stadium (a rounded box whose
    /// corner radius is the smaller half-extent), linear-coverage AA,
    /// premultiplied source-over blend.
    ///
    /// Reads the same fields as [`fill_analytic_ellipse`](Self::fill_analytic_ellipse)
    /// (the schema is byte-identical): `rect_pos`, `rect_size`, `color`,
    /// `border_width`, `border_color`. The half-extents are the rect's half-size
    /// and the corner radius is derived as their minimum. The per-pixel math
    /// mirrors [`ANALYTIC_CAPSULE_MSL`](../../shader)'s fragment (`capsule_sdf` +
    /// border-over-fill) exactly.
    fn fill_analytic_capsule(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let fill = read_f4(layout, inst, "color");
        let border_w = read_f1(layout, inst, "border_width");
        let border_c = read_f4(layout, inst, "border_color");

        let half = [size[0] * 0.5, size[1] * 0.5];
        let center = [pos[0] + half[0], pos[1] + half[1]];

        let (mut x0, mut y0, mut x1, mut y1) = (
            (pos[0] - 1.0).floor().max(0.0) as u32,
            (pos[1] - 1.0).floor().max(0.0) as u32,
            (pos[0] + size[0] + 1.0).ceil().min(width as f32) as u32,
            (pos[1] + size[1] + 1.0).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        let aa = 1.0 / (2.0_f32).sqrt();

        for py in y0..y1 {
            for px in x0..x1 {
                let p = [px as f32 + 0.5, py as f32 + 0.5];
                let d = capsule_sdf(p, center, half);

                let fill_cov = (-d * aa).clamp(0.0, 1.0);
                if fill_cov <= 0.0 && border_w <= 0.0 {
                    continue;
                }

                let mut src = [
                    fill[0] * fill[3] * fill_cov,
                    fill[1] * fill[3] * fill_cov,
                    fill[2] * fill[3] * fill_cov,
                    fill[3] * fill_cov,
                ];
                if border_w > 0.0 {
                    let bcov = (-(d.abs() - border_w * 0.5) * aa).clamp(0.0, 1.0);
                    if bcov > 0.0 {
                        let ba = border_c[3] * bcov;
                        let bsrc = [border_c[0] * ba, border_c[1] * ba, border_c[2] * ba, ba];
                        src = [
                            bsrc[0] + src[0] * (1.0 - ba),
                            bsrc[1] + src[1] * (1.0 - ba),
                            bsrc[2] + src[2] * (1.0 - ba),
                            bsrc[3] + src[3] * (1.0 - ba),
                        ];
                    }
                }
                if src[3] <= 0.0 {
                    continue;
                }

                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one AnalyticLine instance: a stroked segment between two endpoints
    /// with a butt/square/round cap, linear-coverage AA, premultiplied
    /// source-over blend.
    ///
    /// Reads these fields (by name) from the instance bytes, per the AnalyticLine
    /// built-in's schema:
    /// - `p0`, `p1`      : `Float2` segment endpoints in physical pixels
    /// - `width`         : `Float1` stroke width (centered; half-width each side)
    /// - `color`         : `Float4` **straight** fill RGBA
    /// - `cap`           : `Uint1` 0=butt 1=square 2=round
    /// - `border_width`  : `Float1` border width
    /// - `border_color`  : `Float4` **straight** border RGBA
    ///
    /// The per-pixel math mirrors [`ANALYTIC_LINE_MSL`](../../shader)'s fragment
    /// (`segment_sdf`/`capped_segment_sdf` + border-over-fill) exactly. `join` and
    /// `miter_limit` are read by the shader but only matter across multiple
    /// segments; a single segment's shape is fully determined by its cap.
    fn fill_analytic_line(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let p0 = read_f2(layout, inst, "p0");
        let p1 = read_f2(layout, inst, "p1");
        let stroke_w = read_f1(layout, inst, "width");
        let fill = read_f4(layout, inst, "color");
        let cap = read_u1(layout, inst, "cap");
        let border_w = read_f1(layout, inst, "border_width");
        let border_c = read_f4(layout, inst, "border_color");

        let hw = stroke_w * 0.5;

        // Bounding box of the rotated stroke: the endpoint span plus half-width
        // (plus a cap extension for square/round caps) plus a 1px AA pad, on both
        // axes. Cheaper than deriving the exact oriented quad and correct for the
        // scan (pixels outside get zero coverage anyway).
        let cap_ext = if cap == 0 { 0.0 } else { hw };
        let margin = hw + cap_ext + 1.0;
        let (mut x0, mut y0, mut x1, mut y1) = (
            (p0[0].min(p1[0]) - margin).floor().max(0.0) as u32,
            (p0[1].min(p1[1]) - margin).floor().max(0.0) as u32,
            (p0[0].max(p1[0]) + margin).ceil().min(width as f32) as u32,
            (p0[1].max(p1[1]) + margin).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        let aa = 1.0 / (2.0_f32).sqrt();

        for py in y0..y1 {
            for px in x0..x1 {
                let p = [px as f32 + 0.5, py as f32 + 0.5];
                let d = if cap == 2 {
                    segment_sdf(p, p0, p1, hw)
                } else {
                    let ext = if cap == 1 { hw } else { 0.0 };
                    capped_segment_sdf(p, p0, p1, hw, ext)
                };

                let fill_cov = (-d * aa).clamp(0.0, 1.0);
                if fill_cov <= 0.0 && border_w <= 0.0 {
                    continue;
                }

                let mut src = [
                    fill[0] * fill[3] * fill_cov,
                    fill[1] * fill[3] * fill_cov,
                    fill[2] * fill[3] * fill_cov,
                    fill[3] * fill_cov,
                ];
                if border_w > 0.0 {
                    let bcov = (-(d.abs() - border_w * 0.5) * aa).clamp(0.0, 1.0);
                    if bcov > 0.0 {
                        let ba = border_c[3] * bcov;
                        let bsrc = [border_c[0] * ba, border_c[1] * ba, border_c[2] * ba, ba];
                        src = [
                            bsrc[0] + src[0] * (1.0 - ba),
                            bsrc[1] + src[1] * (1.0 - ba),
                            bsrc[2] + src[2] * (1.0 - ba),
                            bsrc[3] + src[3] * (1.0 - ba),
                        ];
                    }
                }
                if src[3] <= 0.0 {
                    continue;
                }

                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one AnalyticShadow instance: a soft drop shadow for an analytic shape
    /// (rounded box / ellipse / capsule), its coverage a closed-form Gaussian ramp
    /// over the shape's signed distance — no offscreen blur pass.
    ///
    /// Reads these fields (by name) from the instance bytes, per the AnalyticShadow
    /// built-in's schema:
    /// - `rect_pos`  : `Float2` source rect top-left in physical pixels
    /// - `rect_size` : `Float2` source rect width/height in physical pixels
    /// - `color`     : `Float4` **straight** (non-premultiplied) linear RGBA
    /// - `radius`    : `Float4` per-corner radius (lt, rt, rb, lb) for the box case
    /// - `offset`    : `Float2` shadow offset in physical pixels
    /// - `sigma`     : `Float1` blur standard deviation in physical pixels
    /// - `spread`    : `Float1` silhouette grow (>0) / shrink (<0) in physical pixels
    /// - `shape`     : `Uint1` 0=rounded box 1=ellipse 2=capsule
    /// - `inner`     : `Uint1` 0=outer drop shadow 1=inner shadow (darkens inside)
    ///
    /// The per-pixel math mirrors [`ANALYTIC_SHADOW_MSL`](../../shader)'s fragment
    /// exactly: sample in rect-centered space shifted by `-offset`, inset the
    /// half-extents by `spread`, evaluate the `shape`-selected SDF, and map the
    /// distance through a Gaussian integral (`0.5*(1 - erf(d/(sqrt2*sigma)))`),
    /// falling back to a device-pixel AA ramp when `sigma` is near zero.
    fn fill_analytic_shadow(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let fill = read_f4(layout, inst, "color");
        let radii = read_f4(layout, inst, "radius");
        let offset = read_f2(layout, inst, "offset");
        let sigma = read_f1(layout, inst, "sigma");
        let spread = read_f1(layout, inst, "spread");
        let shape = read_u1(layout, inst, "shape");
        let inner = read_u1(layout, inst, "inner");

        let half = [size[0] * 0.5, size[1] * 0.5];
        let center = [pos[0] + half[0], pos[1] + half[1]];
        let half_ext = [(half[0] + spread).max(0.0), (half[1] + spread).max(0.0)];

        // Footprint = the padded quad the vertex stage expands to: 3*sigma of blur
        // reach, plus positive spread, plus a 1px AA-fallback pad, plus |offset|.
        let reach = 3.0 * sigma + spread.max(0.0) + 1.0;
        let pad = [reach + offset[0].abs(), reach + offset[1].abs()];
        let (mut x0, mut y0, mut x1, mut y1) = (
            (pos[0] - pad[0]).floor().max(0.0) as u32,
            (pos[1] - pad[1]).floor().max(0.0) as u32,
            (pos[0] + size[0] + pad[0]).ceil().min(width as f32) as u32,
            (pos[1] + size[1] + pad[1]).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        // Device-pixel AA ramp for the sigma≈0 fallback (scale-1 scan → 1/sqrt2).
        let aa = 1.0 / (2.0_f32).sqrt();

        for py in y0..y1 {
            for px in x0..x1 {
                let p = [
                    px as f32 + 0.5 - center[0] - offset[0],
                    py as f32 + 0.5 - center[1] - offset[1],
                ];
                let d = shadow_sdf(shape, p, half_ext, radii);

                // `1.4142135` (not `SQRT_2`) is byte-exact with the MSL emitter's literal.
                #[allow(clippy::approx_constant)]
                let cov = if inner != 0 {
                    if sigma > 0.01 {
                        let soft = 0.5 * (1.0 + erf_approx(d / (1.4142135 * sigma)));
                        let inside = 0.5 * (1.0 - erf_approx(d / (1.4142135 * sigma)));
                        (soft * inside).clamp(0.0, 1.0)
                    } else {
                        (-d * aa).clamp(0.0, 1.0)
                    }
                } else if sigma > 0.01 {
                    (0.5 * (1.0 - erf_approx(d / (1.4142135 * sigma)))).clamp(0.0, 1.0)
                } else {
                    (-d * aa).clamp(0.0, 1.0)
                };
                if cov <= 0.0 {
                    continue;
                }

                let fa = fill[3] * cov;
                let src = [fill[0] * fa, fill[1] * fa, fill[2] * fa, fa];
                if src[3] <= 0.0 {
                    continue;
                }

                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one Image instance: sample the bound texture's uv sub-rect across the
    /// destination rect, modulated by a straight tint, premultiplied source-over.
    ///
    /// Reads these fields (by name) from the instance bytes, per the Image
    /// built-in's schema:
    /// - `rect_pos`  : `Float2` destination top-left in physical pixels
    /// - `rect_size` : `Float2` destination width/height in physical pixels
    /// - `uv_pos`    : `Float2` source sub-rect origin, normalized `0..1`
    /// - `uv_size`   : `Float2` source sub-rect size, normalized `0..1`
    /// - `color`     : `Float4` **straight** linear RGBA tint (a = opacity)
    ///
    /// The texel is premultiplied linear (Viso texture convention); it is scaled
    /// by the tint's premultiplied form (`rgb * a`, `a`) to stay premultiplied,
    /// matching [`IMAGE_MSL`](../../shader) and the Metal path. Sampling honors
    /// the bound sampler's filter (nearest/bilinear) and address mode
    /// (clamp/repeat) with a texel-center `-0.5` convention.
    #[allow(clippy::too_many_arguments)]
    fn fill_image(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        bind_group: Option<BindGroupId>,
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let uv_pos = read_f2(layout, inst, "uv_pos");
        let uv_size = read_f2(layout, inst, "uv_size");
        let tint = read_f4(layout, inst, "color");

        // Resolve the bound texture and sampler from the bind group. Without a
        // texture there is nothing to sample.
        let Some(bg) = bind_group else { return };
        let (mut tex_id, mut samp) = (
            None,
            SamplerDesc {
                filter: crate::resource::FilterMode::Linear,
                address: crate::resource::AddressMode::ClampToEdge,
            },
        );
        for binding in &self.bind_group(bg).bindings {
            match binding {
                crate::resource::Binding::Texture(t) => tex_id = Some(*t),
                crate::resource::Binding::Sampler(s) => samp = self.sampler(*s),
                crate::resource::Binding::Uniform(_) => {}
            }
        }
        let Some(tex_id) = tex_id else { return };
        // Snapshot the texture (dimensions + premultiplied texels) so the
        // per-pixel loop can take `&mut self.surfaces` without aliasing.
        let (tw, th, texels) = {
            let t = self.texture(tex_id);
            (t.width, t.height, t.texels.clone())
        };
        if tw == 0 || th == 0 {
            return;
        }

        // Destination pixel bounds (no AA pad — the image samples exactly its
        // rect), clipped to the surface and the optional scissor.
        let (mut x0, mut y0, mut x1, mut y1) = (
            pos[0].floor().max(0.0) as u32,
            pos[1].floor().max(0.0) as u32,
            (pos[0] + size[0]).ceil().min(width as f32) as u32,
            (pos[1] + size[1]).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }
        if size[0] <= 0.0 || size[1] <= 0.0 {
            return;
        }

        // Tint in premultiplied form: rgb by (rgb * a), a by a.
        let tint_pm = [
            tint[0] * tint[3],
            tint[1] * tint[3],
            tint[2] * tint[3],
            tint[3],
        ];

        for py in y0..y1 {
            for px in x0..x1 {
                // Normalized position within the destination rect (pixel center).
                let fx = (px as f32 + 0.5 - pos[0]) / size[0];
                let fy = (py as f32 + 0.5 - pos[1]) / size[1];
                // Map into the uv sub-rect.
                let u = uv_pos[0] + fx * uv_size[0];
                let v = uv_pos[1] + fy * uv_size[1];

                let texel = sample_texel(&texels, tw, th, u, v, &samp);
                // texel is premultiplied; scale by the premultiplied tint.
                let src = [
                    texel[0] * tint_pm[0],
                    texel[1] * tint_pm[1],
                    texel[2] * tint_pm[2],
                    texel[3] * tint_pm[3],
                ];
                if src[3] <= 0.0 {
                    continue;
                }
                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one separable Gaussian blur instance sampling a source texture.
    ///
    /// Instance layout ([`BlurInstance`](../../render)):
    /// - `rect_pos`/`rect_size` : destination quad in physical pixels
    /// - `uv_pos`/`uv_size`     : source sub-rect, normalized `0..1`
    /// - `dir`                  : per-tap step in normalized uv along the blur axis
    /// - `sigma`                : Gaussian sigma in source texels
    /// - `radius`               : tap radius (taps each side); loop `-R..=R`
    ///
    /// For each destination pixel the source uv is derived exactly as
    /// [`fill_image`](Self::fill_image), then a 1D Gaussian tap loop walks the uv
    /// along `dir` (`u + i*dir[0]`, `v + i*dir[1]`), weighting each premultiplied
    /// texel by `exp(-(i*i)/(2*sigma*sigma))` and normalizing by the weight sum.
    /// The result is premultiplied (no tint) and blended source-over; `sigma <= 0`
    /// degrades to a single center tap.
    ///
    /// Taps are clamped to the `uv_pos`/`uv_size` sub-rect, inset by half a texel so
    /// the outer texel row/column is held rather than blended with whatever lies
    /// beyond it: a pooled render target is larger than the region written into it,
    /// so the sampler's own clamp-to-edge would hold cleared padding, not the
    /// content edge. This mirrors the Metal fragment body exactly.
    #[allow(clippy::too_many_arguments)]
    fn fill_blur(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        bind_group: Option<BindGroupId>,
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let uv_pos = read_f2(layout, inst, "uv_pos");
        let uv_size = read_f2(layout, inst, "uv_size");
        let dir = read_f2(layout, inst, "dir");
        let sigma = read_f1(layout, inst, "sigma");
        let radius = read_f1(layout, inst, "radius");

        let Some(bg) = bind_group else { return };
        let (mut tex_id, mut samp) = (
            None,
            SamplerDesc {
                filter: crate::resource::FilterMode::Linear,
                address: crate::resource::AddressMode::ClampToEdge,
            },
        );
        for binding in &self.bind_group(bg).bindings {
            match binding {
                crate::resource::Binding::Texture(t) => tex_id = Some(*t),
                crate::resource::Binding::Sampler(s) => samp = self.sampler(*s),
                crate::resource::Binding::Uniform(_) => {}
            }
        }
        let Some(tex_id) = tex_id else { return };
        let (tw, th, texels) = {
            let t = self.texture(tex_id);
            (t.width, t.height, t.texels.clone())
        };
        if tw == 0 || th == 0 || size[0] <= 0.0 || size[1] <= 0.0 {
            return;
        }

        let (mut x0, mut y0, mut x1, mut y1) = (
            pos[0].floor().max(0.0) as u32,
            pos[1].floor().max(0.0) as u32,
            (pos[0] + size[0]).ceil().min(width as f32) as u32,
            (pos[1] + size[1]).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        // Number of taps each side. A non-positive sigma or radius degrades to a
        // single center tap (identity resample).
        let r = if sigma > 0.0 {
            radius.max(0.0).round() as i32
        } else {
            0
        };
        let two_sigma_sq = 2.0 * sigma * sigma;

        // The tap window, inset half a texel inside the source sub-rect.
        let half_texel = [0.5 / tw as f32, 0.5 / th as f32];
        let lo = [uv_pos[0] + half_texel[0], uv_pos[1] + half_texel[1]];
        let hi = [
            (uv_pos[0] + uv_size[0] - half_texel[0]).max(lo[0]),
            (uv_pos[1] + uv_size[1] - half_texel[1]).max(lo[1]),
        ];

        for py in y0..y1 {
            for px in x0..x1 {
                let fx = (px as f32 + 0.5 - pos[0]) / size[0];
                let fy = (py as f32 + 0.5 - pos[1]) / size[1];
                let u = uv_pos[0] + fx * uv_size[0];
                let v = uv_pos[1] + fy * uv_size[1];

                let mut acc = [0.0f32; 4];
                let mut wsum = 0.0f32;
                for i in -r..=r {
                    let fi = i as f32;
                    let w = if two_sigma_sq > 0.0 {
                        (-(fi * fi) / two_sigma_sq).exp()
                    } else {
                        1.0
                    };
                    let su = (u + fi * dir[0]).clamp(lo[0], hi[0]);
                    let sv = (v + fi * dir[1]).clamp(lo[1], hi[1]);
                    let texel = sample_texel(&texels, tw, th, su, sv, &samp);
                    acc[0] += w * texel[0];
                    acc[1] += w * texel[1];
                    acc[2] += w * texel[2];
                    acc[3] += w * texel[3];
                    wsum += w;
                }
                if wsum <= 0.0 {
                    continue;
                }
                let src = [acc[0] / wsum, acc[1] / wsum, acc[2] / wsum, acc[3] / wsum];
                if src[3] <= 0.0 {
                    continue;
                }
                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one fused color-effect instance sampling a source texture.
    ///
    /// Instance layout ([`ColorTransformInstance`](../../render)):
    /// - `rect_pos`/`rect_size` : destination quad in physical pixels
    /// - `uv_pos`/`uv_size`     : source sub-rect, normalized `0..1`
    /// - `row0`..`row3`         : output rows of the color matrix over `(r, g, b, a)`
    /// - `offset`               : the matrix's constant column, one term per channel
    /// - `gamma`                : per-channel RGB exponent applied after the matrix
    ///
    /// The source uv is derived exactly as [`fill_image`](Self::fill_image), then the
    /// texel is *unpremultiplied* before the matrix — a color matrix is defined on
    /// straight RGBA, so brightness must not depend on coverage. The four rows plus
    /// the constant column produce the output channels, the result is clamped to the
    /// representable range, the optional gamma is applied to RGB, and the pixel is
    /// repremultiplied and blended source-over. This mirrors the Metal fragment body
    /// exactly, so one pass here realizes the same fused run of color effects.
    #[allow(clippy::too_many_arguments)]
    fn fill_color_transform(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        bind_group: Option<BindGroupId>,
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let uv_pos = read_f2(layout, inst, "uv_pos");
        let uv_size = read_f2(layout, inst, "uv_size");
        let rows = [
            read_f4(layout, inst, "row0"),
            read_f4(layout, inst, "row1"),
            read_f4(layout, inst, "row2"),
            read_f4(layout, inst, "row3"),
        ];
        let offset = read_f4(layout, inst, "offset");
        let gamma = read_f1(layout, inst, "gamma");

        let Some(bg) = bind_group else { return };
        let (mut tex_id, mut samp) = (
            None,
            SamplerDesc {
                filter: crate::resource::FilterMode::Linear,
                address: crate::resource::AddressMode::ClampToEdge,
            },
        );
        for binding in &self.bind_group(bg).bindings {
            match binding {
                crate::resource::Binding::Texture(t) => tex_id = Some(*t),
                crate::resource::Binding::Sampler(s) => samp = self.sampler(*s),
                crate::resource::Binding::Uniform(_) => {}
            }
        }
        let Some(tex_id) = tex_id else { return };
        let (tw, th, texels) = {
            let t = self.texture(tex_id);
            (t.width, t.height, t.texels.clone())
        };
        if tw == 0 || th == 0 || size[0] <= 0.0 || size[1] <= 0.0 {
            return;
        }

        let (mut x0, mut y0, mut x1, mut y1) = (
            pos[0].floor().max(0.0) as u32,
            pos[1].floor().max(0.0) as u32,
            (pos[0] + size[0]).ceil().min(width as f32) as u32,
            (pos[1] + size[1]).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        for py in y0..y1 {
            for px in x0..x1 {
                let fx = (px as f32 + 0.5 - pos[0]) / size[0];
                let fy = (py as f32 + 0.5 - pos[1]) / size[1];
                let u = uv_pos[0] + fx * uv_size[0];
                let v = uv_pos[1] + fy * uv_size[1];
                let texel = sample_texel(&texels, tw, th, u, v, &samp);

                // Straight RGBA in, straight RGBA out. A fully transparent texel
                // carries no color, so it enters the matrix as zero.
                let a = texel[3];
                let straight = if a > 0.0 {
                    [texel[0] / a, texel[1] / a, texel[2] / a, a]
                } else {
                    [0.0; 4]
                };

                let mut dst = [0.0f32; 4];
                for c in 0..4 {
                    let row = rows[c];
                    dst[c] = (row[0] * straight[0]
                        + row[1] * straight[1]
                        + row[2] * straight[2]
                        + row[3] * straight[3]
                        + offset[c])
                        .clamp(0.0, 1.0);
                }
                if gamma != 1.0 {
                    for channel in dst.iter_mut().take(3) {
                        *channel = channel.powf(gamma);
                    }
                }
                if dst[3] <= 0.0 {
                    continue;
                }
                let src = [dst[0] * dst[3], dst[1] * dst[3], dst[2] * dst[3], dst[3]];
                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one frosted material composite: a blurred backdrop in, a finished glass
    /// surface out.
    ///
    /// Instance layout (`MaterialInstance`):
    /// - `rect_pos`/`rect_size` : the surface's quad in physical pixels
    /// - `uv_pos`/`uv_size`     : the blurred backdrop's sub-rect, normalized `0..1`
    /// - `row0`..`row3`/`offset`: the fused tint matrix, exactly as ColorTransform
    /// - `radius`               : mask radii, left-top, right-top, right-bottom, left-bottom
    /// - `gamma`                : post-matrix RGB exponent
    /// - `noise`                : grain amplitude, `0` = none
    /// - `opacity`              : scales the finished surface
    ///
    /// Mirrors the Metal fragment stage-for-stage: tint on straight RGBA, grain from
    /// an integer-lattice hash of the device pixel (so the value matches the shader's
    /// `grain` bit for bit and never varies frame to frame), then the surface's own
    /// rounded-rect coverage as the mask. The AA ramp uses one unit per pixel, which
    /// is what the shader's `aa_factor` evaluates to for this pixel-space quad.
    #[allow(clippy::too_many_arguments)]
    fn fill_material(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        bind_group: Option<BindGroupId>,
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let uv_pos = read_f2(layout, inst, "uv_pos");
        let uv_size = read_f2(layout, inst, "uv_size");
        let rows = [
            read_f4(layout, inst, "row0"),
            read_f4(layout, inst, "row1"),
            read_f4(layout, inst, "row2"),
            read_f4(layout, inst, "row3"),
        ];
        let offset = read_f4(layout, inst, "offset");
        let radius = read_f4(layout, inst, "radius");
        let gamma = read_f1(layout, inst, "gamma");
        let noise = read_f1(layout, inst, "noise");
        let opacity = read_f1(layout, inst, "opacity");

        let Some(bg) = bind_group else { return };
        let (mut tex_id, mut samp) = (
            None,
            SamplerDesc {
                filter: crate::resource::FilterMode::Linear,
                address: crate::resource::AddressMode::ClampToEdge,
            },
        );
        for binding in &self.bind_group(bg).bindings {
            match binding {
                crate::resource::Binding::Texture(t) => tex_id = Some(*t),
                crate::resource::Binding::Sampler(s) => samp = self.sampler(*s),
                crate::resource::Binding::Uniform(_) => {}
            }
        }
        let Some(tex_id) = tex_id else { return };
        let (tw, th, texels) = {
            let t = self.texture(tex_id);
            (t.width, t.height, t.texels.clone())
        };
        if tw == 0 || th == 0 || size[0] <= 0.0 || size[1] <= 0.0 {
            return;
        }

        // The quad is padded 1px each side so the mask's AA ramp is covered.
        let (mut x0, mut y0, mut x1, mut y1) = (
            (pos[0] - 1.0).floor().max(0.0) as u32,
            (pos[1] - 1.0).floor().max(0.0) as u32,
            (pos[0] + size[0] + 1.0).ceil().min(width as f32) as u32,
            (pos[1] + size[1] + 1.0).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        let center = [pos[0] + size[0] * 0.5, pos[1] + size[1] * 0.5];
        let half_size = [size[0] * 0.5, size[1] * 0.5];
        // Half-texel inset: a pooled target is larger than the region written into
        // it, so clamping to the sub-rect edge holds content, not cleared padding.
        let half_texel = [0.5 / tw as f32, 0.5 / th as f32];
        let lo = [uv_pos[0] + half_texel[0], uv_pos[1] + half_texel[1]];
        let hi = [
            (uv_pos[0] + uv_size[0] - half_texel[0]).max(lo[0]),
            (uv_pos[1] + uv_size[1] - half_texel[1]).max(lo[1]),
        ];

        for py in y0..y1 {
            for px in x0..x1 {
                let local = [px as f32 + 0.5, py as f32 + 0.5];
                let d = rrect_sdf(local, center, half_size, radius);
                let cov = (-d).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }

                let fx = (local[0] - pos[0]) / size[0];
                let fy = (local[1] - pos[1]) / size[1];
                let u = (uv_pos[0] + fx * uv_size[0]).clamp(lo[0], hi[0]);
                let v = (uv_pos[1] + fy * uv_size[1]).clamp(lo[1], hi[1]);
                let texel = sample_texel(&texels, tw, th, u, v, &samp);

                let a = texel[3];
                let straight = if a > 0.0 {
                    [texel[0] / a, texel[1] / a, texel[2] / a, a]
                } else {
                    [0.0; 4]
                };

                let mut dst = [0.0f32; 4];
                for c in 0..4 {
                    let row = rows[c];
                    dst[c] = (row[0] * straight[0]
                        + row[1] * straight[1]
                        + row[2] * straight[2]
                        + row[3] * straight[3]
                        + offset[c])
                        .clamp(0.0, 1.0);
                }
                if gamma != 1.0 {
                    for channel in dst.iter_mut().take(3) {
                        *channel = channel.powf(gamma);
                    }
                }
                if noise > 0.0 {
                    let g = grain(local);
                    for channel in dst.iter_mut().take(3) {
                        *channel = (*channel + noise * g).clamp(0.0, 1.0);
                    }
                }

                let oa = dst[3] * cov * opacity;
                if oa <= 0.0 {
                    continue;
                }
                let src = [dst[0] * oa, dst[1] * oa, dst[2] * oa, oa];
                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one isolated advanced-blend composite: two textures in, the finished
    /// blend out.
    ///
    /// Instance layout ([`AdvancedBlendInstance`](../../render)):
    /// - `rect_pos`/`rect_size`       : destination quad in physical pixels
    /// - `uv_pos`/`uv_size`           : source sub-rect in texture 0 (the isolated layer)
    /// - `dst_uv_pos`/`dst_uv_size`   : destination sub-rect in texture 1 (the snapshot)
    /// - `mode`                       : the blend discriminant, `0..=27`
    /// - `opacity`                    : layer opacity folded into the source
    ///
    /// The two sub-rects are independent because the two textures are pooled
    /// separately and cover different world rects. Both bindings are resolved *in
    /// slot order* — the bind group lists texture 0 then texture 1, matching the
    /// `[[texture(n)]]` indices the MSL declares.
    ///
    /// The fragment owns the whole composite, so the destination is read here and
    /// mixed in here: the result is **written**, not blended
    /// ([`BlendMode::Replace`](crate::BlendMode::Replace)). That is the one place
    /// this backend's hardcoded source-over would be wrong, which is why it uses
    /// [`write_pixel`] instead of [`blend_pixel`]. Mirrors the Metal fragment body,
    /// so the same 28 modes are verifiable headlessly.
    #[allow(clippy::too_many_arguments)]
    fn fill_advanced_blend(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        bind_group: Option<BindGroupId>,
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let uv_pos = read_f2(layout, inst, "uv_pos");
        let uv_size = read_f2(layout, inst, "uv_size");
        let dst_uv_pos = read_f2(layout, inst, "dst_uv_pos");
        let dst_uv_size = read_f2(layout, inst, "dst_uv_size");
        let mode = read_u1(layout, inst, "mode");
        let opacity = read_f1(layout, inst, "opacity");

        let Some(bg) = bind_group else { return };
        let mut textures: [Option<TextureId>; 2] = [None, None];
        let mut samp = SamplerDesc {
            filter: crate::resource::FilterMode::Linear,
            address: crate::resource::AddressMode::ClampToEdge,
        };
        let mut slot = 0usize;
        for binding in &self.bind_group(bg).bindings {
            match binding {
                crate::resource::Binding::Texture(t) => {
                    if slot < textures.len() {
                        textures[slot] = Some(*t);
                    }
                    slot += 1;
                }
                crate::resource::Binding::Sampler(s) => samp = self.sampler(*s),
                crate::resource::Binding::Uniform(_) => {}
            }
        }
        let [Some(src_id), Some(dst_id)] = textures else {
            return;
        };
        if size[0] <= 0.0 || size[1] <= 0.0 {
            return;
        }
        let (sw, sh, src_texels) = {
            let t = self.texture(src_id);
            (t.width, t.height, t.texels.clone())
        };
        let (dw, dh, dst_texels) = {
            let t = self.texture(dst_id);
            (t.width, t.height, t.texels.clone())
        };
        if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
            return;
        }

        let (mut x0, mut y0, mut x1, mut y1) = (
            pos[0].floor().max(0.0) as u32,
            pos[1].floor().max(0.0) as u32,
            (pos[0] + size[0]).ceil().min(width as f32) as u32,
            (pos[1] + size[1]).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw_c, sh_c)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw_c);
            y1 = y1.min(sy + sh_c);
        }

        for py in y0..y1 {
            for px in x0..x1 {
                let fx = (px as f32 + 0.5 - pos[0]) / size[0];
                let fy = (py as f32 + 0.5 - pos[1]) / size[1];
                let mut s = sample_texel(
                    &src_texels,
                    sw,
                    sh,
                    uv_pos[0] + fx * uv_size[0],
                    uv_pos[1] + fy * uv_size[1],
                    &samp,
                );
                for c in s.iter_mut() {
                    *c *= opacity;
                }
                let d = sample_texel(
                    &dst_texels,
                    dw,
                    dh,
                    dst_uv_pos[0] + fx * dst_uv_size[0],
                    dst_uv_pos[1] + fy * dst_uv_size[1],
                    &samp,
                );
                let out = blend_composite(mode, s, d);
                self.write_pixel(target, width, px, py, out);
            }
        }
    }

    /// Fill one glyph instance from an R8 single-channel A8 coverage atlas.
    ///
    /// The instance layout matches [`fill_image`](Self::fill_image). The atlas
    /// stores exact per-pixel coverage: the sampled `.r` channel *is* coverage,
    /// used directly (`cov = texel.r`), matching [`GLYPHRUN_MSL`](../../shader).
    /// The run color is premultiplied by that coverage and blended source-over.
    /// A linear sampler gives the coverage its smooth (bilinearly interpolated)
    /// edge.
    #[allow(clippy::too_many_arguments)]
    fn fill_glyph(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        bind_group: Option<BindGroupId>,
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let uv_pos = read_f2(layout, inst, "uv_pos");
        let uv_size = read_f2(layout, inst, "uv_size");
        let color = read_f4(layout, inst, "color");

        // Resolve the bound R8 atlas and sampler.
        let Some(bg) = bind_group else { return };
        let (mut tex_id, mut samp) = (
            None,
            SamplerDesc {
                filter: crate::resource::FilterMode::Linear,
                address: crate::resource::AddressMode::ClampToEdge,
            },
        );
        for binding in &self.bind_group(bg).bindings {
            match binding {
                crate::resource::Binding::Texture(t) => tex_id = Some(*t),
                crate::resource::Binding::Sampler(s) => samp = self.sampler(*s),
                crate::resource::Binding::Uniform(_) => {}
            }
        }
        let Some(tex_id) = tex_id else { return };
        let (tw, th, texels) = {
            let t = self.texture(tex_id);
            (t.width, t.height, t.texels.clone())
        };
        if tw == 0 || th == 0 || size[0] <= 0.0 || size[1] <= 0.0 {
            return;
        }

        let (mut x0, mut y0, mut x1, mut y1) = (
            pos[0].floor().max(0.0) as u32,
            pos[1].floor().max(0.0) as u32,
            (pos[0] + size[0]).ceil().min(width as f32) as u32,
            (pos[1] + size[1]).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        // Run color, premultiplied.
        let base_a = color[3];
        for py in y0..y1 {
            for px in x0..x1 {
                let fx = (px as f32 + 0.5 - pos[0]) / size[0];
                let fy = (py as f32 + 0.5 - pos[1]) / size[1];
                let u = uv_pos[0] + fx * uv_size[0];
                let v = uv_pos[1] + fy * uv_size[1];

                // R8 atlas: decode_texel replicated the stored coverage into
                // every channel, so any channel is the sampled coverage.
                let cov = sample_texel(&texels, tw, th, u, v, &samp)[0];
                let a = base_a * cov;
                if a <= 0.0 {
                    continue;
                }
                let src = [color[0] * a, color[1] * a, color[2] * a, a];
                self.blend_pixel(target, width, px, py, src);
            }
        }
    }

    /// Fill one Gradient instance over an axis-aligned rectangle, reproducing
    /// [`GRADIENT_MSL`](../../shader)'s fragment math on the CPU.
    ///
    /// Reads these fields (by name) from the instance bytes, per the Gradient
    /// built-in's schema:
    /// - `rect_pos`  : `Float2` rect top-left in physical pixels
    /// - `rect_size` : `Float2` rect width/height in physical pixels
    /// - `kind`      : `Uint1`  0=linear 1=radial 2=sweep
    /// - `extend`    : `Uint1`  0=clamp 1=repeat 2=mirror
    /// - `p0`        : `Float2` gradient origin in physical pixels
    /// - `p1`        : `Float2` linear endpoint / `(radius,_)` / `(start_angle,_)`
    /// - `lut_v`     : `Float1` LUT row (v coordinate) for this gradient
    /// - `use_lut`   : `Uint1`  0=inline 2-stop lerp, 1=sample the bound LUT
    /// - `color0`    : `Float4` inline stop 0 (premultiplied linear), `use_lut==0`
    /// - `color1`    : `Float4` inline stop 1 (premultiplied linear), `use_lut==0`
    ///
    /// The raw gradient parameter `t`, the extend wrap, and the resolved color
    /// (LUT sample or inline lerp) all match the shader; both the LUT texel and
    /// the inline colors are premultiplied, so the result is premultiplied and
    /// blended source-over. The AA at the rect edge uses the same device-pixel
    /// coverage proxy the other analytic fills use.
    #[allow(clippy::too_many_arguments)]
    fn fill_gradient(
        &mut self,
        target: FbTarget,
        width: u32,
        height: u32,
        layout: &InstanceLayout,
        inst: &[u8],
        bind_group: Option<BindGroupId>,
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let pos = read_f2(layout, inst, "rect_pos");
        let size = read_f2(layout, inst, "rect_size");
        let kind = read_u1(layout, inst, "kind");
        let extend = read_u1(layout, inst, "extend");
        let g0 = read_f2(layout, inst, "p0");
        let g1 = read_f2(layout, inst, "p1");
        let lut_v = read_f1(layout, inst, "lut_v");
        let use_lut = read_u1(layout, inst, "use_lut");
        let color0 = read_f4(layout, inst, "color0");
        let color1 = read_f4(layout, inst, "color1");

        if size[0] <= 0.0 || size[1] <= 0.0 {
            return;
        }

        // Resolve the bound LUT texture + sampler when `use_lut != 0`. The shader
        // samples a 1D LUT atlas (one row per gradient); the CPU path snapshots
        // the premultiplied texels the same way `fill_image` does.
        let mut lut: Option<(u32, u32, Vec<[f32; 4]>, SamplerDesc)> = None;
        if use_lut != 0 {
            let Some(bg) = bind_group else { return };
            let (mut tex_id, mut samp) = (
                None,
                SamplerDesc {
                    filter: crate::resource::FilterMode::Linear,
                    address: crate::resource::AddressMode::ClampToEdge,
                },
            );
            for binding in &self.bind_group(bg).bindings {
                match binding {
                    crate::resource::Binding::Texture(t) => tex_id = Some(*t),
                    crate::resource::Binding::Sampler(s) => samp = self.sampler(*s),
                    crate::resource::Binding::Uniform(_) => {}
                }
            }
            let Some(tex_id) = tex_id else { return };
            let (tw, th, texels) = {
                let t = self.texture(tex_id);
                (t.width, t.height, t.texels.clone())
            };
            if tw == 0 || th == 0 {
                return;
            }
            lut = Some((tw, th, texels, samp));
        }

        let rect_min = pos;
        let rect_max = [pos[0] + size[0], pos[1] + size[1]];
        let center = [
            (rect_min[0] + rect_max[0]) * 0.5,
            (rect_min[1] + rect_max[1]) * 0.5,
        ];
        let half_ext = [
            (rect_max[0] - rect_min[0]) * 0.5,
            (rect_max[1] - rect_min[1]) * 0.5,
        ];

        // Device-pixel AA proxy, matching `aa_factor` at unit scale (the same
        // constant the other analytic fills use for their 1px ramp).
        let aa = 1.0 / (2.0_f32).sqrt();

        let (mut x0, mut y0, mut x1, mut y1) = (
            (pos[0] - 1.0).floor().max(0.0) as u32,
            (pos[1] - 1.0).floor().max(0.0) as u32,
            (pos[0] + size[0] + 1.0).ceil().min(width as f32) as u32,
            (pos[1] + size[1] + 1.0).ceil().min(height as f32) as u32,
        );
        if let Some((sx, sy, sw, sh)) = scissor {
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx + sw);
            y1 = y1.min(sy + sh);
        }

        for py in y0..y1 {
            for px in x0..x1 {
                let p = [px as f32 + 0.5, py as f32 + 0.5];

                // Rectangle coverage: signed distance to the box, AA-ramped.
                let q = [
                    (p[0] - center[0]).abs() - half_ext[0],
                    (p[1] - center[1]).abs() - half_ext[1],
                ];
                let qmax = [q[0].max(0.0), q[1].max(0.0)];
                let d = (qmax[0] * qmax[0] + qmax[1] * qmax[1]).sqrt() + q[0].max(q[1]).min(0.0);
                let cov = (-d * aa).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }

                let t = gradient_t(kind, p, g0, g1);
                let u = gradient_extend(extend, t);
                let grad = match &lut {
                    Some((tw, th, texels, samp)) => sample_texel(texels, *tw, *th, u, lut_v, samp),
                    None => [
                        color0[0] + (color1[0] - color0[0]) * u,
                        color0[1] + (color1[1] - color0[1]) * u,
                        color0[2] + (color1[2] - color0[2]) * u,
                        color0[3] + (color1[3] - color0[3]) * u,
                    ],
                };

                // grad is premultiplied; modulate by edge coverage.
                let src = [grad[0] * cov, grad[1] * cov, grad[2] * cov, grad[3] * cov];
                if src[3] <= 0.0 {
                    continue;
                }
                self.blend_pixel(target, width, px, py, src);
            }
        }
    }
}

/// The raw gradient parameter for each geometry, before extend wrapping —
/// mirrors `gradient_t` in [`GRADIENT_MSL`](../../shader). linear: project the
/// sample onto the `p0→p1` axis, normalized to `[0,1]` at the endpoints.
/// radial: distance from the center `p0` over the radius (`g1.x`). sweep: the
/// angle around `p0`, offset by the start angle (`g1.x`) and normalized to a
/// single `[0,1]` turn.
fn gradient_t(kind: u32, p: [f32; 2], g0: [f32; 2], g1: [f32; 2]) -> f32 {
    if kind == 1 {
        let r = g1[0].max(1e-6);
        let dx = p[0] - g0[0];
        let dy = p[1] - g0[1];
        return (dx * dx + dy * dy).sqrt() / r;
    }
    if kind == 2 {
        let ang = (p[1] - g0[1]).atan2(p[0] - g0[0]) - g1[0];
        let turn = ang * (1.0 / (2.0 * std::f32::consts::PI));
        return turn - turn.floor();
    }
    let axis = [g1[0] - g0[0], g1[1] - g0[1]];
    let len2 = (axis[0] * axis[0] + axis[1] * axis[1]).max(1e-12);
    ((p[0] - g0[0]) * axis[0] + (p[1] - g0[1]) * axis[1]) / len2
}

/// Apply the extend mode to a raw parameter, yielding a `[0,1]` lookup
/// coordinate — mirrors `gradient_extend` in [`GRADIENT_MSL`](../../shader).
/// 0=clamp, 1=repeat (fract), 2=mirror (triangle wave over period 2).
fn gradient_extend(mode: u32, t: f32) -> f32 {
    if mode == 1 {
        return t - t.floor();
    }
    if mode == 2 {
        let u = t - 2.0 * (t * 0.5).floor();
        return if u > 1.0 { 2.0 - u } else { u };
    }
    t.clamp(0.0, 1.0)
}

/// Sample a premultiplied-linear texture at normalized `(u, v)` with the given
/// filter and address mode, using a texel-center `-0.5` convention (so `u = 0.5
/// / width` hits texel 0's center).
fn sample_texel(
    texels: &[[f32; 4]],
    tw: u32,
    th: u32,
    u: f32,
    v: f32,
    samp: &SamplerDesc,
) -> [f32; 4] {
    use crate::resource::{AddressMode, FilterMode};

    // Texel-space coordinates (continuous), texel centers at integer + 0.5.
    let tx = u * tw as f32 - 0.5;
    let ty = v * th as f32 - 0.5;

    // Mirror one integer axis coordinate into `[0, n)` as a period-2n triangle
    // wave: within `[0, n)` identity, within `[n, 2n)` reflected — the same
    // reflection the gradient extend-mirror uses, one dimension at a time.
    let mirror = |i: i32, n: i32| -> i32 {
        let period = 2 * n;
        let m = i.rem_euclid(period);
        if m < n { m } else { period - 1 - m }
    };

    // Fetch one texel with the address mode applied to integer coords.
    let fetch = |ix: i32, iy: i32| -> [f32; 4] {
        let (cx, cy) = match samp.address {
            AddressMode::ClampToEdge => (ix.clamp(0, tw as i32 - 1), iy.clamp(0, th as i32 - 1)),
            AddressMode::Repeat => (ix.rem_euclid(tw as i32), iy.rem_euclid(th as i32)),
            AddressMode::Mirror => (mirror(ix, tw as i32), mirror(iy, th as i32)),
        };
        texels[(cy as u32 * tw + cx as u32) as usize]
    };

    match samp.filter {
        FilterMode::Nearest => fetch(tx.round() as i32, ty.round() as i32),
        // The headless raster has no mip chain, so trilinear degrades to
        // bilinear on the base level (see `FilterMode::MipmapLinear`); the mip
        // blend only takes effect on a device backend.
        FilterMode::Linear | FilterMode::MipmapLinear => {
            let x0 = tx.floor();
            let y0 = ty.floor();
            let fx = tx - x0;
            let fy = ty - y0;
            let (x0i, y0i) = (x0 as i32, y0 as i32);
            let c00 = fetch(x0i, y0i);
            let c10 = fetch(x0i + 1, y0i);
            let c01 = fetch(x0i, y0i + 1);
            let c11 = fetch(x0i + 1, y0i + 1);
            let mut out = [0.0f32; 4];
            for i in 0..4 {
                let top = c00[i] * (1.0 - fx) + c10[i] * fx;
                let bot = c01[i] * (1.0 - fx) + c11[i] * fx;
                out[i] = top * (1.0 - fy) + bot * fy;
            }
            out
        }
    }
}

/// Read the `n`-th `u32` index from a packed index buffer (little-endian).
fn read_index(bytes: &[u8], format: IndexFormat, n: u32) -> u32 {
    match format {
        IndexFormat::U16 => {
            let off = n as usize * 2;
            u16::from_le_bytes(bytes[off..off + 2].try_into().unwrap()) as u32
        }
        IndexFormat::U32 => {
            let off = n as usize * 4;
            u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
        }
    }
}

/// Twice the signed area of triangle `(a, b, c)` — the 2D edge function. Used
/// both as the barycentric denominator and (with the query point as `c`) for the
/// per-vertex weights.
fn edge_fn(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Signed distance to a rounded box (IQ), negative inside. `k` is the already
/// doubled-and-clamped corner radius.
fn box_sdf(p: [f32; 2], center: [f32; 2], half: [f32; 2], k: f32) -> f32 {
    let q = [
        (p[0] - center[0]).abs() - (half[0] - k),
        (p[1] - center[1]).abs() - (half[1] - k),
    ];
    let mx = [q[0].max(0.0), q[1].max(0.0)];
    let outside = (mx[0] * mx[0] + mx[1] * mx[1]).sqrt();
    outside + q[0].max(q[1]).min(0.0) - k
}

/// Signed distance to a rounded box with an independent radius per corner
/// (`radii` ordered left-top, right-top, right-bottom, left-bottom), negative
/// inside. Mirrors the MSL `rrect_sdf`: the active corner's radius is selected by
/// the sample's quadrant, clamped to the box, then folded into `box_sdf`.
fn rrect_sdf(p: [f32; 2], center: [f32; 2], half: [f32; 2], radii: [f32; 4]) -> f32 {
    let d = [p[0] - center[0], p[1] - center[1]];
    // Quadrant select: x<0 picks a left corner, y<0 picks a top corner.
    let r = if d[0] < 0.0 {
        if d[1] < 0.0 { radii[0] } else { radii[3] }
    } else if d[1] < 0.0 {
        radii[1]
    } else {
        radii[2]
    };
    let k = (2.0 * r).min(half[0].min(half[1]));
    box_sdf(p, center, half, k)
}

/// Grain in `[-0.5, 0.5]` from an integer-lattice hash of the device pixel.
///
/// Mirrors the MSL `grain` operation for operation: unsigned wraparound multiply
/// and shift are exactly defined in both languages, so the CPU and GPU values are
/// bit-identical, and the only input is the integer pixel — never time — so a
/// static frosted surface renders the same grain every frame.
fn grain(p: [f32; 2]) -> f32 {
    let x = p[0].floor() as i32 as u32;
    let y = p[1].floor() as i32 as u32;
    let mut h = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h & 0x00FF_FFFF) as f32 / 16_777_215.0 - 0.5
}

/// Signed distance to an axis-aligned ellipse, negative inside. Mirrors the MSL
/// `ellipse_sdf`: the point is normalized by the per-axis radii, offset by the
/// unit circle, then scaled back by the smaller radius for the AA ramp.
fn ellipse_sdf(p: [f32; 2], center: [f32; 2], radii: [f32; 2]) -> f32 {
    let n = [(p[0] - center[0]) / radii[0], (p[1] - center[1]) / radii[1]];
    ((n[0] * n[0] + n[1] * n[1]).sqrt() - 1.0) * radii[0].min(radii[1])
}

/// Signed distance to a capsule/stadium, negative inside. Mirrors the MSL
/// `capsule_sdf`: a rounded box whose corner radius is the smaller half-extent,
/// so the short axis is fully rounded and the long axis stays straight. `half`
/// are the box half-extents; the radius is derived as their minimum, which is
/// exactly [`box_sdf`] with `k = min(half.x, half.y)`.
fn capsule_sdf(p: [f32; 2], center: [f32; 2], half: [f32; 2]) -> f32 {
    let k = half[0].min(half[1]);
    box_sdf(p, center, half, k)
}

/// Signed distance for the AnalyticShadow family, evaluated in shape-centered
/// space (`p` is already relative to the shape center), negative inside. Mirrors
/// the MSL `shadow_sdf`: `shape` 1 selects the ellipse, 2 the capsule, anything
/// else the per-corner rounded box. The ellipse arm clamps the half-extents to
/// `1e-4` before normalizing — matching the shader's guard against a zero radius,
/// which the sharp-shape [`ellipse_sdf`] deliberately omits.
fn shadow_sdf(shape: u32, p: [f32; 2], half_ext: [f32; 2], radii: [f32; 4]) -> f32 {
    match shape {
        1 => {
            let r = [half_ext[0].max(1e-4), half_ext[1].max(1e-4)];
            let n = [p[0] / r[0], p[1] / r[1]];
            ((n[0] * n[0] + n[1] * n[1]).sqrt() - 1.0) * r[0].min(r[1])
        }
        2 => capsule_sdf(p, [0.0, 0.0], half_ext),
        _ => rrect_sdf(p, [0.0, 0.0], half_ext, radii),
    }
}

/// Rational approximation of the Gauss error function (Abramowitz & Stegun
/// 7.1.26, max abs error ~1.5e-7), byte-exact against the MSL `erf_approx`. Used
/// to map the shadow SDF through a Gaussian coverage ramp
/// (`0.5*(1 - erf(d/(sqrt2*sigma)))`).
#[allow(clippy::excessive_precision)] // A&S coefficients kept byte-exact vs the MSL emitter.
fn erf_approx(x: f32) -> f32 {
    // Match MSL `sign`: sign(0) == 0, so erf(0) == 0 exactly.
    let s = if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    };
    let ax = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * ax);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-ax * ax).exp();
    s * y
}

/// Signed distance to a line segment of half-width `hw` (IQ): project the sample
/// onto the segment (clamped to its endpoints) and subtract the half-width.
/// Mirrors the AnalyticLine fragment's `segment_sdf` exactly.
fn segment_sdf(p: [f32; 2], a: [f32; 2], b: [f32; 2], hw: f32) -> f32 {
    let pa = [p[0] - a[0], p[1] - a[1]];
    let ba = [b[0] - a[0], b[1] - a[1]];
    let denom = ba[0] * ba[0] + ba[1] * ba[1];
    let h = if denom > 0.0 {
        ((pa[0] * ba[0] + pa[1] * ba[1]) / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let dx = pa[0] - ba[0] * h;
    let dy = pa[1] - ba[1] * h;
    (dx * dx + dy * dy).sqrt() - hw
}

/// Signed distance for a butt (`cap_ext == 0`) or square (`cap_ext == hw`) cap:
/// the segment SDF intersected with the two perpendicular end half-planes.
/// Mirrors the AnalyticLine fragment's `capped_segment_sdf` exactly.
fn capped_segment_sdf(p: [f32; 2], a: [f32; 2], b: [f32; 2], hw: f32, cap_ext: f32) -> f32 {
    let ba = [b[0] - a[0], b[1] - a[1]];
    let len = (ba[0] * ba[0] + ba[1] * ba[1]).sqrt();
    let dir = if len > 0.0 {
        [ba[0] / len, ba[1] / len]
    } else {
        [1.0, 0.0]
    };
    let t = (p[0] - a[0]) * dir[0] + (p[1] - a[1]) * dir[1];
    let d = segment_sdf(p, a, b, hw);
    let end_d = (-(t + cap_ext)).max(t - (len + cap_ext));
    d.max(end_d)
}

/// The whole advanced-blend composite for one pixel: premultiplied source and
/// destination in, the premultiplied result out.
///
/// `mode` is the render crate's `Blend` discriminant — this crate sits below
/// `viso-render` and cannot name that type, so the numbering is the ABI:
/// `0..=12` the Porter-Duff operators plus `Plus`, `13..=23` the W3C separable
/// artistic modes, `24..=27` the non-separable HSL ones. A CPU mirror of
/// `ADVANCED_BLEND_FRAGMENT_BODY`, so pixel tests here pin the same math the
/// Metal fragment runs.
fn blend_composite(mode: u32, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    let (sa, da) = (s[3], d[3]);
    let out = if mode <= 12 {
        // Linear in the premultiplied operands: no unpremultiply needed.
        let (fa, fb) = porter_duff(mode, sa, da);
        [
            fa * s[0] + fb * d[0],
            fa * s[1] + fb * d[1],
            fa * s[2] + fb * d[2],
            fa * sa + fb * da,
        ]
    } else {
        // The advanced modes are defined on straight color, with the union of the
        // two coverages as the output alpha:
        //   co = as*(1-ab)*cs + as*ab*B(cb,cs) + (1-as)*ab*cb
        let straight = |c: [f32; 4], a: f32| {
            if a > 0.0 {
                [c[0] / a, c[1] / a, c[2] / a]
            } else {
                [0.0; 3]
            }
        };
        let cs = straight(s, sa);
        let cb = straight(d, da);
        let b = if mode <= 23 {
            [
                blend_separable(mode, cb[0], cs[0]),
                blend_separable(mode, cb[1], cs[1]),
                blend_separable(mode, cb[2], cs[2]),
            ]
        } else {
            blend_nonseparable(mode, cb, cs)
        };
        let mut out = [0.0f32; 4];
        for c in 0..3 {
            out[c] = sa * (1.0 - da) * cs[c] + sa * da * b[c] + (1.0 - sa) * da * cb[c];
        }
        out[3] = sa + da - sa * da;
        out
    };
    [
        out[0].clamp(0.0, 1.0),
        out[1].clamp(0.0, 1.0),
        out[2].clamp(0.0, 1.0),
        out[3].clamp(0.0, 1.0),
    ]
}

/// The Porter-Duff coverage pair `(fa, fb)` for modes `0..=12`. `Plus` is
/// `(1, 1)` and relies on the caller's clamp, which is also the safe default for
/// an out-of-range mode.
fn porter_duff(mode: u32, sa: f32, da: f32) -> (f32, f32) {
    match mode {
        0 => (0.0, 0.0),
        1 => (1.0, 0.0),
        2 => (0.0, 1.0),
        3 => (1.0, 1.0 - sa),
        4 => (1.0 - da, 1.0),
        5 => (da, 0.0),
        6 => (0.0, sa),
        7 => (1.0 - da, 0.0),
        8 => (0.0, 1.0 - sa),
        9 => (da, 1.0 - sa),
        10 => (1.0 - da, sa),
        11 => (1.0 - da, 1.0 - sa),
        _ => (1.0, 1.0),
    }
}

/// `B(cb, cs)` for the separable artistic modes `13..=23`, one channel of
/// straight color. Overlay is hard-light with its arguments swapped; the
/// dodge/burn guards are the W3C limits, which keep the divisions finite at full
/// coverage.
fn blend_separable(mode: u32, cb: f32, cs: f32) -> f32 {
    let screen = |cb: f32, cs: f32| cb + cs - cb * cs;
    let hard_light = |cb: f32, cs: f32| {
        if cs <= 0.5 {
            cb * 2.0 * cs
        } else {
            screen(cb, 2.0 * cs - 1.0)
        }
    };
    match mode {
        13 => cb * cs,
        14 => screen(cb, cs),
        15 => hard_light(cs, cb),
        16 => cb.min(cs),
        17 => cb.max(cs),
        18 => {
            if cb <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        19 => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        20 => hard_light(cb, cs),
        21 => {
            let d = if cb <= 0.25 {
                ((16.0 * cb - 12.0) * cb + 4.0) * cb
            } else {
                cb.sqrt()
            };
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        22 => (cb - cs).abs(),
        _ => cb + cs - 2.0 * cb * cs,
    }
}

/// `B(cb, cs)` for the non-separable modes `24..=27`, which mix a hue /
/// saturation / luminosity component of one operand into the other. Verbatim from
/// the W3C compositing model: luminosity is the Rec.601 luma, and clipping keeps a
/// relit color in gamut by scaling it about its own luminosity rather than
/// clamping per channel.
fn blend_nonseparable(mode: u32, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    let lum = |c: [f32; 3]| 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
    let clip_color = |c: [f32; 3]| {
        let l = lum(c);
        let n = c[0].min(c[1]).min(c[2]);
        let x = c[0].max(c[1]).max(c[2]);
        let mut c = c;
        if n < 0.0 {
            let k = l / (l - n).max(1e-6);
            for v in c.iter_mut() {
                *v = l + (*v - l) * k;
            }
        }
        if x > 1.0 {
            let k = (1.0 - l) / (x - l).max(1e-6);
            for v in c.iter_mut() {
                *v = l + (*v - l) * k;
            }
        }
        c
    };
    let set_lum = |c: [f32; 3], l: f32| {
        let d = l - lum(c);
        clip_color([c[0] + d, c[1] + d, c[2] + d])
    };
    let sat = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
    let set_sat = |c: [f32; 3], s: f32| {
        let n = c[0].min(c[1]).min(c[2]);
        let x = c[0].max(c[1]).max(c[2]);
        if x > n {
            [
                (c[0] - n) * s / (x - n),
                (c[1] - n) * s / (x - n),
                (c[2] - n) * s / (x - n),
            ]
        } else {
            [0.0; 3]
        }
    };
    match mode {
        24 => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        25 => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        26 => set_lum(cs, lum(cb)),
        _ => set_lum(cb, lum(cs)),
    }
}

/// `round(clamp(v, 0, 1) * 255) / 255` — quantize a channel to 8-bit.
/// Quantize a premultiplied source color to what `format` can actually store.
///
/// An 8-bit unorm attachment rounds every channel to a byte and clamps to
/// `[0, 1]`; an extended-range one stores the value as given, which is the whole
/// reason it was chosen for the target.
fn quantize_for(format: TextureFormat, src: [f32; 4]) -> [f32; 4] {
    if format.is_extended_range() {
        return src;
    }
    [
        quantize_unorm8(src[0]),
        quantize_unorm8(src[1]),
        quantize_unorm8(src[2]),
        quantize_unorm8(src[3]),
    ]
}

fn quantize_unorm8(v: f32) -> f32 {
    (v.clamp(0.0, 1.0) * 255.0).round() / 255.0
}

/// `round(clamp(v, 0, 1) * 255)` as a byte.
fn to_unorm8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Un-premultiply a premultiplied color (guards `a == 0`).
fn unpremultiply(r: f32, g: f32, b: f32, a: f32) -> (f32, f32, f32) {
    if a <= 0.0 {
        (0.0, 0.0, 0.0)
    } else {
        (r / a, g / a, b / a)
    }
}

/// Decode one texel's bytes into premultiplied linear RGBA.
fn decode_texel(format: TextureFormat, bytes: &[u8]) -> [f32; 4] {
    match format {
        TextureFormat::Rgba8Unorm => {
            let (r, g, b, a) = (
                bytes[0] as f32 / 255.0,
                bytes[1] as f32 / 255.0,
                bytes[2] as f32 / 255.0,
                bytes[3] as f32 / 255.0,
            );
            [r * a, g * a, b * a, a]
        }
        TextureFormat::Bgra8Unorm => {
            let (b, g, r, a) = (
                bytes[0] as f32 / 255.0,
                bytes[1] as f32 / 255.0,
                bytes[2] as f32 / 255.0,
                bytes[3] as f32 / 255.0,
            );
            [r * a, g * a, b * a, a]
        }
        // Extended range: the same straight-alpha convention as the unorm
        // formats, but the channels are not clamped, so a value above 1.0
        // survives the upload instead of saturating at the top of the range.
        TextureFormat::Rgba16Float => {
            let ch = |i: usize| f16_to_f32(u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]));
            let (r, g, b, a) = (ch(0), ch(1), ch(2), ch(3));
            [r * a, g * a, b * a, a]
        }
        // Single coverage channel: replicated as premultiplied white * coverage.
        TextureFormat::R8Unorm => {
            let a = bytes[0] as f32 / 255.0;
            [a, a, a, a]
        }
        TextureFormat::Depth32Float => [0.0; 4],
    }
}

/// Read a named `Float1` field from instance bytes.
fn read_f1(layout: &InstanceLayout, inst: &[u8], name: &str) -> f32 {
    let off = field_offset(layout, name, AttrFormat::Float1);
    f32::from_le_bytes(inst[off..off + 4].try_into().unwrap())
}

/// Read a named `Uint1` field from instance bytes.
fn read_u1(layout: &InstanceLayout, inst: &[u8], name: &str) -> u32 {
    let off = field_offset(layout, name, AttrFormat::Uint1);
    u32::from_le_bytes(inst[off..off + 4].try_into().unwrap())
}

/// Read a named `Float2` field from instance bytes.
fn read_f2(layout: &InstanceLayout, inst: &[u8], name: &str) -> [f32; 2] {
    let off = field_offset(layout, name, AttrFormat::Float2);
    [
        f32::from_le_bytes(inst[off..off + 4].try_into().unwrap()),
        f32::from_le_bytes(inst[off + 4..off + 8].try_into().unwrap()),
    ]
}

/// Read a named `Float4` field from instance bytes.
fn read_f4(layout: &InstanceLayout, inst: &[u8], name: &str) -> [f32; 4] {
    let off = field_offset(layout, name, AttrFormat::Float4);
    [
        f32::from_le_bytes(inst[off..off + 4].try_into().unwrap()),
        f32::from_le_bytes(inst[off + 4..off + 8].try_into().unwrap()),
        f32::from_le_bytes(inst[off + 8..off + 12].try_into().unwrap()),
        f32::from_le_bytes(inst[off + 12..off + 16].try_into().unwrap()),
    ]
}

/// Look up a field's byte offset by name, asserting its format matches. The
/// layout was validated against the shader schema at pipeline registration, so
/// a missing/mismatched field here is a programming error in the built-in.
fn field_offset(layout: &InstanceLayout, name: &str, want: AttrFormat) -> usize {
    let f = layout
        .fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("built-in shader expects instance field `{name}`"));
    assert_eq!(
        f.format, want,
        "instance field `{name}` has format {:?}, built-in expects {want:?}",
        f.format
    );
    f.offset
}

#[cfg(test)]
mod sampler_tests {
    use super::sample_texel;
    use crate::resource::{AddressMode, FilterMode, SamplerDesc};

    /// A 2×1 texture: texel 0 is red, texel 1 is green (both opaque, premul).
    fn two_texel() -> Vec<[f32; 4]> {
        vec![[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]]
    }

    fn nearest(address: AddressMode) -> SamplerDesc {
        SamplerDesc {
            filter: FilterMode::Nearest,
            address,
        }
    }

    #[test]
    fn mirror_reflects_across_the_edge() {
        let tex = two_texel();
        let samp = nearest(AddressMode::Mirror);
        // Inside [0,1): texel-center sampling — u in [0,0.5) → texel 0 (red),
        // u in [0.5,1) → texel 1 (green).
        assert_eq!(
            sample_texel(&tex, 2, 1, 0.25, 0.5, &samp),
            [1.0, 0.0, 0.0, 1.0]
        );
        assert_eq!(
            sample_texel(&tex, 2, 1, 0.75, 0.5, &samp),
            [0.0, 1.0, 0.0, 1.0]
        );
        // Second period [1,2) is mirrored: near u=1 stays green (last texel),
        // near u=2 reflects back to red (first texel).
        assert_eq!(
            sample_texel(&tex, 2, 1, 1.25, 0.5, &samp),
            [0.0, 1.0, 0.0, 1.0]
        );
        assert_eq!(
            sample_texel(&tex, 2, 1, 1.75, 0.5, &samp),
            [1.0, 0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn mirror_differs_from_repeat_in_the_reflected_period() {
        let tex = two_texel();
        // In the second period, Repeat wraps (green at u=1.25) while Mirror
        // reflects (still green near the seam but red at the far end).
        let repeat = sample_texel(&tex, 2, 1, 1.75, 0.5, &nearest(AddressMode::Repeat));
        let mirror = sample_texel(&tex, 2, 1, 1.75, 0.5, &nearest(AddressMode::Mirror));
        assert_eq!(repeat, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(mirror, [1.0, 0.0, 0.0, 1.0]);
        assert_ne!(repeat, mirror);
    }

    #[test]
    fn mipmap_linear_degrades_to_linear_in_headless() {
        // Headless has no mip chain, so MipmapLinear must sample identically to
        // Linear on the base level.
        let tex = two_texel();
        let base = SamplerDesc {
            filter: FilterMode::Linear,
            address: AddressMode::ClampToEdge,
        };
        let mip = SamplerDesc {
            filter: FilterMode::MipmapLinear,
            address: AddressMode::ClampToEdge,
        };
        for &u in &[0.0, 0.3, 0.5, 0.8, 1.0] {
            assert_eq!(
                sample_texel(&tex, 2, 1, u, 0.5, &base),
                sample_texel(&tex, 2, 1, u, 0.5, &mip),
                "MipmapLinear must match Linear at u={u}"
            );
        }
    }
}
