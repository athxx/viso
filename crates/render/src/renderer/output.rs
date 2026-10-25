//! The output encode of a surface whose format cannot encode.
//!
//! A frame is blended in linear light and stored sRGB-encoded (Rendering
//! §5.4): an `*Srgb` attachment encodes as it is written. A surface without
//! such a format — a GLES window where EGL offers no sRGB colorspace, an
//! `Rgb565Unorm` panel, any unorm surface a backend reports — would store
//! the linear values as they are, every color darker than authored. For such
//! a surface the renderer draws the frame into a working target of the
//! surface's sRGB format, retained across frames, and one last pass samples
//! it and writes the encoding into the surface (the OutputEncode built-in),
//! dithered by one step on a panel narrower than 8 bits. The transfer
//! function is thus applied exactly once, by that pass.
//!
//! The working target always holds the last frame, so a partial redraw
//! redraws into it only what changed; the encode pass then writes it whole,
//! whatever the drawable held.

use viso_gpu::backend::{DrawCommand, Geometry, LoadOp, RenderPass, RenderTarget};
use viso_gpu::{
    BindGroupDesc, BindGroupId, Binding, BufferDesc, BufferId, BufferUsage, ColorSpace, GpuBackend,
    PipelineDesc, PipelineId, SamplerId, TextureDesc, TextureFormat, TextureId,
};
use viso_math::Affine2;
use viso_shader::{PipelineFamily, standard_manifest};

use super::view_uniforms;
use crate::primitive::OutputEncodeInstance;
use crate::scene::key::pod_bytes;

/// The working format a renderer for a surface of `format` in `space` draws
/// in, when the surface cannot encode: its sRGB variant, or for an
/// `Rgb565Unorm` panel the backend does not encode (`encodes_packed`) the
/// narrowest sRGB format with alpha. `None` for a surface that encodes
/// itself, or whose space has no sRGB transfer (an extended-range target is
/// linear).
pub(super) fn working_format(
    format: TextureFormat,
    space: ColorSpace,
    encodes_packed: bool,
) -> Option<TextureFormat> {
    if !matches!(space, ColorSpace::Srgb | ColorSpace::DisplayP3) || format.is_srgb() {
        return None;
    }
    match format {
        TextureFormat::Rgb565Unorm if !encodes_packed => Some(TextureFormat::Bgra8UnormSrgb),
        format => format.srgb(),
    }
}

/// The working target and the pass that encodes it into the surface.
pub(super) struct OutputEncode {
    pipeline: PipelineId,
    sampler: SamplerId,
    format: TextureFormat,
    dither: f32,
    instance: BufferId,
    work: Option<Work>,
}

/// The working target at one size.
struct Work {
    texture: TextureId,
    bind_group: BindGroupId,
    size: [u32; 2],
    /// Whether it holds the frame last presented.
    current: bool,
}

impl OutputEncode {
    /// The encode into a surface of `surface`, from a working target of
    /// `working`, sampled through `sampler`.
    pub(super) fn new<B: GpuBackend>(
        backend: &mut B,
        surface: TextureFormat,
        working: TextureFormat,
        sampler: SamplerId,
    ) -> OutputEncode {
        let entry = standard_manifest()
            .entry(PipelineFamily::OutputEncode)
            .expect("the standard manifest has the output encode");
        let pipeline = backend
            .create_pipeline(
                &PipelineDesc {
                    label: "output-encode",
                    builtin: entry.builtin,
                    variant: entry.variant.packed(),
                    code: entry.code(B::SHADER_LANG),
                    vertex_entry: entry.vertex_entry,
                    fragment_entry: entry.fragment_entry,
                    color_format: surface,
                    depth: None,
                    cull: viso_gpu::CullMode::None,
                    blend: entry.blend,
                    instance_schema: entry.schema,
                },
                &OutputEncodeInstance::LAYOUT,
            )
            .expect("OutputEncodeInstance layout matches the output-encode shader schema");
        let instance = backend.create_buffer(&BufferDesc {
            size: size_of::<OutputEncodeInstance>(),
            usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
            label: "output-encode",
        });
        OutputEncode {
            pipeline,
            sampler,
            format: working,
            // A 5-bit channel's step; an 8-bit surface's quantization is the
            // one an sRGB attachment would make.
            dither: if surface == TextureFormat::Rgb565Unorm {
                1.0 / 31.0
            } else {
                0.0
            },
            instance,
            work: None,
        }
    }

    /// Marks what the working target holds as not presented: a frame drawn
    /// into it that went elsewhere.
    pub(super) fn forget(&mut self) {
        if let Some(work) = &mut self.work {
            work.current = false;
        }
    }

    /// How many presents ago the frame the working target holds at `size`
    /// physical pixels was presented, as [`GpuBackend::frame_age`] counts: `1`
    /// when it holds the last one, `0` when it is yet to be made, of another
    /// size, or holds a frame that was not presented.
    pub(super) fn age(&self, size: [u32; 2]) -> u32 {
        let size = [size[0].max(1), size[1].max(1)];
        u32::from(
            self.work
                .as_ref()
                .is_some_and(|w| w.size == size && w.current),
        )
    }

    /// The working target of `size` physical pixels a frame to be presented
    /// draws into, made anew when the size changed.
    pub(super) fn target<B: GpuBackend>(&mut self, backend: &mut B, size: [u32; 2]) -> TextureId {
        let size = [size[0].max(1), size[1].max(1)];
        if let Some(work) = self.work.as_mut().filter(|w| w.size == size) {
            work.current = true;
            return work.texture;
        }
        if let Some(old) = self.work.take() {
            backend.destroy_bind_group(old.bind_group);
            backend.destroy_texture(old.texture);
        }
        let texture = backend.create_texture(&TextureDesc {
            width: size[0],
            height: size[1],
            format: self.format,
            render_target: true,
            label: "output-working",
        });
        let bind_group = backend.create_bind_group(&BindGroupDesc {
            label: "output-working",
            bindings: vec![Binding::Texture(texture), Binding::Sampler(self.sampler)],
        });
        self.work = Some(Work {
            texture,
            bind_group,
            size,
            current: true,
        });
        texture
    }

    /// The pass writing the working target's encoding into `into`, a target
    /// of its size. It is laid out in physical pixels, so the dither steps
    /// per pixel.
    pub(super) fn push<B: GpuBackend>(
        &self,
        backend: &mut B,
        commands: &mut Vec<DrawCommand>,
        passes: &mut Vec<RenderPass>,
        into: RenderTarget,
    ) {
        let Some(work) = &self.work else {
            return;
        };
        let viewport = [work.size[0] as f32, work.size[1] as f32];
        let instance = OutputEncodeInstance {
            rect_pos: [0.0, 0.0],
            rect_size: viewport,
            uv_pos: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            dither: self.dither,
        };
        backend.write_buffer(self.instance, 0, pod_bytes(&[instance]));
        let first_command = commands.len() as u32;
        commands.push(DrawCommand {
            pipeline: self.pipeline,
            bind_group: Some(work.bind_group),
            geometry: Geometry::Generated { count: 1 },
            instance_buffer: self.instance,
            instance_offset: 0,
            uniforms: view_uniforms(viewport, Affine2::IDENTITY),
            scissor: None,
        });
        passes.push(RenderPass {
            target: into,
            load: LoadOp::Clear([0.0; 4]),
            depth: None,
            first_command,
            command_count: 1,
        });
    }
}
