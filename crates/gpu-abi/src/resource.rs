//! GPU resource descriptors and format enums (the cold-path "create" vocabulary).
//!
//! Descriptors are plain data passed to `GpuBackend` create-methods,
//! which return the typed handles from `lib.rs`. Kept backend-neutral: no Metal
//! / D3D / Vulkan types leak here (§17.1).

/// Texture / render-target pixel format.
///
/// The uncompressed formats hold one texel per [`block`](Self::block); the
/// block-compressed ones (BC, ETC2, ASTC) hold a fixed-size block of texels
/// that the sampler decodes, so a texture of them is written whole blocks at a
/// time and is never a render target. Every color format's texels are
/// premultiplied (Rendering §5.4–§5.5), compressed ones included: an encoder
/// compresses premultiplied texels.
///
/// A `*Srgb` format stores the same bytes as its linear twin, holding
/// sRGB-encoded color: sampling decodes it to linear light, rendering into it
/// encodes, and blending into it runs in linear light (ADR 0048). Its texels
/// are premultiplied in linear light, `enc(dec(c) · a)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    /// 8-bit BGRA, unsigned normalized — the canonical macOS swapchain format.
    Bgra8Unorm,
    /// [`Bgra8Unorm`](Self::Bgra8Unorm) holding sRGB-encoded color: an sRGB
    /// surface's format.
    Bgra8UnormSrgb,
    /// 8-bit RGBA, unsigned normalized.
    Rgba8Unorm,
    /// [`Rgba8Unorm`](Self::Rgba8Unorm) holding sRGB-encoded color: an image's
    /// format.
    Rgba8UnormSrgb,
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
    /// Opaque 16-bit color, 5:6:5 with red in the high bits of a little-endian
    /// `u16` — the panel format of most embedded displays (§30.4). It has no
    /// alpha, so it is a surface format, never a layer's.
    Rgb565Unorm,
    /// 16-bit color with alpha, 4:4:4:4 with red in the high nibble and alpha
    /// in the low one of a little-endian `u16`. A sampled format only: an
    /// image whose alpha survives four bits.
    Rgba4444Unorm,
    /// 16-bit color with one bit of alpha, 5:5:5:1 with red in the high bits
    /// and alpha in bit 0 of a little-endian `u16`. A sampled format only: an
    /// image whose texels are opaque or fully transparent.
    Rgba5551Unorm,
    /// 16-bit half-float RGBA. The storage an HDR target needs: values outside
    /// `[0, 1]` survive it, which no unorm format can do at any bit depth.
    Rgba16Float,
    /// 32-bit float depth.
    Depth32Float,
    /// BC1 (DXT1): 4×4 texels in 8 bytes, two 5:6:5 endpoints and 2-bit
    /// indices; a block may spend one index on transparent black.
    Bc1RgbaUnorm,
    /// [`Bc1RgbaUnorm`](Self::Bc1RgbaUnorm), sRGB-encoded.
    Bc1RgbaUnormSrgb,
    /// BC3 (DXT5): 4×4 texels in 16 bytes, BC1 color beside an interpolated
    /// 8-bit alpha block.
    Bc3RgbaUnorm,
    /// [`Bc3RgbaUnorm`](Self::Bc3RgbaUnorm), sRGB-encoded.
    Bc3RgbaUnormSrgb,
    /// BC7: 4×4 texels in 16 bytes, eight modes of partitioned endpoints.
    Bc7RgbaUnorm,
    /// [`Bc7RgbaUnorm`](Self::Bc7RgbaUnorm), sRGB-encoded.
    Bc7RgbaUnormSrgb,
    /// ETC2 RGB: 4×4 opaque texels in 8 bytes.
    Etc2Rgb8Unorm,
    /// [`Etc2Rgb8Unorm`](Self::Etc2Rgb8Unorm), sRGB-encoded.
    Etc2Rgb8UnormSrgb,
    /// ETC2 RGB with punch-through alpha: 4×4 texels in 8 bytes, each opaque
    /// or transparent black.
    Etc2Rgb8A1Unorm,
    /// [`Etc2Rgb8A1Unorm`](Self::Etc2Rgb8A1Unorm), sRGB-encoded.
    Etc2Rgb8A1UnormSrgb,
    /// ETC2 RGBA: 4×4 texels in 16 bytes, an EAC alpha block before the ETC2
    /// color block.
    Etc2Rgba8Unorm,
    /// [`Etc2Rgba8Unorm`](Self::Etc2Rgba8Unorm), sRGB-encoded.
    Etc2Rgba8UnormSrgb,
    /// ASTC LDR, 4×4 texels in 16 bytes (8 bits per texel).
    Astc4x4Unorm,
    /// [`Astc4x4Unorm`](Self::Astc4x4Unorm), sRGB-encoded.
    Astc4x4UnormSrgb,
    /// ASTC LDR, 6×6 texels in 16 bytes (3.56 bits per texel).
    Astc6x6Unorm,
    /// [`Astc6x6Unorm`](Self::Astc6x6Unorm), sRGB-encoded.
    Astc6x6UnormSrgb,
    /// ASTC LDR, 8×8 texels in 16 bytes (2 bits per texel).
    Astc8x8Unorm,
    /// [`Astc8x8Unorm`](Self::Astc8x8Unorm), sRGB-encoded.
    Astc8x8UnormSrgb,
}

