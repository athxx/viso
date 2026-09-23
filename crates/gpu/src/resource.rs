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
    /// Four independent 8-bit unorm channels of *data*, not color.
    ///
    /// Same storage as [`Rgba8Unorm`](TextureFormat::Rgba8Unorm) — what differs
    /// is the content convention. A color texture's alpha is opacity, so a
    /// backend may premultiply it into RGB; here all four channels are signed
    /// distances (the MTSDF glyph field: three per-edge channels plus a true
    /// distance), and multiplying three of them by the fourth would destroy the
    /// field. Uploaded and sampled verbatim, and never promoted to a wider color
    /// domain, since a distance is not a color in any domain.
    Rgba8Data,
    /// 16-bit half-float RGBA. The storage an HDR target needs: values outside
    /// `[0, 1]` survive it, which no unorm format can do at any bit depth.
    Rgba16Float,
    /// 32-bit float depth.
    Depth32Float,
}

impl TextureFormat {
    /// Bytes per texel (depth formats included).
    pub const fn bytes_per_texel(self) -> usize {
        match self {
            TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Data => 4,
            TextureFormat::R8Unorm => 1,
            TextureFormat::Rgba16Float => 8,
            TextureFormat::Depth32Float => 4,
        }
    }

    /// Whether this format stores values outside `[0, 1]`.
    ///
    /// An unorm format cannot, at any bit depth: 10-bit unorm buys *precision*
    /// inside the unit range, not headroom above it. So this — not the bit depth
    /// — is what decides whether a target can hold an HDR value.
    pub const fn is_extended_range(self) -> bool {
        matches!(self, TextureFormat::Rgba16Float)
    }

    /// Whether this format stores color (as opposed to coverage or depth).
    ///
    /// Coverage and distance planes are deliberately excluded: a glyph or clip
    /// mask is an occupancy fraction in `[0, 1]` by definition and a distance
    /// field is geometry, so no color domain ever promotes either. Widening
    /// every `R8Unorm` atlas alongside the color targets would multiply the
    /// largest textures in the frame for no representable gain.
    pub const fn is_color(self) -> bool {
        matches!(
            self,
            TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm | TextureFormat::Rgba16Float
        )
    }
}

/// Widen one IEEE-754 binary16 value to `f32`.
///
/// The CPU side of [`TextureFormat::Rgba16Float`]: a backend decoding such a
/// texel and an upper layer baking one must agree bit for bit, so the pair lives
/// beside the format rather than inside either caller.
///
/// Exact for every input: binary16's 11-bit significand and 5-bit exponent both
/// fit inside binary32's, so subnormals normalize and infinities/NaNs carry their
/// payload across without a rounding decision anywhere.
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) as u32) << 31;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let mantissa = (bits & 0x3ff) as u32;
    let f = match exp {
        // Zero or subnormal: scale the significand by 2^-24 to normalize it.
        0 => {
            if mantissa == 0 {
                sign
            } else {
                let value = mantissa as f32 * (1.0 / 16_777_216.0);
                return f32::from_bits(sign | value.to_bits());
            }
        }
        // Infinity or NaN: binary32's exponent is all ones too.
        0x1f => sign | 0x7f80_0000 | (mantissa << 13),
        // Normal: rebias the exponent (15 -> 127) and left-align the significand.
        _ => sign | ((exp + 112) << 23) | (mantissa << 13),
    };
    f32::from_bits(f)
}

/// Encode one `f32` as IEEE-754 binary16, saturating to the finite maximum.
///
/// Round-to-nearest-even on the significand, matching what a GPU does when it
/// writes a half-float attachment.
pub fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 31) as u16) << 15;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x7f_ffff;
    if exp == 0xff {
        // Infinity, or a NaN kept non-zero so it does not decode as infinity.
        return sign | 0x7c00 | if mantissa != 0 { 0x200 } else { 0 };
    }
    // Unbiased exponent, rebiased for binary16.
    let e = exp - 127 + 15;
    if e >= 0x1f {
        // Above binary16's range: clamp to the largest finite value rather than
        // silently turning a bright highlight into an infinity.
        return sign | 0x7bff;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        // Subnormal: shift the implicit leading one back into the significand.
        let m = mantissa | 0x80_0000;
        let shift = (14 - e) as u32;
        let half_bits = (m >> shift) as u16;
        let round = (m >> (shift - 1)) & 1;
        return sign | (half_bits + round as u16);
    }
    let half_bits = ((e as u32) << 10) as u16 | (mantissa >> 13) as u16;
    // Round to nearest, ties to even, on the 13 discarded significand bits.
    let rest = mantissa & 0x1fff;
    let round = usize::from(rest > 0x1000 || (rest == 0x1000 && (half_bits & 1) == 1));
    sign | (half_bits + round as u16)
}

