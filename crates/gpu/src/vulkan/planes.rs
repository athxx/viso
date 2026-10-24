//! Pictures in NV12 planes at hand turned on the GPU into a texture of the
//! backend's own: the planes are uploaded as they are into a luma and a
//! chroma image the texture keeps, and a plain shader applies the picture's
//! matrix and range, drawing gamma-encoded RGB into the texture's UNORM view
//! so it samples as sRGB, as any other color image.
//!
//! The pipeline is made once; the plane images once per texture, rewritten
//! by every picture after.

use ash::vk;

use super::ycbcr::{self, Target};
use crate::Nv12;

const FRAGMENT: &[u8] = include_bytes!("nv12.frag.spv");

/// The bytes of the push constants: the region, the matrix's three rows and
/// the chroma site.
const PUSHED: usize = 80;

/// The conversion's pipeline and what it is made through.
pub(super) struct Planar {
    vertex: vk::ShaderModule,
    fragment: vk::ShaderModule,
    sampler: vk::Sampler,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

/// One image a picture's plane is uploaded into.
struct Plane {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    width: u32,
    height: u32,
}

/// A texture's luma and chroma images and the set sampling them.
pub(super) struct Planes {
    luma: Plane,
    chroma: Plane,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
    /// Not yet written: its images' contents are undefined.
    fresh: bool,
}

/// Where a picture's plane lies staged: the buffer, the offset in it and
/// the length of its rows in texels.
#[derive(Clone, Copy)]
pub(super) struct Staged {
    pub(super) buffer: vk::Buffer,
    pub(super) offset: u64,
    pub(super) row_length: u32,
}

impl Planar {
    /// The pipeline drawing through `pass`.
    ///
    /// # Safety
    ///
    /// `pass` is a live render pass of `device` with one `R8G8B8A8_UNORM`
    /// color attachment.
    pub(super) unsafe fn new(
        device: &ash::Device,
        cache: vk::PipelineCache,
        pass: vk::RenderPass,
    ) -> Planar {
        let vertex = ycbcr::module(device, ycbcr::VERTEX);
        let fragment = ycbcr::module(device, FRAGMENT);
        // Linear: chroma, at half resolution, is reconstructed between its
        // samples; luma is sampled at its texel centers, so exactly.
        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
        // SAFETY: plain object creation on a live device, each from the one
        // before; the caller's contract for `pass`.
        unsafe {
            let sampler = device
                .create_sampler(&sampler_info, None)
                .expect("failed to create a Vulkan sampler");
            let samplers = [sampler];
            let binding = |at: u32| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(at)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                    .immutable_samplers(&samplers)
            };
            let bindings = [binding(0), binding(1)];
            let set_layout = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )
                .expect("failed to create a descriptor set layout");
            let set_layouts = [set_layout];
            let ranges = [vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
                .size(PUSHED as u32)];
            let layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(&set_layouts)
                        .push_constant_ranges(&ranges),
                    None,
                )
                .expect("failed to create a pipeline layout");
            let pipeline = ycbcr::pipeline(device, cache, pass, layout, [vertex, fragment]);
            Planar {
                vertex,
                fragment,
                sampler,
                set_layout,
                layout,
                pipeline,
            }
        }
    }

    /// The plane images of a `width` × `height` picture, their memory of
    /// the type `memory_type` picks of the bits allowed.
    pub(super) fn planes(
        &self,
        device: &ash::Device,
        memory_type: impl Fn(u32) -> u32,
        width: u32,
        height: u32,
    ) -> Planes {
        let luma = plane(device, &memory_type, vk::Format::R8_UNORM, width, height);
        let chroma = plane(
            device,
            &memory_type,
            vk::Format::R8G8_UNORM,
            width.div_ceil(2),
            height.div_ceil(2),
        );
        let sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 2,
        }];
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(1)
            .pool_sizes(&sizes);
        // SAFETY: the set is of this converter's layout, written once with
        // the live plane views before any use.
        unsafe {
            let pool = device
                .create_descriptor_pool(&pool_info, None)
                .expect("failed to create a descriptor pool");
            let layouts = [self.set_layout];
            let alloc = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pool)
                .set_layouts(&layouts);
            let set = device
                .allocate_descriptor_sets(&alloc)
                .expect("failed to allocate a descriptor set")[0];
            let info = |view: vk::ImageView| {
                [vk::DescriptorImageInfo::default()
                    .image_view(view)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]
            };
            let (l, c) = (info(luma.view), info(chroma.view));
            let write = |at: u32, images| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(at)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(images)
            };
            device.update_descriptor_sets(&[write(0, &l), write(1, &c)], &[]);
            Planes {
                luma,
                chroma,
                pool,
                set,
                fresh: true,
            }
        }
    }

    /// Records the upload of `picture`'s planes, staged at `luma` and
    /// `chroma`, into `planes` and their conversion into `target`, which
    /// rests in the shader-read layout and returns to it.
    ///
    /// # Safety
    ///
    /// `cmd` is recording outside a render pass; `planes` were made by this
    /// converter for a picture of `picture`'s size, as `target` is, and no
    /// command recorded before reads or writes them unordered; the staged
    /// rows cover the planes; `target`'s framebuffer was made against `pass`.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn record(
        &self,
        device: &ash::Device,
        cmd: vk::CommandBuffer,
        pass: vk::RenderPass,
        picture: &Nv12<'_>,
        planes: &mut Planes,
        [luma, chroma]: [Staged; 2],
        target: &Target,
    ) {
        let from = if std::mem::replace(&mut planes.fresh, false) {
            vk::ImageLayout::UNDEFINED
        } else {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        };
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D {
                width: target.width,
                height: target.height,
            },
        };
        let clear = [vk::ClearValue::default()];
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
        let words = push_constants(picture, planes.chroma.width, planes.chroma.height);
        // SAFETY: the caller's contract; each plane is moved into the
        // transfer layout for its copy and back into the shader-read one for
        // the draw, which the barriers order after the copies.
        unsafe {
            for (plane, staged) in [(&planes.luma, luma), (&planes.chroma, chroma)] {
                super::transition(
                    device,
                    cmd,
                    plane.image,
                    vk::ImageAspectFlags::COLOR,
                    from,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                );
                let region = vk::BufferImageCopy::default()
                    .buffer_offset(staged.offset)
                    .buffer_row_length(staged.row_length)
                    .image_subresource(super::subresource_layers(vk::ImageAspectFlags::COLOR))
                    .image_extent(vk::Extent3D {
                        width: plane.width,
                        height: plane.height,
                        depth: 1,
                    });
                device.cmd_copy_buffer_to_image(
                    cmd,
                    staged.buffer,
                    plane.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
                super::transition(
                    device,
                    cmd,
                    plane.image,
                    vk::ImageAspectFlags::COLOR,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
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
            device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            device.cmd_set_viewport(cmd, 0, &[viewport]);
            device.cmd_set_scissor(cmd, 0, &[area]);
            device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.layout,
                0,
                &[planes.set],
                &[],
            );
            device.cmd_push_constants(
                cmd,
                self.layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                &words,
            );
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
        }
    }

    /// Destroys the pipeline and what it is made through.
    ///
    /// # Safety
    ///
    /// No command in flight uses it; it is destroyed once.
    pub(super) unsafe fn destroy(&mut self, device: &ash::Device) {
        // SAFETY: the caller's contract; children go first.
        unsafe {
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
            device.destroy_sampler(self.sampler, None);
            device.destroy_shader_module(self.vertex, None);
            device.destroy_shader_module(self.fragment, None);
        }
    }
}

