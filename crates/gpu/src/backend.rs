//! The trait-based RHI: a single source-level GPU interface (§17.2).
//!
//! There is exactly one `GpuBackend` trait; the concrete backend is chosen at
//! compile time by [`create_device`] and the facade holds it monomorphized, so
//! the frame hot path has no `dyn GpuBackend` dispatch (ADR-007). The trait
//! exists to keep the Metal and headless-raster backends *source-compatible* and
//! to let cold-path code (setup, tests) be backend-generic.
//!
//! The backend consumes a low-level [`DrawList`] — a flat sequence of
//! [`DrawCommand`]s that `viso-render` lowers its frame packet into. This keeps
//! the DAG edge one-way: `viso-render → viso-gpu` (render knows about draw
//! commands; gpu never knows about primitives, batches, or widgets).

use viso_handle::RawWindowHandle;

use crate::instance::InstanceLayout;
use crate::resource::{
    BindGroupDesc, BufferDesc, Caps, PipelineDesc, SamplerDesc, TextureDesc, TextureFormat,
};
use crate::{BindGroupId, BufferId, PipelineId, SamplerId, SurfaceId, TextureId};

/// A frame acquired from a surface: an opaque, backend-specific token that must
/// be handed back to [`GpuBackend::present`]. Carried by value so the borrow
/// checker enforces "one present per begin_frame".
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// The surface this frame belongs to.
    pub surface: SurfaceId,
    /// Backend-defined index of the acquired drawable (e.g. Metal drawable slot).
    pub drawable: u32,
}

/// A load action for a render pass's color attachment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadOp {
    /// Clear to the given premultiplied RGBA color before drawing.
    Clear([f32; 4]),
    /// Preserve existing contents.
    Load,
}

/// The target a render pass draws into.
#[derive(Debug, Clone, Copy)]
pub enum RenderTarget {
    /// The acquired swapchain frame.
    Surface(Frame),
    /// An offscreen texture (layer / composite pass).
    Texture(TextureId),
}

/// A render pass: one target, one load action, and a contiguous range of draw
/// commands inside the frame's flat [`DrawList::commands`] buffer.
///
/// The commands are referenced by range rather than by a borrowed slice so a
/// pass carries no lifetime and can live in a renderer-owned scratch buffer that
/// is cleared and refilled each frame (0 steady-state heap allocations, §7.1).
#[derive(Debug, Clone, Copy)]
pub struct RenderPass {
    /// Where this pass renders.
    pub target: RenderTarget,
    /// What to do with the target's prior contents.
    pub load: LoadOp,
    /// Index of this pass's first command in [`DrawList::commands`].
    pub first_command: u32,
    /// Number of commands this pass draws, starting at `first_command`.
    pub command_count: u32,
}

impl RenderPass {
    /// This pass's command range into [`DrawList::commands`].
    #[inline]
    pub fn command_range(&self) -> core::ops::Range<usize> {
        let start = self.first_command as usize;
        start..start + self.command_count as usize
    }
}

/// Inline per-draw uniform bytes, stored by value so a [`DrawCommand`] carries no
/// borrow. The backends read [`Self::as_bytes`] and hand it to Metal's
/// `setVertexBytes`/`setFragmentBytes` (headless ignores it), preserving the
/// arbitrary-length inline-byte uniform semantics — capped at [`Self::MAX`] bytes,
/// which comfortably fits the built-ins' viewport uniform.
#[derive(Debug, Clone, Copy)]
pub struct InlineUniforms {
    bytes: [u8; Self::MAX],
    len: u8,
}

impl InlineUniforms {
    /// Maximum inline uniform payload in bytes.
    pub const MAX: usize = 16;

    /// Empty uniforms (no inline bytes bound).
    pub const EMPTY: Self = Self {
        bytes: [0; Self::MAX],
        len: 0,
    };

    /// Build inline uniforms from `src`.
    ///
    /// # Panics
    /// Panics if `src.len() > Self::MAX`.
    #[inline]
    pub fn new(src: &[u8]) -> Self {
        assert!(
            src.len() <= Self::MAX,
            "inline uniform payload {} exceeds {} bytes",
            src.len(),
            Self::MAX
        );
        let mut bytes = [0u8; Self::MAX];
        bytes[..src.len()].copy_from_slice(src);
        Self {
            bytes,
            len: src.len() as u8,
        }
    }

