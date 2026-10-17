//! The user programs a renderer draws: brush-profile programs installed by
//! slot, each with its own persistent instance buffer.
//!
//! A slot holds a pipeline and the [`BrushLayout`] it was laid out by.
//! Installing into an occupied slot replaces the program; releasing a slot
//! destroys its buffer through the backend's fence-deferred path, so a frame
//! still in flight finishes with it.
//!
//! The pipelines are kept by the code they were built from: a program whose
//! code and ABI are those of a pipeline held draws with it, so two slots of one
//! code share a pipeline and a reload that left a shader's code as it was
//! compiles nothing. A pipeline no slot draws with stays a while for an edit
//! that comes back to it: at most [`IDLE_PIPELINES`], the longest unused
//! destroyed first, through the fence like the rest.
//!
//! Each frame, the lowering walk appends every [`ShaderDraw`] of a slot to that
//! slot's word scratch: the node's encoded instance, its box written in, the
//! target origin subtracted. A draw that does not extend the previous draw of
//! its program starts a new run, aligned to the backend's
//! [`program_instance_align`](viso_gpu::Caps::program_instance_align) so its
//! byte offset can bind as a storage-buffer offset. The scratch then syncs
//! against the pool's shadow like every built-in family: an unchanged frame
//! uploads nothing.
//!
//! The clock a brush reads is a uniform, written into each draw as its command
//! is built, so a frame that only moves the clock uploads no instance; the
//! lowering counts the boxes of programs reading it, which is what keeps a
//! window drawing frames.
//!
//! A program sampling images binds one bind group per draw: the draw's
//! textures, then the program's samplers, interned per descriptor when it is
//! installed. The bind group of each texture set is cached on the program, so
//! a steady frame creates none, and destroyed with the program or with a
//! texture it names; it is part of the batch key, so adjacent boxes of one
//! program merge only over the same images.
//!
//! [`ShaderDraw`]: crate::primitive::ShaderDraw

use viso_gpu::backend::{DrawCommand, Geometry, InlineUniforms};
use viso_gpu::{
    BindGroupDesc, BindGroupId, Binding, BlendMode, BufferUsage, GpuBackend, PROGRAM_SAMPLERS,
    PROGRAM_TEXTURES, PipelineId, ProgramAbi, ProgramCode, ProgramDesc, SamplerId, ShaderLang,
    TextureFormat, TextureId,
};
use viso_shader::program::{BrushLayout, FRAGMENT_ENTRY, PackedBrush, VERTEX_ENTRY};

use crate::batch::BatchKey;
use crate::pool::InstancePool;
use crate::primitive::{ProgramSlot, ProgramTextures};

/// A brush-profile program as a renderer installs it: its name, its layout,
/// and its code in each language it was built for — a release package carries
/// the target backend's and the CPU program, a dev host every language its
/// runtime's target uses.
#[derive(Clone)]
pub struct BrushProgram {
    pub name: Box<str>,
    pub layout: BrushLayout,
    pub code: Vec<ProgramCode>,
}

impl From<PackedBrush> for BrushProgram {
    /// The brush a package carries, its code moved over as built.
    fn from(packed: PackedBrush) -> BrushProgram {
        let PackedBrush { name, layout, code } = packed;
        BrushProgram {
            name,
            layout,
            code: code.into_iter().map(|code| code.unpack()).collect(),
        }
    }
}

impl BrushProgram {
    /// Its code in `lang`.
    pub fn code_for(&self, lang: ShaderLang) -> Option<&ProgramCode> {
        self.code.iter().find(|code| code.lang() == lang)
    }

    /// What a backend reading `lang` builds it from to draw into `format`,
    /// as a renderer of that surface format installs it: on any thread,
    /// with the backend's [`program_compiler`](GpuBackend::program_compiler).
    ///
    /// # Errors
    ///
    /// No code in `lang`, or a layout a draw cannot carry or bind.
    pub fn desc(
        &self,
        lang: ShaderLang,
        format: TextureFormat,
    ) -> Result<ProgramDesc<'_>, ProgramError> {
        let code = self.code_for(lang).ok_or(ProgramError::NoCode(lang))?;
        check_layout(&self.layout)?;
        Ok(desc(&self.name, code, &self.layout, format))
    }
}

