//! User shader programs: a program the application wrote, compiled off the
//! render thread and installed as a pipeline.
//!
//! The built-in pipelines are created once from frozen backend code at device
//! init (`GpuBackend::create_pipeline`).
//! A user program instead arrives as code generated from its typed program, in
//! the language the backend consumes, together with the [`ProgramAbi`] its code
//! was laid out for. It is built in two steps so the expensive one never runs
//! on a frame:
//!
//! 1. [`ProgramCompiler::compile`], on any thread: the backend's compiler turns
//!    the code into a ready program — the library, the pipeline state, every
//!    validation the driver does — or the compiler's message;
//! 2. `GpuBackend::install_program`, on
//!    the render thread at a frame boundary: the ready program becomes a
//!    [`PipelineId`](crate::PipelineId), drawn like any other pipeline and
//!    retired through the same fence when it is replaced.
//!
//! Every backend reads one program ABI: the uniform block (inline in the draw
//! command when it fits `InlineUniforms::MAX`,
//! otherwise a [`Binding::Uniform`](crate::Binding::Uniform) buffer), the
//! tightly packed instance array at the draw's instance offset — a multiple of
//! [`Caps::program_instance_align`](crate::Caps::program_instance_align) — and
//! the textures then the samplers of the bind group in declaration order —
//! at most [`PROGRAM_TEXTURES`] and [`PROGRAM_SAMPLERS`], so a bind group's
//! fixed tables serve every program. Where a language numbers textures and
//! samplers in one binding space, texture `i` is binding `i` and sampler `j`
//! binding `PROGRAM_TEXTURES + j`.
//!
//! The headless raster has no shader compiler: its program is a
//! [`CpuProgram`] the shader crate supplies, which runs the program on the CPU
//! into the framebuffer the way the GPU backends rasterize it.

use std::fmt;
use std::sync::Arc;

use crate::resource::{BlendMode, SamplerDesc, ShaderLang, TextureFormat};

/// Textures one program may declare.
pub const PROGRAM_TEXTURES: u32 = 4;
/// Samplers one program may declare.
pub const PROGRAM_SAMPLERS: u32 = 2;

/// The interface a program's code was laid out for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ProgramAbi {
    /// Bytes of one instance in the instance array.
    pub instance_stride: u32,
    /// Bytes of the uniform block; `0` for a program without uniforms.
    pub uniform_size: u32,
    /// Textures the program declares, bound in order from slot 0.
    pub textures: u32,
    /// Samplers the program declares, bound in order from slot 0.
    pub samplers: u32,
    /// Whether the vertex entry samples textures too.
    pub vertex_textures: bool,
}

/// A program's code in one backend's language.
#[derive(Clone)]
pub enum ProgramCode {
    /// Metal Shading Language source.
    Msl(String),
    /// WGSL source.
    Wgsl(String),
    /// HLSL shader model 5.1 source.
    Hlsl(String),
    /// SPIR-V words as little-endian bytes.
    SpirV(Vec<u8>),
    /// GLSL ES 3.00 source, both stages.
    Glsl(String),
    /// The program run on the CPU, for the headless raster.
    Cpu(Arc<dyn CpuProgram>),
}

impl ProgramCode {
    /// The language this code is in; [`ShaderLang::None`] for a CPU program.
    pub fn lang(&self) -> ShaderLang {
        match self {
            ProgramCode::Msl(_) => ShaderLang::Msl,
            ProgramCode::Wgsl(_) => ShaderLang::Wgsl,
            ProgramCode::Hlsl(_) => ShaderLang::Hlsl,
            ProgramCode::SpirV(_) => ShaderLang::SpirV,
            ProgramCode::Glsl(_) => ShaderLang::Glsl,
            ProgramCode::Cpu(_) => ShaderLang::None,
        }
    }

    /// The code's bytes, the identity a pipeline cache keys it by; empty for a
    /// CPU program, which has no text.
    pub fn bytes(&self) -> &[u8] {
        match self {
            ProgramCode::Msl(s)
            | ProgramCode::Wgsl(s)
            | ProgramCode::Hlsl(s)
            | ProgramCode::Glsl(s) => s.as_bytes(),
            ProgramCode::SpirV(b) => b,
            ProgramCode::Cpu(_) => &[],
        }
    }
}