    /// The inline uniform bytes.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// Whether there are any inline uniform bytes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// How a [`DrawCommand`] sources its geometry — the two shapes Viso draws.
///
/// The built-in rect primitives (Quad/Image/GlyphRun) carry no vertex buffer:
/// the vertex shader synthesizes a unit quad from `vertex_id` and instances it,
/// so their geometry is [`Geometry::Generated`]. Vector primitives (Path/Mesh)
/// are CPU-tessellated into a real vertex + index buffer and drawn once as an
/// indexed triangle list — [`Geometry::IndexedMesh`]. Splitting the two keeps
/// the backend's encode path explicit instead of dispatching on the pipeline's
/// built-in tag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Geometry {
    /// A `vertex_id`-generated unit quad, instanced. The per-instance data lives
    /// in [`DrawCommand::instance_buffer`] at `instance_offset`; `count` is the
    /// number of instances (6 vertices each).
    Generated {
        /// Number of instances to draw.
        count: u32,
    },
    /// A real vertex buffer (bound at index 0) drawn as one indexed triangle
    /// list of `index_count` indices. There is no per-instance data; per-vertex
    /// attributes (position, color, AA edge) come from the vertex buffer.
    IndexedMesh {
        /// The per-vertex geometry buffer, bound at index 0.
        vertex_buffer: BufferId,
        /// The index buffer. Its element width is [`Self::IndexedMesh::index_format`];
        /// `index_offset`/`index_count` count elements of that width, not bytes.
        index_buffer: BufferId,
        /// Width of each index in `index_buffer` (16- or 32-bit). Small geometry
        /// (≤ 64k vertices) uses [`IndexFormat::U16`] to halve index bandwidth
        /// and residency; larger geometry uses [`IndexFormat::U32`] (§13.4).
        index_format: IndexFormat,
        /// Offset (in indices) of this draw's first index into `index_buffer`.
        /// Multiple mesh draws (e.g. different clips) share one index buffer;
        /// each starts at its own offset.
        index_offset: u32,
        /// Number of indices to draw (3 per triangle).
        index_count: u32,
    },
}

/// Element width of an index buffer bound by [`Geometry::IndexedMesh`].
///
/// The tessellator picks the narrowest width that addresses a geometry's vertex
/// count: `U16` for ≤ 64k vertices (half the bandwidth and residency of `U32`),
/// `U32` above that. The width is chosen once when geometry is (re-)tessellated
/// and cached with it, so no per-frame branch is on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexFormat {
    /// 16-bit indices (`u16`). Valid when the mesh has ≤ 65 536 vertices.
    U16,
    /// 32-bit indices (`u32`).
    U32,
}

impl IndexFormat {
    /// Bytes per index element (2 for `U16`, 4 for `U32`).
    #[inline]
    pub const fn size(self) -> usize {
        match self {
            IndexFormat::U16 => 2,
            IndexFormat::U32 => 4,
        }
    }
}

/// A single draw call: the lowered form of one render batch.
///
/// [`Self::geometry`] selects the geometry source: `vertex_id`-generated
/// instanced quads, or a real indexed vertex/index buffer for vector meshes.
/// Uniforms are passed inline as bytes (Metal `setVertexBytes`/`setFragmentBytes`).
#[derive(Debug, Clone, Copy)]
pub struct DrawCommand {
    /// The pipeline (shader + blend + formats) for this draw.
    pub pipeline: PipelineId,
    /// Optional bind group (textures + samplers + uniform buffers).
    pub bind_group: Option<BindGroupId>,
    /// Where this draw's geometry comes from (generated quads vs. indexed mesh).
    pub geometry: Geometry,
    /// Per-instance data buffer, bound at index 1 (used by
    /// [`Geometry::Generated`]; ignored for indexed meshes).
    pub instance_buffer: BufferId,
    /// Byte offset into the instance buffer for the first instance of this draw.
    pub instance_offset: usize,
    /// Inline uniform bytes for this draw (bound at a fixed uniform index),
    /// stored by value so the command carries no borrow.
    pub uniforms: InlineUniforms,
    /// Scissor rect in physical pixels `(x, y, w, h)`, if the batch is clipped.
    pub scissor: Option<(u32, u32, u32, u32)>,
}

/// A flat, backend-neutral draw list for one frame: the packet `viso-render`
/// hands to [`GpuBackend::encode`].
pub struct DrawList<'a> {
    /// All draw commands for the frame, concatenated across passes in execution
    /// order. Each [`RenderPass`] in `passes` indexes a contiguous range here.
    pub commands: &'a [DrawCommand],
    /// The passes, in execution order: offscreen work in dependency order (a pass
    /// that samples another's target comes after it), then main. A backend may
    /// assume a target is fully written by the time a later pass reads it, and that
    /// two passes writing the same texture are ordered as listed — offscreen
    /// targets are pooled and aliased, so order is the only lifetime guarantee.
    /// Each pass references its commands by range into `commands`.
    pub passes: &'a [RenderPass],
}

/// The single RHI trait. Cold-path methods create resources; the hot path is
/// `write_buffer` + `encode` + `present`.
pub trait GpuBackend {
    /// Create a GPU buffer.
    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId;
    /// Create a texture.
    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId;
    /// Create a sampler.
    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId;

    /// Create a render pipeline. `layout` is the `#[derive(GpuPod)]` layout
    /// of the instance type; it is validated against `desc.instance_schema`
    /// before the pipeline is built (registration-time layout check).
    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, crate::instance::LayoutError>;

