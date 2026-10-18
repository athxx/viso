//! `CpuRaster` — the CPU rasterizer backend (§30.4): the production backend
//! of the Embedded-CPU profile, the compatibility path of a device without a
//! GPU, and the deterministic reference the golden tests and headless runs
//! draw with.
//!
//! It consumes the same draw lists as the device backends and has no shader
//! compiler: each pipeline's [`BuiltinShader`] tag selects a CPU restatement
//! of that shader ([`fill`]), and a user program arrives as the CPU program
//! its code carries.
//!
//! - **Storage per format** ([`pixels`]): 8-bit targets hold bytes in their
//!   format's order and composite in integers, `Rgb565Unorm` surfaces hold
//!   16 bits (gradients dithered), HDR targets hold `f32`; an `R8Unorm` glyph
//!   or mask atlas holds one byte per texel.
//! - **Tiles** ([`pass`]): a pass bins its draws into 64×64 tiles and visits
//!   only the tiles a draw touches, so a partial redraw costs the damage.
//! - **Rows** ([`span`]): every draw shades a row of premultiplied color and
//!   composites it through SSE2 / NEON / SIMD128 kernels, bit-identical to
//!   their scalar definition.
//! - **Workers** ([`CpuRaster::with_workers`]): bands of tiles rasterize on a
//!   persistent pool; the output does not depend on how many.
//! - **Layers** ([`plan_layers`]): an offscreen pass only composited later —
//!   a group with opacity, a fused color filter — is never drawn whole. Each
//!   tile that composites it evaluates the texels it reads into a worker's
//!   window, so the group costs a tile of scratch, not a target its size.
//!   A layer read any other way (blurred, blended, sampled by a program) is
//!   drawn whole first.
//! - **Scanout** ([`RawWindowHandle::Scanout`]): a display's surface is the
//!   scanout's size and format; each present hands the framebuffer and its
//!   damage to the platform, which copies only the damage into the buffer it
//!   flips.
//!
//! The fragment math follows each built-in's shader IR, so the pixels match
//! a device to within rounding: coverage is a linear ramp over one device
//! pixel, blending is premultiplied source-over, and an 8-bit target rounds
//! every write as an 8-bit attachment does.

mod blit;
mod fill;
mod pass;
mod pixels;
mod pool;
mod sample;
mod span;

use std::sync::{Arc, Mutex};

use viso_handle::{RawWindowHandle, Scanout, ScanoutFormat};

pub use self::blit::{Blitter, Fill, FillPixels};
use self::fill::{Inst, MAX_FIELDS};
use self::pass::{Layer, NO_LAYER, Scratch, Shared, Target, Window};
use self::pixels::{Fmt, Pixels, Px};
use self::pool::Pool;
use crate::backend::{DrawCommand, DrawList, Frame, GpuBackend, LoadOp, RenderPass, RenderTarget};
use crate::instance::InstanceLayout;
use crate::program::{CpuProgram, ProgramAbi, ProgramCode, ProgramCompiler, ProgramDesc};
use crate::resource::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BuiltinShader, Caps, ColorSpace,
    PipelineDesc, SampledFormats, SamplerDesc, TextureDesc, TextureFormat, f32_to_f16,
};
use crate::retire::{Epoch, Fence, ResourceKind, RetireQueue, Retired};
use crate::slots::SlotMap;
use crate::{BindGroupId, BufferId, CpuTexture, PipelineId, SamplerId, SurfaceId, TextureId};

/// The side of a raster tile, in pixels.
const TILE: u32 = 64;

/// A texture: its format, size and pixels.
struct Texture {
    width: u32,
    height: u32,
    format: TextureFormat,
    pixels: Pixels,
}

impl Texture {
    fn view(&self) -> CpuTexture<'_> {
        CpuTexture {
            width: self.width,
            height: self.height,
            texels: self.pixels.texels(self.format),
        }
    }

    fn len(&self) -> usize {
        self.width as usize * self.height as usize
    }
}

/// A registered pipeline: the built-in it runs and where that built-in's
/// fields sit in an instance, or a user program.
struct Pipeline {
    builtin: BuiltinShader,
    blend: BlendMode,
    stride: usize,
    offsets: [u16; MAX_FIELDS],
    /// The first field the built-in reads that the layout lacks (or holds in
    /// another format): drawing it is a bug in the built-in's registration.
    missing: Option<&'static str>,
    program: Option<CpuRasterProgram>,
}