/// What a backend builds `code`, laid out by `layout`, from to draw into
/// `format`, premultiplied source-over.
pub(crate) fn desc<'a>(
    label: &'a str,
    code: &'a ProgramCode,
    layout: &BrushLayout,
    format: TextureFormat,
) -> ProgramDesc<'a> {
    ProgramDesc {
        label,
        code,
        vertex_entry: VERTEX_ENTRY,
        fragment_entry: FRAGMENT_ENTRY,
        color_format: format,
        blend: BlendMode::PremultipliedOver,
        abi: layout.abi(),
    }
}

impl std::fmt::Debug for BrushProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let langs: Vec<ShaderLang> = self.code.iter().map(ProgramCode::lang).collect();
        f.debug_struct("BrushProgram")
            .field("name", &self.name)
            .field("layout", &self.layout)
            .field("code", &langs)
            .finish()
    }
}

/// Why a program was not installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramError {
    /// The slot is past [`BatchKey::PROGRAM_SLOTS`].
    Slot(ProgramSlot),
    /// The uniform block does not fit the inline uniforms every draw carries.
    Uniforms { size: u32 },
    /// The instance stride is not whole 4-byte words.
    Stride { stride: u32 },
    /// More textures or samplers than a program's bind group holds.
    Bindings { textures: u32, samplers: u32 },
    /// The backend's compiler or driver refused the code.
    Compile(String),
    /// The program carries no code in the backend's language.
    NoCode(ShaderLang),
}

impl std::fmt::Display for ProgramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProgramError::Slot(slot) => write!(
                f,
                "program slot {} is past the {} a renderer holds",
                slot.0,
                BatchKey::PROGRAM_SLOTS
            ),
            ProgramError::Uniforms { size } => write!(
                f,
                "a {size}-byte uniform block; a brush's comes inline, at most {} bytes",
                InlineUniforms::MAX
            ),
            ProgramError::Stride { stride } => {
                write!(f, "an instance stride of {stride} bytes is not whole words")
            }
            ProgramError::Bindings { textures, samplers } => write!(
                f,
                "{textures} textures and {samplers} samplers; a program binds at most \
                 {PROGRAM_TEXTURES} and {PROGRAM_SAMPLERS}"
            ),
            ProgramError::Compile(log) => f.write_str(log),
            ProgramError::NoCode(lang) => write!(f, "the program carries no {lang:?} code"),
        }
    }
}

impl std::error::Error for ProgramError {}

/// Pipelines a renderer keeps built with no slot drawing with them.
pub const IDLE_PIPELINES: usize = 8;

/// A pipeline the table holds.
struct Pipeline {
    id: PipelineId,
    /// The code and ABI it was built from; `None` for one a caller built and
    /// handed over, which no other program finds.
    source: Option<(ProgramCode, ProgramAbi)>,
    /// The slots drawing with it.
    users: u32,
    /// When the last slot let go of it, in let-goes: the oldest idle
    /// pipeline goes first.
    idle_since: u64,
}

/// Whether `a` and `b` are the same code: the same text or bytes, or the same
/// CPU program.
fn same_code(a: &ProgramCode, b: &ProgramCode) -> bool {
    match (a, b) {
        (ProgramCode::Cpu(a), ProgramCode::Cpu(b)) => std::sync::Arc::ptr_eq(a, b),
        (a, b) => a.lang() == b.lang() && a.bytes() == b.bytes(),
    }
}

/// One installed program.
struct Installed {
    pipeline: PipelineId,
    layout: BrushLayout,
    /// Its instances, as the device buffer holds them.
    pool: InstancePool<u32>,
    /// This frame's instances, in 4-byte words, runs aligned.
    scratch: Vec<u32>,
    /// Its samplers, in declaration order.
    samplers: Vec<SamplerId>,
    /// The bind group of each texture set its draws have bound.
    bindings: Vec<(ProgramTextures, BindGroupId)>,
}

impl Installed {
    /// Destroys every bind group it cached.
    fn unbind<B: GpuBackend>(&mut self, backend: &mut B) {
        for (_, bind_group) in self.bindings.drain(..) {
            backend.destroy_bind_group(bind_group);
        }
    }
}