/// The primaries and transfer function a compositor reads a surface's texels
/// through.
///
/// Orthogonal to [`TextureFormat`], which fixes only precision and range: P3 at
/// 8 bits per channel and sRGB at 8 bits per channel are the same *format* in
/// different *spaces*, and they are not interchangeable. Keeping the two apart is
/// what lets a wide-gamut target stay 8-bit — the alternative, inferring the space
/// from the format, forces every wide-gamut window to pay for float storage it
/// does not need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorSpace {
    /// sRGB primaries, sRGB transfer, `[0, 1]`. The ordinary SDR window.
    #[default]
    Srgb,
    /// Display P3 primaries, sRGB transfer, `[0, 1]`. Wide gamut, standard range.
    DisplayP3,
    /// sRGB primaries, linear transfer, unbounded. HDR in the sRGB gamut.
    ExtendedLinearSrgb,
    /// Display P3 primaries, linear transfer, unbounded. Wide gamut *and* HDR.
    ExtendedLinearDisplayP3,
}

impl ColorSpace {
    /// The [`ColorDomain`] this space belongs to.
    pub const fn domain(self) -> ColorDomain {
        match self {
            ColorSpace::Srgb => ColorDomain::Sdr,
            ColorSpace::DisplayP3 => ColorDomain::WideGamut,
            ColorSpace::ExtendedLinearSrgb | ColorSpace::ExtendedLinearDisplayP3 => {
                ColorDomain::Hdr
            }
        }
    }
}

/// The three target classes a render graph plans intermediate formats for.
///
/// A [`ColorSpace`] names an exact encoding; a domain names the *storage
/// requirement* that follows from it, which is all the planner needs. Several
/// spaces collapse onto one domain — extended-linear sRGB and extended-linear P3
/// differ in gamut but make the same demand of a texture — so planning on the
/// domain keeps the format decision from multiplying with every space added later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorDomain {
    /// sRGB, standard range. 8-bit unorm is sufficient and correct.
    #[default]
    Sdr,
    /// Wider primaries, still standard range. Needs the right space, not more
    /// bits: promoting it to float would buy headroom nothing can occupy.
    WideGamut,
    /// Unbounded range. Requires extended-range storage end to end; any unorm
    /// stage in the middle clips the highlights permanently.
    Hdr,
}

impl ColorDomain {
    /// Whether a target in this domain must use an extended-range format.
    pub const fn requires_extended_range(self) -> bool {
        matches!(self, ColorDomain::Hdr)
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

/// How a texture is sampled at coordinates between texels (and, for
/// `MipmapLinear`, across mip levels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterMode {
    /// Nearest-texel sampling.
    Nearest,
    /// Bilinear sampling within one mip level.
    Linear,
    /// Trilinear sampling: bilinear within a mip level and linear between the
    /// two straddling mip levels. Used for heavily minified images. The
    /// headless raster has no mip chain, so it degrades to `Linear` on the base
    /// level — the mip blend only takes effect on a device backend (Metal:
    /// `minFilter = linear, mipFilter = linear`).
    MipmapLinear,
}

/// Texture-coordinate wrapping outside `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressMode {
    /// Clamp to the edge texel.
    ClampToEdge,
    /// Repeat (tile).
    Repeat,
    /// Mirror on each repeat: the coordinate reflects at every integer boundary
    /// (a period-2 triangle wave), so tiles alternate flipped.
    Mirror,
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
///
/// `Hash`/`Eq` make this usable as an interning key: the renderer keeps one
/// sampler per distinct descriptor rather than one per draw (§17.1, §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamplerDesc {
    /// Minification/magnification filter.
    pub filter: FilterMode,
    /// Coordinate wrapping.
    pub address: AddressMode,
}

impl SamplerDesc {
    /// Bilinear filtering, clamp-to-edge wrapping — the default for whole-image
    /// draws, glyph coverage, and layer compositing.
    pub const LINEAR_CLAMP: Self = Self {
        filter: FilterMode::Linear,
        address: AddressMode::ClampToEdge,
    };
}