impl Pipeline {
    /// Instance (or vertex) `n` of `bytes` from `offset`, `None` past the end.
    fn instance<'a>(&'a self, bytes: &'a [u8], offset: usize, n: u32) -> Option<Inst<'a>> {
        if let Some(name) = self.missing {
            panic!("built-in shader expects instance field `{name}`");
        }
        let at = offset + n as usize * self.stride;
        Some(Inst {
            bytes: bytes.get(at..at + self.stride)?,
            offsets: &self.offsets,
        })
    }
}

/// A user program for the CPU raster: the CPU program the shader crate built,
/// and what it was compiled for.
#[derive(Clone)]
pub struct CpuRasterProgram {
    cpu: Arc<dyn CpuProgram>,
    blend: BlendMode,
    abi: ProgramAbi,
}

/// The CPU raster's program "compiler": it accepts the CPU program a user
/// program's code carries.
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuCompiler;

impl ProgramCompiler for CpuCompiler {
    type Program = CpuRasterProgram;

    fn compile(&self, desc: &ProgramDesc<'_>) -> Result<CpuRasterProgram, String> {
        desc.check_abi()?;
        match desc.code {
            ProgramCode::Cpu(cpu) => Ok(CpuRasterProgram {
                cpu: Arc::clone(cpu),
                blend: desc.blend,
                abi: desc.abi,
            }),
            other => Err(format!(
                "{}: the CPU raster runs CPU programs, not {:?}",
                desc.label,
                other.lang()
            )),
        }
    }
}

/// Everything a pass reads besides its target, shared by its workers.
#[derive(Default)]
struct Resources {
    buffers: SlotMap<Vec<u8>>,
    textures: SlotMap<Texture>,
    samplers: SlotMap<SamplerDesc>,
    pipelines: SlotMap<Pipeline>,
    bind_groups: SlotMap<Vec<Binding>>,
}

impl Resources {
    fn buffer(&self, id: BufferId) -> &Vec<u8> {
        self.buffers
            .get(id.into())
            .expect("buffer handle does not resolve")
    }

    fn texture(&self, id: TextureId) -> &Texture {
        self.textures
            .get(id.into())
            .expect("texture handle does not resolve")
    }
}

/// A surface: a framebuffer in its format, kept across frames.
struct Surface {
    width: u32,
    height: u32,
    format: TextureFormat,
    /// The space the compositor would read the framebuffer through. The
    /// raster itself is space-agnostic; a test sets it to stand in for a
    /// wide-gamut or HDR swapchain and observe what the renderer plans.
    color_space: ColorSpace,
    pixels: Pixels,
    /// The framebuffer holds the last presented frame.
    presented: bool,
    /// The KMS scanout the frames go to, when the surface is a display's.
    scanout: Option<ScanoutRef>,
}

/// A surface's [`Scanout`]: the platform's, alive as long as its window.
#[derive(Clone, Copy)]
struct ScanoutRef(*const dyn Scanout);

// SAFETY: a scanout is only called from `present_with_damage`, and a raster
// presenting to a display runs on the thread that runs the platform's loop,
// as `Scanout` requires.
unsafe impl Send for ScanoutRef {}

impl Surface {
    fn len(&self) -> usize {
        self.width as usize * self.height as usize
    }

    fn texels(&self) -> crate::CpuTexels<'_> {
        self.pixels.texels(self.format)
    }
}

/// The CPU raster backend.
pub struct CpuRaster {
    res: Resources,
    surfaces: SlotMap<Surface>,
    caps: Caps,
    /// Resources awaiting reclamation, parked with the epoch they were retired in.
    retire_queue: RetireQueue,
    /// Highest epoch finished. A frame completes the moment it is presented,
    /// so this trails `current_epoch` by exactly the frame being built.
    fence: Fence,
    /// The epoch of the frame being built, advanced by `begin_frame`.
    current_epoch: Epoch,
    reclaim_scratch: Vec<Retired>,
    /// While a profiler times frames, the raster time of each frame encoded
    /// since it last asked: the CPU stands in for the GPU here.
    timed: Option<Vec<(u64, u64)>>,
    scratch: Scratch,
    pool: Option<Pool>,
    /// One window per worker of `pool` (the caller's first).
    workers: Vec<Mutex<Window>>,
    /// The frame's deferred layers, each binned in the scratch of its index.
    layers: Vec<Layer>,
    layer_scratch: Vec<Scratch>,
    /// Per command of the draw list, the layer it composites.
    layer_of: Vec<u16>,
    /// Per pass of the draw list, the layer it is, or [`NO_LAYER`] if drawn.
    pass_layer: Vec<u16>,
    blitter: Option<Box<dyn Blitter>>,
    defer_layers: bool,
}