/// The programs of one renderer, by slot.
pub(crate) struct ProgramTable {
    slots: Vec<Option<Installed>>,
    pipelines: Vec<Pipeline>,
    /// Counts let-goes, stamping the pipelines going idle.
    let_goes: u64,
    /// The backend's instance-offset alignment, in words.
    align_words: usize,
    /// The seconds since the window opened, written into the draws of the
    /// programs reading `time`.
    time: f32,
    /// This frame's boxes of programs reading `time`.
    clock_instances: u32,
}

impl ProgramTable {
    /// A table for a backend whose program instance offsets align to `align`
    /// bytes.
    pub(crate) fn new(align: u32) -> ProgramTable {
        ProgramTable {
            slots: Vec::new(),
            pipelines: Vec::new(),
            let_goes: 0,
            align_words: (align as usize / 4).max(1),
            time: 0.0,
            clock_instances: 0,
        }
    }

    /// Sets the clock the next frame's draws read.
    pub(crate) fn set_time(&mut self, seconds: f32) {
        self.time = seconds;
    }

    /// This frame's boxes of programs reading `time`.
    pub(crate) fn clock_instances(&self) -> u32 {
        self.clock_instances
    }

    /// The pipeline built from `code` for `abi`, if the table holds one.
    pub(crate) fn cached(&self, code: &ProgramCode, abi: ProgramAbi) -> Option<PipelineId> {
        self.pipelines
            .iter()
            .find(|p| {
                p.source
                    .as_ref()
                    .is_some_and(|(c, a)| *a == abi && same_code(c, code))
            })
            .map(|p| p.id)
    }

