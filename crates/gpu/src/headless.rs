//! `HeadlessRaster` — a CPU software rasterizer implementing [`GpuBackend`].
//!
//! This backend exists so golden-image tests (and CI machines without a GPU)
//! can render a full frame and read the pixels back. It has no shader compiler:
//! it dispatches on each pipeline's [`BuiltinShader`] tag to a hand-written CPU
//! fill routine that reproduces the corresponding Metal shader's SDF / AA /
//! blend math. The output is intended to match the Metal backend to within a
//! small per-channel tolerance.
//!
//! ## Fidelity model (behavior ported from makepad, rewritten natively)
//!
//! makepad's own headless path JIT-compiles the *same* shader Metal runs, so it
//! is exact by construction. Viso instead re-derives the math in fresh Rust,
//! matching makepad's observable behavior (`platform/src/os/headless/`):
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
    DrawCommand, DrawList, Frame, Geometry, GpuBackend, LoadOp, RenderPass, RenderTarget,
};
use crate::instance::{AttrFormat, InstanceLayout};
use crate::resource::{
    BindGroupDesc, BufferDesc, BuiltinShader, Caps, PipelineDesc, SamplerDesc, TextureDesc,
    TextureFormat,
};
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
    /// Premultiplied linear RGBA, row-major top-left.
    color: Vec<[f32; 4]>,
}

/// A CPU software-rasterizer backend.
pub struct HeadlessRaster {
    buffers: Vec<HeadlessBuffer>,
    textures: Vec<HeadlessTexture>,
    samplers: Vec<SamplerDesc>,
    pipelines: Vec<HeadlessPipeline>,
    bind_groups: Vec<HeadlessBindGroup>,
    surfaces: Vec<HeadlessSurface>,
    caps: Caps,
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
            buffers: Vec::new(),
            textures: Vec::new(),
            samplers: Vec::new(),
            pipelines: Vec::new(),
            bind_groups: Vec::new(),
            surfaces: Vec::new(),
            caps: Caps {
                max_texture_size: 16384,
                presents_to_display: false,
            },
        }
    }

    /// Read the last-presented framebuffer of `surface` as tightly-packed
    /// **BGRA8** bytes, top-left origin (un-premultiplied, `round(v * 255)`).
    ///
    /// This is the golden-test capture path; it mirrors a Metal texture
    /// readback of a `Bgra8Unorm` swapchain.
    pub fn read_pixels_bgra8(&self, surface: SurfaceId) -> Vec<u8> {
        let s = &self.surfaces[surface.0 as usize];
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

    /// Sample one texel `[f32; 4]` (premultiplied linear) from `surface` at
    /// pixel `(x, y)`, top-left origin. Convenience for single-pixel assertions.
    pub fn surface_texel(&self, surface: SurfaceId, x: u32, y: u32) -> [f32; 4] {
        let s = &self.surfaces[surface.0 as usize];
        s.color[(y * s.width + x) as usize]
    }

    /// The number of buffers created over this backend's lifetime.
    ///
    /// Resources are only ever pushed (never removed), so this is the
    /// cumulative create count. A steady-state frame that reuses persistent
    /// buffers leaves it unchanged, which the renderer bench asserts to guard
    /// the hot-path allocation contract (§7.1, §17.4).
    pub fn buffer_count(&self) -> usize {
        self.buffers.len()
    }

    /// The number of textures created over this backend's lifetime.
    ///
    /// Like [`buffer_count`](Self::buffer_count), this only grows on a genuine
    /// create; a steady-state frame whose translucent layers reuse pooled
    /// offscreen textures leaves it unchanged.
    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    /// The number of bind groups created over this backend's lifetime.
    ///
    /// Like [`buffer_count`](Self::buffer_count), this only grows on a genuine
    /// create; cached per-texture bind groups keep it stable across steady
    /// frames.
    pub fn bind_group_count(&self) -> usize {
        self.bind_groups.len()
    }
}