impl fmt::Debug for ProgramCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProgramCode::Cpu(_) => f.write_str("Cpu(..)"),
            other => write!(f, "{:?}({} bytes)", other.lang(), other.bytes().len()),
        }
    }
}

/// What a backend needs to build a program.
#[derive(Debug, Clone, Copy)]
pub struct ProgramDesc<'a> {
    /// Debug label.
    pub label: &'a str,
    /// The program, in `GpuBackend::SHADER_LANG`
    /// (a [`ProgramCode::Cpu`] on the headless raster).
    pub code: &'a ProgramCode,
    pub vertex_entry: &'a str,
    pub fragment_entry: &'a str,
    /// The color attachment format it draws into.
    pub color_format: TextureFormat,
    pub blend: BlendMode,
    pub abi: ProgramAbi,
}

impl ProgramDesc<'_> {
    /// Checks the ABI against what every backend binds.
    ///
    /// # Errors
    ///
    /// More textures than [`PROGRAM_TEXTURES`] or samplers than
    /// [`PROGRAM_SAMPLERS`].
    pub fn check_abi(&self) -> Result<(), String> {
        if self.abi.textures > PROGRAM_TEXTURES || self.abi.samplers > PROGRAM_SAMPLERS {
            return Err(format!(
                "{}: the program declares {} textures and {} samplers; a program binds at \
                 most {PROGRAM_TEXTURES} and {PROGRAM_SAMPLERS}",
                self.label, self.abi.textures, self.abi.samplers
            ));
        }
        Ok(())
    }
}

/// A backend's program compiler: cheap to clone, usable from any thread.
pub trait ProgramCompiler: Clone + Send + 'static {
    /// A program ready to install.
    type Program: Send + 'static;

    /// The program `desc` describes, or the compiler's or the driver's message.
    ///
    /// # Errors
    ///
    /// Code in another language than the backend's, code the compiler
    /// rejects, or a pipeline the driver refuses.
    fn compile(&self, desc: &ProgramDesc<'_>) -> Result<Self::Program, String>;
}

/// A program the CPU raster runs.
pub trait CpuProgram: Send + Sync + 'static {
    /// Draws `draw.count` instances into `target`, blending with `blend`.
    fn draw(&self, target: &mut CpuTarget<'_>, draw: &CpuDraw<'_>, blend: BlendMode);
}

/// A window of a premultiplied linear RGBA framebuffer a [`CpuProgram`] draws
/// into: the CPU raster hands a program one tile of its target at a time.
pub struct CpuTarget<'a> {
    /// The window's pixels, row-major from its top-left, `stride` per row.
    pub pixels: &'a mut [[f32; 4]],
    /// The whole target's size, which positions map against.
    pub width: u32,
    pub height: u32,
    /// The window's top-left in the target.
    pub origin: [u32; 2],
    /// Pixels per row of `pixels`.
    pub stride: u32,
    /// The pixels a draw may write, `[x0, y0, x1, y1)` in the target, inside
    /// the window.
    pub scissor: [u32; 4],
    /// Whether the target stores 8 bits per channel, rounding every write.
    pub unorm8: bool,
}

impl CpuTarget<'_> {
    /// The index in `pixels` of target pixel `(x, y)`, which must be inside
    /// the window.
    #[inline]
    pub fn index(&self, x: u32, y: u32) -> usize {
        ((y - self.origin[1]) * self.stride + (x - self.origin[0])) as usize
    }
}

/// The buffers and bindings one draw of a [`CpuProgram`] reads.
#[derive(Debug, Clone, Copy)]
pub struct CpuDraw<'a> {
    /// The uniform block, laid out as the program's ABI says.
    pub uniforms: &'a [u8],
    /// The instance array from the draw's first instance.
    pub instances: &'a [u8],
    pub count: u32,
    /// The textures of the draw's bind group, in declaration order.
    pub textures: &'a [CpuTexture<'a>],
    /// Its samplers, in declaration order.
    pub samplers: &'a [SamplerDesc],
}