impl Default for CpuRaster {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuRaster {
    /// A CPU raster drawing on the calling thread.
    pub fn new() -> Self {
        Self {
            res: Resources::default(),
            surfaces: SlotMap::new(),
            caps: Caps {
                max_texture_size: 16384,
                presents_to_display: false,
                compute_dispatch: false,
                bindless_texture_slots: 0,
                indirect_draw: false,
                program_instance_align: 4,
                coverage_paths: true,
                // The 16-bit formats sample in place; a block-compressed image
                // is decoded before it is uploaded.
                sampled: SampledFormats::RGB565
                    .union(SampledFormats::RGBA4444)
                    .union(SampledFormats::RGBA5551),
            },
            retire_queue: RetireQueue::new(),
            fence: Fence::new(),
            current_epoch: Epoch::START,
            reclaim_scratch: Vec::new(),
            timed: None,
            scratch: Scratch::default(),
            pool: None,
            workers: vec![Mutex::default()],
            layers: Vec::new(),
            layer_scratch: Vec::new(),
            layer_of: Vec::new(),
            pass_layer: Vec::new(),
            blitter: None,
            defer_layers: true,
        }
    }

    /// Whether offscreen passes that are only composited are deferred to the
    /// tiles compositing them (the default) or drawn whole first, as a device
    /// draws them — the same pixels either way, at a target's memory.
    pub fn set_layer_deferral(&mut self, on: bool) {
        self.defer_layers = on;
    }

    /// Hands the raster's solid fills — a pass's clear — to a 2D
    /// accelerator; `None` returns them to the CPU.
    pub fn set_blitter(&mut self, blitter: Option<Box<dyn Blitter>>) {
        self.blitter = blitter;
    }

    /// A CPU raster that spreads each pass over `workers` threads besides the
    /// caller, started now and parked between passes. `0` draws on the
    /// calling thread alone. The pixels are the same either way.
    pub fn with_workers(workers: usize) -> Self {
        let mut raster = Self::new();
        raster.start_workers(workers);
        raster
    }

    fn start_workers(&mut self, workers: usize) {
        self.pool = (workers > 0).then(|| Pool::new(workers));
        let width = self.pool.as_ref().map_or(1, Pool::width);
        self.workers = (0..width).map(|_| Mutex::default()).collect();
    }

    /// Whether the raster asks for filled paths as scanline coverage (the
    /// default) or as triangles, as a device does — for a test of the
    /// tessellated lane, or one reproducing a device's lowering. Read by a
    /// renderer when it is created.
    pub fn set_coverage_paths(&mut self, on: bool) {
        self.caps.coverage_paths = on;
    }

    /// Resources still parked awaiting reclamation. A steady frame that grows
    /// a buffer parks one slot and reclaims it a frame later, so this stays
    /// bounded and drains to zero.
    pub fn retired_count(&self) -> usize {
        self.retire_queue.len()
    }

    /// Reclaims every parked slot whose epoch the fence has passed (bumping
    /// its generation, so the retired handle goes stale).
    fn reclaim_completed(&mut self) {
        self.reclaim_scratch.clear();
        self.retire_queue
            .drain_completed(self.fence, &mut self.reclaim_scratch);
        for entry in self.reclaim_scratch.drain(..) {
            match entry.kind {
                ResourceKind::Buffer => {
                    self.res.buffers.remove(entry.id);
                }
                ResourceKind::Texture => {
                    self.res.textures.remove(entry.id);
                }
                ResourceKind::Sampler => {
                    self.res.samplers.remove(entry.id);
                }
                ResourceKind::Pipeline => {
                    self.res.pipelines.remove(entry.id);
                }
                ResourceKind::BindGroup => {
                    self.res.bind_groups.remove(entry.id);
                }
            }
        }
    }

    /// The last presented framebuffer of `surface` as packed **BGRA8**,
    /// top-left first, un-premultiplied — a `Bgra8Unorm` swapchain readback.
    pub fn read_pixels_bgra8(&self, surface: SurfaceId) -> Vec<u8> {
        let s = self.surface(surface);
        let texels = s.texels();
        let mut out = Vec::with_capacity(s.len() * 4);
        for i in 0..s.len() {
            let [r, g, b, a] = texels.get(i);
            let (r, g, b) = unpremultiply(r, g, b, a);
            out.extend([b, g, r, a].map(to_unorm8));
        }
        out
    }

