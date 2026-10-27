//! The web's backend: WebGPU where the browser opens a device, else the CPU
//! raster, its frames shown through the canvas's 2D context.
//!
//! The choice is made once a page asked for a device
//! ([`webgpu::prepare`](crate::webgpu::prepare)): a backend made after
//! that draws on the device when there is one, and on the CPU otherwise. The
//! two draw the same pixels; a frame on the CPU reaches the canvas through
//! the window's [`Scanout`](viso_handle::Scanout), the damage alone copied.

use viso_handle::{ExternalImage, Nv12, RawWindowHandle};

use crate::backend::{DrawList, Frame, GpuBackend};
use crate::cpu::{CpuCompiler, CpuRaster, CpuRasterProgram};
use crate::instance::{InstanceLayout, LayoutError};
use crate::program::{ProgramCompiler, ProgramDesc};
use crate::resource::{
    BindGroupDesc, BufferDesc, Caps, PipelineDesc, SamplerDesc, ShaderLang, TextureDesc,
    TextureFormat,
};
use crate::webgpu::{self, WebGpuBackend, WebGpuCompiler, WebGpuProgram};
use crate::{BindGroupId, BufferId, ColorSpace, PipelineId, SamplerId, SurfaceId, TextureId};

/// The backend a page draws with: the browser's WebGPU device, or the CPU
/// raster where the browser has none.
// One per window, held by value: boxing the device would cost every call a
// pointer chase to save bytes no one counts.
#[allow(clippy::large_enum_variant)]
pub enum WebBackend {
    /// The WebGPU device.
    Gpu(WebGpuBackend),
    /// The CPU raster, presenting to the canvas's 2D context.
    Cpu(CpuRaster),
}

/// `$body` on whichever backend `$self` is, bound to `$b`.
macro_rules! each {
    ($self:expr, $b:ident => $body:expr) => {
        match $self {
            WebBackend::Gpu($b) => $body,
            WebBackend::Cpu($b) => $body,
        }
    };
}

impl WebBackend {
    /// The device [`prepare`](webgpu::prepare) opened, or the CPU raster
    /// when it opened none.
    pub fn new() -> Self {
        if webgpu::is_prepared() {
            WebBackend::Gpu(WebGpuBackend::new())
        } else {
            WebBackend::Cpu(CpuRaster::new())
        }
    }

    /// A device to replace a lost one, or `None` while the browser has not
    /// opened its successor yet. Only a device is lost; the CPU raster never
    /// is.
    pub fn try_new() -> Option<Self> {
        WebGpuBackend::try_new().map(WebBackend::Gpu)
    }

    /// Whether it draws on the CPU.
    pub fn is_cpu(&self) -> bool {
        matches!(self, WebBackend::Cpu(_))
    }
}

impl Default for WebBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// The program compiler of a [`WebBackend`]: the device's, or the CPU
/// raster's.
#[derive(Debug, Clone, Copy)]
pub enum WebCompiler {
    /// WebGPU's, taking WGSL.
    Gpu(WebGpuCompiler),
    /// The CPU raster's, taking CPU programs.
    Cpu(CpuCompiler),
}

/// A program a [`WebCompiler`] built, for the backend it came from.
pub enum WebProgram {
    /// For the WebGPU device.
    Gpu(WebGpuProgram),
    /// For the CPU raster.
    Cpu(CpuRasterProgram),
}

impl ProgramCompiler for WebCompiler {
    type Program = WebProgram;

    fn compile(&self, desc: &ProgramDesc<'_>) -> Result<WebProgram, String> {
        match self {
            WebCompiler::Gpu(c) => c.compile(desc).map(WebProgram::Gpu),
            WebCompiler::Cpu(c) => c.compile(desc).map(WebProgram::Cpu),
        }
    }
}

impl GpuBackend for WebBackend {
    /// The device's; [`shader_lang`](GpuBackend::shader_lang) tells which
    /// this backend reads.
    const SHADER_LANG: ShaderLang = ShaderLang::Wgsl;

    const TIMES_FRAMES: bool = WebGpuBackend::TIMES_FRAMES || CpuRaster::TIMES_FRAMES;

    fn shader_lang(&self) -> ShaderLang {
        each!(self, b => b.shader_lang())
    }