/// The storage unit of a [`TextureFormat`]: `width × height` texels in
/// `bytes` bytes. An uncompressed format's block is one texel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FormatBlock {
    pub width: u32,
    pub height: u32,
    pub bytes: u32,
}

impl TextureFormat {
    /// Bytes per texel of an uncompressed format (depth formats included).
    ///
    /// # Panics
    ///
    /// On a block-compressed format, whose texels have no size of their own:
    /// size its data with [`data_size`](Self::data_size) and its rows with
    /// [`row_bytes`](Self::row_bytes).
    pub const fn bytes_per_texel(self) -> usize {
        let block = self.block();
        assert!(
            block.width == 1 && block.height == 1,
            "a block-compressed format has no bytes per texel"
        );
        block.bytes as usize
    }

    /// Whether the format holds sRGB-encoded color.
    pub const fn is_srgb(self) -> bool {
        self.linear() as u8 != self as u8
    }

    /// The linear twin of an sRGB format, which stores the same bytes; any
    /// other format is its own.
    pub const fn linear(self) -> TextureFormat {
        match self {
            TextureFormat::Bgra8UnormSrgb => TextureFormat::Bgra8Unorm,
            TextureFormat::Rgba8UnormSrgb => TextureFormat::Rgba8Unorm,
            TextureFormat::Bc1RgbaUnormSrgb => TextureFormat::Bc1RgbaUnorm,
            TextureFormat::Bc3RgbaUnormSrgb => TextureFormat::Bc3RgbaUnorm,
            TextureFormat::Bc7RgbaUnormSrgb => TextureFormat::Bc7RgbaUnorm,
            TextureFormat::Etc2Rgb8UnormSrgb => TextureFormat::Etc2Rgb8Unorm,
            TextureFormat::Etc2Rgb8A1UnormSrgb => TextureFormat::Etc2Rgb8A1Unorm,
            TextureFormat::Etc2Rgba8UnormSrgb => TextureFormat::Etc2Rgba8Unorm,
            TextureFormat::Astc4x4UnormSrgb => TextureFormat::Astc4x4Unorm,
            TextureFormat::Astc6x6UnormSrgb => TextureFormat::Astc6x6Unorm,
            TextureFormat::Astc8x8UnormSrgb => TextureFormat::Astc8x8Unorm,
            other => other,
        }
    }

    /// The sRGB variant of an 8-bit color format; `None` for a format with
    /// none (a 16-bit packed, float, data or depth one). An sRGB format is
    /// its own.
    pub const fn srgb(self) -> Option<TextureFormat> {
        Some(match self.linear() {
            TextureFormat::Bgra8Unorm => TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Rgba8Unorm => TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Bc1RgbaUnorm => TextureFormat::Bc1RgbaUnormSrgb,
            TextureFormat::Bc3RgbaUnorm => TextureFormat::Bc3RgbaUnormSrgb,
            TextureFormat::Bc7RgbaUnorm => TextureFormat::Bc7RgbaUnormSrgb,
            TextureFormat::Etc2Rgb8Unorm => TextureFormat::Etc2Rgb8UnormSrgb,
            TextureFormat::Etc2Rgb8A1Unorm => TextureFormat::Etc2Rgb8A1UnormSrgb,
            TextureFormat::Etc2Rgba8Unorm => TextureFormat::Etc2Rgba8UnormSrgb,
            TextureFormat::Astc4x4Unorm => TextureFormat::Astc4x4UnormSrgb,
            TextureFormat::Astc6x6Unorm => TextureFormat::Astc6x6UnormSrgb,
            TextureFormat::Astc8x8Unorm => TextureFormat::Astc8x8UnormSrgb,
            _ => return None,
        })
    }