    /// Texture `id` as packed **BGRA8**, top-left first, as stored
    /// (premultiplied) — a `Bgra8Unorm` render-target readback.
    pub fn read_texture(&self, id: TextureId) -> Vec<u8> {
        let t = self.res.texture(id);
        let texels = t.pixels.texels(t.format);
        (0..t.len())
            .flat_map(|i| {
                let [r, g, b, a] = texels.get(i);
                [b, g, r, a].map(to_unorm8)
            })
            .collect()
    }

    /// The last presented framebuffer of `surface` as packed **RGBA16F**,
    /// premultiplied — an extended-range swapchain readback, which keeps
    /// values above `1.0` an 8-bit readback would clip.
    pub fn read_pixels_rgba16f(&self, surface: SurfaceId) -> Vec<u8> {
        let s = self.surface(surface);
        let texels = s.texels();
        let mut out = Vec::with_capacity(s.len() * 8);
        for i in 0..s.len() {
            for c in texels.get(i) {
                out.extend_from_slice(&f32_to_f16(c).to_le_bytes());
            }
        }
        out
    }

    /// Stores `surface` in `format` and reports `color_space` for it: a
    /// wide-gamut or HDR swapchain for a test, or a panel's own format
    /// (`Rgb565Unorm`) for a device. The framebuffer is discarded.
    pub fn set_surface_color_target(
        &mut self,
        surface: SurfaceId,
        format: TextureFormat,
        color_space: ColorSpace,
    ) {
        let s = self
            .surfaces
            .get_mut(surface.into())
            .expect("surface handle does not resolve");
        if s.format != format {
            s.format = format;
            s.pixels = Pixels::zeroed(format, s.len());
            s.presented = false;
        }
        s.color_space = color_space;
    }

    /// One texel of `surface` at `(x, y)`, premultiplied linear, as stored.
    pub fn surface_texel(&self, surface: SurfaceId, x: u32, y: u32) -> [f32; 4] {
        let s = self.surface(surface);
        s.texels().get((y * s.width + x) as usize)
    }

    fn surface(&self, id: SurfaceId) -> &Surface {
        self.surfaces
            .get(id.into())
            .expect("surface handle does not resolve")
    }

    /// Buffers created over this backend's lifetime (never a lookup or a slot
    /// reuse): a steady frame that reuses its buffers leaves it unchanged.
    pub fn buffer_count(&self) -> usize {
        self.res.buffers.created()
    }

    /// Textures created over this backend's lifetime.
    pub fn texture_count(&self) -> usize {
        self.res.textures.created()
    }

    /// Bind groups created over this backend's lifetime.
    pub fn bind_group_count(&self) -> usize {
        self.res.bind_groups.created()
    }

    /// The bytes of pixels held by textures and surfaces.
    pub fn pixel_bytes(&self) -> usize {
        let textures: usize = self.res.textures.iter().map(|t| t.pixels.bytes()).sum();
        let surfaces: usize = self.surfaces.iter().map(|s| s.pixels.bytes()).sum();
        textures + surfaces
    }

    /// The bytes of the workers' windows: what deferred layers and user
    /// programs draw into, a tile or so each.
    pub fn scratch_bytes(&self) -> usize {
        self.workers
            .iter()
            .map(|w| w.lock().expect("raster window lock").bytes())
            .sum()
    }

