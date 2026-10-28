//! Android's backend when the build lets it choose (`VISO_GPU=auto`, the
//! default): Vulkan where the device opens one, else OpenGL ES 3.
//!
//! The choice is made once a process: by the first backend made, or by the
//! first question of the [`shader_lang`](crate::shader_lang) its programs
//! must be in, whichever comes first. Vulkan is tried as the Vulkan backend
//! opens: a loader, an instance, a device with a graphics queue and what the
//! backend needs of it; anything missing and the device draws with GLES,
//! saying why once. `VISO_GPU` set to `vulkan` or `gles` when the app is
//! launched (an intent extra, which the facade hands to [`ask`], or the
//! process environment) makes the choice instead. A backend made again after
//! a device was lost is made the same.

use std::sync::OnceLock;

use viso_handle::{ExternalImage, Nv12, RawWindowHandle};

use crate::backend::{DrawList, Frame, GpuBackend};
use crate::gles::{Gles3Backend, Gles3Compiler, Gles3Program};
use crate::instance::{InstanceLayout, LayoutError};
use crate::program::{ProgramCompiler, ProgramDesc};
use crate::resource::{
    BindGroupDesc, BufferDesc, Caps, PipelineDesc, SamplerDesc, ShaderLang, TextureDesc,
    TextureFormat,
};
use crate::vulkan::{VulkanBackend, VulkanCompiler, VulkanProgram};
use crate::{BindGroupId, BufferId, ColorSpace, PipelineId, SamplerId, SurfaceId, TextureId};

/// The API this process draws with, once chosen.
static CHOSEN: OnceLock<Api> = OnceLock::new();

/// The API the launcher asked for.
static ASKED: OnceLock<Api> = OnceLock::new();

/// Asks for the API `name` names, before the choice is made.
pub fn ask(name: &str) {
    if let Some(api) = Api::named(name) {
        let _ = ASKED.set(api);
    }
}

/// The two APIs an Android device may draw with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    Vulkan,
    Gles,
}

impl Api {
    /// The language its programs are in.
    pub const fn shader_lang(self) -> ShaderLang {
        match self {
            Api::Vulkan => ShaderLang::SpirV,
            Api::Gles => ShaderLang::Glsl,
        }
    }

    fn named(name: &str) -> Option<Api> {
        match name.trim() {
            "vulkan" => Some(Api::Vulkan),
            "gles" => Some(Api::Gles),
            _ => None,
        }
    }

    /// The API the launcher asked for, if it named one.
    fn asked() -> Option<Api> {
        ASKED
            .get()
            .copied()
            .or_else(|| Api::named(&std::env::var(crate::API_VAR).ok()?))
    }
}

/// The API this process draws with, choosing it now if no backend did:
/// opening a Vulkan device to see that one opens.
pub fn chosen() -> Api {
    *CHOSEN.get_or_init(|| match Api::asked() {
        Some(api) => api,
        None => match VulkanBackend::try_open() {
            Ok(_) => Api::Vulkan,
            Err(why) => fell_back(&why),
        },
    })
}

/// Reports that Vulkan could not be used and why: GLES it is.
fn fell_back(why: &str) -> Api {
    eprintln!("viso: drawing with OpenGL ES: no usable Vulkan device ({why})");
    Api::Gles
}

/// The backend an Android device draws with: Vulkan, or OpenGL ES 3 where
/// Vulkan is not to be had.
// One per process, held by value: boxing the device would cost every call a
// pointer chase to save bytes no one counts.
#[allow(clippy::large_enum_variant)]
pub enum AndroidBackend {
    Vulkan(VulkanBackend),
    Gles(Gles3Backend),
}

/// `$body` on whichever backend `$self` is, bound to `$b`.
macro_rules! each {
    ($self:expr, $b:ident => $body:expr) => {
        match $self {
            AndroidBackend::Vulkan($b) => $body,
            AndroidBackend::Gles($b) => $body,
        }
    };
}