    /// The unit the format stores texels in.
    pub const fn block(self) -> FormatBlock {
        let (width, height, bytes) = match self.linear() {
            TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Data => {
                (1, 1, 4)
            }
            TextureFormat::R8Unorm => (1, 1, 1),
            TextureFormat::Rgb565Unorm
            | TextureFormat::Rgba4444Unorm
            | TextureFormat::Rgba5551Unorm => (1, 1, 2),
            TextureFormat::Rgba16Float => (1, 1, 8),
            TextureFormat::Depth32Float => (1, 1, 4),
            TextureFormat::Bc1RgbaUnorm
            | TextureFormat::Etc2Rgb8Unorm
            | TextureFormat::Etc2Rgb8A1Unorm => (4, 4, 8),
            TextureFormat::Bc3RgbaUnorm
            | TextureFormat::Bc7RgbaUnorm
            | TextureFormat::Etc2Rgba8Unorm
            | TextureFormat::Astc4x4Unorm => (4, 4, 16),
            TextureFormat::Astc6x6Unorm => (6, 6, 16),
            TextureFormat::Astc8x8Unorm => (8, 8, 16),
            _ => unreachable!(),
        };
        FormatBlock {
            width,
            height,
            bytes,
        }
    }

    /// Whether the format stores texels in multi-texel blocks a sampler
    /// decodes (BC, ETC2, ASTC).
    pub const fn is_compressed(self) -> bool {
        let block = self.block();
        block.width > 1 || block.height > 1
    }

    /// The bytes of one row of blocks of a `width`-texel-wide region: a
    /// partial block at the right edge is stored whole.
    pub const fn row_bytes(self, width: u32) -> usize {
        let block = self.block();
        width.div_ceil(block.width) as usize * block.bytes as usize
    }

    /// The rows of blocks of a `height`-texel-tall region.
    pub const fn block_rows(self, height: u32) -> u32 {
        height.div_ceil(self.block().height)
    }

    /// The bytes of a `width × height` region, every partial block whole.
    pub const fn data_size(self, width: u32, height: u32) -> usize {
        self.row_bytes(width) * self.block_rows(height) as usize
    }

    /// The capability a backend reports to sample the format natively; empty
    /// for the baseline every backend samples (§6.3).
    pub const fn sampled_class(self) -> SampledFormats {
        match self.linear() {
            TextureFormat::Rgb565Unorm => SampledFormats::RGB565,
            TextureFormat::Rgba4444Unorm => SampledFormats::RGBA4444,
            TextureFormat::Rgba5551Unorm => SampledFormats::RGBA5551,
            TextureFormat::Bc1RgbaUnorm
            | TextureFormat::Bc3RgbaUnorm
            | TextureFormat::Bc7RgbaUnorm => SampledFormats::BC,
            TextureFormat::Etc2Rgb8Unorm
            | TextureFormat::Etc2Rgb8A1Unorm
            | TextureFormat::Etc2Rgba8Unorm => SampledFormats::ETC2,
            TextureFormat::Astc4x4Unorm
            | TextureFormat::Astc6x6Unorm
            | TextureFormat::Astc8x8Unorm => SampledFormats::ASTC,
            _ => SampledFormats::empty(),
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
        !matches!(
            self,
            TextureFormat::R8Unorm | TextureFormat::Rgba8Data | TextureFormat::Depth32Float
        )
    }

    /// Whether this format stores alpha: a format without it is opaque, and a
    /// translucent intermediate cannot be one.
    pub const fn has_alpha(self) -> bool {
        !matches!(
            self,
            TextureFormat::Rgb565Unorm
                | TextureFormat::R8Unorm
                | TextureFormat::Depth32Float
                | TextureFormat::Etc2Rgb8Unorm
                | TextureFormat::Etc2Rgb8UnormSrgb
        )
    }
}

bitflags::bitflags! {
    /// The texture formats beyond the baseline a backend samples natively
    /// ([`Caps::sampled`]). A format the device lacks is decoded to
    /// [`TextureFormat::Rgba8Unorm`] on the CPU before it is uploaded, so the
    /// answer decides cost, never correctness.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SampledFormats: u32 {
        /// [`TextureFormat::Rgb565Unorm`].
        const RGB565 = 1 << 0;
        /// [`TextureFormat::Rgba4444Unorm`].
        const RGBA4444 = 1 << 1;
        /// [`TextureFormat::Rgba5551Unorm`].
        const RGBA5551 = 1 << 2;
        /// BC1, BC3 and BC7.
        const BC = 1 << 3;
        /// ETC2 RGB, RGB with punch-through alpha, and RGBA.
        const ETC2 = 1 << 4;
        /// ASTC LDR at every block size.
        const ASTC = 1 << 5;
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

/// Descriptor for `GpuBackend::create_buffer`.
#[derive(Debug, Clone, Copy)]
pub struct BufferDesc {
    /// Size in bytes.
    pub size: usize,
    /// Intended usage.
    pub usage: BufferUsage,
    /// A debug label for GPU tooling (ignored by headless).
    pub label: &'static str,
}

/// Descriptor for `GpuBackend::create_texture`.
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

/// Descriptor for `GpuBackend::create_sampler`.
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
    /// A lit, depth-tested 3D mesh drawn once per instance ([`crate::scene`]):
    /// the scene lane's one program.
    Scene,
}

/// Descriptor for a render pipeline (`GpuBackend::create_pipeline`).
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
    /// (`GpuBackend::SHADER_LANG`); the headless
    /// raster dispatches on `builtin` and takes [`ShaderCode::None`]. Compiled at
    /// device init, never on a draw.
    pub code: ShaderCode,
    /// Entry point name for the vertex stage.
    pub vertex_entry: &'static str,
    /// Entry point name for the fragment stage.
    pub fragment_entry: &'static str,
    /// Color attachment format.
    pub color_format: TextureFormat,
    /// How the pipeline tests and writes the pass's depth attachment; `None`
    /// for a pipeline drawn in passes without one.
    pub depth: Option<DepthState>,
    /// Which triangles it discards by their winding.
    pub cull: CullMode,
    /// Color blend mode.
    pub blend: BlendMode,
    /// The instance layout the shader expects (validated at registration).
    pub instance_schema: crate::instance::InstanceSchema,
}