/// The push constants converting `picture` from a chroma plane of
/// `cw` × `ch` samples.
fn push_constants(picture: &Nv12<'_>, cw: u32, ch: u32) -> [u8; PUSHED] {
    let rows = picture.matrix.rows(picture.full_range);
    // A plane rounded up to whole samples spans a little more than the
    // picture; a left-sited sample lies on its left luma sample, a quarter
    // of a sample before where its texel's center falls.
    let (sx, sy) = (
        picture.width as f32 / (2 * cw) as f32,
        picture.height as f32 / (2 * ch) as f32,
    );
    let ox = if picture.left_chroma {
        0.25 / cw as f32
    } else {
        0.0
    };
    let words = [
        [0.0, 0.0, 1.0, 1.0],
        rows[0],
        rows[1],
        rows[2],
        [sx, sy, ox, 0.0],
    ];
    let mut out = [0u8; PUSHED];
    for (i, v) in words.as_flattened().iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
    }
    out
}

/// A sampled, transfer-written `width` × `height` image of `format`.
fn plane(
    device: &ash::Device,
    memory_type: &impl Fn(u32) -> u32,
    format: vk::Format,
    width: u32,
    height: u32,
) -> Plane {
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
    // SAFETY: the image's memory comes from a type its requirements allow
    // and is bound once, before its view is made.
    unsafe {
        let image = device
            .create_image(&info, None)
            .expect("failed to create a Vulkan image");
        let req = device.get_image_memory_requirements(image);
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(req.size)
            .memory_type_index(memory_type(req.memory_type_bits));
        let memory = device
            .allocate_memory(&alloc, None)
            .expect("failed to allocate Vulkan image memory");
        device
            .bind_image_memory(image, memory, 0)
            .expect("failed to bind Vulkan image memory");
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(super::subresource_range(vk::ImageAspectFlags::COLOR));
        let view = device
            .create_image_view(&view_info, None)
            .expect("failed to create a Vulkan image view");
        Plane {
            image,
            memory,
            view,
            width,
            height,
        }
    }
}