    /// Offscreen passes the last draw list deferred to the tiles compositing
    /// them, never allocating their targets.
    pub fn deferred_layers(&self) -> usize {
        self.layers.len()
    }
}

/// Draws `pass` into its target.
#[allow(clippy::too_many_arguments)]
fn encode_pass(
    res: &mut Resources,
    surfaces: &mut SlotMap<Surface>,
    scratch: &mut Scratch,
    pool: Option<&Pool>,
    workers: &[Mutex<Window>],
    blitter: Option<&mut (dyn Blitter + '_)>,
    (layers, layer_scratch, layer_of): (&[Layer], &[Scratch], &[u16]),
    list: &[DrawCommand],
    pass: &RenderPass,
) {
    let range = pass.command_range();
    let (slot, width, height, format) = target_pixels(res, surfaces, pass.target);
    // Out of its slot for the pass, so the resources it samples stay
    // borrowable; a pass never samples its own target.
    let mut px = std::mem::take(slot);
    let len = width as usize * height as usize;
    let fmt = Fmt::of(format);
    let mut load = pass.load;
    if matches!(px, Pixels::None) {
        px = match load {
            LoadOp::Clear(c) => Pixels::filled(format, len, c),
            LoadOp::Load => Pixels::zeroed(format, len),
        };
    } else if let (LoadOp::Clear(c), Some(blitter)) = (load, blitter) {
        let fill = |pixels| Fill {
            pixels,
            width,
            height,
            rect: [0, 0, width, height],
        };
        let filled = match &mut px {
            Pixels::Unorm8(pixels) => blitter.fill(fill(FillPixels::Bytes4 {
                value: <[u8; 4]>::store(c, fmt),
                pixels,
            })),
            Pixels::Rgb565(pixels) => blitter.fill(fill(FillPixels::Rgb565 {
                value: u16::store(c, fmt),
                pixels,
            })),
            _ => false,
        };
        if filled {
            load = LoadOp::Load;
        }
    }
    let shared = Shared {
        res,
        layers,
        layer_scratch,
        list,
        layer_of: layer_of.get(range.clone()).unwrap_or_default(),
        workers,
    };
    let unorm8 = !format.is_extended_range();
    let commands = &list[range];
    macro_rules! run {
        ($px:expr) => {
            pass::run(
                shared,
                Target {
                    px: $px,
                    width,
                    height,
                    stride: width as usize,
                    fmt,
                    unorm8,
                },
                load,
                commands,
                scratch,
                pool,
            )
        };
    }
    match &mut px {
        Pixels::Unorm8(p) => run!(p),
        Pixels::Rgb565(p) => run!(p),
        Pixels::Float(p) => run!(p),
        Pixels::R8(_) | Pixels::Packed16(_) | Pixels::Static(_) => {
            panic!("a {format:?} texture is not a render target")
        }
        Pixels::None => unreachable!("allocated above"),
    }
    *target_pixels(res, surfaces, pass.target).0 = px;
}

/// The pixels `target` holds, with its size and format.
fn target_pixels<'a>(
    res: &'a mut Resources,
    surfaces: &'a mut SlotMap<Surface>,
    target: RenderTarget,
) -> (&'a mut Pixels, u32, u32, TextureFormat) {
    match target {
        RenderTarget::Surface(frame) => {
            let s = surfaces
                .get_mut(frame.surface.into())
                .expect("surface handle does not resolve");
            (&mut s.pixels, s.width, s.height, s.format)
        }
        RenderTarget::Texture(id) => {
            let t = res
                .textures
                .get_mut(id.into())
                .expect("texture handle does not resolve");
            (&mut t.pixels, t.width, t.height, t.format)
        }
    }
}

impl GpuBackend for CpuRaster {
    const SHADER_LANG: crate::ShaderLang = crate::ShaderLang::None;
    const TIMES_FRAMES: bool = !cfg!(target_arch = "wasm32");

    fn set_cpu_workers(&mut self, threads: usize) {
        let current = self.pool.as_ref().map_or(0, |p| p.width() - 1);
        if threads != current {
            self.start_workers(threads);
        }
    }

    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        self.res.buffers.insert(vec![0u8; desc.size]).into()
    }

    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId {
        assert!(
            self.caps.samples(desc.format),
            "the CPU raster samples no {:?} texture: decode it to RGBA8 first (Caps::sampled)",
            desc.format
        );
        // Pixels come with the first write or pass: a target a frame never
        // draws holds nothing.
        self.res
            .textures
            .insert(Texture {
                width: desc.width,
                height: desc.height,
                format: desc.format,
                pixels: Pixels::None,
            })
            .into()
    }