/// How a pipeline tests a fragment's depth against the depth attachment's,
/// and whether a fragment that passes writes its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DepthState {
    /// The depth attachment's format, [`TextureFormat::Depth32Float`].
    pub format: TextureFormat,
    pub compare: CompareFunction,
    pub write: bool,
}

impl DepthState {
    /// Reversed-Z opaque geometry: the attachment cleared to `0.0`, a nearer
    /// fragment has the greater depth, and each that passes writes it.
    pub const REVERSED: Self = Self {
        format: TextureFormat::Depth32Float,
        compare: CompareFunction::Greater,
        write: true,
    };

    /// Reversed-Z translucent geometry: tested against the opaque depth, never
    /// written, so blended layers do not hide one another.
    pub const REVERSED_TEST_ONLY: Self = Self {
        format: TextureFormat::Depth32Float,
        compare: CompareFunction::GreaterEqual,
        write: false,
    };
}

/// Which triangles a pipeline discards: a front face winds counter-clockwise
/// in clip space, `y` up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CullMode {
    /// Draw both faces.
    None,
    /// Discard the triangles facing away: a closed mesh's far side.
    Back,
}

/// When a fragment passes the depth test: its depth compared with the one
/// stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareFunction {
    Always,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

impl CompareFunction {
    /// Whether a fragment at `depth` passes against `stored`.
    #[inline]
    pub fn passes(self, depth: f32, stored: f32) -> bool {
        match self {
            CompareFunction::Always => true,
            CompareFunction::Less => depth < stored,
            CompareFunction::LessEqual => depth <= stored,
            CompareFunction::Greater => depth > stored,
            CompareFunction::GreaterEqual => depth >= stored,
        }
    }
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
    /// GLSL ES 3.00 source holding both stages, the vertex one under
    /// `VISO_VERTEX`, without its `#version` line (OpenGL ES 3).
    Glsl,
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
    /// GLSL ES 3.00 source.
    Glsl(&'static str),
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
            ShaderCode::Glsl(_) => ShaderLang::Glsl,
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

/// Descriptor for `GpuBackend::create_bind_group`.
#[derive(Debug, Clone)]
pub struct BindGroupDesc {
    /// Debug label.
    pub label: &'static str,
    /// The bindings, in slot order.
    pub bindings: Vec<Binding>,
}

/// Static device capabilities, queried once via `GpuBackend::caps`.
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
    /// The alignment, in bytes, of a user program draw's instance offset
    /// ([`crate::program`]): the program reads its instances from a storage
    /// buffer bound at that offset, and a backend's storage binding has a
    /// minimum offset alignment (Vulkan's and WebGPU's
    /// `minStorageBufferOffsetAlignment`). A power of two, at least 4.
    pub program_instance_align: u32,
    /// Whether a filled path is cheaper as scanline coverage than as
    /// triangles (§30.4). A device rasterizes a convex fan for free and keeps
    /// paths tessellated unless their outline is complex; the CPU raster pays
    /// per triangle, so it fills every path through a coverage mask, built
    /// once per outline and composited like a glyph.
    pub coverage_paths: bool,
    /// The formats beyond the baseline the device samples (§6.3): the
    /// 16-bit packed ones and the block-compressed families. An image in a
    /// format outside the set is decoded to RGBA8 before upload.
    pub sampled: SampledFormats,
}

impl Caps {
    /// Whether a texture of `format` can be created and sampled as is.
    pub const fn samples(&self, format: TextureFormat) -> bool {
        self.sampled.contains(format.sampled_class())
    }
}