/// A texture as the CPU samples it, row-major from the top.
#[derive(Debug, Clone, Copy)]
pub struct CpuTexture<'a> {
    pub width: u32,
    pub height: u32,
    pub texels: CpuTexels<'a>,
}

/// A texture's texels as stored: what a GPU sample of each returns is
/// [`CpuTexels::get`].
#[derive(Debug, Clone, Copy)]
pub enum CpuTexels<'a> {
    /// Four bytes per texel in the format's byte order; `bgra` swaps the first
    /// and third.
    Rgba8 { texels: &'a [[u8; 4]], bgra: bool },
    /// One coverage byte per texel, read as premultiplied white.
    R8(&'a [u8]),
    /// Opaque 5:6:5 color, red in the high bits.
    Rgb565(&'a [u16]),
    /// 16-bit texels as stored, little-endian, two bytes each, so they read in
    /// place at any alignment (an image borrowed from flash).
    Packed16 {
        texels: &'a [[u8; 2]],
        layout: Packed16,
    },
    /// Linear `f32` RGBA.
    Float(&'a [[f32; 4]]),
    /// Nothing written yet: every texel reads as transparent black.
    None,
}

impl CpuTexels<'_> {
    /// Texel `i` as linear RGBA.
    #[inline]
    pub fn get(&self, i: usize) -> [f32; 4] {
        let unorm = |v: u8| v as f32 / 255.0;
        match *self {
            CpuTexels::Rgba8 { texels, bgra } => {
                let [a, b, c, d] = texels[i];
                let (r, bl) = if bgra { (c, a) } else { (a, c) };
                [unorm(r), unorm(b), unorm(bl), unorm(d)]
            }
            CpuTexels::R8(texels) => [unorm(texels[i]); 4],
            CpuTexels::Rgb565(texels) => {
                let [r, g, b] = rgb565_to_8(texels[i]);
                [unorm(r), unorm(g), unorm(b), 1.0]
            }
            CpuTexels::Packed16 { texels, layout } => {
                layout.rgba8(u16::from_le_bytes(texels[i])).map(unorm)
            }
            CpuTexels::Float(texels) => texels[i],
            CpuTexels::None => [0.0; 4],
        }
    }
}

/// How a 16-bit texel packs its channels, red in the high bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packed16 {
    /// 5:6:5, opaque.
    Rgb565,
    /// 4:4:4:4, alpha in the low nibble.
    Rgba4444,
    /// 5:5:5:1, alpha in bit 0.
    Rgba5551,
}

impl Packed16 {
    /// The layout of `format`, when it is a 16-bit color format.
    pub const fn of(format: TextureFormat) -> Option<Packed16> {
        match format {
            TextureFormat::Rgb565Unorm => Some(Packed16::Rgb565),
            TextureFormat::Rgba4444Unorm => Some(Packed16::Rgba4444),
            TextureFormat::Rgba5551Unorm => Some(Packed16::Rgba5551),
            _ => None,
        }
    }

    /// `v` widened to eight bits per channel by bit replication, so black,
    /// white and every opaque or transparent alpha stay exact.
    #[inline]
    pub fn rgba8(self, v: u16) -> [u8; 4] {
        match self {
            Packed16::Rgb565 => {
                let [r, g, b] = rgb565_to_8(v);
                [r, g, b, 255]
            }
            Packed16::Rgba4444 => {
                let n = |shift: u16| ((v >> shift) & 0xf) as u8 * 17;
                [n(12), n(8), n(4), n(0)]
            }
            Packed16::Rgba5551 => {
                let five = |shift: u16| {
                    let c = ((v >> shift) & 0x1f) as u8;
                    (c << 3) | (c >> 2)
                };
                [five(11), five(6), five(1), if v & 1 != 0 { 255 } else { 0 }]
            }
        }
    }
}

/// A 5:6:5 texel widened to eight bits per channel by bit replication, so
/// black and white stay exact.
#[inline]
pub fn rgb565_to_8(v: u16) -> [u8; 3] {
    let r = (v >> 11) as u8 & 0x1f;
    let g = (v >> 5) as u8 & 0x3f;
    let b = v as u8 & 0x1f;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    ]
}