    fn set_cpu_workers(&mut self, threads: usize) {
        each!(self, b => b.set_cpu_workers(threads))
    }

    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        each!(self, b => b.create_buffer(desc))
    }

    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId {
        each!(self, b => b.create_texture(desc))
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        each!(self, b => b.create_sampler(desc))
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, LayoutError> {
        each!(self, b => b.create_pipeline(desc, layout))
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        each!(self, b => b.create_bind_group(desc))
    }

    type Compiler = WebCompiler;

    fn program_compiler(&self) -> WebCompiler {
        match self {
            WebBackend::Gpu(b) => WebCompiler::Gpu(b.program_compiler()),
            WebBackend::Cpu(b) => WebCompiler::Cpu(b.program_compiler()),
        }
    }

    fn install_program(&mut self, program: WebProgram) -> PipelineId {
        match (self, program) {
            (WebBackend::Gpu(b), WebProgram::Gpu(p)) => b.install_program(p),
            (WebBackend::Cpu(b), WebProgram::Cpu(p)) => b.install_program(p),
            _ => panic!("a program compiled by another backend's compiler"),
        }
    }

    fn destroy_buffer(&mut self, id: BufferId) {
        each!(self, b => b.destroy_buffer(id))
    }

    fn destroy_texture(&mut self, id: TextureId) {
        each!(self, b => b.destroy_texture(id))
    }

    fn destroy_sampler(&mut self, id: SamplerId) {
        each!(self, b => b.destroy_sampler(id))
    }

    fn destroy_pipeline(&mut self, id: PipelineId) {
        each!(self, b => b.destroy_pipeline(id))
    }

    fn destroy_bind_group(&mut self, id: BindGroupId) {
        each!(self, b => b.destroy_bind_group(id))
    }

    fn write_buffer(&mut self, id: BufferId, offset: usize, bytes: &[u8]) {
        each!(self, b => b.write_buffer(id, offset, bytes))
    }

    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]) {
        each!(self, b => b.write_texture(id, x, y, w, h, bytes))
    }

    fn create_texture_static(&mut self, desc: &TextureDesc, texels: &'static [u8]) -> TextureId {
        each!(self, b => b.create_texture_static(desc, texels))
    }

    fn create_texture_mipped(&mut self, desc: &TextureDesc, levels: &[&[u8]]) -> TextureId {
        each!(self, b => b.create_texture_mipped(desc, levels))
    }

    fn import_external(&mut self, image: &ExternalImage) -> Option<TextureId> {
        each!(self, b => b.import_external(image))
    }

    fn adopt_external(&mut self, id: TextureId, image: &ExternalImage) -> bool {
        each!(self, b => b.adopt_external(id, image))
    }

    fn write_nv12(&mut self, id: Option<TextureId>, picture: &Nv12<'_>) -> Option<TextureId> {
        each!(self, b => b.write_nv12(id, picture))
    }

    fn create_surface(&mut self, raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId {
        each!(self, b => b.create_surface(raw, width, height))
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        each!(self, b => b.resize_surface(id, width, height))
    }

    fn destroy_surface(&mut self, id: SurfaceId) {
        each!(self, b => b.destroy_surface(id))
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame> {
        each!(self, b => b.begin_frame(surface))
    }

    fn encode(&mut self, list: &DrawList<'_>) {
        each!(self, b => b.encode(list))
    }

    fn present(&mut self, frame: Frame) {
        each!(self, b => b.present(frame))
    }

    fn frame_age(&self, frame: Frame) -> u32 {
        each!(self, b => b.frame_age(frame))
    }

    fn present_with_damage(&mut self, frame: Frame, damage: &[(u32, u32, u32, u32)]) {
        each!(self, b => b.present_with_damage(frame, damage))
    }

    fn abandon_frame(&mut self, surface: SurfaceId) {
        each!(self, b => b.abandon_frame(surface))
    }

    fn is_lost(&self) -> bool {
        each!(self, b => b.is_lost())
    }

    fn caps(&self) -> &Caps {
        each!(self, b => b.caps())
    }

    fn surface_format(&self, surface: SurfaceId) -> TextureFormat {
        each!(self, b => b.surface_format(surface))
    }

    fn surface_color_space(&self, surface: SurfaceId) -> ColorSpace {
        each!(self, b => b.surface_color_space(surface))
    }

    fn time_frames(&mut self, on: bool) {
        each!(self, b => b.time_frames(on))
    }

    fn frame_epoch(&self) -> u64 {
        each!(self, b => b.frame_epoch())
    }

    fn finished_frame_times(&mut self, out: &mut Vec<(u64, u64)>) {
        each!(self, b => b.finished_frame_times(out))
    }
}