impl GpuBackend for HeadlessRaster {
    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        let id = BufferId(self.buffers.len() as u32);
        self.buffers.push(HeadlessBuffer {
            bytes: vec![0u8; desc.size],
        });
        id
    }

    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId {
        let id = TextureId(self.textures.len() as u32);
        let count = (desc.width * desc.height) as usize;
        self.textures.push(HeadlessTexture {
            width: desc.width,
            height: desc.height,
            format: desc.format,
            texels: vec![[0.0; 4]; count],
        });
        id
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        let id = SamplerId(self.samplers.len() as u32);
        self.samplers.push(*desc);
        id
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, crate::instance::LayoutError> {
        // Registration-time layout check (§32): the derived instance layout must
        // match the shader's declared schema before the pipeline is usable.
        layout.validate_against(&desc.instance_schema)?;
        let id = PipelineId(self.pipelines.len() as u32);
        self.pipelines.push(HeadlessPipeline {
            builtin: desc.builtin,
            layout: *layout,
        });
        Ok(id)
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        let id = BindGroupId(self.bind_groups.len() as u32);
        self.bind_groups.push(HeadlessBindGroup {
            bindings: desc.bindings.clone(),
        });
        id
    }

    fn write_buffer(&mut self, id: BufferId, offset: usize, bytes: &[u8]) {
        let buf = &mut self.buffers[id.0 as usize];
        buf.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]) {
        let tex = &mut self.textures[id.0 as usize];
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
        let id = SurfaceId(self.surfaces.len() as u32);
        self.surfaces.push(HeadlessSurface {
            width,
            height,
            format: TextureFormat::Bgra8Unorm,
            color: vec![[0.0; 4]; (width * height) as usize],
        });
        id
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        let s = &mut self.surfaces[id.0 as usize];
        s.width = width;
        s.height = height;
        s.color = vec![[0.0; 4]; (width * height) as usize];
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Frame {
        Frame {
            surface,
            drawable: 0,
        }
    }

    fn encode(&mut self, list: &DrawList<'_>) {
        for pass in list.passes {
            self.encode_pass(pass);
        }
    }

    fn present(&mut self, _frame: Frame) {
        // No swapchain: the framebuffer already holds the final image, ready for
        // `read_pixels_bgra8`.
    }

    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn surface_format(&self, surface: SurfaceId) -> TextureFormat {
        self.surfaces[surface.0 as usize].format
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
    fn encode_pass(&mut self, pass: &RenderPass<'_>) {
        let (target, width, height) = match pass.target {
            RenderTarget::Surface(frame) => {
                let s = &self.surfaces[frame.surface.0 as usize];
                (FbTarget::Surface(frame.surface), s.width, s.height)
            }
            RenderTarget::Texture(id) => {
                let t = &self.textures[id.0 as usize];
                (FbTarget::Texture(id), t.width, t.height)
            }
        };

        if let LoadOp::Clear(rgba) = pass.load {
            match target {
                FbTarget::Surface(id) => self.surfaces[id.0 as usize].color.fill(rgba),
                FbTarget::Texture(id) => self.textures[id.0 as usize].texels.fill(rgba),
            }
        }

        for cmd in pass.commands {
            self.encode_command(target, width, height, cmd);
        }
    }

    /// The framebuffer slice for `target`, for a single blend write. Scoped per
    /// call to keep the `&mut self` borrow narrow (the fill routines snapshot any
    /// sampled source texture out first, so a texture target never aliases here).
    fn framebuffer(&mut self, target: FbTarget) -> &mut [[f32; 4]] {
        match target {
            FbTarget::Surface(id) => &mut self.surfaces[id.0 as usize].color,
            FbTarget::Texture(id) => &mut self.textures[id.0 as usize].texels,
        }
    }

    /// Rasterize one draw command's instances into the target framebuffer.
    fn encode_command(&mut self, target: FbTarget, width: u32, height: u32, cmd: &DrawCommand<'_>) {
        // Copy the pipeline metadata (both `Copy`) so the per-pixel fill can take
        // `&mut self` for blending without aliasing the pipeline/buffer tables.
        let pipeline = &self.pipelines[cmd.pipeline.0 as usize];
        let builtin = pipeline.builtin;
        let layout = pipeline.layout;

        match cmd.geometry {
            Geometry::Generated { count } => {
                // Copy this command's instance bytes out for the same aliasing
                // reason.
                let span = count as usize * layout.stride;
                let instances = self.buffers[cmd.instance_buffer.0 as usize].bytes
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
                        // Path/Mesh never arrive as generated geometry.
                        BuiltinShader::Path | BuiltinShader::Mesh | BuiltinShader::Layer => {}
                    }
                }
            }
            Geometry::IndexedMesh {
                vertex_buffer,
                index_buffer,
                index_offset,
                index_count,
            } => {
                // Snapshot the vertex + index bytes for the same aliasing reason.
                let verts = self.buffers[vertex_buffer.0 as usize].bytes.clone();
                let idx_bytes = self.buffers[index_buffer.0 as usize].bytes.clone();
                self.fill_mesh(
                    target,
                    width,
                    height,
                    &layout,
                    &verts,
                    &idx_bytes,
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
            let i0 = read_index(idx_bytes, base);
            let i1 = read_index(idx_bytes, base + 1);
            let i2 = read_index(idx_bytes, base + 2);
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
                    blend_pixel(self.framebuffer(target), width, px, py, src);
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

        for py in y0..y1 {
            for px in x0..x1 {
                // Sample at the pixel center.
                let p = [px as f32 + 0.5, py as f32 + 0.5];
                let d = box_sdf(p, center, half, k);

                // aa ≈ 1 at 1:1 scale: linear coverage over ~1px.
                let fill_cov = (-d).clamp(0.0, 1.0);
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
                    let bcov = (-(d.abs() - border_w * 0.5)).clamp(0.0, 1.0);
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

                blend_pixel(self.framebuffer(target), width, px, py, src);
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
        for binding in &self.bind_groups[bg.0 as usize].bindings {
            match binding {
                crate::resource::Binding::Texture(t) => tex_id = Some(*t),
                crate::resource::Binding::Sampler(s) => samp = self.samplers[s.0 as usize],
                crate::resource::Binding::Uniform(_) => {}
            }
        }
        let Some(tex_id) = tex_id else { return };
        // Snapshot the texture (dimensions + premultiplied texels) so the
        // per-pixel loop can take `&mut self.surfaces` without aliasing.
        let (tw, th, texels) = {
            let t = &self.textures[tex_id.0 as usize];
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
                blend_pixel(self.framebuffer(target), width, px, py, src);
            }
        }
    }

    /// Fill one glyph instance from an R8 single-channel SDF atlas.
    ///
    /// The instance layout matches [`fill_image`](Self::fill_image) plus a
    /// `px_range` field. The atlas stores a signed distance field (see the text
    /// subsystem's `sdfer` encode): the sampled `.r` channel is the stored SDF
    /// value, with the glyph edge at `SDF_EDGE = 0.75`. Coverage is decoded as
    /// `clamp((sd - 0.75) * px_range + 0.5, 0, 1)`, matching
    /// [`GLYPHRUN_MSL`](../../shader). The run color is premultiplied by that
    /// coverage and blended source-over. A linear sampler gives the SDF its
    /// smooth (bilinearly interpolated) edge.
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
        let px_range = read_f1(layout, inst, "px_range");

        // Resolve the bound R8 atlas and sampler.
        let Some(bg) = bind_group else { return };
        let (mut tex_id, mut samp) = (
            None,
            SamplerDesc {
                filter: crate::resource::FilterMode::Linear,
                address: crate::resource::AddressMode::ClampToEdge,
            },
        );
        for binding in &self.bind_groups[bg.0 as usize].bindings {
            match binding {
                crate::resource::Binding::Texture(t) => tex_id = Some(*t),
                crate::resource::Binding::Sampler(s) => samp = self.samplers[s.0 as usize],
                crate::resource::Binding::Uniform(_) => {}
            }
        }
        let Some(tex_id) = tex_id else { return };
        let (tw, th, texels) = {
            let t = &self.textures[tex_id.0 as usize];
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

                // R8 atlas: decode_texel replicated the stored SDF into every
                // channel, so any channel is the sampled distance.
                let sd = sample_texel(&texels, tw, th, u, v, &samp)[0];
                let cov = ((sd - SDF_EDGE) * px_range + 0.5).clamp(0.0, 1.0);
                let a = base_a * cov;
                if a <= 0.0 {
                    continue;
                }
                let src = [color[0] * a, color[1] * a, color[2] * a, a];
                blend_pixel(self.framebuffer(target), width, px, py, src);
            }
        }
    }
}

/// The SDF edge value stored by the text subsystem's encoder: the glyph
/// boundary (distance 0) is stored at `1 - cutoff = 0.75`. Coverage decodes
/// around this threshold. Kept in sync with the text crate and `GLYPHRUN_MSL`.
const SDF_EDGE: f32 = 0.75;

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

    // Fetch one texel with the address mode applied to integer coords.
    let fetch = |ix: i32, iy: i32| -> [f32; 4] {
        let (cx, cy) = match samp.address {
            AddressMode::ClampToEdge => (ix.clamp(0, tw as i32 - 1), iy.clamp(0, th as i32 - 1)),
            AddressMode::Repeat => (ix.rem_euclid(tw as i32), iy.rem_euclid(th as i32)),
        };
        texels[(cy as u32 * tw + cx as u32) as usize]
    };

    match samp.filter {
        FilterMode::Nearest => fetch(tx.round() as i32, ty.round() as i32),
        FilterMode::Linear => {
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
fn read_index(bytes: &[u8], n: u32) -> u32 {
    let off = n as usize * 4;
    u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
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

/// Premultiplied source-over into one framebuffer pixel, quantizing the source
/// to 8-bit first so the result is byte-exact against a real Bgra8 target.
fn blend_pixel(fb: &mut [[f32; 4]], width: u32, px: u32, py: u32, src: [f32; 4]) {
    let src = [
        quantize_unorm8(src[0]),
        quantize_unorm8(src[1]),
        quantize_unorm8(src[2]),
        quantize_unorm8(src[3]),
    ];
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

/// `round(clamp(v, 0, 1) * 255) / 255` — quantize a channel to 8-bit.
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
