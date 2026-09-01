//! GPU resource descriptors and format enums (the cold-path "create" vocabulary).
//!
//! Descriptors are plain data passed to [`crate::GpuBackend`] create-methods,
//! which return the typed handles from `lib.rs`. Kept backend-neutral: no Metal
//! / D3D / Vulkan types leak here (§17.1).

/// Texture / render-target pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    /// 8-bit BGRA, unsigned normalized — the canonical macOS swapchain format.
    Bgra8Unorm,
    /// 8-bit RGBA, unsigned normalized.
    Rgba8Unorm,
    /// Single 8-bit channel — glyph coverage / alpha atlases.
    R8Unorm,
    /// 32-bit float depth.
    Depth32Float,
}

impl TextureFormat {
    /// Bytes per texel (depth formats included).
    pub const fn bytes_per_texel(self) -> usize {
        match self {
            TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm => 4,
            TextureFormat::R8Unorm => 1,
            TextureFormat::Depth32Float => 4,
        }
    }
}

/// Alpha-blend mode for a pipeline's color attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// No blending — source overwrites destination.
    Replace,
    /// Premultiplied-alpha "over": `src + dst * (1 - src.a)`. The Viso default.
    PremultipliedOver,
}

/// How a texture is sampled at coordinates between texels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    /// Nearest-texel sampling.
    Nearest,
    /// Bilinear sampling.
    Linear,
}

/// Texture-coordinate wrapping outside `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressMode {
    /// Clamp to the edge texel.
    ClampToEdge,
    /// Repeat (tile).
    Repeat,
}

bitflags::bitflags! {
    /// What a buffer may be used for. Backends translate these to native usage.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BufferUsage: u32 {
        /// Per-vertex geometry.
        const VERTEX = 1 << 0;
        /// Per-instance data.
        const INSTANCE = 1 << 1;
        /// Index buffer.
        const INDEX = 1 << 2;
        /// Uniform / constant buffer.
        const UNIFORM = 1 << 3;
        /// Written by the CPU each frame (ring / staging).
        const CPU_WRITE = 1 << 4;
    }
}

/// Descriptor for [`crate::GpuBackend::create_buffer`].
#[derive(Debug, Clone, Copy)]
pub struct BufferDesc {
    /// Size in bytes.
    pub size: usize,
    /// Intended usage.
    pub usage: BufferUsage,
    /// A debug label for GPU tooling (ignored by headless).
    pub label: &'static str,
}

/// Descriptor for [`crate::GpuBackend::create_texture`].
#[derive(Debug, Clone, Copy)]
pub struct TextureDesc {
    /// Width in texels.
    pub width: u32,
    /// Height in texels.
    pub height: u32,
    /// Pixel format.
    pub format: TextureFormat,
    /// Whether the texture is usable as a render target (offscreen layer pass).
    pub render_target: bool,
    /// A debug label.
    pub label: &'static str,
}

/// Descriptor for [`crate::GpuBackend::create_sampler`].
#[derive(Debug, Clone, Copy)]
pub struct SamplerDesc {
    /// Minification/magnification filter.
    pub filter: FilterMode,
    /// Coordinate wrapping.
    pub address: AddressMode,
}

/// Which built-in drawing program a pipeline runs.
///
/// A GPU backend that executes real shaders (Metal) ignores this and uses
/// [`PipelineDesc::shader_source`]. The headless software rasterizer has no
/// shader compiler, so it uses this tag to select the CPU fill routine that
/// reproduces the corresponding shader's SDF/AA/blend math. Every Viso
/// primitive maps to exactly one built-in program (§30, §D layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinShader {
    /// A (optionally rounded, optionally bordered) axis-aligned quad.
    Quad,
    /// A textured quad sampling an atlas/image (Image primitive).
    Image,
    /// An MSDF glyph quad sampling the glyph atlas (GlyphRun primitive).
    GlyphRun,
    /// A filled/stroked vector path.
    Path,
    /// A triangle mesh with per-vertex color.
    Mesh,
    /// An offscreen layer composited back with clip/opacity (Layer primitive).
    Layer,
}

/// Descriptor for a render pipeline ([`crate::GpuBackend::create_pipeline`]).
///
/// The shader source is a hand-written MSL string (Phase 2); `instance_schema`
/// is the layout the shader's vertex-input struct declares, validated against
/// the `#[derive(GpuInstance)]` layout of the instance type at registration.
#[derive(Debug, Clone, Copy)]
pub struct PipelineDesc {
    /// Debug label.
    pub label: &'static str,
    /// Which built-in drawing program this pipeline runs (headless dispatch).
    pub builtin: BuiltinShader,
    /// Backend shader source (MSL on Metal; ignored by the headless raster).
    pub shader_source: &'static str,
    /// Entry point name for the vertex stage.
    pub vertex_entry: &'static str,
    /// Entry point name for the fragment stage.
    pub fragment_entry: &'static str,
    /// Color attachment format.
    pub color_format: TextureFormat,
    /// Optional depth attachment format.
    pub depth_format: Option<TextureFormat>,
    /// Color blend mode.
    pub blend: BlendMode,
    /// The instance layout the shader expects (validated at registration).
    pub instance_schema: crate::instance::InstanceSchema,
}

/// One binding in a [`BindGroupDesc`].
#[derive(Debug, Clone, Copy)]
pub enum Binding {
    /// A sampled texture.
    Texture(crate::TextureId),
    /// A sampler.
    Sampler(crate::SamplerId),
    /// A uniform buffer.
    Uniform(crate::BufferId),
}

/// Descriptor for [`crate::GpuBackend::create_bind_group`].
#[derive(Debug, Clone)]
pub struct BindGroupDesc {
    /// Debug label.
    pub label: &'static str,
    /// The bindings, in slot order.
    pub bindings: Vec<Binding>,
}

/// Static device capabilities, queried once via [`crate::GpuBackend::caps`].
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// Maximum texture dimension (for atlas sizing).
    pub max_texture_size: u32,
    /// Whether the backend renders to a real display (false for headless).
    pub presents_to_display: bool,
}