    /// Keeps `pipeline`, built from `code` for `abi`, for the programs of that
    /// code; idle until a slot draws with it.
    pub(crate) fn adopt<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        pipeline: PipelineId,
        code: &ProgramCode,
        abi: ProgramAbi,
    ) {
        self.let_goes += 1;
        self.pipelines.push(Pipeline {
            id: pipeline,
            source: Some((code.clone(), abi)),
            users: 0,
            idle_since: self.let_goes,
        });
        self.trim(backend);
    }

    /// Counts one more slot drawing with `pipeline`, a caller's own when the
    /// table does not hold it.
    fn hold(&mut self, pipeline: PipelineId) {
        match self.pipelines.iter_mut().find(|p| p.id == pipeline) {
            Some(held) => held.users += 1,
            None => self.pipelines.push(Pipeline {
                id: pipeline,
                source: None,
                users: 1,
                idle_since: 0,
            }),
        }
    }

    /// Counts one slot fewer drawing with `pipeline`: a caller's own is
    /// destroyed with its last slot, a built one goes idle.
    fn let_go<B: GpuBackend>(&mut self, backend: &mut B, pipeline: PipelineId) {
        let Some(at) = self.pipelines.iter().position(|p| p.id == pipeline) else {
            return;
        };
        let held = &mut self.pipelines[at];
        held.users -= 1;
        if held.users > 0 {
            return;
        }
        if held.source.is_none() {
            self.pipelines.swap_remove(at);
            backend.destroy_pipeline(pipeline);
            return;
        }
        self.let_goes += 1;
        held.idle_since = self.let_goes;
        self.trim(backend);
    }

    /// Destroys the longest idle pipelines past [`IDLE_PIPELINES`].
    fn trim<B: GpuBackend>(&mut self, backend: &mut B) {
        while self.pipelines.iter().filter(|p| p.users == 0).count() > IDLE_PIPELINES {
            let (at, _) = self
                .pipelines
                .iter()
                .enumerate()
                .filter(|(_, p)| p.users == 0)
                .min_by_key(|(_, p)| p.idle_since)
                .expect("past the bound, a pipeline is idle");
            backend.destroy_pipeline(self.pipelines.swap_remove(at).id);
        }
    }

    /// The pipelines held, drawn with or idle.
    pub(crate) fn pipelines(&self) -> usize {
        self.pipelines.len()
    }

    /// Installs `pipeline`, laid out by `layout` and sampling through
    /// `samplers`, in `slot`, letting go of the pipeline it replaces and
    /// destroying the bind groups the old program's draws bound.
    ///
    /// # Errors
    ///
    /// A slot past the key's field, or a layout a draw cannot carry; the slot
    /// is left as it was and `pipeline` is the caller's.
    pub(crate) fn set<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        slot: ProgramSlot,
        pipeline: PipelineId,
        layout: BrushLayout,
        samplers: Vec<SamplerId>,
    ) -> Result<(), ProgramError> {
        check(slot, &layout)?;
        debug_assert_eq!(samplers.len(), layout.samplers.len());
        let index = slot.0 as usize;
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, || None);
        }
        self.hold(pipeline);
        match &mut self.slots[index] {
            Some(installed) => {
                let old = std::mem::replace(&mut installed.pipeline, pipeline);
                installed.unbind(backend);
                installed.layout = layout;
                installed.samplers = samplers;
                self.let_go(backend, old);
            }
            empty => {
                *empty = Some(Installed {
                    pipeline,
                    layout,
                    pool: InstancePool::new(BufferUsage::INSTANCE, "program-instances"),
                    scratch: Vec::new(),
                    samplers,
                    bindings: Vec::new(),
                });
            }
        }
        Ok(())
    }

    /// Empties `slot`, destroying its buffer and letting go of its pipeline.
    /// Whether it held a program.
    pub(crate) fn release<B: GpuBackend>(&mut self, backend: &mut B, slot: ProgramSlot) -> bool {
        let Some(mut installed) = self.slots.get_mut(slot.0 as usize).and_then(Option::take) else {
            return false;
        };
        installed.unbind(backend);
        self.let_go(backend, installed.pipeline);
        let mut pool = installed.pool;
        pool.release(backend);
        true
    }

    /// The pipeline and layout in `slot`.
    pub(crate) fn get(&self, slot: ProgramSlot) -> Option<(PipelineId, &BrushLayout)> {
        let installed = self.slots.get(slot.0 as usize)?.as_ref()?;
        Some((installed.pipeline, &installed.layout))
    }

    /// Whether a draw of `slot` with `params` and `textures` would draw: the
    /// slot holds a program whose instance `params` is, and `textures` holds
    /// each image it samples.
    pub(crate) fn draws(
        &self,
        slot: ProgramSlot,
        params: &[u8],
        textures: &ProgramTextures,
    ) -> bool {
        self.get(slot).is_some_and(|(_, layout)| {
            params.len() == layout.stride as usize
                && textures[..layout.textures.len()]
                    .iter()
                    .all(Option::is_some)
        })
    }

    /// The bind group a draw of `slot` sampling `textures` binds, created on
    /// first sight; `None` for a program binding nothing.
    ///
    /// # Panics
    ///
    /// When the slot is empty, or a texture the program samples is missing:
    /// a draw is lowered only when [`draws`](Self::draws) holds.
    pub(crate) fn bind_group<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        slot: ProgramSlot,
        textures: &ProgramTextures,
    ) -> Option<BindGroupId> {
        let installed = self.slots[slot.0 as usize]
            .as_mut()
            .expect("a program draw names an installed program");
        let count = installed.layout.textures.len();
        if count == 0 && installed.samplers.is_empty() {
            return None;
        }
        if let Some((_, bind_group)) = installed.bindings.iter().find(|(t, _)| t == textures) {
            return Some(*bind_group);
        }
        let bindings = textures[..count]
            .iter()
            .map(|t| Binding::Texture(t.expect("a draw holds each image it samples")))
            .chain(installed.samplers.iter().map(|s| Binding::Sampler(*s)))
            .collect();
        let bind_group = backend.create_bind_group(&BindGroupDesc {
            label: "program",
            bindings,
        });
        installed.bindings.push((*textures, bind_group));
        Some(bind_group)
    }

    /// Destroys every bind group naming `texture`, which is going away.
    pub(crate) fn release_texture<B: GpuBackend>(&mut self, backend: &mut B, texture: TextureId) {
        for installed in self.slots.iter_mut().flatten() {
            installed.bindings.retain(|(textures, bind_group)| {
                let names = textures.contains(&Some(texture));
                if names {
                    backend.destroy_bind_group(*bind_group);
                }
                !names
            });
        }
    }

    /// The device bytes the installed programs' instance buffers hold.
    pub(crate) fn buffer_bytes(&self) -> u64 {
        self.slots.iter().flatten().map(|p| p.pool.bytes()).sum()
    }

    /// The programs installed.
    pub(crate) fn installed(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Clears every program's scratch for a new frame's lowering.
    pub(crate) fn begin_lowering(&mut self) {
        for installed in self.slots.iter_mut().flatten() {
            installed.scratch.clear();
        }
        self.clock_instances = 0;
    }

    /// The byte offset where an instance goes in a buffer whose instances end
    /// at byte `end`: right there when the draw extends the previous draw of
    /// its program, else at the next aligned offset. The one rule both the
    /// lowering and the introspection walk place instances by.
    pub(crate) fn run_start(&self, end: usize, extends: bool) -> usize {
        if extends {
            end
        } else {
            let align = self.align_words * 4;
            end.div_ceil(align) * align
        }
    }

    /// Appends one instance to `slot`'s scratch: `params` with the box at
    /// `pos`/`size` written in. `extends` says the draw joins the previous
    /// draw of the program, which needs no alignment. Returns the instance's
    /// byte offset, or `None` when the slot holds no program for `params`.
    pub(crate) fn push(
        &mut self,
        slot: ProgramSlot,
        extends: bool,
        params: &[u8],
        pos: [f32; 2],
        size: [f32; 2],
    ) -> Option<u32> {
        let end = self.slots.get(slot.0 as usize)?.as_ref()?.scratch.len() * 4;
        let start = self.run_start(end, extends);
        let installed = self.slots[slot.0 as usize].as_mut()?;
        if params.len() != installed.layout.stride as usize {
            return None;
        }
        let scratch = &mut installed.scratch;
        scratch.resize(start / 4, 0);
        let base = scratch.len();
        // Words keep the encoded bytes as they are: the buffer is viewed as
        // bytes in memory order.
        scratch.extend(
            params
                .as_chunks::<4>()
                .0
                .iter()
                .map(|w| u32::from_ne_bytes(*w)),
        );
        let layout = &installed.layout;
        let word = |offset: u32, lane: u32| base + (offset / 4 + lane) as usize;
        let bits = |v: f32| u32::from_ne_bytes(v.to_le_bytes());
        scratch[word(layout.rect_pos, 0)] = bits(pos[0]);
        scratch[word(layout.rect_pos, 1)] = bits(pos[1]);
        scratch[word(layout.rect_size, 0)] = bits(size[0]);
        scratch[word(layout.rect_size, 1)] = bits(size[1]);
        self.clock_instances += u32::from(layout.reads_time());
        Some((base * 4) as u32)
    }

    /// Syncs every program's buffer with its scratch. Returns the upload
    /// ranges and bytes.
    pub(crate) fn sync<B: GpuBackend>(&mut self, backend: &mut B) -> (usize, usize) {
        let mut ranges = 0;
        let mut bytes = 0;
        for installed in self.slots.iter_mut().flatten() {
            ranges += installed.pool.sync(backend, &installed.scratch);
            bytes += installed.pool.last_upload_bytes();
        }
        (ranges, bytes)
    }

    /// The draw of `count` instances of `slot` from byte `start`, into a target
    /// of `viewport`.
    ///
    /// # Panics
    ///
    /// When the slot is empty or holds no buffer: a segment names only a
    /// program it pushed instances to.
    pub(crate) fn command(
        &self,
        slot: ProgramSlot,
        bind_group: Option<BindGroupId>,
        start: u32,
        count: u32,
        viewport: [f32; 2],
        scissor: Option<(u32, u32, u32, u32)>,
    ) -> DrawCommand {
        let installed = self.slots[slot.0 as usize]
            .as_ref()
            .expect("a program segment names an installed program");
        let mut uniforms = [0u8; InlineUniforms::MAX];
        installed
            .layout
            .write_uniforms(&mut uniforms, viewport, self.time);
        DrawCommand {
            pipeline: installed.pipeline,
            bind_group,
            geometry: Geometry::Generated { count },
            instance_buffer: installed
                .pool
                .buffer()
                .expect("a program's buffer exists when a segment draws from it"),
            instance_offset: start as usize,
            uniforms: InlineUniforms::new(&uniforms[..installed.layout.uniform_size as usize]),
            scissor,
        }
    }
}