impl Default for SamplerDesc {
    /// [`SamplerDesc::LINEAR_CLAMP`].
    fn default() -> Self {
        Self::LINEAR_CLAMP
    }
}

/// Which built-in drawing program a pipeline runs.
///
/// A GPU backend that executes real shaders (Metal) ignores this and uses
/// [`PipelineDesc::msl`]. The headless software rasterizer has no
/// shader compiler, so it uses this tag to select the CPU fill routine that
/// reproduces the corresponding shader's SDF/AA/blend math. Every Viso
/// primitive maps to exactly one built-in program (§30, §D layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinShader {
    /// A (optionally rounded, optionally bordered) axis-aligned quad.
    Quad,
    /// A textured quad sampling an atlas/image (Image primitive).
    Image,
    /// A glyph quad sampling an A8 coverage atlas (GlyphRun primitive).
    GlyphRun,
    /// A glyph quad sampling a multi-channel signed distance field atlas, the
    /// scalable text lane (Mtsdf primitive).
    Mtsdf,
    /// A filled/stroked vector path.
    Path,
    /// A triangle mesh with per-vertex color.
    Mesh,
    /// An analytic rounded rectangle with an independent radius per corner
    /// (AnalyticRRect primitive).
    AnalyticRRect,
    /// An analytic axis-aligned ellipse, a circle when its axes are equal
    /// (AnalyticEllipse primitive).
    AnalyticEllipse,
    /// An analytic capsule/stadium: a rounded box whose corner radius is the
    /// smaller half-extent (AnalyticCapsule primitive).
    AnalyticCapsule,
    /// An analytic stroked line segment defined by two endpoints, with cap/join
    /// and SDF antialiasing (AnalyticLine primitive).
    AnalyticLine,
    /// A gradient fill (linear/radial/sweep) over an axis-aligned rectangle,
    /// sampling a cached 1D gradient LUT or lerping two inline premultiplied
    /// stops (Gradient primitive).
    Gradient,
    /// A soft drop shadow for an analytic shape (rounded box / ellipse / capsule),
    /// its coverage a closed-form Gaussian ramp over the shape's signed distance
    /// (AnalyticShadow primitive).
    AnalyticShadow,
    /// An offscreen layer composited back with clip/opacity (Layer primitive).
    Layer,
    /// A separable Gaussian blur pass sampling a source texture along one axis
    /// (content blur of an offscreen layer).
    Blur,
    /// A fused color-effect pass sampling a source texture and mapping each texel
    /// through an affine color matrix plus an optional gamma. One pass realizes a
    /// whole run of mergeable color effects.
    ColorTransform,
    /// A frosted material composite: samples a blurred backdrop, tints it through an
    /// affine color matrix plus an optional gamma, adds grain hashed from the integer
    /// device pixel, and masks the result with the surface's own rounded rect — the
    /// whole §18 chain minus the passes that produced the backdrop and the border
    /// drawn over it.
    Material,
    /// An isolated advanced-blend composite: samples an isolated layer (source) and
    /// a bounded snapshot of what is behind it (destination) from **two** textures,
    /// evaluates a separable-artistic or non-separable-HSL blend function, and
    /// writes the result with [`BlendMode::Replace`] — the destination has already
    /// been accounted for in the fragment, so the fixed-function stage must not mix
    /// it in a second time.
    AdvancedBlend,
}

/// Descriptor for a render pipeline ([`crate::GpuBackend::create_pipeline`]).
///
/// A pipeline is created from a standard-manifest artifact, never from
/// caller-assembled source: `msl` is the frozen backend shader text the manifest
/// carries (MSL on Metal), and `variant` is its packed pipeline-variant identity.
/// `instance_schema` is the layout the shader's vertex-input struct declares,
/// validated against the derived instance layout of the instance type at
/// registration (§32/§36.1).
#[derive(Debug, Clone, Copy)]
pub struct PipelineDesc {
    /// Debug label.
    pub label: &'static str,
    /// Which built-in drawing program this pipeline runs (headless dispatch).
    pub builtin: BuiltinShader,
    /// The packed variant identity of the manifest entry this pipeline realizes
    /// (pipeline-changing dimensions only). A stable integer key, never a string.
    pub variant: u32,
    /// The program in the language this backend consumes
    /// ([`GpuBackend::SHADER_LANG`](crate::GpuBackend::SHADER_LANG)); the headless
    /// raster dispatches on `builtin` and takes [`ShaderCode::None`]. Compiled at
    /// device init, never on a draw.
    pub code: ShaderCode,
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

/// The shading language a backend consumes its programs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderLang {
    /// No program text: the backend dispatches on [`BuiltinShader`].
    None,
    /// Metal Shading Language source.
    Msl,
    /// WGSL source with uniforms in a bound buffer (WebGPU).
    Wgsl,
    /// HLSL shader model 5.1 source (Direct3D 12).
    Hlsl,
    /// A SPIR-V module with both entry points and push-constant uniforms (Vulkan).
    SpirV,
}