/// Destroys a texture's plane images and their set.
///
/// # Safety
///
/// No command in flight uses them; they are destroyed once.
pub(super) unsafe fn destroy_planes(device: &ash::Device, planes: Planes) {
    // SAFETY: the caller's contract; views go before their images.
    unsafe {
        device.destroy_descriptor_pool(planes.pool, None);
        for plane in [planes.luma, planes.chroma] {
            device.destroy_image_view(plane.view, None);
            device.destroy_image(plane.image, None);
            device.free_memory(plane.memory, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::VulkanBackend;
    use crate::{GpuBackend, Nv12, YuvMatrix};

    /// The frozen module, compiled from `nv12.frag` with
    /// `glslangValidator -V --target-env vulkan1.0`, passes the Khronos
    /// validator.
    #[test]
    fn the_planar_shader_is_valid_spir_v() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/vulkan/nv12.frag.spv");
        let Ok(out) = std::process::Command::new("spirv-val")
            .args(["--target-env", "vulkan1.0"])
            .arg(path)
            .output()
        else {
            eprintln!("spirv-val not installed; SPIR-V structure unchecked");
            return;
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// NV12 planes in padded rows — a red block and a white one, studio
    /// range in BT.601 — convert into opaque sRGB-encoded RGBA, and a second
    /// picture rewrites the same texture.
    #[test]
    fn nv12_planes_at_hand_convert_into_an_srgb_texture() {
        let Some(mut gpu) = VulkanBackend::try_new_with(true) else {
            eprintln!("no Vulkan device; skipped");
            return;
        };
        // Four by two, rows padded to eight bytes; chroma one sample a
        // two-by-two block.
        let y = [
            81, 81, 235, 235, 0, 0, 0, 0, //
            81, 81, 235, 235, 0, 0, 0, 0,
        ];
        let uv = [90, 240, 128, 128, 0, 0, 0, 0];
        let picture = Nv12 {
            width: 4,
            height: 2,
            y: &y,
            y_stride: 8,
            uv: &uv,
            uv_stride: 8,
            matrix: YuvMatrix::Bt601,
            full_range: false,
            left_chroma: false,
        };
        let id = gpu
            .write_nv12(None, &picture)
            .expect("Vulkan converts NV12");
        let px = gpu.read_texture(id);
        // Linear chroma blends the blocks' edges: only the outer columns are
        // the blocks' own.
        let at = |px: &[u8], x: usize, y: usize| -> [u8; 4] {
            let i = (y * 4 + x) * 4;
            px[i..i + 4].try_into().unwrap()
        };
        for row in 0..2 {
            let red = at(&px, 0, row);
            assert!(
                red[0] >= 250 && red[1] <= 4 && red[2] <= 4 && red[3] == 255,
                "{red:?}"
            );
            let white = at(&px, 3, row);
            assert!(white.iter().all(|&c| c >= 250), "{white:?}");
        }
        // Black, full range: every texel zero, opaque.
        let black = [0u8; 16];
        let grey = [128u8; 8];
        let next = Nv12 {
            y: &black,
            uv: &grey,
            full_range: true,
            ..picture
        };
        assert_eq!(gpu.write_nv12(Some(id), &next), Some(id));
        let px = gpu.read_texture(id);
        for t in px.chunks(4) {
            assert_eq!(t, [0, 0, 0, 255]);
        }
        assert_eq!(
            gpu.validation_errors(),
            0,
            "the validation layer reported errors"
        );
        gpu.destroy_texture(id);
    }

    /// An odd-sized picture's chroma plane is rounded up to whole samples,
    /// and rows too short for the size are refused.
    #[test]
    fn odd_sized_planes_convert_and_short_ones_are_refused() {
        let Some(mut gpu) = VulkanBackend::try_new_with(true) else {
            eprintln!("no Vulkan device; skipped");
            return;
        };
        let y = [235u8; 9];
        let uv = [128u8; 8];
        let picture = Nv12 {
            width: 3,
            height: 3,
            y: &y,
            y_stride: 3,
            uv: &uv,
            uv_stride: 4,
            matrix: YuvMatrix::Bt709,
            full_range: false,
            left_chroma: true,
        };
        let id = gpu
            .write_nv12(None, &picture)
            .expect("Vulkan converts NV12");
        let px = gpu.read_texture(id);
        assert_eq!(px.len(), 36);
        assert!(px.iter().all(|&c| c >= 250), "{px:?}");
        let short = Nv12 {
            y: &y[..8],
            ..picture
        };
        assert_eq!(gpu.write_nv12(Some(id), &short), None);
        assert_eq!(
            gpu.validation_errors(),
            0,
            "the validation layer reported errors"
        );
        gpu.destroy_texture(id);
    }
}
