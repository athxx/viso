//! Pictures in Y′CbCr another framework decoded — an Android hardware buffer
//! — turned on the GPU into a texture of the backend's own: sampled through
//! a Y′CbCr conversion and drawn into the texture's UNORM view, so the
//! gamma-encoded RGB the conversion gives is stored as it is and the texture
//! samples it as sRGB, as any other color image.
//!
//! A conversion, the sampler it is immutable in and the pipeline drawing
//! through it are made once per encoding; a source image, its view and its
//! descriptor set once per buffer, a few kept while their producers reuse
//! them.

// Only Android hands over pictures to convert; elsewhere the tests convert
// planes of their own.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::io::Cursor;
use std::sync::Arc;

use ash::vk;
use viso_handle::ExternalHolds;

use crate::retire::Epoch;

const VERTEX: &[u8] = include_bytes!("ycbcr.vert.spv");
const FRAGMENT: &[u8] = include_bytes!("ycbcr.frag.spv");

/// How many sources stay made: a few players' buffers.
const SOURCES: usize = 16;

/// What a conversion is made for: the planes' format, or the device's
/// external one, and how they encode color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Encoding {
    /// The planes' format; `UNDEFINED` for an external one.
    pub(super) format: vk::Format,
    /// The device's external format, or 0.
    pub(super) external: u64,
    pub(super) model: vk::SamplerYcbcrModelConversion,
    pub(super) range: vk::SamplerYcbcrRange,
    /// The swizzle of the planes' components, `r`, `g`, `b`, `a`.
    pub(super) components: [vk::ComponentSwizzle; 4],
    pub(super) x_chroma: vk::ChromaLocation,
    pub(super) y_chroma: vk::ChromaLocation,
    /// Whether chroma is reconstructed linearly rather than nearest.
    pub(super) linear: bool,
}

/// One encoding's conversion and the pipeline drawing through it.
struct Kind {
    encoding: Encoding,
    conversion: vk::SamplerYcbcrConversion,
    sampler: vk::Sampler,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

/// A picture's image in Y′CbCr, viewed through its conversion.
pub(super) struct Source {
    /// The holds of the buffer it was made of, which name it while it is
    /// kept; `None` for one of the backend's own.
    holds: Option<Arc<ExternalHolds>>,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
    kind: usize,
    /// Its size in texels.
    size: (u32, u32),
    /// Owned by a queue outside Vulkan, a decoder's: acquired from it before
    /// each conversion and released to it after.
    foreign: bool,
}

/// The parts of the image a source is made over, which a [`Source`] owns
/// once made.
pub(super) struct Image {
    pub(super) image: vk::Image,
    pub(super) memory: vk::DeviceMemory,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) foreign: bool,
}

/// The conversions, their pipelines and the sources made so far.
pub(super) struct Converter {
    vertex: vk::ShaderModule,
    fragment: vk::ShaderModule,
    kinds: Vec<Kind>,
    /// The sources kept, the latest used last.
    sources: Vec<Source>,
    /// Sources let go while a frame in flight may read them, with the epoch
    /// they were last recorded in.
    dead: Vec<(Epoch, Source)>,
}