/// One pipeline's program, in one [`ShaderLang`].
///
/// Every stage reads the same interface: the attribute fields at locations /
/// semantics `0..n` in schema order, the viewport uniform, textures `tex` and
/// `dst_tex` in slots 0 and 1, and one sampler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderCode {
    /// No program text (see [`ShaderLang::None`]).
    None,
    /// MSL source.
    Msl(&'static str),
    /// WGSL source.
    Wgsl(&'static str),
    /// HLSL source.
    Hlsl(&'static str),
    /// SPIR-V words as little-endian bytes (length a multiple of four).
    SpirV(&'static [u8]),
}

impl ShaderCode {
    /// The language this code is in.
    pub const fn lang(self) -> ShaderLang {
        match self {
            ShaderCode::None => ShaderLang::None,
            ShaderCode::Msl(_) => ShaderLang::Msl,
            ShaderCode::Wgsl(_) => ShaderLang::Wgsl,
            ShaderCode::Hlsl(_) => ShaderLang::Hlsl,
            ShaderCode::SpirV(_) => ShaderLang::SpirV,
        }
    }
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
    /// Whether this backend can dispatch compute kernels (§20.1).
    ///
    /// A device capability *and* an RHI one: the answer is `false` until the
    /// backend actually exposes a dispatch entry point, because a capability the
    /// layers above cannot call is not a capability. The Metal and headless
    /// backends therefore both report `false` today — Metal's device has compute,
    /// this RHI has no encoder for it (§17.1 keeps the RHI small).
    ///
    /// Upper layers read this as a *veto*, never as an instruction: a compute lane
    /// is entered on a measured benefit over a large dynamic workload, and this
    /// only says whether that lane exists to be entered at all (§20.1, §7.2).
    pub compute_dispatch: bool,
    /// How many textures one draw can address through a resource table, or `0`
    /// when the backend binds textures per draw (§20.2).
    ///
    /// The bindless capability behind Metal argument/resource tables, a D3D12
    /// descriptor heap, Vulkan descriptor indexing, and WebGPU binding arrays,
    /// reported as the one number that decides anything: the number of slots an
    /// instance's texture index may select from. Zero means the fast path does not
    /// exist here, not that images cost more — the fallback is atlasing, a small
    /// texture set, and bind-group batching, which is what this renderer does.
    ///
    /// Reported honestly, like [`compute_dispatch`](Self::compute_dispatch): both
    /// backends answer `0` because this RHI's [`BindGroupDesc`] binds one texture
    /// per slot at creation and has no table to index. The public paint API does
    /// not change with the answer (§20.2) — only how many draws a frame of images
    /// costs.
    pub bindless_texture_slots: u32,
    /// Whether a draw can read its vertex/instance counts from a buffer the GPU
    /// wrote, rather than from arguments the CPU passed (§24.1).
    ///
    /// The other half of GPU-driven rendering: a compute kernel decides what is
    /// visible and writes the draw arguments, and an indirect draw consumes them, so
    /// the CPU never learns the count. Either half alone is not the capability —
    /// indirect draw without dispatch just moves the same CPU-computed numbers
    /// through a buffer.
    ///
    /// Reported honestly, like the other two: both backends answer `false`, because
    /// this RHI's draw commands carry their counts inline and no encoder here reads
    /// them from memory (§17.1). Upper layers read it as a *veto* — a culling plan
    /// escalates on scene shape and measured cull cost, and this only says whether
    /// the GPU plan exists to be reached at all (§20, §7.2).
    pub indirect_draw: bool,
}
