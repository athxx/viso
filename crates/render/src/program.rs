//! The user programs a renderer draws: brush-profile programs installed by
//! slot, each with its own persistent instance buffer.
//!
//! A slot holds a pipeline and the [`BrushLayout`] it was laid out by.
//! Installing into an occupied slot replaces the program: the old pipeline is
//! destroyed through the backend's fence-deferred path, so a frame still in
//! flight finishes with it. Releasing a slot destroys its pipeline and buffer
//! the same way.
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
//! [`ShaderDraw`]: crate::primitive::ShaderDraw

use viso_gpu::backend::{DrawCommand, Geometry, InlineUniforms};
use viso_gpu::{BufferUsage, GpuBackend, PipelineId};
use viso_shader::program::BrushLayout;

use crate::batch::BatchKey;
use crate::pool::InstancePool;
use crate::primitive::ProgramSlot;

/// Why a program was not installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramError {
    /// The slot is past [`BatchKey::PROGRAM_SLOTS`].
    Slot(ProgramSlot),
    /// The uniform block does not fit the inline uniforms every draw carries.
    Uniforms { size: u32 },
    /// The instance stride is not whole 4-byte words.
    Stride { stride: u32 },
    /// The backend's compiler or driver refused the code.
    Compile(String),
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
            ProgramError::Compile(log) => f.write_str(log),
        }
    }
}

impl std::error::Error for ProgramError {}

/// One installed program.
struct Installed {
    pipeline: PipelineId,
    layout: BrushLayout,
    /// Its instances, as the device buffer holds them.
    pool: InstancePool<u32>,
    /// This frame's instances, in 4-byte words, runs aligned.
    scratch: Vec<u32>,
}

/// The programs of one renderer, by slot.
pub(crate) struct ProgramTable {
    slots: Vec<Option<Installed>>,
    /// The backend's instance-offset alignment, in words.
    align_words: usize,
}

impl ProgramTable {
    /// A table for a backend whose program instance offsets align to `align`
    /// bytes.
    pub(crate) fn new(align: u32) -> ProgramTable {
        ProgramTable {
            slots: Vec::new(),
            align_words: (align as usize / 4).max(1),
        }
    }

    /// Installs `pipeline`, laid out by `layout`, in `slot`, destroying the
    /// pipeline it replaces.
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
    ) -> Result<(), ProgramError> {
        check(slot, &layout)?;
        let index = slot.0 as usize;
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, || None);
        }
        match &mut self.slots[index] {
            Some(installed) => {
                let old = std::mem::replace(&mut installed.pipeline, pipeline);
                if old != pipeline {
                    backend.destroy_pipeline(old);
                }
                installed.layout = layout;
            }
            empty => {
                *empty = Some(Installed {
                    pipeline,
                    layout,
                    pool: InstancePool::new(BufferUsage::INSTANCE, "program-instances"),
                    scratch: Vec::new(),
                });
            }
        }
        Ok(())
    }

    /// Empties `slot`, destroying its pipeline and buffer. Whether it held a
    /// program.
    pub(crate) fn release<B: GpuBackend>(&mut self, backend: &mut B, slot: ProgramSlot) -> bool {
        let Some(installed) = self.slots.get_mut(slot.0 as usize).and_then(Option::take) else {
            return false;
        };
        backend.destroy_pipeline(installed.pipeline);
        let mut pool = installed.pool;
        pool.release(backend);
        true
    }

    /// The pipeline and layout in `slot`.
    pub(crate) fn get(&self, slot: ProgramSlot) -> Option<(PipelineId, &BrushLayout)> {
        let installed = self.slots.get(slot.0 as usize)?.as_ref()?;
        Some((installed.pipeline, &installed.layout))
    }

    /// Whether a draw of `slot` with `params` would draw: the slot holds a
    /// program whose instance `params` is.
    pub(crate) fn draws(&self, slot: ProgramSlot, params: &[u8]) -> bool {
        self.get(slot)
            .is_some_and(|(_, layout)| params.len() == layout.stride as usize)
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
        start: u32,
        count: u32,
        viewport: [f32; 2],
        scissor: Option<(u32, u32, u32, u32)>,
    ) -> DrawCommand {
        let installed = self.slots[slot.0 as usize]
            .as_ref()
            .expect("a program segment names an installed program");
        let mut uniforms = [0u8; InlineUniforms::MAX];
        installed.layout.write_uniforms(&mut uniforms, viewport);
        DrawCommand {
            pipeline: installed.pipeline,
            bind_group: None,
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
            .set(&mut gpu, ProgramSlot(2), pipeline, layout(20))
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
            table.set(&mut gpu, ProgramSlot(0), pipeline, layout(18)),
            Err(ProgramError::Stride { stride: 18 })
        );
    }
}