impl AndroidBackend {
    /// The backend of the API chosen, choosing it if none was.
    ///
    /// # Panics
    ///
    /// When the API chosen cannot be opened: Vulkan that `VISO_GPU` asked
    /// for, or a GLES driver that does not load.
    pub fn new() -> Self {
        Self::open().unwrap_or_else(|why| panic!("no usable GPU: {why}"))
    }

    /// A backend to replace a lost one, of the API drawn with before; `None`
    /// while it cannot be opened.
    pub fn try_new() -> Option<Self> {
        Self::open().ok()
    }

    fn open() -> Result<Self, String> {
        if let Some(api) = CHOSEN.get() {
            return Self::of(*api);
        }
        let backend = match Api::asked() {
            Some(api) => Self::of(api)?,
            None => match VulkanBackend::try_open() {
                Ok(vulkan) => AndroidBackend::Vulkan(vulkan),
                Err(why) => {
                    fell_back(&why);
                    AndroidBackend::Gles(Gles3Backend::load()?)
                }
            },
        };
        // A question about the language answered meanwhile chose the same:
        // both open the same device.
        let _ = CHOSEN.set(backend.api());
        Ok(backend)
    }

    fn of(api: Api) -> Result<Self, String> {
        Ok(match api {
            Api::Vulkan => AndroidBackend::Vulkan(VulkanBackend::try_open()?),
            Api::Gles => AndroidBackend::Gles(Gles3Backend::load()?),
        })
    }

    /// The API it draws with.
    pub fn api(&self) -> Api {
        match self {
            AndroidBackend::Vulkan(_) => Api::Vulkan,
            AndroidBackend::Gles(_) => Api::Gles,
        }
    }

    /// The texels of render target `id`, top row first: a verification
    /// path, never a frame path.
    pub fn read_texture(&mut self, id: TextureId) -> Vec<u8> {
        each!(self, b => b.read_texture(id))
    }
}

impl Default for AndroidBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// The program compiler of an [`AndroidBackend`].
// Cloned once for each compiling thread and a program moved once to be
// installed: boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum AndroidCompiler {
    Vulkan(VulkanCompiler),
    Gles(Gles3Compiler),
}

/// A program an [`AndroidCompiler`] built, for the backend it came from.
#[allow(clippy::large_enum_variant)]
pub enum AndroidProgram {
    Vulkan(VulkanProgram),
    Gles(Gles3Program),
}

impl ProgramCompiler for AndroidCompiler {
    type Program = AndroidProgram;

    fn compile(&self, desc: &ProgramDesc<'_>) -> Result<AndroidProgram, String> {
        match self {
            AndroidCompiler::Vulkan(c) => c.compile(desc).map(AndroidProgram::Vulkan),
            AndroidCompiler::Gles(c) => c.compile(desc).map(AndroidProgram::Gles),
        }
    }
}

impl GpuBackend for AndroidBackend {
    /// The preferred API's; [`shader_lang`](GpuBackend::shader_lang) tells
    /// which this backend reads.
    const SHADER_LANG: ShaderLang = ShaderLang::SpirV;

    const TIMES_FRAMES: bool = VulkanBackend::TIMES_FRAMES || Gles3Backend::TIMES_FRAMES;

    fn shader_lang(&self) -> ShaderLang {
        self.api().shader_lang()
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

    type Compiler = AndroidCompiler;

    fn program_compiler(&self) -> AndroidCompiler {
        match self {
            AndroidBackend::Vulkan(b) => AndroidCompiler::Vulkan(b.program_compiler()),
            AndroidBackend::Gles(b) => AndroidCompiler::Gles(b.program_compiler()),
        }
    }

    fn install_program(&mut self, program: AndroidProgram) -> PipelineId {
        match (self, program) {
            (AndroidBackend::Vulkan(b), AndroidProgram::Vulkan(p)) => b.install_program(p),
            (AndroidBackend::Gles(b), AndroidProgram::Gles(p)) => b.install_program(p),
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