    /// Create a bind group.
    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId;

    /// Retire a buffer for deferred destruction.
    ///
    /// The buffer may still be read by an in-flight frame, so its storage slot is
    /// not freed immediately: it is parked against the current frame's epoch and
    /// reclaimed only once the GPU has finished that epoch (see the epoch/fence
    /// contract on [`begin_frame`](Self::begin_frame) / [`present`](Self::present)).
    /// After reclamation `id` — and every copy of it — resolves to nothing, so a
    /// use-after-destroy is a detectable miss, never a wrong-object hit. Retiring
    /// an already-stale or unknown handle is a no-op.
    fn destroy_buffer(&mut self, id: BufferId);
    /// Retire a texture for deferred destruction (see [`destroy_buffer`](Self::destroy_buffer)).
    fn destroy_texture(&mut self, id: TextureId);
    /// Retire a sampler for deferred destruction (see [`destroy_buffer`](Self::destroy_buffer)).
    fn destroy_sampler(&mut self, id: SamplerId);
    /// Retire a pipeline for deferred destruction (see [`destroy_buffer`](Self::destroy_buffer)).
    fn destroy_pipeline(&mut self, id: PipelineId);
    /// Retire a bind group for deferred destruction (see [`destroy_buffer`](Self::destroy_buffer)).
    fn destroy_bind_group(&mut self, id: BindGroupId);

    /// Overwrite a region of a buffer with CPU bytes (ring/persistent upload).
    fn write_buffer(&mut self, id: BufferId, offset: usize, bytes: &[u8]);

    /// Overwrite a region of a texture (atlas dirty-rect upload).
    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]);

    /// Create a swapchain surface bound to a native window.
    fn create_surface(&mut self, raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId;
    /// Resize a surface's swapchain.
    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32);

    /// Acquire the next drawable for `surface`, opening a new frame.
    ///
    /// Returns `None` when no drawable is available — the surface is momentarily
    /// out of date (a resize/DPI change the compositor has not caught up with) or
    /// the drawable pool is exhausted. This is transient, not an error: the caller
    /// skips the frame and retries next tick, and because the retained scene is
    /// unchanged the same image is produced then. A failed acquire leaves the
    /// epoch untouched, so no phantom in-flight frame is parked to stall the fence.
    ///
    /// On a successful acquire this advances the backend's monotonic frame epoch
    /// and, before doing so, reclaims the storage slots of resources retired in
    /// epochs the GPU has since finished — so a slot freed by
    /// [`destroy_buffer`](Self::destroy_buffer) and friends becomes reusable
    /// exactly one safe in-flight window later, and the retire queue drains as
    /// frames complete rather than growing without bound.
    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame>;
    /// Encode and submit a draw list.
    fn encode(&mut self, list: &DrawList<'_>);
    /// Present a previously begun frame, submitting it to the display.
    ///
    /// The presented frame carries the current epoch; when the GPU finishes it,
    /// the backend's fence advances so the next [`begin_frame`](Self::begin_frame)
    /// can reclaim anything that was awaiting this frame's completion.
    fn present(&mut self, frame: Frame);

    /// Recover from a lost or reset device on `surface` (§6.4).
    ///
    /// A device loss (GPU reset, driver restart, display reconfiguration) or an
    /// abandoned frame invalidates any drawable held between
    /// [`begin_frame`](Self::begin_frame) and [`present`](Self::present): the
    /// present that would have signalled its epoch will never run, so the fence
    /// would stall and parked slots would never reclaim. This hook drops the held
    /// drawable and unblocks the retire queue by treating the current epoch as
    /// finished, so the next [`begin_frame`](Self::begin_frame) reclaims cleanly
    /// and re-acquires a fresh drawable. Surface-owned GPU state is otherwise
    /// re-derived lazily on the next frame; persistent resources (buffers,
    /// textures, pipelines) survive, since a lost drawable does not free them.
    fn device_lost(&mut self, surface: SurfaceId);

    /// Static device capabilities.
    fn caps(&self) -> &Caps;

    /// The surface's swapchain format (color attachment format for pipelines).
    fn surface_format(&self, surface: SurfaceId) -> TextureFormat;

    /// The surface's color space — the primaries and transfer function the
    /// compositor reads its texels through.
    ///
    /// Separate from [`surface_format`](Self::surface_format), which fixes only
    /// precision and range: together they name the target's
    /// [`ColorDomain`](crate::ColorDomain), which is what a render graph plans its
    /// intermediate formats against. A backend that has not yet negotiated a wide
    /// or extended space with its compositor reports the ordinary SDR default,
    /// which is the truthful answer for it rather than an optimistic one.
    fn surface_color_space(&self, surface: SurfaceId) -> crate::ColorSpace {
        let _ = surface;
        crate::ColorSpace::Srgb
    }
}