/// Where a conversion draws: the target texture's image, the framebuffer
/// over its UNORM view, and its size.
pub(super) struct Target {
    pub(super) image: vk::Image,
    pub(super) framebuffer: vk::Framebuffer,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl Converter {
    pub(super) fn new(device: &ash::Device) -> Converter {
        let module = |bytes: &[u8]| {
            let words = ash::util::read_spv(&mut Cursor::new(bytes)).expect("malformed SPIR-V");
            let info = vk::ShaderModuleCreateInfo::default().code(&words);
            // SAFETY: `words` is a whole SPIR-V module, validated when frozen.
            unsafe { device.create_shader_module(&info, None) }
                .expect("failed to create a Vulkan shader module")
        };
        Converter {
            vertex: module(VERTEX),
            fragment: module(FRAGMENT),
            kinds: Vec::new(),
            sources: Vec::new(),
            dead: Vec::new(),
        }
    }

    /// The index of the source made of the buffer `holds` names, moved to
    /// the latest.
    pub(super) fn source_of(&mut self, holds: *const ExternalHolds) -> Option<usize> {
        let at = self.sources.iter().position(|s| {
            s.holds
                .as_ref()
                .is_some_and(|h| std::ptr::eq(Arc::as_ptr(h), holds))
        })?;
        let source = self.sources.remove(at);
        self.sources.push(source);
        Some(self.sources.len() - 1)
    }

    /// The size of source `at`.
    pub(super) fn size(&self, at: usize) -> (u32, u32) {
        self.sources[at].size
    }

    /// The holds of the buffer source `at` was made of.
    pub(super) fn holds(&self, at: usize) -> Option<&Arc<ExternalHolds>> {
        self.sources[at].holds.as_ref()
    }

    /// Makes a source of `image` through a conversion for `encoding`, named
    /// by `holds`, and keeps it as the latest; the oldest goes once too many
    /// are kept, when no frame after `epoch` reads it.
    ///
    /// # Safety
    ///
    /// `image` is a live, bound image of `device` in `encoding`'s format,
    /// with sampled usage, that the source then owns; `pass` is a render
    /// pass with one `R8G8B8A8_UNORM` color attachment.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn make_source(
        &mut self,
        device: &ash::Device,
        cache: vk::PipelineCache,
        pass: vk::RenderPass,
        image: Image,
        encoding: Encoding,
        holds: Option<Arc<ExternalHolds>>,
        epoch: Epoch,
    ) -> Option<usize> {
        // SAFETY: the caller's contract for `pass`.
        let Some(kind) = (unsafe { self.kind(device, cache, pass, encoding) }) else {
            // SAFETY: the image is the caller's to hand over, unused.
            unsafe {
                device.destroy_image(image.image, None);
                device.free_memory(image.memory, None);
            }
            return None;
        };
        let k = &self.kinds[kind];
        // An external format's view is made through its conversion alone.
        let mut through = vk::SamplerYcbcrConversionInfo::default().conversion(k.conversion);
        let info = vk::ImageViewCreateInfo::default()
            .image(image.image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(encoding.format)
            .subresource_range(super::subresource_range(vk::ImageAspectFlags::COLOR))
            .push_next(&mut through);
        // A combined sampler of a multi-planar image may take a descriptor a
        // plane.
        let sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 3,
        }];
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(1)
            .pool_sizes(&sizes);
        // SAFETY: the view is of the caller's image through the kind's
        // conversion, as the immutable sampler samples it; the set is
        // written once, before any use.
        let source = unsafe {
            let view = device
                .create_image_view(&info, None)
                .expect("failed to create a Y′CbCr image view");
            let pool = device
                .create_descriptor_pool(&pool_info, None)
                .expect("failed to create a descriptor pool");
            let layouts = [k.set_layout];
            let alloc = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pool)
                .set_layouts(&layouts);
            let set = device
                .allocate_descriptor_sets(&alloc)
                .expect("failed to allocate a descriptor set")[0];
            let images = [vk::DescriptorImageInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let write = vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&images);
            device.update_descriptor_sets(&[write], &[]);
            Source {
                holds,
                image: image.image,
                memory: image.memory,
                view,
                pool,
                set,
                kind,
                size: (image.width, image.height),
                foreign: image.foreign,
            }
        };
        if self.sources.len() == SOURCES {
            let old = self.sources.remove(0);
            self.dead.push((epoch, old));
        }
        self.sources.push(source);
        Some(self.sources.len() - 1)
    }

    /// The index of the conversion for `encoding`, made if it is new.
    ///
    /// # Safety
    ///
    /// As for [`make_source`](Self::make_source).
    unsafe fn kind(
        &mut self,
        device: &ash::Device,
        cache: vk::PipelineCache,
        pass: vk::RenderPass,
        encoding: Encoding,
    ) -> Option<usize> {
        if let Some(at) = self.kinds.iter().position(|k| k.encoding == encoding) {
            return Some(at);
        }
        let filter = if encoding.linear {
            vk::Filter::LINEAR
        } else {
            vk::Filter::NEAREST
        };
        let mut external = vk::ExternalFormatANDROID::default().external_format(encoding.external);
        let mut info = vk::SamplerYcbcrConversionCreateInfo::default()
            .format(encoding.format)
            .ycbcr_model(encoding.model)
            .ycbcr_range(encoding.range)
            .components(vk::ComponentMapping {
                r: encoding.components[0],
                g: encoding.components[1],
                b: encoding.components[2],
                a: encoding.components[3],
            })
            .x_chroma_offset(encoding.x_chroma)
            .y_chroma_offset(encoding.y_chroma)
            .chroma_filter(filter)
            .force_explicit_reconstruction(false);
        if encoding.external != 0 {
            info = info.push_next(&mut external);
        }
        // SAFETY: plain object creation on a live device, each from the one
        // before; the immutable sampler carries the conversion, as a
        // combined sampler of a converted image must.
        unsafe {
            let conversion = device.create_sampler_ycbcr_conversion(&info, None).ok()?;
            let mut through = vk::SamplerYcbcrConversionInfo::default().conversion(conversion);
            let sampler_info = vk::SamplerCreateInfo::default()
                .mag_filter(filter)
                .min_filter(filter)
                .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .push_next(&mut through);
            let Ok(sampler) = device.create_sampler(&sampler_info, None) else {
                device.destroy_sampler_ycbcr_conversion(conversion, None);
                return None;
            };
            let samplers = [sampler];
            let bindings = [vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                .immutable_samplers(&samplers)];
            let set_layout = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )
                .expect("failed to create a descriptor set layout");
            let set_layouts = [set_layout];
            let ranges = [vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::VERTEX)
                .size(16)];
            let layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(&set_layouts)
                        .push_constant_ranges(&ranges),
                    None,
                )
                .expect("failed to create a pipeline layout");
            let pipeline = self.pipeline(device, cache, pass, layout);
            self.kinds.push(Kind {
                encoding,
                conversion,
                sampler,
                set_layout,
                layout,
                pipeline,
            });
        }
        Some(self.kinds.len() - 1)
    }

    /// The pipeline drawing one triangle over the target, its picture
    /// sampled through the set `layout` names.
    ///
    /// # Safety
    ///
    /// `pass` is a live render pass with one color attachment and `layout`
    /// a live pipeline layout of `device`.
    unsafe fn pipeline(
        &self,
        device: &ash::Device,
        cache: vk::PipelineCache,
        pass: vk::RenderPass,
        layout: vk::PipelineLayout,
    ) -> vk::Pipeline {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.vertex)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(self.fragment)
                .name(c"main"),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
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
        let attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let info = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(layout)
            .render_pass(pass)
            .subpass(0)];
        // SAFETY: the caller's contract; the stages are this converter's live
        // modules, whose interfaces match.
        unsafe { device.create_graphics_pipelines(cache, &info, None) }
            .map_err(|(_, e)| e)
            .expect("failed to create the Y′CbCr pipeline")[0]
    }

    /// Records the conversion of source `at`'s `region` — its offset and
    /// scale in texture coordinates — into `target`, which rests in the
    /// shader-read layout and returns to it.
    ///
    /// # Safety
    ///
    /// `cmd` is recording outside a render pass on the queue family
    /// `family`; `target` is a live texture of `device` whose framebuffer
    /// was made against `pass`.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn record(
        &self,
        device: &ash::Device,
        cmd: vk::CommandBuffer,
        family: u32,
        pass: vk::RenderPass,
        at: usize,
        region: [f32; 4],
        target: &Target,
    ) {
        let source = &self.sources[at];
        let kind = &self.kinds[source.kind];
        let range = super::subresource_range(vk::ImageAspectFlags::COLOR);
        let foreign = |acquire: bool| {
            let barrier = vk::ImageMemoryBarrier::default()
                .image(source.image)
                .subresource_range(range);
            if acquire {
                // What the decoder wrote is kept: an external buffer's
                // contents survive a transfer from the foreign queue.
                barrier
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                    .dst_queue_family_index(family)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)
            } else {
                barrier
                    .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(family)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                    .src_access_mask(vk::AccessFlags::SHADER_READ)
            }
        };
        let clear = [vk::ClearValue::default()];
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D {
                width: target.width,
                height: target.height,
            },
        };
        let begin = vk::RenderPassBeginInfo::default()
            .render_pass(pass)
            .framebuffer(target.framebuffer)
            .render_area(area)
            .clear_values(&clear);
        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: target.width as f32,
            height: target.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let mut words = [0u8; 16];
        for (i, v) in region.iter().enumerate() {
            words[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
        }
        // SAFETY: the caller's contract; the source and the kind's objects
        // are this converter's, live, and the source is in the shader-read
        // layout for the draw.
        unsafe {
            if source.foreign {
                device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[foreign(true)],
                );
            }
            super::transition(
                device,
                cmd,
                target.image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            );
            device.cmd_begin_render_pass(cmd, &begin, vk::SubpassContents::INLINE);
            device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, kind.pipeline);
            device.cmd_set_viewport(cmd, 0, &[viewport]);
            device.cmd_set_scissor(cmd, 0, &[area]);
            device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                kind.layout,
                0,
                &[source.set],
                &[],
            );
            device.cmd_push_constants(cmd, kind.layout, vk::ShaderStageFlags::VERTEX, 0, &words);
            device.cmd_draw(cmd, 3, 1, 0, 0);
            device.cmd_end_render_pass(cmd);
            super::transition(
                device,
                cmd,
                target.image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );
            if source.foreign {
                device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[foreign(false)],
                );
            }
        }
    }

    /// Destroys the sources let go whose epoch `completed` has passed.
    pub(super) fn reclaim(&mut self, device: &ash::Device, completed: Epoch) {
        self.dead.retain_mut(|(epoch, source)| {
            let done = *epoch <= completed;
            if done {
                // SAFETY: no frame up to the one that last read it is in
                // flight.
                unsafe { destroy_source(device, source) };
            }
            !done
        });
    }

    /// Destroys everything made.
    ///
    /// # Safety
    ///
    /// The device is idle.
    pub(super) unsafe fn destroy(&mut self, device: &ash::Device) {
        // SAFETY: the caller's contract; each object is destroyed once,
        // children first.
        unsafe {
            for (_, source) in self.dead.iter_mut() {
                destroy_source(device, source);
            }
            for source in &mut self.sources {
                destroy_source(device, source);
            }
            for k in self.kinds.drain(..) {
                device.destroy_pipeline(k.pipeline, None);
                device.destroy_pipeline_layout(k.layout, None);
                device.destroy_descriptor_set_layout(k.set_layout, None);
                device.destroy_sampler(k.sampler, None);
                device.destroy_sampler_ycbcr_conversion(k.conversion, None);
            }
            device.destroy_shader_module(self.vertex, None);
            device.destroy_shader_module(self.fragment, None);
        }
        self.dead.clear();
        self.sources.clear();
    }
}