    fn create_texture_static(&mut self, desc: &TextureDesc, texels: &'static [u8]) -> TextureId {
        let len = desc.width as usize * desc.height as usize;
        if !Pixels::borrows(desc.format)
            || texels.len() < desc.format.data_size(desc.width, desc.height)
        {
            let texture = self.create_texture(desc);
            self.write_texture(texture, 0, 0, desc.width, desc.height, texels);
            return texture;
        }
        debug_assert!(
            !desc.render_target,
            "a texture over static bytes is never drawn into"
        );
        self.res
            .textures
            .insert(Texture {
                width: desc.width,
                height: desc.height,
                format: desc.format,
                pixels: Pixels::Static(&texels[..len * desc.format.bytes_per_texel()]),
            })
            .into()
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        self.res.samplers.insert(*desc).into()
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, crate::instance::LayoutError> {
        layout.validate_against(&desc.instance_schema)?;
        let mut offsets = [0u16; MAX_FIELDS];
        let mut missing = None;
        for (slot, &(name, format)) in fill::schema(desc.builtin).iter().enumerate() {
            match layout.fields.iter().find(|f| f.name == name) {
                Some(f) if f.format == format => offsets[slot] = f.offset as u16,
                _ => {
                    missing.get_or_insert(name);
                }
            }
        }
        Ok(self
            .res
            .pipelines
            .insert(Pipeline {
                builtin: desc.builtin,
                blend: desc.blend,
                stride: layout.stride,
                offsets,
                missing,
                program: None,
            })
            .into())
    }

    type Compiler = CpuCompiler;

    fn program_compiler(&self) -> CpuCompiler {
        CpuCompiler
    }

    fn install_program(&mut self, program: CpuRasterProgram) -> PipelineId {
        self.res
            .pipelines
            .insert(Pipeline {
                builtin: BuiltinShader::Quad,
                blend: program.blend,
                stride: program.abi.instance_stride as usize,
                offsets: [0; MAX_FIELDS],
                missing: None,
                program: Some(program),
            })
            .into()
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        self.res.bind_groups.insert(desc.bindings.clone()).into()
    }

    fn destroy_buffer(&mut self, id: BufferId) {
        if self.res.buffers.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Buffer, id.into(), self.current_epoch);
        }
    }

    fn destroy_texture(&mut self, id: TextureId) {
        if self.res.textures.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Texture, id.into(), self.current_epoch);
        }
    }

    fn destroy_sampler(&mut self, id: SamplerId) {
        if self.res.samplers.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Sampler, id.into(), self.current_epoch);
        }
    }

    fn destroy_pipeline(&mut self, id: PipelineId) {
        if self.res.pipelines.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Pipeline, id.into(), self.current_epoch);
        }
    }

    fn destroy_bind_group(&mut self, id: BindGroupId) {
        if self.res.bind_groups.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::BindGroup, id.into(), self.current_epoch);
        }
    }

    fn write_buffer(&mut self, id: BufferId, offset: usize, bytes: &[u8]) {
        let buf = self
            .res
            .buffers
            .get_mut(id.into())
            .expect("buffer handle does not resolve");
        buf[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]) {
        let tex = self
            .res
            .textures
            .get_mut(id.into())
            .expect("texture handle does not resolve");
        let len = tex.len();
        tex.pixels
            .write(tex.format, len, tex.width, (x, y, w, h), bytes);
    }

    fn create_surface(&mut self, raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId {
        let (scanout, format, width, height) = match raw {
            RawWindowHandle::Scanout { scanout } => {
                // SAFETY: the platform keeps the scanout alive with its window.
                let (s, (w, h)) = unsafe { (&*scanout, (*scanout).size()) };
                let format = match s.format() {
                    ScanoutFormat::Xrgb8888 => TextureFormat::Bgra8Unorm,
                    ScanoutFormat::Rgb565 => TextureFormat::Rgb565Unorm,
                };
                (Some(ScanoutRef(scanout)), format, w, h)
            }
            _ => (None, TextureFormat::Bgra8Unorm, width, height),
        };
        self.surfaces
            .insert(Surface {
                width,
                height,
                format,
                color_space: ColorSpace::Srgb,
                pixels: Pixels::zeroed(format, width as usize * height as usize),
                presented: false,
                scanout,
            })
            .into()
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        let s = self
            .surfaces
            .get_mut(id.into())
            .expect("surface handle does not resolve");
        s.width = width;
        s.height = height;
        s.pixels = Pixels::zeroed(s.format, s.len());
        s.presented = false;
    }

    fn destroy_surface(&mut self, id: SurfaceId) {
        self.surfaces.remove(id.into());
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame> {
        // A resource destroyed in epoch N is reclaimed no earlier than the
        // begin_frame after N was presented. A framebuffer in memory is never
        // out of date, so acquisition always succeeds.
        self.reclaim_completed();
        self.current_epoch = self.current_epoch.next();
        Some(Frame {
            surface,
            drawable: 0,
        })
    }

    fn encode(&mut self, list: &DrawList<'_>) {
        #[cfg(not(target_family = "wasm"))]
        let started = self.timed.is_some().then(std::time::Instant::now);
        plan_layers(
            &self.res,
            list,
            self.defer_layers,
            &mut self.layers,
            &mut self.layer_of,
            &mut self.pass_layer,
        );
        if self.layer_scratch.len() < self.layers.len() {
            self.layer_scratch
                .resize_with(self.layers.len(), Scratch::default);
        }
        for (layer, scratch) in self.layers.iter().zip(&mut self.layer_scratch) {
            let commands = &list.commands[layer.commands.clone()];
            pass::prepare(&self.res, commands, layer.width, layer.height, scratch);
        }
        for (pass, &layer) in list.passes.iter().zip(&self.pass_layer) {
            if layer != NO_LAYER {
                continue;
            }
            encode_pass(
                &mut self.res,
                &mut self.surfaces,
                &mut self.scratch,
                self.pool.as_ref(),
                &self.workers,
                self.blitter.as_deref_mut(),
                (&self.layers, &self.layer_scratch, &self.layer_of),
                list.commands,
                pass,
            );
        }
        // A target whose last write was deferred holds no pixels: nothing
        // reads them, and its next frame defers it again.
        for layer in &self.layers {
            let rewritten = list.passes[layer.pass + 1..]
                .iter()
                .any(|p| targets(p, layer.texture));
            if !rewritten && let Some(t) = self.res.textures.get_mut(layer.texture.into()) {
                t.pixels = Pixels::None;
            }
        }
        #[cfg(not(target_family = "wasm"))]
        if let (Some(started), Some(timed)) = (started, &mut self.timed) {
            let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            match timed.last_mut() {
                Some((epoch, sum)) if *epoch == self.current_epoch.0 => *sum += nanos,
                _ => timed.push((self.current_epoch.0, nanos)),
            }
        }
    }

    fn present(&mut self, frame: Frame) {
        // The framebuffer already holds the image. With nothing asynchronous,
        // the frame finishes as it is presented: signal its epoch, so the next
        // begin_frame reclaims what it retired.
        if let Some(s) = self.surfaces.get_mut(frame.surface.into()) {
            s.presented = true;
        }
        self.fence.signal(self.current_epoch);
    }

    /// Presents `frame`, and hands a display's scanout the framebuffer and
    /// what changed in it: the scanout copies only that into its buffers.
    fn present_with_damage(&mut self, frame: Frame, damage: &[(u32, u32, u32, u32)]) {
        self.present(frame);
        let Some(s) = self.surfaces.get(frame.surface.into()) else {
            return;
        };
        let Some(ScanoutRef(scanout)) = s.scanout else {
            return;
        };
        let (bytes, stride): (&[u8], usize) = match &s.pixels {
            Pixels::Unorm8(p) => (p.as_flattened(), s.width as usize * 4),
            Pixels::Rgb565(p) => (
                // SAFETY: the `u16`s are plain integers, viewed as their
                // bytes for as long as the borrow of `p`; in memory they are
                // little-endian, as DRM's RGB565 is, on every target a KMS
                // display runs on.
                unsafe { std::slice::from_raw_parts(p.as_ptr().cast(), p.len() * 2) },
                s.width as usize * 2,
            ),
            _ => return,
        };
        // SAFETY: the platform keeps the scanout alive with its window, and
        // this runs on its loop's thread (`ScanoutRef`).
        unsafe { (*scanout).present_pixels(bytes, stride, damage) };
    }

    fn frame_age(&self, frame: Frame) -> u32 {
        // One framebuffer, kept: it holds the frame presented last.
        self.surfaces
            .get(frame.surface.into())
            .map_or(0, |s| u32::from(s.presented))
    }

    fn device_lost(&mut self, _surface: SurfaceId) {
        // Nothing to rebuild; but a frame opened and never presented must not
        // leave the fence behind, or parked slots would never reclaim.
        self.fence.signal(self.current_epoch);
        self.reclaim_completed();
    }

    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn time_frames(&mut self, on: bool) {
        if on != self.timed.is_some() {
            self.timed = on.then(Vec::new);
        }
    }

    fn frame_epoch(&self) -> u64 {
        self.current_epoch.0
    }

    fn finished_frame_times(&mut self, out: &mut Vec<(u64, u64)>) {
        if let Some(timed) = &mut self.timed {
            out.append(timed);
        }
    }

    fn surface_format(&self, surface: SurfaceId) -> TextureFormat {
        self.surface(surface).format
    }

    fn surface_color_space(&self, surface: SurfaceId) -> ColorSpace {
        self.surface(surface).color_space
    }
}

/// Which texture passes of `list` become [`Layer`]s: a pass that clears a
/// color texture which every later read — up to the texture's next write —
/// composites by region ([`fill::composites_by_region`], clamp-to-edge) from
/// a pass that is itself drawn, and whose own inputs no pass rewrites before
/// its last composite. Deciding from the last pass back lets a layer's
/// composite sit in a pass that is decided first.
fn plan_layers(
    res: &Resources,
    list: &DrawList<'_>,
    defer: bool,
    layers: &mut Vec<Layer>,
    layer_of: &mut Vec<u16>,
    pass_layer: &mut Vec<u16>,
) {
    layers.clear();
    layer_of.clear();
    pass_layer.clear();
    pass_layer.resize(list.passes.len(), NO_LAYER);
    if !defer
        || !list
            .passes
            .iter()
            .any(|p| matches!(p.target, RenderTarget::Texture(_)))
    {
        return;
    }
    layer_of.resize(list.commands.len(), NO_LAYER);
    for (pi, pass) in list.passes.iter().enumerate().rev() {
        let (RenderTarget::Texture(texture), LoadOp::Clear(clear)) = (pass.target, pass.load)
        else {
            continue;
        };
        let Some(t) = res.textures.get(texture.into()) else {
            continue;
        };
        let storable = matches!(
            t.format,
            TextureFormat::Bgra8Unorm
                | TextureFormat::Rgba8Unorm
                | TextureFormat::Rgba8Data
                | TextureFormat::Rgba16Float
        );
        if !storable || layers.len() + 1 >= usize::from(NO_LAYER) {
            continue;
        }
        let Some(last) = last_composite(res, list, pi, texture, pass_layer) else {
            continue;
        };
        let inputs_rewritten = list.commands[pass.command_range()].iter().any(|cmd| {
            bindings(res, cmd).iter().any(|b| match *b {
                Binding::Texture(s) => list.passes[pi + 1..=last].iter().any(|p| targets(p, s)),
                _ => false,
            })
        });
        if inputs_rewritten {
            continue;
        }
        let k = layers.len() as u16;
        for p in &list.passes[pi + 1..=last] {
            for ci in p.command_range() {
                if texture_slots(res, &list.commands[ci], texture).is_some() {
                    layer_of[ci] = k;
                }
            }
        }
        pass_layer[pi] = k;
        layers.push(Layer {
            texture,
            width: t.width,
            height: t.height,
            format: t.format,
            clear,
            pass: pi,
            commands: pass.command_range(),
        });
    }
}

/// The last pass that reads `texture` as written by pass `pi`, if there is
/// one and every read there is a region composite from a drawn pass.
fn last_composite(
    res: &Resources,
    list: &DrawList<'_>,
    pi: usize,
    texture: TextureId,
    pass_layer: &[u16],
) -> Option<usize> {
    let mut last = None;
    for (pj, pass) in list.passes.iter().enumerate().skip(pi + 1) {
        for cmd in &list.commands[pass.command_range()] {
            let Some((slot0, others)) = texture_slots(res, cmd, texture) else {
                continue;
            };
            let pipe = res.pipelines.get(cmd.pipeline.into())?;
            let composite = pass_layer[pj] == NO_LAYER
                && slot0
                && !others
                && pipe.program.is_none()
                && fill::composites_by_region(pipe.builtin)
                && sampler_address(res, cmd) == AddressMode::ClampToEdge;
            if !composite {
                return None;
            }
            last = Some(pj);
        }
        if targets(pass, texture) {
            // A pass that loads the texture reads it whole.
            if pass.load == LoadOp::Load {
                return None;
            }
            break;
        }
    }
    last
}

fn bindings<'a>(res: &'a Resources, cmd: &DrawCommand) -> &'a [Binding] {
    cmd.bind_group
        .and_then(|bg| res.bind_groups.get(bg.into()))
        .map_or(&[], |b| &b[..])
}