fn check(slot: ProgramSlot, layout: &BrushLayout) -> Result<(), ProgramError> {
    if slot.0 >= BatchKey::PROGRAM_SLOTS {
        return Err(ProgramError::Slot(slot));
    }
    check_layout(layout)
}

/// Whether a draw can carry a program laid out by `layout` and a bind group
/// bind what it samples.
fn check_layout(layout: &BrushLayout) -> Result<(), ProgramError> {
    let abi = layout.abi();
    if abi.textures > PROGRAM_TEXTURES || abi.samplers > PROGRAM_SAMPLERS {
        return Err(ProgramError::Bindings {
            textures: abi.textures,
            samplers: abi.samplers,
        });
    }
    if layout.uniform_size as usize > InlineUniforms::MAX {
        return Err(ProgramError::Uniforms {
            size: layout.uniform_size,
        });
    }
    if !layout.stride.is_multiple_of(4) || layout.stride == 0 {
        return Err(ProgramError::Stride {
            stride: layout.stride,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_gpu::HeadlessRaster;

    fn layout(stride: u32) -> BrushLayout {
        BrushLayout {
            stride,
            rect_pos: 0,
            rect_size: 8,
            uniform_size: 16,
            viewport_size: 0,
            time: None,
            textures: Vec::new(),
            samplers: Vec::new(),
            vertex_textures: false,
            params: Vec::new(),
        }
    }

    #[test]
    fn a_run_starts_aligned_and_an_extending_draw_follows_its_run() {
        let mut gpu = HeadlessRaster::new();
        // A Vulkan-like alignment over a stride that does not divide it.
        let mut table = ProgramTable::new(256);
        let pipeline = PipelineId {
            index: 0,
            generation: 1,
        };
        table
            .set(&mut gpu, ProgramSlot(2), pipeline, layout(20), Vec::new())
            .unwrap();
        let params = [0u8; 20];
        let at = |table: &mut ProgramTable, extends| {
            table
                .push(ProgramSlot(2), extends, &params, [1.0, 2.0], [3.0, 4.0])
                .unwrap()
        };
        assert_eq!(at(&mut table, false), 0);
        assert_eq!(at(&mut table, true), 20);
        assert_eq!(at(&mut table, false), 256, "a new run aligns");
        assert_eq!(at(&mut table, true), 276);
        assert_eq!(table.run_start(296, false), 512);
        assert_eq!(table.run_start(296, true), 296);
        // The box lands in the instance as the layout places it.
        let words = &table.slots[2].as_ref().unwrap().scratch[64..69];
        assert_eq!(
            words,
            [1.0f32, 2.0, 3.0, 4.0, 0.0].map(|v| u32::from_ne_bytes(v.to_le_bytes()))
        );
        assert_eq!(
            table.push(ProgramSlot(2), true, &params[..16], [0.0; 2], [0.0; 2]),
            None,
            "parameters of another stride"
        );
        assert_eq!(
            table.push(ProgramSlot(1), false, &params, [0.0; 2], [0.0; 2]),
            None
        );
        assert_eq!(
            table.set(&mut gpu, ProgramSlot(0), pipeline, layout(18), Vec::new()),
            Err(ProgramError::Stride { stride: 18 })
        );
    }

    #[test]
    fn a_texture_set_binds_once_and_goes_with_its_texture_or_program() {
        use viso_gpu::{SamplerDesc, TextureDesc, TextureFormat};
        use viso_shader::program::BrushTexture;

        let mut gpu = HeadlessRaster::new();
        let mut table = ProgramTable::new(4);
        let pipeline = PipelineId {
            index: 0,
            generation: 1,
        };
        let mut sampled = layout(16);
        sampled.textures = vec![BrushTexture {
            name: "photo".into(),
            source_span: Default::default(),
        }];
        sampled.samplers = vec![SamplerDesc::LINEAR_CLAMP];
        let sampler = gpu.create_sampler(&SamplerDesc::LINEAR_CLAMP);
        table
            .set(&mut gpu, ProgramSlot(0), pipeline, sampled, vec![sampler])
            .unwrap();
        let texture = |gpu: &mut HeadlessRaster| {
            gpu.create_texture(&TextureDesc {
                width: 1,
                height: 1,
                format: TextureFormat::Rgba8Unorm,
                render_target: false,
                label: "image",
            })
        };
        let (a, b) = (texture(&mut gpu), texture(&mut gpu));
        let set = |t| [Some(t), None, None, None];
        let params = [0u8; 16];
        assert!(table.draws(ProgramSlot(0), &params, &set(a)));
        assert!(
            !table.draws(ProgramSlot(0), &params, &[None; 4]),
            "a missing image draws nothing"
        );
        let before = gpu.bind_group_count();
        let first = table.bind_group(&mut gpu, ProgramSlot(0), &set(a));
        assert!(first.is_some());
        assert_eq!(table.bind_group(&mut gpu, ProgramSlot(0), &set(a)), first);
        let other = table.bind_group(&mut gpu, ProgramSlot(0), &set(b));
        assert_ne!(other, first);
        assert_eq!(gpu.bind_group_count(), before + 2, "one per texture set");

        // A texture going away takes its bind group; the program the rest.
        let retired = gpu.retired_count();
        table.release_texture(&mut gpu, a);
        assert_eq!(table.slots[0].as_ref().unwrap().bindings.len(), 1);
        assert_eq!(gpu.retired_count(), retired + 1);
        table.release(&mut gpu, ProgramSlot(0));
        assert!(gpu.retired_count() >= retired + 2, "the other bind group");

        // A program binding nothing draws without a bind group.
        table
            .set(&mut gpu, ProgramSlot(1), pipeline, layout(16), Vec::new())
            .unwrap();
        assert_eq!(table.bind_group(&mut gpu, ProgramSlot(1), &[None; 4]), None);
    }

    #[test]
    fn a_pipeline_is_kept_by_its_code_and_idles_within_a_bound() {
        let mut gpu = HeadlessRaster::new();
        let mut table = ProgramTable::new(4);
        let id = |index| PipelineId {
            index,
            generation: 1,
        };
        let code = |n: u32| ProgramCode::Wgsl(format!("// program {n}"));
        let abi = layout(16).abi();
        table.adopt(&mut gpu, id(0), &code(0), abi);
        assert_eq!(table.cached(&code(0), abi), Some(id(0)));
        assert_eq!(table.cached(&code(1), abi), None);
        assert_eq!(
            table.cached(&code(0), layout(32).abi()),
            None,
            "the same code for another ABI"
        );

        // Two slots of one code draw with one pipeline, which outlives them.
        for slot in [ProgramSlot(0), ProgramSlot(1)] {
            table
                .set(&mut gpu, slot, id(0), layout(16), Vec::new())
                .unwrap();
        }
        assert_eq!(table.pipelines(), 1);
        table.release(&mut gpu, ProgramSlot(0));
        table.release(&mut gpu, ProgramSlot(1));
        assert_eq!(table.cached(&code(0), abi), Some(id(0)), "idle");

        // Past the bound the longest idle goes first; one drawn with stays.
        table.adopt(&mut gpu, id(1), &code(1), abi);
        table
            .set(&mut gpu, ProgramSlot(0), id(1), layout(16), Vec::new())
            .unwrap();
        for n in 2..2 + IDLE_PIPELINES as u32 {
            table.adopt(&mut gpu, id(n), &code(n), abi);
        }
        assert_eq!(table.pipelines(), 1 + IDLE_PIPELINES);
        assert_eq!(table.cached(&code(0), abi), None, "the oldest went");
        assert_eq!(table.cached(&code(1), abi), Some(id(1)), "drawn with");
        assert_eq!(table.cached(&code(2), abi), Some(id(2)));

        // A caller's own pipeline goes with its last slot; no program finds it.
        let own = id(99);
        table
            .set(&mut gpu, ProgramSlot(3), own, layout(16), Vec::new())
            .unwrap();
        assert_eq!(table.pipelines(), 2 + IDLE_PIPELINES);
        table.release(&mut gpu, ProgramSlot(3));
        assert_eq!(table.pipelines(), 1 + IDLE_PIPELINES);
    }
}