/// Destroys a source's objects.
///
/// # Safety
///
/// No command in flight reads it; it is destroyed once.
unsafe fn destroy_source(device: &ash::Device, source: &mut Source) {
    // SAFETY: the caller's contract; the view goes before its image.
    unsafe {
        device.destroy_descriptor_pool(source.pool, None);
        device.destroy_image_view(source.view, None);
        device.destroy_image(source.image, None);
        device.free_memory(source.memory, None);
    }
}

#[cfg(test)]
mod tests {
    use super::super::VulkanBackend;
    use crate::GpuBackend;

    /// The frozen modules, compiled from `ycbcr.vert` and `ycbcr.frag` with
    /// `glslangValidator -V --target-env vulkan1.1`, pass the Khronos
    /// validator.
    #[test]
    fn the_conversion_shaders_are_valid_spir_v() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/vulkan");
        for name in ["ycbcr.vert.spv", "ycbcr.frag.spv"] {
            let Ok(out) = std::process::Command::new("spirv-val")
                .args(["--target-env", "vulkan1.1"])
                .arg(dir.join(name))
                .output()
            else {
                eprintln!("spirv-val not installed; SPIR-V structure unchecked");
                return;
            };
            assert!(
                out.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// A picture in NV12 planes — a red half and a white one, studio range
    /// in BT.601 — converts into opaque sRGB-encoded RGBA, sampled as any
    /// other texture.
    #[test]
    fn y_cb_cr_planes_convert_into_an_srgb_texture() {
        let Some(mut gpu) = VulkanBackend::try_new_with(true) else {
            eprintln!("no Vulkan device; skipped");
            return;
        };
        // Four by two: chroma one sample a two-by-two block.
        let y = [81, 81, 235, 235, 81, 81, 235, 235];
        let uv = [90, 240, 128, 128];
        let Some(at) = gpu.nv12_source(4, 2, &y, &uv) else {
            eprintln!("no Y′CbCr conversion of NV12 here; skipped");
            return;
        };
        let id = gpu.converted_texture(4, 2);
        gpu.convert(id, at, [0.0, 0.0, 1.0, 1.0]);
        let px = gpu.read_texture(id);
        for texel in px.chunks(4).enumerate() {
            let (i, t) = texel;
            if i % 4 < 2 {
                assert!(
                    t[0] >= 250 && t[1] <= 4 && t[2] <= 4 && t[3] == 255,
                    "{i}: {t:?}"
                );
            } else {
                assert!(t.iter().all(|&c| c >= 250), "{i}: {t:?}");
            }
        }
        assert_eq!(
            gpu.validation_errors(),
            0,
            "the validation layer reported errors"
        );
        gpu.destroy_texture(id);
    }
}