/// Whether `cmd` binds `texture` as its texture 0, and whether at any other
/// texture slot; `None` when it does not bind it.
fn texture_slots(res: &Resources, cmd: &DrawCommand, texture: TextureId) -> Option<(bool, bool)> {
    let (mut slot0, mut others, mut seen) = (false, false, 0);
    for b in bindings(res, cmd) {
        if let Binding::Texture(id) = *b {
            if id == texture {
                if seen == 0 {
                    slot0 = true;
                } else {
                    others = true;
                }
            }
            seen += 1;
        }
    }
    (slot0 || others).then_some((slot0, others))
}

/// The address mode `cmd` samples through: its bind group's last sampler,
/// or clamp-to-edge.
fn sampler_address(res: &Resources, cmd: &DrawCommand) -> AddressMode {
    bindings(res, cmd)
        .iter()
        .rev()
        .find_map(|b| match *b {
            Binding::Sampler(id) => res.samplers.get(id.into()).map(|s| s.address),
            _ => None,
        })
        .unwrap_or(AddressMode::ClampToEdge)
}

fn targets(pass: &RenderPass, texture: TextureId) -> bool {
    matches!(pass.target, RenderTarget::Texture(id) if id == texture)
}

/// `round(clamp(v, 0, 1) * 255)` as a byte.
fn to_unorm8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A premultiplied color's straight RGB (zero where `a == 0`).
fn unpremultiply(r: f32, g: f32, b: f32, a: f32) -> (f32, f32, f32) {
    if a <= 0.0 {
        (0.0, 0.0, 0.0)
    } else {
        (r / a, g / a, b / a)
    }
}
